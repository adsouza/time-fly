//! The globe (spec §7.1): a fixed geographic sphere for the recent path, rigid plate pieces over
//! an ocean shell for deep time, and a crossfade between the two across the bridge.
//!
//! Rendering modes follow the frame selection. `Recent` and `Deep` draw their globe straight
//! into the globe camera's target. During the `Bridge`, two offscreen cameras render the
//! completed recent and deep globes with the same view, and the globe camera shows their linear
//! crossfade; the atmosphere is drawn once, by the globe camera, in every mode.
//!
//! The systems gather → decide → apply: [`globe_view`], [`selection_change`] and
//! [`next_render_status`] are the pure decisions.

#[cfg(not(target_arch = "wasm32"))]
pub mod capture;
pub mod material;
pub mod mesh;

use std::sync::{Arc, Mutex};

use bevy::camera::RenderTarget;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::diagnostic::FrameCount;
use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;
use bevy::render::render_resource::{PipelineCache, TextureFormat};
use bevy::render::storage::ShaderBuffer;
use bevy::render::view::{DebandDither, Tonemapping};
use bevy::render::{Render, RenderApp, RenderSystems};
use bevy::shader::ShaderCacheError;
use timefly_core::globe::{DeepSampling, source_id_ranks};
use timefly_core::surface::{FrameSelection, Timeline};
use tracing::instrument;

use crate::data::{GlobeData, GlobeLoadError, GlobeLoadState};
use crate::diagnostics::aspects::DiagnosticsAppExt;
use crate::diagnostics::{DiagnosticsContext, FirstInteractiveFrame};
use crate::time::GlobeTime;
use material::{
    AtmosphereMaterial, CompositeMaterial, GlobeParams, OceanMaterial, PlateMaterial,
    RecentGlobeMaterial, add_globe_shaders, list_flags, plate_gpu_data,
};
use mesh::{
    IntervalList, build_plate_pieces, cube_sphere, fullscreen_triangle, plate_subdivisions,
};

pub use timefly_core::globe::{EARTH_RADIUS_KM, ELEVATION_EXAGGERATION};

/// Render layer of the fixed recent globe.
pub const RECENT_LAYER: usize = 1;
/// Render layer of the plate pieces and the ocean shell.
pub const DEEP_LAYER: usize = 2;
/// Render layer of the bridge crossfade.
pub const COMPOSITE_LAYER: usize = 3;
/// Render layer of the atmosphere (the default layer, seen by every globe camera mode).
pub const ATMOSPHERE_LAYER: usize = 0;

/// Direction toward the sun at start-up: ahead of and above the initial view of longitude 0, a
/// little to the east, so the terminator shows on the western limb *(call)*.
pub const SUN_DIRECTION: Vec3 = Vec3::new(0.55, 0.32, 0.77);
/// Space behind the globe.
pub const SPACE_COLOR: Color = Color::srgb(0.035, 0.04, 0.07);
/// Distance of the default globe camera.
pub const CAMERA_DISTANCE_KM: f32 = 3.0 * EARTH_RADIUS_KM as f32;

/// Subdivisions of the ocean shell and atmosphere cube-spheres.
const SHELL_SUBDIVISIONS: u32 = 48;
/// Render-world frames with no pending pipeline before the globe counts as interactive.
const CLEAN_FRAMES_FOR_INTERACTIVE: u32 = 3;

/// Which globe the selection shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum GlobeMode {
    Recent,
    Deep,
    Bridge,
}

/// An ICE-6G_C bracket on the fixed recent globe.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RecentSampling {
    /// `[younger, older]` frame indices.
    pub frames: [usize; 2],
    pub older_weight: f64,
}

/// What the globe shows at an age: the decision behind every material and camera update.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GlobeView {
    pub mode: GlobeMode,
    pub recent: Option<RecentSampling>,
    pub deep: Option<DeepSampling>,
    /// The crossfade weight of the deep view (0 recent only, 1 deep only).
    pub deep_weight: f64,
}

impl GlobeView {
    /// Frame indices in use, without repeats, younger first.
    pub fn frames(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let recent = self.recent.map(|r| r.frames).unwrap_or_default();
        let deep = self.deep.map(|d| d.endpoints.map(|e| e.frame));
        let all = match self.mode {
            GlobeMode::Recent => recent.to_vec(),
            GlobeMode::Deep => deep.unwrap_or_default().to_vec(),
            GlobeMode::Bridge => [recent[0]]
                .into_iter()
                .chain(deep.unwrap_or_default())
                .collect(),
        };
        for f in all {
            if !out.contains(&f) {
                out.push(f);
            }
        }
        out
    }
}

/// Decides what the globe shows at `age_myr` from the timeline's frame selection.
pub fn globe_view(timeline: &Timeline, age_myr: f64) -> GlobeView {
    match timeline.select_frames(age_myr) {
        FrameSelection::Recent(b) => GlobeView {
            mode: GlobeMode::Recent,
            recent: Some(RecentSampling {
                frames: [b.younger, b.older],
                older_weight: b.older_weight,
            }),
            deep: None,
            deep_weight: 0.0,
        },
        FrameSelection::Deep(d) => GlobeView {
            mode: GlobeMode::Deep,
            recent: None,
            deep: Some(DeepSampling::from_bracket(timeline, age_myr, &d)),
            deep_weight: 1.0,
        },
        FrameSelection::Bridge {
            recent,
            deep,
            deep_weight,
        } => GlobeView {
            mode: GlobeMode::Bridge,
            recent: Some(RecentSampling {
                frames: [recent, recent],
                older_weight: 0.0,
            }),
            deep: Some(DeepSampling::from_bracket(timeline, age_myr, &deep)),
            deep_weight,
        },
    }
}

/// Domain event: the selected frame indices or the bridge state changed.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct FrameSelectionChanged {
    pub mode: GlobeMode,
    pub frames: Vec<usize>,
}

/// The event to emit when the view changes frames or mode; `None` for a weight-only change.
pub fn selection_change(
    previous: Option<&GlobeView>,
    next: &GlobeView,
) -> Option<FrameSelectionChanged> {
    let frames = next.frames();
    let same = previous.is_some_and(|p| p.mode == next.mode && p.frames() == frames);
    (!same).then_some(FrameSelectionChanged {
        mode: next.mode,
        frames,
    })
}

/// How far the globe is from being interactive, for the loading UI (Task 10) and diagnostics.
#[derive(Resource, Clone, Debug, PartialEq, Default)]
pub enum GlobeRenderStatus {
    /// The globe data is still loading.
    #[default]
    LoadingData,
    /// The data is ready; GPU assets and pipelines are being prepared.
    PreparingGpu,
    /// The first interactive globe frame has been drawn (at this frame count).
    Interactive { first_frame: u32 },
    /// Loading or rendering failed; the message is for the user.
    Failed(String),
}

/// What the render world reports about pipeline preparation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderProbeReport {
    /// Render-world frames seen.
    pub frames: u64,
    /// Consecutive frames without a queued or compiling pipeline.
    pub clean_streak: u32,
    /// Pipelines queued, compiling or waiting for shaders in the last frame.
    pub pending: u32,
    /// The first pipeline error (shader or pipeline creation).
    pub error: Option<String>,
}

/// Shared between the main and render worlds.
#[derive(Resource, Clone, Default)]
pub struct RenderProbe(pub Arc<Mutex<RenderProbeReport>>);

impl RenderProbe {
    fn report(&self) -> RenderProbeReport {
        self.0.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

/// Seconds without pipeline progress before GPU preparation counts as failed.
pub const GPU_PREPARE_TIMEOUT_SECS: f64 = 30.0;

/// Pipeline preparation progress: the fewest pending pipelines seen and when that last improved.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct PrepareProgress {
    pub fewest_pending: Option<u32>,
    pub last_progress_secs: f64,
}

/// Records progress: fewer pending pipelines than ever before (or none) counts.
pub fn track_progress(progress: PrepareProgress, pending: u32, now_secs: f64) -> PrepareProgress {
    if pending == 0
        || progress
            .fewest_pending
            .is_none_or(|fewest| pending < fewest)
    {
        PrepareProgress {
            fewest_pending: Some(pending),
            last_progress_secs: now_secs,
        }
    } else {
        progress
    }
}

/// Decides the next render status. `spawned_at` is the render-probe frame count when the globe
/// was spawned; interactive needs a few render frames since then without pending pipelines.
/// Preparation that makes no progress for [`GPU_PREPARE_TIMEOUT_SECS`] (for example a shader
/// that never loads) fails instead of leaving the loading screen up forever.
pub fn next_render_status(
    current: &GlobeRenderStatus,
    report: &RenderProbeReport,
    spawned_at: u64,
    frame: u32,
    stalled_secs: f64,
) -> GlobeRenderStatus {
    match current {
        GlobeRenderStatus::PreparingGpu => {
            if let Some(error) = &report.error {
                GlobeRenderStatus::Failed(format!("the renderer could not start: {error}"))
            } else if report.frames > spawned_at + u64::from(CLEAN_FRAMES_FOR_INTERACTIVE)
                && report.clean_streak >= CLEAN_FRAMES_FOR_INTERACTIVE
            {
                GlobeRenderStatus::Interactive { first_frame: frame }
            } else if stalled_secs > GPU_PREPARE_TIMEOUT_SECS {
                GlobeRenderStatus::Failed(format!(
                    "the renderer could not start: {} shader pipelines made no progress for \
                     {GPU_PREPARE_TIMEOUT_SECS:.0} s",
                    report.pending
                ))
            } else {
                current.clone()
            }
        }
        other => other.clone(),
    }
}

/// Switches the globe to the verification view: plate pieces draw their plate index and the
/// ocean shell draws blue (see `material::decode_plate_id`). For headless checks only.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct GlobeDebugView(pub bool);

/// Marks the camera that shows the globe. Task 10's orbit controller drives it.
#[derive(Component)]
pub struct GlobeCamera;

/// An offscreen camera of the bridge, rendering one globe layer.
#[derive(Component)]
struct BridgeCamera {
    layer: usize,
}

/// The globe's assets and the last applied view.
#[derive(Resource)]
struct GlobeScene {
    plate: Handle<PlateMaterial>,
    ocean: Handle<OceanMaterial>,
    recent: Handle<RecentGlobeMaterial>,
    composite: Handle<CompositeMaterial>,
    atmosphere: Handle<AtmosphereMaterial>,
    plates_buffer: Handle<ShaderBuffer>,
    list_buffer: Handle<ShaderBuffer>,
    lists: Vec<IntervalList>,
    recent_target: Handle<Image>,
    deep_target: Handle<Image>,
    target_size: UVec2,
    sun: Vec3,
    applied_age_myr: Option<f64>,
    applied_debug: bool,
    view: Option<GlobeView>,
    spawned_at: u64,
    progress: PrepareProgress,
}

/// Spawns and drives the globe once the globe data is ready.
pub struct GlobePlugin;

impl Plugin for GlobePlugin {
    fn build(&self, app: &mut App) {
        add_globe_shaders(&mut app.world_mut().resource_mut::<Assets<Shader>>());
        let probe = RenderProbe::default();
        app.add_plugins((
            MaterialPlugin::<PlateMaterial>::default(),
            MaterialPlugin::<OceanMaterial>::default(),
            MaterialPlugin::<RecentGlobeMaterial>::default(),
            MaterialPlugin::<CompositeMaterial>::default(),
            MaterialPlugin::<AtmosphereMaterial>::default(),
        ))
        .add_domain_event::<FrameSelectionChanged>()
        .init_resource::<GlobeRenderStatus>()
        .init_resource::<GlobeDebugView>()
        .insert_resource(probe.clone())
        .add_systems(Startup, spawn_globe_camera)
        .add_systems(OnEnter(GlobeLoadState::Ready), spawn_globe)
        .add_systems(OnEnter(GlobeLoadState::Failed), report_load_failure)
        .add_systems(
            Update,
            (
                update_globe_view,
                sync_sun,
                sync_bridge_cameras,
                track_render_status,
            )
                .chain()
                .run_if(resource_exists::<GlobeScene>),
        );
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .insert_resource(probe)
                .add_systems(Render, probe_pipelines.in_set(RenderSystems::Cleanup));
        }
    }
}

fn spawn_globe_camera(mut commands: Commands, cameras: Query<(), With<GlobeCamera>>) {
    if !cameras.is_empty() {
        return;
    }
    commands.spawn((
        Name::new("Globe camera"),
        GlobeCamera,
        Camera3d::default(),
        Camera {
            clear_color: ClearColorConfig::Custom(SPACE_COLOR),
            ..default()
        },
        Tonemapping::None,
        DebandDither::Disabled,
        Transform::from_xyz(0.0, 0.0, CAMERA_DISTANCE_KM).looking_at(Vec3::ZERO, Vec3::Y),
        RenderLayers::from_layers(&[ATMOSPHERE_LAYER, RECENT_LAYER]),
    ));
}

/// An offscreen target for one bridge view, sized like the globe camera's target.
fn bridge_target(size: UVec2) -> Image {
    Image::new_target_texture(
        size.x.max(1),
        size.y.max(1),
        TextureFormat::Rgba8UnormSrgb,
        None,
    )
}

#[instrument(level = "debug", skip_all, fields(cube_size = data.cube_size))]
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn spawn_globe(
    mut commands: Commands,
    data: Res<GlobeData>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut plates_m: ResMut<Assets<PlateMaterial>>,
    mut oceans_m: ResMut<Assets<OceanMaterial>>,
    mut recents_m: ResMut<Assets<RecentGlobeMaterial>>,
    mut composites_m: ResMut<Assets<CompositeMaterial>>,
    mut atmospheres_m: ResMut<Assets<AtmosphereMaterial>>,
    mut status: ResMut<GlobeRenderStatus>,
    probe: Res<RenderProbe>,
    real_time: Res<Time<Real>>,
    camera: Query<&Camera, With<GlobeCamera>>,
) {
    let crust = &data.crust;
    let ranks = source_id_ranks(crust.rotations());
    let pieces = build_plate_pieces(
        crust.footprints(),
        &ranks,
        plate_subdivisions(data.cube_size),
    );
    info!(
        triangles = pieces.triangle_count(),
        vertices = pieces.positions.len(),
        interval_lists = pieces.lists.len(),
        "plate pieces built"
    );
    let sun = SUN_DIRECTION.normalize();
    let first = data.frame_image(0).clone();
    let view = globe_view(&data.timeline, 0.0);
    let deep = view.deep.unwrap_or_else(|| {
        DeepSampling::single_frame(&data.timeline, data.timeline.bridge_frame_index(), 0.0, 0.0)
    });
    let plates_buffer = buffers.add(ShaderBuffer::from(plate_gpu_data(crust, &deep)));
    let ages = [
        deep.age_myr,
        deep.source_ages_myr()[0],
        deep.source_ages_myr()[1],
    ];
    let list_buffer = buffers.add(ShaderBuffer::from(list_flags(&pieces.lists, ages)));
    let deep_image = data.frame_image(deep.endpoints[0].frame).clone();
    let size = camera
        .single()
        .ok()
        .and_then(Camera::physical_target_size)
        .unwrap_or(UVec2::new(1280, 720));
    let recent_target = images.add(bridge_target(size));
    let deep_target = images.add(bridge_target(size));

    let plate = plates_m.add(PlateMaterial {
        params: GlobeParams::deep(&deep, data.cube_size, sun),
        younger: deep_image.clone(),
        older: deep_image.clone(),
        plates: plates_buffer.clone(),
        list_flags: list_buffer.clone(),
    });
    let ocean = oceans_m.add(OceanMaterial {
        params: GlobeParams::deep(&deep, data.cube_size, sun),
        younger: deep_image.clone(),
        older: deep_image,
    });
    let recent = recents_m.add(RecentGlobeMaterial {
        params: GlobeParams::recent(0.0, data.cube_size, sun),
        younger: first.clone(),
        older: first,
    });
    let composite = composites_m.add(CompositeMaterial {
        params: GlobeParams::composite(0.0, sun),
        recent_view: recent_target.clone(),
        deep_view: deep_target.clone(),
    });
    let atmosphere = atmospheres_m.add(AtmosphereMaterial {
        params: GlobeParams::atmosphere(sun),
    });

    let recent_sphere = meshes.add(cube_sphere(data.cube_size.max(16)).to_mesh());
    let shell = meshes.add(cube_sphere(SHELL_SUBDIVISIONS).to_mesh());
    commands.spawn((
        Name::new("Recent globe"),
        Mesh3d(recent_sphere),
        MeshMaterial3d(recent.clone()),
        Transform::IDENTITY,
        NoFrustumCulling,
        RenderLayers::layer(RECENT_LAYER),
    ));
    commands.spawn((
        Name::new("Ocean shell"),
        Mesh3d(shell.clone()),
        MeshMaterial3d(ocean.clone()),
        Transform::IDENTITY,
        NoFrustumCulling,
        RenderLayers::layer(DEEP_LAYER),
    ));
    commands.spawn((
        Name::new("Plate pieces"),
        Mesh3d(meshes.add(pieces.to_mesh())),
        MeshMaterial3d(plate.clone()),
        Transform::IDENTITY,
        NoFrustumCulling,
        RenderLayers::layer(DEEP_LAYER),
    ));
    commands.spawn((
        Name::new("Bridge composite"),
        Mesh3d(meshes.add(fullscreen_triangle())),
        MeshMaterial3d(composite.clone()),
        Transform::IDENTITY,
        NoFrustumCulling,
        RenderLayers::layer(COMPOSITE_LAYER),
    ));
    commands.spawn((
        Name::new("Atmosphere"),
        Mesh3d(shell),
        MeshMaterial3d(atmosphere.clone()),
        Transform::IDENTITY,
        NoFrustumCulling,
        RenderLayers::layer(ATMOSPHERE_LAYER),
    ));
    commands.spawn((
        Name::new("Sun"),
        DirectionalLight {
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_translation(sun).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    // The bridge cameras render during warm-up too, so every globe pipeline is compiled before
    // the globe counts as interactive.
    for (order, layer, target) in [
        (-2, RECENT_LAYER, recent_target.clone()),
        (-1, DEEP_LAYER, deep_target.clone()),
    ] {
        commands.spawn((
            Name::new("Bridge camera"),
            BridgeCamera { layer },
            Camera3d::default(),
            Camera {
                order,
                is_active: true,
                clear_color: ClearColorConfig::Custom(SPACE_COLOR),
                ..default()
            },
            RenderTarget::Image(target.into()),
            Tonemapping::None,
            DebandDither::Disabled,
            Transform::from_xyz(0.0, 0.0, CAMERA_DISTANCE_KM).looking_at(Vec3::ZERO, Vec3::Y),
            RenderLayers::layer(layer),
        ));
    }
    commands.insert_resource(GlobeScene {
        plate,
        ocean,
        recent,
        composite,
        atmosphere,
        plates_buffer,
        list_buffer,
        lists: pieces.lists,
        recent_target,
        deep_target,
        target_size: size,
        sun,
        applied_age_myr: None,
        applied_debug: false,
        view: None,
        spawned_at: probe.report().frames,
        progress: PrepareProgress {
            fewest_pending: None,
            last_progress_secs: real_time.elapsed_secs_f64(),
        },
    });
    *status = GlobeRenderStatus::PreparingGpu;
}

fn report_load_failure(error: Option<Res<GlobeLoadError>>, mut status: ResMut<GlobeRenderStatus>) {
    let message = error.map_or_else(
        || "the globe data failed to load".to_string(),
        |e| e.0.clone(),
    );
    *status = GlobeRenderStatus::Failed(message);
}

/// Applies the view for the current age; does nothing while the age is unchanged.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn update_globe_view(
    time: Res<GlobeTime>,
    data: Res<GlobeData>,
    mut scene: ResMut<GlobeScene>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut plates_m: ResMut<Assets<PlateMaterial>>,
    mut oceans_m: ResMut<Assets<OceanMaterial>>,
    mut recents_m: ResMut<Assets<RecentGlobeMaterial>>,
    mut composites_m: ResMut<Assets<CompositeMaterial>>,
    mut atmospheres_m: ResMut<Assets<AtmosphereMaterial>>,
    debug: Res<GlobeDebugView>,
    mut cameras: Query<&mut RenderLayers, With<GlobeCamera>>,
    mut changes: MessageWriter<FrameSelectionChanged>,
    context: Option<ResMut<DiagnosticsContext>>,
) {
    let age_myr = time.years.myr();
    if scene.applied_age_myr == Some(age_myr) && scene.applied_debug == debug.0 {
        return;
    }
    let view = globe_view(&data.timeline, age_myr);
    let sun = scene.sun;
    let dbg = debug.0;
    if let Some(mut m) = atmospheres_m.get_mut(&scene.atmosphere) {
        m.params = m.params.with_debug(dbg);
    }
    if let Some(r) = view.recent
        && let Some(mut m) = recents_m.get_mut(&scene.recent)
    {
        m.params = GlobeParams::recent(r.older_weight, data.cube_size, sun).with_debug(dbg);
        m.younger = data.frame_image(r.frames[0]).clone();
        m.older = data.frame_image(r.frames[1]).clone();
    }
    if let Some(deep) = view.deep {
        let params = GlobeParams::deep(&deep, data.cube_size, sun).with_debug(dbg);
        let [young, old] = deep.endpoints.map(|e| data.frame_image(e.frame).clone());
        if let Some(mut m) = plates_m.get_mut(&scene.plate) {
            m.params = params;
            m.younger = young.clone();
            m.older = old.clone();
        }
        if let Some(mut m) = oceans_m.get_mut(&scene.ocean) {
            m.params = params;
            m.younger = young;
            m.older = old;
        }
        let [source_young, source_old] = deep.source_ages_myr();
        let flags = list_flags(&scene.lists, [deep.age_myr, source_young, source_old]);
        if let Some(mut b) = buffers.get_mut(&scene.plates_buffer) {
            *b = ShaderBuffer::from(plate_gpu_data(&data.crust, &deep));
        }
        if let Some(mut b) = buffers.get_mut(&scene.list_buffer) {
            *b = ShaderBuffer::from(flags);
        }
    }
    if let Some(mut m) = composites_m.get_mut(&scene.composite) {
        m.params = GlobeParams::composite(view.deep_weight, sun).with_debug(dbg);
    }
    if let Some(change) = selection_change(scene.view.as_ref(), &view) {
        let layer = match view.mode {
            GlobeMode::Recent => RECENT_LAYER,
            GlobeMode::Deep => DEEP_LAYER,
            GlobeMode::Bridge => COMPOSITE_LAYER,
        };
        for mut layers in &mut cameras {
            *layers = RenderLayers::from_layers(&[ATMOSPHERE_LAYER, layer]);
        }
        if let Some(mut context) = context {
            context.frame_selection = Some(format!("{:?} {:?}", change.mode, change.frames));
        }
        changes.write(change);
    }
    scene.applied_age_myr = Some(age_myr);
    scene.applied_debug = dbg;
    scene.view = Some(view);
}

/// Copies the sun light's direction into the materials when the light moves.
fn sync_sun(
    lights: Query<&GlobalTransform, (With<DirectionalLight>, Changed<GlobalTransform>)>,
    mut scene: ResMut<GlobeScene>,
    mut plates_m: ResMut<Assets<PlateMaterial>>,
    mut oceans_m: ResMut<Assets<OceanMaterial>>,
    mut recents_m: ResMut<Assets<RecentGlobeMaterial>>,
    mut composites_m: ResMut<Assets<CompositeMaterial>>,
    mut atmospheres_m: ResMut<Assets<AtmosphereMaterial>>,
) {
    let Some(light) = lights.iter().next() else {
        return;
    };
    // A directional light shines along its forward axis; the sun lies the other way.
    let sun = -light.forward().as_vec3();
    if sun.distance(scene.sun) < 1e-6 {
        return;
    }
    scene.sun = sun;
    if let Some(mut m) = plates_m.get_mut(&scene.plate) {
        m.params = m.params.with_sun(sun);
    }
    if let Some(mut m) = oceans_m.get_mut(&scene.ocean) {
        m.params = m.params.with_sun(sun);
    }
    if let Some(mut m) = recents_m.get_mut(&scene.recent) {
        m.params = m.params.with_sun(sun);
    }
    if let Some(mut m) = composites_m.get_mut(&scene.composite) {
        m.params = m.params.with_sun(sun);
    }
    if let Some(mut m) = atmospheres_m.get_mut(&scene.atmosphere) {
        m.params = m.params.with_sun(sun);
    }
}

/// Keeps the bridge cameras on the globe camera's view and size, active only when needed.
#[expect(clippy::type_complexity, reason = "camera queries")]
fn sync_bridge_cameras(
    mut scene: ResMut<GlobeScene>,
    status: Res<GlobeRenderStatus>,
    mut images: ResMut<Assets<Image>>,
    mut composites_m: ResMut<Assets<CompositeMaterial>>,
    main: Query<(&Camera, &Transform, &Projection), (With<GlobeCamera>, Without<BridgeCamera>)>,
    mut bridge: Query<
        (
            &BridgeCamera,
            &mut Camera,
            &mut Transform,
            &mut Projection,
            &mut RenderTarget,
        ),
        Without<GlobeCamera>,
    >,
) {
    let Ok((camera, transform, projection)) = main.single() else {
        return;
    };
    let warming_up = *status == GlobeRenderStatus::PreparingGpu;
    let bridging = scene.view.is_some_and(|v| v.mode == GlobeMode::Bridge);
    let active = warming_up || bridging;
    if active
        && let Some(size) = camera.physical_target_size()
        && size != scene.target_size
        && size.x > 0
        && size.y > 0
    {
        scene.recent_target = images.add(bridge_target(size));
        scene.deep_target = images.add(bridge_target(size));
        scene.target_size = size;
        if let Some(mut m) = composites_m.get_mut(&scene.composite) {
            m.recent_view = scene.recent_target.clone();
            m.deep_view = scene.deep_target.clone();
        }
    }
    for (bridge_camera, mut cam, mut t, mut p, mut target) in &mut bridge {
        if cam.is_active != active {
            cam.is_active = active;
        }
        if !active {
            continue;
        }
        if *t != *transform {
            *t = *transform;
        }
        *p = projection.clone();
        let wanted = if bridge_camera.layer == RECENT_LAYER {
            &scene.recent_target
        } else {
            &scene.deep_target
        };
        if target.as_image() != Some(wanted) {
            *target = RenderTarget::Image(wanted.clone().into());
        }
    }
}

fn track_render_status(
    mut scene: ResMut<GlobeScene>,
    probe: Res<RenderProbe>,
    real_time: Res<Time<Real>>,
    frames: Option<Res<FrameCount>>,
    mut status: ResMut<GlobeRenderStatus>,
    first_frame: Option<ResMut<Messages<FirstInteractiveFrame>>>,
) {
    let frame = frames.map_or(0, |f| f.0);
    let report = probe.report();
    let now = real_time.elapsed_secs_f64();
    scene.progress = track_progress(scene.progress, report.pending, now);
    let stalled = now - scene.progress.last_progress_secs;
    let next = next_render_status(&status, &report, scene.spawned_at, frame, stalled);
    if next == *status {
        return;
    }
    match &next {
        GlobeRenderStatus::Interactive { first_frame: f } => {
            info!(frame = f, "first interactive globe frame");
            if let Some(mut messages) = first_frame {
                messages.write(FirstInteractiveFrame { frame: *f });
            }
        }
        GlobeRenderStatus::Failed(error) => error!(%error, "the globe failed to render"),
        _ => {}
    }
    *status = next;
}

/// Render world: counts frames without pending pipelines and records the first pipeline error.
fn probe_pipelines(cache: Res<PipelineCache>, probe: Res<RenderProbe>) {
    use bevy::render::render_resource::CachedPipelineState;
    let mut pending = cache.waiting_pipelines().count();
    let mut error = None;
    for pipeline in cache.pipelines() {
        match &pipeline.state {
            CachedPipelineState::Queued | CachedPipelineState::Creating(_) => pending += 1,
            // The pipeline cache retries these once the shader assets arrive.
            CachedPipelineState::Err(
                ShaderCacheError::ShaderImportNotYetAvailable
                | ShaderCacheError::ShaderNotLoaded(_),
            ) => pending += 1,
            CachedPipelineState::Err(e) => {
                error.get_or_insert_with(|| e.to_string());
            }
            CachedPipelineState::Ok(_) => {}
        }
    }
    let Ok(mut report) = probe.0.lock() else {
        return;
    };
    report.frames += 1;
    report.pending = pending as u32;
    report.clean_streak = if pending == 0 {
        report.clean_streak + 1
    } else {
        0
    };
    if report.error.is_none() {
        report.error = error;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use timefly_core::surface::{FrameMeta, FrameSource};

    fn timeline() -> Timeline {
        let ice = |age_myr, image_slot| FrameMeta {
            age_myr,
            source: FrameSource::Ice6g,
            image_slot,
        };
        let paleo = |age_myr, source_rotation_age_myr, image_slot| FrameMeta {
            age_myr,
            source: FrameSource::PaleoDem {
                source_rotation_age_myr,
            },
            image_slot,
        };
        Timeline::new(
            vec![
                ice(0.0, 0),
                ice(0.021, 1),
                ice(0.026, 2),
                paleo(0.03, 0.0, 3),
                paleo(66.0, 65.0, 4),
                paleo(540.0, 540.0, 5),
            ],
            vec![(0.0, 0.0), (0.03, -120.0)],
        )
        .unwrap()
    }

    #[test]
    fn view_follows_the_frame_selection() {
        let t = timeline();
        let v = globe_view(&t, 0.0105);
        assert_eq!(v.mode, GlobeMode::Recent);
        assert_eq!(v.recent.unwrap().frames, [0, 1]);
        assert!((v.recent.unwrap().older_weight - 0.5).abs() < 1e-9);
        assert_eq!(v.frames(), vec![0, 1]);

        let v = globe_view(&t, 0.028);
        assert_eq!(v.mode, GlobeMode::Bridge);
        assert!((v.deep_weight - 0.5).abs() < 1e-9);
        assert_eq!(v.recent.unwrap().frames, [2, 2]);
        let deep = v.deep.unwrap();
        assert_eq!(deep.endpoints[0].frame, 3);
        assert!(
            deep.endpoints[0].sea_level_anomaly_m < 0.0,
            "bridge frame gets sea level"
        );
        assert_eq!(v.frames(), vec![2, 3]);

        let v = globe_view(&t, 66.0);
        assert_eq!(v.mode, GlobeMode::Deep);
        let deep = v.deep.unwrap();
        assert_eq!(deep.age_myr, 66.0);
        assert_eq!(deep.source_ages_myr(), [65.0, 65.0]);
        assert_eq!(deep.anomalies_m(), [0.0, 0.0]);
    }

    #[test]
    fn selection_events_fire_on_frame_or_mode_changes_only() {
        let t = timeline();
        let a = globe_view(&t, 0.005);
        let b = globe_view(&t, 0.006);
        assert!(selection_change(None, &a).is_some());
        assert_eq!(selection_change(Some(&a), &b), None, "weight-only change");
        let c = globe_view(&t, 0.022);
        assert_eq!(
            selection_change(Some(&b), &c),
            Some(FrameSelectionChanged {
                mode: GlobeMode::Recent,
                frames: vec![1, 2]
            })
        );
        let d = globe_view(&t, 0.027);
        assert_eq!(
            selection_change(Some(&c), &d).unwrap().mode,
            GlobeMode::Bridge
        );
        let e = globe_view(&t, 0.0299);
        assert_eq!(selection_change(Some(&d), &e), None);
        let f = globe_view(&t, 0.03);
        assert_eq!(
            selection_change(Some(&e), &f).unwrap().mode,
            GlobeMode::Deep
        );
    }

    #[test]
    fn stalled_preparation_fails_after_the_timeout() {
        let preparing = GlobeRenderStatus::PreparingGpu;
        let stuck = RenderProbeReport {
            frames: 500,
            clean_streak: 0,
            pending: 2,
            error: None,
        };
        assert_eq!(
            next_render_status(&preparing, &stuck, 10, 40, 29.0),
            preparing
        );
        assert!(matches!(
            next_render_status(&preparing, &stuck, 10, 40, 31.0),
            GlobeRenderStatus::Failed(m) if m.contains("no progress")
        ));
        // Progress is fewer pending pipelines than ever before; a plateau is not progress.
        let p = track_progress(PrepareProgress::default(), 5, 1.0);
        assert_eq!(
            p,
            PrepareProgress {
                fewest_pending: Some(5),
                last_progress_secs: 1.0
            }
        );
        let p = track_progress(p, 5, 9.0);
        assert_eq!(p.last_progress_secs, 1.0);
        let p = track_progress(p, 3, 12.0);
        assert_eq!(p.last_progress_secs, 12.0);
        let p = track_progress(p, 4, 20.0);
        assert_eq!((p.fewest_pending, p.last_progress_secs), (Some(3), 12.0));
        assert_eq!(track_progress(p, 0, 50.0).last_progress_secs, 50.0);
    }

    #[test]
    fn render_status_waits_for_clean_pipelines_and_reports_errors() {
        let preparing = GlobeRenderStatus::PreparingGpu;
        let report = |frames, clean_streak, error: Option<&str>| RenderProbeReport {
            frames,
            clean_streak,
            pending: 0,
            error: error.map(str::to_string),
        };
        assert_eq!(
            next_render_status(&preparing, &report(12, 5, None), 10, 40, 0.0),
            preparing
        );
        assert_eq!(
            next_render_status(&preparing, &report(20, 1, None), 10, 40, 0.0),
            preparing
        );
        assert_eq!(
            next_render_status(&preparing, &report(20, 3, None), 10, 40, 0.0),
            GlobeRenderStatus::Interactive { first_frame: 40 }
        );
        assert!(matches!(
            next_render_status(&preparing, &report(20, 3, Some("bad shader")), 10, 40, 0.0),
            GlobeRenderStatus::Failed(m) if m.contains("bad shader")
        ));
        let done = GlobeRenderStatus::Interactive { first_frame: 3 };
        assert_eq!(
            next_render_status(&done, &report(99, 0, Some("x")), 10, 40, 0.0),
            done
        );
        assert_eq!(
            next_render_status(
                &GlobeRenderStatus::LoadingData,
                &report(99, 9, None),
                10,
                40,
                0.0
            ),
            GlobeRenderStatus::LoadingData
        );
    }
}
