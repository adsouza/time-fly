use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
#[cfg(not(target_arch = "wasm32"))]
use bevy::window::{PresentMode, WindowResolution};

pub mod args;
pub mod art;
pub mod bench;
pub mod build_features;
#[cfg(not(target_arch = "wasm32"))]
pub mod check_data;
pub mod clock;
pub mod cloud;
pub mod clusters;
pub mod creature;
pub mod data;
pub mod diagnostics;
pub mod fly_camera;
pub mod globe;
pub mod globe_flight;
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
use bench::journey::{JourneyBenchConfig, JourneyBenchPlugin};
use bench::region::{RegionBenchConfig, RegionBenchPlugin};
use creature::CreaturePlugin;
use data::GlobeDataPlugin;
use diagnostics::{DiagnosticsPlugin, Session};
use fly_camera::FlyCameraPlugin;
use globe::GlobePlugin;
use globe_flight::GlobeFlightPlugin;
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
        // Web: the canvas follows its parent's size, so the game fills the page on any host
        // page (the dev server's bare one included) instead of sitting at the engine's default
        // size in the top-left corner. No effect natively.
        fit_canvas_to_parent: true,
        ..default()
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some((w, h)) = args.window {
            window.resolution = WindowResolution::new(w, h);
        }
        if args.scrub_bench || args.region_bench || args.journey_bench {
            window.present_mode = PresentMode::AutoNoVsync;
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = args;
    window
}

/// The benchmark a launch runs.
#[derive(Clone, Debug, PartialEq)]
pub enum ChosenBench {
    Scrub,
    Region(RegionBenchConfig),
    Journey(JourneyBenchConfig),
}

/// The benchmark the launch arguments ask for: the region bench over the journey bench over the
/// scrub bench. With `--region`, the region and journey benches fly that region (the region
/// bench only that one, with no pan or memory cycles); the scrub bench leaves it to the launch.
pub fn chosen_bench(
    args: &LaunchArgs,
    target_fps: u32,
    exit_when_done: bool,
) -> Option<ChosenBench> {
    if args.region_bench {
        Some(ChosenBench::Region(match args.region {
            Some(region) => RegionBenchConfig::single(target_fps, exit_when_done, region),
            None => RegionBenchConfig::new(target_fps, exit_when_done),
        }))
    } else if args.journey_bench {
        Some(ChosenBench::Journey(match args.region {
            Some(region) => JourneyBenchConfig::chosen(target_fps, exit_when_done, region),
            None => JourneyBenchConfig::new(target_fps, exit_when_done),
        }))
    } else if args.scrub_bench {
        Some(ChosenBench::Scrub)
    } else {
        None
    }
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
        GlobeFlightPlugin,
        RegionNavigationPlugin,
        FlyCameraPlugin,
        CreaturePlugin,
        cloud::CloudPlugin,
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
    // `--no-dunes` reaches every region's look, in play and in the benches.
    app.insert_resource(terrain::TerrainOptions {
        dunes: !args.no_dunes,
    });
    // The benches enter their own regions; a `--region` launch region is play or the scrub
    // bench's start.
    match chosen_bench(&args, target_fps, exit_when_done) {
        Some(ChosenBench::Region(config)) => {
            app.add_plugins(RegionBenchPlugin(config));
        }
        Some(ChosenBench::Journey(config)) => {
            app.add_plugins(JourneyBenchPlugin(config));
        }
        chosen => {
            if let Some(region) = args.region {
                app.insert_resource(LaunchRegion(region));
            }
            if chosen == Some(ChosenBench::Scrub) {
                app.add_plugins(ScrubBenchPlugin {
                    target_fps,
                    exit_when_done,
                });
            }
        }
    }
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_benches_take_precedence_in_order() {
        use bench::region::CHOSEN_REGION;
        let arg = args::parse_region("20,37.5,0").unwrap();
        let chosen = |args: LaunchArgs| chosen_bench(&args, 60, true);
        // The region bench over the journey bench over the scrub bench.
        let all = LaunchArgs {
            scrub_bench: true,
            region_bench: true,
            journey_bench: true,
            ..default()
        };
        assert_eq!(
            chosen(all.clone()),
            Some(ChosenBench::Region(RegionBenchConfig::new(60, true)))
        );
        let journey = LaunchArgs {
            region_bench: false,
            ..all.clone()
        };
        assert_eq!(
            chosen(journey.clone()),
            Some(ChosenBench::Journey(JourneyBenchConfig::new(60, true)))
        );
        assert_eq!(
            chosen(LaunchArgs {
                journey_bench: false,
                ..journey.clone()
            }),
            Some(ChosenBench::Scrub)
        );
        assert_eq!(chosen(LaunchArgs::default()), None);
        // `--region` names the region either bench flies.
        let Some(ChosenBench::Journey(c)) = chosen(LaunchArgs {
            region: Some(arg),
            ..journey
        }) else {
            panic!("not the journey bench");
        };
        assert_eq!((c.region.name, c.region.arg), (CHOSEN_REGION, arg));
        assert_eq!(c.cycles, JourneyBenchConfig::new(60, true).cycles);
        assert_eq!(
            chosen(LaunchArgs {
                region: Some(arg),
                ..all
            }),
            Some(ChosenBench::Region(RegionBenchConfig::single(
                60, true, arg
            )))
        );
        // A bare `--region` is play, not a bench.
        assert_eq!(
            chosen(LaunchArgs {
                region: Some(arg),
                ..default()
            }),
            None
        );
    }

    #[test]
    fn the_canvas_fills_its_page() {
        // On the web the canvas follows its parent's size, so any page (the dev server's bare
        // one or `packaging/web/index.html`) shows the game across the whole window, not at the
        // engine's default size in the top-left corner. Natively the setting does nothing.
        assert!(primary_window(&LaunchArgs::default()).fit_canvas_to_parent);
    }
}
