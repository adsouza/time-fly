//! Flight over the whole sphere: a pure f64 step on the unit sphere in the render frame.

use bevy_math::DVec3;

use super::altitudes::{
    DIVE_ALTITUDE_KM, GLOBE_MAX_ALTITUDE_KM, GLOBE_MAX_TURN_DEG_PER_S, GLOBE_START_ALTITUDE_KM,
};
use crate::geo::{geo_to_render, latlon_to_geo};
use crate::globe::EARTH_RADIUS_KM;

/// Speed in altitudes per second, as in the regions (`SPEED_PER_M_AGL`).
pub const SPEED_PER_ALTITUDE: f64 = 0.8;
/// Boost multiplier, as in the regions.
pub const BOOST: f64 = 4.0;
/// The lowest altitude: half the dive altitude, inside the cloud, which the shell eases.
pub const GLOBE_MIN_ALTITUDE_KM: f64 = DIVE_ALTITUDE_KM * 0.5;

/// The fly over the globe. `dir` is a unit position and `heading` a unit tangent at it, both in
/// the render frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GlobeFlight {
    pub dir: DVec3,
    pub heading: DVec3,
    pub altitude_km: f64,
}

impl GlobeFlight {
    /// At a latitude and longitude in degrees, at the start altitude, heading north.
    pub fn start(lat_deg: f64, lon_deg: f64) -> Self {
        let dir = geo_to_render(latlon_to_geo(lat_deg, lon_deg)).0.normalize();
        // North is +Y projected onto the tangent plane; at a pole fall back to longitude 0.
        let mut north = DVec3::Y - dir * dir.y;
        if north.length() < 1e-9 {
            north = DVec3::Z - dir * dir.z;
        }
        GlobeFlight {
            dir,
            heading: north.normalize(),
            altitude_km: GLOBE_START_ALTITUDE_KM,
        }
    }

    /// Position from the planet's centre, in kilometres.
    pub fn position_km(&self) -> DVec3 {
        self.dir * (EARTH_RADIUS_KM + self.altitude_km)
    }

    /// Local up: the unit direction away from the centre.
    pub fn up(&self) -> DVec3 {
        self.dir
    }
}

/// One frame of input. Axes are in `-1..=1`; `turn_rad` already includes drag and is positive to
/// the left, as `yaw` is in the regions.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GlobeInput {
    pub forward: f64,
    pub strafe: f64,
    pub turn_rad: f64,
    pub climb: f64,
    pub boost: bool,
    pub dt_s: f64,
}

/// Rotates `v` about the unit `axis` by `angle` (Rodrigues).
fn rotate(v: DVec3, axis: DVec3, angle: f64) -> DVec3 {
    let (s, c) = angle.sin_cos();
    v * c + axis.cross(v) * s + axis * (axis.dot(v) * (1.0 - c))
}

/// Advances the fly one frame: turn about up, then travel along the great circle toward the
/// input direction, carrying the heading with it and re-orthonormalising it.
/// The globe's flight speed (km/s) at `altitude_km`, along the ground and climbing alike:
/// [`SPEED_PER_ALTITUDE`] × the altitude (× [`BOOST`] when boosted), with the ground track's
/// angular speed capped at `GLOBE_MAX_TURN_DEG_PER_S`.
pub fn globe_speed_km_s(altitude_km: f64, boost: bool) -> f64 {
    let speed = SPEED_PER_ALTITUDE * altitude_km * if boost { BOOST } else { 1.0 };
    speed.min(GLOBE_MAX_TURN_DEG_PER_S.to_radians() * (EARTH_RADIUS_KM + altitude_km))
}

pub fn globe_fly_step(state: GlobeFlight, input: GlobeInput) -> GlobeFlight {
    let dir = state.dir.normalize();
    let up = dir;
    let turned = rotate(state.heading, up, input.turn_rad);
    let heading = (turned - up * up.dot(turned)).normalize();
    let right = heading.cross(up);

    let speed = globe_speed_km_s(state.altitude_km, input.boost);
    let radius = EARTH_RADIUS_KM + state.altitude_km;

    let mut wish = heading * input.forward + right * input.strafe;
    if wish.length() > 1.0 {
        wish = wish.normalize();
    }
    let mut out = GlobeFlight {
        dir,
        heading,
        altitude_km: state.altitude_km,
    };
    let amount = wish.length();
    if amount > 1e-12 && input.dt_s > 0.0 {
        let along = wish / amount;
        let angle = amount * speed * input.dt_s / radius;
        let axis = dir.cross(along).normalize();
        let new_dir = rotate(dir, axis, angle).normalize();
        let carried = rotate(heading, axis, angle);
        let new_heading = carried - new_dir * new_dir.dot(carried);
        out.dir = new_dir;
        out.heading = new_heading.normalize();
    }
    out.altitude_km = (state.altitude_km + input.climb * speed * input.dt_s)
        .clamp(GLOBE_MIN_ALTITUDE_KM, GLOBE_MAX_ALTITUDE_KM);
    out
}

/// The region `yaw` of a heading at `dir`: 0 faces north, positive turns west.
pub fn heading_to_region_yaw(dir: DVec3, heading: DVec3, north: DVec3, east: DVec3) -> f32 {
    let h = heading - dir * dir.dot(heading);
    (-h.dot(east)).atan2(h.dot(north)) as f32
}

/// The tangent heading for a region `yaw`; the inverse of [`heading_to_region_yaw`].
pub fn region_yaw_to_heading(yaw: f32, north: DVec3, east: DVec3) -> DVec3 {
    let yaw = f64::from(yaw);
    (north * yaw.cos() - east * yaw.sin()).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{RenderDir, geo_to_latlon, render_to_geo};
    use crate::worldgen::frame::RegionFrame;
    use proptest::prelude::*;

    const DT: f64 = 1.0 / 60.0;

    fn lat(s: &GlobeFlight) -> f64 {
        geo_to_latlon(render_to_geo(RenderDir(s.dir))).0
    }

    fn fwd(dt_s: f64, boost: bool) -> GlobeInput {
        GlobeInput {
            forward: 1.0,
            strafe: 0.0,
            turn_rad: 0.0,
            climb: 0.0,
            boost,
            dt_s,
        }
    }

    #[test]
    fn crosses_the_pole_without_a_flip() {
        let mut s = GlobeFlight::start(89.0, 10.0);
        let mut max_lat = lat(&s);
        let mut prev = s.heading;
        for _ in 0..200 {
            s = globe_fly_step(s, fwd(DT, false));
            assert!(s.heading.dot(prev) > 0.99, "heading flipped");
            assert!((s.dir.length() - 1.0).abs() < 1e-9);
            assert!(s.heading.dot(s.dir).abs() < 1e-9);
            prev = s.heading;
            max_lat = max_lat.max(lat(&s));
        }
        // Sampling every ~0.43 degrees, the peak is within half a step of the pole.
        assert!(max_lat > 89.5, "never reached the pole: {max_lat}");
        assert!(lat(&s) < max_lat - 1.0, "did not descend after the pole");
    }

    #[test]
    fn forward_moves_along_the_heading_and_keeps_altitude() {
        let s0 = GlobeFlight::start(10.0, 20.0);
        let s1 = globe_fly_step(s0, fwd(DT, false));
        let step = s1.dir - s0.dir;
        assert!(step.length() > 0.0);
        assert!(step.normalize().dot(s0.heading) > 0.999);
        assert_eq!(s1.altitude_km, s0.altitude_km);
        assert!(lat(&s1) > lat(&s0), "north heading gains latitude");
    }

    #[test]
    fn the_ground_track_turns_at_most_thirty_degrees_a_second() {
        let mut s = GlobeFlight::start(0.0, 0.0);
        s.altitude_km = GLOBE_MAX_ALTITUDE_KM;
        let start = s.dir;
        for _ in 0..60 {
            s = globe_fly_step(s, fwd(DT, true));
        }
        let angle = start.dot(s.dir).clamp(-1.0, 1.0).acos().to_degrees();
        assert!(angle <= 30.0 + 1e-6, "{angle}");
        assert!(angle > 29.9, "the cap should bind here: {angle}");
    }

    #[test]
    fn altitude_speed_scales_with_altitude() {
        let travelled = |alt: f64| {
            let mut s = GlobeFlight::start(0.0, 0.0);
            s.altitude_km = alt;
            let s1 = globe_fly_step(s, fwd(0.01, false));
            s1.dir.dot(s.dir).acos() * (EARTH_RADIUS_KM + alt)
        };
        let (lo, hi) = (2_000.0, 4_000.0);
        assert!((travelled(lo) - 0.8 * lo * 0.01).abs() < 1e-6);
        assert!((travelled(hi) / travelled(lo) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn altitude_is_clamped() {
        let mut s = GlobeFlight::start(0.0, 0.0);
        let up = GlobeInput {
            forward: 0.0,
            strafe: 0.0,
            turn_rad: 0.0,
            climb: 1.0,
            boost: false,
            dt_s: 1.0,
        };
        assert_eq!(globe_fly_step(s, up).altitude_km, GLOBE_MAX_ALTITUDE_KM);
        s.altitude_km = GLOBE_MIN_ALTITUDE_KM + 1.0;
        let down = GlobeInput { climb: -1.0, ..up };
        assert_eq!(globe_fly_step(s, down).altitude_km, GLOBE_MIN_ALTITUDE_KM);
    }

    #[test]
    fn turning_left_turns_west() {
        let s = GlobeFlight::start(0.0, 0.0);
        let input = GlobeInput {
            forward: 0.0,
            strafe: 0.0,
            turn_rad: 0.1,
            climb: 0.0,
            boost: false,
            dt_s: DT,
        };
        let t = globe_fly_step(s, input);
        let east = RegionFrame {
            center: RenderDir(s.dir),
        }
        .basis()
        .east;
        assert!(t.heading.dot(east) < 0.0);
    }

    proptest! {
        #[test]
        fn heading_and_yaw_round_trip(
            seeds in proptest::collection::vec((-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0, -3.1f64..3.1), 32)
        ) {
            for (x, y, z, a) in seeds {
                let v = DVec3::new(x, y, z);
                prop_assume!(v.length() > 0.1);
                let dir = v.normalize();
                let basis = RegionFrame { center: RenderDir(dir) }.basis();
                // A random tangent heading.
                let heading = (basis.north * a.cos() - basis.east * a.sin()).normalize();
                let yaw = heading_to_region_yaw(dir, heading, basis.north, basis.east);
                let back = region_yaw_to_heading(yaw, basis.north, basis.east);
                prop_assert!(back.angle_between(heading) < 1e-6);
            }
        }
    }
}
