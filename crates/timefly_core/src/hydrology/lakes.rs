//! Discharge and the lake water balance (rivers and lakes design §4.1, steps 3 and 4).
//!
//! The rules, which the golden fixture (`tests/fixtures/hydro_golden.json`) and the Python
//! reference (Task 11) pin. `e` is a node's elevation, `c` its evaporation capacity (net
//! evaporation × area, m³/s; 0 on ice), "tol" is `DATA_LAKE_FLAT_TOL_M`. "Mapped" nodes are
//! those today's lake map holds ([`super::lakemap`]), each with its lake's id and baked
//! surface; only with data lakes (the terrain is today's): without, no node is mapped and the
//! map rules below never apply.
//!
//! Present mode (data lakes at the present-day snapshot, round 2 of Task 8b): the map decides
//! every lake. No data flats are looked for; table lakes are as below; every other basin makes
//! no lake: Open by the balance, it passes Q_in through its spill (as a breached basin); else
//! it ends at its lowest node (its only sink, a dry one; ties: the lowest id), which keeps the
//! water. The map's lakes then come after the walk ([`super::today`]'s rules).
//!
//! ```text
//! Data flats (only if data_lakes), before discharge, after routing:
//!   eligible: non-ocean, non-ice, in no depression basin.
//!   Scan node ids ascending; each eligible node not yet in a component seeds one. Grow it by BFS
//!     (FIFO, neighbour order) over eligible free neighbours, taking a node only if the
//!     component's max e − min e stays ≤ tol. Every grown node is taken, kept or not.
//!   Keep a component of ≥ DATA_LAKE_FLAT_MIN_NODES nodes only if at most
//!     DATA_LAKE_FLAT_MAX_OUTLETS of its nodes have a strictly lower neighbour off it (a lake
//!     drains through a narrow outlet; an F16 stair or a delta has a long lower edge).
//!   Split it by routing: a node's outlet is where following its receivers first leaves the
//!     component (a node with no receiver is its own outlet). Group outlets by 8-adjacency
//!     among themselves (ascending id); a group's sub-flat is every node whose outlet is in it.
//!   Keep sub-flats of ≥ DATA_LAKE_FLAT_MIN_NODES nodes. Each becomes a basin appended after
//!     routing's, in scan order: its nodes, spill_m = its minimum e, exit = its outlet first in
//!     pop order, rim = the exit's receiver. Its other outlets are redirected to the exit.
//! Lake level table (only if data_lakes), after the flats: each table point's nearest lattice
//!   node; if that node is in a depression basin with a non-ice node below the point's level,
//!   the basin takes the level (the first point in table order wins; a later point within tol
//!   of it is silent). Unplaced, for the shell to warn: a point in no depression, one whose
//!   basin has no non-ice node below its level, or one more than tol from the basin's level.
//!   The level is the present one; the era's elevation change is not added.
//! Discharge: own runoff per land node, then nodes in reverse pop order. A node outside every
//!   basin adds its q to its receiver (none into the ocean). A basin node other than the exit
//!   keeps its q (own runoff + inflow from outside the basin). At the exit the basin resolves:
//!   Q_in = Σ q over its nodes, ascending node id, in f64. Lakes are numbered in the order their
//!   exits resolve (reverse pop order), which lake_tap's tie-break uses.
//! A depression basin with a table level L: a lake at L, kept even above the spill (the table
//!   is trusted over the data's 10 km spill); footprint = non-ice nodes with e < L. It drains
//!   by its surplus, whatever L is against the spill: Open if Q_in > Σc over the nodes below L
//!   (outflow = Q_in − Σc, evaporation = Σc, and the exit then joins the footprint as its
//!   outlet unless ice), else Terminal (outflow 0, evaporation = Q_in). It overrides every rule
//!   below.
//! A flat basin: if Q_in ≥ DATA_LAKE_MIN_BALANCE × Σc and at least DATA_LAKE_MIN_NODES of its
//!   nodes are mapped: Data lake at level = spill = min e, footprint = all its nodes, outflow =
//!   max(0, Q_in − Σc), evaporation = Q_in − outflow. Otherwise no lake (it passes Q_in on).
//!   Either way it routes as a spilling basin (below).
//! A depression basin:
//!   - Data (only if data_lakes): F = the connected non-ice basin nodes within tol of the basin
//!     minimum (ties: lowest id), grown by BFS from it. If |F| ≥ DATA_LAKE_MIN_NODES and
//!     DATA_LAKE_MIN_BALANCE × Σ_F c ≤ Q_in ≤ DATA_LAKE_MAX_BALANCE × Σ_F c: level = median e of F
//!     (mean of the middle two for an even count); candidate = F ∪ {e < level}; it is a lake only
//!     if at least DATA_LAKE_MIN_NODES candidate nodes are mapped (else the balance decides):
//!     footprint = candidate minus ice; outflow = max(0, Q_in − Σ_footprint c) if level ≥
//!     spill − tol, else 0.
//!   - Otherwise the balance: group basin nodes by distinct e, ascending; C_g = Σc through g.
//!     C_last < Q_in: Open. The main mapped lake is the map id holding the most basin nodes
//!       (any node, ice included; ties: the lowest id); S = the highest surface among its nodes.
//!       - Data lakes and no mapped node: no lake (breached: the lattice dammed a valley that
//!         holds no lake today; Q_in passes through the spill).
//!       - spill > S + OPEN_LAKE_MAP_TOL_M: Data lake at level = S, footprint = the main mapped
//!         lake's non-ice nodes (round 2: never every basin node below S, which flooded plains
//!         between lakes), evaporation = Σ_footprint c, outflow = Q_in − evaporation (the real
//!         outlet is a gorge below the lattice; the exit then joins the footprint as its outlet
//!         unless ice, while the evaporation stays that of the mapped nodes).
//!       - Otherwise: level = spill, evaporation = C_last, outflow = Q_in − C_last, footprint =
//!         all non-ice basin nodes; no lake if spill − min e < MIN_LAKE_DEPTH_M.
//!     Else Terminal: g = first group with C_g ≥ Q_in, level = e_g + (Q_in − C_{g−1}) /
//!       (C_g − C_{g−1}) × (e_{g+1} − e_g) (0 when that step is 0; e_{last+1} = spill),
//!       evaporation = Q_in, outflow = 0; sinks = nodes with e ≤ e_g; footprint = sinks minus
//!       ice; no lake if the footprint is empty, or if level − min e < MIN_LAKE_DEPTH_M and
//!       Q_in < RIVER_MIN_DISCHARGE_M3S (a playa).
//! Routing inside a basin, always local: sources are, for a basin that passes water on (Open,
//!   Data lake with outflow, an Open table lake, a flat, no lake), its footprint plus its exit (a
//!   lake with outflow adds the exit to its footprint unless ice); for any other basin
//!   (Terminal, a playa, Data lake without outflow, a Terminal table lake), its sinks (Data and
//!   table lakes: the nodes in the footprint or with e below the level, ice included). BFS from the sources over the
//!   basin's nodes. Every other node routes to the neighbour one step closer with the lowest e
//!   (ties: neighbour order); re-accumulate q and area in descending distance, ties ascending id.
//!   A basin that passes water on then adds each other source's q and area to the exit
//!   (ascending id) and redirects it to the exit; the outflow (the lake's, or q[exit] with no
//!   lake) and the exit's area go to the exit's receiver (the rim, or a lower neighbour outside),
//!   unless ocean. Sinks of any other basin keep their water and have no receiver.
//! Shore steps: 1 on a footprint node with a neighbour that is off the footprint (in the basin
//!   or outside it); a missing neighbour (a cube corner's duplicate, a grid's edge) does not
//!   count. Then BFS inward.
//! ```

use std::collections::VecDeque;

use super::params::{
    DATA_LAKE_FLAT_MAX_OUTLETS, DATA_LAKE_FLAT_MIN_NODES, DATA_LAKE_FLAT_TOL_M,
    DATA_LAKE_MAX_BALANCE, DATA_LAKE_MIN_BALANCE, DATA_LAKE_MIN_NODES, MIN_LAKE_DEPTH_M,
    OPEN_LAKE_MAP_TOL_M, RIVER_MIN_DISCHARGE_M3S,
};
use super::today::Today;
use super::{HydroGraph, MappedLake, NO_BASIN, RECEIVER_EXIT, RECEIVER_NONE, Routed};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LakeKind {
    /// Fills to its spill and overflows.
    Open,
    /// Evaporates all its inflow below the spill.
    Terminal,
    /// Holds the level of a lake surface in the present-day data.
    Data,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lake {
    pub level_m: f32,
    /// The basin's filled level: where it would overflow.
    pub spill_m: f32,
    pub kind: LakeKind,
    pub inflow_m3s: f64,
    pub evaporation_m3s: f64,
    pub outflow_m3s: f64,
    /// The footprint's area.
    pub area_km2: f64,
    /// The basin's exit node; an outflow leaves over the basin's rim.
    pub exit: u32,
}

/// A footprint node: its lake and its distance from the shore in nodes (1 on the shore).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct LakeNode {
    pub node: u32,
    pub lake: u16,
    pub shore_steps: u16,
}

/// The water balance of a basin by evaporation capacity alone.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Balance {
    /// `Open` or `Terminal`.
    pub kind: LakeKind,
    pub level_m: f64,
    pub evaporation_m3s: f64,
    pub outflow_m3s: f64,
    /// The elevation of the highest group the water reaches: nodes at or below it are wet.
    /// The spill for an open basin.
    pub wet_through_m: f32,
}

/// The balance of a basin whose nodes have elevations `e` and capacities `capacity` (m³/s),
/// spilling at `spill_m`, receiving `q_in` (m³/s). The groups are the distinct elevations.
pub(crate) fn balance(e: &[f32], capacity: &[f64], spill_m: f32, q_in: f64) -> Balance {
    let mut by_e: Vec<usize> = (0..e.len()).collect();
    by_e.sort_by(|&a, &b| e[a].total_cmp(&e[b]).then(a.cmp(&b)));
    // (elevation, cumulative capacity through the group).
    let mut groups: Vec<(f32, f64)> = Vec::new();
    let mut total = 0.0;
    for &k in &by_e {
        total += capacity[k];
        match groups.last_mut() {
            Some(g) if g.0 == e[k] => g.1 = total,
            _ => groups.push((e[k], total)),
        }
    }
    if total < q_in || groups.is_empty() {
        return Balance {
            kind: LakeKind::Open,
            level_m: f64::from(spill_m),
            evaporation_m3s: total,
            outflow_m3s: q_in - total,
            wet_through_m: spill_m,
        };
    }
    let g = groups.partition_point(|&(_, c)| c < q_in);
    let below = if g == 0 { 0.0 } else { groups[g - 1].1 };
    let step = groups[g].1 - below;
    let frac = if step > 0.0 {
        (q_in - below) / step
    } else {
        0.0
    };
    let next = groups.get(g + 1).map_or(spill_m, |&(e, _)| e);
    let e_g = f64::from(groups[g].0);
    Balance {
        kind: LakeKind::Terminal,
        level_m: e_g + frac * (f64::from(next) - e_g),
        evaporation_m3s: q_in,
        outflow_m3s: 0.0,
        wet_through_m: groups[g].0,
    }
}

/// How a basin's water leaves it.
enum Outcome {
    /// Routing's tree into the exit, then the rim. `lake` is `None` for a basin too shallow for
    /// one, which passes everything through.
    Spill { lake: Option<(Lake, Vec<bool>)> },
    /// The sinks keep their water.
    Sink {
        lake: Option<(Lake, Vec<bool>)>,
        sinks: Vec<bool>,
        evaporation_m3s: f64,
    },
}

/// Discharge (m³/s) and drainage area (km²) per node, resumable: [`Accumulation::init_step`]
/// until it returns true, then [`Accumulation::walk_step`] until it returns true.
pub(crate) struct Accumulation {
    pub q: Vec<f32>,
    pub area_km2: Vec<f32>,
    pub lakes: Vec<Lake>,
    /// Footprint nodes, sorted by node once the walk is done.
    pub lake_nodes: Vec<LakeNode>,
    /// All the water evaporated in basins, lakes or not (a check on conservation).
    pub evaporated_m3s: f64,
    next_init: u32,
    next_walk: u32,
    /// `routed.basins[first_flat..]` are the data's flats ([`Flats`]), not depressions.
    first_flat: u32,
    /// `(basin, level)` from the lake level table, sorted by basin ([`place_levels`]).
    levels: Vec<(u32, f32)>,
    /// The nodes a present-day mapped lake holds, sorted by node ([`Accumulation::set_mapped`]).
    /// Empty without data lakes.
    mapped: Vec<(u32, MappedLake)>,
    /// The present-day snapshot with today's terrain: the map decides every lake
    /// ([`super::today`]).
    present: bool,
    /// Present mode: each land node's own runoff and area, kept for the overlay.
    own: Option<(Vec<f32>, Vec<f32>)>,
    /// Present mode: the overlay, once the walk is done.
    today: Option<Box<Today>>,
}

/// Places lake level table points `(nearest node, level)`: each point whose node lies in a
/// depression basin with a non-ice node below the level gives that basin its level (the first
/// point wins). Returns the `(basin, level)` pairs, sorted, and the indices of the points that
/// are unplaced: in no depression, with no non-ice basin node below the level, or giving a
/// basin that already has a level another one more than `DATA_LAKE_FLAT_TOL_M` apart.
pub(crate) fn place_levels(
    routed: &Routed,
    first_flat: u32,
    ice: &impl Fn(u32) -> bool,
    points: &[(u32, f32)],
) -> (Vec<(u32, f32)>, Vec<usize>) {
    let mut placed: Vec<(u32, f32)> = Vec::new();
    let mut unplaced = Vec::new();
    for (k, &(node, level)) in points.iter().enumerate() {
        let b = routed.basin_of[node as usize];
        if b == NO_BASIN || b >= first_flat {
            unplaced.push(k);
            continue;
        }
        let wet = routed.basins[b as usize]
            .nodes
            .iter()
            .any(|&v| !ice(v) && routed.elevation_m[v as usize] < level);
        match placed.iter().find(|&&(p, _)| p == b) {
            _ if !wet => unplaced.push(k),
            Some(&(_, other)) => {
                // One water body at one level (Michigan and Huron) is silent; another level is not.
                if f64::from((other - level).abs()) > DATA_LAKE_FLAT_TOL_M {
                    unplaced.push(k);
                }
            }
            None => placed.push((b, level)),
        }
    }
    placed.sort_by_key(|&(b, _)| b);
    (placed, unplaced)
}

/// The data's flats outside every depression, found resumably and registered as basins after
/// routing's own ([`Flats::register`]).
pub(crate) struct Flats {
    /// The next node id to scan.
    next: u32,
    /// One bit per node: already in a component.
    taken: Vec<u64>,
    /// One bit per node: an outlet of a kept group.
    outlet: Vec<u64>,
    queue: VecDeque<u32>,
    /// Kept groups: each one's nodes, sorted, and its outlets.
    groups: Vec<(Vec<u32>, Vec<u32>)>,
    /// `(outlet, group)`, sorted by node once the scan is done.
    lookup: Vec<(u32, u32)>,
    /// Each group's exit (its outlet popped first), `u32::MAX` until found.
    exits: Vec<u32>,
    /// The next position in `routed.order` for the exit search.
    pos: u32,
    found: usize,
    scanned: bool,
}

fn bit_of(bits: &[u64], i: u32) -> bool {
    bits[(i / 64) as usize] >> (i % 64) & 1 == 1
}

fn set_bit(bits: &mut [u64], i: u32) {
    bits[(i / 64) as usize] |= 1 << (i % 64);
}

/// Node `v`'s receiver under routing's own rule (`RECEIVER_EXIT` never occurs outside basins).
fn plain_receiver(g: &impl HydroGraph, routed: &Routed, v: u32) -> Option<u32> {
    match routed.receiver[v as usize] {
        RECEIVER_NONE => None,
        k => g.neighbours(v)[usize::from(k)],
    }
}

impl Flats {
    pub fn new(len: u32) -> Self {
        let words = len.div_ceil(64) as usize;
        Self {
            next: 0,
            taken: vec![0; words],
            outlet: vec![0; words],
            queue: VecDeque::new(),
            groups: Vec::new(),
            lookup: Vec::new(),
            exits: Vec::new(),
            pos: 0,
            found: 0,
            scanned: false,
        }
    }

    pub fn working_bytes(&self) -> usize {
        8 * (self.taken.capacity() + self.outlet.capacity() + self.lookup.capacity())
            + 4 * (self.queue.capacity()
                + self.exits.capacity()
                + self
                    .groups
                    .iter()
                    .map(|(n, o)| n.capacity() + o.capacity())
                    .sum::<usize>())
    }

    /// Scans for flats in ascending node id, then finds each group's exit. Returns `(done,
    /// units used)`.
    pub fn step(
        &mut self,
        g: &impl HydroGraph,
        routed: &Routed,
        ice: &impl Fn(u32) -> bool,
        budget: u32,
    ) -> (bool, u32) {
        let len = g.len();
        let mut used = 0;
        let free = |taken: &[u64], i: u32| {
            !bit_of(taken, i)
                && !routed.ocean[i as usize]
                && routed.basin_of[i as usize] == NO_BASIN
                && !ice(i)
        };
        let e = |i: u32| routed.elevation_m[i as usize];
        while used < budget {
            used += 1;
            if self.next < len {
                let seed = self.next;
                self.next += 1;
                if !free(&self.taken, seed) {
                    continue;
                }
                // Grow while the component's range stays within the tolerance.
                let (mut lo, mut hi) = (e(seed), e(seed));
                set_bit(&mut self.taken, seed);
                let mut component = vec![seed];
                self.queue.push_back(seed);
                while let Some(v) = self.queue.pop_front() {
                    used += 1;
                    for u in g.neighbours(v).into_iter().flatten() {
                        if !free(&self.taken, u) {
                            continue;
                        }
                        let (l, h) = (lo.min(e(u)), hi.max(e(u)));
                        if f64::from(h) - f64::from(l) <= DATA_LAKE_FLAT_TOL_M {
                            (lo, hi) = (l, h);
                            set_bit(&mut self.taken, u);
                            component.push(u);
                            self.queue.push_back(u);
                        }
                    }
                }
                if component.len() >= DATA_LAKE_FLAT_MIN_NODES as usize {
                    component.sort_unstable();
                    used = used.saturating_add(component.len() as u32);
                    self.keep(g, routed, component);
                }
                continue;
            }
            if !self.scanned {
                self.scanned = true;
                self.taken = Vec::new();
                self.queue = VecDeque::new();
                self.lookup.sort_unstable();
                self.exits = vec![u32::MAX; self.groups.len()];
                continue;
            }
            if self.found == self.groups.len() || self.pos as usize == routed.order.len() {
                return (true, used);
            }
            let v = routed.order[self.pos as usize];
            self.pos += 1;
            if bit_of(&self.outlet, v) {
                let k = self
                    .lookup
                    .binary_search_by_key(&v, |&(n, _)| n)
                    .expect("an outlet is listed");
                let f = self.lookup[k].1 as usize;
                if self.exits[f] == u32::MAX {
                    self.exits[f] = v;
                    self.found += 1;
                }
            }
        }
        (false, used)
    }

    /// A component big enough: keep it if its downhill boundary is small, split by the outlet
    /// each node's routing reaches, outlets grouped by adjacency.
    fn keep(&mut self, g: &impl HydroGraph, routed: &Routed, flat: Vec<u32>) {
        let e = |i: u32| routed.elevation_m[i as usize];
        let local = |v: u32| flat.binary_search(&v).ok();
        let downhill = flat
            .iter()
            .filter(|&&v| {
                g.neighbours(v)
                    .into_iter()
                    .flatten()
                    .any(|u| local(u).is_none() && e(u) < e(v))
            })
            .count();
        if downhill > DATA_LAKE_FLAT_MAX_OUTLETS as usize {
            return;
        }
        // Each node's outlet: follow routing's receivers until one leaves the flat.
        let m = flat.len();
        let next: Vec<Option<usize>> = flat
            .iter()
            .map(|&v| plain_receiver(g, routed, v).and_then(local))
            .collect();
        let mut root = vec![usize::MAX; m];
        let mut path = Vec::new();
        for k in 0..m {
            let mut at = k;
            while root[at] == usize::MAX {
                match next[at] {
                    Some(n) => {
                        path.push(at);
                        at = n;
                    }
                    None => root[at] = at,
                }
            }
            let r = root[at];
            for p in path.drain(..) {
                root[p] = r;
            }
        }
        // Outlets (ascending id) grouped by 8-adjacency, in order of their lowest outlet.
        let outlets: Vec<usize> = (0..m).filter(|&k| next[k].is_none()).collect();
        let mut group_of = vec![usize::MAX; m];
        let mut groups = 0;
        for &o in &outlets {
            if group_of[o] != usize::MAX {
                continue;
            }
            group_of[o] = groups;
            let mut queue = VecDeque::from([o]);
            while let Some(k) = queue.pop_front() {
                for j in g
                    .neighbours(flat[k])
                    .into_iter()
                    .flatten()
                    .filter_map(local)
                {
                    if next[j].is_none() && group_of[j] == usize::MAX {
                        group_of[j] = groups;
                        queue.push_back(j);
                    }
                }
            }
            groups += 1;
        }
        for gr in 0..groups {
            let nodes: Vec<u32> = (0..m)
                .filter(|&k| group_of[root[k]] == gr)
                .map(|k| flat[k])
                .collect();
            if nodes.len() < DATA_LAKE_FLAT_MIN_NODES as usize {
                continue;
            }
            let id = self.groups.len() as u32;
            let outs: Vec<u32> = outlets
                .iter()
                .filter(|&&o| group_of[o] == gr)
                .map(|&o| flat[o])
                .collect();
            for &o in &outs {
                set_bit(&mut self.outlet, o);
                self.lookup.push((o, id));
            }
            self.groups.push((nodes, outs));
        }
    }

    /// Appends each kept group to `routed.basins`, with its lowest elevation as its `spill_m`
    /// and its exit's receiver as its rim, and redirects its other outlets to the exit (they
    /// are adjacent outlets of one channel). Returns the index of the first flat basin.
    pub fn register(self, g: &impl HydroGraph, routed: &mut Routed) -> u32 {
        let first = routed.basins.len() as u32;
        for (f, (nodes, outlets)) in self.groups.into_iter().enumerate() {
            let exit = self.exits[f];
            assert_ne!(exit, u32::MAX, "every flat node is popped");
            let b = first + f as u32;
            let rim = plain_receiver(g, routed, exit).unwrap_or(exit);
            let level = nodes
                .iter()
                .map(|&v| routed.elevation_m[v as usize])
                .fold(f32::INFINITY, f32::min);
            for &v in &nodes {
                routed.basin_of[v as usize] = b;
            }
            for &o in outlets.iter().filter(|&&o| o != exit) {
                routed.receiver[o as usize] = RECEIVER_EXIT;
            }
            routed.basins.push(super::Basin {
                nodes,
                spill_m: level,
                exit,
                rim,
            });
        }
        first
    }
}

impl Accumulation {
    /// `first_flat` is the index of the first basin [`Flats::register`] added, or the basin
    /// count when there are none.
    pub fn new(len: u32, first_flat: u32) -> Self {
        Self {
            first_flat,
            levels: Vec::new(),
            mapped: Vec::new(),
            present: false,
            own: None,
            today: None,
            q: vec![0.0; len as usize],
            area_km2: vec![0.0; len as usize],
            lakes: Vec::new(),
            lake_nodes: Vec::new(),
            evaporated_m3s: 0.0,
            next_init: 0,
            next_walk: 0,
        }
    }

    /// Sets the table's basin levels ([`place_levels`]).
    pub fn set_levels(&mut self, levels: Vec<(u32, f32)>) {
        self.levels = levels;
    }

    /// Sets the nodes the present-day lake map holds, sorted by node (only used with data
    /// lakes).
    pub fn set_mapped(&mut self, mapped: Vec<(u32, MappedLake)>) {
        debug_assert!(mapped.windows(2).all(|w| w[0].0 < w[1].0));
        self.mapped = mapped;
    }

    /// Present mode, with data lakes at the present-day snapshot: the walk makes no lake but
    /// the table's, and the map's lakes come after it ([`super::today`]).
    pub fn set_present(&mut self, present: bool) {
        self.present = present;
    }

    /// The mapped lake at `node`, if the map holds it.
    fn mapped_at(&self, node: u32) -> Option<MappedLake> {
        self.mapped
            .binary_search_by_key(&node, |&(n, _)| n)
            .ok()
            .map(|k| self.mapped[k].1)
    }

    pub fn working_bytes(&self) -> usize {
        12 * self.mapped.capacity()
            + self
                .own
                .as_ref()
                .map_or(0, |(q, a)| 4 * (q.capacity() + a.capacity()))
            + self.today.as_ref().map_or(0, |t| t.working_bytes())
            + 4 * (self.q.capacity() + self.area_km2.capacity())
            + self.lakes.capacity() * std::mem::size_of::<Lake>()
            + self.lake_nodes.capacity() * std::mem::size_of::<LakeNode>()
    }

    /// Sets each land node's own runoff (`own(i, area m²)`, m³/s) and area. Returns
    /// `(done, units used)`.
    pub fn init_step(
        &mut self,
        g: &impl HydroGraph,
        routed: &Routed,
        mut own: impl FnMut(u32, f64) -> f32,
        budget: u32,
    ) -> (bool, u32) {
        let mut used = 0;
        while used < budget && self.next_init < g.len() {
            let i = self.next_init;
            if !routed.ocean[i as usize] {
                let area = g.area_m2(i);
                self.q[i as usize] = own(i, area);
                self.area_km2[i as usize] = (area * 1e-6) as f32;
            }
            self.next_init += 1;
            used += 1;
        }
        let done = self.next_init == g.len();
        if done && self.present && self.own.is_none() {
            self.own = Some((self.q.clone(), self.area_km2.clone()));
        }
        (done, used)
    }

    /// Walks the nodes upstream first, resolving each basin at its exit. `capacity(i, e, area)`
    /// is a non-ice node's evaporation capacity (m³/s) at its elevation `e` over its area (m²);
    /// `ice(i)` says a node is an ice node. Rewrites
    /// `routed.receiver` inside terminal basins. Returns `(done, units used)`.
    pub fn walk_step(
        &mut self,
        g: &impl HydroGraph,
        routed: &mut Routed,
        ice: &impl Fn(u32) -> bool,
        mut capacity: impl FnMut(u32, f32, f64) -> f64,
        data_lakes: bool,
        budget: u32,
    ) -> (bool, u32) {
        let mut used = 0;
        let len = routed.order.len() as u32;
        while used < budget && self.next_walk < len {
            let i = routed.order[(len - 1 - self.next_walk) as usize];
            self.next_walk += 1;
            let b = routed.basin_of[i as usize];
            if b == NO_BASIN {
                used += 1;
                let to = match routed.receiver[i as usize] {
                    RECEIVER_NONE => None,
                    k => g.neighbours(i)[usize::from(k)],
                };
                if let Some(j) = to.filter(|&j| !routed.ocean[j as usize]) {
                    self.q[j as usize] += self.q[i as usize];
                    self.area_km2[j as usize] += self.area_km2[i as usize];
                }
            } else if routed.basins[b as usize].exit == i {
                used += self.resolve(g, routed, b as usize, ice, &mut capacity, data_lakes);
            } else {
                used += 1;
            }
        }
        let walked = self.next_walk == len;
        if walked {
            self.lake_nodes.sort_unstable_by_key(|n| n.node);
        }
        if !walked || !(self.present && data_lakes) {
            return (walked, used);
        }
        // Present mode: the map's lakes, after the walk.
        let mut today = match self.today.take() {
            Some(t) => t,
            None => {
                let (q, area) = self.own.take().expect("init_step kept the own runoff");
                Box::new(Today::new(q, area))
            }
        };
        let mapped = std::mem::take(&mut self.mapped);
        let (done, u) = today.step(
            self,
            &mapped,
            g,
            routed,
            ice,
            &mut capacity,
            budget.saturating_sub(used).max(1),
        );
        self.mapped = mapped;
        self.today = Some(today);
        (done, used.saturating_add(u))
    }

    fn resolve(
        &mut self,
        g: &impl HydroGraph,
        routed: &mut Routed,
        b: usize,
        ice: &impl Fn(u32) -> bool,
        capacity: &mut impl FnMut(u32, f32, f64) -> f64,
        data_lakes: bool,
    ) -> u32 {
        let Routed {
            elevation_m,
            ocean,
            receiver,
            basins,
            ..
        } = routed;
        let basin = &basins[b];
        let nodes = &basin.nodes;
        let m = nodes.len();
        let local = |v: u32| nodes.binary_search(&v).ok();
        let exit_k = local(basin.exit).expect("the exit is a basin node");
        let e: Vec<f32> = nodes.iter().map(|&v| elevation_m[v as usize]).collect();
        let iced: Vec<bool> = nodes.iter().map(|&v| ice(v)).collect();
        let area: Vec<f64> = nodes.iter().map(|&v| g.area_m2(v)).collect();
        let cap: Vec<f64> = (0..m)
            .map(|k| {
                if iced[k] {
                    0.0
                } else {
                    capacity(nodes[k], e[k], area[k])
                }
            })
            .collect();
        let q_in: f64 = nodes.iter().map(|&v| f64::from(self.q[v as usize])).sum();
        // Each node's neighbours inside the basin, by local index (`NOT_IN` outside).
        // ABSENT where the graph has no neighbour (a cube corner's duplicate, a grid's edge).
        const NOT_IN: u32 = u32::MAX;
        const ABSENT: u32 = u32::MAX - 1;
        let inside: Vec<[u32; 8]> = nodes
            .iter()
            .map(|&v| {
                g.neighbours(v).map(|j| match j {
                    None => ABSENT,
                    Some(j) => nodes.binary_search(&j).map_or(NOT_IN, |k| k as u32),
                })
            })
            .collect();
        let neighbours_in = |k: usize| inside[k].map(|j| (j < ABSENT).then_some(j as usize));
        let mut kmin = 0;
        for k in 1..m {
            if e[k] < e[kmin] {
                kmin = k;
            }
        }
        let e_min = f64::from(e[kmin]);
        let spill = basin.spill_m;
        let lake_id = self.lakes.len();
        let is_flat = b as u32 >= self.first_flat;
        // The present-day map, only where the terrain is today's.
        let present = self.present && data_lakes;
        let mapped: Vec<Option<MappedLake>> = if data_lakes {
            nodes.iter().map(|&v| self.mapped_at(v)).collect()
        } else {
            vec![None; m]
        };
        let mapped_in = |set: &[bool]| (0..m).filter(|&k| set[k] && mapped[k].is_some()).count();
        let on_map = |set: &[bool]| mapped_in(set) >= DATA_LAKE_MIN_NODES as usize;
        // The mapped lake holding the most basin nodes (ties: the lowest id): its surface (the
        // highest of its nodes') and its nodes.
        let main_lake: Option<(f32, Vec<bool>)> = {
            let mut ids: Vec<u16> = mapped.iter().flatten().map(|l| l.id).collect();
            ids.sort_unstable();
            ids.chunk_by(|a, b| a == b)
                .max_by(|a, b| a.len().cmp(&b.len()).then(b[0].cmp(&a[0])))
                .map(|best| {
                    let id = best[0];
                    let of = |k: usize| mapped[k].filter(|l| l.id == id);
                    let surface = (0..m)
                        .filter_map(of)
                        .map(|l| l.surface_m)
                        .fold(f32::NEG_INFINITY, f32::max);
                    (surface, (0..m).map(|k| of(k).is_some()).collect())
                })
        };
        let make_lake = |kind, level: f64, evaporation, outflow, footprint: &[bool]| {
            let area: f64 = (0..m)
                .filter(|&k| footprint[k])
                .map(|k| area[k] * 1e-6)
                .sum();
            Lake {
                level_m: level as f32,
                spill_m: spill,
                kind,
                inflow_m3s: q_in,
                evaporation_m3s: evaporation,
                outflow_m3s: outflow,
                area_km2: area,
                exit: basin.exit,
            }
        };

        // The data's own lake surface.
        let mut data: Option<(f64, Vec<bool>)> = None;
        if data_lakes && !present && !iced[kmin] && !is_flat {
            let mut flat = vec![false; m];
            let mut queue = VecDeque::from([kmin]);
            flat[kmin] = true;
            let mut members = vec![kmin];
            while let Some(k) = queue.pop_front() {
                for j in neighbours_in(k).into_iter().flatten() {
                    if !flat[j] && !iced[j] && f64::from(e[j]) <= e_min + DATA_LAKE_FLAT_TOL_M {
                        flat[j] = true;
                        members.push(j);
                        queue.push_back(j);
                    }
                }
            }
            let flat_capacity: f64 = members.iter().map(|&k| cap[k]).sum();
            if members.len() >= DATA_LAKE_MIN_NODES as usize
                && q_in >= DATA_LAKE_MIN_BALANCE * flat_capacity
                && q_in <= DATA_LAKE_MAX_BALANCE * flat_capacity
            {
                let mut levels: Vec<f64> = members.iter().map(|&k| f64::from(e[k])).collect();
                levels.sort_by(f64::total_cmp);
                let h = levels.len() / 2;
                let level = if levels.len() % 2 == 1 {
                    levels[h]
                } else {
                    0.5 * (levels[h - 1] + levels[h])
                };
                // A candidate the map does not hold is no lake: the balance decides.
                let candidate: Vec<bool> =
                    (0..m).map(|k| flat[k] || f64::from(e[k]) < level).collect();
                if on_map(&candidate) {
                    data = Some((level, flat));
                }
            }
        }

        // The lake level table: this basin's lake stands at the table's level.
        let table_level = (!is_flat)
            .then(|| {
                self.levels
                    .binary_search_by_key(&(b as u32), |&(x, _)| x)
                    .ok()
            })
            .flatten()
            .map(|k| f64::from(self.levels[k].1));
        let table = table_level.and_then(|level| {
            let sinks: Vec<bool> = e.iter().map(|&x| f64::from(x) < level).collect();
            let mut footprint: Vec<bool> = (0..m).map(|k| sinks[k] && !iced[k]).collect();
            if !footprint.contains(&true) {
                return None;
            }
            // It drains by its surplus, whatever its level against the data's spill: the
            // table's levels are low-water datums, and the data's outlet can stand above them.
            let capacity: f64 = (0..m).filter(|&k| footprint[k]).map(|k| cap[k]).sum();
            Some(if q_in > capacity {
                footprint[exit_k] |= !iced[exit_k];
                let outflow = q_in - capacity;
                let lake = make_lake(LakeKind::Open, level, capacity, outflow, &footprint);
                Outcome::Spill {
                    lake: Some((lake, footprint)),
                }
            } else {
                let lake = make_lake(LakeKind::Terminal, level, q_in, 0.0, &footprint);
                Outcome::Sink {
                    lake: Some((lake, footprint)),
                    sinks,
                    evaporation_m3s: q_in,
                }
            })
        });
        let outcome = if let Some(table) = table {
            table
        } else if present {
            // The map decides every lake (super::today): an open basin passes its water through
            // its spill; a closed one ends at its lowest node, a dry sink.
            match balance(&e, &cap, spill, q_in).kind {
                LakeKind::Open => Outcome::Spill { lake: None },
                _ => {
                    let mut sinks = vec![false; m];
                    sinks[kmin] = true;
                    Outcome::Sink {
                        lake: None,
                        sinks,
                        evaporation_m3s: q_in,
                    }
                }
            }
        } else if is_flat {
            // The data's flat: a lake at its lowest level that passes its inflow on, less what
            // its surface evaporates, if the inflow meets the veto. Its nodes are never ice.
            let footprint = vec![true; m];
            let capacity: f64 = cap.iter().sum();
            if q_in >= DATA_LAKE_MIN_BALANCE * capacity && on_map(&footprint) {
                let outflow = (q_in - capacity).max(0.0);
                let lake = make_lake(LakeKind::Data, e_min, q_in - outflow, outflow, &footprint);
                Outcome::Spill {
                    lake: Some((lake, footprint)),
                }
            } else {
                Outcome::Spill { lake: None }
            }
        } else if let Some((level, flat)) = data {
            let sinks: Vec<bool> = (0..m).map(|k| flat[k] || f64::from(e[k]) < level).collect();
            let mut footprint: Vec<bool> = (0..m).map(|k| sinks[k] && !iced[k]).collect();
            let below: f64 = (0..m).filter(|&k| footprint[k]).map(|k| cap[k]).sum();
            let outflow = if level >= f64::from(spill) - DATA_LAKE_FLAT_TOL_M {
                (q_in - below).max(0.0)
            } else {
                0.0
            };
            if outflow > 0.0 {
                footprint[exit_k] |= !iced[exit_k];
                let lake = make_lake(LakeKind::Data, level, q_in - outflow, outflow, &footprint);
                Outcome::Spill {
                    lake: Some((lake, footprint)),
                }
            } else {
                let lake = make_lake(LakeKind::Data, level, q_in, 0.0, &footprint);
                Outcome::Sink {
                    lake: Some((lake, footprint)),
                    sinks,
                    evaporation_m3s: q_in,
                }
            }
        } else {
            let bal = balance(&e, &cap, spill, q_in);
            let tol = OPEN_LAKE_MAP_TOL_M as f32;
            match (bal.kind, main_lake) {
                // Today's terrain and no mapped lake: the lattice dammed the valley. Breached:
                // the inflow passes through the spill.
                (LakeKind::Open, None) if data_lakes => Outcome::Spill { lake: None },
                // Above the mapped lake: it stands at the mapped surface over the mapped lake's
                // own nodes, and passes the rest of its inflow on (the real outlet is a gorge
                // below the lattice).
                (LakeKind::Open, Some((surface, own))) if spill > surface + tol => {
                    let level = f64::from(surface);
                    let mut footprint: Vec<bool> = (0..m).map(|k| !iced[k] && own[k]).collect();
                    let evaporation: f64 = (0..m).filter(|&k| footprint[k]).map(|k| cap[k]).sum();
                    // The exit joins it as its outlet (unless ice), as for every lake that
                    // overflows: otherwise the exit would carry the whole inflow and pass on
                    // less, a river shrinking downstream.
                    let any = footprint.contains(&true);
                    footprint[exit_k] |= !iced[exit_k];
                    Outcome::Spill {
                        lake: any.then(|| {
                            let lake = make_lake(
                                LakeKind::Data,
                                level,
                                evaporation,
                                q_in - evaporation,
                                &footprint,
                            );
                            (lake, footprint)
                        }),
                    }
                }
                (LakeKind::Open, _) => {
                    let footprint: Vec<bool> = iced.iter().map(|&i| !i).collect();
                    let deep = f64::from(spill) - e_min >= MIN_LAKE_DEPTH_M;
                    Outcome::Spill {
                        lake: (deep && footprint.contains(&true)).then(|| {
                            let lake = make_lake(
                                LakeKind::Open,
                                bal.level_m,
                                bal.evaporation_m3s,
                                bal.outflow_m3s,
                                &footprint,
                            );
                            (lake, footprint)
                        }),
                    }
                }
                (_, _) => {
                    let sinks: Vec<bool> = e.iter().map(|&x| x <= bal.wet_through_m).collect();
                    let footprint: Vec<bool> = (0..m).map(|k| sinks[k] && !iced[k]).collect();
                    let playa =
                        bal.level_m - e_min < MIN_LAKE_DEPTH_M && q_in < RIVER_MIN_DISCHARGE_M3S;
                    let lake = (!playa && footprint.contains(&true)).then(|| {
                        let lake =
                            make_lake(LakeKind::Terminal, bal.level_m, q_in, 0.0, &footprint);
                        (lake, footprint)
                    });
                    Outcome::Sink {
                        lake,
                        sinks,
                        evaporation_m3s: q_in,
                    }
                }
            }
        };

        // Sources: a spilling basin's footprint and exit, or a terminal basin's sinks. Every
        // other node routes to the neighbour one BFS step closer to them, lowest e first.
        let (lake, sources, spill, sink_evaporation) = match outcome {
            Outcome::Spill { lake } => {
                let mut sources = match &lake {
                    Some((_, footprint)) => footprint.clone(),
                    None => vec![false; m],
                };
                sources[exit_k] = true;
                (lake, sources, true, 0.0)
            }
            Outcome::Sink {
                lake,
                sinks,
                evaporation_m3s,
            } => (lake, sinks, false, evaporation_m3s),
        };
        let mut dist = vec![u32::MAX; m];
        let mut queue: VecDeque<usize> = (0..m).filter(|&k| sources[k]).collect();
        for &k in &queue {
            dist[k] = 0;
        }
        while let Some(k) = queue.pop_front() {
            for j in neighbours_in(k).into_iter().flatten() {
                if dist[j] == u32::MAX {
                    dist[j] = dist[k] + 1;
                    queue.push_back(j);
                }
            }
        }
        let mut target = vec![usize::MAX; m];
        for k in (0..m).filter(|&k| !sources[k]) {
            let mut best: Option<(usize, usize)> = None;
            for (slot, j) in neighbours_in(k).into_iter().enumerate() {
                let Some(j) = j else { continue };
                if dist[j] < dist[k] && best.is_none_or(|(_, b)| e[j] < e[b]) {
                    best = Some((slot, j));
                }
            }
            let (slot, j) = best.expect("a basin is connected to its sources");
            receiver[nodes[k] as usize] = slot as u8;
            target[k] = j;
        }
        let mut by_dist: Vec<usize> = (0..m).filter(|&k| !sources[k]).collect();
        by_dist.sort_by(|&a, &b| dist[b].cmp(&dist[a]).then(a.cmp(&b)));
        for k in by_dist {
            let (from, to) = (nodes[k] as usize, nodes[target[k]] as usize);
            self.q[to] += self.q[from];
            self.area_km2[to] += self.area_km2[from];
        }
        if spill {
            // The lake's nodes gather at the exit, which drains over the rim (or to a lower
            // neighbour outside the basin).
            let exit = basin.exit as usize;
            for k in (0..m).filter(|&k| sources[k] && k != exit_k) {
                let v = nodes[k] as usize;
                receiver[v] = RECEIVER_EXIT;
                self.q[exit] += self.q[v];
                self.area_km2[exit] += self.area_km2[v];
            }
            let out = match &lake {
                Some((l, _)) => {
                    self.evaporated_m3s += l.evaporation_m3s;
                    l.outflow_m3s as f32
                }
                None => self.q[exit],
            };
            let down = match receiver[exit] {
                RECEIVER_NONE => None,
                k => g.neighbours(basin.exit)[usize::from(k)],
            };
            if let Some(j) = down.filter(|&j| !ocean[j as usize]) {
                self.q[j as usize] += out;
                self.area_km2[j as usize] += self.area_km2[exit];
            }
        } else {
            self.evaporated_m3s += sink_evaporation;
            for k in (0..m).filter(|&k| sources[k]) {
                receiver[nodes[k] as usize] = RECEIVER_NONE;
            }
        }

        if let Some((lake, footprint)) = lake {
            let id = u16::try_from(lake_id).expect("at most 65,535 lakes per era");
            // Shore distance: 1 where a neighbour is not lake, then BFS inward.
            let mut steps = vec![0u16; m];
            let mut queue = VecDeque::new();
            for k in (0..m).filter(|&k| footprint[k]) {
                let shore = inside[k]
                    .iter()
                    .any(|&j| j == NOT_IN || (j < ABSENT && !footprint[j as usize]));
                if shore {
                    steps[k] = 1;
                    queue.push_back(k);
                }
            }
            while let Some(k) = queue.pop_front() {
                for j in neighbours_in(k).into_iter().flatten() {
                    if footprint[j] && steps[j] == 0 {
                        steps[j] = steps[k].saturating_add(1);
                        queue.push_back(j);
                    }
                }
            }
            for k in (0..m).filter(|&k| footprint[k]) {
                self.lake_nodes.push(LakeNode {
                    node: nodes[k],
                    lake: id,
                    shore_steps: steps[k],
                });
            }
            self.lakes.push(lake);
        }
        m as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrology::climate::tests::shipped_table;
    use crate::hydrology::climate::{Climate, SECONDS_PER_YEAR};
    use crate::hydrology::{GridGraph, Routing, receiver_of};
    use proptest::prelude::*;
    use serde_json::Value;

    const GOLDEN: &str = include_str!("../../tests/fixtures/hydro_golden.json");

    fn route(g: &impl HydroGraph, elevation: Vec<f32>) -> Routed {
        let mut routing = Routing::new(elevation);
        while !routing.step(g, u32::MAX) {}
        routing.finish()
    }

    /// Runs the accumulation to the end with `own` runoff and `capacity` per node.
    fn accumulate(
        g: &impl HydroGraph,
        routed: &mut Routed,
        own: impl Fn(u32) -> f32,
        capacity: impl Fn(u32) -> f64,
        ice: impl Fn(u32) -> bool,
        data_lakes: bool,
        budget: u32,
    ) -> Accumulation {
        accumulate_with(g, routed, own, capacity, ice, data_lakes, budget, &[], &[]).0
    }

    /// [`accumulate`] with lake level table points `(node, level)`, placed only with data
    /// lakes, and mapped nodes `(node, surface)`. Also returns the placed `(basin, level)` pairs
    /// and the unplaced point indices.
    #[allow(clippy::too_many_arguments)]
    fn accumulate_with(
        g: &impl HydroGraph,
        routed: &mut Routed,
        own: impl Fn(u32) -> f32,
        capacity: impl Fn(u32) -> f64,
        ice: impl Fn(u32) -> bool,
        data_lakes: bool,
        budget: u32,
        points: &[(u32, f32)],
        mapped: &[(u32, f32)],
    ) -> (Accumulation, Vec<(u32, f32)>, Vec<usize>) {
        accumulate_full(
            g, routed, own, capacity, ice, data_lakes, false, budget, points, mapped,
        )
    }

    /// [`accumulate_with`] in present mode or not, as the pass runs it (no data flats at the
    /// present day; present mode implies data lakes).
    #[allow(clippy::too_many_arguments)]
    fn accumulate_full(
        g: &impl HydroGraph,
        routed: &mut Routed,
        own: impl Fn(u32) -> f32,
        capacity: impl Fn(u32) -> f64,
        ice: impl Fn(u32) -> bool,
        data_lakes: bool,
        present: bool,
        budget: u32,
        points: &[(u32, f32)],
        mapped: &[(u32, f32)],
    ) -> (Accumulation, Vec<(u32, f32)>, Vec<usize>) {
        let data_lakes = data_lakes || present;
        let first_flat = if data_lakes && !present {
            let mut flats = Flats::new(g.len());
            while !flats.step(g, routed, &ice, budget).0 {}
            flats.register(g, routed)
        } else {
            routed.basins.len() as u32
        };
        let (placed, unplaced) = if data_lakes {
            place_levels(routed, first_flat, &ice, points)
        } else {
            (Vec::new(), Vec::new())
        };
        let mut acc = Accumulation::new(g.len(), first_flat);
        acc.set_levels(placed.clone());
        acc.set_present(present);
        // Set even without data lakes: the pass must then ignore them.
        acc.set_mapped(mapped_lakes(mapped));
        while !acc.init_step(g, routed, |i, _| own(i), budget).0 {}
        while !acc
            .walk_step(g, routed, &ice, |i, _, _| capacity(i), data_lakes, budget)
            .0
        {}
        (acc, placed, unplaced)
    }

    /// Mapped nodes `(node, surface)` as the pass holds them: one lake per distinct surface
    /// (ids in ascending surface order), sorted by node, the first entry for a node kept.
    fn mapped_lakes(mapped: &[(u32, f32)]) -> Vec<(u32, MappedLake)> {
        let mut surfaces: Vec<f32> = mapped.iter().map(|m| m.1).collect();
        surfaces.sort_by(f32::total_cmp);
        surfaces.dedup();
        let mut out: Vec<(u32, MappedLake)> = mapped
            .iter()
            .map(|&(v, s)| {
                let id = surfaces.iter().position(|&x| x == s).unwrap() as u16 + 1;
                (v, MappedLake { id, surface_m: s })
            })
            .collect();
        out.sort_by_key(|&(n, _)| n);
        out.dedup_by_key(|m| m.0);
        out
    }

    fn golden_grid(fx: &Value) -> (GridGraph, Vec<f32>) {
        let g = GridGraph {
            rows: fx["rows"].as_u64().unwrap() as u32,
            cols: fx["cols"].as_u64().unwrap() as u32,
            spacing_m: fx["spacing_m"].as_f64().unwrap(),
        };
        let elevation = fx["elevation_m"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|r| {
                r.as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_f64().unwrap() as f32)
            })
            .collect();
        (g, elevation)
    }

    #[test]
    fn golden_lakes_match_the_fixture() {
        let fx: Value = serde_json::from_str(GOLDEN).unwrap();
        let (g, elevation) = golden_grid(&fx);
        let rim = g.node(1, 1);
        for (case, capacity, kind) in [
            ("A", 0.0, LakeKind::Open),
            ("B", 2.0, LakeKind::Open),
            ("C", 5.0, LakeKind::Terminal),
        ] {
            let c = &fx["q"][case];
            assert_eq!(
                c["capacity_m3s"].as_f64().unwrap_or(0.0),
                capacity,
                "{case}"
            );
            for budget in [1, 3, u32::MAX] {
                let mut r = route(&g, elevation.clone());
                let acc = accumulate(&g, &mut r, |_| 1.0, |_| capacity, |_| false, false, budget);
                let want: Vec<Option<f64>> = c["q"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|row| row.as_array().unwrap().iter().map(|v| v.as_f64()))
                    .collect();
                for i in 0..g.len() {
                    let got = (!r.ocean[i as usize]).then(|| f64::from(acc.q[i as usize]));
                    assert_eq!(got, want[i as usize], "{case}: q at {:?}", g.row_col(i));
                }
                assert_eq!(acc.lakes.len(), 1, "{case}");
                let lake = &acc.lakes[0];
                let l = &c["lake"];
                assert_eq!(lake.kind, kind, "{case}");
                assert_eq!(format!("{:?}", lake.kind), l["kind"].as_str().unwrap());
                let level = l["level_m"].as_f64().unwrap();
                assert!(
                    (f64::from(lake.level_m) - level).abs() < 1e-6,
                    "{case}: {lake:?}"
                );
                for (got, key) in [
                    (lake.inflow_m3s, "inflow_m3s"),
                    (lake.evaporation_m3s, "evaporation_m3s"),
                    (lake.outflow_m3s, "outflow_m3s"),
                ] {
                    assert_eq!(got, l[key].as_f64().unwrap(), "{case}: {key}");
                }
                assert_eq!(lake.spill_m, 3.0);
                assert_eq!(lake.exit, g.node(1, 2));
                assert_eq!(lake.area_km2, 4.0);
                // Every basin node is lake; the corners of the 2 x 2 lake are all on the shore.
                let nodes: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
                assert_eq!(nodes, r.basins[0].nodes);
                assert!(acc.lake_nodes.iter().all(|n| n.shore_steps == 1));
                if kind == LakeKind::Terminal {
                    // The rim gets nothing, and the lake's nodes keep their water.
                    assert_eq!(acc.q[rim as usize], 1.0);
                    for &v in &r.basins[0].nodes {
                        assert_eq!(receiver_of(&g, &r, v), None);
                    }
                }
            }
        }
        // The level the brief states for case C: 2 + 7/15 m.
        let lc = fx["q"]["C"]["lake"]["level_m"].as_f64().unwrap();
        assert!((lc - (2.0 + 7.0 / 15.0)).abs() < 1e-12);
    }

    /// Basin nodes as (elevation, capacity), a spill above them all, and an inflow.
    fn random_basin() -> impl Strategy<Value = (Vec<f32>, Vec<f64>, f32, f64)> {
        let node = (
            -50.0f32..400.0,
            0.0f32..3000.0,
            -25.0f32..32.0,
            50.0f64..2000.0,
        );
        (
            prop::collection::vec(node, 1..40),
            0.02f32..60.0,
            0.0f64..4.0,
            0.0f32..3000.0,
            -25.0f32..32.0,
        )
            .prop_map(|(nodes, headroom, catchment, p, t)| {
                let table = shipped_table();
                // Coarse elevations make groups of several nodes common.
                let e: Vec<f32> = nodes.iter().map(|n| (n.0 / 7.0).round() * 7.0).collect();
                let area: f64 = nodes.iter().map(|n| n.3 * 1e6).sum();
                let cap: Vec<f64> = nodes
                    .iter()
                    .map(|n| {
                        let c = Climate {
                            precip_mm: n.1,
                            temp_c: n.2,
                        };
                        f64::from(table.net_evaporation_mm(c)) * 1e-3 * n.3 * 1e6 / SECONDS_PER_YEAR
                    })
                    .collect();
                let top = e.iter().copied().fold(f32::MIN, f32::max);
                let runoff = table.runoff_mm(Climate {
                    precip_mm: p,
                    temp_c: t,
                });
                let q_in = f64::from(runoff) * 1e-3 * area * (1.0 + catchment) / SECONDS_PER_YEAR;
                (e, cap, top + headroom, q_in)
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn lakes_conserve_the_water_balance(
            (e, cap, spill, q_in) in random_basin(),
            more in 0.0f64..3.0,
        ) {
            let b = balance(&e, &cap, spill, q_in);
            let total = b.evaporation_m3s + b.outflow_m3s;
            prop_assert!((total - q_in).abs() <= 1e-6 * q_in.max(1e-9), "{:?} vs {}", b, q_in);
            prop_assert!(b.evaporation_m3s >= 0.0 && b.outflow_m3s >= 0.0);
            prop_assert_eq!(b.outflow_m3s > 0.0, b.kind == LakeKind::Open);
            if b.outflow_m3s > 0.0 {
                prop_assert_eq!(b.level_m, f64::from(spill));
            }
            if b.level_m < f64::from(spill) {
                prop_assert_eq!(b.outflow_m3s, 0.0);
            }
            let lowest = e.iter().copied().fold(f32::MAX, f32::min);
            prop_assert!(b.level_m >= f64::from(lowest) && b.level_m <= f64::from(spill));
            // The evaporating surface: a terminal lake's wet groups hold at least its inflow.
            if b.kind == LakeKind::Terminal {
                let wet: f64 = e.iter().zip(&cap).filter(|(x, _)| **x <= b.wet_through_m)
                    .map(|(_, c)| c).sum();
                let dry_below: f64 = e.iter().zip(&cap).filter(|(x, _)| **x < b.wet_through_m)
                    .map(|(_, c)| c).sum();
                let slack = 1e-12 * q_in.max(1.0);
                prop_assert!(wet >= q_in - slack && dry_below <= q_in + slack);
            }
            // More inflow never lowers the level.
            let b2 = balance(&e, &cap, spill, q_in * (1.0 + more) + more);
            prop_assert!(b2.level_m >= b.level_m, "{:?} then {:?}", b, b2);
        }

        #[test]
        fn water_is_conserved_through_the_basins(
            levels in prop::collection::vec(-3i32..12, 132),
            evap in prop::collection::vec(0.0f64..3.0, 132),
            ice in prop::collection::vec(0u8..10, 132),
            data_lakes in any::<bool>(),
            table in prop::collection::vec((0u32..132, -3i32..14), 0..4),
            mapped in prop::collection::vec((0u32..132, -3i32..14), 0..24),
            present in any::<bool>(),
        ) {
            // An 11 x 12 grid at 10 km: flats, pits, ice and several outlets.
            let g = GridGraph { rows: 11, cols: 12, spacing_m: 10_000.0 };
            let elevation: Vec<f32> = levels.iter().map(|&l| l as f32 * 3.0).collect();
            let iced = |i: u32| ice[i as usize] == 0;
            let mut r = route(&g, elevation.clone());
            // Lake level table points, off the 3 m grid of elevations.
            let points: Vec<(u32, f32)> =
                table.iter().map(|&(n, l)| (n, l as f32 * 3.0 + 0.7)).collect();
            // Mapped lake surfaces, off the 3 m grid too.
            let mapped: Vec<(u32, f32)> =
                mapped.iter().map(|&(n, l)| (n, l as f32 * 3.0 + 0.4)).collect();
            let (acc, placed, _) = accumulate_full(&g, &mut r, |i| 1.0 + (i % 3) as f32,
                |i| evap[i as usize], iced, data_lakes, present, 5, &points, &mapped);
            let table_exits: Vec<u32> =
                placed.iter().map(|&(b, _)| r.basins[b as usize].exit).collect();
            let mut own = 0.0;
            let mut out = 0.0;
            for i in 0..g.len() {
                let iu = i as usize;
                if r.ocean[iu] {
                    continue;
                }
                own += 1.0 + f64::from(i % 3);
                let sink = r.basin_of[iu] != NO_BASIN && r.receiver[iu] == RECEIVER_NONE;
                // At the present day a lake's nodes gather at its exit, which passes only its
                // outflow (added below).
                let in_lake = present && acc.lake_nodes.iter().any(|n| n.node == i);
                match receiver_of(&g, &r, i) {
                    _ if in_lake => {}
                    Some(j) if r.ocean[j as usize] => out += f64::from(acc.q[iu]),
                    None if !sink => out += f64::from(acc.q[iu]),
                    _ => {}
                }
                // Discharge never shrinks downstream, except out of a lake (its footprint or its
                // exit, which may be ice) or into a basin.
                let lake = |v: u32| {
                    acc.lake_nodes.iter().any(|n| n.node == v)
                        || acc.lakes.iter().any(|l| l.exit == v)
                };
                if let Some(j) = receiver_of(&g, &r, i).filter(|&j| !r.ocean[j as usize])
                    && !lake(i)
                    && !lake(j)
                    && r.basin_of[j as usize] == NO_BASIN
                {
                    prop_assert!(acc.q[j as usize] >= acc.q[iu], "{} -> {}", i, j);
                }
            }
            if present {
                for l in &acc.lakes {
                    let sink = r.basin_of[l.exit as usize] != NO_BASIN
                        && r.receiver[l.exit as usize] == RECEIVER_NONE;
                    match receiver_of(&g, &r, l.exit) {
                        Some(j) if r.ocean[j as usize] => out += l.outflow_m3s,
                        None if !sink => out += l.outflow_m3s,
                        _ => {}
                    }
                }
            }
            let balance = out + acc.evaporated_m3s;
            prop_assert!((balance - own).abs() <= 1e-4 * own, "{} out + {} evaporated vs {}",
                out, acc.evaporated_m3s, own);
            for lake in &acc.lakes {
                let sum = lake.evaporation_m3s + lake.outflow_m3s;
                prop_assert!((sum - lake.inflow_m3s).abs() <= 1e-9 * lake.inflow_m3s.max(1.0));
                // A table lake keeps the table's level, which may stand above the spill.
                if !table_exits.contains(&lake.exit) {
                    prop_assert!(lake.level_m <= lake.spill_m);
                }
            }
            // Every node outside a lake drains to a neighbour.
            for i in 0..g.len() {
                let lake = acc.lake_nodes.iter().any(|n| n.node == i);
                if let Some(j) = receiver_of(&g, &r, i).filter(|_| !lake) {
                    prop_assert!(g.neighbours(i).contains(&Some(j)), "{} -> {}", i, j);
                }
            }
            // Footprints hold no ice, and their nodes are sorted.
            prop_assert!(acc.lake_nodes.iter().all(|n| !iced(n.node)));
            prop_assert!(acc.lake_nodes.windows(2).all(|w| w[0].node < w[1].node));
        }
    }

    /// A 9 x 14 grid at 10 km: ocean in column 0, a ramp rising east, a channel along row 4 to
    /// a shelf at exactly 50 m in rows 2..=6, columns 6..=9 (20 nodes; 19 without (2, 9)).
    fn shelf(nodes: u32) -> (GridGraph, Vec<f32>, Vec<u32>) {
        let g = GridGraph {
            rows: 9,
            cols: 14,
            spacing_m: 10_000.0,
        };
        let mut flat = Vec::new();
        let elevation = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (4, c @ 1..=5) => 8.0 * c as f32,
                (2, 9) if nodes == 19 => 93.0,
                (2..=6, 6..=9) => {
                    flat.push(i);
                    50.0
                }
                (_, c) => 10.0 * c as f32 + 3.0,
            })
            .collect();
        (g, elevation, flat)
    }

    #[test]
    fn a_flat_needs_twenty_nodes() {
        for (nodes, data_lakes, lake) in [(20, true, true), (19, true, false), (20, false, false)] {
            let (g, elevation, flat) = shelf(nodes);
            assert_eq!(flat.len() as u32, nodes);
            let mut r = route(&g, elevation);
            // The hazard: the shelf is no depression; it drains through the channel.
            assert!(r.basins.is_empty());
            // A lake on today's map.
            let mapped: Vec<(u32, f32)> = flat.iter().map(|&v| (v, 50.0)).collect();
            let (acc, _, _) = accumulate_with(
                &g,
                &mut r,
                |_| 1.0,
                |_| 0.1,
                |_| false,
                data_lakes,
                4,
                &[],
                &mapped,
            );
            let lakes = &acc.lakes;
            assert_eq!(
                lakes.len(),
                usize::from(lake),
                "{nodes} {data_lakes}: {lakes:?}"
            );
            if !lake {
                assert!(acc.lake_nodes.is_empty());
                continue;
            }
            let l = &lakes[0];
            assert_eq!((l.kind, l.level_m, l.spill_m), (LakeKind::Data, 50.0, 50.0));
            assert_eq!(l.exit, g.node(4, 6));
            assert_eq!(l.area_km2, 2000.0);
            assert!((l.evaporation_m3s - 2.0).abs() < 1e-9, "{l:?}");
            assert_eq!(l.outflow_m3s, l.inflow_m3s - l.evaporation_m3s);
            assert!(l.inflow_m3s > 20.0, "{l:?}");
            let nodes: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
            assert_eq!(nodes, flat);
            // The outflow leaves by the exit's receiver, the channel's head.
            let head = g.node(4, 5);
            assert_eq!(receiver_of(&g, &r, l.exit), Some(head));
            assert_eq!(acc.q[head as usize], 1.0 + l.outflow_m3s as f32);
            // Every shelf node drains inside the shelf, to the exit in the end.
            for &v in &flat {
                let mut at = v;
                while at != l.exit {
                    at = receiver_of(&g, &r, at).unwrap();
                    assert!(flat.contains(&at), "{v} leaves the shelf at {at}");
                }
            }
        }
    }

    /// A 9 x 11 grid at 10 km: ocean in column 0, 90 m walls with a 50 m gap at (4, 1), and a
    /// cone-shaped basin bed falling to 5 m at (4, 6).
    fn deep_bowl() -> (GridGraph, Vec<f32>) {
        let g = GridGraph {
            rows: 9,
            cols: 11,
            spacing_m: 10_000.0,
        };
        let elevation = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (4, 1) => 50.0,
                (r @ 1..=7, c @ 2..=9) => {
                    let (dr, dc) = (r as f32 - 4.0, c as f32 - 6.0);
                    5.0 + 4.0 * (dr * dr + dc * dc).sqrt()
                }
                _ => 90.0,
            })
            .collect();
        (g, elevation)
    }

    #[test]
    fn a_table_lake_drains_by_its_surplus() {
        let (g, elevation) = deep_bowl();
        let centre = g.node(4, 6);
        let (rim, sea) = (g.node(4, 1), g.node(4, 0));
        let run_mapped = |capacity: f64, points: &[(u32, f32)], mapped: &[(u32, f32)]| {
            let mut r = route(&g, elevation.clone());
            let (acc, _, unplaced) = accumulate_with(
                &g,
                &mut r,
                |_| 5.0,
                |_| capacity,
                |_| false,
                true,
                7,
                points,
                mapped,
            );
            (r, acc, unplaced)
        };
        let run = |capacity: f64, points: &[(u32, f32)]| run_mapped(capacity, points, &[]);
        // Humid: the balance alone fills the bowl to its 50 m spill (a lake the map holds at
        // that level; table lakes need no map).
        let (r, balance, _) = run_mapped(0.1, &[], &[(centre, 50.0)]);
        assert_eq!(r.basins.len(), 1);
        assert_eq!(r.basins[0].spill_m, 50.0);
        let open = &balance.lakes[0];
        assert_eq!((open.kind, open.level_m), (LakeKind::Open, 50.0));
        // The hazard: the table's level sits 1 m under the data's spill.
        for level in [49.0, 15.0, 52.0] {
            let (r, acc, unplaced) = run(0.1, &[(centre, level)]);
            assert!(unplaced.is_empty());
            let lake = &acc.lakes[0];
            // Open by its surplus, at the table's level, even above the spill.
            assert_eq!(
                (lake.kind, lake.level_m),
                (LakeKind::Open, level),
                "{lake:?}"
            );
            // Evaporation over the nodes below the level (the exit is the outlet, not surface).
            let wet = r.basins[0]
                .nodes
                .iter()
                .filter(|&&v| elevation[v as usize] < level)
                .count();
            let capacity = 0.1 * wet as f64;
            assert!((lake.evaporation_m3s - capacity).abs() < 1e-9, "{lake:?}");
            assert_eq!(lake.outflow_m3s, lake.inflow_m3s - lake.evaporation_m3s);
            // The outflow leaves over the rim and reaches the sea.
            assert_eq!(receiver_of(&g, &r, lake.exit), Some(rim));
            assert_eq!(receiver_of(&g, &r, rim), Some(sea));
            assert_eq!(acc.q[rim as usize], 5.0 + lake.outflow_m3s as f32);
            let below: Vec<u32> = r.basins[0]
                .nodes
                .iter()
                .copied()
                .filter(|&v| elevation[v as usize] < level || v == lake.exit)
                .collect();
            let footprint: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
            assert_eq!(footprint, below, "{level}");
        }
        // Arid (a Caspian): terminal at the table's level under the spill.
        let (_, acc, _) = run(1e3, &[(centre, 15.0)]);
        let lake = &acc.lakes[0];
        assert_eq!((lake.kind, lake.level_m), (LakeKind::Terminal, 15.0));
        assert_eq!(lake.outflow_m3s, 0.0);
        assert_eq!(lake.evaporation_m3s, lake.inflow_m3s);
        assert_eq!(acc.q[rim as usize], 5.0);
        let footprint: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
        assert!(footprint.iter().all(|&v| elevation[v as usize] < 15.0));
    }

    #[test]
    fn conflicting_table_points_are_unplaced() {
        let (g, elevation) = deep_bowl();
        let (centre, near) = (g.node(4, 6), g.node(3, 6));
        let wall = g.node(0, 5);
        let mut r = route(&g, elevation);
        let points = [
            (centre, 15.0),
            // The same water body at the same level (Michigan and Huron): silent.
            (near, 15.3),
            // Another level for the same basin.
            (near, 30.0),
            // Below the whole bed (the bed's lowest node is 5 m).
            (centre, 2.0),
            (wall, 12.0),
        ];
        let (placed, unplaced) = place_levels(&r, r.basins.len() as u32, &|_| false, &points);
        assert_eq!(placed, vec![(0, 15.0)]);
        assert_eq!(unplaced, vec![2, 3, 4]);
        // A level whose basin's nodes below it are all ice is unplaced too.
        let iced = |v: u32| elevation_below(&g, v);
        let (placed, unplaced) = place_levels(&r, r.basins.len() as u32, &iced, &[(centre, 9.0)]);
        assert!(placed.is_empty());
        assert_eq!(unplaced, vec![0]);
        let _ = &mut r;
    }

    /// Ice on the deep bowl's nodes below 9 m.
    fn elevation_below(g: &GridGraph, v: u32) -> bool {
        let (_, elevation) = deep_bowl();
        let _ = g;
        elevation[v as usize] < 9.0
    }

    /// A 9 x 17 grid at 10 km, ocean all round, a 70 m plateau, and a shelf at exactly 50 m
    /// in rows 2..=6, columns 3..=13 (55 nodes) with two outlets at opposite corners: 40 m
    /// nodes at (1, 2) and (7, 14), each beside one shelf corner and the sea.
    fn two_outlet_shelf() -> (GridGraph, Vec<f32>, Vec<u32>) {
        let g = GridGraph {
            rows: 9,
            cols: 17,
            spacing_m: 10_000.0,
        };
        let mut flat = Vec::new();
        let elevation = (0..g.len())
            .map(|i| match g.row_col(i) {
                (0 | 8, _) | (_, 0 | 16) => -10.0,
                (1, 2) | (7, 14) => 40.0,
                (2..=6, 3..=13) => {
                    flat.push(i);
                    50.0
                }
                _ => 70.0,
            })
            .collect();
        (g, elevation, flat)
    }

    #[test]
    fn a_flat_drains_by_its_own_outlets() {
        let (g, elevation, flat) = two_outlet_shelf();
        let (west, east) = (g.node(1, 2), g.node(7, 14));
        for (capacity, lakes) in [(10.0, 0), (0.01, 2)] {
            let mut r = route(&g, elevation.clone());
            assert!(r.basins.is_empty());
            let mapped: Vec<(u32, f32)> = flat.iter().map(|&v| (v, 50.0)).collect();
            let (acc, _, _) = accumulate_with(
                &g,
                &mut r,
                |_| 1.0,
                |_| capacity,
                |_| false,
                true,
                3,
                &[],
                &mapped,
            );
            assert_eq!(acc.lakes.len(), lakes, "{:?}", acc.lakes);
            // The 70 m plateau is a flat too, but with a long downhill edge: never a lake.
            assert!(acc.lake_nodes.iter().all(|n| flat.contains(&n.node)));
            // Each half of the shelf leaves by its own outlet, and nothing jumps.
            let (qw, qe) = (
                f64::from(acc.q[west as usize]),
                f64::from(acc.q[east as usize]),
            );
            for i in 0..g.len() {
                let lake = acc.lake_nodes.iter().any(|n| n.node == i);
                if let Some(j) = receiver_of(&g, &r, i).filter(|_| !lake) {
                    assert!(g.neighbours(i).contains(&Some(j)), "{i} -> {j}");
                }
            }
            if lakes == 0 {
                // The hazard: both outlets carry a river's worth of the shelf's water.
                assert!(qw > 20.0 && qe > 20.0, "{qw} {qe}");
                let shelf_own = flat.len() as f64;
                let plateau_in: f64 = (qw - 1.0) + (qe - 1.0);
                assert!(plateau_in >= shelf_own, "{qw} + {qe} vs {shelf_own}");
            } else {
                let exits: Vec<u32> = acc.lakes.iter().map(|l| l.exit).collect();
                assert!(exits.contains(&g.node(2, 3)) && exits.contains(&g.node(6, 13)));
                assert!(
                    acc.lakes
                        .iter()
                        .all(|l| l.level_m == 50.0 && l.outflow_m3s > 0.0)
                );
            }
        }
    }

    #[test]
    fn a_dry_basin_routes_to_its_lake_by_bfs() {
        // A 1 km grid: ocean on the west column; a basin spilling at 50 m with a deep pit at
        // (3, 7) and a long dry shelf. With a small inflow and a large capacity the lake is
        // the pit; every other basin node routes toward it, one BFS step at a time.
        let g = GridGraph {
            rows: 7,
            cols: 11,
            spacing_m: 1_000.0,
        };
        let elevation: Vec<f32> = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (3, 7) => 2.0,
                (3, 1) => 50.0,
                (r, c) if (1..=5).contains(&r) && (2..=9).contains(&c) => {
                    10.0 + c as f32 + r as f32 * 0.5
                }
                _ => 90.0,
            })
            .collect();
        let mut r = route(&g, elevation.clone());
        assert_eq!(r.basins.len(), 1);
        let pit = g.node(3, 7);
        let acc = accumulate(&g, &mut r, |_| 30.0, |_| 1e6, |_| false, false, u32::MAX);
        assert_eq!(acc.lakes.len(), 1);
        let lake = &acc.lakes[0];
        assert_eq!(lake.kind, LakeKind::Terminal);
        assert_eq!(
            acc.lake_nodes.iter().map(|n| n.node).collect::<Vec<_>>(),
            vec![pit]
        );
        let basin = &r.basins[0];
        // The hazard: routing's tree drains the shelf to the exit, not the pit.
        assert_ne!(basin.exit, pit);
        for &v in &basin.nodes {
            let mut at = v;
            let mut steps = 0;
            while let Some(next) = receiver_of(&g, &r, at) {
                assert!(r.basin_of[next as usize] == 0, "{v} leaves the basin");
                at = next;
                steps += 1;
                assert!(steps < 50);
            }
            assert_eq!(at, pit, "{:?} ends at {:?}", g.row_col(v), g.row_col(at));
        }
        // Each dry node drains to its lowest neighbour one BFS step closer to the pit.
        let mut dist = std::collections::HashMap::from([(pit, 0u32)]);
        let mut queue = VecDeque::from([pit]);
        while let Some(v) = queue.pop_front() {
            for j in g.neighbours(v).into_iter().flatten() {
                if basin.nodes.contains(&j) && !dist.contains_key(&j) {
                    dist.insert(j, dist[&v] + 1);
                    queue.push_back(j);
                }
            }
        }
        let mut choices = 0;
        for &v in basin.nodes.iter().filter(|&&v| v != pit) {
            let closer: Vec<u32> = g
                .neighbours(v)
                .into_iter()
                .flatten()
                .filter(|j| dist.get(j).is_some_and(|&d| d < dist[&v]))
                .collect();
            let lowest = closer
                .iter()
                .copied()
                .min_by(|a, b| elevation[*a as usize].total_cmp(&elevation[*b as usize]))
                .unwrap();
            assert_eq!(receiver_of(&g, &r, v), Some(lowest), "{:?}", g.row_col(v));
            choices += u32::from(closer.len() > 1);
        }
        assert!(
            choices > 5,
            "the hazard: dry nodes choose among {choices} closer neighbours"
        );
        // The pit gathers the whole basin's water: its inflow is the lake's.
        assert_eq!(f64::from(acc.q[pit as usize]), lake.inflow_m3s);
    }

    // ---- Lakes checked against today's map (Task 8b). ----

    /// The dam's column in [`dammed_valley`].
    const DAM: u32 = 8;

    /// A 7 x 34 grid at 10 km: ocean in column 0; a one-node-wide valley floor along row 3
    /// rising 10 m per column, its walls sloping down to it from both sides; and at column
    /// [`DAM`] a one-node rim 200 m above the floor. Upstream of the rim the floor is a
    /// depression of 19 nodes, columns `DAM + 1 ..= DAM + 19`, spilling at `10 DAM + 200` m.
    fn dammed_valley() -> (GridGraph, Vec<f32>) {
        let g = GridGraph {
            rows: 7,
            cols: 34,
            spacing_m: 10_000.0,
        };
        let elevation = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (3, DAM) => 10.0 * DAM as f32 + 200.0,
                (3, c) => 10.0 * c as f32,
                (r, c) => 10.0 * c as f32 + 300.0 + 200.0 * (r as f32 - 3.0).abs(),
            })
            .collect();
        (g, elevation)
    }

    const VALLEY_SPILL: f32 = 10.0 * DAM as f32 + 200.0;

    /// The valley's floor nodes below `level` m in the basin.
    fn floor_below(g: &GridGraph, level: f32) -> Vec<u32> {
        (DAM + 1..=DAM + 19)
            .map(|c| g.node(3, c))
            .filter(|&v| 10.0 * (g.row_col(v).1 as f32) < level)
            .collect()
    }

    /// The valley's pass with capacity `capacity` per node, data lakes on or off, and `mapped`
    /// nodes. Own runoff is 1 m3/s per node.
    fn valley(capacity: f64, data_lakes: bool, mapped: &[(u32, f32)]) -> (Routed, Accumulation) {
        let (g, elevation) = dammed_valley();
        let mut r = route(&g, elevation);
        assert_eq!(r.basins.len(), 1);
        assert_eq!(r.basins[0].spill_m, VALLEY_SPILL);
        assert_eq!(r.basins[0].nodes, floor_below(&g, f32::INFINITY));
        let (acc, _, _) = accumulate_with(
            &g,
            &mut r,
            |_| 1.0,
            |_| capacity,
            |_| false,
            data_lakes,
            4,
            &[],
            mapped,
        );
        (r, acc)
    }

    #[test]
    fn a_dammed_humid_valley_is_cut_through() {
        let (g, _) = dammed_valley();
        let rim = g.node(3, DAM);
        // The hazard: by the balance alone (a reference era) the valley fills to the rim.
        let (_, filled) = valley(0.01, false, &[]);
        let lake = &filled.lakes[0];
        assert_eq!((lake.kind, lake.level_m), (LakeKind::Open, VALLEY_SPILL));
        assert!(lake.inflow_m3s > 100.0, "{lake:?}");

        // Today's terrain, and no lake on the map: no lake.
        let (r, acc) = valley(0.01, true, &[]);
        assert!(acc.lakes.is_empty(), "{:?}", acc.lakes);
        assert!(acc.lake_nodes.is_empty());
        // A river chain along the floor from the valley head through the spill to the sea.
        let mut at = g.node(3, g.cols - 1);
        let mut chain = vec![at];
        while let Some(j) = receiver_of(&g, &r, at) {
            assert!(g.neighbours(at).contains(&Some(j)), "{at} -> {j} jumps");
            at = j;
            chain.push(j);
        }
        let floor: Vec<u32> = (0..g.cols).rev().map(|c| g.node(3, c)).collect();
        assert_eq!(chain, floor);
        for w in chain.windows(2).filter(|w| !r.ocean[w[1] as usize]) {
            assert!(acc.q[w[1] as usize] > acc.q[w[0] as usize], "{w:?}");
        }
        assert!(f64::from(acc.q[g.node(3, DAM + 1) as usize]) >= RIVER_MIN_DISCHARGE_M3S);
        // Below the rim: the whole inflow, nothing evaporated.
        let want = 1.0 + lake.inflow_m3s;
        let got = f64::from(acc.q[rim as usize]);
        assert!((got - want).abs() <= 1e-4 * want, "{got} vs {want}");
        assert_eq!(acc.evaporated_m3s, 0.0);
    }

    #[test]
    fn an_open_lake_above_its_mapped_surface_drops_to_it() {
        let (g, _) = dammed_valley();
        let rim = g.node(3, DAM);
        // The map's lake covers the floor up to 30 m under the spill; its surface 50 m under.
        let surface = VALLEY_SPILL - 50.0;
        let mapped: Vec<(u32, f32)> = floor_below(&g, VALLEY_SPILL - 30.0)
            .into_iter()
            .map(|v| (v, surface))
            .collect();
        let (_, open) = valley(0.01, false, &[]);
        let q_in = open.lakes[0].inflow_m3s;
        let (r, acc) = valley(0.01, true, &mapped);
        assert_eq!(acc.lakes.len(), 1, "{:?}", acc.lakes);
        let lake = &acc.lakes[0];
        assert_eq!(lake.kind, LakeKind::Data);
        assert!((lake.level_m - surface).abs() <= 0.01, "{lake:?}");
        assert_eq!(lake.spill_m, VALLEY_SPILL);
        // The footprint: the mapped lake's own nodes (round 2), not every basin node below the
        // surface; some stand above it.
        let footprint: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
        let own: Vec<u32> = mapped.iter().map(|m| m.0).collect();
        assert_eq!(footprint, own);
        assert!(floor_below(&g, surface).len() < own.len());
        // It passes its inflow less what the footprint evaporates over the rim.
        assert_eq!(lake.inflow_m3s, q_in);
        assert!((lake.evaporation_m3s - 0.01 * own.len() as f64).abs() < 1e-9);
        assert_eq!(lake.outflow_m3s, lake.inflow_m3s - lake.evaporation_m3s);
        assert_eq!(receiver_of(&g, &r, lake.exit), Some(rim));
        let got = f64::from(acc.q[rim as usize]);
        assert!(
            (got - (1.0 + lake.outflow_m3s)).abs() < 1e-3,
            "{got} {lake:?}"
        );
    }

    #[test]
    fn a_mapped_open_lake_keeps_its_spill() {
        let (g, _) = dammed_valley();
        let (_, open) = valley(0.01, false, &[]);
        for surface in [VALLEY_SPILL - 9.5, VALLEY_SPILL, VALLEY_SPILL + 30.0] {
            let mapped: Vec<(u32, f32)> = floor_below(&g, 1e9)
                .into_iter()
                .map(|v| (v, surface))
                .collect();
            let (_, acc) = valley(0.01, true, &mapped);
            assert_eq!(acc.lakes, open.lakes, "{surface}");
            assert_eq!(acc.lake_nodes, open.lake_nodes);
        }
    }

    #[test]
    fn an_arid_closed_basin_still_fills_by_balance() {
        let (_, reference) = valley(20.0, false, &[]);
        assert_eq!(reference.lakes.len(), 1);
        assert_eq!(reference.lakes[0].kind, LakeKind::Terminal);
        // Unmapped on today's terrain: a terminal lake stays (Mega-Chad, Bonneville).
        let (_, acc) = valley(20.0, true, &[]);
        assert_eq!(acc.lakes, reference.lakes);
        assert_eq!(acc.lake_nodes, reference.lake_nodes);
    }

    /// A 6 x 24 grid at 10 km: ocean in column 0; a river along rows 2..=3 rising 8 m per column
    /// from the sea to a flat at exactly 50 m in columns 7..=16 (20 nodes, 2 x 10), rising 5 m
    /// per column above it; banks 150 m above the channel. The flat drains by its two
    /// downstream nodes, like a wide river's flats in F16 data.
    fn river_flat() -> (GridGraph, Vec<f32>, Vec<u32>) {
        let g = GridGraph {
            rows: 6,
            cols: 24,
            spacing_m: 10_000.0,
        };
        let mut flat = Vec::new();
        let elevation = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (2..=3, 7..=16) => {
                    flat.push(i);
                    50.0
                }
                (2..=3, c @ 1..=6) => 8.0 * c as f32,
                (2..=3, c) => 50.0 + 5.0 * (c as f32 - 16.0),
                (_, c) => 8.0 * c as f32 + 150.0,
            })
            .collect();
        (g, elevation, flat)
    }

    fn river_flat_lakes(mapped: &[(u32, f32)]) -> (Accumulation, Vec<u32>) {
        let (g, elevation, flat) = river_flat();
        let mut r = route(&g, elevation);
        assert!(r.basins.is_empty());
        let (acc, _, _) = accumulate_with(
            &g,
            &mut r,
            |_| 1.0,
            |_| 0.1,
            |_| false,
            true,
            6,
            &[],
            mapped,
        );
        (acc, flat)
    }

    #[test]
    fn an_unmapped_flat_is_not_a_lake() {
        let (g, _, flat) = river_flat();
        assert_eq!(flat.len(), 20);
        let (acc, _) = river_flat_lakes(&[]);
        assert!(acc.lakes.is_empty(), "{:?}", acc.lakes);
        // One mapped node (a small lake beside the river) is not enough.
        let (acc, _) = river_flat_lakes(&[(g.node(2, 12), 50.0)]);
        assert!(acc.lakes.is_empty(), "{:?}", acc.lakes);
    }

    #[test]
    fn a_mapped_flat_is_a_lake() {
        let (g, _, _) = river_flat();
        let mapped = [(g.node(2, 12), 50.0), (g.node(3, 13), 50.0)];
        let (acc, flat) = river_flat_lakes(&mapped);
        assert_eq!(acc.lakes.len(), 1, "{:?}", acc.lakes);
        let lake = &acc.lakes[0];
        assert_eq!((lake.kind, lake.level_m), (LakeKind::Data, 50.0));
        let footprint: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
        assert_eq!(footprint, flat);
        assert!(lake.outflow_m3s > 0.0);
    }

    #[test]
    fn a_depression_data_lake_needs_the_map() {
        // A 7 x 8 grid at 10 km: ocean in column 0, 90 m walls with a 50 m gap at (3, 1), a
        // 30 m basin bed and a 6-node floor at exactly 5 m: the data's lake surface, if the map
        // holds a lake there.
        let g = GridGraph {
            rows: 7,
            cols: 8,
            spacing_m: 10_000.0,
        };
        let mut floor = Vec::new();
        let elevation: Vec<f32> = (0..g.len())
            .map(|i| match g.row_col(i) {
                (_, 0) => -10.0,
                (3, 1) => 50.0,
                (2..=4, 3..=4) => {
                    floor.push(i);
                    5.0
                }
                (1..=5, 2..=6) => 30.0,
                _ => 90.0,
            })
            .collect();
        let lakes = |mapped: &[(u32, f32)]| {
            let mut r = route(&g, elevation.clone());
            assert_eq!(r.basins.len(), 1);
            let (acc, _, _) = accumulate_with(
                &g,
                &mut r,
                |_| 1.0,
                |_| 2.0,
                |_| false,
                true,
                5,
                &[],
                mapped,
            );
            acc.lakes
        };
        let data = |l: &Vec<Lake>| l.iter().any(|l| l.kind == LakeKind::Data);
        assert!(!data(&lakes(&[])), "{:?}", lakes(&[]));
        assert!(!data(&lakes(&[(floor[0], 5.0)])));
        let two = lakes(&[(floor[0], 5.0), (floor[5], 5.0)]);
        assert_eq!(two.len(), 1);
        assert_eq!(
            (two[0].kind, two[0].level_m),
            (LakeKind::Data, 5.0),
            "{two:?}"
        );
    }

    /// The dammed valley at the present day or not, with `capacity` per node and `mapped`
    /// nodes: `(routing, accumulation)`.
    fn valley_at(capacity: f64, present: bool, mapped: &[(u32, f32)]) -> (Routed, Accumulation) {
        let (g, elevation) = dammed_valley();
        let mut r = route(&g, elevation);
        let (acc, _, _) = accumulate_full(
            &g,
            &mut r,
            |_| 1.0,
            |_| capacity,
            |_| false,
            true,
            present,
            3,
            &[],
            mapped,
        );
        (r, acc)
    }

    /// Two mapped lakes on the valley floor: A over columns `DAM + 2 ..= DAM + 7` (6 nodes) at
    /// `spill − 60`, B over `DAM + 12 ..= DAM + 14` (3 nodes) at `spill − 40`; the floor between
    /// them is unmapped and below both surfaces.
    fn two_lakes(g: &GridGraph) -> (Vec<(u32, f32)>, Vec<u32>, Vec<u32>) {
        let a: Vec<u32> = (DAM + 2..=DAM + 7).map(|c| g.node(3, c)).collect();
        let b: Vec<u32> = (DAM + 12..=DAM + 14).map(|c| g.node(3, c)).collect();
        let mapped = a
            .iter()
            .map(|&v| (v, VALLEY_SPILL - 60.0))
            .chain(b.iter().map(|&v| (v, VALLEY_SPILL - 40.0)))
            .collect();
        (mapped, a, b)
    }

    #[test]
    fn two_mapped_lakes_in_one_basin_flood_nothing_between_them() {
        let (g, _) = dammed_valley();
        let (mapped, a, b) = two_lakes(&g);
        let between: Vec<u32> = (DAM + 8..=DAM + 11).map(|c| g.node(3, c)).collect();
        // The hazard: the floor between the lakes stands below both surfaces.
        assert!(
            between
                .iter()
                .all(|&v| 10.0 * (g.row_col(v).1 as f32) < VALLEY_SPILL - 60.0)
        );
        // A past era: one lake per basin, the mapped lake with the most basin nodes, over its
        // own nodes alone.
        let (_, past) = valley_at(0.01, false, &mapped);
        assert_eq!(past.lakes.len(), 1, "{:?}", past.lakes);
        assert_eq!(past.lakes[0].level_m, VALLEY_SPILL - 60.0);
        // The basin's exit joins the footprint as its outlet.
        let footprint: Vec<u32> = past.lake_nodes.iter().map(|n| n.node).collect();
        assert_eq!(footprint, [vec![g.node(3, DAM + 1)], a.clone()].concat());
        // The present day: each mapped lake over its own nodes, at its own surface.
        let (_, today) = valley_at(0.01, true, &mapped);
        assert_eq!(today.lakes.len(), 2, "{:?}", today.lakes);
        let mut levels: Vec<f32> = today.lakes.iter().map(|l| l.level_m).collect();
        levels.sort_by(f32::total_cmp);
        assert_eq!(levels, [VALLEY_SPILL - 60.0, VALLEY_SPILL - 40.0]);
        let footprint: Vec<u32> = today.lake_nodes.iter().map(|n| n.node).collect();
        let mut both = [a.clone(), b.clone()].concat();
        both.sort_unstable();
        assert_eq!(footprint, both);
        assert!(between.iter().all(|v| !footprint.contains(v)));
    }

    #[test]
    fn at_the_present_day_the_map_decides_every_lake() {
        let (g, elevation) = dammed_valley();
        let (mapped, a, b) = two_lakes(&g);
        let sink = g.node(3, DAM + 1);
        let own_total = (0..g.len())
            .filter(|&v| elevation[v as usize] >= 0.0)
            .count() as f64;
        for (capacity, open) in [(0.01, true), (40.0, false)] {
            let (r, acc) = valley_at(capacity, true, &mapped);
            assert_eq!(acc.lakes.len(), 2, "{capacity}: {:?}", acc.lakes);
            let lake_of = |nodes: &[u32]| {
                acc.lakes
                    .iter()
                    .find(|l| nodes.contains(&l.exit))
                    .expect("a lake on these nodes")
            };
            let (la, lb) = (lake_of(&a), lake_of(&b));
            for l in [la, lb] {
                assert_eq!(l.kind, LakeKind::Data);
                assert!((l.inflow_m3s - l.evaporation_m3s - l.outflow_m3s).abs() < 1e-9);
            }
            // Each lake's evaporating surface is its own nodes.
            assert!((la.evaporation_m3s.min(capacity * 6.0) - la.evaporation_m3s).abs() < 1e-9);
            if open {
                // Humid: B overflows into A, A over the rim to the sea.
                assert!(
                    lb.outflow_m3s > 0.0 && la.outflow_m3s > 0.0,
                    "{lb:?} {la:?}"
                );
                assert!(la.inflow_m3s > lb.outflow_m3s, "{la:?} {lb:?}");
                assert!((lb.evaporation_m3s - 0.03).abs() < 1e-9, "{lb:?}");
            } else {
                // Arid: the basin is closed. B evaporates all it gets...
                assert_eq!(lb.outflow_m3s, 0.0, "{lb:?}");
                // ... and the basin's sink, unmapped, holds no lake: water ends there dry.
                assert!(acc.lake_nodes.iter().all(|n| n.node != sink));
                assert_eq!(receiver_of(&g, &r, sink), None);
            }
            // Conservation: own runoff = what reaches the sea + what evaporates.
            let mut out = 0.0;
            for i in 0..g.len() {
                if r.ocean[i as usize] || acc.lake_nodes.iter().any(|n| n.node == i) {
                    continue;
                }
                if receiver_of(&g, &r, i).is_some_and(|j| r.ocean[j as usize]) {
                    out += f64::from(acc.q[i as usize]);
                }
            }
            for l in &acc.lakes {
                if receiver_of(&g, &r, l.exit).is_some_and(|j| r.ocean[j as usize]) {
                    out += l.outflow_m3s;
                }
            }
            let total = out + acc.evaporated_m3s;
            assert!(
                (total - own_total).abs() < 1e-3 * own_total,
                "{total} vs {own_total}"
            );
        }
        // No mapped lake: no lake at all, even in the closed basin.
        let (r, acc) = valley_at(20.0, true, &[]);
        assert!(acc.lakes.is_empty() && acc.lake_nodes.is_empty());
        assert_eq!(receiver_of(&g, &r, sink), None);
    }

    #[test]
    fn a_present_day_lake_needs_no_node_below_its_surface() {
        // The Dead Sea's case: the lattice's lowest node stands above the lake's fine surface.
        // And the Great Salt Lake's: an inflow far above what the flat evaporates (no veto).
        let (g, _) = dammed_valley();
        let nodes: Vec<u32> = (DAM + 1..=DAM + 3).map(|c| g.node(3, c)).collect();
        let surface = 10.0 * DAM as f32 - 50.0;
        let mapped: Vec<(u32, f32)> = nodes.iter().map(|&v| (v, surface)).collect();
        let (_, acc) = valley_at(0.01, true, &mapped);
        assert_eq!(acc.lakes.len(), 1, "{:?}", acc.lakes);
        let l = &acc.lakes[0];
        assert_eq!((l.kind, l.level_m), (LakeKind::Data, surface));
        let footprint: Vec<u32> = acc.lake_nodes.iter().map(|n| n.node).collect();
        assert_eq!(footprint, nodes);
        assert!(l.inflow_m3s > DATA_LAKE_MAX_BALANCE * l.evaporation_m3s.max(1e-9));
    }

    #[test]
    fn a_present_day_lake_on_a_closed_basins_sink_evaporates_all_it_gets() {
        // The Great Salt Lake's case: the lake covers the basin's sink and gets more than its
        // surface evaporates, but the basin is closed (its other nodes evaporate the rest).
        let (g, elevation) = dammed_valley();
        let lake: Vec<u32> = (DAM + 1..=DAM + 3).map(|c| g.node(3, c)).collect();
        let mapped: Vec<(u32, f32)> = lake
            .iter()
            .map(|&v| (v, 10.0 * DAM as f32 + 40.0))
            .collect();
        let mut r = route(&g, elevation);
        let basin = r.basins[0].nodes.clone();
        let capacity = |v: u32| {
            if lake.contains(&v) {
                0.1
            } else if basin.contains(&v) {
                100.0
            } else {
                0.0
            }
        };
        let (acc, _, _) = accumulate_full(
            &g,
            &mut r,
            |_| 1.0,
            capacity,
            |_| false,
            true,
            true,
            3,
            &[],
            &mapped,
        );
        assert_eq!(acc.lakes.len(), 1, "{:?}", acc.lakes);
        let l = &acc.lakes[0];
        assert!(
            l.inflow_m3s > 0.3,
            "the hazard: more than the surface evaporates: {l:?}"
        );
        assert_eq!(
            receiver_of(&g, &r, l.exit),
            None,
            "the lake's outlet is the sink"
        );
        assert_eq!((l.outflow_m3s, l.evaporation_m3s), (0.0, l.inflow_m3s));
    }

    #[test]
    fn reference_eras_ignore_the_map_in_the_golden_cases() {
        // Golden cases A-C (data lakes off) with every node mapped far below its ground: the
        // map changes nothing.
        let fx: Value = serde_json::from_str(GOLDEN).unwrap();
        let (g, elevation) = golden_grid(&fx);
        let mapped: Vec<(u32, f32)> = (0..g.len()).map(|v| (v, -500.0)).collect();
        for capacity in [0.0, 2.0, 5.0] {
            let mut a = route(&g, elevation.clone());
            let plain = accumulate(&g, &mut a, |_| 1.0, |_| capacity, |_| false, false, 3);
            let mut b = route(&g, elevation.clone());
            let (with_map, _, _) = accumulate_with(
                &g,
                &mut b,
                |_| 1.0,
                |_| capacity,
                |_| false,
                false,
                3,
                &[],
                &mapped,
            );
            assert_eq!(plain.lakes, with_map.lakes);
            assert_eq!(plain.lake_nodes, with_map.lake_nodes);
            assert_eq!(plain.q, with_map.q);
            assert_eq!(a, b);
        }
    }
}
