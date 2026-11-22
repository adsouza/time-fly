use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;

pub mod args;
pub mod data;
pub mod diagnostics;
pub mod globe;
pub mod time;

use data::GlobeDataPlugin;
use diagnostics::{DiagnosticsPlugin, Session};
use globe::GlobePlugin;

/// Builds the Time Fly app. Plugins are added here as milestones land.
pub fn app(session: &Session) -> App {
    let mut app = App::new();
    // Before `DefaultPlugins`: `LogPlugin` takes its layers from the diagnostics resources.
    app.add_plugins(DiagnosticsPlugin::new(session));
    let plugins = DefaultPlugins
        .set(diagnostics::log_plugin())
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
    ));
    app
}
