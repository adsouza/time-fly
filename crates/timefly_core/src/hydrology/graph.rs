//! The river graph (rivers and lakes design §4.1, "Output"): the lattice nodes that carry a river,
//! each with the node it drains to, its discharge and its bed.
//!
//! A river node is not ocean, not ice, not in a lake's footprint, and carries at least
//! [`RIVER_MIN_DISCHARGE_M3S`]. Its bed is the lowest of its own elevation and its upstream river
//! nodes' beds, so beds never rise downstream. Every chain of [`Down::River`] links ends in the
//! ocean, a lake, the ice or a closed basin's dry sink.
//!
//! [`RIVER_MIN_DISCHARGE_M3S`]: super::params::RIVER_MIN_DISCHARGE_M3S

use std::collections::VecDeque;

use bevy_math::DVec3;

use super::{CubeLattice, Lake};
use crate::geo::RenderDir;
use crate::worldgen::frame::EARTH_RADIUS_M;

/// The side of [`RiverGraph::near`]'s buckets, in lattice texels.
pub const NEAR_BUCKET_TEXELS: u32 = 32;

/// Where a river node drains.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Down {
    /// Another river node, by index into [`RiverGraph::nodes`].
    River(u32),
    /// The ocean, at this lattice node.
    Ocean(u32),
    /// A lake (index into `EraHydrology::lakes`), entered at this lattice node.
    Lake(u16, u32),
    /// An ice node.
    Ice(u32),
    /// A closed basin's lowest node with no lake on it (a dry sink), where the river ends: the
    /// river node itself, or the node it drains to.
    Sink(u32),
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RiverNode {
    /// The lattice node.
    pub node: u32,
    pub down: Down,
    /// The area draining through the node.
    pub area_km2: f32,
    pub discharge_m3s: f32,
    pub bed_m: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RiverGraph {
    /// Sorted by lattice node.
    nodes: Vec<RiverNode>,
    /// CSR: river node `i`'s upstream river nodes are
    /// `upstream[upstream_start[i]..upstream_start[i + 1]]`, ascending.
    upstream_start: Vec<u32>,
    upstream: Vec<u32>,
    down_bed: Vec<f32>,
    /// Buckets of `NEAR_BUCKET_TEXELS²` texels, face-major: bucket `b`'s river nodes are
    /// `bucket_items[bucket_start[b]..bucket_start[b + 1]]`. Each non-empty bucket has a cap
    /// `[centre x, y, z, angular radius]` holding all its nodes.
    bucket_start: Vec<u32>,
    bucket_items: Vec<u32>,
    bucket_caps: Vec<[f64; 4]>,
}

/// The angle between unit vectors, accurate at small angles.
fn angle(a: DVec3, b: DVec3) -> f64 {
    2.0 * (0.5 * (a - b).length()).min(1.0).asin()
}

impl RiverGraph {
    /// Builds the graph from the river nodes (ascending lattice ids). `down_of(v)` classifies
    /// where node `v` drains, giving [`Down::River`] the receiver's *lattice* node, which must be
    /// a river node; `info(v)` is its `(discharge, drainage area, elevation)`. `lakes` gives the
    /// lake levels for [`RiverGraph::down_bed_m`].
    pub(crate) fn build(
        lattice: &CubeLattice,
        river_nodes: Vec<u32>,
        down_of: impl Fn(u32) -> Down,
        info: impl Fn(u32) -> (f32, f32, f32),
        lakes: &[Lake],
    ) -> RiverGraph {
        let mut nodes: Vec<RiverNode> = river_nodes
            .iter()
            .map(|&v| {
                let (discharge_m3s, area_km2, elevation_m) = info(v);
                let down = match down_of(v) {
                    Down::River(j) => match river_nodes.binary_search(&j) {
                        Ok(k) => Down::River(k as u32),
                        Err(_) => {
                            // Discharge never shrinks off a lake, so a river's receiver is a
                            // river unless it is ocean, lake or ice.
                            debug_assert!(false, "river node {v} drains to non-river land {j}");
                            Down::Ice(j)
                        }
                    },
                    other => other,
                };
                RiverNode {
                    node: v,
                    down,
                    area_km2,
                    discharge_m3s,
                    bed_m: elevation_m,
                }
            })
            .collect();
        let r = nodes.len();
        let mut upstream_start = vec![0u32; r + 1];
        for n in &nodes {
            if let Down::River(t) = n.down {
                upstream_start[t as usize + 1] += 1;
            }
        }
        for k in 0..r {
            upstream_start[k + 1] += upstream_start[k];
        }
        let mut fill = upstream_start.clone();
        let mut upstream = vec![0u32; upstream_start[r] as usize];
        for (k, n) in nodes.iter().enumerate() {
            if let Down::River(t) = n.down {
                upstream[fill[t as usize] as usize] = k as u32;
                fill[t as usize] += 1;
            }
        }
        // Beds upstream first. `min` is exact, so any topological order gives the same beds.
        let mut pending: Vec<u32> = (0..r)
            .map(|k| upstream_start[k + 1] - upstream_start[k])
            .collect();
        let mut queue: VecDeque<usize> = (0..r).filter(|&k| pending[k] == 0).collect();
        while let Some(k) = queue.pop_front() {
            if let Down::River(t) = nodes[k].down {
                let t = t as usize;
                nodes[t].bed_m = nodes[t].bed_m.min(nodes[k].bed_m);
                pending[t] -= 1;
                if pending[t] == 0 {
                    queue.push_back(t);
                }
            }
        }
        debug_assert!(pending.iter().all(|&p| p == 0), "river links form a cycle");
        let down_bed = nodes
            .iter()
            .map(|n| match n.down {
                Down::River(t) => nodes[t as usize].bed_m,
                Down::Ocean(_) => n.bed_m.min(0.0),
                Down::Lake(l, _) => n.bed_m.min(lakes[l as usize].level_m),
                Down::Ice(_) | Down::Sink(_) => n.bed_m,
            })
            .collect();

        let side = lattice.n.div_ceil(NEAR_BUCKET_TEXELS);
        let buckets = (6 * side * side) as usize;
        let bucket = |v: u32| {
            let (face, col, row) = lattice.face_col_row(v);
            let (bc, br) = (col / NEAR_BUCKET_TEXELS, row / NEAR_BUCKET_TEXELS);
            ((face as u32 * side + br) * side + bc) as usize
        };
        let mut bucket_start = vec![0u32; buckets + 1];
        for n in &nodes {
            bucket_start[bucket(n.node) + 1] += 1;
        }
        for b in 0..buckets {
            bucket_start[b + 1] += bucket_start[b];
        }
        let mut fill = bucket_start.clone();
        let mut bucket_items = vec![0u32; r];
        for (k, n) in nodes.iter().enumerate() {
            let b = bucket(n.node);
            bucket_items[fill[b] as usize] = k as u32;
            fill[b] += 1;
        }
        let bucket_caps = (0..buckets)
            .map(|b| {
                let items = &bucket_items[bucket_start[b] as usize..bucket_start[b + 1] as usize];
                let Some(&first) = items.first() else {
                    return [0.0; 4];
                };
                let (face, col, row) = lattice.face_col_row(nodes[first as usize].node);
                let half = NEAR_BUCKET_TEXELS / 2;
                let mid = |k: u32| {
                    ((k / NEAR_BUCKET_TEXELS) * NEAR_BUCKET_TEXELS + half).min(lattice.n - 1)
                };
                let centre = crate::cube::texel_center_dir(face, mid(col), mid(row), lattice.n).0;
                let radius = items
                    .iter()
                    .map(|&k| angle(centre, lattice.dir(nodes[k as usize].node).0))
                    .fold(0.0, f64::max);
                [centre.x, centre.y, centre.z, radius]
            })
            .collect();

        RiverGraph {
            nodes,
            upstream_start,
            upstream,
            down_bed,
            bucket_start,
            bucket_items,
            bucket_caps,
        }
    }

    pub fn nodes(&self) -> &[RiverNode] {
        &self.nodes
    }

    /// The river node at a lattice node, if it carries one.
    pub fn index_of(&self, lattice_node: u32) -> Option<u32> {
        self.nodes
            .binary_search_by_key(&lattice_node, |n| n.node)
            .ok()
            .map(|k| k as u32)
    }

    /// The river nodes draining into river node `i`, ascending.
    pub fn upstream(&self, i: u32) -> &[u32] {
        let i = i as usize;
        &self.upstream[self.upstream_start[i] as usize..self.upstream_start[i + 1] as usize]
    }

    /// The bed where river node `i` ends: the next node's bed, `min(0, bed)` at the ocean,
    /// `min(level, bed)` at a lake, and its own bed at the ice.
    pub fn down_bed_m(&self, i: u32) -> f32 {
        self.down_bed[i as usize]
    }

    /// Every river node within `radius_m` (great-circle, through the nodes' texel centres) of
    /// `dir`, ascending, into `out` (cleared first). Buckets are tested by angular distance, so
    /// the search is the same across cube seams and corners.
    pub fn near(&self, lattice: &CubeLattice, dir: RenderDir, radius_m: f64, out: &mut Vec<u32>) {
        out.clear();
        let d = dir.0.normalize();
        let reach = radius_m / EARTH_RADIUS_M;
        for (b, cap) in self.bucket_caps.iter().enumerate() {
            let (start, end) = (self.bucket_start[b], self.bucket_start[b + 1]);
            if start == end {
                continue;
            }
            let centre = DVec3::new(cap[0], cap[1], cap[2]);
            if angle(d, centre) - cap[3] > reach + 1e-12 {
                continue;
            }
            for &k in &self.bucket_items[start as usize..end as usize] {
                if angle(d, lattice.dir(self.nodes[k as usize].node).0) <= reach {
                    out.push(k);
                }
            }
        }
        out.sort_unstable();
    }

    /// The bytes the graph holds, by capacity.
    pub fn bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<RiverNode>()
            + 4 * (self.upstream_start.capacity()
                + self.upstream.capacity()
                + self.down_bed.capacity()
                + self.bucket_start.capacity()
                + self.bucket_items.capacity())
            + self.bucket_caps.capacity() * 32
    }
}
