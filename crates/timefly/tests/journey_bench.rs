//! The journey bench, headless, on the tiny globe bake: the real region plugin, navigation,
//! globe flight and the fly camera, with the keyboard's own input plugin, and no window or
//! renderer. Time advances by a fixed step per update; one cycle (dive, two hops, climb).
//!
//! The Alps sit outside the fixture tiles, so every region falls back to the overview (a `warn`
//! each).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy::asset::AssetMetaCheck;
use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use timefly::bench::bench_exit;
use timefly::bench::journey::{JourneyBench, JourneyBenchConfig, JourneyBenchPlugin};
use timefly::data::{GlobeDataPlugin, GlobeLoadState};
use timefly::fly_camera::FlyCameraPlugin;
use timefly::globe::GlobeRenderStatus;
use timefly::globe_flight::GlobeFlightPlugin;
use timefly::region::navigate::RegionNavigationPlugin;
use timefly::region::{GenMode, RegionPlugin, RegionSettings};
use timefly::time::TimePlugin;

const STEP: Duration = Duration::from_micros(16_667);
const WALL_LIMIT: Duration = Duration::from_secs(240);

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

fn journey_app(config: JourneyBenchConfig) -> App {
    let mut app = App::new();
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
    ))
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
    let mut config = JourneyBenchConfig::new(60, true);
    config.cycles = 1;
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
        "hold (from the first frame drawn at cloud density exactly 1 to the first frame after it drawn below 1)",
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
    for (line, name) in holds.iter().zip(["dive", "hop 1", "hop 2", "climb"]) {
        assert!(line.starts_with(&format!("hold: {name} ")), "{line}");
        assert!(line.contains(" ms (cycle 1) -> "), "{line}");
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
    // At a fixed step every frame is 16.67 ms, so the flight frames pass.
    let flight = summary
        .lines()
        .find(|l| l.starts_with("flight (outside the cloud): frames "))
        .unwrap();
    assert!(flight.ends_with("-> PASS"), "{flight}");
    let pass = summary.contains("\nresult: PASS\n");
    assert_eq!(exit, vec![bench_exit(pass)], "{summary}");

    // Reported once.
    app.update();
    assert!(exits(&mut app).is_empty());
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
