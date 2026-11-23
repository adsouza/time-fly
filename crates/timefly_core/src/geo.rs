//! Coordinate frames shared with the Python bake tool.
//!
//! The geographic ("geo") frame has +X at (0°N, 0°E), +Y at (0°N, 90°E) and +Z at the north pole.
//! The render frame is Y-up: longitude 0 faces +Z, east is +X and north is +Y. Going from geo to
//! render is the cyclic permutation `(x, y, z) -> (y, z, x)`, a proper rotation (no mirroring).
//! The two frames are distinct types so that mixing them is a compile error.

use bevy_math::{DQuat, DVec3};

/// A direction in the geographic frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GeoDir(pub DVec3);

/// A direction in the render (Y-up) frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RenderDir(pub DVec3);

/// Unit direction for a latitude and longitude in degrees.
pub fn latlon_to_geo(lat_deg: f64, lon_deg: f64) -> GeoDir {
    let (lat, lon) = (lat_deg.to_radians(), lon_deg.to_radians());
    GeoDir(DVec3::new(
        lat.cos() * lon.cos(),
        lat.cos() * lon.sin(),
        lat.sin(),
    ))
}

/// Latitude and longitude in degrees; longitude is in (-180, 180] and zero at the poles.
pub fn geo_to_latlon(v: GeoDir) -> (f64, f64) {
    let v = v.0;
    let lat = v.z.clamp(-1.0, 1.0).asin().to_degrees();
    if v.x.hypot(v.y) < 1e-12 {
        return (lat, 0.0);
    }
    let lon = v.y.atan2(v.x).to_degrees();
    (lat, if lon <= -180.0 { 180.0 } else { lon })
}

pub fn geo_to_render(v: GeoDir) -> RenderDir {
    RenderDir(DVec3::new(v.0.y, v.0.z, v.0.x))
}

pub fn render_to_geo(v: RenderDir) -> GeoDir {
    GeoDir(DVec3::new(v.0.z, v.0.x, v.0.y))
}

/// Re-expresses a rotation about geo axes as the same rotation about render axes.
pub fn geo_quat_to_render(q: DQuat) -> DQuat {
    DQuat::from_xyzw(q.y, q.z, q.x, q.w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latlon_frames() {
        assert!((geo_to_render(latlon_to_geo(0.0, 0.0)).0 - DVec3::Z).length() < 1e-12); // lon 0 faces +Z
        assert!((geo_to_render(latlon_to_geo(0.0, 90.0)).0 - DVec3::X).length() < 1e-12); // east is +X
        assert!((geo_to_render(latlon_to_geo(90.0, 0.0)).0 - DVec3::Y).length() < 1e-12); // north is +Y
        let (lat, lon) = geo_to_latlon(latlon_to_geo(-33.9, 151.2));
        assert!((lat + 33.9).abs() < 1e-9 && (lon - 151.2).abs() < 1e-9);
    }

    #[test]
    fn quat_permutation_matches_vector_permutation() {
        let q = DQuat::from_axis_angle(DVec3::new(0.3, -0.5, 0.8).normalize(), 1.1);
        let v = latlon_to_geo(12.0, 34.0);
        assert!(
            (geo_to_render(GeoDir(q * v.0)).0 - geo_quat_to_render(q) * geo_to_render(v).0)
                .length()
                < 1e-12
        );
    }

    #[test]
    fn render_geo_round_trip() {
        let v = latlon_to_geo(-20.0, 70.0);
        assert!((render_to_geo(geo_to_render(v)).0 - v.0).length() < 1e-15);
    }

    #[test]
    fn longitude_is_canonical() {
        assert_eq!(geo_to_latlon(latlon_to_geo(90.0, 123.0)).1, 0.0);
        assert_eq!(geo_to_latlon(latlon_to_geo(-90.0, -45.0)).1, 0.0);
        let (_, lon) = geo_to_latlon(GeoDir(DVec3::new(-1.0, -0.0, 0.0)));
        assert_eq!(lon, 180.0);
        let (_, lon) = geo_to_latlon(latlon_to_geo(0.0, -179.0));
        assert!((lon + 179.0).abs() < 1e-9);
    }
}
