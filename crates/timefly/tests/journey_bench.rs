//! The journey bench, headless, on the tiny globe bake: the real region plugin, navigation,
//! globe flight and the fly camera, with the keyboard's own input plugin, and no window or
//! renderer. Time advances by a fixed step per update; one cycle (dive, two hops, climb).
//!
//! Holds are judged at [`HOLD_LIMIT_HEADLESS_MS`], not the native 3 s
//! ([`JourneyBenchConfig::headless`]): at a fixed step a hold counts updates while the region
//! job and the continental pass run in wall time, so its length here follows the machine's load,
//! not the game. The real bench (`--journey-bench`) keeps the native gate.
//!
//! The Alps sit outside the fixture tiles, so every region falls back to the overview (a `warn`
//! each).

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bevy::asset::AssetMetaCheck;
use bevy::input::InputPlugin;
use bevy::math::DVec3;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use timefly::bench::bench_exit;
use timefly::bench::journey::{
    ALPS_ROUTE, CRUISE_AGL_M, Crossing, HOLD_LIMIT_HEADLESS_MS, JourneyBench, JourneyBenchConfig,
    JourneyBenchPlugin, JourneyRoute, WEST_YAW,
};
use timefly::bench::region::BenchRegion;
use timefly::data::{GlobeDataPlugin, GlobeLoadState};
use timefly::fly_camera::FlyCameraPlugin;
use timefly::globe::GlobeRenderStatus;
use timefly::globe_flight::GlobeFlightPlugin;
use timefly::hydro::{HydroPlugin, HydroSettings};
use timefly::region::navigate::RegionNavigationPlugin;
use timefly::region::{GenMode, RegionPlugin, RegionSettings};
use timefly::time::TimePlugin;
use timefly_core::flight::altitudes::HOP_ALTITUDE_M;
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::hydrology::LatticeSizes;

const STEP: Duration = Duration::from_micros(16_667);
const WALL_LIMIT: Duration = Duration::from_secs(240);
/// Continental pass lattices small enough for a test.
const HYDRO_SIZES: LatticeSizes = LatticeSizes {
    recent_n: 16,
    deep_n: 8,
};

fn tiny_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets")
}

/// Every `AppExit` written, read in `Last` (with `MinimalPlugins`, message buffers swap only
/// after a fixed update, so `iter_current_update_messages` can show stale messages).
#[derive(Resource, Default)]
struct Exits(Vec<AppExit>);

fn record_exits(mut exits: ResMut<Exits>, mut reader: MessageReader<AppExit>) {
    exits.0.extend(reader.read().cloned());
}

fn exits(app: &mut App) -> Vec<AppExit> {
    std::mem::take(&mut app.world_mut().resource_mut::<Exits>().0)
}

/// One journey app at a time. A hold is timed in fixed steps while the region job and the
/// continental pass run on threads in wall time, so concurrent apps lengthen each other's holds:
/// with the pass on, a dive's hold here is about 2.3 s alone but passed the 3 s native gate (an
/// early stop since M3 Task 15) with this binary's six tests in parallel, and read 3.0-3.1 s
/// serially at load averages of 10-16. Serializing keeps the apps from slowing each other; the
/// headless hold limit ([`journey_app`]) covers load from outside the binary.
static ONE_APP_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Held by a journey app for its life (one app per test), so the next test's app waits for it.
struct OneAppAtATime(#[allow(dead_code)] MutexGuard<'static, ()>);

/// A headless journey app over `config`, judged as a headless run
/// ([`JourneyBenchConfig::headless`]: holds at [`HOLD_LIMIT_HEADLESS_MS`]).
fn journey_app(config: JourneyBenchConfig) -> App {
    let config = config.headless();
    // A failed test poisons the lock; the next one runs regardless.
    let turn = ONE_APP_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut app = App::new();
    app.insert_non_send(OneAppAtATime(turn));
    app.add_plugins((
        MinimalPlugins,
        InputPlugin,
        AssetPlugin {
            file_path: tiny_root().to_string_lossy().into_owned(),
            meta_check: AssetMetaCheck::Never,
            ..default()
        },
        ImagePlugin::default(),
        StatesPlugin,
        GlobeDataPlugin::default(),
        RegionPlugin,
    ))
    .add_plugins((
        TimePlugin,
        RegionNavigationPlugin,
        GlobeFlightPlugin,
        FlyCameraPlugin,
        JourneyBenchPlugin(config),
        HydroPlugin,
    ))
    .insert_resource(HydroSettings {
        enabled: true,
        sizes: HYDRO_SIZES,
    })
    .insert_resource(RegionSettings {
        inner_res: 129,
        apron_res: 65,
        mode: GenMode::Parallel,
    })
    .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
    .init_resource::<Exits>()
    .add_systems(Last, record_exits)
    // Stands in for the globe's renderer.
    .insert_resource(GlobeRenderStatus::LoadingData);
    app
}

#[test]
fn a_journey_run_reaches_the_summary() {
    let mut config = JourneyBenchConfig::single(60, true, ALPS_ROUTE);
    config.journeys.truncate(1);
    let mut app = journey_app(config);
    let deadline = Instant::now() + WALL_LIMIT;
    let mut exit = Vec::new();
    while app.world().resource::<JourneyBench>().summary().is_none() {
        app.update();
        if *app.world().resource::<State<GlobeLoadState>>().get() == GlobeLoadState::Ready {
            app.insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 });
        }
        exit.extend(exits(&mut app));
        assert!(Instant::now() < deadline, "the bench did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    let summary = app
        .world()
        .resource::<JourneyBench>()
        .summary()
        .unwrap()
        .to_string();
    println!("{summary}");
    assert!(
        summary.starts_with("===== Time Fly journey bench summary ====="),
        "{summary}"
    );
    assert!(
        summary.ends_with("===== end journey bench summary ====="),
        "{summary}"
    );
    // The script ran to its end: no stage failed or timed out.
    assert!(!summary.contains("result: FAIL ("), "{summary}");
    for line in [
        "bake: manifest v2, sha256 ",
        "region: Alps (46.5, 10 at 0 years), 1 cycle of a dive, 2 hops and a climb",
        "hold (from the first frame drawn at cloud density exactly 1 to the first frame after it drawn below 1): headless gate <= 60000 ms each (fixed-step test; the native gate is 3000 ms)\n",
        "\nhold: dive ",
        "\nhold: hop 1 ",
        "\nhold: hop 2 ",
        "\nhold: climb ",
        "target: 60 fps",
        "\nflight (outside the cloud): frames ",
        "\nswap worst frame (",
        "\nhop memory peak (",
        "\njourney memory cycles",
        "\nresult: ",
    ] {
        assert!(summary.contains(line), "missing {line:?} in\n{summary}");
    }
    // One hold line per transition, in the order they happened, each with its cycle.
    let holds: Vec<&str> = summary
        .lines()
        .filter(|l| l.starts_with("hold: "))
        .collect();
    assert_eq!(holds.len(), 4, "{summary}");
    // At a fixed step, every gate passes: a FAIL here is a bench or game fault. Holds are judged
    // at the headless limit, which the machine's load cannot reach.
    assert_eq!(HOLD_LIMIT_HEADLESS_MS, 60_000.0);
    for (line, name) in holds.iter().zip(["dive", "hop 1", "hop 2", "climb"]) {
        assert!(line.starts_with(&format!("hold: {name} ")), "{line}");
        assert!(line.ends_with(" ms (cycle 1) -> PASS"), "{line}");
    }
    // Every swap was seen: the dive's, both hops' and the climb's.
    let swaps = summary
        .lines()
        .find(|l| l.starts_with("swap worst frame ("))
        .unwrap();
    for name in ["dive ", "hop 1 ", "hop 2 ", "climb "] {
        assert!(swaps.contains(name), "{swaps}");
    }
    assert_eq!(swaps.matches(" ms (cycle 1)").count(), 4, "{swaps}");
    assert!(swaps.ends_with("-> PASS"), "{swaps}");
    // Both hops flew from the cruise, under the cloud bank, so each lift rose to it and the fly
    // came back down to the cruise after the swap, ready to lift again.
    let bench = app.world().resource::<JourneyBench>();
    let hops = bench.hops();
    assert_eq!(hops.len(), 2, "{hops:?}");
    for (i, hop) in hops.iter().enumerate() {
        assert_eq!((hop.cycle, hop.hop), (0, i as u32 + 1), "{hop:?}");
        let from = hop.from_agl_m.expect("the lift began");
        // At the cruise: once down to it the fly holds its height, so ground falling away
        // along the leg adds a little.
        assert!(from <= CRUISE_AGL_M + 1_000.0, "{hop:?}");
        assert!(from < HOP_ALTITUDE_M as f32, "{hop:?}");
        let peak = hop.peak_height_m.expect("a height in the hop");
        assert!(peak >= HOP_ALTITUDE_M as f32 - 1.0, "{hop:?}");
    }
    assert!(summary.contains("\nhop lift ("), "{summary}");
    // At a fixed step every frame is 16.67 ms, so the flight frames pass.
    let flight = summary
        .lines()
        .find(|l| l.starts_with("flight (outside the cloud): frames "))
        .unwrap();
    assert!(flight.ends_with("-> PASS"), "{flight}");
    assert!(summary.contains("\nresult: PASS\n"), "{summary}");
    assert_eq!(exit, vec![bench_exit(true)], "{summary}");

    // Reported once.
    app.update();
    assert!(exits(&mut app).is_empty());
}

/// Runs the bench to its summary.
fn run_to_summary(app: &mut App) -> (String, Vec<AppExit>) {
    let deadline = Instant::now() + WALL_LIMIT;
    let mut exit = Vec::new();
    while app.world().resource::<JourneyBench>().summary().is_none() {
        app.update();
        if *app.world().resource::<State<GlobeLoadState>>().get() == GlobeLoadState::Ready {
            app.insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 });
        }
        exit.extend(exits(app));
        assert!(Instant::now() < deadline, "the bench did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    let summary = app.world().resource::<JourneyBench>().summary().unwrap();
    (summary.to_string(), exit)
}

#[test]
fn a_route_set_flies_each_route_its_own_way_and_compares_like_with_like() {
    // The Alps east, then a second route west from the same start, then the Alps again.
    let alps = ALPS_ROUTE;
    let west = JourneyRoute {
        region: BenchRegion {
            name: "Alps west",
            ..alps.region
        },
        yaw: WEST_YAW,
        heading: "west",
        ..alps
    };
    let config = JourneyBenchConfig {
        routes: vec![alps, west],
        journeys: vec![0, 1, 0],
        ..JourneyBenchConfig::new(60, true)
    };
    let mut app = journey_app(config);
    let (summary, exit) = run_to_summary(&mut app);
    println!("{summary}");
    assert!(!summary.contains("result: FAIL ("), "{summary}");
    assert!(summary.contains("\nroutes: Alps (46.5, 10 at 0 years) east, Alps west (46.5, 10 at 0 years) west; 3 journeys of a dive, 2 hops and a climb: Alps, Alps west, Alps\n"), "{summary}");
    for tag in ["(cycle 1, Alps)", "(cycle 2, Alps west)", "(cycle 3, Alps)"] {
        let holds = summary
            .lines()
            .filter(|l| l.starts_with("hold: ") && l.contains(tag))
            .count();
        assert_eq!(holds, 4, "{tag} in\n{summary}");
    }
    // Flight is judged route by route, each on its own frames.
    for route in ["Alps", "Alps west"] {
        let line = summary
            .lines()
            .find(|l| l.starts_with(&format!("  flight, {route}: frames ")))
            .unwrap_or_else(|| panic!("{route} in\n{summary}"));
        assert!(line.ends_with(" rule -> PASS"), "{line}");
    }
    // One pacing for the run, every route judged under it.
    let rule = summary
        .lines()
        .find(|l| l.starts_with("flight by route (the run's "))
        .unwrap_or_else(|| panic!("{summary}"));
    assert!(
        rule.ends_with(" rule for every route; ungated routes reported only) -> PASS"),
        "{rule}"
    );
    // Memory is compared route by route: the Alps against itself; the west route once.
    let cycles = summary
        .lines()
        .find(|l| l.starts_with("journey memory cycles ("))
        .unwrap_or_else(|| panic!("{summary}"));
    assert!(
        cycles.contains("each route against its own first visit"),
        "{cycles}"
    );
    assert!(cycles.contains("; Alps west 1: "), "{cycles}");
    assert!(cycles.contains("Alps: worst of visits 2-2 is "), "{cycles}");
    assert!(!cycles.contains("Alps west: worst"), "{cycles}");
    // The west route's hops went west, the others east: each hop's neighbour lies on the
    // route's side of its start.
    let bench = app.world().resource::<JourneyBench>();
    let hops = bench.hops();
    assert_eq!(hops.len(), 6, "{hops:?}");
    let start = geo_to_render(latlon_to_geo(46.5, 10.0)).0;
    let east = DVec3::Y.cross(start).normalize();
    for hop in hops {
        let centre = hop.centre.expect("the hop's centre").0;
        let along = centre.dot(east);
        let westward = hop.cycle == 1;
        assert_eq!(along < 0.0, westward, "{hop:?}");
        assert!(hop.land_share.is_some(), "{hop:?}");
    }
    assert!(summary.contains("\nresult: PASS\n"), "{summary}");
    assert_eq!(exit, vec![bench_exit(true)], "{summary}");
}

/// The Alps east, then the same route checked for a cube corner its first hop never comes near
/// (a route check, gated), then the Alps again: the first failed gate is journey 2's first hop.
fn failing_at_journey_2(keep_going: bool) -> JourneyBenchConfig {
    let corner = JourneyRoute {
        region: BenchRegion {
            name: "Alps corner",
            ..ALPS_ROUTE.region
        },
        crosses: &[(1, Crossing::Corner)],
        ..ALPS_ROUTE
    };
    JourneyBenchConfig {
        routes: vec![ALPS_ROUTE, corner],
        journeys: vec![0, 1, 0],
        keep_going,
        ..JourneyBenchConfig::new(60, true)
    }
}

#[test]
fn a_failed_gate_stops_the_journey_bench() {
    let mut app = journey_app(failing_at_journey_2(false));
    let (summary, exit) = run_to_summary(&mut app);
    println!("{summary}");
    // Journey 1 flew whole; journey 2 ended with its first hop, whose route check failed.
    let hops: Vec<(u32, u32)> = app
        .world()
        .resource::<JourneyBench>()
        .hops()
        .iter()
        .map(|h| (h.cycle, h.hop))
        .collect();
    assert_eq!(hops, [(0, 1), (0, 2), (1, 1)], "{summary}");
    let stopped = summary
        .lines()
        .find(|l| l.starts_with("stopped early at stop 2 of 3, journey 2 (Alps corner): route check: hop 1 (cycle 2, Alps corner): cube corner "))
        .unwrap_or_else(|| panic!("no stopped-early line in\n{summary}"));
    assert!(
        stopped.ends_with(
            " km from the neighbour's centre (<= 150 km) FAIL; --keep-going runs every stop"
        ),
        "{stopped}"
    );
    for line in [
        "\non a failed gate: stop (--keep-going runs every stop)\n",
        "\nstops run: 2 of 3: journey 1 (Alps), journey 2 (Alps corner)\n",
        "\nstops skipped: 1: journey 3 (Alps)\n",
        "\nresult: FAIL\n",
    ] {
        assert!(summary.contains(line), "missing {line:?} in\n{summary}");
    }
    // Nothing of journey 3, and journey 2 stopped before its second hop.
    assert!(!summary.contains("(cycle 3, Alps)"), "{summary}");
    assert!(
        !summary
            .lines()
            .any(|l| l.starts_with("hold: hop 2 ") && l.contains("(cycle 2, ")),
        "{summary}"
    );
    assert_eq!(exit, vec![bench_exit(false)], "{summary}");
    app.update();
    assert!(exits(&mut app).is_empty());
}

#[test]
fn keep_going_runs_every_journey() {
    let mut app = journey_app(failing_at_journey_2(true));
    let (summary, exit) = run_to_summary(&mut app);
    println!("{summary}");
    assert_eq!(app.world().resource::<JourneyBench>().hops().len(), 6);
    let holds = summary
        .lines()
        .filter(|l| l.starts_with("hold: ") && l.contains("(cycle 3, Alps)"))
        .count();
    assert_eq!(holds, 4, "{summary}");
    let checks = summary
        .lines()
        .find(|l| l.starts_with("route checks ("))
        .unwrap_or_else(|| panic!("{summary}"));
    assert!(checks.ends_with("-> FAIL"), "{checks}");
    assert!(
        summary.contains("\non a failed gate: keep going (--keep-going)\n"),
        "{summary}"
    );
    assert!(!summary.contains("stopped early"), "{summary}");
    assert!(summary.contains("\nresult: FAIL\n"), "{summary}");
    assert_eq!(exit, vec![bench_exit(false)], "{summary}");
}

#[test]
fn a_journey_run_reports_the_continental_pass() {
    let mut config = JourneyBenchConfig::single(60, true, ALPS_ROUTE);
    config.journeys.truncate(1);
    let mut app = journey_app(config);
    let (summary, _) = run_to_summary(&mut app);
    let line = summary
        .lines()
        .find(|l| l.starts_with("continental pass (0 ka): "))
        .unwrap_or_else(|| panic!("no 0 ka pass line in\n{summary}"));
    assert!(
        line.ends_with(" slices on native (target <= 1000 ms, reported)"),
        "{line}"
    );
    // No renderer here, so no GPU timestamps.
    assert!(summary.contains("\ngpu time: unavailable\n"), "{summary}");
}

#[test]
fn a_failed_globe_ends_the_journey_bench() {
    let mut app = journey_app(JourneyBenchConfig::new(60, true));
    app.insert_resource(GlobeRenderStatus::Failed("no adapter".into()));
    app.update();
    let summary = app
        .world()
        .resource::<JourneyBench>()
        .summary()
        .expect("a summary")
        .to_string();
    assert!(
        summary.contains("result: FAIL (the globe failed: no adapter)"),
        "{summary}"
    );
    assert_eq!(exits(&mut app), vec![bench_exit(false)]);
}
