//! The hidden `--check-data[=DIR]` (native): loads a bake through the real loader
//! ([`GlobeDataPlugin`]) in a window-less app with no renderer, so it needs no GPU, and reports
//! whether it reached [`GlobeLoadState::Ready`]. CI runs it on the production bake right after
//! baking, so a bake the current reader can't load fails the build.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;

use crate::args::DataCheck;
use crate::data::{GlobeData, GlobeDataPlugin, GlobeLoadError, GlobeLoadState};
use crate::diagnostics::{DiagnosticsPlugin, Session, log_plugin};

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

/// Runs the check for `--check-data`: prints one line (stdout when it passes, stderr when it
/// fails), logs it, and exits 0 or 1. Either way the session ends clean: a failed check is a
/// result, not a crash, so the next launch shows no crash notice.
pub fn run_data_check(session: Session, check: &DataCheck) -> AppExit {
    let dir = globe_dir(check);
    let mut app = data_check_app(&dir, Some(&session));
    let result = settle(&mut app, CHECK_TIMEOUT);
    let exit = match &result {
        Ok(summary) => {
            info!(%summary, "data check passed");
            println!("globe data OK: {summary}");
            AppExit::Success
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
