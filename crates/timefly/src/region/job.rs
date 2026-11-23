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
use timefly_core::worldgen::coarse::frames_needed;
#[cfg(not(target_arch = "wasm32"))]
use timefly_core::worldgen::generate::RegionGrid;
use timefly_core::worldgen::generate::RegionSpec;
use tracing::instrument;

use super::frames::{CpuGrid, CpuGridSettings, FrameCache};
#[cfg(not(target_arch = "wasm32"))]
use super::generate::GenError;
use super::generate::{self, BudgetedRun, GenInputs};
use super::tiles::{OVERVIEW_PATH, TILE_MANIFEST_PATH, TileCache, TileManifest, tile_path};
#[cfg(target_arch = "wasm32")]
use super::web_budget;
use super::{
    ActiveRegion, GenMode, REGION_SEED, RegionEntered, RegionGenerated, RegionLeft,
    RegionRequested, RegionSettings, RegionSnapshotChanged, RegionState,
};
use crate::data::{GlobeData, describe_load_error};

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

/// The overview, loaded once the manifest is ready and kept in the [`TileCache`].
#[derive(Default)]
enum OverviewState {
    #[default]
    Idle,
    Loading(Handle<CpuGrid>),
    /// In the [`TileCache`].
    Ready,
    /// Unavailable this session (missing, corrupt, or not in the manifest): aprons use the
    /// globe frames.
    Failed,
}

/// Session-wide tile data loads. They belong to no job, so a superseded or abandoned request
/// never cancels them.
#[derive(Resource, Default)]
pub(super) struct TileIndex {
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

    fn overview_settled(&self) -> bool {
        matches!(self.overview, OverviewState::Ready | OverviewState::Failed)
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
    slot: u32,
    frames: Vec<usize>,
    handle: Handle<CpuGrid>,
}

/// A tile loading.
struct PendingTile {
    key: (u8, u8),
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
        /// The image slots this request reads, kept in the [`FrameCache`].
        slots: Vec<u32>,
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

/// The request in flight. Replacing or removing it drops its handles and task, which cancels
/// its loads and its run.
#[derive(Resource)]
pub(super) struct RegionJob {
    spec: RegionSpec,
    started: Instant,
    /// The snapshot of the active region this job replaces.
    replaces: Option<YearsBp>,
    stage: Stage,
}

/// The spec a request asks for, or `None` for a direction that is not one.
pub(super) fn spec_for(request: &RegionRequested, settings: &RegionSettings) -> Option<RegionSpec> {
    let center = request.era_dir.0.normalize_or_zero();
    (center != bevy::math::DVec3::ZERO && center.is_finite()).then_some(RegionSpec {
        center: RenderDir(center),
        snapshot: request.snapshot,
        seed: REGION_SEED,
        inner_res: settings.inner_res,
        apron_res: settings.apron_res,
    })
}

/// The frame files a snapshot reads: each image slot once, with the timeline frames it serves.
fn frame_files(timeline: &Timeline, age_myr: f64) -> Vec<(u32, Vec<usize>)> {
    let mut files: Vec<(u32, Vec<usize>)> = Vec::new();
    for frame in frames_needed(timeline, age_myr) {
        let slot = timeline.frames()[frame].image_slot;
        match files.iter_mut().find(|(s, _)| *s == slot) {
            Some((_, frames)) => frames.push(frame),
            None => files.push((slot, vec![frame])),
        }
    }
    files
}

pub(super) fn leave_region(
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
pub(super) fn start_region_request(
    mut requests: MessageReader<RegionRequested>,
    mut commands: Commands,
    globe: Option<Res<GlobeData>>,
    server: Res<AssetServer>,
    settings: Res<RegionSettings>,
    mut index: ResMut<TileIndex>,
    mut frame_cache: ResMut<FrameCache>,
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
    let Some(spec) = spec_for(&request, &settings) else {
        warn!(
            ?request,
            "region requested at an invalid direction; ignored"
        );
        return;
    };
    if matches!(index.manifest, ManifestState::Idle) {
        index.manifest = ManifestState::Loading(server.load(TILE_MANIFEST_PATH));
    }
    let timeline = &globe.timeline;
    let mut frames = vec![None; timeline.frames().len()];
    let mut slots = Vec::new();
    let mut pending = Vec::new();
    for (slot, served) in frame_files(timeline, spec.snapshot.myr()) {
        slots.push(slot);
        match frame_cache.get(slot) {
            Some(grid) => {
                for &f in &served {
                    frames[f] = Some(grid.clone());
                }
            }
            None => pending.push(PendingFrame {
                slot,
                handle: server.load::<CpuGrid>(globe.frame_path(served[0]).clone()),
                frames: served,
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
            slots,
            pending,
            frames,
        },
    });
    next.set_if_different(RegionState::Generating);
}

pub(super) fn poll_tile_manifest(
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
) {
    let next = match &index.overview {
        OverviewState::Idle => match &index.manifest {
            ManifestState::Ready(manifest) => match &manifest.overview {
                Some(entry) => {
                    OverviewState::Loading(load_checked(&server, OVERVIEW_PATH, entry.settings()))
                }
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
        OverviewState::Loading(handle) => {
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
) {
    let Some(mut job) = job else { return };
    let spec = job.spec;
    let Stage::Frames {
        slots,
        pending,
        frames,
    } = &mut job.stage
    else {
        return;
    };
    let mut failure = None;
    pending.retain(|p| match server.load_state(&p.handle) {
        LoadState::Loaded => match take_grid(&mut grids, &p.handle) {
            Some(grid) => {
                for &f in &p.frames {
                    frames[f] = Some(grid.clone());
                }
                frame_cache.insert(p.slot, grid, slots);
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
    if !pending.is_empty() || !index.manifest_settled() {
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
                    key: (row, col),
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
    }
    job.stage = Stage::Tiles {
        frames,
        needed,
        probe,
        pending,
        fetched: 0,
        bytes: 0,
    };
}

/// Takes in one finished tile load; `true` if the tile is now in the cache.
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
            Some(grid) => match cache.insert(tile.key, grid, needed) {
                Ok(()) => return true,
                Err(e) => e.to_string(),
            },
        },
        LoadState::Failed(e) => describe_load_error(&tile.path, &e).trim_end().to_string(),
        _ => unreachable!("called only for finished loads"),
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
        if receive_tile(tile, state, &mut grids, &mut cache, needed) {
            *fetched += 1;
            *bytes += tile.bytes;
        }
        false
    });
    if !pending.is_empty() || !index.overview_settled() {
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
        GenMode::Parallel => Running::Budgeted(BudgetedRun::new(inputs, web_budget(), *probe)),
        GenMode::Budgeted { budget } => Running::Budgeted(BudgetedRun::new(inputs, budget, *probe)),
    };
    job.stage = Stage::Running {
        run,
        tiles_fetched: *fetched,
        fallback,
    };
}

pub(super) fn poll_generation(
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

#[cfg(test)]
mod tests {
    use bevy::math::DVec3;

    use super::*;

    #[test]
    fn requests_become_specs_only_at_real_directions() {
        let settings = RegionSettings::default();
        let at = |v: DVec3| RegionRequested {
            era_dir: RenderDir(v),
            snapshot: YearsBp(21.0e3),
        };
        let spec = spec_for(&at(DVec3::new(0.0, 0.0, 2.0)), &settings).unwrap();
        assert_eq!(spec.center, RenderDir(DVec3::Z));
        assert_eq!(spec.snapshot, YearsBp(21.0e3));
        assert_eq!(spec.seed, REGION_SEED);
        assert_eq!((spec.inner_res, spec.apron_res), (1025, 513));
        assert!(spec_for(&at(DVec3::ZERO), &settings).is_none());
        assert!(spec_for(&at(DVec3::new(f64::NAN, 0.0, 1.0)), &settings).is_none());
    }
}
