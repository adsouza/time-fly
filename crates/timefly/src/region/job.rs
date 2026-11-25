//! The request in flight and the systems that move it through its stages (frames → tiles →
//! generation), plus the session-wide tile manifest and overview loads, which live in
//! [`TileIndex`] so that cancelling a job never cancels them.

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
use timefly_core::surface::Timeline;
use timefly_core::worldgen::coarse::snapshot_sea_level_anomaly_m;
#[cfg(not(target_arch = "wasm32"))]
use timefly_core::worldgen::generate::RegionGrid;
use timefly_core::worldgen::generate::RegionSpec;
use tracing::instrument;

use super::frames::{CpuGrid, CpuGridSettings, FrameCache, FrameLoads, take_grid};
#[cfg(not(target_arch = "wasm32"))]
use super::generate::GenError;
use super::generate::{self, BudgetedRun, GenInputs};
use super::tiles::{OVERVIEW_PATH, TILE_MANIFEST_PATH, TileCache, TileManifest, tile_path};
#[cfg(target_arch = "wasm32")]
use super::web_budget;
use super::{
    ActiveRegion, GenMode, LargeGenBudget, PendingRegion, REGION_SEED, RegionEntered,
    RegionGenerated, RegionLeft, RegionPreloadCancelled, RegionPreloadFailed, RegionRequested,
    RegionSettings, RegionSnapshotChanged, RegionState, RegionSwapRequested, RequestPurpose,
    gen_budget,
};
use crate::clock::LoadClock;
#[cfg(test)]
use crate::clock::{LOAD_CLOCK_MAX_DELTA_S, advance_load_clock};
use crate::data::{GlobeData, describe_load_error};
use crate::hydro::{HydroGate, HydroWait};

/// A load that has not finished after this long is abandoned, so a stalled fetch (not a 404,
/// which fails at once) never leaves "Descending..." up: a tile or the overview counts as
/// missing, the manifest as unavailable, and a frame fails the request. Measured on the
/// [`LoadClock`]: 20 s of running frames, so a paused or throttled page (a hidden tab, a laptop
/// asleep, a host that stops `requestAnimationFrame`) does not time out its loads.
pub const LOAD_TIMEOUT_S: f64 = 20.0;
/// The slowest transfer a large file is allowed before it times out (bytes per second, about
/// 4 Mbit/s): the 12.4 MB overview gets about 25 s, the tiles (about 100 kB) the plain
/// [`LOAD_TIMEOUT_S`].
pub const MIN_LOAD_RATE_BYTES_PER_S: f64 = 500_000.0;

/// How long a load of a `bytes`-long file may take (unknown size: [`LOAD_TIMEOUT_S`]).
pub fn load_timeout_s(bytes: Option<u64>) -> f64 {
    bytes.map_or(LOAD_TIMEOUT_S, |b| {
        LOAD_TIMEOUT_S.max(b as f64 / MIN_LOAD_RATE_BYTES_PER_S)
    })
}

/// Whether a load started at `started_s` has timed out at `now_s` ([`LoadClock`] seconds).
pub fn load_timed_out(started_s: f64, now_s: f64, bytes: Option<u64>) -> bool {
    now_s - started_s >= load_timeout_s(bytes)
}

/// A load in flight: its handle and when it started ([`LoadClock`] seconds).
struct Loading<A: Asset> {
    handle: Handle<A>,
    started_s: f64,
}

/// The tile manifest, loaded once the globe data is ready and kept.
#[derive(Default)]
enum ManifestState {
    #[default]
    Idle,
    Loading(Loading<TileManifest>),
    Ready(Arc<TileManifest>),
    /// Unavailable this session: every region falls back.
    Failed,
}

/// The overview, loaded once the manifest is ready and kept in the [`TileCache`].
#[derive(Default)]
enum OverviewState {
    #[default]
    Idle,
    /// With the file's size from the manifest.
    Loading(Loading<CpuGrid>, u64),
    /// In the [`TileCache`].
    Ready,
    /// Unavailable this session (missing, corrupt, or not in the manifest): aprons use the
    /// globe frames.
    Failed,
}

/// Session-wide tile data loads, started once the globe data is ready (the continental pass
/// reads the overview with no region requested). They belong to no job, so a superseded or
/// abandoned request never cancels them.
#[derive(Resource, Default)]
pub(crate) struct TileIndex {
    manifest: ManifestState,
    overview: OverviewState,
}

impl TileIndex {
    fn manifest_settled(&self) -> bool {
        matches!(
            self.manifest,
            ManifestState::Ready(_) | ManifestState::Failed
        )
    }

    /// Whether the overview is in the [`TileCache`] or unavailable for the session.
    pub(crate) fn overview_settled(&self) -> bool {
        matches!(self.overview, OverviewState::Ready | OverviewState::Failed)
    }

    fn manifest(&self) -> Option<&TileManifest> {
        match &self.manifest {
            ManifestState::Ready(m) => Some(m),
            _ => None,
        }
    }
}

/// A tile loading.
struct PendingTile {
    key: (u8, u8),
    path: String,
    bytes: u64,
    handle: Handle<CpuGrid>,
    started_s: f64,
}

enum Running {
    #[cfg(not(target_arch = "wasm32"))]
    Task(Task<Result<RegionGrid, GenError>>),
    Budgeted(BudgetedRun),
}

enum Stage {
    Frames(FrameLoads),
    Tiles {
        frames: Vec<Option<Arc<Grid>>>,
        needed: Vec<(u8, u8)>,
        /// Main-thread time spent finding `needed` (the probe field).
        probe: Duration,
        pending: Vec<PendingTile>,
        fetched: u32,
        bytes: u64,
        /// The wait for the era's continental pass, once the tiles are in.
        hydro: HydroWait,
    },
    Running {
        run: Running,
        tiles_fetched: u32,
        bytes_fetched: u64,
        fallback: bool,
        /// How long the job waited for the era's continental pass.
        hydro_wait: Duration,
        hydrology: bool,
    },
}

/// The request in flight. Replacing or removing it drops its handles and task, which cancels
/// its loads and its run.
#[derive(Resource)]
pub(crate) struct RegionJob {
    spec: RegionSpec,
    purpose: RequestPurpose,
    started: Instant,
    /// The snapshot of the active region this job replaces.
    replaces: Option<YearsBp>,
    /// Frame files it loads (not in the frame cache).
    frames_fetched: u32,
    stage: Stage,
}

impl RegionJob {
    /// Its purpose and stage, for diagnostics: `neighbour/frames`, `enter/running`,
    /// `snapshot/hydro` (waiting for the era's continental pass) and so on.
    pub(crate) fn describe(&self) -> &'static str {
        use RequestPurpose::{Enter, Neighbour, Snapshot};
        let waiting = matches!(&self.stage, Stage::Tiles { hydro, .. } if hydro.waiting());
        match (self.purpose, &self.stage) {
            (Enter, Stage::Frames(_)) => "enter/frames",
            (Enter, Stage::Tiles { .. }) if waiting => "enter/hydro",
            (Enter, Stage::Tiles { .. }) => "enter/tiles",
            (Enter, Stage::Running { .. }) => "enter/running",
            (Snapshot, Stage::Frames(_)) => "snapshot/frames",
            (Snapshot, Stage::Tiles { .. }) if waiting => "snapshot/hydro",
            (Snapshot, Stage::Tiles { .. }) => "snapshot/tiles",
            (Snapshot, Stage::Running { .. }) => "snapshot/running",
            (Neighbour, Stage::Frames(_)) => "neighbour/frames",
            (Neighbour, Stage::Tiles { .. }) if waiting => "neighbour/hydro",
            (Neighbour, Stage::Tiles { .. }) => "neighbour/tiles",
            (Neighbour, Stage::Running { .. }) => "neighbour/running",
        }
    }
}

/// The spec a request asks for, or `None` for a direction that is not one. Its sea-level
/// anomaly (for the glacial snowline, ruling R18) is the one its heights carry
/// ([`snapshot_sea_level_anomaly_m`]: weighted by the bridge endpoint in deep brackets that
/// read it, 0 from the first PaleoDEM bracket on).
pub(super) fn spec_for(
    request: &RegionRequested,
    settings: &RegionSettings,
    timeline: &Timeline,
) -> Option<RegionSpec> {
    let center = request.era_dir.0.normalize_or_zero();
    (center != bevy::math::DVec3::ZERO && center.is_finite()).then_some(RegionSpec {
        center: RenderDir(center),
        snapshot: request.snapshot,
        seed: REGION_SEED,
        inner_res: settings.inner_res,
        apron_res: settings.apron_res,
        sea_level_anomaly_m: snapshot_sea_level_anomaly_m(timeline, request.snapshot.myr()),
    })
}

pub(super) fn leave_region(
    mut left: MessageReader<RegionLeft>,
    mut commands: Commands,
    mut pending: ResMut<PendingRegion>,
    mut next: ResMut<NextState<RegionState>>,
) {
    if left.read().count() == 0 {
        return;
    }
    commands.remove_resource::<ActiveRegion>();
    // Any job: an enter or snapshot one, or a neighbour's. A ready neighbour goes too.
    commands.remove_resource::<RegionJob>();
    pending.region = None;
    next.set_if_different(RegionState::Globe);
}

/// [`RegionPreloadCancelled`]: drops the neighbour job in flight (never an enter or snapshot
/// job) and a ready [`PendingRegion`]. The active region and the state are untouched.
pub(super) fn cancel_preload(
    mut cancelled: MessageReader<RegionPreloadCancelled>,
    job: Option<Res<RegionJob>>,
    mut pending: ResMut<PendingRegion>,
    mut commands: Commands,
) {
    if cancelled.read().count() == 0 {
        return;
    }
    if job.is_some_and(|j| j.purpose == RequestPurpose::Neighbour) {
        commands.remove_resource::<RegionJob>();
    }
    pending.region = None;
}

/// [`RegionSwapRequested`]: promotes the pending neighbour to the active region in this frame
/// and sends [`RegionEntered`]. The caller gates it on an opaque cloud.
pub(super) fn swap_region(
    mut swaps: MessageReader<RegionSwapRequested>,
    mut pending: ResMut<PendingRegion>,
    mut commands: Commands,
    mut entered: MessageWriter<RegionEntered>,
    mut next: ResMut<NextState<RegionState>>,
) {
    if swaps.read().count() == 0 {
        return;
    }
    let Some(region) = pending.region.take() else {
        warn!("a region swap was requested with no neighbour ready; ignored");
        return;
    };
    entered.write(RegionEntered {
        spec: region.grid.spec,
        fallback: pending.fallback,
    });
    commands.insert_resource(region);
    next.set_if_different(RegionState::InRegion);
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn start_region_request(
    mut requests: MessageReader<RegionRequested>,
    mut commands: Commands,
    globe: Option<Res<GlobeData>>,
    server: Res<AssetServer>,
    settings: Res<RegionSettings>,
    mut frame_cache: ResMut<FrameCache>,
    active: Option<Res<ActiveRegion>>,
    job: Option<Res<RegionJob>>,
    mut pending: ResMut<PendingRegion>,
    mut failed: MessageWriter<RegionPreloadFailed>,
    mut next: ResMut<NextState<RegionState>>,
    clock: Res<LoadClock>,
) {
    let now_s = clock.elapsed_s;
    // The last enter or snapshot request wins over any neighbour one in the same frame; else the
    // last neighbour request.
    let (mut main, mut neighbour) = (None, None);
    for request in requests.read().copied() {
        match request.purpose {
            RequestPurpose::Neighbour => neighbour = Some(request),
            _ => main = Some(request),
        }
    }
    // Every neighbour request is answered, ready or failed: one the job will not take fails
    // at once, so whoever asked (a hop) never waits on it.
    if main.is_some()
        && let Some(dropped) = neighbour
    {
        debug!(request = ?dropped, "neighbour requested with an enter or snapshot request; dropped");
        failed.write(RegionPreloadFailed {
            reason: "an enter or snapshot request came with it".to_string(),
        });
    }
    let Some(request) = main.or(neighbour) else {
        return;
    };
    let purpose = request.purpose;
    let preload = purpose == RequestPurpose::Neighbour;
    if preload {
        // The concurrency rule: a neighbour never competes with an enter or snapshot job, and
        // needs a region to be a neighbour of. A neighbour job in flight is replaced below.
        let busy = job.is_some_and(|j| j.purpose != RequestPurpose::Neighbour);
        if busy || active.is_none() {
            debug!(
                ?request,
                "neighbour requested while a region job runs or no region is active; ignored"
            );
            failed.write(RegionPreloadFailed {
                reason: if busy {
                    "an enter or snapshot job is running"
                } else {
                    "no region is active"
                }
                .to_string(),
            });
            return;
        }
    }
    let Some(globe) = globe else {
        warn!(
            ?request,
            "region requested before the globe data loaded; ignored"
        );
        if preload {
            failed.write(RegionPreloadFailed {
                reason: "the globe data is not loaded".to_string(),
            });
        }
        return;
    };
    let Some(spec) = spec_for(&request, &settings, &globe.timeline) else {
        warn!(
            ?request,
            "region requested at an invalid direction; ignored"
        );
        if preload {
            failed.write(RegionPreloadFailed {
                reason: "the neighbour's direction is invalid".to_string(),
            });
        }
        return;
    };
    // A new job supersedes any ready neighbour, which may be at another snapshot.
    pending.region = None;
    let loads = FrameLoads::start(
        &globe,
        spec.snapshot.myr(),
        &server,
        &mut frame_cache,
        now_s,
    );
    let replaces = active
        .filter(|_| !preload)
        .map(|a| a.grid.spec.snapshot)
        .filter(|&s| s != spec.snapshot);
    commands.insert_resource(RegionJob {
        spec,
        purpose,
        started: Instant::now(),
        replaces,
        frames_fetched: loads.fetched(),
        stage: Stage::Frames(loads),
    });
    if !preload {
        next.set_if_different(RegionState::Generating);
    }
}

/// Starts the tile manifest's load once the globe data is ready (before any region request:
/// the continental pass reads the overview), and takes it in when it arrives.
pub(super) fn poll_tile_manifest(
    mut index: ResMut<TileIndex>,
    server: Res<AssetServer>,
    mut manifests: ResMut<Assets<TileManifest>>,
    globe: Option<Res<GlobeData>>,
    clock: Res<LoadClock>,
) {
    if matches!(index.manifest, ManifestState::Idle) && globe.is_some() {
        index.manifest = ManifestState::Loading(Loading {
            handle: server.load(TILE_MANIFEST_PATH),
            started_s: clock.elapsed_s,
        });
    }
    let ManifestState::Loading(Loading { handle, started_s }) = &index.manifest else {
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
        _ if load_timed_out(*started_s, clock.elapsed_s, None) => {
            warn!(
                path = TILE_MANIFEST_PATH,
                timeout_s = LOAD_TIMEOUT_S,
                "tile manifest load timed out; regions use globe-resolution data"
            );
            ManifestState::Failed
        }
        _ => return,
    };
}

/// Starts the overview's load once the manifest is ready, and stores it in the [`TileCache`]
/// when it arrives. A failure is a `warn` and is not retried this session.
///
/// The loader checks the overview's SHA-256 (12.4 MB) and decodes it (37 MB) in one piece. On
/// the web that runs on the main thread during the first region's load. Task 10 gates it (ruling
/// R11): in Chrome WebGPU on the M4, no single frame of the first region's load may exceed
/// 250 ms; if one does, the overview is split into latitude bands or decoded in chunks.
pub(super) fn poll_overview(
    mut index: ResMut<TileIndex>,
    server: Res<AssetServer>,
    mut grids: ResMut<Assets<CpuGrid>>,
    mut cache: ResMut<TileCache>,
    clock: Res<LoadClock>,
) {
    let now_s = clock.elapsed_s;
    let next = match &index.overview {
        OverviewState::Idle => match &index.manifest {
            ManifestState::Ready(manifest) => match &manifest.overview {
                Some(entry) => OverviewState::Loading(
                    Loading {
                        handle: load_checked(
                            &server,
                            &manifest.payload_path(OVERVIEW_PATH),
                            entry.settings(),
                        ),
                        started_s: now_s,
                    },
                    entry.bytes,
                ),
                None => {
                    warn!(
                        "the tile manifest lists no overview; region aprons use the globe frames"
                    );
                    OverviewState::Failed
                }
            },
            ManifestState::Failed => OverviewState::Failed,
            _ => return,
        },
        OverviewState::Loading(Loading { handle, started_s }, bytes) => {
            let problem = match server.load_state(handle) {
                LoadState::Loaded => match take_grid(&mut grids, handle) {
                    Some(grid) => match cache.set_overview(grid) {
                        Ok(()) => None,
                        Err(e) => Some(e.to_string()),
                    },
                    None => Some("the grid vanished after loading".to_string()),
                },
                LoadState::Failed(e) => Some(
                    describe_load_error(OVERVIEW_PATH, &e)
                        .trim_end()
                        .to_string(),
                ),
                _ if load_timed_out(*started_s, now_s, Some(*bytes)) => Some(format!(
                    "the load timed out after {:.0} s",
                    load_timeout_s(Some(*bytes))
                )),
                _ => return,
            };
            match problem {
                None => OverviewState::Ready,
                Some(error) => {
                    warn!(
                        path = OVERVIEW_PATH,
                        error, "terrain overview unavailable; region aprons use the globe frames"
                    );
                    OverviewState::Failed
                }
            }
        }
        OverviewState::Ready | OverviewState::Failed => return,
    };
    index.overview = next;
}

/// Abandons the job: a `warn`, and back to the globe (never a stale region or a stuck
/// "descending…"). A neighbour job only reports [`RegionPreloadFailed`]: the region the player
/// is in stays.
fn fail(
    commands: &mut Commands,
    next: &mut NextState<RegionState>,
    preload_failed: &mut MessageWriter<RegionPreloadFailed>,
    purpose: RequestPurpose,
    why: &str,
) {
    commands.remove_resource::<RegionJob>();
    if purpose == RequestPurpose::Neighbour {
        warn!(error = why, "neighbour preload failed; the region stays");
        preload_failed.write(RegionPreloadFailed {
            reason: why.to_string(),
        });
        return;
    }
    warn!(error = why, "region generation failed; back to the globe");
    commands.remove_resource::<ActiveRegion>();
    next.set_if_different(RegionState::Globe);
}

fn load_checked(server: &AssetServer, path: &str, settings: CpuGridSettings) -> Handle<CpuGrid> {
    server
        .load_builder()
        .with_settings(move |s: &mut CpuGridSettings| *s = settings.clone())
        .load::<CpuGrid>(path.to_string())
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn advance_frames(
    job: Option<ResMut<RegionJob>>,
    mut commands: Commands,
    server: Res<AssetServer>,
    mut grids: ResMut<Assets<CpuGrid>>,
    mut frame_cache: ResMut<FrameCache>,
    globe: Option<Res<GlobeData>>,
    index: Res<TileIndex>,
    cache: Res<TileCache>,
    mut next: ResMut<NextState<RegionState>>,
    mut preload_failed: MessageWriter<RegionPreloadFailed>,
    clock: Res<LoadClock>,
) {
    let now_s = clock.elapsed_s;
    let Some(mut job) = job else { return };
    let spec = job.spec;
    let purpose = job.purpose;
    let Stage::Frames(loads) = &mut job.stage else {
        return;
    };
    // The manifest loads with the globe, so it has almost always settled by now.
    if !index.manifest_settled() {
        return;
    }
    let frames = match loads.poll(&server, &mut grids, &mut frame_cache, now_s) {
        None => return,
        Some(Err(why)) => {
            return fail(&mut commands, &mut next, &mut preload_failed, purpose, &why);
        }
        Some(Ok(frames)) => frames,
    };
    let Some(globe) = globe else {
        return fail(
            &mut commands,
            &mut next,
            &mut preload_failed,
            purpose,
            "the globe data is gone",
        );
    };
    let probe_started = Instant::now();
    let needed = match generate::tiles_needed(&spec, &globe.timeline, &globe.crust, &frames) {
        Ok(needed) => needed,
        Err(e) => {
            return fail(
                &mut commands,
                &mut next,
                &mut preload_failed,
                purpose,
                &e.to_string(),
            );
        }
    };
    let probe = probe_started.elapsed();
    let mut pending = Vec::new();
    if let Some(manifest) = index.manifest() {
        for &(row, col) in &needed {
            if cache.contains((row, col)) {
                continue;
            }
            let path = manifest.payload_path(&tile_path(row, col));
            match manifest.tiles.get(&(row, col)) {
                Some(entry) => pending.push(PendingTile {
                    key: (row, col),
                    handle: load_checked(&server, &path, entry.settings()),
                    path,
                    bytes: entry.bytes,
                    started_s: now_s,
                }),
                None => warn!(
                    tile = path,
                    "terrain tile is not in the tile manifest; this region falls back to \
                     globe-resolution data"
                ),
            }
        }
    }
    job.stage = Stage::Tiles {
        frames,
        needed,
        probe,
        pending,
        fetched: 0,
        bytes: 0,
        hydro: HydroWait::default(),
    };
}

/// Takes in one finished (or timed-out) tile load; `true` if the tile is now in the cache.
#[instrument(level = "debug", skip_all, fields(path = %tile.path, timed_out = timed_out), ret)]
fn receive_tile(
    tile: &PendingTile,
    state: LoadState,
    timed_out: bool,
    grids: &mut Assets<CpuGrid>,
    cache: &mut TileCache,
    needed: &[(u8, u8)],
) -> bool {
    let problem = match state {
        _ if timed_out => format!(
            "the load timed out after {:.0} s",
            load_timeout_s(Some(tile.bytes))
        ),
        LoadState::Loaded => match take_grid(grids, &tile.handle) {
            None => "the grid vanished after loading".to_string(),
            Some(grid) => match cache.insert(tile.key, grid, needed) {
                Ok(()) => return true,
                Err(e) => e.to_string(),
            },
        },
        LoadState::Failed(e) => describe_load_error(&tile.path, &e).trim_end().to_string(),
        _ => unreachable!("called only for finished or timed-out loads"),
    };
    warn!(
        tile = tile.path,
        error = problem,
        "terrain tile unavailable; this region falls back to globe-resolution data"
    );
    false
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn advance_tiles(
    job: Option<ResMut<RegionJob>>,
    server: Res<AssetServer>,
    mut grids: ResMut<Assets<CpuGrid>>,
    mut cache: ResMut<TileCache>,
    index: Res<TileIndex>,
    globe: Option<Res<GlobeData>>,
    settings: Res<RegionSettings>,
    mut commands: Commands,
    mut next: ResMut<NextState<RegionState>>,
    mut preload_failed: MessageWriter<RegionPreloadFailed>,
    clock: Res<LoadClock>,
    mut hydro_gate: HydroGate,
) {
    let now_s = clock.elapsed_s;
    let Some(mut job) = job else { return };
    let spec = job.spec;
    let purpose = job.purpose;
    let Stage::Tiles {
        frames,
        needed,
        probe,
        pending,
        fetched,
        bytes,
        hydro,
    } = &mut job.stage
    else {
        return;
    };
    let mut failure = None;
    pending.retain(|tile| {
        let state = server.load_state(&tile.handle);
        let finished = matches!(state, LoadState::Loaded | LoadState::Failed(_));
        let timed_out = !finished && load_timed_out(tile.started_s, now_s, Some(tile.bytes));
        if !finished && !timed_out {
            return true;
        }
        if timed_out && purpose == RequestPurpose::Neighbour {
            // A region that falls back would pop in at the swap with coarse terrain: a slow
            // preload is a failed one, not a degraded one.
            failure = Some(format!(
                "{}: the load timed out after {:.0} s",
                tile.path,
                load_timeout_s(Some(tile.bytes))
            ));
            return false;
        }
        if receive_tile(tile, state, timed_out, &mut grids, &mut cache, needed) {
            *fetched += 1;
            *bytes += tile.bytes;
        }
        false
    });
    if let Some(why) = failure {
        return fail(&mut commands, &mut next, &mut preload_failed, purpose, &why);
    }
    if !pending.is_empty() || !index.overview_settled() {
        return;
    }
    // The era's continental pass: ready, or this job waits for it (and asks for it once).
    let Some(hydrology) = hydro_gate.wait(spec.snapshot, hydro, now_s) else {
        return;
    };
    let hydro_wait = hydro.waited();
    let Some(globe) = globe else {
        return fail(
            &mut commands,
            &mut next,
            &mut preload_failed,
            purpose,
            "the globe data is gone",
        );
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
        hydro: hydrology,
    };
    let hydrology = inputs.hydro.is_some();
    let run = match settings.mode {
        #[cfg(not(target_arch = "wasm32"))]
        GenMode::Parallel => Running::Task(
            bevy::tasks::AsyncComputeTaskPool::get()
                .spawn(async move { generate::generate_parallel(&inputs) }),
        ),
        #[cfg(target_arch = "wasm32")]
        GenMode::Parallel => Running::Budgeted(BudgetedRun::new(inputs, web_budget(), *probe)),
        GenMode::Budgeted { budget } => Running::Budgeted(BudgetedRun::new(inputs, budget, *probe)),
    };
    job.stage = Stage::Running {
        run,
        tiles_fetched: *fetched,
        bytes_fetched: *bytes,
        fallback,
        hydro_wait,
        hydrology,
    };
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn poll_generation(
    job: Option<ResMut<RegionJob>>,
    mut commands: Commands,
    mut generated: MessageWriter<RegionGenerated>,
    mut entered: MessageWriter<RegionEntered>,
    mut changed: MessageWriter<RegionSnapshotChanged>,
    mut preload_failed: MessageWriter<RegionPreloadFailed>,
    mut pending: ResMut<PendingRegion>,
    mut next: ResMut<NextState<RegionState>>,
    large_budget: Option<Res<LargeGenBudget>>,
) {
    let Some(mut job) = job else { return };
    let job = &mut *job;
    let purpose = job.purpose;
    let Stage::Running {
        run,
        tiles_fetched,
        bytes_fetched,
        fallback,
        hydro_wait,
        hydrology,
    } = &mut job.stage
    else {
        return;
    };
    let result = match run {
        #[cfg(not(target_arch = "wasm32"))]
        Running::Task(task) => bevy::tasks::futures::check_ready(task),
        Running::Budgeted(run) => {
            let large = large_budget.is_some_and(|c| c.0);
            let flight_preload = purpose == RequestPurpose::Neighbour;
            let budget = gen_budget(run.base_budget(), large, flight_preload);
            if run.budget() != budget {
                debug!(
                    budget_ms = budget.as_secs_f64() * 1e3,
                    large, flight_preload, "region generation budget"
                );
                run.set_budget(budget);
            }
            run.poll_slice()
        }
    };
    let Some(result) = result else { return };
    let grid = match result {
        Ok(grid) => grid,
        Err(e) => {
            return fail(
                &mut commands,
                &mut next,
                &mut preload_failed,
                purpose,
                &e.to_string(),
            );
        }
    };
    let spec = job.spec;
    // The region's own time: the wait for the era's continental pass is reported apart.
    let own = job.started.elapsed().saturating_sub(*hydro_wait);
    let millis = u32::try_from(own.as_millis()).unwrap_or(u32::MAX);
    let hydro_wait_ms = u32::try_from(hydro_wait.as_micros().div_ceil(1000)).unwrap_or(u32::MAX);
    let region = ActiveRegion {
        frame: spec.frame(),
        grid: Arc::new(grid),
    };
    if purpose == RequestPurpose::Neighbour {
        // The neighbour waits for a `RegionSwapRequested`; the region and state stay.
        pending.region = Some(region);
        pending.fallback = *fallback;
    } else {
        commands.insert_resource(region);
    }
    generated.write(RegionGenerated {
        spec,
        millis,
        tiles_fetched: *tiles_fetched,
        bytes_fetched: *bytes_fetched,
        frames_fetched: job.frames_fetched,
        fallback: *fallback,
        hydro_wait_ms,
        hydrology: *hydrology,
    });
    if purpose == RequestPurpose::Neighbour {
        commands.remove_resource::<RegionJob>();
        return;
    }
    match job.replaces {
        Some(from) => {
            changed.write(RegionSnapshotChanged {
                from,
                to: spec.snapshot,
                spec,
            });
        }
        None => {
            entered.write(RegionEntered {
                spec,
                fallback: *fallback,
            });
        }
    }
    commands.remove_resource::<RegionJob>();
    next.set_if_different(RegionState::InRegion);
}

#[cfg(test)]
mod tests {
    use bevy::math::DVec3;
    use timefly_core::surface::{FrameMeta, FrameSource};

    use super::*;

    /// ICE-6G frames at 0 and 21 ka, the bridge at 30 ka and a PaleoDEM at 100 Ma; the sea
    /// stood 120 m lower at 21 ka.
    fn timeline() -> Timeline {
        let frame = |i: u32, age_myr: f64, source| FrameMeta {
            age_myr,
            source,
            image_slot: i,
        };
        let deep = |age| FrameSource::PaleoDem {
            source_rotation_age_myr: age,
        };
        Timeline::new(
            vec![
                frame(0, 0.0, FrameSource::Ice6g),
                frame(1, 0.021, FrameSource::Ice6g),
                frame(2, 0.03, deep(0.0)),
                frame(3, 100.0, deep(100.0)),
            ],
            vec![(0.0, 0.0), (0.021, -120.0), (0.03, -100.0)],
        )
        .unwrap()
    }

    #[test]
    fn the_load_clock_counts_running_frames_not_stalls() {
        assert_eq!(advance_load_clock(10.0, 0.016), 10.016);
        // A 60 s stall (a paused page) counts as one long frame at most.
        assert_eq!(
            advance_load_clock(10.0, 60.0),
            10.0 + LOAD_CLOCK_MAX_DELTA_S
        );
        assert_eq!(advance_load_clock(10.0, -1.0), 10.0);
        // 20 s of 60 Hz frames still reach the timeout.
        let mut t = 0.0;
        for _ in 0..(LOAD_TIMEOUT_S * 60.0) as usize + 1 {
            t = advance_load_clock(t, 1.0 / 60.0);
        }
        assert!(load_timed_out(0.0, t, None));
        assert!(!load_timed_out(0.0, advance_load_clock(0.0, 60.0), None));
    }

    #[test]
    fn loads_time_out_after_their_limit() {
        assert_eq!(load_timeout_s(None), LOAD_TIMEOUT_S);
        assert_eq!(load_timeout_s(Some(100_000)), LOAD_TIMEOUT_S);
        // The 12.4 MB overview: 24.8 s at 4 Mbit/s.
        assert!((load_timeout_s(Some(12_400_000)) - 24.8).abs() < 1e-9);
        assert!(!load_timed_out(10.0, 10.0 + LOAD_TIMEOUT_S - 0.01, None));
        assert!(load_timed_out(10.0, 10.0 + LOAD_TIMEOUT_S, None));
        assert!(!load_timed_out(0.0, 24.0, Some(12_400_000)));
        assert!(load_timed_out(0.0, 25.0, Some(12_400_000)));
        // A clock that went backwards (it cannot) never times out.
        assert!(!load_timed_out(30.0, 0.0, None));
    }

    #[test]
    fn requests_become_specs_only_at_real_directions() {
        let settings = RegionSettings::default();
        let timeline = timeline();
        let at = |v: DVec3| RegionRequested {
            era_dir: RenderDir(v),
            snapshot: YearsBp(21.0e3),
            purpose: RequestPurpose::Enter,
        };
        let spec = spec_for(&at(DVec3::new(0.0, 0.0, 2.0)), &settings, &timeline).unwrap();
        assert_eq!(spec.center, RenderDir(DVec3::Z));
        assert_eq!(spec.snapshot, YearsBp(21.0e3));
        assert_eq!(spec.seed, REGION_SEED);
        assert_eq!((spec.inner_res, spec.apron_res), (1025, 513));
        // The glacial snowline's anomaly: the timeline's at 21 ka, none at 100 Ma.
        assert_eq!(spec.sea_level_anomaly_m, -120.0);
        let deep = RegionRequested {
            snapshot: YearsBp(100.0e6),
            ..at(DVec3::Z)
        };
        assert_eq!(
            spec_for(&deep, &settings, &timeline)
                .unwrap()
                .sea_level_anomaly_m,
            0.0
        );
        assert!(spec_for(&at(DVec3::ZERO), &settings, &timeline).is_none());
        assert!(spec_for(&at(DVec3::new(f64::NAN, 0.0, 1.0)), &settings, &timeline).is_none());
    }
}
