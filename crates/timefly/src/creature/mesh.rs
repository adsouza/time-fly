//! The dragonfly's low-poly mesh, generated in code (M3 design §5.1): it is always seen from
//! well behind and above, so a slim tapered body and four flat wings read as a dragonfly.
//!
//! Model axes: the body runs along `z` (the head toward `−z`, Bevy's forward), the wings spread
//! along `±x` and `+y` is up. The body is 1 unit long, so the drawn scale is the body length
//! (`timefly_core::flight::chase::fly_scale`). Every vertex carries [`ATTRIBUTE_WING`] (`0` on
//! the body, `1` on a wing) for the shader's flap, and a vertex colour: iridescent blue-green.

use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, MeshVertexAttribute, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::VertexFormat;

/// Per-vertex part: `0` on the body, `1` on a wing (the shader flaps only the wings).
pub const ATTRIBUTE_WING: MeshVertexAttribute =
    MeshVertexAttribute::new("TimeflyWing", 0x7f1e_0a01, VertexFormat::Float32);

/// The body's rings, head to tail: `(z, radius)`. Eight rings, seven segments: a round head,
/// the thick thorax the wings attach to, then the long tapering abdomen.
const BODY_RINGS: [(f32, f32); 8] = [
    (-0.50, 0.012),
    (-0.46, 0.048),
    (-0.40, 0.040),
    (-0.31, 0.058),
    (-0.18, 0.040),
    (0.02, 0.026),
    (0.28, 0.020),
    (0.50, 0.008),
];
/// Sides of each body ring.
const BODY_SIDES: u32 = 6;
/// The wings' height above the body axis.
const WING_Y: f32 = 0.03;
/// Each right wing as `[root front, root back, tip back, tip front]` in `(x, z)`; the left ones
/// mirror them. Fore and hind wings meet at the thorax, each about half the body long.
const WINGS: [[[f32; 2]; 4]; 2] = [
    [[0.03, -0.35], [0.03, -0.28], [0.56, -0.24], [0.57, -0.30]],
    [[0.03, -0.26], [0.03, -0.17], [0.52, -0.16], [0.54, -0.22]],
];

/// One part of the dragonfly, as plain vertex arrays.
#[derive(Default)]
struct Part {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    colors: Vec<[f32; 4]>,
    wing: Vec<f32>,
    indices: Vec<u32>,
}

impl Part {
    fn mesh(self) -> Mesh {
        Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, self.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, self.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, self.colors)
        .with_inserted_attribute(ATTRIBUTE_WING, self.wing)
        .with_inserted_indices(Indices::U32(self.indices))
    }
}

/// Linear RGB, iridescent: green toward the head, blue toward the tail, with darker bands on
/// alternate abdomen segments.
fn body_color(ring: usize) -> [f32; 4] {
    let t = ring as f32 / (BODY_RINGS.len() - 1) as f32;
    let green = Vec3::new(0.03, 0.32, 0.20);
    let blue = Vec3::new(0.02, 0.12, 0.42);
    let band = if ring >= 4 && ring % 2 == 1 { 0.6 } else { 1.0 };
    let c = green.lerp(blue, t) * band;
    [c.x, c.y, c.z, 1.0]
}

/// The body: rings of [`BODY_SIDES`] around `z`, joined into a closed-ended tube.
fn body() -> Part {
    let mut part = Part::default();
    for (ring, &(z, r)) in BODY_RINGS.iter().enumerate() {
        for k in 0..BODY_SIDES {
            let a = k as f32 / BODY_SIDES as f32 * core::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            part.positions.push([r * c, r * s, z]);
            part.normals.push([c, s, 0.0]);
            part.colors.push(body_color(ring));
            part.wing.push(0.0);
        }
    }
    let at = |ring: u32, k: u32| ring * BODY_SIDES + k % BODY_SIDES;
    for ring in 0..BODY_RINGS.len() as u32 - 1 {
        for k in 0..BODY_SIDES {
            let (a, b) = (at(ring, k), at(ring, k + 1));
            let (c, d) = (at(ring + 1, k), at(ring + 1, k + 1));
            // Outward-facing (counter-clockwise seen from outside).
            part.indices.extend([a, b, d, a, d, c]);
        }
    }
    // Caps: a fan over the head and tail rings.
    let last = BODY_RINGS.len() as u32 - 1;
    for k in 1..BODY_SIDES - 1 {
        part.indices.extend([at(0, 0), at(0, k + 1), at(0, k)]);
        part.indices
            .extend([at(last, 0), at(last, k), at(last, k + 1)]);
    }
    part
}

/// The four wings: flat quads with their own vertices (so each is its own piece), pale and
/// iridescent, blue-green at the root shading to blue at the tip.
fn wings() -> Part {
    let mut part = Part::default();
    let root = [0.30, 0.62, 0.55, 1.0];
    let tip = [0.35, 0.55, 0.85, 1.0];
    for side in [1.0f32, -1.0] {
        for wing in WINGS {
            let base = part.positions.len() as u32;
            for (i, [x, z]) in wing.into_iter().enumerate() {
                part.positions.push([side * x, WING_Y, z]);
                part.normals.push([0.0, 1.0, 0.0]);
                part.colors.push(if i < 2 { root } else { tip });
                part.wing.push(1.0);
            }
            // Facing up on both sides of the body (the material draws both faces).
            let quad = if side > 0.0 {
                [0, 1, 2, 0, 2, 3]
            } else {
                [0, 2, 1, 0, 3, 2]
            };
            part.indices.extend(quad.map(|i| base + i));
        }
    }
    part
}

/// The dragonfly's body alone (drawn opaque).
pub fn dragonfly_body_mesh() -> Mesh {
    body().mesh()
}

/// The dragonfly's four wings alone (drawn translucent).
pub fn dragonfly_wings_mesh() -> Mesh {
    wings().mesh()
}

/// The whole dragonfly: the body and its four wings in one mesh.
pub fn dragonfly_mesh() -> Mesh {
    let mut mesh = dragonfly_body_mesh();
    mesh.merge(&dragonfly_wings_mesh())
        .expect("the body and wings share their attributes");
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::VertexAttributeValues;

    fn wings(mesh: &Mesh) -> Vec<f32> {
        match mesh.attribute(ATTRIBUTE_WING) {
            Some(VertexAttributeValues::Float32(w)) => w.clone(),
            other => panic!("no wing attribute: {other:?}"),
        }
    }

    fn positions(mesh: &Mesh) -> Vec<[f32; 3]> {
        mesh.attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| a.as_float3())
            .expect("positions")
            .to_vec()
    }

    fn indices(mesh: &Mesh) -> Vec<usize> {
        mesh.indices().expect("indices").iter().collect()
    }

    /// The number of connected components among the triangles whose vertices are all wing
    /// vertices (triangles connect through shared vertex indices).
    fn wing_components(mesh: &Mesh) -> usize {
        let wing = wings(mesh);
        let mut parent: Vec<usize> = (0..wing.len()).collect();
        fn root(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        let mut used = std::collections::BTreeSet::new();
        for tri in indices(mesh).chunks(3) {
            if tri.iter().all(|&i| wing[i] > 0.5) {
                for &i in tri {
                    used.insert(i);
                }
                let a = root(&mut parent, tri[0]);
                for &i in &tri[1..] {
                    let b = root(&mut parent, i);
                    parent[b] = a;
                }
            }
        }
        let roots: std::collections::BTreeSet<usize> =
            used.iter().map(|&i| root(&mut parent, i)).collect();
        roots.len()
    }

    #[test]
    fn the_dragonfly_mesh_has_a_body_and_four_wings() {
        let mesh = dragonfly_mesh();
        let wing = wings(&mesh);
        let p = positions(&mesh);
        assert_eq!(wing.len(), p.len(), "one wing value per vertex");
        assert!(wing.iter().all(|&w| w == 0.0 || w == 1.0), "{wing:?}");
        let wing_vertices = wing.iter().filter(|&&w| w == 1.0).count();
        assert!(wing_vertices > 0, "no wing vertices");
        assert_eq!(wing_components(&mesh), 4, "four separate wings");
        // The body runs along z (forward is −z), 1 unit long.
        let body_z: Vec<f32> = p
            .iter()
            .zip(&wing)
            .filter(|(_, w)| **w == 0.0)
            .map(|(p, _)| p[2])
            .collect();
        assert!(!body_z.is_empty(), "no body vertices");
        let length = body_z.iter().copied().fold(f32::MIN, f32::max)
            - body_z.iter().copied().fold(f32::MAX, f32::min);
        assert!((length - 1.0).abs() <= 0.05, "body length {length}");
        // Every triangle is wholly body or wholly wing, so the shader flaps whole wings.
        for tri in indices(&mesh).chunks(3) {
            let parts: Vec<f32> = tri.iter().map(|&i| wing[i]).collect();
            assert!(parts.iter().all(|&w| w == parts[0]), "{tri:?} mixes parts");
        }
        // Lit and coloured: normals and vertex colours on every vertex.
        assert_eq!(
            mesh.attribute(Mesh::ATTRIBUTE_NORMAL).map(|a| a.len()),
            Some(p.len())
        );
        assert_eq!(
            mesh.attribute(Mesh::ATTRIBUTE_COLOR).map(|a| a.len()),
            Some(p.len())
        );
    }
}
