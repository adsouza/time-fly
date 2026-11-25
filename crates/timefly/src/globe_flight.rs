//! Globe flight (M3 design §5.4): the dragonfly flies over the globe and the globe camera
//! chases it, replacing M1's drag-to-spin orbit camera.
//!
//! - **State.** [`GlobeFlightState`] holds the core's [`GlobeFlight`] (a direction, a tangent
//!   heading and an altitude, f64 in the render frame), so the fly crosses the poles with no
//!   special case. [`GlobeLook`] holds the view drag's offset.
//! - **Input** ([`GlobeFlightSystems::Step`]), on the globe and while a dive waits in the cloud
//!   (the player can climb back out to cancel it): the region's keys ([`keys_to_input`]), the
//!   drag and the scroll wheel (up descends, down climbs, M1's zoom habit), mapped by
//!   [`globe_input`] and stepped by `globe_fly_step`. Below the dive altitude a descent eases
//!   into the floor inside the cloud ([`descent_ease`]).
//!   A drag turns the fly and tilts the view (dragging down looks further down, as in a
//!   region); the tilt is an offset added to both the chase look and the eye's elevation, and
//!   the stored offset is clamped to [`globe_look_offset_range`], so a drag back never has a
//!   dead zone.
//! - **Chase** ([`GlobeFlightSystems::Apply`], after the camera switch
//!   (`crate::terrain::RegionViewSystems`) and before the bridge cameras copy the globe camera,
//!   [`GlobeCameraSync`]): `globe_chase_pose` in kilometres, the look pitch rising with altitude
//!   (`globe_look_pitch_deg`: 50° at the dive, so the ground fills the lower two-thirds of the
//!   frame, to 85° at the top, with the eye straight over the fly and the globe centred) and
//!   the eye 5° above it, so the fly sits 5° below the view's centre at every altitude. The glide is the region's ([`ChaseGlide`]); the held offsets are carried
//!   by the turn of the local up as the fly travels, so travel is followed rigidly and only
//!   turns and view changes glide. The eye never leaves `GLOBE_MAX_CAMERA_DISTANCE_KM`.
//! - **The fly** ([`FlyPose`], scaled by `fly_scale` at the globe camera's field of view) and
//!   the overlay's [`FlyAltitude`] (`Kilometres`, true altitude above sea level) are written only
//!   while the globe camera is active. The relief exaggeration follows the altitude
//!   ([`GlobeExaggeration`], `globe_exaggeration`).
//! - **Projection** ([`fit_globe_projection`]): the field of view keeps M1's across the shorter
//!   axis (wider in portrait), and the far plane (culling only) reaches past the globe from the
//!   farthest eye.
//! - The diagnostics context's camera distance and direction are written every frame.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll, MouseScrollUnit};
use bevy::math::{DMat3, DQuat, DVec3};
use bevy::prelude::*;
use timefly_core::flight::altitudes::{DIVE_ALTITUDE_KM, GLOBE_MAX_CAMERA_DISTANCE_KM};
use timefly_core::flight::chase::{
    fly_scale, globe_chase_distance_km, globe_chase_pose, globe_look_offset_range,
};
use timefly_core::flight::cloud::globe_exaggeration;
use timefly_core::flight::globe::{
    BOOST, GLOBE_MIN_ALTITUDE_KM, GlobeFlight, GlobeInput, SPEED_PER_ALTITUDE, globe_fly_step,
    globe_speed_km_s,
};

use crate::creature::FlyPose;
use crate::diagnostics::DiagnosticsContext;
use crate::fly_camera::{
    ChaseGlide, FlyFlourish, FlyTurn, KeyInput, LOOK_RADIANS_PER_PX, MAX_STEP_S, TURN_DEG_PER_S,
    fly_speed, keys_to_input,
};
use crate::globe::{
    EARTH_RADIUS_KM, GlobeCamera, GlobeCameraSync, GlobeExaggeration, GlobeExaggerationSync,
};
use crate::region::RegionState;
use crate::region::navigate::{NavPhase, Navigation};
use crate::terrain::RegionViewSystems;
use crate::ui::PointerOverUi;
use crate::ui::altitude::{Altitude, FlyAltitude};

/// Seconds of full climb (or descent) one scroll line asks for: at the keys' 0.8 × altitude a
/// second, a line moves the fly 10 % of its altitude, as a line of M1's zoom moved the camera.
pub const SCROLL_CLIMB_S_PER_LINE: f64 = 0.125;
/// Trackpad pixels per scroll line.
pub const PIXELS_PER_LINE: f32 = 24.0;

/// The globe camera's far plane (km), for culling only (the depth is reversed-Z and infinite):
/// past the far side of the globe from the farthest eye.
pub const GLOBE_FAR_KM: f32 = (GLOBE_MAX_CAMERA_DISTANCE_KM + 2.0 * EARTH_RADIUS_KM) as f32;

/// The fly over the globe, as a resource.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct GlobeFlightState(pub GlobeFlight);

impl Default for GlobeFlightState {
    /// M1's start: over latitude 0, longitude 0, at the top altitude, heading north.
    fn default() -> Self {
        GlobeFlightState(GlobeFlight::start(0.0, 0.0))
    }
}

/// The globe view's drag offset (degrees, positive looks further down), added to both the
/// chase look and the eye's elevation; kept within `globe_look_offset_range` at the current
/// altitude.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
pub struct GlobeLook {
    pub offset_deg: f64,
}

/// Globe flight's systems: [`GlobeFlightSystems::Step`] turns input into
/// [`GlobeFlightState`] on the globe; [`GlobeFlightSystems::Apply`] moves the globe camera and
/// writes the fly's pose. Anything else that writes the state (region navigation, the region
/// bench) runs before `Apply`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobeFlightSystems {
    Step,
    Apply,
}

/// Whether the current left-button drag began on the globe (not on the UI).
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
struct GlobeDrag(bool);

/// The chase camera's glide and the fly's visual state between frames.
#[derive(Resource, Clone, Debug, Default)]
pub(crate) struct GlobeChase {
    glide: ChaseGlide,
    /// The flight last frame: the up to carry the glide by, the turn rate and idleness.
    last: Option<GlobeFlight>,
    flourish: FlyFlourish,
}

impl GlobeChase {
    /// Forgets the glide, the last flight and the flourish: the next frame starts at the wanted
    /// pose (a placement over the globe: the climb's swap).
    pub(crate) fn reset(&mut self) {
        *self = GlobeChase::default();
    }
}

pub struct GlobeFlightPlugin;

impl Plugin for GlobeFlightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GlobeFlightState>()
            .init_resource::<GlobeLook>()
            .init_resource::<GlobeDrag>()
            .init_resource::<GlobeChase>()
            .init_resource::<FlyPose>()
            .init_resource::<FlyAltitude>()
            .init_resource::<GlobeExaggeration>()
            .configure_sets(
                Update,
                (GlobeFlightSystems::Step, GlobeFlightSystems::Apply)
                    .chain()
                    .before(GlobeCameraSync)
                    .before(GlobeExaggerationSync),
            )
            // The cameras switch first, so a swap frame's pose is the active world's.
            .configure_sets(Update, GlobeFlightSystems::Apply.after(RegionViewSystems))
            .add_systems(
                Update,
                step_globe_flight
                    .run_if(on_the_globe)
                    .in_set(GlobeFlightSystems::Step),
            )
            .add_systems(
                Update,
                (fit_globe_projection, apply_globe_flight)
                    .chain()
                    .in_set(GlobeFlightSystems::Apply),
            );
    }
}

/// The flight takes input on the globe, and while a dive waits in the cloud (the player can
/// climb back out to cancel it). While a region is requested otherwise, generates, is shown, or
/// while the fly emerges, region navigation owns the state.
fn on_the_globe(state: Option<Res<State<RegionState>>>, nav: Option<Res<Navigation>>) -> bool {
    let diving = nav
        .as_ref()
        .is_some_and(|n| matches!(n.phase, NavPhase::Diving { .. }));
    diving
        || (state.is_none_or(|s| *s.get() == RegionState::Globe)
            && nav.is_none_or(|n| n.phase == NavPhase::Globe))
}

/// One frame's globe input from the held keys, the drag (logical pixels, y down) and the scroll
/// wheel (lines, positive away from the user), and the change to the view's drag offset
/// (degrees). The keys mean what they mean in a region: forward, strafe, climb, turn (`→` turns
/// right) and boost; a drag right turns right, a drag down looks further down; scrolling up
/// descends and down climbs, [`SCROLL_CLIMB_S_PER_LINE`] of full climb per line (M1's zoom
/// habit). A frame longer than `MAX_STEP_S` moves only that far.
pub fn globe_input(
    held: KeyInput,
    look_delta: Vec2,
    scroll_lines: f64,
    dt_s: f64,
) -> (GlobeInput, f64) {
    let dt = if dt_s.is_finite() {
        dt_s.clamp(0.0, f64::from(MAX_STEP_S))
    } else {
        0.0
    };
    let m = held
        .move_local
        .clamp(Vec3::splat(-1.0), Vec3::ONE)
        .as_dvec3();
    let px = f64::from(LOOK_RADIANS_PER_PX);
    let keys_turn = f64::from(held.turn.clamp(-1.0, 1.0)) * f64::from(TURN_DEG_PER_S).to_radians();
    // A scroll line is SCROLL_CLIMB_S_PER_LINE of full climb (scrolling up descends), whatever
    // the frame time; the step multiplies the climb by the speed and this frame's time.
    let scroll_climb = if dt > 0.0 && scroll_lines.is_finite() {
        -scroll_lines * SCROLL_CLIMB_S_PER_LINE / dt
    } else {
        0.0
    };
    let input = GlobeInput {
        forward: -m.z,
        strafe: m.x,
        // Positive turns left, as `yaw` does in a region.
        turn_rad: -keys_turn * dt - f64::from(look_delta.x) * px,
        climb: m.y + scroll_climb,
        boost: held.boost,
        dt_s: dt,
    };
    (input, (f64::from(look_delta.y) * px).to_degrees())
}

/// How much of a descent the fly keeps at `altitude_km` (design §7.1, Task 7): all of it down
/// to [`DIVE_ALTITUDE_KM`], then less and less toward the floor inside the cloud
/// ([`GLOBE_MIN_ALTITUDE_KM`], half the dive altitude), none at it, so the floor is eased rather
/// than hit.
pub fn descent_ease(altitude_km: f64) -> f64 {
    ((altitude_km - GLOBE_MIN_ALTITUDE_KM) / (DIVE_ALTITUDE_KM - GLOBE_MIN_ALTITUDE_KM))
        .clamp(0.0, 1.0)
}

/// A fly waiting in the cloud drifts at no more than this share of the M2 speed at its height
/// (design §6: "eases to a gentle drift"; the waiting test's bound).
pub const DIVE_DRIFT_SHARE: f64 = 0.05;

/// The share of the keys' horizontal speed a fly waiting in the cloud keeps (fix round 1,
/// minor 2): no more than [`DIVE_DRIFT_SHARE`] of the M2 speed at its height
/// (`fly_speed`, capped at `MAX_SPEED_MPS`), as its descent drifts, so it stays over the dive
/// point while the region generates.
pub fn dive_horizontal_scale(altitude_km: f64, boost: bool) -> f64 {
    let boost = if boost { BOOST } else { 1.0 };
    // Before the step's angular cap, which only slows it further.
    let full = SPEED_PER_ALTITUDE * altitude_km.max(0.0) * 1000.0 * boost;
    let drift = DIVE_DRIFT_SHARE * f64::from(fly_speed((altitude_km * 1000.0) as f32));
    if full > drift { drift / full } else { 1.0 }
}

/// Gathers the keys and the drag and steps [`GlobeFlightState`].
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn step_globe_flight(
    time: Res<Time>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    motion: Option<Res<AccumulatedMouseMotion>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    over_ui: Option<Res<PointerOverUi>>,
    mut drag: ResMut<GlobeDrag>,
    mut flight: ResMut<GlobeFlightState>,
    mut look: ResMut<GlobeLook>,
    nav: Option<Res<Navigation>>,
) {
    if let Some(mouse) = &mouse {
        if mouse.just_pressed(MouseButton::Left) {
            drag.0 = !over_ui.as_ref().is_some_and(|o| o.0);
        } else if !mouse.pressed(MouseButton::Left) {
            drag.0 = false;
        }
    }
    let held = keys.as_deref().map(keys_to_input).unwrap_or_default();
    let look_delta = match (&motion, drag.0) {
        (Some(m), true) => m.delta,
        _ => Vec2::ZERO,
    };
    let over_ui = over_ui.is_some_and(|o| o.0);
    let scroll_lines = match &scroll {
        Some(s) if !over_ui => match s.unit {
            MouseScrollUnit::Line => f64::from(s.delta.y),
            MouseScrollUnit::Pixel => f64::from(s.delta.y / PIXELS_PER_LINE),
        },
        _ => 0.0,
    };
    let (mut input, look_change) =
        globe_input(held, look_delta, scroll_lines, time.delta_secs_f64());
    if input.climb < 0.0 {
        input.climb *= descent_ease(flight.0.altitude_km);
    }
    // Waiting in the cloud, the fly drifts over the dive point rather than flying off it.
    if nav
        .as_ref()
        .is_some_and(|n| matches!(n.phase, NavPhase::Diving { .. }))
    {
        let scale = dive_horizontal_scale(flight.0.altitude_km, input.boost);
        input.forward *= scale;
        input.strafe *= scale;
    }
    // A neutral step would still re-normalise the heading and change its low bits, and the fly
    // would never count as idle (no hover bob): leave the state alone.
    let neutral =
        input.forward == 0.0 && input.strafe == 0.0 && input.climb == 0.0 && input.turn_rad == 0.0;
    if !neutral {
        let next = globe_fly_step(flight.0, input);
        if next != flight.0 {
            flight.0 = next;
        }
    }
    // The stored offset stays within this altitude's range, so a drag back moves the view at
    // once.
    let (lo, hi) = globe_look_offset_range(flight.0.altitude_km);
    let offset = (look.offset_deg + look_change).clamp(lo, hi);
    if offset != look.offset_deg {
        look.offset_deg = offset;
    }
}

/// The rotation whose `−z` is `heading` and `+y` is `up` (the fly's heading frame).
fn heading_rotation(heading: DVec3, up: DVec3) -> Quat {
    let up = up.normalize();
    let forward = (heading - up * heading.dot(up)).normalize();
    let right = forward.cross(up);
    DQuat::from_mat3(&DMat3::from_cols(right, up, -forward)).as_quat()
}

/// Places the globe camera behind and above the fly (smoothed), and writes the fly's pose, the
/// overlay's altitude, the exaggeration and the diagnostics' camera.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn apply_globe_flight(
    flight: Res<GlobeFlightState>,
    look: Res<GlobeLook>,
    time: Res<Time>,
    mut chase: ResMut<GlobeChase>,
    mut pose: ResMut<FlyPose>,
    mut altitude: ResMut<FlyAltitude>,
    mut exaggeration: ResMut<GlobeExaggeration>,
    mut cameras: Query<(&mut Transform, &Camera, &Projection), With<GlobeCamera>>,
    context: Option<ResMut<DiagnosticsContext>>,
) {
    let f = flight.0;
    let e = globe_exaggeration(f.altitude_km) as f32;
    if exaggeration.0 != e {
        exaggeration.0 = e;
    }
    let Ok((mut camera, settings, projection)) = cameras.single_mut() else {
        chase.glide.reset();
        chase.last = None;
        return;
    };
    let fly = f.position_km();
    let up = f.up();
    let want = globe_chase_pose(&f, look.offset_deg);
    // Travel over the sphere turns the local frame: carry the held offsets with it, so only
    // turns and view changes glide.
    let carry = chase.last.map_or(DQuat::IDENTITY, |last| {
        DQuat::from_rotation_arc(last.up(), up)
    });
    let (eye, target) = chase.glide.follow(
        fly,
        &want,
        globe_chase_distance_km(f.altitude_km),
        carry,
        time.delta_secs_f64(),
    );
    let want_camera =
        Transform::from_translation(eye.as_vec3()).looking_at(target.as_vec3(), up.as_vec3());
    if *camera != want_camera {
        *camera = want_camera;
    }
    if let Some(mut context) = context {
        context.camera_distance_km = Some(eye.length());
        context.camera_direction = Some(eye.normalize().as_vec3().to_array());
    }

    // The fly: held at its share of the view, banked by its turn rate (the signed turn of the
    // heading about up, the previous heading carried into this frame), pitched by its climb
    // (against the climb keys' speed at last frame's altitude) and bobbing while idle.
    let dt_s = time.delta_secs();
    let turn = match chase.last {
        Some(last) => {
            let before = carry * last.heading;
            let turned = up.dot(before.cross(f.heading)).atan2(before.dot(f.heading));
            let (yaw_rate, climb) = if dt_s > 0.0 {
                let dt = f64::from(dt_s);
                let full = globe_speed_km_s(last.altitude_km, false).max(1e-9);
                (
                    (turned / dt) as f32,
                    ((f.altitude_km - last.altitude_km) / dt / full) as f32,
                )
            } else {
                (0.0, 0.0)
            };
            FlyTurn {
                yaw_rate,
                climb,
                idle: last == f,
            }
        }
        None => FlyTurn {
            idle: true,
            ..default()
        },
    };
    chase.last = Some(f);
    let vfov = match projection {
        Projection::Perspective(p) => p.fov,
        _ => PerspectiveProjection::default().fov,
    };
    let scale = fly_scale((eye - fly).length(), f64::from(vfov)) as f32;
    let (bank_rad, pitch_rad, bob) = chase.flourish.step(turn, dt_s, time.elapsed_secs(), scale);
    let next = FlyPose {
        translation: (fly + up * f64::from(bob)).as_vec3(),
        rotation: heading_rotation(f.heading, up),
        scale,
        bank_rad,
        pitch_rad,
    };
    // Only the world on screen writes the fly and the overlay's altitude.
    if settings.is_active && *pose != next {
        *pose = next;
    }
    let reading = if settings.is_active {
        Some(Altitude::Kilometres(f.altitude_km))
    } else if matches!(altitude.0, Some(Altitude::Kilometres(_))) {
        None
    } else {
        altitude.0
    };
    if altitude.0 != reading {
        altitude.0 = reading;
    }
}

/// Keeps M1's field of view across the shorter viewport axis (wider in portrait, so the globe
/// and the fly fit), and the far plane at [`GLOBE_FAR_KM`].
pub fn fit_globe_projection(mut cameras: Query<&mut Projection, With<GlobeCamera>>) {
    for mut projection in &mut cameras {
        let Projection::Perspective(p) = &*projection else {
            continue;
        };
        if p.aspect_ratio <= 0.0 {
            continue;
        }
        let base = PerspectiveProjection::default().fov;
        let fov = if p.aspect_ratio < 1.0 {
            2.0 * ((base * 0.5).tan() / p.aspect_ratio).atan()
        } else {
            base
        };
        if (fov != p.fov || p.far != GLOBE_FAR_KM)
            && let Projection::Perspective(p) = &mut *projection
        {
            p.fov = fov;
            p.far = GLOBE_FAR_KM;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::camera::CameraProjection;
    use bevy::time::TimeUpdateStrategy;
    use timefly_core::flight::altitudes::{
        CHASE_GLOBE_TOP_PITCH_DEG, DIVE_ALTITUDE_KM, FLY_SCREEN_FRACTION, GLOBE_MAX_ALTITUDE_KM,
    };
    use timefly_core::flight::chase::{
        CHASE_FLY_BELOW_CENTRE_DEG, globe_chase_elevation_deg, globe_look_pitch_deg,
    };

    use super::*;

    const STEP: Duration = Duration::from_micros(16_667);

    fn app(aspect: f32) -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<AccumulatedMouseMotion>()
            .init_resource::<DiagnosticsContext>()
            .add_plugins(GlobeFlightPlugin);
        let camera = app
            .world_mut()
            .spawn((
                GlobeCamera,
                Camera::default(),
                Projection::Perspective(PerspectiveProjection {
                    aspect_ratio: aspect,
                    ..default()
                }),
                Transform::default(),
            ))
            .id();
        (app, camera)
    }

    fn set_flight(app: &mut App, flight: GlobeFlight) {
        app.world_mut().resource_mut::<GlobeFlightState>().0 = flight;
    }

    fn flight(app: &App) -> GlobeFlight {
        app.world().resource::<GlobeFlightState>().0
    }

    fn camera(app: &App, entity: Entity) -> Transform {
        *app.world().get::<Transform>(entity).unwrap()
    }

    fn vfov(app: &App, entity: Entity) -> f32 {
        match app.world().get::<Projection>(entity).unwrap() {
            Projection::Perspective(p) => p.fov,
            _ => panic!("perspective"),
        }
    }

    /// The angle (degrees) by which `p` sits below the camera's view centre, and its sideways
    /// offset (as a sine) from the view's vertical plane.
    fn seen(cam: &Transform, p: DVec3) -> (f64, f64) {
        let eye = cam.translation.as_dvec3();
        let to = (p - eye).normalize();
        let forward = cam.forward().as_vec3().as_dvec3();
        let up = cam.up().as_vec3().as_dvec3();
        let below = -(to.dot(up)).atan2(to.dot(forward)).to_degrees();
        (below, to.dot(cam.right().as_vec3().as_dvec3()))
    }

    fn at(altitude_km: f64) -> GlobeFlight {
        GlobeFlight {
            altitude_km,
            ..GlobeFlight::start(35.0, -40.0)
        }
    }

    fn run(app: &mut App, frames: usize) {
        for _ in 0..frames {
            app.update();
        }
    }

    #[test]
    fn the_globe_camera_chases_the_fly() {
        let (mut app, cam) = app(16.0 / 9.0);
        for alt in [GLOBE_MAX_ALTITUDE_KM, 3_000.0, DIVE_ALTITUDE_KM] {
            let f = at(alt);
            set_flight(&mut app, f);
            // A large change glides (or snaps); either way it settles.
            run(&mut app, 180);
            let t = camera(&app, cam);
            let eye = t.translation.as_dvec3();
            // Outside the fly's altitude, behind it along the heading (straight over it at the
            // top; f32 camera translations, so within a kilometre).
            assert!(
                eye.length() > EARTH_RADIUS_KM + alt,
                "{alt}: {} km",
                eye.length()
            );
            assert!(
                (eye - f.position_km()).dot(f.heading) < 1.0,
                "{alt}: not behind"
            );
            // No roll about the view axis: the camera's right is level in the fly's frame.
            assert!(t.right().as_vec3().as_dvec3().dot(f.up()).abs() < 1e-3);
            // The fly 5° below the centre, on the vertical centre line.
            let (below, side) = seen(&t, f.position_km());
            assert!(
                (below - CHASE_FLY_BELOW_CENTRE_DEG).abs() < 0.05,
                "{alt}: {below}"
            );
            assert!(side.abs() < 1e-3, "{alt}: {side}");
            // The view looks down by the altitude's pitch, measured in the fly's frame.
            let down = -t
                .forward()
                .as_vec3()
                .as_dvec3()
                .dot(f.up())
                .asin()
                .to_degrees();
            assert!(
                (down - globe_look_pitch_deg(alt)).abs() < 0.05,
                "{alt}: {down}"
            );
            // The fly is drawn there at the size that holds its share of the view.
            let pose = *app.world().resource::<FlyPose>();
            let d = (f.position_km() - eye).length();
            let scale = fly_scale(d, f64::from(vfov(&app, cam))) as f32;
            assert!(
                (pose.scale - scale).abs() < 1e-3 * scale,
                "{pose:?} vs {scale}"
            );
            let bob = crate::creature::HOVER_AMPLITUDE * scale;
            assert!(
                (pose.translation.as_dvec3() - f.position_km()).length() <= f64::from(bob) + 0.05,
                "{pose:?}"
            );
            let fwd = (pose.rotation * Vec3::NEG_Z).as_dvec3();
            assert!(fwd.dot(f.heading) > 0.9999, "{pose:?}");
            // The overlay reads the true altitude; the relief follows it; the diagnostics see
            // the camera.
            assert_eq!(
                app.world().resource::<FlyAltitude>().0,
                Some(Altitude::Kilometres(alt))
            );
            let e = app.world().resource::<GlobeExaggeration>().0;
            assert_eq!(e, globe_exaggeration(alt) as f32);
            let context = app.world().resource::<DiagnosticsContext>();
            let distance = context.camera_distance_km.unwrap();
            assert!((distance - eye.length()).abs() < 1.0, "{distance}");
        }
        // At the top the globe centre is in front of the camera, below the view's centre.
        set_flight(&mut app, at(GLOBE_MAX_ALTITUDE_KM));
        run(&mut app, 60);
        let t = camera(&app, cam);
        let to_centre = -t.translation.as_dvec3().normalize();
        assert!(t.forward().as_vec3().as_dvec3().dot(to_centre) > 0.9);
        let (below, _) = seen(&t, DVec3::ZERO);
        assert!(below > 0.0, "{below}");
        assert_eq!(
            globe_look_pitch_deg(GLOBE_MAX_ALTITUDE_KM),
            CHASE_GLOBE_TOP_PITCH_DEG
        );
    }

    #[test]
    fn the_camera_never_goes_past_three_radii() {
        // 64 headings at the top, each with the drag's whole range (and past it), every frame
        // of the glide between them.
        let (mut app, cam) = app(16.0 / 9.0);
        for i in 0..64 {
            let a = std::f64::consts::TAU * f64::from(i) / 64.0;
            let mut f = at(GLOBE_MAX_ALTITUDE_KM);
            let east = f.dir.cross(f.heading);
            f.heading = (f.heading * a.cos() + east * a.sin()).normalize();
            set_flight(&mut app, f);
            for offset in [-200.0, -30.0, 0.0, 40.0, 200.0] {
                app.world_mut().resource_mut::<GlobeLook>().offset_deg = offset;
                for frame in 0..20 {
                    app.update();
                    let r = camera(&app, cam).translation.as_dvec3().length();
                    assert!(
                        r <= GLOBE_MAX_CAMERA_DISTANCE_KM + 1.0,
                        "heading {i}, offset {offset}, frame {frame}: {r} km"
                    );
                }
            }
        }
    }

    #[test]
    fn the_exaggeration_uniform_follows_altitude() {
        let (mut app, _) = app(16.0 / 9.0);
        app.add_plugins(AssetPlugin::default());
        let (plate, ocean, recent) = crate::globe::add_exaggeration_test_scene(&mut app);
        for alt in [GLOBE_MAX_ALTITUDE_KM, 4_000.0, DIVE_ALTITUDE_KM] {
            set_flight(&mut app, at(alt));
            app.update();
            let want = globe_exaggeration(alt) as f32;
            let world = app.world();
            let y = [
                world
                    .resource::<Assets<crate::globe::material::PlateMaterial>>()
                    .get(&plate)
                    .unwrap()
                    .params
                    .shape
                    .y,
                world
                    .resource::<Assets<crate::globe::material::OceanMaterial>>()
                    .get(&ocean)
                    .unwrap()
                    .params
                    .shape
                    .y,
                world
                    .resource::<Assets<crate::globe::material::RecentGlobeMaterial>>()
                    .get(&recent)
                    .unwrap()
                    .params
                    .shape
                    .y,
            ];
            assert_eq!(y, [want; 3], "{alt} km");
        }
    }

    /// The world-space ray through normalized device coordinates `(x, y)` of the camera.
    fn ray(t: &Transform, p: &PerspectiveProjection, x: f32, y: f32) -> DVec3 {
        let view = p
            .get_clip_from_view()
            .inverse()
            .project_point3(Vec3::new(x, y, 1.0));
        (t.rotation * view.normalize()).as_dvec3()
    }

    /// Whether a ray from `eye` meets the globe (radius `EARTH_RADIUS_KM`) ahead of it.
    fn meets_globe(eye: DVec3, dir: DVec3) -> bool {
        let b = eye.dot(dir);
        let c = eye.length_squared() - EARTH_RADIUS_KM * EARTH_RADIUS_KM;
        let disc = b * b - c;
        disc >= 0.0 && -b - disc.sqrt() > 0.0
    }

    fn perspective(app: &App, cam: Entity) -> PerspectiveProjection {
        match app.world().get::<Projection>(cam).unwrap() {
            Projection::Perspective(p) => p.clone(),
            _ => panic!("perspective"),
        }
    }

    #[test]
    fn the_ground_fills_the_lower_two_thirds_at_the_dive() {
        // Framing target (fix round 1): at the dive altitude and at 1,250 km the ray through
        // the frame's vertical centre line, a third of the way down from its top, meets the
        // globe, landscape and portrait; so does every ray below it on that line.
        for aspect in [16.0 / 9.0, 600.0 / 1000.0] {
            let (mut app, cam) = app(aspect);
            for alt in [DIVE_ALTITUDE_KM, 1_250.0] {
                set_flight(&mut app, at(alt));
                run(&mut app, 180);
                let t = camera(&app, cam);
                let p = perspective(&app, cam);
                let eye = t.translation.as_dvec3();
                for k in 0..=16 {
                    let y = 1.0 / 3.0 - (4.0 / 3.0) * k as f32 / 16.0;
                    assert!(
                        meets_globe(eye, ray(&t, &p, 0.0, y)),
                        "aspect {aspect}, {alt} km: the ray at y = {y} misses the globe"
                    );
                }
            }
        }
    }

    #[test]
    fn at_the_top_the_globe_is_centred_under_the_fly() {
        // Framing target (fix round 1): at the top altitude the globe's centre projects within
        // about a tenth of the view's height of the view's centre, and the fly is over the
        // disc. With the fly 5° below the centre the best possible is the eye straight over the
        // fly: tan 5° / tan 22.5° / 2 = 10.6 % of the height in landscape (6.4 % in portrait).
        for aspect in [16.0 / 9.0, 600.0 / 1000.0] {
            let (mut app, cam) = app(aspect);
            set_flight(&mut app, at(GLOBE_MAX_ALTITUDE_KM));
            run(&mut app, 180);
            let t = camera(&app, cam);
            let p = perspective(&app, cam);
            let clip_from_world = p.get_clip_from_view() * t.to_matrix().inverse();
            let c = clip_from_world.project_point3(Vec3::ZERO);
            let share = f64::from(c.y.abs()) / 2.0;
            assert!(share <= 0.107, "aspect {aspect}: {share} of the height");
            assert!(c.x.abs() < 1e-3, "aspect {aspect}: {c}");
            let eye = t.translation.as_dvec3();
            let fly = flight(&app).position_km();
            assert!(
                meets_globe(eye, (fly - eye).normalize()),
                "the fly is off the disc"
            );
        }
    }

    #[test]
    fn the_scroll_wheel_climbs_and_descends_on_the_globe() {
        let (mut app, _) = app(16.0 / 9.0);
        app.init_resource::<AccumulatedMouseScroll>();
        set_flight(&mut app, at(3_000.0));
        app.update();
        let scroll = |app: &mut App, unit: MouseScrollUnit, y: f32| {
            let mut s = app.world_mut().resource_mut::<AccumulatedMouseScroll>();
            s.unit = unit;
            s.delta = Vec2::new(0.0, y);
            app.update();
            app.world_mut()
                .resource_mut::<AccumulatedMouseScroll>()
                .delta = Vec2::ZERO;
            app.update();
            flight(app).altitude_km
        };
        // Scrolling up (away) descends, as M1's zoom closed in: each line is
        // SCROLL_CLIMB_S_PER_LINE of full climb at the keys' speed (0.8 × altitude a second).
        let per_line = SPEED_PER_ALTITUDE * SCROLL_CLIMB_S_PER_LINE;
        let a = scroll(&mut app, MouseScrollUnit::Line, 2.0);
        assert!((a - 3_000.0 * (1.0 - 2.0 * per_line)).abs() < 1e-6, "{a}");
        let b = scroll(&mut app, MouseScrollUnit::Pixel, -2.0 * PIXELS_PER_LINE);
        assert!((b - a * (1.0 + 2.0 * per_line)).abs() < 1e-3, "{b}");
        // Not over the UI.
        app.insert_resource(PointerOverUi(true));
        assert_eq!(scroll(&mut app, MouseScrollUnit::Line, 3.0), b);
    }

    #[test]
    fn the_fly_stays_in_view_in_portrait() {
        // A 600 × 1000 window: the fly's centre projects inside the view and it spans
        // FLY_SCREEN_FRACTION of the view's height (± 1 %), from the dive to the top.
        let (mut app, cam) = app(600.0 / 1000.0);
        for alt in [DIVE_ALTITUDE_KM, 2_500.0, 5_000.0, GLOBE_MAX_ALTITUDE_KM] {
            set_flight(&mut app, at(alt));
            run(&mut app, 60);
            let t = camera(&app, cam);
            let Projection::Perspective(p) = app.world().get::<Projection>(cam).unwrap().clone()
            else {
                panic!()
            };
            assert!(
                p.fov > PerspectiveProjection::default().fov,
                "widened in portrait"
            );
            let clip_from_world = p.get_clip_from_view() * t.to_matrix().inverse();
            let ndc = |w: Vec3| clip_from_world.project_point3(w);
            let pose = *app.world().resource::<FlyPose>();
            let c = ndc(pose.translation);
            assert!(c.x.abs() < 1.0 && c.y.abs() < 1.0, "{alt}: {c}");
            let half = t.up().as_vec3() * pose.scale / 2.0;
            let height = (ndc(pose.translation + half).y - ndc(pose.translation - half).y) / 2.0;
            assert!(
                (f64::from(height) - FLY_SCREEN_FRACTION).abs() < 0.01 * FLY_SCREEN_FRACTION,
                "{alt}: {height}"
            );
        }
    }

    #[test]
    fn keys_fly_the_globe() {
        let (mut app, _) = app(16.0 / 9.0);
        let f0 = at(3_000.0);
        set_flight(&mut app, f0);
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        let frames = 60;
        run(&mut app, frames);
        let f1 = flight(&app);
        // Along the great circle of the heading, forward.
        assert!(f1.dir.dot(f0.dir.cross(f0.heading)).abs() < 1e-9, "{f1:?}");
        assert!(f1.dir.dot(f0.heading) > 0.0, "{f1:?}");
        assert_eq!(f1.altitude_km, f0.altitude_km);
        let travelled = f0.dir.angle_between(f1.dir) * (EARTH_RADIUS_KM + 3_000.0);
        let want = SPEED_PER_ALTITUDE * 3_000.0 * STEP.as_secs_f64() * frames as f64;
        assert!(
            (travelled - want).abs() < 1e-3 * want,
            "{travelled} vs {want}"
        );
        // Climbing and turning.
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release(KeyCode::KeyW);
        keys.press(KeyCode::Space);
        keys.press(KeyCode::ArrowLeft);
        run(&mut app, 10);
        let f2 = flight(&app);
        assert!(f2.altitude_km > f1.altitude_km);
        let turned = f1.heading.cross(f2.heading).dot(f2.dir);
        assert!(turned > 0.0, "← turns left: {turned}");
    }

    #[test]
    fn the_keys_and_drag_map_like_a_regions() {
        let held = |m: Vec3, turn: f32, boost: bool| KeyInput {
            move_local: m,
            turn,
            boost,
        };
        let (i, look) = globe_input(
            held(Vec3::new(1.0, 1.0, -1.0), 1.0, true),
            Vec2::ZERO,
            0.0,
            0.1,
        );
        assert_eq!(
            (i.forward, i.strafe, i.climb, i.boost),
            (1.0, 1.0, 1.0, true)
        );
        let right = -f64::from(TURN_DEG_PER_S).to_radians() * 0.1;
        assert!(
            (i.turn_rad - right).abs() < 1e-9,
            "→ turns right: {}",
            i.turn_rad
        );
        assert_eq!((i.dt_s, look), (0.1, 0.0));
        // A drag right turns right; a drag down looks further down.
        let (i, look) = globe_input(KeyInput::default(), Vec2::new(10.0, 20.0), 0.0, 0.1);
        let px = f64::from(LOOK_RADIANS_PER_PX);
        assert!((i.turn_rad + 10.0 * px).abs() < 1e-9);
        assert!((look - (20.0 * px).to_degrees()).abs() < 1e-9);
        // A hitch moves no further than MAX_STEP_S.
        let (i, _) = globe_input(KeyInput::default(), Vec2::ZERO, 0.0, 3.0);
        assert_eq!(i.dt_s, f64::from(MAX_STEP_S));
        let (i, _) = globe_input(KeyInput::default(), Vec2::ZERO, 0.0, f64::NAN);
        assert_eq!(i.dt_s, 0.0);
        // A scroll line up is SCROLL_CLIMB_S_PER_LINE of full descent, whatever the frame time;
        // with no frame time it does nothing.
        let (i, _) = globe_input(KeyInput::default(), Vec2::ZERO, 1.0, 0.02);
        assert!(
            (i.climb * i.dt_s + SCROLL_CLIMB_S_PER_LINE).abs() < 1e-12,
            "{i:?}"
        );
        let (i, _) = globe_input(KeyInput::default(), Vec2::ZERO, 1.0, 0.0);
        assert_eq!(i.climb, 0.0);
    }

    #[test]
    fn dragging_down_looks_further_down_and_drags_back_at_once() {
        let (mut app, cam) = app(16.0 / 9.0);
        set_flight(&mut app, at(3_000.0));
        run(&mut app, 30);
        let pitch = |app: &App| {
            let t = camera(app, cam);
            -t.forward()
                .as_vec3()
                .as_dvec3()
                .dot(flight(app).up())
                .asin()
                .to_degrees()
        };
        let before = pitch(&app);
        app.world_mut()
            .resource_mut::<ButtonInput<MouseButton>>()
            .press(MouseButton::Left);
        let drag = |app: &mut App, dy: f32, frames: usize| {
            for _ in 0..frames {
                app.world_mut()
                    .resource_mut::<AccumulatedMouseMotion>()
                    .delta = Vec2::new(0.0, dy);
                app.update();
            }
            app.world_mut()
                .resource_mut::<AccumulatedMouseMotion>()
                .delta = Vec2::ZERO;
        };
        drag(&mut app, 20.0, 5);
        run(&mut app, 60);
        let down = pitch(&app);
        assert!(down > before + 5.0, "{before} -> {down}");
        // Far past the limit, then one pixel back: the view moves at once (no dead zone).
        drag(&mut app, 400.0, 30);
        let (_, hi) = globe_look_offset_range(3_000.0);
        let offset = app.world().resource::<GlobeLook>().offset_deg;
        assert!((offset - hi).abs() < 1e-9, "{offset} vs {hi}");
        drag(&mut app, -1.0, 1);
        assert!(app.world().resource::<GlobeLook>().offset_deg < hi);
        // The eye's elevation follows: the fly stays 5° below the centre.
        run(&mut app, 120);
        let (below, _) = seen(&camera(&app, cam), flight(&app).position_km());
        assert!((below - CHASE_FLY_BELOW_CENTRE_DEG).abs() < 0.05, "{below}");
        // Climbing re-clamps the stored offset to the new altitude's range.
        let elevation = globe_chase_elevation_deg(3_000.0) + offset;
        assert!(
            elevation <= timefly_core::flight::chase::GLOBE_CHASE_MAX_ELEVATION_DEG + 1e-9,
            "{elevation}"
        );
    }

    #[test]
    fn the_chase_follows_travel_rigidly_over_the_globe() {
        // Steady forward flight at the speed cap: the fly holds its place 5° below the centre
        // on every frame (a world-space follower would lag behind the turning frame).
        let (mut app, cam) = app(16.0 / 9.0);
        for alt in [GLOBE_MAX_ALTITUDE_KM, DIVE_ALTITUDE_KM] {
            set_flight(&mut app, at(alt));
            run(&mut app, 30);
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(KeyCode::KeyW);
            for frame in 0..120 {
                app.update();
                let (below, side) = seen(&camera(&app, cam), flight(&app).position_km());
                assert!(
                    (below - CHASE_FLY_BELOW_CENTRE_DEG).abs() < 0.1 && side.abs() < 1e-3,
                    "{alt} km, frame {frame}: {below} {side}"
                );
            }
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .release(KeyCode::KeyW);
        }
    }

    #[test]
    fn left_alone_the_fly_hovers_in_place() {
        // No input: the flight state stays bit-identical (so it counts as idle) and the drawn
        // fly bobs about it.
        let (mut app, _) = app(16.0 / 9.0);
        let f = at(3_000.0);
        set_flight(&mut app, f);
        let mut furthest = 0.0f64;
        for _ in 0..240 {
            app.update();
            let pose = *app.world().resource::<FlyPose>();
            furthest = furthest.max((pose.translation.as_dvec3() - f.position_km()).length());
        }
        assert_eq!(flight(&app), f);
        let amplitude =
            f64::from(crate::creature::HOVER_AMPLITUDE * app.world().resource::<FlyPose>().scale);
        assert!(furthest > 0.5 * amplitude, "{furthest} vs {amplitude}");
    }

    #[test]
    fn the_fly_pitches_with_climbs_on_the_globe() {
        // Spec §5.1, the final review: nose up climbing, down descending, at the climb keys'
        // full speed the limit.
        let (mut app, _) = app(16.0 / 9.0);
        set_flight(&mut app, at(3_000.0));
        run(&mut app, 3);
        let limit = crate::creature::MAX_PITCH_DEG.to_radians();
        for (key, sign) in [(KeyCode::Space, 1.0), (KeyCode::ControlLeft, -1.0)] {
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(key);
            run(&mut app, 60);
            let pitch = app.world().resource::<FlyPose>().pitch_rad;
            assert!(
                (pitch - sign * limit).abs() < 0.05 * limit,
                "{key:?}: {pitch}"
            );
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .release(key);
        }
        run(&mut app, 120);
        let level = app.world().resource::<FlyPose>().pitch_rad;
        assert!(level.abs() < 0.01, "{level}");
    }

    #[test]
    fn only_the_active_globe_writes_the_fly() {
        let (mut app, cam) = app(16.0 / 9.0);
        run(&mut app, 3);
        assert!(matches!(
            app.world().resource::<FlyAltitude>().0,
            Some(Altitude::Kilometres(_))
        ));
        // Off screen (a region shows): neither the pose nor the altitude.
        app.world_mut().get_mut::<Camera>(cam).unwrap().is_active = false;
        let other = FlyPose {
            translation: Vec3::new(1.0, 2.0, 3.0),
            ..default()
        };
        app.insert_resource(other);
        set_flight(&mut app, at(4_000.0));
        run(&mut app, 3);
        assert_eq!(*app.world().resource::<FlyPose>(), other);
        assert_eq!(app.world().resource::<FlyAltitude>().0, None);
        // A region's reading is left alone.
        let region = Some(Altitude::Metres(8_000.0));
        app.world_mut().resource_mut::<FlyAltitude>().0 = region;
        app.update();
        assert_eq!(app.world().resource::<FlyAltitude>().0, region);
        // Back on screen.
        app.world_mut().get_mut::<Camera>(cam).unwrap().is_active = true;
        run(&mut app, 3);
        assert_ne!(*app.world().resource::<FlyPose>(), other);
        assert_eq!(
            app.world().resource::<FlyAltitude>().0,
            Some(Altitude::Kilometres(4_000.0))
        );
    }

    #[test]
    fn the_flight_ignores_input_off_the_globe() {
        use bevy::state::app::StatesPlugin;
        let (mut app, _) = app(16.0 / 9.0);
        app.add_plugins(StatesPlugin).init_state::<RegionState>();
        app.update();
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        for state in [RegionState::Generating, RegionState::InRegion] {
            app.world_mut()
                .resource_mut::<NextState<RegionState>>()
                .set(state);
            app.update();
            let before = flight(&app);
            run(&mut app, 5);
            assert_eq!(flight(&app), before, "{state:?}");
        }
        app.world_mut()
            .resource_mut::<NextState<RegionState>>()
            .set(RegionState::Globe);
        app.update();
        let before = flight(&app);
        run(&mut app, 5);
        assert_ne!(flight(&app), before);
    }

    #[test]
    fn descending_eases_into_the_floor_inside_the_cloud() {
        assert_eq!(descent_ease(GLOBE_MAX_ALTITUDE_KM), 1.0);
        assert_eq!(descent_ease(DIVE_ALTITUDE_KM), 1.0);
        assert_eq!(descent_ease(GLOBE_MIN_ALTITUDE_KM), 0.0);
        let mid = descent_ease((DIVE_ALTITUDE_KM + GLOBE_MIN_ALTITUDE_KM) / 2.0);
        assert!((mid - 0.5).abs() < 1e-12, "{mid}");
        // Held down inside the cloud, the fly slows into the floor rather than hitting it.
        use bevy::state::app::StatesPlugin;
        let (mut app, _) = app(16.0 / 9.0);
        app.add_plugins(StatesPlugin)
            .init_state::<RegionState>()
            .init_resource::<Navigation>();
        app.world_mut().resource_mut::<Navigation>().phase = NavPhase::Diving {
            target: timefly_core::geo::RenderDir(DVec3::Z),
            snapshot: timefly_core::deep_time::YearsBp(0.0),
            since_s: 0.0,
            started: true,
        };
        set_flight(&mut app, at(DIVE_ALTITUDE_KM - 1.0));
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::ControlLeft);
        app.update();
        let mut last = flight(&app).altitude_km;
        let mut step = f64::INFINITY;
        for frame in 0..600 {
            app.update();
            let a = flight(&app).altitude_km;
            assert!(a <= last && a > GLOBE_MIN_ALTITUDE_KM, "frame {frame}: {a}");
            assert!(last - a <= step + 1e-9, "frame {frame}: sped up");
            step = last - a;
            last = a;
        }
        assert!(step < 1.0, "still {step} km a frame");
    }

    /// Fix round 1, minor 2: held forward and boosted through a 3 s (native) generation, a fly
    /// waiting in the cloud drifts no faster than 5 % of the M2 speed at its height, not the
    /// 2,400 km it would cover on the globe.
    #[test]
    fn diving_holds_the_fly_over_the_dive_point() {
        use crate::fly_camera::fly_speed;
        use bevy::state::app::StatesPlugin;
        for alt in [600.0, 900.0, DIVE_ALTITUDE_KM, 2_000.0] {
            for boost in [false, true] {
                let full = SPEED_PER_ALTITUDE
                    * alt
                    * 1000.0
                    * if boost {
                        timefly_core::flight::globe::BOOST
                    } else {
                        1.0
                    };
                let kept = full * dive_horizontal_scale(alt, boost);
                let cap = 0.05 * f64::from(fly_speed((alt * 1000.0) as f32));
                assert!(
                    kept <= cap * (1.0 + 1e-9),
                    "{alt} km, boost {boost}: {kept} m/s"
                );
                assert!(kept > 0.0);
            }
        }
        let (mut app, _) = app(16.0 / 9.0);
        app.add_plugins(StatesPlugin)
            .init_state::<RegionState>()
            .init_resource::<Navigation>();
        app.world_mut().resource_mut::<Navigation>().phase = NavPhase::Diving {
            target: timefly_core::geo::RenderDir(DVec3::Z),
            snapshot: timefly_core::deep_time::YearsBp(0.0),
            since_s: 0.0,
            started: true,
        };
        let start = at(1_100.0);
        set_flight(&mut app, start);
        app.update();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::ShiftLeft);
        let mut last = flight(&app);
        for frame in 0..180 {
            app.update();
            let f = flight(&app);
            let metres = f.dir.angle_between(last.dir) * (EARTH_RADIUS_KM + f.altitude_km) * 1000.0;
            let cap = 0.05 * f64::from(fly_speed((f.altitude_km * 1000.0) as f32));
            assert!(
                metres <= cap * STEP.as_secs_f64() * 1.001,
                "frame {frame}: {metres} m in a frame"
            );
            last = f;
        }
        let km = start.dir.angle_between(last.dir) * (EARTH_RADIUS_KM + 1_100.0);
        assert!(km < 2.0, "drifted {km} km");
    }

    #[test]
    fn the_flight_takes_input_while_diving_but_not_while_emerging() {
        use crate::region::navigate::World;
        use bevy::state::app::StatesPlugin;
        let (mut app, _) = app(16.0 / 9.0);
        app.add_plugins(StatesPlugin)
            .init_state::<RegionState>()
            .init_resource::<Navigation>();
        set_flight(&mut app, at(1_100.0));
        app.update();
        app.world_mut()
            .resource_mut::<NextState<RegionState>>()
            .set(RegionState::Generating);
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(KeyCode::KeyW);
        let diving = NavPhase::Diving {
            target: timefly_core::geo::RenderDir(DVec3::Z),
            snapshot: timefly_core::deep_time::YearsBp(0.0),
            since_s: 0.0,
            started: true,
        };
        for (phase, moves) in [
            (diving, true),
            (
                NavPhase::Emerging {
                    world: World::Globe,
                    elapsed_s: 0.5,
                },
                false,
            ),
            (NavPhase::Flying, false),
        ] {
            app.world_mut().resource_mut::<Navigation>().phase = phase;
            app.update();
            let before = flight(&app);
            run(&mut app, 5);
            assert_eq!(flight(&app) != before, moves, "{phase:?}");
        }
    }

    #[test]
    fn portrait_resize_fits_the_globe_and_landscape_restores_the_field_of_view() {
        let (mut app, entity) = app(1.0);
        let diameter = 2.0 * (EARTH_RADIUS_KM / (2.74 * EARTH_RADIUS_KM)).asin();
        for aspect in [9.0 / 16.0, 16.0 / 9.0] {
            let mut projection = app.world_mut().get_mut::<Projection>(entity).unwrap();
            let Projection::Perspective(p) = &mut *projection else {
                panic!()
            };
            p.aspect_ratio = aspect;
            app.update();
            let Projection::Perspective(p) = app.world().get::<Projection>(entity).unwrap() else {
                panic!()
            };
            let horizontal = 2.0 * ((p.fov / 2.0).tan() * aspect).atan();
            assert!(diameter < f64::from(horizontal.min(p.fov)));
            assert!((horizontal.min(p.fov) - PerspectiveProjection::default().fov).abs() < 1e-6);
            assert_eq!(p.far, GLOBE_FAR_KM);
        }
    }
}
