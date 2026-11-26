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
//!   has the region camera's components; the sun casts the same cascaded shadows. A pipeline's
//!   colour format is its camera's target's, so the camera draws into a 1 × 1 image in the
//!   format Bevy gives a window's main pass ([`WARMUP_TARGET_FORMAT`]). A 1 × 1 target keeps
//!   the camera's own textures (MSAA colour, depth, main textures) at a few bytes: drawn to the
//!   window, the warm-up left about 100 MiB resident for the session.
//! - **Its own layer** ([`WARMUP_LAYER`]), so nothing else ever sees it.
//! - **Phases** ([`WarmupPhase`], decided by [`warmup_action`]). Behind the loading screen the
//!   main-pass pipelines compile with the sun's shadows off; the globe's render probe counts
//!   every pending pipeline, these included, so "interactive" waits for them. Once the globe is
//!   interactive the sun casts shadows until the shadow pass's pipelines have compiled too (the
//!   probe clean again), a fraction of a second. Shadows wait for that because the shadow map is
//!   shared by every view: while the globe's offscreen bridge cameras render (they do during
//!   loading) a four-cascade map stays resident after it is no longer used (about 100 MiB, as
//!   measured on the region bench). Natively the shadow pipelines compile off the main thread; on
//!   the web this is one compile on the idle globe, against the whole set on the first region
//!   frame before. Then the camera stops and the shadows go off, and [`RETIRE_FRAMES`] later the
//!   entities are despawned and every handle dropped: no asset outlives it. Bevy keeps the
//!   compiled pipelines, so the first region finds them ready.
//! - **The cloud.** The transition cloud (`crate::cloud`) draws in the region camera too, at
//!   density 0 here (it outputs nothing), so a snapshot change never waits on its pipeline
//!   while the views must be covered.
//! - **The dragonfly.** The region camera draws the fly (`crate::creature`), so a stand-in fly
//!   (one `dragonfly_mesh`, drawn with the body's opaque material and the wings' blended one)
//!   warms both of its pipelines; in Chrome they would otherwise compile through Tint on the
//!   first region frame (the final review).

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::ecs::system::SystemParam;
use bevy::light::NotShadowCaster;
use bevy::math::DVec3;
use bevy::mesh::MeshTag;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::render::storage::ShaderBuffer;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::terrain::{LodSettings, select_patches};
use timefly_core::worldgen::generate::{Field, RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

use super::material::{TerrainMaterial, TerrainParams, palette_image, patch_instances};
use super::view::{REGION_SKY, region_camera_parts, region_sun, region_sun_parts};
use super::{TerrainOptions, grid_mesh, look_for, patch_buffer, terrain_material, upload_fields};
use crate::cloud::{CloudMaterial, CloudParams};
use crate::creature::{DragonflyMaterial, body_material, dragonfly_mesh, wing_material};
use crate::globe::{GlobeRenderStatus, RenderProbe};
use crate::sky::{SkyMaterial, distance_fog, region_look, sky_params};
use crate::water::{WaterMaterial, water_look, water_material};
use tracing::instrument;

/// The warm-up's render layer: its camera, sun, patch, sea, sky, cloud and dragonfly only.
pub const WARMUP_LAYER: usize = 5;
/// Its camera's order: below every other camera (no other camera shares it).
pub const WARMUP_CAMERA_ORDER: isize = -10;

/// The colour format the region camera's main pass renders in, so its pipelines' format. Bevy
/// renders a non-HDR window camera's main pass in the window's view format, normalized
/// (`bevy_render::camera`): `Bgra8UnormSrgb` becomes `Rgba8UnormSrgb`, and before the window's
/// first swap-chain texture is known the format falls back to `Rgba8UnormSrgb`. Metal, Vulkan and
/// WebGPU windows are 8-bit sRGB, so this is the format in every case this game runs.
pub const WARMUP_TARGET_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

/// Frames the shadow phase lasts at least, and at most (about 10 s at 60 Hz: a pipeline that
/// never finishes compiling must not keep the stand-in forever).
pub const SHADOW_MIN_FRAMES: u32 = 6;
pub const SHADOW_MAX_FRAMES: u32 = 600;
/// Frames without a pending pipeline that end the shadow phase.
pub const CLEAN_FRAMES: u32 = 3;
/// Frames between switching the warm-up's camera and shadows off and despawning it.
pub const RETIRE_FRAMES: u32 = 3;

/// Where a warm-up is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WarmupPhase {
    /// Behind the loading screen, until the globe is interactive: the main-pass pipelines,
    /// the sun's shadows off.
    MainPass,
    /// The globe is interactive: the sun casts shadows, so the shadow pass's pipelines compile,
    /// `frames` so far. It waits for the interactive globe because the shadow map is shared by
    /// every view: while the globe's offscreen bridge cameras render (they do during loading),
    /// a four-cascade map bound by them stays resident (about 100 MiB measured).
    Shadows { frames: u32 },
    /// The camera and the shadows are off, `frames` so far; then the entities go.
    Retiring { frames: u32 },
}

/// What the warm-up does this frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WarmupAction {
    Wait,
    Start,
    /// Go to (or stay in) this phase; apply its switches on entry.
    Phase(WarmupPhase),
    Despawn,
}

/// The warm-up's decision. `phase` is `None` while it is not running; `done` once it has run;
/// `clean_streak` is the render probe's frames without a pending pipeline. It starts once while
/// the globe's pipelines compile, takes the shadow phase once the globe is interactive (none if
/// the globe failed), and never starts again.
pub fn warmup_action(
    status: &GlobeRenderStatus,
    phase: Option<WarmupPhase>,
    done: bool,
    clean_streak: u32,
) -> WarmupAction {
    use WarmupPhase as P;
    match phase {
        None if !done && *status == GlobeRenderStatus::PreparingGpu => WarmupAction::Start,
        None => WarmupAction::Wait,
        Some(P::MainPass) => WarmupAction::Phase(match status {
            GlobeRenderStatus::Interactive { .. } => P::Shadows { frames: 0 },
            GlobeRenderStatus::Failed(_) => P::Retiring { frames: 0 },
            _ => P::MainPass,
        }),
        Some(P::Shadows { frames }) => {
            let compiled = frames >= SHADOW_MIN_FRAMES && clean_streak >= CLEAN_FRAMES;
            let failed = matches!(status, GlobeRenderStatus::Failed(_));
            WarmupAction::Phase(if compiled || failed || frames >= SHADOW_MAX_FRAMES {
                P::Retiring { frames: 0 }
            } else {
                P::Shadows { frames: frames + 1 }
            })
        }
        Some(P::Retiring { frames }) if frames >= RETIRE_FRAMES => WarmupAction::Despawn,
        Some(P::Retiring { frames }) => WarmupAction::Phase(P::Retiring { frames: frames + 1 }),
    }
}

/// The warm-up's entities while it runs (they hold every asset it made).
#[derive(Resource, Debug)]
pub struct RegionWarmup {
    pub entities: Vec<Entity>,
    pub camera: Entity,
    pub sun: Entity,
    pub phase: WarmupPhase,
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
    let field = |size_m: f64| {
        Field::dry(
            3,
            size_m,
            vec![
                -200.0, -50.0, 100.0, -50.0, 100.0, 400.0, 100.0, 400.0, 900.0,
            ],
            vec![[0, 0, 0, 0]; 9],
        )
    };
    RegionGrid {
        spec: RegionSpec {
            inner_res: 3,
            apron_res: 3,
            ..RegionSpec::new(geo_to_render(latlon_to_geo(46.5, 10.0)), YearsBp(0.0), 0)
        },
        inner: field(REGION_SIZE_M),
        apron: field(APRON_SIZE_M),
        boulders: Vec::new(),
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
    cloud: ResMut<'w, Assets<CloudMaterial>>,
    fly: ResMut<'w, Assets<DragonflyMaterial>>,
}

/// Gathers the globe's status and the render probe, decides with [`warmup_action`], and
/// spawns, switches or removes the stand-in region.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn region_warmup(
    mut commands: Commands,
    status: Option<Res<GlobeRenderStatus>>,
    probe: Option<Res<RenderProbe>>,
    warmup: Option<ResMut<RegionWarmup>>,
    mut done: ResMut<WarmupDone>,
    mut assets: WarmupAssets,
    mut cameras: Query<&mut Camera>,
    mut suns: Query<&mut DirectionalLight>,
) {
    let Some(status) = status else { return };
    let clean_streak = probe
        .and_then(|p| p.0.lock().ok().map(|r| r.clean_streak))
        .unwrap_or(u32::MAX);
    let phase = warmup.as_ref().map(|w| w.phase);
    match warmup_action(&status, phase, done.0, clean_streak) {
        WarmupAction::Wait => {}
        WarmupAction::Start => {
            let warmup = spawn_warmup(&mut commands, &mut assets);
            info!(
                entities = warmup.entities.len(),
                "region pipeline warm-up started"
            );
            commands.insert_resource(warmup);
        }
        WarmupAction::Phase(next) => {
            let Some(mut warmup) = warmup else { return };
            let entered = std::mem::discriminant(&next) != std::mem::discriminant(&warmup.phase);
            if entered {
                let shadows = matches!(next, WarmupPhase::Shadows { .. });
                if let Ok(mut sun) = suns.get_mut(warmup.sun) {
                    sun.shadow_maps_enabled = shadows;
                }
                if matches!(next, WarmupPhase::Retiring { .. })
                    && let Ok(mut camera) = cameras.get_mut(warmup.camera)
                {
                    camera.is_active = false;
                }
                info!(phase = ?next, "region pipeline warm-up phase");
            }
            warmup.phase = next;
        }
        WarmupAction::Despawn => {
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
fn spawn_warmup(commands: &mut Commands, assets: &mut WarmupAssets) -> RegionWarmup {
    let WarmupAssets {
        images,
        meshes,
        buffers,
        terrain: terrain_m,
        water: water_m,
        sky: sky_m,
        cloud: cloud_m,
        fly: fly_m,
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
        // A uniform value doesn't change a pipeline, so the default options warm every one.
        look_for(&grid, TerrainOptions::default()),
    ));
    // The sea binds the stand-in's water textures (`Field::dry`: no water, no river), as the
    // region's water binds its own.
    let water = water_m.add(water_material(
        &buffer,
        &fields.inner_water,
        &fields.apron_water,
        water_look(&grid),
    ));
    let mut target = Image::new_target_texture(1, 1, WARMUP_TARGET_FORMAT, None);
    target.asset_usage = RenderAssetUsages::RENDER_WORLD;
    let target = images.add(target);
    let (era, biome) = region_look(&grid);
    let sky = sky_m.add(SkyMaterial {
        params: sky_params(era, biome),
    });
    // Positions only, triangle lists: the quad gives the cloud's pipeline the same key as the
    // full-screen triangle's. Clear (density 0, so it draws nothing), at a depth of 1: at 0 its
    // plane would project to an infinite clip z.
    let cloud = cloud_m.add(CloudMaterial {
        params: CloudParams {
            state: Vec4::new(0.0, 1.0, 0.0, 1.0),
            ..default()
        },
    });
    // The fly's vertex layout (with the wing attribute) for both of its materials.
    let fly_mesh = meshes.add(dragonfly_mesh());
    let fly = |name: &'static str, material: Handle<DragonflyMaterial>| {
        (
            Name::new(name),
            Mesh3d(fly_mesh.clone()),
            MeshMaterial3d(material),
            Transform::IDENTITY,
            NoFrustumCulling,
            NotShadowCaster,
            layer.clone(),
        )
    };
    let drawn = |name: &'static str| {
        (
            Name::new(name),
            Mesh3d(mesh.clone()),
            Transform::IDENTITY,
            NoFrustumCulling,
            layer.clone(),
        )
    };
    let camera = commands
        .spawn((
            Name::new("Warm-up camera"),
            Camera {
                is_active: true,
                order: WARMUP_CAMERA_ORDER,
                clear_color: ClearColorConfig::Custom(REGION_SKY),
                ..default()
            },
            RenderTarget::Image(target.into()),
            region_camera_parts(&grid),
            distance_fog(era, biome),
            layer.clone(),
        ))
        .id();
    // The region's sun, its shadows off until the shadow phase.
    let sun = commands
        .spawn((Name::new("Warm-up sun"), region_sun_parts(), layer.clone()))
        .insert(DirectionalLight {
            shadow_maps_enabled: false,
            ..region_sun()
        })
        .id();
    let entities = vec![
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
            .spawn((drawn("Warm-up cloud"), MeshMaterial3d(cloud)))
            .id(),
        commands
            .spawn(fly("Warm-up dragonfly body", fly_m.add(body_material())))
            .id(),
        commands
            .spawn(fly("Warm-up dragonfly wings", fly_m.add(wing_material())))
            .id(),
        camera,
        sun,
    ];
    RegionWarmup {
        entities,
        camera,
        sun,
        phase: WarmupPhase::MainPass,
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use super::*;
    use crate::cloud::CloudMaterial;
    use crate::creature::{ATTRIBUTE_WING, DragonflyMaterial};
    use crate::terrain::view::RegionCamera;

    #[test]
    fn the_warm_up_runs_once_and_takes_shadows_after_the_globe() {
        use GlobeRenderStatus as S;
        use WarmupAction as A;
        use WarmupPhase as P;
        let interactive = S::Interactive { first_frame: 9 };
        let failed = S::Failed("no adapter".into());
        let act = |status: &S, phase, done, clean| warmup_action(status, phase, done, clean);
        // Starts only while the globe's pipelines compile, and only once.
        assert_eq!(act(&S::LoadingData, None, false, 0), A::Wait);
        assert_eq!(act(&S::PreparingGpu, None, false, 0), A::Start);
        assert_eq!(act(&S::PreparingGpu, None, true, 0), A::Wait);
        assert_eq!(act(&interactive, None, false, 9), A::Wait);
        assert_eq!(act(&interactive, None, true, 9), A::Wait);
        // The main pass holds behind the loading screen.
        assert_eq!(
            act(&S::PreparingGpu, Some(P::MainPass), false, 9),
            A::Phase(P::MainPass)
        );
        // Shadows once the globe is interactive, never on a failed one.
        assert_eq!(
            act(&interactive, Some(P::MainPass), false, 9),
            A::Phase(P::Shadows { frames: 0 })
        );
        assert_eq!(
            act(&failed, Some(P::MainPass), false, 9),
            A::Phase(P::Retiring { frames: 0 })
        );
        // The shadow phase waits for its pipelines: a few frames, then a clean probe.
        assert_eq!(
            act(&interactive, Some(P::Shadows { frames: 0 }), false, 99),
            A::Phase(P::Shadows { frames: 1 })
        );
        let late = P::Shadows {
            frames: SHADOW_MIN_FRAMES,
        };
        assert_eq!(
            act(&interactive, Some(late), false, CLEAN_FRAMES - 1),
            A::Phase(P::Shadows {
                frames: SHADOW_MIN_FRAMES + 1
            })
        );
        assert_eq!(
            act(&interactive, Some(late), false, CLEAN_FRAMES),
            A::Phase(P::Retiring { frames: 0 })
        );
        // ...but not forever.
        let stuck = P::Shadows {
            frames: SHADOW_MAX_FRAMES,
        };
        assert_eq!(
            act(&interactive, Some(stuck), false, 0),
            A::Phase(P::Retiring { frames: 0 })
        );
        // Retiring: a few frames with everything off, then despawned.
        assert_eq!(
            act(&interactive, Some(P::Retiring { frames: 0 }), false, 0),
            A::Phase(P::Retiring { frames: 1 })
        );
        assert_eq!(
            act(
                &interactive,
                Some(P::Retiring {
                    frames: RETIRE_FRAMES
                }),
                false,
                0
            ),
            A::Despawn
        );
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
            .init_asset::<CloudMaterial>()
            .init_asset::<DragonflyMaterial>()
            .insert_resource(GlobeRenderStatus::LoadingData)
            .add_plugins(RegionWarmupPlugin);
        app
    }

    fn asset_counts(app: &App) -> [usize; 8] {
        let w = app.world();
        [
            w.resource::<Assets<Image>>().len(),
            w.resource::<Assets<Mesh>>().len(),
            w.resource::<Assets<ShaderBuffer>>().len(),
            w.resource::<Assets<TerrainMaterial>>().len(),
            w.resource::<Assets<WaterMaterial>>().len(),
            w.resource::<Assets<SkyMaterial>>().len(),
            w.resource::<Assets<CloudMaterial>>().len(),
            w.resource::<Assets<DragonflyMaterial>>().len(),
        ]
    }

    /// Review Focus 5's rule for the warm-up: it leaves no entity and no asset behind.
    #[test]
    fn the_warm_up_leaves_nothing_behind() {
        let mut app = warmup_app();
        app.update();
        assert!(app.world().get_resource::<RegionWarmup>().is_none());
        assert_eq!(asset_counts(&app), [0; 8]);

        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        let entities = app.world().resource::<RegionWarmup>().entities.clone();
        assert_eq!(entities.len(), 8);
        // Its camera has no clusters (ruling R28), without the app-wide observer.
        crate::clusters::assert_no_clusters(app.world_mut());
        // Eight textures (four height and surface fields and the two water fields the sea
        // binds, the palette, the 1 x 1 target), two meshes (the quad and the dragonfly), one
        // buffer, one of each material, the transition cloud's included (M3 Task 5), and the
        // dragonfly's body and wing materials (final review).
        assert_eq!(asset_counts(&app), [8, 2, 1, 1, 1, 1, 1, 2]);
        {
            let world = app.world_mut();
            // Everything on its own layer, none of it the region's camera or sun.
            for &e in &entities {
                let layers = world.get::<RenderLayers>(e).expect("a layer");
                assert_eq!(*layers, RenderLayers::layer(WARMUP_LAYER));
            }
            assert_eq!(world.query::<&RegionCamera>().iter(world).count(), 0);
            let mut cameras = world.query::<(&Camera, &RenderTarget)>();
            let (camera, target) = cameras.single(world).unwrap();
            assert!(camera.is_active);
            assert_eq!(camera.order, WARMUP_CAMERA_ORDER);
            let image = world
                .resource::<Assets<Image>>()
                .get(target.as_image().unwrap())
                .unwrap();
            assert_eq!((image.width(), image.height()), (1, 1));
            assert_eq!(image.texture_descriptor.format, WARMUP_TARGET_FORMAT);
            // No shadows behind the loading screen.
            let mut suns = world.query::<&DirectionalLight>();
            assert!(!suns.single(world).unwrap().shadow_maps_enabled);
        }
        // It stays while the globe prepares.
        app.update();
        assert_eq!(app.world().resource::<RegionWarmup>().entities, entities);

        *app.world_mut().resource_mut::<GlobeRenderStatus>() =
            GlobeRenderStatus::Interactive { first_frame: 30 };
        let parts = |app: &App| {
            let warmup = app.world().resource::<RegionWarmup>();
            let world = app.world();
            (
                warmup.phase,
                world.get::<Camera>(warmup.camera).unwrap().is_active,
                world
                    .get::<DirectionalLight>(warmup.sun)
                    .unwrap()
                    .shadow_maps_enabled,
            )
        };
        // Shadows on for the shadow pass's pipelines (no render probe here: always clean).
        app.update();
        assert_eq!(
            parts(&app),
            (WarmupPhase::Shadows { frames: 0 }, true, true)
        );
        for _ in 0..SHADOW_MIN_FRAMES + 1 {
            app.update();
        }
        // Then the camera stops and the shadows go off, the entities still there.
        assert_eq!(
            parts(&app),
            (WarmupPhase::Retiring { frames: 0 }, false, false)
        );
        // Then they go. Freeing cascades over a few frames: entities, then materials, then
        // their textures.
        for _ in 0..RETIRE_FRAMES + 4 {
            app.update();
        }
        assert!(app.world().get_resource::<RegionWarmup>().is_none());
        for e in entities {
            assert!(app.world().get_entity(e).is_err());
        }
        assert_eq!(asset_counts(&app), [0; 8]);
        // Never again.
        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        assert!(app.world().get_resource::<RegionWarmup>().is_none());
    }

    /// The region camera draws the dragonfly too: its body (opaque) and wings (blended) must
    /// not compile on the first region frame (final review).
    #[test]
    fn the_warm_up_draws_the_dragonfly() {
        let mut app = warmup_app();
        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        let entities = app.world().resource::<RegionWarmup>().entities.clone();
        let world = app.world_mut();
        let mut flies = world.query::<(Entity, &Mesh3d, &MeshMaterial3d<DragonflyMaterial>)>();
        let flies: Vec<_> = flies
            .iter(world)
            .map(|(e, m, mat)| (e, m.0.clone(), mat.0.clone()))
            .collect();
        assert_eq!(flies.len(), 2, "a body and wings");
        let materials = world.resource::<Assets<DragonflyMaterial>>();
        let mut modes: Vec<_> = flies
            .iter()
            .map(|(_, _, mat)| materials.get(mat).unwrap().base.alpha_mode)
            .collect();
        modes.sort_by_key(|m| format!("{m:?}"));
        assert_eq!(modes, [AlphaMode::Blend, AlphaMode::Opaque]);
        let meshes = world.resource::<Assets<Mesh>>();
        for (e, mesh, _) in &flies {
            assert!(entities.contains(e));
            // The real fly's vertex layout: the wing attribute the flap reads.
            assert!(meshes.get(mesh).unwrap().contains_attribute(ATTRIBUTE_WING));
            assert_eq!(
                *world.get::<RenderLayers>(*e).unwrap(),
                RenderLayers::layer(WARMUP_LAYER)
            );
        }
    }

    /// The warm-up's sea binds both water textures (Task 8), built from the stand-in grid's
    /// dry fields like the real region's: a pipeline's key doesn't depend on the data.
    #[test]
    fn the_warm_up_draws_the_new_water() {
        use bevy::render::render_resource::TextureFormat;
        let mut app = warmup_app();
        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        let entities = app.world().resource::<RegionWarmup>().entities.clone();
        let world = app.world_mut();
        let mut seas = world.query::<(Entity, &MeshMaterial3d<WaterMaterial>)>();
        let seas: Vec<_> = seas.iter(world).map(|(e, m)| (e, m.0.clone())).collect();
        assert_eq!(seas.len(), 1);
        assert!(entities.contains(&seas[0].0));
        let material = world
            .resource::<Assets<WaterMaterial>>()
            .get(&seas[0].1)
            .unwrap()
            .clone();
        let images = world.resource::<Assets<Image>>();
        let grid = warmup_grid();
        for (handle, field) in [
            (&material.extension.inner_water, &grid.inner),
            (&material.extension.apron_water, &grid.apron),
        ] {
            let image = images
                .get(handle)
                .expect("a water texture held by the warm-up sea");
            assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba32Uint);
            assert_eq!(image.size(), UVec2::splat(field.res));
        }
        assert_ne!(
            material.extension.inner_water,
            material.extension.apron_water
        );
    }

    /// The warm-up cloud's depth is positive, so its plane never projects to an infinite clip
    /// z (Tint, the final review).
    #[test]
    fn the_warm_up_cloud_sits_at_a_finite_depth() {
        let mut app = warmup_app();
        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        let world = app.world();
        let clouds = world.resource::<Assets<CloudMaterial>>();
        let (_, cloud) = clouds.iter().next().unwrap();
        assert_eq!(cloud.params.state.x, 0.0, "clear: it draws nothing");
        assert!(cloud.params.state.y > 0.0, "{:?}", cloud.params.state);
    }
}
