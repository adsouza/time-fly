//! The fly’s altitude above sea level, sampled twice a second (M3 design §5.3).
//!
//! Whichever world shows the fly writes [`FlyAltitude`] in its own units (the region in metres;
//! the globe in kilometres once it flies the fly, Task 4). Until the globe writes it, an active
//! globe camera's own altitude stands in.

use std::time::Duration;

use bevy::prelude::*;

use super::{GUTTER_PX, TEXT_COLOR};
use crate::globe::{EARTH_RADIUS_KM, GlobeCamera};

const REFRESH: Duration = Duration::from_millis(500);

/// The fly's altitude above sea level, in the active world's units.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Altitude {
    /// In a region (local metres above sea level).
    Metres(f32),
    /// On the globe.
    Kilometres(f64),
}

/// The fly's altitude for the overlay, written by whichever world shows the fly; `None` when
/// neither does.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
pub struct FlyAltitude(pub Option<Altitude>);

#[derive(Component)]
struct AltitudeText;

pub(super) struct AltitudeOverlayPlugin;

impl Plugin for AltitudeOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlyAltitude>()
            .add_systems(Startup, spawn_altitude)
            .add_systems(PostUpdate, update_altitude);
    }
}

fn spawn_altitude(mut commands: Commands) {
    commands.spawn((
        Name::new("Altitude readout"),
        AltitudeText,
        Text::default(),
        TextFont::from_font_size(14.0),
        TextColor(TEXT_COLOR.with_alpha(0.75)),
        Node {
            position_type: PositionType::Absolute,
            top: px(GUTTER_PX),
            left: px(GUTTER_PX),
            ..default()
        },
        Visibility::Hidden,
        Pickable::IGNORE,
    ));
}

/// The readout's text for an altitude.
pub fn altitude_label(altitude: Altitude) -> String {
    match altitude {
        Altitude::Metres(m) => format!("Altitude {m:.0} m"),
        Altitude::Kilometres(km) => format!("Altitude {km:.0} km"),
    }
}

fn update_altitude(
    fly: Res<FlyAltitude>,
    globe: Query<(&Camera, &Transform), With<GlobeCamera>>,
    time: Res<Time<Real>>,
    mut next_refresh: Local<Duration>,
    mut readout: Single<(&mut Text, &mut Visibility), With<AltitudeText>>,
) {
    let (text, visibility) = &mut *readout;
    let now = time.elapsed();
    if now < *next_refresh {
        return;
    }
    *next_refresh = now + REFRESH;
    let label = if let Some(altitude) = fly.0 {
        altitude_label(altitude)
    } else if let Some((_, pose)) = globe.iter().find(|(camera, _)| camera.is_active) {
        let altitude_km = pose.translation.as_dvec3().length() - EARTH_RADIUS_KM;
        altitude_label(Altitude::Kilometres(altitude_km))
    } else {
        visibility.set_if_neq(Visibility::Hidden);
        return;
    };
    text.set_if_neq(Text::new(label));
    visibility.set_if_neq(Visibility::Visible);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;

    fn overlay_app() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AltitudeOverlayPlugin))
            .init_resource::<FlyAltitude>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                250,
            )));
        app.update();
        let world = app.world_mut();
        let entity = world
            .query_filtered::<Entity, With<AltitudeText>>()
            .single(world)
            .unwrap();
        (app, entity)
    }

    fn text(app: &App, entity: Entity) -> String {
        app.world().get::<Text>(entity).unwrap().0.clone()
    }

    fn visible(app: &App, entity: Entity) -> Visibility {
        *app.world().get::<Visibility>(entity).unwrap()
    }

    fn set(app: &mut App, altitude: Option<Altitude>) {
        app.world_mut().resource_mut::<FlyAltitude>().0 = altitude;
    }

    #[test]
    fn the_overlay_shows_the_flys_altitude() {
        let (mut app, entity) = overlay_app();
        // Nothing shows the fly and no camera is active: hidden.
        assert_eq!(visible(&app, entity), Visibility::Hidden);
        // The region's fly, sampled every half second.
        set(&mut app, Some(Altitude::Metres(8_100.6)));
        app.update();
        app.update();
        assert_eq!(visible(&app, entity), Visibility::Visible);
        assert_eq!(text(&app, entity), "Altitude 8101 m");
        set(&mut app, Some(Altitude::Metres(8_200.0)));
        app.update();
        assert_eq!(text(&app, entity), "Altitude 8101 m");
        app.update();
        assert_eq!(text(&app, entity), "Altitude 8200 m");
        // The globe's fly, in kilometres.
        set(&mut app, Some(Altitude::Kilometres(1_234.4)));
        app.update();
        app.update();
        assert_eq!(text(&app, entity), "Altitude 1234 km");
        // The fly's altitude wins over a camera's: an active region camera far from the fly is
        // not what the overlay reads.
        app.world_mut().spawn((
            crate::terrain::view::RegionCamera,
            Camera::default(),
            Transform::from_xyz(0.0, 99_999.0, 0.0),
        ));
        set(&mut app, Some(Altitude::Metres(150.0)));
        app.update();
        app.update();
        assert_eq!(text(&app, entity), "Altitude 150 m");
        // Neither world claims it: hidden again.
        set(&mut app, None);
        app.update();
        app.update();
        assert_eq!(visible(&app, entity), Visibility::Hidden);
    }

    #[test]
    fn without_a_fly_the_globe_camera_still_gives_an_altitude() {
        let (mut app, entity) = overlay_app();
        let globe = app
            .world_mut()
            .spawn((
                GlobeCamera,
                Camera::default(),
                Transform::from_xyz(0.0, 0.0, EARTH_RADIUS_KM as f32 + 2000.0),
            ))
            .id();
        app.update();
        app.update();
        assert_eq!(visible(&app, entity), Visibility::Visible);
        assert_eq!(text(&app, entity), "Altitude 2000 km");
        // An inactive globe camera gives nothing.
        app.world_mut().get_mut::<Camera>(globe).unwrap().is_active = false;
        app.update();
        app.update();
        assert_eq!(visible(&app, entity), Visibility::Hidden);
    }
}
