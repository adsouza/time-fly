use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::window::{PresentMode, WindowResolution};

pub mod args;
pub mod art;
pub mod bench;
pub mod build_features;
pub mod camera;
#[cfg(not(target_arch = "wasm32"))]
pub mod check_data;
pub mod clock;
pub mod clusters;
pub mod data;
pub mod diagnostics;
pub mod fly_camera;
pub mod globe;
pub mod gpu;
pub mod region;
pub mod sky;
pub mod terrain;
pub mod time;
pub mod ui;
pub mod water;
#[cfg(not(target_arch = "wasm32"))]
pub mod window_memory;

use args::LaunchArgs;
use bench::ScrubBenchPlugin;
use bench::region::{RegionBenchConfig, RegionBenchPlugin};
use camera::OrbitCameraPlugin;
use data::GlobeDataPlugin;
use diagnostics::{DiagnosticsPlugin, Session};
use fly_camera::FlyCameraPlugin;
use globe::GlobePlugin;
use region::RegionPlugin;
use region::launch::LaunchRegion;
use region::navigate::RegionNavigationPlugin;
use terrain::TerrainCdlodPlugin;
use ui::GameUiPlugins;

/// Reverse-DNS application ID, shared with `packaging/linux` and `packaging/flatpak`.
pub const APP_ID: &str = "io.github.adsouza.TimeFly";

/// The primary window for the launch arguments: `--window WxH` sets its size in physical
/// pixels, and the benchmarks turn vsync off on native so frame times show the real cost.
pub fn primary_window(args: &LaunchArgs) -> Window {
    #[cfg_attr(
        target_arch = "wasm32",
        expect(unused_mut, reason = "native-only settings")
    )]
    let mut window = Window {
        title: "Time Fly".to_string(),
        // Wayland app ID / X11 WM_CLASS: matches the Linux desktop file and Flatpak app ID, so
        // the shell shows the packaged icon for the window.
        name: Some(APP_ID.to_string()),
        ..default()
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some((w, h)) = args.window {
            window.resolution = WindowResolution::new(w, h);
        }
        if args.scrub_bench || args.region_bench {
            window.present_mode = PresentMode::AutoNoVsync;
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = args;
    window
}

/// Builds the Time Fly app. Plugins are added here as milestones land.
pub fn app(session: &Session) -> App {
    let args = session.args().clone();
    let mut app = App::new();
    // Before `DefaultPlugins`: `LogPlugin` takes its layers from the diagnostics resources.
    app.add_plugins(DiagnosticsPlugin::new(session));
    // A plain launch reopens the window where the player left it (`window_memory`).
    #[cfg(not(target_arch = "wasm32"))]
    let remembered = window_memory::remembers_window(&args)
        .then(window_memory::window_file)
        .flatten();
    #[cfg_attr(
        target_arch = "wasm32",
        expect(unused_mut, reason = "native-only window memory")
    )]
    let mut window = primary_window(&args);
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(geometry) = remembered.as_deref().and_then(window_memory::load_geometry) {
        if let Some(position) = geometry.position {
            window.position = bevy::window::WindowPosition::At(position.into());
        }
        app.insert_resource(window_memory::PendingWindowRestore(geometry));
    }
    let plugins = DefaultPlugins
        .set(diagnostics::log_plugin())
        .set(WindowPlugin {
            primary_window: Some(window),
            ..default()
        })
        .set(AssetPlugin {
            // No asset ships a `.meta` file; without this the web build requests one per asset
            // and gets a 404 for each.
            meta_check: AssetMetaCheck::Never,
            ..default()
        });
    // Bevy's web panic handler replaces the panic hook; ours chains console_error_panic_hook.
    #[cfg(target_arch = "wasm32")]
    let plugins = plugins.disable::<bevy::app::PanicHandlerPlugin>();
    app.add_plugins(plugins).add_plugins((
        clusters::NoClusteringPlugin,
        time::TimePlugin,
        GlobeDataPlugin::default(),
        GlobePlugin,
        RegionPlugin,
        TerrainCdlodPlugin,
        sky::SkyPlugin,
        water::WaterPlugin,
        terrain::warmup::RegionWarmupPlugin,
        OrbitCameraPlugin,
        RegionNavigationPlugin,
        FlyCameraPlugin,
        GameUiPlugins,
    ));
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = remembered {
        app.add_plugins(window_memory::WindowMemoryPlugin { path });
    }
    let default_fps = if cfg!(target_arch = "wasm32") {
        bench::WEB_TARGET_FPS
    } else {
        bench::NATIVE_TARGET_FPS
    };
    let target_fps = args.target_fps.unwrap_or(default_fps);
    let exit_when_done = !cfg!(target_arch = "wasm32");
    if args.region_bench {
        // The region bench enters its own regions; it takes precedence over the scrub bench
        // and the launch region.
        app.add_plugins(RegionBenchPlugin(RegionBenchConfig::new(
            target_fps,
            exit_when_done,
        )));
    } else {
        if let Some(region) = args.region {
            app.insert_resource(LaunchRegion(region));
        }
        if args.scrub_bench {
            app.add_plugins(ScrubBenchPlugin {
                target_fps,
                exit_when_done,
            });
        }
    }
    app
}
