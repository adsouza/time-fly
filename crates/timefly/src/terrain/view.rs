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
use bevy::light::CascadeShadowConfigBuilder;
use bevy::prelude::*;
use timefly_core::terrain::blended_height_m;
use timefly_core::worldgen::generate::RegionGrid;

use super::{TERRAIN_LAYER, TerrainScene, TerrainSettings};
use crate::globe::GlobeCamera;
use crate::region::CAMERA_START_ALTITUDE_M;

/// The debug camera's downward pitch.
pub const DEBUG_PITCH_DEG: f32 = 20.0;
/// The region camera's vertical field of view.
pub const REGION_FOV_DEG: f32 = 60.0;
/// Near plane (m). Depth is reversed-Z with an infinite far plane, so 1 m costs little.
pub const REGION_NEAR_M: f32 = 1.0;
/// Far plane (m), used for culling only: beyond the apron's corner seen from its centre.
pub const REGION_FAR_M: f32 = 2_000_000.0;
/// Shadow cascades cover the nearest 60 km (the camera's ceiling).
pub const SHADOW_DISTANCE_M: f32 = 60_000.0;
/// The first cascade's far bound.
pub const FIRST_CASCADE_M: f32 = 2_500.0;
/// Shadow cascades.
pub const SHADOW_CASCADES: usize = 4;
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
        CascadeShadowConfigBuilder {
            num_cascades: SHADOW_CASCADES,
            first_cascade_far_bound: FIRST_CASCADE_M,
            maximum_distance: SHADOW_DISTANCE_M,
            ..default()
        }
        .build(),
        Transform::from_translation(REGION_SUN_DIRECTION.normalize())
            .looking_at(Vec3::ZERO, Vec3::Y),
    )
}

/// Spawns the region camera (inactive) and the region's sun; returns `(camera, sun)`.
pub(super) fn spawn_region_view(commands: &mut Commands, grid: &RegionGrid) -> (Entity, Entity) {
    let camera = commands
        .spawn((
            Name::new("Region camera"),
            RegionCamera,
            region_camera(),
            region_camera_parts(grid),
            RenderLayers::layer(TERRAIN_LAYER),
        ))
        .id();
    let sun = commands
        .spawn((
            Name::new("Region sun"),
            TerrainSun,
            region_sun_parts(),
            RenderLayers::layer(TERRAIN_LAYER),
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
}
