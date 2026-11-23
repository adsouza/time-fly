//! Regions in the shell (design spec §6.1): turning a [`RegionRequested`] into an
//! [`ActiveRegion`].
//!
//! A request runs through three stages, one job at a time (a new request replaces the job in
//! flight, which cancels it):
//!
//! 1. **Frames.** The globe frames the snapshot's coarse field reads (`frames_needed`), loaded
//!    again as [`CpuGrid`]s; the tile manifest on the first request.
//! 2. **Tiles.** A probe field gives the tiles the inner field reads (`tiles_needed`, which
//!    follows plate rotations). Tiles not in the [`TileCache`] load through the asset server,
//!    checked against the manifest, plus the overview on the first request. A missing or
//!    corrupt tile is a `warn` and makes the region fall back (`fallback == true`), never a
//!    failure.
//! 3. **Generation.** Native: the prep on an
//!    [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool) task, then the rows
//!    across the pool. Web: time-boxed slices of [`WEB_GEN_BUDGET_MS`] per frame.
//!
//! Then [`ActiveRegion`] is inserted, [`RegionGenerated`] and [`RegionEntered`] (or
//! [`RegionSnapshotChanged`], when it replaces a region at another snapshot) are sent, and the
//! state enters [`RegionState::InRegion`]. A [`RegionLeft`] removes the region (or cancels the
//! job) and returns to [`RegionState::Globe`]. A failure (a frame that will not load, an
//! invalid field) logs a `warn` and also returns to the globe.

pub mod frames;
pub mod generate;
pub mod tiles;

use std::sync::Arc;
use std::time::Duration;

use bevy::asset::LoadState;
use bevy::platform::time::Instant;
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::tasks::Task;
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::Grid;
use timefly_core::geo::RenderDir;
use timefly_core::worldgen::coarse::frames_needed;
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::{RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_RES, REGION_RES};
use tracing::instrument;

use crate::data::{GlobeData, describe_load_error};
use crate::diagnostics::aspects::DiagnosticsAppExt;
use frames::{CpuGrid, CpuGridLoader, CpuGridSettings};
#[cfg(not(target_arch = "wasm32"))]
use generate::GenError;
use generate::{BudgetedRun, GenInputs};
use tiles::{
    OVERVIEW_PATH, TILE_MANIFEST_PATH, TileCache, TileManifest, TileManifestLoader, tile_path,
};

/// The web's generation budget per frame (design spec §6.1).
pub const WEB_GEN_BUDGET_MS: f64 = 6.0;
/// The detail seed of every region: a region is a function of its centre and snapshot.
pub const REGION_SEED: u32 = 0x7F1E_0002;

/// Where the player is.
#[derive(States, Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum RegionState {
    #[default]
    Globe,
    /// A region is being prepared (frames, tiles, generation). An [`ActiveRegion`] being
    /// replaced stays until the new one is ready.
    Generating,
    /// [`ActiveRegion`] holds the current region.
    InRegion,
}

/// Request: generate the region centred on `era_dir` (an era-frame direction) at `snapshot`.
/// Accepted in any state; it replaces any request in flight.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionRequested {
    pub era_dir: RenderDir,
    pub snapshot: YearsBp,
}

/// Domain event: a region's grid is ready. `millis` runs from the request to the grid being
/// ready; `tiles_fetched` counts tiles loaded for it (cache hits excluded); `fallback` says some
/// tile it needs is missing, so part of the inner field uses globe-resolution data.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionGenerated {
    pub spec: RegionSpec,
    pub millis: u32,
    pub tiles_fetched: u32,
    pub fallback: bool,
}

/// Domain event: the player entered a region from the globe (or from a region at the same
/// snapshot). Sent with [`RegionGenerated`], when [`ActiveRegion`] is inserted.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionEntered {
    pub spec: RegionSpec,
}

/// Leaving the region, written by whoever leaves (Task 9's `Backspace`, once the view is back on
/// the globe). The plugin then removes [`ActiveRegion`], cancels any job and enters
/// [`RegionState::Globe`]; the logger records it as a domain event.
#[derive(Message, Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct RegionLeft;

/// Domain event: a region replaced the active one at another snapshot (a snapshot change inside
/// a region, design spec §6.2).
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct RegionSnapshotChanged {
    pub from: YearsBp,
    pub to: YearsBp,
    pub spec: RegionSpec,
}

/// The current region: its grid and its frame (local metres ↔ era directions).
#[derive(Resource, Clone, Debug)]
pub struct ActiveRegion {
    pub grid: Arc<RegionGrid>,
    pub frame: RegionFrame,
}

/// How rows are generated.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum GenMode {
    /// Across the async compute pool (native only; the web uses `Budgeted` instead).
    Parallel,
    /// Time-boxed slices on the main thread, `budget` per frame.
    Budgeted { budget: Duration },
}

/// [`WEB_GEN_BUDGET_MS`] as a duration.
pub fn web_budget() -> Duration {
    Duration::from_secs_f64(WEB_GEN_BUDGET_MS / 1000.0)
}

impl GenMode {
    /// The web's time box.
    pub fn web() -> GenMode {
        GenMode::Budgeted {
            budget: web_budget(),
        }
    }
}

impl Default for GenMode {
    fn default() -> Self {
        if cfg!(target_arch = "wasm32") {
            GenMode::web()
        } else {
            GenMode::Parallel
        }
    }
}

/// Region generation settings. The resolutions default to the standard ones; tests lower them.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct RegionSettings {
    pub inner_res: u32,
    pub apron_res: u32,
    pub mode: GenMode,
}

impl Default for RegionSettings {
    fn default() -> Self {
        RegionSettings {
            inner_res: REGION_RES,
            apron_res: APRON_RES,
            mode: GenMode::default(),
        }
    }
}

/// The tile manifest, loaded on the first request and kept.
#[derive(Default)]
enum ManifestState {
    #[default]
    Idle,
    Loading(Handle<TileManifest>),
    Ready(Arc<TileManifest>),
    /// Unavailable this session: every region falls back.
    Failed,
}

#[derive(Resource, Default)]
struct TileIndex {
    manifest: ManifestState,
    /// The overview was requested once; a failure is not retried.
    overview_requested: bool,
}

impl TileIndex {
    fn settled(&self) -> bool {
        matches!(
            self.manifest,
            ManifestState::Ready(_) | ManifestState::Failed
        )
    }

    fn manifest(&self) -> Option<&TileManifest> {
        match &self.manifest {
            ManifestState::Ready(m) => Some(m),
            _ => None,
        }
    }
}

/// A frame file loading for some timeline frames (repeated files share one load).
struct PendingFrame {
    frames: Vec<usize>,
    handle: Handle<CpuGrid>,
}

/// A tile (or, with `key == None`, the overview) loading.
struct PendingTile {
    key: Option<(u8, u8)>,
    path: String,
    bytes: u64,
    handle: Handle<CpuGrid>,
}

enum Running {
    #[cfg(not(target_arch = "wasm32"))]
    Task(Task<Result<RegionGrid, GenError>>),
    Budgeted(BudgetedRun),
}

enum Stage {
    Frames {
        pending: Vec<PendingFrame>,
        frames: Vec<Option<Arc<Grid>>>,
    },
    Tiles {
        frames: Vec<Option<Arc<Grid>>>,
        needed: Vec<(u8, u8)>,
        /// Main-thread time spent finding `needed` (the probe field).
        probe: Duration,
        pending: Vec<PendingTile>,
        fetched: u32,
        bytes: u64,
    },
    Running {
        run: Running,
        tiles_fetched: u32,
        fallback: bool,
    },
}

/// The request in flight.
#[derive(Resource)]
struct RegionJob {
    spec: RegionSpec,
    started: Instant,
    /// The snapshot of the active region this job replaces.
    replaces: Option<YearsBp>,
    stage: Stage,
}

pub struct RegionPlugin;

impl Plugin for RegionPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<CpuGrid>()
            .init_asset_loader::<CpuGridLoader>()
            .init_asset::<TileManifest>()
            .init_asset_loader::<TileManifestLoader>()
            .init_resource::<TileCache>()
            .init_resource::<TileIndex>()
            .init_resource::<RegionSettings>()
            .add_logged_state::<RegionState>()
            .add_domain_event::<RegionRequested>()
            .add_domain_event::<RegionGenerated>()
            .add_domain_event::<RegionEntered>()
            .add_domain_event::<RegionLeft>()
            .add_domain_event::<RegionSnapshotChanged>()
            .add_systems(
                Update,
                (
                    leave_region,
                    start_region_request,
                    poll_tile_manifest,
                    advance_frames,
                    advance_tiles,
                    poll_generation,
                )
                    .chain(),
            );
    }
}

fn leave_region(
    mut left: MessageReader<RegionLeft>,
    mut commands: Commands,
    mut next: ResMut<NextState<RegionState>>,
) {
    if left.read().count() == 0 {
        return;
    }
    commands.remove_resource::<ActiveRegion>();
    commands.remove_resource::<RegionJob>();
    next.set_if_different(RegionState::Globe);
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn start_region_request(
    mut requests: MessageReader<RegionRequested>,
    mut commands: Commands,
    globe: Option<Res<GlobeData>>,
    server: Res<AssetServer>,
    settings: Res<RegionSettings>,
    mut index: ResMut<TileIndex>,
    active: Option<Res<ActiveRegion>>,
    mut next: ResMut<NextState<RegionState>>,
) {
    let Some(request) = requests.read().last().copied() else {
        return;
    };
    let Some(globe) = globe else {
        warn!(
            ?request,
            "region requested before the globe data loaded; ignored"
        );
        return;
    };
    let center = request.era_dir.0.normalize_or_zero();
    if center == bevy::math::DVec3::ZERO || !center.is_finite() {
        warn!(
            ?request,
            "region requested at an invalid direction; ignored"
        );
        return;
    }
    let spec = RegionSpec {
        center: RenderDir(center),
        snapshot: request.snapshot,
        seed: REGION_SEED,
        inner_res: settings.inner_res,
        apron_res: settings.apron_res,
    };
    if matches!(index.manifest, ManifestState::Idle) {
        index.manifest = ManifestState::Loading(server.load(TILE_MANIFEST_PATH));
    }
    let timeline = &globe.timeline;
    let mut pending: Vec<PendingFrame> = Vec::new();
    for frame in frames_needed(timeline, spec.snapshot.myr()) {
        let slot = timeline.frames()[frame].image_slot;
        match pending
            .iter_mut()
            .find(|p| timeline.frames()[p.frames[0]].image_slot == slot)
        {
            Some(p) => p.frames.push(frame),
            None => pending.push(PendingFrame {
                frames: vec![frame],
                handle: server.load::<CpuGrid>(globe.frame_path(frame).clone()),
            }),
        }
    }
    let replaces = active
        .map(|a| a.grid.spec.snapshot)
        .filter(|&s| s != spec.snapshot);
    commands.insert_resource(RegionJob {
        spec,
        started: Instant::now(),
        replaces,
        stage: Stage::Frames {
            pending,
            frames: vec![None; timeline.frames().len()],
        },
    });
    next.set_if_different(RegionState::Generating);
}

fn poll_tile_manifest(
    mut index: ResMut<TileIndex>,
    server: Res<AssetServer>,
    mut manifests: ResMut<Assets<TileManifest>>,
) {
    let ManifestState::Loading(handle) = &index.manifest else {
        return;
    };
    index.manifest = match server.load_state(handle) {
        LoadState::Loaded => match manifests.remove(handle) {
            Some(manifest) => {
                debug!(
                    tiles = manifest.tiles.len(),
                    overview = manifest.overview.is_some(),
                    "tile manifest loaded"
                );
                ManifestState::Ready(Arc::new(manifest))
            }
            None => ManifestState::Failed,
        },
        LoadState::Failed(e) => {
            let error = describe_load_error(TILE_MANIFEST_PATH, &e);
            warn!(
                path = TILE_MANIFEST_PATH,
                error = error.trim_end(),
                "tile manifest unavailable; regions use globe-resolution data"
            );
            ManifestState::Failed
        }
        _ => return,
    };
}

/// Abandons the job: a `warn`, and back to the globe (never a stale region or a stuck
/// "descending…").
fn fail(commands: &mut Commands, next: &mut NextState<RegionState>, why: &str) {
    warn!(error = why, "region generation failed; back to the globe");
    commands.remove_resource::<RegionJob>();
    commands.remove_resource::<ActiveRegion>();
    next.set_if_different(RegionState::Globe);
}

/// Takes a loaded CPU grid out of the asset storage (dropping the caller's handle afterwards
/// leaves no copy behind).
fn take_grid(grids: &mut Assets<CpuGrid>, handle: &Handle<CpuGrid>) -> Option<Arc<Grid>> {
    grids.remove(handle).map(|CpuGrid(grid)| Arc::new(grid))
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn advance_frames(
    job: Option<ResMut<RegionJob>>,
    mut commands: Commands,
    server: Res<AssetServer>,
    mut grids: ResMut<Assets<CpuGrid>>,
    globe: Option<Res<GlobeData>>,
    mut index: ResMut<TileIndex>,
    cache: Res<TileCache>,
    mut next: ResMut<NextState<RegionState>>,
) {
    let Some(mut job) = job else { return };
    let spec = job.spec;
    let Stage::Frames { pending, frames } = &mut job.stage else {
        return;
    };
    let mut failure = None;
    pending.retain(|p| match server.load_state(&p.handle) {
        LoadState::Loaded => match take_grid(&mut grids, &p.handle) {
            Some(grid) => {
                for &f in &p.frames {
                    frames[f] = Some(grid.clone());
                }
                false
            }
            None => {
                failure = Some(format!("frame {} vanished after loading", p.frames[0]));
                false
            }
        },
        LoadState::Failed(e) => {
            let path = server.get_path(&p.handle).map(|p| p.to_string());
            failure = Some(describe_load_error(path.as_deref().unwrap_or("?"), &e));
            false
        }
        _ => true,
    });
    if let Some(why) = failure {
        return fail(&mut commands, &mut next, why.trim_end());
    }
    if !pending.is_empty() || !index.settled() {
        return;
    }
    let Some(globe) = globe else {
        return fail(&mut commands, &mut next, "the globe data is gone");
    };
    let frames = std::mem::take(frames);
    let probe_started = Instant::now();
    let needed = match generate::tiles_needed(&spec, &globe.timeline, &globe.crust, &frames) {
        Ok(needed) => needed,
        Err(e) => return fail(&mut commands, &mut next, &e.to_string()),
    };
    let probe = probe_started.elapsed();
    let mut pending = Vec::new();
    if let Some(manifest) = index.manifest() {
        for &(row, col) in &needed {
            if cache.contains((row, col)) {
                continue;
            }
            let path = tile_path(row, col);
            match manifest.tiles.get(&(row, col)) {
                Some(entry) => pending.push(PendingTile {
                    key: Some((row, col)),
                    handle: load_checked(&server, &path, entry.settings()),
                    path,
                    bytes: entry.bytes,
                }),
                None => warn!(
                    tile = path,
                    "terrain tile is not in the tile manifest; this region falls back to \
                     globe-resolution data"
                ),
            }
        }
        if cache.overview().is_none() && !index.overview_requested {
            if let Some(entry) = &manifest.overview {
                pending.push(PendingTile {
                    key: None,
                    handle: load_checked(&server, OVERVIEW_PATH, entry.settings()),
                    path: OVERVIEW_PATH.to_string(),
                    bytes: entry.bytes,
                });
            } else {
                warn!("the tile manifest lists no overview; region aprons use the globe frames");
            }
        }
    }
    index.overview_requested = true;
    job.stage = Stage::Tiles {
        frames,
        needed,
        probe,
        pending,
        fetched: 0,
        bytes: 0,
    };
}

fn load_checked(server: &AssetServer, path: &str, settings: CpuGridSettings) -> Handle<CpuGrid> {
    server
        .load_builder()
        .with_settings(move |s: &mut CpuGridSettings| *s = settings.clone())
        .load::<CpuGrid>(path.to_string())
}

/// Takes in one finished tile or overview load; `true` if it is now in the cache.
#[instrument(level = "debug", skip_all, fields(path = %tile.path), ret)]
fn receive_tile(
    tile: &PendingTile,
    state: LoadState,
    grids: &mut Assets<CpuGrid>,
    cache: &mut TileCache,
    needed: &[(u8, u8)],
) -> bool {
    let problem = match state {
        LoadState::Loaded => match take_grid(grids, &tile.handle) {
            None => "the grid vanished after loading".to_string(),
            Some(grid) => {
                let stored = match tile.key {
                    Some(key) => cache.insert(key, grid, needed),
                    None => cache.set_overview(grid),
                };
                match stored {
                    Ok(()) => return true,
                    Err(e) => e.to_string(),
                }
            }
        },
        LoadState::Failed(e) => describe_load_error(&tile.path, &e).trim_end().to_string(),
        _ => unreachable!("called only for finished loads"),
    };
    if tile.key.is_some() {
        warn!(
            tile = tile.path,
            error = problem,
            "terrain tile unavailable; this region falls back to globe-resolution data"
        );
    } else {
        warn!(
            path = tile.path,
            error = problem,
            "terrain overview unavailable; region aprons use the globe frames"
        );
    }
    false
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn advance_tiles(
    job: Option<ResMut<RegionJob>>,
    server: Res<AssetServer>,
    mut grids: ResMut<Assets<CpuGrid>>,
    mut cache: ResMut<TileCache>,
    globe: Option<Res<GlobeData>>,
    settings: Res<RegionSettings>,
    mut commands: Commands,
    mut next: ResMut<NextState<RegionState>>,
) {
    let Some(mut job) = job else { return };
    let spec = job.spec;
    let Stage::Tiles {
        frames,
        needed,
        probe,
        pending,
        fetched,
        bytes,
    } = &mut job.stage
    else {
        return;
    };
    pending.retain(|tile| {
        let state = server.load_state(&tile.handle);
        if !matches!(state, LoadState::Loaded | LoadState::Failed(_)) {
            return true;
        }
        if receive_tile(tile, state, &mut grids, &mut cache, needed) && tile.key.is_some() {
            *fetched += 1;
            *bytes += tile.bytes;
        }
        false
    });
    if !pending.is_empty() {
        return;
    }
    let Some(globe) = globe else {
        return fail(&mut commands, &mut next, "the globe data is gone");
    };
    let tiles = cache.tile_set(needed);
    let missing = tiles.missing(needed);
    let fallback = !missing.is_empty();
    info!(
        needed = needed.len(),
        fetched = *fetched,
        bytes = *bytes,
        missing = missing.len(),
        overview = tiles.has_overview(),
        cached = cache.len(),
        probe_ms = format!("{:.1}", probe.as_secs_f64() * 1e3),
        "region tiles ready"
    );
    let inputs = GenInputs {
        spec,
        timeline: globe.timeline.clone(),
        crust: globe.crust.clone(),
        tiles,
        frames: std::mem::take(frames),
        fallback,
    };
    let run = match settings.mode {
        #[cfg(not(target_arch = "wasm32"))]
        GenMode::Parallel => Running::Task(
            bevy::tasks::AsyncComputeTaskPool::get()
                .spawn(async move { generate::generate_parallel(&inputs) }),
        ),
        #[cfg(target_arch = "wasm32")]
        GenMode::Parallel => Running::Budgeted(BudgetedRun::new(inputs, web_budget())),
        GenMode::Budgeted { budget } => Running::Budgeted(BudgetedRun::new(inputs, budget)),
    };
    job.stage = Stage::Running {
        run,
        tiles_fetched: *fetched,
        fallback,
    };
}

fn poll_generation(
    job: Option<ResMut<RegionJob>>,
    mut commands: Commands,
    mut generated: MessageWriter<RegionGenerated>,
    mut entered: MessageWriter<RegionEntered>,
    mut changed: MessageWriter<RegionSnapshotChanged>,
    mut next: ResMut<NextState<RegionState>>,
) {
    let Some(mut job) = job else { return };
    let job = &mut *job;
    let Stage::Running {
        run,
        tiles_fetched,
        fallback,
    } = &mut job.stage
    else {
        return;
    };
    let result = match run {
        #[cfg(not(target_arch = "wasm32"))]
        Running::Task(task) => bevy::tasks::futures::check_ready(task),
        Running::Budgeted(run) => run.poll_slice(),
    };
    let Some(result) = result else { return };
    let grid = match result {
        Ok(grid) => grid,
        Err(e) => return fail(&mut commands, &mut next, &e.to_string()),
    };
    let spec = job.spec;
    let millis = u32::try_from(job.started.elapsed().as_millis()).unwrap_or(u32::MAX);
    commands.insert_resource(ActiveRegion {
        frame: spec.frame(),
        grid: Arc::new(grid),
    });
    generated.write(RegionGenerated {
        spec,
        millis,
        tiles_fetched: *tiles_fetched,
        fallback: *fallback,
    });
    match job.replaces {
        Some(from) => {
            changed.write(RegionSnapshotChanged {
                from,
                to: spec.snapshot,
                spec,
            });
        }
        None => {
            entered.write(RegionEntered { spec });
        }
    }
    commands.remove_resource::<RegionJob>();
    next.set_if_different(RegionState::InRegion);
}
