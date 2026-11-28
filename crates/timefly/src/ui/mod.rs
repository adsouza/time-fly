//! The game's UI: the time slider, the loading/failure status, the credits overlay, the
//! altitude and frame-rate readouts, the position readout and compass rose, and bug shots.
//!
//! Each plugin keeps its decisions in pure functions (unit-tested directly); the systems only
//! gather input, call them and apply the result.

pub mod altitude;
pub mod bug_shot;
pub mod credits;
pub mod fps;
mod polygons;
pub mod position;
pub mod slider;
pub mod status;

use bevy::picking::hover::Hovered;
use bevy::prelude::*;

pub use credits::CreditsPlugin;
pub use fps::FpsOverlayPlugin;
pub use slider::SliderUiPlugin;
pub use status::StatusUiPlugin;

/// Space between the UI and the window edges.
pub const GUTTER_PX: f32 = 16.0;
/// The bug-shot button's size (px), in the window's top-right corner (Task 11c fix round 2).
pub const CORNER_BUTTON_PX: Vec2 = Vec2::new(26.0, 20.0);
/// The gap (px) between the button and the frame-rate readout just left of it, on its row.
pub const CORNER_GAP_PX: f32 = 6.0;
/// UI text.
pub const TEXT_COLOR: Color = Color::srgb(0.93, 0.93, 0.95);
/// Panels behind text.
pub const PANEL_COLOR: Color = Color::srgba(0.07, 0.08, 0.11, 0.82);

/// Whether the pointer is over the UI (or a UI overlay is open), so a drag that starts there
/// steers neither the globe's fly nor the region's.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PointerOverUi(pub bool);

/// Marks a UI root that keeps the pointer from the camera while hovered.
#[derive(Component, Default)]
#[require(Hovered)]
pub struct BlocksCamera;

/// Plugins: slider, status, credits, the readouts and bug shots, plus the pointer-over-UI
/// tracking they share. (The `--from-shot` repro, [`bug_shot::ShotRestorePlugin`], is added by
/// `crate::app`.)
pub struct GameUiPlugins;

impl Plugin for GameUiPlugins {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            SliderUiPlugin,
            StatusUiPlugin,
            CreditsPlugin,
            FpsOverlayPlugin,
            altitude::AltitudeOverlayPlugin,
            position::PositionOverlayPlugin,
            bug_shot::BugShotPlugin,
        ))
        .init_resource::<PointerOverUi>()
        .add_systems(PreUpdate, track_pointer_over_ui);
    }
}

fn track_pointer_over_ui(
    roots: Query<&Hovered, With<BlocksCamera>>,
    credits: Option<Res<credits::CreditsOpen>>,
    mut over: ResMut<PointerOverUi>,
) {
    let next = roots.iter().any(Hovered::get) || credits.is_some_and(|c| c.0);
    if over.0 != next {
        over.0 = next;
    }
}
