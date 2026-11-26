//! A small translucent frame-rate readout in the top-right corner (`60 fps`).
//!
//! The rate is frames over real seconds, refreshed every [`FPS_REFRESH_S`]: steady enough to
//! read, and the text is re-laid out twice a second rather than every frame.

use bevy::prelude::*;

use super::{GUTTER_PX, TEXT_COLOR};

/// Seconds of real time each shown rate averages over (and how often it changes).
pub const FPS_REFRESH_S: f32 = 0.5;
/// The readout's panel: lighter than the UI panels, so it stays out of the way.
pub const FPS_PANEL_COLOR: Color = Color::srgba(0.0, 0.0, 0.0, 0.45);

/// Frames per second for `frames` frames over `secs` real seconds (0 before any time passes).
pub fn fps_over(frames: u32, secs: f32) -> f32 {
    if secs > 0.0 {
        frames as f32 / secs
    } else {
        0.0
    }
}

/// The readout's text, in whole frames: `60 fps`.
pub fn fps_label(fps: f32) -> String {
    format!("{:.0} fps", fps.max(0.0))
}

/// The readout's text.
#[derive(Component)]
pub struct FpsText;

/// Frames and real seconds counted since the last refresh.
#[derive(Default)]
struct FpsWindow {
    frames: u32,
    secs: f32,
}

/// The top-right frame-rate readout.
pub struct FpsOverlayPlugin;

impl Plugin for FpsOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_fps_overlay)
            .add_systems(Update, update_fps);
    }
}

fn spawn_fps_overlay(mut commands: Commands) {
    commands.spawn((
        Name::new("FPS readout"),
        FpsText,
        Text::new(""),
        TextFont::from_font_size(14.0),
        TextColor(TEXT_COLOR),
        Node {
            position_type: PositionType::Absolute,
            top: px(GUTTER_PX),
            right: px(GUTTER_PX),
            padding: UiRect::axes(px(8), px(3)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(FPS_PANEL_COLOR),
        // Above the captions, like the bench banner; never takes the pointer.
        GlobalZIndex(10),
        Pickable::IGNORE,
    ));
}

fn update_fps(
    time: Res<Time<Real>>,
    mut window: Local<FpsWindow>,
    mut text: Query<&mut Text, With<FpsText>>,
) {
    // A rate is intervals over time: the first update (no time has passed yet) is no interval.
    let dt = time.delta_secs();
    if dt <= 0.0 {
        return;
    }
    window.frames += 1;
    window.secs += dt;
    if window.secs < FPS_REFRESH_S {
        return;
    }
    let label = fps_label(fps_over(window.frames, window.secs));
    *window = FpsWindow::default();
    for mut t in &mut text {
        if t.0 != label {
            t.0.clone_from(&label);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;

    use super::*;

    #[test]
    fn the_rate_is_frames_over_real_seconds() {
        assert_eq!(fps_over(30, 0.5), 60.0);
        assert_eq!(fps_over(0, 0.5), 0.0);
        // No time measured yet: no rate, not infinity.
        assert_eq!(fps_over(5, 0.0), 0.0);
    }

    #[test]
    fn the_label_rounds_to_whole_frames() {
        assert_eq!(fps_label(59.6), "60 fps");
        assert_eq!(fps_label(144.2), "144 fps");
        assert_eq!(fps_label(0.0), "0 fps");
    }

    #[test]
    fn the_overlay_shows_the_measured_rate_in_the_top_right() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, FpsOverlayPlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_micros(
                16_667,
            )));
        app.update();
        let (node, text) = {
            let world = app.world_mut();
            let mut q = world.query_filtered::<(&Node, &Text), With<FpsText>>();
            let (node, text) = q.single(world).unwrap();
            (node.clone(), text.0.clone())
        };
        assert_eq!(node.top, px(crate::ui::GUTTER_PX));
        assert_eq!(node.right, px(crate::ui::GUTTER_PX));
        assert_eq!(text, "", "nothing measured before the first refresh");
        // Past one refresh interval at 60 updates a second.
        for _ in 0..40 {
            app.update();
        }
        let world = app.world_mut();
        let mut q = world.query_filtered::<&Text, With<FpsText>>();
        assert_eq!(q.single(world).unwrap().0, "60 fps");
    }
}
