//! The pass in flight: its loads (the era's globe frames, its climate grids and the session's
//! overview), then the run, natively on an
//! [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool) task ([`run_parallel`]) and on the
//! web in time-boxed slices ([`HydroRun`]). Inputs are owned ([`HydroInputs`]), so a run can
//! leave the main thread.

use std::future::Future;
#[cfg(not(target_arch = "wasm32"))]
use std::ops::Range;
use std::pin::Pin;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bevy::asset::LoadState;
use bevy::platform::time::Instant;
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::tasks::Task;
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::Grid;
use timefly_core::geodynamics::CrustModel;
use timefly_core::hydrology::climate::{
    BeyerClimate, ClimateSource, ClimateTable, ZonalClimate, is_climate_grid,
};
use timefly_core::hydrology::{
    CubeLattice, EraHydrology, HydroGraph, HydroPrep, LatticeSizes, NodeSource, SnapshotSource,
    SourceMode, hydro_lattice_for,
};
use timefly_core::surface::Timeline;
use timefly_core::worldgen::coarse::{
    SnapshotField, frames_needed, snapshot_field, snapshot_sea_level_anomaly_m,
};
use timefly_core::worldgen::tiles::TileSet;

use super::climate::{ClimateIndex, climate_table, load_grid};
use super::{
    Admit, HydroCache, HydroPassFailed, HydroPassFinished, HydroPurpose, HydroRequested,
    HydroSettings, RunningPass, WEB_HYDRO_BUDGET_MS, admit,
};
use crate::clock::LoadClock;
use crate::data::{GlobeData, describe_load_error};
use crate::region::frames::{CpuGrid, FrameCache, FrameLoads, take_grid};
use crate::region::generate::{SharedBudget, Slice};
use crate::region::tiles::TileCache;
use crate::region::{LOAD_TIMEOUT_S, LargeGenBudget, TileIndex, gen_budget, load_timed_out};

/// Nodes per parallel sampling block on native.
pub const SAMPLE_BLOCK_NODES: u32 = 4096;
/// Units per [`HydroPrep::step`] on native: the task checks for cancellation and measures the
/// working bytes between steps (about 30 checks at n = 900), and is otherwise one long run.
pub const NATIVE_STEP_UNITS: u32 = 1 << 20;
/// A time-boxed run's first step, in units; later steps aim at a quarter of the slice budget
/// from the measured rate, between [`SLICED_MIN_UNITS`] and [`SLICED_MAX_UNITS`].
pub const SLICED_FIRST_UNITS: u32 = 1024;
pub const SLICED_MIN_UNITS: u32 = 64;
pub const SLICED_MAX_UNITS: u32 = 1 << 20;

/// Everything a pass needs, owned.
#[derive(Clone)]
pub struct HydroInputs {
    pub age: YearsBp,
    pub sizes: LatticeSizes,
    pub timeline: Arc<Timeline>,
    pub crust: Arc<CrustModel>,
    /// The session's overview (no tiles), when it loaded.
    pub tiles: TileSet,
    /// Timeline frame `i`'s CPU grid at index `i`, for every frame [`frames_needed`] lists.
    pub frames: Vec<Option<Arc<Grid>>>,
    /// The Beyer et al. 2020 grids around the age (`[younger, older]`) and the older one's
    /// weight; `None` for the zonal rule.
    pub climate: Option<([Arc<Grid>; 2], f64)>,
    pub table: ClimateTable,
}

impl HydroInputs {
    fn field(&self) -> Result<SnapshotField<'_>, String> {
        let age_myr = self.age.myr();
        for i in frames_needed(&self.timeline, age_myr) {
            if self.frames.get(i).is_none_or(Option::is_none) {
                return Err(format!("timeline frame {i} is not loaded"));
            }
        }
        snapshot_field(
            &self.timeline,
            &self.crust,
            &self.tiles,
            false,
            age_myr,
            |i| self.frames[i].as_deref().expect("checked above"),
        )
        .map_err(|e| e.to_string())
    }

    fn lattice(&self) -> (CubeLattice, SourceMode) {
        hydro_lattice_for(&self.timeline, self.age.myr(), self.sizes)
    }

    fn zonal(&self) -> ZonalClimate<'_> {
        ZonalClimate {
            table: &self.table,
            sea_level_anomaly_m: snapshot_sea_level_anomaly_m(&self.timeline, self.age.myr()),
        }
    }

    fn beyer(&self) -> Option<BeyerClimate<'_>> {
        self.climate
            .as_ref()
            .map(|(g, w)| BeyerClimate::new([&g[0], &g[1]], *w, self.zonal()))
    }
}

/// A finished pass and what it cost.
#[derive(Debug)]
pub struct PassOutput {
    pub hydrology: EraHydrology,
    /// The pass's own compute: natively the task's time from the field to the result (the
    /// parallel sampling counted once, as wall time); time-boxed, the sum of its slices.
    pub cpu: Duration,
    /// The part of `cpu` spent sampling the lattice.
    pub sampling: Duration,
    pub slices: u32,
    pub longest_slice: Duration,
    /// The longest single [`HydroPrep::step`]: what a slice may overshoot its budget by.
    pub longest_step: Duration,
    /// The largest [`HydroPrep::working_bytes`] seen between steps.
    pub peak_bytes: usize,
    pub units: u64,
    /// Whether the climate was Beyer's (else the zonal rule).
    pub beyer: bool,
}

/// Consecutive node ranges of at most `block` nodes covering `0..len`, in order.
#[cfg(not(target_arch = "wasm32"))]
fn node_blocks(len: u32, block: u32) -> Vec<Range<u32>> {
    let block = block.max(1);
    (0..len.div_ceil(block))
        .map(|b| b * block..((b + 1) * block).min(len))
        .collect()
}

/// Steps the pass to the end in [`NATIVE_STEP_UNITS`] steps; `false` if cancelled.
#[cfg(not(target_arch = "wasm32"))]
fn step_native<S: NodeSource, C: ClimateSource>(
    prep: &mut HydroPrep,
    source: &S,
    climate: &C,
    table: &ClimateTable,
    cancel: &AtomicBool,
    peak: &mut usize,
    longest: &mut Duration,
) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let t = Instant::now();
        let done = prep.step(source, climate, table, NATIVE_STEP_UNITS);
        *longest = (*longest).max(t.elapsed());
        *peak = (*peak).max(prep.working_bytes());
        if done {
            return true;
        }
    }
}

/// The pass on the calling thread, its sampling in parallel [`SAMPLE_BLOCK_NODES`] blocks
/// across the [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool). `cancel` stops it
/// between steps.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_parallel(inputs: &HydroInputs, cancel: &AtomicBool) -> Result<PassOutput, String> {
    use bevy::tasks::AsyncComputeTaskPool;

    let started = Instant::now();
    let field = inputs.field()?;
    let (lattice, mode) = inputs.lattice();
    let source = SnapshotSource::new(
        &field,
        &inputs.tiles,
        &inputs.timeline,
        inputs.age.myr(),
        mode,
    );
    let mut prep = HydroPrep::new(lattice, inputs.age);
    let shared = &source;
    let blocks = AsyncComputeTaskPool::get().scope(|s| {
        for range in node_blocks(lattice.len(), SAMPLE_BLOCK_NODES) {
            s.spawn(async move { HydroPrep::sample_range(&lattice, shared, range) });
        }
    });
    for block in blocks {
        prep.add_samples(block);
    }
    let sampling = started.elapsed();
    let mut peak = prep.working_bytes();
    let mut longest = Duration::ZERO;
    let zonal = inputs.zonal();
    let beyer = inputs.beyer();
    let done = match &beyer {
        Some(b) => step_native(
            &mut prep,
            &source,
            b,
            &inputs.table,
            cancel,
            &mut peak,
            &mut longest,
        ),
        None => step_native(
            &mut prep,
            &source,
            &zonal,
            &inputs.table,
            cancel,
            &mut peak,
            &mut longest,
        ),
    };
    if !done {
        return Err("cancelled".to_string());
    }
    let units = prep.units_done();
    let cpu = started.elapsed();
    Ok(PassOutput {
        hydrology: prep.finish(),
        cpu,
        sampling,
        slices: 1,
        longest_slice: cpu,
        longest_step: longest,
        peak_bytes: peak,
        units,
        beyer: beyer.is_some(),
    })
}

/// A time-boxed run's step size, adapted to the measured rate.
struct Stepper {
    budget: SharedBudget,
    units: u32,
    longest: Duration,
    steps: u32,
    peak: usize,
}

impl Stepper {
    fn observe(&mut self, units: u64, spent: Duration) {
        self.longest = self.longest.max(spent);
        let target = self.budget.get().as_secs_f64() / 4.0;
        let next = if spent.is_zero() {
            f64::from(self.units) * 2.0
        } else {
            units as f64 * target / spent.as_secs_f64()
        };
        self.units = next.clamp(f64::from(SLICED_MIN_UNITS), f64::from(SLICED_MAX_UNITS)) as u32;
    }
}

/// Steps the pass to the end, yielding whenever the slice's budget is spent.
async fn step_sliced<S: NodeSource, C: ClimateSource>(
    prep: &mut HydroPrep,
    source: &S,
    climate: &C,
    table: &ClimateTable,
    slice: &mut Slice,
    stepper: &mut Stepper,
) {
    loop {
        let before = prep.units_done();
        let t = Instant::now();
        let done = prep.step(source, climate, table, stepper.units);
        stepper.observe(prep.units_done() - before, t.elapsed());
        stepper.steps += 1;
        // The working bytes walk the basins: measured every 16th step and at the end.
        if done || stepper.steps.is_multiple_of(16) {
            stepper.peak = stepper.peak.max(prep.working_bytes());
        }
        if done {
            return;
        }
        slice.yield_if_spent().await;
    }
}

async fn run_sliced(inputs: HydroInputs, budget: SharedBudget) -> Result<PassOutput, String> {
    let mut slice = Slice::new(budget.clone());
    let field = inputs.field()?;
    let (lattice, mode) = inputs.lattice();
    let source = SnapshotSource::new(
        &field,
        &inputs.tiles,
        &inputs.timeline,
        inputs.age.myr(),
        mode,
    );
    let mut prep = HydroPrep::new(lattice, inputs.age);
    let mut stepper = Stepper {
        budget,
        units: SLICED_FIRST_UNITS,
        longest: Duration::ZERO,
        steps: 0,
        peak: 0,
    };
    let zonal = inputs.zonal();
    let beyer = inputs.beyer();
    match &beyer {
        Some(b) => {
            step_sliced(
                &mut prep,
                &source,
                b,
                &inputs.table,
                &mut slice,
                &mut stepper,
            )
            .await
        }
        None => {
            step_sliced(
                &mut prep,
                &source,
                &zonal,
                &inputs.table,
                &mut slice,
                &mut stepper,
            )
            .await
        }
    }
    let units = prep.units_done();
    let hydrology = prep.finish();
    slice.record();
    Ok(PassOutput {
        hydrology,
        cpu: slice.total,
        sampling: Duration::ZERO,
        slices: slice.count,
        longest_slice: slice.longest,
        longest_step: stepper.longest,
        peak_bytes: stepper.peak,
        units,
        beyer: beyer.is_some(),
    })
}

type RunFuture = Pin<Box<dyn Future<Output = Result<PassOutput, String>> + Send>>;

/// A time-boxed pass, like [`BudgetedRun`](crate::region::generate::BudgetedRun): each
/// [`poll_slice`](Self::poll_slice) steps the pass until about its budget has passed (plus at
/// most one step), then returns. Built for the web, where it is polled once per frame; it runs
/// the same on native and is tested there. The result equals [`run_parallel`]'s.
pub struct HydroRun {
    // A `Mutex` only to make the run `Sync` for a Bevy resource; it is never contended.
    future: Mutex<RunFuture>,
    base: Duration,
    budget: SharedBudget,
}

impl HydroRun {
    pub fn new(inputs: HydroInputs, budget: Duration) -> HydroRun {
        let shared = SharedBudget::new(budget);
        HydroRun {
            future: Mutex::new(Box::pin(run_sliced(inputs, shared.clone()))),
            base: budget,
            budget: shared,
        }
    }

    /// The budget the run was started with.
    pub fn base_budget(&self) -> Duration {
        self.base
    }

    /// The budget of the slices from the next [`poll_slice`](Self::poll_slice) on.
    pub fn set_budget(&self, budget: Duration) {
        self.budget.set(budget);
    }

    pub fn budget(&self) -> Duration {
        self.budget.get()
    }

    /// Runs one slice; `Some` when the pass has finished.
    pub fn poll_slice(&mut self) -> Option<Result<PassOutput, String>> {
        let future = self
            .future
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut cx = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => Some(result),
            Poll::Pending => None,
        }
    }
}

/// Sets its flag when dropped: a replaced or abandoned native task stops at its next step.
#[cfg(not(target_arch = "wasm32"))]
struct CancelOnDrop(Arc<AtomicBool>);

#[cfg(not(target_arch = "wasm32"))]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

enum Run {
    #[cfg(not(target_arch = "wasm32"))]
    Task {
        task: Task<Result<PassOutput, String>>,
        _cancel: CancelOnDrop,
    },
    Sliced(HydroRun),
}

/// A climate grid loading.
struct PendingGrid {
    /// `[younger, older]` slots it fills.
    slots: Vec<usize>,
    path: String,
    bytes: u64,
    handle: Handle<CpuGrid>,
    started_s: f64,
}

enum ClimateLoads {
    /// For the climate manifest to settle.
    Manifest,
    Grids {
        pending: Vec<PendingGrid>,
        grids: [Option<Arc<Grid>>; 2],
        weight: f64,
    },
    Ready(Option<([Arc<Grid>; 2], f64)>),
}

enum Stage {
    Loading {
        frames: FrameLoads,
        ready: Option<Vec<Option<Arc<Grid>>>>,
        climate: ClimateLoads,
    },
    Running(Run),
}

/// The pass in flight. Replacing or removing it drops its loads and its run (a native task
/// stops at its next step).
#[derive(Resource)]
pub struct HydroJob {
    age: YearsBp,
    started: Instant,
    stage: Stage,
}

/// Applies this frame's [`HydroRequested`] messages in order ([`admit`]) and starts the
/// admitted pass's loads.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn start_hydro_requests(
    mut requests: MessageReader<HydroRequested>,
    settings: Res<HydroSettings>,
    mut cache: ResMut<HydroCache>,
    globe: Option<Res<GlobeData>>,
    server: Res<AssetServer>,
    mut frame_cache: ResMut<FrameCache>,
    clock: Res<LoadClock>,
    mut commands: Commands,
) {
    let mut start = None;
    for request in requests.read() {
        if !settings.enabled || globe.is_none() {
            debug!(
                ?request,
                "continental pass requested while disabled or before the globe data; ignored"
            );
            continue;
        }
        match admit(cache.running, cache.settled(request.age), request) {
            Admit::Start => {
                cache.running = Some(RunningPass {
                    age: request.age,
                    purpose: request.purpose,
                });
                start = Some(request.age);
            }
            Admit::Upgrade => {
                if let Some(r) = cache.running.as_mut() {
                    r.purpose = HydroPurpose::Region;
                }
            }
            Admit::Ignore => {}
        }
    }
    let (Some(age), Some(globe)) = (start, globe) else {
        return;
    };
    let frames = FrameLoads::start(
        &globe,
        age.myr(),
        &server,
        &mut frame_cache,
        clock.elapsed_s,
    );
    commands.insert_resource(HydroJob {
        age,
        started: Instant::now(),
        stage: Stage::Loading {
            frames,
            ready: None,
            climate: ClimateLoads::Manifest,
        },
    });
}

/// Ends the job as failed: a `warn`, [`HydroPassFailed`], and the era marked failed.
fn fail(
    commands: &mut Commands,
    cache: &mut HydroCache,
    failed: &mut MessageWriter<HydroPassFailed>,
    age: YearsBp,
    reason: &str,
) {
    commands.remove_resource::<HydroJob>();
    cache.running = None;
    cache.mark_failed(age);
    warn!(
        age_years = age.0,
        error = reason,
        "continental pass failed; regions of this era generate without rivers and lakes"
    );
    failed.write(HydroPassFailed {
        age,
        reason: reason.to_string(),
    });
}

/// Moves the climate loads on; a grid that fails makes the climate zonal for the session.
fn advance_climate(
    loads: &mut ClimateLoads,
    age: YearsBp,
    index: &mut ClimateIndex,
    server: &AssetServer,
    grids: &mut Assets<CpuGrid>,
    now_s: f64,
) {
    if let ClimateLoads::Manifest = loads {
        if !index.settled() {
            return;
        }
        *loads = match index.manifest().and_then(|m| m.bracket(age)) {
            None => ClimateLoads::Ready(None),
            Some((files, weight)) => {
                let mut pending: Vec<PendingGrid> = Vec::new();
                for (slot, file) in files.into_iter().enumerate() {
                    match pending.iter_mut().find(|p| p.path == file.path()) {
                        Some(p) => p.slots.push(slot),
                        None => pending.push(PendingGrid {
                            slots: vec![slot],
                            path: file.path(),
                            bytes: file.bytes,
                            handle: load_grid(server, file),
                            started_s: now_s,
                        }),
                    }
                }
                ClimateLoads::Grids {
                    pending,
                    grids: [None, None],
                    weight,
                }
            }
        };
    }
    let ClimateLoads::Grids {
        pending,
        grids: got,
        weight,
    } = loads
    else {
        return;
    };
    let mut failure = None;
    pending.retain(|p| {
        let problem = match server.load_state(&p.handle) {
            LoadState::Loaded => match take_grid(grids, &p.handle) {
                Some(grid) if is_climate_grid(&grid) => {
                    for &s in &p.slots {
                        got[s] = Some(grid.clone());
                    }
                    return false;
                }
                Some(_) => "not a 1-degree, two-channel climate grid".to_string(),
                None => "the grid vanished after loading".to_string(),
            },
            LoadState::Failed(e) => describe_load_error(&p.path, &e).trim_end().to_string(),
            _ if load_timed_out(p.started_s, now_s, Some(p.bytes)) => {
                format!("the load timed out after {LOAD_TIMEOUT_S:.0} s")
            }
            _ => return true,
        };
        failure.get_or_insert((p.path.clone(), problem));
        false
    });
    if let Some((path, why)) = failure {
        index.fail(&path, &why);
        *loads = ClimateLoads::Ready(None);
        return;
    }
    let ready = match (pending.is_empty(), &*got) {
        (true, [Some(young), Some(old)]) => Some(([young.clone(), old.clone()], *weight)),
        _ => None,
    };
    if let Some(ready) = ready {
        *loads = ClimateLoads::Ready(Some(ready));
    }
}

/// Moves the job's loads on and, once the frames, the climate and the overview are in, starts
/// its run.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
pub(super) fn advance_hydro_job(
    job: Option<ResMut<HydroJob>>,
    mut commands: Commands,
    server: Res<AssetServer>,
    mut grids: ResMut<Assets<CpuGrid>>,
    mut frame_cache: ResMut<FrameCache>,
    mut tile_cache: ResMut<TileCache>,
    index: Res<TileIndex>,
    mut climate_index: ResMut<ClimateIndex>,
    globe: Option<Res<GlobeData>>,
    settings: Res<HydroSettings>,
    mut cache: ResMut<HydroCache>,
    mut failed: MessageWriter<HydroPassFailed>,
    clock: Res<LoadClock>,
) {
    let now_s = clock.elapsed_s;
    let Some(mut job) = job else { return };
    let age = job.age;
    let Stage::Loading {
        frames,
        ready,
        climate,
    } = &mut job.stage
    else {
        return;
    };
    if ready.is_none() {
        match frames.poll(&server, &mut grids, &mut frame_cache, now_s) {
            None => {}
            Some(Ok(f)) => *ready = Some(f),
            Some(Err(why)) => return fail(&mut commands, &mut cache, &mut failed, age, &why),
        }
    }
    advance_climate(climate, age, &mut climate_index, &server, &mut grids, now_s);
    let ClimateLoads::Ready(beyer) = climate else {
        return;
    };
    if ready.is_none() || !index.overview_settled() {
        return;
    }
    let Some(globe) = globe else {
        return fail(
            &mut commands,
            &mut cache,
            &mut failed,
            age,
            "the globe data is gone",
        );
    };
    let inputs = HydroInputs {
        age,
        sizes: settings.sizes,
        timeline: globe.timeline.clone(),
        crust: globe.crust.clone(),
        tiles: tile_cache.tile_set(&[]),
        frames: ready.take().expect("checked above"),
        climate: beyer.take(),
        table: climate_table(),
    };
    let run = if cfg!(target_arch = "wasm32") {
        Run::Sliced(HydroRun::new(
            inputs,
            Duration::from_secs_f64(WEB_HYDRO_BUDGET_MS / 1000.0),
        ))
    } else {
        start_task(inputs)
    };
    job.stage = Stage::Running(run);
}

/// The native run: a task on the async compute pool.
#[cfg(not(target_arch = "wasm32"))]
fn start_task(inputs: HydroInputs) -> Run {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    Run::Task {
        task: bevy::tasks::AsyncComputeTaskPool::get()
            .spawn(async move { run_parallel(&inputs, &flag) }),
        _cancel: CancelOnDrop(cancel),
    }
}

/// The web has no task pool threads (never called there: the web's runs are sliced).
#[cfg(target_arch = "wasm32")]
fn start_task(inputs: HydroInputs) -> Run {
    Run::Sliced(HydroRun::new(
        inputs,
        Duration::from_secs_f64(WEB_HYDRO_BUDGET_MS / 1000.0),
    ))
}

/// Polls the run (one slice on the web, at the large budget while no region is on screen) and
/// takes in its result.
pub(super) fn poll_hydro_run(
    job: Option<ResMut<HydroJob>>,
    mut commands: Commands,
    mut cache: ResMut<HydroCache>,
    mut finished: MessageWriter<HydroPassFinished>,
    mut failed: MessageWriter<HydroPassFailed>,
    large_budget: Option<Res<LargeGenBudget>>,
) {
    let Some(mut job) = job else { return };
    let age = job.age;
    let Stage::Running(run) = &mut job.stage else {
        return;
    };
    let result = match run {
        #[cfg(not(target_arch = "wasm32"))]
        Run::Task { task, .. } => bevy::tasks::futures::check_ready(task),
        Run::Sliced(run) => {
            let large = large_budget.is_some_and(|b| b.0);
            let budget = gen_budget(run.base_budget(), large, false);
            if run.budget() != budget {
                debug!(
                    budget_ms = budget.as_secs_f64() * 1e3,
                    large, "continental pass budget"
                );
                run.set_budget(budget);
            }
            run.poll_slice()
        }
    };
    let Some(result) = result else { return };
    let out = match result {
        Ok(out) => out,
        Err(why) => return fail(&mut commands, &mut cache, &mut failed, age, &why),
    };
    let wall = job.started.elapsed();
    let ms = |d: Duration| u32::try_from(d.as_millis()).unwrap_or(u32::MAX);
    let h = &out.hydrology;
    let platform = if cfg!(target_arch = "wasm32") {
        "web"
    } else {
        "native"
    };
    let event = HydroPassFinished {
        age,
        wall_ms: ms(wall),
        cpu_ms: ms(out.cpu),
        slices: out.slices,
        nodes: h.lattice.len(),
        river_nodes: u32::try_from(h.rivers.nodes().len()).unwrap_or(u32::MAX),
        lakes: u32::try_from(h.lakes.len()).unwrap_or(u32::MAX),
        platform,
    };
    info!(
        age_years = age.0,
        wall_ms = event.wall_ms,
        cpu_ms = event.cpu_ms,
        sampling_ms = ms(out.sampling),
        slices = event.slices,
        longest_slice_ms = format!("{:.1}", out.longest_slice.as_secs_f64() * 1e3),
        longest_step_ms = format!("{:.1}", out.longest_step.as_secs_f64() * 1e3),
        nodes = event.nodes,
        river_nodes = event.river_nodes,
        lakes = event.lakes,
        units = out.units,
        working_bytes = out.peak_bytes,
        result_bytes = h.bytes(),
        climate = if out.beyer { "beyer" } else { "zonal" },
        platform,
        "continental pass done"
    );
    for name in &h.unplaced_lake_levels {
        warn!(
            lake = name.as_str(),
            age_years = age.0,
            "the lake level table's entry fits no depression, sits below every land node or \
             conflicts with another; the lake keeps the pass's own level"
        );
    }
    cache.running = None;
    cache.insert(Arc::new(out.hydrology));
    finished.write(event);
    commands.remove_resource::<HydroJob>();
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use bevy::asset::AssetMetaCheck;
    use bevy::state::app::StatesPlugin;
    use timefly_core::formats::read_tfgrid;

    use super::*;
    use crate::data::{GlobeDataPlugin, GlobeLoadState};
    use crate::hydro::climate::ClimateManifest;

    fn tiny_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets")
    }

    /// The tiny bake's timeline and plates, through the globe data plugin.
    fn tiny_globe() -> (Arc<Timeline>, Arc<CrustModel>) {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin {
                file_path: tiny_root().to_string_lossy().into_owned(),
                meta_check: AssetMetaCheck::Never,
                ..default()
            },
            ImagePlugin::default(),
            StatesPlugin,
            GlobeDataPlugin::default(),
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            app.update();
            match *app.world().resource::<State<GlobeLoadState>>().get() {
                GlobeLoadState::Ready => break,
                GlobeLoadState::Failed => panic!("the tiny bake failed to load"),
                GlobeLoadState::Loading => {}
            }
            assert!(std::time::Instant::now() < deadline, "globe still loading");
            std::thread::sleep(Duration::from_millis(1));
        }
        let data = app.world().resource::<GlobeData>();
        (data.timeline.clone(), data.crust.clone())
    }

    fn read(path: &str) -> Grid {
        read_tfgrid(&std::fs::read(tiny_root().join(path)).unwrap()).unwrap()
    }

    /// The tiny bake's inputs for `years`, read straight from the files, with Beyer's climate.
    fn tiny_inputs(years: f64, sizes: LatticeSizes) -> HydroInputs {
        let (timeline, crust) = tiny_globe();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tiny_root().join("data/globe/manifest.json")).unwrap(),
        )
        .unwrap();
        let frames = manifest["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let file = f["file"].as_str().unwrap();
                Some(Arc::new(read(&format!("data/globe/{file}"))))
            })
            .collect();
        let mut tiles = TileSet::new();
        tiles
            .set_overview(read(crate::region::tiles::OVERVIEW_PATH))
            .unwrap();
        let climate = ClimateManifest::parse(
            &std::fs::read(tiny_root().join(crate::hydro::climate::CLIMATE_MANIFEST_PATH)).unwrap(),
        )
        .unwrap();
        let ([y, o], w) = climate.bracket(YearsBp(years)).unwrap();
        HydroInputs {
            age: YearsBp(years),
            sizes,
            timeline,
            crust,
            tiles,
            frames,
            climate: Some(([Arc::new(read(&y.path())), Arc::new(read(&o.path()))], w)),
            table: climate_table(),
        }
    }

    #[test]
    fn node_blocks_cover_in_order() {
        assert_eq!(node_blocks(10, 4), vec![0..4, 4..8, 8..10]);
        assert!(node_blocks(0, 4).is_empty());
        assert_eq!(node_blocks(3, 0), vec![0..1, 1..2, 2..3]);
    }

    /// A time-boxed run at 2 ms: no slice runs past the budget by more than one step's
    /// measured maximum, and the result is the native task's, bit for bit.
    #[test]
    fn web_slices_respect_the_budget() {
        // 15 ka: a bridge era of the tiny timeline, on a lattice big enough to span many 2 ms
        // slices, with Beyer's climate between the 10 and 21 ka grids.
        let sizes = LatticeSizes {
            recent_n: 128,
            deep_n: 16,
        };
        let inputs = tiny_inputs(15.0e3, sizes);
        assert!(inputs.climate.as_ref().is_some_and(|(_, w)| *w > 0.0));
        let expected = run_parallel(&inputs, &AtomicBool::new(false)).unwrap();
        let budget = Duration::from_millis(2);
        let mut run = HydroRun::new(inputs, budget);
        let mut slices = Vec::new();
        let out = loop {
            let t = std::time::Instant::now();
            let done = run.poll_slice();
            slices.push(t.elapsed());
            if let Some(result) = done {
                break result.unwrap();
            }
        };
        assert_eq!(out.hydrology, expected.hydrology);
        assert!(
            !out.hydrology.rivers.nodes().is_empty(),
            "a trivial fixture"
        );
        let longest = *slices.iter().max().unwrap();
        println!(
            "{} slices (run counted {}), longest {longest:?}, longest step {:?}, {} units",
            slices.len(),
            out.slices,
            out.longest_step,
            out.units
        );
        assert!(slices.len() > 3, "{slices:?}");
        assert_eq!(out.slices as usize, slices.len());
        // The slack covers the poll itself and the result's assembly in the last slice.
        let bound = budget + out.longest_step + Duration::from_micros(500);
        for (k, s) in slices.iter().enumerate() {
            assert!(*s <= bound, "slice {k}: {s:?} > {bound:?}");
        }
        assert_eq!(out.units, expected.units);
    }

    #[test]
    fn a_cancelled_task_stops() {
        let inputs = tiny_inputs(
            0.0,
            LatticeSizes {
                recent_n: 16,
                deep_n: 8,
            },
        );
        let err = run_parallel(&inputs, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(err, "cancelled");
    }
}
