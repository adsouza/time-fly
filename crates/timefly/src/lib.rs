use bevy::prelude::*;

/// Builds the Time Fly app. Plugins are added here as milestones land.
pub fn app() -> App {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins);
    app
}
