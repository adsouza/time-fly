use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::window::{PresentMode, WindowResolution};

pub mod args;
pub mod bench;
pub mod camera;
#[cfg(not(target_arch = "wasm32"))]
pub mod check_data;
pub mod data;
pub mod diagnostics;
pub mod globe;
pub mod gpu;
pub mod time;
pub mod ui;

use args::LaunchArgs;
use bench::ScrubBenchPlugin;
use camera::OrbitCameraPlugin;
use data::GlobeDataPlugin;
use diagnostics::{DiagnosticsPlugin, Session};
use globe::GlobePlugin;
use ui::GameUiPlugins;

/// Reverse-DNS application ID, shared with `packaging/linux` and `packaging/flatpak`.
pub const APP_ID: &str = "io.github.adsouza.TimeFly";

/// The primary window for the launch arguments: `--window WxH` sets its size in physical
/// pixels, and the scrub bench turns vsync off on native so frame times show the real cost.
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
        if args.scrub_bench {
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
    let plugins = DefaultPlugins
        .set(diagnostics::log_plugin())
        .set(WindowPlugin {
            primary_window: Some(primary_window(&args)),
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
        time::TimePlugin,
        GlobeDataPlugin::default(),
        GlobePlugin,
        OrbitCameraPlugin,
        GameUiPlugins,
    ));
    if args.scrub_bench {
        let default_fps = if cfg!(target_arch = "wasm32") {
            bench::WEB_TARGET_FPS
        } else {
            bench::NATIVE_TARGET_FPS
        };
        app.add_plugins(ScrubBenchPlugin {
            target_fps: args.target_fps.unwrap_or(default_fps),
            exit_when_done: !cfg!(target_arch = "wasm32"),
        });
    }
    app
}
