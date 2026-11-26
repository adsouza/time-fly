//! The continental water pass (rivers and lakes design §4.1): a pure function of an era's
//! elevation, ice and climate on a global lattice. Routing fills depressions, picks the ocean,
//! routes every node to a receiver and lists the basins ([`Routing`]). The climate gives each
//! node its runoff and each basin its evaporation ([`climate`]); the lakes settle by water
//! balance; the rivers form a graph ([`RiverGraph`]). [`HydroPrep`] runs the whole pass for an
//! era, resumably, and returns an [`EraHydrology`].
//!
//! Every rule here is mirrored by the Python reference pass (Task 11), so the neighbour order,
//! the tie rules and the float formats are part of the contract, not implementation details.

pub mod climate;
pub mod compare;
mod graph;
pub mod lakemap;
mod lakes;
mod lattice;
pub mod levels;
pub mod params;
mod pass;
mod route;
pub mod straits;
mod today;
pub mod today_rivers;

pub use graph::{Down, NEAR_BUCKET_TEXELS, RiverGraph, RiverNode};
pub use lakes::{Lake, LakeKind};
pub use lattice::{CubeLattice, GridGraph, NEIGHBOUR_STEPS};
pub use pass::{
    HydroPrep, LatticeSizes, MappedLake, NodeBlock, NodeSample, NodeSource, SnapshotSource,
    SourceMode, hydro_lattice_for, river_tile_margin_m,
};
pub use route::{Basin, NO_BASIN, RECEIVER_EXIT, RECEIVER_NONE, Routed, Routing, receiver_of};

use crate::deep_time::YearsBp;
use crate::geo::RenderDir;
use lakes::LakeNode;

/// A graph the pass routes over. Node ids are `0..len()`.
pub trait HydroGraph: Sync {
    /// The number of nodes.
    fn len(&self) -> u32;

    /// `true` when the graph has no nodes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The neighbours of `i` in the order E, NE, N, NW, W, SW, S, SE ([`NEIGHBOUR_STEPS`]).
    /// `None` where a step leaves the graph or would repeat a neighbour (cube corners).
    /// Neighbourhood is symmetric: `j` is a neighbour of `i` exactly when `i` is one of `j`.
    fn neighbours(&self, i: u32) -> [Option<u32>; 8];

    /// The distance between nodes `i` and `j` in metres.
    fn distance_m(&self, i: u32, j: u32) -> f64;

    /// The area node `i` stands for, in square metres.
    fn area_m2(&self, i: u32) -> f64;

    /// The mean distance between neighbouring nodes in metres. Sets the ocean's strait reach.
    fn mean_spacing_m(&self) -> f64;
}

/// The lake at a direction, from the lattice's four bilinear taps.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LakeTap {
    pub lake: u16,
    pub level_m: f32,
    pub kind: LakeKind,
    /// The taps' distance from the shore in nodes (1 on a shore node, 0 off the lake),
    /// interpolated bilinearly.
    pub shore_steps: f32,
}

/// The continental pass's result for one era.
#[derive(Clone, Debug, PartialEq)]
pub struct EraHydrology {
    pub age: YearsBp,
    pub lattice: CubeLattice,
    pub lakes: Vec<Lake>,
    pub rivers: RiverGraph,
    /// Whether lakes held by the data's own surfaces were looked for (present-day sources).
    pub data_lakes: bool,
    /// Lake level table entries whose point lies in no depression basin, by name: the shell
    /// warns about each. Empty without data lakes, when the table is not used.
    pub unplaced_lake_levels: Vec<String>,
    /// One bit per node.
    ocean: Vec<u64>,
    /// Lake footprint nodes, sorted by node.
    lake_nodes: Vec<LakeNode>,
    /// The river nodes on today's burnt courses ([`today_rivers`]), sorted by node, each with
    /// its anchor: the nearest point on its course (unit, era frame), at most
    /// [`TODAY_RIVER_ANCHOR_MAX_SPACINGS`](params::TODAY_RIVER_ANCHOR_MAX_SPACINGS) mean
    /// spacings from the node. The regions relax such a node to its anchor.
    anchors: Vec<(u32, [f64; 3])>,
}

impl EraHydrology {
    pub fn is_ocean(&self, node: u32) -> bool {
        self.ocean[(node / 64) as usize] >> (node % 64) & 1 == 1
    }

    /// Whether any of the four bilinear taps at `dir` is ocean.
    pub fn ocean_tap(&self, dir: RenderDir) -> bool {
        self.lattice
            .taps(dir)
            .iter()
            .any(|&(node, _)| self.is_ocean(node))
    }

    /// The anchor of a river node on one of today's courses: where the regions relax it.
    pub fn anchor(&self, node: u32) -> Option<bevy_math::DVec3> {
        self.anchors
            .binary_search_by_key(&node, |a| a.0)
            .ok()
            .map(|k| bevy_math::DVec3::from_array(self.anchors[k].1))
    }

    fn lake_node(&self, node: u32) -> Option<&LakeNode> {
        self.lake_nodes
            .binary_search_by_key(&node, |n| n.node)
            .ok()
            .map(|k| &self.lake_nodes[k])
    }

    /// The lake whose footprint holds `node`.
    pub fn lake_of(&self, node: u32) -> Option<u16> {
        self.lake_node(node).map(|n| n.lake)
    }

    /// `node`'s distance from its lake's shore in nodes, 0 off every lake.
    pub fn shore_steps(&self, node: u32) -> u16 {
        self.lake_node(node).map_or(0, |n| n.shore_steps)
    }

    /// The highest-level lake among the four taps at `dir` (ties: the lowest lake index), with
    /// the taps' shore distance in that lake interpolated bilinearly (0 for taps off it).
    pub fn lake_tap(&self, dir: RenderDir) -> Option<LakeTap> {
        let taps = self
            .lattice
            .taps(dir)
            .map(|(node, w)| (self.lake_node(node), w));
        let mut best: Option<u16> = None;
        for (n, _) in &taps {
            let Some(n) = n else { continue };
            let better = best.is_none_or(|b| {
                let (lb, ln) = (&self.lakes[b as usize], &self.lakes[n.lake as usize]);
                ln.level_m > lb.level_m || (ln.level_m == lb.level_m && n.lake < b)
            });
            if better {
                best = Some(n.lake);
            }
        }
        let lake = best?;
        let shore_steps = taps
            .iter()
            .filter_map(|(n, w)| {
                n.filter(|n| n.lake == lake)
                    .map(|n| w * f64::from(n.shore_steps))
            })
            .sum::<f64>() as f32;
        let l = &self.lakes[lake as usize];
        Some(LakeTap {
            lake,
            level_m: l.level_m,
            kind: l.kind,
            shore_steps,
        })
    }

    /// A result holding only `rivers` and `lakes`: no ocean bits, no lake footprints. For the
    /// refinement's fixtures.
    #[cfg(test)]
    pub(crate) fn with_rivers(
        lattice: CubeLattice,
        lakes: Vec<Lake>,
        rivers: RiverGraph,
    ) -> EraHydrology {
        EraHydrology {
            age: YearsBp(0.0),
            lattice,
            lakes,
            rivers,
            data_lakes: false,
            unplaced_lake_levels: Vec::new(),
            ocean: vec![0; lattice.len().div_ceil(64) as usize],
            lake_nodes: Vec::new(),
            anchors: Vec::new(),
        }
    }

    /// This result with `anchors` (`(node, anchor)`, any order). For the refinement's fixtures.
    #[cfg(test)]
    pub(crate) fn with_anchors(mut self, anchors: &[(u32, bevy_math::DVec3)]) -> EraHydrology {
        let mut a: Vec<(u32, [f64; 3])> = anchors.iter().map(|&(v, d)| (v, d.to_array())).collect();
        a.sort_by_key(|x| x.0);
        self.anchors = a;
        self
    }

    /// This result with lake footprints `nodes` (`(node, lake)`), shore steps 1 on nodes with a
    /// neighbour off their lake and 2 inside. For the generator's fixtures.
    #[cfg(test)]
    pub(crate) fn with_lake_nodes(mut self, nodes: &[(u32, u16)]) -> EraHydrology {
        let mut nodes = nodes.to_vec();
        nodes.sort_unstable();
        nodes.dedup_by_key(|n| n.0);
        let lake_of = |v: u32| {
            nodes
                .binary_search_by_key(&v, |n| n.0)
                .ok()
                .map(|k| nodes[k].1)
        };
        self.lake_nodes = nodes
            .iter()
            .map(|&(node, lake)| {
                let inside = self
                    .lattice
                    .neighbours(node)
                    .iter()
                    .flatten()
                    .all(|&u| lake_of(u) == Some(lake));
                LakeNode {
                    node,
                    lake,
                    shore_steps: if inside { 2 } else { 1 },
                }
            })
            .collect();
        self
    }

    /// The bytes the result holds, by capacity.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.lakes.capacity() * std::mem::size_of::<Lake>()
            + self.rivers.bytes()
            + 8 * self.ocean.capacity()
            + self.lake_nodes.capacity() * std::mem::size_of::<LakeNode>()
            + self.anchors.capacity() * std::mem::size_of::<(u32, [f64; 3])>()
    }
}
