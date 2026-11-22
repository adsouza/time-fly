//! Globe geometry: cube-spheres for the fixed recent globe, the ocean shell and the atmosphere,
//! and the plate pieces built from the footprint grid.
//!
//! Every corner is keyed by its integer position on the cube lattice, so a corner shared by
//! neighbouring cells or cube faces always gets the identical direction (bit for bit).
//!
//! Plate pieces: each occupied footprint cell is emitted once per candidate plate, as two
//! triangles whose vertices all carry that plate and that cell's interval list for the plate.
//! Vertices are shared only between cells with the same plate and the same interval list, so
//! they are duplicated along plate and validity boundaries. Each vertex also carries the
//! interval list of its corner's canonical cell (see [`build_plate_pieces`]), so every
//! duplicate of a corner makes the same endpoint-availability decision and displaces equally.

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use bevy::render::render_resource::{CompareFunction, VertexFormat};
use timefly_core::cube::{CubeFace, dir_to_texel};
use timefly_core::geo::RenderDir;
use timefly_core::geodynamics::{PlateFootprints, PlateIndex};

/// The plate pieces' depth test (reversed Z, as Bevy's opaque pass); set explicitly by
/// `PlateMaterial` so the draw-order rule below cannot drift from it.
pub const PIECE_DEPTH_COMPARE: CompareFunction = CompareFunction::GreaterEqual;

/// Whether a later-drawn primitive wins an exact depth tie under `compare`.
pub fn later_draw_wins_ties(compare: CompareFunction) -> bool {
    matches!(
        compare,
        CompareFunction::GreaterEqual | CompareFunction::LessEqual | CompareFunction::Always
    )
}

/// Per-vertex plate piece data: `[dense plate index, cell interval list, corner interval list]`.
pub const ATTRIBUTE_PIECE: MeshVertexAttribute =
    MeshVertexAttribute::new("TimeflyPiece", 0x7f1e_0901, VertexFormat::Uint32x3);

/// A corner on the cube lattice, in units of half a cell: each coordinate is in `-n..=n`.
type CornerKey = [i32; 3];

/// The lattice key of corner `(i, j)` (column, row; each `0..=n`) of `face`, matching
/// [`timefly_core::cube::texel_center_dir`]'s face table.
fn corner_key(face: CubeFace, i: u32, j: u32, n: u32) -> CornerKey {
    let s = 2 * i as i32 - n as i32;
    let t = 2 * j as i32 - n as i32;
    let n = n as i32;
    match face {
        CubeFace::PosX => [n, -t, -s],
        CubeFace::NegX => [-n, -t, s],
        CubeFace::PosY => [s, n, t],
        CubeFace::NegY => [s, -n, -t],
        CubeFace::PosZ => [s, -t, n],
        CubeFace::NegZ => [-s, -t, -n],
    }
}

fn key_dir_f64(key: CornerKey) -> bevy::math::DVec3 {
    bevy::math::DVec3::new(f64::from(key[0]), f64::from(key[1]), f64::from(key[2])).normalize()
}

/// The unit direction of a lattice corner, computed once from the key (f64, then rounded).
fn key_dir(key: CornerKey) -> [f32; 3] {
    key_dir_f64(key).as_vec3().to_array()
}

/// The four corners of a cell, counter-clockwise seen from outside the sphere.
fn cell_corners(face: CubeFace, col: u32, row: u32, n: u32) -> [CornerKey; 4] {
    let c = [
        corner_key(face, col, row, n),
        corner_key(face, col + 1, row, n),
        corner_key(face, col + 1, row + 1, n),
        corner_key(face, col, row + 1, n),
    ];
    let d: Vec<Vec3> = c.iter().map(|&k| Vec3::from(key_dir(k))).collect();
    if (d[1] - d[0]).cross(d[2] - d[0]).dot(d[0]) >= 0.0 {
        c
    } else {
        [c[0], c[3], c[2], c[1]]
    }
}

/// Unit-sphere geometry with shared, watertight vertices.
#[derive(Clone, Debug, PartialEq)]
pub struct SphereGeometry {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// A cube-sphere with `n x n` quads per face: `6n² + 2` shared vertices, outward winding.
pub fn cube_sphere(n: u32) -> SphereGeometry {
    let mut vertices: HashMap<CornerKey, u32> = HashMap::new();
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    for face in CubeFace::ALL {
        for row in 0..n {
            for col in 0..n {
                let ids = cell_corners(face, col, row, n).map(|k| {
                    *vertices.entry(k).or_insert_with(|| {
                        positions.push(key_dir(k));
                        positions.len() as u32 - 1
                    })
                });
                indices.extend_from_slice(&[ids[0], ids[1], ids[2], ids[0], ids[2], ids[3]]);
            }
        }
    }
    SphereGeometry { positions, indices }
}

impl SphereGeometry {
    pub fn to_mesh(&self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions.clone())
        .with_inserted_indices(Indices::U32(self.indices.clone()))
    }
}

/// One plate's validity intervals in one footprint cell, as `(younger_myr, older_myr)`.
#[derive(Clone, Debug, PartialEq)]
pub struct IntervalList {
    pub plate: PlateIndex,
    pub intervals: Vec<(f64, f64)>,
}

/// The plate-piece geometry: plate-fixed unit directions with flat per-triangle attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct PlatePieces {
    pub positions: Vec<[f32; 3]>,
    /// Per vertex: `[plate index, cell interval list, corner interval list]`.
    pub pieces: Vec<[u32; 3]>,
    pub indices: Vec<u32>,
    /// Interval lists by id; the GPU gets one validity flag word per list.
    pub lists: Vec<IntervalList>,
}

impl PlatePieces {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// The plate index of triangle `t` (all three vertices agree).
    pub fn triangle_plate(&self, t: usize) -> u32 {
        self.pieces[self.indices[3 * t] as usize][0]
    }

    pub fn to_mesh(&self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::RENDER_WORLD,
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions.clone())
        .with_inserted_attribute(
            ATTRIBUTE_PIECE,
            VertexAttributeValues::Uint32x3(self.pieces.clone()),
        )
        .with_inserted_indices(Indices::U32(self.indices.clone()))
    }
}

/// Sub-quads per footprint cell edge for a cube size: enough that a plate triangle spans at
/// most a 128-per-face lattice cell, so its chord sags well under the ocean shell's depth.
pub fn plate_subdivisions(cube_size: u32) -> u32 {
    128u32.div_ceil(cube_size.max(1)).max(1)
}

/// Builds the plate pieces from the footprint grid, with each cell split into
/// `subdivisions x subdivisions` quads. `ranks[plate]` is the plate's source-ID rank; triangles
/// are ordered by *descending* rank (highest source ID first), then by cell index, so the draw
/// order is stable and the lowest source ID draws last: with [`PIECE_DEPTH_COMPARE`] a later
/// primitive wins an exact depth tie, which matches the CPU rule (ties to the lower source ID).
///
/// Corner decision: for plate `P` at a vertex, the canonical cell is the footprint cell
/// containing the vertex direction under the cube tie rule ([`dir_to_texel`]) when that cell
/// lists `P`; otherwise the lowest-indexed incident cell that lists `P`. Its interval list for
/// `P` decides endpoint availability at that vertex for every duplicate.
pub fn build_plate_pieces(
    footprints: &PlateFootprints,
    ranks: &[u32],
    subdivisions: u32,
) -> PlatePieces {
    let n = footprints.cube_size;
    let k = subdivisions.max(1);
    let fine = n * k;
    let cells = 6 * (n * n) as usize;
    let mut list_ids: HashMap<(u32, Vec<(u64, u64)>), u32> = HashMap::new();
    let mut lists: Vec<IntervalList> = Vec::new();
    // (cell, plate, list id) for every occupied cell and candidate plate.
    let mut occupied: Vec<(usize, u32, u32)> = Vec::new();
    for cell in 0..cells {
        let start = footprints.offsets[cell] as usize;
        let end = footprints.offsets[cell + 1] as usize;
        let entries = &footprints.entries[start..end];
        let mut i = 0;
        while i < entries.len() {
            let plate = entries[i].plate_index;
            let mut intervals = Vec::new();
            while i < entries.len() && entries[i].plate_index == plate {
                intervals.push((entries[i].younger_myr, entries[i].older_myr));
                i += 1;
            }
            let key = (
                plate.0,
                intervals
                    .iter()
                    .map(|&(y, o)| (y.to_bits(), o.to_bits()))
                    .collect::<Vec<_>>(),
            );
            let id = *list_ids.entry(key).or_insert_with(|| {
                lists.push(IntervalList { plate, intervals });
                lists.len() as u32 - 1
            });
            occupied.push((cell, plate.0, id));
        }
    }
    let cell_list: HashMap<(usize, u32), u32> =
        occupied.iter().map(|&(c, p, l)| ((c, p), l)).collect();
    let locate = |cell: usize| {
        let face = CubeFace::ALL[cell / (n * n) as usize];
        let within = cell % (n * n) as usize;
        let (row, col) = ((within / n as usize) as u32, (within % n as usize) as u32);
        (face, col, row)
    };
    // The sub-quads of a cell, each as four fine-lattice corners (outward winding).
    let sub_quads = |cell: usize| {
        let (face, col, row) = locate(cell);
        (0..k).flat_map(move |b| {
            (0..k).map(move |a| cell_corners(face, col * k + a, row * k + b, fine))
        })
    };
    // Incident occupied cells per (plate, vertex), in ascending cell order.
    let mut incident: HashMap<(u32, CornerKey), Vec<usize>> = HashMap::new();
    for &(cell, plate, _) in &occupied {
        for quad in sub_quads(cell) {
            for key in quad {
                let cells = incident.entry((plate, key)).or_default();
                if cells.last() != Some(&cell) && !cells.contains(&cell) {
                    cells.push(cell);
                }
            }
        }
    }
    let corner_list = |plate: u32, key: CornerKey| -> u32 {
        let (face, col, row) = dir_to_texel(RenderDir(key_dir_f64(key)), n);
        let tie_cell = (face as usize * n as usize + row as usize) * n as usize + col as usize;
        let cell = if cell_list.contains_key(&(tie_cell, plate)) {
            tie_cell
        } else {
            incident[&(plate, key)][0]
        };
        cell_list[&(cell, plate)]
    };

    occupied.sort_by_key(|&(cell, plate, _)| (std::cmp::Reverse(ranks[plate as usize]), cell));
    let mut vertices: HashMap<(u32, u32, CornerKey), u32> = HashMap::new();
    let mut corner_lists: HashMap<(u32, CornerKey), u32> = HashMap::new();
    let mut positions = Vec::new();
    let mut pieces = Vec::new();
    let mut indices = Vec::new();
    for &(cell, plate, list) in &occupied {
        for quad in sub_quads(cell) {
            let ids = quad.map(|key| {
                *vertices.entry((plate, list, key)).or_insert_with(|| {
                    let corner = *corner_lists
                        .entry((plate, key))
                        .or_insert_with(|| corner_list(plate, key));
                    positions.push(key_dir(key));
                    pieces.push([plate, list, corner]);
                    positions.len() as u32 - 1
                })
            });
            indices.extend_from_slice(&[ids[0], ids[1], ids[2], ids[0], ids[2], ids[3]]);
        }
    }
    PlatePieces {
        positions,
        pieces,
        indices,
        lists,
    }
}

/// A full-screen triangle in clip space, for the bridge composite.
pub fn fullscreen_triangle() -> Mesh {
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![[-1.0f32, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 1, 2]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use timefly_core::cube::texel_center_dir;
    use timefly_core::geodynamics::FootprintEntry;

    fn v(p: [f32; 3]) -> Vec3 {
        Vec3::from(p)
    }

    /// Every triangle winds counter-clockwise seen from outside.
    fn assert_outward(positions: &[[f32; 3]], indices: &[u32]) {
        for t in indices.chunks(3) {
            let (a, b, c) = (
                v(positions[t[0] as usize]),
                v(positions[t[1] as usize]),
                v(positions[t[2] as usize]),
            );
            assert!(
                (b - a).cross(c - a).dot(a + b + c) > 0.0,
                "inward triangle {t:?}"
            );
        }
    }

    fn assert_unit(positions: &[[f32; 3]]) {
        for p in positions {
            assert!((v(*p).length() - 1.0).abs() < 1e-6, "{p:?}");
        }
    }

    #[test]
    fn cube_sphere_is_watertight_with_outward_winding() {
        for n in [1, 3, 8] {
            let s = cube_sphere(n);
            assert_eq!(s.positions.len(), (6 * n * n + 2) as usize, "n = {n}");
            assert_unit(&s.positions);
            assert_outward(&s.positions, &s.indices);
            let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
            for t in s.indices.chunks(3) {
                for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                    *edges.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
            assert!(
                edges.values().all(|&c| c == 2),
                "every edge has two triangles"
            );
        }
    }

    #[test]
    fn cube_face_edges_share_identical_directions() {
        // Positions are unique: corners shared by faces were merged, not duplicated nearly.
        let s = cube_sphere(8);
        let unique: HashSet<[u32; 3]> = s.positions.iter().map(|p| p.map(f32::to_bits)).collect();
        assert_eq!(unique.len(), s.positions.len());
        // The +X face's right edge is the -Z face's left edge.
        for j in 0..=8 {
            assert_eq!(
                corner_key(CubeFace::PosX, 8, j, 8),
                corner_key(CubeFace::NegZ, 0, j, 8)
            );
        }
        // Cell corners bracket the texel centre the shared convention defines.
        let c = cell_corners(CubeFace::PosY, 2, 5, 8).map(key_dir);
        let mean = c.iter().fold(Vec3::ZERO, |acc, p| acc + v(*p)).normalize();
        let centre = texel_center_dir(CubeFace::PosY, 2, 5, 8).0.as_vec3();
        assert!(mean.angle_between(centre) < 0.01);
    }

    /// Footprints whose cell memberships are `cell(texel centre)`, as `(index, younger, older)`.
    fn footprints(
        n: u32,
        cell: impl Fn(bevy::math::DVec3) -> Vec<(u32, f64, f64)>,
    ) -> PlateFootprints {
        let mut offsets = vec![0];
        let mut entries = Vec::new();
        for face in CubeFace::ALL {
            for row in 0..n {
                for col in 0..n {
                    for (i, y, o) in cell(texel_center_dir(face, col, row, n).0) {
                        entries.push(FootprintEntry {
                            plate_index: PlateIndex(i),
                            younger_myr: y,
                            older_myr: o,
                        });
                    }
                    offsets.push(entries.len() as u32);
                }
            }
        }
        PlateFootprints {
            cube_size: n,
            offsets,
            entries,
        }
    }

    /// Plate 0 over `z > 0.2` (always); plate 1 over `x > 0.2` with a 0-50 Ma interval north of
    /// the equator and 0-540 Ma south of it. The plates overlap where both hold.
    fn two_plates() -> PlateFootprints {
        footprints(8, |d| {
            let mut e = Vec::new();
            if d.z > 0.2 {
                e.push((0, 0.0, 540.0));
            }
            if d.x > 0.2 {
                e.push((1, 0.0, if d.y > 0.0 { 50.0 } else { 540.0 }));
            }
            e
        })
    }

    #[test]
    fn plate_pieces_emit_each_occupied_cell_once_per_plate() {
        let f = two_plates();
        let p = build_plate_pieces(&f, &[1, 0], 1);
        let pairs: usize = (0..f.offsets.len() - 1)
            .map(|c| {
                let cell = &f.entries[f.offsets[c] as usize..f.offsets[c + 1] as usize];
                cell.iter()
                    .map(|e| e.plate_index)
                    .collect::<HashSet<_>>()
                    .len()
            })
            .sum();
        assert_eq!(p.triangle_count(), 2 * pairs);
        assert_unit(&p.positions);
        assert_outward(&p.positions, &p.indices);
        for t in p.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|k| p.pieces[t[k] as usize]);
            assert_eq!(a[0], b[0], "one plate per triangle");
            assert_eq!(a[0], c[0], "one plate per triangle");
            assert_eq!(a[1], b[1], "one interval list per triangle");
            assert_eq!(a[1], c[1], "one interval list per triangle");
            assert_eq!(p.lists[a[1] as usize].plate.0, a[0]);
            assert_eq!(
                p.lists[a[2] as usize].plate.0, a[0],
                "corner list of the same plate"
            );
        }
    }

    #[test]
    fn vertices_are_shared_within_a_list_and_duplicated_across_boundaries() {
        let p = build_plate_pieces(&two_plates(), &[1, 0], 1);
        // Group vertices by direction: a direction used by more than one (plate, list) pair has
        // one vertex per pair, and never two vertices for the same pair.
        let mut by_dir: HashMap<[u32; 3], Vec<(u32, u32)>> = HashMap::new();
        for (pos, piece) in p.positions.iter().zip(&p.pieces) {
            by_dir
                .entry(pos.map(f32::to_bits))
                .or_default()
                .push((piece[0], piece[1]));
        }
        let mut duplicated = 0;
        for owners in by_dir.values() {
            let unique: HashSet<_> = owners.iter().collect();
            assert_eq!(
                unique.len(),
                owners.len(),
                "a (plate, list) pair shares its corner"
            );
            if owners.len() > 1 {
                duplicated += 1;
            }
        }
        assert!(
            duplicated > 0,
            "plate and interval boundaries duplicate corners"
        );
    }

    #[test]
    fn neighbouring_cells_with_different_availability_share_one_corner_decision() {
        // Plate 1's cells on either side of the equator have different interval lists, so the
        // corners on the equator are duplicated; every duplicate must carry the same corner list
        // (the canonical cell's), so displacement agrees across the boundary.
        let p = build_plate_pieces(&two_plates(), &[1, 0], 1);
        let mut corner_decision: HashMap<(u32, [u32; 3]), HashSet<u32>> = HashMap::new();
        let mut cell_lists: HashMap<(u32, [u32; 3]), HashSet<u32>> = HashMap::new();
        for (pos, piece) in p.positions.iter().zip(&p.pieces) {
            let key = (piece[0], pos.map(f32::to_bits));
            corner_decision.entry(key).or_default().insert(piece[2]);
            cell_lists.entry(key).or_default().insert(piece[1]);
        }
        let boundary: Vec<_> = cell_lists
            .iter()
            .filter(|(k, lists)| k.0 == 1 && lists.len() > 1)
            .collect();
        assert!(
            !boundary.is_empty(),
            "the fixture has an interval boundary on plate 1"
        );
        for (key, _) in boundary {
            assert_eq!(corner_decision[key].len(), 1, "one decision at {key:?}");
        }
        assert!(corner_decision.values().all(|d| d.len() == 1));
    }

    #[test]
    fn corner_decision_uses_the_tie_rule_cell() {
        let f = two_plates();
        let p = build_plate_pieces(&f, &[1, 0], 1);
        for (pos, piece) in p.positions.iter().zip(&p.pieces) {
            let dir = RenderDir(Vec3::from(*pos).as_dvec3());
            let list = &p.lists[piece[2] as usize];
            // When the tie-rule cell lists this plate, the corner decision is that cell's list.
            let (face, col, row) = dir_to_texel(dir, 8);
            let cell = (face as usize * 8 + row as usize) * 8 + col as usize;
            let entries = &f.entries[f.offsets[cell] as usize..f.offsets[cell + 1] as usize];
            let own: Vec<(f64, f64)> = entries
                .iter()
                .filter(|e| e.plate_index.0 == piece[0])
                .map(|e| (e.younger_myr, e.older_myr))
                .collect();
            if !own.is_empty() {
                assert_eq!(list.intervals, own);
            }
        }
    }

    #[test]
    fn subdivided_pieces_keep_the_rules_and_hug_the_sphere() {
        let f = two_plates();
        let coarse = build_plate_pieces(&f, &[1, 0], 1);
        let p = build_plate_pieces(&f, &[1, 0], 4);
        assert_eq!(p.triangle_count(), 16 * coarse.triangle_count());
        assert_eq!(p.lists, coarse.lists);
        assert_unit(&p.positions);
        assert_outward(&p.positions, &p.indices);
        let mut decisions: HashMap<(u32, [u32; 3]), HashSet<u32>> = HashMap::new();
        for t in p.indices.chunks(3) {
            let [a, b, c] = [0, 1, 2].map(|i| p.pieces[t[i] as usize]);
            assert!(a[0] == b[0] && a[0] == c[0] && a[1] == b[1] && a[1] == c[1]);
            // Chord sag of a triangle: its centroid stays within 0.2% of the unit sphere.
            let centroid = (v(p.positions[t[0] as usize])
                + v(p.positions[t[1] as usize])
                + v(p.positions[t[2] as usize]))
                / 3.0;
            assert!(centroid.length() > 0.99, "{}", centroid.length());
        }
        for (pos, piece) in p.positions.iter().zip(&p.pieces) {
            decisions
                .entry((piece[0], pos.map(f32::to_bits)))
                .or_default()
                .insert(piece[2]);
        }
        assert!(
            decisions.values().all(|d| d.len() == 1),
            "one decision per vertex"
        );
        assert_eq!(plate_subdivisions(8), 16);
        assert_eq!(plate_subdivisions(128), 1);
        assert_eq!(plate_subdivisions(100), 2);
    }

    #[test]
    fn triangle_order_is_stable_and_by_source_id_rank() {
        let f = two_plates();
        // Plate 1 has the lower source ID (rank 0). Under the pieces' depth compare a later
        // primitive wins exact ties, so plate 1 must draw last.
        assert!(later_draw_wins_ties(PIECE_DEPTH_COMPARE));
        assert!(!later_draw_wins_ties(CompareFunction::Greater));
        let p = build_plate_pieces(&f, &[1, 0], 1);
        let plates: Vec<u32> = (0..p.triangle_count())
            .map(|t| p.triangle_plate(t))
            .collect();
        let first_plate1 = plates.iter().position(|&x| x == 1).unwrap();
        assert!(plates[..first_plate1].iter().all(|&x| x == 0));
        assert!(plates[first_plate1..].iter().all(|&x| x == 1));
        assert_eq!(build_plate_pieces(&f, &[1, 0], 1), p, "deterministic");
    }
}
