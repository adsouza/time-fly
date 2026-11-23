//! Detail noise and the coastline rule (design spec §5.3).

use bevy_math::DVec3;

use crate::deep_time::YearsBp;
use crate::worldgen::coarse::LatticeKey;
use crate::worldgen::frame::EARTH_RADIUS_M;
use crate::worldgen::noise::{fbm, hash3, ridged};

/// Detail relief per metre of source spacing: the first octave's amplitude on plains (scaled
/// down on low ground and up to full at [`PLAINS_FULL_M`]) and in mountains.
const PLAINS_RELIEF: f64 = 0.004;
const PLAINS_FULL_M: f64 = 500.0;
const PLAINS_FLOOR: f64 = 0.1;
const MOUNTAIN_RELIEF: f64 = 0.02;
/// Ridged octaves fade in between these coarse elevations, or these coarse gradients (on land).
const MOUNTAIN_START_M: f64 = 1000.0;
const MOUNTAIN_FULL_M: f64 = 2000.0;
const SLOPE_START: f64 = 0.01;
const SLOPE_FULL: f64 = 0.03;
/// The long-run mean of [`ridged`] (0.678 over 400,000 random points on the sphere, the same for
/// any octave count), removed so detail does not shift the coarse trend.
const RIDGED_MEAN: f64 = 0.678;
const MAX_OCTAVES: u32 = 16;

const PLATE_SALT: u32 = 0x9E37_79B9;
const ERA_SALT: u32 = 0x85EB_CA6B;
const RIDGE_SALT: u32 = 0xC2B2_AE35;

/// The coastline rule (design spec §5.3): where all four corner samples are land the height
/// stays at least +1 m; where all are sea, at most −1 m; mixed corners leave it unchanged.
pub fn coastline_clamp(h_m: f64, corners_m: [f64; 4]) -> f64 {
    if corners_m.iter().all(|&c| c > 0.0) {
        h_m.max(1.0)
    } else if corners_m.iter().all(|&c| c < 0.0) {
        h_m.min(-1.0)
    } else {
        h_m
    }
}

pub(super) fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Detail octaves for a field: from the source spacing down to twice the texel (none when the
/// texel is coarser than half the source spacing).
pub(super) fn octaves_for(spacing_m: f64, texel_m: f64) -> u32 {
    if spacing_m >= 2.0 * texel_m {
        ((spacing_m / (2.0 * texel_m)).log2().floor() as u32 + 1).min(MAX_OCTAVES)
    } else {
        0
    }
}

/// The detail generator of one region: source spacing and seeds.
#[derive(Copy, Clone, Debug)]
pub(super) struct Detail {
    spacing_m: f64,
    seed: u32,
    era_fixed_seed: u32,
}

impl Detail {
    pub(super) fn new(spacing_m: f64, seed: u32, snapshot: YearsBp) -> Detail {
        let age_bits = snapshot.0.to_bits();
        Detail {
            spacing_m,
            seed,
            era_fixed_seed: hash3(
                [age_bits as i32, (age_bits >> 32) as i32, 0],
                seed ^ ERA_SALT,
            ),
        }
    }

    /// The detail noise `D` (m) for a texel: ridged octaves in mountains (high or steep land),
    /// fBm elsewhere, from the source spacing down to twice the texel.
    pub(super) fn detail_m(
        &self,
        key: &LatticeKey,
        coarse_m: f64,
        slope: f64,
        octaves: u32,
    ) -> f64 {
        if octaves == 0 {
            return 0.0;
        }
        let (p, seed) = self.noise_input(key);
        let s = self.spacing_m;
        // Raw octave sum: fbm and ridged normalise by the total amplitude, which differs between
        // octave counts; undoing it keeps the apron's octaves a prefix of the inner field's.
        let total = 2.0 * (1.0 - 0.5f64.powi(octaves as i32));
        let mountain = if coarse_m > 0.0 {
            smoothstep(MOUNTAIN_START_M, MOUNTAIN_FULL_M, coarse_m).max(smoothstep(
                SLOPE_START,
                SLOPE_FULL,
                slope,
            ))
        } else {
            0.0
        };
        let mut d = 0.0;
        if mountain < 1.0 {
            let amp = PLAINS_RELIEF * s * (coarse_m.abs() / PLAINS_FULL_M).clamp(PLAINS_FLOOR, 1.0);
            let n = f64::from(fbm(p, s, octaves, 2.0, 0.5, seed)) * total;
            d += (1.0 - mountain) * amp * n;
        }
        if mountain > 0.0 {
            let n = (f64::from(ridged(p, s, octaves, seed ^ RIDGE_SALT)) - RIDGED_MEAN) * total;
            d += mountain * MOUNTAIN_RELIEF * s * n;
        }
        d
    }

    /// Noise position (m on the sphere) and seed of a lattice key.
    fn noise_input(&self, key: &LatticeKey) -> (DVec3, u32) {
        match *key {
            LatticeKey::Geographic(v) => (v.normalize() * EARTH_RADIUS_M, self.seed),
            LatticeKey::Tracked {
                plate_id,
                plate_fixed,
            } => (
                plate_fixed.normalize() * EARTH_RADIUS_M,
                hash3([plate_id.0 as i32, 0, 0], self.seed ^ PLATE_SALT),
            ),
            LatticeKey::EraFixed(v) => (v.normalize() * EARTH_RADIUS_M, self.era_fixed_seed),
        }
    }
}
