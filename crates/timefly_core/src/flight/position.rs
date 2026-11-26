//! The fly's place and heading for the HUD and the bug shots (M3 Task 12): latitudes and
//! longitudes with hemispheres (and their inverse), the true compass bearing of a heading on
//! the sphere and in a region, and where the heading-up compass rose draws each direction.
//!
//! Bearings are degrees clockwise from true north in `[0, 360)`. At a pole, where north is
//! undefined, "north" is toward longitude 0 (render `+Z`), as [`RegionFrame::north_east`] and
//! the globe flight's start take it, so a bearing is always finite.
//!
//! [`RegionFrame::north_east`]: crate::worldgen::frame::RegionFrame::north_east

use std::fmt::Write;

use bevy_math::DVec3;

use crate::worldgen::frame::FrameBasis;

/// Below this horizontal length of a direction (render X and Z) it counts as a pole.
const POLE_EPS: f64 = 1e-12;

/// The magnitude of `value` rounded to `decimals`, and its hemisphere letter: `pos` for
/// positive values and for anything that rounds to zero, `neg` otherwise.
fn hemisphere(value: f64, decimals: usize, pos: char, neg: char) -> (f64, char) {
    let scale = 10f64.powi(decimals as i32);
    let magnitude = (value.abs() * scale).round() / scale;
    (
        magnitude,
        if value < 0.0 && magnitude != 0.0 {
            neg
        } else {
            pos
        },
    )
}

/// Writes one coordinate as `22.870N` (`spaced`: `22.87° N`).
fn write_coordinate(
    out: &mut String,
    value: f64,
    decimals: usize,
    (pos, neg): (char, char),
    spaced: bool,
) {
    let (magnitude, letter) = hemisphere(value, decimals, pos, neg);
    // Writing to a String cannot fail.
    let _ = if spaced {
        write!(out, "{magnitude:.decimals$}° {letter}")
    } else {
        write!(out, "{magnitude:.decimals$}{letter}")
    };
}

/// Appends the HUD's readout, `22.87° N  71.42° E`, with `decimals` places, to `out` (which is
/// not cleared, so a caller can reuse its buffer without allocating).
pub fn write_lat_lon(out: &mut String, lat_deg: f64, lon_deg: f64, decimals: usize) {
    write_coordinate(out, lat_deg, decimals, ('N', 'S'), true);
    out.push_str("  ");
    write_coordinate(out, lon_deg, decimals, ('E', 'W'), true);
}

/// The HUD's readout, `22.87° N  71.42° E`, with `decimals` places.
pub fn format_lat_lon(lat_deg: f64, lon_deg: f64, decimals: usize) -> String {
    let mut out = String::new();
    write_lat_lon(&mut out, lat_deg, lon_deg, decimals);
    out
}

/// A latitude as a file name writes it, `22.870N` (`decimals` places).
pub fn compact_latitude(lat_deg: f64, decimals: usize) -> String {
    let mut out = String::new();
    write_coordinate(&mut out, lat_deg, decimals, ('N', 'S'), false);
    out
}

/// A longitude as a file name writes it, `71.420E` (`decimals` places).
pub fn compact_longitude(lon_deg: f64, decimals: usize) -> String {
    let mut out = String::new();
    write_coordinate(&mut out, lon_deg, decimals, ('E', 'W'), false);
    out
}

/// Parses one coordinate, `22.87° N`, `22.87°N` or `22.870N`, given its two letters; `None`
/// for anything else (another letter, a sign, no number).
pub fn parse_coordinate(text: &str, (pos, neg): (char, char)) -> Option<f64> {
    let text = text.trim();
    let letter = text.chars().last()?;
    let sign = match letter {
        l if l == pos => 1.0,
        l if l == neg => -1.0,
        _ => return None,
    };
    let number = text[..text.len() - letter.len_utf8()]
        .trim_end()
        .trim_end_matches('°');
    if number.starts_with(['-', '+']) {
        return None;
    }
    let value = number.parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some(sign * value)
}

/// Parses the HUD's readout (`22.87° N  71.42° E`) back to degrees.
pub fn parse_lat_lon(text: &str) -> Option<(f64, f64)> {
    let (lat, lon) = text.trim().split_once("  ")?;
    Some((
        parse_coordinate(lat, ('N', 'S'))?,
        parse_coordinate(lon, ('E', 'W'))?,
    ))
}

/// Unit `(north, east)` tangents at the unit render-frame direction `dir` (north toward
/// longitude 0 at a pole, as the region frame takes it).
fn north_east(dir: DVec3) -> (DVec3, DVec3) {
    let c = dir.normalize();
    if c.x.hypot(c.z) < POLE_EPS {
        let north = DVec3::Z;
        return (north, north.cross(c).normalize());
    }
    let east = DVec3::Y.cross(c).normalize();
    (c.cross(east).normalize(), east)
}

/// The true compass bearing (degrees clockwise from north, in `[0, 360)`) of `heading` at
/// `dir`, both in the render frame (`heading` need not be tangent or unit). A heading with no
/// tangent part bears north.
pub fn compass_bearing_deg(dir: DVec3, heading: DVec3) -> f64 {
    let up = dir.normalize();
    let tangent = heading - up * up.dot(heading);
    let (north, east) = north_east(up);
    let bearing = tangent.dot(east).atan2(tangent.dot(north)).to_degrees();
    let bearing = bearing.rem_euclid(360.0);
    // `rem_euclid` of a tiny negative value is 360 itself in floating point.
    if bearing >= 360.0 { 0.0 } else { bearing }
}

/// The true compass bearing of a region fly at local `(x_m, z_m)` with region `yaw_rad`
/// (0 faces the frame's north, positive turns west): the frame's north is true north only at
/// its centre, so the heading is carried onto the sphere at the fly's own place.
pub fn region_bearing_deg(frame: &FrameBasis, x_m: f64, z_m: f64, yaw_rad: f64) -> f64 {
    // The fly's forward in local metres: `−z` is north, `+x` east, and yaw turns it west.
    let (forward_x, forward_z) = (-yaw_rad.sin(), -yaw_rad.cos());
    let here = frame.local_to_dir(x_m, z_m).0;
    // A metre ahead: far below the frame's curvature, far above f64's resolution of a unit
    // vector (about 4e-11 m at Earth's radius).
    let ahead = frame.local_to_dir(x_m + forward_x, z_m + forward_z).0;
    compass_bearing_deg(here, ahead - here)
}

/// The eight compass points, from north clockwise.
const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];

/// A bearing rounded to a whole degree in `0..360`, as the rose's label shows it.
pub fn whole_degrees(bearing_deg: f64) -> u32 {
    if !bearing_deg.is_finite() {
        return 0;
    }
    (bearing_deg.round().rem_euclid(360.0) as u32) % 360
}

/// The nearest of the eight compass points to a bearing: `NE` for 47°.
pub fn compass_point(bearing_deg: f64) -> &'static str {
    let degrees = whole_degrees(bearing_deg);
    // Each point covers 45°, centred on it.
    POINTS[(((degrees as f64 + 22.5) / 45.0).floor() as usize) % 8]
}

/// Appends the rose's label, `NE 47°`, to `out` (not cleared, so the HUD reuses its buffer).
pub fn write_heading_label(out: &mut String, bearing_deg: f64) {
    let degrees = whole_degrees(bearing_deg);
    let _ = write!(out, "{} {degrees}°", compass_point(f64::from(degrees)));
}

/// Where the heading-up rose draws the direction `bearing_deg` while the fly faces
/// `heading_deg`: degrees clockwise from the rose's top, in `[0, 360)`. The fly's heading is
/// always at the top (0); north is at `−heading`.
pub fn rose_angle_deg(bearing_deg: f64, heading_deg: f64) -> f64 {
    let angle = (bearing_deg - heading_deg).rem_euclid(360.0);
    if angle >= 360.0 { 0.0 } else { angle }
}

/// The screen offset (x right, y down, in the radius's units) of a mark `angle_deg` clockwise
/// from the rose's top at `radius` from its centre.
pub fn rose_offset(angle_deg: f64, radius: f64) -> (f64, f64) {
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    (radius * sin, -radius * cos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight::globe::{GlobeFlight, GlobeInput, globe_fly_step};
    use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
    use crate::worldgen::frame::RegionFrame;
    use proptest::prelude::*;

    fn circular_gap(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    proptest! {
        #[test]
        fn positions_format_with_hemispheres(
            lat in prop_oneof![-90.0f64..=90.0, Just(0.0), Just(-0.0), Just(-0.004), Just(90.0)],
            lon in prop_oneof![-180.0f64..=180.0, Just(180.0), Just(-180.0), Just(-0.004)],
            decimals in 0usize..=4,
        ) {
            let text = format_lat_lon(lat, lon, decimals);
            let (back_lat, back_lon) = parse_lat_lon(&text).expect(&text);
            let half = 0.5 * 10f64.powi(-(decimals as i32)) + 1e-9;
            prop_assert!((back_lat - lat).abs() <= half, "{lat} -> {text}");
            prop_assert!((back_lon - lon).abs() <= half, "{lon} -> {text}");
            // The letters carry the sign; a value that rounds to zero is N or E.
            let (lat_text, lon_text) = text.split_once("  ").unwrap();
            prop_assert!(!text.contains('-'), "{text}");
            prop_assert!(lat_text.ends_with(if back_lat < 0.0 { "° S" } else { "° N" }), "{text}");
            prop_assert!(lon_text.ends_with(if back_lon < 0.0 { "° W" } else { "° E" }), "{text}");
            // The compact form parses back the same way.
            let compact = compact_latitude(lat, decimals);
            prop_assert_eq!(parse_coordinate(&compact, ('N', 'S')), Some(back_lat));
            let compact = compact_longitude(lon, decimals);
            prop_assert_eq!(parse_coordinate(&compact, ('E', 'W')), Some(back_lon));
        }
    }

    #[test]
    fn positions_format_as_the_owner_asked() {
        assert_eq!(format_lat_lon(22.8749, 71.4249, 2), "22.87° N  71.42° E");
        assert_eq!(format_lat_lon(-33.9, -151.2, 2), "33.90° S  151.20° W");
        assert_eq!(format_lat_lon(-0.001, -0.004, 2), "0.00° N  0.00° E");
        assert_eq!(compact_latitude(-22.5, 3), "22.500S");
        assert_eq!(compact_longitude(71.42, 3), "71.420E");
        assert_eq!(parse_coordinate("-3N", ('N', 'S')), None);
        assert_eq!(parse_coordinate("3X", ('N', 'S')), None);
        assert_eq!(parse_lat_lon("22.87° N 71.42° E"), None);
    }

    #[test]
    fn bearings_are_true_directions() {
        let at = |lat: f64, lon: f64| geo_to_render(latlon_to_geo(lat, lon)).0;
        // At the equator, toward the pole is north and toward 90° E is east.
        let dir = at(0.0, 0.0);
        assert!(compass_bearing_deg(dir, DVec3::Y).abs() < 1e-9);
        assert!((compass_bearing_deg(dir, DVec3::X) - 90.0).abs() < 1e-9);
        assert!((compass_bearing_deg(dir, -DVec3::Y) - 180.0).abs() < 1e-9);
        assert!((compass_bearing_deg(dir, -DVec3::X) - 270.0).abs() < 1e-9);
        // Northeast at 40° N: the step toward a point to the north-east.
        let here = at(40.0, 10.0);
        let ne = at(40.01, 10.0 + 0.01 / 40f64.to_radians().cos()) - here;
        assert!((compass_bearing_deg(here, ne) - 45.0).abs() < 0.05);
        // At the pole: finite, measured from longitude 0.
        let pole = at(90.0, 0.0);
        let b = compass_bearing_deg(pole, DVec3::Z);
        assert!(b.is_finite() && b.abs() < 1e-9, "{b}");
        assert!(compass_bearing_deg(pole, DVec3::Y).is_finite());
    }

    #[test]
    fn a_region_bearing_is_true_north_away_from_the_centre() {
        // A region at 60° N: 140 km east of the centre, the frame's north is turned from true
        // north by about (d/R)·tan(lat) ≈ 2.2° (memory: local-frame vectors are not region
        // axes); the bearing is the true one.
        let frame = RegionFrame {
            center: geo_to_render(latlon_to_geo(60.0, 10.0)),
        }
        .basis();
        let centre = region_bearing_deg(&frame, 0.0, 0.0, 0.0);
        assert!(circular_gap(centre, 0.0) < 1e-6, "{centre}");
        // Yaw positive turns west: a quarter turn faces west.
        let west = region_bearing_deg(&frame, 0.0, 0.0, std::f64::consts::FRAC_PI_2);
        assert!(circular_gap(west, 270.0) < 1e-6, "{west}");
        let east_of_centre = region_bearing_deg(&frame, 140_000.0, 0.0, 0.0);
        let convergence = 140_000.0 / 6_371_000.0 * 60f64.to_radians().tan();
        assert!(
            (circular_gap(east_of_centre, 0.0) - convergence.to_degrees()).abs() < 0.05,
            "{east_of_centre}"
        );
        // And it matches a direct reading on the sphere: the fly's place and a step ahead.
        let here = frame.local_to_dir(140_000.0, 0.0).0;
        let (lat, lon) = geo_to_latlon(render_to_geo(RenderDir(here)));
        let north = geo_to_render(latlon_to_geo(lat + 1e-4, lon)).0 - here;
        let b = compass_bearing_deg(here, north);
        assert!(circular_gap(b, 0.0) < 1e-6);
    }

    #[test]
    fn compass_points_and_labels() {
        let label = |b: f64| {
            let mut s = String::new();
            write_heading_label(&mut s, b);
            s
        };
        assert_eq!(label(47.0), "NE 47°");
        assert_eq!(label(0.0), "N 0°");
        assert_eq!(label(359.6), "N 0°");
        assert_eq!(label(22.4), "N 22°");
        assert_eq!(label(22.6), "NE 23°");
        assert_eq!(label(180.0), "S 180°");
        assert_eq!(label(270.0), "W 270°");
        assert_eq!(label(337.4), "NW 337°");
        assert_eq!(label(f64::NAN), "N 0°");
    }

    #[test]
    fn the_rose_turns_with_the_heading() {
        // Heading-up: the fly's heading is at the top; the true directions turn the other way.
        for heading in [0.0, 47.0, 90.0, 180.0, 271.5, 359.0] {
            assert!(rose_angle_deg(heading, heading).abs() < 1e-9);
            let north = rose_angle_deg(0.0, heading);
            assert!(circular_gap(north, -heading) < 1e-9, "{heading}: {north}");
            let east = rose_angle_deg(90.0, heading);
            assert!(
                circular_gap(east, north + 90.0) < 1e-9,
                "E is a quarter clockwise of N"
            );
        }
        // Facing east, north is on the left and east at the top.
        let (x, y) = rose_offset(rose_angle_deg(0.0, 90.0), 10.0);
        assert!((x + 10.0).abs() < 1e-9 && y.abs() < 1e-9, "({x}, {y})");
        let (x, y) = rose_offset(rose_angle_deg(90.0, 90.0), 10.0);
        assert!(x.abs() < 1e-9 && (y + 10.0).abs() < 1e-9, "({x}, {y})");

        // Over a pole on the globe: flying north across the North Pole, the rose shows north
        // ahead before the pole and behind after it, and every frame's bearing is finite.
        let mut fly = GlobeFlight::start(89.0, 30.0);
        fly.altitude_km = 2_000.0;
        let ahead = |f: &GlobeFlight| compass_bearing_deg(f.dir, f.heading);
        assert!(circular_gap(ahead(&fly), 0.0) < 1e-6);
        let mut max_lat = f64::MIN;
        let mut passed = false;
        for _ in 0..4_000 {
            fly = globe_fly_step(
                fly,
                GlobeInput {
                    forward: 1.0,
                    strafe: 0.0,
                    turn_rad: 0.0,
                    climb: 0.0,
                    boost: false,
                    dt_s: 1.0 / 60.0,
                },
            );
            let bearing = ahead(&fly);
            assert!(bearing.is_finite() && (0.0..360.0).contains(&bearing));
            let lat = geo_to_latlon(render_to_geo(RenderDir(fly.dir))).0;
            max_lat = max_lat.max(lat);
            let north_on_rose = rose_angle_deg(0.0, bearing);
            if lat < max_lat - 0.5 {
                // Past the pole: heading south, north at the rose's bottom.
                assert!(circular_gap(bearing, 180.0) < 1e-3, "{bearing}");
                assert!(circular_gap(north_on_rose, 180.0) < 1e-3);
                passed = true;
                break;
            } else if lat < 89.5 {
                assert!(circular_gap(bearing, 0.0) < 1e-3, "{bearing}");
                assert!(circular_gap(north_on_rose, 0.0) < 1e-3);
            }
        }
        assert!(
            max_lat > 89.9 && passed,
            "never crossed the pole: {max_lat}"
        );
        // Exactly at the pole: finite.
        let at_pole = compass_bearing_deg(DVec3::Y, DVec3::Z);
        assert!(at_pole.is_finite());
    }
}
