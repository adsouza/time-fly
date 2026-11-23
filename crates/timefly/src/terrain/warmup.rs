//! Warms the region's render pipelines while the globe loads (ruling R11's first-region frame).
//!
//! The region camera is spawned inactive, and the terrain, sea and sky only draw once it is
//! active, so without this their pipelines (main pass and the sun's shadow pass) compile on the
//! first frame the region camera draws: 1.1 s on a cold Metal shader cache, and through Tint on
//! every cold page load in Chrome. So while the globe's own pipelines compile
//! ([`GlobeRenderStatus::PreparingGpu`], behind the loading screen), a tiny stand-in region is
//! drawn through a camera built like the region camera:
//!
//! - **Same keys.** The terrain, sea and sky materials come from the same constructors as the
//!   real ones; the meshes have the same vertex layout (positions only) and topology; the camera
//!   has the region camera's components and draws to the same window (a pipeline's colour
//!   format is the window's), through a 1 × 1 viewport, before the globe camera clears it
//!   ([`WARMUP_CAMERA_ORDER`]); the sun casts the same cascaded shadows.
//! - **Its own layer** ([`WARMUP_LAYER`]), so nothing else ever sees it.
//! - **Gone when the globe is interactive** (or has failed). The globe's render probe counts
//!   every pending pipeline, these included, so "interactive" already waits for them. The
//!   entities are despawned and every handle dropped: no asset outlives it. Bevy keeps the
//!   compiled pipelines, so the first region finds them ready.

use bevy::camera::Viewport;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::render::storage::ShaderBuffer;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::terrain::{LodSettings, select_patches};
use timefly_core::worldgen::generate::{Field, RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

use super::material::{TerrainMaterial, TerrainParams, palette_image, patch_instances};
use super::view::{REGION_SKY, region_camera_parts, region_sun_parts};
use super::{grid_mesh, look_for, patch_buffer, terrain_material, upload_fields};
use crate::globe::GlobeRenderStatus;
use crate::sky::{SkyMaterial, distance_fog, region_look, sky_params};
use crate::water::{WaterMaterial, water_look, water_material};
use tracing::instrument;

/// The warm-up's render layer: its camera, sun, patch, sea and sky only.
pub const WARMUP_LAYER: usize = 5;
/// Its camera draws before every other camera on the window, so the globe camera's clear
/// covers its one pixel.
pub const WARMUP_CAMERA_ORDER: isize = -10;

/// What the warm-up does this frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WarmupAction {
    Wait,
    Start,
    Keep,
    Finish,
}

/// The warm-up's decision from the globe's status: start once while the globe's pipelines
/// compile, finish once it is interactive or has failed, never start again.
pub fn warmup_action(status: &GlobeRenderStatus, running: bool, done: bool) -> WarmupAction {
    let settled = matches!(
        status,
        GlobeRenderStatus::Interactive { .. } | GlobeRenderStatus::Failed(_)
    );
    match (running, done) {
        (true, _) if settled => WarmupAction::Finish,
        (true, _) => WarmupAction::Keep,
        (false, false) if *status == GlobeRenderStatus::PreparingGpu => WarmupAction::Start,
        _ => WarmupAction::Wait,
    }
}

/// The warm-up's entities while it runs (they hold every asset it made).
#[derive(Resource, Debug)]
pub struct RegionWarmup {
    pub entities: Vec<Entity>,
}

/// Set once the warm-up has run.
#[derive(Resource, Debug, Default)]
struct WarmupDone(bool);

pub struct RegionWarmupPlugin;

impl Plugin for RegionWarmupPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WarmupDone>()
            .add_systems(Update, region_warmup);
    }
}

/// A stand-in region: a 3 × 3 field rising from sea to land, so its look is an ordinary one.
pub fn warmup_grid() -> RegionGrid {
    let field = |size_m: f64| Field {
        res: 3,
        size_m,
        elevation_m: vec![
            -200.0, -50.0, 100.0, -50.0, 100.0, 400.0, 100.0, 400.0, 900.0,
        ],
        surface: vec![[0, 0, 0, 0]; 9],
    };
    RegionGrid {
        spec: RegionSpec {
            inner_res: 3,
            apron_res: 3,
            ..RegionSpec::new(geo_to_render(latlon_to_geo(46.5, 10.0)), YearsBp(0.0), 0)
        },
        inner: field(REGION_SIZE_M),
        apron: field(APRON_SIZE_M),
    }
}

/// The asset stores the stand-in region is made in.
#[derive(SystemParam)]
struct WarmupAssets<'w> {
    images: ResMut<'w, Assets<Image>>,
    meshes: ResMut<'w, Assets<Mesh>>,
    buffers: ResMut<'w, Assets<ShaderBuffer>>,
    terrain: ResMut<'w, Assets<TerrainMaterial>>,
    water: ResMut<'w, Assets<WaterMaterial>>,
    sky: ResMut<'w, Assets<SkyMaterial>>,
}

/// Gathers the globe's status, decides with [`warmup_action`], and spawns or removes the
/// stand-in region.
fn region_warmup(
    mut commands: Commands,
    status: Option<Res<GlobeRenderStatus>>,
    warmup: Option<Res<RegionWarmup>>,
    mut done: ResMut<WarmupDone>,
    mut assets: WarmupAssets,
) {
    let Some(status) = status else { return };
    match warmup_action(&status, warmup.is_some(), done.0) {
        WarmupAction::Wait | WarmupAction::Keep => {}
        WarmupAction::Start => {
            let entities = spawn_warmup(&mut commands, &mut assets);
            info!(entities = entities.len(), "region pipeline warm-up started");
            commands.insert_resource(RegionWarmup { entities });
        }
        WarmupAction::Finish => {
            if let Some(warmup) = warmup {
                for &e in &warmup.entities {
                    commands.entity(e).despawn();
                }
            }
            commands.remove_resource::<RegionWarmup>();
            done.0 = true;
            info!(globe = ?*status, "region pipeline warm-up finished");
        }
    }
}

#[instrument(level = "debug", skip_all)]
fn spawn_warmup(commands: &mut Commands, assets: &mut WarmupAssets) -> Vec<Entity> {
    let WarmupAssets {
        images,
        meshes,
        buffers,
        terrain: terrain_m,
        water: water_m,
        sky: sky_m,
    } = assets;
    let grid = warmup_grid();
    let settings = LodSettings::region();
    let params = TerrainParams::new(&grid, &settings);
    let start = DVec3::new(0.0, f64::from(crate::region::CAMERA_START_ALTITUDE_M), 0.0);
    let patches = select_patches(start, &settings);
    let instances = patch_instances(&patches[..patches.len().min(1)], &settings);
    let fields = upload_fields(&grid, images);
    let palette = images.add(palette_image());
    let buffer = buffers.add(patch_buffer(&params, &instances));
    // Every mesh of the region has positions only, as triangle lists: one quad stands in.
    let mesh = meshes.add(grid_mesh(1));
    let layer = RenderLayers::layer(WARMUP_LAYER);
    let terrain = terrain_m.add(terrain_material(
        &fields,
        &palette,
        &buffer,
        look_for(&grid),
    ));
    let water = water_m.add(water_material(
        &buffer,
        &fields.inner_height,
        &fields.apron_height,
        water_look(&grid),
    ));
    let (era, biome) = region_look(&grid);
    let sky = sky_m.add(SkyMaterial {
        params: sky_params(era, biome),
    });
    let drawn = |name: &'static str| {
        (
            Name::new(name),
            Mesh3d(mesh.clone()),
            Transform::IDENTITY,
            NoFrustumCulling,
            layer.clone(),
        )
    };
    vec![
        commands
            .spawn((
                drawn("Warm-up terrain patch"),
                MeshMaterial3d(terrain),
                MeshTag::new(0),
            ))
            .id(),
        commands
            .spawn((drawn("Warm-up sea"), MeshMaterial3d(water)))
            .id(),
        commands
            .spawn((drawn("Warm-up sky"), MeshMaterial3d(sky)))
            .id(),
        commands
            .spawn((
                Name::new("Warm-up camera"),
                Camera {
                    is_active: true,
                    order: WARMUP_CAMERA_ORDER,
                    clear_color: ClearColorConfig::Custom(REGION_SKY),
                    viewport: Some(Viewport {
                        physical_position: UVec2::ZERO,
                        physical_size: UVec2::ONE,
                        ..default()
                    }),
                    ..default()
                },
                region_camera_parts(&grid),
                distance_fog(era, biome),
                layer.clone(),
            ))
            .id(),
        commands
            .spawn((Name::new("Warm-up sun"), region_sun_parts(), layer))
            .id(),
    ]
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use super::*;
    use crate::terrain::view::RegionCamera;

    #[test]
    fn the_warm_up_runs_once_while_the_globe_prepares() {
        use GlobeRenderStatus as S;
        use WarmupAction as A;
        let interactive = S::Interactive { first_frame: 9 };
        let failed = S::Failed("no adapter".into());
        // Not before the globe's pipelines start compiling.
        assert_eq!(warmup_action(&S::LoadingData, false, false), A::Wait);
        assert_eq!(warmup_action(&S::PreparingGpu, false, false), A::Start);
        assert_eq!(warmup_action(&S::PreparingGpu, true, false), A::Keep);
        assert_eq!(warmup_action(&interactive, true, false), A::Finish);
        assert_eq!(warmup_action(&failed, true, false), A::Finish);
        // Never again, and never late (a globe already interactive needs no warm-up).
        assert_eq!(warmup_action(&S::PreparingGpu, false, true), A::Wait);
        assert_eq!(warmup_action(&interactive, false, true), A::Wait);
        assert_eq!(warmup_action(&interactive, false, false), A::Wait);
    }

    fn warmup_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<Mesh>()
            .init_asset::<ShaderBuffer>()
            .init_asset::<TerrainMaterial>()
            .init_asset::<WaterMaterial>()
            .init_asset::<SkyMaterial>()
            .insert_resource(GlobeRenderStatus::LoadingData)
            .add_plugins(RegionWarmupPlugin);
        app
    }

    fn asset_counts(app: &App) -> [usize; 6] {
        let w = app.world();
        [
            w.resource::<Assets<Image>>().len(),
            w.resource::<Assets<Mesh>>().len(),
            w.resource::<Assets<ShaderBuffer>>().len(),
            w.resource::<Assets<TerrainMaterial>>().len(),
            w.resource::<Assets<WaterMaterial>>().len(),
            w.resource::<Assets<SkyMaterial>>().len(),
        ]
    }

    /// Review Focus 5's rule for the warm-up: it leaves no entity and no asset behind.
    #[test]
    fn the_warm_up_leaves_nothing_behind() {
        let mut app = warmup_app();
        app.update();
        assert!(app.world().get_resource::<RegionWarmup>().is_none());
        assert_eq!(asset_counts(&app), [0; 6]);

        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        let entities = app.world().resource::<RegionWarmup>().entities.clone();
        assert_eq!(entities.len(), 5);
        // Five textures (four fields, the palette), one mesh, one buffer, one of each material.
        assert_eq!(asset_counts(&app), [5, 1, 1, 1, 1, 1]);
        {
            let world = app.world_mut();
            // Everything on its own layer, none of it the region's camera or sun.
            for &e in &entities {
                let layers = world.get::<RenderLayers>(e).expect("a layer");
                assert_eq!(*layers, RenderLayers::layer(WARMUP_LAYER));
            }
            assert_eq!(world.query::<&RegionCamera>().iter(world).count(), 0);
            let mut cameras = world.query::<&Camera>();
            let camera = cameras.single(world).unwrap();
            assert!(camera.is_active);
            assert_eq!(camera.order, WARMUP_CAMERA_ORDER);
            assert_eq!(
                camera.viewport.as_ref().map(|v| v.physical_size),
                Some(UVec2::ONE)
            );
            let mut suns = world.query::<&DirectionalLight>();
            assert!(suns.single(world).unwrap().shadow_maps_enabled);
        }
        // It stays while the globe prepares.
        app.update();
        assert_eq!(app.world().resource::<RegionWarmup>().entities, entities);

        *app.world_mut().resource_mut::<GlobeRenderStatus>() =
            GlobeRenderStatus::Interactive { first_frame: 30 };
        // Freeing cascades over a few frames: entities, then materials, then their textures.
        for _ in 0..4 {
            app.update();
        }
        assert!(app.world().get_resource::<RegionWarmup>().is_none());
        for e in entities {
            assert!(app.world().get_entity(e).is_err());
        }
        assert_eq!(asset_counts(&app), [0; 6]);
        // Never again.
        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        assert!(app.world().get_resource::<RegionWarmup>().is_none());
    }
}
