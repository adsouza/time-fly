//! The chase camera behind the fly, in one world's units (m in regions, km on the globe).

use super::altitudes::*;
use super::cloud::smoothstep;
use super::globe::GlobeFlight;
use crate::globe::EARTH_RADIUS_KM;
use bevy_math::DVec3;

/// How far below the view's centre the fly sits (degrees): the chase look is this much
/// shallower than the eye's elevation above the fly (M3 design §5.3, "a little below centre").
pub const CHASE_FLY_BELOW_CENTRE_DEG: f64 = 5.0;
/// The chase view's look-down pitch at the region's start pitch (degrees):
/// [`CHASE_FLY_BELOW_CENTRE_DEG`] shallower than [`CHASE_ELEVATION_DEG`].
pub const CHASE_START_LOOK_DEG: f64 = CHASE_ELEVATION_DEG - CHASE_FLY_BELOW_CENTRE_DEG;
/// The globe chase's look-down pitch at and below [`DIVE_ALTITUDE_KM`] (degrees), tuned so the
/// ground under the fly fills at least the lower two-thirds of the frame there, landscape or
/// portrait (controller ruling, Task 4 fix round 1; the horizon dips about 38° from the eye).
pub const GLOBE_CHASE_DIVE_LOOK_DEG: f64 = 50.0;
/// The lowest the globe's chase eye may sit above the fly's horizon (degrees): the region's
/// [`CHASE_ELEVATION_DEG`]. A drag moves the eye and the look together, so at and below
/// [`DIVE_ALTITUDE_KM`] a drag up stops with the view looking 15° down, the region's start look;
/// higher it stops steeper, up to 33.8° at the top ([`CHASE_GLOBE_TOP_PITCH_DEG`] − 50°). Lower,
/// a fly descending nose down shows the eye its wings edge-on (M3 Task 14:
/// below about 15°; 2° of wing at the old floor of 5°, `creature`'s edge-on test).
pub const GLOBE_CHASE_MIN_ELEVATION_DEG: f64 = CHASE_ELEVATION_DEG;
/// The globe chase eye's elevation above the fly's horizon at and below [`DIVE_ALTITUDE_KM`]
/// (degrees): [`CHASE_FLY_BELOW_CENTRE_DEG`] above [`GLOBE_CHASE_DIVE_LOOK_DEG`], so at the dive
/// the fly sits that far below the view's centre, as in a region.
pub const GLOBE_CHASE_DIVE_ELEVATION_DEG: f64 =
    GLOBE_CHASE_DIVE_LOOK_DEG + CHASE_FLY_BELOW_CENTRE_DEG;
/// The globe chase eye's elevation at [`GLOBE_MAX_ALTITUDE_KM`] (degrees), owner's choice
/// "A: globe centred" (M3 Task 14): behind the fly, never overhead, and high enough that the
/// globe is behind the fly. From an eye at elevation `E` behind a fly at radius `r` the fly is
/// seen over the globe only when `r·cos E < R`, 64° at the top; at 70° the fly sits over the
/// globe's upper part, and the look ([`CHASE_GLOBE_TOP_PITCH_DEG`]) centres the whole globe.
pub const GLOBE_CHASE_TOP_ELEVATION_DEG: f64 = 70.0;
/// The highest a drag may raise the globe's chase eye (degrees): still behind the fly.
pub const GLOBE_CHASE_MAX_ELEVATION_DEG: f64 = 75.0;
/// The steepest the globe's chase may look (degrees below the fly's horizon): 5° short of
/// straight down, so the camera's up (the fly's) stays well defined.
pub const GLOBE_CHASE_MAX_LOOK_DEG: f64 = 85.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChasePose {
    pub eye: DVec3,
    pub target: DVec3,
    pub up: DVec3,
}

/// Chase pose behind `fly`.
///
/// `forward` is the fly's heading and `up` the local vertical (both need not be unit or
/// orthogonal; the heading is projected onto the horizon). The eye sits `d` behind the fly,
/// where `d = max(min_distance, CHASE_DISTANCE_PER_AGL * agl)`, raised so the fly-to-eye line
/// makes `CHASE_ELEVATION_DEG` with the fly's horizon.
///
/// `look_pitch_deg` is how far below the horizontal the camera looks. The target is the point
/// on the vertical line through the fly that the eye sees at that pitch: with
/// `look_pitch_deg == CHASE_ELEVATION_DEG` the target is the fly itself; steeper looks aim
/// below it. (Only the view ray matters: with the eye nearly over the fly, as at the globe's
/// top, the target moves out along the same ray, see [`chase_pose_with_elevation`].)
pub fn chase_pose(
    fly: DVec3,
    forward: DVec3,
    up: DVec3,
    agl: f64,
    look_pitch_deg: f64,
    min_distance: f64,
) -> ChasePose {
    chase_pose_with_elevation(
        fly,
        forward,
        up,
        agl,
        CHASE_ELEVATION_DEG,
        look_pitch_deg,
        min_distance,
    )
}

/// [`chase_pose`] with the eye `elevation_deg` above the fly's horizon instead of
/// [`CHASE_ELEVATION_DEG`]; the fly then sits `elevation_deg − look_pitch_deg` below the view's
/// centre. The globe raises both with altitude ([`globe_chase_pose`]).
pub fn chase_pose_with_elevation(
    fly: DVec3,
    forward: DVec3,
    up: DVec3,
    agl: f64,
    elevation_deg: f64,
    look_pitch_deg: f64,
    min_distance: f64,
) -> ChasePose {
    let up = up.normalize();
    let fwd = (forward - up * forward.dot(up)).normalize();
    let d = min_distance.max(CHASE_DISTANCE_PER_AGL * agl);
    let elev = elevation_deg.to_radians();
    let eye = fly - fwd * (d * elev.cos()) + up * (d * elev.sin());
    // The target is on the fly's vertical line while the eye is well back from it; nearer
    // overhead (a steep eye on the globe) that point closes on the eye, so the target stays at
    // least a quarter of the chase distance out along the same view ray.
    let reach = (d * elev.cos()).max(0.25 * d);
    let look = look_pitch_deg.to_radians();
    let target = eye + fwd * reach - up * (reach * look.tan());
    ChasePose { eye, target, up }
}

/// How far the globe chase's framing has gone from the dive's to the top's at `altitude_km`: 0
/// at and below [`DIVE_ALTITUDE_KM`], 1 at [`GLOBE_MAX_ALTITUDE_KM`], smoothstep in
/// `ln(altitude)` between (controller rulings, Task 4).
fn globe_framing(altitude_km: f64) -> f64 {
    smoothstep(
        DIVE_ALTITUDE_KM.ln(),
        GLOBE_MAX_ALTITUDE_KM.ln(),
        altitude_km.max(f64::MIN_POSITIVE).ln(),
    )
}

/// The globe chase's look-down pitch (degrees) at `altitude_km`: [`GLOBE_CHASE_DIVE_LOOK_DEG`]
/// at and below [`DIVE_ALTITUDE_KM`], rising to [`CHASE_GLOBE_TOP_PITCH_DEG`] at
/// [`GLOBE_MAX_ALTITUDE_KM`], where it centres the whole globe ([`globe_framing`]).
pub fn globe_look_pitch_deg(altitude_km: f64) -> f64 {
    let t = globe_framing(altitude_km);
    GLOBE_CHASE_DIVE_LOOK_DEG + (CHASE_GLOBE_TOP_PITCH_DEG - GLOBE_CHASE_DIVE_LOOK_DEG) * t
}

/// The globe chase eye's elevation above the fly's horizon (degrees) at `altitude_km`:
/// [`GLOBE_CHASE_DIVE_ELEVATION_DEG`] at and below the dive (the fly
/// [`CHASE_FLY_BELOW_CENTRE_DEG`] below the view's centre, as in a region), rising on the look's
/// curve to [`GLOBE_CHASE_TOP_ELEVATION_DEG`] at the top (behind the fly, the globe behind it;
/// the fly then over the globe's upper part, [`globe_fly_below_centre_deg`]).
pub fn globe_chase_elevation_deg(altitude_km: f64) -> f64 {
    let t = globe_framing(altitude_km);
    GLOBE_CHASE_DIVE_ELEVATION_DEG
        + (GLOBE_CHASE_TOP_ELEVATION_DEG - GLOBE_CHASE_DIVE_ELEVATION_DEG) * t
}

/// How far below the view's centre the globe chase shows the fly at `altitude_km` (degrees,
/// negative above): the eye's elevation less the look's pitch, which a drag moves together.
pub fn globe_fly_below_centre_deg(altitude_km: f64) -> f64 {
    globe_chase_elevation_deg(altitude_km) - globe_look_pitch_deg(altitude_km)
}

/// The globe chase distance (km) at `altitude_km`: as [`chase_pose`] reckons it, with the
/// floor in kilometres (ruling P1).
pub fn globe_chase_distance_km(altitude_km: f64) -> f64 {
    (CHASE_MIN_DISTANCE_M / 1000.0).max(CHASE_DISTANCE_PER_AGL * altitude_km)
}

/// The range of the view drag's offset (degrees, added to both the look pitch and the eye's
/// elevation; positive looks further down) at `altitude_km`: the eye stays between
/// [`GLOBE_CHASE_MIN_ELEVATION_DEG`] and [`GLOBE_CHASE_MAX_ELEVATION_DEG`] above the fly's
/// horizon and within [`GLOBE_MAX_CAMERA_DISTANCE_KM`] of the centre, and the look no steeper
/// than [`GLOBE_CHASE_MAX_LOOK_DEG`] (at the top, where the look centres the globe, that leaves
/// a drag down little room).
pub fn globe_look_offset_range(altitude_km: f64) -> (f64, f64) {
    // |eye|² = r² + 2·r·d·sin(e) + d² for the eye at elevation e, distance d behind a fly at
    // radius r: the 3R cap bounds sin(e).
    let r = EARTH_RADIUS_KM + altitude_km;
    let d = globe_chase_distance_km(altitude_km);
    let cap = GLOBE_MAX_CAMERA_DISTANCE_KM;
    let sin_cap = ((cap * cap - r * r - d * d) / (2.0 * r * d)).clamp(-1.0, 1.0);
    let top = GLOBE_CHASE_MAX_ELEVATION_DEG.min(sin_cap.asin().to_degrees());
    let base = globe_chase_elevation_deg(altitude_km);
    let steepest = GLOBE_CHASE_MAX_LOOK_DEG - globe_look_pitch_deg(altitude_km);
    (
        GLOBE_CHASE_MIN_ELEVATION_DEG - base,
        (top - base).min(steepest),
    )
}

/// The globe chase pose for `flight` (km, render frame) with the view drag's `look_offset_deg`
/// (clamped to [`globe_look_offset_range`]).
pub fn globe_chase_pose(flight: &GlobeFlight, look_offset_deg: f64) -> ChasePose {
    let alt = flight.altitude_km;
    let (lo, hi) = globe_look_offset_range(alt);
    let offset = look_offset_deg.clamp(lo, hi);
    chase_pose_with_elevation(
        flight.position_km(),
        flight.heading,
        flight.up(),
        alt,
        globe_chase_elevation_deg(alt) + offset,
        globe_look_pitch_deg(alt) + offset,
        CHASE_MIN_DISTANCE_M / 1000.0,
    )
}

/// Raise the eye over terrain, keeping the target.
///
/// `ground_at(p)` is the ground level under `p` on the same scale as the eye's height, which is
/// `eye.dot(pose.up)` (a flat world's height, or the radius on the globe). When the eye is below
/// `ground + clearance` it rises to that height and its horizontal offset from the target
/// shrinks so the eye-to-target distance is preserved; this repeats as the ground is resampled
/// at the new position, and a final plain rise guarantees the clearance.
pub fn lift_above_ground(
    pose: ChasePose,
    ground_at: impl Fn(DVec3) -> f64,
    clearance: f64,
) -> ChasePose {
    let ChasePose {
        mut eye,
        target,
        up,
    } = pose;
    let dist = (eye - target).length();
    for _ in 0..8 {
        let need = ground_at(eye) + clearance;
        let h = eye.dot(up);
        if h >= need {
            return ChasePose { eye, target, up };
        }
        let off = eye - target;
        let horiz = off - up * off.dot(up);
        let vert = need - target.dot(up);
        let horiz_len = (dist * dist - vert * vert).max(0.0).sqrt();
        let dir = horiz.try_normalize().unwrap_or(DVec3::ZERO);
        eye = target + up * vert + dir * horiz_len;
    }
    let short = ground_at(eye) + clearance - eye.dot(up);
    if short > 0.0 {
        eye += up * short;
    }
    ChasePose { eye, target, up }
}

/// Uniform scale for the fly model (1 unit tall) so it spans `FLY_SCREEN_FRACTION` of the view
/// height at `view_depth` along the view's axis, for a vertical field of view of `vfov_rad`.
/// Regions pass the eye-to-fly distance (the fly is near the view's centre); the globe passes
/// the depth, since off the centre perspective draws the fly 1 / cos larger.
pub fn fly_scale(view_depth: f64, vfov_rad: f64) -> f64 {
    2.0 * view_depth * (vfov_rad / 2.0).tan() * FLY_SCREEN_FRACTION
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight::globe::GLOBE_MIN_ALTITUDE_KM;

    const UP: DVec3 = DVec3::Y;
    const FWD: DVec3 = DVec3::NEG_Z;

    #[test]
    fn the_chase_eye_is_behind_and_above_the_fly() {
        let fly = DVec3::new(10.0, 1000.0, 20.0);
        let p = chase_pose(fly, FWD, UP, 400.0, 20.0, CHASE_MIN_DISTANCE_M);
        assert!(p.eye.z > fly.z);
        assert!(p.eye.y > fly.y);
        assert!((p.eye.x - fly.x).abs() < 1e-9);
        assert!(((p.eye - fly).length() - 0.5 * 400.0).abs() < 1e-9);
        let rise = (p.eye.y - fly.y).atan2(p.eye.z - fly.z).to_degrees();
        assert!((rise - CHASE_ELEVATION_DEG).abs() < 1e-9);
    }

    #[test]
    fn the_chase_distance_has_a_floor_in_the_callers_units() {
        let fly = DVec3::ZERO;
        let m = chase_pose(fly, FWD, UP, 10.0, 20.0, CHASE_MIN_DISTANCE_M);
        assert!(((m.eye - fly).length() - CHASE_MIN_DISTANCE_M).abs() < 1e-9);
        let km = chase_pose(fly, FWD, UP, 0.0, 20.0, CHASE_MIN_DISTANCE_M / 1000.0);
        assert!(((km.eye - fly).length() - 0.06).abs() < 1e-12);
    }

    #[test]
    fn heading_is_projected_onto_the_horizon() {
        let tilted = DVec3::new(0.0, -0.5, -1.0);
        let a = chase_pose(DVec3::ZERO, tilted, UP, 400.0, 20.0, 1.0);
        let b = chase_pose(DVec3::ZERO, FWD, UP, 400.0, 20.0, 1.0);
        assert!((a.eye - b.eye).length() < 1e-9);
    }

    #[test]
    fn look_pitch_sets_the_view_angle_below_the_horizontal() {
        let fly = DVec3::new(0.0, 500.0, 0.0);
        let at_fly = chase_pose(fly, FWD, UP, 800.0, CHASE_ELEVATION_DEG, 1.0);
        assert!((at_fly.target - fly).length() < 1e-9);
        for pitch in [10.0, 45.0, CHASE_GLOBE_TOP_PITCH_DEG] {
            let p = chase_pose(fly, FWD, UP, 800.0, pitch, 1.0);
            let v = p.target - p.eye;
            let below = (-v.y).atan2(-v.z).to_degrees();
            assert!((below - pitch).abs() < 1e-9, "{below} vs {pitch}");
            // The target stays on the vertical line through the fly.
            assert!((p.target.x - fly.x).abs() < 1e-9 && (p.target.z - fly.z).abs() < 1e-9);
        }
    }

    #[test]
    fn the_fly_keeps_its_screen_size_across_heights() {
        let vfov = 45f64.to_radians();
        for agl in [150.0, 8_000.0, 60_000.0, 300_000.0] {
            let p = chase_pose(
                DVec3::new(0.0, agl, 0.0),
                FWD,
                UP,
                agl,
                20.0,
                CHASE_MIN_DISTANCE_M,
            );
            let d = (p.eye - DVec3::new(0.0, agl, 0.0)).length();
            let body = fly_scale(d, vfov);
            let fraction = body / (2.0 * d * (vfov / 2.0).tan());
            assert!((fraction - FLY_SCREEN_FRACTION).abs() < 0.01 * FLY_SCREEN_FRACTION);
        }
    }

    #[test]
    fn the_eye_rises_over_a_ridge_behind_the_fly() {
        let fly = DVec3::new(0.0, 500.0, 0.0);
        let pose = chase_pose(fly, FWD, UP, 1000.0, 20.0, CHASE_MIN_DISTANCE_M);
        assert!(pose.eye.y < 2000.0, "unlifted eye starts below the ridge");
        let ridge = |p: DVec3| if p.z > 10.0 { 2000.0 } else { 0.0 };
        let lifted = lift_above_ground(pose, ridge, 50.0);
        assert!(lifted.eye.y >= ridge(lifted.eye) + 50.0);
        assert_eq!(lifted.target, pose.target);
        assert_eq!(lifted.up, pose.up);
    }

    #[test]
    fn a_clear_eye_is_left_alone() {
        let pose = chase_pose(DVec3::new(0.0, 500.0, 0.0), FWD, UP, 1000.0, 20.0, 60.0);
        assert_eq!(lift_above_ground(pose, |_| 0.0, 50.0), pose);
    }

    #[test]
    fn lifting_pulls_the_eye_in_toward_the_fly_at_constant_range() {
        // Ground rises steeply toward the camera: the eye must climb, and it keeps its
        // range to the target by closing the horizontal gap.
        let fly = DVec3::new(0.0, 500.0, 0.0);
        let pose = chase_pose(fly, FWD, UP, 1000.0, 20.0, 60.0);
        let slope = |p: DVec3| (p.z * 2.0).max(0.0);
        let lifted = lift_above_ground(pose, slope, 10.0);
        assert!(lifted.eye.y >= slope(lifted.eye) + 10.0);
        assert!(lifted.eye.y > pose.eye.y);
        assert!(lifted.eye.z < pose.eye.z);
        let range = |p: &ChasePose| (p.eye - p.target).length();
        assert!((range(&lifted) - range(&pose)).abs() < 1e-6);
    }

    #[test]
    fn fly_scale_is_linear_in_distance_and_matches_the_formula() {
        let vfov = 1.0;
        let s = fly_scale(100.0, vfov);
        assert!((s - 2.0 * 100.0 * 0.5f64.tan() * FLY_SCREEN_FRACTION).abs() < 1e-12);
        assert!((fly_scale(200.0, vfov) - 2.0 * s).abs() < 1e-12);
    }

    #[test]
    fn the_elevation_sets_the_eye_and_the_fly_sits_the_difference_below_centre() {
        let fly = DVec3::new(0.0, 500.0, 0.0);
        for (elev, look) in [(20.0, 15.0), (45.0, 40.0), (75.0, 70.0), (85.0, 60.0)] {
            let p = chase_pose_with_elevation(fly, FWD, UP, 800.0, elev, look, 1.0);
            let back = p.eye - fly;
            assert!(((back).length() - 400.0).abs() < 1e-9);
            let rise = back.y.atan2(back.z).to_degrees();
            assert!((rise - elev).abs() < 1e-9, "{rise} vs {elev}");
            let v = (p.target - p.eye).normalize();
            let to_fly = (fly - p.eye).normalize();
            let below = v.angle_between(to_fly).to_degrees();
            assert!((below - (elev - look)).abs() < 1e-9, "{below}");
        }
        // `chase_pose` is the fixed-elevation case.
        let a = chase_pose(fly, FWD, UP, 800.0, 33.0, 1.0);
        let b = chase_pose_with_elevation(fly, FWD, UP, 800.0, CHASE_ELEVATION_DEG, 33.0, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn the_globe_look_rises_from_the_dive_to_the_top() {
        let dive = GLOBE_CHASE_DIVE_LOOK_DEG;
        assert!((globe_look_pitch_deg(DIVE_ALTITUDE_KM) - dive).abs() < 1e-12);
        assert!((globe_look_pitch_deg(GLOBE_MIN_ALTITUDE_KM) - dive).abs() < 1e-12);
        assert!(
            (globe_look_pitch_deg(GLOBE_MAX_ALTITUDE_KM) - CHASE_GLOBE_TOP_PITCH_DEG).abs() < 1e-12
        );
        let (lo, hi) = (DIVE_ALTITUDE_KM.ln(), GLOBE_MAX_ALTITUDE_KM.ln());
        let mut prev = dive;
        for i in 1..100 {
            let alt = (lo + (hi - lo) * f64::from(i) / 100.0).exp();
            let p = globe_look_pitch_deg(alt);
            assert!(p > prev, "{alt} km: {p} <= {prev}");
            prev = p;
        }
        // Smoothstep in ln(altitude): halfway in ln is halfway in pitch.
        let mid = ((lo + hi) / 2.0).exp();
        let half = (dive + CHASE_GLOBE_TOP_PITCH_DEG) / 2.0;
        assert!((globe_look_pitch_deg(mid) - half).abs() < 1e-9);
        // The eye rises from the dive's 55° to the top's 70° on the same curve: behind the fly
        // at every altitude, never overhead.
        let elevation = globe_chase_elevation_deg;
        assert!((elevation(GLOBE_MIN_ALTITUDE_KM) - GLOBE_CHASE_DIVE_ELEVATION_DEG).abs() < 1e-12);
        assert!((elevation(DIVE_ALTITUDE_KM) - 55.0).abs() < 1e-12);
        assert!((elevation(GLOBE_MAX_ALTITUDE_KM) - GLOBE_CHASE_TOP_ELEVATION_DEG).abs() < 1e-12);
        assert!((elevation(mid) - (55.0 + GLOBE_CHASE_TOP_ELEVATION_DEG) / 2.0).abs() < 1e-9);
        // At the dive the fly sits 5° below the centre, as in a region; at the top above it,
        // where the look centres the globe.
        assert!(
            (globe_fly_below_centre_deg(DIVE_ALTITUDE_KM) - CHASE_FLY_BELOW_CENTRE_DEG).abs()
                < 1e-12
        );
        assert!(globe_fly_below_centre_deg(GLOBE_MAX_ALTITUDE_KM) < -10.0);
    }

    #[test]
    fn the_globe_chase_stays_behind_the_fly() {
        // Owner, M3 Task 14 ("A: globe centred"): at every altitude, heading and drag offset
        // (past its range too, which the pose clamps) the eye is behind the fly, at most
        // GLOBE_CHASE_MAX_ELEVATION_DEG up (never overhead), and the look stays short of
        // straight down.
        const { assert!(GLOBE_CHASE_MAX_ELEVATION_DEG <= 80.0) };
        let (lo, hi) = (GLOBE_MIN_ALTITUDE_KM.ln(), GLOBE_MAX_ALTITUDE_KM.ln());
        for i in 0..=24 {
            let alt = (lo + (hi - lo) * f64::from(i) / 24.0).exp();
            let (low, high) = globe_look_offset_range(alt);
            assert!(low < 0.0 && high >= 0.0, "{alt} km: {low}..{high}");
            let base = globe_chase_elevation_deg(alt);
            assert!(
                (55.0..=GLOBE_CHASE_TOP_ELEVATION_DEG).contains(&base),
                "{alt}: {base}"
            );
            for mut f in flights() {
                f.altitude_km = alt;
                for k in 0..=16 {
                    let offset = -100.0 + 200.0 * f64::from(k) / 16.0;
                    let p = globe_chase_pose(&f, offset);
                    let back = p.eye - f.position_km();
                    let d = back.length();
                    let rise = back.dot(f.up()).atan2(-back.dot(f.heading)).to_degrees();
                    assert!(
                        rise <= GLOBE_CHASE_MAX_ELEVATION_DEG + 1e-6,
                        "{alt} km, offset {offset}: eye {rise}° up"
                    );
                    // Behind along the heading by at least the steepest eye's share.
                    let behind = -back.dot(f.heading);
                    assert!(
                        behind >= d * GLOBE_CHASE_MAX_ELEVATION_DEG.to_radians().cos() - 1e-6,
                        "{alt} km, offset {offset}: {behind} of {d} km behind"
                    );
                    let v = (p.target - p.eye).normalize();
                    let look = -v.dot(f.up()).asin().to_degrees();
                    assert!(
                        look <= GLOBE_CHASE_MAX_LOOK_DEG + 1e-6,
                        "{alt} km, offset {offset}: looking {look}° down"
                    );
                }
            }
        }
    }

    fn flights() -> Vec<GlobeFlight> {
        let mut out = Vec::new();
        for i in 0..64 {
            let a = std::f64::consts::TAU * f64::from(i) / 64.0;
            let mut f =
                GlobeFlight::start(-70.0 + 140.0 * f64::from(i) / 63.0, 3.0 * a.to_degrees());
            let east = f.dir.cross(f.heading);
            f.heading = (f.heading * a.cos() + east * a.sin()).normalize();
            out.push(f);
        }
        out
    }

    #[test]
    fn the_camera_never_goes_past_three_radii() {
        // 64 headings, and look pitches across the drag's whole range (and beyond, which the
        // pose clamps), at the top altitude and on the way down.
        for alt in [
            GLOBE_MAX_ALTITUDE_KM,
            4_000.0,
            DIVE_ALTITUDE_KM,
            GLOBE_MIN_ALTITUDE_KM,
        ] {
            let (lo, hi) = globe_look_offset_range(alt);
            assert!(lo < 0.0 && hi >= 0.0, "{alt}: {lo}..{hi}");
            for mut f in flights() {
                f.altitude_km = alt;
                for k in 0..=16 {
                    let offset = -100.0 + 200.0 * f64::from(k) / 16.0;
                    let p = globe_chase_pose(&f, offset);
                    let r = p.eye.length();
                    assert!(
                        r <= GLOBE_MAX_CAMERA_DISTANCE_KM + 1e-6,
                        "{alt} km, offset {offset}: eye at {r} km"
                    );
                    // Outside the fly's altitude, and above its horizon.
                    assert!(r > EARTH_RADIUS_KM + alt, "{alt} km, offset {offset}: {r}");
                }
            }
        }
    }

    #[test]
    fn the_globe_drag_offset_keeps_the_eye_between_its_limits() {
        for alt in [
            GLOBE_MIN_ALTITUDE_KM,
            DIVE_ALTITUDE_KM,
            3_000.0,
            GLOBE_MAX_ALTITUDE_KM,
        ] {
            let (lo, hi) = globe_look_offset_range(alt);
            let base = globe_chase_elevation_deg(alt);
            assert!(
                (base + lo - GLOBE_CHASE_MIN_ELEVATION_DEG).abs() < 1e-9,
                "{alt}"
            );
            assert!(base + hi <= GLOBE_CHASE_MAX_ELEVATION_DEG + 1e-9, "{alt}");
            assert!(
                globe_look_pitch_deg(alt) + hi <= GLOBE_CHASE_MAX_LOOK_DEG + 1e-9,
                "{alt}"
            );
            // At the limits the fly still sits where the altitude puts it in the view (the drag
            // moves the eye and the look together).
            let fly_below = globe_fly_below_centre_deg(alt);
            let f = GlobeFlight {
                altitude_km: alt,
                ..GlobeFlight::start(10.0, 20.0)
            };
            for offset in [lo, hi] {
                let p = globe_chase_pose(&f, offset);
                let v = (p.target - p.eye).normalize();
                let to_fly = (f.position_km() - p.eye).normalize();
                let below = v.angle_between(to_fly).to_degrees();
                // The view angle is unsigned: the fly may sit above the centre.
                assert!((below - fly_below.abs()).abs() < 1e-6, "{alt}: {below}");
                let back = p.eye - f.position_km();
                let rise = back.dot(f.up()).atan2(-back.dot(f.heading)).to_degrees();
                assert!((rise - (base + offset)).abs() < 1e-6, "{alt}: {rise}");
            }
            // Past the limits the pose clamps.
            assert_eq!(globe_chase_pose(&f, hi + 30.0), globe_chase_pose(&f, hi));
            assert_eq!(globe_chase_pose(&f, lo - 30.0), globe_chase_pose(&f, lo));
        }
    }

    #[test]
    fn at_the_top_the_globe_fills_the_view_ahead() {
        // The view looks (nearly) at the globe's centre, and the eye sits behind the fly.
        for f in flights() {
            let f = GlobeFlight {
                altitude_km: GLOBE_MAX_ALTITUDE_KM,
                ..f
            };
            let p = globe_chase_pose(&f, 0.0);
            let look = (p.target - p.eye).normalize();
            assert!(look.dot(-p.eye.normalize()) > 0.99, "{p:?}");
            // Behind the fly, looking ahead and down past it.
            let back = p.eye - f.position_km();
            assert!(-back.dot(f.heading) > 0.3 * back.length(), "{p:?}");
            assert!(look.dot(f.heading) > 0.0);
        }
    }

    #[test]
    fn an_eye_straight_over_the_fly_still_has_a_view_direction() {
        // At 90° elevation the eye is on the fly's vertical line: the target must still sit a
        // well-conditioned distance along the view ray (not on the eye).
        let fly = DVec3::new(0.0, 14_653.0, 0.0);
        let p = chase_pose_with_elevation(fly, FWD, UP, 8_282.0, 90.0, 85.0, 1.0);
        let v = p.target - p.eye;
        assert!(v.length() > 100.0, "{p:?}");
        let v = v.normalize();
        let below = (-v.y).atan2(-v.z).to_degrees();
        assert!((below - 85.0).abs() < 1e-9, "{below}");
        let to_fly = (fly - p.eye).normalize();
        assert!((v.angle_between(to_fly).to_degrees() - 5.0).abs() < 1e-9);
    }
}
