//! Headless tests for `RegionPlugin` on the tiny globe bake (`tests/assets/data/globe`) plus
//! four fixture tiles and a small overview (`tests/assets/data/tiles`, written by
//! `write_region_fixtures` below; ruling R3). No window, no renderer.
//!
//! Rewrite the fixtures with
//! `cargo test -p timefly --test region_loading -- --ignored write_region_fixtures`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bevy::asset::AssetMetaCheck;
use bevy::asset::io::file::FileAssetReader;
use bevy::asset::io::{
    AssetReader, AssetReaderError, AssetSourceBuilder, AssetSourceId, PathStream, Reader,
};
use bevy::log::tracing::{self, Subscriber};
use bevy::log::tracing_subscriber::filter::LevelFilter;
use bevy::log::tracing_subscriber::layer::{Context, SubscriberExt};
use bevy::log::tracing_subscriber::{Layer, Registry};
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use timefly::data::{GlobeData, GlobeDataPlugin, GlobeLoadState};
use timefly::region::frames::{CpuGrid, FRAME_CACHE_SLOTS, FrameCache};
use timefly::region::generate::{BudgetedRun, GenInputs, generate_parallel, tiles_needed};
use timefly::region::tiles::{OVERVIEW_PATH, TILE_MANIFEST_PATH, TileCache, tile_path};
use timefly::region::{
    ActiveRegion, GenMode, LOAD_TIMEOUT_S, LargeGenBudget, LoadClock, RegionEntered,
    RegionGenerated, RegionLeft, RegionPlugin, RegionRequested, RegionSettings,
    RegionSnapshotChanged, RegionState,
};
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::{Grid, read_tfgrid};
use timefly_core::geo::{RenderDir, geo_to_render, latlon_to_geo};
use timefly_core::sha256::sha256_hex;
use timefly_core::worldgen::generate::RegionSpec;
use timefly_core::worldgen::tiles::{TILE_SAMPLES, TileSet};

/// Generation must finish within this (the brief's bound).
const REGION_TIMEOUT: Duration = Duration::from_secs(20);
/// The fixture tiles: the four around (0°, 0°).
const FIXTURE_TILES: [(u8, u8); 4] = [(17, 35), (17, 36), (18, 35), (18, 36)];
/// The fixture overview's nodes per degree (the bake uses 12).
const OVERVIEW_PER_DEG: u32 = 1;
/// Small grids for the tests that are not about resolution.
const SMALL: RegionSettings = RegionSettings {
    inner_res: 129,
    apron_res: 65,
    mode: GenMode::Parallel,
};

// ---------------------------------------------------------------------------------------------
// Fixtures (ruling R3)

/// The synthetic present day: a plane tilted up to the north-east, in 25 m steps (so the files
/// compress well), coast through (0°, 0°).
fn fixture_elevation(lat: f64, lon: f64) -> f32 {
    let h = 150.0 * lat + 100.0 * lon;
    (25.0 * (h / 25.0).round()) as f32
}

/// Köppen code: 0 at sea, 1 (Af) north of the equator, 2 (Am) south of it.
fn fixture_koppen(lat: f64, lon: f64) -> f32 {
    if fixture_elevation(lat, lon) <= 0.0 {
        0.0
    } else if lat >= 0.0 {
        1.0
    } else {
        2.0
    }
}

/// A 2-channel f16 lat-lon `.tfgrid` whose node `(i, j)` sits at `(lat0 − i·step, lon0 + j·step)`.
fn latlon_tfgrid(width: u32, height: u32, lat0: f64, lon0: f64, step: f64) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width * height * 4) as usize);
    for i in 0..height {
        for j in 0..width {
            let (lat, lon) = (lat0 - f64::from(i) * step, lon0 + f64::from(j) * step);
            for v in [fixture_elevation(lat, lon), fixture_koppen(lat, lon)] {
                raw.extend_from_slice(&half::f16::from_f32(v).to_le_bytes());
            }
        }
    }
    let z =
        ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest);
    let mut out = b"TFGRID01".to_vec();
    out.extend_from_slice(&[1, 0, 2, 0]); // lat-lon, f16, 2 channels
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&(z.len() as u32).to_le_bytes());
    out.extend_from_slice(&z);
    out
}

fn fixture_tile(row: u8, col: u8) -> Vec<u8> {
    let lat_n = 90.0 - 5.0 * f64::from(row);
    let lon_w = -180.0 + 5.0 * f64::from(col);
    latlon_tfgrid(TILE_SAMPLES, TILE_SAMPLES, lat_n, lon_w, 1.0 / 60.0)
}

fn fixture_overview() -> Vec<u8> {
    let k = OVERVIEW_PER_DEG;
    latlon_tfgrid(360 * k + 1, 180 * k + 1, 90.0, -180.0, 1.0 / f64::from(k))
}

/// Every fixture file, by path under the asset root, with the manifest last.
fn fixture_files() -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = FIXTURE_TILES
        .iter()
        .map(|&(r, c)| (tile_path(r, c), fixture_tile(r, c)))
        .collect();
    files.push((OVERVIEW_PATH.to_string(), fixture_overview()));
    let entry =
        |bytes: &[u8]| serde_json::json!({"bytes": bytes.len(), "sha256": sha256_hex(bytes)});
    let tiles: Vec<serde_json::Value> = FIXTURE_TILES
        .iter()
        .zip(&files)
        .map(|(&(row, col), (_, bytes))| {
            let mut e = entry(bytes);
            e["row"] = row.into();
            e["col"] = col.into();
            e
        })
        .collect();
    let manifest = serde_json::json!({
        "version": 1,
        "tile_deg": 5,
        "samples": TILE_SAMPLES,
        "sources": {"etopo": "synthetic (tests/region_loading.rs)", "koppen": "synthetic"},
        "overview": entry(&files[4].1),
        "tiles": tiles,
    });
    let mut json = serde_json::to_string_pretty(&manifest).unwrap();
    json.push('\n');
    files.push((TILE_MANIFEST_PATH.to_string(), json.into_bytes()));
    files
}

#[test]
#[ignore = "writes the committed fixtures; run by hand after changing the generator"]
fn write_region_fixtures() {
    for (path, bytes) in fixture_files() {
        let path = tiny_root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
    }
}

#[test]
fn region_fixtures_match_their_generator() {
    for (path, bytes) in fixture_files() {
        let committed = fs::read(tiny_root().join(&path))
            .unwrap_or_else(|e| panic!("{path}: {e} (run write_region_fixtures)"));
        if path.ends_with(".json") {
            assert_eq!(committed, bytes, "{path} is stale");
        } else {
            // Compare contents, not compressed bytes.
            assert_eq!(
                read_tfgrid(&committed).unwrap(),
                read_tfgrid(&bytes).unwrap(),
                "{path} is stale"
            );
        }
    }
    // The four tiles are exactly what a present-day region at (0°, 0°) reads.
    let frame = RegionSpec::new(dir(0.0, 0.0), YearsBp(0.0), 0).frame();
    assert_eq!(TileSet::needed_for(&frame), FIXTURE_TILES);
}

// ---------------------------------------------------------------------------------------------
// Log capture

/// Every `info` and louder event of this test binary, formatted. Tests run in parallel, so each
/// asserts on lines naming its own tile.
fn logs() -> Arc<Mutex<Vec<String>>> {
    static LOGS: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    LOGS.get_or_init(|| {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let layer = Capture(lines.clone()).with_filter(LevelFilter::INFO);
        tracing::subscriber::set_global_default(Registry::default().with(layer))
            .expect("no other global subscriber in this test binary");
        lines
    })
    .clone()
}

struct Capture(Arc<Mutex<Vec<String>>>);

impl<S: Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut line = format!("{} ", event.metadata().level());
        event.record(
            &mut |field: &tracing::field::Field, value: &dyn std::fmt::Debug| {
                line.push_str(&format!("{}={value:?} ", field.name()));
            },
        );
        self.0.lock().unwrap().push(line);
    }
}

fn warnings_naming(needle: &str) -> Vec<String> {
    logs()
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("WARN") && l.contains(needle))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Harness

fn tiny_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets")
}

/// A copy of the tiny bake and fixtures in a fresh asset root, to break on purpose.
fn tiny_copy(test: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("region_loading-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    copy_dir(&tiny_root(), &root);
    root
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn dir(lat: f64, lon: f64) -> RenderDir {
    geo_to_render(latlon_to_geo(lat, lon))
}

#[derive(Resource, Default)]
struct Seen {
    generated: Vec<RegionGenerated>,
    entered: Vec<RegionEntered>,
    changed: Vec<RegionSnapshotChanged>,
    states: Vec<RegionState>,
}

fn record(
    mut seen: ResMut<Seen>,
    mut generated: MessageReader<RegionGenerated>,
    mut entered: MessageReader<RegionEntered>,
    mut changed: MessageReader<RegionSnapshotChanged>,
    state: Res<State<RegionState>>,
) {
    seen.generated.extend(generated.read().copied());
    seen.entered.extend(entered.read().copied());
    seen.changed.extend(changed.read().copied());
    if seen.states.last() != Some(state.get()) {
        seen.states.push(*state.get());
    }
}

/// Asset paths (relative to the asset root) whose reads do not finish while listed: a stalled
/// fetch, not a 404. Shared with the reader, so a test can stall a path after the globe has
/// loaded, or release it.
type Stalls = Arc<Mutex<Vec<PathBuf>>>;

/// The file reader, except that reads of the [`Stalls`] paths hang forever.
struct StallingReader {
    inner: FileAssetReader,
    stalls: Stalls,
}

impl AssetReader for StallingReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        while self.stalls.lock().unwrap().iter().any(|p| p == path) {
            std::thread::sleep(Duration::from_millis(1));
            bevy::tasks::futures_lite::future::yield_now().await;
        }
        self.inner.read(path).await
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        self.inner.read_meta(path).await
    }

    async fn read_directory<'a>(
        &'a self,
        path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        self.inner.read_directory(path).await
    }

    async fn is_directory<'a>(&'a self, path: &'a Path) -> Result<bool, AssetReaderError> {
        self.inner.is_directory(path).await
    }
}

/// A headless app with the globe data and regions, the globe data loaded.
fn region_app(asset_root: &Path, settings: RegionSettings) -> App {
    region_app_with(asset_root, settings, None, |_| {})
}

/// [`region_app`] reading through a [`StallingReader`]; returns the paths to stall.
fn stalling_region_app(asset_root: &Path, settings: RegionSettings) -> (App, Stalls) {
    let stalls = Stalls::default();
    let app = region_app_with(asset_root, settings, Some(stalls.clone()), |_| {});
    (app, stalls)
}

/// `more` adds plugins before the first update (so their startup systems run).
fn region_app_with(
    asset_root: &Path,
    settings: RegionSettings,
    stalls: Option<Stalls>,
    more: impl FnOnce(&mut App),
) -> App {
    logs();
    let mut app = App::new();
    if let Some(stalls) = stalls {
        let root = asset_root.to_path_buf();
        app.register_asset_source(
            AssetSourceId::Default,
            AssetSourceBuilder::new(move || {
                Box::new(StallingReader {
                    inner: FileAssetReader::new(root.clone()),
                    stalls: stalls.clone(),
                })
            }),
        );
    }
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: asset_root.to_string_lossy().into_owned(),
            meta_check: AssetMetaCheck::Never,
            ..default()
        },
        ImagePlugin::default(),
        StatesPlugin,
        GlobeDataPlugin::default(),
        RegionPlugin,
    ))
    .insert_resource(settings)
    .init_resource::<Seen>()
    .add_systems(Last, record);
    more(&mut app);
    let deadline = Instant::now() + REGION_TIMEOUT;
    loop {
        app.update();
        match *app.world().resource::<State<GlobeLoadState>>().get() {
            GlobeLoadState::Ready => break,
            GlobeLoadState::Failed => panic!("the tiny bake failed to load"),
            GlobeLoadState::Loading => {}
        }
        assert!(Instant::now() < deadline, "globe data still loading");
        std::thread::sleep(Duration::from_millis(1));
    }
    app
}

fn request(app: &mut App, lat: f64, lon: f64, years: f64) {
    app.world_mut().write_message(RegionRequested {
        era_dir: dir(lat, lon),
        snapshot: YearsBp(years),
    });
}

fn state(app: &App) -> RegionState {
    *app.world().resource::<State<RegionState>>().get()
}

/// Updates until the region state is `target` after a `Generating` phase; returns the number of
/// updates spent generating.
fn run_until(app: &mut App, target: RegionState) -> u32 {
    let deadline = Instant::now() + REGION_TIMEOUT;
    let mut updates = 0;
    loop {
        app.update();
        updates += 1;
        let seen = &app.world().resource::<Seen>().states;
        if state(app) == target && seen.contains(&RegionState::Generating) {
            return updates;
        }
        assert!(
            Instant::now() < deadline,
            "still {:?} after {REGION_TIMEOUT:?} (states {seen:?})",
            state(app)
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn generated(app: &App) -> RegionGenerated {
    let seen = app.world().resource::<Seen>();
    assert_eq!(seen.generated.len(), 1, "{:?}", seen.generated);
    seen.generated[0]
}

fn active(app: &App) -> ActiveRegion {
    app.world()
        .get_resource::<ActiveRegion>()
        .expect("an active region")
        .clone()
}

fn assert_finite(region: &ActiveRegion) {
    for field in [&region.grid.inner, &region.grid.apron] {
        assert_eq!(field.elevation_m.len(), (field.res * field.res) as usize);
        assert!(field.elevation_m.iter().all(|h| h.is_finite()));
    }
}

// ---------------------------------------------------------------------------------------------
// Tests

#[test]
fn generates_a_region_from_the_tiny_bake() {
    let mut app = region_app(&tiny_root(), RegionSettings::default());
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);

    let seen = app.world().resource::<Seen>();
    assert_eq!(
        seen.states,
        [
            RegionState::Globe,
            RegionState::Generating,
            RegionState::InRegion
        ]
    );
    let event = generated(&app);
    assert!(!event.fallback, "{event:?}");
    assert_eq!(event.tiles_fetched, 4);
    let tile_bytes: u64 = FIXTURE_TILES
        .iter()
        .map(|&(r, c)| {
            fs::metadata(tiny_root().join(tile_path(r, c)))
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(
        event.bytes_fetched, tile_bytes,
        "the four tiles' file bytes"
    );
    assert!(event.millis > 0 && event.millis < 20_000, "{event:?}");
    assert_eq!(seen.entered.len(), 1);
    assert_eq!(seen.entered[0].spec, event.spec);
    assert!(seen.changed.is_empty());

    let region = active(&app);
    assert_eq!(region.grid.spec, event.spec);
    assert_eq!(region.frame, event.spec.frame());
    let inner = &region.grid.inner;
    assert_eq!((inner.res, region.grid.apron.res), (1025, 513));
    assert_finite(&region);
    // The fixture coast runs through the centre, land to the north-east: row 0 is north and
    // column 0 west.
    let n = inner.res as usize;
    let ne = inner.elevation_m[n - 1];
    let sw = inner.elevation_m[(n - 1) * n];
    assert!(ne > 100.0 && sw < -100.0, "NE {ne} m, SW {sw} m");

    let cache = app.world().resource::<TileCache>();
    assert_eq!(cache.len(), 4);
    assert!(FIXTURE_TILES.iter().all(|&k| cache.contains(k)));
    assert!(cache.overview().is_some());
}

#[test]
fn missing_tile_falls_back_with_warning() {
    let root = tiny_copy("missing");
    fs::remove_file(root.join(tile_path(17, 35))).unwrap();
    let mut app = region_app(&root, SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);

    let event = generated(&app);
    assert!(event.fallback, "{event:?}");
    assert_eq!(event.tiles_fetched, 3);
    assert_finite(&active(&app));
    let warnings = warnings_naming("data/tiles/r17c35.tfgrid");
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("terrain tile unavailable") && w.contains("file not found")),
        "{warnings:?}"
    );
    // The redacted form: the asset path, never the file system path.
    let root = root.to_string_lossy();
    assert!(warnings.iter().all(|w| !w.contains(&*root)), "{warnings:?}");
}

/// Each update advances `Time<Real>` by a second, so the loads' 20 s timeout passes in about
/// 20 updates.
fn one_second_per_update(app: &mut App) {
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(1)));
}

#[test]
fn a_stalled_tile_times_out_and_falls_back() {
    let (mut app, stalls) = stalling_region_app(&tiny_root(), SMALL);
    stalls.lock().unwrap().push(tile_path(18, 36).into());
    one_second_per_update(&mut app);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    let event = generated(&app);
    assert!(event.fallback, "{event:?}");
    assert_eq!(event.tiles_fetched, 3);
    assert_finite(&active(&app));
    let warnings = warnings_naming(&tile_path(18, 36));
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("terrain tile unavailable") && w.contains("timed out")),
        "{warnings:?}"
    );
    assert!(!app.world().resource::<TileCache>().contains((18, 36)));
}

/// A paused page (one 60 s frame, as when a host stops requestAnimationFrame) does not time out
/// a load that finishes afterwards: the load clock counts running frames.
#[test]
fn a_long_pause_does_not_time_out_loads() {
    let (mut app, stalls) = stalling_region_app(&tiny_root(), SMALL);
    let tile = tile_path(18, 36);
    stalls.lock().unwrap().push(tile.clone().into());
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )));
    request(&mut app, 0.0, 0.0, 0.0);
    let deadline = Instant::now() + REGION_TIMEOUT;
    while app
        .world()
        .resource::<AssetServer>()
        .get_handle::<CpuGrid>(tile.clone())
        .is_none()
    {
        app.update();
        assert!(Instant::now() < deadline, "the tile never started loading");
    }
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs(60)));
    app.update();
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )));
    stalls.lock().unwrap().clear();
    run_until(&mut app, RegionState::InRegion);
    let event = generated(&app);
    assert!(!event.fallback, "{event:?}");
    assert_eq!(event.tiles_fetched, 4);
    let warnings = warnings_naming(&tile);
    assert!(
        warnings.iter().all(|w| !w.contains("timed out")),
        "{warnings:?}"
    );
    assert!(app.world().resource::<LoadClock>().elapsed_s < LOAD_TIMEOUT_S);
}

#[test]
fn a_stalled_overview_times_out_and_the_region_still_loads() {
    let (mut app, stalls) = stalling_region_app(&tiny_root(), SMALL);
    stalls.lock().unwrap().push(OVERVIEW_PATH.into());
    one_second_per_update(&mut app);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    // The tiles all loaded: the inner field has its detail; the apron uses the globe frames.
    let event = generated(&app);
    assert!(!event.fallback, "{event:?}");
    assert_finite(&active(&app));
    assert!(app.world().resource::<TileCache>().overview().is_none());
    let warnings = warnings_naming(OVERVIEW_PATH);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("terrain overview unavailable") && w.contains("timed out")),
        "{warnings:?}"
    );
}

#[test]
fn a_stalled_frame_times_out_back_to_the_globe() {
    let (mut app, stalls) = stalling_region_app(&tiny_root(), SMALL);
    // The globe has its images; the CPU copy of the 540 Ma frame now never arrives.
    let path = {
        let data = app.world().resource::<GlobeData>();
        let last = data.timeline.frames().len() - 1;
        data.frame_path(last).to_string()
    };
    stalls.lock().unwrap().push(path.clone().into());
    one_second_per_update(&mut app);
    request(&mut app, 0.0, 0.0, 400.0e6);
    run_until(&mut app, RegionState::Globe);
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert!(app.world().resource::<Seen>().generated.is_empty());
    let warnings = warnings_naming(&path);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("region generation failed") && w.contains("timed out")),
        "{warnings:?}"
    );
}

/// Review Focus 2: a region that falls back shows "terrain detail unavailable here" for about
/// 3 s after it appears; a complete one never does.
#[test]
fn the_fallback_notice_shows_only_when_a_tile_is_missing() {
    use timefly::fly_camera::FlyCameraPlugin;
    use timefly::region::navigate::{FALLBACK_NOTICE_S, FallbackNotice, RegionNavigationPlugin};
    use timefly::time::TimePlugin;

    for missing in [true, false] {
        let root = tiny_copy(if missing {
            "notice-missing"
        } else {
            "notice-complete"
        });
        if missing {
            fs::remove_file(root.join(tile_path(18, 35))).unwrap();
        }
        let mut app = region_app_with(&root, SMALL, None, |app| {
            app.init_resource::<ButtonInput<KeyCode>>().add_plugins((
                TimePlugin,
                RegionNavigationPlugin,
                FlyCameraPlugin,
            ));
        });
        let shown = |app: &mut App| {
            let mut q = app
                .world_mut()
                .query_filtered::<&Node, With<FallbackNotice>>();
            q.single(app.world()).unwrap().display == Display::Flex
        };
        app.update();
        assert!(!shown(&mut app), "before any region");
        request(&mut app, 0.0, 0.0, 0.0);
        run_until(&mut app, RegionState::InRegion);
        assert_eq!(generated(&app).fallback, missing);
        assert_eq!(shown(&mut app), missing, "missing tile: {missing}");
        // About three seconds later it is gone.
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            250,
        )));
        for _ in 0..(FALLBACK_NOTICE_S / 0.25) as usize + 2 {
            app.update();
        }
        assert!(!shown(&mut app), "after {FALLBACK_NOTICE_S} s");
    }
}

#[test]
fn corrupt_tile_falls_back_with_warning() {
    // (tile, how it is broken, what the warning says)
    let cases: [((u8, u8), &str, &str); 2] = [
        ((17, 36), "truncated", "does not match the manifest's"),
        ((18, 35), "sha", "sha256"),
    ];
    for ((row, col), how, says) in cases {
        let root = tiny_copy(&format!("corrupt-{how}"));
        let path = root.join(tile_path(row, col));
        let mut bytes = fs::read(&path).unwrap();
        match how {
            "truncated" => bytes.truncate(bytes.len() - 100),
            _ => *bytes.last_mut().unwrap() ^= 0x55, // same size, different hash
        }
        fs::write(&path, bytes).unwrap();

        let mut app = region_app(&root, SMALL);
        request(&mut app, 0.0, 0.0, 0.0);
        run_until(&mut app, RegionState::InRegion);
        let event = generated(&app);
        assert!(event.fallback, "{how}: {event:?}");
        assert_eq!(event.tiles_fetched, 3, "{how}");
        assert_finite(&active(&app));
        let warnings = warnings_naming(&tile_path(row, col));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("terrain tile unavailable") && w.contains(says)),
            "{how}: {warnings:?}"
        );
        assert!(!app.world().resource::<TileCache>().contains((row, col)));
    }
}

#[test]
fn leaving_frees_the_region() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    let grid = Arc::downgrade(&active(&app).grid);

    app.world_mut().write_message(RegionLeft);
    app.update();
    app.update();
    assert_eq!(state(&app), RegionState::Globe);
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert!(grid.upgrade().is_none(), "something still holds the grid");
    assert_eq!(app.world().resource::<Assets<CpuGrid>>().len(), 0);
    let server = app.world().resource::<AssetServer>();
    let data = app.world().resource::<GlobeData>();
    let mut paths: Vec<String> = data.frame_paths.iter().map(|p| p.to_string()).collect();
    paths.extend(FIXTURE_TILES.iter().map(|&(r, c)| tile_path(r, c)));
    paths.push(OVERVIEW_PATH.to_string());
    for path in paths {
        assert!(
            server.get_handle::<CpuGrid>(path.clone()).is_none(),
            "{path} still has a strong handle"
        );
    }
    // The tile and frame caches stay, bounded, for the next region (ruling R11): the frame
    // cache holds the present-day request's one frame file, not the region's own grids.
    assert_eq!(app.world().resource::<TileCache>().len(), 4);
    let frames = app.world().resource::<FrameCache>();
    assert!(frames.len() <= FRAME_CACHE_SLOTS);
    assert_eq!(frames.len(), 1);
}

#[test]
fn a_frame_that_will_not_load_returns_to_the_globe() {
    let root = tiny_copy("frame");
    let mut app = region_app(&root, SMALL);
    // The globe has its images; the CPU copy of the 540 Ma frame now fails to decode.
    let path = {
        let data = app.world().resource::<GlobeData>();
        let last = data.timeline.frames().len() - 1;
        data.frame_path(last).to_string()
    };
    fs::write(root.join(&path), b"TFGRID01 truncated").unwrap();
    request(&mut app, 0.0, 0.0, 400.0e6);
    run_until(&mut app, RegionState::Globe);
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert!(app.world().resource::<Seen>().generated.is_empty());
    let warnings = warnings_naming(&path);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("region generation failed")),
        "{warnings:?}"
    );
}

#[test]
fn leaving_while_generating_cancels_the_job() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    app.update();
    app.update();
    assert_eq!(state(&app), RegionState::Generating);
    app.world_mut().write_message(RegionLeft);
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(state(&app), RegionState::Globe);
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert!(app.world().resource::<Seen>().generated.is_empty());
}

/// Updates until the overview's load is in flight (a live `CpuGrid` handle to it), so a test
/// can cancel the job at exactly that point.
fn until_overview_in_flight(app: &mut App) {
    let deadline = Instant::now() + REGION_TIMEOUT;
    loop {
        app.update();
        let server = app.world().resource::<AssetServer>();
        if server.get_handle::<CpuGrid>(OVERVIEW_PATH).is_some() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the overview never started loading"
        );
    }
}

#[test]
fn a_cancelled_job_keeps_the_overview() {
    for cancel in ["leave", "supersede"] {
        let mut app = region_app(&tiny_root(), SMALL);
        request(&mut app, 0.0, 0.0, 0.0);
        until_overview_in_flight(&mut app);
        match cancel {
            "leave" => {
                app.world_mut().write_message(RegionLeft);
                // State changes apply in the next update's transition schedule.
                app.update();
                app.update();
                assert_eq!(state(&app), RegionState::Globe, "{cancel}");
                request(&mut app, 0.0, 0.0, 0.0);
            }
            _ => request(&mut app, 0.0, 0.0, 400.0e6),
        }
        run_until(&mut app, RegionState::InRegion);
        let cache = app.world().resource::<TileCache>();
        assert!(
            cache.overview().is_some(),
            "{cancel}: the overview was lost"
        );
        assert_eq!(
            app.world().resource::<Seen>().generated.len(),
            1,
            "{cancel}"
        );
    }
}

#[test]
fn a_superseding_request_wins() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    app.update();
    app.update();
    assert_eq!(state(&app), RegionState::Generating);
    request(&mut app, 1.0, 1.0, 400.0e6);
    run_until(&mut app, RegionState::InRegion);
    let event = generated(&app);
    assert_eq!(event.spec.snapshot, YearsBp(400.0e6));
    assert_eq!(event.spec.center, dir(1.0, 1.0));
    assert_eq!(active(&app).grid.spec, event.spec);

    // A leave in the middle of a load leaves no CPU grid behind once the loads settle.
    request(&mut app, 0.0, 0.0, 10.0e3);
    app.update();
    app.world_mut().write_message(RegionLeft);
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        app.update();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(state(&app), RegionState::Globe);
    assert_eq!(app.world().resource::<Assets<CpuGrid>>().len(), 0);
    assert_eq!(app.world().resource::<Seen>().generated.len(), 1);
}

#[test]
fn repeated_requests_reuse_decoded_frames() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    let frame = app
        .world()
        .resource::<GlobeData>()
        .frame_path(0)
        .to_string();
    app.world_mut().resource_mut::<Seen>().states.clear();
    request(&mut app, 0.5, 0.5, 0.0);
    let deadline = Instant::now() + REGION_TIMEOUT;
    loop {
        app.update();
        let server = app.world().resource::<AssetServer>();
        assert!(
            server.get_handle::<CpuGrid>(frame.clone()).is_none(),
            "{frame} was loaded again"
        );
        let seen = app.world().resource::<Seen>();
        if state(&app) == RegionState::InRegion && seen.states.contains(&RegionState::Generating) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the second region never finished"
        );
    }
    assert_eq!(app.world().resource::<Seen>().generated.len(), 2);
}

#[test]
fn a_new_snapshot_replaces_the_region() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    app.world_mut().resource_mut::<Seen>().states.clear();
    request(&mut app, 0.0, 0.0, 100.0e6);
    run_until(&mut app, RegionState::InRegion);
    let seen = app.world().resource::<Seen>();
    assert_eq!(seen.generated.len(), 2);
    assert_eq!(seen.entered.len(), 1);
    assert_eq!(seen.changed.len(), 1);
    assert_eq!(seen.changed[0].from, YearsBp(0.0));
    assert_eq!(seen.changed[0].to, YearsBp(100.0e6));
    assert_eq!(active(&app).grid.spec.snapshot, YearsBp(100.0e6));
}

/// Fix round 1, critical 1: `[` then `]` while the first region is still generating. The
/// second request is back at the snapshot already shown, so the job sends `RegionEntered`, not
/// `RegionSnapshotChanged` (Task 5's semantics). Navigation must still finish the fade and fly
/// on at the original snapshot, never hold the haze forever.
#[test]
fn stepping_back_mid_generation_finishes_the_fade() {
    use timefly::fly_camera::{FlyCameraPlugin, FlyState};
    use timefly::region::navigate::{NavPhase, Navigation, RegionNavigationPlugin};
    use timefly::time::{GlobeTime, OLDER_KEY, TimePlugin, YOUNGER_KEY};

    let budgeted = RegionSettings {
        mode: GenMode::Budgeted {
            budget: Duration::from_micros(500),
        },
        ..SMALL
    };
    let mut app = region_app(&tiny_root(), budgeted);
    app.init_resource::<ButtonInput<KeyCode>>().add_plugins((
        TimePlugin,
        RegionNavigationPlugin,
        FlyCameraPlugin,
    ));
    let phase = |app: &App| app.world().resource::<Navigation>().phase;
    let press = |app: &mut App, key: KeyCode| {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(key);
        keys.clear();
    };
    let run = |app: &mut App, done: &dyn Fn(&App) -> bool, what: &str| {
        let deadline = Instant::now() + REGION_TIMEOUT;
        while !done(app) {
            app.update();
            assert!(
                Instant::now() < deadline,
                "{what}: still {:?} in {:?}",
                phase(app),
                state(app)
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    };

    request(&mut app, 0.0, 0.0, 0.0);
    run(&mut app, &|a| phase(a) == NavPhase::Flying, "entering");
    // Fly 20 km east, so the carried centre is not the region's own.
    app.world_mut().resource_mut::<FlyState>().pos_m.x = 20_000.0;
    app.update();

    press(&mut app, OLDER_KEY);
    assert!(
        matches!(phase(&app), NavPhase::ChangingSnapshot { .. }),
        "{:?}",
        phase(&app)
    );
    run(
        &mut app,
        &|a| state(a) == RegionState::Generating,
        "the 100-year region starting",
    );
    press(&mut app, YOUNGER_KEY);
    assert_eq!(app.world().resource::<GlobeTime>().years, YearsBp(0.0));
    run(
        &mut app,
        &|a| phase(a) == NavPhase::Flying,
        "back at the present",
    );
    assert_eq!(state(&app), RegionState::InRegion);
    let region = active(&app);
    assert_eq!(region.grid.spec.snapshot, YearsBp(0.0));
    // Recentred on the camera's crust position (geographic frame: the camera's direction).
    let camera_dir = RegionSpec::new(dir(0.0, 0.0), YearsBp(0.0), 0)
        .frame()
        .local_to_dir(20_000.0, 0.0);
    assert!(region.grid.spec.center.0.distance(camera_dir.0) < 1e-9);
    let fly = *app.world().resource::<FlyState>();
    assert_eq!((fly.pos_m.x, fly.pos_m.z), (0.0, 0.0));
    let seen = app.world().resource::<Seen>();
    assert!(seen.entered.len() >= 2, "{:?}", seen.entered);
}

#[test]
fn generates_bridge_and_deep_snapshots() {
    // 10 ka crossfades ICE-6G into the bridge frame, 30 ka is the bridge frame (modern
    // terrain carried by the plates), 100 Ma is a deep bracket still mostly that frame (ETOPO
    // weight 0.81 in the tiny timeline), and 400 Ma is mostly the 540 Ma PaleoDEM (no tiles).
    for years in [10.0e3, 30.0e3, 100.0e6, 400.0e6] {
        let mut app = region_app(&tiny_root(), SMALL);
        request(&mut app, 0.0, 0.0, years);
        run_until(&mut app, RegionState::InRegion);
        let event = generated(&app);
        assert_eq!(event.spec.snapshot, YearsBp(years));
        assert_finite(&active(&app));
        if years == 400.0e6 {
            assert_eq!(event.tiles_fetched, 0);
            assert!(!event.fallback);
        }
    }
}

#[test]
fn budgeted_generation_matches_parallel() {
    let mut grids = Vec::new();
    let mut updates = Vec::new();
    for mode in [GenMode::Parallel, GenMode::web()] {
        let mut app = region_app(&tiny_root(), RegionSettings { mode, ..SMALL });
        request(&mut app, 0.0, 0.0, 0.0);
        updates.push(run_until(&mut app, RegionState::InRegion));
        grids.push(active(&app).grid);
    }
    assert_eq!(grids[0], grids[1]);
    assert!(
        updates[1] > 3,
        "the time-boxed run should span frames ({updates:?})"
    );
}

/// The job applies the large budget to a time-boxed run's slices while `LargeGenBudget` says so
/// (navigation sets it during first entries and covered snapshot changes).
#[test]
fn the_large_budget_reaches_the_time_boxed_slices() {
    let tiny_budget = RegionSettings {
        mode: GenMode::Budgeted {
            budget: Duration::from_micros(200),
        },
        ..SMALL
    };
    let mut updates = Vec::new();
    for large in [false, true] {
        let mut app = region_app(&tiny_root(), tiny_budget);
        app.insert_resource(LargeGenBudget(large));
        request(&mut app, 0.0, 0.0, 0.0);
        updates.push(run_until(&mut app, RegionState::InRegion));
    }
    println!(
        "updates to generate: small budget {}, large {}",
        updates[0], updates[1]
    );
    assert!(
        updates[1] * 3 < updates[0],
        "a 12 ms slice should need far fewer frames than a 0.2 ms one: {updates:?}"
    );
}

/// The generation inputs for a present-day region at (0°, 0°), read straight from the files.
fn inputs(app: &App, spec: RegionSpec) -> GenInputs {
    let data = app.world().resource::<GlobeData>();
    let frames: Vec<Option<Arc<Grid>>> = (0..data.timeline.frames().len())
        .map(|f| {
            let path = tiny_root().join(data.frame_path(f).path());
            Some(Arc::new(read_tfgrid(&fs::read(path).unwrap()).unwrap()))
        })
        .collect();
    let needed = tiles_needed(&spec, &data.timeline, &data.crust, &frames).unwrap();
    let mut tiles = TileSet::new();
    for &(r, c) in &needed {
        let bytes = fs::read(tiny_root().join(tile_path(r, c))).unwrap();
        tiles.insert(r, c, read_tfgrid(&bytes).unwrap()).unwrap();
    }
    let overview = fs::read(tiny_root().join(OVERVIEW_PATH)).unwrap();
    tiles.set_overview(read_tfgrid(&overview).unwrap()).unwrap();
    GenInputs {
        spec,
        timeline: data.timeline.clone(),
        crust: data.crust.clone(),
        fallback: !tiles.missing(&needed).is_empty(),
        tiles,
        frames,
    }
}

#[test]
fn budgeted_run_is_time_boxed() {
    let app = region_app(&tiny_root(), SMALL);
    let spec = RegionSpec {
        inner_res: 257,
        apron_res: 129,
        ..RegionSpec::new(dir(0.0, 0.0), YearsBp(0.0), 7)
    };
    let inputs = inputs(&app, spec);
    let expected = generate_parallel(&inputs).unwrap();
    let budget = Duration::from_millis(6);
    let mut run = BudgetedRun::new(inputs, budget, Duration::ZERO);
    let mut slices = Vec::new();
    let grid = loop {
        let t = Instant::now();
        let done = run.poll_slice();
        slices.push(t.elapsed());
        if let Some(result) = done {
            break result.unwrap();
        }
    };
    assert_eq!(grid, expected);
    let longest = *slices.iter().max().unwrap();
    println!("{} slices, longest {longest:?}", slices.len());
    assert!(slices.len() > 3, "{slices:?}");
    // One slice overshoots its budget by at most one prep step or one row; generous for
    // unoptimised builds.
    assert!(longest < budget + Duration::from_millis(30), "{longest:?}");
}

/// The last "region tiles ready" log line's `bytes` and `needed` fields.
fn last_tile_stats() -> (u64, u64) {
    let logs = logs();
    let lines = logs.lock().unwrap();
    let line = lines
        .iter()
        .rev()
        .find(|l| l.contains("region tiles ready"))
        .expect("a tile summary");
    let field = |name: &str| -> u64 {
        let at = line.find(&format!("{name}=")).unwrap() + name.len() + 1;
        line[at..].split(' ').next().unwrap().parse().unwrap()
    };
    (field("bytes"), field("needed"))
}

#[test]
#[ignore = "needs the real bake and tiles under assets/; prints timings for performance.md"]
fn real_region_timing() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    if !root.join(TILE_MANIFEST_PATH).exists() {
        eprintln!("no real tiles under {}; skipped", root.display());
        return;
    }
    let mut app = region_app(&root, RegionSettings::default());
    println!(
        "async compute threads: {}",
        bevy::tasks::AsyncComputeTaskPool::get().thread_num()
    );
    let sequential = GenMode::Budgeted {
        budget: Duration::from_secs(3600),
    };
    let cases: [(&str, f64, f64, f64, GenMode); 8] = [
        ("Alps 0 ka, cold", 46.5, 10.0, 0.0, GenMode::Parallel),
        ("Alps 0 ka, warm", 46.5, 10.0, 0.0, GenMode::Parallel),
        ("Alps 0 ka, warm, sequential", 46.5, 10.0, 0.0, sequential),
        ("Scandinavia 21 ka", 63.0, 15.0, 21.0e3, GenMode::Parallel),
        (
            "Alps 1 Ma (deep, ETOPO)",
            46.5,
            10.0,
            1.0e6,
            GenMode::Parallel,
        ),
        ("Alps 1 Ma, warm", 46.5, 10.0, 1.0e6, GenMode::Parallel),
        ("Alps 1 Ma, warm, sequential", 46.5, 10.0, 1.0e6, sequential),
        ("Pangaea 250 Ma", 0.0, 20.0, 250.0e6, GenMode::Parallel),
    ];
    for (name, lat, lon, years, mode) in cases {
        app.world_mut().resource_mut::<RegionSettings>().mode = mode;
        app.world_mut().resource_mut::<Seen>().generated.clear();
        request(&mut app, lat, lon, years);
        run_until(&mut app, RegionState::InRegion);
        let event = app.world().resource::<Seen>().generated[0];
        let (bytes, needed) = last_tile_stats();
        println!(
            "{name}: {} ms, needed {needed}, fetched {} tiles ({bytes} B), fallback {}",
            event.millis, event.tiles_fetched, event.fallback
        );
        if let Some(line) = logs()
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.contains("region generated"))
        {
            println!("    {line}");
        }
        if let Some(line) = logs()
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.contains("region tiles ready"))
        {
            println!("    {line}");
        }
        app.world_mut().write_message(RegionLeft);
        app.update();
    }
}

/// The Aegean at 1 Ma (the slowest bench region) through the web's time-boxed path, natively, at
/// the small and the large slice budgets: the slice counts behind the web first-entry
/// projection (slices × 16.7 ms at 60 Hz). Prints for performance.md.
#[test]
#[ignore = "needs the real bake and tiles under assets/; prints slice counts"]
fn aegean_time_boxed_slices() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    if !root.join(TILE_MANIFEST_PATH).exists() {
        eprintln!("no real tiles under {}; skipped", root.display());
        return;
    }
    let mut app = region_app(&root, RegionSettings::default());
    // Warm the tile cache and overview first, so both runs time the generation alone.
    request(&mut app, 38.0, 24.5, 1.0e6);
    run_until(&mut app, RegionState::InRegion);
    let mut counts = Vec::new();
    for budget_ms in [6.0, 12.0] {
        app.world_mut().write_message(RegionLeft);
        app.update();
        app.world_mut().resource_mut::<RegionSettings>().mode = GenMode::Budgeted {
            budget: Duration::from_secs_f64(budget_ms / 1000.0),
        };
        app.world_mut().resource_mut::<Seen>().states.clear();
        request(&mut app, 38.0, 24.5, 1.0e6);
        let updates = run_until(&mut app, RegionState::InRegion);
        let line = logs()
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|l| l.contains("time-boxed slices"))
            .cloned()
            .expect("a time-boxed run");
        println!("{budget_ms} ms slices: {updates} updates; {line}");
        counts.push(updates);
    }
    println!(
        "projected wall time at 60 Hz: {:.1} s (6 ms), {:.1} s (12 ms)",
        counts[0] as f64 * 16.7e-3,
        counts[1] as f64 * 16.7e-3
    );
}
