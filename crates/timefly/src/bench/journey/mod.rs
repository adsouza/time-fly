//! The journey bench (`--journey-bench`, web `?journey-bench`): M3 design §11.
//!
//! It plays the player's keys through every M3 transition, so the real dive, hops and climb run
//! and nothing places the fly. Once the globe is interactive and a short settle has passed, each
//! journey:
//!
//! 1. **Flies the globe** to above [`JourneyBenchConfig::region`] (within [`ARRIVE_DEG`]) with the
//!    turn keys and `W` ([`globe_keys`]), then descends with `Ctrl` until the dive commits below
//!    `DIVE_ALTITUDE_KM`. It lets go while the dive waits in the cloud and the fly emerges.
//! 2. **Flies east** in the region ([`region_keys`]): turns to face east, then holds `W` to the
//!    edge. The neighbour preloads, the lift and the cloud bank gather, the fly holds in the
//!    cloud and the views swap. `W` stays held, alone, through the whole hop: a turn could point
//!    the wish back into the field, which ends a hop from its hold. Then it does the same again.
//! 3. **Climbs out** with `Backspace` after [`HOPS_PER_CYCLE`] hops, and rests on the globe for
//!    [`LEAVE_SETTLE_SECS`] before memory is sampled.
//!
//! The keys go through `ButtonInput<KeyCode>`, the shell's own input path: [`drive_journey_keys`]
//! presses them before navigation reads them, and re-presses any the window let go (a lost focus
//! releases every key). Measuring runs after navigation, on the frame's own phase and cloud.
//!
//! **Summary and gates** (one block, in the scrub and region benches' format):
//!
//! - **`hold: <transition> <ms>`** for each of a journey's transitions ([`Transition`]: the
//!   dive, hop 1, hop 2 and the climb). The hold is the white-out the player sits through: from
//!   the first frame drawn at cloud density exactly 1 to the first frame after it drawn below 1,
//!   in real time between those frames ([`HoldClock`]). Natively each is gated at
//!   [`HOLD_LIMIT_NATIVE_MS`]; the web reports against [`HOLD_TARGET_WEB_MS`] (design §8).
//! - **Flight**: every frame drawn with no cloud at all (density 0), a preload's included, under
//!   M2's region rule (R14, [`super::passes`]).
//! - **Swap worst frame**: the longest of each swap's frame and the [`SWAP_WINDOW_FRAMES`] − 1
//!   after it (the swap's first draw can land a frame later), the swap read from the phase
//!   change ([`is_swap`]). Natively gated at [`SWAP_FRAME_LIMIT_MS`]; reported only on the web.
//! - **Hop memory peak**: natively, the process footprint's peak during each hop (preload to
//!   back flying) over the in-region baseline (the last flying frame before it), at most
//!   [`HOP_MEMORY_LIMIT_MIB`]. On the web, the wasm linear memory must not grow across any hop
//!   after the first ([`hop_memory_gate`]).
//! - **Journey memory cycles**: memory on the globe after each journey within
//!   [`CYCLE_MEMORY_TOLERANCE`] of the first's ([`memory_cycles_pass`]).
//!
//! A stage that does not finish within [`STAGE_TIMEOUT_SECS`], a dive that comes back out over
//! the globe, or a hop that turns back fails the run with the reason. On native the run exits
//! after the summary: 0 on PASS, `FAILED_RESULT_EXIT_CODE` on FAIL, as the other benches.

use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::globe::GlobeFlight;
use timefly_core::geo::{geo_to_render, latlon_to_geo};

use super::region::{
    BenchRegion, CYCLE_MEMORY_TOLERANCE, Screen, cycle_memory, cycle_memory_kind, dunes_line, emit,
    gate_word, host_stall_line, memory_cycles_pass, pacing_for, pacing_name, show_mib,
};
use super::{
    MAX_MISSED_SHARE, MEDIAN_TOLERANCE, MISSED_INTERVALS, Pacing, STALL_BUDGETS, bench_exit,
    frame_budget_ms, frame_time_stats, header_lines_titled, missed_frames, passes, stall_count,
};
use crate::args::RegionArg;
use crate::data::GlobeData;
use crate::diagnostics::SessionInfo;
use crate::fly_camera::FlyState;
use crate::globe::GlobeRenderStatus;
use crate::globe_flight::GlobeFlightState;
use crate::region::RegionState;
use crate::region::navigate::{LEAVE_KEY, NavPhase, Navigation, NavigationSystems, World};
use crate::terrain::TerrainOptions;
use crate::time::{GlobeTime, TimeChangeCause, TimeChanged};

/// The journey's region: the Alps at the present, high relief and a present-day snapshot.
pub const JOURNEY_REGION: BenchRegion = BenchRegion {
    name: "Alps",
    arg: RegionArg {
        lat_deg: 46.5,
        lon_deg: 10.0,
        years: 0.0,
    },
    pan_from_previous: false,
};
/// The globe's fly is over the region once within this arc of it (degrees): wider than a frame's
/// travel at the top of globe flight (the track's 30°/s cap is 1° a frame at 30 fps), so it never
/// overshoots, and well inside the region's own field of view from the dive altitude.
pub const ARRIVE_DEG: f64 = 2.0;
/// The region's fly heads east (yaw about `+y`, positive west).
pub const EAST_YAW: f32 = -std::f32::consts::FRAC_PI_2;
/// Within this of east (degrees) the region's fly flies on: wider than a frame's turn at 30 fps.
pub const YAW_TOLERANCE_DEG: f32 = 3.0;
/// The turn keys rest within this of the wanted heading (degrees), so they never chatter.
const TURN_DEADBAND_DEG: f64 = 1.5;
/// Over the globe, `W` flies only while the heading is within this of the region's bearing
/// (degrees): a fly facing away turns first.
const FACE_DEG: f64 = 10.0;
/// Journeys per run.
pub const JOURNEY_CYCLES: u32 = 3;
/// Hops per journey.
pub const HOPS_PER_CYCLE: u32 = 2;
/// The longest hold natively, per transition (design §11, gated).
pub const HOLD_LIMIT_NATIVE_MS: f64 = 3_000.0;
/// The web's hold target, per transition (design §8, reported only).
pub const HOLD_TARGET_WEB_MS: f64 = 8_000.0;
/// The longest swap frame natively (design §11, gated).
pub const SWAP_FRAME_LIMIT_MS: f32 = 100.0;
/// A swap's frames: the swap frame and the ones after it, which draw it.
pub const SWAP_WINDOW_FRAMES: u32 = 3;
/// The most a hop may add to the in-region footprint natively (design §11, gated).
pub const HOP_MEMORY_LIMIT_MIB: u64 = 40;
/// Seconds on the globe before the first journey.
pub const SETTLE_SECS: f64 = 1.0;
/// Seconds on the globe after a climb, before memory is sampled and the next journey starts.
pub const LEAVE_SETTLE_SECS: f64 = 1.5;
/// A stage (the globe flight to the dive, the dive, the region with its hops, the climb) that
/// has not finished in this long fails the run.
pub const STAGE_TIMEOUT_SECS: f64 = 180.0;

/// One of a journey's transitions through the cloud.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Dive,
    /// The `n`th hop of the journey, from 1.
    Hop(u32),
    Climb,
}

impl Transition {
    /// Its name in the summary: `dive`, `hop 1`, `hop 2`, `climb`.
    pub fn label(self) -> String {
        match self {
            Transition::Dive => "dive".to_string(),
            Transition::Hop(n) => format!("hop {n}"),
            Transition::Climb => "climb".to_string(),
        }
    }
}

/// A journey's transitions, in order: the dive, `hops` hops, the climb.
pub fn cycle_transitions(hops: u32) -> Vec<Transition> {
    std::iter::once(Transition::Dive)
        .chain((1..=hops).map(Transition::Hop))
        .chain(std::iter::once(Transition::Climb))
        .collect()
}

/// Times a transition's hold: from the first frame drawn at cloud density exactly 1 to the first
/// frame after it drawn below 1, in the real seconds between those frames. Later spells at full
/// cover in the same transition do not count.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct HoldClock {
    opaque_since: Option<f64>,
    hold_ms: Option<f64>,
}

impl HoldClock {
    /// One frame, at `now_s`, drawn at `density`: the hold in milliseconds if it ends on it.
    pub fn observe(&mut self, now_s: f64, density: f32) -> Option<f64> {
        if self.hold_ms.is_some() {
            return None;
        }
        if density == 1.0 {
            self.opaque_since.get_or_insert(now_s);
            return None;
        }
        let since = self.opaque_since?;
        let ms = (now_s - since) * 1000.0;
        self.hold_ms = Some(ms);
        Some(ms)
    }

    /// The hold, once it has ended.
    pub fn hold_ms(&self) -> Option<f64> {
        self.hold_ms
    }
}

/// Whether the views swapped between two frames' phases: the dive's into the region, a hop's
/// into the neighbour, the climb's onto the globe. A dive that fails (out over the globe) and a
/// hop that turns back are not swaps.
pub fn is_swap(before: &NavPhase, after: &NavPhase) -> bool {
    matches!(
        (before, after),
        (
            NavPhase::Diving { .. },
            NavPhase::Emerging {
                world: World::Region,
                ..
            }
        ) | (
            NavPhase::Hopping { swapped: false, .. },
            NavPhase::Hopping { swapped: true, .. }
        ) | (
            NavPhase::Climbing { .. },
            NavPhase::Emerging {
                world: World::Globe,
                ..
            }
        )
    )
}

/// A hold's summary line, `hold: <transition> <ms> ms (cycle <n>) -> <verdict>`, and its gate:
/// natively at most [`HOLD_LIMIT_NATIVE_MS`]; on the web reported against
/// [`HOLD_TARGET_WEB_MS`] (`None`).
pub fn hold_line(t: Transition, ms: f64, cycle: u32, web: bool) -> (String, Option<bool>) {
    let head = format!("hold: {} {ms:.0} ms (cycle {cycle}) -> ", t.label());
    if web {
        (
            format!("{head}reported only (web target <= {HOLD_TARGET_WEB_MS:.0} ms)"),
            None,
        )
    } else {
        let ok = ms <= HOLD_LIMIT_NATIVE_MS;
        (format!("{head}{}", gate_word(ok)), Some(ok))
    }
}

/// The hop memory gate over each hop's `(baseline, peak, end)` samples. Natively (`web` false)
/// the worst `peak − baseline` must be at most [`HOP_MEMORY_LIMIT_MIB`]; on the web the wasm
/// linear memory (it only grows) must not grow, `end − baseline`, in any hop after the first.
/// `(pass, worst bytes over the baseline)`, or `None` with a sample missing or nothing to judge.
pub fn hop_memory_gate(
    hops: &[(Option<u64>, Option<u64>, Option<u64>)],
    web: bool,
) -> Option<(bool, i64)> {
    let over = |base: Option<u64>, top: Option<u64>| Some(top? as i64 - base? as i64);
    if web {
        let growth: Vec<i64> = hops
            .iter()
            .skip(1)
            .map(|&(base, _, end)| over(base, end))
            .collect::<Option<_>>()?;
        let worst = growth.into_iter().max()?;
        Some((worst <= 0, worst))
    } else {
        let worst = hops
            .iter()
            .map(|&(base, peak, _)| over(base, peak))
            .collect::<Option<Vec<i64>>>()?
            .into_iter()
            .max()?;
        Some((worst <= (HOP_MEMORY_LIMIT_MIB << 20) as i64, worst))
    }
}

/// The direction (render frame) over `arg`'s latitude and longitude.
fn region_dir(arg: &RegionArg) -> DVec3 {
    geo_to_render(latlon_to_geo(arg.lat_deg, arg.lon_deg))
        .0
        .normalize()
}

/// The keys flying the globe's fly toward `target` (a direction): turn toward its bearing,
/// fly forward while facing it, and once within [`ARRIVE_DEG`] only descend (`Ctrl`) to the dive.
pub fn globe_keys(flight: &GlobeFlight, target: DVec3) -> Vec<KeyCode> {
    let up = flight.dir.normalize();
    let target = target.normalize();
    if up.dot(target).clamp(-1.0, 1.0).acos() <= ARRIVE_DEG.to_radians() {
        return vec![KeyCode::ControlLeft];
    }
    let toward = (target - up * up.dot(target)).normalize_or_zero();
    if toward == DVec3::ZERO {
        // The antipode: every way leads there.
        return vec![KeyCode::KeyW];
    }
    let h = flight.heading;
    // Positive when the bearing lies to the left (about up), as the turn's own sign.
    let err = up.dot(h.cross(toward)).atan2(h.dot(toward));
    let mut keys = Vec::new();
    if err.abs() > TURN_DEADBAND_DEG.to_radians() {
        keys.push(if err > 0.0 {
            KeyCode::ArrowLeft
        } else {
            KeyCode::ArrowRight
        });
    }
    if err.abs() < FACE_DEG.to_radians() {
        keys.push(KeyCode::KeyW);
    }
    keys
}

/// The keys flying the region's fly, heading `yaw`, in `phase`: while flying, turn to face east
/// and fly forward within [`YAW_TOLERANCE_DEG`] of it; through a hop, `W` alone (a turn could
/// point the wish back in, which ends a hop from its hold); nothing while navigation moves the
/// fly.
pub fn region_keys(phase: &NavPhase, yaw: f32) -> Vec<KeyCode> {
    match phase {
        NavPhase::Hopping { .. } => vec![KeyCode::KeyW],
        NavPhase::Flying => {
            let tau = std::f32::consts::TAU;
            let err =
                (EAST_YAW - yaw + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
            let mut keys = Vec::new();
            if f64::from(err.abs()) > TURN_DEADBAND_DEG.to_radians() {
                // The right arrow lowers the yaw (positive is west of north).
                keys.push(if err > 0.0 {
                    KeyCode::ArrowLeft
                } else {
                    KeyCode::ArrowRight
                });
            }
            if err.abs() <= YAW_TOLERANCE_DEG.to_radians() {
                keys.push(KeyCode::KeyW);
            }
            keys
        }
        _ => Vec::new(),
    }
}

/// The bench's settings. Tests cut the cycles.
#[derive(Clone, Debug, PartialEq)]
pub struct JourneyBenchConfig {
    pub cycles: u32,
    pub region: BenchRegion,
    pub target_fps: u32,
    pub exit_when_done: bool,
}

impl JourneyBenchConfig {
    pub fn new(target_fps: u32, exit_when_done: bool) -> Self {
        JourneyBenchConfig {
            cycles: JOURNEY_CYCLES,
            region: JOURNEY_REGION,
            target_fps,
            exit_when_done,
        }
    }
}

/// Where the run is. Times are `Time<Real>` seconds; `cycle` counts from 0.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Stage {
    WaitingForGlobe,
    Settling {
        since: f64,
    },
    /// Flying the globe to above the region and down to the dive.
    Approach {
        cycle: u32,
        since: f64,
    },
    /// The dive committed: waiting in the cloud, then emerging into the region.
    Dive {
        cycle: u32,
        since: f64,
    },
    /// In the region, `hops` hops done: flying east and hopping.
    Region {
        cycle: u32,
        hops: u32,
        since: f64,
    },
    /// `Backspace` took: climbing out and emerging over the globe.
    Climb {
        cycle: u32,
        since: f64,
    },
    Resting {
        cycle: u32,
        since: f64,
    },
    Done,
}

impl Stage {
    /// The journey a stage belongs to, if it is one of the script's.
    fn cycle(self) -> Option<u32> {
        match self {
            Stage::Approach { cycle, .. }
            | Stage::Dive { cycle, .. }
            | Stage::Region { cycle, .. }
            | Stage::Climb { cycle, .. }
            | Stage::Resting { cycle, .. } => Some(cycle),
            Stage::WaitingForGlobe | Stage::Settling { .. } | Stage::Done => None,
        }
    }
}

/// One transition's hold.
#[derive(Copy, Clone, Debug)]
struct HoldRecord {
    transition: Transition,
    cycle: u32,
    clock: HoldClock,
}

/// One swap's frames.
#[derive(Copy, Clone, Debug)]
struct SwapRecord {
    transition: Transition,
    cycle: u32,
    worst_ms: f32,
    frames_left: u32,
}

/// One hop's memory: the in-region baseline before it, its peak, and on return to flying.
#[derive(Copy, Clone, Debug, Default)]
struct HopMemory {
    baseline: Option<u64>,
    peak: Option<u64>,
    end: Option<u64>,
    open: bool,
}

/// The benchmark's state and measurements.
#[derive(Resource)]
pub struct JourneyBench {
    pub config: JourneyBenchConfig,
    stage: Stage,
    /// Navigation's phase in the last frame measured.
    prev_phase: NavPhase,
    /// Frames drawn with no cloud (density 0) during the script.
    flight_ms: Vec<f32>,
    holds: Vec<HoldRecord>,
    swaps: Vec<SwapRecord>,
    hops: Vec<HopMemory>,
    /// The memory sampled on the last frame flown in a region.
    in_region: Option<u64>,
    /// The memory on the globe after each journey.
    on_globe: Vec<Option<u64>>,
    /// The keys the bench holds down.
    keys_held: Vec<KeyCode>,
    summary: Option<String>,
    passed: Option<bool>,
}

impl JourneyBench {
    pub fn new(config: JourneyBenchConfig) -> Self {
        JourneyBench {
            config,
            stage: Stage::WaitingForGlobe,
            prev_phase: NavPhase::Globe,
            flight_ms: Vec::new(),
            holds: Vec::new(),
            swaps: Vec::new(),
            hops: Vec::new(),
            in_region: None,
            on_globe: Vec::new(),
            keys_held: Vec::new(),
            summary: None,
            passed: None,
        }
    }

    /// The summary block, once the run has ended.
    pub fn summary(&self) -> Option<&str> {
        self.summary.as_deref()
    }

    /// The run's result, once the summary is out.
    pub fn passed(&self) -> Option<bool> {
        self.passed
    }

    /// Starts timing `transition`'s hold (a hop that starts again after turning back before its
    /// lift times afresh).
    fn start_hold(&mut self, transition: Transition, cycle: u32) {
        let record = HoldRecord {
            transition,
            cycle,
            clock: HoldClock::default(),
        };
        match self.holds.last_mut() {
            Some(last)
                if last.transition == transition
                    && last.cycle == cycle
                    && last.clock.hold_ms().is_none() =>
            {
                *last = record;
            }
            _ => self.holds.push(record),
        }
    }
}

pub struct JourneyBenchPlugin(pub JourneyBenchConfig);

impl Plugin for JourneyBenchPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(JourneyBench::new(self.0.clone()))
            .add_systems(
                Update,
                (
                    // Keys first, so navigation and both flies read them this frame.
                    drive_journey_keys.before(NavigationSystems),
                    measure_journey.after(NavigationSystems),
                ),
            );
    }
}

/// Holds the script's keys down through `ButtonInput<KeyCode>`, before navigation reads them.
fn drive_journey_keys(
    mut bench: ResMut<JourneyBench>,
    nav: Option<Res<Navigation>>,
    flight: Option<Res<GlobeFlightState>>,
    fly: Option<Res<FlyState>>,
    keys: Option<ResMut<ButtonInput<KeyCode>>>,
) {
    let Some(mut keys) = keys else {
        return;
    };
    let phase = nav.map_or(NavPhase::Globe, |n| n.phase);
    let want: Vec<KeyCode> = match bench.stage {
        Stage::Approach { .. } if phase == NavPhase::Globe => flight.map_or(Vec::new(), |f| {
            globe_keys(&f.0, region_dir(&bench.config.region.arg))
        }),
        // Tapped: pressed on one frame, let go on the next, until the climb takes.
        Stage::Region { hops, .. } if hops >= HOPS_PER_CYCLE && phase == NavPhase::Flying => {
            if bench.keys_held.contains(&LEAVE_KEY) {
                Vec::new()
            } else {
                vec![LEAVE_KEY]
            }
        }
        Stage::Region { .. } => fly.map_or(Vec::new(), |f| region_keys(&phase, f.yaw)),
        _ => Vec::new(),
    };
    let held = std::mem::take(&mut bench.keys_held);
    for key in held.iter().filter(|k| !want.contains(k)) {
        keys.release(*key);
    }
    // Pressed again if the window let go of it (a lost focus releases every key).
    for key in &want {
        if !keys.pressed(*key) {
            keys.press(*key);
        }
    }
    bench.keys_held = want;
}

/// What the bench reads about the game.
#[derive(SystemParam)]
struct Game<'w> {
    real: Res<'w, Time<Real>>,
    status: Option<Res<'w, GlobeRenderStatus>>,
    region_state: Option<Res<'w, State<RegionState>>>,
    nav: Option<Res<'w, Navigation>>,
    session: Option<Res<'w, SessionInfo>>,
    globe: Option<Res<'w, GlobeData>>,
    options: Option<Res<'w, TerrainOptions>>,
}

/// What the bench changes.
#[derive(SystemParam)]
struct Controls<'w> {
    time: Option<ResMut<'w, GlobeTime>>,
    changes: MessageWriter<'w, TimeChanged>,
    exit: MessageWriter<'w, AppExit>,
}

/// Records this frame (after navigation decided it) and moves the script on.
fn measure_journey(
    mut bench: ResMut<JourneyBench>,
    game: Game,
    mut controls: Controls,
    display: Screen,
) {
    let bench = &mut *bench;
    let now = game.real.elapsed_secs_f64();
    let frame_ms = game.real.delta_secs() * 1000.0;
    let phase = game.nav.as_ref().map_or(NavPhase::Globe, |n| n.phase);
    let density = game.nav.as_ref().map_or(0.0, |n| n.density);
    let state = game
        .region_state
        .as_ref()
        .map_or(RegionState::Globe, |s| *s.get());
    let before = bench.prev_phase;
    bench.prev_phase = phase;
    if bench.stage != Stage::Done
        && let Some(GlobeRenderStatus::Failed(reason)) = game.status.as_deref()
    {
        let reason = format!("the globe failed: {reason}");
        finish(bench, &game, &display, &mut controls, Some(&reason));
        return;
    }
    let swapped = is_swap(&before, &phase);
    if let Some(cycle) = bench.stage.cycle() {
        record_frame(bench, cycle, before, phase, density, frame_ms, now, swapped);
    }
    let timed_out = |since: f64| now - since > STAGE_TIMEOUT_SECS;
    let fail = match bench.stage {
        Stage::WaitingForGlobe => {
            if matches!(
                game.status.as_deref(),
                Some(GlobeRenderStatus::Interactive { .. })
            ) {
                info!("journey bench: globe interactive, settling");
                bench.stage = Stage::Settling { since: now };
            }
            None
        }
        Stage::Settling { since } => {
            if now - since >= SETTLE_SECS {
                let region = bench.config.region;
                if let Some(time) = controls.time.as_mut() {
                    let (from, to) = (time.years, YearsBp(region.arg.years));
                    if from != to {
                        time.years = to;
                        controls.changes.write(TimeChanged {
                            from,
                            to,
                            cause: TimeChangeCause::Bench,
                        });
                    }
                }
                start_journey(bench, 0, now);
            }
            None
        }
        Stage::Approach { cycle, since } => {
            if matches!(phase, NavPhase::Diving { .. }) {
                info!(cycle = cycle + 1, "journey bench: the dive committed");
                bench.stage = Stage::Dive { cycle, since: now };
                None
            } else if timed_out(since) {
                Some("the fly did not reach the dive in time".to_string())
            } else {
                None
            }
        }
        Stage::Dive { cycle, since } => match phase {
            NavPhase::Flying => {
                info!(
                    cycle = cycle + 1,
                    "journey bench: in the region, flying east"
                );
                bench.stage = Stage::Region {
                    cycle,
                    hops: 0,
                    since: now,
                };
                None
            }
            NavPhase::Diving { .. }
            | NavPhase::Emerging {
                world: World::Region,
                ..
            } if !timed_out(since) => None,
            NavPhase::Diving { .. }
            | NavPhase::Emerging {
                world: World::Region,
                ..
            } => Some("the dive did not reach the region in time".to_string()),
            _ => Some("the dive came back out over the globe (see the log)".to_string()),
        },
        Stage::Region { cycle, hops, since } => {
            let hops = hops + u32::from(swapped);
            if swapped {
                info!(cycle = cycle + 1, hop = hops, "journey bench: hopped");
            }
            bench.stage = Stage::Region { cycle, hops, since };
            match phase {
                NavPhase::Hopping {
                    turned_back: true, ..
                } => Some(format!(
                    "hop {} turned back (the neighbour failed or the hold ran out)",
                    hops + 1
                )),
                NavPhase::Climbing { .. } if hops >= HOPS_PER_CYCLE => {
                    info!(cycle = cycle + 1, "journey bench: climbing out");
                    bench.stage = Stage::Climb { cycle, since: now };
                    None
                }
                NavPhase::Climbing { .. } => Some(format!(
                    "the fly climbed out after {hops} of {HOPS_PER_CYCLE} hops"
                )),
                _ if state == RegionState::Globe => {
                    Some("the region was left before its hops (see the log)".to_string())
                }
                _ if timed_out(since) => Some(format!(
                    "the hops were not done in time ({hops} of {HOPS_PER_CYCLE})"
                )),
                _ => None,
            }
        }
        Stage::Climb { cycle, since } => {
            if phase == NavPhase::Globe && state == RegionState::Globe {
                bench.stage = Stage::Resting { cycle, since: now };
                None
            } else if timed_out(since) {
                Some("the climb did not reach the globe in time".to_string())
            } else {
                None
            }
        }
        Stage::Resting { cycle, since } => {
            if now - since >= LEAVE_SETTLE_SECS {
                let sample = cycle_memory();
                info!(cycle = cycle + 1, memory = ?sample, "journey bench: journey done");
                bench.on_globe.push(sample);
                if cycle + 1 < bench.config.cycles {
                    start_journey(bench, cycle + 1, now);
                } else {
                    finish(bench, &game, &display, &mut controls, None);
                }
            }
            None
        }
        Stage::Done => None,
    };
    if let Some(reason) = fail {
        warn!(reason, "journey bench: failed");
        finish(bench, &game, &display, &mut controls, Some(&reason));
    }
}

fn start_journey(bench: &mut JourneyBench, cycle: u32, now: f64) {
    info!(
        cycle = cycle + 1,
        region = bench.config.region.name,
        "journey bench: flying the globe to the dive"
    );
    bench.stage = Stage::Approach { cycle, since: now };
}

/// One frame of the script: its flight time, holds, swaps and memory.
#[expect(clippy::too_many_arguments, reason = "one frame's readings")]
fn record_frame(
    bench: &mut JourneyBench,
    cycle: u32,
    before: NavPhase,
    phase: NavPhase,
    density: f32,
    frame_ms: f32,
    now: f64,
    swapped: bool,
) {
    if density == 0.0 {
        bench.flight_ms.push(frame_ms);
    }
    // Transitions start on the phase change into them.
    let hops_done = match bench.stage {
        Stage::Region { hops, .. } => hops,
        _ => 0,
    };
    let memory = cycle_memory();
    let entered = std::mem::discriminant(&phase) != std::mem::discriminant(&before);
    match phase {
        NavPhase::Diving { .. } if entered => bench.start_hold(Transition::Dive, cycle),
        NavPhase::Hopping { .. } if entered => {
            bench.start_hold(Transition::Hop(hops_done + 1), cycle);
            match bench.hops.last_mut() {
                // Started again after turning back before its lift: the same hop.
                Some(last) if last.open => {}
                _ => bench.hops.push(HopMemory {
                    baseline: bench.in_region,
                    peak: memory,
                    end: None,
                    open: true,
                }),
            }
        }
        NavPhase::Climbing { .. } if entered => bench.start_hold(Transition::Climb, cycle),
        _ => {}
    }
    if let Some(hold) = bench.holds.last_mut() {
        hold.clock.observe(now, density);
    }
    // Swaps: the swap frame and the frames after it.
    for swap in bench.swaps.iter_mut().filter(|s| s.frames_left > 0) {
        swap.worst_ms = swap.worst_ms.max(frame_ms);
        swap.frames_left -= 1;
    }
    if swapped && let Some(hold) = bench.holds.last() {
        bench.swaps.push(SwapRecord {
            transition: hold.transition,
            cycle: hold.cycle,
            worst_ms: frame_ms,
            frames_left: SWAP_WINDOW_FRAMES - 1,
        });
    }
    // Memory: the in-region baseline while flying, the peak through a hop.
    match phase {
        NavPhase::Flying => {
            bench.in_region = memory;
            if let Some(hop) = bench.hops.last_mut().filter(|h| h.open) {
                hop.end = memory;
                hop.open = false;
            }
        }
        NavPhase::Hopping { .. } => {
            if let Some(hop) = bench.hops.last_mut().filter(|h| h.open) {
                hop.peak = super::max_opt(hop.peak, memory);
            }
        }
        _ => {
            if let Some(hop) = bench.hops.last_mut().filter(|h| h.open) {
                hop.end = memory;
                hop.open = false;
            }
        }
    }
}

/// `+1.5 MiB` or `-0.3 MiB`.
fn signed_mib(bytes: i64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    format!("{mib:+.1} MiB")
}

/// The summary block, then the exit.
fn finish(
    bench: &mut JourneyBench,
    game: &Game,
    display: &Screen,
    controls: &mut Controls,
    failure: Option<&str>,
) {
    bench.stage = Stage::Done;
    let web = cfg!(target_arch = "wasm32");
    let fps = bench.config.target_fps;
    let info = display.info();
    let mut out = header_lines_titled(
        "===== Time Fly journey bench summary =====",
        game.session.as_deref(),
        display.adapter.as_deref(),
    );
    let dunes = game.options.as_deref().is_none_or(|o| o.dunes);
    let at = out
        .iter()
        .position(|l| l.starts_with("system spans: "))
        .map_or(out.len(), |i| i + 1);
    out.insert(at, dunes_line(dunes));
    if let Some(size) = info.size {
        out.push(format!("render resolution: {}x{}", size.x, size.y));
    }
    if let Some(globe) = game.globe.as_deref() {
        out.push(format!("bake: {}", globe.revision.summary()));
    }
    let region = bench.config.region;
    let cycles = bench.config.cycles;
    out.push(format!(
        "region: {} ({}, {} at {} years), {cycles} cycle{} of a dive, {HOPS_PER_CYCLE} hops and a climb",
        region.name,
        region.arg.lat_deg,
        region.arg.lon_deg,
        region.arg.years,
        if cycles == 1 { "" } else { "s" }
    ));
    let mut pass = failure.is_none();

    // Holds.
    out.push(format!(
        "hold (from the first frame drawn at cloud density exactly 1 to the first frame after it drawn below 1): {}",
        if web {
            format!("web reported only, target <= {HOLD_TARGET_WEB_MS:.0} ms each")
        } else {
            format!("native gate <= {HOLD_LIMIT_NATIVE_MS:.0} ms each")
        }
    ));
    for hold in &bench.holds {
        match hold.clock.hold_ms() {
            Some(ms) => {
                let (line, gate) = hold_line(hold.transition, ms, hold.cycle + 1, web);
                pass &= gate.unwrap_or(true);
                out.push(line);
            }
            None => {
                pass = false;
                out.push(format!(
                    "hold: {} not measured (cycle {}) -> FAIL",
                    hold.transition.label(),
                    hold.cycle + 1
                ));
            }
        }
    }

    // Flight outside the cloud.
    let flight = &bench.flight_ms;
    let pacing = pacing_for(&info, flight);
    let interval = frame_budget_ms(fps);
    let s = frame_time_stats(flight);
    out.push(format!(
        "present mode: {} requested{}; display refresh {}; median {:.2} ms -> {}",
        info.present_mode
            .map_or("unknown".to_string(), |m| format!("{m:?}")),
        if web {
            " (browser requestAnimationFrame)"
        } else {
            ""
        },
        info.refresh_hz
            .map_or("unknown".to_string(), |hz| format!("{hz:.1} Hz")),
        s.median_ms,
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
    let flight_pass = passes(flight, fps, pacing);
    pass &= flight_pass;
    let missed = missed_frames(flight, fps);
    let share = if s.frames > 0 {
        100.0 * missed as f64 / s.frames as f64
    } else {
        0.0
    };
    out.push(format!(
        "flight (outside the cloud): frames {}, median {:.2} ms, p95 {:.2} ms, worst {:.2} ms, stalls (>{:.1} ms = {STALL_BUDGETS}x interval) {} -> {}",
        s.frames,
        s.median_ms,
        s.p95_ms,
        s.worst_ms,
        STALL_BUDGETS * interval,
        stall_count(flight, fps),
        gate_word(flight_pass)
    ));
    out.push(format!(
        "  missed frames (>{:.1} ms = {MISSED_INTERVALS}x interval): {missed} ({share:.2}%)",
        MISSED_INTERVALS * interval,
    ));
    out.push(host_stall_line("flight", flight));

    // Swaps.
    let scope = format!(
        "{}; the swap frame and the {} after it",
        if web {
            format!("web reported only, native gate <= {SWAP_FRAME_LIMIT_MS:.0} ms")
        } else {
            format!("native gate <= {SWAP_FRAME_LIMIT_MS:.0} ms")
        },
        SWAP_WINDOW_FRAMES - 1
    );
    if bench.swaps.is_empty() {
        out.push(format!("swap worst frame ({scope}): no swap"));
    } else {
        let worst = bench.swaps.iter().map(|s| s.worst_ms).fold(0.0, f32::max);
        let each: Vec<String> = bench
            .swaps
            .iter()
            .map(|s| {
                format!(
                    "{} {:.2} ms (cycle {})",
                    s.transition.label(),
                    s.worst_ms,
                    s.cycle + 1
                )
            })
            .collect();
        let ok = worst <= SWAP_FRAME_LIMIT_MS;
        let verdict = if web {
            "reported only".to_string()
        } else {
            pass &= ok;
            gate_word(ok).to_string()
        };
        out.push(format!(
            "swap worst frame ({scope}): {worst:.2} ms; {} -> {verdict}",
            each.join(", ")
        ));
    }

    // Hop memory.
    let samples: Vec<_> = bench
        .hops
        .iter()
        .map(|h| (h.baseline, h.peak, h.end))
        .collect();
    let hop_scope = if web {
        "wasm linear memory, which grows but never shrinks; gate: no growth in any hop after the first".to_string()
    } else {
        format!(
            "{}; gate: peak <= in-region baseline before the hop +{HOP_MEMORY_LIMIT_MIB} MiB",
            cycle_memory_kind()
        )
    };
    let each: Vec<String> = bench
        .hops
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let top = if web { h.end } else { h.peak };
            match (h.baseline, top) {
                (Some(b), Some(t)) => format!("{}: {}", i + 1, signed_mib(t as i64 - b as i64)),
                _ => format!("{}: n/a", i + 1),
            }
        })
        .collect();
    match hop_memory_gate(&samples, web) {
        Some((ok, worst)) => {
            pass &= ok;
            out.push(format!(
                "hop memory peak ({hop_scope}): worst {} over the baseline; hops {} -> {}",
                signed_mib(worst),
                each.join(", "),
                gate_word(ok)
            ));
        }
        None => out.push(format!(
            "hop memory peak ({hop_scope}): not measured ({} hops, memory {})",
            bench.hops.len(),
            if cycle_memory().is_some() {
                "available"
            } else {
                "not reported here"
            }
        )),
    }

    // Journey memory cycles.
    let listed = bench
        .on_globe
        .iter()
        .enumerate()
        .map(|(i, m)| format!("{}: {}", i + 1, show_mib(*m)))
        .collect::<Vec<_>>()
        .join(", ");
    match memory_cycles_pass(&bench.on_globe) {
        Some((ok, increase)) => {
            pass &= ok;
            out.push(format!(
                "journey memory cycles ({}, on the globe after each climb): {listed}; worst of cycles 2-{} is {:+.1}% of cycle 1 (limit +{:.0}%) -> {}",
                cycle_memory_kind(),
                bench.on_globe.len(),
                increase * 100.0,
                CYCLE_MEMORY_TOLERANCE * 100.0,
                gate_word(ok)
            ));
        }
        None => out.push(format!(
            "journey memory cycles: not measured ({} cycle{} done{})",
            bench.on_globe.len(),
            if bench.on_globe.len() == 1 { "" } else { "s" },
            if listed.is_empty() {
                String::new()
            } else {
                format!("; {listed}")
            }
        )),
    }
    out.push(match failure {
        Some(reason) => format!("result: FAIL ({reason})"),
        None => format!("result: {}", gate_word(pass)),
    });
    out.push("===== end journey bench summary =====".to_string());
    let summary = out.join("\n");
    emit(&summary);
    bench.summary = Some(summary);
    bench.passed = Some(pass);
    if bench.config.exit_when_done {
        controls.exit.write(bench_exit(pass));
    }
}

#[cfg(test)]
mod tests;
