//! The continental water pass in the shell (rivers and lakes design §4.1, Task 4): Task 3's
//! [`HydroPrep`](timefly_core::hydrology::HydroPrep) run once per era and kept in a
//! [`HydroCache`] of [`HYDRO_CACHE_ERAS`] eras.
//!
//! - **Prefetch.** On the globe, once the snapshot nearest [`GlobeTime`] has not changed for
//!   [`HYDRO_SETTLE_S`] on the [`LoadClock`], the plugin asks for its pass
//!   ([`HydroPurpose::Prefetch`]), unless it is cached, running or failed. A slider drag through
//!   many eras therefore runs one pass, for the era it settles on.
//! - **Region jobs** reaching their tiles stage wait for their era's pass ([`HydroGate`]): an
//!   absent era is asked for once ([`HydroPurpose::Region`]); a failed one generates with
//!   `hydro: None` and a `warn`; a wait longer than [`HYDRO_WAIT_MAX_S`] gives up the same way,
//!   so a dive or a hop's hold never waits on the pass past M3's bounds.
//! - **One pass at a time** ([`admit`]): a region request replaces a prefetch (or a region pass
//!   for another era); a prefetch replaces only a prefetch. A replaced native task is cancelled
//!   (it stops within one sample block or one step) and the next pass's run waits until it has
//!   stopped ([`job::HydroTasks`]), so two passes never run at once.
//! - **The run.** Native: an [`AsyncComputeTaskPool`](bevy::tasks::AsyncComputeTaskPool) task
//!   that samples the lattice in parallel blocks, then steps the pass. Web: time-boxed slices of
//!   [`WEB_HYDRO_BUDGET_MS`] per frame, or
//!   [`WEB_GEN_LARGE_BUDGET_MS`](crate::region::WEB_GEN_LARGE_BUDGET_MS) while
//!   [`LargeGenBudget`](crate::region::LargeGenBudget) says no region is on screen
//!   ([`job::HydroRun`]).
//! - **Climate** ([`climate`]): the Beyer et al. 2020 grids up to 120 ka, the zonal rule beyond;
//!   a missing or failed climate manifest or grid means zonal for every age, one `warn` per
//!   session.
//!
//! Each pass sends [`HydroPassFinished`] (and logs `continental pass done` at `info`) or
//! [`HydroPassFailed`], and warns once for each lake level table entry it could not place.

pub mod climate;
pub mod job;

use std::sync::Arc;
use std::time::Duration;

use bevy::ecs::system::SystemParam;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::hydrology::params::{HYDRO_CACHE_ERAS, HYDRO_SETTLE_S};
use timefly_core::hydrology::{EraHydrology, LatticeSizes};

use crate::clock::LoadClock;
use crate::data::GlobeData;
use crate::diagnostics::aspects::DiagnosticsAppExt;
use crate::region::{LOAD_TIMEOUT_S, RegionState, RegionSystems};
use crate::time::{GlobeTime, nearest_snapshot};
use climate::{ClimateIndex, ClimateManifest, ClimateManifestLoader};

/// The web's slice budget per frame for the continental pass on the globe. While
/// [`LargeGenBudget`](crate::region::LargeGenBudget) is true (an opaque cloud, no region on
/// screen) the pass takes [`WEB_GEN_LARGE_BUDGET_MS`](crate::region::WEB_GEN_LARGE_BUDGET_MS)
/// instead.
pub const WEB_HYDRO_BUDGET_MS: f64 = 6.0;
/// The longest a region job waits for its era's pass, on the [`LoadClock`] *(call: the region
/// job's own load timeout)*. Past it the region generates without the era's hydrology and a
/// `warn`, leaving M3's hold bound (`HOLD_MAX_S`, twice this) the time to generate.
pub const HYDRO_WAIT_MAX_S: f64 = LOAD_TIMEOUT_S;

/// Who asked for an era's pass.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HydroPurpose {
    /// The globe time settled on the era ([`HYDRO_SETTLE_S`]).
    Prefetch,
    /// A region job waits for it.
    Region,
}

/// Request: run the continental pass for `age` (a standard snapshot). Applied by [`admit`].
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct HydroRequested {
    pub age: YearsBp,
    pub purpose: HydroPurpose,
}

/// Domain event: an era's pass is done and in the [`HydroCache`]. `wall_ms` runs from the
/// request to the result (the frame and climate loads included); `cpu_ms` is the pass's own
/// compute (natively the task's time, its parallel sampling counted once; on the web the sum of
/// its slices); `slices` is 1 for a native task; `nodes` is the lattice's size.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct HydroPassFinished {
    pub age: YearsBp,
    pub wall_ms: u32,
    pub cpu_ms: u32,
    pub slices: u32,
    pub nodes: u32,
    pub river_nodes: u32,
    pub lakes: u32,
    /// `"native"` or `"web"`.
    pub platform: &'static str,
}

/// Domain event: an era's pass failed (a frame that will not load, an invalid field). Logged
/// at `warn`; regions of the era generate without its hydrology.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct HydroPassFailed {
    pub age: YearsBp,
    pub reason: String,
}

/// Continental pass settings (default on, at the plan's lattice sizes). Disabled, no pass runs
/// and a region never waits: it generates with `hydro: None`. Tests lower the sizes.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct HydroSettings {
    pub enabled: bool,
    pub sizes: LatticeSizes,
}

impl Default for HydroSettings {
    fn default() -> Self {
        HydroSettings {
            enabled: true,
            sizes: LatticeSizes::default(),
        }
    }
}

/// An era's pass, as the cache knows it.
#[derive(Clone, Debug, PartialEq)]
pub enum HydroStatus {
    Ready(Arc<EraHydrology>),
    /// The pass in flight is this era's.
    Running,
    /// The era's pass failed this session; it is not retried.
    Failed,
    Absent,
}

/// The pass in flight.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RunningPass {
    pub age: YearsBp,
    pub purpose: HydroPurpose,
}

#[derive(Debug)]
struct CachedEra {
    key: u64,
    hydrology: Arc<EraHydrology>,
    last_used: u64,
}

/// Failed eras remembered (so a failure is not retried every frame); the oldest is forgotten.
const FAILED_ERAS: usize = 8;

/// Finished passes, least recently used first out, at most [`HYDRO_CACHE_ERAS`] eras keyed by
/// `age.0.to_bits()`; plus the pass in flight and the eras whose pass failed.
#[derive(Resource, Default, Debug)]
pub struct HydroCache {
    eras: Vec<CachedEra>,
    clock: u64,
    running: Option<RunningPass>,
    failed: Vec<u64>,
}

fn key(age: YearsBp) -> u64 {
    age.0.to_bits()
}

impl HydroCache {
    /// The era's status, without marking it used.
    pub fn status(&self, age: YearsBp) -> HydroStatus {
        let k = key(age);
        if let Some(e) = self.eras.iter().find(|e| e.key == k) {
            HydroStatus::Ready(e.hydrology.clone())
        } else if self.running.is_some_and(|r| key(r.age) == k) {
            HydroStatus::Running
        } else if self.failed.contains(&k) {
            HydroStatus::Failed
        } else {
            HydroStatus::Absent
        }
    }

    /// The era's result, marked as just used.
    pub fn get(&mut self, age: YearsBp) -> Option<Arc<EraHydrology>> {
        self.clock += 1;
        let clock = self.clock;
        self.eras.iter_mut().find(|e| e.key == key(age)).map(|e| {
            e.last_used = clock;
            e.hydrology.clone()
        })
    }

    /// Adds an era's result (keyed by its age), then evicts the least recently used eras down
    /// to [`HYDRO_CACHE_ERAS`].
    pub fn insert(&mut self, hydrology: Arc<EraHydrology>) {
        self.clock += 1;
        let k = key(hydrology.age);
        self.eras.retain(|e| e.key != k);
        self.failed.retain(|&f| f != k);
        self.eras.push(CachedEra {
            key: k,
            hydrology,
            last_used: self.clock,
        });
        while self.eras.len() > HYDRO_CACHE_ERAS {
            let oldest = (0..self.eras.len())
                .min_by_key(|&i| self.eras[i].last_used)
                .expect("not empty");
            self.eras.remove(oldest);
        }
    }

    /// Cached eras.
    pub fn len(&self) -> usize {
        self.eras.len()
    }

    pub fn is_empty(&self) -> bool {
        self.eras.is_empty()
    }

    /// The pass in flight.
    pub fn running(&self) -> Option<RunningPass> {
        self.running
    }

    fn mark_failed(&mut self, age: YearsBp) {
        let k = key(age);
        self.failed.retain(|&f| f != k);
        self.failed.push(k);
        if self.failed.len() > FAILED_ERAS {
            self.failed.remove(0);
        }
    }

    /// Whether the era needs no pass: cached or failed.
    fn settled(&self, age: YearsBp) -> bool {
        matches!(
            self.status(age),
            HydroStatus::Ready(_) | HydroStatus::Failed
        )
    }
}

/// What a [`HydroRequested`] does to the pass in flight.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Start the requested era's pass, replacing any in flight.
    Start,
    /// The pass in flight is the requested era's: a region now waits for it.
    Upgrade,
    Ignore,
}

/// The one-pass-at-a-time rule. A request for an era that is cached or failed (`settled`), or
/// already running, starts nothing. A region request replaces any other pass; a prefetch
/// replaces only a prefetch (the slider settled elsewhere), never a region's pass.
pub fn admit(running: Option<RunningPass>, settled: bool, request: &HydroRequested) -> Admit {
    use HydroPurpose::{Prefetch, Region};
    if settled {
        return Admit::Ignore;
    }
    match running {
        None => Admit::Start,
        Some(r) if key(r.age) == key(request.age) => match (r.purpose, request.purpose) {
            (Prefetch, Region) => Admit::Upgrade,
            _ => Admit::Ignore,
        },
        Some(_) if request.purpose == Region => Admit::Start,
        Some(r) if r.purpose == Prefetch => Admit::Start,
        Some(_) => Admit::Ignore,
    }
}

/// A region job's wait for its era's pass ([`HydroGate::wait`]).
#[derive(Debug, Default)]
pub struct HydroWait {
    /// When the wait began (wall clock, and [`LoadClock`] seconds for its bound).
    since: Option<(Instant, f64)>,
    requested: bool,
    waited: Duration,
}

impl HydroWait {
    /// Whether the job is waiting now.
    pub fn waiting(&self) -> bool {
        self.since.is_some()
    }

    /// How long the finished wait took (wall clock; zero when the era was ready at once).
    pub fn waited(&self) -> Duration {
        self.waited
    }

    fn finish(&mut self, hydro: Option<Arc<EraHydrology>>) -> Option<Arc<EraHydrology>> {
        if let Some((since, _)) = self.since.take() {
            self.waited = since.elapsed();
        }
        hydro
    }
}

/// What a region job needs to wait for its era's pass. Works without the [`HydroPlugin`]: then
/// a region never waits.
#[derive(SystemParam)]
pub struct HydroGate<'w> {
    settings: Option<Res<'w, HydroSettings>>,
    cache: Option<ResMut<'w, HydroCache>>,
    requests: Option<ResMut<'w, Messages<HydroRequested>>>,
}

impl HydroGate<'_> {
    /// `Some(hydro)` once the job may generate: the era's result, or `None` when the pass is
    /// disabled, failed, or took longer than [`HYDRO_WAIT_MAX_S`] (`now_s` on the
    /// [`LoadClock`]). `None` while it waits; an absent era is asked for once.
    pub fn wait(
        &mut self,
        age: YearsBp,
        wait: &mut HydroWait,
        now_s: f64,
    ) -> Option<Option<Arc<EraHydrology>>> {
        let enabled = self.settings.as_ref().is_some_and(|s| s.enabled);
        let Some(cache) = self.cache.as_mut().filter(|_| enabled) else {
            return Some(wait.finish(None));
        };
        match cache.status(age) {
            HydroStatus::Ready(_) => {
                let hydro = cache.get(age);
                Some(wait.finish(hydro))
            }
            HydroStatus::Failed => {
                warn!(
                    age_years = age.0,
                    "the era's continental pass failed; the region generates without its rivers \
                     and lakes"
                );
                Some(wait.finish(None))
            }
            status => {
                let (_, since_s) = *wait.since.get_or_insert((Instant::now(), now_s));
                if status == HydroStatus::Absent
                    && !wait.requested
                    && let Some(requests) = self.requests.as_mut()
                {
                    requests.write(HydroRequested {
                        age,
                        purpose: HydroPurpose::Region,
                    });
                    wait.requested = true;
                }
                if now_s - since_s >= HYDRO_WAIT_MAX_S {
                    warn!(
                        age_years = age.0,
                        wait_s = HYDRO_WAIT_MAX_S,
                        "the era's continental pass is taking too long; the region generates \
                         without its rivers and lakes"
                    );
                    return Some(wait.finish(None));
                }
                None
            }
        }
    }
}

/// The snapshot the globe time stood on and since when ([`LoadClock`] seconds).
#[derive(Resource, Default)]
struct Settle(Option<(YearsBp, f64)>);

/// Asks for the settled era's pass ([`HydroPurpose::Prefetch`]) on the globe.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn prefetch_settled_era(
    time: Option<Res<GlobeTime>>,
    state: Option<Res<State<RegionState>>>,
    globe: Option<Res<GlobeData>>,
    settings: Res<HydroSettings>,
    cache: Res<HydroCache>,
    clock: Res<LoadClock>,
    mut settle: ResMut<Settle>,
    mut requests: MessageWriter<HydroRequested>,
) {
    let (Some(time), true, true) = (time, globe.is_some(), settings.enabled) else {
        return;
    };
    let now_s = clock.elapsed_s;
    let snapshot = nearest_snapshot(time.years);
    let since_s = match settle.0 {
        Some((s, since_s)) if s == snapshot => since_s,
        _ => {
            settle.0 = Some((snapshot, now_s));
            now_s
        }
    };
    let on_globe = state.is_none_or(|s| *s.get() == RegionState::Globe);
    let busy = cache
        .running()
        .is_some_and(|r| r.purpose == HydroPurpose::Region);
    if on_globe
        && now_s - since_s >= HYDRO_SETTLE_S
        && cache.status(snapshot) == HydroStatus::Absent
        && !busy
    {
        requests.write(HydroRequested {
            age: snapshot,
            purpose: HydroPurpose::Prefetch,
        });
    }
}

/// The continental pass's systems: after the region job's, so a region's request starts its
/// pass in the same frame.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HydroSystems;

/// Runs the continental pass per era. Add it after `RegionPlugin`, whose tile index, frame cache
/// and grid loader it shares.
pub struct HydroPlugin;

impl Plugin for HydroPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ClimateManifest>()
            .init_asset_loader::<ClimateManifestLoader>()
            .init_resource::<HydroSettings>()
            .init_resource::<HydroCache>()
            .init_resource::<ClimateIndex>()
            .init_resource::<Settle>()
            .init_resource::<job::HydroTasks>()
            .add_domain_event::<HydroRequested>()
            .add_domain_event::<HydroPassFinished>()
            .add_domain_event::<HydroPassFailed>()
            .configure_sets(Update, HydroSystems.after(RegionSystems))
            .add_systems(
                Update,
                (
                    climate::poll_climate_manifest,
                    prefetch_settled_era,
                    job::start_hydro_requests,
                    job::poll_replaced,
                    job::advance_hydro_job,
                    job::poll_hydro_run,
                )
                    .chain()
                    .in_set(HydroSystems),
            );
        crate::clock::add_load_clock(app);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use timefly_core::geo::RenderDir;
    use timefly_core::hydrology::climate::ZonalClimate;
    use timefly_core::hydrology::{CubeLattice, HydroPrep, NodeSample, NodeSource};

    use super::*;

    struct Plain;

    impl NodeSource for Plain {
        fn node(&self, dir: RenderDir) -> NodeSample {
            NodeSample {
                elevation_m: (500.0 * dir.0.x) as f32,
                ice: 0.0,
            }
        }

        fn data_lakes(&self) -> bool {
            false
        }
    }

    fn era(years: f64) -> Arc<EraHydrology> {
        let table = climate::climate_table();
        let zonal = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        let mut prep = HydroPrep::new(CubeLattice { n: 4 }, YearsBp(years));
        while !prep.step(&Plain, &zonal, &table, u32::MAX) {}
        Arc::new(prep.finish())
    }

    #[test]
    fn the_cache_keeps_two_eras() {
        assert_eq!(HYDRO_CACHE_ERAS, 2);
        let mut cache = HydroCache::default();
        let (a, b, c) = (YearsBp(0.0), YearsBp(21.0e3), YearsBp(1.0e6));
        assert_eq!(cache.status(a), HydroStatus::Absent);
        cache.insert(era(a.0));
        cache.insert(era(b.0));
        // `a` is used again, so `b` is the least recently used.
        assert!(cache.get(a).is_some());
        cache.insert(era(c.0));
        assert_eq!(cache.len(), HYDRO_CACHE_ERAS);
        assert!(matches!(cache.status(a), HydroStatus::Ready(h) if h.age == a));
        assert_eq!(cache.status(b), HydroStatus::Absent);
        assert!(matches!(cache.status(c), HydroStatus::Ready(_)));
        // Re-inserting an era replaces it rather than holding it twice.
        cache.insert(era(c.0));
        assert_eq!(cache.len(), 2);
        // Running and failed eras.
        cache.running = Some(RunningPass {
            age: b,
            purpose: HydroPurpose::Prefetch,
        });
        assert_eq!(cache.status(b), HydroStatus::Running);
        cache.running = None;
        cache.mark_failed(b);
        assert_eq!(cache.status(b), HydroStatus::Failed);
        cache.insert(era(b.0));
        assert!(matches!(cache.status(b), HydroStatus::Ready(_)));
        assert!(cache.failed.is_empty());
    }

    fn request(age: u8, region: bool) -> HydroRequested {
        HydroRequested {
            age: YearsBp(f64::from(age) * 1000.0),
            purpose: if region {
                HydroPurpose::Region
            } else {
                HydroPurpose::Prefetch
            },
        }
    }

    /// A request, or the pass in flight finishing (and joining the settled eras).
    #[derive(Clone, Debug)]
    enum Event {
        Request(u8, bool),
        Finish,
    }

    fn event() -> impl Strategy<Value = Event> {
        prop_oneof![
            4 => (0u8..5, any::<bool>()).prop_map(|(a, r)| Event::Request(a, r)),
            1 => Just(Event::Finish),
        ]
    }

    proptest! {
        /// Review Focus 2's rules over any sequence of requests and finished passes (a slider
        /// dragged and keys mashed while regions come and go): a prefetch never touches a
        /// region's pass; a region request for an unsettled era is always the pass in flight
        /// afterwards; the newest prefetch wins over older ones; a settled era never runs.
        #[test]
        fn one_pass_runs_and_a_region_always_gets_its_era(
            events in prop::collection::vec(event(), 1..60)
        ) {
            let mut running: Option<RunningPass> = None;
            let mut settled: Vec<u64> = Vec::new();
            for e in events {
                let Event::Request(age, region) = e else {
                    if let Some(r) = running.take() {
                        settled.push(key(r.age));
                    }
                    continue;
                };
                let req = request(age, region);
                let is_settled = settled.contains(&key(req.age));
                let before = running;
                match admit(running, is_settled, &req) {
                    Admit::Start => running = Some(RunningPass { age: req.age, purpose: req.purpose }),
                    Admit::Upgrade => {
                        running.as_mut().expect("upgrades a pass in flight").purpose =
                            HydroPurpose::Region;
                    }
                    Admit::Ignore => {}
                }
                if req.purpose == HydroPurpose::Prefetch
                    && before.is_some_and(|b| b.purpose == HydroPurpose::Region)
                {
                    prop_assert_eq!(running, before, "a prefetch touched a region's pass");
                }
                if !is_settled {
                    let r = running.expect("an unsettled request leaves a pass in flight");
                    if req.purpose == HydroPurpose::Region {
                        prop_assert_eq!(r, RunningPass { age: req.age, purpose: HydroPurpose::Region });
                    } else if before.is_none_or(|b| b.purpose == HydroPurpose::Prefetch) {
                        prop_assert_eq!(key(r.age), key(req.age), "the newest prefetch wins");
                    }
                } else {
                    prop_assert_eq!(running, before, "a settled era started a pass");
                }
                if let Some(r) = running {
                    prop_assert!(!settled.contains(&key(r.age)));
                }
            }
        }
    }
}
