//! The prevailing wind: which way sand dunes run, from latitude alone.
//!
//! Three zones per hemisphere (trades, westerlies, polar easterlies), blended with `smoothstep`
//! across the zone edges. Dunes are painted from one fixed 3D axis per region ([`wind_axis`]):
//! a crest coordinate taken along each pixel's own tangent axes would be identically zero,
//! because the tangent is perpendicular to the position, so the phase is `dot(p, W)` instead.

use super::frame::RegionFrame;
use bevy_math::{DVec2, DVec3};

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The wind's bearing (degrees clockwise from north) the wind blows toward at a latitude.
fn bearing_deg(lat_deg: f64) -> f64 {
    // Zone bearings, south pole to north pole: polar 290, westerlies 110, trades 315 | 225,
    // westerlies 70, polar 250. Each term steps from one zone's bearing to the next.
    290.0
        + (110.0 - 290.0) * smoothstep(-65.0, -55.0, lat_deg)
        + (315.0 - 110.0) * smoothstep(-35.0, -25.0, lat_deg)
        + (225.0 - 315.0) * smoothstep(-5.0, 5.0, lat_deg)
        + (70.0 - 225.0) * smoothstep(25.0, 35.0, lat_deg)
        + (250.0 - 70.0) * smoothstep(55.0, 65.0, lat_deg)
}

/// The unit `(east, north)` direction the prevailing wind blows toward at a latitude: trades
/// (225 deg in the north, 315 in the south) within 30 degrees of the equator, westerlies (70 and
/// 110) to 60, polar winds (250 and 290) beyond. Zone edges blend the bearing linearly with
/// `smoothstep` over +-5 degrees. At the equator the bearing is 270: due west.
pub fn prevailing_wind_toward(lat_deg: f64) -> DVec2 {
    let b = bearing_deg(lat_deg).to_radians();
    DVec2::new(b.sin(), b.cos())
}

/// The wind at a region's centre, as `(local, axis)`.
/// - `local` is the wind in the region's xz plane, `(e, -n)`, since `-z` is north.
/// - `axis` is the 3D unit vector `east_c * e + north_c * n`, tangent to the sphere at the
///   centre.
///
/// The latitude is the centre direction's era-frame latitude.
pub fn wind_axis(frame: &RegionFrame) -> (DVec2, DVec3) {
    let c = frame.center.0.normalize();
    let lat_deg = c.y.clamp(-1.0, 1.0).asin().to_degrees();
    let w = prevailing_wind_toward(lat_deg);
    let (north, east) = frame.north_east();
    (DVec2::new(w.x, -w.y), east * w.x + north * w.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{geo_to_render, latlon_to_geo};

    fn frame_at(lat: f64, lon: f64) -> RegionFrame {
        RegionFrame {
            center: geo_to_render(latlon_to_geo(lat, lon)),
        }
    }

    #[test]
    fn bearings_at_zone_centres() {
        for (lat, b) in [
            (15.0, 225.0f64),
            (45.0, 70.0),
            (75.0, 250.0),
            (-15.0, 315.0),
            (-45.0, 110.0),
            (-75.0, 290.0),
        ] {
            let w = prevailing_wind_toward(lat);
            let want = DVec2::new(b.to_radians().sin(), b.to_radians().cos());
            assert!((w - want).length() < 1e-9, "lat {lat}: {w} vs {want}");
        }
    }

    #[test]
    fn easterly_at_the_equator() {
        let w = prevailing_wind_toward(0.0);
        assert!((w - DVec2::new(-1.0, 0.0)).length() < 1e-12, "{w}");
    }

    #[test]
    fn unit_and_continuous() {
        let mut prev = prevailing_wind_toward(-90.0);
        for i in 0..=18_000 {
            let lat = -90.0 + f64::from(i) * 0.01;
            let w = prevailing_wind_toward(lat);
            assert!((w.length() - 1.0).abs() < 1e-12, "lat {lat}");
            assert!(
                (w - prev).length() < 0.05,
                "lat {lat}: jump {}",
                (w - prev).length()
            );
            prev = w;
        }
    }

    #[test]
    fn wind_axis_is_tangent_and_unit() {
        for (lat, lon) in [(0.0, 0.0), (45.0, 10.0), (89.0, 0.0), (36.1, -179.9)] {
            let f = frame_at(lat, lon);
            let (local, axis) = wind_axis(&f);
            assert!((axis.length() - 1.0).abs() < 1e-12, "({lat},{lon}) len");
            assert!(axis.dot(f.center.0).abs() < 1e-12, "({lat},{lon}) tangent");
            assert!((local.length() - 1.0).abs() < 1e-12);
            // The local wind and the axis agree through the frame: stepping along the local
            // wind moves the position along the axis.
            let step = 1000.0;
            let moved = f.local_to_dir(local.x * step, local.y * step).0 - f.center.0;
            assert!(moved.normalize().dot(axis) > 0.999_999, "({lat},{lon})");
        }
    }
}
