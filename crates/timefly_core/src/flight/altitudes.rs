//! Altitude, timing and camera constants for the fly and the zoom-and-swap.

use crate::globe::EARTH_RADIUS_KM;

pub const GLOBE_MAX_CAMERA_DISTANCE_KM: f64 = 2.75 * EARTH_RADIUS_KM;
/// The neutral chase eye reaches the camera limit here at 70° elevation.
pub const GLOBE_MAX_ALTITUDE_KM: f64 = 7_552.881;
pub const GLOBE_START_ALTITUDE_KM: f64 = GLOBE_MAX_ALTITUDE_KM;
/// The globe stays clear through a camera distance of 1.15 Earth radii, at every chase angle.
pub const CLOUD_TOP_KM: f64 = 630.0;
pub const DIVE_ALTITUDE_KM: f64 = 300.0;
pub const GLOBE_EXAGGERATION_FULL_KM: f64 = 6_000.0;
pub const GLOBE_EXAGGERATION_AT_CLOUD: f64 = 2.0;
pub const REGION_CLOUD_BASE_M: f64 = 300_000.0;
pub const REGION_CLOUD_BAND_M: f64 = 60_000.0;
pub const REGION_EMERGE_TO_M: f64 = 230_000.0;
pub const HOP_ALTITUDE_M: f64 = 60_000.0;
pub const PRELOAD_MIN_M: f64 = 50_000.0;
pub const NEIGHBOUR_PAST_EDGE_M: f64 = 100_000.0;
pub const LIFT_LEAD_M: f64 = 20_000.0;
pub const EXPECTED_GEN_S_NATIVE: f64 = 3.0;
pub const EXPECTED_GEN_S_WEB: f64 = 8.0;
pub const GLOBE_MAX_TURN_DEG_PER_S: f64 = 30.0;
pub const CHASE_DISTANCE_PER_AGL: f64 = 0.5;
pub const CHASE_MIN_DISTANCE_M: f64 = 60.0;
pub const CHASE_ELEVATION_DEG: f64 = 20.0;
/// The globe chase's look-down pitch at `GLOBE_MAX_ALTITUDE_KM` (M3 Task 14, owner's "A: globe
/// centred": with the eye 70° up behind the fly, 85° puts the globe's centre just below the
/// view's centre and its whole outline in a 16:9 or 9:16 view, the fly over its upper part).
pub const CHASE_GLOBE_TOP_PITCH_DEG: f64 = 85.0;
pub const FLY_SCREEN_FRACTION: f64 = 0.05;
pub const CLOUD_GATHER_S: f64 = 0.6;
pub const CLOUD_CLEAR_S: f64 = 1.2;
pub const EMERGE_S: f64 = 2.0;
pub const WEB_FLIGHT_GEN_BUDGET_MS: f64 = 3.0;
