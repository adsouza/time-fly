//! The whole continental pass for one era, resumable: sample the era's field on the lattice,
//! burn the straits and (where the terrain is today's) today's river courses into the routing
//! elevation, route, measure the distance to the coast (only for a climate that needs it),
//! accumulate discharge with the lake hook, and build the river graph (its beds along today's
//! courses at their levels, not the burnt surface).
//!
//! - **Coast distance** is a chamfer distance on the lattice: 2 per straight step, 3 per diagonal,
//!   from the ocean, so `km = cost / 2 × mean spacing`. Exact integers, so any visit order agrees.
//! - **Own runoff** per land node: [`ClimateTable::runoff_mm`] of its climate, or
//!   `ice_melt_runoff_mm` on an ice node (ice fraction ≥ [`ICE_NODE_FRACTION`]), times its area.
//!   A basin node's evaporation capacity is its climate's net evaporation times its area, 0 on ice.

use std::ops::Range;

use super::climate::{Climate, ClimateSource, ClimateTable, SECONDS_PER_YEAR};
use super::graph::{Down, GraphBuild, RiverGraph};
use super::lakemap::shipped_lake_map;
use super::lakes::{Accumulation, Flats, place_levels};
use super::levels::{LakeLevel, shipped_lake_levels};
use super::params::{
    HYDRO_DEEP_N, HYDRO_RECENT_N, ICE_NODE_FRACTION, PRESENT_FRAME_MAX_MYR,
    RIVER_MIN_DISCHARGE_M3S, SUBPOINT_M, TODAY_RIVER_ANCHOR_MAX_SPACINGS,
};
use super::ridged::shipped_ridged_steps;
use super::straits::{Strait, shipped_straits, strait_levels};
#[cfg(test)]
use super::today_rivers::TodayRiver;
use super::today_rivers::{BurntNodes, TodayBurn, TodayTable, table_surfaces};
use super::{CubeLattice, EraHydrology, HydroGraph, NO_BASIN, Routed, Routing, receiver_of};
use crate::deep_time::YearsBp;
use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use crate::surface::{FrameSelection, Timeline};
use crate::worldgen::coarse::{CoarseField, SnapshotField, snapshot_sea_level_anomaly_m};
use crate::worldgen::tiles::{TILE_MARGIN_M, TileSet};

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

    /// How far [`NodeSource::node`]'s elevation at `dir` stands from the present-day overview
    /// there (m): the era's sea-level anomaly, ICE-6G change or crossfade. `None` where unknown
    /// or the terrain is not today's; the pass then burns no strait there ([`super::straits`]).
    fn present_shift_m(&self, _dir: RenderDir) -> Option<f32> {
        None
    }

    /// Whether the terrain at `dir` is today's, so the pass burns today's river courses there
    /// ([`super::today_rivers`]): where [`NodeSource::present_shift_m`] is known, the gorges'
    /// gate.
    fn todays_terrain(&self, dir: RenderDir) -> bool {
        self.present_shift_m(dir).is_some()
    }

    /// The present-day mapped lake at `dir` ([`lakemap`](super::lakemap)): its map id and its
    /// baked surface in the frame of [`NodeSource::node`]'s elevation. `None` where the map holds
    /// no lake, and everywhere unless [`NodeSource::data_lakes`] holds (the terrain is today's).
    /// The pass reads it only with data lakes.
    fn mapped(&self, _dir: RenderDir) -> Option<MappedLake> {
        None
    }

    /// Whether the present-day map holds a lake at `dir` (Task 8b): false unless
    /// [`NodeSource::data_lakes`] holds.
    fn mapped_lake(&self, dir: RenderDir) -> bool {
        self.mapped(dir).is_some()
    }
}

/// A present-day mapped lake at a node ([`NodeSource::mapped`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MappedLake {
    /// The lake's id in the map (1-based); nodes with one id are one lake.
    pub id: u16,
    /// Its baked surface, in the node's elevation frame.
    pub surface_m: f32,
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

    /// The node minus the present-day overview: the era's sea-level anomaly, the recent field's
    /// ICE-6G change or the bridge's crossfade; nothing at 0 ka in recent mode.
    fn present_shift_m(&self, dir: RenderDir) -> Option<f32> {
        if !self.data_lakes() {
            return None;
        }
        let (lat, lon) = geo_to_latlon(render_to_geo(dir));
        let (present, _) = self.tiles.sample_overview(lat, lon)?;
        Some(self.node(dir).elevation_m - present)
    }

    /// [`Self::present_shift_m`]'s gate without sampling the node: data lakes and an overview
    /// sample.
    fn todays_terrain(&self, dir: RenderDir) -> bool {
        if !self.data_lakes() {
            return false;
        }
        let (lat, lon) = geo_to_latlon(render_to_geo(dir));
        self.tiles.sample_overview(lat, lon).is_some()
    }

    /// The shipped map's lake, its level moved by [`Self::present_shift_m`] (0 where unknown).
    fn mapped(&self, dir: RenderDir) -> Option<MappedLake> {
        if !self.data_lakes() {
            return None;
        }
        let (lat, lon) = geo_to_latlon(render_to_geo(dir));
        let map = shipped_lake_map();
        let id = map.lake_at(lat, lon)?;
        let level = map.level(id);
        let shift = self.present_shift_m(dir).unwrap_or(0.0);
        Some(MappedLake {
            id,
            surface_m: level + shift,
        })
    }
}

/// How far beyond a region's inner field its river refinement reads the terrain (m), for
/// [`CoarseField::tiles_needed_with_margin`]: the farthest read
/// ([`river_read_reach_m`](crate::worldgen::generate::rivers::river_read_reach_m) at
/// [`SUBPOINT_M`]) plus [`TILE_MARGIN_M`] for the coarse lattice's taps around it. About 120 km
/// on the recent lattice.
pub fn river_tile_margin_m(lattice: &CubeLattice) -> f64 {
    TILE_MARGIN_M + crate::worldgen::generate::rivers::river_read_reach_m(lattice, SUBPOINT_M)
}

/// Samples of consecutive nodes from `start`, for [`HydroPrep::add_samples`].
#[derive(Clone, Debug, PartialEq)]
pub struct NodeBlock {
    pub start: u32,
    pub samples: Vec<NodeSample>,
    pub data_lakes: bool,
    /// The block's nodes the lake map holds, ascending; empty without data lakes.
    pub mapped: Vec<(u32, MappedLake)>,
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
    /// Sampling is done; the straits are burnt next ([`HydroPrep::burn_straits`]).
    Straits,
    /// Today's river courses are burnt into the routing elevation ([`TodayBurn`]).
    TodayRivers(Box<TodayBurn>),
    Route(Box<Routing>),
    Flats(Box<Routed>, Flats),
    Coast(Box<Routed>, Coast),
    Runoff(Box<Routed>, Vec<u16>, Accumulation),
    Walk(Box<Routed>, Vec<u16>, Accumulation),
    Rivers(Box<Routed>, Accumulation, u32, Vec<u32>),
    /// The river graph in steps, then the ocean bits, packed from this node on.
    Build(Box<Routed>, Accumulation, Box<GraphBuild>, Vec<u64>, u32),
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
    /// The index of the first basin that is a data flat, not a depression.
    first_flat: u32,
    /// The lake level table, and its placement once routing and the flats are done.
    lake_levels: Vec<LakeLevel>,
    placed_levels: Vec<(u32, f32)>,
    unplaced_levels: Vec<String>,
    /// The endorheic table lakes' basins (Task 11d fix round 2).
    endorheic_basins: Vec<u32>,
    elevation: Vec<f32>,
    /// One bit per node: ice fraction ≥ [`ICE_NODE_FRACTION`].
    ice: Vec<u64>,
    /// The nodes the lake map holds, ascending, with data lakes only ([`NodeSource::mapped`]);
    /// moved into the accumulation.
    mapped: Vec<(u32, MappedLake)>,
    /// The seaways burnt into the elevation with data lakes ([`super::straits`]).
    straits: Vec<Strait>,
    /// Today's river courses, burnt with data lakes ([`super::today_rivers`]).
    today: TodayTable,
    /// The burnt nodes and their levels, from the burn until the river graph is built: its beds
    /// there are the levels, while routing, the flats, the climate and the lakes read the burnt
    /// surface (Task 11b, fix round 1: raising the elevation back after routing put basin nodes
    /// above their own spill).
    beds: BurntNodes,
    /// The river nodes' anchors on today's courses, ascending (gathered with the river nodes).
    anchors: Vec<(u32, [f64; 3])>,
    /// Tests' own ridged steps in place of the shipped ones ([`super::ridged`]).
    ridged: Option<Vec<(u32, u32)>>,
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
        + r.basin_of.capacity()
        + r.held.capacity())
        + r.ocean.capacity()
        + r.receiver.capacity()
        + basins
}

impl HydroPrep {
    /// The pass with the shipped lake level table ([`shipped_lake_levels`]).
    pub fn new(lattice: CubeLattice, age: YearsBp) -> Self {
        Self::with_lake_levels(lattice, age, shipped_lake_levels())
    }

    /// The pass with another lake level table.
    pub fn with_lake_levels(lattice: CubeLattice, age: YearsBp, levels: &[LakeLevel]) -> Self {
        let len = lattice.len();
        Self {
            lake_levels: levels.to_vec(),
            placed_levels: Vec::new(),
            unplaced_levels: Vec::new(),
            endorheic_basins: Vec::new(),
            lattice,
            age,
            units: 0,
            data_lakes: false,
            first_flat: 0,
            elevation: Vec::with_capacity(len as usize),
            ice: vec![0; len.div_ceil(64) as usize],
            mapped: Vec::new(),
            straits: shipped_straits().to_vec(),
            today: TodayTable::Shipped,
            beds: BurntNodes::default(),
            anchors: Vec::new(),
            ridged: None,
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
                        let dir = lattice.dir(i);
                        self.push_sample(i, source.node(dir));
                        if self.data_lakes
                            && let Some(lake) = source.mapped(dir)
                        {
                            self.mapped.push((i, lake));
                        }
                    }
                    used += (end - start).max(1);
                    if end == len {
                        Stage::Straits
                    } else {
                        Stage::Sample
                    }
                }
                Stage::Straits => {
                    used += self.burn_straits(source);
                    if self.data_lakes {
                        let table = table_surfaces(&self.lake_levels, source);
                        Stage::TodayRivers(Box::new(TodayBurn::new(table)))
                    } else {
                        self.route()
                    }
                }
                Stage::TodayRivers(mut burn) => {
                    let ice = &self.ice;
                    let (done, u) = burn.step(
                        &self.today,
                        &lattice,
                        &mut self.elevation,
                        &self.mapped,
                        &|v| bit(ice, v),
                        source,
                        left,
                    );
                    used += u;
                    if done {
                        self.beds = burn.into_beds();
                        self.route()
                    } else {
                        Stage::TodayRivers(burn)
                    }
                }
                Stage::Route(mut routing) => {
                    let before = routing.units_done();
                    let done = routing.step(&lattice, left);
                    used += ((routing.units_done() - before) as u32).max(1);
                    if done {
                        let mut routed = Box::new(routing.finish());
                        routed.filled_m = Vec::new();
                        self.after_route(routed, climate.needs_coast_km())
                    } else {
                        Stage::Route(routing)
                    }
                }
                Stage::Flats(mut routed, mut flats) => {
                    let ice = &self.ice;
                    let (done, u) = flats.step(&lattice, &routed, &|i| bit(ice, i), left);
                    used += u;
                    if done {
                        self.first_flat = flats.register(&lattice, &mut routed);
                        self.after_flats(routed, climate.needs_coast_km())
                    } else {
                        Stage::Flats(routed, flats)
                    }
                }
                Stage::Coast(routed, mut coast) => {
                    let (done, u) = coast.step(&lattice, &routed, left);
                    used += u;
                    if done {
                        Stage::Runoff(routed, coast.cost, self.new_accumulation())
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
                            if let Some(a) = self.beds.anchor(i) {
                                let c = lattice.dir(i).0;
                                let reach = TODAY_RIVER_ANCHOR_MAX_SPACINGS
                                    * lattice.mean_spacing_m()
                                    / crate::worldgen::frame::EARTH_RADIUS_M;
                                self.anchors.push((i, held_within(c, a, reach).to_array()));
                            }
                        }
                    }
                    used += (end - next).max(1);
                    if end == len {
                        let ocean = vec![0u64; len.div_ceil(64) as usize];
                        Stage::Build(routed, acc, Box::new(GraphBuild::new(list)), ocean, 0)
                    } else {
                        Stage::Rivers(routed, acc, end, list)
                    }
                }
                Stage::Build(routed, acc, mut graph, mut ocean, next) => {
                    if !graph.done() {
                        let down_of = |v: u32| self.down_of(&routed, &acc, v);
                        let beds = &self.beds;
                        let info = |v: u32| {
                            let i = v as usize;
                            let bed = beds.get(v).unwrap_or(routed.elevation_m[i]);
                            (acc.q[i], acc.area_km2[i], bed)
                        };
                        let (_, u) = graph.step(&lattice, &down_of, &info, &acc.lakes, left);
                        used += u.max(1);
                        Stage::Build(routed, acc, graph, ocean, next)
                    } else if next < len {
                        let end = next.saturating_add(left).min(len);
                        for i in next..end {
                            if routed.ocean[i as usize] {
                                ocean[(i / 64) as usize] |= 1 << (i % 64);
                            }
                        }
                        used += (end - next).max(1);
                        Stage::Build(routed, acc, graph, ocean, end)
                    } else {
                        used += 1;
                        self.beds = BurntNodes::default();
                        let mut h = self.assemble(acc, graph.finish(), ocean);
                        h.anchors = std::mem::take(&mut self.anchors);
                        Stage::Done(Box::new(h))
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

    /// Routing over the (burnt) elevation.
    fn route(&mut self) -> Stage {
        // Endorheic table lakes seed routing like the sea at their level (with data lakes).
        let sinks = if self.data_lakes {
            self.lake_points()
                .into_iter()
                .zip(&self.lake_levels)
                .filter(|(_, l)| l.endorheic)
                .map(|(p, _)| p)
                .collect()
        } else {
            Vec::new()
        };
        Stage::Route(Box::new(
            Routing::new(std::mem::take(&mut self.elevation))
                .with_inland(self.beds.inland(), self.beds.ends())
                .with_corridor(self.beds.corridor())
                .with_courses(self.beds.successors())
                .with_sinks(sinks)
                .with_blocked(self.blocked_steps()),
        ))
    }

    /// The ridged steps routing forbids (Task 11g, [`super::ridged`]): the shipped ones where the
    /// lattice is theirs and the shipped courses burn (tests may give their own), each kept only
    /// where this era burnt its course node.
    fn blocked_steps(&self) -> Vec<(u32, u32)> {
        let shipped = || {
            let s = shipped_ridged_steps();
            (matches!(self.today, TodayTable::Shipped) && s.n == self.lattice.n)
                .then_some(s.steps.as_slice())
        };
        let steps = match &self.ridged {
            Some(given) => Some(given.as_slice()),
            None => shipped(),
        };
        steps.map_or_else(Vec::new, |steps| {
            steps
                .iter()
                .copied()
                .filter(|&(_, c)| self.beds.get(c).is_some())
                .collect()
        })
    }

    /// The lake level table's points as `(nearest node, level)`, in table order.
    fn lake_points(&self) -> Vec<(u32, f32)> {
        self.lake_levels
            .iter()
            .map(|l| {
                let dir = geo_to_render(latlon_to_geo(l.lat, l.lon));
                (self.lattice.nearest(dir), l.level_m)
            })
            .collect()
    }

    /// The stage after routing: the data's flats, unless the map decides every
    /// lake (the present day), else what follows them.
    fn after_route(&mut self, routed: Box<Routed>, needs_coast_km: bool) -> Stage {
        self.first_flat = routed.basins.len() as u32;
        if self.data_lakes && !self.present() {
            Stage::Flats(routed, Flats::new(self.lattice.len()))
        } else {
            self.after_flats(routed, needs_coast_km)
        }
    }

    /// The stage after the flats: places the lake level table (with data lakes only), then
    /// the coast distance if the climate needs it, else runoff.
    fn after_flats(&mut self, routed: Box<Routed>, needs_coast_km: bool) -> Stage {
        if self.data_lakes {
            let points = self.lake_points();
            let ice = &self.ice;
            let (placed, unplaced) =
                place_levels(&routed, self.first_flat, &|i| bit(ice, i), &points);
            // The endorheic lakes' basins (their sinks), where placed.
            self.endorheic_basins = points
                .iter()
                .zip(&self.lake_levels)
                .filter(|(_, l)| l.endorheic)
                .map(|(&(v, _), _)| routed.basin_of[v as usize])
                .filter(|b| placed.iter().any(|p| p.0 == *b))
                .collect();
            self.placed_levels = placed;
            self.unplaced_levels = unplaced
                .into_iter()
                .map(|k| self.lake_levels[k].name.clone())
                .collect();
        }
        if needs_coast_km {
            Stage::Coast(routed, Coast::new(self.lattice.len()))
        } else {
            Stage::Runoff(routed, Vec::new(), self.new_accumulation())
        }
    }

    fn new_accumulation(&mut self) -> Accumulation {
        let mut acc = Accumulation::new(self.lattice.len(), self.first_flat);
        acc.set_levels(self.placed_levels.clone());
        acc.set_endorheic(self.endorheic_basins.clone());
        acc.set_mapped(std::mem::take(&mut self.mapped));
        acc.set_present(self.present());
        acc
    }

    /// The present-day snapshot with today's terrain: the map decides every lake.
    fn present(&self) -> bool {
        self.data_lakes && self.age.0 <= 0.0
    }

    /// Lowers each strait's and gorge's nodes to its level ([`strait_levels`]), moved by the
    /// era's shift, with data lakes only.
    /// Returns the units used (the nodes visited, at least 1).
    fn burn_straits<S: NodeSource>(&mut self, source: &S) -> u32 {
        let mut used = 1;
        if !self.data_lakes {
            return used;
        }
        for strait in &self.straits {
            for (v, level) in strait_levels(&self.lattice, strait) {
                used += 1;
                if let Some(shift) = source.present_shift_m(self.lattice.dir(v)) {
                    let e = &mut self.elevation[v as usize];
                    *e = e.min(level + shift);
                }
            }
        }
        used
    }

    /// The stage the pass is in, by name: `sample`, `straits`, `today_rivers`, `route`, `flats`,
    /// `coast`, `runoff`, `walk`, `rivers`, `build` or `done` (for logs that say where a long step
    /// went).
    pub fn stage_name(&self) -> &'static str {
        match &self.stage {
            Stage::Sample => "sample",
            Stage::Straits => "straits",
            Stage::TodayRivers(_) => "today_rivers",
            Stage::Route(_) => "route",
            Stage::Flats(..) => "flats",
            Stage::Coast(..) => "coast",
            Stage::Runoff(..) => "runoff",
            Stage::Walk(..) => "walk",
            Stage::Rivers(..) => "rivers",
            Stage::Build(..) => "build",
            Stage::Done(_) => "done",
            Stage::Moving => "moving",
        }
    }

    /// The pass with other lake level and strait tables (tests).
    #[cfg(test)]
    pub(crate) fn with_tables(
        lattice: CubeLattice,
        age: YearsBp,
        levels: &[LakeLevel],
        straits: &[Strait],
    ) -> Self {
        let mut prep = Self::with_lake_levels(lattice, age, levels);
        prep.straits = straits.to_vec();
        prep
    }

    /// This pass with other river courses in place of the shipped table (tests).
    #[cfg(test)]
    pub(crate) fn with_today_rivers(mut self, rivers: Vec<TodayRiver>) -> Self {
        self.today = TodayTable::Given(rivers);
        self
    }

    /// This pass with other ridged steps in place of the shipped ones (tests; sorted).
    #[cfg(test)]
    pub(crate) fn with_ridged_steps(mut self, steps: Vec<(u32, u32)>) -> Self {
        self.ridged = Some(steps);
        self
    }

    fn push_sample(&mut self, i: u32, s: NodeSample) {
        self.elevation.push(s.elevation_m);
        if is_ice_sample(&s) {
            self.ice[(i / 64) as usize] |= 1 << (i % 64);
        }
    }

    /// Where river node `v` drains ([`RiverGraph::build`]'s `down_of`).
    fn down_of(&self, routed: &Routed, acc: &Accumulation, v: u32) -> Down {
        let lattice = self.lattice;
        let Some(j) = receiver_of(&lattice, routed, v) else {
            // A closed basin's sink with no lake on it; otherwise only a graph with no ocean
            // at all has a land node with no receiver: its lowest node.
            return if routed.basin_of[v as usize] == NO_BASIN {
                Down::Ocean(v)
            } else {
                Down::Sink(v)
            };
        };
        if routed.ocean[j as usize] {
            Down::Ocean(j)
        } else if let Some(l) = lake_at(acc, j) {
            Down::Lake(l, j)
        } else if bit(&self.ice, j) {
            Down::Ice(j)
        } else {
            Down::River(j)
        }
    }

    /// The pass's result from the finished graph and the packed ocean bits.
    fn assemble(&self, acc: Accumulation, rivers: RiverGraph, ocean: Vec<u64>) -> EraHydrology {
        EraHydrology {
            age: self.age,
            lattice: self.lattice,
            lakes: acc.lakes,
            rivers,
            data_lakes: self.data_lakes,
            unplaced_lake_levels: self.unplaced_levels.clone(),
            ocean,
            lake_nodes: acc.lake_nodes,
            anchors: Vec::new(),
            basin_routed: acc.basin_routed,
        }
    }

    /// Samples nodes `range` of `lattice` (pure, so blocks can be sampled in parallel).
    pub fn sample_range<S: NodeSource>(
        lattice: &CubeLattice,
        source: &S,
        range: Range<u32>,
    ) -> NodeBlock {
        let data_lakes = source.data_lakes();
        let mapped = if data_lakes {
            range
                .clone()
                .filter_map(|i| source.mapped(lattice.dir(i)).map(|l| (i, l)))
                .collect()
        } else {
            Vec::new()
        };
        NodeBlock {
            start: range.start,
            samples: range.clone().map(|i| source.node(lattice.dir(i))).collect(),
            data_lakes,
            mapped,
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
        if self.data_lakes {
            self.mapped.extend_from_slice(&block.mapped);
        }
        self.units += block.samples.len() as u64;
        for (k, s) in block.samples.iter().enumerate() {
            self.push_sample(block.start + k as u32, *s);
        }
        if self.elevation.len() == len as usize {
            self.stage = Stage::Straits;
        }
    }

    /// The units of work done so far.
    pub fn units_done(&self) -> u64 {
        self.units
    }

    /// The bytes the pass holds now, by capacity.
    pub fn working_bytes(&self) -> usize {
        let own = 4 * self.elevation.capacity()
            + 8 * (self.ice.capacity() + self.mapped.capacity())
            + self.beds.bytes()
            + 32 * self.anchors.capacity();
        own + match &self.stage {
            Stage::Sample | Stage::Straits | Stage::Moving => 0,
            Stage::TodayRivers(b) => b.bytes(),
            Stage::Route(r) => r.working_bytes(),
            Stage::Flats(r, f) => routed_bytes(r) + f.working_bytes(),
            Stage::Coast(r, c) => routed_bytes(r) + c.bytes(),
            Stage::Runoff(r, c, a) | Stage::Walk(r, c, a) => {
                routed_bytes(r) + 2 * c.capacity() + a.working_bytes()
            }
            Stage::Build(r, a, g, ocean, _) => {
                routed_bytes(r) + a.working_bytes() + g.working_bytes() + 8 * ocean.capacity()
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

/// `p` moved toward `c` (unit vectors) until it lies within `reach` radians of it.
fn held_within(c: bevy_math::DVec3, p: bevy_math::DVec3, reach: f64) -> bevy_math::DVec3 {
    let angle = c.angle_between(p);
    if angle <= reach {
        return p;
    }
    let t = (p - c * c.dot(p)).normalize();
    (c * reach.cos() + t * reach.sin()).normalize()
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
        DATA_LAKE_FLAT_MAX_OUTLETS, DATA_LAKE_FLAT_MIN_NODES, DATA_LAKE_FLAT_TOL_M,
        DATA_LAKE_MAX_BALANCE, DATA_LAKE_MIN_BALANCE, DATA_LAKE_MIN_NODES, HYDRO_RECENT_N,
        ICE_NODE_FRACTION, LARGE_LAKE_KM2, RIVER_MIN_DISCHARGE_M3S, TODAY_RIVER_BURN_M,
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
        /// spill out to `ring_r`, then down to -20 m at the coast, `coast_r` (km). The fixture's
        /// bowls have no ring (`ring_r = wall_r`): a flat ring would be a data flat.
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
        /// `(km north of the centre, radius km, depth m)`. A terraced dome has a flat ledge
        /// 100 to `LEDGE_R` km out within 50° of north (its long lower edge keeps it from being a
        /// data flat), with a dip 0.3 m deep 110 to 175 km out within 40°: a basin too shallow
        /// for a lake, which rivers pass through and climb out of onto the ledge.
        Dome {
            peak_m: f64,
            coast_r: f64,
            crater: Option<(f64, f64, f64)>,
            terrace: bool,
        },
        /// A lake shelf like Victoria's in the data: exactly flat at `level_m` out to `flat_r`,
        /// walls rising to `rim_m` at `wall_r`, then down to -20 m at `coast_r`; a notch north
        /// of the shelf, `notch_km` either side of the north line and widening downstream,
        /// descends from the shelf straight to the coast. Not a depression; its downhill
        /// boundary is the few shelf nodes beside the notch's head.
        Shelf {
            level_m: f64,
            flat_r: f64,
            rim_m: f64,
            wall_r: f64,
            coast_r: f64,
            notch_km: f64,
        },
        /// A plateau sloping `slope` m/km northward from `base_m`, stored in `step_m` stairs (as
        /// F16 stores high plateaus), out to `top_r`, then a cliff to -20 m at `coast_r`.
        Stairs {
            base_m: f64,
            slope: f64,
            step_m: f64,
            top_r: f64,
            coast_r: f64,
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
    const VICTORIA: usize = 8;
    const DRY_FLAT: usize = 9;
    const STAIRS: usize = 10;
    /// The wet dome's ledge reaches this far out (km).
    const LEDGE_R: f64 = 230.0;

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
                Shape::Bowl { coast_r, .. }
                | Shape::Dome { coast_r, .. }
                | Shape::Shelf { coast_r, .. }
                | Shape::Stairs { coast_r, .. } => coast_r,
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
        /// Today's lake map: `(island, radius km, surface m)`, a mapped lake within the radius
        /// of the island's centre (read only with data lakes).
        map: Vec<(usize, f64, f32)>,
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
            let cone = bowl(100.0, 0.0, 200.0, 500.0, 200.0, 450.0, 0.05, 0.0);
            let flat = bowl(-28.0, 150.0, 400.0, 30.0, 400.0, 650.0, 0.0, 0.0);
            let shelf = Shape::Shelf {
                level_m: 600.0,
                flat_r: 137.0,
                rim_m: 1100.0,
                wall_r: 420.0,
                coast_r: 600.0,
                // Wide enough for one node at the head on the n = 300 lattice.
                notch_km: 18.0,
            };
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
                // Inflow about 3 x what the flat evaporates: a data lake, though the balance
                // alone would raise it up the walls.
                island(
                    dir(40.0, 20.0),
                    flat,
                    Climate {
                        precip_mm: 650.0,
                        temp_c: 13.0,
                    },
                ),
                island(dir(-40.0, 60.0), flat, b(BiomeClass::HotDesert)),
                // On the cube corner where +X, +Y and +Z meet: the apex's three corner texels are
                // equidistant, so the lowest nodes tie within the data tolerance.
                island(
                    RenderDir(DVec3::ONE),
                    bowl(0.0, 0.0, 650.0, 800.0, 650.0, 900.0, 0.05, 40.0),
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
                // Humid enough that its lakes evaporate nothing net.
                island(dir(25.0, 150.0), dome, b(BiomeClass::TemperateRainforest)),
                island(dir(5.0, 95.0), shelf, b(BiomeClass::MonsoonSavanna)),
                // Semi-arid: the flat fails the veto, yet 250+ m3/s cross it.
                island(
                    dir(-50.0, -120.0),
                    shelf,
                    Climate {
                        precip_mm: 300.0,
                        temp_c: 15.0,
                    },
                ),
                island(
                    dir(-30.0, 120.0),
                    Shape::Stairs {
                        base_m: 5100.0,
                        slope: 0.05,
                        step_m: 4.0,
                        top_r: 350.0,
                        coast_r: 400.0,
                    },
                    b(BiomeClass::TropicalRainforest),
                ),
            ];
            islands[ICE].ice_r = 350.0;
            World {
                islands,
                coast_scale: None,
                data_lakes: true,
                // The humid bowl's lake at its crest, the Caspian-like flat and the
                // Victoria-like shelf are today's lakes.
                map: vec![
                    (HUMID, 150.0, 500.0),
                    (CASPIAN, 150.0, -28.0),
                    (VICTORIA, 137.0, 600.0),
                ],
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
                    let bearing = x.atan2(y).to_degrees();
                    if terrace && (100.0..=LEDGE_R).contains(&r) && bearing.abs() < 50.0 {
                        e = cone(LEDGE_R);
                        if (110.0..=175.0).contains(&r) && bearing.abs() < 40.0 {
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
                Shape::Shelf {
                    level_m,
                    flat_r,
                    rim_m,
                    wall_r,
                    coast_r,
                    notch_km,
                } => {
                    if r > coast_r {
                        return (-2000.0, 0.0);
                    }
                    if r <= flat_r {
                        level_m
                    } else if y > 0.0 && x.abs() < notch_km + 0.3 * (r - flat_r) {
                        level_m + (-20.0 - level_m) * (r - flat_r) / (coast_r - flat_r)
                    } else if r <= wall_r {
                        level_m + (rim_m - level_m) * (r - flat_r) / (wall_r - flat_r)
                    } else {
                        rim_m + (-20.0 - rim_m) * (r - wall_r) / (coast_r - wall_r)
                    }
                }
                Shape::Stairs {
                    base_m,
                    slope,
                    step_m,
                    top_r,
                    coast_r,
                } => {
                    if r > coast_r {
                        return (-2000.0, 0.0);
                    }
                    let stair = step_m * ((base_m + slope * y) / step_m).floor();
                    if r <= top_r {
                        stair
                    } else {
                        stair + (-20.0 - stair) * (r - top_r) / (coast_r - top_r)
                    }
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

        /// Read even without data lakes, so tests see that the pass ignores it then. Map
        /// entry `k` is lake id `k + 1`.
        fn mapped(&self, dir: RenderDir) -> Option<MappedLake> {
            let (k, (r, _, _)) = self.nearest(dir.0);
            self.map
                .iter()
                .position(|&(island, radius, _)| island == k && r < radius)
                .map(|e| MappedLake {
                    id: e as u16 + 1,
                    surface_m: self.map[e].2,
                })
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

    /// A recent era in the past: the map caps lakes, the water balance makes them.
    const PAST: YearsBp = YearsBp(6_000.0);
    /// The present-day snapshot: the map decides every lake.
    const PRESENT: YearsBp = YearsBp(0.0);

    /// Runs the pass for a past era ([`PAST`]) to the end in steps of `budget` units.
    fn run<S: NodeSource, C: ClimateSource>(
        lattice: CubeLattice,
        source: &S,
        climate: &C,
        table: &ClimateTable,
        budget: u32,
    ) -> EraHydrology {
        run_at(lattice, PAST, source, climate, table, budget)
    }

    /// [`run`] at `age`.
    fn run_at<S: NodeSource, C: ClimateSource>(
        lattice: CubeLattice,
        age: YearsBp,
        source: &S,
        climate: &C,
        table: &ClimateTable,
        budget: u32,
    ) -> EraHydrology {
        let mut prep = HydroPrep::new(lattice, age);
        while !prep.step(source, climate, table, budget) {}
        prep.finish()
    }

    const WORLD_N: u32 = 300;

    struct Shared {
        world: World,
        table: ClimateTable,
        h: EraHydrology,
    }

    /// The world in a past recent era ([`PAST`]).
    fn shared() -> &'static Shared {
        static SHARED: OnceLock<Shared> = OnceLock::new();
        SHARED.get_or_init(|| Shared::new(true, PAST))
    }

    /// The same world without data lakes: shallow flat basins pass their water through.
    fn shared_without_data() -> &'static Shared {
        static SHARED: OnceLock<Shared> = OnceLock::new();
        SHARED.get_or_init(|| Shared::new(false, PAST))
    }

    /// The world at the present-day snapshot: the map decides every lake.
    fn shared_present() -> &'static Shared {
        static SHARED: OnceLock<Shared> = OnceLock::new();
        SHARED.get_or_init(|| Shared::new(true, PRESENT))
    }

    impl Shared {
        fn new(data_lakes: bool, age: YearsBp) -> Shared {
            let mut world = World::new();
            world.data_lakes = data_lakes;
            let table = shipped_table();
            let h = run_at(
                CubeLattice { n: WORLD_N },
                age,
                &world,
                &world,
                &table,
                u32::MAX,
            );
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
        assert_eq!(lake.level_m, lake.spill_m);
        // No flat ring: the spill is the crest as the lattice samples it.
        assert!(lake.spill_m > 440.0 && lake.spill_m <= 500.0, "{lake:?}");
        assert!(
            lake.outflow_m3s > 0.0 && lake.evaporation_m3s >= 0.0,
            "{lake:?}"
        );
        let bowl = s.area_within(HUMID, 200.0);
        assert!(
            lake.area_km2 > 0.6 * bowl,
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
        assert!(lake.spill_m > 25.0 && lake.spill_m <= 30.0, "{lake:?}");
        assert!(lake.level_m < lake.spill_m - 50.0);
        assert_eq!(lake.outflow_m3s, 0.0);
        // The hazard: the inflow is about 3 x what the flat evaporates, inside both data
        // bounds, so by water balance alone the lake would rise above the flat.
        let c = s.world.islands[CASPIAN].climate;
        let capacity = m3s(f64::from(s.table.net_evaporation_mm(c)), lake.area_km2);
        let ratio = lake.inflow_m3s / capacity;
        assert!((2.0..5.0).contains(&ratio), "inflow / capacity {ratio}");
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

    /// The wet dome's dip nodes: 0.3 m below its ledge.
    fn dip_nodes(s: &Shared) -> Vec<u32> {
        let lattice = s.lattice();
        let ledge = 1200.0 + (-20.0 - 1200.0) * LEDGE_R / 350.0;
        (0..lattice.len())
            .filter(|&i| {
                s.island_of(i).is_some_and(|(k, _)| k == WET)
                    && (f64::from(s.elevation(i)) - (ledge - 0.3)).abs() < 1e-3
            })
            .collect()
    }

    #[test]
    fn a_shallow_dip_on_a_river_is_not_a_data_lake() {
        let s = shared();
        assert!(s.h.data_lakes);
        let lattice = s.lattice();
        let dip = dip_nodes(s);
        // The hazard: two or more connected dip nodes within the data tolerance, carrying a
        // river far larger than what they evaporate.
        assert!(dip.len() >= DATA_LAKE_MIN_NODES as usize, "{dip:?}");
        assert!(dip.iter().any(|&i| {
            lattice
                .neighbours(i)
                .into_iter()
                .flatten()
                .any(|j| dip.contains(&j))
        }));
        let c = s.world.islands[WET].climate;
        let cap = m3s(
            f64::from(s.table.net_evaporation_mm(c)),
            dip.iter().map(|&i| lattice.area_m2(i) * 1e-6).sum(),
        );
        assert!(dip.iter().all(|&i| s.h.lake_of(i).is_none()));
        assert!(s.lakes_on(WET).is_empty(), "{:?}", s.lakes_on(WET));
        let fed = dip
            .iter()
            .filter_map(|&i| s.h.rivers.index_of(i))
            .map(|k| f64::from(s.h.rivers.nodes()[k as usize].discharge_m3s))
            .fold(0.0, f64::max);
        assert!(
            fed > DATA_LAKE_MAX_BALANCE * cap,
            "a river of {fed} through {cap}"
        );
    }

    #[test]
    fn a_victoria_like_flat_is_an_open_data_lake() {
        let s = shared();
        let flat = floor_nodes(s, VICTORIA, 137.0);
        // The hazard: a flat of more than DATA_LAKE_FLAT_MIN_NODES nodes at exactly one
        // level, which is not a depression (it drains through its notch).
        assert!(
            flat.len() > DATA_LAKE_FLAT_MIN_NODES as usize,
            "{}",
            flat.len()
        );
        assert!(flat.iter().all(|&i| s.elevation(i) == 600.0));
        // It drains through a narrow outlet: few nodes on its downhill boundary.
        let outlets = downhill_boundary(s, &flat);
        assert!(
            (1..=DATA_LAKE_FLAT_MAX_OUTLETS as usize).contains(&outlets),
            "{outlets}"
        );
        let lakes = s.lakes_on(VICTORIA);
        assert_eq!(lakes.len(), 1, "{lakes:?}");
        let id = lakes[0] as u16;
        let lake = &s.h.lakes[lakes[0]];
        assert_eq!(lake.kind, LakeKind::Data, "{lake:?}");
        assert_eq!(lake.level_m, 600.0);
        assert!(flat.iter().all(|&i| s.h.lake_of(i) == Some(id)));
        // Inflow passes through, minus evaporation over the footprint.
        let c = s.world.islands[VICTORIA].climate;
        let evap = m3s(f64::from(s.table.net_evaporation_mm(c)), lake.area_km2);
        assert!(
            (lake.evaporation_m3s - evap).abs() <= 1e-6 * evap,
            "{lake:?} vs {evap}"
        );
        assert_eq!(lake.outflow_m3s, lake.inflow_m3s - lake.evaporation_m3s);
        assert!(lake.outflow_m3s > 0.5 * lake.inflow_m3s, "{lake:?}");
        // The river continues out of the notch carrying the outflow, to the sea.
        let (k, out) =
            s.h.rivers
                .nodes()
                .iter()
                .enumerate()
                .filter(|(_, n)| {
                    s.island_of(n.node)
                        .is_some_and(|(i, r)| i == VICTORIA && r > 137.0)
                })
                .filter(|(_, n)| f64::from(n.discharge_m3s) >= lake.outflow_m3s * (1.0 - 1e-6))
                .min_by(|a, b| a.1.discharge_m3s.total_cmp(&b.1.discharge_m3s))
                .expect("a river leaves the lake");
        let one_node = lake.area_km2 / flat.len() as f64;
        let local = m3s(f64::from(s.table.runoff_mm(c)), 4.0 * one_node);
        assert!(
            f64::from(out.discharge_m3s) - lake.outflow_m3s <= local,
            "{out:?} vs outflow {}",
            lake.outflow_m3s
        );
        assert!(matches!(chain_end(&s.h.rivers, k as u32), Down::Ocean(_)));
        // Rivers off the walls end in the lake.
        assert!(
            s.h.rivers
                .nodes()
                .iter()
                .any(|n| matches!(n.down, Down::Lake(l, _) if l == id))
        );
        // Without data lakes the flat is just land.
        assert!(shared_without_data().lakes_on(VICTORIA).is_empty());
    }

    #[test]
    fn an_arid_flat_stays_dry() {
        let s = shared();
        let flat = floor_nodes(s, DRY_FLAT, 137.0);
        assert!(flat.len() > DATA_LAKE_FLAT_MIN_NODES as usize);
        assert!(flat.iter().all(|&i| s.elevation(i) == 600.0));
        // The hazard: water reaches the flat, but less than the veto's share of what it
        // evaporates.
        let c = s.world.islands[DRY_FLAT].climate;
        let inflow = m3s(
            f64::from(s.table.runoff_mm(c)),
            s.area_within(DRY_FLAT, 420.0),
        );
        let cap = m3s(
            f64::from(s.table.net_evaporation_mm(c)),
            s.area_within(DRY_FLAT, 137.0),
        );
        assert!(inflow > 0.0 && inflow < DATA_LAKE_MIN_BALANCE * cap);
        assert!(s.lakes_on(DRY_FLAT).is_empty());
        assert!(flat.iter().all(|&i| s.h.lake_of(i).is_none()));
    }

    /// The lattice node a river node drains to.
    fn down_node(rivers: &RiverGraph, n: &crate::hydrology::RiverNode) -> u32 {
        match n.down {
            Down::River(t) => rivers.nodes()[t as usize].node,
            Down::Ocean(j) | Down::Lake(_, j) | Down::Ice(j) | Down::Sink(j) => j,
        }
    }

    /// Shelf nodes beside a strictly lower node off the shelf, at exactly `level`.
    fn downhill_boundary(s: &Shared, flat: &[u32]) -> usize {
        let lattice = s.lattice();
        flat.iter()
            .filter(|&&v| {
                lattice
                    .neighbours(v)
                    .into_iter()
                    .flatten()
                    .any(|j| !flat.contains(&j) && s.elevation(j) < s.elevation(v))
            })
            .count()
    }

    #[test]
    fn river_links_join_lattice_neighbours() {
        // The hazards: routing's own receivers jump to a basin's exit across the wet dome's
        // shallow dip, and a semi-arid flat that fails the veto carries rivers across itself.
        let s = shared();
        let lattice = s.lattice();
        let elevation: Vec<f32> = (0..lattice.len()).map(|i| s.elevation(i)).collect();
        let mut routing = Routing::new(elevation);
        while !routing.step(&lattice, u32::MAX) {}
        let r = routing.finish();
        let dip = dip_nodes(s);
        let jumps = dip
            .iter()
            .filter(|&&v| {
                r.receiver[v as usize] == crate::hydrology::RECEIVER_EXIT && {
                    let exit = r.basins[r.basin_of[v as usize] as usize].exit;
                    !lattice.neighbours(v).contains(&Some(exit))
                }
            })
            .count();
        assert!(jumps > 0, "no jump across the dip in routing's tree");
        let flat = floor_nodes(s, DRY_FLAT, 137.0);
        let crossing = flat
            .iter()
            .filter(|&&i| s.h.rivers.index_of(i).is_some())
            .count();
        assert!(crossing >= 3, "{crossing} river nodes on the failed flat");
        assert!(s.lakes_on(DRY_FLAT).is_empty());
        for world in [shared(), shared_without_data()] {
            let rivers = &world.h.rivers;
            for n in rivers.nodes() {
                let j = down_node(rivers, n);
                assert!(
                    lattice.neighbours(n.node).contains(&Some(j)),
                    "{n:?} drains to {j}, not a neighbour"
                );
            }
        }
    }

    #[test]
    fn a_quantisation_staircase_makes_no_lake() {
        let s = shared();
        let lattice = s.lattice();
        // The hazard: a humid plateau whose 4 m stairs are exact flats of 20+ nodes.
        let mut by_level = std::collections::BTreeMap::<i64, Vec<u32>>::new();
        for i in 0..lattice.len() {
            if s.island_of(i)
                .is_some_and(|(k, r)| k == STAIRS && r <= 350.0)
            {
                by_level.entry(s.elevation(i) as i64).or_default().push(i);
            }
        }
        let widest = by_level.values().max_by_key(|v| v.len()).unwrap();
        assert!(
            widest.len() >= DATA_LAKE_FLAT_MIN_NODES as usize,
            "{by_level:?}"
        );
        assert!(downhill_boundary(s, widest) > DATA_LAKE_FLAT_MAX_OUTLETS as usize);
        assert_eq!(
            s.table.net_evaporation_mm(s.world.islands[STAIRS].climate),
            0.0
        );
        assert!(s.lakes_on(STAIRS).is_empty(), "{:?}", s.lakes_on(STAIRS));
        assert!(widest.iter().all(|&i| s.h.lake_of(i).is_none()));
    }

    #[test]
    fn shore_steps_ignore_missing_corner_neighbours() {
        let s = shared();
        let lattice = s.lattice();
        let id = s.lakes_on(TERMINAL)[0] as u16;
        // The hazard: the lake covers cube-corner texels, which have only seven neighbours.
        let corners: Vec<u32> = (0..lattice.len())
            .filter(|&i| s.h.lake_of(i) == Some(id) && lattice.neighbours(i).contains(&None))
            .collect();
        assert_eq!(corners.len(), 3, "{corners:?}");
        // Shore steps are the BFS distance from the dry land around the lake.
        let mut want = std::collections::HashMap::new();
        let mut queue = std::collections::VecDeque::new();
        for i in 0..lattice.len() {
            if s.h.lake_of(i) == Some(id)
                && lattice
                    .neighbours(i)
                    .into_iter()
                    .flatten()
                    .any(|j| s.h.lake_of(j) != Some(id))
            {
                want.insert(i, 1u16);
                queue.push_back(i);
            }
        }
        while let Some(v) = queue.pop_front() {
            let d = want[&v];
            for j in lattice.neighbours(v).into_iter().flatten() {
                if s.h.lake_of(j) == Some(id) && !want.contains_key(&j) {
                    want.insert(j, d + 1);
                    queue.push_back(j);
                }
            }
        }
        for (&i, &d) in &want {
            assert_eq!(s.h.shore_steps(i), d, "node {i}");
        }
        for &c in &corners {
            assert!(
                s.h.shore_steps(c) >= 4,
                "corner {c}: {}",
                s.h.shore_steps(c)
            );
        }
    }

    #[test]
    fn the_lake_level_table_applies_only_with_data_lakes() {
        let lattice = CubeLattice { n: 75 };
        let table = shipped_table();
        let (lat, lon) = latlon(RenderDir(World::new().islands[ARID].centre));
        let level = |name: &str, lat, lon, level_m| crate::hydrology::levels::LakeLevel {
            name: name.into(),
            lat,
            lon,
            level_m,
            overview_m: None,
            source: "test".into(),
            note: String::new(),
            endorheic: false,
        };
        let levels = [
            level("Arid", lat, lon, 300.0),
            level("Nowhere", -60.0, -150.0, 5.0),
        ];
        for data_lakes in [true, false] {
            let mut world = World::new();
            world.data_lakes = data_lakes;
            let mut prep = HydroPrep::with_lake_levels(lattice, YearsBp(0.0), &levels);
            while !prep.step(&world, &world, &table, u32::MAX) {}
            let h = prep.finish();
            let on_arid: Vec<&crate::hydrology::Lake> = h
                .lakes
                .iter()
                .filter(|l| {
                    world
                        .island_at(lattice.dir(l.exit).0)
                        .is_some_and(|(k, _)| k == ARID)
                })
                .collect();
            if data_lakes {
                // The arid bowl's balance makes no lake; the table makes a terminal one at its
                // level, since it evaporates more than it receives.
                assert_eq!(on_arid.len(), 1, "{on_arid:?}");
                assert_eq!(
                    (on_arid[0].kind, on_arid[0].level_m),
                    (LakeKind::Terminal, 300.0)
                );
                assert_eq!(on_arid[0].outflow_m3s, 0.0);
                assert_eq!(h.unplaced_lake_levels, vec!["Nowhere".to_string()]);
            } else {
                assert!(on_arid.is_empty(), "{on_arid:?}");
                assert!(h.unplaced_lake_levels.is_empty());
            }
        }
    }

    /// Task 11d fix round 2: an endorheic table lake's seeded routing slices like the rest of the
    /// pass, and its lake is terminal at its level.
    #[test]
    fn an_endorheic_table_lake_passes_the_same_time_boxed() {
        let lattice = CubeLattice { n: 75 };
        let table = shipped_table();
        let (lat, lon) = latlon(RenderDir(World::new().islands[ARID].centre));
        let levels = [crate::hydrology::levels::LakeLevel {
            name: "Arid".into(),
            lat,
            lon,
            level_m: 300.0,
            overview_m: None,
            source: "test".into(),
            note: String::new(),
            endorheic: true,
        }];
        let run = |budget: u32| {
            let mut world = World::new();
            world.data_lakes = true;
            let mut prep = HydroPrep::with_lake_levels(lattice, YearsBp(0.0), &levels);
            while !prep.step(&world, &world, &table, budget) {}
            prep.finish()
        };
        let whole = run(u32::MAX);
        assert_eq!(run(97), whole);
        let world = World::new();
        let on_arid: Vec<&crate::hydrology::Lake> = whole
            .lakes
            .iter()
            .filter(|l| {
                world
                    .island_at(lattice.dir(l.exit).0)
                    .is_some_and(|(k, _)| k == ARID)
            })
            .collect();
        assert_eq!(on_arid.len(), 1, "{on_arid:?}");
        assert_eq!(
            (on_arid[0].kind, on_arid[0].level_m, on_arid[0].outflow_m3s),
            (LakeKind::Terminal, 300.0, 0.0)
        );
    }

    /// Every river chain ends in the ocean, a lake, the ice, or (at the present day only) a
    /// closed basin's dry sink: the river node itself, a lake on none of its taps. Returns the
    /// chain ends `(ocean, lake, ice, sink)`.
    fn chain_ends(s: &Shared, h: &EraHydrology) -> (u32, u32, u32, u32) {
        let rivers = &h.rivers;
        let (mut ocean, mut lake, mut ice, mut sink) = (0, 0, 0, 0);
        for (k, n) in rivers.nodes().iter().enumerate() {
            match n.down {
                Down::River(t) => assert!((t as usize) < rivers.nodes().len()),
                Down::Ocean(j) => assert!(h.is_ocean(j)),
                Down::Lake(l, j) => assert_eq!(h.lake_of(j), Some(l)),
                Down::Ice(j) => assert!(s.is_ice(j)),
                Down::Sink(j) => {
                    assert_eq!(h.age.0, 0.0, "a dry sink only at the present day");
                    assert_eq!(j, n.node, "{n:?}");
                    assert_eq!(h.lake_of(j), None);
                }
            }
            match chain_end(rivers, k as u32) {
                Down::Ocean(_) => ocean += 1,
                Down::Lake(..) => lake += 1,
                Down::Ice(_) => ice += 1,
                Down::Sink(_) => sink += 1,
                Down::River(_) => unreachable!(),
            }
        }
        (ocean, lake, ice, sink)
    }

    #[test]
    fn rivers_end_at_the_sea_a_lake_or_the_ice() {
        let s = shared();
        let rivers = &s.h.rivers;
        let (ocean, lake, ice, sink) = chain_ends(s, &s.h);
        assert_eq!(sink, 0, "no dry sink in a past era");
        // At the present day an unmapped closed basin holds no lake: its rivers end at its sink.
        let p = shared_present();
        let (_, _, _, present_sinks) = chain_ends(p, &p.h);
        assert!(
            present_sinks > 0,
            "the hazard: closed basins at the present day"
        );
        println!(
            "{} river nodes: {ocean} reach the ocean, {lake} a lake, {ice} the ice; at the \
             present day {present_sinks} end at a dry sink",
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
        // The hazard: some reaches climb in elevation, out of the ledge's shallow dip (a
        // pass-through basin), with data lakes on or off.
        for s in [shared(), shared_without_data()] {
            let climbs = beds_never_rise(s);
            assert!(climbs > 0, "{climbs}");
        }
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
                Down::Ice(_) | Down::Sink(_) => {}
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
        assert!(lake.area_km2 > 50_000.0, "{lake:?}");
        // The hazard: the apex's corner texels tie within the data tolerance (a 2+ node flat),
        // but the inflow is far over DATA_LAKE_MAX_BALANCE x what they evaporate.
        let lattice = s.lattice();
        let bowl: Vec<u32> = (0..lattice.len())
            .filter(|&i| {
                s.island_of(i)
                    .is_some_and(|(k, r)| k == TERMINAL && r < 650.0)
            })
            .collect();
        let lowest = *bowl
            .iter()
            .min_by(|&&a, &&b| s.elevation(a).total_cmp(&s.elevation(b)))
            .unwrap();
        let low = s.elevation(lowest);
        let ties: Vec<u32> = lattice
            .neighbours(lowest)
            .into_iter()
            .flatten()
            .filter(|&j| f64::from(s.elevation(j) - low) <= DATA_LAKE_FLAT_TOL_M)
            .collect();
        assert!(!ties.is_empty(), "no tie at the apex");
        let c = s.world.islands[TERMINAL].climate;
        let flat_cap = m3s(
            f64::from(s.table.net_evaporation_mm(c)),
            (ties.len() + 1) as f64 * lattice.area_m2(lowest) * 1e-6,
        );
        assert!(lake.inflow_m3s > DATA_LAKE_MAX_BALANCE * flat_cap);
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
    fn reference_eras_ignore_the_map() {
        let lattice = CubeLattice { n: 75 };
        let table = shipped_table();
        let mut mapped = World::new();
        // A map that would drop the humid bowl's lake 300 m and keep the wet dome's.
        mapped.map[0].2 = 200.0;
        mapped.map.push((WET, 1000.0, 100.0));
        let mut unmapped = mapped.clone();
        unmapped.map.clear();
        let pass = |w: &World| run(lattice, w, w, &table, u32::MAX);
        // The test bites: with data lakes, the map changes the result.
        assert_ne!(pass(&mapped), pass(&unmapped));
        // A reference era (no data lakes) ignores it, whatever the source reads.
        mapped.data_lakes = false;
        unmapped.data_lakes = false;
        let reference = pass(&unmapped);
        assert_eq!(pass(&mapped), reference);
        assert!(!reference.lakes.is_empty());
        // Sampled in blocks too.
        let mut prep = HydroPrep::new(lattice, PAST);
        let block = HydroPrep::sample_range(&lattice, &mapped, 0..lattice.len());
        assert!(block.mapped.is_empty());
        prep.add_samples(block);
        while !prep.step(&mapped, &mapped, &table, u32::MAX) {}
        assert_eq!(prep.finish(), reference);
    }

    #[test]
    fn present_day_lakes_are_the_maps() {
        let p = shared_present();
        let h = &p.h;
        let lattice = p.lattice();
        // Each lake is a mapped lake over its own nodes, at its surface, or none.
        let mapped = |v: u32| p.world.mapped(lattice.dir(v));
        let mut want: Vec<u32> = (0..lattice.len())
            .filter(|&v| mapped(v).is_some() && !h.is_ocean(v) && !p.is_ice(v))
            .collect();
        want.sort_unstable();
        let got: Vec<u32> = (0..lattice.len())
            .filter(|&v| h.lake_of(v).is_some())
            .collect();
        assert_eq!(got, want);
        for v in got {
            let l = &h.lakes[h.lake_of(v).unwrap() as usize];
            assert_eq!(l.kind, LakeKind::Data, "{l:?}");
            assert_eq!(l.level_m, mapped(v).unwrap().surface_m, "{l:?}");
        }
        // Three mapped lakes: the humid bowl's, the Caspian-like flat's, the Victoria-like shelf's.
        assert_eq!(h.lakes.len(), 3, "{:?}", h.lakes);
        // The humid lakes overflow; the arid cone's closed basin holds none.
        let humid = &h.lakes[h.lake_of(floor_nodes(p, HUMID, 100.0)[0]).unwrap() as usize];
        assert!(humid.outflow_m3s > 0.0, "{humid:?}");
        assert!(p.lakes_on(ARID).is_empty());
        assert!(shared().lakes_on(TERMINAL).len() == 1 && p.lakes_on(TERMINAL).is_empty());
    }

    /// An ocean planet with a 1,500 km continent at (45° N, 30° E) holding a −500 m inland sea
    /// 300 km across at (43° N, 34° E); `shift` is the source's present-day shift.
    #[derive(Clone)]
    struct InlandSea {
        data_lakes: bool,
        shift: f32,
    }

    impl NodeSource for InlandSea {
        fn node(&self, d: RenderDir) -> NodeSample {
            let km =
                |lat: f64, lon: f64| d.0.angle_between(dir(lat, lon).0) * EARTH_RADIUS_M * 1e-3;
            let e = if km(43.0, 34.0) < 300.0 {
                -500.0
            } else if km(45.0, 30.0) < 1500.0 {
                200.0
            } else {
                -2000.0
            };
            NodeSample {
                elevation_m: e + self.shift,
                ice: 0.0,
            }
        }

        fn data_lakes(&self) -> bool {
            self.data_lakes
        }

        fn present_shift_m(&self, _dir: RenderDir) -> Option<f32> {
            self.data_lakes.then_some(self.shift)
        }
    }

    #[test]
    fn a_strait_joins_a_sea_to_the_ocean_where_the_sea_stands_above_its_sill() {
        use crate::hydrology::straits::Strait;
        let lattice = CubeLattice { n: 75 };
        let table = shipped_table();
        let climate = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        // A seaway from the inland sea south to the ocean, below the lattice.
        let strait = Strait {
            name: "test".into(),
            points: vec![(43.0, 34.0), (38.0, 34.5), (30.0, 35.0)],
            sill_m: -35.0,
            levels: Vec::new(),
        };
        let sea = lattice.nearest(dir(43.0, 34.0));
        let pass = |source: &InlandSea, straits: &[Strait]| {
            let mut prep = HydroPrep::with_tables(lattice, PRESENT, &[], straits);
            while !prep.step(source, &climate, &table, u32::MAX) {}
            prep.finish()
        };
        let today = InlandSea {
            data_lakes: true,
            shift: 0.0,
        };
        // The hazard: without the strait the sea is an inland basin.
        assert!(!pass(&today, &[]).is_ocean(sea));
        let joined = pass(&today, std::slice::from_ref(&strait));
        assert!(joined.is_ocean(sea));
        assert_eq!(joined.lake_tap(dir(43.0, 34.0)), None);
        // A glacial era: the sea stood 120 m lower, so the 35 m sill was dry.
        let glacial = InlandSea {
            data_lakes: true,
            shift: 120.0,
        };
        assert!(!pass(&glacial, std::slice::from_ref(&strait)).is_ocean(sea));
        // A reference era burns nothing.
        let reference = InlandSea {
            data_lakes: false,
            shift: 0.0,
        };
        assert!(!pass(&reference, std::slice::from_ref(&strait)).is_ocean(sea));
    }

    // ---- Rivers Task 11b: today's river courses. ----

    /// The lattice for the plains: a 33 km spacing.
    const PLAIN_LATTICE: CubeLattice = CubeLattice { n: 300 };

    /// An ocean planet with one continent, 0–20° N by 0–20° E, and a river course across it
    /// (`line`, a great circle between its ends). `elevation(lat, lon, km)` is the land's
    /// height, `km` the distance from the course's great circle; nodes off the course stand
    /// `sides_m` higher. Ice north of `ice_north_of`. The shift is known (0 m) even without data
    /// lakes, so tests see the data-lakes gate.
    #[derive(Clone)]
    struct Plain {
        line: [(f64, f64); 2],
        elevation: fn(f64, f64, f64) -> f64,
        sides_m: f64,
        course_nodes: std::collections::HashSet<u32>,
        ice_north_of: f64,
        data_lakes: bool,
        climate: Climate,
        /// A mapped lake north of this latitude, at this surface (m).
        lake: Option<(f64, f32)>,
    }

    impl Plain {
        fn new(line: [(f64, f64); 2], elevation: fn(f64, f64, f64) -> f64) -> Plain {
            let course = TodayRiver {
                name: "course".into(),
                points: line.to_vec(),
            };
            let nodes = crate::hydrology::today_rivers::today_river_nodes(&PLAIN_LATTICE, &course);
            Plain {
                line,
                elevation,
                sides_m: 0.0,
                course_nodes: nodes.into_iter().collect(),
                ice_north_of: 90.0,
                data_lakes: true,
                climate: shipped_table().biome(BiomeClass::TropicalRainforest),
                lake: None,
            }
        }

        fn course(&self) -> TodayRiver {
            TodayRiver {
                name: "course".into(),
                points: self.line.to_vec(),
            }
        }

        fn nodes(&self) -> Vec<u32> {
            crate::hydrology::today_rivers::today_river_nodes(&PLAIN_LATTICE, &self.course())
        }

        fn km_from_line(&self, d: DVec3) -> f64 {
            let [(la, lo), (lb, lob)] = self.line;
            let n = dir(la, lo).0.cross(dir(lb, lob).0).normalize();
            d.normalize().dot(n).abs().asin() * EARTH_RADIUS_M / 1000.0
        }

        /// The pass at `age` with the course (or with no courses).
        fn pass(&self, age: YearsBp, with_course: bool) -> EraHydrology {
            let table = shipped_table();
            let rivers = if with_course {
                vec![self.course()]
            } else {
                Vec::new()
            };
            let mut prep =
                HydroPrep::with_tables(PLAIN_LATTICE, age, &[], &[]).with_today_rivers(rivers);
            while !prep.step(self, self, &table, u32::MAX) {}
            prep.finish()
        }

        /// The burnt nodes and their levels once the burn is done.
        fn burnt(&self, age: YearsBp) -> Vec<(u32, f32)> {
            let table = shipped_table();
            let mut prep = HydroPrep::with_tables(PLAIN_LATTICE, age, &[], &[])
                .with_today_rivers(vec![self.course()]);
            while prep.stage_name() != "route" {
                assert!(!prep.step(self, self, &table, 4096), "the pass ended");
            }
            prep.beds.list().to_vec()
        }

        /// The burnt nodes and their routing elevations once the burn is done.
        fn routes(&self, age: YearsBp) -> Vec<(u32, f32)> {
            let table = shipped_table();
            let mut prep = HydroPrep::with_tables(PLAIN_LATTICE, age, &[], &[])
                .with_today_rivers(vec![self.course()]);
            while prep.stage_name() != "route" {
                assert!(!prep.step(self, self, &table, 4096), "the pass ended");
            }
            prep.beds.routes()
        }

        /// The burnt nodes and their river-graph beds once the burn is done.
        fn burnt_beds(&self, age: YearsBp) -> Vec<(u32, f32)> {
            let table = shipped_table();
            let mut prep = HydroPrep::with_tables(PLAIN_LATTICE, age, &[], &[])
                .with_today_rivers(vec![self.course()]);
            while prep.stage_name() != "route" {
                assert!(!prep.step(self, self, &table, 4096), "the pass ended");
            }
            prep.beds.beds()
        }

        fn ice(&self, node: u32) -> bool {
            is_ice_sample(&self.node(PLAIN_LATTICE.dir(node)))
        }
    }

    impl NodeSource for Plain {
        fn node(&self, d: RenderDir) -> NodeSample {
            let (lat, lon) = geo_to_latlon(render_to_geo(d));
            let land = (0.0..20.0).contains(&lat) && (0.0..20.0).contains(&lon);
            let e = if land {
                let off = !self.course_nodes.contains(&PLAIN_LATTICE.nearest(d));
                (self.elevation)(lat, lon, self.km_from_line(d.0))
                    + if off { self.sides_m } else { 0.0 }
            } else {
                -2000.0
            };
            NodeSample {
                elevation_m: e as f32,
                ice: if lat > self.ice_north_of { 1.0 } else { 0.0 },
            }
        }

        fn data_lakes(&self) -> bool {
            self.data_lakes
        }

        fn present_shift_m(&self, _dir: RenderDir) -> Option<f32> {
            Some(0.0)
        }

        fn mapped(&self, d: RenderDir) -> Option<MappedLake> {
            let (lat, lon) = geo_to_latlon(render_to_geo(d));
            let (north_of, surface_m) = self.lake?;
            (self.data_lakes && lat > north_of && (0.0..20.0).contains(&lon))
                .then_some(MappedLake { id: 1, surface_m })
        }
    }

    impl ClimateSource for Plain {
        fn climate(&self, _dir: RenderDir, _elevation_m: f32, _coast_km: f32) -> Climate {
            self.climate
        }
    }

    /// Whether each land node of the course drains to the next (a node before the sea to any
    /// ocean node); the first node that does not, otherwise.
    fn follows(h: &EraHydrology, nodes: &[u32]) -> Result<(), String> {
        for w in nodes.windows(2) {
            if h.is_ocean(w[0]) {
                return Ok(());
            }
            let k = h
                .rivers
                .index_of(w[0])
                .ok_or(format!("{} carries no river", w[0]))?;
            let n = h.rivers.nodes()[k as usize];
            match n.down {
                Down::Ocean(_) if h.is_ocean(w[1]) => {}
                _ if down_node(&h.rivers, &n) == w[1] => {}
                _ => return Err(format!("{n:?} does not drain to {}", w[1])),
            }
        }
        Err("the course never reaches the sea".into())
    }

    /// A plain tilting south at 5 m a degree, the course across it to the south-east on a 3 m
    /// ridge: down the slope the lattice leaves it.
    fn ridge_plain() -> Plain {
        Plain::new([(18.5, 1.5), (-1.0, 19.0)], |lat, _, km| {
            2.0 + 5.0 * lat + if km < 20.0 { 3.0 } else { 0.0 }
        })
    }

    /// A valley along the course (every other node 30 m higher), with a 20 m hump across it at
    /// 8–10° N: a dam the lattice ponds behind.
    fn hump_valley() -> Plain {
        let mut valley = Plain::new([(18.5, 1.5), (-1.0, 19.0)], |lat, _, _| {
            2.0 + 5.0 * lat + 20.0 * (1.0 - (lat - 9.0).abs()).max(0.0)
        });
        valley.sides_m = 30.0;
        valley
    }

    /// A course south along a 0–1 m coastal plain to the sea (the Mississippi below Baton
    /// Rouge), with an ocean lagoon one node beside it at 5–8° N (Lake Pontchartrain by
    /// LaPlace).
    fn laplace_plain() -> Plain {
        Plain::new([(10.0, 10.0), (-1.0, 10.0)], |lat, lon, _| {
            if (5.0..8.0).contains(&lat) && lon > 10.2 {
                -5.0
            } else {
                0.5 + 0.05 * lat.clamp(0.0, 10.0)
            }
        })
    }

    fn latlon_of(v: u32) -> (f64, f64) {
        geo_to_latlon(render_to_geo(PLAIN_LATTICE.dir(v)))
    }

    /// Task 11d fix round 2 (rule B): within 10 m of the sea a burnt course routed at 0 m, a flat
    /// with no direction that drained to the first ocean it met. Its nodes now fall toward its
    /// end, and only its end meets the sea.
    #[test]
    fn a_burnt_course_meets_the_sea_only_at_its_end() {
        let plain = laplace_plain();
        let nodes = plain.nodes();
        let h = plain.pass(PRESENT, true);
        // The hazard: course nodes beside the lagoon, which is ocean.
        let beside = nodes.iter().filter(|&&v| {
            !h.is_ocean(v)
                && PLAIN_LATTICE
                    .neighbours(v)
                    .into_iter()
                    .flatten()
                    .any(|u| h.is_ocean(u) && latlon_of(u).0 > 4.0)
        });
        assert!(beside.count() >= 3);
        follows(&h, &nodes).unwrap();
        // The burn changes no ocean.
        let bare = plain.pass(PRESENT, false);
        for v in 0..PLAIN_LATTICE.len() {
            assert_eq!(h.is_ocean(v), bare.is_ocean(v), "{v}");
        }
    }

    /// Whether each interior node of `course` (all but its last) is land and drains to the next
    /// (the last may be ocean); the first that does not, otherwise.
    fn runs_to_its_end(h: &EraHydrology, course: &[u32]) -> Result<(), String> {
        for w in course.windows(2) {
            if h.is_ocean(w[0]) {
                return Err(format!("interior node {} is ocean", w[0]));
            }
            let k = h
                .rivers
                .index_of(w[0])
                .ok_or(format!("{} carries no river", w[0]))?;
            let n = h.rivers.nodes()[k as usize];
            match n.down {
                Down::Ocean(j) if j == w[1] => {}
                Down::Ocean(j) => {
                    return Err(format!("{n:?} meets the sea at {j}, not at {}", w[1]));
                }
                _ if down_node(&h.rivers, &n) == w[1] => {}
                _ => return Err(format!("{n:?} does not drain to {}", w[1])),
            }
        }
        Ok(())
    }

    /// An estuary 3 m below the sea 0.6 degrees either side of the course from 5° N down to its
    /// end at 0.2° N, open to the sea (the Yangtze below Zhenjiang).
    fn estuary_plain() -> Plain {
        estuary_plain_to(0.2)
    }

    /// [`estuary_plain`] with the course ending at `end_lat` degrees N.
    fn estuary_plain_to(end_lat: f64) -> Plain {
        Plain::new([(10.0, 10.0), (end_lat, 10.0)], |lat, lon, _| {
            if lat < 5.0 && (lon - 10.0).abs() < 0.6 {
                -3.0
            } else {
                0.5 + 0.05 * lat.clamp(0.0, 10.0)
            }
        })
    }

    /// Task 11d fix round 3 (the course corridor): ETOPO's estuary floors made the sea start up
    /// the estuary, where a river met it 160 km short of its mouth. A course's interior nodes
    /// are never ocean, and the river runs along it to its own end.
    #[test]
    fn an_estuary_course_runs_to_its_own_end() {
        let plain = estuary_plain();
        let nodes = plain.nodes();
        // The hazard: without the course the estuary is ocean up to 5° N.
        let bare = plain.pass(PRESENT, false);
        let sunk = nodes[..nodes.len() - 1]
            .iter()
            .filter(|&&v| bare.is_ocean(v))
            .count();
        assert!(sunk >= 10, "{sunk}");
        let h = plain.pass(PRESENT, true);
        runs_to_its_end(&h, &nodes).unwrap();
        // The rest of the estuary is still the sea.
        let beside: Vec<u32> = (0..PLAIN_LATTICE.len())
            .filter(|&v| bare.is_ocean(v) && !nodes.contains(&v))
            .collect();
        assert!(beside.len() >= 10);
        assert!(beside.iter().all(|&v| h.is_ocean(v)));
    }

    /// Task 11d fix round 3 (the corridor's guard): a course drawn a degree out into the open
    /// sea (Natural Earth's lines sometimes run past the coast) builds no spit: its corridor ends
    /// at its first open-sea node, which then serves as its end. The estuary above still holds.
    #[test]
    fn a_course_run_out_to_sea_builds_no_spit() {
        let plain = estuary_plain_to(-1.0);
        let nodes = plain.nodes();
        let bare = plain.pass(PRESENT, false);
        let h = plain.pass(PRESENT, true);
        // The hazard: the course runs on over the -2000 m sea south of 0° N.
        let lat = |v: u32| latlon_of(v).0;
        let out: Vec<u32> = nodes.iter().copied().filter(|&v| lat(v) < -0.2).collect();
        assert!(out.len() >= 2, "{out:?}");
        // No spit: the sea beyond the coast is the sea, as without the course.
        for v in 0..PLAIN_LATTICE.len() {
            if lat(v) < -0.2 {
                assert_eq!(h.is_ocean(v), bare.is_ocean(v), "{v}");
            }
        }
        // The estuary's course nodes stay land, and the river runs down them to the coast.
        let estuary: Vec<u32> = nodes
            .iter()
            .copied()
            .filter(|&v| (0.3..5.0).contains(&lat(v)))
            .collect();
        assert!(estuary.len() >= 10);
        assert!(estuary.iter().all(|&v| !h.is_ocean(v)));
        follows(&h, &nodes).unwrap();
    }

    /// Task 11d fix round 3 (rule B's junction hole): a node that ends one course but lies inside
    /// another is that course's interior, so the other course never meets the sea there.
    #[test]
    fn a_junction_beside_a_lagoon_keeps_the_through_course() {
        let plain = laplace_plain();
        let main = plain.nodes();
        // A tributary from the west ending on the main course beside the lagoon (5-8° N).
        let tributary = TodayRiver {
            name: "tributary".into(),
            points: vec![(6.5, 6.0), (6.5, 10.0)],
        };
        let joined = crate::hydrology::today_rivers::today_river_nodes(&PLAIN_LATTICE, &tributary);
        let at = *joined.last().unwrap();
        assert!(main.contains(&at), "the tributary ends on the main course");
        let table = shipped_table();
        let mut prep = HydroPrep::with_tables(PLAIN_LATTICE, PRESENT, &[], &[])
            .with_today_rivers(vec![plain.course(), tributary]);
        while !prep.step(&plain, &plain, &table, u32::MAX) {}
        let h = prep.finish();
        // The hazard: the junction is beside the lagoon's ocean.
        assert!(
            PLAIN_LATTICE
                .neighbours(at)
                .into_iter()
                .flatten()
                .any(|u| h.is_ocean(u))
        );
        follows(&h, &main).unwrap();
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(6))]

        /// Task 11d fix round 3: whatever the estuary and lagoons beside it, no interior burnt
        /// node drains to the ocean, and none is ocean.
        #[test]
        fn no_interior_burnt_node_drains_to_the_ocean(
            top in 1.0f64..8.0,
            half in 0.2f64..1.0,
            // Estuary floors (ETOPO's −0.2 to −7 m); below OPEN_SEA_M a course's corridor ends.
            depth in 0.2f64..9.5,
            lagoon in 2.0f64..8.0,
        ) {
            static PARAMS: std::sync::Mutex<(f64, f64, f64, f64)> =
                std::sync::Mutex::new((0.0, 0.0, 0.0, 0.0));
            *PARAMS.lock().unwrap() = (top, half, depth, lagoon);
            let plain = Plain::new([(10.0, 10.0), (0.2, 10.0)], |lat, lon, _| {
                let (top, half, depth, lagoon) = *PARAMS.lock().unwrap();
                // The estuary, and a lagoon beside the course.
                let estuary = lat < top && (lon - 10.0).abs() < half;
                let beside = (lagoon..lagoon + 1.5).contains(&lat) && lon > 10.0 + half + 0.1;
                if estuary || beside {
                    -depth
                } else {
                    0.5 + 0.05 * lat.clamp(0.0, 10.0)
                }
            });
            let nodes = plain.nodes();
            let h = plain.pass(PRESENT, true);
            for &v in &nodes[..nodes.len() - 1] {
                proptest::prop_assert!(!h.is_ocean(v), "interior node {} is ocean", v);
                if let Some(k) = h.rivers.index_of(v) {
                    let n = h.rivers.nodes()[k as usize];
                    if let Down::Ocean(j) = n.down {
                        proptest::prop_assert_eq!(j, nodes[nodes.len() - 1], "{:?}", n);
                    }
                }
            }
        }
    }

    /// Task 11d fix round 2 (rule B, the Hudi's case): a course crossing another's 0 m flat
    /// keeps its own direction past the crossing, though the other's sea is nearer.
    #[test]
    fn a_crossing_course_never_runs_backwards_along_a_flat() {
        let plain = Plain::new([(10.0, 10.0), (-1.0, 10.0)], |lat, _, _| {
            0.5 + 0.05 * lat.clamp(0.0, 10.0)
        });
        let cross = TodayRiver {
            name: "cross".into(),
            points: vec![(5.0, 3.0), (5.0, 20.5)],
        };
        let table = shipped_table();
        let mut prep = HydroPrep::with_tables(PLAIN_LATTICE, PRESENT, &[], &[])
            .with_today_rivers(vec![plain.course(), cross.clone()]);
        while !prep.step(&plain, &plain, &table, u32::MAX) {}
        let h = prep.finish();
        let main: std::collections::HashSet<u32> = plain.nodes().into_iter().collect();
        let nodes = crate::hydrology::today_rivers::today_river_nodes(&PLAIN_LATTICE, &cross);
        let at = nodes
            .iter()
            .rposition(|v| main.contains(v))
            .expect("the courses cross");
        // The hazard: the main course reaches the sea 6 degrees from the crossing, the crossing
        // one 10.5 degrees, so a flat drains the crossing course's east part back west.
        // No node past the crossing drains back to the one before it on its course (the node
        // beside the crossing may join the main course there: a confluence)...
        for w in nodes[at + 1..].windows(2) {
            if let Some(k) = h.rivers.index_of(w[1]) {
                let n = h.rivers.nodes()[k as usize];
                assert_ne!(
                    down_node(&h.rivers, &n),
                    w[0],
                    "{n:?} runs back to {}",
                    w[0]
                );
            }
        }
        // ... and beyond the junction the crossing course follows itself to its own sea.
        follows(&h, &nodes[at + 2..]).unwrap();
    }

    // ---- Task 11d fix round 4: course nodes drain to their course successors. ----

    /// A course south across a 0–0.2 m delta plain to the sea at 0° N (the Mississippi below
    /// Baton Rouge): 0.05 m marsh east of it below 4° N, and beyond the marsh a lagoon 2 m deep,
    /// open to the sea (Barataria). The course's fall of 0.01 m a node stands above the marsh.
    fn marsh_plain() -> Plain {
        Plain::new([(10.0, 10.0), (-1.0, 10.0)], |lat, lon, _| {
            if lat < 4.0 && lon > 10.9 {
                -2.0
            } else if lat < 4.0 && lon > 10.15 {
                0.05
            } else {
                0.02 * lat.clamp(0.0, 10.0)
            }
        })
    }

    /// The course's nodes up to its first ocean node (its end at the coast).
    fn to_the_coast(h: &EraHydrology, nodes: &[u32]) -> Vec<u32> {
        let end = nodes
            .iter()
            .position(|&v| h.is_ocean(v))
            .expect("the course reaches the sea");
        nodes[..=end].to_vec()
    }

    /// Task 11d fix round 4 (the Mississippi at (30.06, −90.70)): a course whose fall toward its
    /// end stands above the marsh beside it still runs to its own end, not across the marsh to
    /// the lagoon's sea.
    #[test]
    fn a_delta_course_beside_a_marsh_reaches_its_own_end() {
        let plain = marsh_plain();
        let nodes = plain.nodes();
        let h = plain.pass(PRESENT, true);
        // The hazard: the lagoon is the sea, and course nodes stand beside the marsh, which
        // lies below their routing elevations.
        let lagoon = (0..PLAIN_LATTICE.len()).filter(|&v| {
            let (lat, lon) = latlon_of(v);
            (1.0..3.0).contains(&lat) && lon > 11.0 && lon < 12.0
        });
        assert!(lagoon.clone().count() >= 3);
        assert!(lagoon.clone().all(|v| h.is_ocean(v)));
        let routes: std::collections::HashMap<u32, f32> =
            plain.routes(PRESENT).into_iter().collect();
        let beside = nodes
            .iter()
            .filter(|&&v| {
                PLAIN_LATTICE.neighbours(v).into_iter().flatten().any(|u| {
                    !routes.contains_key(&u)
                        && plain.node(PLAIN_LATTICE.dir(u)).elevation_m < routes[&v]
                        && plain.node(PLAIN_LATTICE.dir(u)).elevation_m >= 0.0
                })
            })
            .count();
        assert!(beside >= 3, "{beside}");
        runs_to_its_end(&h, &to_the_coast(&h, &nodes)).unwrap();
    }

    /// The marsh plain with an estuary 3 m deep along the course below 2° N (the corridor), a
    /// tributary from the west ending on the course and a distributary leaving it to the south
    /// west: every rule of the course order at once.
    fn delta_plain() -> (Plain, Vec<TodayRiver>) {
        let plain = Plain::new([(10.0, 10.0), (-1.0, 10.0)], |lat, lon, _| {
            if lat < 2.0 && (lon - 10.0).abs() < 0.4 {
                -3.0
            } else if lat < 4.0 && lon > 10.9 {
                -2.0
            } else if lat < 4.0 && lon > 10.15 {
                0.05
            } else {
                0.02 * lat.clamp(0.0, 10.0)
            }
        });
        let tributary = TodayRiver {
            name: "tributary".into(),
            points: vec![(6.5, 5.0), (6.5, 10.0)],
        };
        let distributary = TodayRiver {
            name: "distributary".into(),
            points: vec![(7.0, 10.0), (-1.0, 4.0)],
        };
        let courses = vec![plain.course(), tributary, distributary];
        (plain, courses)
    }

    /// Task 11d fix round 4: the time-boxed pass equals the whole pass, and the pass sampled in
    /// parallel blocks, on a fixture whose courses engage the corridor, shared nodes and the
    /// course order (the shipped-rivers test's Synthetic terrain engages none of them).
    #[test]
    fn the_time_boxed_pass_matches_run_parallel_on_a_delta() {
        let (plain, courses) = delta_plain();
        let table = shipped_table();
        let prep = || {
            HydroPrep::with_tables(PLAIN_LATTICE, PRESENT, &[], &[])
                .with_today_rivers(courses.clone())
        };
        let mut whole = prep();
        while !whole.step(&plain, &plain, &table, u32::MAX) {}
        let units = whole.units_done();
        let whole = whole.finish();
        // The hazards: the estuary is the sea without the courses, and the courses share nodes.
        let bare = plain.pass(PRESENT, false);
        let main = plain.nodes();
        let corridor = main
            .iter()
            .filter(|&&v| bare.is_ocean(v) && !whole.is_ocean(v));
        assert!(corridor.count() >= 3);
        let steps = crate::hydrology::today_rivers::course_steps(&whole, &courses, &plain);
        let count = |s| steps.iter().filter(|e| e.3 == s).count();
        use crate::hydrology::today_rivers::CourseStep;
        assert!(count(CourseStep::Follows) >= 40, "{steps:?}");
        assert_eq!(count(CourseStep::Other), 0, "{steps:?}");
        // Step by step, as the web's time-boxed run does.
        for budget in [1, 7, 64] {
            let mut stepped = prep();
            while !stepped.step(&plain, &plain, &table, budget) {}
            assert_eq!(stepped.units_done(), units, "budget {budget}");
            assert_eq!(stepped.finish(), whole, "budget {budget}");
        }
        // Sampled in parallel blocks, as the native task does, then stepped.
        let len = PLAIN_LATTICE.len();
        let blocks: Vec<NodeBlock> = std::thread::scope(|scope| {
            let handles: Vec<_> = [0, len / 3, len]
                .windows(2)
                .map(|w| {
                    let plain = &plain;
                    let range = w[0]..w[1];
                    scope.spawn(move || HydroPrep::sample_range(&PLAIN_LATTICE, plain, range))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut parallel = prep();
        for block in blocks {
            parallel.add_samples(block);
        }
        while !parallel.step(&plain, &plain, &table, 1 << 18) {}
        assert_eq!(parallel.finish(), whole);
    }

    /// Task 11d fix round 4: over the shipped table, every interior node of every course drains
    /// to its course successor, apart from the counted exceptions [`CourseStep`] names (shared
    /// nodes, nodes in lakes or basins, nodes with too little water, the ocean past a corridor's
    /// end); none drains anywhere else.
    ///
    /// [`CourseStep`]: crate::hydrology::today_rivers::CourseStep
    #[test]
    fn the_shipped_courses_drain_to_their_successors() {
        use crate::hydrology::today_rivers::{CourseStep, course_steps, shipped_today_rivers};
        let lattice = CubeLattice { n: 150 };
        // Two sea levels: the second puts most courses on land.
        for offset in [1450.0, 300.0] {
            let today = Today(Synthetic(offset));
            let h = run_at(lattice, PRESENT, &today, &today, &shipped_table(), u32::MAX);
            let steps = course_steps(&h, shipped_today_rivers(), &today);
            let mut counts = std::collections::BTreeMap::new();
            for e in &steps {
                *counts.entry(e.3).or_insert(0) += 1;
            }
            println!(
                "offset {offset}: {} interior nodes: {counts:?}",
                steps.len()
            );
            // The fixture is not trivial: hundreds of course nodes carry rivers.
            assert!(
                counts.get(&CourseStep::Follows).copied().unwrap_or(0) > 500,
                "{counts:?}"
            );
            let other: Vec<_> = steps
                .iter()
                .filter(|e| e.3 == CourseStep::Other)
                .take(5)
                .collect();
            assert!(other.is_empty(), "offset {offset}: {counts:?}; {other:?}");
        }
    }

    /// Task 11d fix round 4 (the Angara at Listvyanka): a course leaving a deep lake starts at
    /// the lake's surface. Its first nodes lie in the lake, on its bed below the sea, and the
    /// shore node beyond, outside the map, samples the bed as well (a 5′ block mixing it in);
    /// both read the lake's surface, so the land below is not pinned at 0 m. The map's own baked
    /// surface reads the bed here (Baikal's reads −328 m), so a lake of the level table is read
    /// at its table level.
    #[test]
    fn a_course_leaving_a_deep_lake_starts_at_its_surface() {
        use crate::hydrology::levels::LakeLevel;
        use crate::hydrology::today_rivers::course_beds;
        let mut plain = Plain::new([(19.0, 10.0), (-1.0, 10.0)], |lat, _, _| {
            if lat > 17.0 {
                -300.0
            } else if lat > 16.8 {
                -50.0
            } else {
                50.0 + 3.0 * lat
            }
        });
        plain.lake = Some((17.0, -300.0));
        let levels = [LakeLevel {
            name: "deep lake".into(),
            lat: 18.5,
            lon: 10.0,
            level_m: 120.0,
            overview_m: None,
            source: "a fixture".into(),
            note: String::new(),
            endorheic: false,
        }];
        let nodes = plain.nodes();
        let mapped = |v: u32| plain.mapped(PLAIN_LATTICE.dir(v)).is_some();
        let own = |v: u32| plain.node(PLAIN_LATTICE.dir(v)).elevation_m;
        // The hazard: lake nodes on the bed, and a shore node off the map below the sea.
        assert!(nodes.iter().filter(|&&v| mapped(v)).count() >= 2);
        let shore: Vec<u32> = nodes
            .iter()
            .copied()
            .filter(|&v| !mapped(v) && own(v) < 0.0)
            .collect();
        assert!(!shore.is_empty(), "no shore node");
        let beds = course_beds(&PLAIN_LATTICE, &[plain.course()], &plain, &levels);
        let pinned: Vec<_> = beds.iter().filter(|b| b.pinned_at_sea_level).collect();
        assert!(pinned.is_empty(), "pinned at 0 m: {pinned:?}");
        // The land below the lake (47 m and up) keeps its own level as its bed, not 0 m.
        let land: Vec<_> = beds
            .iter()
            .filter(|b| b.elevation_m > 0.0 && !mapped(b.node))
            .collect();
        assert!(land.len() > 25);
        assert!(land.iter().all(|b| b.bed_m > 40.0), "{land:?}");
        // The pass burns the same beds its measurement reports, and the course starts at the
        // lake's table level.
        let table = shipped_table();
        let mut prep = HydroPrep::with_tables(PLAIN_LATTICE, PRESENT, &levels, &[])
            .with_today_rivers(vec![plain.course()]);
        while prep.stage_name() != "route" {
            assert!(!prep.step(&plain, &plain, &table, 4096), "the pass ended");
        }
        let got: Vec<(u32, f32)> = beds.iter().map(|b| (b.node, b.bed_m)).collect();
        assert_eq!(got, prep.beds.beds());
        assert_eq!(prep.beds.list()[0], (nodes[0], 120.0));
        // Without the table, the map's bed-level surface would pin the land below.
        let unlisted = course_beds(&PLAIN_LATTICE, &[plain.course()], &plain, &[]);
        assert!(unlisted.iter().any(|b| b.pinned_at_sea_level));
    }

    #[test]
    fn a_burnt_course_routes_a_plain_river_along_its_line() {
        let plain = ridge_plain();
        let nodes = plain.nodes();
        assert!(nodes.len() > 40, "{}", nodes.len());
        for age in [PRESENT, PAST] {
            // The hazard: down the slope, the lattice leaves the course.
            assert!(follows(&plain.pass(age, false), &nodes).is_err());
            let h = plain.pass(age, true);
            follows(&h, &nodes).unwrap();
            assert!(h.lakes.is_empty(), "{:?}", h.lakes);
        }
    }

    #[test]
    fn a_burnt_course_never_rises_downstream() {
        let valley = hump_valley();
        let nodes = valley.nodes();
        // The hazard: over the hump the course climbs more than the burn's depth above where it
        // has been, so a burn of its own elevations would leave a pit behind the hump.
        let mut lowest = f32::INFINITY;
        let climb = nodes
            .iter()
            .map(|&v| {
                let e = valley.node(PLAIN_LATTICE.dir(v)).elevation_m;
                lowest = lowest.min(e);
                e - lowest
            })
            .fold(0.0, f32::max);
        assert!(f64::from(climb) > TODAY_RIVER_BURN_M, "{climb}");
        let h = valley.pass(PAST, true);
        follows(&h, &nodes).unwrap();
        let beds: Vec<f32> = nodes
            .iter()
            .filter_map(|&v| h.rivers.index_of(v))
            .map(|k| h.rivers.nodes()[k as usize].bed_m)
            .collect();
        assert!(beds.len() > 40);
        assert!(beds.windows(2).all(|w| w[1] <= w[0]), "{beds:?}");
        // Across the hump routing holds its level.
        let burnt = valley.burnt(PAST);
        assert!(burnt.windows(2).all(|w| w[1].1 <= w[0].1), "{burnt:?}");
        let flat = burnt.windows(2).filter(|w| w[1].1 == w[0].1).count();
        assert!(flat >= 4, "{flat} level steps over the hump");
        // Fix round 2: the river graph's beds keep falling across the hump (interpolated by
        // path length to the next lower level), never above the held level.
        let held: Vec<(u32, f32)> = burnt
            .windows(2)
            .filter(|w| valley.node(PLAIN_LATTICE.dir(w[1].0)).elevation_m > w[1].1)
            .map(|w| w[1])
            .collect();
        assert!(held.len() >= 4, "{held:?}");
        let graph_bed = |v: u32| h.rivers.nodes()[h.rivers.index_of(v).unwrap() as usize].bed_m;
        let mut across: Vec<u32> = Vec::new();
        for (k, &v) in nodes.iter().enumerate() {
            if held.iter().any(|&(u, _)| u == v) {
                if across.is_empty() && k > 0 {
                    across.push(nodes[k - 1]);
                }
                across.push(v);
            } else if !across.is_empty() {
                across.push(v);
                break;
            }
        }
        let beds_across: Vec<f32> = across.iter().map(|&v| graph_bed(v)).collect();
        assert!(
            beds_across.windows(2).all(|w| w[1] < w[0]),
            "beds across the hump: {beds_across:?}"
        );
        for (&(v, level), bed) in held.iter().zip(held.iter().map(|&(v, _)| graph_bed(v))) {
            assert!(
                bed <= level,
                "node {v}: bed {bed} above the held level {level}"
            );
        }
    }

    /// Task 11b, fix round 2: a plateau cut by a canyon 2 km wide that the lattice cannot see.
    /// The course runs in the canyon: from node `A`'s centre it bends out to run 15 km north
    /// of its lattice row (0.45 spacings: past the relaxation's interior reach of 2/3 of the
    /// corridor, as Hells Canyon lies 5–7 km off its nodes at n = 900), then back into node `B`
    /// by the coast. The canyon floor falls 0.5 m a km toward `B`; the plateau stands 1,200 m
    /// above it; the sea lies past `B`.
    struct CanyonPlateau {
        polyline: Vec<DVec3>,
        a: DVec3,
        row_normal: DVec3,
        len_km: f64,
        climate: Climate,
    }

    const CANYON_LATTICE: CubeLattice = CubeLattice { n: 300 };
    const CANYON_ROW: u32 = 150;
    const CANYON_COLS: (u32, u32) = (135, 165);

    impl CanyonPlateau {
        fn new() -> Self {
            let l = CANYON_LATTICE;
            let (c0, c1) = CANYON_COLS;
            let at = |c: u32| l.dir(l.node(CubeFace::PosX, c, CANYON_ROW)).0;
            let (a, b) = (at(c0), at(c1));
            let row_normal = a.cross(b).normalize();
            let off = |p: DVec3| (p + row_normal * (15.0 / 6371.0)).normalize();
            let polyline = vec![a, off(at(c0 + 3)), off(at(c1 - 3)), b];
            CanyonPlateau {
                polyline,
                a,
                row_normal,
                len_km: a.angle_between(b) * EARTH_RADIUS_M / 1000.0,
                climate: shipped_table().biome(BiomeClass::TropicalRainforest),
            }
        }

        fn course(&self) -> TodayRiver {
            TodayRiver {
                name: "canyon".into(),
                points: self
                    .polyline
                    .iter()
                    .map(|&d| geo_to_latlon(render_to_geo(RenderDir(d))))
                    .collect(),
            }
        }

        /// Kilometres from `d` to the course's polyline.
        fn km_to_course(&self, d: DVec3) -> f64 {
            let d = d.normalize();
            self.polyline
                .windows(2)
                .map(|w| {
                    let (p, q) = (w[0], w[1]);
                    let n = p.cross(q).normalize();
                    let s = d.dot(n);
                    let foot = d - n * s;
                    let inside = p.cross(foot).dot(n) >= 0.0 && foot.cross(q).dot(n) >= 0.0;
                    if inside {
                        s.abs().asin()
                    } else {
                        d.angle_between(p).min(d.angle_between(q))
                    }
                })
                .fold(f64::INFINITY, f64::min)
                * EARTH_RADIUS_M
                / 1000.0
        }

        fn elevation(&self, d: DVec3) -> f64 {
            let d = d.normalize();
            let t = self.row_normal.cross(self.a);
            let along = d.dot(t).atan2(d.dot(self.a)) * EARTH_RADIUS_M / 1000.0;
            let cross = d.dot(self.row_normal).asin() * EARTH_RADIUS_M / 1000.0;
            if along > self.len_km + 3.0 || along < -100.0 || cross.abs() > 250.0 {
                return -2000.0;
            }
            let floor = 600.0 - 0.5 * along;
            let km = self.km_to_course(d);
            floor + 1200.0 * (km - 1.0).clamp(0.0, 1.0)
        }
    }

    impl NodeSource for CanyonPlateau {
        fn node(&self, dir: RenderDir) -> NodeSample {
            NodeSample {
                elevation_m: self.elevation(dir.0) as f32,
                ice: 0.0,
            }
        }

        fn data_lakes(&self) -> bool {
            true
        }

        fn present_shift_m(&self, _dir: RenderDir) -> Option<f32> {
            Some(0.0)
        }
    }

    impl ClimateSource for CanyonPlateau {
        fn climate(&self, _dir: RenderDir, _elevation_m: f32, _coast_km: f32) -> Climate {
            self.climate
        }
    }

    #[test]
    fn a_burnt_course_is_refined_in_its_own_canyon() {
        use crate::worldgen::REGION_SIZE_M;
        use crate::worldgen::generate::rivers::RiverBuilder;
        let canyon = CanyonPlateau::new();
        let table = shipped_table();
        let mut prep = HydroPrep::with_tables(CANYON_LATTICE, PRESENT, &[], &[])
            .with_today_rivers(vec![canyon.course()]);
        while !prep.step(&canyon, &canyon, &table, u32::MAX) {}
        let h = prep.finish();
        let l = CANYON_LATTICE;
        let (c0, c1) = CANYON_COLS;
        let course: Vec<u32> = (c0..=c1)
            .map(|c| l.node(CubeFace::PosX, c, CANYON_ROW))
            .collect();
        // The lattice's nodes stand on the plateau, 15 km off the canyon.
        let mid = course[course.len() / 2];
        assert!((canyon.km_to_course(l.dir(mid).0) - 15.0).abs() < 1.0);
        assert!(
            canyon.elevation(l.dir(mid).0) > canyon.elevation(l.dir(course[0]).0) + 800.0,
            "{} {} {}",
            canyon.elevation(l.dir(mid).0),
            canyon.elevation(l.dir(course[0]).0),
            canyon.len_km
        );
        let to_sea: Vec<u32> = (c0..=c1 + 1)
            .map(|c| l.node(CubeFace::PosX, c, CANYON_ROW))
            .collect();
        follows(&h, &to_sea).unwrap();
        // The region at the course's middle.
        let floor = |d: RenderDir| canyon.elevation(d.0);
        let key = |d: RenderDir| (d.0 * EARTH_RADIUS_M, 11);
        let mut b = RiverBuilder::new(
            &h,
            &RegionFrame { center: l.dir(mid) },
            REGION_SIZE_M / 2.0,
            SUBPOINT_M,
            RIVER_MIN_DISCHARGE_M3S as f32,
        );
        while !b.step(&floor, &key, 997) {}
        let rivers = b.finish();
        let on_course = |river: u32| {
            let n = &h.rivers.nodes()[river as usize];
            let inner = &course[4..course.len() - 4];
            inner.contains(&n.node) && inner.contains(&down_node(&h.rivers, n))
        };
        let mut checked = 0;
        let (mut worst_km, mut worst_cut) = (0.0f64, 0.0f64);
        for sp in rivers.subpoints().iter().filter(|sp| on_course(sp.river)) {
            worst_km = worst_km.max(canyon.km_to_course(sp.dir));
            worst_cut = worst_cut.max(canyon.elevation(sp.dir) - f64::from(sp.level_m));
            checked += 1;
        }
        assert!(checked > 100, "{checked} sub-points on the course");
        assert!(
            worst_km <= 1.0,
            "the river leaves its canyon by {worst_km:.1} km"
        );
        assert!(
            worst_cut < 50.0,
            "the river runs {worst_cut:.0} m below the ground"
        );
    }

    #[test]
    fn the_burn_steers_routing_only() {
        let valley = hump_valley();
        let nodes = valley.nodes();
        let h = valley.pass(PAST, true);
        // The bed along the course is the burn's bed: the running minimum of the era's own
        // elevations (sloped across the hump, fix round 2), not the burnt routing level 10 m
        // below it. Where the course sets a new minimum it is that minimum exactly.
        let beds = valley.burnt_beds(PAST);
        let mut level = f32::INFINITY;
        let (mut checked, mut setters) = (0, 0);
        for (&v, &(b, bed)) in nodes.iter().zip(&beds) {
            assert_eq!(b, v);
            if h.is_ocean(v) {
                break;
            }
            let e = valley.node(PLAIN_LATTICE.dir(v)).elevation_m;
            let k = h.rivers.index_of(v).expect("a river node");
            assert_eq!(h.rivers.nodes()[k as usize].bed_m, bed, "node {v}");
            if e < level {
                assert_eq!(bed, e, "node {v} sets the minimum");
                setters += 1;
            }
            level = level.min(e);
            assert!(
                bed <= level && bed > level - 20.0,
                "node {v}: {bed} against {level}"
            );
            checked += 1;
        }
        assert!(checked > 40 && setters > 30, "{checked} {setters}");
        // The burnt levels are those running minima, the sea's nodes included.
        let burnt = valley.burnt(PAST);
        assert_eq!(burnt.len(), nodes.len());
        let mut level = f32::INFINITY;
        for (&v, &(b, l)) in nodes.iter().zip(&burnt) {
            level = level.min(valley.node(PLAIN_LATTICE.dir(v)).elevation_m);
            assert_eq!((b, l), (v, level));
        }
    }

    /// Task 11b, fix round 1: a course ending inland leaves a pit at its end, a basin on the
    /// burnt surface. Its lakes must agree with that basin: no level above the basin's spill,
    /// a finite, non-empty footprint.
    #[test]
    fn a_course_ending_inland_keeps_its_lakes_within_their_basins() {
        let mut plain = ridge_plain();
        // The course stops mid-plain on its 3 m ridge, in a semi-arid climate, so the pit at its
        // end is a closed basin.
        plain.line = [(18.5, 1.5), (9.0, 10.0)];
        plain.course_nodes = plain.nodes().into_iter().collect();
        plain.climate = Climate {
            precip_mm: 300.0,
            temp_c: 20.0,
        };
        let nodes = plain.nodes();
        let end = *nodes.last().unwrap();
        for age in [PAST, YearsBp(21_000.0)] {
            let h = plain.pass(age, true);
            // The fixture makes the hazard: a lake at the course's end.
            let lake = h.lake_of(end).map(|l| &h.lakes[l as usize]);
            assert!(lake.is_some(), "no lake at the course's end: {:?}", h.lakes);
            for (k, l) in h.lakes.iter().enumerate() {
                assert!(
                    l.level_m.is_finite() && l.level_m <= l.spill_m,
                    "lake {k}: {l:?}"
                );
                assert!(
                    l.area_km2.is_finite() && l.area_km2 > 0.0,
                    "lake {k}: {l:?}"
                );
                assert!(
                    (0..PLAIN_LATTICE.len()).any(|v| h.lake_of(v) == Some(k as u16)),
                    "lake {k} has no footprint"
                );
            }
        }
    }

    /// The reality check's measurement of the burn (`course_beds`) gives the pass's own levels.
    #[test]
    fn course_beds_measure_the_pass_burn() {
        use crate::hydrology::today_rivers::course_beds;
        let mut valley = hump_valley();
        valley.ice_north_of = 16.0;
        let burnt = valley.burnt_beds(PAST);
        let beds = course_beds(&PLAIN_LATTICE, &[valley.course()], &valley, &[]);
        let got: Vec<(u32, f32)> = beds.iter().map(|b| (b.node, b.bed_m)).collect();
        assert_eq!(got, burnt);
        assert!(
            beds.iter()
                .all(|b| b.course == 0 && b.elevation_m >= b.bed_m)
        );
        // The hump stands above its held level; the sea's nodes come after the coast.
        assert!(beds.iter().any(|b| b.elevation_m - b.bed_m > 10.0));
        assert!(!beds.iter().any(|b| b.pinned_at_sea_level));
    }

    #[test]
    fn a_burnt_coastal_plain_stays_land() {
        // A coastal plain 2 m above the sea, the course due south to the coast.
        let plain = Plain::new([(10.0, 10.0), (-1.0, 10.0)], |_, _, _| 2.0);
        let nodes = plain.nodes();
        for age in [PRESENT, PAST] {
            let (with, without) = (plain.pass(age, true), plain.pass(age, false));
            let ocean = |h: &EraHydrology| {
                (0..PLAIN_LATTICE.len())
                    .filter(|&v| h.is_ocean(v))
                    .collect::<Vec<_>>()
            };
            assert_eq!(ocean(&with), ocean(&without));
            // The burn is engaged: the course carries the river to the sea.
            follows(&with, &nodes).unwrap();
            assert!(nodes.iter().filter(|&&v| !with.is_ocean(v)).count() > 25);
        }
        // Its land nodes keep their 2 m beds (they route at sea level, not 8 m below it).
        let burnt = plain.burnt(PRESENT);
        assert!(burnt.iter().filter(|&&(_, l)| l == 2.0).count() > 25);
    }

    #[test]
    fn ice_nodes_and_reference_eras_are_not_burnt() {
        let mut plain = ridge_plain();
        plain.ice_north_of = 12.0;
        let nodes = plain.nodes();
        let iced = nodes.iter().filter(|&&v| plain.ice(v)).count();
        assert!(iced > 10, "the fixture's course crosses the ice: {iced}");
        let burnt = plain.burnt(PAST);
        assert!(burnt.iter().all(|&(v, _)| !plain.ice(v)), "{burnt:?}");
        assert!(
            burnt.len() + iced >= nodes.len() - 3,
            "{} burnt",
            burnt.len()
        );
        // A reference era burns nothing, though the source knows its shift.
        plain.data_lakes = false;
        assert!(plain.burnt(PAST).is_empty());
        assert_eq!(plain.pass(PAST, true), plain.pass(PAST, false));
    }

    /// [`Synthetic`]'s relief as today's terrain everywhere, so the shipped courses burn.
    struct Today(Synthetic);

    impl NodeSource for Today {
        fn node(&self, dir: RenderDir) -> NodeSample {
            self.0.node(dir)
        }

        fn data_lakes(&self) -> bool {
            true
        }

        fn present_shift_m(&self, _dir: RenderDir) -> Option<f32> {
            Some(0.0)
        }
    }

    impl ClimateSource for Today {
        fn climate(&self, dir: RenderDir, elevation_m: f32, coast_km: f32) -> Climate {
            self.0.climate(dir, elevation_m, coast_km)
        }
    }

    #[test]
    fn the_time_boxed_pass_matches_run_parallel_with_todays_rivers() {
        let lattice = CubeLattice { n: 75 };
        let today = Today(Synthetic(1450.0));
        let table = shipped_table();
        let whole = run_at(lattice, PRESENT, &today, &today, &table, u32::MAX);
        // The shipped courses are burnt: the result differs without them.
        let mut bare = HydroPrep::new(lattice, PRESENT).with_today_rivers(Vec::new());
        while !bare.step(&today, &today, &table, u32::MAX) {}
        assert_ne!(bare.finish(), whole);
        // Step by step, as the web's time-boxed run does.
        let mut prep = HydroPrep::new(lattice, PRESENT);
        let mut today_steps = 0;
        let mut burnt = 0;
        while !prep.step(&today, &today, &table, 7) {
            if prep.stage_name() == "today_rivers" {
                today_steps += 1;
            }
            if prep.stage_name() == "route" {
                burnt = burnt.max(prep.beds.list().len());
            }
        }
        assert!(
            today_steps > 100 && burnt > 500,
            "{today_steps} steps, {burnt} burnt"
        );
        let units = prep.units_done();
        assert_eq!(prep.finish(), whole);
        // Sampled in parallel blocks, as the native task does, then stepped.
        let len = lattice.len();
        let blocks: Vec<NodeBlock> = std::thread::scope(|scope| {
            let handles: Vec<_> = [0, len / 2, len]
                .windows(2)
                .map(|w| {
                    let (lattice, today) = (&lattice, &today);
                    let range = w[0]..w[1];
                    scope.spawn(move || HydroPrep::sample_range(lattice, today, range))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut parallel = HydroPrep::new(lattice, PRESENT);
        for block in blocks {
            parallel.add_samples(block);
        }
        while !parallel.step(&today, &today, &table, 1 << 18) {}
        assert_eq!(parallel.units_done(), units);
        assert_eq!(parallel.finish(), whole);
    }

    /// Task 11g: with ridged steps, the time-boxed pass still matches the native one bit for
    /// bit, and the steps change the routing. The steps are the shipped courses' at n = 75 over
    /// a fine terrain with ridges on the synthetic one.
    #[test]
    fn the_time_boxed_pass_matches_run_parallel_with_ridged_steps() {
        use crate::hydrology::params::JOIN_RIDGE_M;
        use crate::hydrology::ridged::{course_nodes, ridged_steps};
        use crate::hydrology::today_rivers::shipped_today_rivers;
        let lattice = CubeLattice { n: 75 };
        let today = Today(Synthetic(1450.0));
        let table = shipped_table();
        let course = course_nodes(&lattice, shipped_today_rivers());
        let unburnt = |v: u32| f64::from(today.node(lattice.dir(v)).elevation_m);
        let fine = |d: DVec3| {
            f64::from(today.node(RenderDir(d)).elevation_m)
                + 400.0 * ((97.0 * d.x).sin() * (89.0 * d.z).cos()).max(0.0)
        };
        let steps = ridged_steps(&lattice, &course, &unburnt, &fine, JOIN_RIDGE_M);
        assert!(steps.len() > 50, "{}", steps.len());
        let run = |steps: Vec<(u32, u32)>, budget: u32| {
            let mut prep = HydroPrep::new(lattice, PRESENT).with_ridged_steps(steps);
            while !prep.step(&today, &today, &table, budget) {}
            (prep.units_done(), prep.finish())
        };
        let (_, whole) = run(steps.clone(), u32::MAX);
        // The steps are taken: the result differs without them.
        assert_ne!(run(Vec::new(), u32::MAX).1, whole);
        // Step by step, as the web's time-boxed run does.
        let (stepped_units, stepped) = run(steps.clone(), 7);
        assert_eq!(stepped, whole);
        // Sampled in parallel blocks, as the native task does, then stepped.
        let len = lattice.len();
        let blocks: Vec<NodeBlock> = std::thread::scope(|scope| {
            let handles: Vec<_> = [0, len / 2, len]
                .windows(2)
                .map(|w| {
                    let (lattice, today) = (&lattice, &today);
                    let range = w[0]..w[1];
                    scope.spawn(move || HydroPrep::sample_range(lattice, today, range))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let mut parallel = HydroPrep::new(lattice, PRESENT).with_ridged_steps(steps);
        for block in blocks {
            parallel.add_samples(block);
        }
        while !parallel.step(&today, &today, &table, 1 << 18) {}
        assert_eq!(parallel.units_done(), stepped_units);
        assert_eq!(parallel.finish(), whole);
    }

    #[test]
    fn the_pass_is_the_same_for_any_step_size() {
        for age in [PAST, PRESENT] {
            same_for_any_step_size(age);
        }
    }

    fn same_for_any_step_size(age: YearsBp) {
        // 75 is not a power of two; the coast-scaled climate makes the coast phase run.
        let lattice = CubeLattice { n: 75 };
        let mut world = World::new();
        world.coast_scale = Some(400.0);
        let table = shipped_table();
        let whole = run_at(lattice, age, &world, &world, &table, u32::MAX);
        let one = run_at(lattice, age, &world, &world, &table, 1);
        assert_eq!(whole, one);
        let mut prep = HydroPrep::new(lattice, age);
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
        let mut a = HydroPrep::new(lattice, age);
        let mut b = HydroPrep::new(lattice, age);
        while !a.step(&world, &world, &table, u32::MAX) {}
        while !b.step(&world, &world, &table, 3) {}
        assert_eq!(a.units_done(), b.units_done());
        assert!(a.units_done() >= u64::from(len));
    }

    /// Rivers and lakes Task 10, fix round 1: the river graph and the ocean bits are built
    /// over several steps (in one go they took 12.8 ms natively at n = 900), and the stage is
    /// named for the logs.
    #[test]
    fn the_river_graph_is_built_over_several_steps() {
        let lattice = CubeLattice { n: 75 };
        let mut world = World::new();
        world.coast_scale = Some(400.0);
        let table = shipped_table();
        let whole = run_at(lattice, PAST, &world, &world, &table, u32::MAX);
        let mut prep = HydroPrep::new(lattice, PAST);
        let mut building = 0;
        let mut stages = Vec::new();
        while !prep.step(&world, &world, &table, 64) {
            if stages.last() != Some(&prep.stage_name()) {
                stages.push(prep.stage_name());
            }
            building += usize::from(prep.stage_name() == "build");
        }
        assert!(
            building >= 4,
            "{building} steps ended in the build; stages {stages:?}"
        );
        assert_eq!(stages.last(), Some(&"build"), "{stages:?}");
        assert_eq!(prep.stage_name(), "done");
        assert_eq!(prep.finish(), whole);
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

    /// Times today's river courses on the recent lattice in the web's largest steps (8,192
    /// units, `SLICED_MAX_UNITS` in the shell): the longest step and the cost of a unit in each
    /// stage. Not a gate: `cargo test -p timefly_core --lib -- --ignored --nocapture
    /// time_the_today_rivers_stage`.
    #[test]
    #[ignore = "timing"]
    fn time_the_today_rivers_stage() {
        let lattice = CubeLattice { n: HYDRO_RECENT_N };
        let today = Today(Synthetic(1450.0));
        let table = shipped_table();
        let mut prep = HydroPrep::new(lattice, PRESENT);
        prep.add_samples(HydroPrep::sample_range(&lattice, &today, 0..lattice.len()));
        let mut stats: Vec<(&str, std::time::Duration, u64, u32)> = Vec::new();
        loop {
            let (from, before) = (prep.stage_name(), prep.units_done());
            let t = std::time::Instant::now();
            let done = prep.step(&today, &today, &table, 1 << 13);
            let spent = t.elapsed();
            let name = if from == prep.stage_name() {
                from
            } else {
                "(crossing)"
            };
            let units = prep.units_done() - before;
            match stats.iter_mut().find(|s| s.0 == name) {
                Some(s) => {
                    s.1 = s.1.max(spent);
                    s.2 += units;
                    s.3 += 1;
                }
                None => stats.push((name, spent, units, 1)),
            }
            if done || from == "route" && prep.stage_name() != "route" {
                break;
            }
        }
        for (name, longest, units, steps) in stats {
            println!("{name}: {steps} steps, {units} units, longest {longest:?}");
        }
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
        // The margin: at least the incision window plus two lattice steps and the corridor.
        let m = river_tile_margin_m(&CubeLattice { n: 900 });
        let spacing = CubeLattice { n: 900 }.mean_spacing_m();
        assert!(m > 30_000.0 + 2.4 * spacing && m < 150_000.0, "{m}");
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
            // Today's lake map, in the era's frame, only where the terrain is today's.
            let titicaca = dir(-15.8, -69.4);
            let manaus = dir(-3.1, -60.0);
            assert_eq!(source.mapped_lake(titicaca), source.data_lakes());
            assert!(!source.mapped_lake(manaus));
            if source.data_lakes() {
                // Moved as the node is from the present overview.
                let (lat, lon) = latlon(titicaca);
                let present = f64::from(tiles.sample_overview(lat, lon).unwrap().0);
                let shift = f64::from(source.node(titicaca).elevation_m) - present;
                let want = f64::from(shipped_lake_map().level_at(-15.8, -69.4).unwrap()) + shift;
                let got = f64::from(source.mapped(titicaca).unwrap().surface_m);
                assert!((got - want).abs() < 1e-3, "{age}: {got} vs {want}");
                if mode == SourceMode::PresentFrame {
                    assert!((shift + anomaly).abs() < 1e-3, "{shift} vs {anomaly}");
                }
            } else {
                assert_eq!(source.mapped(titicaca), None);
            }
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
