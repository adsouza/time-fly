use bevy::math::{DVec3, Vec2, Vec3};
use timefly_core::flight::altitudes::{DIVE_ALTITUDE_KM, HOP_ALTITUDE_M, REGION_EMERGE_TO_M};
use timefly_core::flight::globe::{GlobeFlight, globe_fly_step};
use timefly_core::geo::{RenderDir, geo_to_render, latlon_to_geo};

use super::*;
use crate::fly_camera::{FlyInput, FlyState, MAX_SPEED_MPS, fly_speed, fly_step, keys_to_input};
use crate::globe_flight::{descent_ease, globe_input};
use crate::region::navigate::{HopEdge, World};

fn dir(lat: f64, lon: f64) -> DVec3 {
    geo_to_render(latlon_to_geo(lat, lon)).0.normalize()
}

fn held(keys: &[KeyCode]) -> ButtonInput<KeyCode> {
    let mut input = ButtonInput::default();
    for k in keys {
        input.press(*k);
    }
    input
}

fn hopping(swapped: bool, turned_back: bool, lift: f32) -> NavPhase {
    NavPhase::Hopping {
        centre: RenderDir(DVec3::X),
        edge: HopEdge { axis: 0, side: 1.0 },
        lift_from_agl_m: 8_000.0,
        swapped,
        elapsed_s: 0.0,
        lift,
        failed: turned_back,
        turned_back,
    }
}

#[test]
fn holds_run_from_the_first_opaque_frame_to_the_first_frame_below_it() {
    // The hold's definition: from the first frame drawn at density exactly 1.0 to the first
    // frame after it drawn below 1.0, in real seconds between those frames.
    let mut clock = HoldClock::default();
    let frames = [
        (0.0, 0.0),
        (0.5, 0.4),
        (0.9, 0.999_999_9),
        (1.0, 1.0),
        (1.2, 1.0),
        (1.5, 1.0),
        (1.6, 0.98),
        (1.7, 1.0),
        (2.0, 0.5),
    ];
    let ended: Vec<Option<f64>> = frames.iter().map(|&(t, d)| clock.observe(t, d)).collect();
    // Only the frame that leaves full cover ends it; a later opaque spell does not count.
    let at = ended.iter().position(Option::is_some).unwrap();
    assert_eq!(at, 6);
    assert!((ended[6].unwrap() - 600.0).abs() < 1e-9, "{ended:?}");
    assert_eq!(ended.iter().filter(|e| e.is_some()).count(), 1);
    assert!((clock.hold_ms().unwrap() - 600.0).abs() < 1e-9);

    // Never opaque, or still opaque: no hold yet. Just under 1 is not opaque.
    let mut clear = HoldClock::default();
    for (t, d) in [(0.0, 0.0), (1.0, 0.999_999_9), (2.0, 0.3)] {
        assert_eq!(clear.observe(t, d), None);
    }
    assert_eq!(clear.hold_ms(), None);
    let mut held_clock = HoldClock::default();
    held_clock.observe(0.0, 1.0);
    held_clock.observe(5.0, 1.0);
    assert_eq!(held_clock.hold_ms(), None);
    // One frame opaque, then clear: the hold is that frame's span.
    let mut blink = HoldClock::default();
    blink.observe(3.0, 1.0);
    assert!((blink.observe(3.016, 0.9).unwrap() - 16.0).abs() < 1e-6);
}

#[test]
fn a_journey_names_its_four_transitions() {
    let transitions = cycle_transitions(HOPS_PER_CYCLE);
    assert_eq!(
        transitions,
        [
            Transition::Dive,
            Transition::Hop(1),
            Transition::Hop(2),
            Transition::Climb
        ]
    );
    let labels: Vec<String> = transitions.iter().map(|t| t.label()).collect();
    assert_eq!(labels, ["dive", "hop 1", "hop 2", "climb"]);

    // Native gates each hold at 3 s; the web reports against 8 s.
    assert_eq!(
        hold_line(Transition::Dive, 1_234.4, 1, false),
        (
            "hold: dive 1234 ms (cycle 1) -> PASS".to_string(),
            Some(true)
        )
    );
    assert_eq!(
        hold_line(Transition::Hop(1), 3_000.0, 2, false),
        (
            "hold: hop 1 3000 ms (cycle 2) -> PASS".to_string(),
            Some(true)
        )
    );
    assert_eq!(
        hold_line(Transition::Hop(2), 3_000.4, 1, false),
        (
            "hold: hop 2 3000 ms (cycle 1) -> FAIL".to_string(),
            Some(false)
        )
    );
    assert_eq!(
        hold_line(Transition::Climb, 9_000.0, 3, true),
        (
            "hold: climb 9000 ms (cycle 3) -> reported only (web target <= 8000 ms)".to_string(),
            None
        )
    );
}

#[test]
fn swaps_are_read_from_the_phase_change() {
    let diving = NavPhase::Diving {
        target: RenderDir(DVec3::X),
        snapshot: timefly_core::deep_time::YearsBp(0.0),
        since_s: 0.0,
        started: true,
    };
    let into = |world| NavPhase::Emerging {
        world,
        elapsed_s: 0.0,
    };
    let climbing = NavPhase::Climbing {
        elapsed_s: 0.6,
        centre: None,
    };
    // The dive's swap, both hops' and the climb's.
    assert!(is_swap(&diving, &into(World::Region)));
    assert!(is_swap(
        &hopping(false, false, 1.0),
        &hopping(true, false, 1.0)
    ));
    assert!(is_swap(&climbing, &into(World::Globe)));
    // A failed dive comes out over the globe; a hop that turns back never swaps.
    assert!(!is_swap(&diving, &into(World::Globe)));
    assert!(!is_swap(
        &hopping(false, false, 1.0),
        &hopping(false, true, 1.0)
    ));
    for phase in [
        NavPhase::Globe,
        NavPhase::Flying,
        diving,
        hopping(false, false, 0.3),
        hopping(true, false, 1.0),
        climbing,
    ] {
        assert!(!is_swap(&phase, &phase), "{phase:?}");
    }
    assert!(!is_swap(&NavPhase::Flying, &hopping(false, false, 0.0)));
}

/// Steps the globe's fly as the shell does for held `keys` over `dt_s`.
fn globe_frame(f: GlobeFlight, keys: &[KeyCode], dt_s: f64) -> GlobeFlight {
    let (mut input, _) = globe_input(keys_to_input(&held(keys)), Vec2::ZERO, 0.0, dt_s);
    if input.climb < 0.0 {
        input.climb *= descent_ease(f.altitude_km);
    }
    globe_fly_step(f, input)
}

#[test]
fn the_globe_keys_fly_to_the_region_and_descend_to_the_dive() {
    let target = dir(46.5, 10.0);
    // M1's start (heading north), facing away from the region, and a climb's end east of it
    // heading east at the cloud top.
    let start = GlobeFlight::start(0.0, 0.0);
    let away = GlobeFlight {
        heading: -start.heading,
        ..start
    };
    let after_climb = {
        let f = GlobeFlight::start(46.5, 16.0);
        let east = DVec3::Y.cross(f.dir).normalize();
        GlobeFlight {
            heading: (east - f.dir * f.dir.dot(east)).normalize(),
            altitude_km: 2_500.0,
            ..f
        }
    };
    for fps in [60.0, 30.0] {
        for (name, start) in [("start", start), ("away", away), ("climb", after_climb)] {
            let mut f = start;
            let mut dived = None;
            for frame in 0..(fps as usize * 30) {
                let keys = globe_keys(&f, target);
                f = globe_frame(f, &keys, 1.0 / fps);
                if f.altitude_km < DIVE_ALTITUDE_KM {
                    dived = Some(frame);
                    break;
                }
            }
            let frame = dived.unwrap_or_else(|| panic!("{name} at {fps} fps never dived: {f:?}"));
            let arc = f.dir.dot(target).clamp(-1.0, 1.0).acos().to_degrees();
            assert!(arc <= ARRIVE_DEG, "{name} at {fps} fps dived {arc:.2}° off");
            assert!(
                (frame as f64) < 12.0 * fps,
                "{name} at {fps} fps took {frame} frames"
            );
        }
    }
    // Over the region, the keys only descend.
    let over = GlobeFlight::start(46.5, 10.0);
    assert_eq!(globe_keys(&over, target), [KeyCode::ControlLeft]);
}

#[test]
fn the_region_keys_descend_in_place_then_turn_east_and_fly_out_steadily() {
    let flat = |_: f32, _: f32| 0.0;
    for fps in [60.0_f32, 30.0] {
        for yaw in [0.0, 1.0, std::f32::consts::PI, -2.5, EAST_YAW] {
            // Where the fly emerges: the region's centre at `REGION_EMERGE_TO_M`.
            let mut fly = FlyState {
                pos_m: Vec3::new(0.0, REGION_EMERGE_TO_M as f32, 0.0),
                yaw,
                ..default()
            };
            // As the shell keeps it: down at the cruise once, cruising from then on.
            let mut cruising = false;
            let mut cruise_at = None;
            for frame in 0..(fps as usize * 45) {
                cruising |= fly.pos_m.y <= CRUISE_AGL_M;
                if cruising && cruise_at.is_none() {
                    cruise_at = Some(frame as f32 / fps);
                }
                if !cruising {
                    // The descent is straight down: no travel toward the edge yet.
                    assert_eq!((fly.pos_m.x, fly.pos_m.z), (0.0, 0.0), "{yaw} at {fps} fps");
                }
                let keys = region_keys(&NavPhase::Flying, fly.yaw, fly.pos_m.y, cruising);
                let k = keys_to_input(&held(&keys));
                let input = FlyInput {
                    move_local: k.move_local,
                    turn: k.turn,
                    boost: k.boost,
                    dt_s: 1.0 / fps,
                    confined: true,
                    ..default()
                };
                fly = fly_step(fly, input, flat);
            }
            // Down from 230 km in about 21 s, at the 10 km/s cap all the way.
            let cruise_at = cruise_at.unwrap_or_else(|| panic!("{yaw} at {fps} fps never cruised"));
            assert!(cruise_at < 23.0, "{yaw} at {fps} fps: {cruise_at} s");
            // At the east edge, near the middle of it, facing east, at the cruise height: under
            // the hop's lift, and still at the speed cap.
            let p = fly.pos_m;
            assert!(p.x >= 149_000.0, "{yaw} at {fps} fps: {p}");
            assert!(
                (CRUISE_AGL_M - 500.0..=CRUISE_AGL_M).contains(&p.y),
                "{yaw} at {fps} fps: {p}"
            );
            assert!(p.y < HOP_ALTITUDE_M as f32);
            assert_eq!(fly_speed(p.y), MAX_SPEED_MPS);
            assert!(p.z.abs() < 15_000.0, "{yaw} at {fps} fps: {p}");
            let off = (fly.yaw - EAST_YAW + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            assert!(off.abs().to_degrees() <= YAW_TOLERANCE_DEG, "{off}");
        }
    }
    // Through a hop, only forward: never a turn, which could point the wish back in.
    for phase in [
        hopping(false, false, 0.0),
        hopping(false, false, 0.5),
        hopping(false, false, 1.0),
        hopping(true, false, 1.0),
    ] {
        for yaw in [EAST_YAW, EAST_YAW + 0.04, 0.0] {
            assert_eq!(
                region_keys(&phase, yaw, CRUISE_AGL_M, true),
                [KeyCode::KeyW],
                "{phase:?}"
            );
        }
    }
    // Facing east: above the cruise it descends, without flying; once cruising, a valley that
    // puts it higher above the ground does not stop it.
    assert_eq!(
        region_keys(&NavPhase::Flying, EAST_YAW, 25_000.0, false),
        [KeyCode::ControlLeft]
    );
    assert_eq!(
        region_keys(&NavPhase::Flying, EAST_YAW, 25_000.0, true),
        [KeyCode::KeyW]
    );
    assert_eq!(
        region_keys(&NavPhase::Flying, EAST_YAW, CRUISE_AGL_M, false),
        [KeyCode::KeyW]
    );
    // Nothing while navigation moves the fly.
    for phase in [
        NavPhase::Emerging {
            world: World::Region,
            elapsed_s: 0.5,
        },
        NavPhase::Climbing {
            elapsed_s: 0.1,
            centre: None,
        },
        NavPhase::Globe,
    ] {
        assert!(
            region_keys(&phase, 0.0, REGION_EMERGE_TO_M as f32, false).is_empty(),
            "{phase:?}"
        );
    }
}

#[test]
fn hop_memory_is_gated_on_the_peak_natively_and_on_growth_on_the_web() {
    let mib = |m: u64| Some(m << 20);
    // Natively: the worst peak over its own in-region baseline, at most +64 MiB (the owner's
    // ruling of 2026-11-25; the measured breakdown is in `docs/testing/m3/performance.md`).
    let hops = [
        (mib(800), mib(830), mib(810)),
        (mib(810), mib(855), mib(812)),
    ];
    assert_eq!(hop_memory_gate(&hops, false), Some((true, 45 << 20)));
    assert_eq!(
        hop_memory_gate(&[(mib(800), mib(864), None)], false),
        Some((true, 64 << 20)),
        "exactly +64 MiB"
    );
    assert_eq!(
        hop_memory_gate(&[(mib(800), mib(865), mib(800))], false),
        Some((false, 65 << 20))
    );
    // A peak below the baseline (memory freed) is fine.
    assert_eq!(
        hop_memory_gate(&[(mib(800), mib(790), mib(790))], false),
        Some((true, -(10 << 20)))
    );
    // The web: the first hop may grow the linear memory; no hop after it may.
    let web = [
        (mib(64), None, mib(96)),
        (mib(96), None, mib(96)),
        (mib(96), None, mib(96)),
    ];
    assert_eq!(hop_memory_gate(&web, true), Some((true, 0)));
    let grows = [(mib(64), None, mib(96)), (mib(96), None, mib(100))];
    assert_eq!(hop_memory_gate(&grows, true), Some((false, 4 << 20)));
    // Not judged: no hops, a missing sample, or on the web a single hop.
    assert_eq!(hop_memory_gate(&[], false), None);
    assert_eq!(hop_memory_gate(&[(None, mib(1), mib(1))], false), None);
    assert_eq!(hop_memory_gate(&[(mib(64), None, mib(96))], true), None);
    assert_eq!(
        hop_memory_gate(&[(mib(64), None, mib(96)), (mib(96), None, None)], true),
        None
    );
}

#[test]
fn a_hop_started_again_keeps_one_record_and_records_its_lift() {
    let mut bench = JourneyBench::new(JourneyBenchConfig::new(60, false));
    bench.stage = Stage::Region {
        cycle: 0,
        hops: 0,
        since: 0.0,
        cruising: true,
    };
    let hop = |lift: f32, swapped: bool| NavPhase::Hopping {
        centre: RenderDir(DVec3::X),
        edge: HopEdge { axis: 0, side: 1.0 },
        lift_from_agl_m: 19_800.0,
        swapped,
        elapsed_s: 0.0,
        lift,
        failed: false,
        turned_back: false,
    };
    // Preloads, turns back before the lift, preloads again, lifts, swaps, returns.
    let frames = [
        (NavPhase::Flying, 0.0, 19_800.0),
        (hop(0.0, false), 0.0, 19_800.0),
        (NavPhase::Flying, 0.0, 19_800.0),
        (hop(0.0, false), 0.0, 19_800.0),
        (hop(0.5, false), 0.8, 40_000.0),
        (hop(1.0, false), 1.0, 60_000.0),
        (hop(1.0, true), 1.0, 60_000.0),
        (hop(1.0, true), 0.6, 30_000.0),
        (NavPhase::Flying, 0.0, 19_900.0),
    ];
    let mut before = NavPhase::Flying;
    for (i, (phase, density, height_m)) in frames.into_iter().enumerate() {
        let frame = Frame {
            before,
            phase,
            density,
            frame_ms: 16.0,
            now: i as f64 / 60.0,
            swapped: is_swap(&before, &phase),
            height_m,
        };
        record_frame(&mut bench, 0, frame);
        before = phase;
    }
    // One hop, numbered as its hold is, with the lift it made.
    let hops = bench.hops();
    assert_eq!(hops.len(), 1, "{hops:?}");
    assert_eq!((hops[0].cycle, hops[0].hop), (0, 1));
    assert_eq!(hops[0].from_agl_m, Some(19_800.0));
    assert_eq!(hops[0].peak_height_m, Some(60_000.0));
    let holds: Vec<Transition> = bench.holds.iter().map(|h| h.transition).collect();
    assert_eq!(holds, [Transition::Hop(1)]);
}
