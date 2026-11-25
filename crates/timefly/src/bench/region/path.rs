//! The region bench's scripted flight: a pure function of the region's ground (where the coast
//! is) and its drawn surface (what the fly keeps clear of: the ground or the water over it), so
//! the same region always gives the same camera path.
//!
//! [`SCRIPT_SECS`] of flight in five segments of [`SEGMENT_SECS`] ([`Segment`]). Each pitch is
//! set by the chase look it gives ([`pitch_for_look_deg`]): the chase camera looks that far below
//! the horizontal, as M2's free camera looked at the same pitch, so flight views frame the
//! terrain as M2's did.
//!
//! 1. **Low pass** (0–15 s): from the region's start pose ([`FlyState::start`]) down to
//!    [`LOW_PASS_AGL_M`] over the first 3 s, then on toward the coast at 600 m/s, following the
//!    ground.
//! 2. **Climb** (15–30 s): on along the same heading, speeding up, to [`CLIMB_TOP_M`] above sea
//!    level, looking down more steeply.
//! 3. **Coast run** (30–45 s): down to the coast over 6 s, then 9 s along it ([`Coast`]). The
//!    coast is the nearest sea-land crossing on 36 rays from the centre, out to the apron's
//!    edge. When it lies in the apron, the run follows the field's edge toward it, higher up and
//!    looking at it; with no coast at all, it runs across the heading.
//! 4. **Apron sweep** (45–60 s): out to the field's edge in the coast's direction (or the
//!    heading's), climbing to [`SWEEP_AGL_M`] and panning ±50° across the apron's horizon.
//! 5. **High pass** (60–75 s, M3): up to [`HIGH_PASS_TOP_M`], near the region's cloud base,
//!    drifting back over the centre and looking further down, then turning half round, so the
//!    view crosses both the apron's fogged edge and the whole inner field.
//!
//! The path never leaves [`FIELD_LIMIT_M`] on either axis and never comes within [`MIN_AGL_M`]
//! of the ground under it, well above the fly camera's clearance. The limit keeps it out of the
//! hop's preload reach (M3 Task 9, ruling P5: the path is a test driver, so it was pulled in
//! from 0.92 of the half field to 0.5, and the edge segments are not strictly comparable with
//! M2's numbers): it never flies out within `PRELOAD_MIN_M` of the edge, nor within a
//! generation's flight at its speed on either platform, so the bench never hops.

use std::f32::consts::{PI, TAU};

use bevy::prelude::*;
use timefly_core::terrain::{blended_height_m, blended_surface_m};
use timefly_core::worldgen::generate::RegionGrid;

use crate::fly_camera::{EDGE_M, FlyState, START_PITCH_DEG, pitch_for_look_deg};
use crate::region::CAMERA_START_ALTITUDE_M;

/// Seconds of each [`Segment`].
pub const SEGMENT_SECS: f32 = 15.0;
/// Seconds of scripted flight; the bench may play it faster (its tests do).
pub const SCRIPT_SECS: f32 = 5.0 * SEGMENT_SECS;
/// The low pass's height above the ground.
pub const LOW_PASS_AGL_M: f32 = 300.0;
/// The top of the climb, above sea level.
pub const CLIMB_TOP_M: f32 = 30_000.0;
/// The coast run's height above the ground.
pub const COAST_AGL_M: f32 = 1_000.0;
/// The least height above sea level when the coast lies in the apron (seen from afar).
pub const COAST_VIEW_M: f32 = 3_000.0;
/// The apron sweep's height above the ground.
pub const SWEEP_AGL_M: f32 = 6_000.0;
/// The top of the high pass, above sea level: in the cloud base's band (within
/// `REGION_CLOUD_BAND_M` of `REGION_CLOUD_BASE_M`) and below the climb out (ruling P3:
/// `REGION_CLOUD_BASE_M − 1 km`), so the bench stays in the region.
pub const HIGH_PASS_TOP_M: f32 = 280_000.0;
/// The path's floor above the ground (the fly camera's own clearance is 150 m).
pub const MIN_AGL_M: f32 = 300.0;
/// The path stays within this of the centre on either axis: 75 km inside the edge. A fly within
/// `PRELOAD_MIN_M` of the edge, or a generation's flight at its speed, preloads a neighbour, and
/// on the web a generation is expected to take `EXPECTED_GEN_S_WEB` (8 s): at 0.6 of the half
/// field the coast run's transit (about 13 km/s) came within that reach (fix round 1).
pub const FIELD_LIMIT_M: f32 = 0.5 * EDGE_M;
/// A coast within this of the centre on both axes is run along; farther out it is viewed from
/// the field's edge.
pub const COAST_IN_FIELD_M: f32 = 0.45 * EDGE_M;
/// The low pass's speed.
const LOW_SPEED_MPS: f32 = 600.0;
/// The climb ends at this speed.
const CLIMB_END_SPEED_MPS: f32 = 4_000.0;
/// The length of the run along the coast.
const COAST_RUN_M: f32 = 13_500.0;
/// The apron sweep pans this far each side of its direction.
const SWEEP_HALF_ANGLE_DEG: f32 = 50.0;
/// The coast search: rays, step and reach (the apron's half-width, less a margin).
const COAST_RAYS: usize = 36;
const COAST_STEP_M: f32 = 2_000.0;
const COAST_REACH_M: f32 = 1_150_000.0;
/// The ground-following window: the highest ground on a 3 × 3 grid this far apart.
const FOLLOW_SPAN_M: f32 = 600.0;

/// The path's five segments, [`SEGMENT_SECS`] each.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Segment {
    LowPass,
    Climb,
    CoastRun,
    ApronSweep,
    HighPass,
}

impl Segment {
    pub const ALL: [Segment; 5] = [
        Segment::LowPass,
        Segment::Climb,
        Segment::CoastRun,
        Segment::ApronSweep,
        Segment::HighPass,
    ];

    /// The segment at `script_s` seconds into the path.
    pub fn at(script_s: f32) -> Segment {
        let q = SEGMENT_SECS;
        match script_s {
            t if t < q => Segment::LowPass,
            t if t < 2.0 * q => Segment::Climb,
            t if t < 3.0 * q => Segment::CoastRun,
            t if t < 4.0 * q => Segment::ApronSweep,
            _ => Segment::HighPass,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Segment::LowPass => "low pass",
            Segment::Climb => "climb",
            Segment::CoastRun => "coast run",
            Segment::ApronSweep => "apron sweep",
            Segment::HighPass => "high pass",
        }
    }
}

/// The coast the path runs along. Points are local metres `(x, z)` (`+x` east, `−z` north).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Coast {
    /// Within [`COAST_IN_FIELD_M`] on both axes: run along it.
    InField { point: Vec2, tangent: Vec2 },
    /// Farther out: viewed from the field's edge.
    InApron { point: Vec2, tangent: Vec2 },
    /// No sea-land crossing within [`COAST_REACH_M`].
    None,
}

impl Coast {
    /// One line for the bench's block.
    pub fn describe(&self) -> String {
        match self {
            Coast::InField { point, .. } => format!(
                "in the field, {:.1} km from the centre (run along it)",
                point.length() / 1000.0
            ),
            Coast::InApron { point, .. } => format!(
                "in the apron, {:.0} km from the centre (viewed from the field's edge)",
                point.length() / 1000.0
            ),
            Coast::None => format!(
                "none within {:.0} km (the run crosses the heading)",
                COAST_REACH_M / 1000.0
            ),
        }
    }
}

/// The planned path for one region.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BenchPath {
    /// The start pose's height ([`FlyState::start`]).
    pub start_y: f32,
    /// The low pass's and the climb's heading, a unit `(x, z)`.
    pub heading: Vec2,
    pub coast: Coast,
    pub run_start: Vec2,
    pub run_end: Vec2,
    /// The view's yaw along the run.
    pub run_yaw: f32,
    /// Whether the run keeps at least [`COAST_VIEW_M`] above sea level.
    pub run_high: bool,
    pub sweep_end: Vec2,
    pub sweep_yaw: f32,
}

/// The yaw (about `+y` from north, positive west) that looks along `dir` `(x, z)`.
pub fn yaw_of(dir: Vec2) -> f32 {
    (-dir.x).atan2(-dir.y)
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `a` to `b` by the shorter way round.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    let d = (b - a + PI).rem_euclid(TAU) - PI;
    a + d * t
}

fn clamp_field(p: Vec2) -> Vec2 {
    p.clamp(Vec2::splat(-FIELD_LIMIT_M), Vec2::splat(FIELD_LIMIT_M))
}

/// `dir` scaled until its larger component reaches `limit`.
fn to_square(dir: Vec2, limit: f32) -> Vec2 {
    dir * (limit / dir.x.abs().max(dir.y.abs()).max(1e-6))
}

fn perp(v: Vec2) -> Vec2 {
    Vec2::new(-v.y, v.x)
}

/// The highest `surface` (the drawn one: the ground or the water over it) on a 3 × 3 grid
/// [`FOLLOW_SPAN_M`] apart around `p`.
fn surface_around(surface: &impl Fn(f32, f32) -> f32, p: Vec2) -> f32 {
    let mut top = f32::MIN;
    for i in -1..=1 {
        for j in -1..=1 {
            let q = p + Vec2::new(i as f32, j as f32) * FOLLOW_SPAN_M;
            top = top.max(surface(q.x, q.y));
        }
    }
    top
}

/// The nearest sea-land crossing on [`COAST_RAYS`] rays from the centre (ties to the first ray,
/// north, then clockwise), refined by bisection.
pub fn find_coast(ground: &impl Fn(f32, f32) -> f32) -> Coast {
    let land = |p: Vec2| ground(p.x, p.y) > 0.0;
    let centre_land = land(Vec2::ZERO);
    let mut best: Option<(f32, Vec2)> = None;
    for k in 0..COAST_RAYS {
        let a = k as f32 * TAU / COAST_RAYS as f32;
        let dir = Vec2::new(a.sin(), -a.cos());
        let steps = (COAST_REACH_M / COAST_STEP_M) as u32;
        let Some(i) = (1..=steps).find(|&i| land(dir * i as f32 * COAST_STEP_M) != centre_land)
        else {
            continue;
        };
        if best.is_some_and(|(r, _)| r <= (i - 1) as f32 * COAST_STEP_M) {
            continue;
        }
        let (mut lo, mut hi) = ((i - 1) as f32 * COAST_STEP_M, i as f32 * COAST_STEP_M);
        for _ in 0..12 {
            let mid = 0.5 * (lo + hi);
            if land(dir * mid) == centre_land {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let r = 0.5 * (lo + hi);
        if best.is_none_or(|(b, _)| r < b) {
            best = Some((r, dir));
        }
    }
    let Some((r, dir)) = best else {
        return Coast::None;
    };
    let point = dir * r;
    let h = 500.0;
    let g = |p: Vec2| ground(p.x, p.y);
    let gradient = Vec2::new(
        g(point + Vec2::X * h) - g(point - Vec2::X * h),
        g(point + Vec2::Y * h) - g(point - Vec2::Y * h),
    );
    let mut tangent = if gradient.length() > 1e-3 {
        perp(gradient).normalize()
    } else {
        perp(dir)
    };
    if tangent.dot(perp(dir)) < 0.0 {
        tangent = -tangent;
    }
    if point.x.abs().max(point.y.abs()) <= COAST_IN_FIELD_M {
        Coast::InField { point, tangent }
    } else {
        Coast::InApron { point, tangent }
    }
}

/// Rings and rays of [`distance_to_coast`]'s search.
const COAST_RING_STEP_M: f32 = 250.0;
const COAST_RING_RAYS: usize = 32;

/// How far `p` is from the nearest sea-land crossing, searched on [`COAST_RING_RAYS`] rays in
/// rings [`COAST_RING_STEP_M`] apart out to `reach` (so within one ring step); `None` when the
/// ground keeps `p`'s side throughout.
pub fn distance_to_coast(ground: &impl Fn(f32, f32) -> f32, p: Vec2, reach: f32) -> Option<f32> {
    let land = |q: Vec2| ground(q.x, q.y) > 0.0;
    let here = land(p);
    let rings = (reach / COAST_RING_STEP_M).ceil() as u32;
    (1..=rings)
        .map(|i| i as f32 * COAST_RING_STEP_M)
        .find(|&r| {
            (0..COAST_RING_RAYS).any(|k| {
                let a = k as f32 * TAU / COAST_RING_RAYS as f32;
                land(p + Vec2::new(a.sin(), -a.cos()) * r) != here
            })
        })
}

/// The farthest the run along the coast (the coast run's last 9 s) gets from a coastline, for a
/// coast in the inner field; `None` for a coast in the apron, no coast, or a run point with no
/// coast within 20 km.
pub fn coast_run_distance(path: &BenchPath, ground: impl Fn(f32, f32) -> f32) -> Option<f32> {
    if !matches!(path.coast, Coast::InField { .. }) {
        return None;
    }
    let mut worst = 0.0f32;
    for i in 0..=36 {
        let p = path.run_start.lerp(path.run_end, i as f32 / 36.0);
        worst = worst.max(distance_to_coast(&ground, p, 20_000.0)?);
    }
    Some(worst)
}

/// Distance along the heading at `t` (0–30 s): 600 m/s, then speeding up to 4,000 m/s.
fn along(t: f32) -> f32 {
    let quarter = SEGMENT_SECS;
    if t <= quarter {
        LOW_SPEED_MPS * t
    } else {
        let tau = (t - quarter).min(quarter);
        let accel = (CLIMB_END_SPEED_MPS - LOW_SPEED_MPS) / quarter;
        LOW_SPEED_MPS * quarter + LOW_SPEED_MPS * tau + 0.5 * accel * tau * tau
    }
}

impl BenchPath {
    /// Plans the path over `ground(x, z)` (metres, sea level 0: the coast is where it crosses
    /// 0) and the drawn `surface(x, z)` (the ground or the water over it, which the fly keeps
    /// clear of), e.g. [`BenchPath::for_grid`].
    pub fn plan(ground: impl Fn(f32, f32) -> f32, surface: impl Fn(f32, f32) -> f32) -> BenchPath {
        let start_y = surface(0.0, 0.0) + CAMERA_START_ALTITUDE_M;
        let coast = find_coast(&ground);
        let climb_end_d = along(2.0 * SEGMENT_SECS);
        let (run_start, run_end, run_yaw, run_high, toward) = match coast {
            Coast::InField { point, tangent } => {
                let half = tangent * (COAST_RUN_M / 2.0);
                let toward = point.try_normalize();
                (
                    clamp_field(point - half),
                    clamp_field(point + half),
                    yaw_of(tangent),
                    false,
                    toward,
                )
            }
            Coast::InApron { point, tangent } => {
                let dir = point.normalize();
                let edge = to_square(dir, COAST_IN_FIELD_M);
                let half = tangent * (COAST_RUN_M / 2.0);
                (
                    clamp_field(edge - half),
                    clamp_field(edge + half),
                    yaw_of(dir),
                    true,
                    Some(dir),
                )
            }
            Coast::None => (Vec2::ZERO, Vec2::ZERO, 0.0, false, None),
        };
        let heading = match coast {
            Coast::None => Vec2::NEG_Y,
            _ => run_start.try_normalize().unwrap_or(Vec2::NEG_Y),
        };
        let (run_start, run_end, run_yaw) = match coast {
            Coast::None => {
                let from = clamp_field(heading * climb_end_d);
                let across = perp(heading);
                (
                    from,
                    clamp_field(from + across * COAST_RUN_M),
                    yaw_of(across),
                )
            }
            _ => (run_start, run_end, run_yaw),
        };
        let sweep_dir = toward.unwrap_or(heading);
        BenchPath {
            start_y,
            heading,
            coast,
            run_start,
            run_end,
            run_yaw,
            run_high,
            sweep_end: to_square(sweep_dir, FIELD_LIMIT_M),
            sweep_yaw: yaw_of(sweep_dir),
        }
    }

    /// The path over a region's blended ground and drawn surface.
    pub fn for_grid(grid: &RegionGrid) -> BenchPath {
        BenchPath::plan(
            |x, z| blended_height_m(grid, f64::from(x), f64::from(z)),
            |x, z| blended_surface_m(grid, f64::from(x), f64::from(z)),
        )
    }

    /// The camera at `script_s` seconds (clamped to `[0, SCRIPT_SECS]`), over the same
    /// `surface` the path was planned on.
    pub fn at(&self, script_s: f32, surface: impl Fn(f32, f32) -> f32) -> FlyState {
        let t = script_s.clamp(0.0, SCRIPT_SECS);
        let q = SEGMENT_SECS;
        let follow = |p: Vec2, agl: f32| surface_around(&surface, p) + agl;
        let heading_yaw = yaw_of(self.heading);
        let start_pitch = -START_PITCH_DEG.to_radians();
        // M2's pitches (6°, 30°, 10° and 3° down) as chase looks (Task 3 changed what a pitch
        // means: M2's −3° now looks 2° up).
        let look = |deg: f64| pitch_for_look_deg(deg).to_radians();
        let low_pitch = look(6.0);
        let climb_pitch = look(30.0);
        let run_pitch = look(10.0);
        let sweep_pitch = look(3.0);
        let high_pitch = look(25.0);
        let run_y = |p: Vec2| {
            let y = follow(p, COAST_AGL_M);
            if self.run_high {
                y.max(COAST_VIEW_M)
            } else {
                y
            }
        };
        let on_heading = |t: f32| clamp_field(self.heading * along(t));

        let (pos, y, yaw, pitch) = match Segment::at(t) {
            Segment::LowPass => {
                let p = on_heading(t);
                let s = smoothstep(t / 3.0);
                (
                    p,
                    self.start_y + (follow(p, LOW_PASS_AGL_M) - self.start_y) * s,
                    lerp_angle(0.0, heading_yaw, s),
                    start_pitch + (low_pitch - start_pitch) * s,
                )
            }
            Segment::Climb => {
                let p = on_heading(t);
                let s = smoothstep((t - q) / q);
                let low = follow(p, LOW_PASS_AGL_M);
                (
                    p,
                    low + (CLIMB_TOP_M - low) * s,
                    heading_yaw,
                    low_pitch + (climb_pitch - low_pitch) * s,
                )
            }
            Segment::CoastRun => {
                let transit = 0.4 * SEGMENT_SECS;
                let u = t - 2.0 * q;
                if u < transit {
                    let s = smoothstep(u / transit);
                    let from = on_heading(2.0 * q);
                    let p = from.lerp(self.run_start, s);
                    (
                        p,
                        CLIMB_TOP_M + (run_y(self.run_start) - CLIMB_TOP_M) * s,
                        lerp_angle(heading_yaw, self.run_yaw, s),
                        climb_pitch + (run_pitch - climb_pitch) * s,
                    )
                } else {
                    let s = (u - transit) / (q - transit);
                    let p = self.run_start.lerp(self.run_end, s);
                    (p, run_y(p), self.run_yaw, run_pitch)
                }
            }
            Segment::HighPass => {
                let u = (t - 4.0 * q) / q;
                // Where the sweep ends: its yaw by the same expression, so nothing jumps.
                let y0 = follow(self.sweep_end, SWEEP_AGL_M);
                let yaw0 = lerp_angle(self.run_yaw, self.sweep_yaw, 1.0);
                let climb = smoothstep(u / 0.7);
                (
                    self.sweep_end.lerp(Vec2::ZERO, smoothstep(u)),
                    y0 + (HIGH_PASS_TOP_M - y0) * climb,
                    yaw0 + PI * smoothstep((u - 0.5) / 0.5),
                    sweep_pitch + (high_pitch - sweep_pitch) * climb,
                )
            }
            Segment::ApronSweep => {
                let u = (t - 3.0 * q) / q;
                let p = self.run_end.lerp(self.sweep_end, smoothstep(u));
                let turn = smoothstep(u * 3.0);
                let y0 = run_y(self.run_end);
                let pan = SWEEP_HALF_ANGLE_DEG.to_radians() * (TAU * u).sin() * turn;
                (
                    p,
                    y0 + (follow(p, SWEEP_AGL_M) - y0) * smoothstep(u * 2.0),
                    lerp_angle(self.run_yaw, self.sweep_yaw, turn) + pan,
                    run_pitch + (sweep_pitch - run_pitch) * turn,
                )
            }
        };
        let y = y.max(follow(pos, MIN_AGL_M));
        FlyState {
            pos_m: Vec3::new(pos.x, y, pos.y),
            yaw,
            pitch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fly_camera::{CAMERA_CEILING_M, CAMERA_CLEARANCE_M, MAX_PITCH_DEG, MIN_PITCH_DEG};

    /// Hills over a plane that falls to the east: the coast runs north-south near x = 20 km.
    fn coastal(x: f32, z: f32) -> f32 {
        400.0 - x / 50.0 + 150.0 * (x / 7_000.0).sin() * (z / 9_000.0).cos()
    }

    /// Mountains everywhere, no sea within reach.
    fn inland(x: f32, z: f32) -> f32 {
        2_500.0 + 1_800.0 * (x / 11_000.0).sin() * (z / 13_000.0).cos()
    }

    /// Land within 400 km to the south-west only.
    fn far_coast(x: f32, z: f32) -> f32 {
        if (x + 300_000.0).hypot(z - 280_000.0) < 120_000.0 {
            -500.0
        } else {
            900.0
        }
    }

    /// The drawn surface over `ground`: the sea where it is below 0.
    fn sea(ground: fn(f32, f32) -> f32) -> impl Fn(f32, f32) -> f32 + Copy {
        move |x, z| ground(x, z).max(0.0)
    }

    /// The path over `ground` with the sea over it.
    fn plan(ground: fn(f32, f32) -> f32) -> BenchPath {
        BenchPath::plan(ground, sea(ground))
    }

    fn samples(path: &BenchPath, ground: fn(f32, f32) -> f32) -> Vec<FlyState> {
        (0..=(SCRIPT_SECS * 60.0) as u32)
            .map(|i| path.at(i as f32 / 60.0, sea(ground)))
            .collect()
    }

    #[test]
    fn region_bench_script_is_deterministic() {
        for ground in [coastal as fn(f32, f32) -> f32, inland, far_coast] {
            let a = plan(ground);
            let b = plan(ground);
            assert_eq!(a, b);
            assert_eq!(samples(&a, ground), samples(&b, ground));
        }
        // A different region gives a different path.
        assert_ne!(plan(coastal), plan(inland));
    }

    #[test]
    fn the_coast_is_found_and_classified() {
        match find_coast(&coastal) {
            Coast::InField { point, tangent } => {
                assert!((coastal(point.x, point.y)).abs() < 5.0, "{point}");
                assert!(point.x > 0.0, "the sea is to the east: {point}");
                // The coast runs roughly north-south.
                assert!(tangent.y.abs() > 0.7, "{tangent}");
                assert!((tangent.length() - 1.0).abs() < 1e-4);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(find_coast(&inland), Coast::None);
        match find_coast(&far_coast) {
            Coast::InApron { point, .. } => {
                assert!(point.x < 0.0 && point.y > 0.0, "south-west: {point}");
                let expected = 280_000f32.hypot(300_000.0) - 120_000.0;
                assert!((point.length() - expected).abs() < 3_000.0, "{point}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The Himalayas (ruling R24): ridges to 8.8 km over a 4–5 km plateau, every peak steep.
    fn himalaya(x: f32, z: f32) -> f32 {
        let ridges = (x / 9_000.0).sin().abs() * (z / 13_000.0).cos().abs();
        4_200.0 + 4_600.0 * ridges.powi(3) + 300.0 * (x / 2_100.0 + z / 1_700.0).sin()
    }

    /// Every frame of the path, over each kind of region: inside the field, clear of the
    /// ground, under the ceiling, a sane pitch, and no jumps between frames.
    #[test]
    fn the_path_stays_in_the_field_and_above_the_ground() {
        let top = (0..400)
            .flat_map(|i| (0..400).map(move |k| (i, k)))
            .map(|(i, k)| himalaya(i as f32 * 750.0 - 150_000.0, k as f32 * 750.0 - 150_000.0))
            .fold(f32::MIN, f32::max);
        assert!(top > 8_700.0, "the himalaya ground reaches 8.7 km: {top}");
        for ground in [coastal as fn(f32, f32) -> f32, inland, far_coast, himalaya] {
            let path = plan(ground);
            let frames = samples(&path, ground);
            for (i, f) in frames.iter().enumerate() {
                let p = f.pos_m;
                assert!(p.is_finite() && f.yaw.is_finite() && f.pitch.is_finite());
                assert!(
                    p.x.abs() <= FIELD_LIMIT_M && p.z.abs() <= FIELD_LIMIT_M,
                    "{i}: {p}"
                );
                let floor = sea(ground)(p.x, p.z);
                assert!(p.y >= floor + MIN_AGL_M - 1e-3, "{i}: {p} over {floor}");
                assert!(p.y >= floor + CAMERA_CLEARANCE_M);
                assert!(p.y <= CAMERA_CEILING_M);
                assert!((MIN_PITCH_DEG..=MAX_PITCH_DEG).contains(&f.pitch.to_degrees()));
            }
            for (i, w) in frames.windows(2).enumerate() {
                let (a, b) = (w[0].pos_m, w[1].pos_m);
                let horizontal = Vec2::new(b.x - a.x, b.z - a.z).length();
                assert!(horizontal <= 1_500.0, "{i}: {horizontal} m in a frame");
                assert!((b.y - a.y).abs() <= 1_000.0, "{i}: {a} -> {b}");
            }
        }
    }

    #[test]
    fn the_path_starts_at_the_start_pose_and_follows_its_script() {
        let path = plan(coastal);
        let start = path.at(0.0, sea(coastal));
        let pose = FlyState {
            pos_m: Vec3::new(0.0, coastal(0.0, 0.0) + CAMERA_START_ALTITUDE_M, 0.0),
            ..default()
        };
        assert!(start.pos_m.abs_diff_eq(pose.pos_m, 1e-3), "{start:?}");
        assert!((start.yaw - pose.yaw).abs() < 1e-6 && (start.pitch - pose.pitch).abs() < 1e-6);
        // The low pass holds 300 m over the highest ground around it.
        for t in [4.0, 8.0, 14.9] {
            let f = path.at(t, sea(coastal));
            let around = surface_around(&sea(coastal), Vec2::new(f.pos_m.x, f.pos_m.z));
            assert!(
                (f.pos_m.y - around - LOW_PASS_AGL_M).abs() < 1e-2,
                "{t}: {f:?}"
            );
        }
        // The climb tops out at 30 km.
        assert!((path.at(29.999, sea(coastal)).pos_m.y - CLIMB_TOP_M).abs() < 1.0);
        // The run follows the coast: the ground under it stays near sea level.
        for t in [37.0, 40.0, 44.9] {
            let p = path.at(t, sea(coastal)).pos_m;
            assert!(coastal(p.x, p.z).abs() < 400.0, "{t}: {p}");
        }
        // The sweep ends at the field's edge in the coast's direction (east).
        let end = path.at(4.0 * SEGMENT_SECS - 1e-3, sea(coastal)).pos_m;
        assert!((end.x - FIELD_LIMIT_M).abs() < 1.0, "{end}");
        assert_eq!(Segment::at(0.0), Segment::LowPass);
        assert_eq!(Segment::at(15.0), Segment::Climb);
        assert_eq!(Segment::at(44.99), Segment::CoastRun);
        assert_eq!(Segment::at(59.99), Segment::ApronSweep);
        assert_eq!(Segment::at(60.0), Segment::HighPass);
        assert_eq!(Segment::at(SCRIPT_SECS), Segment::HighPass);
    }

    /// Islands and inlets everywhere, like the Aegean: the coast run stays near a real coast.
    fn archipelago(x: f32, z: f32) -> f32 {
        -60.0 + 400.0 * (x / 9_000.0 + 0.7).sin() * (z / 12_000.0 + 0.3).cos()
    }

    #[test]
    fn the_coast_run_follows_a_coast_in_the_field() {
        for ground in [coastal as fn(f32, f32) -> f32, archipelago] {
            let path = plan(ground);
            assert!(
                matches!(path.coast, Coast::InField { .. }),
                "{:?}",
                path.coast
            );
            let worst = coast_run_distance(&path, ground).expect("a coast near the run");
            assert!(worst <= 10_000.0, "{worst} m from the coast");
        }
        // No coast in the field: nothing to measure.
        assert_eq!(coast_run_distance(&plan(inland), inland), None);
        assert_eq!(coast_run_distance(&plan(far_coast), far_coast), None);
        // The distance search itself.
        assert!(distance_to_coast(&coastal, Vec2::ZERO, 50_000.0).is_some());
        assert_eq!(distance_to_coast(&inland, Vec2::ZERO, 50_000.0), None);
        let d = distance_to_coast(&far_coast, Vec2::ZERO, 400_000.0).unwrap();
        assert!(
            (d - (280_000f32.hypot(300_000.0) - 120_000.0)).abs() < 4_000.0,
            "{d}"
        );
    }

    /// The high pass climbs into the cloud base's band (at least 250 km) and stays below the
    /// climb-out trigger (`REGION_CLOUD_BASE_M − 1 km`, ruling P3), so the bench never leaves
    /// the region through the cloud.
    #[test]
    fn the_bench_path_reaches_the_cloud_base_band() {
        use timefly_core::flight::altitudes::REGION_CLOUD_BASE_M;
        let trigger = (REGION_CLOUD_BASE_M - 1_000.0) as f32;
        for ground in [coastal as fn(f32, f32) -> f32, inland, far_coast, himalaya] {
            let path = plan(ground);
            let high: Vec<FlyState> = samples(&path, ground)
                .into_iter()
                .enumerate()
                .filter(|(i, _)| Segment::at(*i as f32 / 60.0) == Segment::HighPass)
                .map(|(_, f)| f)
                .collect();
            let top = high.iter().map(|f| f.pos_m.y).fold(f32::MIN, f32::max);
            assert!(top >= 250_000.0, "the high pass tops out at {top} m");
            assert!(
                high.iter().all(|f| f.pos_m.y < trigger),
                "{top} m reaches the climb-out trigger"
            );
        }
    }

    #[test]
    fn yaw_looks_along_the_direction() {
        for dir in [Vec2::NEG_Y, Vec2::X, Vec2::new(-1.0, 1.0).normalize()] {
            let state = FlyState {
                pos_m: Vec3::ZERO,
                yaw: yaw_of(dir),
                pitch: 0.0,
            };
            let forward = state.rotation() * Vec3::NEG_Z;
            assert!(
                Vec2::new(forward.x, forward.z).abs_diff_eq(dir, 1e-5),
                "{dir}"
            );
        }
    }
}
