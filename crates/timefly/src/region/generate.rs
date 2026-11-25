//! The generation runner: builds a region's coarse field from owned inputs and runs Task 4's
//! generator, either across [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool) tasks
//! (native) or in slices of at most about [`WEB_GEN_BUDGET_MS`](super::WEB_GEN_BUDGET_MS) per
//! frame (the web, where the pool runs on the main thread).
//!
//! Inputs are owned (`Arc`s and a [`TileSet`] of `Arc`s), so a run can leave the main thread;
//! the field, the prep and the generator borrow from them inside one future or task.

use std::future::Future;
use std::ops::Range;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use bevy::platform::time::Instant;
use timefly_core::formats::Grid;
use timefly_core::geodynamics::CrustModel;
use timefly_core::hydrology::params::MAX_BOULDERS;
use timefly_core::hydrology::{EraHydrology, LatticeSizes, hydro_lattice_for, river_tile_margin_m};
use timefly_core::surface::Timeline;
use timefly_core::worldgen::coarse::{
    CoarseField, FieldError, SnapshotField, frames_needed, snapshot_field,
};
use timefly_core::worldgen::generate::{
    FieldKind, FieldRows, RegionGenerator, RegionGrid, RegionPrep, RegionSpec,
};
use timefly_core::worldgen::tiles::{TILE_MARGIN_M, TileSet};
use tracing::{info, instrument, warn};

/// Rows per parallel task on native: small enough to balance the pool's threads.
pub const NATIVE_BLOCK_ROWS: u32 = 16;
/// Lattice units per [`RegionPrep::step`] in a time-boxed slice. The largest measured step is
/// about 0.45 ms on an M4 (Task 4).
pub const WEB_PREP_STEP: u32 = 256;

/// Why a region could not be generated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenError {
    /// Timeline frame `frame`, which the snapshot reads, is not loaded.
    MissingFrame(usize),
    Field(FieldError),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::MissingFrame(i) => write!(f, "timeline frame {i} is not loaded"),
            GenError::Field(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GenError {}

/// Everything a run needs, owned.
#[derive(Clone)]
pub struct GenInputs {
    pub spec: RegionSpec,
    pub timeline: Arc<Timeline>,
    pub crust: Arc<CrustModel>,
    /// The cached tiles of the region's needed set, and the overview.
    pub tiles: TileSet,
    /// Timeline frame `i`'s CPU grid at index `i`, for every frame
    /// [`frames_needed`] lists; other entries may be `None`.
    pub frames: Vec<Option<Arc<Grid>>>,
    /// Some needed tile is missing (`tiles.missing(&needed)` is not empty).
    pub fallback: bool,
    /// The era's continental pass ([`crate::hydro`]), or `None` when it is disabled or failed.
    /// The generator refines its rivers and lakes on the region's terrain (`RegionPrep::new`'s
    /// `hydro`); with `None` the region has M3's heights and only the sea for water.
    pub hydro: Option<Arc<EraHydrology>>,
}

fn field<'a>(
    spec: &RegionSpec,
    timeline: &Timeline,
    crust: &'a CrustModel,
    tiles: &'a TileSet,
    frames: &'a [Option<Arc<Grid>>],
    fallback: bool,
) -> Result<SnapshotField<'a>, GenError> {
    let age_myr = spec.snapshot.myr();
    for i in frames_needed(timeline, age_myr) {
        if frames.get(i).is_none_or(Option::is_none) {
            return Err(GenError::MissingFrame(i));
        }
    }
    snapshot_field(timeline, crust, tiles, fallback, age_myr, |i| {
        frames[i].as_deref().expect("checked above")
    })
    .map_err(GenError::Field)
}

/// The tiles the region's inner field reads, widened by `margin_m` beyond the inner square
/// ([`tile_margin_m`]), from a probe field built without tiles (`tiles_needed` does not depend
/// on the tiles loaded or on `fallback`).
#[instrument(level = "debug", skip_all, fields(snapshot_myr = spec.snapshot.myr(), margin_m), err)]
pub fn tiles_needed(
    spec: &RegionSpec,
    timeline: &Timeline,
    crust: &CrustModel,
    frames: &[Option<Arc<Grid>>],
    margin_m: f64,
) -> Result<Vec<(u8, u8)>, GenError> {
    let empty = TileSet::new();
    let probe = field(spec, timeline, crust, &empty, frames, false)?;
    Ok(probe.tiles_needed_with_margin(&spec.frame(), margin_m))
}

/// How far beyond the inner square a region reads its tiles: [`TILE_MARGIN_M`], or with the
/// era's pass enabled ([`HydroSettings`](crate::hydro::HydroSettings), `hydro_sizes`), the reach
/// of its rivers' refinement on the era's lattice ([`river_tile_margin_m`], about 116 km on the
/// recent lattice), whichever is wider.
pub fn tile_margin_m(timeline: &Timeline, age_myr: f64, hydro_sizes: Option<LatticeSizes>) -> f64 {
    hydro_sizes.map_or(TILE_MARGIN_M, |sizes| {
        let (lattice, _) = hydro_lattice_for(timeline, age_myr, sizes);
        TILE_MARGIN_M.max(river_tile_margin_m(&lattice))
    })
}

impl GenInputs {
    fn field(&self) -> Result<SnapshotField<'_>, GenError> {
        field(
            &self.spec,
            &self.timeline,
            &self.crust,
            &self.tiles,
            &self.frames,
            self.fallback,
        )
    }
}

/// Warns once when the region had more boulders than [`MAX_BOULDERS`] (core does not log).
fn warn_dropped_boulders<F: CoarseField>(generator: &RegionGenerator<'_, F>) {
    let dropped = generator.boulders_dropped();
    if dropped > 0 {
        warn!(
            dropped,
            kept = MAX_BOULDERS,
            "region has more boulders than the cap; dropped the rest"
        );
    }
}

/// Consecutive row ranges of at most `block` rows covering `0..rows`, in order.
pub fn row_blocks(rows: u32, block: u32) -> Vec<Range<u32>> {
    let block = block.max(1);
    (0..rows.div_ceil(block))
        .map(|b| b * block..((b + 1) * block).min(rows))
        .collect()
}

/// Generates the region on the calling thread for the prep, then across the
/// [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool) for the rows: disjoint row
/// blocks, each task reading the one shared (`Sync`) generator. Blocks come back in spawn
/// order, which is the order [`RegionGenerator::assemble`] needs.
///
/// [`RegionGenerator::assemble`]: timefly_core::worldgen::generate::RegionGenerator::assemble
#[cfg(not(target_arch = "wasm32"))]
#[instrument(level = "debug", skip_all, fields(snapshot_myr = inputs.spec.snapshot.myr(), fallback = inputs.fallback), err)]
pub fn generate_parallel(inputs: &GenInputs) -> Result<RegionGrid, GenError> {
    use bevy::tasks::AsyncComputeTaskPool;

    use crate::diagnostics::hop_memory::trace_generation;

    let started = Instant::now();
    let field = inputs.field()?;
    let owned = RegionPrep::new(inputs.spec, &field, inputs.hydro.as_deref()).finish();
    let prep_ms = started.elapsed().as_secs_f64() * 1e3;
    warn_dropped_boulders(&owned);
    trace_generation("prep done: lattice and coast field", || {
        owned.working_bytes()
    });
    let generator = &owned;
    let inner = row_blocks(generator.rows_in(FieldKind::Inner), NATIVE_BLOCK_ROWS);
    let apron = row_blocks(generator.rows_in(FieldKind::Apron), NATIVE_BLOCK_ROWS);
    let inner_blocks = inner.len();
    let mut blocks = AsyncComputeTaskPool::get().scope(|s| {
        for (kind, ranges) in [(FieldKind::Inner, &inner), (FieldKind::Apron, &apron)] {
            for range in ranges.iter().cloned() {
                s.spawn(async move { generator.rows(kind, range) });
            }
        }
    });
    trace_generation("rows done: row blocks", || {
        blocks.iter().map(FieldRows::bytes).sum()
    });
    let apron_blocks = blocks.split_off(inner_blocks);
    let grid = generator.assemble(blocks, apron_blocks);
    trace_generation("assembled: grid", || grid.bytes());
    drop(owned);
    trace_generation("generator dropped", || 0);
    info!(
        prep_ms = format!("{prep_ms:.0}"),
        rows_ms = format!("{:.0}", started.elapsed().as_secs_f64() * 1e3 - prep_ms),
        threads = AsyncComputeTaskPool::get().thread_num(),
        "region generated in parallel"
    );
    Ok(grid)
}

/// A future that is pending once: it hands control back to whoever polls the run.
pub(crate) struct YieldOnce(pub(crate) bool);

impl Future for YieldOnce {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            Poll::Ready(())
        } else {
            self.0 = true;
            Poll::Pending
        }
    }
}

/// A slice budget the run reads at every check and its owner may change between slices
/// ([`BudgetedRun::set_budget`]), in nanoseconds.
#[derive(Clone, Debug)]
pub(crate) struct SharedBudget(Arc<AtomicU64>);

impl SharedBudget {
    pub(crate) fn new(budget: Duration) -> SharedBudget {
        let b = SharedBudget(Arc::new(AtomicU64::new(0)));
        b.set(budget);
        b
    }

    pub(crate) fn set(&self, budget: Duration) {
        let nanos = u64::try_from(budget.as_nanos()).unwrap_or(u64::MAX);
        self.0.store(nanos, Ordering::Relaxed);
    }

    pub(crate) fn get(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::Relaxed))
    }
}

/// The time spent in the current slice, and the slices so far.
pub(crate) struct Slice {
    budget: SharedBudget,
    started: Instant,
    pub(crate) count: u32,
    pub(crate) longest: Duration,
    /// The time of every slice so far, the current one up to its last [`Slice::record`].
    pub(crate) total: Duration,
    /// The time of the finished slices.
    done: Duration,
}

impl Slice {
    pub(crate) fn new(budget: SharedBudget) -> Slice {
        Slice {
            budget,
            started: Instant::now(),
            count: 1,
            longest: Duration::ZERO,
            total: Duration::ZERO,
            done: Duration::ZERO,
        }
    }

    /// Counts the time spent so far in this slice towards the longest slice and the total.
    pub(crate) fn record(&mut self) -> Duration {
        let spent = self.started.elapsed();
        self.longest = self.longest.max(spent);
        self.total = self.done + spent;
        spent
    }

    /// Yields when the slice's budget is spent and starts the next slice on resuming.
    pub(crate) async fn yield_if_spent(&mut self) {
        let spent = self.record();
        if spent >= self.budget.get() {
            self.done += spent;
            YieldOnce(false).await;
            self.started = Instant::now();
            self.count += 1;
        }
    }
}

/// `earlier` is main-thread work for this region done before the run (the tile probe), which
/// counts as a slice of its own.
async fn run_budgeted(
    inputs: GenInputs,
    budget: SharedBudget,
    earlier: Duration,
) -> Result<RegionGrid, GenError> {
    let started = Instant::now();
    let mut slice = Slice::new(budget);
    slice.longest = earlier;
    let field = inputs.field()?;
    slice.yield_if_spent().await;
    let mut prep = RegionPrep::new(inputs.spec, &field, inputs.hydro.as_deref());
    while !prep.step(WEB_PREP_STEP) {
        slice.yield_if_spent().await;
    }
    slice.yield_if_spent().await;
    // Done stepping, so this does no lattice work; timed like everything else.
    let generator = prep.finish();
    warn_dropped_boulders(&generator);
    slice.yield_if_spent().await;
    let mut done: [Vec<FieldRows>; 2] = [Vec::new(), Vec::new()];
    for (k, kind) in [FieldKind::Inner, FieldKind::Apron].into_iter().enumerate() {
        for range in row_blocks(generator.rows_in(kind), 1) {
            done[k].push(generator.rows(kind, range));
            slice.yield_if_spent().await;
        }
    }
    let [inner, apron] = done;
    let grid = generator.assemble(inner, apron);
    slice.record();
    info!(
        slices = slice.count,
        longest_ms = format!("{:.1}", slice.longest.as_secs_f64() * 1e3),
        total_ms = format!("{:.0}", started.elapsed().as_secs_f64() * 1e3),
        "region generated in time-boxed slices"
    );
    Ok(grid)
}

type RunFuture = Pin<Box<dyn Future<Output = Result<RegionGrid, GenError>> + Send>>;

/// A time-boxed run: each [`poll_slice`](Self::poll_slice) works until about its budget has
/// passed (plus at most one prep step, one row, or the final assembly), then returns. The budget
/// can change between slices ([`set_budget`](Self::set_budget)). Built for
/// the web, where it is polled once per frame; it runs the same on native (and is tested
/// there). Its closing log line's `longest_ms` covers every slice, the field build,
/// `prep.finish()`, `assemble` and the tile probe that preceded the run.
pub struct BudgetedRun {
    // A `Mutex` only to make the run `Sync` for a Bevy resource; it is never contended.
    future: Mutex<RunFuture>,
    /// The budget it was started with.
    base: Duration,
    budget: SharedBudget,
}

impl BudgetedRun {
    /// `earlier` is main-thread time already spent on this region in an earlier frame (the tile
    /// probe), reported with the slices.
    pub fn new(inputs: GenInputs, budget: Duration, earlier: Duration) -> BudgetedRun {
        let shared = SharedBudget::new(budget);
        BudgetedRun {
            future: Mutex::new(Box::pin(run_budgeted(inputs, shared.clone(), earlier))),
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

    /// The budget the next slice gets.
    pub fn budget(&self) -> Duration {
        self.budget.get()
    }

    /// Runs one slice; `Some` when the run has finished.
    pub fn poll_slice(&mut self) -> Option<Result<RegionGrid, GenError>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_blocks_cover_in_order() {
        assert_eq!(row_blocks(5, 2), vec![0..2, 2..4, 4..5]);
        assert_eq!(row_blocks(4, 2), vec![0..2, 2..4]);
        assert_eq!(row_blocks(3, 0), vec![0..1, 1..2, 2..3]);
        assert!(row_blocks(0, 4).is_empty());
        let blocks = row_blocks(1025, NATIVE_BLOCK_ROWS);
        assert_eq!(blocks.first().unwrap().start, 0);
        assert_eq!(blocks.last().unwrap().end, 1025);
        assert!(blocks.windows(2).all(|w| w[0].end == w[1].start));
    }

    #[test]
    fn a_shared_budget_changes_between_slices() {
        let budget = SharedBudget::new(Duration::from_millis(6));
        let seen_by_run = budget.clone();
        assert_eq!(seen_by_run.get(), Duration::from_millis(6));
        budget.set(Duration::from_millis(12));
        assert_eq!(seen_by_run.get(), Duration::from_millis(12));
        budget.set(Duration::from_secs(u64::MAX));
        assert_eq!(seen_by_run.get(), Duration::from_nanos(u64::MAX));
    }

    #[test]
    fn yield_once_is_pending_once() {
        let mut y = std::pin::pin!(YieldOnce(false));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(y.as_mut().poll(&mut cx).is_pending());
        assert!(y.as_mut().poll(&mut cx).is_ready());
    }
}
