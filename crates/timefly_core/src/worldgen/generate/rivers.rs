//! Rivers refined on a region's own terrain (rivers and lakes design §4.2): the era's
//! continental river graph ([`RiverGraph`](crate::hydrology::RiverGraph)) relaxed onto the
//! valley floors, given meanders, an incised bed, hydraulic geometry and a speed, then carved
//! into the region's heights texel by texel ([`RegionRivers::texel`]).
//!
//! **Agreement across regions (design §4.3).** Every output is a pure function of the
//! direction, the era's [`EraHydrology`], the field's terrain (`floor_at`, the coarse lattice
//! at the field's source level) and its detail keys (`key_at`). The region frame only decides
//! *which* segments are refined: the gather below takes every segment that can touch the
//! region together with everything those segments' outputs read, and each output is computed
//! from global quantities (lattice directions, coarse path lengths measured from the river's
//! mouth, pass beds), never from the frame. Two regions therefore agree bit for bit wherever
//! both refine a segment.
//!
//! The rules, which the tests pin (`C` = [`CORRIDOR_FRACTION`] × the lattice's mean spacing,
//! `L` = [`INCISION_WINDOW_M`], `w` and `d` from [`hydraulic_geometry`]):
//!
//! ```text
//! A segment is a river node and its down link (a lattice step, Task 3). Rivers with discharge
//!   below min_q are not refined; a segment whose down node is not refined is terminal.
//! m: a point's coarse path distance to its river's end (great-circle node-to-node lengths,
//!   summed from the mouth up). Sub-point k of K = ceil(len / subpoint) sits at t = k / K,
//!   m = m_down + (1 − t)·len.
//! Gather: R = refined segments whose coarse arc comes within
//!   half_size + V(w, 2·subpoint) + 1.5·max(w, w_down) + C(1 + √2) + 2·subpoint
//!   of the region's square; D = the refined segments up to 500 m downstream of R; then every
//!   refined segment upstream of R ∪ D whose end lies within L + 500 m of path above the
//!   start of an R or D segment.
//! Relax (interior minima only, Task 6 fix round 1): a river node → the lowest interior local
//!   minimum of floor_at on a 7 × 7 grid over ±C in its own north/east: a grid point with
//!   |i|, |j| ≤ 2 (off the border) strictly lower than both row neighbours (i ± 1) or both
//!   column neighbours (j ± 1) (ties: smaller i² + j², then grid index (j + 3)·7 + (i + 3),
//!   i east, j north); none: the node stays put. Ocean, lake and ice end nodes stay put.
//!   Sub-point k (0 < k < K): the 13 samples at offsets ±C across the great circle between the
//!   relaxed ends; its raw offset is the lowest interior local minimum (|j| ≤ 5, strictly
//!   lower than samples j ± 1; ties: smaller |offset|, then the negative side, the right
//!   bank), or 0 when there is none. Next to an end whose node did not move (relaxed point ==
//!   lattice direction, or an unrelaxed ocean, lake or ice end), the raw offsets taper linearly
//!   to 0: sub-point k of the 3 nearest is scaled by k / 4 ((K − k) / 4 at the far end; the
//!   smaller weight where both apply). Offsets are then smoothed [1, 2, 1] / 4 with the end
//!   offsets 0.
//!   On a plain or a slope the minimum lies at the search's edge, and taking it sent each
//!   segment sideways out of its pinned node and back (hairpins); a real valley is an
//!   interior minimum.
//! Meander (R only): node values A = 1.5·w·(1 − smoothstep(MEANDER_FLAT_SLOPE, pass slope)),
//!   λ = 11·w, A = 0 if λ < 2 km; the pass slope is (bed − down bed) / len. A node's normal is
//!   left of the bisector of its outgoing chord and its main incoming one (largest discharge,
//!   ties lowest river index), between relaxed nodes. A terminal end takes its segment's start
//!   values and the normal left of its own chord. A, λ and the normal interpolate in t (node
//!   values exactly at t = 0 and 1); offset = A × fbm(key_at(p), λ, 2 octaves) along the
//!   normal at the relaxed point p.
//! Bed: B(m) = the pass bed lerped along the segment (down bed + (bed − down bed) × clamped
//!   fraction), non-increasing downstream; s ⊕ L walks the global graph.
//!   τ(s') = max(floor_at(s'), B(s' ⊕ L)) at the relaxed sub-point.
//!   bed(x) = min(B(x), min τ(s') over sub-points s' upstream of or at x, on every branch,
//!            with m(x) ≥ m(s') − L).
//!   level = bed − LEVEL_BELOW_BED × d(Q of the segment).
//!   slope = (bed(x ⊖ 500 m) − bed(x ⊕ 500 m)) / span, along the main branch upstream, clamped
//!   at a source or a terminal end; speed and rapid by river_speed_mps.
//! Tangent (unit flow direction at a sub-point): a central difference of the meandered
//!   centreline between nodes; at a node, the node's own meander (its A, λ and normal held)
//!   evaluated one sub-point spacing either side along its travel bisector, so every segment
//!   meeting there gives the same bits; one-sided at a terminal end.
//! Texel: candidates are the sub-segments within their valley half-width V = max(4w,
//!   w_eff/2 + 3·texel), w_eff = max(w, CHANNEL_MIN_TEXELS × texel), at great-circle distance
//!   d; level and speed lerp along the sub-segment, w and depth are its start's.
//!   flow = speed × the ends' tangents lerped along the sub-segment, projected at the texel
//!   and normalised: continuous along a river, across the switch between sub-segments and
//!   through nodes (the shader can flow-map it without tearing).
//!   cut(d) = level − depth·(1 − (2d / w_eff)²) in the channel (d ≤ w_eff / 2), else
//!     bank = level + BANK_FREEBOARD_M + VALLEY_WALL_SLOPE·(d − w_eff / 2), blended to h by
//!     smoothstep(V / 2, V, d); fade = 1 − that blend.
//!   h' = min(h, min cut). The hit is the candidate with the largest ribbon w/2 − d (ties:
//!   lower river, then lower k). Outside every candidate's channel, where the hit's fade is 1
//!   and h' is not under the water its river ends in, h' = max(h', hit.level + freeboard).
//! ```
//!
//! Why the level never rises downstream: B is non-increasing, and a term τ(s') leaves the
//! window only once m(x) < m(s') − L, where B(s' ⊕ L) ≥ B(x) already bounds it. The window and
//! the walk compare the same `m` values, so this holds exactly in floating point.

use std::collections::HashMap;
use std::f64::consts::SQRT_2;

use bevy_math::DVec3;

use super::detail::smoothstep;
use crate::geo::RenderDir;
use crate::hydrology::params::{
    BANK_FREEBOARD_M, CHANNEL_MIN_TEXELS, CORRIDOR_FRACTION, CORRIDOR_SAMPLES, DEPTH_COEFF,
    DEPTH_EXP, INCISION_WINDOW_M, LEVEL_BELOW_BED, LOWLAND_SPEED_MS, MANNING_N,
    MAX_CHANNEL_DISCHARGE_M3S, MEANDER_AMPLITUDE_PER_WIDTH, MEANDER_FLAT_SLOPE,
    MEANDER_MIN_WAVELENGTH_M, MEANDER_WAVELENGTH_PER_WIDTH, NODE_SEARCH_GRID, RAPID_SLOPE,
    RAPID_SPEED_BASE_MS, RAPID_SPEED_GAIN_MS, VALLEY_HALF_WIDTH_PER_WIDTH, VALLEY_WALL_SLOPE,
    WIDTH_COEFF, WIDTH_EXP,
};
use crate::hydrology::{CubeLattice, Down, EraHydrology, HydroGraph, RiverNode};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis, RegionFrame};
use crate::worldgen::noise::fbm;

/// Half the slope's baseline along the coarse path (m).
pub const SLOPE_HALF_M: f64 = 500.0;
/// A valley reaches this many texels past the channel's edge.
const VALLEY_TEXELS: f64 = 3.0;
/// The longest lattice step, in mean spacings: a diagonal at a face centre is 1.95.
const MAX_STEP_SPACINGS: f64 = 2.0;
/// The gather samples each coarse arc this often (m).
const GATHER_SAMPLE_M: f64 = 1000.0;
/// Distances measured in a region's local plane (azimuthal equidistant) exceed great-circle
/// ones by at most ρ / sin ρ: 1.3% at the apron's corners. Every frame-space test is widened by
/// this factor.
const LOCAL_SLACK: f64 = 1.02;
/// Units a node's relaxation costs (49 reads, about four sub-points' 13).
const NODE_UNITS: u32 = 4;
const MEANDER_SALT: u32 = 0x27D4_EB2F;

/// A channel's `(width, depth)` in metres for a discharge in m³/s: `w = 5·Q^0.5`,
/// `d = 0.27·Q^0.39`, with `Q` clamped to `[0, MAX_CHANNEL_DISCHARGE_M3S]`.
pub fn hydraulic_geometry(q_m3s: f32) -> (f32, f32) {
    let q = if q_m3s.is_nan() {
        0.0
    } else {
        f64::from(q_m3s).clamp(0.0, MAX_CHANNEL_DISCHARGE_M3S)
    };
    (
        (WIDTH_COEFF * q.powf(WIDTH_EXP)) as f32,
        (DEPTH_COEFF * q.powf(DEPTH_EXP)) as f32,
    )
}

/// `(speed, rapid)` for a channel `depth_m` deep on a bed `slope`. `rapid` is the smoothstep of
/// the slope over [`RAPID_SLOPE`]; where it is 0 the speed is Manning's
/// `depth^(2/3)·√slope / MANNING_N` clamped to the lowland range, elsewhere `3 + 3·rapid`.
pub fn river_speed_mps(depth_m: f32, slope: f32) -> (f32, f32) {
    let s = f64::from(slope).max(0.0);
    let rapid = smoothstep(RAPID_SLOPE[0], RAPID_SLOPE[1], s);
    if rapid == 0.0 {
        let v = f64::from(depth_m).max(0.0).powf(2.0 / 3.0) * s.sqrt() / MANNING_N;
        (
            v.clamp(LOWLAND_SPEED_MS[0], LOWLAND_SPEED_MS[1]) as f32,
            0.0,
        )
    } else {
        (
            (RAPID_SPEED_BASE_MS + RAPID_SPEED_GAIN_MS * rapid) as f32,
            rapid as f32,
        )
    }
}

/// The rapid weight a speed implies: `clamp((speed − 3) / 3, 0, 1)`, the inverse of
/// [`river_speed_mps`] in rapids. Mirrored by the water shader.
pub fn rapid_from_speed(speed_mps: f32) -> f32 {
    ((speed_mps - RAPID_SPEED_BASE_MS as f32) / RAPID_SPEED_GAIN_MS as f32).clamp(0.0, 1.0)
}

fn width_m(q_m3s: f32) -> f64 {
    f64::from(hydraulic_geometry(q_m3s).0)
}

/// A channel's valley half-width `V` at a texel size.
fn valley_half_width_m(w: f64, texel_m: f64) -> f64 {
    let w_eff = w.max(CHANNEL_MIN_TEXELS * texel_m);
    (VALLEY_HALF_WIDTH_PER_WIDTH * w).max(w_eff / 2.0 + VALLEY_TEXELS * texel_m)
}

/// The relaxation corridor's half-width `C` on a lattice.
fn corridor_m(lattice: &CubeLattice) -> f64 {
    CORRIDOR_FRACTION * lattice.mean_spacing_m()
}

/// How far a segment's centreline and valley can reach from its coarse arc: the valley (texels
/// up to `2·subpoint`), the meanders, the relaxation and the sub-segment slack.
fn gather_reach_m(w: f64, w_down: f64, relax_m: f64, subpoint_m: f64) -> f64 {
    valley_half_width_m(w, 2.0 * subpoint_m)
        + MEANDER_AMPLITUDE_PER_WIDTH * w.max(w_down)
        + relax_m
        + 2.0 * subpoint_m
}

/// The farthest a [`RiverBuilder`] reads `floor_at` beyond its region's square (m, in the
/// frame's local metres): a segment gathered at the edge of its reach, its start node a lattice
/// step further, the incision window and the slope baseline above it, one more step for the
/// segment that straddles the window's end, and the relaxation around that segment. Rivers are
/// widest at [`MAX_CHANNEL_DISCHARGE_M3S`]. About 111 km on the recent lattice at
/// [`SUBPOINT_M`](crate::hydrology::params::SUBPOINT_M).
pub fn river_read_reach_m(lattice: &CubeLattice, subpoint_m: f64) -> f64 {
    let relax = corridor_m(lattice) * (1.0 + SQRT_2);
    let w = width_m(f32::MAX);
    let gather =
        gather_reach_m(w, w, relax, subpoint_m) * LOCAL_SLACK + 0.5 * GATHER_SAMPLE_M + 1.0;
    let step = MAX_STEP_SPACINGS * lattice.mean_spacing_m();
    LOCAL_SLACK * (gather + 2.0 * step + INCISION_WINDOW_M + 2.0 * SLOPE_HALF_M + relax)
}

fn angle(a: DVec3, b: DVec3) -> f64 {
    a.cross(b).length().atan2(a.dot(b))
}

/// `p` moved `dist_m` along the unit tangent `dir`, on the sphere. Exactly `p` for 0.
pub(super) fn offset_point(p: DVec3, dir: DVec3, dist_m: f64) -> DVec3 {
    if dist_m == 0.0 {
        return p;
    }
    let a = dist_m / EARTH_RADIUS_M;
    (p * a.cos() + dir * a.sin()).normalize()
}

/// The unit tangent at `p` toward `q`, if they differ.
fn tangent_toward(p: DVec3, q: DVec3) -> Option<DVec3> {
    let t = q - p * p.dot(q);
    let l = t.length();
    (l > 1e-15).then(|| t / l)
}

/// NaN never wins a lowest-floor search.
fn clean(f: f64) -> f64 {
    if f.is_nan() { f64::INFINITY } else { f }
}

fn t_of(k: u32, k_max: u32) -> f64 {
    f64::from(k) / f64::from(k_max)
}

fn sub_count(len_m: f64, subpoint_m: f64) -> u32 {
    (len_m / subpoint_m).ceil().max(1.0) as u32
}

/// The point at `t` on the great circle from `p0` to `p1` (the ends exactly).
pub(super) fn chord_point(p0: DVec3, p1: DVec3, t: f64) -> DVec3 {
    if t == 0.0 {
        p0
    } else if t == 1.0 {
        p1
    } else {
        (p0 * (1.0 - t) + p1 * t).normalize()
    }
}

/// The unit normal of the chord's plane (left of travel); the coarse chord's if the relaxed
/// ends coincide.
fn chord_normal(p0: DVec3, p1: DVec3, c0: DVec3, c1: DVec3) -> DVec3 {
    let n = p0.cross(p1);
    if n.length() > 1e-12 {
        n.normalize()
    } else {
        c0.cross(c1).normalize()
    }
}

/// The unit tangent at `c` across the chord, toward its left bank.
pub(super) fn across(c: DVec3, normal: DVec3) -> DVec3 {
    (normal - c * c.dot(normal)).normalize()
}

fn end_node(n: &RiverNode, nodes: &[RiverNode]) -> u32 {
    match n.down {
        Down::River(j) => nodes[j as usize].node,
        Down::Ocean(v) | Down::Lake(_, v) | Down::Ice(v) | Down::Sink(v) => v,
    }
}

/// A river node relaxed onto the valley floor: the lowest interior local minimum of
/// `floor_at` on a 7 × 7 grid over `±C` in the node's own north and east (ties: smaller
/// offset, then grid index). A grid point is one when it is off the grid's border and strictly
/// lower than both its neighbours along its row (east-west) or along its column (north-south):
/// a valley floor falls downstream, so it is never lower than all eight neighbours, but it is
/// lower than both of its neighbours across the valley. With none (a plain, a slope), the node
/// stays put.
pub fn relax_node(lattice: &CubeLattice, node: u32, floor_at: &impl Fn(RenderDir) -> f64) -> DVec3 {
    let p = lattice.dir(node).0;
    let basis = RegionFrame {
        center: RenderDir(p),
    }
    .basis();
    let half = (NODE_SEARCH_GRID / 2) as i32;
    let side = NODE_SEARCH_GRID as i32;
    let step = corridor_m(lattice) / f64::from(half);
    let grid: Vec<(f64, DVec3)> = (-half..=half)
        .flat_map(|j| (-half..=half).map(move |i| (i, j)))
        .map(|(i, j)| {
            let d = basis
                .local_to_dir(f64::from(i) * step, -f64::from(j) * step)
                .0;
            (clean(floor_at(RenderDir(d))), d)
        })
        .collect();
    let at = |i: i32, j: i32| grid[((j + half) * side + (i + half)) as usize].0;
    let mut best = (f64::INFINITY, i32::MAX, i32::MAX, p);
    for j in 1 - half..half {
        for i in 1 - half..half {
            let f = at(i, j);
            let minimum =
                (f < at(i - 1, j) && f < at(i + 1, j)) || (f < at(i, j - 1) && f < at(i, j + 1));
            let index = (j + half) * side + (i + half);
            let r2 = i * i + j * j;
            if minimum && (f, r2, index) < (best.0, best.1, best.2) {
                best = (f, r2, index, grid[index as usize].1);
            }
        }
    }
    best.3
}

/// The raw corridor offset at chord point `c`: the lowest interior local minimum of the 13
/// samples across the chord (not at either edge, strictly lower than both neighbours; ties:
/// smaller |offset|, then the negative side), or 0 when there is none.
fn raw_offset(c: DVec3, normal: DVec3, corridor: f64, floor_at: &impl Fn(RenderDir) -> f64) -> f64 {
    let dir = across(c, normal);
    let half = (CORRIDOR_SAMPLES / 2) as i32;
    let offset = |j: i32| f64::from(j) * corridor / f64::from(half);
    let f: Vec<f64> = (-half..=half)
        .map(|j| clean(floor_at(RenderDir(offset_point(c, dir, offset(j))))))
        .collect();
    let at = |j: i32| f[(j + half) as usize];
    let mut best = (f64::INFINITY, i32::MAX, true, 0.0);
    for j in 1 - half..half {
        let key = (at(j), j.abs(), j > 0);
        if at(j) < at(j - 1) && at(j) < at(j + 1) && key < (best.0, best.1, best.2) {
            best = (key.0, key.1, key.2, offset(j));
        }
    }
    best.3
}

/// Sub-points next to an end whose node did not move over which the raw offsets taper to 0.
const TAPER_SUBPOINTS: u32 = 3;

/// Tapers the raw offsets `0..=K` linearly to 0 over the [`TAPER_SUBPOINTS`] sub-points next
/// to each pinned end (a node that did not move): sub-point `k` of the first `TAPER_SUBPOINTS`
/// is scaled by `k / (TAPER_SUBPOINTS + 1)`, likewise `K − k` at the far end, the smaller weight
/// where both apply. A valley beside a pinned node is then entered gradually instead of in one
/// smoothed sideways leg (Task 6 fix round 2).
fn taper(raw: &mut [f64], start_pinned: bool, end_pinned: bool) {
    let k_max = raw.len() - 1;
    let span = f64::from(TAPER_SUBPOINTS + 1);
    for (k, r) in raw.iter_mut().enumerate() {
        let mut w: f64 = 1.0;
        if start_pinned {
            w = w.min(k as f64 / span);
        }
        if end_pinned {
            w = w.min((k_max - k) as f64 / span);
        }
        if w < 1.0 {
            *r *= w;
        }
    }
}

/// The relaxed points `0..=K` from the raw offsets (ends 0): smoothed [1, 2, 1] / 4.
fn smoothed_points(p0: DVec3, p1: DVec3, normal: DVec3, raw: &[f64]) -> Vec<DVec3> {
    let k_max = (raw.len() - 1) as u32;
    (0..=k_max)
        .map(|k| {
            if k == 0 {
                return p0;
            }
            if k == k_max {
                return p1;
            }
            let i = k as usize;
            let s = (raw[i - 1] + 2.0 * raw[i] + raw[i + 1]) * 0.25;
            let c = chord_point(p0, p1, t_of(k, k_max));
            offset_point(c, across(c, normal), s)
        })
        .collect()
}

/// The relaxed, pre-meander centreline of river segment `river` (river node `river` to its down
/// node): `K + 1` points for `K = ceil(length / subpoint_m)`, both ends included. The start is
/// [`relax_node`]; the end too when it is a river node (an ocean, lake or ice node stays put).
/// A pure function of the graph and `floor_at`: what [`RiverBuilder`] refines, before
/// meanders, for any caller with its own terrain (Task 11's reality check).
pub fn relaxed_centreline(
    hydro: &EraHydrology,
    river: u32,
    subpoint_m: f64,
    floor_at: &impl Fn(RenderDir) -> f64,
) -> Vec<DVec3> {
    let lattice = hydro.lattice;
    let nodes = hydro.rivers.nodes();
    let n = &nodes[river as usize];
    let end = end_node(n, nodes);
    let (c0, c1) = (lattice.dir(n.node).0, lattice.dir(end).0);
    let p0 = relax_node(&lattice, n.node, floor_at);
    let p1 = if matches!(n.down, Down::River(_)) {
        relax_node(&lattice, end, floor_at)
    } else {
        c1
    };
    let k_max = sub_count(lattice.distance_m(n.node, end), subpoint_m);
    let normal = chord_normal(p0, p1, c0, c1);
    let corridor = corridor_m(&lattice);
    let mut raw: Vec<f64> = (0..=k_max)
        .map(|k| {
            if k == 0 || k == k_max {
                0.0
            } else {
                raw_offset(
                    chord_point(p0, p1, t_of(k, k_max)),
                    normal,
                    corridor,
                    floor_at,
                )
            }
        })
        .collect();
    taper(&mut raw, p0 == c0, p1 == c1);
    smoothed_points(p0, p1, normal, &raw)
}

/// Path lengths and pass beds along the global graph, memoised.
struct PassPath<'a> {
    hydro: &'a EraHydrology,
    /// River index → (`m` at the node: its path length to the river's end; the segment's
    /// length; the level of the water its river ends in, −∞ at ice).
    memo: HashMap<u32, (f64, f64, f32)>,
}

impl<'a> PassPath<'a> {
    fn new(hydro: &'a EraHydrology) -> Self {
        Self {
            hydro,
            memo: HashMap::new(),
        }
    }

    fn node(&self, i: u32) -> &RiverNode {
        &self.hydro.rivers.nodes()[i as usize]
    }

    /// Fills the memo for `i` and everything downstream of it, summing from the mouth up.
    fn ensure(&mut self, i: u32) {
        let mut chain = Vec::new();
        let mut j = i;
        while !self.memo.contains_key(&j) {
            chain.push(j);
            match self.node(j).down {
                Down::River(k) => j = k,
                _ => break,
            }
        }
        let nodes = self.hydro.rivers.nodes();
        for &j in chain.iter().rev() {
            let n = &nodes[j as usize];
            let len = self.hydro.lattice.distance_m(n.node, end_node(n, nodes));
            let (below, level) = match n.down {
                Down::River(k) => {
                    let m = self.memo[&k];
                    (m.0, m.2)
                }
                Down::Ocean(_) => (0.0, 0.0),
                Down::Lake(l, _) => (0.0, self.hydro.lakes[l as usize].level_m),
                Down::Ice(_) | Down::Sink(_) => (0.0, f32::NEG_INFINITY),
            };
            self.memo.insert(j, (below + len, len, level));
        }
    }

    fn m_start(&self, i: u32) -> f64 {
        self.memo[&i].0
    }

    fn len(&self, i: u32) -> f64 {
        self.memo[&i].1
    }

    fn end_level(&self, i: u32) -> f32 {
        self.memo[&i].2
    }

    fn m_down(&self, i: u32) -> f64 {
        match self.node(i).down {
            Down::River(k) => self.memo[&k].0,
            _ => 0.0,
        }
    }

    /// `B` at `m` on segment `i`: its pass bed lerped toward its down bed, the fraction clamped.
    fn seg_bed(&self, i: u32, m: f64) -> f64 {
        let n = self.node(i);
        let bed = f64::from(n.bed_m);
        let below = f64::from(self.hydro.rivers.down_bed_m(i));
        let frac = ((m - self.m_down(i)) / self.len(i)).clamp(0.0, 1.0);
        below + (bed - below) * frac
    }

    /// `B` at `m` on the path down from segment `i`: past the end, the end's bed.
    fn pass_bed(&self, i: u32, m: f64) -> f64 {
        let mut j = i;
        loop {
            if self.m_down(j) <= m {
                return self.seg_bed(j, m);
            }
            match self.node(j).down {
                Down::River(k) => j = k,
                _ => return self.seg_bed(j, m),
            }
        }
    }
}

/// One refined point of a river.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SubPoint {
    /// The centreline, meanders included (unit, era frame).
    pub dir: DVec3,
    /// `floor_at` at the relaxed (pre-meander) point.
    pub coarse_m: f64,
    /// The incised bed: the valley floor the channel runs in.
    pub bed_m: f32,
    /// The water surface.
    pub level_m: f32,
    pub half_width_m: f32,
    pub depth_m: f32,
    pub speed_mps: f32,
    pub rapid: f32,
    /// The unit flow direction, tangent at `dir` (see the module's tangent rule). Texel flow
    /// lerps it along each sub-segment.
    pub tangent: DVec3,
    /// The river node whose segment this is.
    pub river: u32,
    /// The index along the segment.
    pub k: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Role {
    /// Can touch the region: refined in full.
    Reach,
    /// Within 500 m downstream of a reach segment: the slope's far end.
    Down,
    /// Upstream within the incision window: relaxed for its floor only.
    Window,
}

/// A gathered segment and its relaxation.
#[derive(Clone, Debug)]
struct Seg {
    river: u32,
    start: u32,
    end: u32,
    /// The end is a river node, relaxed like any other.
    end_relaxed: bool,
    /// The refined river downstream, if any (none: terminal).
    down: Option<u32>,
    down_seg: Option<usize>,
    /// Gathered refined upstream segments.
    ups: Vec<usize>,
    main_up: Option<usize>,
    role: Role,
    q: f32,
    len: f64,
    m_down: f64,
    m_start: f64,
    k_max: u32,
    raw: Vec<f64>,
    points: Vec<DVec3>,
    floor: Vec<f64>,
    tau: Vec<f64>,
}

impl Seg {
    fn last_k(&self) -> u32 {
        if self.down.is_none() {
            self.k_max
        } else {
            self.k_max - 1
        }
    }

    fn m_at(&self, k: u32) -> f64 {
        self.m_down + (1.0 - t_of(k, self.k_max)) * self.len
    }
}

/// Where a [`RiverBuilder`] is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Relax(usize),
    Corridor(usize, u32),
    Points(usize, u32),
    Done,
}

/// A region's refined river segment.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "`river` and `end_dir` are read by the shared-node test"
    )
)]
struct RiverSeg {
    river: u32,
    /// Its sub-points: `subpoints[first..first + count]`.
    first: u32,
    count: u32,
    /// The down segment's first sub-point, which closes this one's last sub-segment.
    next: Option<SubPoint>,
    /// This segment's own meandered point at t = 1 (equal to `next.dir` where there is one).
    end_dir: DVec3,
    end_level_m: f32,
}

/// Refines the rivers that can touch a region, resumably ([`step`](Self::step)). One unit of
/// work is a sub-point (a node's relaxation counts four).
pub struct RiverBuilder<'a> {
    hydro: &'a EraHydrology,
    path: PassPath<'a>,
    basis: FrameBasis,
    subpoint_m: f64,
    corridor: f64,
    segs: Vec<Seg>,
    /// Lattice nodes to relax, ascending, with their relaxed directions once done.
    relaxed: Vec<(u32, DVec3)>,
    /// Reach segments, ascending river.
    reach: Vec<usize>,
    phase: Phase,
    subpoints: Vec<SubPoint>,
    out: Vec<RiverSeg>,
    /// Work units done so far.
    units: u64,
}

impl<'a> RiverBuilder<'a> {
    /// Gathers the segments the region needs (no terrain reads). `half_size_m` is the region's
    /// half side, `subpoint_m` the sub-point spacing (texel queries must use texels no larger
    /// than twice it), `min_q` the smallest discharge refined.
    pub fn new(
        hydro: &'a EraHydrology,
        frame: &RegionFrame,
        half_size_m: f64,
        subpoint_m: f64,
        min_q: f32,
    ) -> Self {
        let lattice = hydro.lattice;
        let graph = &hydro.rivers;
        let nodes = graph.nodes();
        let basis = frame.basis();
        let corridor = corridor_m(&lattice);
        let relax = corridor * (1.0 + SQRT_2);
        let step = MAX_STEP_SPACINGS * lattice.mean_spacing_m();
        let refined = |i: u32| nodes[i as usize].discharge_m3s >= min_q;
        let refined_down = |i: u32| match nodes[i as usize].down {
            Down::River(j) if refined(j) => Some(j),
            _ => None,
        };
        let mut path = PassPath::new(hydro);

        // R: segments within reach of the square.
        let cap = width_m(f32::MAX);
        let radius = LOCAL_SLACK
            * (half_size_m * SQRT_2 + gather_reach_m(cap, cap, relax, subpoint_m))
            + step
            + GATHER_SAMPLE_M;
        let mut near = Vec::new();
        graph.near(&lattice, frame.center, radius, &mut near);
        let mut reach: Vec<u32> = Vec::new();
        for &i in &near {
            if !refined(i) {
                continue;
            }
            let n = &nodes[i as usize];
            let w = width_m(n.discharge_m3s);
            let w_down = match n.down {
                Down::River(j) => width_m(nodes[j as usize].discharge_m3s),
                _ => w,
            };
            let r = LOCAL_SLACK * gather_reach_m(w, w_down, relax, subpoint_m);
            let (a, b) = (lattice.dir(n.node).0, lattice.dir(end_node(n, nodes)).0);
            if arc_near_square(&basis, a, b, half_size_m, r) {
                reach.push(i);
            }
        }
        // D: up to 500 m downstream of R.
        let mut downs: Vec<u32> = Vec::new();
        for &i in &reach {
            let mut left = SLOPE_HALF_M;
            let mut next = refined_down(i);
            while let Some(j) = next {
                downs.push(j);
                path.ensure(j);
                let len = path.len(j);
                if len >= left {
                    break;
                }
                left -= len;
                next = refined_down(j);
            }
        }
        // The window: upstream of R ∪ D, by an absolute threshold on m.
        let mut threshold: HashMap<u32, f64> = HashMap::new();
        let mut work: Vec<u32> = Vec::new();
        for &j in reach.iter().chain(&downs) {
            path.ensure(j);
            let t = path.m_start(j) + SLOPE_HALF_M + INCISION_WINDOW_M;
            let e = threshold.entry(j).or_insert(f64::NEG_INFINITY);
            if t > *e {
                *e = t;
                work.push(j);
            }
        }
        while let Some(j) = work.pop() {
            let t = threshold[&j];
            path.ensure(j);
            if path.m_start(j) > t {
                continue;
            }
            for &u in graph.upstream(j) {
                if !refined(u) {
                    continue;
                }
                let e = threshold.entry(u).or_insert(f64::NEG_INFINITY);
                if t > *e {
                    *e = t;
                    work.push(u);
                }
            }
        }
        let mut gathered: Vec<u32> = threshold.keys().copied().collect();
        gathered.sort_unstable();
        reach.sort_unstable();
        downs.sort_unstable();
        downs.dedup();

        let mut segs: Vec<Seg> = gathered
            .iter()
            .map(|&i| {
                path.ensure(i);
                let n = &nodes[i as usize];
                let len = path.len(i);
                let role = if reach.binary_search(&i).is_ok() {
                    Role::Reach
                } else if downs.binary_search(&i).is_ok() {
                    Role::Down
                } else {
                    Role::Window
                };
                Seg {
                    river: i,
                    start: n.node,
                    end: end_node(n, nodes),
                    end_relaxed: matches!(n.down, Down::River(_)),
                    down: refined_down(i),
                    down_seg: None,
                    ups: Vec::new(),
                    main_up: None,
                    role,
                    q: n.discharge_m3s,
                    len,
                    m_down: path.m_down(i),
                    m_start: path.m_start(i),
                    k_max: sub_count(len, subpoint_m),
                    raw: Vec::new(),
                    points: Vec::new(),
                    floor: Vec::new(),
                    tau: Vec::new(),
                }
            })
            .collect();
        let index = |i: u32| gathered.binary_search(&i).ok();
        for seg in &mut segs {
            seg.down_seg = seg.down.and_then(index);
            let ups: Vec<u32> = graph
                .upstream(seg.river)
                .iter()
                .copied()
                .filter(|&u| refined(u))
                .collect();
            seg.ups = ups.iter().filter_map(|&u| index(u)).collect();
            // Largest discharge, ties lowest river index (the list is ascending).
            let main = ups.iter().copied().fold(None::<u32>, |best, u| match best {
                Some(b) if nodes[b as usize].discharge_m3s >= nodes[u as usize].discharge_m3s => {
                    Some(b)
                }
                _ => Some(u),
            });
            seg.main_up = main.and_then(index);
            debug_assert!(
                seg.role == Role::Window || main.is_none() || seg.main_up.is_some(),
                "a reach segment's main branch is gathered"
            );
        }
        let mut relaxed: Vec<(u32, DVec3)> = segs
            .iter()
            .flat_map(|s| std::iter::once(s.start).chain(s.end_relaxed.then_some(s.end)))
            .map(|v| (v, DVec3::ZERO))
            .collect();
        relaxed.sort_unstable_by_key(|r| r.0);
        relaxed.dedup_by_key(|r| r.0);
        let reach_segs = (0..segs.len())
            .filter(|&s| segs[s].role == Role::Reach)
            .collect();
        RiverBuilder {
            hydro,
            path,
            basis,
            subpoint_m,
            corridor,
            segs,
            relaxed,
            reach: reach_segs,
            phase: Phase::Relax(0),
            subpoints: Vec::new(),
            out: Vec::new(),
            units: 0,
        }
    }

    /// Work units done so far by [`step`](Self::step): a progress measure, and what a caller
    /// time-boxing the steps charges for them.
    pub fn units_done(&self) -> u64 {
        self.units
    }

    /// Does about `budget` units of work (at least one) and returns `true` once the rivers are
    /// complete. `floor_at` is the field lattice's coarse elevation at the field's level and
    /// `key_at` the detail noise input of a direction's lattice key; both must be the same on
    /// every call. The result is the same for any budgets.
    pub fn step(
        &mut self,
        floor_at: &impl Fn(RenderDir) -> f64,
        key_at: &impl Fn(RenderDir) -> (DVec3, u32),
        budget: u32,
    ) -> bool {
        let budget = budget.max(1);
        let mut used = 0u32;
        while used < budget {
            match self.phase {
                Phase::Relax(i) => {
                    if i == self.relaxed.len() {
                        self.phase = Phase::Corridor(0, 0);
                        continue;
                    }
                    let node = self.relaxed[i].0;
                    self.relaxed[i].1 = relax_node(&self.hydro.lattice, node, floor_at);
                    used = used.saturating_add(NODE_UNITS);
                    self.phase = Phase::Relax(i + 1);
                }
                Phase::Corridor(s, k) => {
                    if s == self.segs.len() {
                        self.phase = Phase::Points(0, 0);
                        continue;
                    }
                    used += 1;
                    self.phase = if self.corridor_step(s, k, floor_at) {
                        Phase::Corridor(s + 1, 0)
                    } else {
                        Phase::Corridor(s, k + 1)
                    };
                }
                Phase::Points(r, k) => {
                    if r == self.reach.len() {
                        self.phase = Phase::Done;
                        continue;
                    }
                    used += 1;
                    let s = self.reach[r];
                    let sp = self.sub_point(s, k, key_at);
                    self.subpoints.push(sp);
                    self.phase = if k == self.segs[s].last_k() {
                        self.close_segment(s, key_at);
                        Phase::Points(r + 1, 0)
                    } else {
                        Phase::Points(r, k + 1)
                    };
                }
                Phase::Done => {
                    self.units += u64::from(used);
                    return true;
                }
            }
        }
        self.units += u64::from(used);
        self.phase == Phase::Done
    }

    fn relaxed_dir(&self, node: u32) -> DVec3 {
        let k = self
            .relaxed
            .binary_search_by_key(&node, |r| r.0)
            .expect("a gathered node");
        self.relaxed[k].1
    }

    /// The relaxed ends and chord normal of segment `s`.
    fn ends(&self, s: usize) -> (DVec3, DVec3, DVec3) {
        let seg = &self.segs[s];
        let lattice = &self.hydro.lattice;
        let (c0, c1) = (lattice.dir(seg.start).0, lattice.dir(seg.end).0);
        let p0 = self.relaxed_dir(seg.start);
        let p1 = if seg.end_relaxed {
            self.relaxed_dir(seg.end)
        } else {
            c1
        };
        (p0, p1, chord_normal(p0, p1, c0, c1))
    }

    /// One unit of segment `s`'s corridor: raw offset `k` (the ends are 0). Returns `true`
    /// when the segment is complete (points, floors and τ filled).
    fn corridor_step(&mut self, s: usize, k: u32, floor_at: &impl Fn(RenderDir) -> f64) -> bool {
        let (p0, p1, normal) = self.ends(s);
        let k_max = self.segs[s].k_max;
        if k == 0 {
            self.segs[s].raw = vec![0.0; k_max as usize + 1];
        } else if k < k_max {
            let c = chord_point(p0, p1, t_of(k, k_max));
            self.segs[s].raw[k as usize] = raw_offset(c, normal, self.corridor, floor_at);
        }
        if k + 1 < k_max {
            return false;
        }
        let lattice = &self.hydro.lattice;
        let (c0, c1) = (
            lattice.dir(self.segs[s].start).0,
            lattice.dir(self.segs[s].end).0,
        );
        taper(&mut self.segs[s].raw, p0 == c0, p1 == c1);
        let points = smoothed_points(p0, p1, normal, &self.segs[s].raw);
        let seg = &self.segs[s];
        let floor: Vec<f64> = (0..=seg.last_k())
            .map(|k| clean(floor_at(RenderDir(points[k as usize]))))
            .collect();
        let tau = (0..=seg.last_k())
            .map(|k| {
                let after = self
                    .path
                    .pass_bed(seg.river, seg.m_at(k) - INCISION_WINDOW_M);
                floor[k as usize].max(after)
            })
            .collect();
        let seg = &mut self.segs[s];
        seg.points = points;
        seg.floor = floor;
        seg.tau = tau;
        seg.raw = Vec::new();
        true
    }

    /// The smallest τ over the sub-points in the window of the point at `m_x` on segment `s`.
    fn window_into(&self, s: usize, m_x: f64, best: &mut f64) {
        let seg = &self.segs[s];
        for k in (0..=seg.last_k()).rev() {
            let m = seg.m_at(k);
            if m < m_x {
                continue;
            }
            if m - INCISION_WINDOW_M > m_x {
                return;
            }
            *best = best.min(seg.tau[k as usize]);
        }
        for &u in &seg.ups {
            self.window_into(u, m_x, best);
        }
    }

    /// The incised bed at `m` on segment `s`.
    fn bed_at(&self, s: usize, m: f64) -> f64 {
        let mut best = f64::INFINITY;
        self.window_into(s, m, &mut best);
        self.path.seg_bed(self.segs[s].river, m).min(best)
    }

    /// The point 500 m up the main branch from `m` on `s`, clamped at a source.
    fn up_point(&self, mut s: usize, m: f64) -> (usize, f64) {
        let target = m + SLOPE_HALF_M;
        loop {
            if target <= self.segs[s].m_start {
                return (s, target);
            }
            match self.segs[s].main_up {
                Some(u) => s = u,
                None => return (s, self.segs[s].m_start),
            }
        }
    }

    /// The point 500 m downstream from `m` on `s`, clamped at the refined river's end.
    fn down_point(&self, mut s: usize, m: f64) -> (usize, f64) {
        let target = m - SLOPE_HALF_M;
        loop {
            if target >= self.segs[s].m_down {
                return (s, target);
            }
            match self.segs[s].down_seg {
                Some(d) => s = d,
                None => return (s, self.segs[s].m_down),
            }
        }
    }

    fn slope_at(&self, s: usize, m: f64) -> f64 {
        let (su, mu) = self.up_point(s, m);
        let (sd, md) = self.down_point(s, m);
        let span = mu - md;
        if span > 0.0 {
            ((self.bed_at(su, mu) - self.bed_at(sd, md)) / span).max(0.0)
        } else {
            0.0
        }
    }

    /// A river node's meander amplitude and wavelength.
    fn node_values(&self, river: u32) -> (f64, f64) {
        let n = self.path.node(river);
        let w = width_m(n.discharge_m3s);
        let lambda = MEANDER_WAVELENGTH_PER_WIDTH * w;
        if lambda < MEANDER_MIN_WAVELENGTH_M {
            return (0.0, lambda);
        }
        let slope = (f64::from(n.bed_m) - f64::from(self.hydro.rivers.down_bed_m(river)))
            / self.path.len(river);
        let flat = 1.0 - smoothstep(MEANDER_FLAT_SLOPE[0], MEANDER_FLAT_SLOPE[1], slope);
        (MEANDER_AMPLITUDE_PER_WIDTH * w * flat, lambda)
    }

    /// The travel direction at segment `s`'s start node, before meanders: the bisector of its
    /// outgoing chord and its main incoming one (unit, tangent at the relaxed node).
    fn node_travel(&self, s: usize) -> DVec3 {
        let seg = &self.segs[s];
        let p = seg.points[0];
        let k_max = seg.k_max as usize;
        let out = tangent_toward(p, seg.points[k_max]).unwrap_or(DVec3::ZERO);
        let into = seg
            .main_up
            .and_then(|u| tangent_toward(p, self.segs[u].points[0]))
            .map_or(DVec3::ZERO, |t| -t);
        let b = out + into;
        if b.length() > 1e-9 {
            b.normalize()
        } else {
            out
        }
    }

    /// The meander normal at segment `s`'s start node: left of its travel direction.
    fn node_normal(&self, s: usize) -> DVec3 {
        let p = self.segs[s].points[0];
        p.cross(self.node_travel(s)).normalize()
    }

    /// The flow direction at segment `s`'s start node, after meanders: the node's own meander
    /// (its amplitude, wavelength and normal held) evaluated one sub-point spacing either side
    /// of the node along its travel direction, as a central difference. It depends only on the
    /// node, so every segment meeting there gives the same bits.
    fn node_tangent(&self, s: usize, key_at: &impl Fn(RenderDir) -> (DVec3, u32)) -> DVec3 {
        let seg = &self.segs[s];
        let p = seg.points[0];
        let travel = self.node_travel(s);
        let (a, lambda) = self.node_values(seg.river);
        let normal = self.node_normal(s);
        let here = meander_point(p, a, lambda, normal, key_at);
        let h = seg.len / f64::from(seg.k_max);
        let side =
            |sign: f64| meander_point(offset_point(p, travel, sign * h), a, lambda, normal, key_at);
        let t = side(1.0) - side(-1.0);
        let t = t - here * here.dot(t);
        if t.length() > 1e-15 {
            t.normalize()
        } else {
            travel
        }
    }

    /// The meander normal at a terminal segment's end: left of its own chord.
    fn terminal_normal(&self, s: usize) -> DVec3 {
        let seg = &self.segs[s];
        let p = seg.points[seg.k_max as usize];
        let into = tangent_toward(p, seg.points[0]).map_or(DVec3::ZERO, |t| -t);
        p.cross(into).normalize()
    }

    /// Segment `s`'s meandered point `k`.
    fn meander(&self, s: usize, k: u32, key_at: &impl Fn(RenderDir) -> (DVec3, u32)) -> DVec3 {
        let seg = &self.segs[s];
        let p = seg.points[k as usize];
        let t = t_of(k, seg.k_max);
        let start = || (self.node_values(seg.river), self.node_normal(s));
        let end = || match seg.down_seg {
            Some(v) => (self.node_values(self.segs[v].river), self.node_normal(v)),
            None => (self.node_values(seg.river), self.terminal_normal(s)),
        };
        let ((a, lambda), n) = if t == 0.0 {
            start()
        } else if t == 1.0 {
            end()
        } else {
            let ((a0, l0), n0) = start();
            let ((a1, l1), n1) = end();
            (
                ((1.0 - t) * a0 + t * a1, (1.0 - t) * l0 + t * l1),
                n0 * (1.0 - t) + n1 * t,
            )
        };
        meander_point(p, a, lambda, n, key_at)
    }

    /// Sub-point `k` of segment `s`, its tangent left for [`close_segment`](Self::close_segment).
    /// Only reach segments meander past their start.
    fn sub_point(&self, s: usize, k: u32, key_at: &impl Fn(RenderDir) -> (DVec3, u32)) -> SubPoint {
        let seg = &self.segs[s];
        let m = seg.m_at(k);
        let bed = self.bed_at(s, m);
        let (w, d) = hydraulic_geometry(seg.q);
        let level = bed - LEVEL_BELOW_BED * f64::from(d);
        let (speed, rapid) = river_speed_mps(d, self.slope_at(s, m) as f32);
        let dir = if seg.role == Role::Reach || k == 0 {
            self.meander(s, k, key_at)
        } else {
            seg.points[k as usize]
        };
        SubPoint {
            dir,
            coarse_m: seg.floor[k as usize],
            bed_m: bed as f32,
            level_m: level as f32,
            half_width_m: w / 2.0,
            depth_m: d,
            speed_mps: speed,
            rapid,
            tangent: DVec3::ZERO,
            river: seg.river,
            k,
        }
    }

    /// Finishes reach segment `s` once its sub-points are out: its end point, the next
    /// segment's start, and the tangents.
    fn close_segment(&mut self, s: usize, key_at: &impl Fn(RenderDir) -> (DVec3, u32)) {
        let seg = &self.segs[s];
        let count = seg.last_k() + 1;
        let first = self.subpoints.len() as u32 - count;
        let end_dir = self.meander(s, seg.k_max, key_at);
        let mut next = seg.down_seg.map(|v| self.sub_point(v, 0, key_at));
        let mut dirs: Vec<DVec3> = self.subpoints[first as usize..]
            .iter()
            .map(|p| p.dir)
            .collect();
        if let Some(n) = &next {
            dirs.push(n.dir);
        }
        let fallback =
            tangent_toward(seg.points[0], seg.points[seg.k_max as usize]).unwrap_or(DVec3::ZERO);
        // Central differences between nodes; at a node, the node's own rule (shared by every
        // segment meeting there); one-sided at a terminal end.
        let tangent = |k: usize| {
            let prev = dirs[k.saturating_sub(1)];
            let after = dirs[(k + 1).min(dirs.len() - 1)];
            let p = dirs[k];
            let t = after - prev;
            let t = t - p * p.dot(t);
            if t.length() > 1e-15 {
                t.normalize()
            } else {
                fallback
            }
        };
        let start = self.node_tangent(s, key_at);
        for k in 0..count as usize {
            self.subpoints[first as usize + k].tangent = if k == 0 { start } else { tangent(k) };
        }
        if let Some(n) = &mut next {
            n.tangent = self.node_tangent(seg.down_seg.expect("next has a segment"), key_at);
        }
        self.out.push(RiverSeg {
            river: seg.river,
            first,
            count,
            next,
            end_dir,
            end_level_m: self.path.end_level(seg.river),
        });
    }

    /// The refined rivers. Debug builds check that [`step`](Self::step) returned `true`.
    pub fn finish(self) -> RegionRivers {
        debug_assert!(self.phase == Phase::Done, "the rivers are not finished");
        RegionRivers::new(self.basis, self.subpoint_m, self.subpoints, self.out)
    }
}

/// `p` meandered by amplitude `a` and wavelength `lambda` along `normal` (projected at `p`):
/// `a × fbm(key_at(p), λ, 2 octaves)`. Exactly `p` when `a` is 0.
fn meander_point(
    p: DVec3,
    a: f64,
    lambda: f64,
    normal: DVec3,
    key_at: &impl Fn(RenderDir) -> (DVec3, u32),
) -> DVec3 {
    if a == 0.0 {
        return p;
    }
    let n = normal - p * p.dot(normal);
    let l = n.length();
    if !l.is_finite() || l <= 1e-12 {
        return p;
    }
    let (pos, seed) = key_at(RenderDir(p));
    let f = f64::from(fbm(pos, lambda, 2, 2.0, 0.5, seed ^ MEANDER_SALT));
    offset_point(p, n.normalize(), a * f)
}

/// The flow direction at texel `p` on a sub-segment: its ends' tangents lerped by `u`,
/// projected onto the tangent plane at `p`; the chord's direction if that vanishes.
fn flow_tangent(p: DVec3, ta: DVec3, tb: DVec3, u: f64, chord: Option<DVec3>) -> DVec3 {
    let t = ta * (1.0 - u) + tb * u;
    let t = t - p * p.dot(t);
    if t.length() > 1e-12 {
        t.normalize()
    } else {
        chord.unwrap_or(ta)
    }
}

/// Whether the arc from `a` to `b` comes within `r` of the square `|x|, |z| ≤ half` (local m).
fn arc_near_square(basis: &FrameBasis, a: DVec3, b: DVec3, half: f64, r: f64) -> bool {
    let len = angle(a, b) * EARTH_RADIUS_M;
    let n = (len / GATHER_SAMPLE_M).ceil().max(1.0) as u32;
    let slack = 0.5 * len / f64::from(n) + 1.0;
    (0..=n).any(|j| {
        basis
            .dir_to_local(RenderDir(chord_point(a, b, t_of(j, n))))
            .is_some_and(|(x, z)| {
                (x.abs() - half).max(0.0).hypot((z.abs() - half).max(0.0)) <= r + slack
            })
    })
}

/// A texel's carved height and the river it shows, if any.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RiverTexel {
    pub h_m: f64,
    pub hit: Option<RiverHit>,
}

/// The river at a texel. Flow is in the texel's own east and north
/// (`RegionFrame { center: dir }.north_east()`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RiverHit {
    pub level_m: f32,
    /// `w/2 − d`: positive inside the water, a distance to the centreline field below a texel.
    pub ribbon_m: f32,
    pub flow_east_mps: f32,
    pub flow_north_mps: f32,
    /// 1 in the inner half of the valley, falling to 0 at its edge.
    pub fade: f32,
    /// The channel's half-width `w/2` (Task 6).
    pub half_width_m: f32,
    /// The signed distance to the centreline: positive on the left bank (looking downstream).
    /// `half_width_m − |offset_m| = ribbon_m`. Unlike the ribbon it is linear across the
    /// centreline, so it interpolates between texels on either side of a sub-texel river
    /// (Task 6).
    pub offset_m: f32,
}

/// A sub-segment in the index: its segment, its start's index along it, its reach and its
/// local ends.
#[derive(Copy, Clone, Debug)]
struct SubSeg {
    seg: u32,
    j: u32,
    reach_m: f64,
    a: [f64; 2],
    b: [f64; 2],
}

/// A region's refined rivers, queried texel by texel.
#[derive(Clone, Debug)]
pub struct RegionRivers {
    basis: FrameBasis,
    subpoint_m: f64,
    subpoints: Vec<SubPoint>,
    segs: Vec<RiverSeg>,
    subsegs: Vec<SubSeg>,
    /// A uniform grid over the sub-segments' reach, in local metres: cell `(cx, cz)` lists
    /// `items[start[c]..start[c + 1]]`, `c = cz·nx + cx`.
    origin: [f64; 2],
    cell_m: f64,
    nx: u32,
    nz: u32,
    start: Vec<u32>,
    items: Vec<u32>,
}

/// Distance in the plane from `p` to the segment `a`–`b`.
fn plane_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dz * dz;
    let u = if l2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p[0] - a[0] - u * dx).hypot(p[1] - a[1] - u * dz)
}

/// Great-circle distance (m) from `p` to the arc `a`–`b`, the fraction along it of the nearest
/// point, and the arc's travel direction at `p` (none for a degenerate arc).
fn arc_distance(p: DVec3, a: DVec3, b: DVec3) -> (f64, f64, Option<DVec3>) {
    let n = a.cross(b);
    let nl = n.length();
    if nl < 1e-15 {
        return (angle(p, a) * EARTH_RADIUS_M, 0.0, None);
    }
    let n = n / nl;
    let travel = n.cross(p);
    let travel = (travel.length() > 1e-15).then(|| travel.normalize());
    let s = p.dot(n);
    let q = p - n * s;
    if q.length() > 1e-15 {
        let q = q.normalize();
        if a.cross(q).dot(n) >= 0.0 && q.cross(b).dot(n) >= 0.0 {
            let u = (angle(a, q) / angle(a, b)).clamp(0.0, 1.0);
            return (s.clamp(-1.0, 1.0).asin().abs() * EARTH_RADIUS_M, u, travel);
        }
    }
    let (da, db) = (angle(p, a), angle(p, b));
    if da <= db {
        (da * EARTH_RADIUS_M, 0.0, travel)
    } else {
        (db * EARTH_RADIUS_M, 1.0, travel)
    }
}

impl RegionRivers {
    fn new(
        basis: FrameBasis,
        subpoint_m: f64,
        subpoints: Vec<SubPoint>,
        segs: Vec<RiverSeg>,
    ) -> RegionRivers {
        let mut rivers = RegionRivers {
            basis,
            subpoint_m,
            subpoints,
            segs,
            subsegs: Vec::new(),
            origin: [0.0; 2],
            cell_m: 1.0,
            nx: 0,
            nz: 0,
            start: vec![0],
            items: Vec::new(),
        };
        let texel_bound = 2.0 * subpoint_m;
        for (si, seg) in rivers.segs.iter().enumerate() {
            let pairs = seg.count - 1 + u32::from(seg.next.is_some());
            for j in 0..pairs {
                let (a, b) = rivers.pair(si, j);
                let (Some(la), Some(lb)) = (
                    basis.dir_to_local(RenderDir(a.dir)),
                    basis.dir_to_local(RenderDir(b.dir)),
                ) else {
                    continue;
                };
                let w = 2.0 * f64::from(a.half_width_m);
                rivers.subsegs.push(SubSeg {
                    seg: si as u32,
                    j,
                    reach_m: LOCAL_SLACK * valley_half_width_m(w, texel_bound) + 10.0,
                    a: [la.0, la.1],
                    b: [lb.0, lb.1],
                });
            }
        }
        rivers.subpoints.shrink_to_fit();
        rivers.segs.shrink_to_fit();
        rivers.subsegs.shrink_to_fit();
        if rivers.subsegs.is_empty() {
            return rivers;
        }
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for ss in &rivers.subsegs {
            for axis in 0..2 {
                lo[axis] = lo[axis].min(ss.a[axis].min(ss.b[axis]) - ss.reach_m);
                hi[axis] = hi[axis].max(ss.a[axis].max(ss.b[axis]) + ss.reach_m);
            }
        }
        let extent = (hi[0] - lo[0]).max(hi[1] - lo[1]);
        // About a valley half-width: each sub-segment lands in about 25 cells.
        let cell_m = (4.0 * subpoint_m).max(extent / 2048.0);
        let nx = ((hi[0] - lo[0]) / cell_m).ceil().max(1.0) as u32;
        let nz = ((hi[1] - lo[1]) / cell_m).ceil().max(1.0) as u32;
        let range = |ss: &SubSeg| {
            let cell = |v: f64, axis: usize, n: u32| {
                (((v - lo[axis]) / cell_m).floor().max(0.0) as u32).min(n - 1)
            };
            (
                cell(ss.a[0].min(ss.b[0]) - ss.reach_m, 0, nx),
                cell(ss.a[0].max(ss.b[0]) + ss.reach_m, 0, nx),
                cell(ss.a[1].min(ss.b[1]) - ss.reach_m, 1, nz),
                cell(ss.a[1].max(ss.b[1]) + ss.reach_m, 1, nz),
            )
        };
        let mut start = vec![0u32; (nx * nz) as usize + 1];
        for ss in &rivers.subsegs {
            let (x0, x1, z0, z1) = range(ss);
            for cz in z0..=z1 {
                for cx in x0..=x1 {
                    start[(cz * nx + cx) as usize + 1] += 1;
                }
            }
        }
        for c in 0..(nx * nz) as usize {
            start[c + 1] += start[c];
        }
        let mut fill = start.clone();
        let mut items = vec![0u32; start[(nx * nz) as usize] as usize];
        for (id, ss) in rivers.subsegs.iter().enumerate() {
            let (x0, x1, z0, z1) = range(ss);
            for cz in z0..=z1 {
                for cx in x0..=x1 {
                    let c = (cz * nx + cx) as usize;
                    items[fill[c] as usize] = id as u32;
                    fill[c] += 1;
                }
            }
        }
        rivers.origin = lo;
        rivers.cell_m = cell_m;
        rivers.nx = nx;
        rivers.nz = nz;
        rivers.start = start;
        rivers.items = items;
        rivers
    }

    /// The sub-points of every segment that can touch the region, by river then `k`.
    pub fn subpoints(&self) -> &[SubPoint] {
        &self.subpoints
    }

    /// The bytes the rivers hold, by capacity.
    pub fn bytes(&self) -> usize {
        self.subpoints.capacity() * std::mem::size_of::<SubPoint>()
            + self.segs.capacity() * std::mem::size_of::<RiverSeg>()
            + self.subsegs.capacity() * std::mem::size_of::<SubSeg>()
            + 4 * (self.start.capacity() + self.items.capacity())
    }

    /// Every sub-segment's ends, by segment then `k`: consecutive sub-points, and each
    /// non-terminal segment's last sub-point with the down segment's first.
    pub fn sub_segments(&self) -> impl Iterator<Item = (&SubPoint, &SubPoint)> + '_ {
        self.segs.iter().enumerate().flat_map(move |(si, seg)| {
            let pairs = seg.count - 1 + u32::from(seg.next.is_some());
            (0..pairs).map(move |j| self.pair(si, j))
        })
    }

    /// The ends of sub-segment `j` of segment `seg`.
    fn pair(&self, seg: usize, j: u32) -> (&SubPoint, &SubPoint) {
        let s = &self.segs[seg];
        let a = &self.subpoints[(s.first + j) as usize];
        let b = if j + 1 < s.count {
            &self.subpoints[(s.first + j + 1) as usize]
        } else {
            s.next
                .as_ref()
                .expect("a closing sub-segment has a next point")
        };
        (a, b)
    }

    /// The carved height and the river at a texel at `dir` whose height is `h_m`, for texels
    /// `texel_m` wide. A pure function of `dir`, `h_m`, `texel_m` and the era's rivers inside
    /// the region's square. Flow follows the sub-points' tangents, lerped.
    ///
    /// Panics if `texel_m` exceeds twice the sub-point spacing: the gather and the index bound
    /// valleys by that texel, and a coarser one would silently drop candidates near the reach.
    pub fn texel(&self, dir: RenderDir, h_m: f64, texel_m: f64) -> RiverTexel {
        assert!(
            texel_m <= 2.0 * self.subpoint_m * (1.0 + 1e-9),
            "texel {texel_m} m is coarser than twice the sub-point spacing ({} m, so at most {} m): \
             the gather and the index would drop candidates near their reach",
            self.subpoint_m,
            2.0 * self.subpoint_m
        );
        let mut out = RiverTexel { h_m, hit: None };
        if self.subsegs.is_empty() {
            return out;
        }
        let p = dir.0.normalize();
        let Some((x, z)) = self.basis.dir_to_local(RenderDir(p)) else {
            return out;
        };
        let cx = ((x - self.origin[0]) / self.cell_m).floor();
        let cz = ((z - self.origin[1]) / self.cell_m).floor();
        if !(cx >= 0.0 && cz >= 0.0 && cx < f64::from(self.nx) && cz < f64::from(self.nz)) {
            return out;
        }
        let c = (cz as u32 * self.nx + cx as u32) as usize;
        struct Best {
            ribbon: f64,
            river: u32,
            k: u32,
            level: f64,
            fade: f64,
            speed: f64,
            tangent: DVec3,
            end_level: f32,
            half_width: f64,
            offset: f64,
        }
        let mut carved = h_m;
        let mut in_channel = false;
        let mut best: Option<Best> = None;
        for &id in &self.items[self.start[c] as usize..self.start[c + 1] as usize] {
            let ss = &self.subsegs[id as usize];
            if plane_distance([x, z], ss.a, ss.b) > ss.reach_m {
                continue;
            }
            let (a, b) = self.pair(ss.seg as usize, ss.j);
            let (d, u, travel) = arc_distance(p, a.dir, b.dir);
            let w = 2.0 * f64::from(a.half_width_m);
            let v = valley_half_width_m(w, texel_m);
            if d > v {
                continue;
            }
            let w_eff = w.max(CHANNEL_MIN_TEXELS * texel_m);
            let (la, lb) = (f64::from(a.level_m), f64::from(b.level_m));
            let level = la + (lb - la) * u;
            let blend = smoothstep(v / 2.0, v, d);
            let cut = if d <= w_eff / 2.0 {
                in_channel = true;
                let r = 2.0 * d / w_eff;
                level - f64::from(a.depth_m) * (1.0 - r * r)
            } else {
                let bank = level + BANK_FREEBOARD_M + VALLEY_WALL_SLOPE * (d - w_eff / 2.0);
                bank + (h_m - bank) * blend
            };
            carved = carved.min(cut);
            let ribbon = w / 2.0 - d;
            let left = a.dir.cross(b.dir).dot(p) >= 0.0;
            let better = best.as_ref().is_none_or(|b| {
                ribbon > b.ribbon || (ribbon == b.ribbon && (a.river, a.k) < (b.river, b.k))
            });
            if better {
                let (sa, sb) = (f64::from(a.speed_mps), f64::from(b.speed_mps));
                best = Some(Best {
                    ribbon,
                    river: a.river,
                    k: a.k,
                    level,
                    fade: 1.0 - blend,
                    speed: sa + (sb - sa) * u,
                    tangent: flow_tangent(p, a.tangent, b.tangent, u, travel),
                    end_level: self.segs[ss.seg as usize].end_level_m,
                    half_width: w / 2.0,
                    offset: if left { d } else { -d },
                });
            }
        }
        if let Some(b) = best {
            // The fade as the hit reports it: 1 within an f32 ulp of the inner half.
            let fade = b.fade as f32;
            if !in_channel && fade == 1.0 && carved >= f64::from(b.end_level) {
                carved = carved.max(b.level + BANK_FREEBOARD_M);
            }
            let (north, east) = RegionFrame {
                center: RenderDir(p),
            }
            .north_east();
            out.hit = Some(RiverHit {
                level_m: b.level as f32,
                ribbon_m: b.ribbon as f32,
                flow_east_mps: (b.speed * b.tangent.dot(east)) as f32,
                flow_north_mps: (b.speed * b.tangent.dot(north)) as f32,
                fade,
                half_width_m: b.half_width as f32,
                offset_m: b.offset as f32,
            });
        }
        out.h_m = carved;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cube::CubeFace;
    use crate::deep_time::YearsBp;
    use crate::hydrology::climate::ZonalClimate;
    use crate::hydrology::climate::tests::shipped_table;
    use crate::hydrology::params::{RIVER_MIN_DISCHARGE_M3S, SUBPOINT_M};
    use crate::hydrology::{HydroPrep, NodeSample, NodeSource};
    use crate::worldgen::REGION_SIZE_M;
    use crate::worldgen::testkit::{RiverChain, lattice_walk, river_hydro};
    use proptest::prelude::*;
    use std::collections::BTreeMap;

    /// Odd and non-dyadic: the PosX face's middle row is exactly the equator, and its mean
    /// spacing (11.26 km) puts the node grid's second step at 3.0 km.
    const N: u32 = 819;
    const ROW: u32 = 409;
    const LAT: CubeLattice = CubeLattice { n: N };
    const HALF: f64 = REGION_SIZE_M / 2.0;
    const MIN_Q: f32 = RIVER_MIN_DISCHARGE_M3S as f32;
    const INNER_TEXEL_M: f64 = REGION_SIZE_M / 1024.0;

    fn eq(col: u32) -> u32 {
        LAT.node(CubeFace::PosX, col, ROW)
    }

    /// Metres east along the equator from the PosX face centre (east is render -Z there).
    fn along_m(d: DVec3) -> f64 {
        (-d.z).atan2(d.x) * EARTH_RADIUS_M
    }

    fn north_m(d: DVec3) -> f64 {
        d.y.clamp(-1.0, 1.0).asin() * EARTH_RADIUS_M
    }

    fn col_along(col: u32) -> f64 {
        along_m(LAT.dir(eq(col)).0)
    }

    fn centre() -> RenderDir {
        LAT.dir(eq(ROW))
    }

    /// The main river: east along the equator row from column 380 to 440, then the ocean.
    fn main_chain(q: impl Fn(u32) -> f32, bed: impl Fn(f64) -> f64) -> RiverChain {
        let cols: Vec<u32> = (380..=440).collect();
        RiverChain {
            nodes: cols.iter().map(|&c| eq(c)).collect(),
            q: cols.iter().map(|&c| q(c)).collect(),
            bed: cols.iter().map(|&c| bed(col_along(c)) as f32).collect(),
            end: Down::Ocean(eq(441)),
        }
    }

    fn key(d: RenderDir) -> (DVec3, u32) {
        (d.0 * EARTH_RADIUS_M, 11)
    }

    fn build_at(
        hydro: &EraHydrology,
        centre: RenderDir,
        floor: &impl Fn(RenderDir) -> f64,
        budget: u32,
    ) -> RegionRivers {
        let mut b = RiverBuilder::new(
            hydro,
            &RegionFrame { center: centre },
            HALF,
            SUBPOINT_M,
            MIN_Q,
        );
        while !b.step(floor, &key, budget) {}
        b.finish()
    }

    fn build(hydro: &EraHydrology, floor: &impl Fn(RenderDir) -> f64) -> RegionRivers {
        build_at(hydro, centre(), floor, 997)
    }

    fn metres_between(a: DVec3, b: DVec3) -> f64 {
        a.cross(b).length().atan2(a.dot(b)) * EARTH_RADIUS_M
    }

    /// Sub-points by river.
    fn by_river(rr: &RegionRivers) -> BTreeMap<u32, Vec<SubPoint>> {
        let mut out: BTreeMap<u32, Vec<SubPoint>> = BTreeMap::new();
        for sp in rr.subpoints() {
            out.entry(sp.river).or_default().push(*sp);
        }
        out
    }

    #[test]
    fn width_depth_and_speed_follow_hydraulic_geometry() {
        let (w, d) = hydraulic_geometry(200_000.0);
        assert!((w - 2236.0).abs() < 1.0, "{w}");
        assert!((d - 31.6).abs() < 0.2, "{d}");
        let (w20, d20) = hydraulic_geometry(20.0);
        assert!(
            (w20 - 5.0 * 20f32.sqrt()).abs() < 1e-3 && (d20 - 0.27 * 20f32.powf(0.39)).abs() < 1e-4
        );

        let (_, d100) = hydraulic_geometry(100.0);
        let (s, r) = river_speed_mps(d100, 0.02);
        assert!(r > 0.0 && (3.0..=6.0).contains(&s), "{s} {r}");
        assert!((s - (3.0 + 3.0 * r)).abs() < 1e-5);
        assert!(
            (rapid_from_speed(s) - r).abs() < 1e-5,
            "the shader's inverse"
        );

        let (_, d5k) = hydraulic_geometry(5000.0);
        let (s, r) = river_speed_mps(d5k, 1e-4);
        assert_eq!(r, 0.0);
        assert!((0.3..=3.0).contains(&s), "{s}");
        let manning = d5k.powf(2.0 / 3.0) * 1e-4f32.sqrt() / MANNING_N as f32;
        assert!((s - manning).abs() < 1e-5, "{s} vs {manning}");
        assert_eq!(rapid_from_speed(s), 0.0);

        // Lowland speeds clamp to [0.3, 3]; the rapids saturate at 6.
        assert_eq!(river_speed_mps(0.5, 1e-7).0, 0.3);
        assert_eq!(river_speed_mps(40.0, 0.0079).0, 3.0);
        assert_eq!(river_speed_mps(1.0, 0.2), (6.0, 1.0));
        assert_eq!(rapid_from_speed(7.0), 1.0);
    }

    // ---- A tilted plain (fix round 1 of Task 6). ----

    /// Task 5's first relaxation, kept to show the hazard: the lowest of all 49 grid points
    /// and of all 13 corridor offsets, edges included.
    fn legacy_centreline(
        hydro: &EraHydrology,
        river: u32,
        floor_at: &impl Fn(RenderDir) -> f64,
    ) -> Vec<DVec3> {
        let lattice = hydro.lattice;
        let corridor = corridor_m(&lattice);
        let relax = |node: u32| {
            let p = lattice.dir(node).0;
            let basis = RegionFrame {
                center: RenderDir(p),
            }
            .basis();
            let half = (NODE_SEARCH_GRID / 2) as i32;
            let step = corridor / f64::from(half);
            let mut best = (f64::INFINITY, i32::MAX, i32::MAX, p);
            for j in -half..=half {
                for i in -half..=half {
                    let index = (j + half) * NODE_SEARCH_GRID as i32 + (i + half);
                    let d = basis
                        .local_to_dir(f64::from(i) * step, -f64::from(j) * step)
                        .0;
                    let f = clean(floor_at(RenderDir(d)));
                    if (f, i * i + j * j, index) < (best.0, best.1, best.2) {
                        best = (f, i * i + j * j, index, d);
                    }
                }
            }
            best.3
        };
        let nodes = hydro.rivers.nodes();
        let n = &nodes[river as usize];
        let end = end_node(n, nodes);
        let (c0, c1) = (lattice.dir(n.node).0, lattice.dir(end).0);
        let p0 = relax(n.node);
        let p1 = if matches!(n.down, Down::River(_)) {
            relax(end)
        } else {
            c1
        };
        let k_max = sub_count(lattice.distance_m(n.node, end), SUBPOINT_M);
        let normal = chord_normal(p0, p1, c0, c1);
        let raw: Vec<f64> = (0..=k_max)
            .map(|k| {
                if k == 0 || k == k_max {
                    return 0.0;
                }
                let c = chord_point(p0, p1, t_of(k, k_max));
                let dir = across(c, normal);
                let half = (CORRIDOR_SAMPLES / 2) as i32;
                let mut best = (f64::INFINITY, i32::MAX, true, 0.0);
                for j in -half..=half {
                    let o = f64::from(j) * corridor / f64::from(half);
                    let f = clean(floor_at(RenderDir(offset_point(c, dir, o))));
                    if (f, j.abs(), j > 0) < (best.0, best.1, best.2) {
                        best = (f, j.abs(), j > 0, o);
                    }
                }
                best.3
            })
            .collect();
        smoothed_points(p0, p1, normal, &raw)
    }

    /// The tilted-plain fixture: a 50 m river (no meanders) along row 450 of the recent lattice
    /// (n = 900), on a plain falling 1e-3 along the river and 3e-3 across it.
    fn tilted_plain() -> (EraHydrology, impl Fn(RenderDir) -> f64, RenderDir) {
        let lat = CubeLattice { n: 900 };
        let at = |col: u32| lat.node(CubeFace::PosX, col, 450);
        let cols: Vec<u32> = (420..=480).collect();
        let hydro = river_hydro(
            lat,
            &[RiverChain {
                nodes: cols.iter().map(|&c| at(c)).collect(),
                q: vec![100.0; cols.len()],
                bed: cols
                    .iter()
                    .map(|&c| (500.0 - 1e-3 * along_m(lat.dir(at(c)).0)) as f32)
                    .collect(),
                end: Down::Ocean(at(481)),
            }],
            vec![],
        );
        let floor = |d: RenderDir| 400.0 - 1e-3 * along_m(d.0) + 3e-3 * north_m(d.0);
        (hydro, floor, lat.dir(at(450)))
    }

    /// Fix round 1's ruling: on a tilted plain, the corridor's lowest sample is at its edge, so
    /// relaxing to it sends every segment out sideways from its pinned node and back in (the
    /// hairpins). Only interior minima count now, so the river stays on its coarse line.
    #[test]
    fn a_tilted_plain_leaves_the_river_on_its_line() {
        let (hydro, floor, centre) = tilted_plain();
        let rr = build_at(&hydro, centre, &floor, 997);
        let lat = hydro.lattice;
        let nodes = hydro.rivers.nodes();
        // The signed distance of a point from its segment's coarse line.
        let offset = |river: u32, p: DVec3| {
            let n = &nodes[river as usize];
            let (c0, c1) = (lat.dir(n.node).0, lat.dir(end_node(n, nodes)).0);
            p.dot(c0.cross(c1).normalize()).asin() * EARTH_RADIUS_M
        };
        let (mut subsegs, mut flips, mut longest, mut widest) = (0, 0, 0.0f64, 0.0f64);
        let mut last: Option<f64> = None;
        for (a, b) in rr.sub_segments() {
            longest = longest.max(metres_between(a.dir, b.dir));
            subsegs += 1;
            let o = offset(a.river, a.dir);
            widest = widest.max(o.abs());
            if let Some(prev) = last
                && prev.abs() > 1.0
                && o.abs() > 1.0
                && prev.signum() != o.signum()
            {
                flips += 1;
            }
            last = Some(o);
        }
        assert!(subsegs > 500, "{subsegs} sub-segments");
        assert!(
            longest <= 1.5 * SUBPOINT_M,
            "a sub-segment of {longest:.0} m"
        );
        assert_eq!(flips, 0, "offset sign flips");
        // A plain has no interior minimum: nodes and sub-points stay on the coarse line.
        assert!(widest < 1.0, "{widest:.1} m off the coarse line");
        // The hazard: the old rule's centrelines on the same plain.
        let mut old_longest = 0.0f64;
        for river in by_river(&rr).keys() {
            let pts = legacy_centreline(&hydro, *river, &floor);
            for w in pts.windows(2) {
                old_longest = old_longest.max(metres_between(w[0], w[1]));
            }
        }
        assert!(
            old_longest > 3.0 * SUBPOINT_M,
            "the old rule's longest leg {old_longest:.0} m"
        );
    }

    /// Fix round 2: a valley 3.6 km beside the coarse line, where sub-points find an interior
    /// minimum (corridor reach 5C/6 = 3.4 km) but the nodes do not (2C/3 = 2.7 km, the border
    /// sample at 4.1 km is lower). Without a taper the smoothed k = 1 point jumps 2.6 km out of
    /// the pinned node; tapered over 3 sub-points, the river bends into the valley gradually.
    #[test]
    fn a_valley_beside_a_pinned_node_is_entered_gradually() {
        let lat = CubeLattice { n: 900 };
        let at = |col: u32| lat.node(CubeFace::PosX, col, 450);
        let row_north = north_m(lat.dir(at(450)).0);
        let cols: Vec<u32> = (420..=480).collect();
        let hydro = river_hydro(
            lat,
            &[RiverChain {
                nodes: cols.iter().map(|&c| at(c)).collect(),
                q: vec![100.0; cols.len()],
                bed: cols
                    .iter()
                    .map(|&c| (500.0 - 1e-3 * along_m(lat.dir(at(c)).0)) as f32)
                    .collect(),
                end: Down::Ocean(at(481)),
            }],
            vec![],
        );
        let axis = row_north + 3600.0;
        let floor =
            move |d: RenderDir| 400.0 - 1e-3 * along_m(d.0) + 0.05 * (north_m(d.0) - axis).abs();
        // The hazard: the nodes stay put, and the untapered smoothing leaves them sideways.
        for &c in &cols[..cols.len() - 1] {
            assert_eq!(
                relax_node(&lat, at(c), &floor),
                lat.dir(at(c)).0,
                "node {c} moved"
            );
        }
        let rr = build_at(&hydro, lat.dir(at(450)), &floor, 997);
        let rivers = by_river(&rr);
        let (mut longest, mut old_longest, mut in_valley) = (0.0f64, 0.0f64, 0);
        let mut same = 0;
        for (&river, sps) in &rivers {
            let pts = relaxed_centreline(&hydro, river, SUBPOINT_M, &floor);
            // The builder's centreline is the published one, bit for bit (no meanders at 50 m).
            for sp in sps {
                assert_eq!(
                    sp.dir.to_array().map(f64::to_bits),
                    pts[sp.k as usize].to_array().map(f64::to_bits)
                );
                same += 1;
            }
            for w in pts.windows(2) {
                longest = longest.max(metres_between(w[0], w[1]));
            }
            in_valley += pts
                .iter()
                .filter(|p| (north_m(**p) - axis).abs() < 300.0)
                .count();
            // The same corridor without the taper.
            let n = &hydro.rivers.nodes()[river as usize];
            let (c0, c1) = (
                lat.dir(n.node).0,
                lat.dir(end_node(n, hydro.rivers.nodes())).0,
            );
            let k_max = (pts.len() - 1) as u32;
            let normal = chord_normal(c0, c1, c0, c1);
            let raw: Vec<f64> = (0..=k_max)
                .map(|k| {
                    if k == 0 || k == k_max {
                        0.0
                    } else {
                        raw_offset(
                            chord_point(c0, c1, t_of(k, k_max)),
                            normal,
                            corridor_m(&lat),
                            &floor,
                        )
                    }
                })
                .collect();
            for w in smoothed_points(c0, c1, normal, &raw).windows(2) {
                old_longest = old_longest.max(metres_between(w[0], w[1]));
            }
        }
        assert!(
            old_longest > 4.0 * SUBPOINT_M,
            "untapered leg {old_longest:.0} m"
        );
        assert!(longest < 2.2 * SUBPOINT_M, "tapered leg {longest:.0} m");
        assert!(in_valley > 300, "{in_valley} points on the valley axis");
        assert!(same > 300, "{same} sub-points compared");
    }

    #[test]
    fn the_relaxed_river_lies_in_the_valley_floor() {
        // A 50 m river (no meanders) on the equator row; the valley's axis runs 3 km north.
        let hydro = river_hydro(LAT, &[main_chain(|_| 100.0, |a| 500.0 - 1e-3 * a)], vec![]);
        let floor =
            |d: RenderDir| 400.0 - 1e-3 * along_m(d.0) + 0.05 * (north_m(d.0) - 3000.0).abs();
        let rr = build(&hydro, &floor);
        let rivers = by_river(&rr);
        assert!(rivers.len() >= 15, "{} segments in reach", rivers.len());
        let mut checked = 0;
        for (&river, sps) in &rivers {
            // The hazard: the coarse line is the equator, 3 km off the axis.
            let coarse = LAT.dir(hydro.rivers.nodes()[river as usize].node).0;
            assert!(north_m(coarse).abs() < 1e-6);
            let relaxed = relaxed_centreline(&hydro, river, SUBPOINT_M, &floor);
            for sp in sps {
                let p = relaxed[sp.k as usize];
                assert!(
                    (north_m(p) - 3000.0).abs() < 300.0,
                    "{river}/{}: {}",
                    sp.k,
                    north_m(p)
                );
                // The builder's centreline is the published relaxation (no meanders at 50 m).
                assert_eq!(
                    sp.dir.to_array().map(f64::to_bits),
                    p.to_array().map(f64::to_bits)
                );
                checked += 1;
            }
        }
        assert!(checked > 300, "{checked}");
    }

    /// Pass beds falling 2% eastward from 3000 m at column 380.
    fn steep_bed(a: f64) -> f64 {
        3000.0 - 0.02 * (a - col_along(380))
    }

    #[test]
    fn incision_follows_a_gorge_below_the_pass_bed() {
        let hydro = river_hydro(LAT, &[main_chain(|_| 100.0, steep_bed)], vec![]);
        let g0 = col_along(405) + 3000.0;
        let gorge = |a: f64| (g0..=g0 + 10_000.0).contains(&a);
        let floor = |d: RenderDir| {
            let a = along_m(d.0);
            steep_bed(a) + 0.05 * north_m(d.0).abs() - if gorge(a) { 400.0 } else { 0.0 }
        };
        let rr = build(&hydro, &floor);
        let mut inside = 0;
        for sp in rr.subpoints() {
            let a = along_m(sp.dir);
            if !gorge(a) {
                continue;
            }
            inside += 1;
            // The hazard: the floor is 400 m under the pass bed here.
            assert!(
                sp.coarse_m < steep_bed(a) - 399.0,
                "{} vs {}",
                sp.coarse_m,
                steep_bed(a)
            );
            assert!(
                (f64::from(sp.bed_m) - sp.coarse_m).abs() < 1.0,
                "bed {} floor {}",
                sp.bed_m,
                sp.coarse_m
            );
        }
        assert!(inside >= 15, "{inside} sub-points in the gorge");
    }

    // ---- Random valleys over a tree with two confluences. ----

    /// Main river on the equator row (columns 380..=440); tributary A down column 400 from row
    /// 395, joining at (400, 409); tributary B up column 420 from row 424, joining at (420, 409).
    fn tree(main_slope: f64) -> EraHydrology {
        let bed = |a: f64| 2000.0 - main_slope * (a - col_along(380));
        // λ = 11·w ≥ 2.4 km on the main river: it meanders on flat ground.
        let main = main_chain(
            |c| 2000.0 + if c >= 400 { 900.0 } else { 0.0 } + if c >= 420 { 400.0 } else { 0.0 },
            bed,
        );
        let trib = |col: u32, from_row: u32, step: usize, count: u32, q: f32| {
            let start = LAT.node(CubeFace::PosX, col, from_row);
            let nodes = lattice_walk(&LAT, start, step, count);
            let at_junction = bed(col_along(col));
            let len = nodes.len();
            RiverChain {
                q: vec![q; len],
                bed: (0..len)
                    .map(|k| (at_junction + 40.0 * (len - k) as f64) as f32)
                    .collect(),
                nodes,
                end: Down::River(eq(col)),
            }
        };
        river_hydro(
            LAT,
            &[
                main,
                trib(400, 395, 6, 13, 900.0),
                trib(420, 424, 2, 14, 400.0),
            ],
            vec![],
        )
    }

    /// A rolling valley field: a plane falling east, walls, and random pits and humps.
    fn valley_field(
        bumps: Vec<(f64, f64, f64, f64)>,
        gorge: (f64, f64),
    ) -> impl Fn(RenderDir) -> f64 {
        move |d: RenderDir| {
            let (a, n) = (along_m(d.0), north_m(d.0));
            let mut f = 2000.0 - 2e-3 * (a - col_along(380)) + 0.02 * n.abs();
            for &(ba, bn, amp, r) in &bumps {
                let q = ((a - ba).powi(2) + (n - bn).powi(2)) / (r * r);
                f += amp * (-q).exp();
            }
            if (gorge.0..=gorge.0 + 8000.0).contains(&a) {
                f -= gorge.1;
            }
            f
        }
    }

    fn bump_strategy() -> impl Strategy<Value = Vec<(f64, f64, f64, f64)>> {
        let span = col_along(440) - col_along(380);
        prop::collection::vec(
            (
                col_along(380)..col_along(380) + span,
                -60_000.0f64..60_000.0,
                -300.0f64..300.0,
                2_000.0f64..20_000.0,
            ),
            0..12,
        )
    }

    /// Every sub-point of every gathered segment, through the builder's internals, so
    /// confluences and the steps between segments are covered.
    fn all_points(b: &RiverBuilder) -> Vec<(usize, Vec<SubPoint>)> {
        (0..b.segs.len())
            .filter(|&s| b.segs[s].role != Role::Window)
            .map(|s| {
                (
                    s,
                    (0..=b.segs[s].last_k())
                        .map(|k| b.sub_point(s, k, &key))
                        .collect(),
                )
            })
            .collect()
    }

    fn built<'h>(hydro: &'h EraHydrology, floor: &impl Fn(RenderDir) -> f64) -> RiverBuilder<'h> {
        let mut b = RiverBuilder::new(
            hydro,
            &RegionFrame { center: centre() },
            HALF,
            SUBPOINT_M,
            MIN_Q,
        );
        while !b.step(floor, &key, 4093) {}
        b
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 16, ..ProptestConfig::default() })]

        #[test]
        fn the_level_never_rises_downstream(
            bumps in bump_strategy(),
            gorge_at in 0.0f64..1.0,
            gorge_m in 0.0f64..500.0,
            main_slope in prop_oneof![Just(1e-4), 1e-4f64..2e-2],
        ) {
            let hydro = tree(main_slope);
            let g = col_along(380) + gorge_at * (col_along(440) - col_along(380));
            let floor = valley_field(bumps, (g, gorge_m));
            let b = built(&hydro, &floor);
            let points = all_points(&b);
            let first: BTreeMap<u32, SubPoint> =
                points.iter().map(|(s, p)| (b.segs[*s].river, p[0])).collect();
            let mut steps = 0;
            let mut confluences = 0;
            for (s, p) in &points {
                for w in p.windows(2) {
                    prop_assert!(w[1].level_m <= w[0].level_m, "{:?} -> {:?}", w[0], w[1]);
                    prop_assert!(w[1].bed_m <= w[0].bed_m);
                    steps += 1;
                }
                if let Some(down) = b.segs[*s].down
                    && let Some(next) = first.get(&down)
                {
                    let last = p.last().unwrap();
                    prop_assert!(next.level_m <= last.level_m, "{last:?} -> {next:?}");
                    confluences += u32::from(hydro.rivers.upstream(down).len() > 1);
                    steps += 1;
                }
            }
            prop_assert!(steps > 500, "{steps}");
            prop_assert!(confluences >= 2, "both confluences are checked: {confluences}");
        }

        #[test]
        fn incision_never_cuts_below_the_window_rule(
            bumps in bump_strategy(),
            gorge_at in 0.0f64..1.0,
            gorge_m in 100.0f64..500.0,
        ) {
            let hydro = tree(2e-3);
            // The gorge inside the region's reach (columns 397-421; the square spans 396-422).
            let g = col_along(397) + gorge_at * (col_along(421) - col_along(397) - 8000.0);
            let floor = valley_field(bumps, (g, gorge_m));
            let b = built(&hydro, &floor);
            let mut incised = 0;
            for (s, p) in all_points(&b) {
                let seg = &b.segs[s];
                for sp in p {
                    let m = seg.m_at(sp.k);
                    let pass = b.path.pass_bed(seg.river, m);
                    let after = b.path.pass_bed(seg.river, m - INCISION_WINDOW_M);
                    let rule = pass.min(sp.coarse_m).min(after);
                    // f32 storage rounds the f64 bed by at most half an ulp (1.2e-4 m here).
                    prop_assert!(f64::from(sp.bed_m) >= rule - 1e-3,
                        "bed {} rule {rule} (pass {pass}, floor {}, after {after})", sp.bed_m, sp.coarse_m);
                    if f64::from(sp.bed_m) < pass - 1.0 {
                        incised += 1;
                    }
                }
            }
            prop_assert!(incised > 0, "the gorge cuts below the pass bed");
        }
    }

    /// Flat ground (pass slope 1e-4) or steep (1e-2), with a 200 m river.
    fn meander_case(q: f32, slope: f64) -> (EraHydrology, impl Fn(RenderDir) -> f64) {
        let bed = move |a: f64| 1000.0 - slope * (a - col_along(380));
        let hydro = river_hydro(LAT, &[main_chain(|_| q, bed)], vec![]);
        let floor = move |d: RenderDir| bed(along_m(d.0)) - 2.0 + 0.05 * north_m(d.0).abs();
        (hydro, floor)
    }

    /// Each sub-point's distance from its relaxed (pre-meander) position, by river.
    fn meander_offsets(hydro: &EraHydrology, floor: &impl Fn(RenderDir) -> f64) -> Vec<f64> {
        let rr = build(hydro, floor);
        let mut out = Vec::new();
        for (river, sps) in by_river(&rr) {
            let relaxed = relaxed_centreline(hydro, river, SUBPOINT_M, floor);
            for sp in sps {
                out.push(metres_between(sp.dir, relaxed[sp.k as usize]));
            }
        }
        assert!(out.len() > 300);
        out
    }

    #[test]
    fn meanders_grow_on_flat_ground_and_vanish_in_steep_valleys() {
        let (w, _) = hydraulic_geometry(1600.0);
        assert!((w - 200.0).abs() < 1e-3, "{w}");
        let amplitude = MEANDER_AMPLITUDE_PER_WIDTH * f64::from(w);
        let (flat, flat_floor) = meander_case(1600.0, 1e-4);
        let offsets = meander_offsets(&flat, &flat_floor);
        let max = offsets.iter().copied().fold(0.0, f64::max);
        assert!(max > 0.3 * amplitude, "flat: {max} of {amplitude}");
        assert!(max <= amplitude, "flat: {max} of {amplitude}");
        let moved = offsets.iter().filter(|&&o| o > 1.0).count();
        assert!(
            moved * 2 > offsets.len(),
            "most sub-points move: {moved}/{}",
            offsets.len()
        );

        let (steep, steep_floor) = meander_case(1600.0, 1e-2);
        assert!(
            meander_offsets(&steep, &steep_floor)
                .iter()
                .all(|&o| o == 0.0),
            "no meanders in a steep valley"
        );
    }

    #[test]
    fn small_rivers_do_not_meander() {
        let (w, _) = hydraulic_geometry(100.0);
        assert!((w - 50.0).abs() < 1e-3, "{w}");
        // On this flat ground a wider river meanders at full amplitude (the slope term is 1)...
        let flat = 1.0 - smoothstep(MEANDER_FLAT_SLOPE[0], MEANDER_FLAT_SLOPE[1], 1e-4);
        assert_eq!(flat, 1.0);
        // ...but λ = 550 m is under the 2 km floor.
        let (small, floor) = meander_case(100.0, 1e-4);
        assert!(meander_offsets(&small, &floor).iter().all(|&o| o == 0.0));
    }

    /// The main branch into river node `v`: the largest discharge.
    fn main_up(hydro: &EraHydrology, v: u32) -> u32 {
        let nodes = hydro.rivers.nodes();
        *hydro
            .rivers
            .upstream(v)
            .iter()
            .max_by(|&&a, &&b| {
                nodes[a as usize]
                    .discharge_m3s
                    .total_cmp(&nodes[b as usize].discharge_m3s)
            })
            .unwrap()
    }

    #[test]
    fn meanders_meet_at_shared_nodes() {
        let hydro = tree(1e-4);
        let floor = valley_field(vec![(col_along(400), 2000.0, -30.0, 6000.0)], (0.0, 0.0));
        let rr = build(&hydro, &floor);
        let first: BTreeMap<u32, SubPoint> = by_river(&rr)
            .into_iter()
            .map(|(r, sps)| (r, sps[0]))
            .collect();
        let bits = |v: DVec3| v.to_array().map(f64::to_bits);
        let (mut shared, mut swung, mut confluences) = (0, 0, 0);
        for seg in &rr.segs {
            let Some(next) = seg.next else { continue };
            // The incoming segment's own t = 1 point is the outgoing one's start.
            assert_eq!(bits(seg.end_dir), bits(next.dir), "river {}", seg.river);
            let down = hydro.rivers.nodes()[seg.river as usize].down;
            let Down::River(v) = down else { panic!() };
            if let Some(start) = first.get(&v) {
                assert_eq!(bits(seg.end_dir), bits(start.dir));
                shared += 1;
                let relaxed = relaxed_centreline(&hydro, v, SUBPOINT_M, &floor)[0];
                let swing = metres_between(start.dir, relaxed);
                swung += u32::from(swing > 1.0);
                if hydro.rivers.upstream(v).len() > 1 && seg.river == main_up(&hydro, v) {
                    // At a confluence the normal follows the main stem (east-flowing), not the
                    // tributary: the swing is north-south.
                    confluences += 1;
                    let east = along_m(start.dir) - along_m(relaxed);
                    assert!(swing > 1.0 && east.abs() < 0.2 * swing, "{east} of {swing}");
                }
            }
        }
        assert!(shared > 20, "{shared}");
        // The confluence at column 400; the one at 420 lies past the region's reach.
        assert_eq!(confluences, 1);
        assert!(swung > 10, "the shared nodes meander: {swung}");
    }

    #[test]
    fn a_texel_on_the_centreline_is_below_the_level() {
        let (hydro, floor) = meander_case(1600.0, 1e-4);
        let rr = build(&hydro, &floor);
        let mut n = 0;
        for sp in rr.subpoints() {
            let t = rr.texel(RenderDir(sp.dir), sp.coarse_m + 50.0, INNER_TEXEL_M);
            let hit = t.hit.expect("the centreline is in the river");
            assert!(t.h_m < f64::from(sp.level_m), "{} vs {}", t.h_m, sp.level_m);
            assert!(
                (t.h_m - f64::from(sp.level_m - sp.depth_m)).abs() < 0.05,
                "{}",
                t.h_m
            );
            assert!(
                (hit.ribbon_m - sp.half_width_m).abs() < 0.5,
                "{}",
                hit.ribbon_m
            );
            assert!((hit.level_m - sp.level_m).abs() < 0.05);
            assert_eq!(hit.fade, 1.0);
            let speed = hit.flow_east_mps.hypot(hit.flow_north_mps);
            assert!(
                (speed - sp.speed_mps).abs() < 0.05,
                "{speed} vs {}",
                sp.speed_mps
            );
            // Eastward flow on an eastward river.
            assert!(hit.flow_east_mps > 0.5 * speed);
            n += 1;
        }
        assert!(n > 300);
    }

    /// The texel's flow as a vector in the era frame.
    fn flow_vector(d: DVec3, hit: &RiverHit) -> DVec3 {
        let (north, east) = RegionFrame {
            center: RenderDir(d),
        }
        .north_east();
        east * f64::from(hit.flow_east_mps) + north * f64::from(hit.flow_north_mps)
    }

    #[test]
    fn flow_turns_smoothly_along_a_meandering_river() {
        // w = 200 m on flat ground: λ = 2.2 km, A = 300 m, a bend every few sub-points.
        let (hydro, floor) = meander_case(1600.0, 1e-4);
        let rr = build(&hydro, &floor);
        // The centreline in flow order (one river: rivers ascend along the row), each
        // segment's start shared with the previous one's closing point.
        let line: Vec<DVec3> = by_river(&rr).values().flatten().map(|s| s.dir).collect();
        // Its sharpest discrete curvature: the turn at a vertex over the mean of its chords.
        let mut kappa = 0.0f64;
        for w in line.windows(3) {
            let (c0, c1) = (w[1] - w[0], w[2] - w[1]);
            let turn = c0.cross(c1).length().atan2(c0.dot(c1));
            let mean = 0.5 * (metres_between(w[0], w[1]) + metres_between(w[1], w[2]));
            kappa = kappa.max(turn / mean);
        }
        let step = INNER_TEXEL_M;
        let bound = 1.3 * kappa * step + 0.002;
        // The hazard: a chord switch turns by a whole vertex angle, about κ × 500 m.
        assert!(kappa * SUBPOINT_M > bound, "{kappa}");
        assert!(
            kappa * step > 5f64.to_radians(),
            "a meandering river: {kappa}"
        );
        // Texels every `step` metres of arc along the centreline.
        let (mut prev, mut worst, mut samples) = (None::<DVec3>, 0.0f64, 0);
        let mut carry = 0.0;
        for w in line.windows(2) {
            let len = metres_between(w[0], w[1]);
            let mut s = carry;
            while s < len {
                let d = chord_point(w[0], w[1], s / len);
                let hit = rr
                    .texel(RenderDir(d), 1000.0, step)
                    .hit
                    .expect("on the river");
                let f = flow_vector(d, &hit);
                if let Some(p) = prev {
                    let turn = p.cross(f).length().atan2(p.dot(f));
                    worst = worst.max(turn);
                    assert!(
                        turn <= bound,
                        "{}° > {}°",
                        turn.to_degrees(),
                        bound.to_degrees()
                    );
                }
                prev = Some(f);
                samples += 1;
                s += step;
            }
            carry = s - len;
        }
        assert!(samples > 500, "{samples}");
        println!(
            "worst turn {:.2}° per texel; bound {:.2}°; κ max {kappa:.2e} /m",
            worst.to_degrees(),
            bound.to_degrees()
        );
    }

    #[test]
    fn flow_is_continuous_at_nodes() {
        // Joins along the main river and at a confluence, on meandering flat ground.
        let hydro = tree(1e-4);
        let floor = valley_field(vec![(col_along(400), 2000.0, -30.0, 6000.0)], (0.0, 0.0));
        let rr = build(&hydro, &floor);
        let first: BTreeMap<u32, SubPoint> = by_river(&rr)
            .into_iter()
            .map(|(r, sps)| (r, sps[0]))
            .collect();
        let bits = |v: DVec3| v.to_array().map(f64::to_bits);
        let (mut joins, mut confluences) = (0, 0);
        for seg in &rr.segs {
            let Some(next) = seg.next else { continue };
            let Down::River(v) = hydro.rivers.nodes()[seg.river as usize].down else {
                panic!()
            };
            let Some(start) = first.get(&v) else { continue };
            // Both sides of the node give its tangent.
            assert_eq!(
                bits(next.tangent),
                bits(start.tangent),
                "river {} -> {v}",
                seg.river
            );
            let hit = rr
                .texel(RenderDir(start.dir), 1000.0, INNER_TEXEL_M)
                .hit
                .unwrap();
            let f = flow_vector(start.dir, &hit).normalize();
            assert!(
                f.dot(start.tangent) > 1.0 - 1e-9,
                "the node's flow is its tangent"
            );
            joins += 1;
            confluences += u32::from(hydro.rivers.upstream(v).len() > 1);
        }
        assert!(joins > 20 && confluences >= 2, "{joins} {confluences}");
    }

    #[test]
    #[should_panic(expected = "twice the sub-point spacing")]
    fn a_texel_coarser_than_twice_the_subpoint_panics() {
        let (hydro, floor) = meander_case(1600.0, 1e-4);
        let rr = build(&hydro, &floor);
        rr.texel(centre(), 1000.0, 2.0 * SUBPOINT_M * 1.01);
    }

    #[test]
    fn banks_hold_the_river() {
        let (hydro, floor) = meander_case(1600.0, 1e-4);
        let rr = build(&hydro, &floor);
        let frame = RegionFrame { center: centre() };
        let (w, _) = hydraulic_geometry(1600.0);
        let w_eff = f64::from(w).max(CHANNEL_MIN_TEXELS * INNER_TEXEL_M);
        let (mut banks, mut raised, mut channel) = (0, 0, 0);
        let step = INNER_TEXEL_M;
        // Every 500 m along 200 km of river, 37.5 m apart across its 1.2 km valley half-width.
        for row in 0..400 {
            for col in 0..81 {
                let x = -100_000.0 + 500.0 * f64::from(row) + 0.37 * step;
                let z = -1_500.0 + 37.5 * f64::from(col) + 0.11;
                let d = frame.local_to_dir(x, z);
                // A plain beside the channel, its detail dipping up to 7 m under the pass bed.
                let h = 1000.0 - 1e-4 * (along_m(d.0) - col_along(380)) - 4.0
                    + 3.0 * (x / 700.0).sin() * (z / 450.0).cos();
                let t = rr.texel(d, h, INNER_TEXEL_M);
                let Some(hit) = t.hit else { continue };
                let outside = f64::from(hit.ribbon_m) < (f64::from(w) - w_eff) / 2.0;
                if !outside {
                    channel += 1;
                    continue;
                }
                if hit.fade < 1.0 {
                    continue;
                }
                banks += 1;
                let hold = f64::from(hit.level_m) + BANK_FREEBOARD_M;
                raised += u32::from(h < hold);
                assert!(t.h_m >= hold - 1e-3, "{} vs {hold} at ({x}, {z})", t.h_m);
            }
        }
        assert!(channel > 100 && banks > 1000, "{channel} {banks}");
        assert!(raised > 100, "the dips would flood the banks: {raised}");
    }

    #[test]
    fn banks_do_not_rise_out_of_the_sea() {
        // A lowland river reaching the ocean at column 441; the region sits on its mouth.
        let mouth = col_along(441);
        let hydro = river_hydro(
            LAT,
            &[main_chain(|_| 1600.0, |a| 1e-4 * (mouth - a))],
            vec![],
        );
        let floor = |d: RenderDir| 1e-4 * (mouth - along_m(d.0)) - 1.0 + 0.05 * north_m(d.0).abs();
        let (w, _) = hydraulic_geometry(1600.0);
        let w_eff = f64::from(w).max(CHANNEL_MIN_TEXELS * INNER_TEXEL_M);
        let rr = build_at(&hydro, LAT.dir(eq(438)), &floor, 997);
        let last = rr
            .subpoints()
            .iter()
            .max_by(|a, b| along_m(a.dir).total_cmp(&along_m(b.dir)))
            .unwrap();
        let (mut sea, mut land) = (0, 0);
        for step in 0..40 {
            // Beside the last sub-points, 400 m north of the channel, in the inner half-valley.
            let d = RegionFrame {
                center: RenderDir(last.dir),
            }
            .local_to_dir(-150.0 * f64::from(step), -400.0);
            for h in [-30.0, 0.2] {
                let t = rr.texel(d, h, INNER_TEXEL_M);
                let Some(hit) = t.hit else { continue };
                if hit.fade < 1.0 || f64::from(hit.ribbon_m) >= (f64::from(w) - w_eff) / 2.0 {
                    continue;
                }
                if h < 0.0 {
                    // Under the sea the river ends in: no levee.
                    assert_eq!(t.h_m, h);
                    sea += 1;
                } else {
                    assert!(t.h_m >= f64::from(hit.level_m) + BANK_FREEBOARD_M - 1e-3);
                    land += 1;
                }
            }
        }
        assert!(sea > 10 && land > 10, "{sea} {land}");
    }

    // ---- A real pass over a synthetic continent. ----

    /// A continent around the PosX face centre with radial ridges and valleys, ocean beyond.
    fn continent(d: RenderDir) -> f64 {
        let p = d.0.normalize();
        let r = p.x.clamp(-1.0, 1.0).acos();
        let theta = p.z.atan2(p.y);
        1800.0 * (1.0 - r / 0.45)
            + 250.0 * (7.0 * theta).sin() * (r * 30.0).cos()
            + 120.0 * (p.y * 40.0).sin()
    }

    struct Terrain;

    impl NodeSource for Terrain {
        fn node(&self, dir: RenderDir) -> NodeSample {
            NodeSample {
                elevation_m: continent(dir) as f32,
                ice: 0.0,
            }
        }

        fn data_lakes(&self) -> bool {
            false
        }
    }

    fn pass(n: u32) -> EraHydrology {
        let table = shipped_table();
        let climate = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        let mut prep = HydroPrep::with_lake_levels(CubeLattice { n }, YearsBp(0.0), &[]);
        while !prep.step(&Terrain, &climate, &table, 50_000) {}
        prep.finish()
    }

    /// Brute force over the global graph: every refined segment with a sub-point inside the
    /// window of a point a reach or down segment needs (its own sub-points, and the slope's
    /// point 500 m up), and every segment within 500 m downstream of a reach segment, is gathered.
    fn assert_gather_is_complete(hydro: &EraHydrology, centre: RenderDir) -> usize {
        let mut b = RiverBuilder::new(
            hydro,
            &RegionFrame { center: centre },
            HALF,
            SUBPOINT_M,
            MIN_Q,
        );
        let gathered: std::collections::BTreeSet<u32> = b.segs.iter().map(|s| s.river).collect();
        let nodes = hydro.rivers.nodes();
        let refined = |i: u32| nodes[i as usize].discharge_m3s >= MIN_Q;
        let needs: Vec<(u32, f64, Role)> = b
            .segs
            .iter()
            .filter(|s| s.role != Role::Window)
            .map(|s| (s.river, s.m_start + SLOPE_HALF_M, s.role))
            .collect();
        let mut checked = 0;
        for (river, top, role) in needs {
            let mut stack: Vec<u32> = hydro.rivers.upstream(river).to_vec();
            while let Some(u) = stack.pop() {
                if !refined(u) {
                    continue;
                }
                b.path.ensure(u);
                let (m_down, len) = (b.path.m_down(u), b.path.len(u));
                let k_max = sub_count(len, SUBPOINT_M);
                let inside = (0..k_max)
                    .any(|k| m_down + (1.0 - t_of(k, k_max)) * len - INCISION_WINDOW_M <= top);
                if inside {
                    assert!(gathered.contains(&u), "{u} is in the window above {river}");
                    checked += 1;
                    stack.extend_from_slice(hydro.rivers.upstream(u));
                }
            }
            if role == Role::Reach {
                let mut left = SLOPE_HALF_M;
                let mut j = river;
                while let Down::River(d) = nodes[j as usize].down {
                    if !refined(d) || left <= 0.0 {
                        break;
                    }
                    assert!(gathered.contains(&d), "{d} is within 500 m below {river}");
                    b.path.ensure(d);
                    left -= b.path.len(d);
                    j = d;
                }
            }
        }
        checked
    }

    #[test]
    fn the_gather_holds_every_window() {
        let east = RegionFrame { center: centre() }.local_to_dir(100_000.0, 0.0);
        assert!(assert_gather_is_complete(&tree(3e-4), centre()) > 20);
        assert!(assert_gather_is_complete(&tree(3e-4), east) > 20);
        // A finer pass whose straight steps near the face centre (30.4 km) end just inside the
        // window plus the slope baseline (30.5 km), where a short gather would stop.
        let hydro = pass(419);
        let count = hydro.rivers.nodes().len() as u32;
        let mut checked = 0;
        for i in (0..count).step_by((count / 150).max(1) as usize) {
            checked += assert_gather_is_complete(
                &hydro,
                hydro.lattice.dir(hydro.rivers.nodes()[i as usize].node),
            );
        }
        assert!(checked > 20, "{checked}");
    }

    /// Every field of a sub-point, as bits.
    type Bits = (u32, [u64; 3], u64, u32, u32, u32, u32, u32, u32, [u64; 3]);

    fn bits(sps: &[SubPoint]) -> Vec<Bits> {
        sps.iter()
            .map(|s| {
                (
                    s.k,
                    s.dir.to_array().map(f64::to_bits),
                    s.coarse_m.to_bits(),
                    s.bed_m.to_bits(),
                    s.level_m.to_bits(),
                    s.half_width_m.to_bits(),
                    s.depth_m.to_bits(),
                    s.speed_mps.to_bits(),
                    s.rapid.to_bits(),
                    s.tangent.to_array().map(f64::to_bits),
                )
            })
            .collect()
    }

    /// Asserts two regions agree on every segment both refine; returns (shared, in one only).
    fn agree(a: &BTreeMap<u32, Vec<SubPoint>>, b: &BTreeMap<u32, Vec<SubPoint>>) -> (usize, usize) {
        let mut shared = 0;
        for (river, sa) in a {
            if let Some(sb) = b.get(river) {
                assert_eq!(bits(sa), bits(sb), "river {river}");
                shared += 1;
            }
        }
        let only_one = a.keys().filter(|r| !b.contains_key(r)).count()
            + b.keys().filter(|r| !a.contains_key(r)).count();
        (shared, only_one)
    }

    #[test]
    fn refined_rivers_are_the_same_from_any_region() {
        // Hand-built: a meandering river with two confluences, regions 100 km apart along it,
        // so one region's downstream edge falls between a segment and its down segment.
        let hydro = tree(3e-4);
        // The floor stands 30 m over the pass bed, with a 1 km notch 100 m deep every 35 km:
        // after each notch the bed holds for a window's length at B(notch ⊕ L), so the far end
        // of every window decides beds, and a region that gathered too short a window differs.
        let floor = |d: RenderDir| {
            let (a, n) = (along_m(d.0), north_m(d.0));
            let notch = if (a - col_along(380)).rem_euclid(35_000.0) < 1000.0 {
                100.0
            } else {
                0.0
            };
            2030.0 - 3e-4 * (a - col_along(380)) + 0.02 * n.abs() - notch
        };
        let east = RegionFrame { center: centre() }.local_to_dir(100_000.0, 0.0);
        let a = by_river(&build_at(&hydro, centre(), &floor, 997));
        let b = by_river(&build_at(&hydro, east, &floor, u32::MAX));
        let (shared, only_one) = agree(&a, &b);
        assert!(shared >= 15 && only_one > 0, "{shared} {only_one}");
        let moving = a
            .values()
            .flatten()
            .filter(|s| s.speed_mps > 0.3 && s.rapid == 0.0)
            .count();
        assert!(moving > 100, "Manning speeds, not a clamp: {moving}");

        // A real pass over a synthetic continent.
        let hydro = pass(75);
        let floor =
            |d: RenderDir| continent(d) + 40.0 * (d.0.y * 3000.0).sin() * (d.0.z * 2100.0).cos();
        // Centre on the largest river within the continent's middle.
        let big = hydro
            .rivers
            .nodes()
            .iter()
            .filter(|n| hydro.lattice.dir(n.node).0.x > 0.995)
            .max_by(|a, b| a.discharge_m3s.total_cmp(&b.discharge_m3s))
            .expect("rivers on the continent");
        let a_centre = hydro.lattice.dir(big.node);
        let b_centre = RegionFrame { center: a_centre }.local_to_dir(100_000.0, 30_000.0);
        let a = by_river(&build_at(&hydro, a_centre, &floor, 997));
        let b = by_river(&build_at(&hydro, b_centre, &floor, u32::MAX));
        let (shared, only_one) = agree(&a, &b);
        assert!(shared >= 4, "{shared} shared segments");
        assert!(only_one > 0, "the two regions gather different sets");
        // The step budget changes nothing.
        let again = by_river(&build_at(&hydro, a_centre, &floor, 1));
        assert_eq!(
            a.values().map(|s| bits(s)).collect::<Vec<_>>(),
            again.values().map(|s| bits(s)).collect::<Vec<_>>()
        );
    }

    /// Times a river-dense region: the recent lattice (6 × 900²) over the wet synthetic
    /// continent, centred on its biggest river. Not a gate: `cargo test --release -p
    /// timefly_core --lib -- --ignored --nocapture time_a_river_dense_region`.
    #[test]
    #[ignore = "timing; run in release"]
    fn time_a_river_dense_region() {
        use std::cell::Cell;
        use std::time::Instant;
        let t = Instant::now();
        let hydro = pass(crate::hydrology::params::HYDRO_RECENT_N);
        println!(
            "pass {:.2} s, {} river nodes",
            t.elapsed().as_secs_f64(),
            hydro.rivers.nodes().len()
        );
        let reads = Cell::new(0u64);
        let floor = |d: RenderDir| {
            reads.set(reads.get() + 1);
            continent(d) + 40.0 * (d.0.y * 3000.0).sin() * (d.0.z * 2100.0).cos()
        };
        // The densest 300 km square: the most refined river nodes within 150 km.
        let mut near = Vec::new();
        let best = hydro
            .rivers
            .nodes()
            .iter()
            .step_by(37)
            .filter(|n| n.discharge_m3s >= MIN_Q)
            .map(|n| {
                hydro
                    .rivers
                    .near(&hydro.lattice, hydro.lattice.dir(n.node), HALF, &mut near);
                let refined = near
                    .iter()
                    .filter(|&&i| hydro.rivers.nodes()[i as usize].discharge_m3s >= MIN_Q)
                    .count();
                (refined, n.node)
            })
            .max()
            .expect("rivers");
        let centre = hydro.lattice.dir(best.1);
        for run in 0..3 {
            reads.set(0);
            let t = Instant::now();
            let mut b = RiverBuilder::new(
                &hydro,
                &RegionFrame { center: centre },
                HALF,
                SUBPOINT_M,
                MIN_Q,
            );
            let gather = t.elapsed().as_secs_f64();
            let (segs, reach) = (b.segs.len(), b.reach.len());
            while !b.step(&floor, &key, u32::MAX) {}
            let rr = b.finish();
            let build = t.elapsed().as_secs_f64();
            let basis = RegionFrame { center: centre }.basis();
            let t = Instant::now();
            let (mut hits, mut sum) = (0u32, 0.0);
            for row in 0..1025u32 {
                for col in 0..1025u32 {
                    let x = -HALF + f64::from(col) * INNER_TEXEL_M;
                    let z = -HALF + f64::from(row) * INNER_TEXEL_M;
                    let d = basis.local_to_dir(x, z);
                    let out = rr.texel(d, 500.0, INNER_TEXEL_M);
                    hits += u32::from(out.hit.is_some());
                    sum += out.h_m;
                }
            }
            let texels = t.elapsed().as_secs_f64();
            println!(
                "run {run}: {} refined river nodes within 150 km; gathered {segs} segments ({reach} in \
                 reach), {} sub-points, {} floor reads; RiverBuilder new {:.1} ms, new..finish \
                 {:.1} ms; texels 1025² {:.1} ms ({hits} hits, {:.1} KiB, checksum {sum:.0})",
                best.0,
                rr.subpoints().len(),
                reads.get(),
                gather * 1e3,
                build * 1e3,
                texels * 1e3,
                rr.bytes() as f64 / 1024.0,
            );
        }
    }
}
