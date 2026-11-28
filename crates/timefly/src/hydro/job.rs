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
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU32, Ordering};
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
/// Units per [`HydroPrep::step`] on native: the task checks for cancellation between steps
/// (about 180 checks at n = 900, each step at most about 40 ms there, the pass's final build
/// being the longest) and is otherwise one long run.
pub const NATIVE_STEP_UNITS: u32 = 1 << 18;
/// The native task measures [`HydroPrep::working_bytes`] (which walks the basins) every this
/// many steps, and at the end.
pub const NATIVE_BYTES_EVERY: u32 = 4;
/// A time-boxed run's first step, in units; later steps aim at an eighth of the slice budget
/// (0.75 ms of a 6 ms slice, leaving room for a stage change's own work, about 0.7 ms
/// natively) from the measured rate, at most doubling from one step to the next, between
/// [`SLICED_MIN_UNITS`] and [`SLICED_MAX_UNITS`].
pub const SLICED_FIRST_UNITS: u32 = 1024;
pub const SLICED_MIN_UNITS: u32 = 64;
/// The cap: a unit's cost varies about 25× inside one stage (3–110 ns natively on the M4, real
/// data at 0 ka; sampling about 107 ns), so a step sized from cheap units overran when they
/// turned costly (20–30 ms steps). At 110 ns a unit this cap is about 0.9 ms natively; with
/// a stage change in the step the longest steps on real data read 1.1–1.3 ms (0 ka and 21 ka,
/// release; the run's total time is unchanged).
pub const SLICED_MAX_UNITS: u32 = 1 << 13;

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
    /// Where that step went: the stage it started in, or `from→to` when it crossed stages
    /// ([`HydroPrep::stage_name`]).
    pub longest_step_stage: String,
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

/// Why a native run ended without a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PassError {
    /// Its cancel flag was set: it stopped at the next sample block or step, having sampled
    /// `nodes_sampled` nodes and done `units` units of the pass after sampling.
    Cancelled { nodes_sampled: u64, units: u64 },
    /// The era's field could not be built.
    Field(String),
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::Cancelled { .. } => f.write_str("cancelled"),
            PassError::Field(e) => f.write_str(e),
        }
    }
}

/// The longest step so far and the stage (or stages) it ran in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LongestStep {
    pub time: Duration,
    /// `stage`, or `from→to` for a step that crossed stages.
    pub stage: String,
}

impl LongestStep {
    /// A step from stage `from` to stage `to` that took `spent`.
    pub fn observe(&mut self, from: &'static str, to: &'static str, spent: Duration) {
        if spent > self.time || self.stage.is_empty() {
            self.time = spent;
            self.stage = if from == to {
                from.to_string()
            } else {
                format!("{from}→{to}")
            };
        }
    }
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
    longest: &mut LongestStep,
) -> bool {
    let mut steps = 0u32;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let from = prep.stage_name();
        let t = Instant::now();
        let done = prep.step(source, climate, table, NATIVE_STEP_UNITS);
        longest.observe(from, prep.stage_name(), t.elapsed());
        steps += 1;
        if done || steps.is_multiple_of(NATIVE_BYTES_EVERY) {
            *peak = (*peak).max(prep.working_bytes());
        }
        if done {
            return true;
        }
    }
}

/// The pass on the calling thread, its sampling in parallel [`SAMPLE_BLOCK_NODES`] blocks
/// across the [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool). `cancel` stops it
/// within one sample block (each block checks it first, and sampling checks it once more at
/// the end) or one step.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_parallel(inputs: &HydroInputs, cancel: &AtomicBool) -> Result<PassOutput, PassError> {
    use bevy::tasks::AsyncComputeTaskPool;
    use timefly_core::hydrology::NodeBlock;

    let started = Instant::now();
    let field = inputs.field().map_err(PassError::Field)?;
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
            s.spawn(async move {
                if cancel.load(Ordering::Relaxed) {
                    // Skipped: the run is abandoned after the scope.
                    return NodeBlock {
                        start: range.start,
                        samples: Vec::new(),
                        data_lakes: false,
                        mapped: Vec::new(),
                    };
                }
                HydroPrep::sample_range(&lattice, shared, range)
            });
        }
    });
    // The flag only ever turns on, so a block that skipped is always seen here.
    if cancel.load(Ordering::Relaxed) {
        return Err(PassError::Cancelled {
            nodes_sampled: blocks.iter().map(|b| b.samples.len() as u64).sum(),
            units: 0,
        });
    }
    for block in blocks {
        prep.add_samples(block);
    }
    let sampling = started.elapsed();
    let mut peak = prep.working_bytes();
    let mut longest = LongestStep::default();
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
        return Err(PassError::Cancelled {
            nodes_sampled: u64::from(lattice.len()),
            units: prep.units_done() - u64::from(lattice.len()),
        });
    }
    let units = prep.units_done();
    let cpu = started.elapsed();
    Ok(PassOutput {
        hydrology: prep.finish(),
        cpu,
        sampling,
        slices: 1,
        longest_slice: cpu,
        longest_step: longest.time,
        longest_step_stage: longest.stage,
        peak_bytes: peak,
        units,
        beyer: beyer.is_some(),
    })
}

/// A time-boxed run's step size, adapted to the measured rate.
struct Stepper {
    budget: SharedBudget,
    units: u32,
    longest: LongestStep,
    steps: u32,
    peak: usize,
}

impl Stepper {
    fn observe(&mut self, units: u64, spent: Duration) {
        let target = self.budget.get().as_secs_f64() / 8.0;
        let grown = f64::from(self.units) * 2.0;
        let next = if spent.is_zero() {
            grown
        } else {
            (units as f64 * target / spent.as_secs_f64()).min(grown)
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
    slice: &mut Slice<impl Fn() -> Instant>,
    stepper: &mut Stepper,
) {
    loop {
        let before = prep.units_done();
        let from = prep.stage_name();
        let t = slice.now();
        let done = prep.step(source, climate, table, stepper.units);
        let spent = slice.now() - t;
        stepper.longest.observe(from, prep.stage_name(), spent);
        stepper.observe(prep.units_done() - before, spent);
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

async fn run_sliced(
    inputs: HydroInputs,
    budget: SharedBudget,
    now: impl Fn() -> Instant + Sync,
) -> Result<PassOutput, String> {
    let mut slice = Slice::with_clock(budget.clone(), &now);
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
        longest: LongestStep::default(),
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
        longest_step: stepper.longest.time,
        longest_step_stage: stepper.longest.stage,
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
        Self::with_clock(inputs, budget, Instant::now)
    }

    fn with_clock(
        inputs: HydroInputs,
        budget: Duration,
        now: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> HydroRun {
        let shared = SharedBudget::new(budget);
        HydroRun {
            future: Mutex::new(Box::pin(run_sliced(inputs, shared.clone(), now))),
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

/// A native task's cancel flag, also set when dropped (an app shutting down mid-pass): the
/// task stops within one sample block or one step.
#[cfg(not(target_arch = "wasm32"))]
struct CancelOnDrop(Arc<AtomicBool>);

#[cfg(not(target_arch = "wasm32"))]
impl CancelOnDrop {
    fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.cancel();
    }
}

enum Run {
    #[cfg(not(target_arch = "wasm32"))]
    Task {
        task: Task<Result<PassOutput, PassError>>,
        cancel: CancelOnDrop,
    },
    Sliced(HydroRun),
}

/// Native pass tasks running now and the most that ever ran at once.
#[derive(Debug, Default)]
struct TaskGauge {
    live: AtomicU32,
    peak: AtomicU32,
}

/// Counts a task as running until dropped (when its closure returns).
#[cfg(not(target_arch = "wasm32"))]
struct Live(Arc<TaskGauge>);

#[cfg(not(target_arch = "wasm32"))]
impl Live {
    fn enter(gauge: Arc<TaskGauge>) -> Live {
        let now = gauge.live.fetch_add(1, Ordering::SeqCst) + 1;
        gauge.peak.fetch_max(now, Ordering::SeqCst);
        Live(gauge)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Live {
    fn drop(&mut self) {
        self.0.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The native pass tasks. A replaced pass's task is cancelled and kept here until it has
/// stopped (within one sample block or one step); the next pass's run starts only then, so two
/// passes never share the pool or hold their working memory at once. On the web a replaced
/// run is a sliced future on the main thread and simply dropped.
#[derive(Resource, Default)]
pub struct HydroTasks {
    gauge: Arc<TaskGauge>,
    draining: Vec<Run>,
    replaced: u32,
    cancelled: u32,
    completed_after_replace: u32,
}

impl HydroTasks {
    /// Pass tasks running now.
    pub fn live(&self) -> u32 {
        self.gauge.live.load(Ordering::SeqCst)
    }

    /// The most pass tasks that ever ran at once.
    pub fn peak(&self) -> u32 {
        self.gauge.peak.load(Ordering::SeqCst)
    }

    /// Running passes replaced by another era's request.
    pub fn replaced(&self) -> u32 {
        self.replaced
    }

    /// Replaced passes that stopped early.
    pub fn cancelled(&self) -> u32 {
        self.cancelled
    }

    /// Replaced passes that finished anyway (their result discarded).
    pub fn completed_after_replace(&self) -> u32 {
        self.completed_after_replace
    }

    /// Whether a replaced task is still stopping.
    pub fn draining(&self) -> bool {
        !self.draining.is_empty()
    }
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
    /// Its run moved to [`HydroTasks`] to stop; the job is being replaced.
    #[cfg(not(target_arch = "wasm32"))]
    Replaced,
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
    job: Option<ResMut<HydroJob>>,
    mut tasks: ResMut<HydroTasks>,
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
    // A running native task is cancelled and kept until it stops ([`HydroTasks`]).
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(mut old) = job
        && matches!(old.stage, Stage::Running(Run::Task { .. }))
        && let Stage::Running(run) = std::mem::replace(&mut old.stage, Stage::Replaced)
    {
        if let Run::Task { cancel, .. } = &run {
            cancel.cancel();
        }
        debug!(
            age_years = old.age.0,
            "continental pass replaced; its task is stopping"
        );
        tasks.replaced += 1;
        tasks.draining.push(run);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (job, &mut tasks);
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

/// Takes in replaced tasks that have stopped.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn poll_replaced(mut tasks: ResMut<HydroTasks>) {
    let tasks = &mut *tasks;
    let (mut cancelled, mut completed) = (0, 0);
    tasks.draining.retain_mut(|run| match run {
        Run::Task { task, .. } => match bevy::tasks::futures::check_ready(task) {
            None => true,
            Some(Err(PassError::Cancelled {
                nodes_sampled,
                units,
            })) => {
                debug!(nodes_sampled, units, "replaced continental pass stopped");
                cancelled += 1;
                false
            }
            Some(Ok(_)) => {
                completed += 1;
                false
            }
            Some(Err(PassError::Field(_))) => false,
        },
        Run::Sliced(_) => false,
    });
    tasks.cancelled += cancelled;
    tasks.completed_after_replace += completed;
}

/// The web drops a replaced sliced run at once: nothing is ever kept to stop.
#[cfg(target_arch = "wasm32")]
pub(super) fn poll_replaced(mut tasks: ResMut<HydroTasks>) {
    tasks.draining.clear();
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
    tasks: Res<HydroTasks>,
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
    // Never two passes at once: a replaced task must have stopped first.
    if ready.is_none() || !index.overview_settled() || tasks.draining() {
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
        start_task(inputs, &tasks)
    };
    job.stage = Stage::Running(run);
}

/// The native run: a task on the async compute pool.
#[cfg(not(target_arch = "wasm32"))]
fn start_task(inputs: HydroInputs, tasks: &HydroTasks) -> Run {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let gauge = tasks.gauge.clone();
    Run::Task {
        task: bevy::tasks::AsyncComputeTaskPool::get().spawn(async move {
            let _live = Live::enter(gauge);
            run_parallel(&inputs, &flag)
        }),
        cancel: CancelOnDrop(cancel),
    }
}

/// The web has no task pool threads (never called there: the web's runs are sliced).
#[cfg(target_arch = "wasm32")]
fn start_task(inputs: HydroInputs, _tasks: &HydroTasks) -> Run {
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
        Run::Task { task, .. } => {
            bevy::tasks::futures::check_ready(task).map(|r| r.map_err(|e| e.to_string()))
        }
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
        longest_step_stage = out.longest_step_stage.as_str(),
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
        // One clock tick per observation, including step start, step end, and the budget
        // check. Only the run advances this clock, so time descheduled cannot extend a poll.
        let tick = Duration::from_micros(250);
        let ticks = Arc::new(AtomicU32::new(0));
        let clock_ticks = ticks.clone();
        let epoch = Instant::now();
        let mut run = HydroRun::with_clock(inputs, budget, move || {
            epoch + tick * clock_ticks.fetch_add(1, Ordering::Relaxed)
        });
        let mut polls = 0;
        let out = loop {
            let before = ticks.load(Ordering::Relaxed);
            let result = run.poll_slice();
            polls += 1;
            // The first observation starts the slice. A complete iteration costs three
            // ticks. A pending poll must return at the first check at or beyond its budget.
            let spent = tick * (ticks.load(Ordering::Relaxed) - before - 1);
            assert!(spent < budget + 3 * tick, "poll {polls} spent {spent:?}");
            if let Some(result) = result {
                break result.unwrap();
            }
            assert!(spent >= budget, "poll {polls} yielded early at {spent:?}");
        };
        assert_eq!(out.hydrology, expected.hydrology);
        assert!(!out.hydrology.rivers.nodes().is_empty());
        assert!(polls > 3);
        assert_eq!(out.slices, polls);
        assert_eq!(out.longest_step, tick);
        assert_eq!(out.units, expected.units);
    }

    /// A cancelled run samples nothing more: each block checks the flag before sampling.
    #[test]
    fn a_cancelled_task_stops_before_sampling() {
        let inputs = tiny_inputs(
            0.0,
            LatticeSizes {
                recent_n: 64,
                deep_n: 8,
            },
        );
        let err = run_parallel(&inputs, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(
            err,
            PassError::Cancelled {
                nodes_sampled: 0,
                units: 0
            }
        );
        assert_eq!(err.to_string(), "cancelled");
    }

    /// Rivers and lakes Task 10, fix round 1: on real data a unit's cost varies about 25×
    /// inside one stage (3–110 ns natively), so a step sized from cheap units ran 20–30 ms
    /// when they turned costly. The step may at most double, and never passes the cap.
    #[test]
    fn a_time_boxed_step_stays_short_when_units_turn_costly() {
        let mut s = Stepper {
            budget: SharedBudget::new(Duration::from_millis(6)),
            units: SLICED_FIRST_UNITS,
            longest: LongestStep::default(),
            steps: 0,
            peak: 0,
        };
        // Cheap units, 1 ns each: each step at most doubles the last, up to the cap.
        for _ in 0..40 {
            let before = s.units;
            s.observe(u64::from(before), Duration::from_nanos(u64::from(before)));
            assert!(s.units <= 2 * before, "{before} -> {}", s.units);
        }
        assert_eq!(s.units, SLICED_MAX_UNITS);
        // At the costliest units measured (about 110 ns), the cap's step is under 1 ms.
        assert!(
            f64::from(SLICED_MAX_UNITS) * 110e-9 <= 1.0e-3,
            "{SLICED_MAX_UNITS}"
        );
        // A slow step shrinks the next to an eighth of the budget at its rate.
        s.observe(u64::from(s.units), Duration::from_micros(1_500));
        assert_eq!(s.units, SLICED_MAX_UNITS / 2);
        // An empty step (nothing measured) doubles, within the cap.
        let before = s.units;
        s.observe(u64::from(before), Duration::ZERO);
        assert_eq!(s.units, (2 * before).min(SLICED_MAX_UNITS));
    }
}
