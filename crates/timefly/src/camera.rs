//! The orbit camera: drag rotates the view around the globe, the scroll wheel zooms between
//! [`MIN_DISTANCE_KM`] and [`MAX_DISTANCE_KM`]. It starts at [`START_DISTANCE_KM`] facing
//! longitude 0.
//!
//! Written here rather than with Bevy's `pan_orbit_camera` so the step is a pure function
//! ([`orbit_step`]) and the controller runs in a known order: before the globe copies the
//! camera's `Transform` to the bridge cameras ([`GlobeCameraSync`]), so the bridge views never
//! lag a frame. The camera is not parented to a rig, so its local `Transform` is its world one.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::prelude::*;
use timefly_core::geo::{geo_to_render, latlon_to_geo};

use crate::diagnostics::DiagnosticsContext;
use crate::globe::{EARTH_RADIUS_KM, GlobeCamera, GlobeCameraSync};
use crate::ui::PointerOverUi;

/// Closest zoom: the globe nearly fills the default 45° vertical view (95% of its height).
pub const MIN_DISTANCE_KM: f64 = EARTH_RADIUS_KM * 2.74;
pub const MAX_DISTANCE_KM: f64 = EARTH_RADIUS_KM * 6.0;
pub const START_DISTANCE_KM: f64 = EARTH_RADIUS_KM * 3.0;
/// Degrees of rotation per dragged pixel at the start distance; scales with the distance.
pub const DEGREES_PER_PIXEL: f64 = 0.2;
/// Distance factor per scroll line (scrolling up zooms in).
pub const ZOOM_PER_LINE: f64 = 0.9;
/// Highest latitude the camera reaches, short of the pole so "up" stays defined.
pub const MAX_LATITUDE_DEG: f64 = 89.0;
/// Trackpad pixels per scroll line.
const PIXELS_PER_LINE: f32 = 24.0;

/// Where the camera is: above `(lat, lon)` at `distance_km` from the globe centre.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct OrbitState {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub distance_km: f64,
}

impl Default for OrbitState {
    fn default() -> Self {
        OrbitState {
            lat_deg: 0.0,
            lon_deg: 0.0,
            distance_km: START_DISTANCE_KM,
        }
    }
}

impl OrbitState {
    /// The camera's position direction (unit, render frame).
    pub fn direction(&self) -> Vec3 {
        geo_to_render(latlon_to_geo(self.lat_deg, self.lon_deg))
            .0
            .as_vec3()
    }

    /// The camera transform: at the orbit position, looking at the globe centre, north up.
    pub fn transform(&self) -> Transform {
        Transform::from_translation(self.direction() * self.distance_km as f32)
            .looking_at(Vec3::ZERO, Vec3::Y)
    }
}

/// One frame of camera input.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct OrbitInput {
    /// Pointer movement while dragging the globe, in logical pixels (y down).
    pub drag_px: Vec2,
    /// Scroll lines (positive = away from the user = zoom in).
    pub scroll_lines: f32,
}

/// The next orbit state. Dragging moves the surface with the pointer; latitude stops short of
/// the poles, longitude wraps to (-180, 180] and the distance stays within the zoom limits.
pub fn orbit_step(state: OrbitState, input: OrbitInput) -> OrbitState {
    let rate = DEGREES_PER_PIXEL * state.distance_km / START_DISTANCE_KM;
    let lon = state.lon_deg - f64::from(input.drag_px.x) * rate;
    let lat = state.lat_deg + f64::from(input.drag_px.y) * rate;
    let distance = state.distance_km * ZOOM_PER_LINE.powf(f64::from(input.scroll_lines));
    OrbitState {
        lat_deg: lat.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG),
        lon_deg: wrap_longitude(lon),
        distance_km: distance.clamp(MIN_DISTANCE_KM, MAX_DISTANCE_KM),
    }
}

fn wrap_longitude(lon: f64) -> f64 {
    let wrapped = (lon + 180.0).rem_euclid(360.0) - 180.0;
    if wrapped == -180.0 { 180.0 } else { wrapped }
}

/// Whether the current left-button drag began on the globe (not on the UI).
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
struct OrbitDrag(bool);

pub struct OrbitCameraPlugin;

impl Plugin for OrbitCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OrbitState>()
            .init_resource::<OrbitDrag>()
            .add_systems(
                Update,
                (orbit_camera_system, apply_orbit)
                    .chain()
                    .before(GlobeCameraSync),
            );
    }
}

/// Gathers drag and scroll input and steps the orbit.
fn orbit_camera_system(
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    motion: Option<Res<AccumulatedMouseMotion>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    over_ui: Option<Res<PointerOverUi>>,
    mut drag: ResMut<OrbitDrag>,
    mut state: ResMut<OrbitState>,
) {
    let over_ui = over_ui.is_some_and(|o| o.0);
    if let Some(mouse) = &mouse {
        if mouse.just_pressed(MouseButton::Left) {
            drag.0 = !over_ui;
        } else if !mouse.pressed(MouseButton::Left) && drag.0 {
            drag.0 = false;
        }
    }
    let drag_px = match (&motion, drag.0) {
        (Some(motion), true) => motion.delta,
        _ => Vec2::ZERO,
    };
    let scroll_lines = match &scroll {
        Some(s) if !over_ui => match s.unit {
            MouseScrollUnit::Line => s.delta.y,
            MouseScrollUnit::Pixel => s.delta.y / PIXELS_PER_LINE,
        },
        _ => 0.0,
    };
    let input = OrbitInput {
        drag_px,
        scroll_lines,
    };
    if input == OrbitInput::default() {
        return;
    }
    let next = orbit_step(*state, input);
    if next != *state {
        *state = next;
    }
}

/// Moves the globe camera to the orbit state; writes only on change.
fn apply_orbit(
    state: Res<OrbitState>,
    mut cameras: Query<&mut Transform, With<GlobeCamera>>,
    added: Query<(), Added<GlobeCamera>>,
    context: Option<ResMut<DiagnosticsContext>>,
) {
    if !state.is_changed() && added.is_empty() {
        return;
    }
    let transform = state.transform();
    for mut t in &mut cameras {
        if *t != transform {
            *t = transform;
        }
    }
    if let Some(mut context) = context {
        let direction = state.direction();
        context.camera_distance_km = Some(state.distance_km);
        context.camera_direction = Some(direction.to_array());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(drag: (f32, f32), scroll: f32) -> OrbitInput {
        OrbitInput {
            drag_px: Vec2::new(drag.0, drag.1),
            scroll_lines: scroll,
        }
    }

    #[test]
    fn starts_at_three_radii_facing_longitude_zero() {
        let s = OrbitState::default();
        assert_eq!(s.distance_km, 3.0 * EARTH_RADIUS_KM);
        let t = s.transform();
        assert!((t.translation - Vec3::Z * 3.0 * EARTH_RADIUS_KM as f32).length() < 1e-2);
        assert!((t.forward().as_vec3() - Vec3::NEG_Z).length() < 1e-6);
    }

    #[test]
    fn zoom_stays_within_limits() {
        let mut s = OrbitState::default();
        for _ in 0..200 {
            s = orbit_step(s, input((0.0, 0.0), 1.0));
        }
        assert_eq!(s.distance_km, MIN_DISTANCE_KM);
        for _ in 0..200 {
            s = orbit_step(s, input((0.0, 0.0), -1.0));
        }
        assert_eq!(s.distance_km, MAX_DISTANCE_KM);
        // One line out from the start (one line in already reaches the closest zoom).
        let one = orbit_step(OrbitState::default(), input((0.0, 0.0), -1.0));
        assert!((one.distance_km - START_DISTANCE_KM / ZOOM_PER_LINE).abs() < 1e-9);
    }

    #[test]
    fn closest_zoom_nearly_fills_the_view() {
        // The globe camera uses Bevy's default perspective; its fov is vertical, the limiting
        // direction in a landscape window.
        let fov = f64::from(PerspectiveProjection::default().fov);
        let globe = 2.0 * (EARTH_RADIUS_KM / MIN_DISTANCE_KM).asin();
        let share = globe / fov;
        assert!(
            (0.95..1.0).contains(&share),
            "at the closest zoom the globe spans {:.1}% of the view's height",
            share * 100.0
        );
    }

    #[test]
    fn drag_rotates_with_the_pointer_and_stops_short_of_the_poles() {
        let s = OrbitState::default();
        // Dragging right pulls the surface right: the camera moves west.
        let right = orbit_step(s, input((10.0, 0.0), 0.0));
        assert!((right.lon_deg + 10.0 * DEGREES_PER_PIXEL).abs() < 1e-9);
        assert_eq!(right.distance_km, s.distance_km);
        // Dragging down pulls the surface down: the camera moves north.
        let down = orbit_step(s, input((0.0, 10.0), 0.0));
        assert!(down.lat_deg > 0.0);
        let pole = orbit_step(s, input((0.0, 1.0e6), 0.0));
        assert_eq!(pole.lat_deg, MAX_LATITUDE_DEG);
        assert!(pole.transform().translation.is_finite());
        // Longitude wraps.
        let around = orbit_step(s, input((-1000.0, 0.0), 0.0));
        assert!((-180.0..=180.0).contains(&around.lon_deg));
        assert!((around.lon_deg - (200.0 - 360.0)).abs() < 1e-9);
        // Closer in, the same drag turns less.
        let near = OrbitState {
            distance_km: MIN_DISTANCE_KM,
            ..s
        };
        assert!(orbit_step(near, input((10.0, 0.0), 0.0)).lon_deg.abs() < right.lon_deg.abs());
    }

    #[test]
    fn controller_moves_the_globe_camera() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<AccumulatedMouseScroll>()
            .init_resource::<DiagnosticsContext>()
            .add_plugins(OrbitCameraPlugin);
        let camera = app
            .world_mut()
            .spawn((GlobeCamera, Transform::default()))
            .id();
        app.update();
        let at = |app: &App| app.world().get::<Transform>(camera).unwrap().translation;
        assert!((at(&app) - Vec3::Z * START_DISTANCE_KM as f32).length() < 1e-2);

        app.world_mut()
            .resource_mut::<AccumulatedMouseScroll>()
            .delta = Vec2::new(0.0, -3.0);
        app.update();
        let expected = START_DISTANCE_KM * ZOOM_PER_LINE.powi(-3);
        assert!((f64::from(at(&app).length()) - expected).abs() < 0.5);
        let context = app.world().resource::<DiagnosticsContext>();
        assert!((context.camera_distance_km.unwrap() - expected).abs() < 1e-6);

        // Scrolling over the UI leaves the camera alone.
        app.insert_resource(PointerOverUi(true));
        app.update();
        assert!((f64::from(at(&app).length()) - expected).abs() < 0.5);
    }
}
