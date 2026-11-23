//! The hidden `--check-data[=DIR]` (native): loads a bake through the real loader
//! ([`GlobeDataPlugin`]) in a window-less app with no renderer, so it needs no GPU, and reports
//! whether it reached [`GlobeLoadState::Ready`]. Then it loads the region tiles in the `tiles`
//! folder beside the globe folder through the game's tile loaders ([`check_tiles`]): the tile
//! manifest, the overview and the [`CHECKED_TILES`] largest tiles, each checked against the
//! manifest's size and SHA-256. CI runs it on the production bake right after baking, so a bake
//! the current reader can't load fails the build.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy::asset::{AssetMetaCheck, LoadState, UntypedAssetId};
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use timefly_core::worldgen::tiles::TILE_SAMPLES;

use crate::args::DataCheck;
use crate::data::{
    GlobeData, GlobeDataPlugin, GlobeLoadError, GlobeLoadState, describe_load_error,
};
use crate::diagnostics::{DiagnosticsPlugin, Session, log_plugin};
use crate::region::frames::{CpuGrid, CpuGridLoader, CpuGridSettings};
use crate::region::tiles::{
    OVERVIEW_PATH, TILE_MANIFEST_PATH, TileManifest, TileManifestLoader, tile_path,
};

/// How long a check may take before it counts as failed. The production bake loads in about a
/// second on a laptop; this leaves room for a slow CI disk.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(300);

/// The game's own bake, relative to its asset folder.
const DEFAULT_GLOBE_DIR: &str = "data/globe";

/// What a successful check loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataSummary {
    pub frames: usize,
    pub images: usize,
    pub plates: usize,
    pub cube_size: u32,
    /// [`crate::data::BakeRevision::summary`].
    pub revision: String,
}

impl std::fmt::Display for DataSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} frames, {} images, {} plates, cube size {}, {}",
            self.frames, self.images, self.plates, self.cube_size, self.revision
        )
    }
}

/// A window-less app that loads the manifest in `globe_dir` (the bake's output folder). With a
/// session, it also has the diagnostics and the real log layers.
fn data_check_app(globe_dir: &Path, session: Option<&Session>) -> App {
    let mut app = App::new();
    if let Some(session) = session {
        // Before `LogPlugin`, which takes its layers from the diagnostics resources.
        app.add_plugins(DiagnosticsPlugin::headless(session))
            .add_plugins(log_plugin());
    }
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: globe_dir.to_string_lossy().into_owned(),
            meta_check: AssetMetaCheck::Never,
            ..default()
        },
        ImagePlugin::default(),
        StatesPlugin,
        GlobeDataPlugin {
            manifest_path: "manifest.json".to_string(),
        },
    ));
    app.finish();
    app.cleanup();
    app
}

/// Updates `app` until the load leaves `Loading` or `timeout` passes.
fn settle(app: &mut App, timeout: Duration) -> Result<DataSummary, String> {
    let deadline = Instant::now() + timeout;
    loop {
        app.update();
        match *app.world().resource::<State<GlobeLoadState>>().get() {
            GlobeLoadState::Ready => {
                let data = app.world().resource::<GlobeData>();
                return Ok(DataSummary {
                    frames: data.timeline.frames().len(),
                    images: data.images.len(),
                    plates: data.crust.rotations().plate_ids.len(),
                    cube_size: data.cube_size,
                    revision: data.revision.summary(),
                });
            }
            GlobeLoadState::Failed => {
                return Err(app
                    .world()
                    .get_resource::<GlobeLoadError>()
                    .map_or_else(|| "unknown error".to_string(), |e| e.0.clone()));
            }
            GlobeLoadState::Loading if Instant::now() >= deadline => {
                return Err(format!("still loading after {timeout:?}"));
            }
            GlobeLoadState::Loading => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

/// Loads the bake in `globe_dir` and summarises it, or says why it failed. No diagnostics; for
/// tests.
pub fn check_globe_data(globe_dir: &Path, timeout: Duration) -> Result<DataSummary, String> {
    settle(&mut data_check_app(&absolute(globe_dir), None), timeout)
}

/// `DIR` relative to the working directory (Bevy resolves a relative asset folder against the
/// executable's directory instead).
fn absolute(dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| dir.to_path_buf(), |cwd| cwd.join(dir))
    }
}

/// The folder to check: `DIR`, or the game's own bake beside the executable (Bevy's default
/// asset folder, `BEVY_ASSET_ROOT` and `cargo run` included).
fn globe_dir(check: &DataCheck) -> PathBuf {
    match &check.dir {
        Some(dir) => absolute(Path::new(dir)),
        None => bevy::asset::io::file::FileAssetReader::get_base_path()
            .join("assets")
            .join(DEFAULT_GLOBE_DIR),
    }
}

/// Tiles [`check_tiles`] loads besides the overview: the largest in the manifest, so the check
/// decodes real data rather than all-ocean tiles.
pub const CHECKED_TILES: usize = 3;

/// What a successful tile check loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileSummary {
    /// Tiles the manifest lists.
    pub listed: usize,
    /// The overview's width and height.
    pub overview: (u32, u32),
    /// The tiles loaded, largest first.
    pub loaded: Vec<(u8, u8)>,
}

impl fmt::Display for TileSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let loaded: Vec<String> = self
            .loaded
            .iter()
            .map(|(r, c)| format!("r{r:02}c{c:02}"))
            .collect();
        write!(
            f,
            "{} tiles listed, overview {}x{}, loaded {} ({}), sizes and SHA-256 match the manifest",
            self.listed,
            self.overview.0,
            self.overview.1,
            self.loaded.len(),
            loaded.join(", ")
        )
    }
}

/// The tile folder beside a globe folder: `…/data/globe` → `…/data/tiles`.
pub fn tiles_dir_beside(globe_dir: &Path) -> PathBuf {
    globe_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("tiles")
}

/// A path's last component: the game's tile paths, relative to the tile folder.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Updates `app` until `id` has loaded, failed, or `deadline` passed.
fn wait_for(
    app: &mut App,
    id: UntypedAssetId,
    name: &str,
    deadline: Instant,
) -> Result<(), String> {
    loop {
        app.update();
        match app.world().resource::<AssetServer>().load_state(id) {
            LoadState::Loaded => return Ok(()),
            LoadState::Failed(e) => return Err(describe_load_error(name, &e)),
            _ if Instant::now() >= deadline => return Err(format!("{name}: still loading")),
            _ => std::thread::sleep(Duration::from_millis(2)),
        }
    }
}

/// Loads the tile manifest in `dir` (a baked `tiles` folder), then the overview and the
/// [`CHECKED_TILES`] largest tiles, through the game's loaders, which check each file's size and
/// SHA-256 against the manifest. Tiles must be [`TILE_SAMPLES`] square.
pub fn check_tiles(dir: &Path, timeout: Duration) -> Result<TileSummary, String> {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: absolute(dir).to_string_lossy().into_owned(),
            meta_check: AssetMetaCheck::Never,
            ..default()
        },
    ))
    .init_asset::<TileManifest>()
    .init_asset_loader::<TileManifestLoader>()
    .init_asset::<CpuGrid>()
    .init_asset_loader::<CpuGridLoader>();
    app.finish();
    app.cleanup();
    let deadline = Instant::now() + timeout;
    let server = app.world().resource::<AssetServer>().clone();

    let manifest_name = file_name(TILE_MANIFEST_PATH);
    let manifest: Handle<TileManifest> = server.load(manifest_name);
    wait_for(&mut app, manifest.id().untyped(), manifest_name, deadline)?;
    let manifest = app
        .world()
        .resource::<Assets<TileManifest>>()
        .get(&manifest)
        .cloned()
        .ok_or_else(|| format!("{manifest_name}: vanished after loading"))?;
    let overview = manifest
        .overview
        .clone()
        .ok_or_else(|| format!("{manifest_name}: lists no overview"))?;
    let mut tiles: Vec<_> = manifest.tiles.iter().collect();
    tiles.sort_by(|a, b| b.1.bytes.cmp(&a.1.bytes).then(a.0.cmp(b.0)));
    if tiles.len() < CHECKED_TILES {
        return Err(format!(
            "{manifest_name}: lists {} tiles, fewer than {CHECKED_TILES}",
            tiles.len()
        ));
    }
    tiles.truncate(CHECKED_TILES);

    let load = |name: &str, settings: CpuGridSettings| {
        server
            .load_builder()
            .with_settings(move |s: &mut CpuGridSettings| *s = settings.clone())
            .load::<CpuGrid>(name.to_string())
    };
    let mut loads = vec![(
        file_name(OVERVIEW_PATH).to_string(),
        load(file_name(OVERVIEW_PATH), overview.settings()),
    )];
    for &(&(row, col), entry) in &tiles {
        let path = tile_path(row, col);
        let name = file_name(&path).to_string();
        let handle = load(&name, entry.settings());
        loads.push((name, handle));
    }
    for (name, handle) in &loads {
        wait_for(&mut app, handle.id().untyped(), name, deadline)?;
    }
    let grids = app.world().resource::<Assets<CpuGrid>>();
    let grid = |(name, handle): &(String, Handle<CpuGrid>)| {
        grids
            .get(handle)
            .map(|g| (g.0.width, g.0.height))
            .ok_or_else(|| format!("{name}: vanished after loading"))
    };
    let overview_size = grid(&loads[0])?;
    for load in &loads[1..] {
        let size = grid(load)?;
        if size != (TILE_SAMPLES, TILE_SAMPLES) {
            return Err(format!(
                "{}: {}x{} samples, expected {TILE_SAMPLES}x{TILE_SAMPLES}",
                load.0, size.0, size.1
            ));
        }
    }
    Ok(TileSummary {
        listed: manifest.tiles.len(),
        overview: overview_size,
        loaded: tiles.iter().map(|(key, _)| **key).collect(),
    })
}

/// Runs the check for `--check-data`: prints one line per part (stdout when it passes, stderr
/// when it fails), logs it, and exits 0 or 1. The tiles are checked once the globe passes.
/// Either way the session ends clean: a failed check is a result, not a crash, so the next
/// launch shows no crash notice.
pub fn run_data_check(session: Session, check: &DataCheck) -> AppExit {
    let dir = globe_dir(check);
    let mut app = data_check_app(&dir, Some(&session));
    let result = settle(&mut app, CHECK_TIMEOUT);
    let exit = match &result {
        Ok(summary) => {
            info!(%summary, "data check passed");
            println!("globe data OK: {summary}");
            match check_tiles(&tiles_dir_beside(&dir), CHECK_TIMEOUT) {
                Ok(tiles) => {
                    info!(%tiles, "tile check passed");
                    println!("region tiles OK: {tiles}");
                    AppExit::Success
                }
                Err(error) => {
                    error!(%error, "tile check failed");
                    session.eprint(&format!("region tiles FAILED: {error}"));
                    AppExit::from_code(1)
                }
            }
        }
        Err(error) => {
            error!(%error, "data check failed");
            session.eprint(&format!("globe data FAILED: {error}"));
            AppExit::from_code(1)
        }
    };
    drop(app);
    session.finish_clean(&exit);
    exit
}
