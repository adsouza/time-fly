//! The loading and failure overlay: "Loading Earth's history..." until the first interactive
//! globe frame, or what went wrong if the data fails to load or the renderer cannot start.
//!
//! Bevy's built-in font has ASCII glyphs only, so UI text uses three dots, not an ellipsis.

use bevy::prelude::*;

use super::{PANEL_COLOR, TEXT_COLOR};
use crate::globe::GlobeRenderStatus;

pub const LOADING_TEXT: &str = "Loading Earth's history...";

const BACKDROP: Color = Color::srgb(0.035, 0.04, 0.07);
const FAILED_COLOR: Color = Color::srgb(1.0, 0.72, 0.62);

/// What the overlay says for a render status; `None` hides it.
pub fn status_message(status: &GlobeRenderStatus) -> Option<String> {
    match status {
        GlobeRenderStatus::LoadingData | GlobeRenderStatus::PreparingGpu => {
            Some(LOADING_TEXT.to_string())
        }
        GlobeRenderStatus::Interactive { .. } => None,
        GlobeRenderStatus::Failed(error) => Some(format!(
            "Time Fly could not show the globe.\n\n{error}\n\nThe session log has the details."
        )),
    }
}

/// The overlay's root node.
#[derive(Component)]
pub struct StatusOverlay;

/// The overlay's text.
#[derive(Component)]
pub struct StatusText;

pub struct StatusUiPlugin;

impl Plugin for StatusUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GlobeRenderStatus>()
            .add_systems(Startup, spawn_status)
            .add_systems(Update, show_status);
    }
}

fn spawn_status(mut commands: Commands) {
    commands
        .spawn((
            Name::new("Status"),
            StatusOverlay,
            GlobalZIndex(200),
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: px(0),
                bottom: px(0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(BACKDROP),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    max_width: px(640),
                    margin: UiRect::all(px(16)),
                    padding: UiRect::axes(px(20), px(14)),
                    border_radius: BorderRadius::all(px(8)),
                    ..default()
                },
                BackgroundColor(PANEL_COLOR),
            ))
            .with_children(|panel| {
                panel.spawn((
                    StatusText,
                    Text::new(LOADING_TEXT),
                    TextFont::from_font_size(22.0),
                    TextColor(TEXT_COLOR),
                ));
            });
        });
}

fn show_status(
    status: Res<GlobeRenderStatus>,
    mut overlays: Query<&mut Node, With<StatusOverlay>>,
    mut texts: Query<(&mut Text, &mut TextColor), With<StatusText>>,
) {
    if !status.is_changed() {
        return;
    }
    let message = status_message(&status);
    let display = if message.is_some() {
        Display::Flex
    } else {
        Display::None
    };
    for mut node in &mut overlays {
        node.display = display;
    }
    if let Some(message) = message {
        let color = if matches!(*status, GlobeRenderStatus::Failed(_)) {
            FAILED_COLOR
        } else {
            TEXT_COLOR
        };
        for (mut text, mut text_color) in &mut texts {
            text.0.clone_from(&message);
            text_color.0 = color;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_follow_the_render_status() {
        assert_eq!(
            status_message(&GlobeRenderStatus::LoadingData).as_deref(),
            Some(LOADING_TEXT)
        );
        assert_eq!(
            status_message(&GlobeRenderStatus::PreparingGpu).as_deref(),
            Some(LOADING_TEXT)
        );
        assert_eq!(
            status_message(&GlobeRenderStatus::Interactive { first_frame: 9 }),
            None
        );
        let failed = status_message(&GlobeRenderStatus::Failed("no adapter".into())).unwrap();
        assert!(failed.contains("could not show the globe") && failed.contains("no adapter"));
    }

    #[test]
    fn overlay_stays_until_the_first_interactive_frame() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins).add_plugins(StatusUiPlugin);
        let display = |app: &mut App| {
            app.world_mut()
                .query_filtered::<&Node, With<StatusOverlay>>()
                .single(app.world())
                .unwrap()
                .display
        };
        app.update();
        assert_eq!(display(&mut app), Display::Flex);
        *app.world_mut().resource_mut::<GlobeRenderStatus>() = GlobeRenderStatus::PreparingGpu;
        app.update();
        assert_eq!(display(&mut app), Display::Flex);
        *app.world_mut().resource_mut::<GlobeRenderStatus>() =
            GlobeRenderStatus::Interactive { first_frame: 3 };
        app.update();
        assert_eq!(display(&mut app), Display::None);
    }
}
