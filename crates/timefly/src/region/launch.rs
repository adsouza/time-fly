//! `--region=LAT,LON,AGE_YEARS` (web: `?region=…`): enter a region at launch, once the globe
//! data is ready, by sending the same [`RegionRequested`] a click on the globe will. The
//! latitude and longitude are in the era's frame (where the globe draws that place at that age).
//! The age is used as given, without snapping to a snapshot, so any age can be inspected; the
//! globe's time follows it, so the slider shows the region's age.

use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};

use super::RegionRequested;
use crate::args::RegionArg;
use crate::time::GlobeTime;

/// The launch region, inserted by `crate::app` when `--region` was given.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct LaunchRegion(pub RegionArg);

/// The request for a launch region.
pub fn launch_request(arg: &RegionArg) -> RegionRequested {
    RegionRequested {
        era_dir: geo_to_render(latlon_to_geo(arg.lat_deg, arg.lon_deg)),
        snapshot: YearsBp(arg.years),
    }
}

/// Sends the launch region's request (on entering `GlobeLoadState::Ready`).
pub(super) fn request_launch_region(
    launch: Res<LaunchRegion>,
    mut time: ResMut<GlobeTime>,
    mut requests: MessageWriter<RegionRequested>,
) {
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
