//! The chase camera behind the fly, in one world's units (m in regions, km on the globe).

use super::altitudes::*;
use bevy_math::DVec3;

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
/// below it (the globe tilts toward straight down at the top).
pub fn chase_pose(
    fly: DVec3,
    forward: DVec3,
    up: DVec3,
    agl: f64,
    look_pitch_deg: f64,
    min_distance: f64,
) -> ChasePose {
    let up = up.normalize();
    let fwd = (forward - up * forward.dot(up)).normalize();
    let d = min_distance.max(CHASE_DISTANCE_PER_AGL * agl);
    let elev = CHASE_ELEVATION_DEG.to_radians();
    let eye = fly - fwd * (d * elev.cos()) + up * (d * elev.sin());
    let reach = d * elev.cos();
    let target = eye + fwd * reach - up * (reach * look_pitch_deg.to_radians().tan());
    ChasePose { eye, target, up }
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
/// height at `eye_to_fly`, for a vertical field of view of `vfov_rad`.
pub fn fly_scale(eye_to_fly: f64, vfov_rad: f64) -> f64 {
    2.0 * eye_to_fly * (vfov_rad / 2.0).tan() * FLY_SCREEN_FRACTION
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
