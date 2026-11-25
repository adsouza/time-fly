//! The whole continental pass for one era, resumable: sample the era's field on the lattice,
//! route, measure the distance to the coast (only for a climate that needs it), accumulate
//! discharge with the lake hook, and build the river graph.
//!
//! - **Coast distance** is a chamfer distance on the lattice: 2 per straight step, 3 per diagonal,
//!   from the ocean, so `km = cost / 2 × mean spacing`. Exact integers, so any visit order agrees.
//! - **Own runoff** per land node: [`ClimateTable::runoff_mm`] of its climate, or
//!   `ice_melt_runoff_mm` on an ice node (ice fraction ≥ [`ICE_NODE_FRACTION`]), times its area.
//!   A basin node's evaporation capacity is its climate's net evaporation times its area, 0 on ice.

use std::ops::Range;

use super::climate::{Climate, ClimateSource, ClimateTable, SECONDS_PER_YEAR};
use super::graph::{Down, RiverGraph};
use super::lakes::Accumulation;
use super::params::{
    CORRIDOR_FRACTION, HYDRO_DEEP_N, HYDRO_RECENT_N, ICE_NODE_FRACTION, INCISION_WINDOW_M,
    PRESENT_FRAME_MAX_MYR, RIVER_MIN_DISCHARGE_M3S,
};
use super::{CubeLattice, EraHydrology, HydroGraph, Routed, Routing, receiver_of};
use crate::deep_time::YearsBp;
use crate::geo::{RenderDir, geo_to_latlon, render_to_geo};
use crate::surface::{FrameSelection, Timeline};
use crate::worldgen::coarse::{CoarseField, SnapshotField, snapshot_sea_level_anomaly_m};
use crate::worldgen::tiles::TileSet;

/// What the pass reads at one lattice node.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NodeSample {
    /// Relative to the era's sea level.
    pub elevation_m: f32,
    /// Ice fraction in [0, 1].
    pub ice: f32,
}

/// The era's surface at lattice nodes.
pub trait NodeSource: Sync {
    fn node(&self, dir: RenderDir) -> NodeSample;

    /// Whether the elevation holds present-day lake surfaces as flats (ETOPO), so the pass looks
    /// for lakes the data already holds.
    fn data_lakes(&self) -> bool;
}

/// How [`SnapshotSource`] reads an era.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceMode {
    /// An ICE-6G bracket: the field's overview sample.
    Recent,
    /// Between the last ICE-6G frame and the bridge frame: the held recent sample crossfaded
    /// into the present-day overview at the era's sea level.
    Bridge,
    /// A deep bracket no older than [`PRESENT_FRAME_MAX_MYR`]: the overview at the era's sea
    /// level, without the plate model (plates move a few km in that time).
    PresentFrame,
    /// Older deep eras: the globe's reference surface ([`DeepField::reference_sample`]).
    ///
    /// [`DeepField::reference_sample`]: crate::worldgen::coarse::DeepField::reference_sample
    Reference,
}

/// The lattice sizes the pass picks between.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct LatticeSizes {
    pub recent_n: u32,
    pub deep_n: u32,
}

impl Default for LatticeSizes {
    fn default() -> Self {
        Self {
            recent_n: HYDRO_RECENT_N,
            deep_n: HYDRO_DEEP_N,
        }
    }
}

/// The lattice and source mode for an era: the recent lattice wherever present-day data drives
/// the terrain, the deep one otherwise (design §4.1, "Resolution follows the source").
pub fn hydro_lattice_for(
    timeline: &Timeline,
    age_myr: f64,
    sizes: LatticeSizes,
) -> (CubeLattice, SourceMode) {
    let (n, mode) = match timeline.select_frames(age_myr) {
        FrameSelection::Recent(_) => (sizes.recent_n, SourceMode::Recent),
        FrameSelection::Bridge { .. } => (sizes.recent_n, SourceMode::Bridge),
        FrameSelection::Deep(_) if age_myr <= PRESENT_FRAME_MAX_MYR => {
            (sizes.recent_n, SourceMode::PresentFrame)
        }
        FrameSelection::Deep(_) => (sizes.deep_n, SourceMode::Reference),
    };
    (CubeLattice { n }, mode)
}

/// The pass's view of a snapshot field. Where the overview is missing, or the field is not the
/// mode's kind, a node reads the field's own overview sample.
#[derive(Copy, Clone, Debug)]
pub struct SnapshotSource<'a> {
    field: &'a SnapshotField<'a>,
    tiles: &'a TileSet,
    mode: SourceMode,
    /// The sea-level anomaly the era's heights carry ([`snapshot_sea_level_anomaly_m`]).
    anomaly_m: f64,
}

impl<'a> SnapshotSource<'a> {
    pub fn new(
        field: &'a SnapshotField<'a>,
        tiles: &'a TileSet,
        timeline: &Timeline,
        age_myr: f64,
        mode: SourceMode,
    ) -> Self {
        Self {
            field,
            tiles,
            mode,
            anomaly_m: snapshot_sea_level_anomaly_m(timeline, age_myr),
        }
    }

    /// Present-day elevation at the era's sea level, from the overview.
    fn present_m(&self, dir: RenderDir) -> Option<f64> {
        let (lat, lon) = geo_to_latlon(render_to_geo(dir));
        let (h, _) = self.tiles.sample_overview(lat, lon)?;
        Some(f64::from(h) - self.anomaly_m)
    }
}

impl NodeSource for SnapshotSource<'_> {
    fn node(&self, dir: RenderDir) -> NodeSample {
        let own = || {
            let s = self.field.sample_overview(dir);
            (s.elevation_m, s.ice)
        };
        let (elevation_m, ice) = match (self.mode, self.field) {
            (SourceMode::Bridge, SnapshotField::Bridge(cf)) => {
                let recent = cf.recent.sample_overview(dir);
                match self.present_m(dir) {
                    Some(present) => {
                        let w = cf.deep_weight;
                        (
                            (1.0 - w) * recent.elevation_m + w * present,
                            ((1.0 - w) * f64::from(recent.ice)) as f32,
                        )
                    }
                    None => own(),
                }
            }
            (SourceMode::PresentFrame, _) => match self.present_m(dir) {
                Some(present) => (present, 0.0),
                None => own(),
            },
            (SourceMode::Reference, SnapshotField::Deep(deep)) => {
                let s = deep.reference_sample(dir);
                (s.elevation_m, s.ice)
            }
            _ => own(),
        };
        NodeSample {
            elevation_m: elevation_m as f32,
            ice,
        }
    }

    fn data_lakes(&self) -> bool {
        self.mode != SourceMode::Reference
    }
}

/// How far beyond a region's inner field rivers must be known (m): a segment's relaxation
/// corridor and the incision window past the lattice's 1.5-spacing reach.
pub fn river_tile_margin_m(lattice: &CubeLattice) -> f64 {
    (1.5 + CORRIDOR_FRACTION) * lattice.mean_spacing_m() + INCISION_WINDOW_M
}

/// Samples of consecutive nodes from `start`, for [`HydroPrep::add_samples`].
#[derive(Clone, Debug, PartialEq)]
pub struct NodeBlock {
    pub start: u32,
    pub samples: Vec<NodeSample>,
    pub data_lakes: bool,
}

/// A chamfer distance from the ocean, resumable.
struct Coast {
    cost: Vec<u16>,
    buckets: [Vec<u32>; 4],
    seeded: u32,
    d: u32,
}

const NO_COAST: u16 = u16::MAX;

impl Coast {
    fn new(len: u32) -> Self {
        Self {
            cost: vec![NO_COAST; len as usize],
            buckets: Default::default(),
            seeded: 0,
            d: 0,
        }
    }

    fn bytes(&self) -> usize {
        2 * self.cost.capacity() + 4 * self.buckets.iter().map(Vec::capacity).sum::<usize>()
    }

    fn step(&mut self, g: &impl HydroGraph, routed: &Routed, budget: u32) -> (bool, u32) {
        let mut used = 0;
        while used < budget {
            used += 1;
            if self.seeded < g.len() {
                let i = self.seeded;
                self.seeded += 1;
                if routed.ocean[i as usize] {
                    self.cost[i as usize] = 0;
                    let coast = g
                        .neighbours(i)
                        .into_iter()
                        .flatten()
                        .any(|j| !routed.ocean[j as usize]);
                    if coast {
                        self.buckets[0].push(i);
                    }
                }
                continue;
            }
            let b = (self.d % 4) as usize;
            let Some(v) = self.buckets[b].pop() else {
                if self.buckets.iter().all(Vec::is_empty) {
                    self.buckets = Default::default();
                    return (true, used);
                }
                self.d += 1;
                continue;
            };
            if u32::from(self.cost[v as usize]) != self.d {
                continue;
            }
            for (k, j) in g.neighbours(v).into_iter().enumerate() {
                let Some(j) = j else { continue };
                if routed.ocean[j as usize] {
                    continue;
                }
                let next = self.d + if k % 2 == 0 { 2 } else { 3 };
                if next < u32::from(self.cost[j as usize]) {
                    self.cost[j as usize] = next as u16;
                    self.buckets[(next % 4) as usize].push(j);
                }
            }
        }
        (false, used)
    }
}

enum Stage {
    Sample,
    Route(Box<Routing>),
    Coast(Box<Routed>, Coast),
    Runoff(Box<Routed>, Vec<u16>, Accumulation),
    Walk(Box<Routed>, Vec<u16>, Accumulation),
    Rivers(Box<Routed>, Accumulation, u32, Vec<u32>),
    Done(Box<EraHydrology>),
    /// Only while a step moves between stages.
    Moving,
}

/// The resumable pass. Call [`HydroPrep::step`] with the same sources until it returns `true`,
/// then [`HydroPrep::finish`]. The result does not depend on how the work is sliced, nor on
/// whether the samples came from [`HydroPrep::step`] or from [`HydroPrep::add_samples`].
pub struct HydroPrep {
    lattice: CubeLattice,
    age: YearsBp,
    units: u64,
    data_lakes: bool,
    elevation: Vec<f32>,
    /// One bit per node: ice fraction ≥ [`ICE_NODE_FRACTION`].
    ice: Vec<u64>,
    stage: Stage,
}

fn bit(bits: &[u64], i: u32) -> bool {
    bits[(i / 64) as usize] >> (i % 64) & 1 == 1
}

fn is_ice_sample(s: &NodeSample) -> bool {
    f64::from(s.ice) >= ICE_NODE_FRACTION
}

fn routed_bytes(r: &Routed) -> usize {
    let basins: usize = r
        .basins
        .iter()
        .map(|b| std::mem::size_of_val(b) + 4 * b.nodes.capacity())
        .sum();
    4 * (r.elevation_m.capacity()
        + r.filled_m.capacity()
        + r.order.capacity()
        + r.basin_of.capacity())
        + r.ocean.capacity()
        + r.receiver.capacity()
        + basins
}

impl HydroPrep {
    pub fn new(lattice: CubeLattice, age: YearsBp) -> Self {
        let len = lattice.len();
        Self {
            lattice,
            age,
            units: 0,
            data_lakes: false,
            elevation: Vec::with_capacity(len as usize),
            ice: vec![0; len.div_ceil(64) as usize],
            stage: Stage::Sample,
        }
    }

    /// Does up to `max_units` units of work (a node sampled, visited or scanned; a basin costs
    /// its size) and returns `true` once the pass is complete. `source`, `climate` and `table`
    /// must be the same on every call.
    pub fn step<S: NodeSource, C: ClimateSource>(
        &mut self,
        source: &S,
        climate: &C,
        table: &ClimateTable,
        max_units: u32,
    ) -> bool {
        let len = self.lattice.len();
        let mut used: u32 = 0;
        while used < max_units {
            let left = max_units - used;
            let lattice = self.lattice;
            let stage = std::mem::replace(&mut self.stage, Stage::Moving);
            self.stage = match stage {
                Stage::Sample => {
                    if self.elevation.is_empty() {
                        self.data_lakes = source.data_lakes();
                    }
                    let start = self.elevation.len() as u32;
                    let end = start.saturating_add(left).min(len);
                    for i in start..end {
                        let s = source.node(lattice.dir(i));
                        self.push_sample(i, s);
                    }
                    used += (end - start).max(1);
                    if end == len {
                        Stage::Route(Box::new(Routing::new(std::mem::take(&mut self.elevation))))
                    } else {
                        Stage::Sample
                    }
                }
                Stage::Route(mut routing) => {
                    let before = routing.units_done();
                    let done = routing.step(&lattice, left);
                    used += ((routing.units_done() - before) as u32).max(1);
                    if done {
                        let mut routed = Box::new(routing.finish());
                        routed.filled_m = Vec::new();
                        if climate.needs_coast_km() {
                            Stage::Coast(routed, Coast::new(len))
                        } else {
                            Stage::Runoff(routed, Vec::new(), Accumulation::new(len))
                        }
                    } else {
                        Stage::Route(routing)
                    }
                }
                Stage::Coast(routed, mut coast) => {
                    let (done, u) = coast.step(&lattice, &routed, left);
                    used += u;
                    if done {
                        Stage::Runoff(routed, coast.cost, Accumulation::new(len))
                    } else {
                        Stage::Coast(routed, coast)
                    }
                }
                Stage::Runoff(routed, coast, mut acc) => {
                    let ice = &self.ice;
                    let own = |i: u32, area_m2: f64| {
                        let mm = if bit(ice, i) {
                            f64::from(table.ice_melt_runoff_mm)
                        } else {
                            let e = routed.elevation_m[i as usize];
                            let c = node_climate(&lattice, climate, &coast, i, e);
                            f64::from(table.runoff_mm(c))
                        };
                        (mm * 1e-3 * area_m2 / SECONDS_PER_YEAR) as f32
                    };
                    let (done, u) = acc.init_step(&lattice, &routed, own, left);
                    used += u;
                    if done {
                        Stage::Walk(routed, coast, acc)
                    } else {
                        Stage::Runoff(routed, coast, acc)
                    }
                }
                Stage::Walk(mut routed, coast, mut acc) => {
                    let ice = &self.ice;
                    let is_ice = |i: u32| bit(ice, i);
                    let capacity = |i: u32, e: f32, area_m2: f64| {
                        let c = node_climate(&lattice, climate, &coast, i, e);
                        f64::from(table.net_evaporation_mm(c)) * 1e-3 * area_m2 / SECONDS_PER_YEAR
                    };
                    let (done, u) = acc.walk_step(
                        &lattice,
                        &mut routed,
                        &is_ice,
                        capacity,
                        self.data_lakes,
                        left,
                    );
                    used += u;
                    if done {
                        Stage::Rivers(routed, acc, 0, Vec::new())
                    } else {
                        Stage::Walk(routed, coast, acc)
                    }
                }
                Stage::Rivers(routed, acc, next, mut list) => {
                    let end = next.saturating_add(left).min(len);
                    for i in next..end {
                        let iu = i as usize;
                        if !routed.ocean[iu]
                            && !bit(&self.ice, i)
                            && f64::from(acc.q[iu]) >= RIVER_MIN_DISCHARGE_M3S
                            && lake_at(&acc, i).is_none()
                        {
                            list.push(i);
                        }
                    }
                    used += (end - next).max(1);
                    if end == len {
                        used = used.saturating_add(list.len() as u32);
                        Stage::Done(Box::new(self.build(&routed, acc, list)))
                    } else {
                        Stage::Rivers(routed, acc, end, list)
                    }
                }
                done @ Stage::Done(_) => {
                    self.stage = done;
                    return true;
                }
                Stage::Moving => unreachable!("a step left the pass between stages"),
            };
            if matches!(self.stage, Stage::Done(_)) {
                break;
            }
        }
        self.units += u64::from(used);
        matches!(self.stage, Stage::Done(_))
    }

    fn push_sample(&mut self, i: u32, s: NodeSample) {
        self.elevation.push(s.elevation_m);
        if is_ice_sample(&s) {
            self.ice[(i / 64) as usize] |= 1 << (i % 64);
        }
    }

    fn build(&self, routed: &Routed, acc: Accumulation, list: Vec<u32>) -> EraHydrology {
        let lattice = self.lattice;
        let down_of = |v: u32| {
            let Some(j) = receiver_of(&lattice, routed, v) else {
                // Only a graph with no ocean at all has a land node with no receiver: its
                // lowest node, where everything drains.
                return Down::Ocean(v);
            };
            if routed.ocean[j as usize] {
                Down::Ocean(j)
            } else if let Some(l) = lake_at(&acc, j) {
                Down::Lake(l, j)
            } else if bit(&self.ice, j) {
                Down::Ice(j)
            } else {
                Down::River(j)
            }
        };
        let info = |v: u32| {
            let i = v as usize;
            (acc.q[i], acc.area_km2[i], routed.elevation_m[i])
        };
        let rivers = RiverGraph::build(&lattice, list, down_of, info, &acc.lakes);
        let mut ocean = vec![0u64; lattice.len().div_ceil(64) as usize];
        for (i, _) in routed.ocean.iter().enumerate().filter(|(_, o)| **o) {
            ocean[i / 64] |= 1 << (i % 64);
        }
        EraHydrology {
            age: self.age,
            lattice,
            lakes: acc.lakes,
            rivers,
            data_lakes: self.data_lakes,
            ocean,
            lake_nodes: acc.lake_nodes,
        }
    }

    /// Samples nodes `range` of `lattice` (pure, so blocks can be sampled in parallel).
    pub fn sample_range<S: NodeSource>(
        lattice: &CubeLattice,
        source: &S,
        range: Range<u32>,
    ) -> NodeBlock {
        NodeBlock {
            start: range.start,
            samples: range.clone().map(|i| source.node(lattice.dir(i))).collect(),
            data_lakes: source.data_lakes(),
        }
    }

    /// Adds samples from [`HydroPrep::sample_range`]. Blocks must come in order, starting where
    /// sampling stands, before the first [`HydroPrep::step`] past sampling.
    pub fn add_samples(&mut self, block: NodeBlock) {
        assert!(matches!(self.stage, Stage::Sample), "sampling is over");
        assert_eq!(
            block.start as usize,
            self.elevation.len(),
            "blocks must come in order"
        );
        let len = self.lattice.len();
        assert!(
            block.start as usize + block.samples.len() <= len as usize,
            "a block past the last node"
        );
        if self.elevation.is_empty() {
            self.data_lakes = block.data_lakes;
        }
        self.units += block.samples.len() as u64;
        for (k, s) in block.samples.iter().enumerate() {
            self.push_sample(block.start + k as u32, *s);
        }
        if self.elevation.len() == len as usize {
            self.stage = Stage::Route(Box::new(Routing::new(std::mem::take(&mut self.elevation))));
        }
    }

    /// The units of work done so far.
    pub fn units_done(&self) -> u64 {
        self.units
    }

    /// The bytes the pass holds now, by capacity.
    pub fn working_bytes(&self) -> usize {
        let own = 4 * self.elevation.capacity() + 8 * self.ice.capacity();
        own + match &self.stage {
            Stage::Sample | Stage::Moving => 0,
            Stage::Route(r) => r.working_bytes(),
            Stage::Coast(r, c) => routed_bytes(r) + c.bytes(),
            Stage::Runoff(r, c, a) | Stage::Walk(r, c, a) => {
                routed_bytes(r) + 2 * c.capacity() + a.working_bytes()
            }
            Stage::Rivers(r, a, _, list) => {
                routed_bytes(r) + a.working_bytes() + 4 * list.capacity()
            }
            Stage::Done(h) => h.bytes(),
        }
    }

    /// The era's hydrology. Debug builds check that [`HydroPrep::step`] returned `true`.
    pub fn finish(self) -> EraHydrology {
        match self.stage {
            Stage::Done(h) => *h,
            _ => {
                debug_assert!(false, "the pass is not finished");
                panic!("HydroPrep::finish before step returned true")
            }
        }
    }
}

/// The climate at node `i`, with its coast distance when the climate asked for one.
fn node_climate(
    lattice: &CubeLattice,
    climate: &impl ClimateSource,
    coast: &[u16],
    i: u32,
    elevation_m: f32,
) -> Climate {
    let coast_km = match coast.get(i as usize) {
        None => 0.0,
        Some(&NO_COAST) => f32::INFINITY,
        Some(&c) => (f64::from(c) * 0.5 * lattice.mean_spacing_m() * 1e-3) as f32,
    };
    climate.climate(lattice.dir(i), elevation_m, coast_km)
}

/// The lake whose footprint holds `node`.
fn lake_at(acc: &Accumulation, node: u32) -> Option<u16> {
    acc.lake_nodes
        .binary_search_by_key(&node, |n| n.node)
        .ok()
        .map(|k| acc.lake_nodes[k].lake)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cube::CubeFace;
    use crate::hydrology::climate::SECONDS_PER_YEAR;
    use crate::hydrology::climate::tests::shipped_table;
    use crate::hydrology::climate::{Climate, ZonalClimate};
    use crate::hydrology::params::{
        DATA_LAKE_FLAT_TOL_M, DATA_LAKE_MIN_BALANCE, DATA_LAKE_MIN_NODES, HYDRO_RECENT_N,
        ICE_NODE_FRACTION, LARGE_LAKE_KM2, RIVER_MIN_DISCHARGE_M3S,
    };
    use crate::hydrology::{Down, HydroGraph, LakeKind, RiverGraph};
    use crate::worldgen::biome::BiomeClass;
    use crate::worldgen::coarse::{CoarseField, snapshot_field, snapshot_sea_level_anomaly_m};
    use crate::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};
    use crate::worldgen::testkit::{dir, frames, ice_frame, latlon, rift, timeline};
    use crate::worldgen::tiles::tests::overview_from;
    use bevy_math::DVec3;
    use std::sync::OnceLock;

    // ---- A synthetic world: eight islands, each one hazard, on an ocean planet. ----

    #[derive(Copy, Clone, Debug)]
    enum Shape {
        /// A bowl: the floor out to `floor_r`, walls up to the spill at `wall_r`, a ring at the
        /// spill out to `ring_r`, then down to -20 m at the coast, `coast_r` (km).
        Bowl {
            floor_m: f64,
            floor_r: f64,
            wall_r: f64,
            spill_m: f64,
            ring_r: f64,
            coast_r: f64,
            /// m/km eastward inside the walls, fading to 0 at the wall: breaks symmetric ties.
            tilt: f64,
            /// The depth of six radial valleys inside the walls.
            valleys: f64,
        },
        /// A cone from `peak_m` down to -20 m at `coast_r`, with an optional crater
        /// `(km north of the centre, radius km, depth m)`. A terraced dome is flat from 100 to
        /// 200 km out, with a dip 0.3 m deep 110 to 150 km out on the terrace's north side: a
        /// basin too shallow for a lake, which rivers pass through and climb out of onto the
        /// terrace.
        Dome {
            peak_m: f64,
            coast_r: f64,
            crater: Option<(f64, f64, f64)>,
            terrace: bool,
        },
    }

    const ARID: usize = 0;
    const HUMID: usize = 1;
    const CASPIAN: usize = 2;
    const SALT: usize = 3;
    const TERMINAL: usize = 4;
    const ICE: usize = 5;
    const DESERT: usize = 6;
    const WET: usize = 7;

    #[derive(Clone, Debug)]
    struct Island {
        centre: DVec3,
        shape: Shape,
        climate: Climate,
        /// Ice within this radius (km).
        ice_r: f64,
    }

    impl Island {
        fn coast_r(&self) -> f64 {
            match self.shape {
                Shape::Bowl { coast_r, .. } | Shape::Dome { coast_r, .. } => coast_r,
            }
        }
    }

    /// Where a direction lies relative to an island: `(r km, east km, north km)`.
    fn local(centre: DVec3, d: DVec3) -> (f64, f64, f64) {
        let r = 2.0 * (0.5 * (d - centre).length()).asin() * EARTH_RADIUS_M / 1000.0;
        let (north, east) = RegionFrame {
            center: RenderDir(centre),
        }
        .north_east();
        let k = EARTH_RADIUS_M / 1000.0;
        (r, d.dot(east) * k, d.dot(north) * k)
    }

    #[derive(Clone, Debug)]
    struct World {
        islands: Vec<Island>,
        /// The climate, by island, scaled with the distance to the coast when `coast_scale` is
        /// set (so the coast phase matters).
        coast_scale: Option<f32>,
        data_lakes: bool,
    }

    impl World {
        fn new() -> World {
            let t = shipped_table();
            let b = |biome| t.biome(biome);
            let bowl =
                |floor_m, floor_r, wall_r, spill_m, ring_r, coast_r, tilt, valleys| Shape::Bowl {
                    floor_m,
                    floor_r,
                    wall_r,
                    spill_m,
                    ring_r,
                    coast_r,
                    tilt,
                    valleys,
                };
            let cone = bowl(100.0, 0.0, 200.0, 500.0, 260.0, 450.0, 0.05, 0.0);
            let flat = bowl(-28.0, 150.0, 400.0, 30.0, 450.0, 650.0, 0.0, 0.0);
            let dome = Shape::Dome {
                peak_m: 1200.0,
                coast_r: 350.0,
                crater: None,
                terrace: true,
            };
            let island = |centre: RenderDir, shape, climate| Island {
                centre: centre.0.normalize(),
                shape,
                climate,
                ice_r: 0.0,
            };
            let mut islands = vec![
                island(dir(20.0, -100.0), cone, b(BiomeClass::HotDesert)),
                island(dir(-15.0, -40.0), cone, b(BiomeClass::TropicalRainforest)),
                island(dir(40.0, 20.0), flat, b(BiomeClass::TemperateBroadleaf)),
                island(dir(-40.0, 60.0), flat, b(BiomeClass::HotDesert)),
                // Across the cube corner where +X, +Y and +Z meet. The floor is off the corner
                // and tilted, so no two lowest nodes tie within the data tolerance.
                island(
                    RenderDir(DVec3::new(1.0, 1.03, 0.98)),
                    bowl(0.0, 0.0, 650.0, 800.0, 700.0, 900.0, 0.2, 40.0),
                    b(BiomeClass::Steppe),
                ),
                island(
                    dir(68.0, -30.0),
                    Shape::Dome {
                        peak_m: 2500.0,
                        coast_r: 800.0,
                        crater: Some((120.0, 90.0, 400.0)),
                        terrace: false,
                    },
                    b(BiomeClass::Tundra),
                ),
                island(
                    dir(-10.0, 150.0),
                    dome,
                    Climate {
                        precip_mm: 0.0,
                        temp_c: 25.0,
                    },
                ),
                island(dir(25.0, 150.0), dome, b(BiomeClass::TemperateBroadleaf)),
            ];
            islands[ICE].ice_r = 350.0;
            World {
                islands,
                coast_scale: None,
                data_lakes: true,
            }
        }

        /// The nearest island and where `d` lies from its centre.
        fn nearest(&self, d: DVec3) -> (usize, (f64, f64, f64)) {
            let d = d.normalize();
            let k = (0..self.islands.len())
                .min_by(|&a, &b| {
                    let (da, db) = (
                        (d - self.islands[a].centre).length(),
                        (d - self.islands[b].centre).length(),
                    );
                    da.total_cmp(&db)
                })
                .unwrap();
            (k, local(self.islands[k].centre, d))
        }

        /// The island a direction is on, with its radius from the centre (km).
        fn island_at(&self, d: DVec3) -> Option<(usize, f64)> {
            let (k, (r, _, _)) = self.nearest(d);
            (r < self.islands[k].coast_r()).then_some((k, r))
        }

        fn elevation_ice(&self, d: DVec3) -> (f64, f32) {
            let (k, (r, x, y)) = self.nearest(d);
            let island = &self.islands[k];
            let ice = if r < island.ice_r { 1.0 } else { 0.0 };
            let e = match island.shape {
                Shape::Bowl {
                    floor_m,
                    floor_r,
                    wall_r,
                    spill_m,
                    ring_r,
                    coast_r,
                    tilt,
                    valleys,
                } => {
                    if r > coast_r {
                        return (-2000.0, 0.0);
                    }
                    if r <= floor_r {
                        floor_m
                    } else if r <= wall_r {
                        let wall =
                            floor_m + (spill_m - floor_m) * (r - floor_r) / (wall_r - floor_r);
                        let bearing = x.atan2(y);
                        let valley = (6.0 * bearing).cos().max(0.0).powi(4)
                            * (std::f64::consts::PI * r / wall_r).sin();
                        wall + tilt * x * (1.0 - r / wall_r) - valleys * valley
                    } else if r <= ring_r {
                        spill_m
                    } else {
                        spill_m + (-20.0 - spill_m) * (r - ring_r) / (coast_r - ring_r)
                    }
                }
                Shape::Dome {
                    peak_m,
                    coast_r,
                    crater,
                    terrace,
                } => {
                    if r > coast_r {
                        return (-2000.0, 0.0);
                    }
                    let cone = |r: f64| peak_m + (-20.0 - peak_m) * r / coast_r;
                    let mut e = cone(r);
                    if terrace && (100.0..=200.0).contains(&r) {
                        e = cone(200.0);
                        let bearing = x.atan2(y).to_degrees();
                        if (110.0..=150.0).contains(&r) && bearing.abs() < 25.0 {
                            e -= 0.3;
                        }
                    }
                    if let Some((north, radius, depth)) = crater {
                        let dc = x.hypot(y - north);
                        if dc < radius {
                            e -= depth * (1.0 - dc / radius);
                        }
                    }
                    e
                }
            };
            (e, ice)
        }
    }

    impl NodeSource for World {
        fn node(&self, dir: RenderDir) -> NodeSample {
            let (e, ice) = self.elevation_ice(dir.0);
            NodeSample {
                elevation_m: e as f32,
                ice,
            }
        }

        fn data_lakes(&self) -> bool {
            self.data_lakes
        }
    }

    impl ClimateSource for World {
        fn climate(&self, dir: RenderDir, _elevation_m: f32, coast_km: f32) -> Climate {
            let mut c = self.islands[self.nearest(dir.0).0].climate;
            if let Some(scale) = self.coast_scale {
                c.precip_mm *= 1.0 + coast_km / scale;
            }
            c
        }

        fn needs_coast_km(&self) -> bool {
            self.coast_scale.is_some()
        }
    }

    /// Runs the pass to the end in steps of `budget` units.
    fn run<S: NodeSource, C: ClimateSource>(
        lattice: CubeLattice,
        source: &S,
        climate: &C,
        table: &ClimateTable,
        budget: u32,
    ) -> EraHydrology {
        let mut prep = HydroPrep::new(lattice, YearsBp(0.0));
        while !prep.step(source, climate, table, budget) {}
        prep.finish()
    }

    const WORLD_N: u32 = 300;

    struct Shared {
        world: World,
        table: ClimateTable,
        h: EraHydrology,
    }

    fn shared() -> &'static Shared {
        static SHARED: OnceLock<Shared> = OnceLock::new();
        SHARED.get_or_init(|| Shared::new(true))
    }

    /// The same world without data lakes: shallow flat basins pass their water through.
    fn shared_without_data() -> &'static Shared {
        static SHARED: OnceLock<Shared> = OnceLock::new();
        SHARED.get_or_init(|| Shared::new(false))
    }

    impl Shared {
        fn new(data_lakes: bool) -> Shared {
            let mut world = World::new();
            world.data_lakes = data_lakes;
            let table = shipped_table();
            let h = run(CubeLattice { n: WORLD_N }, &world, &world, &table, u32::MAX);
            Shared { world, table, h }
        }

        fn lattice(&self) -> CubeLattice {
            self.h.lattice
        }

        fn island_of(&self, node: u32) -> Option<(usize, f64)> {
            self.world.island_at(self.lattice().dir(node).0)
        }

        /// Lakes whose exit lies on island `k`.
        fn lakes_on(&self, k: usize) -> Vec<usize> {
            (0..self.h.lakes.len())
                .filter(|&l| self.island_of(self.h.lakes[l].exit).map(|(i, _)| i) == Some(k))
                .collect()
        }

        /// The total area (km²) of island `k`'s nodes within `r_km` of its centre.
        fn area_within(&self, k: usize, r_km: f64) -> f64 {
            let lattice = self.lattice();
            (0..lattice.len())
                .filter(|&i| {
                    self.island_of(i)
                        .is_some_and(|(island, r)| island == k && r < r_km)
                })
                .map(|i| lattice.area_m2(i) * 1e-6)
                .sum()
        }

        fn elevation(&self, node: u32) -> f32 {
            self.world.node(self.lattice().dir(node)).elevation_m
        }

        fn is_ice(&self, node: u32) -> bool {
            f64::from(self.world.node(self.lattice().dir(node)).ice) >= ICE_NODE_FRACTION
        }
    }

    /// m³/s from mm/yr over km².
    fn m3s(mm: f64, km2: f64) -> f64 {
        mm * 1e-3 * km2 * 1e6 / SECONDS_PER_YEAR
    }

    /// Follows river links from river node `k` to the first non-river link.
    fn chain_end(rivers: &RiverGraph, k: u32) -> Down {
        let mut at = k;
        for _ in 0..=rivers.nodes().len() {
            match rivers.nodes()[at as usize].down {
                Down::River(next) => at = next,
                end => return end,
            }
        }
        panic!("river links from {k} cycle")
    }

    #[test]
    fn an_arid_basin_gets_no_large_lake() {
        let s = shared();
        let c = s.world.islands[ARID].climate;
        let bowl = s.area_within(ARID, 200.0);
        let catchment = s.area_within(ARID, 260.0);
        // The hazard: a 400 km closed bowl, which filled to its spill would be a large lake,
        // and whose evaporation capacity far exceeds its inflow.
        assert!(bowl > 50.0 * LARGE_LAKE_KM2, "{bowl} km2");
        let inflow = m3s(f64::from(s.table.runoff_mm(c)), catchment);
        let capacity = m3s(f64::from(s.table.net_evaporation_mm(c)), bowl);
        assert!(
            inflow > 0.0 && capacity > 100.0 * inflow,
            "{inflow} vs {capacity}"
        );
        for l in s.lakes_on(ARID) {
            let lake = &s.h.lakes[l];
            assert!(lake.area_km2 < LARGE_LAKE_KM2, "{lake:?}");
            assert_ne!(lake.kind, LakeKind::Open, "{lake:?}");
        }
    }

    #[test]
    fn a_humid_basin_spills_to_its_outlet() {
        let s = shared();
        let lakes = s.lakes_on(HUMID);
        assert_eq!(lakes.len(), 1, "{:?}", lakes);
        let lake = &s.h.lakes[lakes[0]];
        assert_eq!(lake.kind, LakeKind::Open, "{lake:?}");
        assert_eq!(lake.level_m, 500.0);
        assert_eq!(lake.spill_m, 500.0);
        assert!(
            lake.outflow_m3s > 0.0 && lake.evaporation_m3s >= 0.0,
            "{lake:?}"
        );
        let bowl = s.area_within(HUMID, 200.0);
        assert!(
            lake.area_km2 > 0.8 * bowl,
            "{} of {bowl} km2",
            lake.area_km2
        );
        // The outflow carries on downstream as a river from the rim.
        let lattice = s.lattice();
        assert!(s.h.rivers.nodes().iter().any(|n| {
            s.island_of(n.node)
                .is_some_and(|(k, r)| k == HUMID && r > 200.0)
                && f64::from(n.discharge_m3s) >= lake.outflow_m3s * 0.99
        }));
        let _ = lattice;
    }

    /// Island `k`'s flat floor nodes (within `floor_r` of the centre).
    fn floor_nodes(s: &Shared, k: usize, floor_r: f64) -> Vec<u32> {
        let lattice = s.lattice();
        (0..lattice.len())
            .filter(|&i| s.island_of(i).is_some_and(|(j, r)| j == k && r <= floor_r))
            .collect()
    }

    #[test]
    fn a_data_lake_keeps_its_flat_level() {
        let s = shared();
        let floor = floor_nodes(s, CASPIAN, 150.0);
        // The hazard: the data's flat is truly flat, below sea level, and big enough.
        assert!(
            floor.len() >= 20 * DATA_LAKE_MIN_NODES as usize,
            "{}",
            floor.len()
        );
        assert!(floor.iter().all(|&i| s.elevation(i) == -28.0));
        assert!(floor.iter().all(|&i| !s.h.is_ocean(i)));
        let lakes = s.lakes_on(CASPIAN);
        assert_eq!(lakes.len(), 1, "{lakes:?}");
        let lake = &s.h.lakes[lakes[0]];
        assert_eq!(lake.kind, LakeKind::Data, "{lake:?}");
        assert!((lake.level_m + 28.0).abs() <= 0.01, "{lake:?}");
        assert_eq!(lake.spill_m, 30.0);
        assert_eq!(lake.outflow_m3s, 0.0);
        // By water balance alone it would fill to the spill: the inflow exceeds what the whole
        // basin could evaporate.
        let c = s.world.islands[CASPIAN].climate;
        let basin = s.area_within(CASPIAN, 400.0);
        let capacity = m3s(f64::from(s.table.net_evaporation_mm(c)), basin);
        assert!(
            lake.inflow_m3s > 2.0 * capacity,
            "{} vs {capacity}",
            lake.inflow_m3s
        );
        // The footprint is the flat (plus wall nodes within the tolerance).
        assert!(
            floor
                .iter()
                .all(|&i| s.h.lake_of(i) == Some(lakes[0] as u16))
        );
        assert!(
            lake.area_km2 < 1.3 * s.area_within(CASPIAN, 150.0),
            "{lake:?}"
        );
    }

    #[test]
    fn a_dry_salt_flat_is_not_a_data_lake() {
        let s = shared();
        let floor = floor_nodes(s, SALT, 150.0);
        assert!(floor.len() >= 20 * DATA_LAKE_MIN_NODES as usize);
        assert!(floor.iter().all(|&i| s.elevation(i) == -28.0));
        // The hazard: water does reach the flat, but far less than its surface evaporates.
        let c = s.world.islands[SALT].climate;
        let inflow = m3s(f64::from(s.table.runoff_mm(c)), s.area_within(SALT, 450.0));
        let flat = m3s(
            f64::from(s.table.net_evaporation_mm(c)),
            s.area_within(SALT, 150.0),
        );
        assert!(
            inflow > 0.0 && inflow < DATA_LAKE_MIN_BALANCE * flat,
            "{inflow} vs {flat}"
        );
        assert!(inflow < RIVER_MIN_DISCHARGE_M3S);
        assert_eq!(s.lakes_on(SALT), Vec::<usize>::new());
        assert!(floor.iter().all(|&i| s.h.lake_of(i).is_none()));
        let _ = DATA_LAKE_FLAT_TOL_M;
    }

    #[test]
    fn rivers_end_at_the_sea_a_lake_or_the_ice() {
        let s = shared();
        let rivers = &s.h.rivers;
        let (mut ocean, mut lake, mut ice) = (0, 0, 0);
        for (k, n) in rivers.nodes().iter().enumerate() {
            match n.down {
                Down::River(t) => assert!((t as usize) < rivers.nodes().len()),
                Down::Ocean(j) => assert!(s.h.is_ocean(j)),
                Down::Lake(l, j) => assert_eq!(s.h.lake_of(j), Some(l)),
                Down::Ice(j) => assert!(s.is_ice(j)),
            }
            match chain_end(rivers, k as u32) {
                Down::Ocean(_) => ocean += 1,
                Down::Lake(..) => lake += 1,
                Down::Ice(_) => ice += 1,
                Down::River(_) => unreachable!(),
            }
        }
        println!(
            "{} river nodes: {ocean} reach the ocean, {lake} a lake, {ice} the ice",
            rivers.nodes().len()
        );
        assert!(ocean > 100 && lake > 20, "{ocean} {lake}");
    }

    #[test]
    fn discharge_never_shrinks_between_lakes() {
        let s = shared();
        let rivers = &s.h.rivers;
        let mut links = 0;
        for (k, n) in rivers.nodes().iter().enumerate() {
            assert!(f64::from(n.discharge_m3s) >= RIVER_MIN_DISCHARGE_M3S);
            assert!(n.area_km2 > 0.0);
            if let Down::River(t) = n.down {
                let d = &rivers.nodes()[t as usize];
                assert!(d.discharge_m3s >= n.discharge_m3s, "{n:?} -> {d:?}");
                assert!(d.area_km2 >= n.area_km2, "{n:?} -> {d:?}");
                assert!(rivers.upstream(t).contains(&(k as u32)));
                links += 1;
            }
            for &u in rivers.upstream(k as u32) {
                assert_eq!(rivers.nodes()[u as usize].down, Down::River(k as u32));
            }
            assert_eq!(rivers.index_of(n.node), Some(k as u32));
        }
        assert!(links > 100, "{links}");
    }

    #[test]
    fn river_beds_never_rise_downstream() {
        beds_never_rise(shared());
        // The hazard: some reaches climb in elevation, out of the terrace's shallow dip (a
        // pass-through basin; with data lakes on, the flat dip is a data lake instead).
        let climbs = beds_never_rise(shared_without_data());
        assert!(climbs > 0, "{climbs}");
    }

    /// Checks every river node's bed; returns how many lie below their own node.
    fn beds_never_rise(s: &Shared) -> u32 {
        let rivers = &s.h.rivers;
        let mut lowered = 0;
        for (k, n) in rivers.nodes().iter().enumerate() {
            let e = s.elevation(n.node);
            assert!(n.bed_m <= e, "{n:?} above its node at {e}");
            lowered += u32::from(n.bed_m < e);
            let down = rivers.down_bed_m(k as u32);
            assert!(down <= n.bed_m, "{n:?} -> {down}");
            match n.down {
                Down::River(t) => assert_eq!(down, rivers.nodes()[t as usize].bed_m),
                Down::Ocean(_) => assert!(down <= 0.0),
                Down::Lake(l, _) => assert!(down <= s.h.lakes[l as usize].level_m),
                Down::Ice(_) => {}
            }
        }
        lowered
    }

    #[test]
    fn rivers_inside_a_terminal_basin_flow_to_its_lake() {
        let s = shared();
        let lakes = s.lakes_on(TERMINAL);
        assert_eq!(lakes.len(), 1, "{lakes:?}");
        let id = lakes[0] as u16;
        let lake = &s.h.lakes[lakes[0]];
        assert_eq!(lake.kind, LakeKind::Terminal, "{lake:?}");
        assert!(lake.level_m < lake.spill_m - 100.0, "{lake:?}");
        let inside: Vec<u32> = (0..s.h.rivers.nodes().len() as u32)
            .filter(|&k| {
                let node = s.h.rivers.nodes()[k as usize].node;
                s.island_of(node)
                    .is_some_and(|(i, r)| i == TERMINAL && r < 640.0)
            })
            .collect();
        // The hazard: the basin's dry floor holds many river nodes, which routing's tree would
        // send to the exit, not to the lake.
        assert!(inside.len() > 20, "{}", inside.len());
        let exit_r = s.island_of(lake.exit).unwrap().1;
        println!(
            "{} river nodes inside; lake {:.0} km2 at {:.1} m, exit {exit_r:.0} km out",
            inside.len(),
            lake.area_km2,
            lake.level_m
        );
        for &k in &inside {
            match chain_end(&s.h.rivers, k) {
                Down::Lake(l, _) => assert_eq!(l, id),
                other => panic!("{:?} ends at {other:?}", s.h.rivers.nodes()[k as usize]),
            }
        }
    }

    #[test]
    fn no_rivers_or_lakes_on_ice() {
        let s = shared();
        let lattice = s.lattice();
        for n in s.h.rivers.nodes() {
            assert!(!s.is_ice(n.node), "{n:?}");
        }
        for i in 0..lattice.len() {
            if s.h.lake_of(i).is_some() {
                assert!(!s.is_ice(i), "lake node {i} is ice");
            }
        }
        // The hazard: a crater under the ice (a basin of ice nodes, with melt runoff), and
        // rivers starting at the ice margin.
        let island = &s.world.islands[ICE];
        let (north, _) = RegionFrame {
            center: RenderDir(island.centre),
        }
        .north_east();
        let angle = 120.0e3 / EARTH_RADIUS_M;
        let crater = (island.centre * angle.cos() + north * angle.sin()).normalize();
        let floor = s.world.elevation_ice(crater);
        assert!(floor.1 == 1.0 && floor.0 < 2500.0 * (1.0 - 120.0 / 800.0) - 300.0);
        let spacing = lattice.mean_spacing_m() / 1000.0;
        let margin = s.h.rivers.nodes().iter().filter(|n| {
            s.island_of(n.node).is_some_and(|(k, r)| {
                k == ICE && r >= island.ice_r && r < island.ice_r + 3.0 * spacing
            })
        });
        assert!(margin.count() > 5);
    }

    #[test]
    fn desert_channels_are_not_rivers() {
        let s = shared();
        let area = s.area_within(DESERT, 350.0);
        // The hazard: a 50,000 km2 slope whose channels would carry rivers if it rained.
        assert!(area > 50_000.0, "{area}");
        assert_eq!(s.table.runoff_mm(s.world.islands[DESERT].climate), 0.0);
        let on = |k: usize| {
            s.h.rivers
                .nodes()
                .iter()
                .filter(|n| s.island_of(n.node).is_some_and(|(i, _)| i == k))
                .count()
        };
        assert_eq!(on(DESERT), 0);
        assert!(on(WET) > 10, "{}", on(WET));
    }

    #[test]
    fn lake_and_ocean_taps_read_the_four_taps() {
        let s = shared();
        let lattice = s.lattice();
        let caspian = s.lakes_on(CASPIAN)[0] as u16;
        let floor = floor_nodes(s, CASPIAN, 150.0);
        let deepest = floor
            .iter()
            .copied()
            .max_by_key(|&i| (s.h.shore_steps(i), std::cmp::Reverse(i)))
            .unwrap();
        assert!(s.h.shore_steps(deepest) >= 3);
        // At a texel centre the node's own tap carries all the weight.
        let tap = s.h.lake_tap(lattice.dir(deepest)).unwrap();
        assert_eq!(tap.lake, caspian);
        assert_eq!(tap.kind, LakeKind::Data);
        assert_eq!(tap.level_m, s.h.lakes[caspian as usize].level_m);
        assert!((tap.shore_steps - f32::from(s.h.shore_steps(deepest))).abs() < 1e-6);
        // Between a shore node and dry land the shore distance falls toward 0.
        let shore = (0..lattice.len())
            .find(|&i| s.h.lake_of(i) == Some(caspian) && s.h.shore_steps(i) == 1)
            .unwrap();
        let dry = lattice
            .neighbours(shore)
            .into_iter()
            .flatten()
            .find(|&j| s.h.lake_of(j).is_none())
            .unwrap();
        let between = RenderDir((lattice.dir(shore).0 + lattice.dir(dry).0).normalize());
        let t = s.h.lake_tap(between).unwrap();
        assert!(t.shore_steps > 0.0 && t.shore_steps < 1.0, "{t:?}");
        // Far inland on the dry dome: no lake, no ocean.
        let wet = s.world.islands[WET].centre;
        assert_eq!(s.h.lake_tap(RenderDir(wet)), None);
        assert!(!s.h.ocean_tap(RenderDir(wet)));
        assert!(s.h.ocean_tap(dir(-60.0, -150.0)));
        // A node next to the coast has an ocean tap within half a texel.
        let coast = (0..lattice.len())
            .find(|&i| {
                !s.h.is_ocean(i)
                    && lattice
                        .neighbours(i)
                        .into_iter()
                        .flatten()
                        .any(|j| s.h.is_ocean(j))
            })
            .unwrap();
        let ocean = lattice
            .neighbours(coast)
            .into_iter()
            .flatten()
            .find(|&j| s.h.is_ocean(j))
            .unwrap();
        let mid = RenderDir((lattice.dir(coast).0 * 0.6 + lattice.dir(ocean).0 * 0.4).normalize());
        assert!(s.h.ocean_tap(mid));
        assert!(s.h.bytes() > 0);
    }

    #[test]
    fn near_finds_the_rivers_within_a_radius_across_seams() {
        let s = shared();
        let lattice = s.lattice();
        let rivers = &s.h.rivers;
        let mut out = Vec::new();
        let corner = RenderDir(DVec3::ONE.normalize());
        let edge = RenderDir(DVec3::new(1.0, 1.0, 0.2).normalize());
        let mut found = 0;
        for (d, radius) in [
            (corner, 300_000.0),
            (corner, 650_000.0),
            (edge, 900_000.0),
            (RenderDir(s.world.islands[WET].centre), 250_000.0),
            (RenderDir(s.world.islands[ICE].centre), 500_000.0),
            (dir(-60.0, -150.0), 100_000.0),
        ] {
            rivers.near(&lattice, d, radius, &mut out);
            let want: Vec<u32> = (0..rivers.nodes().len() as u32)
                .filter(|&k| {
                    let v = lattice.dir(rivers.nodes()[k as usize].node).0;
                    2.0 * (0.5 * (v - d.0.normalize()).length()).asin() * EARTH_RADIUS_M <= radius
                })
                .collect();
            assert_eq!(out, want, "{d:?} {radius}");
            found += want.len();
        }
        // The hazard: the corner island's rivers lie on three faces.
        let faces: std::collections::HashSet<CubeFace> = rivers
            .nodes()
            .iter()
            .filter(|n| s.island_of(n.node).is_some_and(|(k, _)| k == TERMINAL))
            .map(|n| lattice.face_col_row(n.node).0)
            .collect();
        assert_eq!(faces.len(), 3, "{faces:?}");
        assert!(found > 50);
    }

    #[test]
    fn the_pass_is_the_same_for_any_step_size() {
        // 75 is not a power of two; the coast-scaled climate makes the coast phase run.
        let lattice = CubeLattice { n: 75 };
        let mut world = World::new();
        world.coast_scale = Some(400.0);
        let table = shipped_table();
        let whole = run(lattice, &world, &world, &table, u32::MAX);
        let one = run(lattice, &world, &world, &table, 1);
        assert_eq!(whole, one);
        let mut prep = HydroPrep::new(lattice, YearsBp(0.0));
        let len = lattice.len();
        let cuts = [0, 7, len / 3, len / 3 + 1, len - 5, len];
        let blocks: Vec<NodeBlock> = std::thread::scope(|scope| {
            let handles: Vec<_> = cuts
                .windows(2)
                .map(|w| {
                    let (lattice, world) = (&lattice, &world);
                    let range = w[0]..w[1];
                    scope.spawn(move || HydroPrep::sample_range(lattice, world, range))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for block in blocks {
            prep.add_samples(block);
        }
        while !prep.step(&world, &world, &table, 1000) {}
        assert_eq!(prep.finish(), whole);
        // The fixture is not trivial at this size.
        assert!(!whole.lakes.is_empty() && !whole.rivers.nodes().is_empty());
        let mut a = HydroPrep::new(lattice, YearsBp(0.0));
        let mut b = HydroPrep::new(lattice, YearsBp(0.0));
        while !a.step(&world, &world, &table, u32::MAX) {}
        while !b.step(&world, &world, &table, 3) {}
        assert_eq!(a.units_done(), b.units_done());
        assert!(a.units_done() >= u64::from(len));
    }

    #[test]
    fn working_bytes_stay_within_the_budget() {
        // The budget is 160 MiB at the recent lattice; per node that is 34.5 bytes.
        let per_node = 160.0 * 1024.0 * 1024.0 / f64::from(6 * HYDRO_RECENT_N * HYDRO_RECENT_N);
        let lattice = CubeLattice { n: 150 };
        let world = World::new();
        let table = shipped_table();
        let mut prep = HydroPrep::new(lattice, YearsBp(0.0));
        let mut peak = prep.working_bytes();
        while !prep.step(&world, &world, &table, 5_000) {
            peak = peak.max(prep.working_bytes());
        }
        let h = prep.finish();
        let got = peak as f64 / f64::from(lattice.len());
        println!(
            "peak {peak} B = {got:.1} B per node; result {} B",
            h.bytes()
        );
        assert!(got <= per_node, "{got} B per node");
        assert!(h.bytes() < peak);
    }

    #[test]
    fn coast_distance_is_a_two_three_chamfer() {
        // One ocean node in the corner of a 10 km grid: k diagonal steps cost 3k, straight 2k.
        let g = crate::hydrology::GridGraph {
            rows: 6,
            cols: 8,
            spacing_m: 10_000.0,
        };
        let elevation: Vec<f32> = (0..g.len())
            .map(|i| if i == 0 { -10.0 } else { 10.0 })
            .collect();
        let mut routing = Routing::new(elevation);
        while !routing.step(&g, u32::MAX) {}
        let routed = routing.finish();
        assert_eq!(routed.ocean.iter().filter(|&&o| o).count(), 1);
        for budget in [1, 7, u32::MAX] {
            let mut coast = Coast::new(g.len());
            while !coast.step(&g, &routed, budget).0 {}
            for i in 0..g.len() {
                let (r, c) = g.row_col(i);
                let want = 3 * r.min(c) + 2 * r.abs_diff(c);
                assert_eq!(u32::from(coast.cost[i as usize]), want, "({r}, {c})");
            }
        }
        // No ocean at all: every node is infinitely far from the coast.
        let mut routing = Routing::new(vec![5.0; g.len() as usize]);
        while !routing.step(&g, u32::MAX) {}
        let routed = routing.finish();
        let mut coast = Coast::new(g.len());
        while !coast.step(&g, &routed, 3).0 {}
        assert!(coast.cost.iter().all(|&c| c == NO_COAST));
        let lattice = CubeLattice { n: 7 };
        let world = World {
            coast_scale: Some(1.0),
            ..World::new()
        };
        let km = |cost: u16| node_climate(&lattice, &world, &[cost], 0, 0.0);
        let spacing_km = lattice.mean_spacing_m() / 1000.0;
        let base = world.islands[world.nearest(lattice.dir(0).0).0]
            .climate
            .precip_mm;
        // The test climate scales precipitation by 1 + coast km.
        let p = km(5).precip_mm;
        assert!(
            (f64::from(p / base) - 1.0 - 2.5 * spacing_km).abs() < 1e-2,
            "{p}"
        );
        assert_eq!(km(NO_COAST).precip_mm, f32::INFINITY);
    }

    /// Synthetic data for the timing run: Task 2's relief lowered by `.0` metres, polar ice
    /// caps, and a climate that varies with latitude and longitude.
    struct Synthetic(f64);

    impl NodeSource for Synthetic {
        fn node(&self, dir: RenderDir) -> NodeSample {
            let d = dir.0;
            let e = 2500.0 * (3.1 * d.x + 1.3).sin() * (2.3 * d.y - 0.4).cos()
                + 900.0 * (7.7 * d.z + 2.1 * d.x).sin()
                + 300.0 * (17.0 * d.y + 5.0 * d.z).sin()
                + 120.0 * (61.0 * d.x + 7.0).sin() * (53.0 * d.z - 3.0).sin()
                + 40.0 * (233.0 * d.y + 1.0).sin() * (197.0 * d.x).cos()
                + 900.0
                - self.0;
            NodeSample {
                elevation_m: e as f32,
                ice: if d.y.abs() > 0.93 { 1.0 } else { 0.0 },
            }
        }

        fn data_lakes(&self) -> bool {
            true
        }
    }

    impl ClimateSource for Synthetic {
        fn climate(&self, dir: RenderDir, _elevation_m: f32, _coast_km: f32) -> Climate {
            let d = dir.0;
            let lat = d.y.clamp(-1.0, 1.0).asin();
            Climate {
                precip_mm: (100.0
                    + 1200.0 * (1.0 + (3.0 * lat).cos()) * (0.6 + 0.4 * (4.0 * d.x).sin()))
                    as f32,
                temp_c: (28.0 - 45.0 * lat.abs() / std::f64::consts::FRAC_PI_2) as f32,
            }
        }
    }

    /// Times the full pass on the recent lattice (6 x 900²) over synthetic data, and reports the
    /// peak working memory. Not a gate: `cargo test --release -p timefly_core --lib --
    /// --ignored --nocapture time_a_full_pass`.
    #[test]
    #[ignore = "timing; run in release"]
    fn time_a_full_pass_on_the_recent_lattice() {
        let lattice = CubeLattice { n: HYDRO_RECENT_N };
        let table = shipped_table();
        let zonal = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        let mut peaks = Vec::new();
        // Task 2's relief (17% ocean), then lowered to about Earth's 71% ocean.
        for lower in [0.0, 1450.0] {
            let synthetic = Synthetic(lower);
            let t0 = std::time::Instant::now();
            let block = HydroPrep::sample_range(&lattice, &synthetic, 0..lattice.len());
            let sampling = t0.elapsed().as_secs_f64();
            let timed = |name: &str, climate: &dyn Fn(&mut HydroPrep, u32) -> bool| {
                let mut prep = HydroPrep::new(lattice, YearsBp(0.0));
                let t = std::time::Instant::now();
                prep.add_samples(block.clone());
                let mut peak = prep.working_bytes();
                while !climate(&mut prep, 100_000) {
                    peak = peak.max(prep.working_bytes());
                }
                let secs = t.elapsed().as_secs_f64();
                let units = prep.units_done();
                let h = prep.finish();
                let ocean = (0..lattice.len()).filter(|&i| h.is_ocean(i)).count();
                println!(
                    "{name}: pass {secs:.3} s after sampling ({units} units); peak working {:.1} MiB; \
                 result {:.1} MiB; {:.1}% ocean, {} lakes, {} river nodes",
                    peak as f64 / 1048576.0,
                    h.bytes() as f64 / 1048576.0,
                    100.0 * ocean as f64 / f64::from(lattice.len()),
                    h.lakes.len(),
                    h.rivers.nodes().len()
                );
                peak
            };
            println!(
                "relief lowered {lower} m; sampling 6 x 900^2 synthetic nodes: {sampling:.3} s on one thread"
            );
            peaks.push(timed("table climate", &|prep, budget| {
                prep.step(&synthetic, &synthetic, &table, budget)
            }));
            peaks.push(timed(
                "zonal climate (with coast distance)",
                &|prep, budget| prep.step(&synthetic, &zonal, &table, budget),
            ));
        }
        assert!(peaks.iter().all(|&p| p <= 160 * 1024 * 1024), "{peaks:?}");
    }

    #[test]
    fn hydro_lattice_follows_the_era() {
        let s = timeline();
        let sizes = LatticeSizes::default();
        assert_eq!(
            sizes,
            LatticeSizes {
                recent_n: 900,
                deep_n: 180
            }
        );
        let at = |ka: f64| {
            let (lattice, mode) = hydro_lattice_for(&s, ka / 1000.0, sizes);
            (mode, lattice.n)
        };
        assert_eq!(at(0.0), (SourceMode::Recent, 900));
        assert_eq!(at(21.0), (SourceMode::Recent, 900));
        assert_eq!(at(28.0), (SourceMode::Bridge, 900));
        assert_eq!(at(50.0), (SourceMode::PresentFrame, 900));
        assert_eq!(at(200.0), (SourceMode::PresentFrame, 900));
        assert_eq!(at(201.0), (SourceMode::Reference, 180));
        assert_eq!(at(1000.0), (SourceMode::Reference, 180));
        assert_eq!(at(100_000.0), (SourceMode::Reference, 180));
        let small = LatticeSizes {
            recent_n: 45,
            deep_n: 24,
        };
        assert_eq!(hydro_lattice_for(&s, 100.0, small).0, CubeLattice { n: 24 });
        assert_eq!(hydro_lattice_for(&s, 0.0, small).0, CubeLattice { n: 45 });
        // The margin: 1.9 spacings plus the incision window.
        let m = river_tile_margin_m(&CubeLattice { n: 900 });
        let spacing = CubeLattice { n: 900 }.mean_spacing_m();
        assert!((m - (1.9 * spacing + 30_000.0)).abs() < 1e-6, "{m}");
    }

    #[test]
    fn snapshot_source_matches_the_overview_and_ice() {
        let s = timeline();
        let crust = rift(0);
        let mut tiles = TileSet::new();
        tiles
            .set_overview(overview_from(2, |lat, lon| {
                (
                    (300.0 * (lat * 0.13).sin() * (lon * 0.07).cos() + 120.0) as f32,
                    26,
                )
            }))
            .unwrap();
        let mut fr = frames(|_, d| ((700.0 * d.0.x.sin() - 100.0) as f32, d.0.y > -0.8));
        fr[0] = ice_frame(100.0, 0.0);
        fr[1] = ice_frame(140.0, 0.25);
        fr[2] = ice_frame(180.0, 0.5);
        fr[3] = ice_frame(220.0, 0.75);
        let dirs: Vec<RenderDir> = (0..40)
            .map(|k| dir(-70.0 + 3.7 * f64::from(k), -170.0 + 8.9 * f64::from(k)))
            .collect();
        let overview = |d: RenderDir| {
            let (lat, lon) = latlon(d);
            f64::from(tiles.sample_overview(lat, lon).unwrap().0)
        };
        for (age, mode) in [
            (0.021, SourceMode::Recent),
            (0.028, SourceMode::Bridge),
            (0.05, SourceMode::PresentFrame),
            (1.0, SourceMode::Reference),
        ] {
            assert_eq!(hydro_lattice_for(&s, age, LatticeSizes::default()).1, mode);
            let field = snapshot_field(&s, &crust, &tiles, false, age, |i| &fr[i]).unwrap();
            let source = SnapshotSource::new(&field, &tiles, &s, age, mode);
            assert_eq!(source.data_lakes(), mode != SourceMode::Reference);
            let anomaly = snapshot_sea_level_anomaly_m(&s, age);
            let mut iced = 0;
            for &d in &dirs {
                let got = source.node(d);
                let (want_e, want_ice) = match (&field, mode) {
                    (SnapshotField::Recent(_), SourceMode::Recent) => {
                        let o = field.sample_overview(d);
                        (o.elevation_m, o.ice)
                    }
                    (SnapshotField::Bridge(cf), SourceMode::Bridge) => {
                        let r = cf.recent.sample_overview(d);
                        let w = cf.deep_weight;
                        assert!(w > 0.1 && w < 0.9, "{w}");
                        (
                            (1.0 - w) * r.elevation_m + w * (overview(d) - anomaly),
                            ((1.0 - w) * f64::from(r.ice)) as f32,
                        )
                    }
                    (SnapshotField::Deep(_), SourceMode::PresentFrame) => {
                        assert!(anomaly < -50.0, "{anomaly}");
                        (overview(d) - anomaly, 0.0)
                    }
                    (SnapshotField::Deep(f), SourceMode::Reference) => {
                        let r = f.reference_sample(d);
                        (r.elevation_m, r.ice)
                    }
                    other => panic!("{age}: {other:?}"),
                };
                assert!(
                    (f64::from(got.elevation_m) - f64::from(want_e as f32)).abs() <= 1e-9,
                    "{age} {d:?}: {got:?} vs {want_e}"
                );
                assert!(
                    (got.ice - want_ice).abs() <= 1e-6,
                    "{age}: {got:?} vs {want_ice}"
                );
                iced += u32::from(got.ice > 0.0);
            }
            if mode == SourceMode::Recent {
                assert_eq!(iced, dirs.len() as u32, "the hazard: ice is read");
            }
        }
    }

    #[test]
    fn a_deep_era_pass_runs_on_a_paleodem_field() {
        let s = timeline();
        let crust = rift(0);
        let tiles = TileSet::new();
        let fr = frames(|_, d| {
            let p = d.0 * 2.7;
            (
                (1500.0 * (p.x.sin() + 0.6 * p.y.cos() * p.z.sin()) - 300.0) as f32,
                d.0.y > -0.7,
            )
        });
        let age = 100.0;
        let sizes = LatticeSizes {
            recent_n: HYDRO_RECENT_N,
            deep_n: 24,
        };
        let (lattice, mode) = hydro_lattice_for(&s, age, sizes);
        assert_eq!((lattice.n, mode), (24, SourceMode::Reference));
        let field = snapshot_field(&s, &crust, &tiles, false, age, |i| &fr[i]).unwrap();
        assert!(matches!(field, SnapshotField::Deep(_)));
        let source = SnapshotSource::new(&field, &tiles, &s, age, mode);
        let table = shipped_table();
        let zonal = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: snapshot_sea_level_anomaly_m(&s, age),
        };
        let h = run(lattice, &source, &zonal, &table, 777);
        assert!(!h.data_lakes);
        let land = (0..lattice.len()).filter(|&i| !h.is_ocean(i)).count();
        assert!(land > 100 && land < lattice.len() as usize - 100, "{land}");
        assert!(!h.rivers.nodes().is_empty());
        for (k, n) in h.rivers.nodes().iter().enumerate() {
            assert!(n.discharge_m3s.is_finite() && n.bed_m.is_finite() && n.area_km2.is_finite());
            assert!(!matches!(chain_end(&h.rivers, k as u32), Down::River(_)));
        }
        for lake in &h.lakes {
            assert!(
                lake.level_m.is_finite() && lake.inflow_m3s.is_finite(),
                "{lake:?}"
            );
            assert!(lake.level_m <= lake.spill_m);
        }
        println!(
            "100 Ma at n = 24: {land} land nodes, {} river nodes, {} lakes",
            h.rivers.nodes().len(),
            h.lakes.len()
        );
    }
}
