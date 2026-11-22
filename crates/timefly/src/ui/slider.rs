//! The time slider along the bottom of the window, with the age label above it.
//!
//! Time runs left to right: the left end is 540 Ma and the right end is the present. The core's
//! slider position `s` (0 = present, log-scaled) is mirrored at this UI edge:
//! `widget value = 1 - s`. With that mapping the last ~12,000 years fill roughly the right third.
//!
//! The slider UI fades to [`IDLE_OPACITY`] after [`IDLE_FADE_END_SECS`] without input and comes
//! back at once on hover, drag, scroll or a key press.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::picking::hover::Hovered;
use bevy::prelude::*;
use bevy::ui::UiSystems;
use bevy::ui_widgets::{
    Slider, SliderDragState, SliderRange, SliderStep, SliderThumb, SliderValue, TrackClick,
    ValueChange,
};
use timefly_core::deep_time::{YearsBp, format_years_bp, slider_to_years, years_to_slider};

use super::{BlocksCamera, GUTTER_PX, PANEL_COLOR, TEXT_COLOR};
use crate::time::{GlobeTime, TimeChangeCause, TimeChanged};

/// Opacity of the idle UI.
pub const IDLE_OPACITY: f32 = 0.3;
/// Idle seconds when the fade starts.
pub const IDLE_FADE_START_SECS: f32 = 2.5;
/// Idle seconds when the UI reaches [`IDLE_OPACITY`].
pub const IDLE_FADE_END_SECS: f32 = 3.0;

const TRACK_HEIGHT_PX: f32 = 28.0;
const THUMB_WIDTH_PX: f32 = 14.0;
const TRACK_COLOR: Color = Color::srgba(0.75, 0.8, 0.9, 0.35);
const THUMB_COLOR: Color = Color::srgb(0.97, 0.85, 0.55);

/// The slider's widget value (0 = 540 Ma at the left, 1 = present at the right) for an age.
pub fn widget_value_for(years: YearsBp) -> f64 {
    1.0 - years_to_slider(years)
}

/// The age at a widget value; the inverse of [`widget_value_for`].
pub fn years_for(widget_value: f64) -> YearsBp {
    slider_to_years(1.0 - widget_value)
}

/// UI opacity after `idle_secs` without input: 1 until [`IDLE_FADE_START_SECS`], then a linear
/// fade to [`IDLE_OPACITY`] at [`IDLE_FADE_END_SECS`].
pub fn ui_opacity(idle_secs: f32) -> f32 {
    let t = ((idle_secs - IDLE_FADE_START_SECS) / (IDLE_FADE_END_SECS - IDLE_FADE_START_SECS))
        .clamp(0.0, 1.0);
    1.0 + (IDLE_OPACITY - 1.0) * t
}

/// Seconds since the last UI-relevant input.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
pub struct UiIdle {
    pub idle_secs: f32,
}

/// The slider UI's current opacity (from [`ui_opacity`]).
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct UiOpacity(pub f32);

impl Default for UiOpacity {
    fn default() -> Self {
        UiOpacity(1.0)
    }
}

/// The slider widget.
#[derive(Component)]
pub struct TimeSlider;

/// The age label above the slider.
#[derive(Component)]
pub struct AgeLabel;

/// The node that moves along the track.
#[derive(Component)]
struct ThumbNode;

/// A UI colour that follows [`UiOpacity`]; `base` is the colour at full opacity, applied to the
/// text colour or the background (text nodes also carry a transparent background).
#[derive(Component, Copy, Clone)]
struct Faded {
    base: Color,
    text: bool,
}

pub struct SliderUiPlugin;

impl Plugin for SliderUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiIdle>()
            .init_resource::<UiOpacity>()
            .add_systems(Startup, spawn_slider)
            .add_observer(on_slider_change)
            .add_systems(Update, track_idle)
            .add_systems(
                PostUpdate,
                (sync_slider_to_time, position_thumb, apply_opacity)
                    .chain()
                    .before(UiSystems::Prepare),
            );
    }
}

fn spawn_slider(mut commands: Commands, time: Option<Res<GlobeTime>>) {
    let years = time.map_or(YearsBp(0.0), |t| t.years);
    let value = widget_value_for(years) as f32;
    let faded = |base: Color| Faded { base, text: false };
    let faded_text = |base: Color| Faded { base, text: true };
    let caption = |text: &str| {
        (
            Text::new(text),
            TextFont::from_font_size(13.0),
            TextColor(TEXT_COLOR),
            faded_text(TEXT_COLOR),
        )
    };
    commands
        .spawn((
            Name::new("Time slider UI"),
            BlocksCamera,
            Node {
                position_type: PositionType::Absolute,
                left: px(GUTTER_PX),
                right: px(GUTTER_PX),
                bottom: px(GUTTER_PX),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Stretch,
                row_gap: px(6),
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn(Node {
                justify_content: JustifyContent::Center,
                ..default()
            })
            .with_children(|row| {
                row.spawn((
                    Node {
                        padding: UiRect::axes(px(12), px(4)),
                        border_radius: BorderRadius::all(px(6)),
                        ..default()
                    },
                    BackgroundColor(PANEL_COLOR),
                    faded(PANEL_COLOR),
                ))
                .with_children(|panel| {
                    panel.spawn((
                        AgeLabel,
                        Text::new(format_years_bp(years)),
                        TextFont::from_font_size(20.0),
                        TextColor(TEXT_COLOR),
                        faded_text(TEXT_COLOR),
                    ));
                });
            });
            root.spawn((
                Name::new("Time slider"),
                TimeSlider,
                Slider {
                    track_click: TrackClick::Snap,
                    ..default()
                },
                SliderValue(value),
                SliderRange::new(0.0, 1.0),
                SliderStep(0.01),
                Hovered::default(),
                Node {
                    height: px(TRACK_HEIGHT_PX),
                    border_radius: BorderRadius::all(px(TRACK_HEIGHT_PX / 2.0)),
                    ..default()
                },
                BackgroundColor(TRACK_COLOR),
                faded(TRACK_COLOR),
            ))
            .with_children(|track| {
                // The thumb travels the track width minus its own, as the widget assumes.
                track
                    .spawn(Node {
                        position_type: PositionType::Absolute,
                        left: px(0),
                        right: px(THUMB_WIDTH_PX),
                        top: px(0),
                        bottom: px(0),
                        ..default()
                    })
                    .with_children(|travel| {
                        travel.spawn((
                            ThumbNode,
                            SliderThumb,
                            Node {
                                position_type: PositionType::Absolute,
                                left: percent(value * 100.0),
                                width: px(THUMB_WIDTH_PX),
                                top: px(0),
                                bottom: px(0),
                                border_radius: BorderRadius::all(px(THUMB_WIDTH_PX / 2.0)),
                                ..default()
                            },
                            BackgroundColor(THUMB_COLOR),
                            faded(THUMB_COLOR),
                        ));
                    });
            });
            root.spawn(Node {
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::horizontal(px(4)),
                ..default()
            })
            .with_children(|row| {
                row.spawn(caption("540 Ma"));
                row.spawn(caption("Present"));
            });
        });
}

/// Dragging (or clicking) the slider sets the globe time.
fn on_slider_change(
    change: On<ValueChange<f32>>,
    sliders: Query<(), With<TimeSlider>>,
    mut commands: Commands,
    mut time: ResMut<GlobeTime>,
    mut changes: MessageWriter<TimeChanged>,
) {
    if !sliders.contains(change.source) {
        return;
    }
    let value = change.value.clamp(0.0, 1.0);
    commands.entity(change.source).insert(SliderValue(value));
    let from = time.years;
    let to = years_for(f64::from(value));
    if to != from {
        time.years = to;
        changes.write(TimeChanged {
            from,
            to,
            cause: TimeChangeCause::Slider,
        });
    }
}

/// Moves the thumb and the label when the time changes from elsewhere (keys, the bench).
fn sync_slider_to_time(
    time: Res<GlobeTime>,
    mut commands: Commands,
    sliders: Query<(Entity, &SliderValue), With<TimeSlider>>,
    mut labels: Query<&mut Text, With<AgeLabel>>,
) {
    if !time.is_changed() {
        return;
    }
    let value = widget_value_for(time.years) as f32;
    for (entity, current) in &sliders {
        // A drag already wrote its own value; don't round-trip it through the age.
        if (current.0 - value).abs() > 1e-6 {
            commands.entity(entity).insert(SliderValue(value));
        }
    }
    let label = format_years_bp(time.years);
    for mut text in &mut labels {
        if text.0 != label {
            text.0.clone_from(&label);
        }
    }
}

fn position_thumb(
    sliders: Query<&SliderValue, (With<TimeSlider>, Changed<SliderValue>)>,
    mut thumbs: Query<&mut Node, With<ThumbNode>>,
) {
    let Some(value) = sliders.iter().next() else {
        return;
    };
    for mut node in &mut thumbs {
        node.left = percent(value.0.clamp(0.0, 1.0) * 100.0);
    }
}

/// Gathers input, advances the idle clock and decides the opacity.
fn track_idle(
    time: Res<Time>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    sliders: Query<(&SliderDragState, &Hovered), With<TimeSlider>>,
    mut idle: ResMut<UiIdle>,
    mut opacity: ResMut<UiOpacity>,
) {
    let key = keys.is_some_and(|k| k.get_just_pressed().next().is_some());
    let button = mouse.is_some_and(|m| m.get_pressed().next().is_some());
    let scrolled = scroll.is_some_and(|s| s.delta != Vec2::ZERO);
    let slider = sliders
        .iter()
        .any(|(drag, hover)| drag.dragging || hover.get());
    let next = if key || button || scrolled || slider {
        0.0
    } else {
        idle.idle_secs + time.delta_secs()
    };
    if idle.idle_secs != next {
        idle.idle_secs = next;
    }
    let value = ui_opacity(next);
    if opacity.0 != value {
        opacity.0 = value;
    }
}

fn apply_opacity(
    opacity: Res<UiOpacity>,
    mut nodes: Query<(
        Ref<Faded>,
        Option<&mut BackgroundColor>,
        Option<&mut TextColor>,
    )>,
) {
    for (faded, background, text) in &mut nodes {
        if !opacity.is_changed() && !faded.is_added() {
            continue;
        }
        let color = faded.base.with_alpha(faded.base.alpha() * opacity.0);
        match (faded.text, background, text) {
            (true, _, Some(mut text)) => text.0 = color,
            (false, Some(mut background), _) => background.0 = color,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::time::TimeUpdateStrategy;

    use super::*;
    use crate::time::TimePlugin;

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins((TimePlugin, SliderUiPlugin));
        app.update();
        app
    }

    fn slider(app: &mut App) -> Entity {
        app.world_mut()
            .query_filtered::<Entity, With<TimeSlider>>()
            .single(app.world())
            .unwrap()
    }

    fn widget_value(app: &mut App) -> f32 {
        let entity = slider(app);
        app.world().get::<SliderValue>(entity).unwrap().0
    }

    fn years(app: &App) -> f64 {
        app.world().resource::<GlobeTime>().years.0
    }

    /// What the widget does on a drag: emit a value change for the slider.
    fn drag_to(app: &mut App, value: f32) {
        let source = slider(app);
        app.world_mut().trigger(ValueChange {
            source,
            value,
            is_final: false,
        });
        app.update();
    }

    #[test]
    fn mapping_is_mirrored() {
        assert_eq!(widget_value_for(YearsBp(0.0)), 1.0);
        assert!(widget_value_for(YearsBp(540.0e6)).abs() < 1e-12);
        for v in [0.0, 0.1298, 0.5, 0.6269, 1.0] {
            assert!((widget_value_for(years_for(v)) - v).abs() < 1e-9, "{v}");
        }
        // The last ~12,000 years fill roughly the right third.
        let third = 1.0 - widget_value_for(YearsBp(12_000.0));
        assert!((0.3..0.36).contains(&third), "{third}");
    }

    #[test]
    fn slider_drag_sets_time() {
        let mut app = app();
        drag_to(&mut app, 0.5);
        assert!((years(&app) - 164_266.77).abs() < 0.01, "{}", years(&app));
        let events: Vec<TimeChanged> = app
            .world()
            .resource::<Messages<TimeChanged>>()
            .iter_current_update_messages()
            .copied()
            .collect();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].cause, TimeChangeCause::Slider);
        assert_eq!(widget_value(&mut app), 0.5);

        drag_to(&mut app, 1.0);
        assert_eq!(years(&app), 0.0, "present at the right");
        drag_to(&mut app, 0.0);
        assert!((years(&app) - 540.0e6).abs() < 1.0, "{}", years(&app));
    }

    #[test]
    fn external_time_moves_thumb() {
        let mut app = app();
        app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(66.0e6);
        app.update();
        let v = widget_value(&mut app);
        assert!((v - 0.1298).abs() < 1e-4, "{v}");
        let label = app
            .world_mut()
            .query_filtered::<&Text, With<AgeLabel>>()
            .single(app.world())
            .unwrap()
            .0
            .clone();
        assert_eq!(label, format_years_bp(YearsBp(66.0e6)));

        app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(21.0e3);
        app.update();
        let v = widget_value(&mut app);
        assert!((v - 0.6269).abs() < 1e-4, "{v}");
    }

    #[test]
    fn ui_fades_when_idle() {
        assert_eq!(ui_opacity(0.0), 1.0);
        assert_eq!(ui_opacity(2.4), 1.0);
        assert_eq!(ui_opacity(3.0), IDLE_OPACITY);
        assert_eq!(ui_opacity(100.0), IDLE_OPACITY);

        let mut app = app();
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            100,
        )));
        for _ in 0..10 {
            app.update();
        }
        assert_eq!(app.world().resource::<UiOpacity>().0, 1.0, "still active");
        for _ in 0..21 {
            app.update();
        }
        let idle = app.world().resource::<UiIdle>().idle_secs;
        assert!((idle - 3.1).abs() < 1e-3, "{idle}");
        assert_eq!(app.world().resource::<UiOpacity>().0, IDLE_OPACITY);

        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyA);
        app.update();
        assert_eq!(app.world().resource::<UiOpacity>().0, 1.0);
        // The colours follow.
        let thumb = app
            .world_mut()
            .query_filtered::<&BackgroundColor, With<ThumbNode>>()
            .single(app.world())
            .unwrap()
            .0;
        assert_eq!(thumb.alpha(), 1.0);
        // Text fades its glyphs, never its (transparent) background.
        let (label, background) = app
            .world_mut()
            .query_filtered::<(&TextColor, Option<&BackgroundColor>), With<AgeLabel>>()
            .single(app.world())
            .map(|(t, b)| (t.0, b.map(|b| b.0)))
            .unwrap();
        assert_eq!(label, TEXT_COLOR);
        assert!(
            background.is_none_or(|b| b.alpha() == 0.0),
            "{background:?}"
        );
    }
}
