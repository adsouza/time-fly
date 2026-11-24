//! The region view: the [`RegionCamera`] and the region's sun, and the switch between them and
//! the globe camera.
//!
//! The region lives in its own world units (1 unit = 1 m around the region centre, design
//! §3) and its own render layer ([`TERRAIN_LAYER`]), so the globe (1 unit = 1 km, layers 0–3)
//! never shows in it. While the terrain scene exists and [`TerrainSettings::visible`] is on, the
//! region camera is active and draws the UI, and the globe camera is inactive (which also stops
//! the globe's offscreen bridge cameras).
//!
//! The camera spawns at Task 7's debug view, [`CAMERA_START_ALTITUDE_M`] above the ground at
//! the region centre, looking north and pitched down [`DEBUG_PITCH_DEG`] (the fly camera's start
//! pose). From then on `crate::fly_camera` moves it; the terrain follows whatever moves the
//! `RegionCamera`'s `Transform`.

use bevy::camera::Exposure;
use bevy::camera::visibility::RenderLayers;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::light::cluster::ClusterConfig;
use bevy::light::{CascadeShadowConfig, CascadeShadowConfigBuilder};
use bevy::prelude::*;
use timefly_core::flight::altitudes::REGION_CLOUD_BASE_M;
use timefly_core::terrain::blended_height_m;
use timefly_core::worldgen::generate::RegionGrid;

use super::{TERRAIN_LAYER, TerrainScene, TerrainSettings};
use crate::cloud::CLOUD_LAYER;
use crate::creature::FLY_LAYER;
use crate::globe::GlobeCamera;
use crate::region::CAMERA_START_ALTITUDE_M;

/// The debug camera's downward pitch.
pub const DEBUG_PITCH_DEG: f32 = 20.0;
/// The region camera's vertical field of view.
pub const REGION_FOV_DEG: f32 = 60.0;
/// Near plane (m). Depth is reversed-Z with an infinite far plane, so 1 m costs little.
pub const REGION_NEAR_M: f32 = 1.0;
/// Far plane (m) up to [`M2_CEILING_M`], used for culling only: beyond the apron's corner seen
/// from its centre. Higher up, [`view_ranges`] pushes it out.
pub const REGION_FAR_M: f32 = 2_000_000.0;
/// Shadow cascades cover the nearest 60 km (M2's ceiling) up to [`M2_CEILING_M`]; higher up,
/// [`view_ranges`] stretches them.
pub const SHADOW_DISTANCE_M: f32 = 60_000.0;
/// The first cascade's far bound.
pub const FIRST_CASCADE_M: f32 = 2_500.0;
/// Shadow cascades.
pub const SHADOW_CASCADES: usize = 4;
/// M2's ceiling: at or below this fly height [`view_ranges`] gives M2's render ranges exactly.
pub const M2_CEILING_M: f32 = 60_000.0;
/// M2's fog end (m): from the camera, where the clearest M2 fog (`fog_visibility_m`'s 460 km)
/// reaches [`FOG_COMPLETE`]. M2's fog is exponential-squared from the eye, so its start is 0.
pub const FOG_END_M: f32 = 665_000.0;
/// The fog fraction that counts as fully fogged: the terrain's own colour keeps under half an
/// 8-bit step.
pub const FOG_COMPLETE: f32 = 0.998;
/// The fog's start at the cloud base (`REGION_CLOUD_BASE_M`) *(call)*: just past the ground under
/// the fly (378 km from the chase camera), so the region below reads clearly from up there.
pub const FOG_START_AT_BASE_M: f32 = 380_000.0;
/// The fog's end at the cloud base *(call)*: inside the nearest the apron's edge comes into view
/// (about 1,160 km; `the_apron_edge_is_never_visible`).
pub const FOG_END_AT_BASE_M: f32 = 1_100_000.0;
/// The far plane grows by this many metres per metre the fly climbs above [`M2_CEILING_M`]
/// *(call)*, so it holds the apron's far corner from every chase camera.
pub const FAR_PER_M: f32 = 1.0;
/// Above [`M2_CEILING_M`] the shadow cascades stretch out to the fog's end over this much
/// climb *(call)*, so their edge sinks into full fog instead of cutting across the view.
pub const SHADOW_STRETCH_M: f32 = 60_000.0;

/// The region view's distances for one fly height ([`view_ranges`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ViewRanges {
    /// The camera's far plane (m), for culling.
    pub far_m: f32,
    /// The shadow cascades' maximum distance (m).
    pub shadow_m: f32,
    /// The distance (m) at which the fog starts: nearer ground is unfogged.
    pub fog_start_m: f32,
    /// The distance (m) at which every era's fog is complete ([`FOG_COMPLETE`]).
    pub fog_end_m: f32,
}

impl ViewRanges {
    /// The factor on the distance past [`ViewRanges::fog_start_m`] that the fog reads, so the
    /// era's M2 fog completes at [`ViewRanges::fog_end_m`]: 1 at M2's heights.
    pub fn fog_scale(&self) -> f32 {
        FOG_END_M / (self.fog_end_m - self.fog_start_m).max(1.0)
    }

    /// The first shadow cascade's far bound: [`FIRST_CASCADE_M`], scaled with the shadow
    /// distance so the cascades keep M2's proportions.
    pub fn first_cascade_m(&self) -> f32 {
        FIRST_CASCADE_M * (self.shadow_m / SHADOW_DISTANCE_M)
    }
}

/// The region view's ranges for a fly `height_m` above sea level. Pure; M2's values exactly
/// at or below [`M2_CEILING_M`]. Higher up:
///
/// - **Fog.** Its start and end move linearly with the height, from M2's (0 and [`FOG_END_M`])
///   at [`M2_CEILING_M`] to [`FOG_START_AT_BASE_M`] and [`FOG_END_AT_BASE_M`] at the cloud base:
///   the near ground clears (seen from high up, it lies under thin air), and the fog still
///   completes before the apron's edge comes into view.
/// - **Shadows** stretch from [`SHADOW_DISTANCE_M`] to the fog's end over [`SHADOW_STRETCH_M`].
/// - **Far plane:** [`REGION_FAR_M`] plus [`FAR_PER_M`] of the climb.
pub fn view_ranges(height_m: f32) -> ViewRanges {
    let above = (height_m - M2_CEILING_M).max(0.0);
    let base = REGION_CLOUD_BASE_M as f32 - M2_CEILING_M;
    let up = above / base;
    let fog_start_m = FOG_START_AT_BASE_M * up;
    let fog_end_m = FOG_END_M + (FOG_END_AT_BASE_M - FOG_END_M) * up;
    let t = (above / SHADOW_STRETCH_M).clamp(0.0, 1.0);
    let stretch = t * t * (3.0 - 2.0 * t);
    ViewRanges {
        far_m: REGION_FAR_M + above * FAR_PER_M,
        shadow_m: SHADOW_DISTANCE_M + (fog_end_m - SHADOW_DISTANCE_M) * stretch,
        fog_start_m,
        fog_end_m,
    }
}

/// The ranges for the fly's current height (M2's without a fly).
pub fn fly_view_ranges(fly: Option<&crate::fly_camera::FlyState>) -> ViewRanges {
    view_ranges(fly.map_or(0.0, |f| f.pos_m.y))
}

/// The sun's cascades for `ranges`: [`SHADOW_CASCADES`] from
/// [`ViewRanges::first_cascade_m`] out to [`ViewRanges::shadow_m`].
pub fn region_cascades(ranges: &ViewRanges) -> CascadeShadowConfig {
    CascadeShadowConfigBuilder {
        num_cascades: SHADOW_CASCADES,
        first_cascade_far_bound: ranges.first_cascade_m(),
        maximum_distance: ranges.shadow_m,
        ..default()
    }
    .build()
}

/// Sets the region camera's far plane and the region sun's cascades from the fly's height
/// ([`view_ranges`]), touching each only when it changes. The fog's start and scale ride in
/// the terrain's buffer (`update_patches`).
pub(super) fn apply_view_ranges(
    fly: Option<Res<crate::fly_camera::FlyState>>,
    mut cameras: Query<&mut Projection, With<RegionCamera>>,
    mut suns: Query<&mut CascadeShadowConfig, With<TerrainSun>>,
) {
    let ranges = fly_view_ranges(fly.as_deref());
    for mut projection in &mut cameras {
        let stale = matches!(&*projection, Projection::Perspective(p) if p.far != ranges.far_m);
        if stale && let Projection::Perspective(p) = &mut *projection {
            p.far = ranges.far_m;
        }
    }
    let cascades = region_cascades(&ranges);
    for mut config in &mut suns {
        if config.bounds != cascades.bounds {
            *config = cascades.clone();
        }
    }
}

/// Direction toward the region's sun: west-south-west, about 26° up, so relief reads across a
/// north-facing view *(call)*.
pub const REGION_SUN_DIRECTION: Vec3 = Vec3::new(-0.72, 0.42, 0.52);
/// The sun's illuminance (lux).
pub const REGION_SUN_LUX: f32 = 7_000.0;
/// The sun's colour: a warm, art-directed afternoon light (spec §9).
pub const REGION_SUN_COLOR: Color = Color::srgb(1.0, 0.93, 0.80);
/// The region camera's exposure (EV100): Bevy's default, pinned so the terrain's light ramp
/// knows what a sunlit surface measures.
pub const REGION_EV100: f32 = Exposure::EV100_BLENDER;
/// The region camera's ambient light (cd/m²): enough to read shadowed slopes. Its colour is
/// the sky's zenith (the sky plugin sets it per era and biome).
pub const REGION_AMBIENT: f32 = 200.0;
/// Clear colour behind the terrain; the sky dome covers it.
pub const REGION_SKY: Color = Color::srgb(0.55, 0.68, 0.82);
/// Camera order: above the globe camera's, though only one of them is active at a time.
pub const REGION_CAMERA_ORDER: isize = 1;

/// What a surface facing the region's sun receives after exposure (the diffuse term's
/// `illuminance × exposure / π`): the unit the terrain's light ramp measures in.
pub fn full_sun_illumination() -> f32 {
    REGION_SUN_LUX
        * Exposure {
            ev100: REGION_EV100,
        }
        .exposure()
        / core::f32::consts::PI
}

/// The camera that views a region. Spawned with the terrain scene, inactive until the view
/// switches to it.
#[derive(Component, Copy, Clone, Debug, Default)]
pub struct RegionCamera;

/// The region's directional light (shadow-casting, on [`TERRAIN_LAYER`]). The globe's own sun
/// is a different entity, with `crate::globe::GlobeSun`.
#[derive(Component, Copy, Clone, Debug, Default)]
pub struct TerrainSun;

/// The debug camera over a grid: [`CAMERA_START_ALTITUDE_M`] above the higher of the ground and
/// the sea at the centre, looking north (−z), pitched down [`DEBUG_PITCH_DEG`].
pub fn debug_camera_transform(grid: &RegionGrid) -> Transform {
    let ground = blended_height_m(grid, 0.0, 0.0).max(0.0);
    Transform::from_xyz(0.0, ground + CAMERA_START_ALTITUDE_M, 0.0)
        .with_rotation(Quat::from_rotation_x(-DEBUG_PITCH_DEG.to_radians()))
}

/// The region camera's `Camera`: inactive until the view switches to it.
pub(super) fn region_camera() -> Camera {
    Camera {
        is_active: false,
        order: REGION_CAMERA_ORDER,
        clear_color: ClearColorConfig::Custom(REGION_SKY),
        ..default()
    }
}

/// Everything on the region camera but its `Camera`, marker and layers: what decides its
/// render pipelines' keys (with the window it draws to), so the pipeline warm-up
/// (`super::warmup`) builds its camera from the same parts.
pub(super) fn region_camera_parts(grid: &RegionGrid) -> impl Bundle {
    (
        Camera3d::default(),
        // No clustered lights in a region (one directional sun); on WebGPU Bevy 0.20 re-creates
        // the cluster buffers every frame and they are never destroyed (ruling R28).
        ClusterConfig::None,
        Projection::Perspective(PerspectiveProjection {
            fov: REGION_FOV_DEG.to_radians(),
            near: REGION_NEAR_M,
            far: REGION_FAR_M,
            near_clip_plane: Vec4::new(0.0, 0.0, -1.0, -REGION_NEAR_M),
            ..default()
        }),
        // A LUT-free tonemapper: the LUT ones need a feature this build may not carry.
        Tonemapping::AcesFitted,
        Exposure {
            ev100: REGION_EV100,
        },
        AmbientLight {
            color: REGION_SKY,
            brightness: REGION_AMBIENT,
            ..default()
        },
        debug_camera_transform(grid),
    )
}

/// The region's directional light.
pub(super) fn region_sun() -> DirectionalLight {
    DirectionalLight {
        illuminance: REGION_SUN_LUX,
        color: REGION_SUN_COLOR,
        shadow_maps_enabled: true,
        ..default()
    }
}

/// The region's sun without its marker and layers (shared with the pipeline warm-up).
pub(super) fn region_sun_parts() -> impl Bundle {
    (
        region_sun(),
        region_cascades(&view_ranges(0.0)),
        Transform::from_translation(REGION_SUN_DIRECTION.normalize())
            .looking_at(Vec3::ZERO, Vec3::Y),
    )
}

/// The region camera's layers: the terrain's, the dragonfly's and the transition cloud's.
pub fn region_view_layers() -> RenderLayers {
    RenderLayers::from_layers(&[TERRAIN_LAYER, FLY_LAYER, CLOUD_LAYER])
}

/// The region sun's layers: the terrain's only. Bevy lists a directional light in every view
/// whose layers meet its own and lights every mesh in that view with it, so the sun still
/// lights the dragonfly in the region's view, but stays out of the globe's (which also sees
/// [`FLY_LAYER`]), where the globe's sun alone lights the fly.
pub fn region_sun_layers() -> RenderLayers {
    RenderLayers::layer(TERRAIN_LAYER)
}

/// Spawns the region camera (inactive) and the region's sun; returns `(camera, sun)`.
pub(super) fn spawn_region_view(commands: &mut Commands, grid: &RegionGrid) -> (Entity, Entity) {
    let camera = commands
        .spawn((
            Name::new("Region camera"),
            RegionCamera,
            region_camera(),
            region_camera_parts(grid),
            region_view_layers(),
        ))
        .id();
    let sun = commands
        .spawn((
            Name::new("Region sun"),
            TerrainSun,
            region_sun_parts(),
            region_sun_layers(),
        ))
        .id();
    debug!(?camera, ?sun, "region camera and sun spawned");
    (camera, sun)
}

/// Whether the region camera should replace the globe camera.
pub fn region_view_wanted(scene_exists: bool, settings: &TerrainSettings) -> bool {
    scene_exists && settings.visible
}

/// Activates exactly one of the globe and region cameras, and gives it the UI.
#[expect(clippy::type_complexity, reason = "camera queries")]
pub(super) fn sync_region_view(
    mut commands: Commands,
    scene: Option<Res<TerrainScene>>,
    settings: Res<TerrainSettings>,
    mut globe: Query<
        (Entity, &mut Camera, Has<IsDefaultUiCamera>),
        (With<GlobeCamera>, Without<RegionCamera>),
    >,
    mut region: Query<
        (Entity, &mut Camera, Has<IsDefaultUiCamera>),
        (With<RegionCamera>, Without<GlobeCamera>),
    >,
) {
    let wanted = region_view_wanted(scene.is_some(), &settings) && !region.is_empty();
    let mut switch = |entity: Entity, camera: &mut Camera, has_ui: bool, active: bool| {
        if camera.is_active != active {
            camera.is_active = active;
        }
        if active && !has_ui {
            commands.entity(entity).insert(IsDefaultUiCamera);
        } else if !active && has_ui {
            commands.entity(entity).remove::<IsDefaultUiCamera>();
        }
    };
    let mut changed = false;
    for (entity, mut camera, has_ui) in &mut region {
        changed |= camera.is_active != wanted;
        switch(entity, &mut camera, has_ui, wanted);
    }
    for (entity, mut camera, has_ui) in &mut globe {
        switch(entity, &mut camera, has_ui, !wanted);
    }
    if changed {
        info!(region_view = wanted, "view switched");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_view_needs_a_scene_and_visibility() {
        let on = TerrainSettings::default();
        let off = TerrainSettings {
            visible: false,
            ..on
        };
        assert!(region_view_wanted(true, &on));
        assert!(!region_view_wanted(false, &on));
        assert!(!region_view_wanted(true, &off));
    }

    #[test]
    fn the_region_camera_sees_the_dragonfly_and_one_sun_lights_it_in_each_world() {
        let camera = region_view_layers();
        assert!(camera.intersects(&RenderLayers::layer(TERRAIN_LAYER)));
        assert!(camera.intersects(&RenderLayers::layer(FLY_LAYER)));
        assert!(camera.intersects(&RenderLayers::layer(CLOUD_LAYER)));
        // The region sun is in the region's view only; the globe's sun (default layer 0) in
        // the globe's only. The globe camera's layers, in every mode, include the fly's and the
        // cloud's.
        let sun = region_sun_layers();
        let globe_sun = RenderLayers::default();
        assert!(sun.intersects(&camera) && !globe_sun.intersects(&camera));
        use crate::globe::{COMPOSITE_LAYER, DEEP_LAYER, RECENT_LAYER, globe_view_layers};
        for layer in [RECENT_LAYER, DEEP_LAYER, COMPOSITE_LAYER] {
            let globe = globe_view_layers(layer);
            assert!(globe.intersects(&RenderLayers::layer(FLY_LAYER)), "{layer}");
            assert!(
                globe.intersects(&RenderLayers::layer(CLOUD_LAYER)),
                "{layer}"
            );
            assert!(globe_sun.intersects(&globe), "{layer}");
            assert!(!sun.intersects(&globe), "{layer}");
        }
    }
}
