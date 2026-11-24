//! The region bench, headless, on the tiny globe bake and its four fixture tiles: the real
//! region plugin and navigation, no window and no renderer. Time advances by a fixed step per
//! update, the path plays in 6 s and the memory cycles are cut to two.
//!
//! The bench regions sit outside the fixture tiles, so they fall back to the overview (a `warn`
//! each; in the tiny timeline even 250 Ma leans on the ETOPO-backed bridge frame).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use timefly::args::RegionArg;
use timefly::bench::bench_exit;
use timefly::bench::region::banner::BenchBannerText;
use timefly::bench::region::{BENCH_REGIONS, RegionBench, RegionBenchConfig, RegionBenchPlugin};
use timefly::data::{GlobeDataPlugin, GlobeLoadState};
use timefly::globe::GlobeRenderStatus;
use timefly::region::navigate::RegionNavigationPlugin;
use timefly::region::{GenMode, RegionPlugin, RegionSettings};
use timefly::terrain::TerrainOptions;
use timefly::time::TimePlugin;

const STEP: Duration = Duration::from_micros(16_667);
const WALL_LIMIT: Duration = Duration::from_secs(120);

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

fn bench_app(config: RegionBenchConfig) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
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
    .init_resource::<ButtonInput<KeyCode>>()
    .add_plugins((
        TimePlugin,
        RegionNavigationPlugin,
        RegionBenchPlugin(config),
    ))
    .insert_resource(RegionSettings {
        inner_res: 129,
        apron_res: 65,
        mode: GenMode::Parallel,
    })
    .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
    .init_resource::<Exits>()
    .init_resource::<BannerTexts>()
    .add_systems(Last, (record_exits, record_banner))
    // Stands in for the globe's renderer.
    .insert_resource(GlobeRenderStatus::LoadingData);
    app
}

/// Every distinct text the bench banner showed, in order.
#[derive(Resource, Default)]
struct BannerTexts(Vec<String>);

fn record_banner(
    mut seen: ResMut<BannerTexts>,
    texts: Query<&Text, (With<BenchBannerText>, Changed<Text>)>,
) {
    for t in &texts {
        if seen.0.last() != Some(&t.0) {
            seen.0.push(t.0.clone());
        }
    }
}

fn exits(app: &mut App) -> Vec<AppExit> {
    std::mem::take(&mut app.world_mut().resource_mut::<Exits>().0)
}

#[test]
fn region_bench_reports_all_regions() {
    let mut config = RegionBenchConfig::new(60, true);
    config.path_secs = 6.0;
    config.cycles = 2;
    config.timeout_secs = 1.0e6;
    let mut app = bench_app(config);
    let deadline = Instant::now() + WALL_LIMIT;
    let mut exit = Vec::new();
    while app.world().resource::<RegionBench>().summary().is_none() {
        app.update();
        if *app.world().resource::<State<GlobeLoadState>>().get() == GlobeLoadState::Ready {
            app.insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 });
        }
        exit.extend(exits(&mut app));
        assert!(Instant::now() < deadline, "the bench did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    let bench = app.world().resource::<RegionBench>();
    let reports = bench.reports();
    assert_eq!(reports.len(), BENCH_REGIONS.len(), "{reports:#?}");
    for (report, region) in reports.iter().zip(&BENCH_REGIONS) {
        let title = format!("===== Time Fly region bench: {} (", region.name);
        assert!(report.starts_with(&title), "{report}");
        assert!(report.ends_with("===== end region bench ====="), "{report}");
        for line in [
            "bake: manifest v2, sha256 ",
            "generation: ",
            " ms; tiles fetched ",
            "load (request to first path frame",
            "coast: ",
            "memory (",
            "target: 60 fps",
            "flight (75 s path): frames ",
            "  low pass: frames ",
            "  apron sweep: frames ",
            "  high pass: frames ",
            "result: ",
        ] {
            assert!(report.contains(line), "missing {line:?} in\n{report}");
        }
        assert!(!report.contains("result: FAIL ("), "{report}");
        // 6 s of path at 60 updates a second.
        assert!(report.contains("flight (75 s path): frames 36"), "{report}");
    }
    assert!(
        reports[0].contains("first-region worst frame (R11"),
        "{}",
        reports[0]
    );
    // None of the bench regions has a fixture tile; each reports its first-entry generation
    // (gated on the web only).
    for report in reports {
        assert!(report.contains("tiles fetched 0 (0 bytes)"), "{report}");
        assert!(
            report.contains("first-entry generation: reported only natively"),
            "{report}"
        );
        // Host stalls are reported for the load window and the flight, none here.
        assert!(
            report.contains(
                "host stalls in the load window (frames > 1000 ms; page paused or throttled?): 0"
            ),
            "{report}"
        );
        assert!(
            report.contains("host stalls in the flight (frames > 1000 ms"),
            "{report}"
        );
    }
    // The globe pan into each panned region (ruling R25): 8 s at 60 updates a second, and the
    // same snapshot as the region before it, so no frame is fetched again.
    for (report, region) in reports.iter().zip(&BENCH_REGIONS) {
        let pan = report.contains("globe pan from ");
        assert_eq!(pan, region.pan_from_previous, "{report}");
        if pan {
            assert!(
                report.contains("(8 s, great circle): frames 48"),
                "{report}"
            );
            assert!(report.contains("frames fetched 0;"), "{report}");
            assert!(report.contains("host stalls in the pan (frames > 1000 ms"));
        }
    }

    let summary = bench.summary().unwrap();
    assert!(
        summary.starts_with("===== Time Fly region bench summary ====="),
        "{summary}"
    );
    for line in [
        "regions (R14 frame rate): Pangaea interior PASS, Red Sea PASS, Himalayas PASS, Sundaland PASS, Japan PASS, Beringia PASS, Rockies ice margin PASS, Panama PASS",
        "first-region worst frame (R11",
        "warm generation (2 re-entries of Himalayas",
        "first entries (cold tiles; reported only natively): Pangaea interior ",
        "Himalayas load-window worst frame (cold tiles; reported only): ",
        "memory cycles",
        "result: ",
    ] {
        assert!(summary.contains(line), "missing {line:?} in\n{summary}");
    }
    assert!(summary.ends_with("===== end region bench summary ====="));
    let pass = summary.contains("\nresult: PASS\n");
    assert_eq!(exit, vec![bench_exit(pass)], "{summary}");

    // Reported once.
    app.update();
    assert!(exits(&mut app).is_empty());

    // The banner followed the stops, the pan and the result, and kept the result.
    let shown = &app.world().resource::<BannerTexts>().0;
    for text in [
        "Next: Pangaea interior · 250 million years ago · 1 of 9",
        "Pangaea interior · 250 million years ago · 1 of 9",
        "Sundaland · 21 thousand years ago · 4 of 9",
        "Globe · Beringia → Rockies ice margin",
        "Panama · 3 million years ago · 9 of 9",
        "Memory cycles · Himalayas · 1 of 2",
    ] {
        assert!(
            shown.iter().any(|s| s == text),
            "{text:?} not in {shown:#?}"
        );
    }
    let last = shown.last().unwrap();
    assert_eq!(
        *last,
        format!(
            "Region bench finished: {}",
            if pass { "PASS" } else { "FAIL" }
        )
    );
    // Written only on change: a few dozen texts over thousands of frames.
    assert!(shown.len() < 60, "{}", shown.len());
}

/// Runs a single-region bench over the Red Sea stop's centre to its summary, with `options` if
/// given (otherwise the app has no `TerrainOptions`, as a test app may not).
fn run_single(options: Option<TerrainOptions>) -> (App, Vec<AppExit>) {
    let arg = RegionArg {
        lat_deg: 20.0,
        lon_deg: 37.5,
        years: 0.0,
    };
    let mut config = RegionBenchConfig::single(60, true, arg);
    config.path_secs = 6.0;
    config.timeout_secs = 1.0e6;
    let mut app = bench_app(config);
    if let Some(options) = options {
        app.insert_resource(options);
    }
    let deadline = Instant::now() + WALL_LIMIT;
    let mut exit = Vec::new();
    while app.world().resource::<RegionBench>().summary().is_none() {
        app.update();
        if *app.world().resource::<State<GlobeLoadState>>().get() == GlobeLoadState::Ready {
            app.insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 });
        }
        exit.extend(exits(&mut app));
        assert!(Instant::now() < deadline, "the bench did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    (app, exit)
}

#[test]
fn a_single_region_run_reaches_the_summary() {
    let (app, exit) = run_single(None);
    let bench = app.world().resource::<RegionBench>();
    let reports = bench.reports();
    assert_eq!(reports.len(), 1, "{reports:#?}");
    // Without the option, the dunes are on, and every block says so after the spans line.
    for text in [&reports[0], bench.summary().unwrap()] {
        let lines: Vec<&str> = text.lines().collect();
        let spans = lines
            .iter()
            .position(|l| l.starts_with("system spans: "))
            .unwrap();
        assert_eq!(lines[spans + 1], "dunes: on", "{text}");
    }
    assert!(
        reports[0].starts_with("===== Time Fly region bench: Chosen region (20, 37.5 at 0 years)"),
        "{}",
        reports[0]
    );
    assert!(
        reports[0].contains("first-region worst frame (R11"),
        "{}",
        reports[0]
    );
    assert!(!reports[0].contains("globe pan from "), "{}", reports[0]);
    let summary = bench.summary().unwrap();
    for line in [
        "regions: single (20, 37.5 at 0 years)",
        "regions (R14 frame rate): Chosen region ",
        "warm generation (target <= ",
        "not measured (single-region run)",
        "memory cycles: not measured (single-region run)",
    ] {
        assert!(summary.contains(line), "missing {line:?} in\n{summary}");
    }
    assert!(!summary.contains("Himalayas load-window"), "{summary}");
    let pass = summary.contains("\nresult: PASS\n");
    assert_eq!(exit, vec![bench_exit(pass)], "{summary}");
    let shown = &app.world().resource::<BannerTexts>().0;
    assert!(
        shown
            .iter()
            .any(|s| s == "Chosen region · Present · 1 of 1"),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|s| s.contains("of 9")), "{shown:#?}");
}

#[test]
fn a_no_dunes_run_says_so() {
    let (app, _) = run_single(Some(TerrainOptions { dunes: false }));
    let bench = app.world().resource::<RegionBench>();
    for text in [&bench.reports()[0], bench.summary().unwrap()] {
        assert!(text.contains("\ndunes: off\n"), "{text}");
        assert!(!text.contains("dunes: on"), "{text}");
    }
}

#[test]
fn a_failed_globe_ends_the_region_bench() {
    let mut app = bench_app(RegionBenchConfig::new(60, true));
    app.insert_resource(GlobeRenderStatus::Failed("no adapter".into()));
    app.update();
    let bench = app.world().resource::<RegionBench>();
    let summary = bench.summary().expect("a summary");
    assert!(
        summary.contains("result: FAIL (the globe failed: no adapter)"),
        "{summary}"
    );
    assert_eq!(exits(&mut app), vec![bench_exit(false)]);
}

/// Ruling R22: the Aegean at 1 Ma, generated from the real bake at full resolution: the bench
/// path's coast run must fly along a real coast inside the inner field. Needs the real bake and
/// tiles under `assets/` (not in CI); run by hand with
/// `cargo test -p timefly --test region_bench -- --ignored`.
#[test]
#[ignore = "needs the real bake and tiles under assets/"]
fn aegean_coast_run_follows_a_real_coast() {
    use timefly::bench::region::path::coast_run_distance;
    use timefly::bench::region::{BenchPath, Coast};
    use timefly::region::launch::launch_request;
    use timefly::region::{ActiveRegion, RegionState};
    use timefly_core::terrain::blended_height_m;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    assert!(
        root.join("data/tiles/manifest.json").exists(),
        "no real bake under {}",
        root.display()
    );
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: root.to_string_lossy().into_owned(),
            meta_check: AssetMetaCheck::Never,
            ..default()
        },
        ImagePlugin::default(),
        StatesPlugin,
        GlobeDataPlugin::default(),
        RegionPlugin,
    ));
    let deadline = Instant::now() + WALL_LIMIT;
    while *app.world().resource::<State<GlobeLoadState>>().get() != GlobeLoadState::Ready {
        app.update();
        assert!(Instant::now() < deadline, "the globe data did not load");
        std::thread::sleep(Duration::from_millis(1));
    }
    let aegean = BENCH_REGIONS.iter().find(|r| r.name == "Aegean").unwrap();
    app.world_mut().write_message(launch_request(&aegean.arg));
    loop {
        app.update();
        if *app.world().resource::<State<RegionState>>().get() == RegionState::InRegion {
            break;
        }
        assert!(Instant::now() < deadline, "the Aegean did not generate");
        std::thread::sleep(Duration::from_millis(1));
    }
    let grid = app.world().resource::<ActiveRegion>().grid.clone();
    let ground = |x: f32, z: f32| blended_height_m(&grid, f64::from(x), f64::from(z));
    let path = BenchPath::plan(ground);
    let Coast::InField { point, .. } = path.coast else {
        panic!(
            "the Aegean's nearest coast should be in the field: {:?}",
            path.coast
        );
    };
    let worst = coast_run_distance(&path, ground).expect("a coast near the run");
    println!(
        "Aegean coast {:.1} km from the centre; the run stays within {:.0} m of a coastline",
        point.length() / 1000.0,
        worst
    );
    assert!(worst <= 10_000.0, "{worst} m from the coast");
}
