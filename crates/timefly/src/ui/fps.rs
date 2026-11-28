//! Small translucent frame-rate and polygon-count readouts in the top-right corner.
//!
//! The rate is frames over real seconds, refreshed every [`FPS_REFRESH_S`]: steady enough to
//! read, and the text is re-laid out twice a second rather than every frame.

use bevy::prelude::*;

use super::polygons::{PolygonCount, add_polygon_count};
use super::{CORNER_BUTTON_PX, CORNER_GAP_PX, GUTTER_PX, TEXT_COLOR};

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

#[derive(Component)]
struct PolygonText;

/// Frames and real seconds counted since the last refresh.
#[derive(Default)]
struct FpsWindow {
    frames: u32,
    secs: f32,
}

/// The frame-rate readout, in the top-right corner just left of the bug-shot button.
pub struct FpsOverlayPlugin;

impl Plugin for FpsOverlayPlugin {
    fn build(&self, app: &mut App) {
        add_polygon_count(app);
        app.add_systems(Startup, spawn_fps_overlay)
            .add_systems(Update, (update_fps, update_polygons));
    }
}

fn spawn_fps_overlay(mut commands: Commands) {
    commands.spawn((Name::new("FPS readout"), FpsText, corner_readout(GUTTER_PX)));
    commands.spawn((
        Name::new("Polygon count readout"),
        PolygonText,
        corner_readout(GUTTER_PX + CORNER_BUTTON_PX.y + 4.0),
    ));
}

fn corner_readout(top: f32) -> impl Bundle {
    (
        Text::new(""),
        TextFont::from_font_size(14.0),
        TextColor(TEXT_COLOR),
        // Just left of the bug-shot button in the corner, on its row: the button's height,
        // the 14 px text (about 17 px a line) centred in it.
        Node {
            position_type: PositionType::Absolute,
            top: px(top),
            right: px(GUTTER_PX + CORNER_BUTTON_PX.x + CORNER_GAP_PX),
            height: px(CORNER_BUTTON_PX.y),
            padding: UiRect::axes(px(8), px(1.5)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(FPS_PANEL_COLOR),
        // Above the captions, like the bench banner; never takes the pointer.
        GlobalZIndex(10),
        Pickable::IGNORE,
    )
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

fn update_polygons(
    count: Res<PolygonCount>,
    mut shown: Local<Option<u64>>,
    mut text: Query<&mut Text, With<PolygonText>>,
) {
    let Some(count) = count.sample() else {
        return;
    };
    if *shown == Some(count) {
        return;
    }
    *shown = Some(count);
    let mut label = count.to_string();
    let digits = label.len();
    for i in (1..digits).rev().filter(|i| (digits - i).is_multiple_of(3)) {
        label.insert(i, ',');
    }
    label.push_str(" polys");
    for mut text in &mut text {
        text.0.clone_from(&label);
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
        // Just left of the bug-shot button in the top-right corner, on its row, centred on it
        // (fix round 2, the owner's request): the button's height, the button's width and a
        // gap in from the gutter.
        use crate::ui::{CORNER_BUTTON_PX, CORNER_GAP_PX, GUTTER_PX};
        assert_eq!(node.position_type, PositionType::Absolute);
        assert_eq!(node.top, px(GUTTER_PX));
        assert_eq!(
            node.right,
            px(GUTTER_PX + CORNER_BUTTON_PX.x + CORNER_GAP_PX)
        );
        assert_eq!(node.height, px(CORNER_BUTTON_PX.y));
        assert_eq!(
            node.padding.top, node.padding.bottom,
            "the text centred on the row"
        );
        assert_eq!(text, "", "nothing measured before the first refresh");
        // Past one refresh interval at 60 updates a second.
        for _ in 0..40 {
            app.update();
        }
        let world = app.world_mut();
        let mut q = world.query_filtered::<&Text, With<FpsText>>();
        assert_eq!(q.single(world).unwrap().0, "60 fps");
    }

    #[test]
    fn the_polygon_count_appears_below_fps() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, FpsOverlayPlugin));
        app.update();
        let world = app.world_mut();
        let mut polygons = world.query_filtered::<(&Node, &Text), With<PolygonText>>();
        let (node, text) = polygons.single(world).unwrap();
        let (top, right) = (node.top, node.right);
        assert!(text.0.is_empty());
        let mut fps = world.query_filtered::<&Node, With<FpsText>>();
        assert_eq!(right, fps.single(world).unwrap().right);
        assert_eq!(top, px(GUTTER_PX + CORNER_BUTTON_PX.y + 4.0));

        world.resource::<PolygonCount>().publish(1_234_567);
        app.update();
        assert_eq!(polygons.single(app.world()).unwrap().1.0, "1,234,567 polys");
    }
}
