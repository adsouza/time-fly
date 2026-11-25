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
use timefly::hydro::{
    HydroCache, HydroPassFailed, HydroPassFinished, HydroPlugin, HydroPurpose, HydroRequested,
    HydroSettings, HydroStatus,
};
use timefly::region::frames::{CpuGrid, FRAME_CACHE_SLOTS, FrameCache};
use timefly::region::generate::{BudgetedRun, GenInputs, generate_parallel, tiles_needed};
use timefly::region::tiles::{OVERVIEW_PATH, TILE_MANIFEST_PATH, TileCache, tile_path};
use timefly::region::{
    ActiveRegion, GenMode, LOAD_TIMEOUT_S, LargeGenBudget, LoadClock, PendingRegion, RegionEntered,
    RegionGenerated, RegionLeft, RegionPlugin, RegionPreloadCancelled, RegionPreloadFailed,
    RegionRequested, RegionSettings, RegionSnapshotChanged, RegionState, RegionSwapRequested,
    RequestPurpose,
};
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::{Grid, read_tfgrid};
use timefly_core::geo::{RenderDir, geo_to_render, latlon_to_geo};
use timefly_core::hydrology::LatticeSizes;
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
    failed: Vec<RegionPreloadFailed>,
    states: Vec<RegionState>,
}

fn record(
    mut seen: ResMut<Seen>,
    mut generated: MessageReader<RegionGenerated>,
    mut entered: MessageReader<RegionEntered>,
    mut changed: MessageReader<RegionSnapshotChanged>,
    mut failed: MessageReader<RegionPreloadFailed>,
    state: Res<State<RegionState>>,
) {
    seen.failed.extend(failed.read().cloned());
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
    request_for(app, RequestPurpose::Enter, lat, lon, years);
}

fn request_for(app: &mut App, purpose: RequestPurpose, lat: f64, lon: f64, years: f64) {
    app.world_mut().write_message(RegionRequested {
        era_dir: dir(lat, lon),
        snapshot: YearsBp(years),
        purpose,
    });
}

/// Whether the region is centred on (`lat`, `lon`): the spec normalises its centre.
fn centred(region: &ActiveRegion, lat: f64, lon: f64) -> bool {
    region.grid.spec.center.0.dot(dir(lat, lon).0) > 1.0 - 1e-12
}

fn neighbour(app: &mut App, lat: f64, lon: f64) {
    request_for(app, RequestPurpose::Neighbour, lat, lon, 0.0);
}

fn pending(app: &App) -> Option<ActiveRegion> {
    app.world().resource::<PendingRegion>().region.clone()
}

/// Updates until the neighbour is ready in [`PendingRegion`].
fn run_until_pending(app: &mut App) {
    let deadline = Instant::now() + REGION_TIMEOUT;
    while pending(app).is_none() {
        app.update();
        assert!(Instant::now() < deadline, "no neighbour after the timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Updates for `ms` of wall time, long enough for anything wrongly in flight to land.
fn settle(app: &mut App, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        app.update();
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// An app in a region at (0°, 0°), present day, with the state history cleared.
fn app_in_region(settings: RegionSettings) -> App {
    let mut app = region_app(&tiny_root(), settings);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    app.world_mut().resource_mut::<Seen>().states.clear();
    app
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
fn region_loads_all_payloads_from_the_published_generation() {
    let root = tiny_copy("versioned");
    let dir = root.join("data/tiles");
    let generation = "b".repeat(64);
    let payloads = dir.join(&generation);
    fs::create_dir(&payloads).unwrap();
    for entry in fs::read_dir(&dir).unwrap() {
        let file = entry.unwrap();
        if file.path().extension().is_some_and(|ext| ext == "tfgrid") {
            fs::rename(file.path(), payloads.join(file.file_name())).unwrap();
        }
    }
    let path = dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["version"] = 2.into();
    manifest["generation"] = generation.into();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut app = region_app(&root, SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    assert!(!generated(&app).fallback);
    assert_eq!(generated(&app).tiles_fetched, 4);
    assert!(app.world().resource::<TileCache>().overview().is_some());
}

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
    // Stalled from the start: the overview loads with the globe data, not with the request.
    let stalls = Stalls::default();
    stalls.lock().unwrap().push(OVERVIEW_PATH.into());
    let mut app = region_app_with(&tiny_root(), SMALL, Some(stalls), |_| {});
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
        // The overview loads with the globe data; held in flight until the job is cancelled.
        let stalls = Stalls::default();
        stalls.lock().unwrap().push(OVERVIEW_PATH.into());
        let mut app = region_app_with(&tiny_root(), SMALL, Some(stalls.clone()), |_| {});
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
        stalls.lock().unwrap().clear();
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

/// A region at the snapshot just left (the bench's Beringia → Rockies pan, both 21 ka) takes its
/// frames from the frame cache: `RegionGenerated::frames_fetched` is 0.
#[test]
fn a_same_snapshot_entry_fetches_no_frames() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 21.0e3);
    run_until(&mut app, RegionState::InRegion);
    let first = generated(&app);
    assert!(first.frames_fetched > 0, "{first:?}");
    app.world_mut().write_message(RegionLeft);
    app.update();
    app.update();
    app.world_mut().resource_mut::<Seen>().generated.clear();
    app.world_mut().resource_mut::<Seen>().states.clear();
    request(&mut app, 2.0, 3.0, 21.0e3);
    run_until(&mut app, RegionState::InRegion);
    assert_eq!(generated(&app).frames_fetched, 0);
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
        hydro: None,
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

// ---------------------------------------------------------------------------------------------
// Task 8: preloading a neighbour

#[test]
fn a_neighbour_generates_without_replacing_the_active_region() {
    let mut app = app_in_region(SMALL);
    let before = active(&app);
    neighbour(&mut app, 0.0, 0.3);
    run_until_pending(&mut app);
    // The region is untouched: same grid, same state, never `Generating`, no `RegionEntered`.
    assert!(Arc::ptr_eq(&active(&app).grid, &before.grid));
    assert_eq!(state(&app), RegionState::InRegion);
    assert_eq!(
        app.world().resource::<Seen>().states,
        vec![RegionState::InRegion]
    );
    assert_eq!(app.world().resource::<Seen>().entered.len(), 1);
    let ready = pending(&app).unwrap();
    assert!(centred(&ready, 0.0, 0.3));
    assert!(!centred(&before, 0.0, 0.3));

    app.world_mut().write_message(RegionSwapRequested);
    app.update();
    // The promote happens in the frame the message is read.
    assert!(centred(&active(&app), 0.0, 0.3));
    assert!(pending(&app).is_none());
    assert_eq!(state(&app), RegionState::InRegion);
    let seen = app.world().resource::<Seen>();
    assert_eq!(seen.entered.len(), 2);
    assert_eq!(seen.entered[1].spec, ready.grid.spec);
    assert!(seen.changed.is_empty());
}

#[test]
fn cancelling_a_preload_keeps_the_region() {
    let mut app = app_in_region(SMALL);
    let before = active(&app);
    neighbour(&mut app, 0.0, 0.3);
    app.update();
    app.update();
    app.world_mut().write_message(RegionPreloadCancelled);
    settle(&mut app, 500);
    assert!(pending(&app).is_none(), "a cancelled job must not land");
    assert_eq!(app.world().resource::<Seen>().generated.len(), 1);
    assert!(Arc::ptr_eq(&active(&app).grid, &before.grid));
    assert_eq!(state(&app), RegionState::InRegion);

    // A cancel after the neighbour is ready drops it too: it is never promoted later.
    neighbour(&mut app, 0.0, 0.3);
    run_until_pending(&mut app);
    let weak = Arc::downgrade(&pending(&app).unwrap().grid);
    app.world_mut().write_message(RegionPreloadCancelled);
    app.update();
    assert!(pending(&app).is_none());
    assert!(weak.upgrade().is_none(), "the cancelled grid leaked");
    app.world_mut().write_message(RegionSwapRequested);
    app.update();
    assert!(Arc::ptr_eq(&active(&app).grid, &before.grid));
}

#[test]
fn a_preload_that_times_out_reports_failure() {
    let (mut app, stalls) = stalling_region_app(&tiny_root(), SMALL);
    stalls.lock().unwrap().push(tile_path(18, 36).into());
    one_second_per_update(&mut app);
    // The active region itself falls back on the stalled tile, after its own timeout.
    request(&mut app, 0.0, 0.0, 0.0);
    run_until(&mut app, RegionState::InRegion);
    let before = active(&app);
    app.world_mut().resource_mut::<Seen>().states.clear();

    neighbour(&mut app, 0.0, 0.3);
    let deadline = Instant::now() + REGION_TIMEOUT;
    while app.world().resource::<Seen>().failed.is_empty() {
        app.update();
        assert!(Instant::now() < deadline, "no failure reported");
        std::thread::sleep(Duration::from_millis(1));
    }
    let seen = app.world().resource::<Seen>();
    assert_eq!(seen.failed.len(), 1);
    assert!(
        seen.failed[0].reason.contains("timed out"),
        "{:?}",
        seen.failed
    );
    assert_eq!(seen.generated.len(), 1, "only the active region");
    assert!(pending(&app).is_none());
    assert!(Arc::ptr_eq(&active(&app).grid, &before.grid));
    assert_eq!(state(&app), RegionState::InRegion);
    assert_eq!(
        app.world().resource::<Seen>().states,
        vec![RegionState::InRegion]
    );
    let warnings = warnings_naming("preload");
    assert!(
        warnings.iter().any(|w| w.contains("timed out")),
        "{warnings:?}"
    );
}

#[test]
fn a_hop_holds_two_grids_at_most() {
    let mut app = app_in_region(SMALL);
    let old = Arc::downgrade(&active(&app).grid);
    neighbour(&mut app, 0.0, 0.3);
    run_until_pending(&mut app);
    let new = Arc::downgrade(&pending(&app).unwrap().grid);
    // Two grids live: the active one and the neighbour.
    assert_eq!(old.strong_count(), 1);
    assert_eq!(new.strong_count(), 1);
    app.world_mut().write_message(RegionSwapRequested);
    app.update();
    app.update();
    assert!(pending(&app).is_none());
    assert!(old.upgrade().is_none(), "the old grid is still held");
    assert_eq!(new.strong_count(), 1, "the new grid is the active region's");
    assert_eq!(Arc::as_ptr(&active(&app).grid), new.as_ptr());
}

#[test]
fn leaving_drops_a_neighbour_in_flight_and_a_ready_one() {
    // In flight.
    let mut app = app_in_region(SMALL);
    neighbour(&mut app, 0.0, 0.3);
    app.update();
    app.world_mut().write_message(RegionLeft);
    settle(&mut app, 500);
    assert_eq!(state(&app), RegionState::Globe);
    assert!(pending(&app).is_none(), "a stale neighbour landed");
    assert_eq!(app.world().resource::<Seen>().generated.len(), 1);

    // Ready.
    let mut app = app_in_region(SMALL);
    neighbour(&mut app, 0.0, 0.3);
    run_until_pending(&mut app);
    let weak = Arc::downgrade(&pending(&app).unwrap().grid);
    app.world_mut().write_message(RegionLeft);
    app.update();
    app.update();
    assert!(pending(&app).is_none());
    assert!(weak.upgrade().is_none(), "the neighbour's grid leaked");
    // A swap after leaving has nothing to promote.
    app.world_mut().write_message(RegionSwapRequested);
    app.update();
    assert!(app.world().get_resource::<ActiveRegion>().is_none());
    assert_eq!(state(&app), RegionState::Globe);
}

/// The concurrency rule: a neighbour request is ignored while an enter or snapshot job runs.
#[test]
fn a_neighbour_is_ignored_while_an_enter_job_runs() {
    let mut app = region_app(&tiny_root(), SMALL);
    request(&mut app, 0.0, 0.0, 0.0);
    app.update();
    app.update();
    assert_eq!(state(&app), RegionState::Generating);
    neighbour(&mut app, 0.0, 0.3);
    run_until(&mut app, RegionState::InRegion);
    settle(&mut app, 500);
    assert!(pending(&app).is_none());
    assert_eq!(app.world().resource::<Seen>().generated.len(), 1);
    assert!(centred(&active(&app), 0.0, 0.0));

    // And while a snapshot change runs.
    app.world_mut().resource_mut::<Seen>().states.clear();
    request_for(&mut app, RequestPurpose::Snapshot, 0.0, 0.0, 100.0e6);
    app.update();
    app.update();
    assert_eq!(state(&app), RegionState::Generating);
    neighbour(&mut app, 0.0, 0.3);
    run_until(&mut app, RegionState::InRegion);
    settle(&mut app, 500);
    assert!(pending(&app).is_none());
    assert_eq!(app.world().resource::<Seen>().generated.len(), 2);
}

/// The concurrency rule: a newer neighbour request cancels an older one.
#[test]
fn a_newer_neighbour_cancels_an_older_one() {
    let slow = RegionSettings {
        mode: GenMode::Budgeted {
            budget: Duration::from_micros(200),
        },
        ..SMALL
    };
    let mut app = app_in_region(slow);
    neighbour(&mut app, 0.0, 0.3);
    app.update();
    neighbour(&mut app, 0.3, 0.0);
    run_until_pending(&mut app);
    settle(&mut app, 300);
    assert!(centred(&pending(&app).unwrap(), 0.3, 0.0));
    // The active region and the newer neighbour only: the older one never landed.
    assert_eq!(app.world().resource::<Seen>().generated.len(), 2);
}

/// A new enter or snapshot request invalidates a ready neighbour (it may be at another
/// snapshot), so a later swap cannot promote it.
#[test]
fn a_snapshot_request_drops_the_ready_neighbour() {
    let mut app = app_in_region(SMALL);
    neighbour(&mut app, 0.0, 0.3);
    run_until_pending(&mut app);
    request_for(&mut app, RequestPurpose::Snapshot, 0.0, 0.0, 100.0e6);
    app.update();
    assert!(pending(&app).is_none());
    run_until(&mut app, RegionState::InRegion);
    app.world_mut().write_message(RegionSwapRequested);
    app.update();
    assert_eq!(active(&app).grid.spec.snapshot, YearsBp(100.0e6));
    assert!(centred(&active(&app), 0.0, 0.0));
}

// ---------------------------------------------------------------------------------------------
// Task 9: a hop never waits on a neighbour request that gets no answer

/// A neighbour request the job will not take (no region to be a neighbour of, an enter or
/// snapshot job running, or another request winning the same frame) answers with
/// `RegionPreloadFailed`, so whoever asked never waits for "ready or failed" in vain.
#[test]
fn an_ignored_neighbour_reports_failure() {
    let failures = |app: &App| app.world().resource::<Seen>().failed.len();
    // No active region.
    let mut app = region_app(&tiny_root(), SMALL);
    neighbour(&mut app, 0.0, 0.3);
    app.update();
    assert_eq!(failures(&app), 1);
    assert!(pending(&app).is_none());

    // While an enter job runs.
    request(&mut app, 0.0, 0.0, 0.0);
    app.update();
    app.update();
    assert_eq!(state(&app), RegionState::Generating);
    neighbour(&mut app, 0.0, 0.3);
    app.update();
    assert_eq!(failures(&app), 2);
    run_until(&mut app, RegionState::InRegion);
    assert!(pending(&app).is_none());

    // In the same frame as a snapshot request, which wins.
    neighbour(&mut app, 0.0, 0.3);
    request_for(&mut app, RequestPurpose::Snapshot, 0.0, 0.0, 100.0e6);
    app.update();
    assert_eq!(failures(&app), 3);
    run_until(&mut app, RegionState::InRegion);
    settle(&mut app, 300);
    assert!(pending(&app).is_none());
    assert_eq!(
        failures(&app),
        3,
        "{:?}",
        app.world().resource::<Seen>().failed
    );
    let reasons: Vec<_> = app
        .world()
        .resource::<Seen>()
        .failed
        .iter()
        .map(|f| f.reason.clone())
        .collect();
    assert!(reasons.iter().all(|r| !r.is_empty()), "{reasons:?}");
}

// ---------------------------------------------------------------------------------------------
// Task 9, fix round 1: hops through the real region job

mod hops {
    use super::*;
    use timefly::fly_camera::FlyState;
    use timefly::region::navigate::{NavPhase, Navigation, RegionNavigationPlugin};
    use timefly::time::{GlobeTime, TimePlugin};
    use timefly_core::flight::hop::EDGE_M;

    /// Navigation over the tiny bake, in the region at (0°, 0°) at `years`, flying (the fly's
    /// pose is scripted, as the bench writes it), `step` a frame.
    fn hop_app(stalls: Option<Stalls>, years: f64, step: Duration) -> App {
        hop_app_with(stalls, years, step, |_| {})
    }

    /// [`hop_app`], with `more` plugins (added after navigation, before the first update).
    pub(super) fn hop_app_with(
        stalls: Option<Stalls>,
        years: f64,
        step: Duration,
        more: impl FnOnce(&mut App),
    ) -> App {
        let mut app = region_app_with(&tiny_root(), SMALL, stalls, |app| {
            app.init_resource::<ButtonInput<KeyCode>>()
                .add_plugins((TimePlugin, RegionNavigationPlugin));
            more(app);
        });
        app.insert_resource(TimeUpdateStrategy::ManualDuration(step));
        app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(years);
        request(&mut app, 0.0, 0.0, years);
        let deadline = Instant::now() + REGION_TIMEOUT * 2;
        while phase(&app) != NavPhase::Flying {
            app.update();
            assert!(Instant::now() < deadline, "never flying: {:?}", phase(&app));
            std::thread::sleep(Duration::from_millis(1));
        }
        app
    }

    pub(super) fn phase(app: &App) -> NavPhase {
        app.world().resource::<Navigation>().phase
    }

    /// Flies the fly east from 25 km short of the edge at `speed` (at its controls' share of
    /// it, as the fly camera would), until `done`.
    pub(super) fn fly_east(app: &mut App, speed: f32, step: Duration, done: impl Fn(&App) -> bool) {
        {
            let mut fly = app.world_mut().resource_mut::<FlyState>();
            fly.pos_m = Vec3::new(EDGE_M - 25_000.0, 3_000.0, 0.0);
            fly.yaw = -std::f32::consts::FRAC_PI_2;
        }
        app.update();
        let deadline = Instant::now() + REGION_TIMEOUT * 2;
        while !done(app) {
            let share = phase(app).fly_controls().map_or(0.0, |c| c.horizontal);
            let dx = speed * share * step.as_secs_f32();
            app.world_mut().resource_mut::<FlyState>().pos_m.x += dx;
            app.update();
            assert!(Instant::now() < deadline, "still {:?}", phase(app));
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// At a deep era the neighbour is an era-frame direction like the region's own: no crust
    /// position is resolved for it, so it generates and the hop swaps there as at the present
    /// (missing crust cannot fail a hop; spec §5.2, era-fixed points stay explorable).
    #[test]
    fn a_deep_era_hop_swaps_without_resolving_crust() {
        let step = Duration::from_millis(50);
        let mut app = hop_app(None, 100.0e6, step);
        let before = active(&app);
        fly_east(&mut app, 10_000.0, step, |a| {
            matches!(phase(a), NavPhase::Hopping { swapped: true, .. })
        });
        assert_eq!(
            app.world().resource::<timefly::cloud::CloudDensity>().0,
            1.0
        );
        let after = active(&app);
        assert_eq!(after.grid.spec.snapshot, YearsBp(100.0e6));
        let (x, z) = before.frame.dir_to_local(after.frame.center).unwrap();
        assert!(
            (x - 250_000.0).abs() < 5_000.0 && z.abs() < 5_000.0,
            "({x}, {z})"
        );
        assert!(app.world().resource::<Seen>().failed.is_empty());
    }

    /// A neighbour the real job fails (its tile load stalls past the timeout) turns the fly round
    /// 10 km inside the edge under the opaque cloud; the region stays.
    #[test]
    fn a_hop_whose_neighbour_times_out_turns_back() {
        let stalls = Stalls::default();
        stalls.lock().unwrap().push(tile_path(18, 36).into());
        let step = Duration::from_secs(1);
        let mut app = hop_app(Some(stalls), 0.0, step);
        let before = active(&app);
        fly_east(&mut app, 10_000.0, step, |a| {
            matches!(
                phase(a),
                NavPhase::Hopping {
                    turned_back: true,
                    ..
                }
            )
        });
        assert_eq!(
            app.world().resource::<timefly::cloud::CloudDensity>().0,
            1.0
        );
        let seen = app.world().resource::<Seen>();
        assert_eq!(seen.failed.len(), 1, "{:?}", seen.failed);
        assert!(
            seen.failed[0].reason.contains("timed out"),
            "{:?}",
            seen.failed
        );
        assert!(Arc::ptr_eq(&active(&app).grid, &before.grid));
        let fly = *app.world().resource::<FlyState>();
        assert_eq!(fly.pos_m.x, EDGE_M - 10_000.0);
        assert!(
            (fly.yaw - std::f32::consts::FRAC_PI_2).abs() < 1e-5,
            "{}",
            fly.yaw
        );
        assert_eq!(state(&app), RegionState::InRegion);
    }
}

// ---------------------------------------------------------------------------------------------
// Rivers and lakes, Task 4: the continental pass in the shell

/// Lattices small enough for a test: 6 × 16² nodes in recent eras, 6 × 8² in deep ones.
const HYDRO_SIZES: LatticeSizes = LatticeSizes {
    recent_n: 16,
    deep_n: 8,
};

/// The pass's messages and the regions generated, in the order they were sent.
#[derive(Clone, Debug, PartialEq)]
enum HydroEvent {
    Requested(HydroRequested),
    Finished(HydroPassFinished),
    Failed(HydroPassFailed),
    Generated(RegionGenerated),
}

#[derive(Resource, Default)]
struct HydroSeen(Vec<HydroEvent>);

impl HydroSeen {
    fn requested(&self) -> Vec<HydroRequested> {
        self.0
            .iter()
            .filter_map(|e| match e {
                HydroEvent::Requested(r) => Some(*r),
                _ => None,
            })
            .collect()
    }

    fn finished(&self) -> Vec<HydroPassFinished> {
        self.0
            .iter()
            .filter_map(|e| match e {
                HydroEvent::Finished(f) => Some(*f),
                _ => None,
            })
            .collect()
    }

    fn generated(&self) -> Vec<RegionGenerated> {
        self.0
            .iter()
            .filter_map(|e| match e {
                HydroEvent::Generated(g) => Some(*g),
                _ => None,
            })
            .collect()
    }

    fn position(&self, f: impl Fn(&HydroEvent) -> bool) -> Option<usize> {
        self.0.iter().position(f)
    }
}

/// Read in `Last` (`MinimalPlugins` swaps message buffers only after a fixed update).
fn record_hydro(
    mut seen: ResMut<HydroSeen>,
    mut requested: MessageReader<HydroRequested>,
    mut finished: MessageReader<HydroPassFinished>,
    mut failed: MessageReader<HydroPassFailed>,
    mut generated: MessageReader<RegionGenerated>,
) {
    seen.0
        .extend(requested.read().map(|r| HydroEvent::Requested(*r)));
    seen.0
        .extend(failed.read().map(|f| HydroEvent::Failed(f.clone())));
    seen.0
        .extend(finished.read().map(|f| HydroEvent::Finished(*f)));
    seen.0
        .extend(generated.read().map(|g| HydroEvent::Generated(*g)));
}

/// Adds the pass at the test sizes, globe time and the recorder.
fn add_hydro(app: &mut App, enabled: bool) {
    add_hydro_sized(app, enabled, HYDRO_SIZES);
}

fn add_hydro_sized(app: &mut App, enabled: bool, sizes: LatticeSizes) {
    use timefly::time::TimePlugin;
    if !app.is_plugin_added::<TimePlugin>() {
        app.add_plugins(TimePlugin);
    }
    app.add_plugins(HydroPlugin)
        .insert_resource(HydroSettings { enabled, sizes })
        .init_resource::<HydroSeen>()
        .add_systems(Last, record_hydro);
}

/// A headless app with the globe data, regions and the continental pass, the globe data loaded.
fn hydro_app(root: &Path, stalls: Option<Stalls>, more: impl FnOnce(&mut App)) -> App {
    region_app_with(root, SMALL, stalls, |app| {
        add_hydro(app, true);
        more(app);
    })
}

fn hydro_seen(app: &App) -> &HydroSeen {
    app.world().resource::<HydroSeen>()
}

/// Updates until `done`, failing after the timeout.
fn update_until(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + REGION_TIMEOUT;
    while !done(app) {
        app.update();
        assert!(
            Instant::now() < deadline,
            "{what}: not after {REGION_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn ka(k: f64) -> YearsBp {
    YearsBp(k * 1000.0)
}

/// The tile manifest and the overview load once the globe data is ready, with no region
/// requested: the continental pass reads the overview on the globe.
#[test]
fn the_tile_index_loads_with_the_globe() {
    let mut app = region_app(&tiny_root(), SMALL);
    update_until(&mut app, "the overview", |app| {
        app.world().resource::<TileCache>().overview().is_some()
    });
    assert_eq!(state(&app), RegionState::Globe);
    assert!(app.world().resource::<Seen>().generated.is_empty());
    assert!(
        app.world().resource::<TileCache>().is_empty(),
        "no tiles yet"
    );
}

/// The present day's pass runs on the globe once the globe data is ready and the time has
/// settled, with no region job.
#[test]
fn the_pass_runs_once_the_globe_data_is_ready() {
    let mut app = hydro_app(&tiny_root(), None, |_| {});
    update_until(&mut app, "the 0 ka pass", |app| {
        !hydro_seen(app).finished().is_empty()
    });
    let seen = hydro_seen(&app);
    assert_eq!(
        seen.requested(),
        [HydroRequested {
            age: YearsBp(0.0),
            purpose: HydroPurpose::Prefetch
        }]
    );
    let done = seen.finished()[0];
    assert_eq!(done.age, YearsBp(0.0));
    assert_eq!(done.nodes, 6 * 16 * 16);
    assert_eq!((done.slices, done.platform), (1, "native"));
    assert!(done.wall_ms >= done.cpu_ms, "{done:?}");
    assert!(seen.generated().is_empty());
    assert_eq!(state(&app), RegionState::Globe);
    let cache = app.world().resource::<HydroCache>();
    assert!(matches!(cache.status(YearsBp(0.0)), HydroStatus::Ready(h) if h.age == YearsBp(0.0)));
    assert!(cache.running().is_none());
}

/// A dive at 21 ka waits for its era's pass, which it asks for once, and generates with it.
#[test]
fn a_dive_waits_for_its_eras_pass() {
    let mut app = hydro_app(&tiny_root(), None, |_| {});
    request(&mut app, 0.0, 0.0, 21.0e3);
    run_until(&mut app, RegionState::InRegion);
    let seen = hydro_seen(&app);
    let finished = seen
        .position(|e| matches!(e, HydroEvent::Finished(f) if f.age == ka(21.0)))
        .expect("the 21 ka pass finished");
    let generated = seen
        .position(|e| matches!(e, HydroEvent::Generated(_)))
        .expect("the region generated");
    assert!(finished < generated, "{:?}", seen.0);
    let region = seen.generated()[0];
    assert!(region.hydrology, "{region:?}");
    assert!(region.hydro_wait_ms > 0, "{region:?}");
    let asked = seen
        .requested()
        .into_iter()
        .filter(|r| r.purpose == HydroPurpose::Region)
        .collect::<Vec<_>>();
    assert_eq!(
        asked,
        [HydroRequested {
            age: ka(21.0),
            purpose: HydroPurpose::Region
        }]
    );
}

/// Review Focus 1: without the climate data (an older bake, a failed fetch) the pass falls back
/// to the zonal rule with one `warn`, finishes, and the region still gets its hydrology without
/// waiting for any timeout.
#[test]
fn missing_climate_falls_back_to_the_zonal_rule() {
    let root = tiny_copy("no-climate");
    fs::remove_dir_all(root.join("data/climate")).unwrap();
    let mut app = hydro_app(&root, None, |_| {});
    request(&mut app, 0.0, 0.0, 21.0e3);
    run_until(&mut app, RegionState::InRegion);
    // Another era, so a second pass also meets the missing climate.
    app.world_mut().write_message(RegionLeft);
    request(&mut app, 0.0, 0.0, 10.0e3);
    app.world_mut().resource_mut::<Seen>().states.clear();
    run_until(&mut app, RegionState::InRegion);
    let warnings = warnings_naming("climate data unavailable");
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("data/climate/manifest.json"),
        "{warnings:?}"
    );
    let seen = hydro_seen(&app);
    let ages: Vec<YearsBp> = seen.finished().iter().map(|f| f.age).collect();
    assert!(
        ages.contains(&ka(21.0)) && ages.contains(&ka(10.0)),
        "{ages:?}"
    );
    for region in seen.generated() {
        assert!(region.hydrology, "{region:?}");
        assert!(region.hydro_wait_ms < 5_000, "{region:?}");
    }
    let zonal = logs()
        .lock()
        .unwrap()
        .iter()
        .filter(|l| {
            l.contains("continental pass done")
                && l.contains("age_years=21000.0")
                && l.contains("climate=\"zonal\"")
        })
        .count();
    assert!(zonal >= 1, "the 21 ka pass used the zonal rule");
}

/// Review Focus 2: the slider dragged through ten snapshots, one per frame, then held: one pass
/// runs, for the era it settled on.
#[test]
fn dragging_the_slider_runs_one_pass_for_the_settled_era() {
    use timefly::time::GlobeTime;
    use timefly_core::deep_time::STANDARD_SNAPSHOTS;
    // The load clock stands still while the globe loads, so the present never settles.
    let mut app = hydro_app(&tiny_root(), None, |app| {
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::ZERO));
    });
    app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )));
    for &years in &STANDARD_SNAPSHOTS[1..=10] {
        app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(years);
        app.update();
    }
    let held = YearsBp(STANDARD_SNAPSHOTS[10]);
    assert_eq!(held, YearsBp(1.0e6));
    update_until(&mut app, "the held era's pass", |app| {
        !hydro_seen(app).finished().is_empty()
    });
    // Long enough for anything else wrongly asked for to finish too.
    settle(&mut app, 500);
    let seen = hydro_seen(&app);
    assert_eq!(
        seen.requested(),
        [HydroRequested {
            age: held,
            purpose: HydroPurpose::Prefetch
        }]
    );
    let finished = seen.finished();
    assert_eq!(finished.len(), 1, "{finished:?}");
    assert_eq!(finished[0].age, held);
    assert_eq!(finished[0].nodes, 6 * 8 * 8, "a deep era's lattice");
    assert!(app.world().resource::<HydroCache>().len() <= 2);
}

/// Review Focus 2: a region's request replaces a prefetch in flight, so the region's era
/// finishes first (here the prefetch is held by a stalled climate grid and never finishes).
#[test]
fn a_region_request_preempts_a_prefetch() {
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(tiny_root().join("data/climate/manifest.json")).unwrap())
            .unwrap();
    let present = format!(
        "data/climate/{}",
        manifest["files"][0]["file"].as_str().unwrap()
    );
    assert!(present.contains("bio_0ka"));
    let stalls = Stalls::default();
    stalls.lock().unwrap().push(present.into());
    let mut app = hydro_app(&tiny_root(), Some(stalls.clone()), |_| {});
    update_until(&mut app, "the present day's prefetch in flight", |app| {
        app.world().resource::<HydroCache>().status(YearsBp(0.0)) == HydroStatus::Running
    });
    request(&mut app, 0.0, 0.0, 21.0e3);
    run_until(&mut app, RegionState::InRegion);
    settle(&mut app, 300);
    let seen = hydro_seen(&app);
    assert_eq!(
        seen.requested(),
        [
            HydroRequested {
                age: YearsBp(0.0),
                purpose: HydroPurpose::Prefetch
            },
            HydroRequested {
                age: ka(21.0),
                purpose: HydroPurpose::Region
            }
        ]
    );
    let ages: Vec<YearsBp> = seen.finished().iter().map(|f| f.age).collect();
    assert_eq!(ages, [ka(21.0)], "the prefetch was replaced");
    assert!(seen.generated()[0].hydrology);
    let cache = app.world().resource::<HydroCache>();
    assert_eq!(cache.status(YearsBp(0.0)), HydroStatus::Absent);
    stalls.lock().unwrap().clear();
}

/// Disabled, no pass runs, not even on the settled globe, and a region never waits.
#[test]
fn a_disabled_pass_never_holds_a_region() {
    let mut app = region_app_with(&tiny_root(), SMALL, None, |app| add_hydro(app, false));
    settle(&mut app, 700);
    request(&mut app, 0.0, 0.0, 21.0e3);
    run_until(&mut app, RegionState::InRegion);
    let seen = hydro_seen(&app);
    let region = seen.generated()[0];
    assert!(!region.hydrology, "{region:?}");
    assert_eq!(region.hydro_wait_ms, 0);
    assert!(
        seen.requested().is_empty() && seen.finished().is_empty(),
        "{:?}",
        seen.0
    );
    assert!(app.world().resource::<HydroCache>().is_empty());
}

/// A hop's neighbour waits for its era's pass like a dive (here the cache was emptied in
/// flight), and the hop swaps to a neighbour that has it.
#[test]
fn a_hop_neighbour_waits_for_its_eras_pass() {
    use hops::{fly_east, hop_app_with, phase};
    use timefly::region::navigate::NavPhase;
    let step = Duration::from_millis(50);
    let mut app = hop_app_with(None, 0.0, step, |app| add_hydro(app, true));
    let entered = hydro_seen(&app).generated();
    assert_eq!(entered.len(), 1);
    assert!(entered[0].hydrology, "{entered:?}");
    app.insert_resource(HydroCache::default());
    let before = hydro_seen(&app).0.len();
    fly_east(&mut app, 10_000.0, step, |a| {
        matches!(phase(a), NavPhase::Hopping { swapped: true, .. })
    });
    let seen = &hydro_seen(&app).0[before..];
    let asked = seen
        .iter()
        .position(|e| {
            matches!(e, HydroEvent::Requested(r)
                if *r == HydroRequested { age: YearsBp(0.0), purpose: HydroPurpose::Region })
        })
        .expect("the neighbour asked for its era");
    let finished = seen
        .iter()
        .position(|e| matches!(e, HydroEvent::Finished(f) if f.age == YearsBp(0.0)))
        .expect("the era's pass ran again");
    let generated = seen
        .iter()
        .position(|e| matches!(e, HydroEvent::Generated(_)))
        .expect("the neighbour generated");
    assert!(asked < finished && finished < generated, "{seen:?}");
    let HydroEvent::Generated(neighbour) = &seen[generated] else {
        unreachable!()
    };
    assert!(
        neighbour.hydrology && neighbour.hydro_wait_ms > 0,
        "{neighbour:?}"
    );
    assert!(app.world().resource::<Seen>().failed.is_empty());
}

/// The real present day's pass at the plan's sizes (n = 900), natively: the in-app task (the
/// globe's 0 ka prefetch: loads, parallel sampling and the pass), then the same inputs through
/// the web's time-boxed path at 6 and 12 ms on one thread. Prints for the Task 4 report.
///
/// The worktree has no globe data: point `TIMEFLY_REAL_ASSET_ROOT` at a folder whose `data`
/// links to the main checkout's `crates/timefly/assets/data`.
#[test]
#[ignore = "needs the real bake, tiles and climate; prints the pass's native timing"]
fn real_continental_pass_timing() {
    use std::sync::atomic::AtomicBool;
    use timefly::hydro::climate::{CLIMATE_MANIFEST_PATH, ClimateManifest, climate_table};
    use timefly::hydro::job::{HydroInputs, HydroRun, run_parallel};

    let root = std::env::var_os("TIMEFLY_REAL_ASSET_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets"));
    if !root.join(TILE_MANIFEST_PATH).exists() {
        eprintln!("no real data under {}; skipped", root.display());
        return;
    }
    let mut app = region_app_with(&root, RegionSettings::default(), None, |app| {
        add_hydro_sized(app, true, LatticeSizes::default());
    });
    println!(
        "async compute threads: {}",
        bevy::tasks::AsyncComputeTaskPool::get().thread_num()
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    while hydro_seen(&app).finished().is_empty() {
        app.update();
        assert!(Instant::now() < deadline, "no pass after 120 s");
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("in-app: {:?}", hydro_seen(&app).finished()[0]);
    for line in logs().lock().unwrap().iter() {
        if line.contains("continental pass done") || line.contains("lake level table") {
            println!("    {line}");
        }
    }

    // The same inputs, read from the files, for the time-boxed path.
    let data = app.world().resource::<GlobeData>();
    let read = |path: &str| Arc::new(read_tfgrid(&fs::read(root.join(path)).unwrap()).unwrap());
    let frames: Vec<Option<Arc<Grid>>> = (0..data.timeline.frames().len())
        .map(|f| Some(read(data.frame_path(f).path().to_str().unwrap())))
        .collect();
    let mut tiles = TileSet::new();
    tiles
        .set_overview(
            app.world()
                .resource::<TileCache>()
                .overview()
                .unwrap()
                .clone(),
        )
        .unwrap();
    let climate =
        ClimateManifest::parse(&fs::read(root.join(CLIMATE_MANIFEST_PATH)).unwrap()).unwrap();
    let ([y, o], w) = climate.bracket(YearsBp(0.0)).unwrap();
    let inputs = HydroInputs {
        age: YearsBp(0.0),
        sizes: LatticeSizes::default(),
        timeline: data.timeline.clone(),
        crust: data.crust.clone(),
        tiles,
        frames,
        climate: Some(([read(&y.path()), read(&o.path())], w)),
        table: climate_table(),
    };
    let t = Instant::now();
    let parallel = run_parallel(&inputs, &AtomicBool::new(false)).unwrap();
    println!(
        "run_parallel again: {:?} (sampling {:?}, longest step {:?}, peak {:.1} MiB, result {:.1} MiB)",
        t.elapsed(),
        parallel.sampling,
        parallel.longest_step,
        parallel.peak_bytes as f64 / 1048576.0,
        parallel.hydrology.bytes() as f64 / 1048576.0
    );
    for budget_ms in [6u64, 12] {
        let mut run = HydroRun::new(inputs.clone(), Duration::from_millis(budget_ms));
        let t = Instant::now();
        let out = loop {
            if let Some(r) = run.poll_slice() {
                break r.unwrap();
            }
        };
        assert_eq!(out.hydrology, parallel.hydrology);
        println!(
            "time-boxed at {budget_ms} ms: {:?} wall, {} slices, cpu {:?}, longest slice {:?}, \
             longest step {:?}; at 60 Hz ≈ {:.1} s",
            t.elapsed(),
            out.slices,
            out.cpu,
            out.longest_slice,
            out.longest_step,
            f64::from(out.slices) * 16.7e-3
        );
    }
}
