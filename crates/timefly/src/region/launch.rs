//! `--region=LAT,LON,AGE_YEARS` (web: `?region=…`): enter a region at launch, once the globe
//! is interactive ([`GlobeRenderStatus::Interactive`]), by sending the same [`RegionRequested`] a click on the globe will. The
//! latitude and longitude are in the era's frame (where the globe draws that place at that age).
//! The age is used as given, without snapping to a snapshot, so any age can be inspected; the
//! globe's time follows it, so the slider shows the region's age.

use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};

use super::{RegionRequested, RequestPurpose};
use crate::args::RegionArg;
use crate::globe::GlobeRenderStatus;
use crate::time::GlobeTime;

/// The launch region, inserted by `crate::app` when `--region` was given.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct LaunchRegion(pub RegionArg);

/// The request for a launch region.
pub fn launch_request(arg: &RegionArg) -> RegionRequested {
    RegionRequested {
        era_dir: geo_to_render(latlon_to_geo(arg.lat_deg, arg.lon_deg)),
        snapshot: YearsBp(arg.years),
        purpose: RequestPurpose::Enter,
    }
}

/// Registers [`request_launch_region`].
pub(super) fn add_launch_systems(app: &mut App) {
    app.add_systems(
        Update,
        request_launch_region
            .run_if(resource_exists::<LaunchRegion>)
            .run_if(globe_interactive),
    );
}

/// Whether the globe is interactive: its data loaded and its shaders compiled (on the web they
/// compile first), as the region bench waits for, so a launch region never starts under the
/// loading screen.
fn globe_interactive(status: Option<Res<GlobeRenderStatus>>) -> bool {
    matches!(
        status.as_deref(),
        Some(GlobeRenderStatus::Interactive { .. })
    )
}

/// Sends the launch region's request, once: the resource is removed, so the launch region is
/// never requested again.
pub(super) fn request_launch_region(
    launch: Res<LaunchRegion>,
    mut commands: Commands,
    mut time: ResMut<GlobeTime>,
    mut requests: MessageWriter<RegionRequested>,
) {
    commands.remove_resource::<LaunchRegion>();
    let request = launch_request(&launch.0);
    info!(
        lat_deg = launch.0.lat_deg,
        lon_deg = launch.0.lon_deg,
        years = launch.0.years,
        "entering the launch region"
    );
    time.years = request.snapshot;
    requests.write(request);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_request_waits_for_the_interactive_globe_and_is_sent_once() {
        #[derive(Resource, Default)]
        struct Sent(usize);
        let mut app = App::new();
        app.add_message::<RegionRequested>()
            .init_resource::<GlobeTime>()
            .init_resource::<Sent>()
            .insert_resource(GlobeRenderStatus::LoadingData)
            .add_systems(
                Last,
                |mut sent: ResMut<Sent>, mut r: MessageReader<RegionRequested>| {
                    sent.0 += r.read().count();
                },
            )
            .insert_resource(LaunchRegion(RegionArg {
                lat_deg: 46.5,
                lon_deg: 10.0,
                years: 21_000.0,
            }));
        add_launch_systems(&mut app);
        // Not while the data loads or the globe's pipelines compile (the region bench's rule).
        for status in [
            GlobeRenderStatus::LoadingData,
            GlobeRenderStatus::PreparingGpu,
        ] {
            *app.world_mut().resource_mut::<GlobeRenderStatus>() = status;
            app.update();
            app.update();
        }
        assert_eq!(app.world().resource::<Sent>().0, 0);
        assert!(app.world().get_resource::<LaunchRegion>().is_some());
        assert_eq!(app.world().resource::<GlobeTime>().years, YearsBp(0.0));
        // Once interactive: sent once.
        *app.world_mut().resource_mut::<GlobeRenderStatus>() =
            GlobeRenderStatus::Interactive { first_frame: 5 };
        app.update();
        app.update();
        app.update();
        assert_eq!(app.world().resource::<Sent>().0, 1);
        assert!(app.world().get_resource::<LaunchRegion>().is_none());
        assert_eq!(app.world().resource::<GlobeTime>().years, YearsBp(21_000.0));
    }

    #[test]
    fn a_failed_globe_never_launches_a_region() {
        let mut app = App::new();
        app.add_message::<RegionRequested>()
            .init_resource::<GlobeTime>()
            .insert_resource(GlobeRenderStatus::Failed("no GPU".into()))
            .insert_resource(LaunchRegion(RegionArg {
                lat_deg: 46.5,
                lon_deg: 10.0,
                years: 21_000.0,
            }));
        add_launch_systems(&mut app);
        app.update();
        app.update();
        assert!(app.world().get_resource::<LaunchRegion>().is_some());
    }

    #[test]
    fn launch_request_points_at_the_era_frame_place() {
        let arg = RegionArg {
            lat_deg: 46.5,
            lon_deg: 10.0,
            years: 21_000.0,
        };
        let r = launch_request(&arg);
        assert_eq!(r.snapshot, YearsBp(21_000.0));
        assert_eq!(r.era_dir, geo_to_render(latlon_to_geo(46.5, 10.0)));
        assert!((r.era_dir.0.length() - 1.0).abs() < 1e-12);
    }
}
