//! Cloud-layer density and the altitude-dependent globe exaggeration.

use super::altitudes::*;
use crate::globe::ELEVATION_EXAGGERATION;

/// Hermite smoothstep: 0 at/below `e0`, 1 at/above `e1`. Also valid for `e0 > e1` (falling edge).
pub(crate) fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Globe cloud density: 0 at `CLOUD_TOP_KM` and above, 1 at `DIVE_ALTITUDE_KM` and below.
pub fn globe_density(altitude_km: f64) -> f32 {
    smoothstep(CLOUD_TOP_KM, DIVE_ALTITUDE_KM, altitude_km) as f32
}

/// Region cloud density: 0 at `base - band` and below, 1 at `base` and above.
pub fn region_density(height_m: f32) -> f32 {
    smoothstep(
        REGION_CLOUD_BASE_M - REGION_CLOUD_BAND_M,
        REGION_CLOUD_BASE_M,
        height_m as f64,
    ) as f32
}

/// Globe exaggeration: full at `GLOBE_EXAGGERATION_FULL_KM`+, `GLOBE_EXAGGERATION_AT_CLOUD` at
/// `CLOUD_TOP_KM`-, smoothstep in `ln(altitude)` between.
pub fn globe_exaggeration(altitude_km: f64) -> f64 {
    let t = smoothstep(
        CLOUD_TOP_KM.ln(),
        GLOBE_EXAGGERATION_FULL_KM.ln(),
        altitude_km.max(f64::MIN_POSITIVE).ln(),
    );
    GLOBE_EXAGGERATION_AT_CLOUD + (ELEVATION_EXAGGERATION - GLOBE_EXAGGERATION_AT_CLOUD) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globe_density_is_clear_above_the_cloud_top_and_opaque_at_the_dive() {
        assert_eq!(globe_density(CLOUD_TOP_KM + 1.0), 0.0);
        assert_eq!(globe_density(CLOUD_TOP_KM), 0.0);
        assert_eq!(globe_density(DIVE_ALTITUDE_KM), 1.0);
        assert_eq!(globe_density(10.0), 1.0);
        let mid = globe_density((CLOUD_TOP_KM + DIVE_ALTITUDE_KM) / 2.0);
        assert!(mid > 0.0 && mid < 1.0);
    }

    #[test]
    fn region_density_is_opaque_at_the_cloud_base() {
        assert_eq!(region_density(REGION_CLOUD_BASE_M as f32), 1.0);
        assert_eq!(region_density(1.0e6), 1.0);
        assert_eq!(
            region_density((REGION_CLOUD_BASE_M - REGION_CLOUD_BAND_M) as f32),
            0.0
        );
        assert_eq!(region_density(0.0), 0.0);
        let mid = region_density((REGION_CLOUD_BASE_M - REGION_CLOUD_BAND_M / 2.0) as f32);
        assert!(mid > 0.0 && mid < 1.0);
    }

    #[test]
    fn exaggeration_eases_from_twenty_to_the_cloud_value() {
        assert_eq!(
            globe_exaggeration(GLOBE_MAX_ALTITUDE_KM),
            ELEVATION_EXAGGERATION
        );
        assert_eq!(
            globe_exaggeration(GLOBE_EXAGGERATION_FULL_KM),
            ELEVATION_EXAGGERATION
        );
        assert_eq!(
            globe_exaggeration(CLOUD_TOP_KM),
            GLOBE_EXAGGERATION_AT_CLOUD
        );
        assert_eq!(globe_exaggeration(100.0), GLOBE_EXAGGERATION_AT_CLOUD);
        let (lo, hi) = (CLOUD_TOP_KM.ln(), GLOBE_EXAGGERATION_FULL_KM.ln());
        let mut prev = GLOBE_EXAGGERATION_AT_CLOUD;
        for i in 1..=100 {
            let alt = (lo + (hi - lo) * i as f64 / 101.0).exp();
            let e = globe_exaggeration(alt);
            assert!(
                e > prev,
                "not strictly increasing at {alt} km: {e} <= {prev}"
            );
            assert!(e < ELEVATION_EXAGGERATION);
            prev = e;
        }
    }
}
