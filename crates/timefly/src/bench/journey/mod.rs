//! The journey bench (`--journey-bench`, web `?journey-bench`): M3 design §11.
//!
//! It plays the player's keys through every M3 transition, so the real dive, hops and climb run
//! and nothing places the fly. A run flies a set of routes ([`JOURNEY_ROUTES`] in
//! [`JOURNEY_ORDER`]: the Andes, Pangaea at 250 Ma, the Grand Canyon, Mt Fuji over a coast to a
//! cube corner, then the Andes again; owner, 2026-11-25), or one route with `--region`. Once the globe
//! is interactive and a short settle has passed, each journey sets the time to its route's age
//! and:
//!
//! 1. **Flies the globe** to above the route's region (within [`ARRIVE_DEG`]) with the turn keys
//!    and `W` ([`globe_keys`]), then descends with `Q`, still steering in to within
//!    [`SETTLE_DEG`], until the dive commits below `DIVE_ALTITUDE_KM`. It lets go while the dive
//!    waits in the cloud and the fly emerges.
//! 2. **Descends, then flies out** along the route's heading in the region ([`region_keys`]:
//!    north for the Andes, Pangaea and the Grand Canyon, west for Mt Fuji, east for the headless
//!    tests' Alps). The fly emerges at 230 km,
//!    above `HOP_ALTITUDE_M`, where a hop's lift would do nothing; so before the first hop it
//!    holds `Q` alone, straight down in place (about 21 s at the 10 km/s cap), to
//!    [`CRUISE_AGL_M`] above the ground, turning to face the heading meanwhile. Then it holds `W` to the
//!    edge, still at the speed cap, so the preload's lead is the worst case. The neighbour
//!    preloads, the lift raises the fly to the cloud bank and the cloud gathers, the fly holds in
//!    the cloud and the views swap; after the swap `ReturnToAgl` brings it back to the cruise, so
//!    the second hop lifts too. `W` stays held, alone, through the whole hop: a turn could point
//!    the wish back into the field, which ends a hop from its hold. The summary's `hop lift` line
//!    reports each hop's lift (reported only).
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
//!   [`HOP_MEMORY_LIMIT_MIB`]. On the web, the wasm linear memory never shrinks, so a route
//!   larger than any before it raises it for good: a route's first visit may grow it, and no
//!   hop of a repeat visit may ([`hop_memory_gate`], [`JourneyBenchConfig::is_repeat`]).
//! - **Journey memory cycles**: memory on the globe after each journey within
//!   [`CYCLE_MEMORY_TOLERANCE`] of the same route's first journey ([`memory_cycles_pass`] per
//!   route, [`JourneyBenchConfig::by_route`]): a route is compared only with itself. On the web
//!   the growth of a route's repeat visits is judged against the memory before them
//!   ([`JourneyBenchConfig::memory_cycle_verdicts`]).
//! - **Route checks** (gated): a route's hops cross what the route is chosen for
//!   ([`Crossing`]: a coast by the neighbour's land share, a cube corner by its centre).
//! - **Sites** (reported only): the canyon's depth and Fuji's height, read from the grid of the
//!   hop's neighbour ([`read_site`]).
//! - **Continental passes and GPU frame time** (reported only): each pass that finished during
//!   the run ([`pass_lines`]), and the GPU frame times read on flight frames, per route
//!   ([`gpu_frame_line`]; a reading arrives a few frames after the frame it times).
//!
//! **Stopping early.** A stop is a journey. A gate decided within one ends the run as soon as it
//! fails ([`failed_journey_gate`]): a hold or a swap frame over its native gate, a hop's memory
//! over its gate, or a route check. The summary names the stops run, the failed gate and the
//! stops skipped. The flight (one pacing for the run, from every flight frame pooled) and the
//! journey memory cycles are judged at the end. `--keep-going` flies every journey.
//!
//! A stage that does not finish within [`STAGE_TIMEOUT_SECS`] (the longest, the region's, is
//! about 21 s of descent, 15 s and 25 s of flight and two holds: about 65 s natively), a dive that comes back out over
//! the globe, or a hop that turns back fails the run with the reason. On native the run exits
//! after the summary: 0 on PASS, `FAILED_RESULT_EXIT_CODE` on FAIL, as the other benches.

use bevy::ecs::system::SystemParam;
use bevy::math::DVec3;
use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::altitudes::HOP_ALTITUDE_M;
use timefly_core::flight::globe::GlobeFlight;
use timefly_core::geo::{RenderDir, geo_to_render, latlon_to_geo};
use timefly_core::terrain::{blended_height_m, blended_surface_m};
use timefly_core::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};
use timefly_core::worldgen::generate::RegionGrid;

use super::region::{
    BENCH_REGIONS, BenchRegion, CHOSEN_REGION, CYCLE_MEMORY_TOLERANCE, Screen, cycle_memory,
    cycle_memory_kind, dunes_line, emit, gate_word, host_stall_line, memory_cycles_pass,
    pacing_for, pacing_name, show_mib,
};
use super::{
    EarlyStop, MAX_MISSED_SHARE, MEDIAN_TOLERANCE, MISSED_INTERVALS, Pacing, STALL_BUDGETS,
    bench_exit, frame_budget_ms, frame_time_stats, gpu_available, gpu_frame_line, gpu_status_line,
    header_lines_titled, missed_frames, on_failure_line, pass_lines, passes, stall_count,
    stopped_early_lines,
};
use crate::args::RegionArg;
use crate::data::GlobeData;
use crate::diagnostics::SessionInfo;
use crate::diagnostics::gpu_time::{GpuFrameMeasured, GpuFrameTime};
use crate::fly_camera::FlyState;
use crate::globe::GlobeRenderStatus;
use crate::globe_flight::GlobeFlightState;
use crate::hydro::HydroPassFinished;
use crate::region::navigate::{LEAVE_KEY, NavPhase, Navigation, NavigationSystems, World};
use crate::region::{ActiveRegion, RegionState};
use crate::terrain::TerrainOptions;
use crate::time::{GlobeTime, TimeChangeCause, TimeChanged};

/// The Alps at the present: high relief and a present-day snapshot (M3's first journey region).
pub const JOURNEY_REGION: BenchRegion = BenchRegion {
    name: "Alps",
    arg: RegionArg {
        lat_deg: 46.5,
        lon_deg: 10.0,
        years: 0.0,
    },
    pan_from_previous: false,
    flight_gated: true,
};
/// The globe's fly is over the region once within this arc of it (degrees): wider than a frame's
/// travel at the top of globe flight (the track's 30°/s cap is 1° a frame at 30 fps), so it never
/// overshoots, and well inside the region's own field of view from the dive altitude. Inside
/// it the fly descends.
pub const ARRIVE_DEG: f64 = 2.0;
/// Inside [`ARRIVE_DEG`] the fly keeps steering toward the region while it descends, until it
/// is within this arc (degrees): a route's hops have to reach what the route is chosen for (a
/// cube corner, a coast, a site), one or two hops from wherever the dive lands.
pub const SETTLE_DEG: f64 = 0.25;
/// The region fly's headings (yaw about `+y` from north, positive west).
pub const EAST_YAW: f32 = -std::f32::consts::FRAC_PI_2;
pub const WEST_YAW: f32 = std::f32::consts::FRAC_PI_2;
pub const NORTH_YAW: f32 = 0.0;
/// The hop a corner route checks (Mt Fuji's second) must centre the neighbour within this of a
/// cube-face corner
/// (km): the inner field's half-width, so the corner lies in or at the edge of the neighbour's
/// inner field, where the generator's lattice spans three cube faces.
pub const CORNER_REACH_KM: f64 = 150.0;
/// The hop a coast route checks (Mt Fuji's first) must give the neighbour's inner field a land
/// share in this range.
pub const COAST_LAND_SHARE: (f32, f32) = (0.05, 0.95);

/// What a route's hop is chosen to cross, checked at run time (gated).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Crossing {
    /// Land and sea: the neighbour's inner field has a land share within [`COAST_LAND_SHARE`].
    Coast,
    /// A cube-face corner of the terrain's cube sphere: the neighbour's centre within
    /// [`CORNER_REACH_KM`] of one ([`corner_km`]).
    Corner,
}

/// A landform a route's hop lands on, read from the neighbour's grid (reported only).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Site {
    /// The hop (from 1) whose neighbour holds it.
    pub hop: u32,
    pub name: &'static str,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub kind: SiteKind,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SiteKind {
    /// A north-south profile: the floor at the site and the highest ground on either side.
    Canyon,
    /// The height at the site against the highest ground near it and a ring around it.
    Peak,
}

/// One route of the journey bench: where it dives, the heading of its hops, and what its hops
/// are chosen to cross or land on (owner, 2026-11-25: geographic diversity, a cube corner, a
/// coastline, the Grand Canyon, the Andes and Mt Fuji).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct JourneyRoute {
    pub region: BenchRegion,
    pub yaw: f32,
    pub heading: &'static str,
    /// `(hop from 1, what it crosses)`, checked at run time (gated).
    pub crosses: &'static [(u32, Crossing)],
    pub site: Option<Site>,
    /// Whether its flight frames gate the run. Pangaea's are reported only (owner,
    /// 2026-11-25): its sand seas run near the interval at 2560 x 1440 and fail the uncapped
    /// rule whenever the display doesn't pace the run.
    pub flight_gated: bool,
}

/// The Alps east: M3's first journey route, now only the headless tests' (the tiny fixture).
pub const ALPS_ROUTE: JourneyRoute = JourneyRoute {
    region: JOURNEY_REGION,
    yaw: EAST_YAW,
    heading: "east",
    crosses: &[],
    site: None,
    flight_gated: true,
};

/// The routes, in the order a run flies them ([`JOURNEY_ORDER`]):
/// - **Andes** north from Aconcagua (32.65° S, 70.01° W; 6,040 m in the 1′ data), present day:
///   both hops stay over the range (35% and 43% of their inner fields above 3,000 m in the 1′
///   data, against 11% and 1% flying south).
/// - **Pangaea interior** north (a bench region): deep time, 250 Ma.
/// - **Grand Canyon** north from central Arizona, present day: the first hop's neighbour is
///   centred on the canyon at 36.1° N, 112.1° W (1′ data: floor about 1,030 m, rims 2,150 and
///   2,450 m).
/// - **Mt Fuji** west from the Pacific east of the Bōsō peninsula, present day: the first hop
///   flies from the sea over the Kantō coast onto Fuji (35.36° N, 138.73° E; 3,164 m in the 1′
///   data, the local high point), and the second lands 104–106 km from the cube corner at
///   35.26° N, 135° E (measured; render direction `(1, 1, −1) / √3`, with geo `(x, y, z)` = render
///   `(z, x, y)`), where the generator's lattice spans three cube faces.
pub const JOURNEY_ROUTES: [JourneyRoute; 4] = [
    JourneyRoute {
        region: BenchRegion {
            name: "Andes",
            arg: RegionArg {
                lat_deg: -32.65,
                lon_deg: -70.01,
                years: 0.0,
            },
            pan_from_previous: false,
            flight_gated: true,
        },
        yaw: NORTH_YAW,
        heading: "north",
        crosses: &[],
        site: None,
        flight_gated: true,
    },
    JourneyRoute {
        region: BENCH_REGIONS[0],
        yaw: NORTH_YAW,
        heading: "north",
        crosses: &[],
        site: None,
        flight_gated: false,
    },
    JourneyRoute {
        region: BenchRegion {
            name: "Grand Canyon",
            arg: RegionArg {
                lat_deg: 33.85,
                lon_deg: -112.1,
                years: 0.0,
            },
            pan_from_previous: false,
            flight_gated: true,
        },
        yaw: NORTH_YAW,
        heading: "north",
        crosses: &[],
        site: Some(Site {
            hop: 1,
            name: "Grand Canyon",
            lat_deg: 36.1,
            lon_deg: -112.1,
            kind: SiteKind::Canyon,
        }),
        flight_gated: true,
    },
    JourneyRoute {
        region: BenchRegion {
            name: "Mt Fuji",
            arg: RegionArg {
                lat_deg: 35.36,
                lon_deg: 141.48,
                years: 0.0,
            },
            pan_from_previous: false,
            flight_gated: true,
        },
        yaw: WEST_YAW,
        heading: "west",
        crosses: &[(1, Crossing::Coast), (2, Crossing::Corner)],
        site: Some(Site {
            hop: 1,
            name: "Mt Fuji",
            lat_deg: 35.36,
            lon_deg: 138.73,
            kind: SiteKind::Peak,
        }),
        flight_gated: true,
    },
];
/// The journeys of a run, by route: every route once, then the Andes again, so the memory
/// cycles compare the Andes with itself (about 7 minutes natively).
pub const JOURNEY_ORDER: [usize; 5] = [0, 1, 2, 3, 0];
/// Within this of the route's heading (degrees) the region's fly flies on: wider than a frame's
/// turn at 30 fps.
pub const YAW_TOLERANCE_DEG: f32 = 3.0;
/// The region's fly descends to this height above the ground before it flies out: under
/// `HOP_ALTITUDE_M`, so each hop's lift rises to the cloud bank and the fly returns here after
/// the swap, and from 12.5 km up `fly_speed` is at its 10 km/s cap, so the leg out is still the
/// fastest flight (the preload's worst lead).
pub const CRUISE_AGL_M: f32 = 20_000.0;
/// The turn keys rest within this of the wanted heading (degrees), so they never chatter.
const TURN_DEADBAND_DEG: f64 = 1.5;
/// Over the globe, `W` flies only while the heading is within this of the region's bearing
/// (degrees): a fly facing away turns first.
const FACE_DEG: f64 = 10.0;
/// Journeys of a single-route run (`--journey-bench --region=…`).
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
/// The most a hop may add to the in-region footprint natively (design §10, gated). The owner's
/// +64 MiB of 2026-11-25, over the design's +40: measured, a hop peaks at about +22 MiB and the
/// first hop of a run at up to +37.5 MiB (the swap's upload transient plus pages malloc has no
/// freed block of the right size for); a hop after the OS reclaimed the retained pages read
/// +48.0 MiB on the route set (`docs/testing/m3/performance.md`, "Hop memory" and "Journey
/// route set").
pub const HOP_MEMORY_LIMIT_MIB: u64 = 64;
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

/// A hold's summary line, `hold: <transition> <ms> ms (<tag>) -> <verdict>` (the tag names the
/// journey, [`JourneyBenchConfig::tag`]), and its gate: natively at most
/// [`HOLD_LIMIT_NATIVE_MS`]; on the web reported against [`HOLD_TARGET_WEB_MS`] (`None`).
pub fn hold_line(t: Transition, ms: f64, tag: &str, web: bool) -> (String, Option<bool>) {
    let head = format!("hold: {} {ms:.0} ms ({tag}) -> ", t.label());
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

/// One hop's memory samples, for [`hop_memory_gate`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HopMemory {
    /// The in-region baseline: the last flying frame before the hop.
    pub baseline: Option<u64>,
    /// The highest sample during the hop.
    pub peak: Option<u64>,
    /// The sample once back flying.
    pub end: Option<u64>,
    /// Whether the hop is on a repeat visit of its route ([`JourneyBenchConfig::is_repeat`]).
    pub repeat: bool,
}

/// The hop memory gate. Natively (`web` false) the worst `peak − baseline` of every hop must be
/// at most [`HOP_MEMORY_LIMIT_MIB`]. On the web the wasm linear memory only grows, so a region
/// larger than any before it (the Mt Fuji route's corner lattice, 35 MiB against the Andes'
/// 14–16) raises it for good: each route's first visit may grow it, and no hop of a repeat
/// visit may (`end − baseline` at most 0). `(pass, worst bytes over the baseline)` of the
/// judged hops, or `None` with a sample missing or nothing to judge.
pub fn hop_memory_gate(hops: &[HopMemory], web: bool) -> Option<(bool, i64)> {
    let over = |base: Option<u64>, top: Option<u64>| Some(top? as i64 - base? as i64);
    if web {
        let growth: Vec<i64> = hops
            .iter()
            .filter(|h| h.repeat)
            .map(|h| over(h.baseline, h.end))
            .collect::<Option<_>>()?;
        let worst = growth.into_iter().max()?;
        Some((worst <= 0, worst))
    } else {
        let worst = hops
            .iter()
            .map(|h| over(h.baseline, h.peak))
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
/// fly forward while facing it, and once within [`ARRIVE_DEG`] only descend (`Q`, a descend key on every platform) to the dive.
pub fn globe_keys(flight: &GlobeFlight, target: DVec3) -> Vec<KeyCode> {
    let up = flight.dir.normalize();
    let target = target.normalize();
    let arc = up.dot(target).clamp(-1.0, 1.0).acos();
    if arc <= SETTLE_DEG.to_radians() {
        return vec![KeyCode::KeyQ];
    }
    let toward = (target - up * up.dot(target)).normalize_or_zero();
    if toward == DVec3::ZERO {
        // The antipode: every way leads there.
        return vec![KeyCode::KeyW];
    }
    // Arrived: descend, still steering in until within `SETTLE_DEG`.
    let arrived = arc <= ARRIVE_DEG.to_radians();
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
    if arrived {
        keys.push(KeyCode::KeyQ);
    }
    keys
}

/// The keys flying the region's fly, heading `yaw`, `agl_m` above the ground, in `phase`, on a
/// route heading `want` (a yaw):
/// - flying, not yet `cruising` and above [`CRUISE_AGL_M`]: descend (`Q`) in place, turning
///   toward `want` meanwhile;
/// - flying otherwise: turn to face `want` and fly forward within [`YAW_TOLERANCE_DEG`] of it
///   (once cruising, a valley that puts the fly higher above the ground does not stop it);
/// - through a hop, `W` alone (a turn could point the wish back in, which ends a hop from its
///   hold);
/// - nothing while navigation moves the fly.
///
/// The shell sets `cruising` once the fly has been at or under [`CRUISE_AGL_M`].
pub fn region_keys(
    phase: &NavPhase,
    yaw: f32,
    agl_m: f32,
    cruising: bool,
    want: f32,
) -> Vec<KeyCode> {
    match phase {
        NavPhase::Hopping { .. } => vec![KeyCode::KeyW],
        NavPhase::Flying => {
            let tau = std::f32::consts::TAU;
            let err = (want - yaw + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
            let mut keys = Vec::new();
            if f64::from(err.abs()) > TURN_DEADBAND_DEG.to_radians() {
                // The right arrow lowers the yaw (positive is west of north).
                keys.push(if err > 0.0 {
                    KeyCode::ArrowLeft
                } else {
                    KeyCode::ArrowRight
                });
            }
            if !cruising && agl_m > CRUISE_AGL_M {
                keys.push(KeyCode::KeyQ);
            } else if err.abs() <= YAW_TOLERANCE_DEG.to_radians() {
                keys.push(KeyCode::KeyW);
            }
            keys
        }
        _ => Vec::new(),
    }
}

/// The bench's settings. Tests cut the journeys.
#[derive(Clone, Debug, PartialEq)]
pub struct JourneyBenchConfig {
    pub routes: Vec<JourneyRoute>,
    /// The journeys in order, as indices into `routes`.
    pub journeys: Vec<usize>,
    pub target_fps: u32,
    pub exit_when_done: bool,
    /// `--keep-going`: fly every journey even after a gate has failed.
    pub keep_going: bool,
}

impl JourneyBenchConfig {
    /// The full route set ([`JOURNEY_ROUTES`] in [`JOURNEY_ORDER`]).
    pub fn new(target_fps: u32, exit_when_done: bool) -> Self {
        JourneyBenchConfig {
            routes: JOURNEY_ROUTES.to_vec(),
            journeys: JOURNEY_ORDER.to_vec(),
            target_fps,
            exit_when_done,
            keep_going: false,
        }
    }

    /// One route, [`JOURNEY_CYCLES`] times.
    pub fn single(target_fps: u32, exit_when_done: bool, route: JourneyRoute) -> Self {
        JourneyBenchConfig {
            routes: vec![route],
            journeys: vec![0; JOURNEY_CYCLES as usize],
            target_fps,
            exit_when_done,
            keep_going: false,
        }
    }

    /// The bench over the region at `arg` (`--journey-bench --region=…`): one route east.
    pub fn chosen(target_fps: u32, exit_when_done: bool, arg: RegionArg) -> Self {
        JourneyBenchConfig::single(
            target_fps,
            exit_when_done,
            JourneyRoute {
                region: BenchRegion {
                    name: CHOSEN_REGION,
                    arg,
                    pan_from_previous: false,
                    flight_gated: true,
                },
                yaw: EAST_YAW,
                heading: "east",
                crosses: &[],
                site: None,
                flight_gated: true,
            },
        )
    }

    /// Journey `cycle`'s route.
    pub fn route(&self, cycle: u32) -> &JourneyRoute {
        &self.routes[self.journeys[cycle as usize]]
    }

    /// How a summary line names journey `cycle`: `cycle 2`, or with several routes
    /// `cycle 6, Alps`.
    pub fn tag(&self, cycle: u32) -> String {
        if self.routes.len() == 1 {
            format!("cycle {}", cycle + 1)
        } else {
            format!("cycle {}, {}", cycle + 1, self.route(cycle).region.name)
        }
    }

    /// The stops' names, one per journey: `journey 2`, or with several routes
    /// `journey 2 (Mt Fuji)`.
    pub fn stop_names(&self) -> Vec<String> {
        (0..self.journeys.len())
            .map(|j| {
                if self.routes.len() == 1 {
                    format!("journey {}", j + 1)
                } else {
                    format!("journey {} ({})", j + 1, self.route(j as u32).region.name)
                }
            })
            .collect()
    }

    /// Whether journey `cycle` visits a route flown earlier in the run.
    pub fn is_repeat(&self, cycle: u32) -> bool {
        let route = self.journeys[cycle as usize];
        self.journeys[..cycle as usize].contains(&route)
    }

    /// The journey memory cycles' verdicts, `(route, visits, (pass, increase))`, for each route
    /// flown more than once with every sample it needs. Natively each route's samples on the
    /// globe ([`Self::by_route`]) go to [`memory_cycles_pass`]: its repeats against its first
    /// visit. On the web the linear memory never shrinks, so after a larger route a repeat
    /// starts higher than its first visit did; like with like is what the repeats themselves
    /// added: each repeat visit's growth over the sample before it (the journey just before),
    /// summed, against the memory before the route's first repeat, within
    /// [`CYCLE_MEMORY_TOLERANCE`].
    pub fn memory_cycle_verdicts(
        &self,
        on_globe: &[Option<u64>],
        web: bool,
    ) -> Vec<(usize, usize, (bool, f64))> {
        if !web {
            return self
                .by_route(on_globe)
                .into_iter()
                .filter_map(|(r, samples)| {
                    memory_cycles_pass(&samples).map(|result| (r, samples.len(), result))
                })
                .collect();
        }
        (0..self.routes.len())
            .filter_map(|r| {
                let visits: Vec<usize> = (0..on_globe.len().min(self.journeys.len()))
                    .filter(|&j| self.journeys[j] == r)
                    .collect();
                let &first_repeat = visits.get(1)?;
                let base = on_globe[first_repeat - 1]? as f64;
                let mut grown = 0.0;
                for &j in &visits[1..] {
                    grown += on_globe[j]? as f64 - on_globe[j - 1]? as f64;
                }
                let increase = grown / base.max(1.0);
                Some((
                    r,
                    visits.len(),
                    (increase <= CYCLE_MEMORY_TOLERANCE, increase),
                ))
            })
            .collect()
    }

    /// The memory samples on the globe after each journey, by route (in route order), each in
    /// the order the route was flown: a route is compared only with itself.
    pub fn by_route<T: Copy>(&self, per_journey: &[T]) -> Vec<(usize, Vec<T>)> {
        (0..self.routes.len())
            .map(|r| {
                let samples = per_journey
                    .iter()
                    .zip(&self.journeys)
                    .filter(|&(_, &route)| route == r)
                    .map(|(&s, _)| s)
                    .collect();
                (r, samples)
            })
            .collect()
    }
}

/// The great-circle distance (km) from an era-frame direction to the nearest corner of the
/// terrain's cube sphere (the render frame's `(±1, ±1, ±1) / √3`, `crate::cube`'s face planes).
pub fn corner_km(dir: RenderDir) -> f64 {
    let d = dir.0.normalize();
    let cos = (d.x.abs() + d.y.abs() + d.z.abs()) / 3f64.sqrt();
    cos.clamp(-1.0, 1.0).acos() * EARTH_RADIUS_M / 1000.0
}

/// The share of a grid's inner field above sea level.
pub fn land_share(grid: &RegionGrid) -> f32 {
    let h = &grid.inner.elevation_m;
    h.iter().filter(|&&e| e > 0.0).count() as f32 / h.len().max(1) as f32
}

/// A route's check of one of its hops (the hop [`JourneyRoute::crosses`] names): `(pass, text)`.
/// A hop not measured fails.
pub fn crossing_check(c: Crossing, hop: &HopRecord) -> (bool, String) {
    match c {
        Crossing::Corner => match hop.centre {
            Some(centre) => {
                let km = corner_km(centre);
                (
                    km <= CORNER_REACH_KM,
                    format!(
                        "cube corner {km:.0} km from the neighbour's centre (<= {CORNER_REACH_KM:.0} km)"
                    ),
                )
            }
            None => (false, "cube corner: not measured".to_string()),
        },
        Crossing::Coast => match hop.land_share {
            Some(share) => {
                let (lo, hi) = COAST_LAND_SHARE;
                (
                    (lo..=hi).contains(&share),
                    format!(
                        "coast: land {:.0}% of the neighbour's inner field ({:.0}-{:.0}%)",
                        share * 100.0,
                        lo * 100.0,
                        hi * 100.0
                    ),
                )
            }
            None => (false, "coast: not measured".to_string()),
        },
    }
}

/// One route's flight outside the cloud, judged.
#[derive(Clone, Debug, PartialEq)]
pub struct RouteFlight {
    pub route: usize,
    pub frames: Vec<f32>,
    pub pacing: Pacing,
    pub pass: bool,
    /// Whether the verdict gates the run ([`JourneyRoute::flight_gated`]).
    pub gated: bool,
}

/// Judges each route's flight frames (`flight_ms`, the journey of each in `flight_cycle`) at
/// `fps`, all under one pacing for the run: the one `classify` finds for every flight frame
/// pooled. A heavy route whose own median sits near the interval in an uncapped run is judged
/// by the uncapped rule; classified alone, it would flip itself into the lenient paced rule.
pub fn judge_flight_by_route(
    config: &JourneyBenchConfig,
    flight_ms: &[f32],
    flight_cycle: &[u32],
    fps: u32,
    classify: impl Fn(&[f32]) -> Pacing,
) -> Vec<RouteFlight> {
    let pacing = classify(flight_ms);
    config
        .routes
        .iter()
        .enumerate()
        .map(|(r, route)| {
            let frames: Vec<f32> = flight_ms
                .iter()
                .zip(flight_cycle)
                .filter(|&(_, &c)| config.journeys[c as usize] == r)
                .map(|(&ms, _)| ms)
                .collect();
            RouteFlight {
                route: r,
                pass: passes(&frames, fps, pacing),
                frames,
                pacing,
                gated: route.flight_gated,
            }
        })
        .collect()
}

/// The flight gate over the routes' verdicts: every gated route passes (an ungated route's
/// verdict is reported only).
pub fn flight_by_route_passes(verdicts: &[RouteFlight]) -> bool {
    verdicts.iter().all(|v| v.pass || !v.gated)
}

/// A site read from the grid it lies in (`None` outside the inner field): the text of the
/// summary's site line.
pub fn read_site(site: &Site, grid: &RegionGrid, frame: &RegionFrame) -> Option<String> {
    let dir = geo_to_render(latlon_to_geo(site.lat_deg, site.lon_deg));
    let (x, z) = frame.dir_to_local(dir)?;
    let half = grid.inner.size_m / 2.0;
    if x.abs() > half || z.abs() > half {
        return None;
    }
    let h = |dx: f64, dz: f64| blended_height_m(grid, x + dx, z + dz);
    let at = h(0.0, 0.0);
    Some(match site.kind {
        SiteKind::Canyon => {
            // North-south through the site, 500 m steps: the floor within 3 km, the highest
            // ground 3-15 km to either side (`-z` is north).
            let steps = |from: i32, to: i32| (from..=to).map(|k| h(0.0, f64::from(k) * 500.0));
            let floor = steps(-6, 6).fold(f32::INFINITY, f32::min);
            let north = steps(-30, -6).fold(f32::NEG_INFINITY, f32::max);
            let south = steps(6, 30).fold(f32::NEG_INFINITY, f32::max);
            format!(
                "floor {floor:.0} m (at the site {at:.0} m), rims {north:.0} m north and {south:.0} m south within 15 km: {:.0} m deep",
                north.min(south) - floor
            )
        }
        SiteKind::Peak => {
            let ring = |r: f64| -> Vec<f32> {
                (0..36)
                    .map(|k| {
                        let a = f64::from(k).to_radians() * 10.0;
                        h(r * a.sin(), r * a.cos())
                    })
                    .collect()
            };
            let near = (1..=20)
                .flat_map(|r| ring(f64::from(r) * 500.0))
                .fold(at, f32::max);
            let around = ring(20_000.0);
            let mean = around.iter().sum::<f32>() / around.len() as f32;
            format!(
                "{at:.0} m at the site; the highest within 10 km {near:.0} m; the 20 km ring {mean:.0} m on average"
            )
        }
    })
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
    /// In the region, `hops` hops done: flying out along the route's heading and hopping.
    Region {
        cycle: u32,
        hops: u32,
        since: f64,
        /// Down at [`CRUISE_AGL_M`] once: from then on it flies out, whatever the ground does.
        cruising: bool,
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

/// One hop: its lift, and its memory (the in-region baseline before it, its peak, and on return
/// to flying).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HopRecord {
    /// The journey, from 0.
    pub cycle: u32,
    /// The hop's number in its journey, from 1.
    pub hop: u32,
    /// The height above the ground the lift started from (`lift_from_agl_m`), once it began.
    pub from_agl_m: Option<f32>,
    /// The fly's highest height above sea level during the hop.
    pub peak_height_m: Option<f32>,
    /// The neighbour's centre (era frame), from the hop's phase.
    pub centre: Option<RenderDir>,
    /// The share of the region's inner field above sea level once the hop is done (the
    /// neighbour's, after a swap).
    pub land_share: Option<f32>,
    /// The route's site read from the region once the hop is done ([`read_site`]).
    pub site: Option<String>,
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
    /// The journey of each of `flight_ms`.
    flight_cycle: Vec<u32>,
    /// GPU frame times read on flight frames, and the journey of each.
    gpu_ms: Vec<f32>,
    gpu_cycle: Vec<u32>,
    /// How frames were timed on the GPU, as last seen.
    gpu: Option<GpuFrameTime>,
    /// The continental passes that finished during the run, in order.
    passes: Vec<HydroPassFinished>,
    holds: Vec<HoldRecord>,
    swaps: Vec<SwapRecord>,
    hops: Vec<HopRecord>,
    /// The memory sampled on the last frame flown in a region.
    in_region: Option<u64>,
    /// The memory on the globe after each journey.
    on_globe: Vec<Option<u64>>,
    /// The keys the bench holds down.
    keys_held: Vec<KeyCode>,
    summary: Option<String>,
    passed: Option<bool>,
    /// Where the run stopped at its first failed gate, if it did.
    stopped: Option<EarlyStop>,
}

impl JourneyBench {
    pub fn new(config: JourneyBenchConfig) -> Self {
        JourneyBench {
            config,
            stage: Stage::WaitingForGlobe,
            prev_phase: NavPhase::Globe,
            flight_ms: Vec::new(),
            flight_cycle: Vec::new(),
            gpu_ms: Vec::new(),
            gpu_cycle: Vec::new(),
            gpu: None,
            passes: Vec::new(),
            holds: Vec::new(),
            swaps: Vec::new(),
            hops: Vec::new(),
            in_region: None,
            on_globe: Vec::new(),
            keys_held: Vec::new(),
            summary: None,
            passed: None,
            stopped: None,
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

    /// The hops so far, in order.
    pub fn hops(&self) -> &[HopRecord] {
        &self.hops
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
            // Read whether or not the pass and the GPU timer run in this app.
            .add_message::<HydroPassFinished>()
            .add_message::<GpuFrameMeasured>()
            .add_systems(
                Update,
                (
                    // Keys first, so navigation and both flies read them this frame.
                    drive_journey_keys.before(NavigationSystems),
                    measure_journey.after(NavigationSystems),
                ),
            );
        trace::add_trace(app);
    }
}

/// Holds the script's keys down through `ButtonInput<KeyCode>`, before navigation reads them.
fn drive_journey_keys(
    mut bench: ResMut<JourneyBench>,
    nav: Option<Res<Navigation>>,
    flight: Option<Res<GlobeFlightState>>,
    fly: Option<Res<FlyState>>,
    region: Option<Res<ActiveRegion>>,
    keys: Option<ResMut<ButtonInput<KeyCode>>>,
) {
    let Some(mut keys) = keys else {
        return;
    };
    let phase = nav.map_or(NavPhase::Globe, |n| n.phase);
    let want: Vec<KeyCode> = match bench.stage {
        Stage::Approach { .. } if phase == NavPhase::Globe => flight.map_or(Vec::new(), |f| {
            globe_keys(
                &f.0,
                region_dir(&bench.config.route(cycle_of(bench.stage)).region.arg),
            )
        }),
        // Tapped: pressed on one frame, let go on the next, until the climb takes.
        Stage::Region { hops, .. } if hops >= HOPS_PER_CYCLE && phase == NavPhase::Flying => {
            if bench.keys_held.contains(&LEAVE_KEY) {
                Vec::new()
            } else {
                vec![LEAVE_KEY]
            }
        }
        Stage::Region {
            cycle,
            hops,
            since,
            cruising,
        } => fly.map_or(Vec::new(), |f| {
            let ground = region.as_deref().map_or(0.0, |r| {
                blended_surface_m(&r.grid, f64::from(f.pos_m.x), f64::from(f.pos_m.z))
            });
            let agl_m = f.pos_m.y - ground;
            // Down at the cruise once (while flying): cruising from then on.
            let cruising = cruising || (phase == NavPhase::Flying && agl_m <= CRUISE_AGL_M);
            bench.stage = Stage::Region {
                cycle,
                hops,
                since,
                cruising,
            };
            region_keys(
                &phase,
                f.yaw,
                agl_m,
                cruising,
                bench.config.route(cycle).yaw,
            )
        }),
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
    fly: Option<Res<'w, FlyState>>,
    region: Option<Res<'w, ActiveRegion>>,
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
    mut passes: MessageReader<HydroPassFinished>,
    mut gpu_frames: MessageReader<GpuFrameMeasured>,
    gpu: Option<Res<GpuFrameTime>>,
) {
    let bench = &mut *bench;
    bench.passes.extend(passes.read().copied());
    bench.gpu = gpu.as_deref().cloned();
    let gpu_ms: Vec<f32> = gpu_frames.read().map(|m| m.ms).collect();
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
    if let Some(cycle) = bench.stage.cycle()
        && density == 0.0
    {
        bench.gpu_cycle.extend(gpu_ms.iter().map(|_| cycle));
        bench.gpu_ms.extend(gpu_ms);
    }
    if let Some(cycle) = bench.stage.cycle() {
        let frame = Frame {
            before,
            phase,
            density,
            frame_ms,
            now,
            swapped,
            height_m: game.fly.as_ref().map_or(0.0, |f| f.pos_m.y),
        };
        record_frame(bench, cycle, frame);
        // A hop just done: read the region it left the fly in.
        let route = *bench.config.route(cycle);
        if phase == NavPhase::Flying
            && let Some(hop) = bench.hops.last_mut()
            && hop.cycle == cycle
            && !hop.open
            && hop.land_share.is_none()
            && let Some(region) = game.region.as_deref()
        {
            hop.land_share = Some(land_share(&region.grid));
            hop.site = route
                .site
                .filter(|site| site.hop == hop.hop)
                .and_then(|site| read_site(&site, &region.grid, &region.frame));
        }
        if !bench.config.keep_going
            && let Some(gate) = failed_journey_gate(bench, cfg!(target_arch = "wasm32"))
        {
            warn!(
                cycle = cycle + 1,
                gate, "journey bench: a gate failed, stopping early"
            );
            bench.stopped = Some(EarlyStop {
                stops_run: cycle as usize + 1,
                gate,
            });
            finish(bench, &game, &display, &mut controls, None);
            return;
        }
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
                start_journey(bench, 0, now, &mut controls);
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
                    heading = bench.config.route(cycle).heading,
                    "journey bench: in the region, flying out"
                );
                bench.stage = Stage::Region {
                    cycle,
                    hops: 0,
                    since: now,
                    cruising: false,
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
        Stage::Region {
            cycle,
            hops,
            since,
            cruising,
        } => {
            let hops = hops + u32::from(swapped);
            if swapped {
                info!(cycle = cycle + 1, hop = hops, "journey bench: hopped");
            }
            bench.stage = Stage::Region {
                cycle,
                hops,
                since,
                cruising,
            };
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
                if (cycle as usize) + 1 < bench.config.journeys.len() {
                    start_journey(bench, cycle + 1, now, &mut controls);
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

/// Starts journey `cycle` on the globe: the time goes to its route's age, then the fly heads
/// for the route's region.
fn start_journey(bench: &mut JourneyBench, cycle: u32, now: f64, controls: &mut Controls) {
    let region = bench.config.route(cycle).region;
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
    info!(
        cycle = cycle + 1,
        region = region.name,
        "journey bench: flying the globe to the dive"
    );
    bench.stage = Stage::Approach { cycle, since: now };
}

/// The journey a stage belongs to, or the first.
fn cycle_of(stage: Stage) -> u32 {
    stage.cycle().unwrap_or(0)
}

/// One frame's readings, after navigation.
#[derive(Copy, Clone, Debug)]
struct Frame {
    /// Navigation's phase last frame and this one.
    before: NavPhase,
    phase: NavPhase,
    density: f32,
    frame_ms: f32,
    now: f64,
    swapped: bool,
    /// The region fly's height above sea level.
    height_m: f32,
}

/// One frame of the script: its flight time, holds, swaps, hops and memory.
fn record_frame(bench: &mut JourneyBench, cycle: u32, frame: Frame) {
    let Frame {
        before,
        phase,
        density,
        frame_ms,
        now,
        swapped,
        height_m,
    } = frame;
    if density == 0.0 {
        bench.flight_ms.push(frame_ms);
        bench.flight_cycle.push(cycle);
    }
    // Transitions start on the phase change into them.
    let hops_done = match bench.stage {
        Stage::Region { hops, .. } => hops,
        _ => 0,
    };
    // Sampled only where the gates read it: flying in a region and through a hop.
    let memory = matches!(phase, NavPhase::Flying | NavPhase::Hopping { .. })
        .then(cycle_memory)
        .flatten();
    let entered = std::mem::discriminant(&phase) != std::mem::discriminant(&before);
    match phase {
        NavPhase::Diving { .. } if entered => bench.start_hold(Transition::Dive, cycle),
        NavPhase::Hopping { centre, .. } if entered => {
            let hop = hops_done + 1;
            bench.start_hold(Transition::Hop(hop), cycle);
            match bench.hops.last_mut() {
                // Started again after turning back before its lift: the same hop, keyed as its
                // hold is, its baseline kept.
                Some(last) if last.cycle == cycle && last.hop == hop => last.open = true,
                _ => bench.hops.push(HopRecord {
                    cycle,
                    hop,
                    centre: Some(centre),
                    baseline: bench.in_region,
                    peak: memory,
                    open: true,
                    ..default()
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
        NavPhase::Hopping {
            lift,
            lift_from_agl_m,
            swapped,
            ..
        } => {
            if let Some(hop) = bench.hops.last_mut().filter(|h| h.open) {
                hop.peak = super::max_opt(hop.peak, memory);
                hop.peak_height_m = Some(hop.peak_height_m.map_or(height_m, |p| p.max(height_m)));
                // Frozen once the lift begins; the return after the swap may move it.
                if !swapped && lift > 0.0 {
                    hop.from_agl_m = Some(lift_from_agl_m);
                }
            }
        }
        _ => {
            if let Some(hop) = bench.hops.last_mut().filter(|h| h.open) {
                hop.open = false;
            }
        }
    }
}

/// The first gate decided within a journey that has failed, as the summary's lines name it
/// (`hold: hop 1 3500 ms (cycle 1) -> FAIL`): a hold over its native gate once it has ended, a
/// swap frame over its native gate once its window has closed, a hop's memory over its gate
/// ([`hop_memory_gate`] on that hop alone) once the hop is over, or a route check once its hop
/// is over and the region it left was read. Each is final when it fails, so the run has failed.
/// Holds and swaps are reported only on the web (`web`), so never stop a run there.
fn failed_journey_gate(bench: &JourneyBench, web: bool) -> Option<String> {
    let config = &bench.config;
    if !web {
        for hold in &bench.holds {
            if let Some(ms) = hold.clock.hold_ms()
                && ms > HOLD_LIMIT_NATIVE_MS
            {
                return Some(hold_line(hold.transition, ms, &config.tag(hold.cycle), web).0);
            }
        }
        for s in &bench.swaps {
            if s.frames_left == 0 && s.worst_ms > SWAP_FRAME_LIMIT_MS {
                return Some(format!(
                    "swap worst frame: {} {:.2} ms ({}) -> FAIL",
                    s.transition.label(),
                    s.worst_ms,
                    config.tag(s.cycle)
                ));
            }
        }
    }
    for h in bench.hops.iter().filter(|h| !h.open) {
        let sample = HopMemory {
            baseline: h.baseline,
            peak: h.peak,
            end: h.end,
            repeat: config.is_repeat(h.cycle),
        };
        if let Some((false, worst)) = hop_memory_gate(&[sample], web) {
            return Some(format!(
                "hop memory peak: hop {} {} ({}) -> FAIL",
                h.hop,
                signed_mib(worst),
                config.tag(h.cycle)
            ));
        }
        if h.land_share.is_none() {
            continue;
        }
        for &(hop, c) in config.route(h.cycle).crosses {
            if hop != h.hop {
                continue;
            }
            let (ok, text) = crossing_check(c, h);
            if !ok {
                return Some(format!(
                    "route check: hop {hop} ({}): {text} FAIL",
                    config.tag(h.cycle)
                ));
            }
        }
    }
    None
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
    let config = &bench.config;
    let cycles = config.journeys.len();
    let place = |r: &JourneyRoute| {
        format!(
            "{} ({}, {} at {} years)",
            r.region.name, r.region.arg.lat_deg, r.region.arg.lon_deg, r.region.arg.years
        )
    };
    if let [route] = config.routes.as_slice() {
        out.push(format!(
            "region: {}, {cycles} cycle{} of a dive, {HOPS_PER_CYCLE} hops and a climb",
            place(route),
            if cycles == 1 { "" } else { "s" }
        ));
    } else {
        let routes: Vec<String> = config
            .routes
            .iter()
            .map(|r| format!("{} {}", place(r), r.heading))
            .collect();
        let order: Vec<&str> = config
            .journeys
            .iter()
            .map(|&i| config.routes[i].region.name)
            .collect();
        out.push(format!(
            "routes: {}; {cycles} journeys of a dive, {HOPS_PER_CYCLE} hops and a climb: {}",
            routes.join(", "),
            order.join(", ")
        ));
    }
    out.push(on_failure_line(config.keep_going));
    let mut pass = failure.is_none() && bench.stopped.is_none();

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
                let (line, gate) = hold_line(hold.transition, ms, &config.tag(hold.cycle), web);
                pass &= gate.unwrap_or(true);
                out.push(line);
            }
            None => {
                pass = false;
                out.push(format!(
                    "hold: {} not measured ({}) -> FAIL",
                    hold.transition.label(),
                    config.tag(hold.cycle)
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
    // With several routes each is judged on its own frames (as the region bench judges each
    // region, R14), all under the run's one pacing (the pooled frames'): routes render very
    // differently (Pangaea's dunes run near the interval at 2560 x 1440, Fuji's sea far under
    // it), so pooled frames would mix them. Pangaea's verdict is reported only (owner,
    // 2026-11-25).
    let several = config.routes.len() > 1;
    let pooled_pass = passes(flight, fps, pacing);
    let mut route_lines = Vec::new();
    let flight_pass;
    if several {
        let verdicts = judge_flight_by_route(
            config,
            &bench.flight_ms,
            &bench.flight_cycle,
            fps,
            |frames| pacing_for(&info, frames),
        );
        flight_pass = flight_by_route_passes(&verdicts);
        for v in &verdicts {
            let rs = frame_time_stats(&v.frames);
            route_lines.push(format!(
                "  flight, {}: frames {}, median {:.2} ms, p95 {:.2} ms, worst {:.2} ms, missed (>{:.1} ms) {} -> {} rule -> {}",
                config.routes[v.route].region.name,
                rs.frames,
                rs.median_ms,
                rs.p95_ms,
                rs.worst_ms,
                MISSED_INTERVALS * interval,
                missed_frames(&v.frames, fps),
                pacing_name(v.pacing),
                if v.gated {
                    gate_word(v.pass).to_string()
                } else {
                    format!("{} (reported only)", gate_word(v.pass))
                }
            ));
        }
    } else {
        flight_pass = pooled_pass;
    }
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
        if several {
            format!(
                "{} pooled; judged per route below",
                if pooled_pass { "pass" } else { "over" }
            )
        } else {
            gate_word(flight_pass).to_string()
        }
    ));
    out.push(format!(
        "  missed frames (>{:.1} ms = {MISSED_INTERVALS}x interval): {missed} ({share:.2}%)",
        MISSED_INTERVALS * interval,
    ));
    out.push(host_stall_line("flight", flight));
    if several {
        out.extend(route_lines);
        out.push(format!(
            "flight by route (the run's {} rule for every route; ungated routes reported only) -> {}",
            pacing_name(pacing),
            gate_word(flight_pass)
        ));
    }
    // GPU frame times on the flight frames (reported only), per route.
    let gpu_on = gpu_available(bench.gpu.as_ref());
    for (r, route) in config.routes.iter().enumerate() {
        let frames: Vec<f32> = bench
            .gpu_ms
            .iter()
            .zip(&bench.gpu_cycle)
            .filter(|&(_, &c)| config.journeys.get(c as usize) == Some(&r))
            .map(|(&ms, _)| ms)
            .collect();
        let line = gpu_frame_line(route.region.name, &frames, gpu_on);
        if !gpu_on {
            out.push(line);
            break;
        }
        out.push(if several { format!("  {line}") } else { line });
    }

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
                    "{} {:.2} ms ({})",
                    s.transition.label(),
                    s.worst_ms,
                    config.tag(s.cycle)
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

    // Hop lift (reported only): the lift's start and the hop's highest height.
    let lifts: Vec<String> = bench
        .hops
        .iter()
        .map(|h| {
            let km =
                |m: Option<f32>| m.map_or("n/a".to_string(), |m| format!("{:.1} km", m / 1000.0));
            format!(
                "hop {} {} -> {} ({})",
                h.hop,
                km(h.from_agl_m),
                km(h.peak_height_m),
                config.tag(h.cycle)
            )
        })
        .collect();
    out.push(format!(
        "hop lift (reported only; from the height above the ground before the lift to the hop's highest height above sea level, the lift rising to {:.0} km): {}",
        HOP_ALTITUDE_M / 1000.0,
        if lifts.is_empty() {
            "no hop".to_string()
        } else {
            lifts.join(", ")
        }
    ));

    // Hop memory.
    let samples: Vec<HopMemory> = bench
        .hops
        .iter()
        .map(|h| HopMemory {
            baseline: h.baseline,
            peak: h.peak,
            end: h.end,
            repeat: config.is_repeat(h.cycle),
        })
        .collect();
    let hop_scope = if web {
        "wasm linear memory, which grows but never shrinks; gate: no growth in any hop of a route's repeat visit (a route's first visit may grow it)".to_string()
    } else {
        format!(
            "{}; gate: peak <= in-region baseline before the hop +{HOP_MEMORY_LIMIT_MIB} MiB",
            cycle_memory_kind()
        )
    };
    let each: Vec<String> = bench
        .hops
        .iter()
        .map(|h| {
            let top = if web { h.end } else { h.peak };
            let over = match (h.baseline, top) {
                (Some(b), Some(t)) => signed_mib(t as i64 - b as i64),
                _ => "n/a".to_string(),
            };
            format!("hop {} {over} ({})", h.hop, config.tag(h.cycle))
        })
        .collect();
    match hop_memory_gate(&samples, web) {
        Some((ok, worst)) => {
            pass &= ok;
            out.push(format!(
                "hop memory peak ({hop_scope}): worst {} over the baseline; {} -> {}",
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

    // Route checks (gated): each route's hops cross what the route is chosen for.
    let mut checks = Vec::new();
    let mut checks_pass = true;
    for h in &bench.hops {
        for &(hop, c) in config.route(h.cycle).crosses {
            if hop != h.hop {
                continue;
            }
            let (ok, text) = crossing_check(c, h);
            checks_pass &= ok;
            checks.push(format!(
                "hop {hop} ({}): {text} {}",
                config.tag(h.cycle),
                gate_word(ok)
            ));
        }
    }
    let wanted_checks: usize = config
        .journeys
        .iter()
        .map(|&r| config.routes[r].crosses.len())
        .sum();
    let wanted = config
        .journeys
        .iter()
        .filter(|&&r| !config.routes[r].crosses.is_empty())
        .count();
    if wanted > 0 {
        // Every check measured: a hop that never happened fails its route's check.
        let ok = checks_pass && checks.len() == wanted_checks;
        pass &= ok;
        out.push(format!(
            "route checks (each route's hops cross what they are chosen for): {} -> {}",
            if checks.is_empty() {
                "none measured".to_string()
            } else {
                checks.join("; ")
            },
            gate_word(ok)
        ));
    }
    // Sites (reported only): the landform a route's hop lands on.
    let sites: Vec<String> = bench
        .hops
        .iter()
        .filter_map(|h| {
            let site = config.route(h.cycle).site.filter(|s| s.hop == h.hop)?;
            Some(format!(
                "{} ({:.2}, {:.2}) in hop {}'s region ({}): {}",
                site.name,
                site.lat_deg,
                site.lon_deg,
                site.hop,
                config.tag(h.cycle),
                h.site
                    .as_deref()
                    .unwrap_or("not read (outside the inner field?)")
            ))
        })
        .collect();
    if !sites.is_empty() {
        out.push(format!("sites (reported only): {}", sites.join("; ")));
    }

    // Journey memory cycles: each route against its own first journey.
    let mut listed = Vec::new();
    for (r, samples) in config.by_route(&bench.on_globe) {
        if samples.is_empty() {
            continue;
        }
        let each = samples
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{}: {}", i + 1, show_mib(*m)))
            .collect::<Vec<_>>()
            .join(", ");
        listed.push(if config.routes.len() == 1 {
            each
        } else {
            format!("{} {each}", config.routes[r].region.name)
        });
    }
    let judged: Vec<_> = config
        .memory_cycle_verdicts(&bench.on_globe, web)
        .into_iter()
        .map(|(r, n, result)| (config.routes[r].region.name, n, result))
        .collect();
    let listed = listed.join("; ");
    if judged.is_empty() {
        out.push(format!(
            "journey memory cycles: not measured ({} cycle{} done{})",
            bench.on_globe.len(),
            if bench.on_globe.len() == 1 { "" } else { "s" },
            if listed.is_empty() {
                String::new()
            } else {
                format!("; {listed}")
            }
        ));
    } else {
        let ok = judged.iter().all(|(_, _, (ok, _))| *ok);
        pass &= ok;
        let worst = judged
            .iter()
            .map(|(name, n, (_, increase))| {
                if web {
                    // What the repeats themselves added, against the memory before them.
                    let whose = if config.routes.len() == 1 {
                        String::new()
                    } else {
                        format!("{name}: ")
                    };
                    format!(
                        "{whose}visits 2-{n} grew {:+.1}% over the memory before them",
                        increase * 100.0
                    )
                } else if config.routes.len() == 1 {
                    format!(
                        "worst of cycles 2-{n} is {:+.1}% of cycle 1",
                        increase * 100.0
                    )
                } else {
                    format!(
                        "{name}: worst of visits 2-{n} is {:+.1}% of its first",
                        increase * 100.0
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "journey memory cycles ({}, on the globe after each climb{}): {listed}; {worst} (limit +{:.0}%) -> {}",
            cycle_memory_kind(),
            if web {
                "; each repeat visit's own growth, as the memory only grows"
            } else if config.routes.len() == 1 {
                ""
            } else {
                "; each route against its own first visit"
            },
            CYCLE_MEMORY_TOLERANCE * 100.0,
            gate_word(ok)
        ));
    }
    out.extend(pass_lines(&bench.passes));
    if gpu_on {
        out.push(gpu_status_line(bench.gpu.as_ref()));
    }
    if let Some(stop) = &bench.stopped {
        out.extend(stopped_early_lines(stop, &config.stop_names(), None));
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

pub mod trace;

#[cfg(test)]
mod tests;
