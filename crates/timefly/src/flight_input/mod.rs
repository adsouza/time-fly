//! Translates device input into one flight intent, consumed by both flight models and navigation.

mod keyboard;
mod target;
mod touch;

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::input::touch::{TouchInput, TouchPhase};
use bevy::picking::Pickable;
use bevy::prelude::*;
use bevy::ui::{CalculatedClip, UiGlobalTransform};
use bevy::window::{PrimaryWindow, WindowFocused};

use crate::fly_camera::{FlySystems, MAX_STEP_S};
use crate::globe_flight::{GlobeFlightSystems, PIXELS_PER_LINE};
use crate::region::RegionState;
use crate::region::navigate::{LEAVE_KEY, NavPhase, Navigation, NavigationSystems};
use crate::ui::{BlocksCamera, PointerOverUi, credits::CreditsOpen};

pub use self::keyboard::{
    FlightAxes, descend_modifiers, is_mac_platform, keys_to_input, platform_is_mac,
};

use self::target::{Destination, TargetWorld};
use self::touch::Gestures;

/// Time constant for the part of a scroll gesture that continues after the event.
const SCROLL_COAST_S: f64 = 0.12;
const MAX_SCROLL_COAST_LINES: f64 = 4.0;
const SCROLL_STOP_LINES: f64 = 0.005;

/// Motion for the current frame, in the same local axes as the keyboard. The duration is
/// normally the frame time. For a fast gesture it can be longer, so motion tracks finger
/// displacement even when the frame time is short.
#[derive(Resource, Default)]
pub(crate) struct FlightIntent {
    pub movement: FlightAxes,
    pub dt_s: f64,
    pub look_delta: Vec2,
    pub scroll_lines: f64,
    pub leave: bool,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum FlightView {
    #[default]
    Uncontrolled,
    Globe,
    Region,
}

fn flight_view(state: Option<&State<RegionState>>, phase: NavPhase) -> FlightView {
    if phase.fly_controls().is_some() {
        FlightView::Region
    } else if matches!(phase, NavPhase::Diving { .. })
        || (phase == NavPhase::Globe && state.is_none_or(|s| *s.get() == RegionState::Globe))
    {
        FlightView::Globe
    } else {
        FlightView::Uncontrolled
    }
}

pub(crate) struct FlightInputPlugin;

impl Plugin for FlightInputPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<TouchInput>()
            .add_message::<WindowFocused>()
            .init_resource::<FlightIntent>()
            .add_systems(
                Update,
                gather_intent
                    .before(NavigationSystems)
                    .before(FlySystems::Step)
                    .before(GlobeFlightSystems::Step),
            );
    }
}

pub(crate) fn add_flight_input(app: &mut App) {
    if !app.is_plugin_added::<FlightInputPlugin>() {
        app.add_plugins(FlightInputPlugin);
    }
}

#[derive(SystemParam)]
struct Devices<'w, 's> {
    keys: Option<Res<'w, ButtonInput<KeyCode>>>,
    mouse: Option<Res<'w, ButtonInput<MouseButton>>>,
    motion: Option<Res<'w, AccumulatedMouseMotion>>,
    scroll: Option<Res<'w, AccumulatedMouseScroll>>,
    touch: MessageReader<'w, 's, TouchInput>,
    focus: MessageReader<'w, 's, WindowFocused>,
}

type TouchUiNode = (
    Entity,
    &'static ComputedNode,
    &'static UiGlobalTransform,
    &'static InheritedVisibility,
    Option<&'static Pickable>,
    Option<&'static CalculatedClip>,
);

#[derive(SystemParam)]
struct TouchUi<'w, 's> {
    window: Query<'w, 's, (Entity, &'static Window), With<PrimaryWindow>>,
    nodes: Query<'w, 's, TouchUiNode>,
    parents: Query<'w, 's, &'static ChildOf>,
    blockers: Query<'w, 's, (), With<BlocksCamera>>,
    pointer_over_ui: Option<Res<'w, PointerOverUi>>,
    credits: Option<Res<'w, CreditsOpen>>,
}

impl TouchUi<'_, '_> {
    fn at(&self, position: Vec2, scale: f32) -> bool {
        let physical = position * scale;
        // BlocksCamera can be on a transparent parent, as with the time slider. Check the
        // visible pickable parts at the press position, even if the finger has already moved.
        self.nodes
            .iter()
            .any(|(entity, node, transform, visibility, pickable, clip)| {
                visibility.get()
                    && pickable.is_none_or(|p| p.is_hoverable)
                    && clip.is_none_or(|c| c.contains_point(physical))
                    && node.contains_point(*transform, physical)
                    && (self.blockers.contains(entity)
                        || self
                            .parents
                            .iter_ancestors(entity)
                            .any(|e| self.blockers.contains(e)))
            })
    }
}

#[derive(Default)]
struct InputState {
    view: FlightView,
    mouse_look: bool,
    coast_scroll_lines: f64,
    gestures: Gestures,
    destination: Option<Destination>,
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn gather_intent(
    mut devices: Devices,
    ui: TouchUi,
    target_world: TargetWorld,
    nav: Option<Res<Navigation>>,
    region_state: Option<Res<State<RegionState>>>,
    time: Res<Time>,
    real: Res<Time<Real>>,
    mut state: Local<InputState>,
    mut output: ResMut<FlightIntent>,
) {
    let phase = nav.as_ref().map_or(NavPhase::Globe, |n| n.phase);
    let view = flight_view(region_state.as_deref(), phase);
    if state.view != view {
        state.view = view;
        state.mouse_look = false;
        state.gestures.cancel_taps();
    }
    let modal = ui.credits.as_ref().is_some_and(|c| c.0);
    let over_ui = ui.pointer_over_ui.as_ref().is_some_and(|o| o.0) || modal;
    let mut mouse_pressed = false;
    if let Some(mouse) = devices.mouse.as_ref() {
        if mouse.just_pressed(MouseButton::Left) {
            state.mouse_look = !over_ui && view != FlightView::Uncontrolled;
        } else if !mouse.pressed(MouseButton::Left) {
            state.mouse_look = false;
        }
        mouse_pressed = mouse.get_just_pressed().next().is_some();
    }
    let mouse_look = devices
        .motion
        .as_ref()
        .filter(|_| state.mouse_look)
        .map_or(Vec2::ZERO, |m| m.delta);
    let raw_scroll_lines = devices
        .scroll
        .as_ref()
        .filter(|_| !over_ui)
        .map_or(0.0, |s| match s.unit {
            MouseScrollUnit::Line => f64::from(s.delta.y),
            MouseScrollUnit::Pixel => f64::from(s.delta.y / PIXELS_PER_LINE),
        });
    let raw_scroll_lines = if raw_scroll_lines.is_finite() {
        raw_scroll_lines
    } else {
        0.0
    };

    let window = ui.window.single().ok();
    let mut lost_focus = false;
    for event in devices.focus.read() {
        if window.is_some_and(|(id, _)| id == event.window && !event.focused) {
            lost_focus = true;
            state.mouse_look = false;
            state.gestures = Gestures::default();
            state.destination = None;
        }
    }
    if modal {
        state.gestures.block();
    }
    let touch = state.gestures.read(
        devices.touch.read().filter_map(|event| {
            let (id, window) = window?;
            if id != event.window {
                return None;
            }
            let blocked = event.phase == TouchPhase::Started
                && (modal || ui.at(event.position, window.scale_factor()));
            Some((*event, blocked))
        }),
        real.elapsed_secs_f64(),
    );

    let held = devices
        .keys
        .as_deref()
        .map(keys_to_input)
        .unwrap_or_default();
    let leave = devices
        .keys
        .as_ref()
        .is_some_and(|k| k.just_pressed(LEAVE_KEY));
    let dt_s = time.delta_secs_f64().min(f64::from(MAX_STEP_S));
    let short_side = window.map_or(1.0, |(_, w)| w.width().min(w.height()).max(1.0));
    // One screen width of panning is 1.5 seconds of cruise. A doubling of finger spacing
    // is about half a second of forward travel, regardless of the initial spacing.
    let pan = touch.pan * (1.5 / short_side);
    let travel_s = Vec3::new(pan.x, -pan.y, -touch.pinch * 0.75);
    let look_delta = mouse_look + touch.look;
    let scroll_lines = if view == FlightView::Globe
        && !over_ui
        && !lost_focus
        && window.is_none_or(|(_, w)| w.focused)
        && held.move_local.y == 0.0
        && travel_s.y == 0.0
    {
        step_scroll(&mut state.coast_scroll_lines, raw_scroll_lines, dt_s)
    } else {
        state.coast_scroll_lines = 0.0;
        0.0
    };
    let (movement, dt_s) = combine_movement(held, travel_s, dt_s);
    let mut intent = FlightIntent {
        movement,
        dt_s,
        look_delta,
        scroll_lines,
        leave,
    };

    let can_target =
        (phase == NavPhase::Globe || phase.is_flying()) && view != FlightView::Uncontrolled;
    let interrupted = modal
        || mouse_pressed
        || touch.look != Vec2::ZERO
        || travel_s != Vec3::ZERO
        || held != FlightAxes::default()
        || mouse_look != Vec2::ZERO
        || raw_scroll_lines != 0.0
        || scroll_lines != 0.0
        || leave;
    if !can_target || interrupted || touch.started {
        state.destination = None;
    }
    if can_target
        && !interrupted
        && let Some(position) = touch.double_tap
    {
        state.destination = target_world.pick(view, position);
    }
    if let Some(destination) = state.destination
        && !target_world.steer(view, destination, &mut intent)
    {
        state.destination = None;
    }
    *output = intent;
}

/// Apply half the wheel motion right away and release the rest with an exponential decay.
/// Opposite scroll input drops the pending motion so it can reverse direction immediately.
fn step_scroll(pending_lines: &mut f64, lines: f64, dt_s: f64) -> f64 {
    if dt_s <= 0.0 {
        return 0.0;
    }
    if *pending_lines * lines < 0.0 {
        *pending_lines = 0.0;
    }
    let immediate = 0.5 * lines;
    let pending =
        (*pending_lines + immediate).clamp(-MAX_SCROLL_COAST_LINES, MAX_SCROLL_COAST_LINES);
    let remaining = pending * (-dt_s / SCROLL_COAST_S).exp();
    *pending_lines = if remaining.abs() > SCROLL_STOP_LINES {
        remaining
    } else {
        0.0
    };
    immediate + (pending - remaining)
}

fn combine_movement(mut held: FlightAxes, travel_s: Vec3, dt_s: f64) -> (FlightAxes, f64) {
    if travel_s == Vec3::ZERO {
        return (held, dt_s);
    }
    let travel_s = travel_s.clamp_length_max(MAX_STEP_S);
    let duration = dt_s.max(f64::from(travel_s.length()));
    let key_share = (dt_s / duration) as f32;
    held.move_local =
        (held.move_local * key_share + travel_s / duration as f32).clamp(Vec3::NEG_ONE, Vec3::ONE);
    held.turn *= key_share;
    (held, duration)
}

#[cfg(test)]
pub(crate) mod tests;
