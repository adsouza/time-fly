//! The region frame: an azimuthal equidistant projection between local metres and era-frame
//! directions (design spec §5.1).
//!
//! Local axes: `+x` is east, `-z` is north (`+y` is up, not used here). Distances from the centre
//! are exact; the inverse is exact too.

use crate::geo::RenderDir;
use crate::globe::EARTH_RADIUS_KM;
use bevy_math::DVec3;

/// Earth's radius in metres: the sphere the frame projects onto.
pub const EARTH_RADIUS_M: f64 = EARTH_RADIUS_KM * 1000.0;

/// Below this horizontal length of the centre (render X and Z) the centre counts as a pole.
const POLE_EPS: f64 = 1e-12;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RegionFrame {
    /// Unit direction of the region's centre, in the era frame (render axes).
    pub center: RenderDir,
}

impl RegionFrame {
    /// Unit vectors `(north, east)` tangent to the sphere at the centre. At a pole, "north" points
    /// toward longitude 0 (render +Z); east is `north x centre`, so the frame stays right-handed.
    pub fn north_east(&self) -> (DVec3, DVec3) {
        let c = self.center.0.normalize();
        let horizontal = c.x.hypot(c.z);
        if horizontal < POLE_EPS {
            let north = DVec3::Z;
            return (north, north.cross(c).normalize());
        }
        let east = DVec3::Y.cross(c).normalize();
        (c.cross(east).normalize(), east)
    }

    /// The centre and tangent basis, computed once. Generation projects about 1.3 million points
    /// per region, so callers in a loop take the basis once and project through it.
    pub fn basis(&self) -> FrameBasis {
        let (north, east) = self.north_east();
        FrameBasis {
            center: self.center.0.normalize(),
            north,
            east,
        }
    }

    /// Era-frame direction of the point `x_m` metres east and `-z_m` metres north of the centre.
    pub fn local_to_dir(&self, x_m: f64, z_m: f64) -> RenderDir {
        self.basis().local_to_dir(x_m, z_m)
    }

    /// Local `(x, z)` metres of a direction, or `None` at the antipode, where the azimuth is
    /// undefined. Every other direction projects, so the whole sphere is covered.
    pub fn dir_to_local(&self, d: RenderDir) -> Option<(f64, f64)> {
        self.basis().dir_to_local(d)
    }
}

/// A [`RegionFrame`]'s unit centre and its `(north, east)` tangents, as [`RegionFrame::basis`]
/// computes them. Projects exactly as the frame does.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FrameBasis {
    pub center: DVec3,
    pub north: DVec3,
    pub east: DVec3,
}

impl FrameBasis {
    /// See [`RegionFrame::local_to_dir`].
    pub fn local_to_dir(&self, x_m: f64, z_m: f64) -> RenderDir {
        let c = self.center;
        let offset = self.east * x_m - self.north * z_m;
        let dist = offset.length();
        if dist == 0.0 {
            return RenderDir(c);
        }
        let angle = dist / EARTH_RADIUS_M;
        RenderDir((c * angle.cos() + offset * (angle.sin() / dist)).normalize())
    }

    /// See [`RegionFrame::dir_to_local`].
    pub fn dir_to_local(&self, d: RenderDir) -> Option<(f64, f64)> {
        let c = self.center;
        let cos = c.dot(d.0);
        let tangent = d.0 - c * cos;
        let sin = tangent.length();
        if sin < 1e-15 {
            return (cos > 0.0).then_some((0.0, 0.0));
        }
        let angle = sin.atan2(cos);
        let offset = tangent * (angle * EARTH_RADIUS_M / sin);
        Some((offset.dot(self.east), -offset.dot(self.north)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{geo_to_render, latlon_to_geo};
    use crate::worldgen::APRON_SIZE_M;
    use bevy_math::DVec3;
    use proptest::prelude::*;

    fn centre_strategy() -> impl Strategy<Value = (f64, f64)> {
        (
            prop_oneof![-90.0f64..=90.0, Just(90.0), Just(-90.0), Just(0.0)],
            prop_oneof![-180.0f64..=180.0, Just(180.0), Just(-180.0), Just(0.0)],
        )
    }

    fn frame_at(lat: f64, lon: f64) -> RegionFrame {
        RegionFrame {
            center: geo_to_render(latlon_to_geo(lat, lon)),
        }
    }

    proptest! {
        #[test]
        fn frame_round_trips(
            (lat, lon) in centre_strategy(),
            x in -APRON_SIZE_M / 2.0..=APRON_SIZE_M / 2.0,
            z in -APRON_SIZE_M / 2.0..=APRON_SIZE_M / 2.0,
        ) {
            let f = frame_at(lat, lon);
            let d = f.local_to_dir(x, z);
            prop_assert!((d.0.length() - 1.0).abs() < 1e-12);
            let (bx, bz) = f.dir_to_local(d).expect("within the apron");
            prop_assert!((bx - x).abs() < 1e-6 && (bz - z).abs() < 1e-6, "({x},{z}) -> ({bx},{bz})");
            // Distance from the centre is exact (the 1e-6 m floor is the f64 resolution of a
            // unit vector scaled to Earth's radius).
            let dist = x.hypot(z);
            let angle = f.center.0.cross(d.0).length().atan2(f.center.0.dot(d.0));
            let got = angle * EARTH_RADIUS_M;
            prop_assert!((got - dist).abs() <= 1e-9 * dist + 1e-6, "{got} vs {dist}");
        }

        #[test]
        fn minus_z_points_north((lat, lon) in centre_strategy()) {
            let f = frame_at(lat, lon);
            let (n, e) = f.north_east();
            prop_assert!((n.length() - 1.0).abs() < 1e-12 && (e.length() - 1.0).abs() < 1e-12);
            prop_assert!(n.dot(e).abs() < 1e-12 && n.dot(f.center.0).abs() < 1e-12);
            // Right-handed: east = north x centre.
            prop_assert!((n.cross(f.center.0) - e).length() < 1e-12);
            let step = 1000.0;
            let d = f.local_to_dir(0.0, -step);
            let moved = (d.0 - f.center.0) * (EARTH_RADIUS_M / step);
            prop_assert!((moved - n).length() < 1e-3);
            if lat.abs() < 89.999 {
                prop_assert!(n.y > 0.0 || lat.abs() > 89.0, "north has a +Y component");
            }
        }
    }

    #[test]
    fn at_the_equator_axes_are_east_and_north() {
        let (n, e) = frame_at(0.0, 0.0).north_east();
        assert!((e - DVec3::X).length() < 1e-12);
        assert!((n - DVec3::Y).length() < 1e-12);
    }

    #[test]
    fn at_a_pole_north_points_to_longitude_zero() {
        for lat in [90.0, -90.0] {
            let (n, e) = frame_at(lat, 123.0).north_east();
            assert!((n - DVec3::Z).length() < 1e-12, "lat {lat}: {n:?}");
            assert!(e.dot(DVec3::Y).abs() < 1e-12);
        }
    }

    #[test]
    fn the_antipode_has_no_local_position() {
        let f = frame_at(20.0, 30.0);
        assert!(f.dir_to_local(RenderDir(-f.center.0)).is_none());
        assert_eq!(f.dir_to_local(f.center), Some((0.0, 0.0)));
    }
}
