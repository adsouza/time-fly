//! The region's sea (M2 design §6.5, spec §7.5): a plane at sea level (`y = 0`), curved like the
//! terrain, out to the apron's edge.
//!
//! [`WaterMaterial`] extends `StandardMaterial`, so the sea takes the region sun's light, the
//! terrain's shadows, the fog and the tonemapping exactly as the land does. Its extension binds
//! the terrain's own patch buffer (for the camera and the field layout) and height textures, and
//! `water.wesl` (importing `package::terrain_cdlod`, the WESL copy of `blended_height_m`):
//! - **Vertex:** a polar mesh centred on the camera ([`water_mesh`]), dropped by `d² / 2R` like
//!   the terrain, then pulled toward the camera along the view ray by [`WATER_DEPTH_PULL`] of
//!   the distance (plus [`WATER_PULL_M`]). That changes only depth, never the pixel, so the sea
//!   wins depth ties against a terrain mesh lying near `y = 0` instead of z-fighting with it.
//! - **Fragment:** the sea-floor depth is `−blended_height` at the fragment's surface point.
//!   Fragments over land (depth ≤ 0) are discarded, so the shoreline is the height field's own
//!   zero contour, per pixel. Then the depth tint ([`water_tint`]), foam where `0 < depth < 4 m`
//!   broken up by `noise.wesl`, Bevy PBR lighting, and a Schlick Fresnel blend (f0 = 0.02) toward
//!   the sky's colour along the reflected ray.
//!
//! The camera never goes below the sea (Task 9's clearance), so there is no underwater view.
//! The water is opaque: the depth tint already stands for the floor beneath it.

use std::sync::Arc;

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use timefly_core::worldgen::generate::RegionGrid;

use crate::sky::{region_look, sky_tint};
use crate::terrain::{TERRAIN_LAYER, TerrainScene, TerrainSceneSystems};

/// `water.wesl`: the sea's vertex and fragment shaders.
pub const WATER_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e31");

/// The sea's fraction of the view distance by which it is pulled toward the camera (depth only).
pub const WATER_DEPTH_PULL: f32 = 6.0e-4;
/// The constant part of the pull (m).
pub const WATER_PULL_M: f32 = 1.0;
/// Foam lives where the sea floor is less than this deep (m).
pub const FOAM_DEPTH_M: f32 = 4.0;
/// Schlick's reflectance at normal incidence for water.
pub const WATER_F0: f32 = 0.02;
/// The water mesh's outer radius (m): past the apron's corner from its centre.
pub const WATER_RADIUS_M: f32 = 1_750_000.0;
/// Segments around the water mesh.
pub const WATER_SEGMENTS: u32 = 256;
/// The water mesh's ring spacing: this many metres near the camera, growing to
/// [`WATER_RING_GROWTH`] of the radius farther out.
pub const WATER_RING_M: f32 = 25.0;
pub const WATER_RING_GROWTH: f32 = 0.04;

/// Water colour right at the shore (sRGB): a clear turquoise over sand.
pub const WATER_SHALLOW: Color = Color::srgb(0.27, 0.66, 0.64);
/// Water colour over the deep sea floor (sRGB): a dark navy.
pub const WATER_DEEP: Color = Color::srgb(0.035, 0.14, 0.26);
/// Water colour right at the shore as seen from altitude (sRGB): the shallow turquoise, calmed
/// to a sea teal so the shoreline does not glow as a bright rim from the air.
pub const WATER_SHALLOW_FAR: Color = Color::srgb(0.07, 0.27, 0.36);
/// Camera height above the sea (m) up to which the shallow tint is the clear-water one.
pub const WATER_CALM_FROM_M: f32 = 300.0;
/// Camera height above the sea (m) from which the shallow tint is fully the calmed one.
pub const WATER_CALM_TO_M: f32 = 3000.0;
/// The depth (m) over which the shallow colour gives way to the deep one, as `1 − e^(−d/scale)`.
pub const WATER_DEPTH_SCALE_M: f32 = 28.0;

/// The water's own colour over a sea floor `depth_m` below the surface, before lighting: the
/// shallow colour at 0 m, darkening monotonically toward the deep colour. Mirrored by
/// `water_tint` in `water.wesl` (from the same constants, through [`WaterLook`]).
pub fn water_tint(depth_m: f32) -> Color {
    let t = 1.0 - (-depth_m.max(0.0) / WATER_DEPTH_SCALE_M).exp();
    Color::LinearRgba(WATER_SHALLOW.to_linear().mix(&WATER_DEEP.to_linear(), t))
}

/// The sea's look uniform (`WaterLook` in `water.wesl`).
#[derive(ShaderType, Copy, Clone, Debug, PartialEq, Reflect)]
pub struct WaterLook {
    /// [`WATER_SHALLOW`], linear; `w`: [`WATER_DEPTH_SCALE_M`].
    pub shallow: Vec4,
    /// [`WATER_DEEP`], linear; `w`: [`FOAM_DEPTH_M`].
    pub deep: Vec4,
    /// The sky's zenith (linear) for the Fresnel reflection; `w`: [`WATER_F0`].
    pub sky_zenith: Vec4,
    /// The sky's horizon (linear); `w`: [`WATER_DEPTH_PULL`].
    pub sky_horizon: Vec4,
    /// `x`: [`WATER_PULL_M`]; `y`: [`WATER_CALM_FROM_M`]; `z`: [`WATER_CALM_TO_M`]; `w`: reserved.
    pub pull: Vec4,
    /// [`WATER_SHALLOW_FAR`], linear: the shallow colour from altitude (`w` reserved).
    pub shallow_far: Vec4,
}

/// The look for a region: the constants above and the region's sky.
pub fn water_look(grid: &RegionGrid) -> WaterLook {
    let (era, biome) = region_look(grid);
    let (zenith, horizon, _) = sky_tint(era, biome);
    let v = |c: Color| Vec3::from_slice(&c.to_linear().to_f32_array()[..3]);
    WaterLook {
        shallow: v(WATER_SHALLOW).extend(WATER_DEPTH_SCALE_M),
        deep: v(WATER_DEEP).extend(FOAM_DEPTH_M),
        sky_zenith: v(zenith).extend(WATER_F0),
        sky_horizon: v(horizon).extend(WATER_DEPTH_PULL),
        pull: Vec4::new(WATER_PULL_M, WATER_CALM_FROM_M, WATER_CALM_TO_M, 0.0),
        shallow_far: v(WATER_SHALLOW_FAR).extend(0.0),
    }
}

/// The ring radii of [`water_mesh`], from 0 out to [`WATER_RADIUS_M`].
pub fn water_rings() -> Vec<f32> {
    let mut rings = vec![0.0f32];
    let mut r = 0.0f32;
    while r < WATER_RADIUS_M {
        r = (r + WATER_RING_M.max(r * WATER_RING_GROWTH)).min(WATER_RADIUS_M);
        rings.push(r);
    }
    rings
}

/// The sea's mesh: a position-only polar grid in metres from the camera (the shader adds the
/// camera's xz and the curvature drop): a centre vertex, then [`water_rings`] of
/// [`WATER_SEGMENTS`] vertices, wound to face `+y`.
pub fn water_mesh() -> Mesh {
    let rings = water_rings();
    let seg = WATER_SEGMENTS;
    let mut positions: Vec<[f32; 3]> = vec![[0.0; 3]];
    for &r in &rings[1..] {
        for k in 0..seg {
            let a = k as f32 / seg as f32 * core::f32::consts::TAU;
            positions.push([r * a.cos(), 0.0, r * a.sin()]);
        }
    }
    let at = |ring: u32, k: u32| 1 + (ring - 1) * seg + k % seg;
    let mut indices = Vec::new();
    for k in 0..seg {
        indices.extend([0, at(1, k + 1), at(1, k)]);
    }
    for ring in 1..rings.len() as u32 - 1 {
        for k in 0..seg {
            let (a, b) = (at(ring, k), at(ring, k + 1));
            let (c, d) = (at(ring + 1, k), at(ring + 1, k + 1));
            indices.extend([a, b, d, a, d, c]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

/// The sea's material.
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// The sea's extension of `StandardMaterial`: the terrain's patch buffer and height textures
/// (shared, never copied) and its look.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct WaterExtension {
    #[storage(100, read_only)]
    pub patches: Handle<ShaderBuffer>,
    #[texture(101, sample_type = "float", filterable = false)]
    pub inner_height: Handle<Image>,
    #[texture(102, sample_type = "float", filterable = false)]
    pub apron_height: Handle<Image>,
    #[uniform(103)]
    pub look: WaterLook,
}

impl MaterialExtension for WaterExtension {
    fn vertex_shader() -> ShaderRef {
        WATER_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        WATER_SHADER.into()
    }
    // The sea neither casts shadows nor sits in a prepass (it pulls its depth toward the camera
    // in the main pass only).
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
}

/// The sea's material over the terrain's patch buffer and height textures (shared with the
/// pipeline warm-up, `crate::terrain::warmup`, so its pipeline keys match).
pub fn water_material(
    patches: &Handle<ShaderBuffer>,
    inner_height: &Handle<Image>,
    apron_height: &Handle<Image>,
    look: WaterLook,
) -> WaterMaterial {
    WaterMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.12,
            // f0 = 0.16 × reflectance² = 0.02 (WATER_F0).
            reflectance: (WATER_F0 / 0.16).sqrt(),
            ..default()
        },
        extension: WaterExtension {
            patches: patches.clone(),
            inner_height: inner_height.clone(),
            apron_height: apron_height.clone(),
            look,
        },
    }
}

/// The sea for the current region (present while its entity exists).
#[derive(Resource, Debug)]
pub struct RegionWater {
    /// The grid whose heights it reads.
    pub grid: Arc<RegionGrid>,
    pub entity: Entity,
    pub material: Handle<WaterMaterial>,
}

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        let water = Shader::from_wesl(
            include_str!("../assets/shaders/water.wesl"),
            "embedded://timefly/water.wesl",
        );
        let _ = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(WATER_SHADER.id(), water);
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default());
        add_water_systems(app);
    }
}

/// The systems without the render plugin and shader, for headless tests.
pub(crate) fn add_water_systems(app: &mut App) {
    app.add_systems(Update, sync_water.after(TerrainSceneSystems));
}

/// Gathers the scene and the sea, decides (the sky's rule: build, rebind on a replaced grid,
/// remove), and applies it.
fn sync_water(
    mut commands: Commands,
    scene: Option<Res<TerrainScene>>,
    water: Option<ResMut<RegionWater>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
) {
    use crate::sky::{SkyAction, sky_action};
    let action = sky_action(
        scene.as_ref().map(|s| &s.grid),
        water.as_ref().map(|w| &w.grid),
    );
    match (action, scene, water) {
        (SkyAction::Build, Some(scene), _) => {
            let material = materials.add(water_material(
                &scene.buffer,
                &scene.inner_height,
                &scene.apron_height,
                water_look(&scene.grid),
            ));
            let entity = commands
                .spawn((
                    Name::new("Region sea"),
                    Mesh3d(meshes.add(water_mesh())),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    NoFrustumCulling,
                    RenderLayers::layer(TERRAIN_LAYER),
                ))
                .id();
            info!("region sea built");
            commands.insert_resource(RegionWater {
                grid: scene.grid.clone(),
                entity,
                material,
            });
        }
        (SkyAction::Retint, Some(scene), Some(mut water)) => {
            if let Some(mut m) = materials.get_mut(&water.material) {
                m.extension.inner_height = scene.inner_height.clone();
                m.extension.apron_height = scene.apron_height.clone();
                m.extension.look = water_look(&scene.grid);
            }
            water.grid = scene.grid.clone();
            info!("region sea rebound to the replaced grid");
        }
        (SkyAction::Teardown, _, Some(water)) => {
            commands.entity(water.entity).despawn();
            commands.remove_resource::<RegionWater>();
            info!("region sea removed");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn water_mesh_reaches_the_horizon_and_faces_up() {
        let rings = water_rings();
        assert_eq!(rings[0], 0.0);
        assert_eq!(*rings.last().unwrap(), WATER_RADIUS_M);
        // Beyond the apron's corner, so the sea covers every apron texel.
        assert!(WATER_RADIUS_M > 1_200_000.0 * 2f32.sqrt());
        for w in rings.windows(2) {
            let step = w[1] - w[0];
            assert!(step > 0.0);
            // f32 rounding of the running sum: an ulp or two at the outer radius.
            assert!(step <= WATER_RING_M.max(w[0] * WATER_RING_GROWTH) + 1e-6 * w[1] + 1e-3);
        }
        // The pull hides the mesh's chord sag: radially and around each ring.
        for &r in &rings {
            let step = WATER_RING_M.max(r * WATER_RING_GROWTH);
            let radial_sag = step * step / (8.0 * 6_371_000.0);
            let angle = core::f32::consts::PI / WATER_SEGMENTS as f32;
            let chord_in = r * (1.0 - angle.cos());
            let tangential_sag = r * chord_in / 6_371_000.0;
            let pull = WATER_PULL_M + WATER_DEPTH_PULL * r;
            assert!(radial_sag.max(tangential_sag) < pull, "ring {r}");
        }
        let mesh = water_mesh();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        assert_eq!(p.len(), 1 + (rings.len() - 1) * WATER_SEGMENTS as usize);
        let Some(Indices::U32(idx)) = mesh.indices() else {
            panic!("indices");
        };
        for t in idx.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(p[i as usize]));
            assert!((b - a).cross(c - a).y > 0.0, "a triangle faces down");
        }
    }

    #[test]
    fn water_depth_tint_is_monotonic() {
        let lum = |d: f32| water_tint(d).luminance();
        assert_eq!(water_tint(0.0).to_linear(), WATER_SHALLOW.to_linear());
        let mut last = lum(0.0);
        for step in 1..=400 {
            let d = step as f32 * 0.5;
            let l = lum(d);
            assert!(l < last, "{d} m: {l} not darker than {last}");
            last = l;
        }
        // Most of the way to the deep colour by 200 m, and never past it.
        let deep = WATER_DEEP.luminance();
        assert!(lum(200.0) - deep < 0.01 * (lum(0.0) - deep));
        assert!(lum(10_000.0) >= deep - 1e-6);
        // Negative depth (land above the plane) clamps to the shore colour.
        assert_eq!(water_tint(-5.0).to_linear(), WATER_SHALLOW.to_linear());
    }
}
