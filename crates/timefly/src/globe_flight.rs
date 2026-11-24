//! Globe flight (M3 design §5.4): the dragonfly flies over the globe and the globe camera
//! chases it, replacing M1's drag-to-spin orbit camera.
//!
//! - **State.** [`GlobeFlightState`] holds the core's [`GlobeFlight`] (a direction, a tangent
//!   heading and an altitude, f64 in the render frame), so the fly crosses the poles with no
//!   special case. [`GlobeLook`] holds the view drag's offset.
//! - **Input** ([`GlobeFlightSystems::Step`]), only on the globe: the region's keys
//!   ([`keys_to_input`]) and drag, mapped by [`globe_input`] and stepped by `globe_fly_step`.
//!   A drag turns the fly and tilts the view (dragging down looks further down, as in a
//!   region); the tilt is an offset added to both the chase look and the eye's elevation, and
//!   the stored offset is clamped to [`globe_look_offset_range`], so a drag back never has a
//!   dead zone.
//! - **Chase** ([`GlobeFlightSystems::Apply`], before the bridge cameras copy the globe camera,
//!   [`GlobeCameraSync`]): `globe_chase_pose` in kilometres, the look pitch rising with altitude
//!   (`globe_look_pitch_deg`) and the eye 5° above it, so the fly sits 5° below the view's centre
//!   at every altitude. The glide is the region's ([`ChaseGlide`]); the held offsets are carried
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

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::math::{DMat3, DQuat, DVec3};
use bevy::prelude::*;
use timefly_core::flight::altitudes::GLOBE_MAX_CAMERA_DISTANCE_KM;
use timefly_core::flight::chase::{
    fly_scale, globe_chase_distance_km, globe_chase_pose, globe_look_offset_range,
};
use timefly_core::flight::cloud::globe_exaggeration;
use timefly_core::flight::globe::{GlobeFlight, GlobeInput, globe_fly_step};

use crate::creature::FlyPose;
use crate::diagnostics::DiagnosticsContext;
use crate::fly_camera::{
    ChaseGlide, FlyFlourish, KeyInput, LOOK_RADIANS_PER_PX, MAX_STEP_S, TURN_DEG_PER_S,
    keys_to_input,
};
use crate::globe::{
    EARTH_RADIUS_KM, GlobeCamera, GlobeCameraSync, GlobeExaggeration, GlobeExaggerationSync,
};
use crate::region::RegionState;
use crate::region::navigate::{NavPhase, Navigation};
use crate::ui::PointerOverUi;
use crate::ui::altitude::{Altitude, FlyAltitude};

/// The time constant (s) of [`ease_flight_toward`]: about 95 % of the way in three of them.
pub const EASE_TIME_S: f64 = 0.6;
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
struct GlobeChase {
    glide: ChaseGlide,
    /// The flight last frame: the up to carry the glide by, the turn rate and idleness.
    last: Option<GlobeFlight>,
    flourish: FlyFlourish,
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

/// The flight takes input only on the globe: while a region is requested, generates, is shown
/// or is being left, region navigation owns the state.
fn on_the_globe(state: Option<Res<State<RegionState>>>, nav: Option<Res<Navigation>>) -> bool {
    state.is_none_or(|s| *s.get() == RegionState::Globe)
        && nav.is_none_or(|n| n.phase == NavPhase::Globe)
}

/// One frame's globe input from the held keys and the drag (logical pixels, y down), and the
/// change to the view's drag offset (degrees). The keys mean what they mean in a region:
/// forward, strafe, climb, turn (`→` turns right) and boost; a drag right turns right, a drag
/// down looks further down. A frame longer than `MAX_STEP_S` moves only that far.
pub fn globe_input(held: KeyInput, look_delta: Vec2, dt_s: f64) -> (GlobeInput, f64) {
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
    let input = GlobeInput {
        forward: -m.z,
        strafe: m.x,
        // Positive turns left, as `yaw` does in a region.
        turn_rad: -keys_turn * dt - f64::from(look_delta.x) * px,
        climb: m.y,
        boost: held.boost,
        dt_s: dt,
    };
    (input, (f64::from(look_delta.y) * px).to_degrees())
}

/// The flight `dt_s` further along an exponential ease (time constant [`EASE_TIME_S`]) toward
/// the point over `target` (a direction) at `altitude_km`: along the great circle, carrying the
/// heading.
pub fn ease_flight_toward(
    state: GlobeFlight,
    target: DVec3,
    altitude_km: f64,
    dt_s: f64,
) -> GlobeFlight {
    let k = 1.0 - (-dt_s.max(0.0) / EASE_TIME_S).exp();
    if k <= 0.0 {
        return state;
    }
    let dir = state.dir.normalize();
    let target = target.normalize();
    let angle = dir.angle_between(target);
    let axis = dir
        .cross(target)
        .try_normalize()
        .unwrap_or_else(|| dir.cross(state.heading).normalize());
    let turn = DQuat::from_axis_angle(axis, angle * k);
    let (dir, heading) = if angle > 1e-12 {
        ((turn * dir).normalize(), turn * state.heading)
    } else {
        (dir, state.heading)
    };
    GlobeFlight {
        dir,
        heading: (heading - dir * dir.dot(heading)).normalize(),
        altitude_km: state.altitude_km + (altitude_km - state.altitude_km) * k,
    }
}

/// Gathers the keys and the drag and steps [`GlobeFlightState`].
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn step_globe_flight(
    time: Res<Time>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    motion: Option<Res<AccumulatedMouseMotion>>,
    over_ui: Option<Res<PointerOverUi>>,
    mut drag: ResMut<GlobeDrag>,
    mut flight: ResMut<GlobeFlightState>,
    mut look: ResMut<GlobeLook>,
) {
    if let Some(mouse) = &mouse {
        if mouse.just_pressed(MouseButton::Left) {
            drag.0 = !over_ui.is_some_and(|o| o.0);
        } else if !mouse.pressed(MouseButton::Left) {
            drag.0 = false;
        }
    }
    let held = keys.as_deref().map(keys_to_input).unwrap_or_default();
    let look_delta = match (&motion, drag.0) {
        (Some(m), true) => m.delta,
        _ => Vec2::ZERO,
    };
    let (input, look_change) = globe_input(held, look_delta, time.delta_secs_f64());
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
    // heading about up, the previous heading carried into this frame) and bobbing while idle.
    let dt_s = time.delta_secs();
    let (yaw_rate, idle) = match chase.last {
        Some(last) => {
            let before = carry * last.heading;
            let turned = up.dot(before.cross(f.heading)).atan2(before.dot(f.heading));
            let rate = if dt_s > 0.0 {
                (turned / f64::from(dt_s)) as f32
            } else {
                0.0
            };
            (rate, last == f)
        }
        None => (0.0, true),
    };
    chase.last = Some(f);
    let vfov = match projection {
        Projection::Perspective(p) => p.fov,
        _ => PerspectiveProjection::default().fov,
    };
    let scale = fly_scale((eye - fly).length(), f64::from(vfov)) as f32;
    let (bank_rad, bob) = chase
        .flourish
        .step(yaw_rate, idle, dt_s, time.elapsed_secs(), scale);
    let next = FlyPose {
        translation: (fly + up * f64::from(bob)).as_vec3(),
        rotation: heading_rotation(f.heading, up),
        scale,
        bank_rad,
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
    use timefly_core::flight::globe::SPEED_PER_ALTITUDE;

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
            // Outside the fly's altitude, behind it along the heading.
            assert!(
                eye.length() > EARTH_RADIUS_KM + alt,
                "{alt}: {} km",
                eye.length()
            );
            assert!(
                (eye - f.position_km()).dot(f.heading) < 0.0,
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
        let (i, look) = globe_input(held(Vec3::new(1.0, 1.0, -1.0), 1.0, true), Vec2::ZERO, 0.1);
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
        let (i, look) = globe_input(KeyInput::default(), Vec2::new(10.0, 20.0), 0.1);
        let px = f64::from(LOOK_RADIANS_PER_PX);
        assert!((i.turn_rad + 10.0 * px).abs() < 1e-9);
        assert!((look - (20.0 * px).to_degrees()).abs() < 1e-9);
        // A hitch moves no further than MAX_STEP_S.
        let (i, _) = globe_input(KeyInput::default(), Vec2::ZERO, 3.0);
        assert_eq!(i.dt_s, f64::from(MAX_STEP_S));
        let (i, _) = globe_input(KeyInput::default(), Vec2::ZERO, f64::NAN);
        assert_eq!(i.dt_s, 0.0);
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
        assert!(elevation <= 85.0 + 1e-9, "{elevation}");
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

    #[test]
    fn the_descent_eases_along_the_great_circle_to_the_dive_altitude() {
        let from = at(GLOBE_MAX_ALTITUDE_KM);
        let target = GlobeFlight::start(-10.0, 30.0).dir;
        let one = ease_flight_toward(from, target, DIVE_ALTITUDE_KM, 0.1);
        assert!(one.dir.angle_between(target) < from.dir.angle_between(target));
        assert!(one.altitude_km < from.altitude_km && one.altitude_km > DIVE_ALTITUDE_KM);
        // On the great circle, with the heading still a unit tangent.
        let normal = from.dir.cross(target).normalize();
        assert!(one.dir.dot(normal).abs() < 1e-9);
        assert!((one.heading.length() - 1.0).abs() < 1e-9 && one.heading.dot(one.dir).abs() < 1e-9);
        let mut s = from;
        for _ in 0..100 {
            s = ease_flight_toward(s, target, DIVE_ALTITUDE_KM, 0.1);
        }
        assert!(s.dir.angle_between(target) < 1e-6);
        assert!((s.altitude_km - DIVE_ALTITUDE_KM).abs() < 1e-3);
        assert_eq!(
            ease_flight_toward(from, target, DIVE_ALTITUDE_KM, 0.0),
            from
        );
        // Already there: the heading is kept.
        let there = ease_flight_toward(s, s.dir, DIVE_ALTITUDE_KM, 0.1);
        assert!(there.heading.dot(s.heading) > 1.0 - 1e-12);
    }
}
