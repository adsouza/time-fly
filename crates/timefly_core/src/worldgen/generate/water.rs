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

use bevy_math::{DVec2, DVec3};
use half::f16;

use super::rivers::{RegionRivers, RiverHit, SubPoint, across, chord_point, offset_point};
use crate::geo::RenderDir;
use crate::hydrology::params::{
    BANK_FREEBOARD_M, BOULDER_BREAK_M, BOULDER_BREAK_SHARE, BOULDER_CLUSTER_AREA_M2,
    BOULDER_CLUSTER_MAX, BOULDER_CLUSTER_MEMBERS, BOULDER_CLUSTER_REACH,
    BOULDER_CLUSTERS_PER_SUBPOINT_MAX, BOULDER_KEEP_FRACTION, BOULDER_MAX_RADIUS_M,
    BOULDER_MEMBER_SHARE, BOULDER_RADIUS_M, DATA_LAKE_DEPTH_RAMP_NODES, DATA_LAKE_MAX_DEPTH_M,
    FOAM_SITE_DEPTH_M, NO_WATER_DROP_M, OCEAN_STRAIT_M, RIBBON_NONE_M,
};
use crate::hydrology::{EraHydrology, HydroGraph, LakeKind, LakeTap};
use crate::terrain::{TapTexel, WaterSample, water_from_taps};
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
    /// Whether its foam collar is drawn: every foam site where the drawn-water fitting lands it
    /// ([`hold_in_drawn_water`]; the collar shader clips its foam at the drawn water, fix round
    /// 5; the region then drops the rocks in its own apron-blend band,
    /// [`region_boulders`](crate::terrain::drawn::region_boulders)).
    pub collar: bool,
    /// Its collar's V wake (radii downstream): the
    /// [`collar_wake_radii`](crate::terrain::drawn::collar_wake_radii) of its flow's speed; 0
    /// without a collar, or for a boil (a rock under the water, whose collar has no V).
    pub wake_radii: f32,
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
    /// Its index `c` among its sub-segment's clusters.
    pub index: u32,
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

    /// Its sub-segment's first sub-point: the origin of the lattices its rocks are checked
    /// against ([`DrawnLattices`]), shared by the sub-segment's clusters.
    pub fn origin(&self) -> DVec3 {
        self.a
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
const BOULDER_KEEP_SALT: u32 = 0xD3A2_646C;
const BOULDER_MEMBERS_SALT: u32 = 0x2C1B_3C6D;

/// Whether cluster `c` of the sub-segment from sub-point `(river, k)` is kept: its hash falls
/// under [`BOULDER_KEEP_FRACTION`]. A function of the cluster alone, so every region that holds
/// it keeps it, whole, or drops it.
pub(super) fn cluster_kept(river: u32, k: u32, c: u32) -> bool {
    fraction(hash3([river as i32, k as i32, c as i32], BOULDER_KEEP_SALT)) < BOULDER_KEEP_FRACTION
}

/// The guard on a region's boulders: keeps the first `cap`, returning how many it dropped.
pub(super) fn cap_boulders(boulders: &mut Vec<Boulder>, cap: usize) -> u32 {
    let dropped = boulders.len().saturating_sub(cap);
    boulders.truncate(cap);
    u32::try_from(dropped).unwrap_or(u32::MAX)
}

fn fraction(h: u32) -> f64 {
    f64::from(h) / 4_294_967_296.0
}

fn lerp(range: [f64; 2], f: f64) -> f64 {
    range[0] + (range[1] - range[0]) * f
}

/// The boulder clusters of every rapid sub-segment that could put a boulder inside the square
/// `|x|, |z| ≤ half_m` of `basis` (the centre within the square widened by the cluster's
/// reach and the channel's half-width, as far as the drawn-water fitting can move a rock), in
/// sub-segment order.
///
/// A sub-segment from sub-point `a = (river, k)` with `rapid > 0`, `L` long and `w = 2 hw` wide,
/// holds `floor(rapid × L × w / BOULDER_CLUSTER_AREA_M2 + h0)` clusters (at most
/// [`BOULDER_CLUSTERS_PER_SUBPOINT_MAX`]), `h0` the hash fraction of `(river, k)` (Task 11c fix
/// round 4: a cluster every 30–60 m along a 100–150 m full rapid, fewer on narrow ones). Every
/// value below is a hash of `(river, k, c)` for cluster `c` or of `(river, k, 6c + m)` for its
/// member `m`. With `cap = min(hw / 2, BOULDER_MAX_RADIUS_M)`:
///
/// - **Its members**: `n = max(1, floor(rapid × lerp([2, 6), u)))` (2–5 at `rapid` 1,
///   [`BOULDER_CLUSTER_MEMBERS`]).
/// - **The centre** (member 0) sits at arc parameter `t = u` and `x = c × (hw − cap)` across
///   the arc (`c` in [−1, 1]), its hashed radius `H = min(lerp(BOULDER_RADIUS_M, r), cap)`.
/// - **Member `m ≥ 1`**: hashed radius `lerp(BOULDER_MEMBER_SHARE, s) × H`, a bearing, and a
///   spread `lerp([1.1, BOULDER_CLUSTER_REACH], e)` (in spread radii, [`BoulderCluster::spread_m`]).
/// - **The breakers**: the first `ceil(n × rapid × BOULDER_BREAK_SHARE)` members, the centre
///   first, rise [`BOULDER_BREAK_M`]`[1]` (the centre) or `lerp(BOULDER_BREAK_M, b)` above the
///   water.
///
/// Only the clusters [`cluster_kept`] keeps are listed (Task 11c fix round 1), whole.
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
        let hw = f64::from(a.half_width_m);
        let length_m = a.dir.angle_between(b.dir) * EARTH_RADIUS_M;
        let rapid = f64::from(a.rapid);
        let clusters = subsegment_clusters(a, b) as i32;
        let cap = (hw / 2.0).min(BOULDER_MAX_RADIUS_M);
        let size = BOULDER_CLUSTER_MAX as i32;
        for cluster in 0..clusters {
            if !cluster_kept(a.river, a.k, cluster as u32) {
                continue;
            }
            let cell = [river, k, cluster];
            let [lo, hi] = BOULDER_CLUSTER_MEMBERS.map(f64::from);
            let hashed = lerp([lo, hi + 1.0], fraction(hash3(cell, BOULDER_MEMBERS_SALT)));
            let n = ((rapid * hashed).floor() as i32).clamp(1, size);
            let breakers = (f64::from(n) * rapid * BOULDER_BREAK_SHARE).ceil() as i32;
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
                index: cluster as u32,
                a: a.dir,
                b: b.dir,
                length_m,
                t: fraction(hash3(cell, BOULDER_ALONG_SALT)),
                x: (2.0 * fraction(hash3(cell, BOULDER_ACROSS_SALT)) - 1.0) * (hw - cap),
                reach_m: hw - cap,
                cap_m: cap,
                hashed_m: lerp(BOULDER_RADIUS_M, fraction(hash3(cell, BOULDER_RADIUS_SALT)))
                    .min(cap),
                members,
            };
            // A member lies within the reach of the centre, and fitting moves it at most to the
            // centreline.
            let margin = BOULDER_CLUSTER_REACH * cap + hw + 1.0;
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

/// How many clusters (before thinning) the sub-segment from `a` to `b` holds:
/// `floor(rapid × L × 2 hw / BOULDER_CLUSTER_AREA_M2 + h0)`, at most
/// [`BOULDER_CLUSTERS_PER_SUBPOINT_MAX`] ([`boulder_clusters`]).
pub(super) fn subsegment_clusters(a: &SubPoint, b: &SubPoint) -> u32 {
    if a.rapid <= 0.0 {
        return 0;
    }
    let h0 = fraction(hash3([a.river as i32, a.k as i32, -1], BOULDER_COUNT_SALT));
    let length_m = a.dir.angle_between(b.dir) * EARTH_RADIUS_M;
    let area = length_m * 2.0 * f64::from(a.half_width_m);
    (f64::from(a.rapid) * area / BOULDER_CLUSTER_AREA_M2 + h0)
        .floor()
        .min(f64::from(BOULDER_CLUSTERS_PER_SUBPOINT_MAX)) as u32
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

/// The texel lattices a cluster's rocks are checked against (Task 11c fix round 2): the drawn
/// water is the region's own texels bilinearly blended, and no two regions share a lattice, so
/// a rock is kept only where every one of these draws water over it. Each is a turn of the
/// lattice (degrees; it repeats every 90°) and its phase (fractions of a texel east and south),
/// spread so a rock near a bank meets a cell's corners from several sides. Fix round 4: they
/// are laid out about the sub-segment's first sub-point ([`DrawnLattices`]), not each
/// cluster's centre, so a rock's phase in each is its own and the sub-segment's clusters share
/// their texels.
pub(super) const DRAWN_LATTICES: [(f64, f64, f64); 16] = [
    (0.0, 0.15, 0.35),
    (0.0, 0.4, 0.85),
    (0.0, 0.65, 0.1),
    (0.0, 0.9, 0.6),
    (22.5, 0.4, 0.85),
    (22.5, 0.65, 0.1),
    (22.5, 0.9, 0.6),
    (22.5, 0.15, 0.35),
    (45.0, 0.65, 0.1),
    (45.0, 0.9, 0.6),
    (45.0, 0.15, 0.35),
    (45.0, 0.4, 0.85),
    (67.5, 0.9, 0.6),
    (67.5, 0.15, 0.35),
    (67.5, 0.4, 0.85),
    (67.5, 0.65, 0.1),
];

/// The texels [`DrawnLattices`] has read, by lattice, row and column, for one sub-segment
/// (`key`): the generator keeps it from one cluster to the next of the same sub-segment, so
/// each texel is read once. A cache of pure reads: what is in it never changes a result. Rows
/// and columns within [`DRAWN_CACHE_REACH`] of the origin sit in a flat array, stamped with the
/// sub-segment they were read for (hashing every lookup, or clearing the array for each
/// sub-segment, cost more than the reads it saved), the rest in a map.
#[derive(Debug)]
pub(super) struct DrawnCache {
    pub key: Option<(u32, u32)>,
    /// Which sub-segment this is (the array's entries of any other are stale).
    stamp: u32,
    near: Vec<(u32, TapTexel)>,
    far: std::collections::HashMap<(u8, i64, i64), TapTexel>,
}

/// [`DrawnCache`]'s array holds rows and columns `−this..this` about the origin.
const DRAWN_CACHE_REACH: i64 = 8;
const DRAWN_CACHE_SIDE: usize = 2 * DRAWN_CACHE_REACH as usize;

impl Default for DrawnCache {
    fn default() -> Self {
        let empty = TapTexel::new(0.0, 0.0, RIVER_NONE);
        DrawnCache {
            key: None,
            stamp: 0,
            near: vec![(0, empty); DRAWN_LATTICES.len() * DRAWN_CACHE_SIDE * DRAWN_CACHE_SIDE],
            far: Default::default(),
        }
    }
}

impl DrawnCache {
    /// Starts on sub-segment `key`, dropping another's texels.
    pub fn start(&mut self, key: (u32, u32)) {
        if self.key != Some(key) {
            self.key = Some(key);
            self.stamp = self.stamp.wrapping_add(1);
            if self.stamp == 0 {
                // Wrapped: entries stamped long ago could pass for this one's.
                self.near.iter_mut().for_each(|e| e.0 = u32::MAX);
                self.stamp = 1;
            }
            self.far.clear();
        }
    }

    /// The array slot of lattice `k`'s texel `(i, j)`, if it is near the origin.
    fn slot(k: usize, i: i64, j: i64) -> Option<usize> {
        let r = DRAWN_CACHE_REACH;
        ((-r..r).contains(&i) && (-r..r).contains(&j)).then(|| {
            let side = DRAWN_CACHE_SIDE;
            (k * side + (i + r) as usize) * side + (j + r) as usize
        })
    }

    fn get(&self, k: usize, i: i64, j: i64) -> Option<TapTexel> {
        match Self::slot(k, i, j) {
            Some(n) => {
                let (stamp, t) = self.near[n];
                (stamp == self.stamp && self.key.is_some()).then_some(t)
            }
            None => self.far.get(&(k as u8, i, j)).copied(),
        }
    }

    fn insert(&mut self, k: usize, i: i64, j: i64, t: TapTexel) {
        match Self::slot(k, i, j) {
            Some(n) => self.near[n] = (self.stamp, t),
            None => {
                self.far.insert((k as u8, i, j), t);
            }
        }
    }
}

/// The [`DRAWN_LATTICES`] about a sub-segment's first sub-point `origin` (fix round 4), texels
/// `texel_m` apart, each texel read through `bed` (the generator's own texel rule) when first
/// needed. Points are metres east and south of `origin` in its tangent plane. Every lattice
/// extends as far as it is read, so a point anywhere reads its own cell's four texels (round 2's
/// emulation built one 2 × 2 cell about each cluster's centre and clamped points beyond it to
/// the cell's edge, Important 1 of the rounds 2–3 re-review).
pub(super) struct DrawnLattices<'a> {
    origin: DVec3,
    east: DVec3,
    north: DVec3,
    texel_m: f64,
    bed: &'a dyn Fn(DVec3) -> BoulderBed,
    cache: &'a mut DrawnCache,
}

impl<'a> DrawnLattices<'a> {
    pub fn new(
        origin: DVec3,
        texel_m: f64,
        bed: &'a dyn Fn(DVec3) -> BoulderBed,
        cache: &'a mut DrawnCache,
    ) -> Self {
        let (north, east) = RegionFrame {
            center: RenderDir(origin),
        }
        .north_east();
        DrawnLattices {
            origin,
            east,
            north,
            texel_m,
            bed,
            cache,
        }
    }

    /// The bed under `dir`.
    pub fn bed(&self, dir: DVec3) -> BoulderBed {
        (self.bed)(dir)
    }

    /// `dir` in metres east and south of the origin.
    pub fn tangent(&self, dir: DVec3) -> DVec2 {
        let v = (dir - self.origin) * EARTH_RADIUS_M;
        DVec2::new(v.dot(self.east), -v.dot(self.north))
    }

    /// The direction `p` metres east and south of the origin.
    pub fn dir_of(&self, p: DVec2) -> DVec3 {
        (self.origin + (self.east * p.x - self.north * p.y) / EARTH_RADIUS_M).normalize()
    }

    /// Lattice `k`'s texel at row `i`, column `j`.
    fn texel(&mut self, k: usize, i: i64, j: i64) -> TapTexel {
        if let Some(t) = self.cache.get(k, i, j) {
            return t;
        }
        let (deg, a, b) = DRAWN_LATTICES[k];
        let (sn, c) = deg.to_radians().sin_cos();
        let (x, z) = ((j as f64 + a) * self.texel_m, (i as f64 + b) * self.texel_m);
        let dir = self.dir_of(DVec2::new(x * c - z * sn, x * sn + z * c));
        let bed = (self.bed)(dir);
        let t = TapTexel::new(bed.base_m, bed.level_m, bed.river);
        self.cache.insert(k, i, j, t);
        t
    }

    /// The water lattice `k` draws at `p` (m east and south of the origin): its cell's four
    /// texels blended exactly as a region blends its own ([`water_from_taps`]).
    pub fn sample(&mut self, k: usize, p: DVec2) -> WaterSample {
        let (deg, a, b) = DRAWN_LATTICES[k];
        let (sn, c) = deg.to_radians().sin_cos();
        let (x, z) = (p.x * c + p.y * sn, -p.x * sn + p.y * c);
        let (u, v) = (x / self.texel_m - a, z / self.texel_m - b);
        let (c0, r0) = (u.floor(), v.floor());
        let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
        let (c0, r0) = (c0 as i64, r0 as i64);
        let taps = [
            self.texel(k, r0, c0),
            self.texel(k, r0, c0 + 1),
            self.texel(k, r0 + 1, c0),
            self.texel(k, r0 + 1, c0 + 1),
        ];
        water_from_taps(&taps, fu, fv, self.texel_m)
    }

    /// Whether every lattice draws water at `p`: if so, the least ribbon less
    /// [`DRAWN_SPREAD_FACTOR`](crate::terrain::drawn::DRAWN_SPREAD_FACTOR) times their
    /// disagreement (where the lattices disagree, a region's own may draw the bank farther in).
    /// It stops at the first dry lattice.
    pub fn held(&mut self, p: DVec2) -> Option<f64> {
        use crate::terrain::drawn::DRAWN_SPREAD_FACTOR;
        let (mut lo, mut hi) = (f64::MAX, f64::MIN);
        for k in 0..DRAWN_LATTICES.len() {
            let w = self.sample(k, p);
            if !(w.thickness_m > 0.0 && w.ribbon_m >= 0.0) {
                return None;
            }
            lo = lo.min(f64::from(w.ribbon_m));
            hi = hi.max(f64::from(w.ribbon_m));
        }
        Some(lo - DRAWN_SPREAD_FACTOR * (hi - lo))
    }

    /// Whether a rock of radius `r` fits at `p`: every lattice holds its centre with a ribbon
    /// of `r` plus [`DRAWN_ROCK_MARGIN_M`](crate::terrain::drawn::DRAWN_ROCK_MARGIN_M), and
    /// draws water a radius out on all four sides.
    pub fn fits(&mut self, p: DVec2, r: f64) -> bool {
        use crate::terrain::drawn::DRAWN_ROCK_MARGIN_M;
        self.held(p)
            .is_some_and(|ribbon| ribbon >= r + DRAWN_ROCK_MARGIN_M)
            && [(r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)]
                .iter()
                .all(|&(de, ds)| self.held(p + DVec2::new(de, ds)).is_some())
    }
}

/// A rock that the drawn water doesn't hold where it was sized is moved toward its channel's
/// centreline by these shares of its distance from it, the least first...
pub(super) const FIT_MOVES: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];
/// ...and, failing every move, shrunk by these shares of its radius, the least first, down to
/// [`FIT_MIN_RADIUS_M`] (Task 11c fix round 3).
pub(super) const FIT_SHRINKS: [f64; 6] = [1.0, 0.8, 0.65, 0.5, 0.35, 0.2];
/// The smallest a fitted rock may be (m): Task 6's least radius. A rock that doesn't fit at it
/// is dropped.
pub(super) const FIT_MIN_RADIUS_M: f64 = 0.4;

/// The boulders of `cluster`, as `sized` (its members' boulders and beds, in member order),
/// fitted to the water every one of `lattices` draws, each marked `collar` where it is a foam
/// site whose collar's footprint they all draw wet.
///
/// A rock fits where [`DrawnLattices::fits`] says so. One that doesn't fit where it was sized is
/// tried at [`FIT_SHRINKS`] of its radius (the largest first, not below [`FIT_MIN_RADIUS_M`]),
/// each at [`FIT_MOVES`] of the way across to its channel's centreline (the least first, so a
/// move is always tried before a shrink; across is square to its flow, its distance from the
/// centreline its river texel's offset); the rock is dropped only if none fits (fix round 3).
/// Where it lands it is re-seated on the bed `lattices` reads there.
///
/// A breaker (a member with a rise, fix round 4, Important 2 of the rounds 2–3 re-review) must
/// still break where it lands: a try whose re-seated top stands under the water is grown to
/// break there (`(depth + rise) / 2`, within the cluster's cap) if that still fits, and passed
/// over otherwise. Only if no try breaks is the first that fits kept, under the water.
///
/// Every foam site where it lands gets a collar: a breaker with a flow the full wake of its
/// speed ([`collar_wake_radii`]), a rock under the water (a boil) none. Fix round 5: the collar
/// shader clips its foam at the region's own drawn water, so a collar's footprint is no longer
/// held to the emulated lattices (round 4 shortened or dropped the wakes near banks). A pure
/// function of the pass and the directions, so overlapping regions keep the same rocks.
///
/// [`collar_wake_radii`]: crate::terrain::drawn::collar_wake_radii
pub(super) fn hold_in_drawn_water(
    lattices: &mut DrawnLattices,
    cluster: &BoulderCluster,
    sized: Vec<(Boulder, BoulderBed)>,
) -> Vec<Boulder> {
    assert_eq!(sized.len(), cluster.members.len());
    sized
        .into_iter()
        .zip(&cluster.members)
        .filter_map(|((b, b_bed), m)| fit_rock(lattices, cluster.cap_m, b, b_bed, m.rise_m))
        .collect()
}

/// One rock of [`hold_in_drawn_water`]: fitted, re-seated and given its collar, or `None`.
pub(super) fn fit_rock(
    lattices: &mut DrawnLattices,
    cap_m: f64,
    b: Boulder,
    b_bed: BoulderBed,
    rise_m: Option<f64>,
) -> Option<Boulder> {
    use crate::terrain::drawn::collar_wake_radii;
    let p0 = lattices.tangent(b.dir);
    let r = f64::from(b.radius_m);
    // Toward the centreline: square to the flow, the offset's sign saying which side (positive
    // on the left of travel). No flow: it can only shrink.
    let [_, fe, fnorth, offset] = decode_river(b_bed.river);
    let flow = DVec2::new(f64::from(fe), -f64::from(fnorth)).try_normalize();
    // Left of travel, in (east, south): the flow turned a quarter anticlockwise seen from above,
    // which is (−south, east) in these axes.
    let to_centre = flow.map(|f| DVec2::new(f.y, -f.x) * -f64::from(offset));
    let breaks = |bed: &BoulderBed, radius: f64| bed.base_m + 2.0 * radius as f32 > bed.level_m;
    let mut first_fit: Option<(DVec2, f64, BoulderBed)> = None;
    let mut chosen = None;
    'tries: for rr in FIT_SHRINKS
        .iter()
        .map(|k| r * k)
        .filter(|&rr| rr >= FIT_MIN_RADIUS_M.min(r))
    {
        for &m in FIT_MOVES
            .iter()
            .filter(|&&m| m == 0.0 || to_centre.is_some())
        {
            let p = p0 + to_centre.unwrap_or_default() * m;
            if !lattices.fits(p, rr) {
                continue;
            }
            let landing = if p == p0 {
                b_bed
            } else {
                lattices.bed(lattices.dir_of(p))
            };
            let Some(rise) = rise_m else {
                chosen = Some((p, rr, landing));
                break 'tries;
            };
            if breaks(&landing, rr) {
                chosen = Some((p, rr, landing));
                break 'tries;
            }
            let need = (landing.depth_m() + rise) / 2.0;
            if need > rr && need <= cap_m && lattices.fits(p, need) && breaks(&landing, need) {
                chosen = Some((p, need, landing));
                break 'tries;
            }
            first_fit.get_or_insert((p, rr, landing));
        }
    }
    let (p, rr, landing) = chosen.or(first_fit)?;
    let fitted = if p == p0 && rr == r {
        b
    } else {
        let dir = if p == p0 { b.dir } else { lattices.dir_of(p) };
        boulder(dir, rr as f32, b.seed, &landing)
    };
    let mut out = Boulder {
        collar: false,
        wake_radii: 0.0,
        ..fitted
    };
    if fitted.foam_site().is_some() {
        let speed = fitted.flow_mps[0].hypot(fitted.flow_mps[1]);
        out.collar = true;
        if fitted.top_m() > fitted.level_m && speed > 1e-6 {
            out.wake_radii = collar_wake_radii(speed) as f32;
        }
    }
    Some(out)
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
        collar: false,
        wake_radii: 0.0,
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

    /// A hand-built river for the fitting's unit tests: flowing east at 5 m/s along the
    /// origin's parallel, `2 hw` wide, its level 100 m over a bed `base + slope × |offset|` (m),
    /// flat beyond the banks and below the level everywhere, so the ribbon alone bounds the
    /// water.
    /// Returns the origin and the bed under a direction.
    fn hand_built_river(
        hw: f32,
        base: f32,
        slope: f32,
    ) -> (DVec3, impl Fn(DVec3) -> BoulderBed + Copy) {
        let origin = crate::worldgen::testkit::dir(20.0, 30.0).0;
        let (north, _) = RegionFrame {
            center: RenderDir(origin),
        }
        .north_east();
        let bed = move |d: DVec3| {
            // The offset is positive on the left of travel: north.
            let offset = ((d - origin) * EARTH_RADIUS_M).dot(north) as f32;
            BoulderBed {
                base_m: base + slope * offset.abs().min(hw),
                level_m: 100.0,
                river: encode_river(hw - offset.abs(), 5.0, 0.0, offset),
            }
        };
        (origin, bed)
    }

    /// Fix round 4 (Important 3 of the rounds 2–3 re-review): `hold_in_drawn_water`'s fitting
    /// on a hand-built cell. A 3 m rock 37 m off the centreline of an 80 m river has a ribbon of
    /// 3 m, short of its radius plus the 1.5 m margin, so it is moved (by a quarter, to 27.75 m)
    /// rather than shrunk; re-seated on the bed where it lands; and given its collar there.
    #[test]
    fn fitting_moves_a_bank_rock_toward_the_centreline() {
        use crate::terrain::drawn::collar_wake_radii;
        let (origin, bed) = hand_built_river(40.0, 96.0, 0.05);
        let texel_m = 300_000.0 / 1024.0;
        let mut cache = DrawnCache::default();
        let mut lattices = DrawnLattices::new(origin, texel_m, &bed, &mut cache);
        let at = |l: &DrawnLattices, e: f64, s: f64| l.dir_of(DVec2::new(e, s));
        // 40 m east of the origin, 37 m north (south −37).
        let dir0 = at(&lattices, 40.0, -37.0);
        let bed0 = bed(dir0);
        let rock = boulder(dir0, 3.0, 77, &bed0);
        assert!(rock.foam_site().is_some() && rock.top_m() > rock.level_m);
        let offset_of = |d: DVec3| decode_river(bed(d).river)[3];
        assert!((offset_of(dir0) - 37.0).abs() < 1e-3);
        assert!(
            !lattices.fits(lattices.tangent(dir0), 3.0),
            "it doesn't fit where sized"
        );
        let got = fit_rock(&mut lattices, 15.0, rock, bed0, None).expect("fitted, not dropped");
        assert_eq!(got.radius_m, 3.0, "moved, not shrunk");
        assert!(got.radius_m as f64 >= FIT_MIN_RADIUS_M);
        let (o0, o1) = (offset_of(dir0), offset_of(got.dir));
        assert!(
            o1.abs() < o0.abs() && (o1 - 0.75 * o0).abs() < 1e-3,
            "a quarter of the way in: {o0} to {o1}"
        );
        let landed = bed(got.dir);
        assert_eq!(
            (got.base_m, got.level_m, got.flow_mps),
            (landed.base_m, landed.level_m, [5.0, 0.0]),
            "re-seated on the bed where it lands"
        );
        assert_eq!(got.wet, boulder(got.dir, 3.0, 77, &landed).wet);
        // The collar where the rock landed: a breaker's, the full wake of its speed (fix round
        // 5: the shader clips the foam at the drawn water, so the wake isn't shortened here).
        assert!(got.collar);
        assert_eq!(f64::from(got.wake_radii), collar_wake_radii(5.0));
        assert_eq!(collar_wake_radii(5.0), 13.0);
    }

    /// Fix round 4 (Important 2): a breaker that the fitting slides into deeper water is grown
    /// to break there, within its cluster's cap; past the cap it ships under the water only if
    /// no try breaks.
    #[test]
    fn a_fitted_breaker_still_breaks() {
        // The bed falls 0.15 m a metre toward the centreline.
        let (origin, bed) = hand_built_river(30.0, 94.0, 0.15);
        let texel_m = 300_000.0 / 1024.0;
        let mut cache = DrawnCache::default();
        let mut lattices = DrawnLattices::new(origin, texel_m, &bed, &mut cache);
        let dir0 = lattices.dir_of(DVec2::new(40.0, -28.0));
        let bed0 = bed(dir0);
        // 1.2 m, rising 0.6 m: top 100.6 at the bank (bed 98.2), where its 2 m ribbon is short.
        let rock = boulder(dir0, 1.2, 5, &bed0);
        assert!(rock.top_m() > rock.level_m);
        assert!(!lattices.fits(lattices.tangent(dir0), 1.2));
        // Without the breaker's rule it lands a quarter in, under the water (bed 97.15).
        let plain = fit_rock(&mut lattices, 5.0, rock, bed0, None).unwrap();
        assert_eq!(plain.radius_m, 1.2);
        assert!(plain.top_m() <= plain.level_m, "slid under: {plain:?}");
        // As a breaker it grows to (depth + rise) / 2 there, and breaks.
        let got = fit_rock(&mut lattices, 5.0, rock, bed0, Some(0.5)).unwrap();
        assert_eq!(got.dir, plain.dir, "the same move");
        let need = (f64::from(got.level_m - got.base_m) + 0.5) / 2.0;
        assert!((f64::from(got.radius_m) - need).abs() < 1e-5, "{got:?}");
        assert!(got.top_m() > got.level_m);
        // A cap too small to grow: it keeps looking for a try that breaks, and with none (every
        // try lands deeper), ships the first that fitted, under the water.
        let capped = fit_rock(&mut lattices, 1.3, rock, bed0, Some(0.5)).unwrap();
        assert_eq!(capped, plain);
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
