//! Joining paths (rivers Task 11e): a river segment from a node off today's burnt courses into
//! a burnt course node is refined along the least-cost path over the field's terrain instead of
//! the relaxed chord, so a tributary reaches its canyon down its own side valley rather than
//! through a ridge between them.
//!
//! The rules ([`RiverBuilder`](super::rivers::RiverBuilder) and
//! [`relaxed_centreline`](super::rivers::relaxed_centreline) apply them):
//!
//! ```text
//! Lattice: from p0 (the start node relaxed) to p1 (the course node's anchor), I = ceil(len / s)
//!   columns along the great circle (column i at t = i / I) and J = floor(H / s) rows either
//!   side across it (row j offset j·s toward the chord's left), H = JOIN_PATH_HALF_WIDTH_FRACTION
//!   × the lattice's mean spacing and s = max(the field's sub-point spacing,
//!   √(len · 2H / MAX_POINTS)) (the sub-point spacing on the recent lattice). (0, 0) is p0 and (I, 0)
//!   is p1 exactly. A function of p0, p1, the chord normal and s alone: every region that
//!   refines the segment builds the same lattice.
//! Floors: floor_at at every lattice point (NaN reads as +∞).
//! Cost: 8-neighbour steps; a step of great-circle length ℓ between points a and b costs
//!   ℓ × (1 + JOIN_PATH_CLIMB_WEIGHT × max(0, (f_a + f_b)/2 − (line_a + line_b)/2)), line the
//!   straight line in t between the floors at p0 and p1. Dijkstra from (0, 0) to (I, 0); the
//!   heap orders by (cost bits, point index), neighbours in a fixed order, a cost replaced only
//!   when strictly lower. The chord (row 0) is a lattice path, so the path never costs more.
//! Centreline: the path's points resampled at K + 1 equal arc-length fractions,
//!   K = ceil(path length / s), each between its two path points on their great circle; then the
//!   interior points smoothed once, [1, 2, 1] / 4 and normalised. The ends are p0 and p1.
//! ```
//!
//! The path stays within H of the chord between the relaxed ends, so the gather's relaxation
//! reach allows max(C, H) beyond the node's own relaxation.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bevy_math::DVec3;

use super::rivers::{across, chord_point, offset_point};
use crate::geo::RenderDir;
use crate::hydrology::params::JOIN_PATH_CLIMB_WEIGHT;
use crate::worldgen::frame::EARTH_RADIUS_M;

/// A search lattice holds about this many points at most: its spacing grows past the sub-point
/// spacing to keep within it. The recent lattice's links (at most 2 spacings, 20.5 km, with a
/// 10.2 km half-width at 500 m) take under half of it; a coarse lattice's (tests', at 100 km
/// spacings) would otherwise take hundreds of thousands.
const MAX_POINTS: usize = 4096;

/// The eight steps, in the search's fixed order: `(di, dj)`.
const STEPS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (1, -1),
    (0, 1),
    (0, -1),
    (-1, 0),
    (-1, 1),
    (-1, -1),
];

/// A joining segment's search lattice.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct JoinGrid {
    p0: DVec3,
    p1: DVec3,
    normal: DVec3,
    step_m: f64,
    /// The last column, `I`.
    ni: u32,
    /// Rows either side, `J`.
    nj: u32,
}

fn clean(f: f64) -> f64 {
    if f.is_nan() { f64::INFINITY } else { f }
}

impl JoinGrid {
    /// The lattice between the relaxed ends `p0` and `p1` whose chord has the unit `normal`
    /// (left of travel), `half_m` either side, at spacing `subpoint_m`, or coarser where that
    /// would pass [`MAX_POINTS`] (a coarse lattice's long links).
    pub(super) fn new(p0: DVec3, p1: DVec3, normal: DVec3, subpoint_m: f64, half_m: f64) -> Self {
        let len = p0.angle_between(p1) * EARTH_RADIUS_M;
        let step_m = subpoint_m.max((len * 2.0 * half_m / MAX_POINTS as f64).sqrt());
        JoinGrid {
            p0,
            p1,
            normal,
            step_m,
            ni: (len / step_m).ceil().max(1.0) as u32,
            nj: (half_m / step_m).floor().max(0.0) as u32,
        }
    }

    /// The columns, `I + 1`: what reading the floors takes, a column at a time.
    pub(super) fn columns(&self) -> u32 {
        self.ni + 1
    }

    fn rows(&self) -> usize {
        2 * self.nj as usize + 1
    }

    /// The points of the lattice.
    pub(super) fn len(&self) -> usize {
        self.columns() as usize * self.rows()
    }

    fn index(&self, i: u32, j: i32) -> usize {
        i as usize * self.rows() + (j + self.nj as i32) as usize
    }

    fn of_index(&self, k: usize) -> (u32, i32) {
        let rows = self.rows();
        ((k / rows) as u32, (k % rows) as i32 - self.nj as i32)
    }

    /// Point `(i, j)`.
    pub(super) fn point(&self, i: u32, j: i32) -> DVec3 {
        let t = if i == self.ni {
            1.0
        } else {
            f64::from(i) / f64::from(self.ni)
        };
        let c = chord_point(self.p0, self.p1, t);
        if j == 0 {
            c
        } else {
            offset_point(c, across(c, self.normal), f64::from(j) * self.step_m)
        }
    }

    /// Column `i`'s floors, rows `−J..=J`, appended to `floors` (columns in order).
    pub(super) fn read_column(
        &self,
        i: u32,
        floor_at: &impl Fn(RenderDir) -> f64,
        floors: &mut Vec<f64>,
    ) {
        let nj = self.nj as i32;
        floors.extend((-nj..=nj).map(|j| clean(floor_at(RenderDir(self.point(i, j))))));
    }

    /// Every floor, column by column.
    pub(super) fn read_all(&self, floor_at: &impl Fn(RenderDir) -> f64) -> Vec<f64> {
        let mut floors = Vec::with_capacity(self.len());
        for i in 0..self.columns() {
            self.read_column(i, floor_at, &mut floors);
        }
        floors
    }

    /// The cost of the step between points `a` and `b` (indices).
    fn step_cost(&self, floors: &[f64], a: usize, b: usize) -> f64 {
        let ((ia, ja), (ib, jb)) = (self.of_index(a), self.of_index(b));
        let (f0, f1) = (floors[self.index(0, 0)], floors[self.index(self.ni, 0)]);
        let line = |i: u32| f0 + (f1 - f0) * f64::from(i) / f64::from(self.ni);
        let above = (0.5 * (floors[a] + floors[b]) - 0.5 * (line(ia) + line(ib))).max(0.0);
        let len = self.point(ia, ja).angle_between(self.point(ib, jb)) * EARTH_RADIUS_M;
        len * (1.0 + JOIN_PATH_CLIMB_WEIGHT * above)
    }

    /// The least-cost path's point indices from `(0, 0)` to `(I, 0)`, and its cost. When no
    /// path reaches the goal (a wall of non-finite floors across the half-width), the chord
    /// (row 0), at an infinite cost.
    pub(super) fn search(&self, floors: &[f64]) -> (Vec<usize>, f64) {
        debug_assert_eq!(floors.len(), self.len());
        let (start, goal) = (self.index(0, 0), self.index(self.ni, 0));
        let mut cost = vec![f64::INFINITY; self.len()];
        let mut prev = vec![usize::MAX; self.len()];
        let mut heap = BinaryHeap::new();
        cost[start] = 0.0;
        heap.push(Reverse((0u64, start)));
        let (ni, nj) = (self.ni as i32, self.nj as i32);
        while let Some(Reverse((bits, u))) = heap.pop() {
            let c = f64::from_bits(bits);
            if c > cost[u] {
                continue;
            }
            if u == goal {
                break;
            }
            let (i, j) = self.of_index(u);
            for (di, dj) in STEPS {
                let (a, b) = (i as i32 + di, j + dj);
                if a < 0 || a > ni || b < -nj || b > nj {
                    continue;
                }
                let v = self.index(a as u32, b);
                let next = c + self.step_cost(floors, u, v);
                if next < cost[v] {
                    cost[v] = next;
                    prev[v] = u;
                    heap.push(Reverse((next.to_bits(), v)));
                }
            }
        }
        if !cost[goal].is_finite() {
            return (
                (0..=self.ni).map(|i| self.index(i, 0)).collect(),
                f64::INFINITY,
            );
        }
        let mut path = vec![goal];
        while let Some(&k) = path.last() {
            if k == start {
                break;
            }
            path.push(prev[k]);
        }
        path.reverse();
        (path, cost[goal])
    }

    /// The chord's cost: row 0 from `(0, 0)` to `(I, 0)`.
    #[cfg(test)]
    pub(super) fn chord_cost(&self, floors: &[f64]) -> f64 {
        (0..self.ni)
            .map(|i| self.step_cost(floors, self.index(i, 0), self.index(i + 1, 0)))
            .sum()
    }

    /// The joining centreline from the floors: the path resampled every at most `subpoint_m`
    /// and smoothed (the module's rules). Its ends are exactly `p0` and `p1`.
    pub(super) fn centreline(&self, floors: &[f64], subpoint_m: f64) -> Vec<DVec3> {
        let (path, _) = self.search(floors);
        let pts: Vec<DVec3> = path
            .iter()
            .map(|&k| {
                let (i, j) = self.of_index(k);
                self.point(i, j)
            })
            .collect();
        let mut along = vec![0.0f64; pts.len()];
        for k in 1..pts.len() {
            along[k] = along[k - 1] + pts[k - 1].angle_between(pts[k]) * EARTH_RADIUS_M;
        }
        let total = along[pts.len() - 1];
        let k_max = (total / subpoint_m).ceil().max(1.0) as u32;
        let mut seg = 0;
        let resampled: Vec<DVec3> = (0..=k_max)
            .map(|k| {
                if k == 0 {
                    return pts[0];
                }
                if k == k_max {
                    return pts[pts.len() - 1];
                }
                let target = total * f64::from(k) / f64::from(k_max);
                while seg + 2 < pts.len() && along[seg + 1] < target {
                    seg += 1;
                }
                let span = along[seg + 1] - along[seg];
                let t = if span > 0.0 {
                    ((target - along[seg]) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                chord_point(pts[seg], pts[seg + 1], t)
            })
            .collect();
        (0..resampled.len())
            .map(|k| {
                if k == 0 || k + 1 == resampled.len() {
                    resampled[k]
                } else {
                    (resampled[k - 1] + 2.0 * resampled[k] + resampled[k + 1]).normalize()
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A unit vector `east_m` east and `north_m` north of (0°, 0°) (render +Z faces longitude 0).
    fn at(east_m: f64, north_m: f64) -> DVec3 {
        let (lon, lat) = (east_m / EARTH_RADIUS_M, north_m / EARTH_RADIUS_M);
        DVec3::new(lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos())
    }

    fn east_north(d: DVec3) -> (f64, f64) {
        (
            d.x.atan2(d.z) * EARTH_RADIUS_M,
            d.y.clamp(-1.0, 1.0).asin() * EARTH_RADIUS_M,
        )
    }

    fn grid(p0: DVec3, p1: DVec3) -> JoinGrid {
        let normal = p0.cross(p1).normalize();
        JoinGrid::new(p0, p1, normal, 500.0, 6_000.0)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Task 11e: on any field the joining path never costs more than the chord between its
        /// ends, it runs from end to end, and the same floors give the same path.
        #[test]
        fn a_joining_path_never_climbs_more_than_the_chord(
            end in (4_000.0f64..20_000.0, -8_000.0f64..8_000.0),
            bumps in prop::collection::vec(
                (-5_000.0f64..25_000.0, -12_000.0f64..12_000.0, -900.0f64..900.0, 500.0f64..6_000.0),
                0..10,
            ),
        ) {
            let field = |d: RenderDir| {
                let (e, n) = east_north(d.0);
                bumps.iter().fold(800.0 - 0.01 * e, |f, &(be, bn, amp, r)| {
                    f + amp * (-((e - be).powi(2) + (n - bn).powi(2)) / (r * r)).exp()
                })
            };
            let (p0, p1) = (at(0.0, 0.0), at(end.0, end.1));
            let g = grid(p0, p1);
            let floors = g.read_all(&field);
            let (path, cost) = g.search(&floors);
            prop_assert!(cost <= g.chord_cost(&floors), "{cost} > {}", g.chord_cost(&floors));
            prop_assert_eq!(path[0], g.index(0, 0));
            prop_assert_eq!(*path.last().unwrap(), g.index(g.ni, 0));
            prop_assert_eq!(g.search(&floors), (path, cost));
            let line = g.centreline(&floors, 500.0);
            prop_assert_eq!(line[0], p0);
            prop_assert_eq!(*line.last().unwrap(), p1);
            for w in line.windows(2) {
                prop_assert!(w[0].angle_between(w[1]) * EARTH_RADIUS_M <= 2.0 * 500.0);
            }
        }
    }

    /// A coarse lattice's long link keeps its search small: the spacing grows past the
    /// sub-point spacing (a 200 km link with a 100 km half-width at 500 m would take 160,000
    /// points). A recent lattice's longest link keeps the sub-point spacing.
    #[test]
    fn a_long_links_lattice_stays_small() {
        let (p0, p1) = (at(0.0, 0.0), at(200_000.0, 0.0));
        let g = JoinGrid::new(p0, p1, p0.cross(p1).normalize(), 500.0, 100_000.0);
        assert!(
            g.len() <= MAX_POINTS + 2 * g.columns() as usize + g.rows(),
            "{}",
            g.len()
        );
        assert!(g.step_m > 500.0);
        let (p0, p1) = (at(0.0, 0.0), at(34_000.0, 0.0));
        let g = JoinGrid::new(p0, p1, p0.cross(p1).normalize(), 500.0, 10_245.0);
        assert_eq!(g.step_m, 500.0);
    }

    /// A ridge across the chord with a gap at one side: the path takes the gap (no lattice point
    /// of it on the ridge), its cost is below the chord's, and the centreline stays within the
    /// half-width. (Smoothing may cut the gap's corner by a fraction of a step.)
    #[test]
    fn a_joining_path_takes_the_gap_round_a_ridge() {
        let field = |d: RenderDir| {
            let (e, n) = east_north(d.0);
            if (4_000.0..5_000.0).contains(&e) && n < 3_000.0 {
                2_000.0
            } else {
                500.0
            }
        };
        let (p0, p1) = (at(0.0, 0.0), at(10_000.0, 0.0));
        let g = grid(p0, p1);
        let floors = g.read_all(&field);
        let (path, cost) = g.search(&floors);
        assert!(cost < g.chord_cost(&floors));
        assert!(path.iter().all(|&k| floors[k] == 500.0));
        let line = g.centreline(&floors, 500.0);
        assert!(line.iter().all(|&d| east_north(d).1.abs() <= 6_000.0 + 1.0));
    }

    /// A wall of non-finite floors across the whole half-width (no data, read as NaN, which
    /// `clean` makes infinite) leaves the goal unreachable: the search falls back to the chord,
    /// at an infinite cost, rather than following an unset predecessor, and the centreline still
    /// runs from end to end.
    #[test]
    fn an_unreachable_goal_falls_back_to_the_chord() {
        let field = |d: RenderDir| {
            let (e, _) = east_north(d.0);
            if (4_000.0..5_000.0).contains(&e) {
                f64::NAN
            } else {
                500.0
            }
        };
        let (p0, p1) = (at(0.0, 0.0), at(10_000.0, 0.0));
        let g = grid(p0, p1);
        let floors = g.read_all(&field);
        let (path, cost) = g.search(&floors);
        assert!(cost.is_infinite() && cost > 0.0, "{cost}");
        let chord: Vec<usize> = (0..=g.ni).map(|i| g.index(i, 0)).collect();
        assert_eq!(path, chord);
        let line = g.centreline(&floors, 500.0);
        assert_eq!(line[0], p0);
        assert_eq!(*line.last().unwrap(), p1);
        assert!(line.iter().all(|&d| east_north(d).1.abs() <= 1.0));
    }
}
