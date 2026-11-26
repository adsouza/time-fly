//! The credits overlay: `Esc` toggles a scrollable panel with every dataset's citation and
//! licence (`assets/credits.txt`).

use bevy::prelude::*;
use bevy::ui_widgets::ScrollArea;

use super::{BlocksCamera, GUTTER_PX, TEXT_COLOR};

/// The credits text, compiled in so it can never go missing.
pub const CREDITS_TEXT: &str = include_str!("../../assets/credits.txt");

/// The key that opens and closes the credits.
pub const CREDITS_KEY: KeyCode = KeyCode::Escape;

const OVERLAY_COLOR: Color = Color::srgba(0.02, 0.025, 0.04, 0.92);

/// Whether the credits overlay is open.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditsOpen(pub bool);

/// The overlay's root node.
#[derive(Component)]
pub struct CreditsOverlay;

/// The overlay's display for an open state.
pub fn credits_display(open: bool) -> Display {
    if open { Display::Flex } else { Display::None }
}

pub struct CreditsPlugin;

impl Plugin for CreditsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CreditsOpen>()
            .add_systems(Startup, spawn_credits)
            .add_systems(Update, (toggle_credits, show_credits).chain());
    }
}

fn spawn_credits(mut commands: Commands) {
    commands
        .spawn((
            Name::new("Credits"),
            CreditsOverlay,
            BlocksCamera,
            GlobalZIndex(100),
            Node {
                display: credits_display(false),
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: px(0),
                bottom: px(0),
                padding: UiRect::all(px(GUTTER_PX * 2.0)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: px(10),
                ..default()
            },
            BackgroundColor(OVERLAY_COLOR),
        ))
        .with_children(|root| {
            root.spawn((
                Text::new("Credits  (Esc to close)"),
                TextFont::from_font_size(22.0),
                TextColor(TEXT_COLOR),
            ));
            root.spawn((
                ScrollArea,
                Node {
                    max_width: px(860),
                    width: percent(100),
                    flex_grow: 1.0,
                    overflow: Overflow::scroll_y(),
                    flex_direction: FlexDirection::Column,
                    ..default()
                },
            ))
            .with_children(|scroll| {
                scroll.spawn((
                    Text::new(CREDITS_TEXT),
                    TextFont::from_font_size(15.0),
                    TextColor(TEXT_COLOR),
                ));
            });
        });
}

fn toggle_credits(keys: Option<Res<ButtonInput<KeyCode>>>, mut open: ResMut<CreditsOpen>) {
    if keys.is_some_and(|k| k.just_pressed(CREDITS_KEY)) {
        open.0 = !open.0;
        info!(open = open.0, "credits");
    }
}

fn show_credits(open: Res<CreditsOpen>, mut overlays: Query<&mut Node, With<CreditsOverlay>>) {
    if !open.is_changed() {
        return;
    }
    let display = credits_display(open.0);
    for mut node in &mut overlays {
        if node.display != display {
            node.display = display;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(CreditsPlugin);
        app.update();
        app
    }

    fn tap(app: &mut App, key: KeyCode) {
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
        let mut input = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        input.release(key);
        input.clear();
        app.update();
    }

    fn overlay_display(app: &mut App) -> Display {
        app.world_mut()
            .query_filtered::<&Node, With<CreditsOverlay>>()
            .single(app.world())
            .unwrap()
            .display
    }

    #[test]
    fn esc_toggles_credits() {
        let mut app = app();
        assert_eq!(overlay_display(&mut app), Display::None);
        tap(&mut app, KeyCode::Escape);
        assert_eq!(overlay_display(&mut app), Display::Flex);
        assert!(app.world().resource::<CreditsOpen>().0);
        tap(&mut app, KeyCode::Escape);
        assert_eq!(overlay_display(&mut app), Display::None);
        tap(&mut app, KeyCode::KeyA);
        assert_eq!(overlay_display(&mut app), Display::None, "only Esc toggles");
    }

    #[test]
    fn credits_cite_every_dataset() {
        for needle in [
            "Scotese",
            "Wright",
            "10.5281/zenodo.5460860",
            "CC BY 4.0",
            "scotese_and_wright2018",
            "ICE-6G_C",
            "Argus",
            "10.1093/gji/ggu140",
            "Peltier",
            "10.1002/2014JB011176",
            "Spratt",
            "Lisiecki",
            "10.5194/cp-12-1079-2016",
            "10.25921/rd66-5820",
            "NOAA",
            "Beyer",
            "Krapp",
            "10.1038/s41597-020-0552-1",
            "10.6084/m9.figshare.12293345",
            "Natural Earth",
            "naturalearthdata.com",
            "rivers + lake centerlines",
            "Bevy",
        ] {
            assert!(CREDITS_TEXT.contains(needle), "credits.txt lacks {needle}");
        }
        // Bevy's built-in font has ASCII glyphs only.
        assert!(CREDITS_TEXT.is_ascii(), "credits.txt must be plain ASCII");
    }
}
