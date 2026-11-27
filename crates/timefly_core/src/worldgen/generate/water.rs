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
//! edge  = the lake's reach ends within LAKE_EDGE_TEXELS texels (lake_edge): some tap is off the
//!         lake, and one of eight probes LAKE_EDGE_TEXELS × texel_m away (E, NE, N, NW, W, SW,
//!         S, SE in the texel's own frame) has no tap on it
//! h1    = max(h1, L + BANK_FREEBOARD_M) on an edge (the dam: a lake's water never ends on a
//!         lattice-cell line over lower ground), and the lake is no candidate there
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
    BANK_FREEBOARD_M, BOULDER_BREAK_M, BOULDER_CLUSTER_MAX, BOULDER_CLUSTER_REACH,
    BOULDER_MAX_RADIUS_M, BOULDER_MEMBER_SHARE, BOULDER_RADIUS_M, BOULDERS_PER_SUBPOINT_MAX,
    DATA_LAKE_DEPTH_RAMP_NODES, DATA_LAKE_MAX_DEPTH_M, FOAM_SITE_DEPTH_M, NO_WATER_DROP_M,
    OCEAN_STRAIT_M, RIBBON_NONE_M,
};
use crate::hydrology::{EraHydrology, HydroGraph, LakeKind, LakeTap};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis, RegionFrame};
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
    /// The water level at `dir` (m): the inner field's texel water there.
    pub level_m: f32,
    /// The river's flow at `dir` (m/s), `[east, north]` in `dir`'s own frame, as the river
    /// texel stores it; zero where the water there is not a river.
    pub flow_mps: [f32; 2],
}

impl Boulder {
    /// Its top (m): the bed plus its height.
    pub fn top_m(&self) -> f32 {
        self.base_m + 2.0 * self.radius_m
    }

    /// Its foam site: a boulder standing in the water (its base under the level) whose top is
    /// less than [`FOAM_SITE_DEPTH_M`] under the level, or above it.
    pub fn foam_site(&self) -> Option<FoamSite> {
        (self.base_m < self.level_m && self.top_m() >= self.level_m - FOAM_SITE_DEPTH_M).then_some(
            FoamSite {
                dir: self.dir,
                radius_m: self.radius_m,
                top_m: self.top_m(),
                level_m: self.level_m,
                flow_mps: self.flow_mps,
                seed: self.seed,
            },
        )
    }
}

/// Where the water churns white around a rock at the surface (Task 11c): the shell's foam
/// collars. A function of its boulder, so of the direction and the pass.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FoamSite {
    /// Its boulder's foot (unit, era frame).
    pub dir: DVec3,
    /// Its boulder's radius (m).
    pub radius_m: f32,
    /// Its boulder's top (m).
    pub top_m: f32,
    /// The water level there (m).
    pub level_m: f32,
    /// The flow there (m/s), `[east, north]` in `dir`'s own frame ([`Boulder::flow_mps`]).
    pub flow_mps: [f32; 2],
    pub seed: u32,
}

/// The foam sites of `boulders`, in their order.
pub fn foam_sites(boulders: &[Boulder]) -> impl Iterator<Item = FoamSite> + '_ {
    boulders.iter().filter_map(Boulder::foam_site)
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

/// How far (texels) [`lake_edge`] probes for the end of a lake's reach: more than one texel,
/// so a texel's grid neighbours, a little off its own east and north, are covered.
pub const LAKE_EDGE_TEXELS: f64 = 1.5;

/// Whether `dir` lies within [`LAKE_EDGE_TEXELS`] texels of the end of lake `lake`'s reach
/// (the cells with a tap on its footprint): some tap at `dir` is off the lake, and one of eight
/// probes that far away has no tap on it. A texel there is the lake's dam ([`dam_m`]), so the
/// lake's water never ends on a lattice-cell line while the ground beyond is lower (Task 8b).
pub(super) fn lake_edge(hydro: &EraHydrology, dir: RenderDir, lake: u16, texel_m: f64) -> bool {
    let on = |d: RenderDir| {
        hydro
            .lattice
            .taps(d)
            .iter()
            .filter(|&&(v, _)| hydro.lake_of(v) == Some(lake))
            .count()
    };
    if on(dir) == 4 {
        return false;
    }
    let (north, east) = RegionFrame { center: dir }.north_east();
    let r = LAKE_EDGE_TEXELS * texel_m / EARTH_RADIUS_M;
    let d = std::f64::consts::FRAC_1_SQRT_2;
    [
        (1.0, 0.0),
        (d, d),
        (0.0, 1.0),
        (-d, d),
        (-1.0, 0.0),
        (-d, -d),
        (0.0, -1.0),
        (d, -d),
    ]
    .iter()
    .any(|&(e, n)| on(RenderDir((dir.0 + (east * e + north * n) * r).normalize())) == 0)
}

/// The ground on a lake's dam: at least `BANK_FREEBOARD_M` above the lake's level.
pub(super) fn dam_m(h: f64, lake: &LakeTap) -> f64 {
    h.max(f64::from(lake.level_m) + BANK_FREEBOARD_M)
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

/// A member of a [`BoulderCluster`]: its hashes.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct MemberHash {
    /// Its hashed radius as a share of the centre's (1 for the centre).
    pub share: f64,
    /// Its bearing from the centre (radians, from along the arc toward its left).
    pub angle: f64,
    /// Its distance from the centre, in spread radii (0 for the centre).
    pub spread: f64,
    /// A breaker's rise (m): how far its top stands above the cluster's deepest water.
    pub rise_m: Option<f64>,
    pub seed: u32,
}

/// A rapid's cluster of boulders, before the water they stand in is read: where its centre is
/// and its members' hashes, the centre first.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BoulderCluster {
    /// Its sub-segment's first sub-point `(river, k)`, and its rapid.
    pub river: u32,
    pub k: u32,
    pub rapid: f32,
    /// Its sub-segment's ends and the arc's length (m).
    a: DVec3,
    b: DVec3,
    length_m: f64,
    /// The centre's arc parameter and across offset (m).
    t: f64,
    x: f64,
    /// How far across the arc a member may sit (m): `hw − cap`.
    reach_m: f64,
    /// The largest radius any member may take (m): `min(hw / 2, BOULDER_MAX_RADIUS_M)`.
    pub cap_m: f64,
    /// The centre's hashed radius `H` (m), within the cap.
    pub hashed_m: f64,
    pub members: Vec<MemberHash>,
}

impl BoulderCluster {
    /// The point `x` metres across its arc at `t`.
    fn at(&self, t: f64, x: f64) -> DVec3 {
        let on_arc = chord_point(self.a, self.b, t);
        let normal = self.a.cross(self.b);
        if normal.length() > 1e-15 {
            offset_point(on_arc, across(on_arc, normal.normalize()), x)
        } else {
            on_arc
        }
    }

    /// Its centre (unit, era frame).
    pub fn centre(&self) -> DVec3 {
        self.at(self.t, self.x)
    }

    /// The radius its members spread by, with the water `centre_depth_m` deep over the centre's
    /// bed: `min(max(H, (depth + rise) / 2), cap)`, the centre's radius if that water were the
    /// cluster's deepest.
    pub fn spread_m(&self, centre_depth_m: f64) -> f64 {
        let rise = self.members[0].rise_m.unwrap_or(0.0);
        self.hashed_m
            .max((centre_depth_m.max(0.0) + rise) / 2.0)
            .min(self.cap_m)
    }

    /// Its members' directions, the members spread by `spread_m`: member `m ≥ 1` at its bearing
    /// and `spread × spread_m` from the centre (metres along and across the arc), clamped back
    /// into `t ∈ [0, 1]` and `|x| ≤ hw − cap`, which only brings it closer.
    pub fn member_dirs(&self, spread_m: f64) -> Vec<DVec3> {
        self.members
            .iter()
            .map(|m| {
                if m.spread == 0.0 {
                    return self.centre();
                }
                let rho = m.spread * spread_m;
                let t = if self.length_m > 0.0 {
                    (self.t + rho * m.angle.cos() / self.length_m).clamp(0.0, 1.0)
                } else {
                    self.t
                };
                let x = (self.x + rho * m.angle.sin()).clamp(-self.reach_m, self.reach_m);
                self.at(t, x)
            })
            .collect()
    }
}

const BOULDER_COUNT_SALT: u32 = 0x5BD1_E995;
const BOULDER_ALONG_SALT: u32 = 0x1B87_3593;
const BOULDER_ACROSS_SALT: u32 = 0xCC9E_2D51;
const BOULDER_RADIUS_SALT: u32 = 0xE654_6B64;
const BOULDER_SEED_SALT: u32 = 0x9E37_79B1;
const BOULDER_ANGLE_SALT: u32 = 0x85EB_CA6B;
const BOULDER_SPREAD_SALT: u32 = 0xC2B2_AE35;
const BOULDER_SHARE_SALT: u32 = 0x27D4_EB2F;
const BOULDER_BREAK_SALT: u32 = 0x1656_67B1;

fn fraction(h: u32) -> f64 {
    f64::from(h) / 4_294_967_296.0
}

fn lerp(range: [f64; 2], f: f64) -> f64 {
    range[0] + (range[1] - range[0]) * f
}

/// The boulder clusters of every rapid sub-segment that could put a boulder inside the square
/// `|x|, |z| ≤ half_m` of `basis` (the centre within the square widened by the cluster's
/// reach), in sub-segment order.
///
/// A sub-segment from sub-point `a = (river, k)` with `rapid > 0` holds
/// `n = floor(rapid × BOULDERS_PER_SUBPOINT_MAX + h0)` boulders, `h0` the hash fraction of
/// `(river, k)`, in `ceil(n / BOULDER_CLUSTER_MAX)` clusters of [`BOULDER_CLUSTER_MAX`] (the
/// last one the rest). Every value below is a hash of `(river, k, c)` for cluster `c` or of
/// `(river, k, j)` for boulder `j`. With `hw` the sub-segment's half-width and `cap =
/// min(hw / 2, BOULDER_MAX_RADIUS_M)`:
///
/// - **The centre** (member 0) sits at arc parameter `t = u` and `x = c × (hw − cap)` across
///   the arc (`c` in [−1, 1]), its hashed radius `H = min(lerp(BOULDER_RADIUS_M, r), cap)`.
/// - **Member `m ≥ 1`**: hashed radius `lerp(BOULDER_MEMBER_SHARE, s) × H`, a bearing, and a
///   spread `lerp([1.1, BOULDER_CLUSTER_REACH], e)` (in spread radii, [`BoulderCluster::spread_m`]).
/// - **The breakers**: the first `ceil(members × rapid / 3)` members, the centre first, rise
///   [`BOULDER_BREAK_M`]`[1]` (the centre) or `lerp(BOULDER_BREAK_M, b)` above the water.
pub(super) fn boulder_clusters(
    rivers: &RegionRivers,
    basis: &FrameBasis,
    half_m: f64,
) -> Vec<BoulderCluster> {
    let mut out = Vec::new();
    for (a, b) in rivers.sub_segments() {
        if a.rapid <= 0.0 {
            continue;
        }
        let (river, k) = (a.river as i32, a.k as i32);
        let h0 = fraction(hash3([river, k, -1], BOULDER_COUNT_SALT));
        let count = (f64::from(a.rapid) * f64::from(BOULDERS_PER_SUBPOINT_MAX) + h0).floor() as i32;
        let hw = f64::from(a.half_width_m);
        let cap = (hw / 2.0).min(BOULDER_MAX_RADIUS_M);
        let size = BOULDER_CLUSTER_MAX as i32;
        for cluster in 0..(count + size - 1) / size {
            let n = (count - cluster * size).min(size);
            let breakers = (f64::from(n) * f64::from(a.rapid) / 3.0).ceil() as i32;
            let cell = [river, k, cluster];
            let members = (0..n)
                .map(|m| {
                    let cell = [river, k, cluster * size + m];
                    let rise_m = (m < breakers).then(|| {
                        if m == 0 {
                            BOULDER_BREAK_M[1]
                        } else {
                            lerp(BOULDER_BREAK_M, fraction(hash3(cell, BOULDER_BREAK_SALT)))
                        }
                    });
                    let seed = hash3(cell, BOULDER_SEED_SALT);
                    if m == 0 {
                        MemberHash {
                            share: 1.0,
                            angle: 0.0,
                            spread: 0.0,
                            rise_m,
                            seed,
                        }
                    } else {
                        MemberHash {
                            share: lerp(
                                BOULDER_MEMBER_SHARE,
                                fraction(hash3(cell, BOULDER_SHARE_SALT)),
                            ),
                            angle: std::f64::consts::TAU
                                * fraction(hash3(cell, BOULDER_ANGLE_SALT)),
                            spread: lerp(
                                [1.1, BOULDER_CLUSTER_REACH],
                                fraction(hash3(cell, BOULDER_SPREAD_SALT)),
                            ),
                            rise_m,
                            seed,
                        }
                    }
                })
                .collect();
            let c = BoulderCluster {
                river: a.river,
                k: a.k,
                rapid: a.rapid,
                a: a.dir,
                b: b.dir,
                length_m: a.dir.angle_between(b.dir) * EARTH_RADIUS_M,
                t: fraction(hash3(cell, BOULDER_ALONG_SALT)),
                x: (2.0 * fraction(hash3(cell, BOULDER_ACROSS_SALT)) - 1.0) * (hw - cap),
                reach_m: hw - cap,
                cap_m: cap,
                hashed_m: lerp(BOULDER_RADIUS_M, fraction(hash3(cell, BOULDER_RADIUS_SALT)))
                    .min(cap),
                members,
            };
            let margin = BOULDER_CLUSTER_REACH * cap + 1.0;
            let near = basis
                .dir_to_local(RenderDir(c.centre()))
                .is_some_and(|(x, z)| x.abs().max(z.abs()) <= half_m + margin);
            if near {
                out.push(c);
            }
        }
    }
    out
}

/// What a boulder stands on: the bed's height, the water's level and its river texel there.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct BoulderBed {
    pub base_m: f32,
    pub level_m: f32,
    pub river: [u16; 4],
}

impl BoulderBed {
    /// The water's depth over the bed (m), at least 0.
    pub fn depth_m(&self) -> f64 {
        (f64::from(self.level_m) - f64::from(self.base_m)).max(0.0)
    }
}

/// The boulders of `cluster`, its members spread by `spread_m` to `dirs` and standing on `beds`
/// (all in member order). With `D` the deepest water over any member's bed, the centre's
/// radius is `R = min(max(spread_m, (D + rise) / 2), cap)`; a member's is `min(share × H, R)`,
/// or for a breaker `min(max(that, (D + rise) / 2), R)`. So the centre is the largest, every
/// member lies within [`BOULDER_CLUSTER_REACH`] × `R` of it and wholly in the channel, and the
/// breakers' tops stand above the water wherever the cap allows (water up to `2 × cap − rise`
/// deep).
pub(super) fn size_cluster(
    cluster: &BoulderCluster,
    spread_m: f64,
    dirs: &[DVec3],
    beds: &[BoulderBed],
) -> Vec<Boulder> {
    assert!(cluster.members.len() == dirs.len() && dirs.len() == beds.len());
    let depth = beds.iter().map(BoulderBed::depth_m).fold(0.0f64, f64::max);
    let breaking = |rise: Option<f64>| rise.map_or(0.0, |r| (depth + r) / 2.0);
    let big = spread_m
        .max(breaking(cluster.members[0].rise_m))
        .min(cluster.cap_m);
    cluster
        .members
        .iter()
        .zip(dirs.iter().zip(beds))
        .enumerate()
        .map(|(i, (m, (&dir, bed)))| {
            let radius = if i == 0 {
                big
            } else {
                (m.share * cluster.hashed_m)
                    .max(breaking(m.rise_m))
                    .min(big)
            };
            boulder(dir, radius as f32, m.seed, bed)
        })
        .collect()
}

/// A boulder at `dir` of radius `radius_m` on `bed`.
pub(super) fn boulder(dir: DVec3, radius_m: f32, seed: u32, bed: &BoulderBed) -> Boulder {
    let wet = ((bed.level_m - bed.base_m) / (2.0 * radius_m)).clamp(0.0, 1.0);
    let [_, east, north, _] = decode_river(bed.river);
    Boulder {
        dir,
        base_m: bed.base_m,
        radius_m,
        seed,
        wet: if wet.is_nan() { 0.0 } else { wet },
        level_m: bed.level_m,
        flow_mps: [east, north],
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
