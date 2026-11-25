//! The graphs the water pass routes over: the global cube lattice, and a flat grid for fixtures.

use super::HydroGraph;
use crate::cube::{CubeFace, bilinear_taps, dir_to_texel, texel_beyond, texel_center_dir};
use crate::geo::RenderDir;
use crate::worldgen::frame::EARTH_RADIUS_M;

/// Neighbour steps as `(column, row)` offsets, in the order E, NE, N, NW, W, SW, S, SE. Row 0 is
/// north: the top of a cube face, or the first row of a grid.
pub const NEIGHBOUR_STEPS: [(i32, i32); 8] = [
    (1, 0),
    (1, -1),
    (0, -1),
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// The order a cube texel's steps are resolved in: E, N, W, S, then the diagonals. A step that
/// lands on a texel an earlier one reached is `None`, so at a cube corner the duplicate dropped is
/// always the diagonal into the corner.
const RESOLVE_ORDER: [usize; 8] = [0, 2, 4, 6, 1, 3, 5, 7];

/// The global cube-map lattice: `n x n` texels per face under the shared cube convention
/// ([`crate::cube`]). Node `face·n² + row·n + col`, with faces in [`CubeFace::ALL`] order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CubeLattice {
    pub n: u32,
}

impl CubeLattice {
    pub fn node(&self, face: CubeFace, col: u32, row: u32) -> u32 {
        debug_assert!(col < self.n && row < self.n);
        face as u32 * self.n * self.n + row * self.n + col
    }

    pub fn face_col_row(&self, id: u32) -> (CubeFace, u32, u32) {
        let per_face = self.n * self.n;
        let within = id % per_face;
        (
            CubeFace::ALL[(id / per_face) as usize],
            within % self.n,
            within / self.n,
        )
    }

    /// The direction through the node's texel centre.
    pub fn dir(&self, id: u32) -> RenderDir {
        let (face, col, row) = self.face_col_row(id);
        texel_center_dir(face, col, row, self.n)
    }

    /// `√(4πR² / 6n²)`: the side of a square with a texel's mean area.
    pub fn mean_spacing_m(&self) -> f64 {
        let n = f64::from(self.n);
        (4.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M / (6.0 * n * n)).sqrt()
    }

    /// The four nodes a seamless bilinear sample at `dir` blends, with their weights
    /// ([`bilinear_taps`]).
    pub fn taps(&self, dir: RenderDir) -> [(u32, f64); 4] {
        bilinear_taps(dir, self.n).map(|((face, col, row), w)| (self.node(face, col, row), w))
    }

    /// The node whose texel contains `dir` ([`dir_to_texel`]).
    pub fn nearest(&self, dir: RenderDir) -> u32 {
        let (face, col, row) = dir_to_texel(dir, self.n);
        self.node(face, col, row)
    }
}

impl HydroGraph for CubeLattice {
    fn len(&self) -> u32 {
        6 * self.n * self.n
    }

    /// A ±1 step in face coordinates. Off the face, the stepped texel centre's direction picks
    /// the texel ([`texel_beyond`]), the same seam rule as [`bilinear_taps`].
    fn neighbours(&self, i: u32) -> [Option<u32>; 8] {
        let (face, col, row) = self.face_col_row(i);
        let n = self.n;
        let mut out = [None; 8];
        if (1..n - 1).contains(&col) && (1..n - 1).contains(&row) {
            for (slot, (dc, dr)) in out.iter_mut().zip(NEIGHBOUR_STEPS) {
                let c = col.wrapping_add_signed(dc);
                let r = row.wrapping_add_signed(dr);
                *slot = Some(self.node(face, c, r));
            }
            return out;
        }
        for k in RESOLVE_ORDER {
            let (dc, dr) = NEIGHBOUR_STEPS[k];
            let (c, r) = (
                i64::from(col) + i64::from(dc),
                i64::from(row) + i64::from(dr),
            );
            let nf = i64::from(n);
            let j = if (0..nf).contains(&c) && (0..nf).contains(&r) {
                self.node(face, c as u32, r as u32)
            } else {
                let (f, c, r) = texel_beyond(face, c, r, n);
                self.node(f, c, r)
            };
            if j != i && !out.contains(&Some(j)) {
                out[k] = Some(j);
            }
        }
        out
    }

    /// The great-circle distance between the texel centres.
    fn distance_m(&self, i: u32, j: u32) -> f64 {
        let chord = (self.dir(i).0 - self.dir(j).0).length();
        2.0 * (0.5 * chord).asin() * EARTH_RADIUS_M
    }

    /// The texel's exact solid angle × R².
    fn area_m2(&self, i: u32) -> f64 {
        let (_, col, row) = self.face_col_row(i);
        let n = f64::from(self.n);
        let edge = |k: u32| 2.0 * f64::from(k) / n - 1.0;
        // Solid angle of the face-plane rectangle [0, x] × [0, y] (signed), at unit distance.
        let corner = |x: f64, y: f64| (x * y).atan2((1.0 + x * x + y * y).sqrt());
        let (x0, x1, y0, y1) = (edge(col), edge(col + 1), edge(row), edge(row + 1));
        let omega = corner(x1, y1) - corner(x0, y1) - corner(x1, y0) + corner(x0, y0);
        omega * EARTH_RADIUS_M * EARTH_RADIUS_M
    }

    fn mean_spacing_m(&self) -> f64 {
        CubeLattice::mean_spacing_m(self)
    }
}

/// A flat `rows x cols` grid at `spacing_m`, for fixtures and property tests. Row 0 is north;
/// node `row·cols + col`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GridGraph {
    pub rows: u32,
    pub cols: u32,
    pub spacing_m: f64,
}

impl GridGraph {
    pub fn node(&self, row: u32, col: u32) -> u32 {
        debug_assert!(row < self.rows && col < self.cols);
        row * self.cols + col
    }

    pub fn row_col(&self, id: u32) -> (u32, u32) {
        (id / self.cols, id % self.cols)
    }
}

impl HydroGraph for GridGraph {
    fn len(&self) -> u32 {
        self.rows * self.cols
    }

    fn neighbours(&self, i: u32) -> [Option<u32>; 8] {
        let (row, col) = self.row_col(i);
        NEIGHBOUR_STEPS.map(|(dc, dr)| {
            let c = col.checked_add_signed(dc).filter(|&c| c < self.cols)?;
            let r = row.checked_add_signed(dr).filter(|&r| r < self.rows)?;
            Some(self.node(r, c))
        })
    }

    fn distance_m(&self, i: u32, j: u32) -> f64 {
        let ((ri, ci), (rj, cj)) = (self.row_col(i), self.row_col(j));
        let dr = f64::from(ri) - f64::from(rj);
        let dc = f64::from(ci) - f64::from(cj);
        // `sqrt` rather than `hypot`: IEEE-exact, so the Python reference matches bit for bit.
        self.spacing_m * (dr * dr + dc * dc).sqrt()
    }

    fn area_m2(&self, _i: u32) -> f64 {
        self.spacing_m * self.spacing_m
    }

    fn mean_spacing_m(&self) -> f64 {
        self.spacing_m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cube::{CubeFace, bilinear_taps, texel_center_dir};
    use crate::hydrology::HydroGraph;
    use crate::hydrology::params::{HYDRO_DEEP_N, HYDRO_RECENT_N};
    use crate::worldgen::frame::EARTH_RADIUS_M;
    use std::collections::HashSet;

    /// The length of neighbour step `k` in texels: 1 for E, N, W, S and √2 for the diagonals.
    fn step_len(k: usize) -> f64 {
        let (dc, dr) = NEIGHBOUR_STEPS[k];
        f64::from(dc * dc + dr * dr).sqrt()
    }

    #[test]
    fn neighbour_steps_run_e_ne_n_nw_w_sw_s_se() {
        // Row 0 is north (the top of a face), so N is a row step of -1.
        assert_eq!(
            NEIGHBOUR_STEPS,
            [
                (1, 0),
                (1, -1),
                (0, -1),
                (-1, -1),
                (-1, 0),
                (-1, 1),
                (0, 1),
                (1, 1)
            ]
        );
        let g = GridGraph {
            rows: 3,
            cols: 3,
            spacing_m: 1.0,
        };
        let id = |r, c| g.node(r, c);
        let expected = [
            id(1, 2),
            id(0, 2),
            id(0, 1),
            id(0, 0),
            id(1, 0),
            id(2, 0),
            id(2, 1),
            id(2, 2),
        ];
        assert_eq!(g.neighbours(id(1, 1)), expected.map(Some));
        assert_eq!(
            g.neighbours(id(0, 0)),
            [
                Some(id(0, 1)),
                None,
                None,
                None,
                None,
                None,
                Some(id(1, 0)),
                Some(id(1, 1))
            ]
        );
        assert_eq!(g.row_col(id(2, 1)), (2, 1));
        assert!((g.distance_m(id(0, 0), id(1, 1)) - 2f64.sqrt()).abs() < 1e-12);
        assert_eq!(g.area_m2(0), 1.0);
    }

    #[test]
    fn cube_lattice_ids_and_samplers_follow_the_cube_convention() {
        for n in [7, 8] {
            let lattice = CubeLattice { n };
            assert_eq!(lattice.len(), 6 * n * n);
            for (f, face) in CubeFace::ALL.into_iter().enumerate() {
                for (col, row) in [(0, 0), (n - 1, 2), (3, n - 1)] {
                    let id = lattice.node(face, col, row);
                    assert_eq!(id, f as u32 * n * n + row * n + col);
                    assert_eq!(lattice.face_col_row(id), (face, col, row));
                    let dir = lattice.dir(id);
                    assert_eq!(dir, texel_center_dir(face, col, row, n));
                    assert_eq!(lattice.nearest(dir), id);
                }
            }
            // Taps follow `bilinear_taps`, including across a seam.
            let dir = RenderDir(bevy_math::DVec3::new(1.0, 0.3, 0.999).normalize());
            let taps = lattice.taps(dir);
            for (tap, ((face, col, row), w)) in taps.iter().zip(bilinear_taps(dir, n)) {
                assert_eq!(*tap, (lattice.node(face, col, row), w));
            }
        }
    }

    #[test]
    fn lattice_spacings_match_the_plan() {
        let recent = CubeLattice { n: HYDRO_RECENT_N }.mean_spacing_m();
        let deep = CubeLattice { n: HYDRO_DEEP_N }.mean_spacing_m();
        assert_eq!((recent / 100.0).round(), 102.0, "{recent}");
        assert_eq!((deep / 1000.0).round(), 51.0, "{deep}");
        let expected =
            (4.0 * std::f64::consts::PI * EARTH_RADIUS_M.powi(2) / (6.0 * 900.0 * 900.0)).sqrt();
        assert_eq!(recent, expected);
    }

    #[test]
    fn cube_lattice_neighbours_are_symmetric_and_cross_seams() {
        // 8 is the brief's size; 7 is odd and non-dyadic, so f32-style crack bugs can't hide.
        for n in [8, 7] {
            let lattice = CubeLattice { n };
            let mean = lattice.mean_spacing_m();
            let mut corners = 0;
            let (mut lo, mut hi) = (f64::INFINITY, 0.0f64);
            for i in 0..lattice.len() {
                let (face, col, row) = lattice.face_col_row(i);
                let nb = lattice.neighbours(i);
                let present: Vec<u32> = nb.iter().flatten().copied().collect();
                let distinct: HashSet<u32> = present.iter().copied().collect();
                assert_eq!(distinct.len(), present.len(), "duplicate neighbour of {i}");
                assert!(!distinct.contains(&i), "{i} is its own neighbour");
                let corner = (col == 0 || col == n - 1) && (row == 0 || row == n - 1);
                corners += u32::from(corner);
                assert_eq!(
                    present.len(),
                    if corner { 7 } else { 8 },
                    "{face:?} {col} {row}"
                );
                for (k, j) in nb.iter().enumerate() {
                    let Some(j) = *j else { continue };
                    assert!(
                        lattice.neighbours(j).contains(&Some(i)),
                        "{j} is not back to {i}"
                    );
                    let ratio = lattice.distance_m(i, j) / (mean * step_len(k));
                    lo = lo.min(ratio);
                    hi = hi.max(ratio);
                    assert!((0.5..=1.6).contains(&ratio), "{i} -> {j}: {ratio}");
                }
                // Every edge texel reaches across its edge (the hazard the seam rule exists for).
                let edge = col == 0 || col == n - 1 || row == 0 || row == n - 1;
                let crosses = present.iter().any(|&j| lattice.face_col_row(j).0 != face);
                assert_eq!(crosses, edge, "{face:?} {col} {row}");
            }
            assert_eq!(corners, 24);
            println!("n = {n}: distance / (mean spacing x step) in [{lo:.3}, {hi:.3}]");
        }
    }

    #[test]
    fn texel_areas_sum_to_the_sphere() {
        let sphere = 4.0 * std::f64::consts::PI * EARTH_RADIUS_M * EARTH_RADIUS_M;
        for n in [1, 7, 8, 13, 180] {
            let lattice = CubeLattice { n };
            let total: f64 = (0..lattice.len()).map(|i| lattice.area_m2(i)).sum();
            assert!(
                (total / sphere - 1.0).abs() < 1e-9,
                "n = {n}: {total} vs {sphere}"
            );
            assert!((0..lattice.len()).all(|i| lattice.area_m2(i) > 0.0));
        }
        // A corner texel subtends less than a centre texel, as the cube's projection implies.
        let lattice = CubeLattice { n: 8 };
        let corner = lattice.area_m2(lattice.node(CubeFace::PosY, 0, 0));
        let centre = lattice.area_m2(lattice.node(CubeFace::PosY, 3, 3));
        assert!(corner < 0.5 * centre);
    }
}
