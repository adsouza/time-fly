//! Discharge and the lake water balance (rivers and lakes design §4.1, steps 3 and 4).
//!
//! The rules, which the golden fixture (`tests/fixtures/hydro_golden.json`) and the Python
//! reference (Task 11) pin. `e` is a node's elevation, `c` its evaporation capacity (net
//! evaporation × area, m³/s; 0 on ice), "tol" is `DATA_LAKE_FLAT_TOL_M`.
//!
//! ```text
//! Data flats (only if data_lakes), before discharge, after routing:
//!   eligible: non-ocean, non-ice, in no depression basin.
//!   Scan node ids ascending; each eligible node not yet in a component seeds one. Grow it by BFS
//!     (FIFO, neighbour order) over eligible free neighbours, taking a node only if the
//!     component's max e − min e stays ≤ tol. Every grown node is taken, kept or not.
//!   Keep components of ≥ DATA_LAKE_FLAT_MIN_NODES nodes. Each kept flat becomes a basin appended
//!     after routing's: its nodes, spill_m = its minimum e, exit = its node first in pop order,
//!     rim = the exit's receiver. Every other flat node whose receiver leaves the flat (or has
//!     none) is redirected to the exit.
//! Discharge: own runoff per land node, then nodes in reverse pop order. A node outside every
//!   basin adds its q to its receiver (none into the ocean). A basin node other than the exit
//!   keeps its q (own runoff + inflow from outside the basin). At the exit the basin resolves:
//!   Q_in = Σ q over its nodes, ascending node id, in f64.
//! A flat basin: if Q_in ≥ DATA_LAKE_MIN_BALANCE × Σc: Data lake at level = spill = min e,
//!   footprint = all its nodes, outflow = max(0, Q_in − Σc), evaporation = Q_in − outflow.
//!   Otherwise no lake (it passes Q_in on). Either way it routes as a spilling basin (below).
//! A depression basin:
//!   - Data (only if data_lakes): F = the connected non-ice basin nodes within tol of the basin
//!     minimum (ties: lowest id), grown by BFS from it. If |F| ≥ DATA_LAKE_MIN_NODES and
//!     DATA_LAKE_MIN_BALANCE × Σ_F c ≤ Q_in ≤ DATA_LAKE_MAX_BALANCE × Σ_F c: level = median e of F
//!     (mean of the middle two for an even count); footprint = (F ∪ {e < level}) minus ice;
//!     outflow = max(0, Q_in − Σ_footprint c) if level ≥ spill − tol, else 0.
//!   - Otherwise the balance: group basin nodes by distinct e, ascending; C_g = Σc through g.
//!     C_last < Q_in: Open, level = spill, evaporation = C_last, outflow = Q_in − C_last,
//!       footprint = all non-ice basin nodes; no lake if spill − min e < MIN_LAKE_DEPTH_M.
//!     Else Terminal: g = first group with C_g ≥ Q_in, level = e_g + (Q_in − C_{g−1}) /
//!       (C_g − C_{g−1}) × (e_{g+1} − e_g) (0 when that step is 0; e_{last+1} = spill),
//!       evaporation = Q_in, outflow = 0; sinks = nodes with e ≤ e_g; footprint = sinks minus
//!       ice; no lake if the footprint is empty, or if level − min e < MIN_LAKE_DEPTH_M and
//!       Q_in < RIVER_MIN_DISCHARGE_M3S (a playa).
//! Routing inside a basin that passes water on (Open, Data with outflow, a flat, no lake):
//!   receivers stay routing's (a tree into the exit); re-accumulate q and area along it in
//!   descending tree depth, ties ascending id. A Data lake with outflow adds its exit to the
//!   footprint unless ice. The outflow (the lake's, or q[exit] with no lake) and the exit's area
//!   go to the exit's receiver (the rim, or a lower neighbour outside), unless ocean.
//! Routing inside any other basin (Terminal, a playa, Data without outflow):
//!   sinks (Data: F ∪ {e < level}) keep their water and have no receiver. Every other node routes
//!   to the neighbour inside the basin one BFS step closer to the sinks with the lowest e (ties:
//!   neighbour order); re-accumulate in descending BFS distance, ties ascending id.
//! Shore steps: 1 on a footprint node with any neighbour off the footprint, then BFS inward.
//! ```

use std::collections::VecDeque;

use super::params::{
    DATA_LAKE_FLAT_MIN_NODES, DATA_LAKE_FLAT_TOL_M, DATA_LAKE_MAX_BALANCE, DATA_LAKE_MIN_BALANCE,
    DATA_LAKE_MIN_NODES, MIN_LAKE_DEPTH_M, RIVER_MIN_DISCHARGE_M3S,
};
use super::{HydroGraph, NO_BASIN, RECEIVER_EXIT, RECEIVER_NONE, Routed};

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
}

/// The data's flats outside every depression, found resumably and registered as basins after
/// routing's own ([`Flats::register`]).
pub(crate) struct Flats {
    /// The next node id to scan.
    next: u32,
    /// One bit per node: already in a component.
    taken: Vec<u64>,
    /// One bit per node: in a kept flat.
    member: Vec<u64>,
    queue: VecDeque<u32>,
    /// Kept components, each sorted by node.
    flats: Vec<Vec<u32>>,
    /// `(node, flat)` for every kept node, sorted by node once the scan is done.
    lookup: Vec<(u32, u32)>,
    /// Each flat's exit (its node popped first), `u32::MAX` until found.
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

impl Flats {
    pub fn new(len: u32) -> Self {
        let words = len.div_ceil(64) as usize;
        Self {
            next: 0,
            taken: vec![0; words],
            member: vec![0; words],
            queue: VecDeque::new(),
            flats: Vec::new(),
            lookup: Vec::new(),
            exits: Vec::new(),
            pos: 0,
            found: 0,
            scanned: false,
        }
    }

    pub fn working_bytes(&self) -> usize {
        8 * (self.taken.capacity() + self.member.capacity() + self.lookup.capacity())
            + 4 * (self.queue.capacity()
                + self.exits.capacity()
                + self.flats.iter().map(Vec::capacity).sum::<usize>())
    }

    /// Scans for flats in ascending node id, then finds each one's exit. Returns `(done, units
    /// used)`.
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
        while used < budget {
            used += 1;
            if self.next < len {
                let seed = self.next;
                self.next += 1;
                if !free(&self.taken, seed) {
                    continue;
                }
                // Grow while the component's range stays within the tolerance.
                let e = |i: u32| routed.elevation_m[i as usize];
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
                    let f = self.flats.len() as u32;
                    for &v in &component {
                        set_bit(&mut self.member, v);
                        self.lookup.push((v, f));
                    }
                    self.flats.push(component);
                }
                continue;
            }
            if !self.scanned {
                self.scanned = true;
                self.taken = Vec::new();
                self.queue = VecDeque::new();
                self.lookup.sort_unstable();
                self.exits = vec![u32::MAX; self.flats.len()];
                continue;
            }
            if self.found == self.flats.len() || self.pos as usize == routed.order.len() {
                return (true, used);
            }
            let v = routed.order[self.pos as usize];
            self.pos += 1;
            if bit_of(&self.member, v) {
                let k = self
                    .lookup
                    .binary_search_by_key(&v, |&(n, _)| n)
                    .expect("a member is listed");
                let f = self.lookup[k].1 as usize;
                if self.exits[f] == u32::MAX {
                    self.exits[f] = v;
                    self.found += 1;
                }
            }
        }
        (false, used)
    }

    /// Appends each flat to `routed.basins`, with its lowest elevation as its `spill_m` and its
    /// exit's receiver as its rim, and redirects every receiver that leaves the flat to the
    /// exit. Returns the index of the first flat basin.
    pub fn register(self, g: &impl HydroGraph, routed: &mut Routed) -> u32 {
        let first = routed.basins.len() as u32;
        for (f, nodes) in self.flats.into_iter().enumerate() {
            let exit = self.exits[f];
            assert_ne!(exit, u32::MAX, "every flat node is popped");
            let b = first + f as u32;
            let rim = super::receiver_of(g, routed, exit).unwrap_or(exit);
            let level = nodes
                .iter()
                .map(|&v| routed.elevation_m[v as usize])
                .fold(f32::INFINITY, f32::min);
            for &v in &nodes {
                routed.basin_of[v as usize] = b;
                if v == exit {
                    continue;
                }
                let to = match routed.receiver[v as usize] {
                    RECEIVER_NONE => None,
                    k => g.neighbours(v)[usize::from(k)],
                };
                if to.is_none_or(|j| nodes.binary_search(&j).is_err()) {
                    routed.receiver[v as usize] = RECEIVER_EXIT;
                }
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
            q: vec![0.0; len as usize],
            area_km2: vec![0.0; len as usize],
            lakes: Vec::new(),
            lake_nodes: Vec::new(),
            evaporated_m3s: 0.0,
            next_init: 0,
            next_walk: 0,
        }
    }

    pub fn working_bytes(&self) -> usize {
        4 * (self.q.capacity() + self.area_km2.capacity())
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
        (self.next_init == g.len(), used)
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
        let done = self.next_walk == len;
        if done {
            self.lake_nodes.sort_unstable_by_key(|n| n.node);
        }
        (done, used)
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
        const NOT_IN: u32 = u32::MAX;
        let inside: Vec<[u32; 8]> = nodes
            .iter()
            .map(|&v| {
                g.neighbours(v).map(|j| {
                    j.and_then(|j| nodes.binary_search(&j).ok())
                        .map_or(NOT_IN, |k| k as u32)
                })
            })
            .collect();
        let neighbours_in = |k: usize| inside[k].map(|j| (j != NOT_IN).then_some(j as usize));
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
        if data_lakes && !iced[kmin] && !is_flat {
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
                data = Some((level, flat));
            }
        }

        let outcome = if is_flat {
            // The data's flat: a lake at its lowest level that passes its inflow on, less what
            // its surface evaporates, if the inflow meets the veto. Its nodes are never ice.
            let footprint = vec![true; m];
            let capacity: f64 = cap.iter().sum();
            if q_in >= DATA_LAKE_MIN_BALANCE * capacity {
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
            match bal.kind {
                LakeKind::Open => {
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
                _ => {
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

        let lake = match outcome {
            Outcome::Spill { lake } => {
                // Depth in routing's tree, which every basin node but the exit drains along.
                let parent: Vec<usize> = (0..m)
                    .map(|k| {
                        if k == exit_k {
                            return k;
                        }
                        let v = nodes[k];
                        let to = match receiver[v as usize] {
                            RECEIVER_EXIT => basin.exit,
                            r => g.neighbours(v)[usize::from(r)].expect("a basin receiver"),
                        };
                        local(to).expect("basin receivers stay in the basin")
                    })
                    .collect();
                let mut depth = vec![u32::MAX; m];
                depth[exit_k] = 0;
                let mut path = Vec::new();
                for k in 0..m {
                    let mut at = k;
                    while depth[at] == u32::MAX {
                        path.push(at);
                        at = parent[at];
                    }
                    let mut d = depth[at];
                    while let Some(p) = path.pop() {
                        d += 1;
                        depth[p] = d;
                    }
                }
                let mut by_depth: Vec<usize> = (0..m).filter(|&k| k != exit_k).collect();
                by_depth.sort_by(|&a, &b| depth[b].cmp(&depth[a]).then(a.cmp(&b)));
                for k in by_depth {
                    let (from, to) = (nodes[k] as usize, nodes[parent[k]] as usize);
                    self.q[to] += self.q[from];
                    self.area_km2[to] += self.area_km2[from];
                }
                let exit = basin.exit as usize;
                let out = match &lake {
                    Some((l, _)) => {
                        self.evaporated_m3s += l.evaporation_m3s;
                        l.outflow_m3s as f32
                    }
                    None => self.q[exit],
                };
                // The exit drains over the rim, or to a lower neighbour outside the basin.
                let down = match receiver[exit] {
                    RECEIVER_NONE => None,
                    k => g.neighbours(basin.exit)[usize::from(k)],
                };
                if let Some(j) = down.filter(|&j| !ocean[j as usize]) {
                    self.q[j as usize] += out;
                    self.area_km2[j as usize] += self.area_km2[exit];
                }
                lake
            }
            Outcome::Sink {
                lake,
                sinks,
                evaporation_m3s,
            } => {
                self.evaporated_m3s += evaporation_m3s;
                let mut dist = vec![u32::MAX; m];
                let mut queue: VecDeque<usize> = (0..m).filter(|&k| sinks[k]).collect();
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
                for k in 0..m {
                    let v = nodes[k] as usize;
                    if sinks[k] {
                        receiver[v] = RECEIVER_NONE;
                        continue;
                    }
                    let mut best: Option<(usize, usize)> = None;
                    for (slot, j) in neighbours_in(k).into_iter().enumerate() {
                        let Some(j) = j else { continue };
                        if dist[j] < dist[k] && best.is_none_or(|(_, b)| e[j] < e[b]) {
                            best = Some((slot, j));
                        }
                    }
                    let (slot, j) = best.expect("a basin is connected to its sinks");
                    receiver[v] = slot as u8;
                    target[k] = j;
                }
                let mut by_dist: Vec<usize> = (0..m).filter(|&k| !sinks[k]).collect();
                by_dist.sort_by(|&a, &b| dist[b].cmp(&dist[a]).then(a.cmp(&b)));
                for k in by_dist {
                    let (from, to) = (nodes[k] as usize, nodes[target[k]] as usize);
                    self.q[to] += self.q[from];
                    self.area_km2[to] += self.area_km2[from];
                }
                lake
            }
        };

        if let Some((lake, footprint)) = lake {
            let id = u16::try_from(lake_id).expect("at most 65,535 lakes per era");
            // Shore distance: 1 where a neighbour is not lake, then BFS inward.
            let mut steps = vec![0u16; m];
            let mut queue = VecDeque::new();
            for k in (0..m).filter(|&k| footprint[k]) {
                let shore = neighbours_in(k)
                    .iter()
                    .any(|j| j.is_none_or(|j| !footprint[j]));
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
        let first_flat = if data_lakes {
            let mut flats = Flats::new(g.len());
            while !flats.step(g, routed, &ice, budget).0 {}
            flats.register(g, routed)
        } else {
            routed.basins.len() as u32
        };
        let mut acc = Accumulation::new(g.len(), first_flat);
        while !acc.init_step(g, routed, |i, _| own(i), budget).0 {}
        while !acc
            .walk_step(g, routed, &ice, |i, _, _| capacity(i), data_lakes, budget)
            .0
        {}
        acc
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
        ) {
            // An 11 x 12 grid at 10 km: flats, pits, ice and several outlets.
            let g = GridGraph { rows: 11, cols: 12, spacing_m: 10_000.0 };
            let elevation: Vec<f32> = levels.iter().map(|&l| l as f32 * 3.0).collect();
            let iced = |i: u32| ice[i as usize] == 0;
            let mut r = route(&g, elevation.clone());
            let acc = accumulate(&g, &mut r, |i| 1.0 + (i % 3) as f32,
                |i| evap[i as usize], iced, data_lakes, 5);
            let mut own = 0.0;
            let mut out = 0.0;
            for i in 0..g.len() {
                let iu = i as usize;
                if r.ocean[iu] {
                    continue;
                }
                own += 1.0 + f64::from(i % 3);
                let sink = r.basin_of[iu] != NO_BASIN && r.receiver[iu] == RECEIVER_NONE;
                match receiver_of(&g, &r, i) {
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
            let balance = out + acc.evaporated_m3s;
            prop_assert!((balance - own).abs() <= 1e-4 * own, "{} out + {} evaporated vs {}",
                out, acc.evaporated_m3s, own);
            for lake in &acc.lakes {
                let sum = lake.evaporation_m3s + lake.outflow_m3s;
                prop_assert!((sum - lake.inflow_m3s).abs() <= 1e-9 * lake.inflow_m3s.max(1.0));
                prop_assert!(lake.level_m <= lake.spill_m);
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
            let acc = accumulate(&g, &mut r, |_| 1.0, |_| 0.1, |_| false, data_lakes, 4);
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
        // The pit gathers the whole basin's water: its inflow is the lake's.
        assert_eq!(f64::from(acc.q[pit as usize]), lake.inflow_m3s);
    }
}
