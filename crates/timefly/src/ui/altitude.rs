//! The active camera’s altitude above sea level, sampled twice a second.

use std::time::Duration;

use bevy::prelude::*;

use super::{GUTTER_PX, TEXT_COLOR};
use crate::globe::{EARTH_RADIUS_KM, GlobeCamera};
use crate::terrain::view::RegionCamera;

const REFRESH: Duration = Duration::from_millis(500);

#[derive(Component)]
struct AltitudeText;

pub(super) struct AltitudeOverlayPlugin;

impl Plugin for AltitudeOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_altitude)
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

fn update_altitude(
    globe: Query<(&Camera, &Transform), With<GlobeCamera>>,
    region: Query<(&Camera, &Transform), With<RegionCamera>>,
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
    let label = if let Some((_, pose)) = region.iter().find(|(camera, _)| camera.is_active) {
        format!("Altitude {:.0} m", pose.translation.y)
    } else if let Some((_, pose)) = globe.iter().find(|(camera, _)| camera.is_active) {
        let altitude_km = pose.translation.as_dvec3().length() - EARTH_RADIUS_KM;
        format!("Altitude {altitude_km:.0} km")
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

    #[test]
    fn altitude_samples_the_active_camera_every_half_second() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AltitudeOverlayPlugin))
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                250,
            )));
        let globe = app
            .world_mut()
            .spawn((
                GlobeCamera,
                Camera::default(),
                Transform::from_xyz(0.0, 0.0, EARTH_RADIUS_KM as f32 + 2000.0),
            ))
            .id();
        let region = app
            .world_mut()
            .spawn((
                RegionCamera,
                Camera {
                    is_active: false,
                    ..default()
                },
                Transform::from_xyz(200.0, 8100.6, -500.0),
            ))
            .id();
        app.update();
        let world = app.world_mut();
        let entity = world
            .query_filtered::<Entity, With<AltitudeText>>()
            .single(world)
            .unwrap();
        let text = |app: &App| app.world().get::<Text>(entity).unwrap().0.clone();
        let visible = |app: &App| *app.world().get::<Visibility>(entity).unwrap();
        assert_eq!(visible(&app), Visibility::Visible);
        assert_eq!(text(&app), "Altitude 2000 km");
        app.world_mut().get_mut::<Camera>(globe).unwrap().is_active = false;
        app.world_mut().get_mut::<Camera>(region).unwrap().is_active = true;
        app.update();
        assert_eq!(text(&app), "Altitude 2000 km");
        app.update();
        assert_eq!(text(&app), "Altitude 8101 m");
        app.world_mut()
            .get_mut::<Transform>(region)
            .unwrap()
            .translation
            .y = 8200.0;
        app.update();
        assert_eq!(text(&app), "Altitude 8101 m");
        app.update();
        assert_eq!(text(&app), "Altitude 8200 m");
    }
}
