//! Priority-flood routing (design §4.1, steps 1 and 2): the ocean, the filled surface, one
//! receiver per node and the basins. Resumable, so the shell can time-slice it on the web; the
//! result does not depend on how the work is sliced.
//!
//! The rules, which the golden fixture (`tests/fixtures/hydro_golden.json`) and the Python
//! reference pin:
//!
//! - **Ocean:** nodes with `e < 0`, by 8-connected component. Start from the largest component
//!   (ties: the lowest node id). Repeat until nothing changes: dilate the ocean by
//!   `ceil(OCEAN_STRAIT_M / spacing)` graph steps through any node, and add every below-zero
//!   component the dilation touches. Dilated land stays land.
//! - **The course corridor** ([`Routing::with_corridor`], Task 11d fix round 3): a burnt
//!   course's interior node that the rules above make ocean is land after all, at its course's
//!   land-chain elevation (never below 0 m): ETOPO's channel and lagoon floors otherwise carry
//!   the sea up estuaries. The labelling itself is unchanged. Each course's corridor ends at its
//!   first such node below [`OPEN_SEA_M`] (open sea: a line drawn past the coast), which
//!   becomes one of the ends an inland node may drain to.
//! - **Inland nodes** ([`Routing::with_inland`]): an ocean node that ends no course pushes no
//!   inland neighbour in the flood, so an inland node's parent is never such an ocean node.
//!   Should the flood run out with nodes unreached, the lowest of them seeds it again.
//! - **Sinks** ([`Routing::with_sinks`], Task 11d fix round 2): for each `(node, level)` of an
//!   endorheic lake, the non-ocean nodes with `e ≤ level` 8-connected to `node` (none if `node`
//!   is ocean or above `level`) are pushed first, keyed `(level, seq)`, with `filled = level` and
//!   no parent: they seed the flood like the sea at that level, and have no receiver. They make
//!   one basin together (whatever `e − level`), its exit the first popped, its rim the exit.
//! - **Flood:** push every ocean node with a non-ocean neighbour, in ascending id, keyed
//!   `(e, seq)`. Pop the minimum `(filled, seq)`; for each unvisited non-ocean neighbour `n`, in
//!   neighbour order: `filled[n] = max(e[n], filled[c])`, `parent[n] = c`, push
//!   `(filled[n], seq++)`. `seq` is a running u64; f32 keys compare by total order. With no ocean
//!   at all, seed the lowest node (ties: the lowest id).
//! - **Receiver:** the neighbour with a strictly lower filled level and the largest
//!   `(filled[i] − filled[j]) / distance` (ties: first in neighbour order); with none strictly
//!   lower, the parent. A burnt course's interior node ([`Routing::with_inland`]) takes no ocean
//!   neighbour as its receiver unless that neighbour ends a course: a course meets the sea only
//!   at its own end (Task 11d fix rounds 2 and 3).
//! - **Basins:** 8-connected components of nodes with `filled > e + DEPRESSION_EPS_M`. The exit
//!   is the basin node popped first; the rim is its parent. Every other basin node whose receiver
//!   leaves the basin is redirected to the exit, so pop order is a topological order (receiver
//!   before node).

use super::HydroGraph;
use super::params::{DEPRESSION_EPS_M, OCEAN_STRAIT_M, OPEN_SEA_M};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};

/// `Routed::receiver` for a node with no receiver: an ocean node, or the seed of a graph with no
/// ocean.
pub const RECEIVER_NONE: u8 = 8;
/// `Routed::receiver` for a basin node redirected to its basin's exit, which need not be a
/// neighbour.
pub const RECEIVER_EXIT: u8 = 9;
/// `Routed::basin_of` for a node in no basin.
pub const NO_BASIN: u32 = u32::MAX;

const NO_PARENT: u8 = RECEIVER_NONE;
const NO_COMPONENT: u32 = u32::MAX;

/// A depression on the filled surface: where a lake may stand (Task 3 decides how full).
#[derive(Clone, Debug, PartialEq)]
pub struct Basin {
    /// The basin's nodes, sorted by id.
    pub nodes: Vec<u32>,
    /// The filled level, the same at every node of the basin.
    pub spill_m: f32,
    /// The basin node popped first; the rest of the basin drains to it.
    pub exit: u32,
    /// The exit's parent, outside the basin: where the basin spills.
    pub rim: u32,
}

/// The routing of one elevation field over one graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Routed {
    /// The elevation routed, one value per node.
    pub elevation_m: Vec<f32>,
    pub ocean: Vec<bool>,
    /// The depression-filled surface. Equal to the elevation on the ocean.
    pub filled_m: Vec<f32>,
    /// The neighbour index of each node's receiver, or [`RECEIVER_NONE`] or [`RECEIVER_EXIT`].
    /// Read it through [`receiver_of`].
    pub receiver: Vec<u8>,
    /// The non-ocean nodes in pop order: every receiver comes before its node.
    pub order: Vec<u32>,
    /// Basins in the order their exits were popped. With data lakes, `HydroPrep` appends the
    /// data's flats after them (their `spill_m` is the flat's level).
    pub basins: Vec<Basin>,
    /// Each node's index into `basins`, or [`NO_BASIN`].
    pub basin_of: Vec<u32>,
}

/// The node `i` drains to, or `None` for the ocean and a no-ocean graph's seed.
pub fn receiver_of(g: &impl HydroGraph, routed: &Routed, i: u32) -> Option<u32> {
    match routed.receiver[i as usize] {
        RECEIVER_NONE => None,
        RECEIVER_EXIT => Some(routed.basins[routed.basin_of[i as usize] as usize].exit),
        k => g.neighbours(i)[usize::from(k)],
    }
}

/// An f32 as a u32 whose unsigned order is [`f32::total_cmp`]'s.
fn ordered(x: f32) -> u32 {
    let bits = x.to_bits();
    if bits & 0x8000_0000 != 0 {
        !bits
    } else {
        bits | 0x8000_0000
    }
}

/// The larger of `a` and `b` by total order.
fn max_total(a: f32, b: f32) -> f32 {
    if a.total_cmp(&b).is_ge() { a } else { b }
}

/// The neighbour index of `of` among `at`'s neighbours.
fn back_index(g: &impl HydroGraph, at: u32, of: u32) -> u8 {
    let k = g.neighbours(at).iter().position(|&j| j == Some(of));
    k.expect("hydrology graphs have symmetric neighbours") as u8
}

/// Nodes per unit call in [`Phase::Steps`] and [`Phase::Ocean`].
const BLOCK_UNITS: u32 = 1024;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Labelling below-zero components; `next` is the next id to scan.
    Label {
        next: u32,
    },
    /// Filling the dilation's step counts, then making component `c` (the largest) ocean from
    /// its `k`-th node on, in blocks: in one unit they took 6.8 ms at n = 900 (rivers and
    /// lakes Task 10, fix round 1).
    Steps {
        c: u32,
    },
    Ocean {
        c: u32,
        k: u32,
    },
    /// Dilating the ocean across straits.
    Dilate,
    /// Growing and pushing the endorheic lakes' sinks.
    Sinks,
    /// Pushing the flood's seeds; `next` is the next id to scan.
    Seed {
        next: u32,
    },
    Flood,
    /// Picking receivers; `next` is the next id.
    Receivers {
        next: u32,
    },
    /// Labelling basins; `next` is the next position in pop order.
    Basins {
        next: u32,
    },
    /// Redirecting basin `basin`'s nodes from `k` on.
    Redirect {
        basin: u32,
        k: u32,
    },
    Done,
}

/// A resumable router. Call [`Routing::step`] with the same graph until it returns `true`, then
/// [`Routing::finish`].
pub struct Routing {
    elevation: Vec<f32>,
    strait_m: f64,
    phase: Phase,
    queue: VecDeque<u32>,
    // Below-zero components, numbered in scan order (so by their lowest id), and the dilation's
    // step counts. Freed once the ocean is known.
    component: Vec<u32>,
    component_start: Vec<u32>,
    component_nodes: Vec<u32>,
    component_ocean: Vec<bool>,
    steps: Vec<u32>,
    reach: u32,
    lowest: u32,
    ocean: Vec<bool>,
    ocean_count: u32,
    any_ocean: bool,
    heap: BinaryHeap<Reverse<(u32, u64, u32)>>,
    seq: u64,
    visited: Vec<bool>,
    filled: Vec<f32>,
    parent: Vec<u8>,
    order: Vec<u32>,
    receiver: Vec<u8>,
    basins: Vec<Basin>,
    basin_of: Vec<u32>,
    open: Option<Basin>,
    units: u64,
    /// Nodes (sorted) that take no ocean neighbour as their receiver.
    inland: Vec<u32>,
    /// Nodes (sorted) that end a burnt course: the only ocean an inland node may drain to.
    ends: Vec<u32>,
    /// Each course's interior nodes below the sea, in course order: `(node, land elevation,
    /// own elevation)`.
    corridor: Vec<Vec<(u32, f32, f32)>>,
    /// Non-ocean nodes the flood has reached.
    reached: u32,
    /// The corridor's nodes made land (sorted), while it is applied.
    made_land: Vec<u32>,
    /// Endorheic lakes' `(node, level)`, and once grown, each node's sink flag (empty with none).
    sink_points: Vec<(u32, f32)>,
    sink: Vec<bool>,
}

impl Routing {
    /// A router for `elevation_m`, one value per graph node.
    pub fn new(elevation_m: Vec<f32>) -> Self {
        Self::with_strait_m(elevation_m, OCEAN_STRAIT_M)
    }

    /// As [`Routing::new`], with another strait reach (tests show it is what decides).
    pub(crate) fn with_strait_m(elevation_m: Vec<f32>, strait_m: f64) -> Self {
        let len = elevation_m.len();
        assert!(u32::try_from(len).is_ok(), "too many nodes");
        // Arrays the later phases need are allocated when those phases start, so they do not add
        // to the ocean dilation's peak (`working_bytes`).
        Self {
            filled: Vec::new(),
            elevation: elevation_m,
            strait_m,
            phase: Phase::Label { next: 0 },
            queue: VecDeque::new(),
            component: vec![NO_COMPONENT; len],
            component_start: Vec::new(),
            component_nodes: Vec::new(),
            component_ocean: Vec::new(),
            steps: Vec::new(),
            reach: 0,
            lowest: 0,
            ocean: vec![false; len],
            ocean_count: 0,
            any_ocean: false,
            heap: BinaryHeap::new(),
            seq: 0,
            visited: Vec::new(),
            parent: Vec::new(),
            order: Vec::new(),
            receiver: Vec::new(),
            basins: Vec::new(),
            basin_of: Vec::new(),
            open: None,
            units: 0,
            inland: Vec::new(),
            ends: Vec::new(),
            corridor: Vec::new(),
            reached: 0,
            made_land: Vec::new(),
            sink_points: Vec::new(),
            sink: Vec::new(),
        }
    }

    /// The router, with each burnt course's interior nodes below the sea in course order,
    /// `(node, land elevation, own elevation)`: any the rules make ocean stays land at its land
    /// elevation, up to the course's first open-sea node (Task 11d fix round 3).
    pub fn with_corridor(mut self, corridor: Vec<Vec<(u32, f32, f32)>>) -> Self {
        self.corridor = corridor;
        self
    }

    fn is_inland(&self, i: u32) -> bool {
        self.inland.binary_search(&i).is_ok()
    }

    fn is_end(&self, i: u32) -> bool {
        self.ends.binary_search(&i).is_ok()
    }

    /// The router, with each endorheic lake's `(node, level)` seeding the flood like the sea at
    /// its level (Task 11d fix round 2; the module's rules).
    pub fn with_sinks(mut self, points: Vec<(u32, f32)>) -> Self {
        self.sink_points = points;
        self
    }

    /// The router, with `inland` (sorted node ids) never draining into an ocean neighbour: a
    /// burnt course's nodes other than its end (Task 11d fix round 2).
    pub fn with_inland(mut self, inland: Vec<u32>, ends: Vec<u32>) -> Self {
        debug_assert!(inland.is_sorted() && ends.is_sorted());
        self.inland = inland;
        self.ends = ends;
        self
    }

    /// Does up to `budget` units of work (one unit is a node visit or a heap pop) and returns
    /// `true` once routing is complete. `g` must be the same graph on every call.
    pub fn step(&mut self, g: &impl HydroGraph, budget: u32) -> bool {
        assert_eq!(
            g.len() as usize,
            self.elevation.len(),
            "one elevation per node"
        );
        let mut left = budget;
        while left > 0 && self.phase != Phase::Done {
            let cost = self.unit(g);
            self.units += u64::from(cost);
            left = left.saturating_sub(cost);
        }
        self.phase == Phase::Done
    }

    /// The units of work done so far.
    pub fn units_done(&self) -> u64 {
        self.units
    }

    /// The bytes the router holds now, by capacity.
    pub fn working_bytes(&self) -> usize {
        use std::mem::size_of;
        let basins: usize = self
            .basins
            .iter()
            .chain(self.open.as_ref())
            .map(|b| size_of::<Basin>() + b.nodes.capacity() * 4)
            .sum();
        4 * (self.elevation.capacity()
            + self.filled.capacity()
            + self.component.capacity()
            + self.component_start.capacity()
            + self.component_nodes.capacity()
            + self.steps.capacity()
            + self.queue.capacity()
            + self.order.capacity()
            + self.basin_of.capacity()
            + self.inland.capacity()
            + self.ends.capacity()
            + self.made_land.capacity()
            + 3 * self.corridor.iter().map(Vec::capacity).sum::<usize>()
            + 2 * self.sink_points.capacity())
            + self.sink.capacity()
            + self.component_ocean.capacity()
            + self.ocean.capacity()
            + self.visited.capacity()
            + self.parent.capacity()
            + self.receiver.capacity()
            + self.heap.capacity() * size_of::<Reverse<(u32, u64, u32)>>()
            + basins
    }

    /// The result. Panics unless [`Routing::step`] has returned `true`.
    pub fn finish(self) -> Routed {
        assert_eq!(self.phase, Phase::Done, "routing is not finished");
        Routed {
            elevation_m: self.elevation,
            ocean: self.ocean,
            filled_m: self.filled,
            receiver: self.receiver,
            order: self.order,
            basins: self.basins,
            basin_of: self.basin_of,
        }
    }

    fn len(&self) -> u32 {
        self.elevation.len() as u32
    }

    /// One unit of work. Returns its cost, at least 1.
    fn unit(&mut self, g: &impl HydroGraph) -> u32 {
        match self.phase {
            Phase::Label { next } => self.label(g, next),
            Phase::Steps { c } => self.fill_steps(c),
            Phase::Ocean { c, k } => self.add_largest(c, k),
            Phase::Dilate => self.dilate(g),
            Phase::Sinks => self.sinks(g),
            Phase::Seed { next } => self.seed(g, next),
            Phase::Flood => self.flood(g),
            Phase::Receivers { next } => self.pick_receiver(g, next),
            Phase::Basins { next } => self.label_basin(g, next),
            Phase::Redirect { basin, k } => self.redirect(g, basin, k),
            Phase::Done => 1,
        }
    }

    fn label(&mut self, g: &impl HydroGraph, next: u32) -> u32 {
        if let Some(v) = self.queue.pop_front() {
            let c = self.component[v as usize];
            for u in g.neighbours(v).into_iter().flatten() {
                let uu = u as usize;
                if self.elevation[uu] < 0.0 && self.component[uu] == NO_COMPONENT {
                    self.component[uu] = c;
                    self.component_nodes.push(u);
                    self.queue.push_back(u);
                }
            }
            return 1;
        }
        if next == self.len() {
            self.start_dilation(g);
            return 1;
        }
        let i = next as usize;
        let e = self.elevation[i];
        if e.total_cmp(&self.elevation[self.lowest as usize]).is_lt() {
            self.lowest = next;
        }
        if e < 0.0 && self.component[i] == NO_COMPONENT {
            self.component[i] = self.component_start.len() as u32;
            self.component_start.push(self.component_nodes.len() as u32);
            self.component_nodes.push(next);
            self.queue.push_back(next);
        }
        self.phase = Phase::Label { next: next + 1 };
        1
    }

    fn start_dilation(&mut self, g: &impl HydroGraph) {
        self.component_start.push(self.component_nodes.len() as u32);
        let count = self.component_start.len() - 1;
        self.component_ocean = vec![false; count];
        let size = |c: usize| self.component_start[c + 1] - self.component_start[c];
        let mut largest: Option<usize> = None;
        for c in 0..count {
            if largest.is_none_or(|l| size(c) > size(l)) {
                largest = Some(c);
            }
        }
        match largest {
            Some(c) => {
                self.reach = (self.strait_m / g.mean_spacing_m()).ceil() as u32;
                self.steps = Vec::with_capacity(self.elevation.len());
                self.phase = Phase::Steps { c: c as u32 };
            }
            None => self.end_ocean(),
        }
    }

    /// Up to [`BLOCK_UNITS`] more step counts, all `u32::MAX`; then component `c` turns ocean.
    fn fill_steps(&mut self, c: u32) -> u32 {
        let len = self.elevation.len();
        let n = (len - self.steps.len()).min(BLOCK_UNITS as usize);
        self.steps.resize(self.steps.len() + n, u32::MAX);
        if self.steps.len() == len {
            let c = c as usize;
            self.component_ocean[c] = true;
            self.any_ocean = true;
            self.ocean_count += self.component_start[c + 1] - self.component_start[c];
            self.phase = Phase::Ocean {
                c: c as u32,
                k: self.component_start[c],
            };
        }
        (n as u32).max(1)
    }

    /// [`Routing::add_component`]'s nodes for the largest component, from its `k`-th node, up
    /// to [`BLOCK_UNITS`] of them; then the dilation.
    fn add_largest(&mut self, c: u32, k: u32) -> u32 {
        let end = self.component_start[c as usize + 1];
        let stop = end.min(k.saturating_add(BLOCK_UNITS));
        for i in k..stop {
            let v = self.component_nodes[i as usize];
            self.ocean[v as usize] = true;
            self.steps[v as usize] = 0;
            self.queue.push_back(v);
        }
        self.phase = if stop == end {
            Phase::Dilate
        } else {
            Phase::Ocean { c, k: stop }
        };
        (stop - k).max(1)
    }

    /// Makes component `c` ocean, at step 0. Returns the nodes visited.
    fn add_component(&mut self, c: usize) -> u32 {
        self.component_ocean[c] = true;
        self.any_ocean = true;
        let (start, end) = (self.component_start[c], self.component_start[c + 1]);
        self.ocean_count += end - start;
        for k in start..end {
            let v = self.component_nodes[k as usize];
            self.ocean[v as usize] = true;
            self.steps[v as usize] = 0;
            self.queue.push_back(v);
        }
        end - start
    }

    /// One node of the strait dilation. A node's step count only ever falls, and a node is
    /// expanded again when it does, so the result is the fixed point whatever the visit order.
    fn dilate(&mut self, g: &impl HydroGraph) -> u32 {
        let Some(v) = self.queue.pop_front() else {
            self.end_ocean();
            return 1;
        };
        let d = self.steps[v as usize];
        let mut cost = 1;
        if d < self.reach {
            for u in g.neighbours(v).into_iter().flatten() {
                let uu = u as usize;
                if d + 1 < self.steps[uu] {
                    self.steps[uu] = d + 1;
                    let c = self.component[uu];
                    if c != NO_COMPONENT && !self.component_ocean[c as usize] {
                        cost += self.add_component(c as usize);
                    } else {
                        self.queue.push_back(u);
                    }
                }
            }
        }
        cost
    }

    fn end_ocean(&mut self) {
        // The course corridor: interior course nodes the sea reached are land, up to each
        // course's first open-sea node, which ends it. A node another course already made land
        // keeps the lower land elevation.
        for course in std::mem::take(&mut self.corridor) {
            for (v, r, e) in course {
                let i = v as usize;
                if !self.ocean[i] {
                    if self.made_land.binary_search(&v).is_ok() {
                        self.elevation[i] = self.elevation[i].min(r);
                    }
                    continue;
                }
                if e < OPEN_SEA_M {
                    if let Err(at) = self.ends.binary_search(&v) {
                        self.ends.insert(at, v);
                    }
                    break;
                }
                self.ocean[i] = false;
                self.ocean_count -= 1;
                self.elevation[i] = r;
                if let Err(at) = self.made_land.binary_search(&v) {
                    self.made_land.insert(at, v);
                }
            }
        }
        self.made_land = Vec::new();
        self.component = Vec::new();
        self.component_start = Vec::new();
        self.component_nodes = Vec::new();
        self.component_ocean = Vec::new();
        self.steps = Vec::new();
        self.queue = VecDeque::new();
        let len = self.elevation.len();
        self.filled = self.elevation.clone();
        self.visited = vec![false; len];
        self.parent = vec![NO_PARENT; len];
        self.order = Vec::with_capacity(len - self.ocean_count as usize);
        self.phase = Phase::Sinks;
    }

    /// Grows and pushes every endorheic lake's sinks in one unit, costing the nodes visited.
    fn sinks(&mut self, g: &impl HydroGraph) -> u32 {
        self.phase = Phase::Seed { next: 0 };
        if self.sink_points.is_empty() {
            return 1;
        }
        self.sink = vec![false; self.elevation.len()];
        let mut cost = 1u32;
        for &(node, level) in &std::mem::take(&mut self.sink_points) {
            let n = node as usize;
            if self.ocean[n] || self.visited[n] || self.elevation[n] > level {
                continue;
            }
            let mut queue = VecDeque::from([node]);
            self.visited[n] = true;
            self.reached += 1;
            while let Some(v) = queue.pop_front() {
                cost = cost.saturating_add(1);
                self.sink[v as usize] = true;
                self.filled[v as usize] = level;
                self.push(v);
                for u in g.neighbours(v).into_iter().flatten() {
                    let uu = u as usize;
                    if !self.ocean[uu] && !self.visited[uu] && self.elevation[uu] <= level {
                        self.visited[uu] = true;
                        self.reached += 1;
                        queue.push_back(u);
                    }
                }
            }
        }
        cost
    }

    fn is_sink(&self, i: u32) -> bool {
        self.sink.get(i as usize).copied().unwrap_or(false)
    }

    fn push(&mut self, i: u32) {
        self.heap
            .push(Reverse((ordered(self.filled[i as usize]), self.seq, i)));
        self.seq += 1;
    }

    fn seed(&mut self, g: &impl HydroGraph, next: u32) -> u32 {
        if !self.any_ocean {
            if self.len() > 0 {
                self.visited[self.lowest as usize] = true;
                self.reached += 1;
                self.push(self.lowest);
            }
            self.phase = Phase::Flood;
            return 1;
        }
        if next == self.len() {
            self.phase = Phase::Flood;
            return 1;
        }
        let coast = g
            .neighbours(next)
            .into_iter()
            .flatten()
            .any(|u| !self.ocean[u as usize]);
        if self.ocean[next as usize] && coast {
            self.push(next);
        }
        self.phase = Phase::Seed { next: next + 1 };
        1
    }

    fn flood(&mut self, g: &impl HydroGraph) -> u32 {
        if self.heap.is_empty() && self.reached < self.len() - self.ocean_count {
            // Nodes the flood could not reach (an inland node walled in by ocean that ends no
            // course): the lowest seeds it again. One scan; never seen on real data.
            let lowest = (0..self.len())
                .filter(|&i| !self.ocean[i as usize] && !self.visited[i as usize])
                .min_by(|&a, &b| {
                    self.elevation[a as usize]
                        .total_cmp(&self.elevation[b as usize])
                        .then(a.cmp(&b))
                })
                .expect("an unreached node");
            self.visited[lowest as usize] = true;
            self.reached += 1;
            self.push(lowest);
            return self.len();
        }
        let Some(Reverse((_, _, c))) = self.heap.pop() else {
            self.heap = BinaryHeap::new();
            self.visited = Vec::new();
            self.receiver = vec![RECEIVER_NONE; self.elevation.len()];
            self.phase = Phase::Receivers { next: 0 };
            return 1;
        };
        if !self.ocean[c as usize] {
            self.order.push(c);
        }
        let level = self.filled[c as usize];
        // An ocean node that ends no course reaches no inland node.
        let shut = self.ocean[c as usize] && !self.inland.is_empty() && !self.is_end(c);
        for u in g.neighbours(c).into_iter().flatten() {
            let uu = u as usize;
            if self.ocean[uu] || self.visited[uu] || (shut && self.is_inland(u)) {
                continue;
            }
            self.visited[uu] = true;
            self.reached += 1;
            self.filled[uu] = max_total(self.elevation[uu], level);
            self.parent[uu] = back_index(g, u, c);
            self.push(u);
        }
        1
    }

    fn pick_receiver(&mut self, g: &impl HydroGraph, next: u32) -> u32 {
        if next == self.len() {
            self.basin_of = vec![NO_BASIN; self.elevation.len()];
            self.phase = Phase::Basins { next: 0 };
            return 1;
        }
        let i = next as usize;
        if !self.ocean[i] && !self.is_sink(next) {
            let level = self.filled[i];
            let inland = self.is_inland(next);
            let mut best: Option<(u8, f64)> = None;
            for (k, j) in g.neighbours(next).into_iter().enumerate() {
                let Some(j) = j else { continue };
                if inland && self.ocean[j as usize] && !self.is_end(j) {
                    continue;
                }
                let other = self.filled[j as usize];
                if other < level {
                    let slope = (f64::from(level) - f64::from(other)) / g.distance_m(next, j);
                    if best.is_none_or(|(_, s)| slope > s) {
                        best = Some((k as u8, slope));
                    }
                }
            }
            self.receiver[i] = best.map_or(self.parent[i], |(k, _)| k);
        }
        self.phase = Phase::Receivers { next: next + 1 };
        1
    }

    fn depressed(&self, i: u32) -> bool {
        let i = i as usize;
        f64::from(self.filled[i]) - f64::from(self.elevation[i]) > DEPRESSION_EPS_M
    }

    fn label_basin(&mut self, g: &impl HydroGraph, next: u32) -> u32 {
        if let Some(v) = self.queue.pop_front() {
            let b = self.basins.len() as u32;
            for u in g.neighbours(v).into_iter().flatten() {
                if self.basin_of[u as usize] == NO_BASIN && (self.depressed(u) || self.is_sink(u)) {
                    self.basin_of[u as usize] = b;
                    let open = self.open.as_mut().expect("a basin is open");
                    debug_assert_eq!(self.filled[u as usize], open.spill_m);
                    open.nodes.push(u);
                    self.queue.push_back(u);
                }
            }
            if self.queue.is_empty() {
                let mut basin = self.open.take().expect("a basin is open");
                basin.nodes.sort_unstable();
                self.basins.push(basin);
            }
            return 1;
        }
        if next as usize == self.order.len() {
            self.phase = Phase::Redirect { basin: 0, k: 0 };
            return 1;
        }
        let v = self.order[next as usize];
        if self.basin_of[v as usize] == NO_BASIN && (self.depressed(v) || self.is_sink(v)) {
            // Scanning in pop order, the first node met of a basin is the one popped first.
            self.basin_of[v as usize] = self.basins.len() as u32;
            let parent = usize::from(self.parent[v as usize]);
            // An endorheic lake's sinks spill nowhere: the rim is the exit.
            let rim = if self.is_sink(v) {
                v
            } else {
                g.neighbours(v)[parent].expect("a depressed node has a parent")
            };
            self.open = Some(Basin {
                nodes: vec![v],
                spill_m: self.filled[v as usize],
                exit: v,
                rim,
            });
            self.queue.push_back(v);
        }
        self.phase = Phase::Basins { next: next + 1 };
        1
    }

    fn redirect(&mut self, g: &impl HydroGraph, basin: u32, k: u32) -> u32 {
        let Some(b) = self.basins.get(basin as usize) else {
            self.parent = Vec::new();
            self.sink = Vec::new();
            self.queue = VecDeque::new();
            self.phase = Phase::Done;
            return 1;
        };
        let Some(&v) = b.nodes.get(k as usize) else {
            self.phase = Phase::Redirect {
                basin: basin + 1,
                k: 0,
            };
            return 1;
        };
        if v != b.exit && self.receiver[v as usize] != RECEIVER_NONE {
            let r = usize::from(self.receiver[v as usize]);
            let to = g.neighbours(v)[r].expect("a basin node has a receiver");
            if self.basin_of[to as usize] != basin {
                self.receiver[v as usize] = RECEIVER_EXIT;
            }
        }
        self.phase = Phase::Redirect { basin, k: k + 1 };
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cube::CubeFace;
    use crate::geo::RenderDir;
    use crate::hydrology::{CubeLattice, GridGraph, HydroGraph};
    use bevy_math::DVec3;
    use proptest::prelude::*;
    use serde_json::Value;

    const GOLDEN: &str = include_str!("../../tests/fixtures/hydro_golden.json");

    fn route(g: &impl HydroGraph, elevation: Vec<f32>, budget: u32) -> Routed {
        let mut routing = Routing::new(elevation);
        while !routing.step(g, budget) {}
        routing.finish()
    }

    /// Rivers and lakes Task 10, fix round 1: the largest ocean component turns ocean (and the
    /// dilation's step counts fill) over several steps, not in one unit (6.8 ms natively at
    /// n = 900), with the same result.
    /// Task 11d fix round 3: an inland node (a burnt course's interior) never takes an ocean
    /// receiver, even when its only lower neighbour is the sea and the flood reached it from
    /// the sea (`pick_receiver`'s parent fallback).
    #[test]
    fn an_inland_node_never_takes_an_ocean_receiver() {
        let g = GridGraph {
            rows: 3,
            cols: 5,
            spacing_m: 1000.0,
        };
        // Ocean in column 0; (1, 1) at 1 m between the sea and 5 m land.
        let elevation: Vec<f32> = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (1, 1) => 1.0,
                _ => 5.0,
            })
            .collect();
        let inland = g.node(1, 1);
        let run = |inland: Vec<u32>| {
            let mut routing = Routing::new(elevation.clone()).with_inland(inland, Vec::new());
            while !routing.step(&g, 3) {}
            routing.finish()
        };
        // The hazard: alone, it drains to the sea.
        let plain = run(Vec::new());
        let to = receiver_of(&g, &plain, inland).unwrap();
        assert!(plain.ocean[to as usize]);
        let r = run(vec![inland]);
        assert!(receiver_of(&g, &r, inland).is_none_or(|j| !r.ocean[j as usize]));
        // Every other node routes as before.
        for v in (0..g.len()).filter(|&v| v != inland) {
            assert_eq!(r.ocean[v as usize], plain.ocean[v as usize]);
        }
    }

    #[test]
    fn the_largest_ocean_is_marked_over_several_steps() {
        // An 80 × 80 grid, its western half sea (3,200 nodes, one component).
        let g = GridGraph {
            rows: 80,
            cols: 80,
            spacing_m: 1_000.0,
        };
        let elevation: Vec<f32> = (0..g.len())
            .map(|i| {
                let (r, c) = g.row_col(i);
                if c < 40 {
                    -10.0
                } else {
                    (c as f32 - 39.0) * 5.0 + (r % 3) as f32
                }
            })
            .collect();
        let at_once = route(&g, elevation.clone(), u32::MAX);
        let mut routing = Routing::new(elevation);
        let (mut filling, mut marking) = (0, 0);
        while !routing.step(&g, 64) {
            match routing.phase {
                Phase::Steps { .. } => filling += 1,
                Phase::Ocean { .. } => marking += 1,
                _ => {}
            }
        }
        assert!(filling >= 2 && marking >= 2, "{filling} {marking}");
        assert_eq!(routing.finish(), at_once);
    }

    fn cell(g: &GridGraph, v: &Value) -> u32 {
        g.node(v[0].as_u64().unwrap() as u32, v[1].as_u64().unwrap() as u32)
    }

    fn grid_of<T>(v: &Value, f: impl Fn(&Value) -> T) -> Vec<T> {
        v.as_array()
            .unwrap()
            .iter()
            .flat_map(|row| row.as_array().unwrap().iter().map(&f))
            .collect()
    }

    #[test]
    fn golden_grid_matches_the_fixture() {
        let fx: Value = serde_json::from_str(GOLDEN).unwrap();
        let g = GridGraph {
            rows: fx["rows"].as_u64().unwrap() as u32,
            cols: fx["cols"].as_u64().unwrap() as u32,
            spacing_m: fx["spacing_m"].as_f64().unwrap(),
        };
        let elevation = grid_of(&fx["elevation_m"], |v| v.as_f64().unwrap() as f32);
        let r = route(&g, elevation, u32::MAX);

        assert_eq!(r.ocean, grid_of(&fx["ocean"], |v| v.as_bool().unwrap()));
        assert_eq!(
            r.filled_m,
            grid_of(&fx["filled_m"], |v| v.as_f64().unwrap() as f32)
        );
        let order: Vec<u32> = fx["order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| cell(&g, v))
            .collect();
        assert_eq!(r.order, order);
        let receivers = grid_of(&fx["receiver"], |v| (!v.is_null()).then(|| cell(&g, v)));
        for i in 0..g.len() {
            assert_eq!(
                receiver_of(&g, &r, i),
                receivers[i as usize],
                "receiver of {:?}",
                g.row_col(i)
            );
        }
        let basins: Vec<Basin> = fx["basins"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| Basin {
                nodes: b["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| cell(&g, v))
                    .collect(),
                spill_m: b["spill_m"].as_f64().unwrap() as f32,
                exit: cell(&g, &b["exit"]),
                rim: cell(&g, &b["rim"]),
            })
            .collect();
        assert_eq!(r.basins, basins);
        for (b, basin) in r.basins.iter().enumerate() {
            assert!(
                basin
                    .nodes
                    .iter()
                    .all(|&v| r.basin_of[v as usize] == b as u32)
            );
        }
        assert_eq!(r.basin_of.iter().filter(|&&b| b != NO_BASIN).count(), 4);
        // (2,2)'s parent is the rim, outside the basin, so it is redirected to the exit.
        assert_eq!(r.receiver[g.node(2, 2) as usize], RECEIVER_EXIT);

        // The fixture's case A discharge is consistent with these receivers (Task 3 checks q).
        let mut q: Vec<f64> = r.ocean.iter().map(|&o| if o { 0.0 } else { 1.0 }).collect();
        for &i in r.order.iter().rev() {
            if let Some(j) = receiver_of(&g, &r, i).filter(|&j| !r.ocean[j as usize]) {
                q[j as usize] += q[i as usize];
            }
        }
        let expected = grid_of(&fx["q"]["A"]["q"], |v| v.as_f64());
        for i in 0..g.len() as usize {
            assert_eq!(
                expected[i],
                (!r.ocean[i]).then_some(q[i]),
                "q at {:?}",
                g.row_col(i as u32)
            );
        }
    }

    /// Follows receivers from `start` to the node that has none. Panics on a cycle.
    fn path(g: &impl HydroGraph, r: &Routed, start: u32) -> Vec<u32> {
        let mut path = vec![start];
        while let Some(next) = receiver_of(g, r, *path.last().unwrap()) {
            path.push(next);
            assert!(
                path.len() <= g.len() as usize,
                "receivers cycle from {start}"
            );
        }
        path
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn no_node_drains_uphill_on_the_filled_surface(
            levels in prop::collection::vec(-4i32..12, 144),
            spacing_km in prop::sample::select(vec![1.0, 10.0, 30.0]),
        ) {
            // Coarse levels make flats, ties and several below-zero components common.
            let g = GridGraph { rows: 12, cols: 12, spacing_m: spacing_km * 1000.0 };
            let elevation: Vec<f32> = levels.iter().map(|&l| l as f32 * 10.0).collect();
            let r = route(&g, elevation.clone(), u32::MAX);
            for budget in [1, 7] {
                prop_assert_eq!(&route(&g, elevation.clone(), budget), &r);
            }
            let mut rank = vec![u32::MAX; 144];
            for (p, &i) in r.order.iter().enumerate() {
                rank[i as usize] = p as u32;
            }
            let any_ocean = r.ocean.iter().any(|&o| o);
            for i in 0..g.len() {
                let iu = i as usize;
                prop_assert!(r.filled_m[iu] >= elevation[iu]);
                if r.ocean[iu] {
                    prop_assert!(elevation[iu] < 0.0);
                    prop_assert_eq!(receiver_of(&g, &r, i), None);
                    prop_assert_eq!(rank[iu], u32::MAX);
                    continue;
                }
                prop_assert!(rank[iu] != u32::MAX, "land node {} never popped", i);
                if let Some(j) = receiver_of(&g, &r, i) {
                    prop_assert!(r.filled_m[j as usize] <= r.filled_m[iu]);
                    prop_assert!(r.ocean[j as usize] || rank[j as usize] < rank[iu]);
                }
                let p = path(&g, &r, i);
                prop_assert!(p.len() < g.len() as usize);
                let end = *p.last().unwrap() as usize;
                if any_ocean {
                    prop_assert!(r.ocean[end], "{} ends on land at {}", i, end);
                } else {
                    prop_assert_eq!(end as u32, r.order[0]);
                }
            }
        }
    }

    /// Angle from +Z, for the seam fixture: an ocean cap on +Z, land rising away from it.
    fn seam_elevation(lattice: &CubeLattice) -> Vec<f32> {
        let cap = 25f64.to_radians();
        (0..lattice.len())
            .map(|i| {
                let angle = lattice.dir(i).0.angle_between(DVec3::Z);
                if angle < cap {
                    -100.0
                } else {
                    (10.0 + 1000.0 * (angle - cap)) as f32
                }
            })
            .collect()
    }

    #[test]
    fn routing_crosses_cube_face_seams() {
        // 16 is the brief's size; 15 is odd and non-dyadic.
        for n in [16, 15] {
            let lattice = CubeLattice { n };
            let r = route(&lattice, seam_elevation(&lattice), u32::MAX);
            let face = |i: u32| lattice.face_col_row(i).0;
            // The hazard: +X holds no ocean, so its centre can only drain across a seam.
            assert!(
                (0..lattice.len())
                    .filter(|&i| face(i) == CubeFace::PosX)
                    .all(|i| !r.ocean[i as usize])
            );
            let start = lattice.nearest(RenderDir(DVec3::X));
            let p = path(&lattice, &r, start);
            assert!(
                r.ocean[*p.last().unwrap() as usize],
                "n = {n}: path ends on land"
            );
            let faces: Vec<CubeFace> = p.iter().map(|&i| face(i)).collect();
            let crossing = faces.iter().position(|&f| f != CubeFace::PosX).unwrap();
            assert!(crossing > 1, "n = {n}: {faces:?}");
            assert!(
                faces[crossing..].iter().all(|&f| f == CubeFace::PosZ),
                "n = {n}: {faces:?}"
            );
            // The seam is crossed between two land nodes, not by dropping straight into the sea.
            assert!(!r.ocean[p[crossing] as usize], "n = {n}");
        }
    }

    const SEA_ROWS: u32 = 12;

    /// A 10 km grid: ocean in columns 0..=7, then `land_cols` of land, then a −50 m basin in
    /// rows 4..=7 three columns wide, then land. Land is 100 m except a 30 m channel along row 5
    /// from the ocean to the basin, so the basin's spill is 30 m.
    fn strait_fixture(land_cols: u32) -> (GridGraph, Vec<f32>, Vec<u32>) {
        let g = GridGraph {
            rows: SEA_ROWS,
            cols: 8 + land_cols + 3 + 4,
            spacing_m: 10_000.0,
        };
        let basin_cols = 8 + land_cols..8 + land_cols + 3;
        let mut elevation = vec![100.0f32; g.len() as usize];
        let mut basin = Vec::new();
        for row in 0..g.rows {
            for col in 0..g.cols {
                let i = g.node(row, col);
                elevation[i as usize] = if col < 8 {
                    -100.0
                } else if basin_cols.contains(&col) && (4..=7).contains(&row) {
                    basin.push(i);
                    -50.0
                } else if row == 5 && col < basin_cols.start {
                    30.0
                } else {
                    100.0
                };
            }
        }
        (g, elevation, basin)
    }

    fn route_with_strait(g: &GridGraph, elevation: Vec<f32>, strait_m: f64) -> Routed {
        let mut routing = Routing::with_strait_m(elevation, strait_m);
        while !routing.step(g, u32::MAX) {}
        routing.finish()
    }

    #[test]
    fn a_sea_behind_a_narrow_strait_stays_ocean() {
        let (g, elevation, basin) = strait_fixture(1);
        // The hazard: the strip is land, so the basin is not 8-connected to the ocean, and with
        // no strait reach it would be an inland basin.
        assert!((0..SEA_ROWS).all(|row| elevation[g.node(row, 8) as usize] >= 0.0));
        let none = route_with_strait(&g, elevation.clone(), 0.0);
        assert!(basin.iter().all(|&i| !none.ocean[i as usize]));
        let r = route(&g, elevation, u32::MAX);
        assert!(basin.iter().all(|&i| r.ocean[i as usize]));
        assert!(
            (0..SEA_ROWS).all(|row| !r.ocean[g.node(row, 8) as usize]),
            "dilated land stays land"
        );
        assert!(r.basins.is_empty());
        // The reach is ceil(25 km / 10 km) = 3 steps, so a basin 3 steps out is still joined.
        let (g, elevation, basin) = strait_fixture(2);
        let r = route(&g, elevation, u32::MAX);
        assert!(basin.iter().all(|&i| r.ocean[i as usize]));
    }

    #[test]
    fn a_basin_beyond_the_strait_distance_is_not_ocean() {
        let (g, elevation, basin) = strait_fixture(4);
        let r = route(&g, elevation.clone(), u32::MAX);
        assert!(basin.iter().all(|&i| !r.ocean[i as usize]));
        assert!(basin.iter().all(|&i| r.filled_m[i as usize] == 30.0));
        assert_eq!(r.basins.len(), 1);
        let b = &r.basins[0];
        assert_eq!(b.nodes, basin);
        assert_eq!(b.spill_m, 30.0);
        assert_eq!(b.exit, g.node(5, 12));
        assert_eq!(b.rim, g.node(5, 11));
        // Every basin node drains to the exit, and the exit over the rim.
        for &i in &basin {
            let p = path(&g, &r, i);
            assert!(i == b.exit || p.contains(&b.exit));
            assert!(r.ocean[*p.last().unwrap() as usize]);
        }
        // Only the distance decides: a 60 km reach (6 steps) would join the basin to the ocean.
        let wide = route_with_strait(&g, elevation, 60_000.0);
        assert!(basin.iter().all(|&i| wide.ocean[i as usize]));
    }

    #[test]
    fn flats_drain_in_flood_order() {
        // Ocean round the border; 50 m walls; a 10 m plateau in rows 2..=4, columns 2..=8; two
        // 5 m outlets at (3, 1) and (3, 9). The west outlet is reached first (lower seq), so the
        // plateau splits by flood distance, with the middle column going west.
        let g = GridGraph {
            rows: 7,
            cols: 11,
            spacing_m: 1_000.0,
        };
        let (west, east) = (g.node(3, 1), g.node(3, 9));
        let elevation: Vec<f32> = (0..g.len())
            .map(|i| match g.row_col(i) {
                (0 | 6, _) | (_, 0 | 10) => -10.0,
                (3, 1 | 9) => 5.0,
                (2..=4, 2..=8) => 10.0,
                _ => 50.0,
            })
            .collect();
        let r = route(&g, elevation.clone(), u32::MAX);
        for budget in [1, 3, 1000] {
            assert_eq!(route(&g, elevation.clone(), budget), r);
        }
        assert!(r.basins.is_empty());
        let mut flat = 0;
        for row in 2..=4 {
            for col in 2..=8 {
                let i = g.node(row, col);
                let lower = g
                    .neighbours(i)
                    .iter()
                    .flatten()
                    .any(|&j| r.filled_m[j as usize] < 10.0);
                flat += u32::from(!lower);
                let p = path(&g, &r, i);
                let outlet = if col <= 5 { west } else { east };
                assert!(p.contains(&outlet), "({row}, {col}) drains by {p:?}");
            }
        }
        // The hazard: most of the plateau has no strictly lower neighbour.
        assert_eq!(flat, 3 * 5);
    }

    #[test]
    fn no_ocean_seeds_the_lowest_node() {
        let g = GridGraph {
            rows: 3,
            cols: 3,
            spacing_m: 1_000.0,
        };
        let elevation = vec![5.0, 4.0, 6.0, 7.0, 2.0, 2.0, 9.0, 8.0, 3.0];
        let r = route(&g, elevation, u32::MAX);
        assert!(r.ocean.iter().all(|&o| !o));
        assert_eq!(r.order[0], 4, "ties go to the lowest id");
        assert_eq!(r.receiver[4], RECEIVER_NONE);
        for i in 0..g.len() {
            assert_eq!(*path(&g, &r, i).last().unwrap(), 4);
        }
    }

    /// Times one full route on the recent lattice (6 × 900²) over synthetic elevation. Not a
    /// gate: run with `cargo test --release -p timefly_core -- --ignored --nocapture
    /// time_a_full_route`.
    #[test]
    #[ignore = "timing; run in release"]
    fn time_a_full_route_on_the_recent_lattice() {
        let lattice = CubeLattice {
            n: crate::hydrology::params::HYDRO_RECENT_N,
        };
        let elevation: Vec<f32> = (0..lattice.len())
            .map(|i| {
                let d = lattice.dir(i).0;
                let e = 2500.0 * (3.1 * d.x + 1.3).sin() * (2.3 * d.y - 0.4).cos()
                    + 900.0 * (7.7 * d.z + 2.1 * d.x).sin()
                    + 300.0 * (17.0 * d.y + 5.0 * d.z).sin()
                    + 120.0 * (61.0 * d.x + 7.0).sin() * (53.0 * d.z - 3.0).sin()
                    + 40.0 * (233.0 * d.y + 1.0).sin() * (197.0 * d.x).cos()
                    + 900.0;
                e as f32
            })
            .collect();
        let start = std::time::Instant::now();
        let r = route(&lattice, elevation, u32::MAX);
        let elapsed = start.elapsed();
        let ocean = r.ocean.iter().filter(|&&o| o).count();
        let in_basins: usize = r.basins.iter().map(|b| b.nodes.len()).sum();
        println!(
            "6 x 900^2 route: {:.3} s, {} nodes, {:.1}% ocean, {} basins holding {} nodes",
            elapsed.as_secs_f64(),
            lattice.len(),
            100.0 * ocean as f64 / f64::from(lattice.len()),
            r.basins.len(),
            in_basins
        );
    }
}
