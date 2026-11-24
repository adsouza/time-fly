//! The region benchmark (`--region-bench`, web `?region-bench`): M2 design §8.
//!
//! Once the globe is interactive (on the web its shaders compile first, so a region never
//! starts under a stalled render probe) and a short settle has passed, it enters each of the
//! [`BENCH_REGIONS`] in turn, as the launch argument does: it sets the globe's time to the
//! region's age and sends [`RegionRequested`]. Navigation fades in; once the player is flying
//! and a settle has passed at the start pose, the bench drives [`FlyState`] along the region's
//! scripted path ([`BenchPath`], [`SCRIPT_SECS`] of flight), then writes [`RegionLeft`] and
//! waits on the globe. A region marked `pan_from_previous` is reached by an 8 s globe pan
//! from the one before ([`pan_globe`]). It then re-enters [`CYCLE_REGION`] [`MEMORY_CYCLES`]
//! times, sampling memory on the globe after each leave.
//!
//! **Blocks.** One per region, in the scrub bench's format: frame statistics under the R14 rule
//! (M1 ruling R14, see [`super::passes`]), generation milliseconds, tiles and bytes fetched,
//! the load window, memory and the bake revision. Then a summary block with the gates:
//!
//! - **Frame rate** (R14), per region.
//! - **First-region worst frame** (ruling R11): no frame from the first request to the first
//!   path frame may exceed [`FIRST_REGION_FRAME_LIMIT_MS`]. That window holds the one-time
//!   tile manifest and overview loads, the first terrain pipelines and the entering fade. The
//!   gate applies in Chrome WebGPU, where the overview decodes on the main thread; natively the
//!   number is reported and does not decide the result.
//! - **Warm generation**, over the memory cycles' re-entries: at most
//!   [`NATIVE_GEN_TARGET_MS`] natively, [`WEB_GEN_TARGET_MS`] on the web.
//! - **First-entry generation** on the web ([`first_entry_gate`]): every region's first entry
//!   (cold tiles, the Aegean's deep coastal generation included) within [`WEB_GEN_TARGET_MS`];
//!   a region over it fails. Natively the numbers are reported only.
//! - **Memory cycles**: after each leave the memory returns within
//!   [`CYCLE_MEMORY_TOLERANCE`] of the first cycle's. Natively the process footprint; on the web
//!   the wasm linear memory (it only grows, so the check is that it stops growing).
//!
//! On native the run exits after the summary: 0 on PASS, [`FAILED_RESULT_EXIT_CODE`] on FAIL
//! (a result, not a crash). The web logs each block to the console as one message.
//!
//! [`FAILED_RESULT_EXIT_CODE`]: crate::diagnostics::session::FAILED_RESULT_EXIT_CODE

pub mod banner;
pub mod path;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::window::{Monitor, PresentMode, PrimaryMonitor, PrimaryWindow};
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use timefly_core::terrain::blended_height_m;

use super::{
    MAX_MISSED_SHARE, MEDIAN_TOLERANCE, MISSED_INTERVALS, Pacing, STALL_BUDGETS, bench_exit,
    frame_budget_ms, frame_time_stats, header_lines_titled, memory, mib, missed_frames, pacing,
    passes, stall_count,
};
use crate::args::RegionArg;
use crate::data::GlobeData;
use crate::diagnostics::SessionInfo;
use crate::fly_camera::{FlyState, FlySystems};
use crate::globe::GlobeRenderStatus;
use crate::globe_flight::{GlobeFlightState, GlobeFlightSystems};
use crate::region::launch::launch_request;
use crate::region::navigate::{NavPhase, Navigation, NavigationSystems};
use crate::region::{
    ActiveRegion, RegionGenerated, RegionLeft, RegionRequested, RegionState, RegionSystems,
};
use crate::terrain::TerrainOptions;
use crate::time::GlobeTime;
pub use path::{BenchPath, Coast, SCRIPT_SECS, Segment};
use timefly_core::flight::altitudes::GLOBE_MAX_ALTITUDE_KM;
use timefly_core::flight::globe::GlobeFlight;

/// A bench region: a name and the launch argument that enters it. With `pan_from_previous`,
/// the bench reaches it from the previous region by a globe pan ([`pan_globe`]) instead of
/// entering it from where the globe was left.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BenchRegion {
    pub name: &'static str,
    pub arg: RegionArg,
    pub pan_from_previous: bool,
}

/// Seconds the globe pan between two bench regions takes (eased at both ends).
pub const PAN_SECS: f64 = 8.0;
/// The fly's altitude during the globe pan: the top of globe flight, whose view (the whole disc
/// under the fly) is the nearest to M1's orbit pan (Task 4 fix round 1).
pub const PAN_ALTITUDE_KM: f64 = GLOBE_MAX_ALTITUDE_KM;

/// The globe's fly `t_s` seconds into a pan from `from` to `to` (lat, lon in degrees) at
/// `altitude_km`: along the great circle, eased in and out over [`PAN_SECS`], heading along it.
pub fn pan_globe(from: (f64, f64), to: (f64, f64), altitude_km: f64, t_s: f64) -> GlobeFlight {
    let dir = |(lat, lon): (f64, f64)| geo_to_render(latlon_to_geo(lat, lon)).0.normalize();
    let (a, b) = (dir(from), dir(to));
    let u = (t_s / PAN_SECS).clamp(0.0, 1.0);
    let s = u * u * (3.0 - 2.0 * u);
    let angle = a.dot(b).clamp(-1.0, 1.0).acos();
    let p = if angle < 1e-12 {
        a
    } else {
        (a * ((1.0 - s) * angle).sin() + b * (s * angle).sin()) / angle.sin()
    };
    let p = p.normalize();
    let (lat, lon) = geo_to_latlon(render_to_geo(RenderDir(p)));
    let start = GlobeFlight::start(lat, lon);
    // Along the arc: the great circle's normal crossed with the position.
    let heading = a
        .cross(b)
        .try_normalize()
        .map_or(start.heading, |n| n.cross(p).normalize());
    GlobeFlight {
        dir: p,
        heading,
        altitude_km,
    }
}

/// The bench regions, in order (M2 design §8; owner decisions, rulings R24–R26):
/// - **Pangaea interior** (era frame, 250 Ma): inland hot desert, no tiles. First, so it holds
///   R11's first-region window (overview load and first pipelines; no tile fetch).
/// - **Red Sea** (0 ka, the Sudanese coast): a present-day coast at sea level 0 and Köppen BWh
///   desert (ruling R30).
/// - **Himalayas** (0 ka, Everest's area): the highest relief; the memory cycles' region
///   ([`CYCLE_REGION`]).
/// - **Sundaland** (21 ka): the exposed tropical shelf and its 21 ka coast.
/// - **Japan** (era frame, 1 Ma): mountains and both coasts, the close-coast case.
/// - **Beringia** (21 ka): land below today's sea level and its 21 ka coast.
/// - a globe pan to the **Rockies ice margin** (21 ka): ice edge and the glacial snowline.
/// - **Panama** (era frame, 3 Ma): the closing isthmus where plate pieces meet.
pub const BENCH_REGIONS: [BenchRegion; 8] = [
    BenchRegion {
        name: "Pangaea interior",
        arg: RegionArg {
            lat_deg: -22.0,
            lon_deg: -18.0,
            years: 250.0e6,
        },
        pan_from_previous: false,
    },
    BenchRegion {
        name: "Red Sea",
        arg: RegionArg {
            lat_deg: 20.0,
            lon_deg: 37.5,
            years: 0.0,
        },
        pan_from_previous: false,
    },
    BenchRegion {
        name: "Himalayas",
        arg: RegionArg {
            lat_deg: 28.0,
            lon_deg: 86.9,
            years: 0.0,
        },
        pan_from_previous: false,
    },
    BenchRegion {
        name: "Sundaland",
        arg: RegionArg {
            lat_deg: 5.0,
            lon_deg: 107.0,
            years: 21_000.0,
        },
        pan_from_previous: false,
    },
    BenchRegion {
        name: "Japan",
        arg: RegionArg {
            lat_deg: 36.0,
            lon_deg: 138.5,
            years: 1.0e6,
        },
        pan_from_previous: false,
    },
    BenchRegion {
        name: "Beringia",
        arg: RegionArg {
            lat_deg: 62.0,
            lon_deg: -175.0,
            years: 21_000.0,
        },
        pan_from_previous: false,
    },
    BenchRegion {
        name: "Rockies ice margin",
        arg: RegionArg {
            lat_deg: 48.5,
            lon_deg: -113.5,
            years: 21_000.0,
        },
        pan_from_previous: true,
    },
    BenchRegion {
        name: "Panama",
        arg: RegionArg {
            lat_deg: 9.0,
            lon_deg: -79.5,
            years: 3.0e6,
        },
        pan_from_previous: false,
    },
];

/// The region the memory cycles and the warm-generation gate re-enter (owner decision, R26):
/// it fetches tiles, so re-entries exercise the tile cache's eviction and refetch.
pub const CYCLE_REGION: &str = "Himalayas";

/// [`CYCLE_REGION`]'s index in `regions`, if it is there (a single-region run has none).
pub fn cycle_region(regions: &[BenchRegion]) -> Option<usize> {
    regions.iter().position(|r| r.name == CYCLE_REGION)
}

/// Bench stops: the regions plus the globe pans before panned regions.
pub fn stop_count(regions: &[BenchRegion]) -> usize {
    regions.len() + regions.iter().filter(|r| r.pan_from_previous).count()
}

/// The name a single-region run gives the region it was asked to bench.
pub const CHOSEN_REGION: &str = "Chosen region";

/// Enter/leave cycles after the regions, re-entering the first region.
pub const MEMORY_CYCLES: u32 = 10;
/// After each leave, memory stays within this share above the first cycle's.
pub const CYCLE_MEMORY_TOLERANCE: f64 = 0.10;
/// Ruling R11: the longest frame allowed while the first region loads.
pub const FIRST_REGION_FRAME_LIMIT_MS: f32 = 250.0;
/// Warm generation targets (spec §15).
pub const NATIVE_GEN_TARGET_MS: u32 = 3_000;
pub const WEB_GEN_TARGET_MS: u32 = 8_000;
/// Seconds on the globe before the first region, and at a region's start pose before its path.
pub const SETTLE_SECS: f64 = 1.0;
/// Seconds on the globe after a leave, before memory is sampled and the next region starts.
pub const LEAVE_SETTLE_SECS: f64 = 1.5;
/// A region that is not flying this long after its request fails.
pub const REGION_TIMEOUT_SECS: f64 = 180.0;

/// The bench's settings. Tests shorten the path and the cycles.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionBenchConfig {
    pub target_fps: u32,
    pub exit_when_done: bool,
    /// Real seconds the [`SCRIPT_SECS`] path takes.
    pub path_secs: f32,
    pub cycles: u32,
    pub timeout_secs: f64,
    /// The regions to fly, in order: [`BENCH_REGIONS`] for the full bench, or one
    /// [`CHOSEN_REGION`] for `--region-bench --region=…`.
    pub regions: Vec<BenchRegion>,
}

impl RegionBenchConfig {
    pub fn new(target_fps: u32, exit_when_done: bool) -> Self {
        RegionBenchConfig {
            target_fps,
            exit_when_done,
            path_secs: SCRIPT_SECS,
            cycles: MEMORY_CYCLES,
            timeout_secs: REGION_TIMEOUT_SECS,
            regions: BENCH_REGIONS.to_vec(),
        }
    }

    /// The bench over one region (`--region-bench --region=…`): its flight only, with no globe
    /// pan and no memory cycles. The region is the first, so R11 applies to it.
    pub fn single(target_fps: u32, exit_when_done: bool, arg: RegionArg) -> Self {
        RegionBenchConfig {
            cycles: 0,
            regions: vec![BenchRegion {
                name: CHOSEN_REGION,
                arg,
                pan_from_previous: false,
            }],
            ..RegionBenchConfig::new(target_fps, exit_when_done)
        }
    }

    /// The region a single-region run benches, if this is one.
    pub fn single_region(&self) -> Option<RegionArg> {
        match self.regions.as_slice() {
            [only] if only.name == CHOSEN_REGION => Some(only.arg),
            _ => None,
        }
    }
}

/// `dunes: on` or `dunes: off` (`--no-dunes`), so a run with the dunes off is never read as one
/// with them on.
pub fn dunes_line(dunes: bool) -> String {
    format!("dunes: {}", if dunes { "on" } else { "off" })
}

/// Puts [`dunes_line`] right after the header's `system spans:` line.
fn push_dunes_line(out: &mut Vec<String>, game: &Game) {
    let dunes = game.options.as_deref().is_none_or(|o| o.dunes);
    let at = out
        .iter()
        .position(|l| l.starts_with("system spans: "))
        .map_or(out.len(), |i| i + 1);
    out.insert(at, dunes_line(dunes));
}

/// The summary's line naming what was benched: `regions: all 8` or
/// `regions: single (LAT, LON at AGE years)`.
pub fn regions_line(config: &RegionBenchConfig) -> String {
    match config.single_region() {
        Some(a) => format!(
            "regions: single ({}, {} at {} years)",
            a.lat_deg, a.lon_deg, a.years
        ),
        None if config.regions.as_slice() == BENCH_REGIONS => {
            format!("regions: all {}", BENCH_REGIONS.len())
        }
        None => format!("regions: {} chosen", config.regions.len()),
    }
}

pub struct RegionBenchPlugin(pub RegionBenchConfig);

impl Plugin for RegionBenchPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(RegionBench::new(self.0.clone()))
            .add_systems(Startup, banner::spawn_banner)
            .add_systems(
                Update,
                (
                    drive_region_bench
                        .after(RegionSystems)
                        .after(NavigationSystems)
                        .after(FlySystems::Step)
                        .before(FlySystems::Apply)
                        .after(GlobeFlightSystems::Step)
                        .before(GlobeFlightSystems::Apply),
                    banner::update_banner.after(drive_region_bench),
                ),
            );
    }
}

/// What follows a leave.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Next {
    Region(usize),
    Cycle(u32),
    Summary,
}

/// Where the run is. Times are `Time<Real>` seconds.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Stage {
    WaitingForGlobe,
    Settling {
        since: f64,
    },
    /// A bench region was requested; waiting until the player flies (the load window).
    Loading {
        region: usize,
        since: f64,
        started: bool,
    },
    /// At the start pose, settling (still the load window).
    Poised {
        region: usize,
        since: f64,
    },
    Flying {
        region: usize,
        since: f64,
    },
    /// Panning the globe from the previous region toward `region` ([`pan_globe`]), then
    /// entering it.
    Panning {
        region: usize,
        since: f64,
        from: (f64, f64),
    },
    /// [`RegionLeft`] was written; waiting for the globe. `cycle` is the cycle that left.
    Leaving {
        next: Next,
        cycle: Option<u32>,
        since: f64,
    },
    Resting {
        next: Next,
        cycle: Option<u32>,
        since: f64,
    },
    CycleLoading {
        cycle: u32,
        since: f64,
        started: bool,
    },
    CyclePoised {
        cycle: u32,
        since: f64,
    },
    Done,
}

/// One bench region's measurements.
#[derive(Clone, Debug, Default)]
struct RegionRun {
    requested_at: f64,
    /// Frames from the request to the first path frame.
    load_ms: Vec<f32>,
    load_secs: Option<f64>,
    generated: Option<RegionGenerated>,
    path: Option<BenchPath>,
    flight_ms: Vec<f32>,
    segment_ms: [Vec<f32>; 5],
    /// The globe pan that led here, if any.
    pan_ms: Vec<f32>,
    memory_before: Option<u64>,
    memory_peak: Option<u64>,
    failure: Option<String>,
    pass: bool,
}

/// One enter/leave cycle.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct CycleRun {
    generation_ms: Option<u32>,
    in_region: Option<u64>,
    on_globe: Option<u64>,
    failed: bool,
}

/// The benchmark's state and measurements.
#[derive(Resource)]
pub struct RegionBench {
    pub config: RegionBenchConfig,
    stage: Stage,
    runs: Vec<RegionRun>,
    cycles: Vec<CycleRun>,
    /// Web: the JS heap's peak over the run, where the browser reports it.
    js_heap_peak: Option<u64>,
    reports: Vec<String>,
    summary: Option<String>,
    /// The summary's result, once it is out.
    passed: Option<bool>,
    /// The path segment being flown (for the banner).
    segment: Segment,
}

impl RegionBench {
    pub fn new(config: RegionBenchConfig) -> Self {
        RegionBench {
            runs: vec![RegionRun::default(); config.regions.len()],
            config,
            stage: Stage::WaitingForGlobe,
            cycles: Vec::new(),
            js_heap_peak: None,
            reports: Vec::new(),
            summary: None,
            passed: None,
            segment: Segment::LowPass,
        }
    }

    /// The regions this run flies.
    pub fn regions(&self) -> &[BenchRegion] {
        &self.config.regions
    }

    /// Stops in this run: its regions plus the globe pans before panned regions.
    pub fn stop_count(&self) -> usize {
        stop_count(&self.config.regions)
    }

    /// The memory cycles' region in this run, if it has one.
    pub fn cycle_region(&self) -> Option<usize> {
        cycle_region(&self.config.regions)
    }

    /// The per-region blocks logged so far.
    pub fn reports(&self) -> &[String] {
        &self.reports
    }

    /// The summary block, once the run has ended.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }
}

/// The memory the cycles compare: the process footprint natively, the wasm linear memory on the
/// web.
fn cycle_memory() -> Option<u64> {
    #[cfg(target_arch = "wasm32")]
    return memory::web_memory().1;
    #[cfg(not(target_arch = "wasm32"))]
    memory::footprint_now()
}

/// What [`cycle_memory`] measures.
fn cycle_memory_kind() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "wasm linear memory, which grows but never shrinks"
    } else {
        memory::FOOTPRINT_KIND
    }
}

/// Logs a block as one message (the browser console shows it whole) and prints it on native.
fn emit(report: &str) {
    info!("{report}");
    #[cfg(not(target_arch = "wasm32"))]
    println!("{report}");
}

/// Whether the cycles' memory came back: the largest after-leave sample of cycles 2.. against
/// the first cycle's, as `(pass, increase)`; `None` when there are fewer than two samples.
pub fn memory_cycles_pass(on_globe: &[Option<u64>]) -> Option<(bool, f64)> {
    let first = (*on_globe.first()?)? as f64;
    let rest: Vec<u64> = on_globe.iter().skip(1).copied().collect::<Option<_>>()?;
    let worst = *rest.iter().max()? as f64;
    let increase = (worst - first) / first.max(1.0);
    Some((increase <= CYCLE_MEMORY_TOLERANCE, increase))
}

/// The display the run is measured on.
#[derive(SystemParam)]
struct Screen<'w, 's> {
    adapter: Option<Res<'w, RenderAdapterInfo>>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    monitors: Query<'w, 's, &'static Monitor, With<PrimaryMonitor>>,
}

impl Screen<'_, '_> {
    fn info(&self) -> DisplayInfo {
        let window = self.windows.single().ok();
        DisplayInfo {
            size: window.map(|w| UVec2::new(w.physical_width(), w.physical_height())),
            present_mode: window.map(|w| w.present_mode),
            refresh_hz: self
                .monitors
                .iter()
                .next()
                .and_then(|m| m.refresh_rate_millihertz)
                .map(|mhz| mhz as f32 / 1000.0),
        }
    }
}

struct DisplayInfo {
    size: Option<UVec2>,
    present_mode: Option<PresentMode>,
    refresh_hz: Option<f32>,
}

/// What the bench reads about the game.
#[derive(SystemParam)]
struct Game<'w, 's> {
    real: Res<'w, Time<Real>>,
    status: Option<Res<'w, GlobeRenderStatus>>,
    region_state: Option<Res<'w, State<RegionState>>>,
    nav: Option<Res<'w, Navigation>>,
    region: Option<Res<'w, ActiveRegion>>,
    session: Option<Res<'w, SessionInfo>>,
    globe: Option<Res<'w, GlobeData>>,
    options: Option<Res<'w, TerrainOptions>>,
    generated: MessageReader<'w, 's, RegionGenerated>,
}

impl Game<'_, '_> {
    fn state(&self) -> RegionState {
        self.region_state
            .as_ref()
            .map_or(RegionState::Globe, |s| *s.get())
    }

    /// In a region and flying (or, without navigation, in a region).
    fn flying(&self) -> bool {
        self.state() == RegionState::InRegion
            && self
                .nav
                .as_ref()
                .is_none_or(|n| n.phase == NavPhase::Flying)
    }

    /// Back on the globe, navigation included.
    fn on_globe(&self) -> bool {
        self.state() == RegionState::Globe
            && self.nav.as_ref().is_none_or(|n| n.phase == NavPhase::Globe)
    }
}

/// What the bench changes.
#[derive(SystemParam)]
struct Controls<'w> {
    time: Option<ResMut<'w, GlobeTime>>,
    fly: Option<ResMut<'w, FlyState>>,
    flight: Option<ResMut<'w, GlobeFlightState>>,
    requests: MessageWriter<'w, RegionRequested>,
    leave: MessageWriter<'w, RegionLeft>,
    exit: MessageWriter<'w, AppExit>,
}

impl Controls<'_> {
    fn enter(&mut self, region: &BenchRegion) {
        let request = launch_request(&region.arg);
        if let Some(time) = self.time.as_mut() {
            time.years = YearsBp(region.arg.years);
        }
        info!(
            region = region.name,
            lat_deg = region.arg.lat_deg,
            lon_deg = region.arg.lon_deg,
            years = region.arg.years,
            "region bench: entering"
        );
        self.requests.write(request);
    }
}

fn drive_region_bench(
    mut bench: ResMut<RegionBench>,
    mut game: Game,
    mut controls: Controls,
    display: Screen,
) {
    let now = game.real.elapsed_secs_f64();
    let frame_ms = game.real.delta_secs() * 1000.0;
    let generated: Vec<RegionGenerated> = game.generated.read().copied().collect();
    let bench = &mut *bench;
    #[cfg(target_arch = "wasm32")]
    {
        bench.js_heap_peak = super::max_opt(bench.js_heap_peak, memory::web_memory().0);
    }
    if bench.stage != Stage::Done
        && let Some(GlobeRenderStatus::Failed(reason)) = game.status.as_deref()
    {
        let reason = format!("the globe failed: {reason}");
        finish(bench, &game, &display, &mut controls, Some(&reason));
        return;
    }
    let timeout = bench.config.timeout_secs;
    match bench.stage {
        Stage::WaitingForGlobe => {
            if matches!(
                game.status.as_deref(),
                Some(GlobeRenderStatus::Interactive { .. })
            ) {
                info!("region bench: globe interactive, settling");
                bench.stage = Stage::Settling { since: now };
            }
        }
        Stage::Settling { since } => {
            if now - since >= SETTLE_SECS {
                start_region(bench, 0, now, &mut controls);
            }
        }
        Stage::Loading {
            region,
            since,
            started,
        } => {
            let run = &mut bench.runs[region];
            run.load_ms.push(frame_ms);
            run.memory_peak = super::max_opt(run.memory_peak, cycle_memory());
            if let Some(g) = generated.last() {
                run.generated = Some(*g);
            }
            let started = started || game.state() != RegionState::Globe;
            if game.flying()
                && let Some(active) = game.region.as_deref()
            {
                run.path = Some(BenchPath::for_grid(&active.grid));
                bench.stage = Stage::Poised { region, since: now };
            } else if started && game.state() == RegionState::Globe {
                fail_region(
                    bench,
                    region,
                    "the region did not load (see the log)",
                    now,
                    &game,
                    &display,
                    &mut controls,
                );
            } else if now - since > timeout {
                fail_region(
                    bench,
                    region,
                    "the region was not ready in time",
                    now,
                    &game,
                    &display,
                    &mut controls,
                );
            } else {
                bench.stage = Stage::Loading {
                    region,
                    since,
                    started,
                };
            }
        }
        Stage::Poised { region, since } => {
            let run = &mut bench.runs[region];
            run.load_ms.push(frame_ms);
            run.memory_peak = super::max_opt(run.memory_peak, cycle_memory());
            if !game.flying() {
                fail_region(
                    bench,
                    region,
                    "the region was left before its path",
                    now,
                    &game,
                    &display,
                    &mut controls,
                );
            } else if now - since >= SETTLE_SECS {
                run.load_secs = Some(now - run.requested_at);
                info!(
                    region = bench.config.regions[region].name,
                    "region bench: flying the path"
                );
                bench.stage = Stage::Flying { region, since: now };
                fly(bench, region, 0.0, &game, &mut controls);
            }
        }
        Stage::Flying { region, since } => {
            let script = ((now - since) as f32) * SCRIPT_SECS / bench.config.path_secs.max(1e-3);
            bench.segment = banner::segment_at(script);
            let run = &mut bench.runs[region];
            run.flight_ms.push(frame_ms);
            let segment = Segment::ALL
                .iter()
                .position(|s| *s == Segment::at(script.min(SCRIPT_SECS)))
                .unwrap_or(0);
            run.segment_ms[segment].push(frame_ms);
            run.memory_peak = super::max_opt(run.memory_peak, cycle_memory());
            if !game.flying() {
                fail_region(
                    bench,
                    region,
                    "the region was left during its path",
                    now,
                    &game,
                    &display,
                    &mut controls,
                );
            } else if script >= SCRIPT_SECS {
                let block = region_block(bench, region, &game, &display);
                emit(&block);
                bench.reports.push(block);
                controls.leave.write(RegionLeft);
                bench.stage = Stage::Leaving {
                    next: after_region(region, bench.config.regions.len(), bench.config.cycles),
                    cycle: None,
                    since: now,
                };
            } else {
                fly(bench, region, script, &game, &mut controls);
            }
        }
        Stage::Leaving { next, cycle, since } => {
            if game.on_globe() {
                bench.stage = Stage::Resting {
                    next,
                    cycle,
                    since: now,
                };
            } else if now - since > timeout {
                let reason = "the region did not leave in time";
                finish(bench, &game, &display, &mut controls, Some(reason));
            }
        }
        Stage::Resting { next, cycle, since } => {
            if now - since < LEAVE_SETTLE_SECS {
                return;
            }
            if let Some(c) = cycle
                && let Some(record) = bench.cycles.get_mut(c as usize)
            {
                record.on_globe = cycle_memory();
                info!(cycle = c + 1, on_globe = ?record.on_globe, "region bench: cycle left");
            }
            match next {
                Next::Region(region) if bench.config.regions[region].pan_from_previous => {
                    let prev = bench.config.regions[region - 1].arg;
                    info!(
                        from = bench.config.regions[region - 1].name,
                        to = bench.config.regions[region].name,
                        "region bench: panning the globe"
                    );
                    bench.stage = Stage::Panning {
                        region,
                        since: now,
                        from: (prev.lat_deg, prev.lon_deg),
                    };
                }
                Next::Region(region) => start_region(bench, region, now, &mut controls),
                Next::Cycle(c) => {
                    bench.cycles.push(CycleRun::default());
                    let cycled = bench.config.regions[bench.cycle_region().unwrap_or(0)];
                    controls.enter(&cycled);
                    bench.stage = Stage::CycleLoading {
                        cycle: c,
                        since: now,
                        started: false,
                    };
                }
                Next::Summary => finish(bench, &game, &display, &mut controls, None),
            }
        }
        Stage::Panning {
            region,
            since,
            from,
        } => {
            bench.runs[region].pan_ms.push(frame_ms);
            let to = (
                bench.config.regions[region].arg.lat_deg,
                bench.config.regions[region].arg.lon_deg,
            );
            let t = now - since;
            if let Some(flight) = controls.flight.as_mut() {
                let next = pan_globe(from, to, PAN_ALTITUDE_KM, t);
                if flight.0 != next {
                    flight.0 = next;
                }
            }
            if t >= PAN_SECS {
                start_region(bench, region, now, &mut controls);
            }
        }
        Stage::CycleLoading {
            cycle,
            since,
            started,
        } => {
            if let Some(g) = generated.last()
                && let Some(record) = bench.cycles.get_mut(cycle as usize)
            {
                record.generation_ms = Some(g.millis);
            }
            let started = started || game.state() != RegionState::Globe;
            let failed = (started && game.state() == RegionState::Globe) || now - since > timeout;
            if game.flying() {
                bench.stage = Stage::CyclePoised { cycle, since: now };
            } else if failed {
                if let Some(record) = bench.cycles.get_mut(cycle as usize) {
                    record.failed = true;
                }
                let reason = "a memory cycle's region did not load";
                finish(bench, &game, &display, &mut controls, Some(reason));
            } else {
                bench.stage = Stage::CycleLoading {
                    cycle,
                    since,
                    started,
                };
            }
        }
        Stage::CyclePoised { cycle, since } => {
            if now - since >= SETTLE_SECS {
                if let Some(record) = bench.cycles.get_mut(cycle as usize) {
                    record.in_region = cycle_memory();
                }
                controls.leave.write(RegionLeft);
                let next = if cycle + 1 < bench.config.cycles {
                    Next::Cycle(cycle + 1)
                } else {
                    Next::Summary
                };
                bench.stage = Stage::Leaving {
                    next,
                    cycle: Some(cycle),
                    since: now,
                };
            }
        }
        Stage::Done => {}
    }
}

/// What follows region `region`.
fn after_region(region: usize, regions: usize, cycles: u32) -> Next {
    if region + 1 < regions {
        Next::Region(region + 1)
    } else if cycles > 0 {
        Next::Cycle(0)
    } else {
        Next::Summary
    }
}

fn start_region(bench: &mut RegionBench, region: usize, now: f64, controls: &mut Controls) {
    let spec = bench.config.regions[region];
    let run = &mut bench.runs[region];
    run.requested_at = now;
    run.memory_before = cycle_memory();
    controls.enter(&spec);
    bench.stage = Stage::Loading {
        region,
        since: now,
        started: false,
    };
}

/// Puts the camera at the path's pose at `script` seconds.
fn fly(bench: &RegionBench, region: usize, script: f32, game: &Game, controls: &mut Controls) {
    let (Some(path), Some(active), Some(fly)) = (
        bench.runs[region].path.as_ref(),
        game.region.as_deref(),
        controls.fly.as_mut(),
    ) else {
        return;
    };
    let grid = &active.grid;
    **fly = path.at(script, |x, z| {
        blended_height_m(grid, f64::from(x), f64::from(z))
    });
}

/// Ends a region with a FAIL block and moves on (leaving first if a region is up).
fn fail_region(
    bench: &mut RegionBench,
    region: usize,
    reason: &str,
    now: f64,
    game: &Game,
    display: &Screen,
    controls: &mut Controls,
) {
    warn!(
        region = bench.config.regions[region].name,
        reason, "region bench: region failed"
    );
    bench.runs[region].failure = Some(reason.to_string());
    let block = region_block(bench, region, game, display);
    emit(&block);
    bench.reports.push(block);
    if game.state() != RegionState::Globe {
        controls.leave.write(RegionLeft);
    }
    bench.stage = Stage::Leaving {
        next: after_region(region, bench.config.regions.len(), bench.config.cycles),
        cycle: None,
        since: now,
    };
}

fn secs(v: Option<f64>) -> String {
    v.map_or("unknown".to_string(), |s| format!("{s:.2} s"))
}

fn show_mib(v: Option<u64>) -> String {
    v.map_or("n/a".to_string(), mib)
}

fn stat_line(name: &str, frames: &[f32]) -> String {
    let s = frame_time_stats(frames);
    format!(
        "{name}: frames {}, median {:.2} ms, worst {:.2} ms",
        s.frames, s.median_ms, s.worst_ms
    )
}

/// The pacing in effect for a series of frames on this display.
fn pacing_for(display: &DisplayInfo, frames: &[f32]) -> Pacing {
    let uncapped_requested = matches!(
        display.present_mode,
        Some(PresentMode::AutoNoVsync | PresentMode::Immediate | PresentMode::Mailbox)
    );
    pacing(
        cfg!(target_arch = "wasm32"),
        uncapped_requested,
        display.refresh_hz,
        frame_time_stats(frames).median_ms,
    )
}

fn pacing_name(pacing: Pacing) -> &'static str {
    match pacing {
        Pacing::DisplayPaced => "display-paced",
        Pacing::Uncapped => "uncapped",
    }
}

/// A frame longer than this is reported as a host stall: the page was paused or throttled (a
/// browser pane can stop `requestAnimationFrame` for tens of seconds), not a game hitch.
/// Reporting only: no pass rule reads it.
pub const HOST_STALL_MS: f32 = 1_000.0;

/// The host stalls among `frames` (ms): their count and the longest (0 without any).
pub fn host_stalls(frames: &[f32]) -> (usize, f32) {
    frames
        .iter()
        .filter(|&&ms| ms > HOST_STALL_MS)
        .fold((0, 0.0), |(n, longest), &ms| (n + 1, f32::max(longest, ms)))
}

fn host_stall_line(window: &str, frames: &[f32]) -> String {
    let (count, longest) = host_stalls(frames);
    format!(
        "  host stalls in the {window} (frames > {HOST_STALL_MS:.0} ms; page paused or throttled?): {count}{}",
        if count > 0 {
            format!(", longest {longest:.0} ms")
        } else {
            String::new()
        }
    )
}

/// A region's first-entry generation (`millis`) against the web's [`WEB_GEN_TARGET_MS`]: `None`
/// natively (reported only), else whether it is within the target (no region generated fails).
pub fn first_entry_gate(millis: Option<u32>, web: bool) -> Option<bool> {
    web.then(|| millis.is_some_and(|m| m <= WEB_GEN_TARGET_MS))
}

fn gate_word(pass: bool) -> &'static str {
    if pass { "PASS" } else { "FAIL" }
}

/// The first-region worst frame against the R11 limit.
fn first_region_gate(bench: &RegionBench) -> Option<(f32, bool)> {
    let first = &bench.runs[0];
    (!first.load_ms.is_empty()).then(|| {
        let worst = frame_time_stats(&first.load_ms).worst_ms;
        (worst, worst <= FIRST_REGION_FRAME_LIMIT_MS)
    })
}

fn r11_line(bench: &RegionBench) -> String {
    let scope = if cfg!(target_arch = "wasm32") {
        "decides the result"
    } else {
        "native: reported only, the gate applies in Chrome WebGPU"
    };
    match first_region_gate(bench) {
        Some((worst, ok)) => format!(
            "first-region worst frame (R11 gate <= {FIRST_REGION_FRAME_LIMIT_MS:.0} ms, request to first path frame, overview decode included): {worst:.2} ms -> {} ({scope})",
            if ok { "PASS" } else { "FAIL" }
        ),
        None => format!("first-region worst frame (R11): not measured ({scope})"),
    }
}

/// The cycle region's load-window worst frame (its tiles fetched cold), reported only beside
/// the R11 gate on the first region.
fn cycle_region_load_line(bench: &RegionBench) -> String {
    let Some(cycled) = bench.cycle_region() else {
        return format!("{CYCLE_REGION} load-window worst frame (cold tiles): not in this run");
    };
    let run = &bench.runs[cycled];
    if run.load_ms.is_empty() {
        return format!("{CYCLE_REGION} load-window worst frame (cold tiles): not measured");
    }
    format!(
        "{CYCLE_REGION} load-window worst frame (cold tiles; reported only): {:.2} ms",
        frame_time_stats(&run.load_ms).worst_ms
    )
}

/// One region's block; also records whether it passed.
fn region_block(bench: &mut RegionBench, region: usize, game: &Game, display: &Screen) -> String {
    let fps = bench.config.target_fps;
    let spec = bench.config.regions[region];
    let info = display.info();
    let r = &bench.runs[region];
    let title = format!(
        "===== Time Fly region bench: {} ({}, {} at {} years) =====",
        spec.name, spec.arg.lat_deg, spec.arg.lon_deg, spec.arg.years
    );
    let mut out = header_lines_titled(&title, game.session.as_deref(), display.adapter.as_deref());
    push_dunes_line(&mut out, game);
    if let Some(size) = info.size {
        out.push(format!("render resolution: {}x{}", size.x, size.y));
    }
    if let Some(globe) = game.globe.as_deref() {
        out.push(format!("bake: {}", globe.revision.summary()));
    }
    match &r.generated {
        Some(g) => out.push(format!(
            "generation: {} ms; tiles fetched {} ({} bytes); frames fetched {}; fallback {}",
            g.millis,
            g.tiles_fetched,
            g.bytes_fetched,
            g.frames_fetched,
            if g.fallback {
                "yes (a tile is missing)"
            } else {
                "no"
            }
        )),
        None => out.push("generation: no region generated".to_string()),
    }
    let web = cfg!(target_arch = "wasm32");
    let millis = r.generated.map(|g| g.millis);
    let gen_gate = first_entry_gate(millis, web);
    out.push(match gen_gate {
        Some(ok) => format!(
            "first-entry generation (web gate <= {WEB_GEN_TARGET_MS} ms): {} -> {}",
            millis.map_or("none".to_string(), |m| format!("{m} ms")),
            gate_word(ok)
        ),
        None => format!(
            "first-entry generation: reported only natively (the web gates it at <= {WEB_GEN_TARGET_MS} ms)"
        ),
    });
    let load = frame_time_stats(&r.load_ms);
    out.push(format!(
        "load (request to first path frame, {SETTLE_SECS:.0} s settle included): {}, frames {}, worst {:.2} ms",
        secs(r.load_secs),
        load.frames,
        load.worst_ms
    ));
    out.push(host_stall_line("load window", &r.load_ms));
    if region == 0 {
        out.push(r11_line(bench));
    }
    if bench.cycle_region() == Some(region) && region != 0 {
        out.push(cycle_region_load_line(bench));
    }
    if let Some(path) = &r.path {
        out.push(format!("coast: {}", path.coast.describe()));
        if let (Some(active), Coast::InField { .. }) = (game.region.as_deref(), path.coast) {
            let grid = &active.grid;
            let ground = |x: f32, z: f32| blended_height_m(grid, f64::from(x), f64::from(z));
            out.push(match path::coast_run_distance(path, ground) {
                Some(m) => format!(
                    "  the run stays within {:.1} km of the coastline",
                    m / 1000.0
                ),
                None => "  the run strays over 20 km from the coastline".to_string(),
            });
        }
    }
    out.push(format!(
        "memory ({}): before entering {}, peak in region {}, lifetime peak {}",
        cycle_memory_kind(),
        show_mib(r.memory_before),
        show_mib(r.memory_peak),
        show_mib(memory::footprint_peak()),
    ));
    if cfg!(target_arch = "wasm32") {
        out.push(format!(
            "JS heap (performance.memory, Chrome only): peak so far {}",
            show_mib(bench.js_heap_peak)
        ));
    }
    let pan_pass = if r.pan_ms.is_empty() {
        true
    } else {
        let pan_pacing = pacing_for(&info, &r.pan_ms);
        let p = frame_time_stats(&r.pan_ms);
        let ok = passes(&r.pan_ms, fps, pan_pacing);
        out.push(format!(
            "globe pan from {} ({PAN_SECS:.0} s, great circle): frames {}, median {:.2} ms, p95 {:.2} ms, worst {:.2} ms -> {} ({} rule)",
            bench.config.regions[region.saturating_sub(1)].name,
            p.frames,
            p.median_ms,
            p.p95_ms,
            p.worst_ms,
            if ok { "PASS" } else { "FAIL" },
            pacing_name(pan_pacing)
        ));
        out.push(host_stall_line("pan", &r.pan_ms));
        ok
    };
    let pacing = pacing_for(&info, &r.flight_ms);
    let interval = frame_budget_ms(fps);
    out.push(format!(
        "present mode: {} requested{}; display refresh {}; median {:.2} ms -> {}",
        info.present_mode
            .map_or("unknown".to_string(), |m| format!("{m:?}")),
        if cfg!(target_arch = "wasm32") {
            " (browser requestAnimationFrame)"
        } else {
            ""
        },
        info.refresh_hz
            .map_or("unknown".to_string(), |hz| format!("{hz:.1} Hz")),
        frame_time_stats(&r.flight_ms).median_ms,
        pacing_name(pacing)
    ));
    out.push(format!("target: {fps} fps (interval {interval:.2} ms)"));
    out.push(match pacing {
        Pacing::DisplayPaced => format!(
            "pass rule (display-paced): missed frames (>{:.1} ms) <= {:.0}% and median <= {:.2} ms (interval +{:.0}%)",
            MISSED_INTERVALS * interval,
            MAX_MISSED_SHARE * 100.0,
            (1.0 + MEDIAN_TOLERANCE) * interval,
            MEDIAN_TOLERANCE * 100.0
        ),
        Pacing::Uncapped => format!("pass rule (uncapped): p95 <= {interval:.2} ms"),
    });
    let s = frame_time_stats(&r.flight_ms);
    let flight_pass = passes(&r.flight_ms, fps, pacing);
    let missed = missed_frames(&r.flight_ms, fps);
    let share = if s.frames > 0 {
        100.0 * missed as f64 / s.frames as f64
    } else {
        0.0
    };
    out.push(format!(
        "flight ({SCRIPT_SECS:.0} s path): frames {}, median {:.2} ms, p95 {:.2} ms, worst {:.2} ms, stalls (>{:.1} ms = {STALL_BUDGETS}x interval) {} -> {}",
        s.frames,
        s.median_ms,
        s.p95_ms,
        s.worst_ms,
        STALL_BUDGETS * interval,
        stall_count(&r.flight_ms, fps),
        if flight_pass { "PASS" } else { "FAIL" }
    ));
    out.push(format!(
        "  missed frames (>{:.1} ms = {MISSED_INTERVALS}x interval): {missed} ({share:.2}%)",
        MISSED_INTERVALS * interval,
    ));
    out.push(host_stall_line("flight", &r.flight_ms));
    for (segment, frames) in Segment::ALL.iter().zip(&r.segment_ms) {
        out.push(format!("  {}", stat_line(segment.label(), frames)));
    }
    let pass = r.failure.is_none() && flight_pass && pan_pass && gen_gate.unwrap_or(true);
    out.push(match &r.failure {
        Some(reason) => format!("result: FAIL ({reason})"),
        None => format!(
            "result: {} (decided by the {} rule{})",
            if pass { "PASS" } else { "FAIL" },
            match pacing {
                Pacing::DisplayPaced => "display-paced missed-frame",
                Pacing::Uncapped => "uncapped p95",
            },
            if gen_gate.is_some() {
                " and the first-entry generation gate"
            } else {
                ""
            }
        ),
    });
    out.push("===== end region bench =====".to_string());
    bench.runs[region].pass = pass;
    out.join("\n")
}

/// The summary block, then the exit.
fn finish(
    bench: &mut RegionBench,
    game: &Game,
    display: &Screen,
    controls: &mut Controls,
    failure: Option<&str>,
) {
    bench.stage = Stage::Done;
    let web = cfg!(target_arch = "wasm32");
    let mut out = header_lines_titled(
        "===== Time Fly region bench summary =====",
        game.session.as_deref(),
        display.adapter.as_deref(),
    );
    push_dunes_line(&mut out, game);
    out.push(regions_line(&bench.config));
    let mut pass = failure.is_none();
    let regions: Vec<String> = bench
        .config
        .regions
        .iter()
        .zip(&bench.runs)
        .map(|(spec, run)| {
            let measured = run.failure.is_some() || !run.flight_ms.is_empty();
            format!(
                "{} {}",
                spec.name,
                match (measured, run.pass) {
                    (false, _) => "not run",
                    (true, true) => "PASS",
                    (true, false) => "FAIL",
                }
            )
        })
        .collect();
    pass &= bench.runs.iter().all(|r| r.pass);
    out.push(format!("regions (R14 frame rate): {}", regions.join(", ")));
    out.push(r11_line(bench));
    if bench.cycle_region().is_some() {
        out.push(cycle_region_load_line(bench));
    }
    if web {
        pass &= first_region_gate(bench).is_some_and(|(_, ok)| ok);
    }

    let target = if web {
        WEB_GEN_TARGET_MS
    } else {
        NATIVE_GEN_TARGET_MS
    };
    let warm: Vec<u32> = bench
        .cycles
        .iter()
        .filter_map(|c| c.generation_ms)
        .collect();
    let skipped = if bench.config.single_region().is_some() {
        " (single-region run)"
    } else {
        ""
    };
    if warm.is_empty() {
        out.push(format!(
            "warm generation (target <= {target} ms): not measured{skipped}"
        ));
    } else {
        let mut sorted = warm.clone();
        sorted.sort_unstable();
        let max = *sorted.last().unwrap_or(&0);
        let ok = max <= target;
        pass &= ok;
        out.push(format!(
            "warm generation ({} re-entries of {}, target <= {target} ms): median {} ms, max {} ms -> {}",
            warm.len(),
            bench.config.regions[bench.cycle_region().unwrap_or(0)].name,
            sorted[(sorted.len() - 1) / 2],
            max,
            if ok { "PASS" } else { "FAIL" }
        ));
    }
    let mut cold_pass = true;
    let cold: Vec<String> = bench
        .config
        .regions
        .iter()
        .zip(&bench.runs)
        .filter(|(_, run)| run.generated.is_some() || run.failure.is_some())
        .map(|(spec, run)| {
            let millis = run.generated.map(|g| g.millis);
            let shown = millis.map_or("none".to_string(), |m| format!("{m} ms"));
            match first_entry_gate(millis, web) {
                Some(ok) => {
                    cold_pass &= ok;
                    format!("{} {shown} {}", spec.name, gate_word(ok))
                }
                None => format!("{} {shown}", spec.name),
            }
        })
        .collect();
    if web {
        pass &= cold_pass;
        out.push(format!(
            "first entries (cold tiles, web gate <= {WEB_GEN_TARGET_MS} ms each): {} -> {}",
            cold.join(", "),
            gate_word(cold_pass)
        ));
    } else {
        out.push(format!(
            "first entries (cold tiles; reported only natively): {}",
            cold.join(", ")
        ));
    }

    let on_globe: Vec<Option<u64>> = bench.cycles.iter().map(|c| c.on_globe).collect();
    let listed = |pick: fn(&CycleRun) -> Option<u64>| {
        bench
            .cycles
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}: {}", i + 1, show_mib(pick(c))))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match memory_cycles_pass(&on_globe) {
        Some((ok, increase)) => {
            pass &= ok;
            out.push(format!(
                "memory cycles ({}, on the globe after each leave): {}; worst of cycles 2-{} is {:+.1}% of cycle 1 (limit +{:.0}%) -> {}",
                cycle_memory_kind(),
                listed(|c| c.on_globe),
                bench.cycles.len(),
                increase * 100.0,
                CYCLE_MEMORY_TOLERANCE * 100.0,
                if ok { "PASS" } else { "FAIL" }
            ));
            out.push(format!("  in the region: {}", listed(|c| c.in_region)));
        }
        None if !skipped.is_empty() => {
            out.push(format!("memory cycles: not measured{skipped}"));
        }
        None => out.push(format!(
            "memory cycles: not measured ({} cycles, memory {})",
            bench.cycles.len(),
            if cycle_memory().is_some() {
                "available"
            } else {
                "not reported here"
            }
        )),
    }
    if bench.cycles.iter().any(|c| c.failed) {
        pass = false;
    }
    out.push(match failure {
        Some(reason) => format!("result: FAIL ({reason})"),
        None => format!("result: {}", if pass { "PASS" } else { "FAIL" }),
    });
    out.push("===== end region bench summary =====".to_string());
    let summary = out.join("\n");
    emit(&summary);
    bench.summary = Some(summary);
    bench.passed = Some(pass && failure.is_none());
    if bench.config.exit_when_done {
        controls.exit.write(bench_exit(pass));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_cli_flags() {
        let parsed = crate::args::parse_launch_args(
            [
                "--region-bench",
                "--target-fps",
                "60",
                "--window",
                "2560x1440",
            ]
            .map(String::from),
        );
        assert!(parsed.region_bench);
        assert!(!parsed.scrub_bench);
        assert_eq!(parsed.target_fps, Some(60));
        assert_eq!(parsed.window, Some((2560, 1440)));
        assert!(parsed.unknown.is_empty(), "{:?}", parsed.unknown);
        let web = crate::args::parse_query_args("?region-bench&target-fps=30");
        assert!(web.region_bench);
        assert_eq!(web.target_fps, Some(30));
        assert!(web.unknown.is_empty(), "{:?}", web.unknown);
        // Not a prefix match.
        let other = crate::args::parse_launch_args(["--region-benchmark".to_string()]);
        assert!(!other.region_bench);
        assert_eq!(other.unknown, vec!["--region-benchmark".to_string()]);
        // The region bench turns vsync off on native, as the scrub bench does.
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(
            crate::primary_window(&parsed).present_mode,
            PresentMode::AutoNoVsync
        );
    }

    #[test]
    fn memory_cycles_return_within_ten_percent() {
        let mib = |m: u64| Some(m << 20);
        let ok = [mib(800), mib(830), mib(860), mib(870)];
        let (pass, increase) = memory_cycles_pass(&ok).unwrap();
        assert!(pass);
        assert!((increase - 70.0 / 800.0).abs() < 1e-9);
        // Dropping below the first is fine; growing past +10% is not.
        assert!(memory_cycles_pass(&[mib(800), mib(700)]).unwrap().0);
        assert!(
            !memory_cycles_pass(&[mib(800), mib(830), mib(881)])
                .unwrap()
                .0
        );
        assert!(
            memory_cycles_pass(&[mib(800), mib(880)]).unwrap().0,
            "exactly +10%"
        );
        // Too few or missing samples are not judged.
        assert_eq!(memory_cycles_pass(&[mib(800)]), None);
        assert_eq!(memory_cycles_pass(&[]), None);
        assert_eq!(memory_cycles_pass(&[mib(800), None]), None);
        assert_eq!(memory_cycles_pass(&[None, mib(800)]), None);
    }

    #[test]
    fn host_stalls_are_frames_over_a_second() {
        assert_eq!(host_stalls(&[]), (0, 0.0));
        assert_eq!(host_stalls(&[16.7, 999.0, 250.0]), (0, 0.0));
        assert_eq!(host_stalls(&[16.7, 1_000.5, 24_000.0, 33.0]), (2, 24_000.0));
        let line = host_stall_line("flight", &[16.7, 45_000.0]);
        assert!(
            line.contains("page paused or throttled?): 1, longest 45000 ms"),
            "{line}"
        );
    }

    #[test]
    fn first_entries_are_gated_on_the_web_only() {
        assert_eq!(first_entry_gate(Some(683), true), Some(true));
        assert_eq!(first_entry_gate(Some(WEB_GEN_TARGET_MS), true), Some(true));
        assert_eq!(
            first_entry_gate(Some(WEB_GEN_TARGET_MS + 1), true),
            Some(false)
        );
        assert_eq!(first_entry_gate(None, true), Some(false));
        assert_eq!(first_entry_gate(Some(9_000), false), None);
        assert_eq!(first_entry_gate(None, false), None);
    }

    #[test]
    fn the_bench_regions_are_pinned() {
        let names: Vec<_> = BENCH_REGIONS.iter().map(|r| r.name).collect();
        assert_eq!(
            names,
            [
                "Pangaea interior",
                "Red Sea",
                "Himalayas",
                "Sundaland",
                "Japan",
                "Beringia",
                "Rockies ice margin",
                "Panama"
            ]
        );
        let arg = |lat_deg, lon_deg, years| RegionArg {
            lat_deg,
            lon_deg,
            years,
        };
        let args: Vec<_> = BENCH_REGIONS.iter().map(|r| r.arg).collect();
        assert_eq!(
            args,
            [
                arg(-22.0, -18.0, 250.0e6),
                // Ruling R30: a present-day coast at sea level 0 and BWh desert.
                arg(20.0, 37.5, 0.0),
                // Ruling R24: Everest (27.99, 86.93) is in the inner field.
                arg(28.0, 86.9, 0.0),
                // Ruling R26: 21 ka anomaly land and coast on the tropical shelf.
                arg(5.0, 107.0, 21_000.0),
                arg(36.0, 138.5, 1.0e6),
                // Ruling R25: 21 ka anomaly land and its coast; the ice margin under mountains.
                arg(62.0, -175.0, 21_000.0),
                arg(48.5, -113.5, 21_000.0),
                arg(9.0, -79.5, 3.0e6),
            ]
        );
        // Only the Rockies are reached by a pan, from Beringia at the same snapshot.
        let pans: Vec<_> = BENCH_REGIONS.iter().map(|r| r.pan_from_previous).collect();
        assert_eq!(
            pans,
            [false, false, false, false, false, false, true, false]
        );
        assert_eq!(BENCH_REGIONS[5].arg.years, BENCH_REGIONS[6].arg.years);
        assert_eq!(stop_count(&BENCH_REGIONS), 9);
        assert_eq!(after_region(0, 8, 10), Next::Region(1));
        assert_eq!(after_region(6, 8, 10), Next::Region(7));
        assert_eq!(after_region(7, 8, 10), Next::Cycle(0));
        assert_eq!(after_region(7, 8, 0), Next::Summary);
        // A single-region run goes from its one region straight to the summary.
        assert_eq!(after_region(0, 1, 0), Next::Summary);
    }

    #[test]
    fn reports_say_whether_dunes_were_drawn() {
        assert_eq!(dunes_line(true), "dunes: on");
        assert_eq!(dunes_line(false), "dunes: off");
    }

    #[test]
    fn single_config_flies_one_region_without_cycles() {
        let arg = RegionArg {
            lat_deg: 20.0,
            lon_deg: 37.5,
            years: 0.0,
        };
        let c = RegionBenchConfig::single(30, true, arg);
        assert_eq!(c.regions.len(), 1);
        assert_eq!(c.regions[0].name, "Chosen region");
        assert_eq!(c.regions[0].arg, arg);
        assert!(!c.regions[0].pan_from_previous);
        assert_eq!(c.cycles, 0);
        let bench = RegionBench::new(c.clone());
        assert_eq!(bench.stop_count(), 1);
        assert_eq!(bench.cycle_region(), None);
        assert_eq!(regions_line(&c), "regions: single (20, 37.5 at 0 years)");
        assert_eq!(
            regions_line(&RegionBenchConfig::new(30, true)),
            "regions: all 8"
        );
        // The full bench is unchanged.
        let full = RegionBench::new(RegionBenchConfig::new(30, true));
        assert_eq!(full.stop_count(), 9);
        assert_eq!(full.config.regions, BENCH_REGIONS.to_vec());
    }

    #[test]
    fn the_memory_cycles_re_enter_the_himalayas_by_name() {
        assert_eq!(CYCLE_REGION, "Himalayas");
        let cycled = cycle_region(&BENCH_REGIONS).unwrap();
        assert_eq!(BENCH_REGIONS[cycled].name, CYCLE_REGION);
        // Not the first region: the cycles must not depend on the order.
        assert_ne!(cycled, 0);
        assert!(BENCH_REGIONS[cycled].arg.years == 0.0);
    }

    #[test]
    fn the_pan_follows_the_great_circle_eased() {
        // The bench pans at the top of globe flight, the view nearest M1's orbit pan.
        assert_eq!(PAN_ALTITUDE_KM, GLOBE_MAX_ALTITUDE_KM);
        let from = (62.0, -175.0);
        let to = (48.5, -113.5);
        let dir = |f: GlobeFlight| f.dir;
        let (a, b) = (
            geo_to_render(latlon_to_geo(from.0, from.1)).0,
            geo_to_render(latlon_to_geo(to.0, to.1)).0,
        );
        let start = pan_globe(from, to, 955.0, 0.0);
        assert!((dir(start) - a).length() < 1e-9);
        assert_eq!(start.altitude_km, 955.0);
        let end = pan_globe(from, to, 955.0, PAN_SECS);
        assert!((dir(end) - b).length() < 1e-9);
        assert_eq!(pan_globe(from, to, 955.0, PAN_SECS + 3.0), end);
        // Heading along the arc, a unit tangent, all the way.
        for k in 0..=8 {
            let f = pan_globe(from, to, 955.0, PAN_SECS * f64::from(k) / 8.0);
            assert!((f.heading.length() - 1.0).abs() < 1e-9 && f.heading.dot(f.dir).abs() < 1e-9);
            let later = pan_globe(from, to, 955.0, PAN_SECS * (f64::from(k) + 0.5) / 8.0);
            if k < 8 {
                assert!((later.dir - f.dir).dot(f.heading) > 0.0, "{k}");
            }
        }
        // Halfway in time is halfway along the arc (the ease is symmetric).
        let mid = dir(pan_globe(from, to, 955.0, PAN_SECS / 2.0));
        assert!((mid.dot(a) - mid.dot(b)).abs() < 1e-9);
        assert!(
            (mid.normalize().dot(a.cross(b).normalize())).abs() < 1e-9,
            "on the great circle"
        );
        // Eased: the first tenth of the time covers under a tenth of the arc.
        let early = dir(pan_globe(from, to, 955.0, PAN_SECS / 10.0));
        let total = a.dot(b).acos();
        assert!(early.dot(a).clamp(-1.0, 1.0).acos() < 0.1 * total);
    }
}
