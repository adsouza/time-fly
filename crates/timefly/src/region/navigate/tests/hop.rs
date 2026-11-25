//! The region hop (Task 9): `step_nav`'s hop, pure, then headless hops with the stand-in
//! neighbour job, the region swap, the terrain scene and both cameras.

use proptest::prelude::*;

use super::*;

use crate::bench::region::path::{BenchPath, SCRIPT_SECS};
use crate::region::{PendingRegion, RegionPreloadCancelled, RegionPreloadFailed};
use timefly_core::flight::altitudes::{
    EXPECTED_GEN_S_NATIVE, EXPECTED_GEN_S_WEB, HOP_ALTITUDE_M, LIFT_LEAD_M, NEIGHBOUR_PAST_EDGE_M,
    PRELOAD_MIN_M,
};
use timefly_core::flight::hop::{EDGE_M, should_preload};

/// The Alps at the present: the region the hops leave.
fn alps() -> RenderDir {
    RenderDir(geo_to_render(latlon_to_geo(46.5, 10.0)).0.normalize())
}

/// The ground under the hops (flat).
const HOP_GROUND_M: f32 = 500.0;
/// The hops' height above it.
const HOP_AGL_M: f32 = 1_500.0;

/// Flying in the Alps' region at the present, `x_m` east of the centre and `agl_m` above flat
/// ground at [`HOP_GROUND_M`].
fn in_alps(x_m: f32, agl_m: f32) -> NavInputs {
    NavInputs {
        region_state: RegionState::InRegion,
        time: YearsBp(0.0),
        region_centre: Some(alps()),
        region_snapshot: Some(YearsBp(0.0)),
        region_height_m: HOP_GROUND_M + agl_m,
        region_ground_m: HOP_GROUND_M,
        region_pos_m: Vec2::new(x_m, 0.0),
        ..inputs()
    }
}

/// Flying east at 1 km/s in the Alps' region from `x_m`, [`HOP_AGL_M`] up: the inputs and the
/// fly's wish (its velocity when nothing holds it).
fn flying_east(x_m: f32) -> (NavInputs, Vec2) {
    let wish = Vec2::X * 1_000.0;
    let i = NavInputs {
        region_vel_mps: wish,
        ..in_alps(x_m, HOP_AGL_M)
    };
    (i, wish)
}

/// The local `(x, z)` of `dir` in the region centred on `centre`.
fn local_of(centre: RenderDir, dir: RenderDir) -> (f64, f64) {
    RegionFrame { center: centre }.dir_to_local(dir).unwrap()
}

/// The neighbour east of the Alps' region, level with its centre.
fn east_of_alps() -> RenderDir {
    RegionFrame { center: alps() }.local_to_dir(f64::from(EDGE_M) + NEIGHBOUR_PAST_EDGE_M, 0.0)
}

/// One frame of a hop, as [`frame`], then the fly moved along `wish` (m/s) at the share of it
/// its controls give in the next phase (a turn back reverses it), and that taken as its
/// velocity for the next frame.
fn hop_frame(
    phase: NavPhase,
    i: &mut NavInputs,
    wish: &mut Vec2,
) -> (NavPhase, Vec<NavAction>, f32, f32) {
    let out = frame(phase, i);
    // A turn back faces the fly as it says (the keys fly it that way).
    if let Some(yaw) = out.1.iter().find_map(|a| match *a {
        NavAction::TurnBack { yaw, .. } => Some(yaw),
        _ => None,
    }) {
        *wish = Vec2::new(-yaw.sin(), -yaw.cos()) * wish.length();
    }
    // As the shell moves the fly: at its controls' share of the wish, kept inside the field
    // unless they free it, its velocity measured from the move, or the wish where an edge holds
    // it (navigation reads the player's intent there).
    let controls = out.0.fly_controls();
    let share = controls.map_or(0.0, |c| c.horizontal);
    let from = i.region_pos_m;
    let to = from + *wish * share * i.dt_s;
    i.region_pos_m = match controls {
        Some(c) if c.confined => timefly_core::flight::hop::confine(from, to),
        _ => to,
    };
    let moved = (i.region_pos_m - from) / i.dt_s;
    // The keys' wish at full speed, as the shell reads it (`wish_mps`), whatever share of it
    // the controls give.
    i.region_vel_mps = timefly_core::flight::hop::velocity_at_edges(i.region_pos_m, moved, *wish);
    out
}

/// A recorded frame of a pure hop: the phase after it, its actions, the cloud it drew, and the
/// fly's height and `x` after it.
#[derive(Clone, Debug)]
struct HopFrame {
    phase: NavPhase,
    actions: Vec<NavAction>,
    density: f32,
    height_m: f32,
    x_m: f32,
}

/// Flies from `phase` for at most `frames` frames, until `done`, with `each` adjusting the
/// inputs before every frame.
fn fly_hop(
    mut phase: NavPhase,
    i: &mut NavInputs,
    wish: &mut Vec2,
    frames: usize,
    mut each: impl FnMut(usize, &NavPhase, &mut NavInputs),
    done: impl Fn(&HopFrame) -> bool,
) -> Vec<HopFrame> {
    let mut out = Vec::new();
    for k in 0..frames {
        each(k, &phase, i);
        let (next, actions, _, density) = hop_frame(phase, i, wish);
        phase = next;
        out.push(HopFrame {
            phase,
            actions,
            density,
            height_m: i.region_height_m,
            x_m: i.region_pos_m.x,
        });
        if done(out.last().unwrap()) {
            return out;
        }
    }
    out
}

fn has(actions: &[NavAction], what: impl Fn(&NavAction) -> bool) -> bool {
    actions.iter().any(what)
}

fn preloads(frames: &[HopFrame]) -> Vec<RenderDir> {
    frames
        .iter()
        .flat_map(|f| f.actions.iter())
        .filter_map(|a| match *a {
            NavAction::Preload(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn swapped(p: &NavPhase) -> bool {
    matches!(p, NavPhase::Hopping { swapped: true, .. })
}

fn hopping(p: &NavPhase) -> bool {
    matches!(p, NavPhase::Hopping { .. })
}

/// The lift's progress of a hop before it ends (1 at full cover).
fn lift_of(p: &NavPhase) -> f32 {
    match *p {
        NavPhase::Hopping {
            lift,
            swapped: false,
            turned_back: false,
            ..
        } => lift,
        _ => 0.0,
    }
}

/// A hop's leaving actions: none of these may happen while it keeps the region.
fn leaves(a: &NavAction) -> bool {
    matches!(
        a,
        NavAction::Leave | NavAction::CancelRequest | NavAction::SwapToNeighbour
    )
}

#[test]
fn nearing_the_edge_preloads_once() {
    for speed in [1_000.0f32, 25_000.0] {
        let reach = (PRELOAD_MIN_M as f32).max(speed * EXPECTED_GEN_S);
        let mut i = in_alps(EDGE_M - reach - 20.0 * speed * 0.1, HOP_AGL_M);
        let mut wish = Vec2::X * speed;
        i.region_vel_mps = wish;
        let mut first = None;
        let frames = fly_hop(
            NavPhase::Flying,
            &mut i,
            &mut wish,
            2_000,
            |_, _, _| {},
            |f| f.x_m > EDGE_M - LIFT_LEAD_M as f32 - speed,
        );
        for (k, f) in frames.iter().enumerate() {
            if first.is_none() && has(&f.actions, |a| matches!(a, NavAction::Preload(_))) {
                first = Some(k);
            }
            assert!(!has(&f.actions, |a| leaves(a)
                || matches!(a, NavAction::CancelPreload)));
        }
        let k = first.expect("a preload");
        // At the preload distance for its speed, not a frame before.
        let x_before = if k == 0 {
            EDGE_M - reach - 20.0 * speed * 0.1
        } else {
            frames[k - 1].x_m
        };
        assert!(EDGE_M - x_before <= reach + 1.0, "{speed}: {x_before}");
        assert!(
            EDGE_M - x_before > reach - speed * 0.1 - 1.0,
            "{speed}: {x_before}"
        );
        // Once, and the hop holds on to it.
        let all = preloads(&frames);
        assert_eq!(all.len(), 1, "{speed}: {all:?}");
        assert!(frames[k..].iter().all(|f| hopping(&f.phase)));
        assert!(frames[..k].iter().all(|f| f.phase == NavPhase::Flying));
        // 100 km past the east edge, level with the fly.
        let (x, z) = local_of(alps(), all[0]);
        assert!((x - 250_000.0).abs() < 1.0 && z.abs() < 1.0, "({x}, {z})");
        // The region is untouched and the cloud clear until the lift.
        assert!(
            frames
                .iter()
                .all(|f| f.density == 0.0 || lift_of(&f.phase) > 0.0)
        );
    }
    // Flying along the edge or back in never preloads, and the hop never begins away from it.
    for wish in [Vec2::NEG_X, Vec2::Y, Vec2::NEG_Y] {
        let mut i = in_alps(EDGE_M - 30_000.0, HOP_AGL_M);
        let mut wish = wish * 1_000.0;
        i.region_vel_mps = wish;
        let frames = fly_hop(
            NavPhase::Flying,
            &mut i,
            &mut wish,
            50,
            |_, _, _| {},
            |_| false,
        );
        assert!(
            frames
                .iter()
                .all(|f| f.phase == NavPhase::Flying && f.actions.is_empty())
        );
    }
}

#[test]
fn turning_back_cancels_the_preload_and_keeps_the_region() {
    // (start x, frames flown out) for a turn before the lift, mid-lift, and in the hold at
    // full cover with no neighbour ready.
    for (start, out_frames) in [
        (EDGE_M - 45_000.0, 10),
        (EDGE_M - 15_000.0, 20),
        (EDGE_M - 2_000.0, 60),
    ] {
        let (mut i, mut wish) = flying_east(start);
        let out = fly_hop(
            NavPhase::Flying,
            &mut i,
            &mut wish,
            out_frames,
            |_, _, _| {},
            |_| false,
        );
        let at_turn = out.last().unwrap().clone();
        assert!(hopping(&at_turn.phase), "{start}: {:?}", at_turn.phase);
        wish = -wish;
        let back = fly_hop(
            at_turn.phase,
            &mut i,
            &mut wish,
            200,
            |_, _, _| {},
            |f| f.phase == NavPhase::Flying,
        );
        let end = back.last().unwrap();
        assert_eq!(end.phase, NavPhase::Flying, "{start}: never back to flying");
        // The preload is cancelled once; the region is kept and nothing swaps.
        let cancels = back
            .iter()
            .filter(|f| has(&f.actions, |a| matches!(a, NavAction::CancelPreload)))
            .count();
        assert_eq!(cancels, 1, "{start}");
        assert!(out.iter().chain(&back).all(|f| !has(&f.actions, leaves)));
        assert_eq!(i.region_centre, Some(alps()));
        // The cloud clears (never thickening once turned), within the clear time, and is gone.
        assert!(
            back.windows(2).all(|w| w[1].density <= w[0].density),
            "{start}"
        );
        // Opaque until back inside the field (fix round 2: the hold drifts past the edge), then a
        // frame to see the turn, then the clear.
        let inside = back.iter().position(|f| f.x_m <= EDGE_M).unwrap_or(0);
        assert!(back[..inside].iter().all(|f| f.density == 1.0), "{start}");
        let clear_frames = (CLOUD_CLEAR_S / 0.1).ceil() as usize + 3;
        let clear = back
            .iter()
            .position(|f| f.density == 0.0)
            .unwrap_or(back.len());
        assert!(
            clear - inside <= clear_frames,
            "{start}: cleared {} frames after back inside",
            clear - inside
        );
        assert_eq!(end.density, 0.0);
        // The fly is back at its height above the ground.
        assert!(
            (end.height_m - (HOP_GROUND_M + HOP_AGL_M)).abs() < 1.0,
            "{start}: {}",
            end.height_m
        );
    }
}

#[test]
fn reaching_the_edge_lifts_to_sixty_km() {
    for agl in [HOP_AGL_M, 79_500.0] {
        let start_y = HOP_GROUND_M + agl;
        let mut i = in_alps(EDGE_M - 40_000.0, agl);
        let mut wish = Vec2::X * 1_000.0;
        i.region_vel_mps = wish;
        let frames = fly_hop(
            NavPhase::Flying,
            &mut i,
            &mut wish,
            1_000,
            |_, _, _| {},
            |f| f.x_m >= EDGE_M,
        );
        let at_edge = frames.last().unwrap();
        assert!(at_edge.x_m >= EDGE_M);
        // Lifted to 60 km by the edge, and opaque there; a fly above that is not lifted.
        let top = start_y.max(HOP_ALTITUDE_M as f32);
        // The frame that reached the edge decided from just short of it; the next is at it.
        let (next, actions, _, density) = frame(at_edge.phase, &mut i);
        assert!(hopping(&next));
        assert_eq!(i.region_height_m, top, "{agl}: {actions:?}");
        assert_eq!(density, 1.0, "{agl}");
        // Gently: never down on the way out, and only within the lift's lead of the edge.
        assert!(frames.windows(2).all(|w| w[1].height_m >= w[0].height_m));
        for f in &frames {
            if f.x_m < EDGE_M - LIFT_LEAD_M as f32 - 1_100.0 {
                assert_eq!(f.height_m, start_y, "{agl}: lifted at {}", f.x_m);
                assert_eq!(f.density, 0.0);
            }
        }
        if agl > HOP_ALTITUDE_M as f32 {
            assert!(frames.iter().all(|f| f.height_m == start_y), "{agl}");
        }
    }
}

/// Flies out of the Alps' region from 135 km east at 1 km/s until the hop holds at full cover
/// at the edge, with no neighbour ready; returns the phase and the inputs there.
fn held_at_the_edge() -> (NavPhase, NavInputs, Vec2) {
    let (mut i, mut wish) = flying_east(EDGE_M - 15_000.0);
    let frames = fly_hop(
        NavPhase::Flying,
        &mut i,
        &mut wish,
        1_000,
        |_, _, _| {},
        |f| f.x_m >= EDGE_M && f.density == 1.0,
    );
    let end = frames.last().unwrap();
    assert_eq!(end.density, 1.0, "{:?}", end.phase);
    (end.phase, i, wish)
}

#[test]
fn the_hop_swaps_only_when_opaque_and_ready() {
    let (mut phase, mut i, mut wish) = held_at_the_edge();
    // Held in the cloud while the neighbour generates: opaque, the fly slowed to a drift.
    let x0 = i.region_pos_m.x;
    for _ in 0..50 {
        let (next, actions, _, density) = hop_frame(phase, &mut i, &mut wish);
        assert!(!has(&actions, leaves), "{actions:?}");
        assert_eq!(density, 1.0);
        phase = next;
    }
    assert_eq!(
        phase.fly_controls(),
        Some(FlyControls {
            horizontal: HOLD_DRIFT,
            vertical: false,
            confined: false,
        })
    );
    let drift = i.region_pos_m.x - x0;
    assert!(
        drift > 0.0 && drift <= 50.0 * 0.1 * 1_000.0 * HOLD_DRIFT + 1.0,
        "{drift}"
    );
    // Not another region, nor another snapshot, nor this one again.
    for wrong in [
        (east_of_alps(), YearsBp(21_000.0)),
        (
            RegionFrame { center: alps() }.local_to_dir(0.0, -250_000.0),
            YearsBp(0.0),
        ),
        (alps(), YearsBp(0.0)),
    ] {
        i.neighbour = Some(wrong);
        let (next, actions, _, _) = hop_frame(phase, &mut i, &mut wish);
        assert!(!has(&actions, leaves), "{wrong:?}");
        phase = next;
    }
    // Ready, but the last frame drew a hair short of opaque.
    i.neighbour = Some((east_of_alps(), YearsBp(0.0)));
    i.shown_density = 0.999_999;
    let (next, actions, _, _) = step_and_draw(phase, &mut i);
    assert!(!has(&actions, leaves), "{actions:?}");
    assert!(hopping(&next) && !swapped(&next));
    // Ready after a frame drawn opaque: the swap, on a frame still opaque.
    i.shown_density = 1.0;
    let (next, actions, _, density) = hop_frame(next, &mut i, &mut wish);
    assert_eq!(
        actions
            .iter()
            .filter(|a| matches!(a, NavAction::SwapToNeighbour))
            .count(),
        1,
        "{actions:?}"
    );
    assert!(swapped(&next));
    assert_eq!(density, 1.0);
    assert_eq!(i.region_centre, Some(east_of_alps()));

    // Ready early: still no swap until the lift is complete and drawn.
    let (mut i, mut wish) = flying_east(EDGE_M - 15_000.0);
    let frames = fly_hop(
        NavPhase::Flying,
        &mut i,
        &mut wish,
        1_000,
        |_, phase, i| {
            if let NavPhase::Hopping { centre, .. } = *phase {
                i.neighbour = Some((centre, YearsBp(0.0)));
            }
        },
        |f| swapped(&f.phase),
    );
    let at = frames
        .iter()
        .position(|f| has(&f.actions, |a| matches!(a, NavAction::SwapToNeighbour)))
        .expect("a swap");
    assert!(at > 0 && frames[at - 1].density == 1.0 && frames[at].density == 1.0);
    assert!(frames[..at].iter().all(|f| !has(&f.actions, leaves)));
    assert!(
        frames[at - 1].x_m >= EDGE_M - 1_000.0,
        "{}",
        frames[at - 1].x_m
    );
}

/// [`step_nav`] alone, with the cloud it draws, for inputs whose `shown_density` the test sets.
fn step_and_draw(phase: NavPhase, i: &mut NavInputs) -> (NavPhase, Vec<NavAction>, f32, f32) {
    let before = i.shown_density;
    let (next, actions) = step_nav(phase, i);
    (next, actions, before, next.cloud_density(i))
}

/// Runs a hop from 135 km out with the neighbour ready from the start, through the swap;
/// returns the phase just after the swap and the inputs there.
fn just_swapped(vertical_held: bool) -> (NavPhase, NavInputs, Vec2) {
    let (mut i, mut wish) = flying_east(EDGE_M - 15_000.0);
    let frames = fly_hop(
        NavPhase::Flying,
        &mut i,
        &mut wish,
        1_000,
        |_, phase, i| {
            if let NavPhase::Hopping {
                centre,
                swapped: false,
                ..
            } = *phase
            {
                i.neighbour = Some((centre, YearsBp(0.0)));
            }
            i.vertical_held = vertical_held && swapped(phase);
        },
        |f| swapped(&f.phase),
    );
    let end = frames.last().unwrap();
    assert!(swapped(&end.phase));
    (end.phase, i, wish)
}

#[test]
fn after_the_hop_the_fly_returns_to_its_height_above_ground() {
    let (phase, mut i, mut wish) = just_swapped(false);
    assert!(i.region_height_m > 50_000.0, "{}", i.region_height_m);
    // Over a plateau in the new region, the same height above it.
    i.region_ground_m = 2_000.0;
    let frames = fly_hop(
        phase,
        &mut i,
        &mut wish,
        400,
        |_, _, _| {},
        |f| f.phase == NavPhase::Flying,
    );
    let end = frames.last().unwrap();
    assert_eq!(end.phase, NavPhase::Flying);
    assert!(
        (end.height_m - (2_000.0 + HOP_AGL_M)).abs() < 1.0,
        "{}",
        end.height_m
    );
    // Down all the way, and the cloud only clearing, gone within its clear time.
    assert!(frames.windows(2).all(|w| w[1].height_m <= w[0].height_m));
    assert!(frames.windows(2).all(|w| w[1].density <= w[0].density));
    let clear = (CLOUD_CLEAR_S / 0.1).ceil() as usize;
    assert!(frames[clear..].iter().all(|f| f.density == 0.0));
    assert!(frames.iter().all(|f| !has(&f.actions, leaves)));

    // Climbing or descending held: the player's height stands, and the hop ends once clear.
    let (phase, mut i, mut wish) = just_swapped(true);
    let height = i.region_height_m;
    let frames = fly_hop(
        phase,
        &mut i,
        &mut wish,
        400,
        |_, _, i| i.vertical_held = true,
        |f| f.phase == NavPhase::Flying,
    );
    assert_eq!(frames.last().unwrap().phase, NavPhase::Flying);
    assert!(frames.len() <= clear + 1, "{} frames", frames.len());
    assert!(frames.iter().all(|f| f.height_m == height));
    assert!(
        frames
            .iter()
            .all(|f| !has(&f.actions, |a| matches!(a, NavAction::ReturnToAgl { .. })))
    );
}

#[test]
fn a_failed_neighbour_returns_the_fly_inside_the_edge() {
    // Failing in the hold at full cover, and failing before the lift began.
    for fail_at_x in [EDGE_M, EDGE_M - 45_000.0] {
        let (mut i, mut wish) = flying_east(EDGE_M - 49_000.0);
        let mut failed_once = false;
        let frames = fly_hop(
            NavPhase::Flying,
            &mut i,
            &mut wish,
            2_000,
            |_, phase, i| {
                i.preload_failed = !failed_once
                    && hopping(phase)
                    && i.region_pos_m.x >= fail_at_x
                    && (fail_at_x < EDGE_M || i.shown_density == 1.0);
                failed_once |= i.preload_failed;
            },
            |f| f.phase == NavPhase::Flying && f.x_m < EDGE_M - 1_000.0,
        );
        assert!(failed_once);
        let end = frames.last().unwrap();
        assert_eq!(end.phase, NavPhase::Flying, "{fail_at_x}: stranded");
        // Turned back once, in the cloud at full cover, 10 km inside the edge.
        let turns: Vec<_> = frames
            .iter()
            .enumerate()
            .filter_map(|(k, f)| {
                f.actions.iter().find_map(|a| match *a {
                    NavAction::TurnBack { pos_m, .. } => Some((k, pos_m)),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(turns.len(), 1, "{fail_at_x}: {turns:?}");
        let (k, pos) = turns[0];
        assert_eq!(pos, Vec2::new(EDGE_M - TURN_BACK_INSIDE_M, 0.0));
        assert!(k > 0 && frames[k - 1].density == 1.0 && frames[k].density == 1.0);
        // The region is kept, nothing else is requested, and the cloud clears.
        assert_eq!(i.region_centre, Some(alps()));
        assert_eq!(preloads(&frames).len(), 1, "{fail_at_x}");
        assert!(frames.iter().all(|f| !has(&f.actions, leaves)));
        assert!(frames[k..].windows(2).all(|w| w[1].density <= w[0].density));
        assert_eq!(end.density, 0.0);
        // Flying back in, at its height above the ground.
        assert!(wish.x < 0.0);
        assert!(
            (end.height_m - (HOP_GROUND_M + HOP_AGL_M)).abs() < 1.0,
            "{}",
            end.height_m
        );
    }
}

/// Grounds for the bench path: a coast every 5 km from 20 to 150 km out, and at 200 and 400 km,
/// along each of sixteen directions (the coast search's rays are 10° apart, so a coast between
/// them lands at another distance), the sea beyond it; and land everywhere.
fn bench_grounds() -> Vec<Box<dyn Fn(f32, f32) -> f32>> {
    let mut grounds: Vec<Box<dyn Fn(f32, f32) -> f32>> = vec![Box::new(|x, z| {
        2_500.0 + 1_800.0 * (x / 11_000.0).sin() * (z / 13_000.0).cos()
    })];
    for k in 0..16 {
        let a = k as f32 * std::f32::consts::TAU / 16.0;
        let dir = Vec2::new(a.sin(), -a.cos());
        let near = (4..=30).map(|r| r as f32 * 5_000.0);
        for r in near.chain([200_000.0, 400_000.0]) {
            grounds.push(Box::new(move |x, z| (r - Vec2::new(x, z).dot(dir)) / 50.0));
        }
    }
    grounds
}

#[test]
fn the_region_bench_path_never_triggers_a_hop() {
    let dt = 1.0 / 60.0;
    for (g, ground) in bench_grounds().iter().enumerate() {
        let surface = |x: f32, z: f32| ground(x, z).max(0.0);
        let path = BenchPath::plan(ground, surface);
        let samples: Vec<_> = (0..=(SCRIPT_SECS / dt) as u32)
            .map(|k| path.at(k as f32 * dt, surface))
            .collect();
        for (k, w) in samples.windows(2).enumerate() {
            let xz = |f: &FlyState| Vec2::new(f.pos_m.x, f.pos_m.z);
            let p = xz(&w[1]);
            let v = (p - xz(&w[0])) / dt;
            // Native and web (minor 2: the web's `?region-bench` must not preload either).
            for gen_s in [EXPECTED_GEN_S_NATIVE, EXPECTED_GEN_S_WEB] {
                assert!(
                    !should_preload(p, v, v.length(), gen_s as f32),
                    "ground {g}, {:.2} s, {gen_s} s generation: at {p} moving {v}",
                    (k + 1) as f32 * dt
                );
            }
        }
    }
}

/// Point 4 (Task 8 note b): a hop never asks for the region it is in, and a neighbour ready at
/// the region's own spec is never swapped to.
#[test]
fn a_hop_never_asks_for_the_region_itself() {
    for k in 0..64 {
        let a = k as f32 * std::f32::consts::TAU / 64.0;
        let heading = Vec2::new(a.sin(), -a.cos());
        for reach in [0.0f32, 20_000.0, 49_000.0] {
            let pos = heading * (EDGE_M - reach) / heading.abs().max_element();
            let i = NavInputs {
                region_pos_m: pos,
                region_vel_mps: heading * 1_000.0,
                ..in_alps(0.0, HOP_AGL_M)
            };
            let (_, actions) = step_nav(NavPhase::Flying, &i);
            for c in actions.iter().filter_map(|a| match *a {
                NavAction::Preload(c) => Some(c),
                _ => None,
            }) {
                assert!(!same_region(c, YearsBp(0.0), alps(), YearsBp(0.0)));
                let (x, z) = local_of(alps(), c);
                assert!(x.abs().max(z.abs()) > 240_000.0, "({x}, {z})");
            }
        }
    }
}

#[test]
fn a_slider_change_before_the_lift_ends_the_hop_and_during_it_waits() {
    // Before the lift: the preload is cancelled and the change applies in flight.
    let (mut i, mut wish) = flying_east(EDGE_M - 45_000.0);
    let (phase, actions, _, _) = hop_frame(NavPhase::Flying, &mut i, &mut wish);
    assert!(has(&actions, |a| matches!(a, NavAction::Preload(_))));
    assert!(phase.holds_time());
    i.time_queued = true;
    let (phase, actions, _, _) = hop_frame(phase, &mut i, &mut wish);
    assert_eq!(phase, NavPhase::Flying);
    assert_eq!(actions, vec![NavAction::CancelPreload]);

    // During the lift it waits: the hop goes on, holding the time, and may swap.
    let (phase, mut i, mut wish) = held_at_the_edge();
    assert!(lift_of(&phase) > 0.0 && phase.holds_time());
    i.time_queued = true;
    i.neighbour = Some((east_of_alps(), YearsBp(0.0)));
    let (phase, actions, _, _) = hop_frame(phase, &mut i, &mut wish);
    assert!(has(&actions, |a| matches!(a, NavAction::SwapToNeighbour)));
    assert!(phase.holds_time());
}

#[test]
fn the_lift_takes_the_vertical_controls_and_the_hold_slows_the_fly() {
    let hop = |lift: f32, swapped: bool| NavPhase::Hopping {
        centre: east_of_alps(),
        edge: HopEdge::toward(alps(), east_of_alps()),
        lift_from_agl_m: HOP_AGL_M,
        swapped,
        elapsed_s: 0.0,
        lift,
        failed: false,
        turned_back: false,
    };
    // Kept inside the field except while a hop holds at full cover (fix round 1).
    let full = Some(FlyControls {
        horizontal: 1.0,
        vertical: true,
        confined: true,
    });
    assert_eq!(NavPhase::Flying.fly_controls(), full);
    assert_eq!(hop(0.0, false).fly_controls(), full);
    assert_eq!(
        hop(0.5, false).fly_controls(),
        Some(FlyControls {
            horizontal: 1.0,
            vertical: false,
            confined: true,
        })
    );
    assert_eq!(
        hop(1.0, false).fly_controls(),
        Some(FlyControls {
            horizontal: HOLD_DRIFT,
            vertical: false,
            confined: false,
        })
    );
    assert_eq!(hop(1.0, true).fly_controls(), full);
    assert_eq!(NavPhase::Globe.fly_controls(), None);
    // Still flying the region while the neighbour only preloads; not once the lift shows.
    assert!(NavPhase::Flying.is_flying() && hop(0.0, false).is_flying());
    assert!(!hop(0.01, false).is_flying() && !hop(1.0, true).is_flying());
    assert!(!NavPhase::Globe.is_flying());
    assert_eq!(
        NavPhase::Emerging {
            world: World::Region,
            elapsed_s: 0.0
        }
        .fly_controls(),
        None
    );
}

// Headless hops: the stand-in neighbour job, the region swap, the terrain scene and both
// cameras.

/// A stand-in for the region job's neighbour preload: a `Neighbour` request becomes a flat grid
/// in [`PendingRegion`] `after` updates later, or a `RegionPreloadFailed` with `fail`; a
/// `RegionPreloadCancelled` or another request drops it. `made` keeps a weak reference to every
/// grid it made, to count the live ones.
#[derive(Resource)]
struct FakeNeighbours {
    after: u32,
    fail: bool,
    /// The neighbours it makes fell back (a tile they need missing).
    fallback: bool,
    pending: Option<(RegionRequested, u32)>,
    made: Vec<std::sync::Weak<RegionGrid>>,
}

fn fake_neighbours(
    mut job: ResMut<FakeNeighbours>,
    mut requests: MessageReader<RegionRequested>,
    mut cancelled: MessageReader<RegionPreloadCancelled>,
    mut pending: ResMut<PendingRegion>,
    mut failed: MessageWriter<RegionPreloadFailed>,
) {
    if cancelled.read().count() > 0 {
        job.pending = None;
        pending.region = None;
    }
    for r in requests.read() {
        pending.region = None;
        job.pending = (r.purpose == RequestPurpose::Neighbour).then_some((*r, job.after));
    }
    let fail = job.fail;
    let Some((r, left)) = job.pending.as_mut() else {
        return;
    };
    if *left > 0 {
        *left -= 1;
        return;
    }
    let r = *r;
    job.pending = None;
    if fail {
        failed.write(RegionPreloadFailed {
            reason: "the stand-in failed".to_string(),
        });
        return;
    }
    let g = grid(r.era_dir, r.snapshot, HOP_GROUND_M);
    job.made.push(Arc::downgrade(&g));
    pending.region = Some(ActiveRegion {
        frame: RegionFrame {
            center: g.spec.center,
        },
        grid: g,
    });
    pending.fallback = job.fallback;
}

/// The worlds app with the stand-in neighbour job, flying in the Alps' region at the present
/// (its grid only weakly held here), the fly 140 km east of the centre facing east,
/// [`HOP_AGL_M`] up, with `W` held.
fn hop_app(after: u32, fail: bool, x_m: f32) -> (App, std::sync::Weak<RegionGrid>) {
    let mut app = worlds_app();
    app.insert_resource(FakeNeighbours {
        after,
        fail,
        fallback: false,
        pending: None,
        made: Vec::new(),
    })
    .add_systems(Update, fake_neighbours.in_set(crate::region::RegionSystems));
    let g = grid(alps(), YearsBp(0.0), HOP_GROUND_M);
    let weak = Arc::downgrade(&g);
    enter(&mut app, &g);
    drop(g);
    *app.world_mut().resource_mut::<FlyState>() = FlyState {
        pos_m: Vec3::new(x_m, HOP_GROUND_M + HOP_AGL_M, 0.0),
        yaw: -std::f32::consts::FRAC_PI_2,
        ..default()
    };
    app.world_mut()
        .resource_mut::<ButtonInput<KeyCode>>()
        .press(KeyCode::KeyW);
    app.world_mut().resource_mut::<DrawnFrames>().0.clear();
    (app, weak)
}

fn drawn(app: &App) -> Vec<Drawn> {
    app.world().resource::<DrawnFrames>().0.clone()
}

/// Points 6 and 7: the swap frame is consistent. The cloud is opaque on both sides; the terrain
/// scene takes the new grid on the swap frame itself; the fly is carried to the same place and
/// the chase camera starts at its pose; the fly's pose follows the camera on every frame.
#[test]
fn the_hop_swaps_worlds_under_the_cloud() {
    let (mut app, old) = hop_app(5, false, EDGE_M - 10_000.0);
    run_until(&mut app, 600, |a| swapped(&phase(a)));
    run_until(&mut app, 600, |a| phase(a) == NavPhase::Flying);
    let frames = drawn(&app);
    let swap = first(&frames, 0, swapped);
    let (before, at) = (&frames[swap - 1], &frames[swap]);
    assert_eq!((before.density, at.density), (1.0, 1.0));
    assert_eq!(before.scene_centre, Some(alps()));
    let new = at.scene_centre.expect("a scene");
    assert!(same_region(new, YearsBp(0.0), east_of_alps(), YearsBp(0.0)));
    assert!(frames.iter().all(|f| f.region_camera));
    assert_pose_follows_the_camera(&frames);
    // The same place on the sphere, give or take the swap frame's own flight (its controls are
    // the player's again).
    let place = |centre: RenderDir, f: &FlyState| {
        RegionFrame { center: centre }
            .local_to_dir(f64::from(f.pos_m.x), f64::from(f.pos_m.z))
            .0
    };
    let moved = place(alps(), &before.fly).angle_between(place(new, &at.fly)) * 6.371e6;
    let flown = crate::fly_camera::MAX_SPEED_MPS * STEP.as_secs_f32();
    assert!(moved <= f64::from(flown) + 1.0, "{moved} m");
    assert!(
        (at.fly.pos_m.x + NEIGHBOUR_PAST_EDGE_M as f32).abs() < 2.0 * flown,
        "{:?}",
        at.fly
    );
    // Its height above the ground kept (flat at the same height on both sides).
    assert!((at.fly.pos_m.y - before.fly.pos_m.y).abs() < 1.0);
    // The chase camera at its wanted pose: no glide from the old region's.
    let want = crate::fly_camera::region_chase_pose(&at.fly, |_, _| f64::from(HOP_GROUND_M));
    let want =
        Transform::from_translation(want.eye.as_vec3()).looking_at(want.target.as_vec3(), Vec3::Y);
    assert_transform_near(&at.eye, &want, "the region camera on the swap frame");
    // The old grid is gone; the new one is the region's and the scene's.
    assert!(old.upgrade().is_none(), "the old grid is still held");
    let active = app.world().resource::<ActiveRegion>();
    assert!(same_region(
        active.frame.center,
        YearsBp(0.0),
        new,
        YearsBp(0.0)
    ));
    assert!(app.world().resource::<PendingRegion>().region.is_none());
    assert_eq!(app.world().resource::<Seen>().left, 0);
    // Then the cloud clears.
    let flying = first(&frames, swap, |p| *p == NavPhase::Flying);
    assert!(
        frames[swap..flying]
            .windows(2)
            .all(|w| w[1].density <= w[0].density)
    );
    assert_eq!(frames[flying].density, 0.0);
}

/// The hop's memory (follow-up): the swap's staged replace draws the new water over the old
/// heights for one frame only, the swap frame, at density exactly 1; the cloud clears over the
/// complete new fields. The old region's ground is 300 m higher, so the heights tell apart.
#[test]
fn a_hops_staged_swap_draws_mixed_fields_only_under_the_opaque_cloud() {
    let (mut app, _) = hop_app(5, false, EDGE_M - 10_000.0);
    enter(&mut app, &grid(alps(), YearsBp(0.0), HOP_GROUND_M + 300.0));
    app.world_mut().resource_mut::<FlyState>().pos_m.y = HOP_GROUND_M + 300.0 + HOP_AGL_M;
    app.init_resource::<FieldFrames>()
        .add_systems(Last, record_fields);
    run_until(&mut app, 600, |a| swapped(&phase(a)));
    run_until(&mut app, 600, |a| phase(a) == NavPhase::Flying);
    let frames = app.world().resource::<FieldFrames>().0.clone();
    let swap = frames
        .iter()
        .position(|f| f.centre != alps())
        .expect("the swap");
    assert!(
        frames[swap].pending,
        "the swap frame starts the staged replace"
    );
    assert_mixed_fields_only_under_the_cloud(&frames, 1);
    assert!(
        frames[swap + 1].density < 1.0,
        "the cloud clears from the next frame"
    );
}

/// The time a hop has held at full cover, for the caption: only in the hold, before its end.
#[test]
fn only_a_hops_hold_counts_toward_the_caption() {
    let at = |lift: f32, swapped: bool, turned_back: bool| NavPhase::Hopping {
        centre: east_of_alps(),
        edge: HopEdge { axis: 0, side: 1.0 },
        lift_from_agl_m: HOP_AGL_M,
        swapped,
        elapsed_s: 2.5,
        lift,
        failed: turned_back,
        turned_back,
    };
    assert_eq!(at(1.0, false, false).hop_hold_s(), Some(2.5));
    // Lifting, or ended (the end's `elapsed_s` is the clear's), or not a hop: none.
    assert_eq!(at(0.6, false, false).hop_hold_s(), None);
    assert_eq!(at(1.0, true, false).hop_hold_s(), None);
    assert_eq!(at(1.0, false, true).hop_hold_s(), None);
    assert_eq!(NavPhase::Flying.hop_hold_s(), None);
}

/// Spec §6 "Holding", in the shell: a hold longer than `CAPTION_DELAY_S` at full cover shows
/// "Descending..." (a web hold can last up to 40 s at full white-out), which a neighbour job,
/// never entering `RegionState::Generating`, did not (the final review). The swap hides it.
#[test]
fn a_long_hop_hold_shows_the_caption() {
    // 1 km short of the edge, so the hold starts within a second; the neighbour is ready 4 s
    // (40 updates) after it is asked for.
    let (mut app, _) = hop_app(40, false, EDGE_M - 1_000.0);
    let shown = |app: &mut App| {
        let world = app.world_mut();
        let mut q = world.query_filtered::<&Node, With<DescendingCaption>>();
        q.single(world).unwrap().display == Display::Flex
    };
    let mut seen = Vec::new();
    for _ in 0..200 {
        app.update();
        let p = phase(&app);
        seen.push((p.hop_hold_s(), swapped(&p), shown(&mut app)));
        if p == NavPhase::Flying && seen.iter().any(|(_, sw, _)| *sw) {
            break;
        }
    }
    let held: Vec<_> = seen.iter().filter_map(|&(h, _, s)| Some((h?, s))).collect();
    assert!(
        held.iter().any(|&(h, _)| h > 2.0),
        "the hold never lasted: {seen:?}"
    );
    for &(h, s) in &held {
        // Exactly at the delay it may go either way (the frame's step lands there).
        if (f64::from(h) - CAPTION_DELAY_S).abs() > 1e-3 {
            assert_eq!(s, f64::from(h) > CAPTION_DELAY_S, "held {h} s: {seen:?}");
        }
    }
    // Hidden again once the hop has swapped.
    assert!(
        seen.iter().filter(|(_, sw, _)| *sw).all(|(_, _, s)| !s),
        "{seen:?}"
    );
    assert_eq!(phase(&app), NavPhase::Flying);
}

/// The final review: a neighbour with missing tiles falls back without a word (only timeouts
/// fail a neighbour), and its `RegionGenerated` came at the preload, so the fallback notice
/// never showed after a hop. The swap's `RegionEntered` carries the fallback, and the notice
/// shows once the neighbour is on screen; a neighbour that did not fall back shows none.
#[test]
fn a_fallback_neighbour_shows_the_notice_after_the_swap() {
    for fallback in [true, false] {
        let (mut app, _) = hop_app(5, false, EDGE_M - 10_000.0);
        app.world_mut().resource_mut::<FakeNeighbours>().fallback = fallback;
        let shown = |app: &mut App| {
            let world = app.world_mut();
            let mut q = world.query_filtered::<&Node, With<FallbackNotice>>();
            q.single(world).unwrap().display == Display::Flex
        };
        let mut before_swap = false;
        for _ in 0..600 {
            app.update();
            if swapped(&phase(&app)) {
                break;
            }
            before_swap |= shown(&mut app);
        }
        assert!(swapped(&phase(&app)), "never swapped");
        assert!(!before_swap, "shown before the swap");
        // Within a frame or two of the swap, and for FALLBACK_NOTICE_S.
        let mut frames = Vec::new();
        for _ in 0..((FALLBACK_NOTICE_S + 1.0) / STEP.as_secs_f64()) as usize {
            app.update();
            frames.push(shown(&mut app));
        }
        if fallback {
            assert!(frames[..2].iter().any(|&s| s), "{frames:?}");
            assert!(!frames.last().unwrap(), "{frames:?}");
        } else {
            assert!(frames.iter().all(|&s| !s), "{frames:?}");
        }
    }
}

/// Point 6: from the preload to the swap at most two grids live, with the terrain scene drawing;
/// the old one is released on the swap frame.
#[test]
fn a_hop_holds_two_grids_at_most_with_the_terrain_scene() {
    let (mut app, old) = hop_app(5, false, EDGE_M - 30_000.0);
    let mut swap_frame = None;
    for k in 0..600 {
        app.update();
        let made = &app.world().resource::<FakeNeighbours>().made;
        let live = made
            .iter()
            .chain([&old])
            .filter(|w| w.strong_count() > 0)
            .count();
        assert!(live <= 2, "frame {k}: {live} grids");
        let scene = app.world().resource::<crate::terrain::TerrainScene>();
        if swap_frame.is_none() && swapped(&phase(&app)) {
            swap_frame = Some(k);
            assert_eq!(made.len(), 1);
            assert!(Arc::ptr_eq(&scene.grid, &made[0].upgrade().unwrap()));
            assert!(
                old.upgrade().is_none(),
                "the old grid outlived the swap frame"
            );
            assert_eq!(app.world().resource::<CloudDensity>().0, 1.0);
        }
        if phase(&app) == NavPhase::Flying && swap_frame.is_some() {
            break;
        }
    }
    assert!(swap_frame.is_some(), "never swapped");
    assert_eq!(
        app.world()
            .resource::<FakeNeighbours>()
            .made
            .iter()
            .filter(|w| w.strong_count() > 0)
            .count(),
        1
    );
}

/// Review Focus 4, in the shell: a neighbour that fails turns the fly round, 10 km inside the
/// edge, under the opaque cloud; the region is kept and the cloud clears.
#[test]
fn a_failed_hop_turns_the_fly_round_in_its_region() {
    let (mut app, old) = hop_app(5, true, EDGE_M - 10_000.0);
    run_until(&mut app, 600, |a| {
        matches!(
            phase(a),
            NavPhase::Hopping {
                turned_back: true,
                ..
            }
        )
    });
    run_until(&mut app, 600, |a| phase(a) == NavPhase::Flying);
    let frames = drawn(&app);
    let turn = first(&frames, 0, |p| {
        matches!(
            p,
            NavPhase::Hopping {
                turned_back: true,
                ..
            }
        )
    });
    let (before, at) = (&frames[turn - 1], &frames[turn]);
    assert_eq!((before.density, at.density), (1.0, 1.0));
    // Facing west now, 10 km inside the east edge (less the frame's flight back in).
    let yaw = (at.fly.yaw - before.fly.yaw).rem_euclid(std::f32::consts::TAU);
    assert!((yaw - std::f32::consts::PI).abs() < 1e-4, "{yaw}");
    let x = at.fly.pos_m.x;
    assert!(
        x <= EDGE_M - TURN_BACK_INSIDE_M && x > EDGE_M - TURN_BACK_INSIDE_M - 1_100.0,
        "{x}"
    );
    assert_pose_follows_the_camera(&frames);
    // The same region, never left; clear again and flying back in.
    assert!(old.upgrade().is_some());
    assert!(frames.iter().all(|f| f.scene_centre == Some(alps())));
    assert_eq!(app.world().resource::<Seen>().left, 0);
    let end = frames.last().unwrap();
    assert_eq!(end.density, 0.0);
    assert!(end.fly.pos_m.x < x);
}

/// Point 5: a slider change mid-hop. During the lift it waits: the neighbour, generated at the
/// shown snapshot, is swapped to, and the change then applies once as a snapshot change. Before
/// the lift it ends the hop: the preload is cancelled and the change applies; nothing swaps.
#[test]
fn a_slider_change_mid_hop_never_swaps_to_a_stale_neighbour() {
    // During the lift.
    let (mut app, _) = hop_app(10, false, EDGE_M - 10_000.0);
    run_until(&mut app, 100, |a| lift_of(&phase(a)) > 0.0);
    set_time(&mut app, 100e6);
    let mut swapped_at = None;
    for _ in 0..600 {
        app.update();
        let p = phase(&app);
        if p.holds_time() {
            assert_eq!(years(&app), YearsBp(0.0), "{p:?}");
            assert_eq!(
                app.world().resource::<Navigation>().queued_time(),
                Some(YearsBp(100e6))
            );
        }
        if swapped_at.is_none() && swapped(&p) {
            let active = app.world().resource::<ActiveRegion>();
            assert_eq!(active.grid.spec.snapshot, YearsBp(0.0));
            swapped_at = Some(());
        }
        if p == NavPhase::Flying {
            break;
        }
    }
    assert!(swapped_at.is_some());
    assert_eq!(phase(&app), NavPhase::Flying);
    assert_eq!(years(&app), YearsBp(100e6), "applied once flying");
    app.update();
    assert!(matches!(phase(&app), NavPhase::ChangingSnapshot { .. }));
    let requests: Vec<_> = app
        .world()
        .resource::<Seen>()
        .requests
        .iter()
        .map(|r| (r.purpose, r.snapshot))
        .collect();
    assert_eq!(
        requests,
        vec![
            (RequestPurpose::Neighbour, YearsBp(0.0)),
            (RequestPurpose::Snapshot, YearsBp(100e6))
        ]
    );

    // Before the lift.
    let (mut app, _) = hop_app(10, false, EDGE_M - 45_000.0);
    run_until(&mut app, 20, |a| hopping(&phase(a)));
    set_time(&mut app, 100e6);
    app.update();
    assert_eq!(phase(&app), NavPhase::Flying);
    assert_eq!(app.world().resource::<Seen>().cancelled, 1);
    app.update();
    assert!(matches!(phase(&app), NavPhase::ChangingSnapshot { .. }));
    for _ in 0..40 {
        app.update();
        assert!(!hopping(&phase(&app)) || lift_of(&phase(&app)) == 0.0);
    }
    assert_eq!(app.world().resource::<Seen>().swaps, 0);
    let requests: Vec<_> = app
        .world()
        .resource::<Seen>()
        .requests
        .iter()
        .map(|r| (r.purpose, r.snapshot))
        .collect();
    assert_eq!(
        requests[..2],
        [
            (RequestPurpose::Neighbour, YearsBp(0.0)),
            (RequestPurpose::Snapshot, YearsBp(100e6))
        ]
    );
}

// ---------------------------------------------------------------------------------------------
// Fix round 1: the fly stays in the field while a hop ends

/// Records, over `frames`, any frame where the fly was outside the inner field while the cloud
/// was less than opaque.
fn outside_in_clear_air(frames: &[HopFrame], z: &[f32]) -> Vec<String> {
    frames
        .iter()
        .zip(z)
        .filter(|(f, z)| (f.x_m.abs() > EDGE_M || z.abs() > EDGE_M) && f.density < 1.0)
        .map(|(f, z)| format!("({}, {z}) at density {} in {:?}", f.x_m, f.density, f.phase))
        .collect()
}

/// After a failed neighbour turns it round, the player turns straight back out, boosting, and
/// every neighbour keeps failing: the fly never leaves the field under a cloud less than opaque,
/// and each try ends turned back inside it again.
#[test]
fn a_turned_back_fly_never_leaves_the_field_in_clear_air() {
    let (mut i, mut wish) = flying_east(EDGE_M - 10_000.0);
    let boosted_out = Vec2::X * 4.0 * crate::fly_camera::MAX_SPEED_MPS;
    let mut phase = NavPhase::Flying;
    let (mut frames, mut z) = (Vec::new(), Vec::new());
    let mut turns = 0;
    for _ in 0..3_000 {
        // Every neighbour fails as soon as it is asked for.
        i.preload_failed = matches!(
            phase,
            NavPhase::Hopping {
                swapped: false,
                turned_back: false,
                ..
            }
        );
        let (next, actions, _, density) = hop_frame(phase, &mut i, &mut wish);
        if has(&actions, |a| matches!(a, NavAction::TurnBack { .. })) {
            turns += 1;
            // The player turns straight back out, boosting.
            wish = boosted_out;
        }
        z.push(i.region_pos_m.y);
        frames.push(HopFrame {
            phase: next,
            actions,
            density,
            height_m: i.region_height_m,
            x_m: i.region_pos_m.x,
        });
        phase = next;
        if turns >= 3 {
            break;
        }
    }
    assert!(turns >= 3, "{turns} turn-backs, ending {phase:?}");
    let bad = outside_in_clear_air(&frames, &z);
    assert!(
        bad.is_empty(),
        "{} frames out in clear air: {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
    // Never farther out than the hold's drift.
    let max_x = frames.iter().map(|f| f.x_m).fold(f32::MIN, f32::max);
    assert!(max_x < EDGE_M + 2_000.0, "{max_x}");
    assert_eq!(i.region_centre, Some(alps()));
}

/// The fly after a swap boosts straight for the neighbour's north edge: it never crosses it in
/// clear air, and hops again through the cloud there.
#[test]
fn a_boosted_fly_after_a_swap_hops_again_through_the_cloud() {
    let (mut phase, mut i, _) = just_swapped(false);
    // Boosted at the speed cap, toward the north edge (−z).
    let mut wish = Vec2::NEG_Y * 4.0 * crate::fly_camera::MAX_SPEED_MPS;
    let mut z = Vec::new();
    let mut frames = Vec::new();
    for _ in 0..2_000 {
        if let NavPhase::Hopping {
            centre,
            swapped: false,
            turned_back: false,
            ..
        } = phase
        {
            i.neighbour = Some((centre, YearsBp(0.0)));
        }
        let x = i.region_pos_m.x;
        let (next, actions, _, density) = hop_frame(phase, &mut i, &mut wish);
        z.push(i.region_pos_m.y);
        let swapped_again = matches!(next, NavPhase::Hopping { swapped: true, .. })
            && has(&actions, |a| matches!(a, NavAction::SwapToNeighbour));
        frames.push(HopFrame {
            phase: next,
            actions,
            density,
            height_m: i.region_height_m,
            x_m: x,
        });
        phase = next;
        if swapped_again {
            break;
        }
    }
    assert!(
        frames
            .last()
            .is_some_and(|f| has(&f.actions, |a| matches!(a, NavAction::SwapToNeighbour))),
        "never hopped north: {:?}",
        frames.last().map(|f| f.phase)
    );
    let bad = outside_in_clear_air(&frames, &z);
    assert!(
        bad.is_empty(),
        "{} frames out in clear air: {:?}",
        bad.len(),
        &bad[..bad.len().min(5)]
    );
}

/// Minor 6: `Backspace` mid-hop climbs out from the cloud already there, and drops the
/// neighbour's job.
#[test]
fn backspace_mid_hop_cancels_the_preload_and_keeps_the_cloud() {
    let (mut i, mut wish) = flying_east(EDGE_M - 12_000.0);
    let frames = fly_hop(
        NavPhase::Flying,
        &mut i,
        &mut wish,
        100,
        |_, _, _| {},
        |f| f.density > 0.5,
    );
    let before = frames.last().unwrap();
    assert!(hopping(&before.phase) && before.density > 0.5);
    i.climb_key = true;
    let (next, actions, _, density) = frame(before.phase, &mut i);
    assert!(matches!(next, NavPhase::Climbing { .. }), "{next:?}");
    assert!(
        has(&actions, |a| matches!(a, NavAction::CancelPreload)),
        "{actions:?}"
    );
    assert!(density >= before.density, "{} -> {density}", before.density);
}

/// Minor 1: at a corner, a fly that stops flying out of the hop's edge and heads for the other
/// one retargets the neighbour, and a failure of the old one no longer counts.
#[test]
fn a_hop_retargets_at_a_corner_and_forgets_the_old_failure() {
    // Heading east 40 km short of the east edge and 45 km short of the north one.
    let (mut i, mut wish) = flying_east(EDGE_M - 40_000.0);
    i.region_pos_m.y = -(EDGE_M - 45_000.0);
    let (phase, actions, _, _) = hop_frame(NavPhase::Flying, &mut i, &mut wish);
    let east = actions
        .iter()
        .find_map(|a| match *a {
            NavAction::Preload(c) => Some(c),
            _ => None,
        })
        .expect("the east neighbour");
    assert!(local_of(alps(), east).0 > 240_000.0);
    // The east neighbour fails.
    i.preload_failed = true;
    let (phase, _, _, _) = hop_frame(phase, &mut i, &mut wish);
    i.preload_failed = false;
    assert!(
        matches!(phase, NavPhase::Hopping { failed: true, .. }),
        "{phase:?}"
    );
    // Turning north (−z): a new neighbour, past the north edge, and the failure forgotten.
    wish = Vec2::NEG_Y * 1_000.0;
    i.region_vel_mps = wish;
    let (phase, actions, _, _) = hop_frame(phase, &mut i, &mut wish);
    let north = actions
        .iter()
        .find_map(|a| match *a {
            NavAction::Preload(c) => Some(c),
            _ => None,
        })
        .expect("the north neighbour");
    assert!(
        local_of(alps(), north).1 < -240_000.0,
        "{:?}",
        local_of(alps(), north)
    );
    let NavPhase::Hopping { centre, failed, .. } = phase else {
        panic!("{phase:?}");
    };
    assert_eq!(centre, north);
    assert!(!failed);
    // And the hop goes on to the north edge and swaps there.
    let frames = fly_hop(
        phase,
        &mut i,
        &mut wish,
        2_000,
        |_, phase, i| {
            if let NavPhase::Hopping {
                centre,
                swapped: false,
                ..
            } = *phase
            {
                i.neighbour = Some((centre, YearsBp(0.0)));
            }
        },
        |f| swapped(&f.phase),
    );
    assert!(swapped(&frames.last().unwrap().phase));
    assert_eq!(i.region_centre, Some(north));
}

/// The shell's half of the fix: the fly camera keeps the fly inside the field, and pressing out
/// of an edge it is held at still starts a hop. A neighbour that always fails turns the fly
/// round; the player turns straight back out, boosting, twice: never out in clear air, turned
/// back under the cloud each time.
#[test]
fn a_failing_hop_never_lets_the_fly_out_in_clear_air() {
    let (mut app, _) = hop_app(3, true, EDGE_M - 10_000.0);
    let turned_back = |a: &App| {
        matches!(
            phase(a),
            NavPhase::Hopping {
                turned_back: true,
                ..
            }
        )
    };
    for round in 0..3 {
        run_until(&mut app, 2_000, turned_back);
        if round == 2 {
            break;
        }
        // Facing out again, boosting.
        app.world_mut().resource_mut::<FlyState>().yaw = -std::f32::consts::FRAC_PI_2;
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ShiftLeft);
        run_until(&mut app, 2_000, |a| !turned_back(a));
    }
    let frames = drawn(&app);
    let out: Vec<_> = frames
        .iter()
        .filter(|f| f.fly.pos_m.x.abs().max(f.fly.pos_m.z.abs()) > EDGE_M && f.density < 1.0)
        .map(|f| format!("{:?} at {}", f.fly.pos_m, f.density))
        .collect();
    assert!(
        out.is_empty(),
        "{} frames out in clear air: {:?}",
        out.len(),
        &out[..out.len().min(4)]
    );
    let turns = frames
        .windows(2)
        .filter(|w| {
            !matches!(
                w[0].phase,
                NavPhase::Hopping {
                    turned_back: true,
                    ..
                }
            ) && matches!(
                w[1].phase,
                NavPhase::Hopping {
                    turned_back: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(turns, 3);
    assert!(frames.iter().all(|f| f.scene_centre == Some(alps())));
    assert_pose_follows_the_camera(&frames);
}

// ---------------------------------------------------------------------------------------------
// Fix round 2: angled approaches, and the cloud up until the fly is back inside

/// Out of the east edge `deg` off straight, sliding south along it once held: `(x, z)`.
fn angled_out(deg: f32, speed: f32) -> Vec2 {
    let (s, c) = deg.to_radians().sin_cos();
    Vec2::new(c, s) * speed
}

/// Runs `phase` with `wish`, every neighbour ready once asked for (none while `failing`), for
/// at most `frames` frames or until a swap; returns the frames and the fly's `z` after each.
fn run_hops(
    mut phase: NavPhase,
    i: &mut NavInputs,
    wish: &mut Vec2,
    frames: usize,
    failing: bool,
) -> (Vec<HopFrame>, Vec<f32>) {
    let (mut out, mut z) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        if let NavPhase::Hopping {
            centre,
            swapped: false,
            turned_back: false,
            ..
        } = phase
        {
            i.neighbour = (!failing).then_some((centre, YearsBp(0.0)));
            i.preload_failed = failing;
        } else {
            i.preload_failed = false;
        }
        let (next, actions, _, density) = hop_frame(phase, i, wish);
        z.push(i.region_pos_m.y);
        let swap = has(&actions, |a| matches!(a, NavAction::SwapToNeighbour));
        out.push(HopFrame {
            phase: next,
            actions,
            density,
            height_m: i.region_height_m,
            x_m: i.region_pos_m.x,
        });
        phase = next;
        if swap {
            break;
        }
    }
    (out, z)
}

/// The re-review's Important: held at the field's edge, a fly pushing out at an angle slides
/// along it; that must still start a hop, from plain flying and after a turn-back.
#[test]
fn angled_approaches_to_a_held_edge_still_hop() {
    for deg in [10.0f32, 20.0, 30.0] {
        // Flying, held at the east edge (the end of a hop's end phase leaves it there).
        let mut wish = angled_out(deg, 1_000.0);
        // What the shell measures there is the slide along the edge; what it hands navigation
        // counts the push out of it.
        let at = Vec2::new(EDGE_M, 0.0);
        let slide = Vec2::new(0.0, wish.y);
        let mut i = NavInputs {
            region_pos_m: at,
            region_vel_mps: timefly_core::flight::hop::velocity_at_edges(at, slide, wish),
            ..in_alps(0.0, HOP_AGL_M)
        };
        let (frames, z) = run_hops(NavPhase::Flying, &mut i, &mut wish, 600, false);
        let first = frames
            .iter()
            .position(|f| has(&f.actions, |a| matches!(a, NavAction::Preload(_))));
        assert_eq!(first, Some(0), "{deg}°: no hop from the edge");
        assert!(
            frames.last().is_some_and(|f| swapped(&f.phase)),
            "{deg}°: never swapped: {:?}",
            frames.last().map(|f| f.phase)
        );
        let bad = outside_in_clear_air(&frames, &z);
        assert!(bad.is_empty(), "{deg}°: {:?}", &bad[..bad.len().min(3)]);

        // After a failed neighbour turned it round, straight back out at the angle.
        let (mut i, mut wish) = flying_east(EDGE_M - 10_000.0);
        let (frames, mut z) = run_hops(NavPhase::Flying, &mut i, &mut wish, 600, true);
        let mut frames = frames;
        let turn = frames
            .iter()
            .position(|f| has(&f.actions, |a| matches!(a, NavAction::TurnBack { .. })))
            .expect("turned back");
        frames.truncate(turn + 1);
        z.truncate(turn + 1);
        let mut phase = frames[turn].phase;
        wish = angled_out(deg, 1_000.0);
        let (after, after_z) = run_hops(phase, &mut i, &mut wish, 3_000, false);
        phase = after.last().unwrap().phase;
        assert!(
            after
                .iter()
                .any(|f| has(&f.actions, |a| matches!(a, NavAction::Preload(_)))),
            "{deg}°: no hop after the turn-back, ending {phase:?}"
        );
        assert!(swapped(&phase), "{deg}°: never swapped: {phase:?}");
        frames.extend(after);
        z.extend(after_z);
        let bad = outside_in_clear_air(&frames, &z);
        assert!(bad.is_empty(), "{deg}°: {:?}", &bad[..bad.len().min(3)]);
    }
}

/// The player turns to `wish` in the hold: the velocity navigation sees in that frame (past the
/// edge the keys count, `velocity_at_edges`).
fn turn_to(i: &mut NavInputs, wish: Vec2) {
    i.region_vel_mps =
        timefly_core::flight::hop::velocity_at_edges(i.region_pos_m, i.region_vel_mps, wish);
}

/// Holds past the east edge for `hold` frames at 10 km/s, no neighbour ready; returns the
/// phase and inputs there, the fly some km out.
fn held_past_the_edge(start_z: f32, hold: usize) -> (NavPhase, NavInputs) {
    let (mut i, _) = flying_east(EDGE_M - 15_000.0);
    i.region_pos_m.y = start_z;
    let mut wish = Vec2::X * 10_000.0;
    i.region_vel_mps = wish;
    let mut phase = NavPhase::Flying;
    let mut held = 0;
    for _ in 0..2_000 {
        let (next, _, _, density) = hop_frame(phase, &mut i, &mut wish);
        phase = next;
        if density == 1.0 && i.region_pos_m.x > EDGE_M {
            held += 1;
            if held >= hold {
                break;
            }
        }
    }
    assert!(i.region_pos_m.x > EDGE_M + 2_000.0, "{}", i.region_pos_m.x);
    (phase, i)
}

/// Minor 1: turning back out of the hold, or retargeting from it, keeps the cloud opaque
/// until the fly is back inside the field.
#[test]
fn the_cloud_stays_up_until_the_fly_is_back_inside() {
    // Straight back in (west); and north along the edge, retargeting to the north edge while
    // still out past the east one, then back in (west). (Turning back in at a slant is a turn
    // back, fix round 3: it ends the hop rather than retargeting.)
    for (start_z, back) in [(0.0, Vec2::NEG_X), (-(EDGE_M - 30_000.0), Vec2::NEG_Y)] {
        let (phase, mut i) = held_past_the_edge(start_z, 100);
        let mut wish = back * 10_000.0;
        turn_to(&mut i, wish);
        let (mut frames, mut z) = (Vec::new(), Vec::new());
        let mut p = phase;
        for _ in 0..6_000 {
            // Once retargeted north, back in.
            if matches!(p, NavPhase::Hopping { edge, .. } if edge.axis == 1) && wish.x == 0.0 {
                wish = Vec2::NEG_X * 10_000.0;
                turn_to(&mut i, wish);
            }
            let (next, actions, _, density) = hop_frame(p, &mut i, &mut wish);
            z.push(i.region_pos_m.y);
            frames.push(HopFrame {
                phase: next,
                actions,
                density,
                height_m: i.region_height_m,
                x_m: i.region_pos_m.x,
            });
            p = next;
            if i.region_pos_m.x < EDGE_M - 5_000.0 {
                break;
            }
        }
        assert!(i.region_pos_m.x < EDGE_M - 5_000.0, "{back}: never back in");
        let bad = outside_in_clear_air(&frames, &z);
        assert!(
            bad.is_empty(),
            "{back}: {} frames: {:?}",
            bad.len(),
            &bad[..bad.len().min(3)]
        );
        if back.y < 0.0 {
            assert!(
                frames
                    .iter()
                    .any(|f| has(&f.actions, |a| matches!(a, NavAction::Preload(_)))),
                "no retarget north"
            );
        }
        assert_eq!(i.region_centre, Some(alps()));
    }
}

/// The re-review's Important, in the shell: with the real fly camera sliding the fly along the
/// edge it pushes out of at 10°, 20° and 30° off straight, a hop starts and swaps, from plain
/// flying held at the edge and after a turn-back; never out in clear air.
#[test]
fn angled_approaches_hop_in_the_shell() {
    let clear_air = |frames: &[Drawn]| -> Vec<String> {
        frames
            .iter()
            .filter(|f| f.fly.pos_m.x.abs().max(f.fly.pos_m.z.abs()) > EDGE_M && f.density < 1.0)
            .map(|f| format!("{:?} at {}", f.fly.pos_m, f.density))
            .collect()
    };
    let turned_back = |a: &App| {
        matches!(
            phase(a),
            NavPhase::Hopping {
                turned_back: true,
                ..
            }
        )
    };
    for deg in [10.0f32, 20.0, 30.0] {
        let yaw = -std::f32::consts::FRAC_PI_2 - deg.to_radians();
        // Flying, held at the east edge.
        let (mut app, _) = hop_app(3, false, EDGE_M);
        app.world_mut().resource_mut::<FlyState>().yaw = yaw;
        run_until(&mut app, 600, |a| swapped(&phase(a)));
        let bad = clear_air(&drawn(&app));
        assert!(
            bad.is_empty(),
            "{deg}° from the edge: {:?}",
            &bad[..bad.len().min(3)]
        );

        // After a turn-back, straight back out at the angle.
        let (mut app, _) = hop_app(3, true, EDGE_M - 10_000.0);
        run_until(&mut app, 2_000, turned_back);
        app.world_mut().resource_mut::<FakeNeighbours>().fail = false;
        app.world_mut().resource_mut::<FlyState>().yaw = yaw;
        run_until(&mut app, 3_000, |a| swapped(&phase(a)));
        let frames = drawn(&app);
        let bad = clear_air(&frames);
        assert!(
            bad.is_empty(),
            "{deg}° after the turn-back: {:?}",
            &bad[..bad.len().min(3)]
        );
        assert_pose_follows_the_camera(&frames);
    }
}

// ---------------------------------------------------------------------------------------------
// Fix round 3: a turn back out of the hold never swaps, and lands facing inward

/// Flies `phase` with `wish` until the fly is `inside_m` inside the east edge and flying, at
/// most `frames` frames, with `each` adjusting the inputs first; returns the frames and `z`s.
fn back_in(
    mut phase: NavPhase,
    i: &mut NavInputs,
    wish: &mut Vec2,
    frames: usize,
    mut each: impl FnMut(&NavPhase, &mut NavInputs),
) -> (Vec<HopFrame>, Vec<f32>) {
    let (mut out, mut z) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        each(&phase, i);
        let (next, actions, _, density) = hop_frame(phase, i, wish);
        z.push(i.region_pos_m.y);
        out.push(HopFrame {
            phase: next,
            actions,
            density,
            height_m: i.region_height_m,
            x_m: i.region_pos_m.x,
        });
        phase = next;
        if phase == NavPhase::Flying && i.region_pos_m.abs().max_element() < EDGE_M - 1_000.0 {
            break;
        }
    }
    (out, z)
}

/// The re-review's Important: turning back out of the hold ends the hop. A neighbour that
/// becomes ready on the way back in is never swapped to; the fly stays in its region and is
/// back inside before the cloud clears.
#[test]
fn a_turn_back_out_of_the_hold_never_swaps() {
    let (phase, mut i) = held_past_the_edge(0.0, 100);
    let mut wish = Vec2::NEG_X * 10_000.0;
    turn_to(&mut i, wish);
    let (frames, z) = back_in(phase, &mut i, &mut wish, 3_000, |_, i| {
        // Ready on the way back.
        i.neighbour = Some((east_of_alps(), YearsBp(0.0)));
    });
    assert_eq!(
        frames.last().unwrap().phase,
        NavPhase::Flying,
        "never back to flying"
    );
    assert!(
        frames.iter().all(|f| !has(&f.actions, leaves)),
        "swapped or left"
    );
    assert!(
        frames[0].actions.contains(&NavAction::CancelPreload),
        "the turn did not cancel the preload: {:?}",
        frames[0].actions
    );
    assert_eq!(i.region_centre, Some(alps()));
    let bad = outside_in_clear_air(&frames, &z);
    assert!(bad.is_empty(), "{:?}", &bad[..bad.len().min(3)]);
}

/// A neighbour that fails while the fly is on its way back in, or while it holds flying along
/// the edge: it never ends up facing out.
#[test]
fn a_failure_on_the_way_back_lands_facing_inward() {
    // On the way back in, straight and slanting along the edge.
    for back in [Vec2::NEG_X, Vec2::new(0.0, 1.0)] {
        let (phase, mut i) = held_past_the_edge(0.0, 100);
        let mut wish = back * 10_000.0;
        turn_to(&mut i, wish);
        let (frames, z) = back_in(phase, &mut i, &mut wish, 6_000, |_, i| {
            i.preload_failed = true;
            // Along the edge, out there, it never comes back in by itself: once turned, fly on.
        });
        let turn = frames.iter().find_map(|f| {
            f.actions.iter().find_map(|a| match *a {
                NavAction::TurnBack { yaw, .. } => Some(yaw),
                _ => None,
            })
        });
        if let Some(yaw) = turn {
            assert!(
                (yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
                "{back}: turned to {yaw}, not west"
            );
        }
        // Facing (and flying) west, into the field.
        assert!(
            wish.x < 0.0 && wish.y.abs() < 1e-3 * wish.length(),
            "{back}: facing {wish}"
        );
        assert_eq!(
            frames.last().unwrap().phase,
            NavPhase::Flying,
            "{back}: stranded"
        );
        assert!(i.region_pos_m.x < EDGE_M, "{back}: {}", i.region_pos_m);
        let bad = outside_in_clear_air(&frames, &z);
        assert!(bad.is_empty(), "{back}: {:?}", &bad[..bad.len().min(3)]);
    }
}

/// Minor 1: past the east edge, turned north along it and retargeted to the north edge, the
/// north neighbour fails: the turn back brings the fly inside on both axes, facing in (south)
/// across the north edge, and the cloud waits for that.
#[test]
fn a_failed_corner_retarget_lands_inside_on_both_axes() {
    let (phase, mut i) = held_past_the_edge(-(EDGE_M - 30_000.0), 100);
    let mut wish = Vec2::NEG_Y * 10_000.0;
    turn_to(&mut i, wish);
    let mut retargeted = false;
    let (frames, z) = back_in(phase, &mut i, &mut wish, 6_000, |phase, i| {
        if let NavPhase::Hopping { edge, .. } = *phase {
            retargeted |= edge.axis == 1;
        }
        // The north neighbour fails once asked for.
        i.preload_failed = retargeted;
    });
    assert!(retargeted, "never retargeted north");
    let (turn, yaw) = frames
        .iter()
        .find_map(|f| {
            f.actions.iter().find_map(|a| match *a {
                NavAction::TurnBack { pos_m, yaw } => Some((pos_m, yaw)),
                _ => None,
            })
        })
        .expect("turned back");
    let inside = EDGE_M - TURN_BACK_INSIDE_M;
    assert_eq!(turn, Vec2::new(inside, -inside), "not inside on both axes");
    assert_eq!(
        yaw,
        std::f32::consts::PI,
        "not facing south, into the field"
    );
    let bad = outside_in_clear_air(&frames, &z);
    assert!(
        bad.is_empty(),
        "{} frames: {:?}",
        bad.len(),
        &bad[..bad.len().min(3)]
    );
    assert_eq!(frames.last().unwrap().phase, NavPhase::Flying);
}

// ---------------------------------------------------------------------------------------------
// Fix round 4: a turn back in the cloud can never strand the fly
//
// The hop's invariants, checked frame by frame on random play by
// `a_hop_keeps_its_invariants_whatever_the_player_does`:
// - I1: the fly is outside the inner field only in a hop's hold, drawn at density exactly 1;
// - I2: the white-out and a hop's end each last a bounded time, whatever the player and the
//   neighbour job do;
// - I3: a swap happens on a frame drawn opaque after one drawn opaque, to the neighbour asked
//   for and ready, while the player still flies out of the hop's edge;
// - I4: a failure or a turn back leaves the fly inside the field, moving or facing in.

/// The most frames at `dt_s` a hop may draw in a row at full cover (I2): its hold's budget,
/// and the frame that ends it.
fn max_whiteout_frames(dt_s: f32) -> usize {
    (HOLD_MAX_S / dt_s).ceil() as usize + 2
}

/// The most frames at `dt_s` a hop's end (the clear and the return to its height) may last
/// (I2): [`HOP_END_MAX_S`], whatever the ground does to the return (the final review).
fn max_ending_frames(dt_s: f32) -> usize {
    (HOP_END_MAX_S / dt_s).ceil() as usize + 1
}

/// An ended hop (swapped into the neighbour) in the Alps' region, `agl_m` above the ground,
/// returning to `back_to_m`.
fn hop_end(agl_m: f32, back_to_m: f32) -> (NavPhase, NavInputs) {
    let phase = NavPhase::Hopping {
        centre: east_of_alps(),
        edge: HopEdge { axis: 0, side: 1.0 },
        lift_from_agl_m: back_to_m,
        swapped: true,
        elapsed_s: 0.0,
        lift: 1.0,
        failed: false,
        turned_back: false,
    };
    (phase, in_alps(0.0, agl_m))
}

/// The final review: over steep ground under boosted flight `ReturnToAgl` may never get within
/// a metre, which held the slider and confined the fly at the next edge. The end is bounded:
/// past the clear it lasts at most `HOP_END_MAX_S`.
#[test]
fn a_hops_end_is_bounded_when_the_return_never_arrives() {
    // The shell never gets the fly nearer than 5 km to its height: the ground under it keeps
    // rising and falling faster than the return.
    let (mut phase, i) = hop_end(HOP_ALTITUDE_M as f32, 2_000.0);
    let i = NavInputs {
        region_height_m: i.region_ground_m + 7_000.0,
        ..i
    };
    let mut frames = 0;
    while phase != NavPhase::Flying {
        let (next, actions) = step_nav(phase, &i);
        assert!(
            has(&actions, |a| matches!(a, NavAction::ReturnToAgl { .. })),
            "{actions:?}"
        );
        phase = next;
        frames += 1;
        assert!(frames <= max_ending_frames(i.dt_s), "{phase:?}");
    }
    let ended = frames as f32 * i.dt_s;
    assert!(
        (ended - HOP_END_MAX_S).abs() <= i.dt_s + 1e-4,
        "{ended} s against {HOP_END_MAX_S} s"
    );
    // The cap is past the clear.
    assert!(HOP_END_MAX_S > CLOUD_CLEAR_S as f32);
}

/// The cap never cuts a return short over flat ground: the longest, from the hop's height down
/// to the lowest the fly flies (its clearance over the ground), arrives before it.
#[test]
fn a_return_over_flat_ground_arrives_before_the_cap() {
    let low = crate::fly_camera::CAMERA_CLEARANCE_M;
    let (mut phase, mut i) = hop_end(HOP_ALTITUDE_M as f32, low);
    let mut t = 0.0;
    while phase != NavPhase::Flying {
        phase = frame(phase, &mut i).0;
        t += i.dt_s;
        assert!(t < 60.0, "{phase:?}");
    }
    let agl = i.region_height_m - i.region_ground_m;
    assert!((agl - low).abs() <= 1.0, "ended {agl} m up, for {low} m");
    assert!(
        t < HOP_END_MAX_S - 1.0,
        "{t} s against the cap {HOP_END_MAX_S} s"
    );
}

/// The re-review's Important (fix round 4): a moment's turn back in the hold (a tap of `S`, a
/// yaw sweep past the perpendicular), then out again. The fly is never held in the white-out:
/// it is back in the field on the turn's frame, flying within the clear and the return to its
/// height, and its push out then hops again, through the cloud.
#[test]
fn a_moment_turned_back_in_the_hold_never_strands_the_fly() {
    for (back, turn_frames) in [
        (Vec2::NEG_X, 1),
        (Vec2::new(-1.0, 0.4).normalize(), 1),
        (Vec2::NEG_X, 3),
    ] {
        let (phase, mut i) = held_past_the_edge(0.0, 100);
        let dt = i.dt_s;
        let mut wish = back * 10_000.0;
        turn_to(&mut i, wish);
        let (mut frames, mut z) = (Vec::new(), Vec::new());
        let mut p = phase;
        let bound = max_ending_frames(dt) + turn_frames + 1;
        let mut again = None;
        for k in 0..bound + 200 {
            if k == turn_frames {
                // Out again.
                wish = Vec2::X * 10_000.0;
                turn_to(&mut i, wish);
            }
            if let NavPhase::Hopping {
                centre,
                swapped: false,
                turned_back: false,
                ..
            } = p
                && again.is_some()
            {
                // The new hop's neighbour is ready once asked for.
                i.neighbour = Some((centre, YearsBp(0.0)));
            }
            let (next, actions, _, density) = hop_frame(p, &mut i, &mut wish);
            z.push(i.region_pos_m.y);
            if again.is_none() && k > 0 && has(&actions, |a| matches!(a, NavAction::Preload(_))) {
                again = Some(k);
            }
            let swap = has(&actions, |a| matches!(a, NavAction::SwapToNeighbour));
            frames.push(HopFrame {
                phase: next,
                actions,
                density,
                height_m: i.region_height_m,
                x_m: i.region_pos_m.x,
            });
            p = next;
            if swap {
                break;
            }
        }
        // Bounded: flying, then hopping again, within the end's time.
        let flying = frames.iter().position(|f| f.phase == NavPhase::Flying);
        assert!(
            flying.is_some_and(|k| k <= bound),
            "{back} for {turn_frames}: stranded; flying at {flying:?} of {bound}, ending {:?} at \
             ({}, {}) density {}",
            frames.last().unwrap().phase,
            i.region_pos_m.x,
            i.region_pos_m.y,
            frames.last().unwrap().density
        );
        assert!(
            again.is_some_and(|k| k <= bound + 1),
            "{back} for {turn_frames}: never hopped again ({again:?})"
        );
        assert!(
            frames.last().is_some_and(|f| swapped(&f.phase)),
            "{back} for {turn_frames}: the push out never swapped: {:?}",
            frames.last().map(|f| f.phase)
        );
        // The turn ended the hop: no swap to the old neighbour, the preload dropped, and the fly
        // back in the field on that frame.
        let turn = &frames[0];
        assert!(
            turn.actions.contains(&NavAction::CancelPreload),
            "{back}: {:?}",
            turn.actions
        );
        assert!(
            frames[..again.unwrap()]
                .iter()
                .all(|f| !has(&f.actions, leaves)),
            "{back}: swapped or left before hopping again"
        );
        assert!(
            turn.x_m <= EDGE_M && z[0].abs() <= EDGE_M,
            "{back}: still out at ({}, {})",
            turn.x_m,
            z[0]
        );
        let bad = outside_in_clear_air(&frames, &z);
        assert!(bad.is_empty(), "{back}: {:?}", &bad[..bad.len().min(3)]);
    }
}

/// I2 without the neighbour job's help: a neighbour that never answers, or a player who keeps
/// retargeting at a corner (each retarget asks again), still ends the white-out within the
/// hold's budget, turned back inside and facing in.
#[test]
fn a_hold_ends_within_its_budget_whatever_the_job_and_the_player_do() {
    for zigzag in [false, true] {
        let start_z = if zigzag { -(EDGE_M - 20_000.0) } else { 0.0 };
        let (phase, mut i) = held_past_the_edge(start_z, 100);
        let dt = i.dt_s;
        let mut wish = Vec2::X * 10_000.0;
        turn_to(&mut i, wish);
        let (mut frames, mut z) = (Vec::new(), Vec::new());
        let mut p = phase;
        let mut asked = 0;
        for k in 0..max_whiteout_frames(dt) + max_ending_frames(dt) {
            if zigzag && k % 10 == 0 {
                // North along the east edge, then east again: each turn retargets.
                wish = if (k / 10) % 2 == 0 {
                    Vec2::NEG_Y
                } else {
                    Vec2::X
                } * 10_000.0;
                turn_to(&mut i, wish);
            }
            let (next, actions, _, density) = hop_frame(p, &mut i, &mut wish);
            asked += actions
                .iter()
                .filter(|a| matches!(a, NavAction::Preload(_)))
                .count();
            z.push(i.region_pos_m.y);
            frames.push(HopFrame {
                phase: next,
                actions,
                density,
                height_m: i.region_height_m,
                x_m: i.region_pos_m.x,
            });
            p = next;
            if p == NavPhase::Flying {
                break;
            }
        }
        if zigzag {
            assert!(asked >= 4, "the corner never retargeted: {asked} requests");
        }
        let run = frames
            .iter()
            .scan(0usize, |n, f| {
                *n = if f.density == 1.0 { *n + 1 } else { 0 };
                Some(*n)
            })
            .max()
            .unwrap_or(0);
        assert!(
            run <= max_whiteout_frames(dt),
            "zigzag {zigzag}: {run} frames of white-out, ending {:?}",
            frames.last().map(|f| f.phase)
        );
        let (pos, yaw) = frames
            .iter()
            .find_map(|f| {
                f.actions.iter().find_map(|a| match *a {
                    NavAction::TurnBack { pos_m, yaw } => Some((pos_m, yaw)),
                    _ => None,
                })
            })
            .expect("never turned back");
        assert!(pos.abs().max_element() < EDGE_M, "{pos}");
        let facing = Vec2::new(-yaw.sin(), -yaw.cos());
        let edge = frames
            .iter()
            .find_map(|f| match f.phase {
                NavPhase::Hopping {
                    edge,
                    turned_back: true,
                    ..
                } => Some(edge),
                _ => None,
            })
            .unwrap();
        assert!(edge.out(facing) < -0.99, "facing {facing} across {edge:?}");
        assert_eq!(p, NavPhase::Flying, "zigzag {zigzag}: never flying");
        assert_eq!(i.region_centre, Some(alps()));
        let bad = outside_in_clear_air(&frames, &z);
        assert!(bad.is_empty(), "{:?}", &bad[..bad.len().min(3)]);
    }
}

/// The hold's budget counts as the `LoadClock` does: a stalled frame (a hidden tab, a laptop
/// asleep) is one short frame, not a minute of white-out that turns the fly back.
#[test]
fn a_stalled_frame_never_spends_the_holds_budget() {
    let (phase, mut i) = held_past_the_edge(0.0, 100);
    let held = |p: &NavPhase| match *p {
        NavPhase::Hopping { elapsed_s, .. } => elapsed_s,
        _ => f32::NAN,
    };
    let before = held(&phase);
    i.dt_s = 60.0;
    let (next, actions) = step_nav(phase, &i);
    assert!(
        !has(&actions, |a| matches!(a, NavAction::TurnBack { .. })),
        "{actions:?}"
    );
    let spent = held(&next) - before;
    assert!(
        spent > 0.0 && spent <= crate::clock::LOAD_CLOCK_MAX_DELTA_S as f32,
        "{spent} s"
    );
}

/// The re-review's Important, in the shell: with the real fly camera, the fly holding past the
/// east edge faces west for one frame, then east again. It is never held in the white-out: back
/// in the field on the turn's frame, then flying and hopping again; never out in clear air.
#[test]
fn a_moment_turned_back_in_the_hold_never_strands_the_fly_in_the_shell() {
    // The neighbour stays unready through the hold.
    let (mut app, _) = hop_app(100_000, false, EDGE_M - 10_000.0);
    let east = -std::f32::consts::FRAC_PI_2;
    // Far enough out that a frame back in at full speed leaves it out.
    let out = |a: &App| a.world().resource::<FlyState>().pos_m.x > EDGE_M + 3_000.0;
    run_until(&mut app, 2_000, out);
    assert!(out(&app));
    let asked = |a: &App| {
        a.world()
            .resource::<Seen>()
            .requests
            .iter()
            .filter(|r| r.purpose == RequestPurpose::Neighbour)
            .count()
    };
    let before = asked(&app);
    app.world_mut().resource_mut::<FlyState>().yaw = east + std::f32::consts::PI;
    app.update();
    app.world_mut().resource_mut::<FlyState>().yaw = east;
    let turned = app.world().resource::<FlyState>().pos_m;
    let bound = max_ending_frames(STEP.as_secs_f32()) + 1;
    for _ in 0..bound {
        if asked(&app) > before {
            break;
        }
        app.update();
    }
    assert!(
        asked(&app) > before,
        "stranded: {:?} at {:?}, density {}",
        phase(&app),
        app.world().resource::<FlyState>().pos_m,
        app.world().resource::<CloudDensity>().0
    );
    assert!(turned.x <= EDGE_M, "still out after the turn: {turned:?}");
    let frames = drawn(&app);
    let bad: Vec<_> = frames
        .iter()
        .filter(|f| f.fly.pos_m.x.abs().max(f.fly.pos_m.z.abs()) > EDGE_M && f.density < 1.0)
        .map(|f| format!("{:?} at {}", f.fly.pos_m, f.density))
        .collect();
    assert!(bad.is_empty(), "{:?}", &bad[..bad.len().min(3)]);
    assert!(frames.iter().all(|f| f.scene_centre == Some(alps())));
    assert_eq!(app.world().resource::<Seen>().left, 0);
    assert_pose_follows_the_camera(&frames);
}

/// Plays `segments` (a wish, held for some frames) from flying at `start` in the Alps' region,
/// a hop's neighbour ready at once when `ready_for` its edge, until a swap; returns the frames,
/// the fly's `z` after each, and where the swap put the fly.
fn play(
    start: Vec2,
    segments: &[(Vec2, usize)],
    ready_for: impl Fn(HopEdge) -> bool,
) -> (Vec<HopFrame>, Vec<f32>, Option<Vec2>) {
    let mut i = in_alps(0.0, HOP_AGL_M);
    i.region_pos_m = start;
    let mut phase = NavPhase::Flying;
    let (mut frames, mut z) = (Vec::new(), Vec::new());
    for &(w, n) in segments {
        let mut wish = w;
        turn_to(&mut i, wish);
        for _ in 0..n {
            i.neighbour = match phase {
                NavPhase::Hopping {
                    centre,
                    edge,
                    swapped: false,
                    turned_back: false,
                    ..
                } if ready_for(edge) => Some((centre, YearsBp(0.0))),
                _ => None,
            };
            let (next, actions, _, density) = hop_frame(phase, &mut i, &mut wish);
            let swap = has(&actions, |a| matches!(a, NavAction::SwapToNeighbour));
            z.push(i.region_pos_m.y);
            frames.push(HopFrame {
                phase: next,
                actions,
                density,
                height_m: i.region_height_m,
                x_m: i.region_pos_m.x,
            });
            phase = next;
            if swap {
                return (frames, z, Some(i.region_pos_m));
            }
        }
    }
    (frames, z, None)
}

/// Found by the property (fix round 4): a neighbour asked for along an older heading is asked
/// for again, level with the fly, once the fly would come out more than a field's half from it
/// along the edge, so the swap lands it inside the neighbour's field (boosted south-west, then
/// turned south-east to slide down the east edge to the corner).
#[test]
fn a_swap_always_lands_inside_the_neighbours_field() {
    let segments = [
        (Vec2::new(-28_284.271, 28_284.271), 23),
        (Vec2::ZERO, 1),
        (Vec2::new(7_337.435, 6_794.266), 600),
    ];
    let (frames, z, landed) = play(Vec2::new(EDGE_M, -34_697.81), &segments, |_| true);
    let landed = landed
        .unwrap_or_else(|| panic!("never swapped, ending {:?}", frames.last().map(|f| f.phase)));
    assert!(
        landed.abs().max_element() <= EDGE_M,
        "landed outside the neighbour's field at {landed}"
    );
    assert!(
        preloads(&frames).len() >= 2,
        "asked once: {:?}",
        preloads(&frames)
    );
    let bad = outside_in_clear_air(&frames, &z);
    assert!(bad.is_empty(), "{:?}", &bad[..bad.len().min(3)]);
}

/// Found by the property (fix round 4): a fly held past the east edge 30 km from the south one,
/// turning back in while its hold drift still carries it south, turns back. It never retargets
/// to the west neighbour across the field, the edge its slanting heading reaches first.
#[test]
fn a_turn_back_never_retargets_across_the_field() {
    let mut i = in_alps(EDGE_M - 15_000.0, HOP_AGL_M);
    i.region_pos_m.y = 120_000.0;
    let mut wish = Vec2::new(10_000.0, 600.0);
    i.region_vel_mps = wish;
    let mut phase = NavPhase::Flying;
    for _ in 0..2_000 {
        phase = hop_frame(phase, &mut i, &mut wish).0;
        if i.region_pos_m.x > EDGE_M + 2_000.0 {
            break;
        }
    }
    assert!(lift_of(&phase) == 1.0 && i.region_pos_m.x > EDGE_M + 2_000.0);
    let mut wish = Vec2::new(-10_000.0, 600.0);
    turn_to(&mut i, wish);
    let (frames, z) = back_in(phase, &mut i, &mut wish, 3_000, |p, i| {
        if let NavPhase::Hopping { centre, .. } = *p {
            i.neighbour = Some((centre, YearsBp(0.0)));
        }
    });
    let turn = &frames[0];
    assert!(
        turn.actions.contains(&NavAction::CancelPreload)
            && has(&turn.actions, |a| matches!(
                a,
                NavAction::PlaceInside { .. }
            )),
        "not turned back: {:?}",
        turn.actions
    );
    assert!(preloads(&frames).is_empty(), "{:?}", preloads(&frames));
    assert!(frames.iter().all(|f| !has(&f.actions, leaves)));
    assert_eq!(frames.last().unwrap().phase, NavPhase::Flying);
    assert_eq!(i.region_centre, Some(alps()));
    let bad = outside_in_clear_air(&frames, &z);
    assert!(bad.is_empty(), "{:?}", &bad[..bad.len().min(3)]);
}

/// Found by the property (fix round 4), shrunk and replayed: held out past the south edge, the
/// fly retargets east while 54 km short of the east edge, with the east neighbour ready. The
/// swap waits until it would land the fly inside the neighbour's field.
#[test]
fn a_swap_waits_until_the_fly_would_land_inside() {
    let v = Vec2::new;
    let segments = [
        (v(10_000.0, 0.0), 9, false),
        (v(-9_999.648, -83.842_16), 42, false),
        (v(-0.000_437_113_9, 10_000.0), 10, false),
        (v(9_572.613, 2_892.245), 18, false),
        (v(6_297.324_7, 7_768.120_6), 22, false),
        (v(740.190_4, -672.397_34), 1, false),
        (v(0.0, 0.0), 1, false),
    ];
    let answers = vec![Answer::Ready(1), Answer::Never, Answer::Ready(1)];
    if let Err(e) = check_hop_invariants(v(EDGE_M - 35_503.605, 135_015.7), &segments, answers) {
        panic!("{e}");
    }
}

/// How the pure property's stand-in job answers a neighbour request.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// Ready this many frames after the request.
    Ready(u32),
    /// Failed this many frames after the request.
    Fail(u32),
    /// Never (a job that hangs).
    Never,
}

/// The pure property's stand-in neighbour job: the requests are answered as `answers` says, in
/// turn; a cancel or a newer request drops the one before, ready or not.
struct PureJob {
    answers: Vec<Answer>,
    next: usize,
    asked: Option<(RenderDir, Answer, u32)>,
}

impl PureJob {
    /// Before a frame: the answer due now, into the inputs.
    fn answer(&mut self, i: &mut NavInputs) {
        i.preload_failed = false;
        let Some((centre, answer, waited)) = self.asked.as_mut() else {
            return;
        };
        *waited += 1;
        match *answer {
            Answer::Ready(k) if *waited >= k => {
                i.neighbour = Some((*centre, YearsBp(0.0)));
                self.asked = None;
            }
            Answer::Fail(k) if *waited >= k => {
                i.preload_failed = true;
                self.asked = None;
            }
            _ => {}
        }
    }

    /// After a frame: its requests and cancels.
    fn hear(&mut self, actions: &[NavAction], i: &mut NavInputs) {
        for a in actions {
            match *a {
                NavAction::Preload(centre) => {
                    i.neighbour = None;
                    let answer = self.answers[self.next % self.answers.len()];
                    self.next += 1;
                    self.asked = Some((centre, answer, 0));
                }
                NavAction::CancelPreload => {
                    i.neighbour = None;
                    self.asked = None;
                }
                _ => {}
            }
        }
    }
}

/// A player's wish: mostly out of the east edge (where the hops start), else along a compass
/// point (along an edge exactly) or anywhere; at rest, cruising or boosted.
fn wish_strategy() -> impl Strategy<Value = Vec2> {
    use std::f32::consts::{FRAC_PI_3, FRAC_PI_4, TAU};
    let dir = prop_oneof![
        3 => -FRAC_PI_3..FRAC_PI_3,
        2 => (0u8..8).prop_map(|k| f32::from(k) * FRAC_PI_4),
        1 => 0.0f32..TAU,
    ];
    let speed = prop_oneof![
        1 => Just(0.0f32),
        2 => Just(1_000.0f32),
        3 => Just(10_000.0f32),
        1 => Just(40_000.0f32),
    ];
    (dir, speed).prop_map(|(a, s)| Vec2::new(a.cos(), a.sin()) * s)
}

fn answer_strategy() -> impl Strategy<Value = Answer> {
    prop_oneof![
        3 => (1u32..120).prop_map(Answer::Ready),
        2 => (1u32..120).prop_map(Answer::Fail),
        1 => Just(Answer::Never),
    ]
}

/// Plays `segments` (a wish, held for some frames, with climb or descend held or not) from
/// flying at `start` in the Alps' region, the neighbour job answering as `answers` says, then
/// holds the last wish long enough for any white-out to have ended; checks I1–I4 on every
/// frame.
fn check_hop_invariants(
    start: Vec2,
    segments: &[(Vec2, u32, bool)],
    answers: Vec<Answer>,
) -> Result<(), TestCaseError> {
    let mut i = in_alps(0.0, HOP_AGL_M);
    i.region_pos_m = start;
    let dt = i.dt_s;
    let (max_whiteout, max_ending) = (max_whiteout_frames(dt), max_ending_frames(dt));
    let tail = (
        segments.last().unwrap().0,
        (max_whiteout + max_ending) as u32 + 20,
        false,
    );
    let mut job = PureJob {
        answers,
        next: 0,
        asked: None,
    };
    let mut phase = NavPhase::Flying;
    let (mut whiteout, mut ending) = (0usize, 0usize);
    for (k, &(w, frames, vertical)) in segments.iter().chain([&tail]).enumerate() {
        let mut wish = w;
        turn_to(&mut i, wish);
        i.vertical_held = vertical;
        for f in 0..frames {
            job.answer(&mut i);
            let (before, seen, shown, ready) =
                (phase, i.region_vel_mps, i.shown_density, i.neighbour);
            let (next, actions, _, density) = hop_frame(phase, &mut i, &mut wish);
            job.hear(&actions, &mut i);
            phase = next;
            let at = format!(
                "segment {k} frame {f}: {before:?} -> {next:?}, at {} moving {seen}, density \
                 {density}, {actions:?}",
                i.region_pos_m
            );
            let outside = i.region_pos_m.abs().max_element() > EDGE_M;
            // I1.
            prop_assert!(!outside || density == 1.0, "I1, out in clear air: {at}");
            prop_assert!(
                !outside || next.fly_controls().is_some_and(|c| !c.confined),
                "I1, out but not in a hold: {at}"
            );
            // I2.
            whiteout = if density == 1.0 { whiteout + 1 } else { 0 };
            prop_assert!(
                whiteout <= max_whiteout,
                "I2, {whiteout} frames white: {at}"
            );
            let ends = matches!(
                next,
                NavPhase::Hopping { swapped: true, .. }
                    | NavPhase::Hopping {
                        turned_back: true,
                        ..
                    }
            );
            ending = if ends { ending + 1 } else { 0 };
            prop_assert!(ending <= max_ending, "I2, {ending} frames ending: {at}");
            // I3.
            if has(&actions, |a| matches!(a, NavAction::SwapToNeighbour)) {
                prop_assert!(shown == 1.0 && density == 1.0, "I3, not opaque: {at}");
                let NavPhase::Hopping { centre, edge, .. } = before else {
                    return Err(TestCaseError::fail(format!(
                        "I3, a swap out of a hop: {at}"
                    )));
                };
                prop_assert!(
                    ready.is_some_and(|(c, s)| same_region(c, s, centre, YearsBp(0.0))),
                    "I3, not the neighbour asked for: {at}"
                );
                prop_assert!(edge.out(seen) >= 0.0, "I3, swapped turning back: {at}");
            }
            prop_assert!(
                !has(&actions, |a| matches!(
                    a,
                    NavAction::Leave | NavAction::CancelRequest
                )),
                "left the region: {at}"
            );
            // I4.
            let edge = match next {
                NavPhase::Hopping { edge, .. } => Some(edge),
                _ => None,
            };
            for a in &actions {
                match *a {
                    NavAction::TurnBack { yaw, .. } => {
                        prop_assert!(!outside, "I4, turned back outside: {at}");
                        let facing = Vec2::new(-yaw.sin(), -yaw.cos());
                        prop_assert!(
                            edge.is_some_and(|e| e.out(facing) < -0.99),
                            "I4, turned back facing out: {at}"
                        );
                    }
                    NavAction::PlaceInside { .. } => {
                        prop_assert!(!outside, "I4, placed outside: {at}");
                        prop_assert!(
                            edge.is_some_and(|e| e.out(seen) < 0.0),
                            "I4, placed without turning back: {at}"
                        );
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// I1–I4 on every frame of random play near an edge: random wishes held for random spans
    /// (moments included), climb or descend held at random, and a neighbour job that is ready,
    /// fails or hangs after random delays (fix round 4). `quarter` turns the play by that many
    /// quarter turns, so each edge is the one first flown at.
    #[test]
    fn a_hop_keeps_its_invariants_whatever_the_player_does(
        from_edge in 0.0f32..60_000.0,
        along in -EDGE_M..EDGE_M,
        quarter in 0u8..4,
        segments in prop::collection::vec((wish_strategy(), 1u32..60, any::<bool>()), 1..30),
        answers in prop::collection::vec(answer_strategy(), 1..6),
    ) {
        // A quarter turn takes east (+x) to south (+z).
        let turn = |v: Vec2| (0..quarter).fold(v, |v, _| Vec2::new(-v.y, v.x));
        let segments: Vec<_> = segments.into_iter().map(|(w, n, v)| (turn(w), n, v)).collect();
        check_hop_invariants(turn(Vec2::new(EDGE_M - from_edge, along)), &segments, answers)?;
    }
}
