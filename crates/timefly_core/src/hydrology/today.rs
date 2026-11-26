//! The present day's lakes from the map (rivers Task 8b, round 2: at the present-day snapshot
//! the map decides every lake). Runs after the walk, resumably, when [`Accumulation`] is in
//! present mode. The walk has then made no lake but the table's: an open basin passes its water
//! through its spill, a closed one ends at its lowest node (its sink).
//!
//! ```text
//! next(v)  = v's receiver after the walk (a basin exit for RECEIVER_EXIT), none for the ocean,
//!            a sink, or a terminal table lake's sinks.
//! Units: each open table lake (outlet = its exit, capacity = its evaporation), then each mapped
//!   lake in ascending map id: footprint F = its mapped nodes that are not ocean, ice, or in a
//!   table lake's footprint (none left: no lake). Its outlet o is the F node, among those whose
//!   chain of next() meets no other F node, with the largest discharge after the walk (ties: the
//!   lowest id); every other F node's next becomes o (lakes in id order, so next() stays a
//!   forest). Capacity = Σ evaporation capacity over F.
//! Discharge again from each land node's own runoff, upstream first (Kahn's order on next()):
//!   a node adds its q and area to next (none into the ocean). At a unit's outlet Q_in = q there
//!   and the outflow is max(0, Q_in − capacity), or 0 when the outlet has no next (Terminal);
//!   it passes the outflow on. Water at a node with no next (not an outlet) stays: evaporated.
//! A mapped lake: kind Data at its surface (the highest of its nodes'), footprint F, inflow
//!   Q_in, outflow as above, evaporation Q_in − outflow, exit o, shore steps from F. Mapped
//!   lakes follow the table's lakes in the lake list, in map id order. Open table lakes take
//!   the new Q_in and outflow.
//! ```

use std::collections::VecDeque;

use super::lakes::{Accumulation, Lake, LakeKind, LakeNode};
use super::{HydroGraph, MappedLake, Routed, receiver_of};

const NONE: u32 = u32::MAX;

/// One outlet: a mapped lake or an open table lake.
struct Unit {
    outlet: u32,
    capacity: f64,
    /// `(map id, surface, footprint sorted)` for a mapped lake; `None` for the table's.
    mapped: Option<(u16, f32, Vec<u32>)>,
    /// The table lake's index in `Accumulation::lakes`.
    table: Option<usize>,
    q_in: f64,
    out: f64,
}

enum Phase {
    Next,
    Lakes,
    Indegree,
    Seed,
    Flow,
    Done,
}

/// The overlay's resumable state.
pub(crate) struct Today {
    phase: Phase,
    i: u32,
    next: Vec<u32>,
    groups: Vec<(u16, f32, Vec<u32>)>,
    units: Vec<Unit>,
    /// `(outlet, unit)`, sorted by outlet once the units are known.
    outlets: Vec<(u32, usize)>,
    indeg: Vec<u32>,
    queue: VecDeque<u32>,
    q: Vec<f32>,
    area_km2: Vec<f32>,
    evaporated_m3s: f64,
}

impl Today {
    /// The overlay over `len` nodes, starting from each land node's own runoff and area.
    pub fn new(own_q: Vec<f32>, own_area_km2: Vec<f32>) -> Self {
        let len = own_q.len();
        Self {
            phase: Phase::Next,
            i: 0,
            next: vec![NONE; len],
            groups: Vec::new(),
            units: Vec::new(),
            outlets: Vec::new(),
            indeg: Vec::new(),
            queue: VecDeque::new(),
            q: own_q,
            area_km2: own_area_km2,
            evaporated_m3s: 0.0,
        }
    }

    pub fn working_bytes(&self) -> usize {
        4 * (self.next.capacity()
            + self.indeg.capacity()
            + self.queue.capacity()
            + self.q.capacity()
            + self.area_km2.capacity())
            + self
                .groups
                .iter()
                .map(|g| 4 * g.2.capacity())
                .sum::<usize>()
    }

    /// Does up to `budget` units of the overlay; returns `(done, units used)`.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        acc: &mut Accumulation,
        mapped: &[(u32, MappedLake)],
        g: &impl HydroGraph,
        routed: &Routed,
        ice: &impl Fn(u32) -> bool,
        capacity: &mut impl FnMut(u32, f32, f64) -> f64,
        budget: u32,
    ) -> (bool, u32) {
        let len = g.len();
        let mut used = 0u32;
        while used < budget {
            used += 1;
            match self.phase {
                Phase::Next => {
                    if self.i == len {
                        self.start_lakes(acc, mapped, routed, ice);
                        continue;
                    }
                    let v = self.i;
                    self.i += 1;
                    if !routed.ocean[v as usize] {
                        self.next[v as usize] = receiver_of(g, routed, v).unwrap_or(NONE);
                    }
                }
                Phase::Lakes => {
                    let Some((id, surface, footprint)) = self.groups.pop() else {
                        for (k, u) in self.units.iter().enumerate() {
                            self.outlets.push((u.outlet, k));
                        }
                        self.outlets.sort_unstable();
                        self.indeg = vec![0; len as usize];
                        self.i = 0;
                        self.phase = Phase::Indegree;
                        continue;
                    };
                    used = used.saturating_add(
                        self.place(g, acc, routed, id, surface, footprint, capacity),
                    );
                }
                Phase::Indegree => {
                    if self.i == len {
                        self.i = 0;
                        self.phase = Phase::Seed;
                        continue;
                    }
                    let n = self.next[self.i as usize];
                    if n != NONE {
                        self.indeg[n as usize] += 1;
                    }
                    self.i += 1;
                }
                Phase::Seed => {
                    if self.i == len {
                        self.phase = Phase::Flow;
                        continue;
                    }
                    let v = self.i;
                    self.i += 1;
                    if !routed.ocean[v as usize] && self.indeg[v as usize] == 0 {
                        self.queue.push_back(v);
                    }
                }
                Phase::Flow => {
                    let Some(v) = self.queue.pop_front() else {
                        self.finish(acc, g);
                        self.phase = Phase::Done;
                        continue;
                    };
                    self.flow(routed, v);
                }
                Phase::Done => return (true, used),
            }
        }
        (matches!(self.phase, Phase::Done), used)
    }

    /// Groups the mapped nodes into lakes and lists the open table lakes as units.
    fn start_lakes(
        &mut self,
        acc: &Accumulation,
        mapped: &[(u32, MappedLake)],
        routed: &Routed,
        ice: &impl Fn(u32) -> bool,
    ) {
        for (k, lake) in acc.lakes.iter().enumerate() {
            if lake.outflow_m3s > 0.0 {
                self.units.push(Unit {
                    outlet: lake.exit,
                    capacity: lake.evaporation_m3s,
                    mapped: None,
                    table: Some(k),
                    q_in: 0.0,
                    out: 0.0,
                });
            }
        }
        let table = |v: u32| acc.lake_nodes.binary_search_by_key(&v, |n| n.node).is_ok();
        let mut by_id: Vec<(u16, u32, f32)> = mapped
            .iter()
            .filter(|&&(v, _)| !routed.ocean[v as usize] && !ice(v) && !table(v))
            .map(|&(v, l)| (l.id, v, l.surface_m))
            .collect();
        by_id.sort_by_key(|&(id, v, _)| (id, v));
        for chunk in by_id.chunk_by(|a, b| a.0 == b.0) {
            let surface = chunk.iter().map(|c| c.2).fold(f32::NEG_INFINITY, f32::max);
            self.groups
                .push((chunk[0].0, surface, chunk.iter().map(|c| c.1).collect()));
        }
        // Popped from the back: ascending id.
        self.groups.reverse();
        self.phase = Phase::Lakes;
    }

    /// Places one mapped lake: its outlet, the redirects, its capacity. Returns the units used.
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        g: &impl HydroGraph,
        acc: &Accumulation,
        routed: &Routed,
        id: u16,
        surface: f32,
        footprint: Vec<u32>,
        capacity: &mut impl FnMut(u32, f32, f64) -> f64,
    ) -> u32 {
        let inside = |v: u32| footprint.binary_search(&v).is_ok();
        let mut used = 0u32;
        let mut outlet: Option<u32> = None;
        for &v in &footprint {
            let mut at = self.next[v as usize];
            let mut meets = false;
            while at != NONE {
                used = used.saturating_add(1);
                if inside(at) {
                    meets = true;
                    break;
                }
                at = self.next[at as usize];
            }
            let better = outlet.is_none_or(|o| acc.q[v as usize] > acc.q[o as usize]);
            if !meets && better {
                outlet = Some(v);
            }
        }
        let outlet = outlet.expect("a forest's footprint has a downstream-most node");
        let mut cap = 0.0;
        for &v in &footprint {
            if v != outlet {
                self.next[v as usize] = outlet;
            }
            cap += capacity(v, routed.elevation_m[v as usize], g.area_m2(v));
        }
        self.units.push(Unit {
            outlet,
            capacity: cap,
            mapped: Some((id, surface, footprint)),
            table: None,
            q_in: 0.0,
            out: 0.0,
        });
        used
    }

    /// Passes node `v`'s water on (its outflow, at an outlet).
    fn flow(&mut self, routed: &Routed, v: u32) {
        let vi = v as usize;
        let n = self.next[vi];
        let unit = self
            .outlets
            .binary_search_by_key(&v, |&(o, _)| o)
            .ok()
            .map(|k| self.outlets[k].1);
        let pass = match unit {
            Some(k) => {
                let u = &mut self.units[k];
                u.q_in = f64::from(self.q[vi]);
                u.out = if n == NONE {
                    0.0
                } else {
                    (u.q_in - u.capacity).max(0.0)
                };
                self.evaporated_m3s += u.q_in - u.out;
                u.out as f32
            }
            None => {
                if n == NONE {
                    self.evaporated_m3s += f64::from(self.q[vi]);
                }
                self.q[vi]
            }
        };
        if n == NONE {
            return;
        }
        let ni = n as usize;
        if !routed.ocean[ni] {
            self.q[ni] += pass;
            self.area_km2[ni] += self.area_km2[vi];
        }
        self.indeg[ni] -= 1;
        if self.indeg[ni] == 0 && !routed.ocean[ni] {
            self.queue.push_back(n);
        }
    }

    /// Writes the discharge, the evaporation and the lakes back.
    fn finish(&mut self, acc: &mut Accumulation, g: &impl HydroGraph) {
        acc.q = std::mem::take(&mut self.q);
        acc.area_km2 = std::mem::take(&mut self.area_km2);
        acc.evaporated_m3s = self.evaporated_m3s;
        for u in &mut self.units {
            if let Some(k) = u.table {
                let lake = &mut acc.lakes[k];
                lake.inflow_m3s = u.q_in;
                lake.outflow_m3s = u.out;
                lake.evaporation_m3s = u.q_in - u.out;
            }
        }
        // Mapped lakes in ascending map id.
        let mut mapped: Vec<&mut Unit> = self
            .units
            .iter_mut()
            .filter(|u| u.mapped.is_some())
            .collect();
        mapped.sort_by_key(|u| u.mapped.as_ref().map(|m| m.0));
        for u in mapped {
            let (_, surface, footprint) = u.mapped.take().expect("a mapped unit");
            let id = u16::try_from(acc.lakes.len()).expect("at most 65,535 lakes per era");
            let area: f64 = footprint.iter().map(|&v| g.area_m2(v) * 1e-6).sum();
            acc.lakes.push(Lake {
                level_m: surface,
                spill_m: surface,
                kind: LakeKind::Data,
                inflow_m3s: u.q_in,
                evaporation_m3s: u.q_in - u.out,
                outflow_m3s: u.out,
                area_km2: area,
                exit: u.outlet,
                surplus_m3s: 0.0,
            });
            let inside = |v: u32| footprint.binary_search(&v).is_ok();
            let mut steps = vec![0u16; footprint.len()];
            let mut queue = VecDeque::new();
            for (k, &v) in footprint.iter().enumerate() {
                if g.neighbours(v).into_iter().flatten().any(|j| !inside(j)) {
                    steps[k] = 1;
                    queue.push_back(k);
                }
            }
            while let Some(k) = queue.pop_front() {
                for j in g.neighbours(footprint[k]).into_iter().flatten() {
                    if let Ok(kj) = footprint.binary_search(&j)
                        && steps[kj] == 0
                    {
                        steps[kj] = steps[k].saturating_add(1);
                        queue.push_back(kj);
                    }
                }
            }
            for (k, &v) in footprint.iter().enumerate() {
                acc.lake_nodes.push(LakeNode {
                    node: v,
                    lake: id,
                    shore_steps: steps[k],
                });
            }
        }
        acc.lake_nodes.sort_unstable_by_key(|n| n.node);
        self.next = Vec::new();
        self.indeg = Vec::new();
    }
}
