//! Discharge and the lake water balance (rivers and lakes design §4.1, steps 3 and 4).
//!
//! Discharge accumulates in reverse pop order. A basin's nodes hold their own runoff and the
//! inflow from outside the basin; at the basin's exit (the last of its nodes in reverse pop order)
//! the basin is resolved:
//!
//! - `Q_in` is the sum of its nodes' values, in ascending node id, in f64. Each non-ice node can
//!   evaporate `c_i` (net evaporation × area). Group the nodes by distinct elevation, ascending;
//!   `C_g` is the cumulative capacity through group `g`.
//! - **Data** (only for sources with present-day lake surfaces): the connected non-ice nodes
//!   within [`DATA_LAKE_FLAT_TOL_M`] of the basin minimum number at least
//!   [`DATA_LAKE_MIN_NODES`], and `Q_in ≥ DATA_LAKE_MIN_BALANCE ×` their capacity. The level is
//!   their median; the footprint is them plus the nodes below the level. The lake spills
//!   `max(0, Q_in − footprint capacity)` if the level is within the tolerance of the spill.
//! - Otherwise **Open** if `C_last < Q_in` (level = spill, evaporation `C_last`), else
//!   **Terminal** (evaporation `Q_in`, level interpolated in the first group with `C_g ≥ Q_in`,
//!   [`balance`]); its footprint is the groups through that one.
//! - **No lake** where the water would stand less than [`MIN_LAKE_DEPTH_M`] deep: an open basin
//!   passes its inflow through, and a terminal one is a playa unless a river could reach it
//!   (`Q_in ≥ RIVER_MIN_DISCHARGE_M3S`).
//!
//! Then routing inside the basin. A basin that passes water on keeps routing's receivers, which
//! form a tree into the exit, and re-accumulates along it in descending depth (ties ascending
//! id); its outflow goes to the exit's receiver (the rim, or a lower neighbour outside). A terminal basin's sinks (its footprint, with any ice below
//! the level) keep their water; every other node routes to the neighbour one BFS step closer to
//! the sinks with the lowest elevation (ties: neighbour order), and re-accumulates in descending
//! BFS distance (ties ascending id). Footprints exclude ice, and each footprint node records its
//! BFS distance in nodes from the shore (1 on the shore).

use std::collections::VecDeque;

use super::params::{
    DATA_LAKE_FLAT_TOL_M, DATA_LAKE_MIN_BALANCE, DATA_LAKE_MIN_NODES, MIN_LAKE_DEPTH_M,
    RIVER_MIN_DISCHARGE_M3S,
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
}

impl Accumulation {
    pub fn new(len: u32) -> Self {
        Self {
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
        if data_lakes && !iced[kmin] {
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

        let outcome = if let Some((level, flat)) = data {
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
        let mut acc = Accumulation::new(g.len());
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
