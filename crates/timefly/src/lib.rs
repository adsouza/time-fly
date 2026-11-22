use bevy::prelude::*;

pub mod args;
pub mod diagnostics;

use diagnostics::{DiagnosticsPlugin, Session};

/// Builds the Time Fly app. Plugins are added here as milestones land.
pub fn app(session: &Session) -> App {
    let mut app = App::new();
    // Before `DefaultPlugins`: `LogPlugin` takes its layers from the diagnostics resources.
    app.add_plugins(DiagnosticsPlugin::new(session));
    let plugins = DefaultPlugins.set(diagnostics::log_plugin());
    // Bevy's web panic handler replaces the panic hook; ours chains console_error_panic_hook.
    #[cfg(target_arch = "wasm32")]
    let plugins = plugins.disable::<bevy::app::PanicHandlerPlugin>();
    app.add_plugins(plugins);
    app
}
