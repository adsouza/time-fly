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
        hold_line(Transition::Dive, 1_234.4, "cycle 1", false),
        (
            "hold: dive 1234 ms (cycle 1) -> PASS".to_string(),
            Some(true)
        )
    );
    assert_eq!(
        hold_line(Transition::Hop(1), 3_000.0, "cycle 2, Japan", false),
        (
            "hold: hop 1 3000 ms (cycle 2, Japan) -> PASS".to_string(),
            Some(true)
        )
    );
    assert_eq!(
        hold_line(Transition::Hop(2), 3_000.4, "cycle 1", false),
        (
            "hold: hop 2 3000 ms (cycle 1) -> FAIL".to_string(),
            Some(false)
        )
    );
    assert_eq!(
        hold_line(Transition::Climb, 9_000.0, "cycle 3", true),
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
            // It steers in while it descends, so the dive lands near the route's start.
            assert!(
                arc <= DIVE_ACCURACY_DEG,
                "{name} at {fps} fps dived {arc:.2}° off"
            );
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
                let keys = region_keys(&NavPhase::Flying, fly.yaw, fly.pos_m.y, cruising, EAST_YAW);
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
                region_keys(&phase, yaw, CRUISE_AGL_M, true, EAST_YAW),
                [KeyCode::KeyW],
                "{phase:?}"
            );
        }
    }
    // Facing east: above the cruise it descends, without flying; once cruising, a valley that
    // puts it higher above the ground does not stop it.
    assert_eq!(
        region_keys(&NavPhase::Flying, EAST_YAW, 25_000.0, false, EAST_YAW),
        [KeyCode::ControlLeft]
    );
    assert_eq!(
        region_keys(&NavPhase::Flying, EAST_YAW, 25_000.0, true, EAST_YAW),
        [KeyCode::KeyW]
    );
    assert_eq!(
        region_keys(&NavPhase::Flying, EAST_YAW, CRUISE_AGL_M, false, EAST_YAW),
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
            region_keys(&phase, 0.0, REGION_EMERGE_TO_M as f32, false, EAST_YAW).is_empty(),
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

/// How far from a route's start the scripted globe flight dives (degrees): a corner or site 250
/// km away needs the dive within a fraction of the inner field's 150 km half-width.
const DIVE_ACCURACY_DEG: f64 = 0.5;

/// The neighbours of a route flown straight out from its start: hop `n`'s frame and centre.
fn hops_of(route: &JourneyRoute, n: u32) -> Vec<(RegionFrame, RenderDir)> {
    let mut frame = RegionFrame {
        center: RenderDir(dir(route.region.arg.lat_deg, route.region.arg.lon_deg)),
    };
    let heading = Vec2::new(-route.yaw.sin(), -route.yaw.cos());
    (0..n)
        .map(|_| {
            let centre = timefly_core::flight::hop::neighbour_centre(&frame, Vec2::ZERO, heading);
            frame = RegionFrame { center: centre };
            (frame, centre)
        })
        .collect()
}

#[test]
fn the_cube_corners_sit_at_35_degrees_and_odd_multiples_of_45() {
    for lat in [35.264_389_682_754_654, -35.264_389_682_754_654] {
        for lon in [45.0, 135.0, -45.0, -135.0] {
            assert!(corner_km(RenderDir(dir(lat, lon))) < 0.01, "{lat}, {lon}");
        }
    }
    // A face centre is as far from a corner as anything gets: acos(1 / √3) of arc.
    let far = (1.0 / 3f64.sqrt()).acos() * EARTH_RADIUS_M / 1000.0;
    for d in [DVec3::X, DVec3::NEG_Y, DVec3::Z] {
        assert!((corner_km(RenderDir(d)) - far).abs() < 1e-6);
    }
}

#[test]
fn the_routes_reach_what_they_are_chosen_for() {
    let config = JourneyBenchConfig::new(60, true);
    // Every route is flown; the Andes first and again last, so its memory compares with itself.
    let mut flown: Vec<usize> = config.journeys.clone();
    flown.sort_unstable();
    flown.dedup();
    assert_eq!(flown, (0..JOURNEY_ROUTES.len()).collect::<Vec<_>>());
    let last = config.journeys.len() as u32 - 1;
    assert_eq!(config.route(0).region.name, "Andes");
    assert_eq!(config.route(last).region, config.route(0).region);
    // The Andes route flies along the range, north: both hops stay near 70° W.
    for (_, centre) in hops_of(config.route(0), HOPS_PER_CYCLE) {
        let (_, lon) = timefly_core::geo::geo_to_latlon(timefly_core::geo::render_to_geo(centre));
        assert!((lon + 70.01).abs() < 0.05, "{lon}");
    }
    // A deep-time route; a coast and a cube corner, each on one hop.
    assert!(JOURNEY_ROUTES.iter().any(|r| r.region.arg.years >= 100.0e6));
    let all: Vec<(u32, Crossing)> = JOURNEY_ROUTES
        .iter()
        .flat_map(|r| r.crosses.iter().copied())
        .collect();
    assert!(all.iter().any(|&(_, c)| c == Crossing::Coast));
    assert!(all.iter().any(|&(_, c)| c == Crossing::Corner));
    // A corner hop centres its neighbour near a cube corner; every other hop's is far from one.
    for route in &JOURNEY_ROUTES {
        for (i, (_, centre)) in hops_of(route, HOPS_PER_CYCLE).into_iter().enumerate() {
            let hop = i as u32 + 1;
            let km = corner_km(centre);
            if route.crosses.contains(&(hop, Crossing::Corner)) {
                assert!(
                    km <= CORNER_REACH_KM - 30.0,
                    "{} hop {hop}: {km:.0} km",
                    route.region.name
                );
            } else {
                assert!(km > 300.0, "{} hop {hop}: {km:.0} km", route.region.name);
            }
        }
    }
    // A route with a site lands that hop on it: within 30 km of the neighbour's centre.
    let mut sites = Vec::new();
    for route in &JOURNEY_ROUTES {
        let Some(site) = route.site else { continue };
        let (frame, _) = hops_of(route, site.hop)[site.hop as usize - 1];
        let (x, z) = frame
            .dir_to_local(RenderDir(dir(site.lat_deg, site.lon_deg)))
            .unwrap();
        assert!(x.hypot(z) < 30_000.0, "{}: {x:.0}, {z:.0}", site.name);
        sites.push(site.name);
    }
    assert_eq!(sites, ["Grand Canyon", "Mt Fuji"]);
}

#[test]
fn journeys_are_named_and_their_memory_compared_by_route() {
    let config = JourneyBenchConfig::new(60, true);
    assert_eq!(config.tag(0), "cycle 1, Andes");
    assert_eq!(config.tag(4), "cycle 5, Andes");
    assert_eq!(config.tag(3), "cycle 4, Mt Fuji");
    let samples = [10, 20, 30, 40, 11];
    let grouped = config.by_route(&samples);
    assert_eq!(grouped[0], (0, vec![10, 11]));
    assert_eq!(grouped[3], (3, vec![40]));
    // One route: the old names and every cycle against the first.
    let single = JourneyBenchConfig::single(60, true, ALPS_ROUTE);
    assert_eq!(single.journeys, [0, 0, 0]);
    assert_eq!(single.tag(1), "cycle 2");
    assert_eq!(single.by_route(&[1, 2, 3]), vec![(0, vec![1, 2, 3])]);
    let chosen = JourneyBenchConfig::chosen(
        60,
        true,
        RegionArg {
            lat_deg: 1.0,
            lon_deg: 2.0,
            years: 0.0,
        },
    );
    assert_eq!(chosen.routes.len(), 1);
    assert_eq!(chosen.routes[0].yaw, EAST_YAW);
    assert_eq!(chosen.journeys.len(), JOURNEY_CYCLES as usize);
}

#[test]
fn route_checks_judge_the_first_hop() {
    let corner = RenderDir(dir(35.26, 135.0));
    let near = HopRecord {
        centre: Some(corner),
        land_share: Some(0.4),
        ..default()
    };
    assert!(crossing_check(Crossing::Corner, &near).0);
    assert!(crossing_check(Crossing::Coast, &near).0);
    let inland = HopRecord {
        centre: Some(RenderDir(dir(46.5, 10.0))),
        land_share: Some(1.0),
        ..default()
    };
    let (ok, text) = crossing_check(Crossing::Corner, &inland);
    assert!(!ok, "{text}");
    let (ok, text) = crossing_check(Crossing::Coast, &inland);
    assert!(!ok, "{text}");
    assert_eq!(
        text,
        "coast: land 100% of the neighbour's inner field (5-95%)"
    );
    assert!(!crossing_check(Crossing::Coast, &HopRecord::default()).0);
    assert!(!crossing_check(Crossing::Corner, &HopRecord::default()).0);
}

/// A grid whose inner field's heights follow `f(x, z)` in local metres.
fn shaped_grid(f: impl Fn(f64, f64) -> f32) -> (RegionGrid, RegionFrame) {
    use timefly_core::worldgen::generate::{Field, RegionSpec};
    use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};
    let field = |res: u32, size_m: f64| {
        let step = size_m / f64::from(res - 1);
        let elevation_m = (0..res)
            .flat_map(|r| (0..res).map(move |c| (c, r)))
            .map(|(c, r)| {
                f(
                    -size_m / 2.0 + f64::from(c) * step,
                    -size_m / 2.0 + f64::from(r) * step,
                )
            })
            .collect();
        Field {
            res,
            size_m,
            elevation_m,
            surface: vec![[0; 4]; (res * res) as usize],
        }
    };
    let centre = RenderDir(dir(36.1, -112.1));
    let grid = RegionGrid {
        spec: RegionSpec {
            inner_res: 601,
            apron_res: 65,
            ..RegionSpec::new(centre, timefly_core::deep_time::YearsBp(0.0), 1)
        },
        inner: field(601, REGION_SIZE_M),
        apron: field(65, APRON_SIZE_M),
    };
    (grid, RegionFrame { center: centre })
}

#[test]
fn sites_read_a_canyon_and_a_peak_from_the_grid() {
    let canyon = Site {
        hop: 1,
        name: "Grand Canyon",
        lat_deg: 36.1,
        lon_deg: -112.1,
        kind: SiteKind::Canyon,
    };
    // A 1,000 m trench 8 km wide running east-west through the centre, on a 2,000 m plateau.
    let (grid, frame) = shaped_grid(|_, z| if z.abs() < 4_000.0 { 1_000.0 } else { 2_000.0 });
    let text = read_site(&canyon, &grid, &frame).unwrap();
    assert!(text.contains("floor 1000 m"), "{text}");
    let deep: f32 = text
        .rsplit(": ")
        .next()
        .and_then(|t| t.strip_suffix(" m deep"))
        .and_then(|t| t.parse().ok())
        .unwrap_or_else(|| panic!("{text}"));
    assert!((deep - 1000.0).abs() <= 5.0, "{text}");
    assert!((land_share(&grid) - 1.0).abs() < 1e-6);
    // A cone 4,000 m high, 30 km across, in the sea.
    let peak = Site {
        kind: SiteKind::Peak,
        name: "Peak",
        ..canyon
    };
    let (grid, frame) = shaped_grid(|x, z| (4_000.0 - x.hypot(z) / 7.5) as f32);
    let text = read_site(&peak, &grid, &frame).unwrap();
    assert!(
        text.starts_with("4000 m at the site; the highest within 10 km 4000 m"),
        "{text}"
    );
    let share = land_share(&grid);
    assert!(share > 0.0 && share < 0.05, "{share}");
    // Outside the inner field: not read.
    let far = Site {
        lat_deg: 40.0,
        ..canyon
    };
    assert_eq!(read_site(&far, &grid, &frame), None);
}

/// A run's flight frames by journey: `(journey, frame ms)` runs of `n` frames.
fn flight_of(parts: &[(u32, f32, usize)]) -> (Vec<f32>, Vec<u32>) {
    let mut ms = Vec::new();
    let mut cycles = Vec::new();
    for &(cycle, frame, n) in parts {
        ms.extend(std::iter::repeat_n(frame, n));
        cycles.extend(std::iter::repeat_n(cycle, n));
    }
    (ms, cycles)
}

/// How the shell classifies frames requested `AutoNoVsync` on a 60 Hz display.
fn native_60hz(frames: &[f32]) -> Pacing {
    crate::bench::pacing(false, true, Some(60.0), frame_time_stats(frames).median_ms)
}

#[test]
fn flight_pacing_is_decided_once_per_run() {
    // An uncapped run (pooled median about 10 ms): the Andes and Fuji light, Pangaea heavy with
    // its median near the 16.67 ms interval. Pangaea must be judged by the run's uncapped rule
    // (p95 <= 16.67 ms, which it fails), not flip itself into the lenient display-paced rule.
    let config = JourneyBenchConfig::new(60, true);
    let (ms, cycles) = flight_of(&[
        (0, 9.0, 4_000),
        (1, 16.5, 1_500),
        (1, 18.0, 500),
        (2, 10.0, 3_000),
        (3, 8.0, 3_000),
        (4, 9.0, 4_000),
    ]);
    assert_eq!(native_60hz(&ms), Pacing::Uncapped);
    let verdicts = judge_flight_by_route(&config, &ms, &cycles, 60, native_60hz);
    let pangaea = &verdicts[1];
    assert_eq!(config.routes[pangaea.route].region.name, "Pangaea interior");
    assert_eq!(pangaea.pacing, Pacing::Uncapped, "pass {}", pangaea.pass);
    assert!(!pangaea.pass);
    for v in &verdicts {
        assert_eq!(v.pacing, Pacing::Uncapped);
    }
    assert!(verdicts.iter().enumerate().all(|(i, v)| i == 1 || v.pass));
    // A display-paced run judges every route by the paced rule.
    let (paced, cycles) = flight_of(&[(0, 16.67, 3_000), (1, 16.7, 2_000), (2, 16.6, 2_000)]);
    let config3 = JourneyBenchConfig {
        journeys: vec![0, 1, 2],
        ..JourneyBenchConfig::new(60, true)
    };
    let verdicts = judge_flight_by_route(&config3, &paced, &cycles, 60, native_60hz);
    assert!(
        verdicts[..3]
            .iter()
            .all(|v| v.pacing == Pacing::DisplayPaced && v.pass)
    );
}

#[test]
fn an_ungated_routes_flight_never_fails_the_run() {
    // Pangaea's flight is reported only (owner, 2026-11-25); every other route's gates.
    let gated: Vec<bool> = JOURNEY_ROUTES.iter().map(|r| r.flight_gated).collect();
    assert_eq!(gated, [true, false, true, true]);
    const { assert!(ALPS_ROUTE.flight_gated) };
    let verdict = |route, pass, gated| RouteFlight {
        route,
        frames: vec![10.0],
        pacing: Pacing::Uncapped,
        pass,
        gated,
    };
    // An ungated route's failure: the run passes.
    assert!(flight_by_route_passes(&[
        verdict(0, true, true),
        verdict(1, false, false),
        verdict(2, true, true),
    ]));
    // A gated route's failure: the run fails.
    assert!(!flight_by_route_passes(&[
        verdict(0, false, true),
        verdict(1, true, false),
    ]));
    // From the frames: a failing Pangaea is reported, the run still passes.
    let config = JourneyBenchConfig::new(60, true);
    let (ms, cycles) = flight_of(&[
        (0, 9.0, 4_000),
        (1, 18.0, 2_000),
        (2, 10.0, 3_000),
        (3, 8.0, 3_000),
        (4, 9.0, 4_000),
    ]);
    let verdicts = judge_flight_by_route(&config, &ms, &cycles, 60, native_60hz);
    assert!(!verdicts[1].pass && !verdicts[1].gated);
    assert!(flight_by_route_passes(&verdicts));
}
