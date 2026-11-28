use std::time::Duration;

use bevy::camera::RenderTargetInfo;
use bevy::time::TimeUpdateStrategy;
use bevy::window::WindowResolution;

use crate::diagnostics::DiagnosticsContext;
use crate::globe::GlobeCamera;
use crate::globe_flight::{GlobeFlightPlugin, GlobeFlightState, GlobeLook};

use super::*;
use TouchPhase::{Canceled, Ended, Moved, Started};

pub(crate) const SIZE: UVec2 = UVec2::new(400, 600);
const STEP: Duration = Duration::from_millis(20);

pub(crate) fn spawn_touch_window(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            Window {
                resolution: WindowResolution::new(SIZE.x, SIZE.y),
                ..default()
            },
            PrimaryWindow,
        ))
        .id()
}

pub(crate) fn send(app: &mut App, window: Entity, events: &[(u64, TouchPhase, Vec2)]) {
    for &(id, phase, position) in events {
        app.world_mut().write_message(TouchInput {
            id,
            phase,
            position,
            window,
            force: None,
        });
    }
    app.update();
}

pub(crate) fn prepare_camera(camera: &mut Camera, projection: &Projection) {
    camera.computed.target_info = Some(RenderTargetInfo {
        physical_size: SIZE,
        scale_factor: 1.0,
    });
    camera.computed.clip_from_view = projection.get_clip_from_view();
}

fn globe_app() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, GlobeFlightPlugin))
        .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
        .init_resource::<DiagnosticsContext>();
    let window = spawn_touch_window(&mut app);
    app.update();
    (app, window)
}

fn flight(app: &App) -> timefly_core::flight::globe::GlobeFlight {
    app.world().resource::<GlobeFlightState>().0
}

#[test]
fn flight_input_touch_controls_the_globe_without_keyboard() {
    let (mut app, window) = globe_app();
    let start = flight(&app);
    let a = Vec2::new(120.0, 320.0);
    let b = Vec2::new(280.0, 320.0);
    send(
        &mut app,
        window,
        &[(0, Started, a), (0, Moved, a + Vec2::splat(20.0))],
    );
    assert_ne!(flight(&app).heading, start.heading);
    assert!(app.world().resource::<GlobeLook>().offset_deg > 0.0);
    assert_eq!(flight(&app).dir, start.dir);

    let a = a + Vec2::splat(20.0);
    send(&mut app, window, &[(1, Started, b)]);
    let before = flight(&app);
    let down = Vec2::new(0.0, 100.0);
    send(
        &mut app,
        window,
        &[(0, Moved, a + down), (1, Moved, b + down)],
    );
    let descended = flight(&app);
    assert!(descended.altitude_km < before.altitude_km);
    assert!(
        descended.dir.distance(before.dir) < 1e-9,
        "parallel drag must not pinch"
    );

    let a = a + down;
    let b = b + down;
    let apart = Vec2::new(30.0, 0.0);
    send(
        &mut app,
        window,
        &[(0, Moved, a - apart), (1, Moved, b + apart)],
    );
    let forward = flight(&app);
    assert!((forward.dir - descended.dir).dot(descended.heading) > 0.0);
    send(&mut app, window, &[(0, Moved, a), (1, Moved, b)]);
    let reversed = flight(&app);
    assert!((reversed.dir - forward.dir).dot(forward.heading) < 0.0);
    assert_eq!(reversed.altitude_km, descended.altitude_km);

    let look = app.world().resource::<GlobeLook>().offset_deg;
    send(&mut app, window, &[(1, Ended, b)]);
    assert_eq!(app.world().resource::<GlobeLook>().offset_deg, look);
    assert_eq!(flight(&app), reversed);
}

#[test]
fn flight_input_keeps_ui_contacts_out_of_scene_gestures() {
    let (mut app, window) = globe_app();
    let center = Vec2::new(200.0, 300.0);
    let root = app.world_mut().spawn((BlocksCamera, Pickable::IGNORE)).id();
    app.world_mut().spawn((
        ComputedNode {
            size: Vec2::splat(40.0),
            ..default()
        },
        UiGlobalTransform::from_translation(center),
        InheritedVisibility::VISIBLE,
        ChildOf(root),
    ));
    let start = flight(&app);
    let shifted = center + Vec2::splat(100.0);
    send(
        &mut app,
        window,
        &[
            (0, Started, center),
            (0, Moved, shifted),
            (1, Started, shifted + Vec2::X * 50.0),
            (0, Ended, shifted),
            (1, Moved, center),
            (1, Canceled, center),
        ],
    );
    assert_eq!(flight(&app), start);
    assert_eq!(app.world().resource::<GlobeLook>().offset_deg, 0.0);
    send(
        &mut app,
        window,
        &[(2, Started, shifted), (2, Moved, center)],
    );
    assert_ne!(flight(&app).heading, start.heading);
}

#[test]
fn flight_input_double_tap_boosts_to_a_globe_spot_and_stops() {
    let (mut app, window) = globe_app();
    let start = flight(&app);
    let radius = timefly_core::globe::EARTH_RADIUS_KM;
    let turn = 0.2_f64;
    let target = start.dir * turn.cos() + start.heading * turn.sin();
    let camera_pose = Transform::from_translation((target * (radius + 2_000.0)).as_vec3())
        .looking_at((target * radius).as_vec3(), Vec3::Y);
    let projection = Projection::Perspective(default());
    let mut camera = Camera::default();
    prepare_camera(&mut camera, &projection);
    app.world_mut().spawn((
        GlobeCamera,
        camera,
        projection,
        camera_pose,
        GlobalTransform::from(camera_pose),
    ));

    let center = SIZE.as_vec2() * 0.5;
    send(
        &mut app,
        window,
        &[(0, Started, center), (0, Ended, center)],
    );
    assert_eq!(flight(&app), start);
    send(
        &mut app,
        window,
        &[(1, Started, center), (1, Ended, center)],
    );
    assert!(app.world().resource::<FlightIntent>().movement.boost);
    assert!(flight(&app).dir.distance(start.dir) > 0.0);

    // A fresh press cancels the route even before the first drag delta.
    let before_cancel = flight(&app);
    send(&mut app, window, &[(2, Started, center)]);
    app.update();
    assert_eq!(flight(&app), before_cancel);
    send(&mut app, window, &[(2, Canceled, center)]);

    send(
        &mut app,
        window,
        &[(3, Started, center), (3, Ended, center)],
    );
    assert_eq!(
        flight(&app),
        before_cancel,
        "canceled touches cannot form a double tap"
    );
    send(
        &mut app,
        window,
        &[(4, Started, center), (4, Ended, center)],
    );
    for _ in 0..60 {
        app.update();
    }
    let arrived = flight(&app);
    assert!(arrived.dir.distance(target) < 1e-5, "{arrived:?}");
    assert_eq!(start.altitude_km, arrived.altitude_km);
    app.update();
    assert_eq!(flight(&app), arrived);
}
