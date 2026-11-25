//! The water a texel shows, and the boulders in a region's rapids (rivers and lakes design
//! §4.2, Task 6). Pure functions of the texel's terrain, the era's [`EraHydrology`] and the
//! field's refined rivers, so overlapping regions agree bit for bit (design §4.3).
//!
//! The per-texel rule, with `h0 = coastline_clamp(coarse + D, corners)` exactly as in M3:
//!
//! ```text
//! lake  = hydro.lake_tap(dir); s = min(1, shore_steps)
//! h1    = h0 − s × max(0, h0 − cap) for a Data lake, else h0,
//!         cap = L − 1 − DATA_LAKE_MAX_DEPTH_M × min(1, shore_steps / DATA_LAKE_DEPTH_RAMP_NODES)
//!         (= min(h0, cap) wherever shore_steps ≥ 1; the depth fades in over the shore cell)
//! rt    = rivers[field].texel(dir, h1, texel_m); h = rt.h_m   (the stored height)
//! candidates (level, river texel):
//!   sea   if any corner < 0 and (no pass, or an ocean node among the hydro taps, or a tap's
//!         neighbour within OCEAN_STRAIT_M of dir):        (0, RIVER_NONE)
//!   lake  if lake:                                       (L, RIVER_NONE)
//!   river if rt.hit with fade > 0:                       (hit.level, [ribbon, flow_e, flow_n, offset])
//! water = the highest level (ties: lake, sea, river), except that a river whose ribbon is ≤ 0
//!   at the texel (its water is not over the texel's centre) yields to the highest lake or sea
//!   standing above h; no candidate: (h − NO_WATER_DROP_M, RIVER_NONE).
//! ```
//!
//! The river texel is stored as f16 bits of `[ribbon_m, flow_east_mps, flow_north_mps,
//! offset_m]`, with [`RIVER_NONE`] = `[RIBBON_NONE_M, 0, 0, 0]` where the water is not a river.
//! `offset_m` is the signed distance to the centreline (positive on the left bank), which,
//! unlike the ribbon, is linear across the centreline: a reader interpolating texels recovers a
//! sub-texel river as `ribbon = (ribbon_i + |offset_i|) − |offset|` interpolated
//! ([`crate::terrain::blended_water`]).

use bevy_math::DVec3;
use half::f16;

use super::rivers::{RegionRivers, RiverHit, across, chord_point, offset_point};
use crate::geo::RenderDir;
use crate::hydrology::params::{
    BOULDER_RADIUS_M, BOULDERS_PER_SUBPOINT_MAX, DATA_LAKE_DEPTH_RAMP_NODES, DATA_LAKE_MAX_DEPTH_M,
    MAX_BOULDERS, NO_WATER_DROP_M, OCEAN_STRAIT_M, RIBBON_NONE_M,
};
use crate::hydrology::{EraHydrology, HydroGraph, LakeKind, LakeTap};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis};
use crate::worldgen::noise::hash3;

/// The river texel of water that is not a river: `[RIBBON_NONE_M, 0, 0, 0]` as f16 bits.
pub const RIVER_NONE: [u16; 4] = [f16::from_f32_const(RIBBON_NONE_M as f32).to_bits(), 0, 0, 0];

/// A boulder in a rapid (inner field only). Every field is a function of the direction and the
/// era's pass, so overlapping regions hold the same boulders where both squares contain them.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Boulder {
    /// Its centre's foot on the channel bed (unit, era frame).
    pub dir: DVec3,
    /// The inner field's carved height at `dir`: the channel bed it rests on.
    pub base_m: f32,
    pub radius_m: f32,
    /// For per-instance variation (hue, shape).
    pub seed: u32,
    /// The fraction of the boulder's height under the water surface:
    /// `clamp((level − base) / (2 × radius), 0, 1)`, for wet-darkening.
    pub wet: f32,
}

/// The f16 bits of a river texel.
pub fn encode_river(
    ribbon_m: f32,
    flow_east_mps: f32,
    flow_north_mps: f32,
    offset_m: f32,
) -> [u16; 4] {
    [ribbon_m, flow_east_mps, flow_north_mps, offset_m].map(|x| f16::from_f32(x).to_bits())
}

/// `[ribbon_m, flow_east_mps, flow_north_mps, offset_m]` from a river texel's bits.
pub fn decode_river(bits: [u16; 4]) -> [f32; 4] {
    bits.map(|b| f16::from_bits(b).to_f32())
}

/// The bed under a lake (`h1` in the module's rule).
pub(super) fn lake_bed_m(h0: f64, lake: Option<&LakeTap>) -> f64 {
    let Some(tap) = lake.filter(|t| t.kind == LakeKind::Data) else {
        return h0;
    };
    let steps = f64::from(tap.shore_steps);
    let ramp = (steps / DATA_LAKE_DEPTH_RAMP_NODES).min(1.0);
    let cap = f64::from(tap.level_m) - 1.0 - DATA_LAKE_MAX_DEPTH_M * ramp;
    h0 - steps.min(1.0) * (h0 - cap).max(0.0)
}

/// Whether an ocean node is among the hydro lattice's four taps at `dir`, or is a neighbour of
/// one within [`OCEAN_STRAIT_M`] of `dir`. The taps alone miss coastal water narrower than a
/// lattice cell (a bay, an estuary) whose cell has no ocean corner. The distance bound mirrors
/// the pass's strait rule (below-sea water within `OCEAN_STRAIT_M` of the ocean is ocean), so it
/// never reaches an inland basin the pass left inland: on the deep lattice (51 km spacing, a
/// strait reach of one step) every neighbour of a tap is farther than that, and only the taps
/// count; on the recent one (10 km) neighbours within 25 km do.
///
/// [`OCEAN_STRAIT_M`]: crate::hydrology::params::OCEAN_STRAIT_M
pub(super) fn ocean_near(hydro: &EraHydrology, dir: RenderDir) -> bool {
    let taps = hydro.lattice.taps(dir);
    if taps.iter().any(|&(v, _)| hydro.is_ocean(v)) {
        return true;
    }
    let p = dir.0.normalize();
    let reach = OCEAN_STRAIT_M / EARTH_RADIUS_M;
    taps.iter().any(|&(v, _)| {
        hydro
            .lattice
            .neighbours(v)
            .iter()
            .flatten()
            .any(|&u| hydro.is_ocean(u) && p.angle_between(hydro.lattice.dir(u).0) <= reach)
    })
}

/// A texel's water: its level and river texel.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Water {
    pub level_m: f32,
    pub river: [u16; 4],
}

/// The module's water rule at a texel of height `h`.
pub(super) fn choose_water(
    h: f32,
    sea: bool,
    lake: Option<&LakeTap>,
    hit: Option<&RiverHit>,
) -> Water {
    // Standing water, highest first (ties: the lake).
    let standing = match (lake.map(|l| l.level_m), sea) {
        (Some(l), true) if 0.0 > l => Some(0.0),
        (Some(l), _) => Some(l),
        (None, true) => Some(0.0),
        (None, false) => None,
    };
    let river = hit.filter(|h| h.fade > 0.0);
    let river_wins = match (river, standing) {
        (Some(r), Some(s)) => r.level_m > s && (r.ribbon_m > 0.0 || s <= h),
        (Some(_), None) => true,
        (None, _) => false,
    };
    if river_wins {
        let r = river.expect("the river won");
        return Water {
            level_m: r.level_m,
            river: encode_river(r.ribbon_m, r.flow_east_mps, r.flow_north_mps, r.offset_m),
        };
    }
    // The lower of a lake and the sea is never shown where the higher stands: one level per
    // texel. The yield rule above only looks at the highest standing water.
    match standing {
        Some(level_m) => Water {
            level_m,
            river: RIVER_NONE,
        },
        None => Water {
            level_m: h - NO_WATER_DROP_M as f32,
            river: RIVER_NONE,
        },
    }
}

/// Where a boulder goes, before its base is read.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct BoulderSite {
    pub dir: DVec3,
    pub radius_m: f32,
    pub seed: u32,
}

const BOULDER_COUNT_SALT: u32 = 0x5BD1_E995;
const BOULDER_ALONG_SALT: u32 = 0x1B87_3593;
const BOULDER_ACROSS_SALT: u32 = 0xCC9E_2D51;
const BOULDER_RADIUS_SALT: u32 = 0xE654_6B64;
const BOULDER_SEED_SALT: u32 = 0x9E37_79B1;

fn fraction(h: u32) -> f64 {
    f64::from(h) / 4_294_967_296.0
}

/// The boulders of every rapid sub-segment whose boulders fall inside the square `|x|, |z| ≤
/// half_m` of `basis`, in sub-segment order, at most [`MAX_BOULDERS`]; and how many more there
/// were.
///
/// A sub-segment starting at sub-point `(river, k)` with `rapid > 0` holds
/// `floor(rapid × BOULDERS_PER_SUBPOINT_MAX + h0)` boulders, `h0` the hash fraction of
/// `(river, k)`. Boulder `j`'s fractions `u`, `c`, `r` and its seed are hashes of
/// `(river, k, j)`: it sits at `t = u` along the sub-segment's arc, `c × (w/2 − radius)` across
/// it (`c` in [−1, 1]), with `radius = min(lerp(BOULDER_RADIUS_M, r), w/4)`, so it lies wholly
/// in the channel.
pub(super) fn boulder_sites(
    rivers: &RegionRivers,
    basis: &FrameBasis,
    half_m: f64,
) -> (Vec<BoulderSite>, u32) {
    let mut out = Vec::new();
    let mut dropped = 0u32;
    for (a, b) in rivers.sub_segments() {
        if a.rapid <= 0.0 {
            continue;
        }
        let (river, k) = (a.river as i32, a.k as i32);
        let h0 = fraction(hash3([river, k, -1], BOULDER_COUNT_SALT));
        let count = (f64::from(a.rapid) * f64::from(BOULDERS_PER_SUBPOINT_MAX) + h0).floor() as i32;
        let hw = f64::from(a.half_width_m);
        let normal = a.dir.cross(b.dir);
        for j in 0..count {
            let cell = [river, k, j];
            let u = fraction(hash3(cell, BOULDER_ALONG_SALT));
            let c = 2.0 * fraction(hash3(cell, BOULDER_ACROSS_SALT)) - 1.0;
            let r = fraction(hash3(cell, BOULDER_RADIUS_SALT));
            let radius = (BOULDER_RADIUS_M[0] + (BOULDER_RADIUS_M[1] - BOULDER_RADIUS_M[0]) * r)
                .min(hw / 2.0);
            let on_arc = chord_point(a.dir, b.dir, u);
            let dir = if normal.length() > 1e-15 {
                offset_point(
                    on_arc,
                    across(on_arc, normal.normalize()),
                    c * (hw - radius),
                )
            } else {
                on_arc
            };
            let inside = basis
                .dir_to_local(RenderDir(dir))
                .is_some_and(|(x, z)| x.abs() <= half_m && z.abs() <= half_m);
            if !inside {
                continue;
            }
            if out.len() >= MAX_BOULDERS as usize {
                dropped += 1;
                continue;
            }
            out.push(BoulderSite {
                dir,
                radius_m: radius as f32,
                seed: hash3(cell, BOULDER_SEED_SALT),
            });
        }
    }
    (out, dropped)
}

/// A boulder at `site`, on a texel of height `base_m` under water at `level_m`.
pub(super) fn boulder(site: &BoulderSite, base_m: f32, level_m: f32) -> Boulder {
    let wet = ((level_m - base_m) / (2.0 * site.radius_m)).clamp(0.0, 1.0);
    Boulder {
        dir: site.dir,
        base_m,
        radius_m: site.radius_m,
        seed: site.seed,
        wet: if wet.is_nan() { 0.0 } else { wet },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tap(level_m: f32, kind: LakeKind, shore_steps: f32) -> LakeTap {
        LakeTap {
            lake: 0,
            level_m,
            kind,
            shore_steps,
        }
    }

    fn hit(level_m: f32, ribbon_m: f32, fade: f32) -> RiverHit {
        RiverHit {
            level_m,
            ribbon_m,
            flow_east_mps: 1.5,
            flow_north_mps: -0.25,
            fade,
            half_width_m: 11.0,
            offset_m: 11.0 - ribbon_m,
        }
    }

    #[test]
    fn the_data_lake_bed_follows_the_ramp_inside_and_fades_at_the_edge() {
        let l = 1133.0;
        // Inside (shore_steps ≥ 1): exactly min(h0, cap).
        for (steps, cap) in [(1.0, l - 1.0 - 25.0), (2.0, l - 51.0), (5.0, l - 51.0)] {
            let t = tap(l as f32, LakeKind::Data, steps as f32);
            assert_eq!(lake_bed_m(l + 3.0, Some(&t)), cap);
            assert_eq!(
                lake_bed_m(l - 80.0, Some(&t)),
                l - 80.0,
                "a deeper bed stays"
            );
        }
        // The shore cell: continuous to h0 at the footprint's outer edge, so no cliff.
        let mut last = f64::NEG_INFINITY;
        for i in 0..=100 {
            let steps = 1.0 - f64::from(i) / 100.0;
            let t = tap(l as f32, LakeKind::Data, steps as f32);
            let h1 = lake_bed_m(l + 20.0, Some(&t));
            assert!(
                h1 >= last - 1e-9,
                "rises toward the edge: {h1} after {last}"
            );
            last = h1;
        }
        assert_eq!(last, l + 20.0);
        // Table and balance lakes keep their (real) bed; so does land off every lake.
        for kind in [LakeKind::Open, LakeKind::Terminal] {
            assert_eq!(lake_bed_m(-500.0, Some(&tap(-28.0, kind, 3.0))), -500.0);
        }
        assert_eq!(lake_bed_m(12.0, None), 12.0);
    }

    #[test]
    fn the_highest_water_wins_with_the_stated_ties() {
        let lake = tap(10.0, LakeKind::Open, 2.0);
        // Nothing: the dry level.
        assert_eq!(choose_water(5.0, false, None, None).level_m, 5.0 - 64.0);
        assert_eq!(choose_water(5.0, false, None, None).river, RIVER_NONE);
        // The sea and a lake: the higher, ties the lake.
        assert_eq!(choose_water(-3.0, true, None, None).level_m, 0.0);
        assert_eq!(choose_water(-3.0, true, Some(&lake), None).level_m, 10.0);
        let low = tap(-28.0, LakeKind::Terminal, 1.0);
        assert_eq!(choose_water(-40.0, true, Some(&low), None).level_m, 0.0);
        // A river over its channel beats lower standing water; its texel is encoded.
        let r = hit(12.0, 3.0, 1.0);
        let w = choose_water(4.0, true, Some(&lake), Some(&r));
        assert_eq!(w.level_m, 12.0);
        assert_eq!(decode_river(w.river), [3.0, 1.5, -0.25, 8.0]);
        // Ties go to the lake, then the sea, before the river.
        assert_eq!(
            choose_water(4.0, false, Some(&lake), Some(&hit(10.0, 3.0, 1.0))).river,
            RIVER_NONE
        );
        assert_eq!(
            choose_water(-4.0, true, None, Some(&hit(0.0, 3.0, 1.0))).river,
            RIVER_NONE
        );
        // A river away from its channel yields to standing water over the texel (no hole in the
        // sea at a river mouth), but not to standing water below the ground.
        let off = hit(2.0, -150.0, 1.0);
        assert_eq!(choose_water(-3.0, true, None, Some(&off)).level_m, 0.0);
        assert_eq!(choose_water(1.0, true, None, Some(&off)).level_m, 2.0);
        // A river with no fade is no candidate.
        assert_eq!(
            choose_water(1.0, false, None, Some(&hit(2.0, 1.0, 0.0))).level_m,
            1.0 - 64.0
        );
    }

    #[test]
    fn river_texels_round_trip_through_f16() {
        let bits = encode_river(-135.25, 2.5, -0.75, 146.5);
        assert_eq!(decode_river(bits), [-135.25, 2.5, -0.75, 146.5]);
        assert_eq!(
            decode_river(RIVER_NONE),
            [RIBBON_NONE_M as f32, 0.0, 0.0, 0.0]
        );
    }
}
