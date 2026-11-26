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
    /// lake levels for [`RiverGraph::down_bed_m`]. The same as stepping a [`GraphBuild`] to the
    /// end. (The pass steps a [`GraphBuild`] itself; tests build at once.)
    #[cfg(test)]
    pub(crate) fn build(
        lattice: &CubeLattice,
        river_nodes: Vec<u32>,
        down_of: impl Fn(u32) -> Down,
        info: impl Fn(u32) -> (f32, f32, f32),
        lakes: &[Lake],
    ) -> RiverGraph {
        let mut build = GraphBuild::new(river_nodes);
        while !build.step(lattice, &down_of, &info, lakes, u32::MAX).0 {}
        build.finish()
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

/// Where a [`GraphBuild`] stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum BuildPhase {
    /// Classifying each river node.
    Nodes,
    /// Counting each node's upstream links, then their prefix sums.
    Count,
    Prefix,
    /// Filling the upstream lists.
    Link,
    /// Each node's pending upstream count, then the nodes with none.
    Pending,
    Seed,
    /// Beds, upstream first.
    Beds,
    DownBed,
    /// Bucketing the nodes for [`RiverGraph::near`]: counts, prefix sums, items, caps.
    BucketCount,
    BucketPrefix,
    BucketFill,
    Caps,
    Done,
}

/// [`RiverGraph::build`] in resumable steps, so a time-boxed pass (rivers and lakes Task 10,
/// fix round 1: the build in one go took 12.8 ms natively at 0 ka) never builds the graph at
/// once. A node, a link, a prefix entry or a bucket is a unit; a bucket's cap costs its items
/// as well. The work and its order are [`RiverGraph::build`]'s, so the graph is the same.
pub(crate) struct GraphBuild {
    river_nodes: Vec<u32>,
    phase: BuildPhase,
    next: usize,
    nodes: Vec<RiverNode>,
    upstream_start: Vec<u32>,
    fill: Vec<u32>,
    upstream: Vec<u32>,
    pending: Vec<u32>,
    queue: VecDeque<usize>,
    down_bed: Vec<f32>,
    bucket_start: Vec<u32>,
    bucket_items: Vec<u32>,
    bucket_caps: Vec<[f64; 4]>,
}

impl GraphBuild {
    pub(crate) fn new(river_nodes: Vec<u32>) -> GraphBuild {
        GraphBuild {
            nodes: Vec::with_capacity(river_nodes.len()),
            river_nodes,
            phase: BuildPhase::Nodes,
            next: 0,
            upstream_start: Vec::new(),
            fill: Vec::new(),
            upstream: Vec::new(),
            pending: Vec::new(),
            queue: VecDeque::new(),
            down_bed: Vec::new(),
            bucket_start: Vec::new(),
            bucket_items: Vec::new(),
            bucket_caps: Vec::new(),
        }
    }

    pub(crate) fn done(&self) -> bool {
        self.phase == BuildPhase::Done
    }

    /// The bytes it holds, by capacity.
    pub(crate) fn working_bytes(&self) -> usize {
        4 * (self.river_nodes.capacity()
            + self.upstream_start.capacity()
            + self.fill.capacity()
            + self.upstream.capacity()
            + self.pending.capacity()
            + self.down_bed.capacity()
            + self.bucket_start.capacity()
            + self.bucket_items.capacity())
            + self.nodes.capacity() * std::mem::size_of::<RiverNode>()
            + self.queue.capacity() * std::mem::size_of::<usize>()
            + self.bucket_caps.capacity() * 32
    }

    /// Does up to about `budget` units (a bucket's cap may finish past it); `(done, used)`. The
    /// arguments are [`RiverGraph::build`]'s and must be the same on every call.
    pub(crate) fn step(
        &mut self,
        lattice: &CubeLattice,
        down_of: &impl Fn(u32) -> Down,
        info: &impl Fn(u32) -> (f32, f32, f32),
        lakes: &[Lake],
        budget: u32,
    ) -> (bool, u32) {
        let r = self.river_nodes.len();
        let side = lattice.n.div_ceil(NEAR_BUCKET_TEXELS);
        let buckets = (6 * side * side) as usize;
        let bucket = |v: u32| {
            let (face, col, row) = lattice.face_col_row(v);
            let (bc, br) = (col / NEAR_BUCKET_TEXELS, row / NEAR_BUCKET_TEXELS);
            ((face as u32 * side + br) * side + bc) as usize
        };
        let mut used: u32 = 0;
        while used < budget && self.phase != BuildPhase::Done {
            used += 1;
            let k = self.next;
            match self.phase {
                BuildPhase::Nodes => {
                    if k == r {
                        self.upstream_start = vec![0u32; r + 1];
                        self.phase = BuildPhase::Count;
                        self.next = 0;
                        continue;
                    }
                    let v = self.river_nodes[k];
                    let (discharge_m3s, area_km2, elevation_m) = info(v);
                    let down = match down_of(v) {
                        Down::River(j) => match self.river_nodes.binary_search(&j) {
                            Ok(t) => Down::River(t as u32),
                            Err(_) => {
                                // Discharge never shrinks off a lake, so a river's receiver is
                                // a river unless it is ocean, lake or ice.
                                debug_assert!(false, "river node {v} drains to non-river land {j}");
                                Down::Ice(j)
                            }
                        },
                        other => other,
                    };
                    self.nodes.push(RiverNode {
                        node: v,
                        down,
                        area_km2,
                        discharge_m3s,
                        bed_m: elevation_m,
                    });
                }
                BuildPhase::Count => {
                    if k == r {
                        self.phase = BuildPhase::Prefix;
                        self.next = 0;
                        continue;
                    }
                    if let Down::River(t) = self.nodes[k].down {
                        self.upstream_start[t as usize + 1] += 1;
                    }
                }
                BuildPhase::Prefix => {
                    if k == r {
                        self.fill = self.upstream_start.clone();
                        self.upstream = vec![0u32; self.upstream_start[r] as usize];
                        self.phase = BuildPhase::Link;
                        self.next = 0;
                        continue;
                    }
                    self.upstream_start[k + 1] += self.upstream_start[k];
                }
                BuildPhase::Link => {
                    if k == r {
                        self.fill = Vec::new();
                        self.pending = Vec::with_capacity(r);
                        self.phase = BuildPhase::Pending;
                        self.next = 0;
                        continue;
                    }
                    if let Down::River(t) = self.nodes[k].down {
                        let t = t as usize;
                        self.upstream[self.fill[t] as usize] = k as u32;
                        self.fill[t] += 1;
                    }
                }
                BuildPhase::Pending => {
                    if k == r {
                        self.phase = BuildPhase::Seed;
                        self.next = 0;
                        continue;
                    }
                    self.pending
                        .push(self.upstream_start[k + 1] - self.upstream_start[k]);
                }
                BuildPhase::Seed => {
                    if k == r {
                        self.phase = BuildPhase::Beds;
                        self.next = 0;
                        continue;
                    }
                    if self.pending[k] == 0 {
                        self.queue.push_back(k);
                    }
                }
                BuildPhase::Beds => {
                    // `min` is exact, so any topological order gives the same beds.
                    let Some(k) = self.queue.pop_front() else {
                        debug_assert!(
                            self.pending.iter().all(|&p| p == 0),
                            "river links form a cycle"
                        );
                        self.pending = Vec::new();
                        self.queue = VecDeque::new();
                        self.down_bed = Vec::with_capacity(r);
                        self.phase = BuildPhase::DownBed;
                        self.next = 0;
                        continue;
                    };
                    if let Down::River(t) = self.nodes[k].down {
                        let t = t as usize;
                        self.nodes[t].bed_m = self.nodes[t].bed_m.min(self.nodes[k].bed_m);
                        self.pending[t] -= 1;
                        if self.pending[t] == 0 {
                            self.queue.push_back(t);
                        }
                    }
                    continue;
                }
                BuildPhase::DownBed => {
                    if k == r {
                        self.bucket_start = vec![0u32; buckets + 1];
                        self.phase = BuildPhase::BucketCount;
                        self.next = 0;
                        continue;
                    }
                    let n = &self.nodes[k];
                    self.down_bed.push(match n.down {
                        Down::River(t) => self.nodes[t as usize].bed_m,
                        Down::Ocean(_) => n.bed_m.min(0.0),
                        Down::Lake(l, _) => n.bed_m.min(lakes[l as usize].level_m),
                        Down::Ice(_) | Down::Sink(_) => n.bed_m,
                    });
                }
                BuildPhase::BucketCount => {
                    if k == r {
                        self.phase = BuildPhase::BucketPrefix;
                        self.next = 0;
                        continue;
                    }
                    self.bucket_start[bucket(self.nodes[k].node) + 1] += 1;
                }
                BuildPhase::BucketPrefix => {
                    if k == buckets {
                        self.fill = self.bucket_start.clone();
                        self.bucket_items = vec![0u32; r];
                        self.phase = BuildPhase::BucketFill;
                        self.next = 0;
                        continue;
                    }
                    self.bucket_start[k + 1] += self.bucket_start[k];
                }
                BuildPhase::BucketFill => {
                    if k == r {
                        self.fill = Vec::new();
                        self.bucket_caps = Vec::with_capacity(buckets);
                        self.phase = BuildPhase::Caps;
                        self.next = 0;
                        continue;
                    }
                    let b = bucket(self.nodes[k].node);
                    self.bucket_items[self.fill[b] as usize] = k as u32;
                    self.fill[b] += 1;
                }
                BuildPhase::Caps => {
                    if k == buckets {
                        self.phase = BuildPhase::Done;
                        continue;
                    }
                    let items = &self.bucket_items
                        [self.bucket_start[k] as usize..self.bucket_start[k + 1] as usize];
                    used = used.saturating_add(items.len() as u32);
                    let cap = match items.first() {
                        None => [0.0; 4],
                        Some(&first) => {
                            let (face, col, row) =
                                lattice.face_col_row(self.nodes[first as usize].node);
                            let half = NEAR_BUCKET_TEXELS / 2;
                            let mid = |c: u32| {
                                ((c / NEAR_BUCKET_TEXELS) * NEAR_BUCKET_TEXELS + half)
                                    .min(lattice.n - 1)
                            };
                            let centre =
                                crate::cube::texel_center_dir(face, mid(col), mid(row), lattice.n)
                                    .0;
                            let radius = items
                                .iter()
                                .map(|&i| angle(centre, lattice.dir(self.nodes[i as usize].node).0))
                                .fold(0.0, f64::max);
                            [centre.x, centre.y, centre.z, radius]
                        }
                    };
                    self.bucket_caps.push(cap);
                }
                BuildPhase::Done => unreachable!("the loop stops at Done"),
            }
            self.next += 1;
        }
        (self.phase == BuildPhase::Done, used)
    }

    /// The graph, once [`step`](Self::step) has returned done.
    pub(crate) fn finish(self) -> RiverGraph {
        assert!(self.done(), "the graph build is not finished");
        RiverGraph {
            nodes: self.nodes,
            upstream_start: self.upstream_start,
            upstream: self.upstream,
            down_bed: self.down_bed,
            bucket_start: self.bucket_start,
            bucket_items: self.bucket_items,
            bucket_caps: self.bucket_caps,
        }
    }
}
