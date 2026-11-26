//! Today's major river courses (rivers Task 11b): Natural Earth 1:10m river centrelines of
//! scalerank ≤ 7, upstream first, compiled in from `data/hydrology/rivers_today.json`
//! (`timefly-bake rivers-today`, `tools/bake/timefly_bake/rivers_today.py`).
//!
//! Where the terrain is today's ([`NodeSource::todays_terrain`](super::NodeSource)), the pass
//! burns each course into its routing after the straits: along the course's nodes
//! ([`today_river_nodes`]), upstream first, the level is the running minimum of the era's own
//! node elevations (a node within reach of a mapped lake reads at least its surface:
//! `lake_surface_near`), and the node routes at `level − TODAY_RIVER_BURN_M`
//! ([`TODAY_RIVER_BURN_M`](super::params::TODAY_RIVER_BURN_M)), chained over every course so
//! each falls toward its end (`BurntNodes`); routing drains each interior node to its course
//! successor (`route`'s course order). The river graph's bed there is
//! the level itself, so the regions carve no trench; the flats, the climate and the lakes read the
//! burnt surface, as routing did, so every basin agrees with its own spill. Without it the lattice routes rivers across
//! flat plains by its own lines: the lower Indus ran about 90 km east of its course through the
//! Sindh (Task 11's reality check).
//!
//! The table is under 1 MB, so the time-boxed pass reads it one river at a time
//! ([`TodayRiverReader`]) and finds each course's nodes in blocks ([`RiverNodes`]).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::OnceLock;

use bevy_math::DVec3;
use serde_json::Value;

use super::params::{DEPRESSION_EPS_M, TODAY_RIVER_BURN_M};
use super::{CubeLattice, HydroGraph, MappedLake, NodeSource};
use crate::geo::{RenderDir, geo_to_render, latlon_to_geo};

/// The shipped table's JSON.
pub const TODAY_RIVERS_JSON: &str = include_str!("../../../../data/hydrology/rivers_today.json");

/// One course: its name (empty where Natural Earth gives none) and its centreline (lat, lon
/// degrees), upstream first.
#[derive(Clone, Debug, PartialEq)]
pub struct TodayRiver {
    pub name: String,
    pub points: Vec<(f64, f64)>,
}

/// River `k` of the table from its JSON object.
fn river_from(k: usize, r: &Value) -> Result<TodayRiver, String> {
    let name = r["name"]
        .as_str()
        .ok_or(format!("river {k}: no \"name\""))?
        .to_owned();
    if r["scalerank"].as_f64().is_none() {
        return Err(format!("river {k}: no \"scalerank\""));
    }
    let points = r["points"]
        .as_array()
        .ok_or(format!("river {k}: no \"points\""))?
        .iter()
        .map(
            |p| match (p.as_array().map(Vec::len), p[0].as_f64(), p[1].as_f64()) {
                (Some(2), Some(lat), Some(lon))
                    if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                {
                    Ok((lat, lon))
                }
                _ => Err(format!("river {k}: bad point {p}")),
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    if points.len() < 2 {
        return Err(format!("river {k}: needs two points"));
    }
    Ok(TodayRiver { name, points })
}

/// Reads a table's rivers one at a time: the top-level `"rivers"` array, element by element. It
/// does not check the rest of the document ([`parse_today_rivers`] does).
pub struct TodayRiverReader<'a> {
    text: &'a str,
    /// Where the next element starts, once the array is found.
    pos: Option<usize>,
    done: bool,
    count: usize,
}

impl<'a> TodayRiverReader<'a> {
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            pos: None,
            done: false,
            count: 0,
        }
    }

    fn skip_ws(&self, mut at: usize) -> usize {
        let bytes = self.text.as_bytes();
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        at
    }

    /// The position after the `[` of the `"rivers"` array. A JSON string cannot hold an
    /// unescaped `"`, so `"rivers"` followed by `:` is a key.
    fn find_array(&self) -> Result<usize, String> {
        let bytes = self.text.as_bytes();
        let mut from = 0;
        while let Some(k) = self.text[from..].find("\"rivers\"") {
            let at = self.skip_ws(from + k + "\"rivers\"".len());
            if bytes.get(at) == Some(&b':') {
                let at = self.skip_ws(at + 1);
                return if bytes.get(at) == Some(&b'[') {
                    Ok(at + 1)
                } else {
                    Err("\"rivers\" is not an array".into())
                };
            }
            from += k + 1;
        }
        Err("no \"rivers\" array".into())
    }

    /// The next river, or `None` after the last.
    pub fn next_river(&mut self) -> Result<Option<TodayRiver>, String> {
        if self.done {
            return Ok(None);
        }
        let pos = match self.pos {
            Some(p) => p,
            None => {
                let p = self.skip_ws(self.find_array()?);
                if self.text.as_bytes().get(p) == Some(&b']') {
                    self.done = true;
                    return Ok(None);
                }
                p
            }
        };
        let mut values = serde_json::Deserializer::from_str(&self.text[pos..]).into_iter::<Value>();
        let value = values
            .next()
            .ok_or(format!("river {}: the table ends", self.count))?
            .map_err(|e| format!("river {}: {e}", self.count))?;
        let river = river_from(self.count, &value)?;
        self.count += 1;
        let at = self.skip_ws(pos + values.byte_offset());
        match self.text.as_bytes().get(at) {
            Some(b',') => self.pos = Some(self.skip_ws(at + 1)),
            Some(b']') => self.done = true,
            _ => return Err(format!("river {}: no ',' or ']' after it", self.count - 1)),
        }
        Ok(Some(river))
    }
}

/// Parses `{ "comment", "source", "rivers": [{ "name", "scalerank", "points": [[lat, lon], …]
/// }] }`, checking the whole document.
pub fn parse_today_rivers(json: &str) -> Result<Vec<TodayRiver>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let want = v["rivers"].as_array().ok_or("no \"rivers\" array")?.len();
    let mut reader = TodayRiverReader::new(json);
    let mut out = Vec::with_capacity(want);
    while let Some(river) = reader.next_river()? {
        out.push(river);
    }
    if out.len() != want {
        return Err(format!("read {} rivers, the table holds {want}", out.len()));
    }
    Ok(out)
}

/// The shipped table, parsed once. Its tests guarantee it parses.
pub fn shipped_today_rivers() -> &'static [TodayRiver] {
    static RIVERS: OnceLock<Vec<TodayRiver>> = OnceLock::new();
    RIVERS.get_or_init(|| parse_today_rivers(TODAY_RIVERS_JSON).expect("the shipped rivers parse"))
}

/// A course's lattice nodes, found a step at a time ([`today_river_nodes`]).
pub struct RiverNodes {
    dirs: Vec<DVec3>,
    /// The step between samples (radians).
    step: f64,
    /// The polyline segment being sampled, its sample count (0 until it is started) and the
    /// next sample.
    seg: usize,
    samples: u32,
    k: u32,
    nodes: Vec<u32>,
    /// Each node's position in `nodes`.
    at: HashMap<u32, usize>,
    /// Each node's anchor so far: the nearest point to its centre on the polyline segments
    /// whose samples fell in it, and that point's angle from the centre.
    anchors: HashMap<u32, (f64, DVec3)>,
}

/// The point of the minor arc `a`–`b` nearest to `c` (unit vectors).
fn nearest_on_arc(c: DVec3, a: DVec3, b: DVec3) -> DVec3 {
    let n = a.cross(b);
    let len = n.length();
    if len > 1e-15 {
        let n = n / len;
        let foot = c - n * c.dot(n);
        let fl = foot.length();
        if fl > 1e-15 {
            let f = foot / fl;
            if a.cross(f).dot(n) >= 0.0 && f.cross(b).dot(n) >= 0.0 {
                return f;
            }
        }
    }
    if c.angle_between(a) <= c.angle_between(b) {
        a
    } else {
        b
    }
}

impl RiverNodes {
    pub fn new(lattice: &CubeLattice, river: &TodayRiver) -> Self {
        Self {
            dirs: river
                .points
                .iter()
                .map(|&(lat, lon)| geo_to_render(latlon_to_geo(lat, lon)).0.normalize())
                .collect(),
            step: 0.25 * lattice.mean_spacing_m() / crate::worldgen::frame::EARTH_RADIUS_M,
            seg: 0,
            samples: 0,
            k: 0,
            nodes: Vec::new(),
            at: HashMap::new(),
            anchors: HashMap::new(),
        }
    }

    /// Offers the segment `a`–`b` as `v`'s anchor: kept when it passes nearer `v`'s centre
    /// (ties: the first).
    fn offer_anchor(&mut self, lattice: &CubeLattice, v: u32, a: DVec3, b: DVec3) {
        let c = lattice.dir(v).0;
        let p = nearest_on_arc(c, a, b);
        let angle = c.angle_between(p);
        match self.anchors.entry(v) {
            Entry::Occupied(mut o) => {
                if angle < o.get().0 {
                    o.insert((angle, p));
                }
            }
            Entry::Vacant(e) => {
                e.insert((angle, p));
            }
        }
    }

    /// Adds `v`: a repeat of the last node is skipped, and a return to an earlier node cuts the
    /// loop since it, so the course never repeats a node and consecutive nodes stay neighbours.
    /// An L-shaped corner is cut too: when `v` is a diagonal neighbour of the node before the
    /// last, the last goes, so the steepest way down a burnt course is its next node, not a
    /// diagonal past it. A V-shaped turn (two diagonal steps, its ends side by side) keeps its
    /// apex: at a hairpin that is the bend's own valley (the Jhelum at Muzaffarabad).
    fn push(&mut self, lattice: &CubeLattice, v: u32) {
        if let Some(&p) = self.at.get(&v) {
            for u in self.nodes.drain(p + 1..) {
                self.at.remove(&u);
            }
            return;
        }
        while let [.., x, corner] = self.nodes[..] {
            // Odd neighbour slots are the diagonals (`NEIGHBOUR_STEPS`).
            let diagonal = lattice
                .neighbours(x)
                .iter()
                .position(|&u| u == Some(v))
                .is_some_and(|k| k % 2 == 1);
            if !diagonal {
                break;
            }
            self.nodes.pop();
            self.at.remove(&corner);
        }
        self.at.insert(v, self.nodes.len());
        self.nodes.push(v);
    }

    /// Samples up to `budget` points; returns whether the course is done and the samples taken.
    pub fn step(&mut self, lattice: &CubeLattice, budget: u32) -> (bool, u32) {
        let mut used = 0;
        while used < budget {
            let Some(&[a, b]) = self.dirs.get(self.seg..self.seg + 2) else {
                return (true, used);
            };
            if self.samples == 0 {
                self.samples = (a.angle_between(b) / self.step).ceil().max(1.0) as u32;
            }
            let t = f64::from(self.k) / f64::from(self.samples);
            let v = lattice.nearest(RenderDir(a.lerp(b, t).normalize()));
            self.push(lattice, v);
            self.offer_anchor(lattice, v, a, b);
            used += 1;
            self.k += 1;
            if self.k > self.samples {
                self.seg += 1;
                self.k = 0;
                self.samples = 0;
            }
        }
        (self.dirs.len() < self.seg + 2, used)
    }

    /// The course's nodes, upstream first.
    pub fn finish(self) -> Vec<u32> {
        self.nodes
    }

    /// The course's nodes, upstream first, each with its anchor: the nearest point to its
    /// centre on the polyline's segments that sampled it.
    pub fn finish_with_anchors(self) -> Vec<(u32, DVec3)> {
        let anchors = self.anchors;
        self.nodes.into_iter().map(|v| (v, anchors[&v].1)).collect()
    }

    /// The bytes held, by capacity.
    pub fn bytes(&self) -> usize {
        24 * self.dirs.capacity()
            + 4 * self.nodes.capacity()
            + 16 * self.at.capacity()
            + 36 * self.anchors.capacity()
    }
}

/// The lattice nodes along a course, upstream first: the nearest node to points a quarter of the
/// mean spacing apart, so consecutive nodes are neighbours, without repeats (a loop back to an
/// earlier node is cut) and without L-shaped corners (a node whose neighbours on the course are
/// diagonal neighbours themselves is dropped).
pub fn today_river_nodes(lattice: &CubeLattice, river: &TodayRiver) -> Vec<u32> {
    let mut nodes = RiverNodes::new(lattice, river);
    while !nodes.step(lattice, u32::MAX).0 {}
    nodes.finish()
}

/// One node's burn along a course ([`course_levels`]).
#[derive(Copy, Clone, Debug, PartialEq)]
struct CourseLevel {
    node: u32,
    /// Routing's level: the running minimum.
    level: f32,
    /// `level − TODAY_RIVER_BURN_M`, before any floor: routing's elevation is built from it over
    /// every course at once ([`BurntNodes`]).
    base: f32,
    /// The node's own elevation as the course read it ([`course_levels`]'s `elevation`).
    own: f32,
    /// The course's last burnable node: the only one that may meet the sea.
    last: bool,
    /// The next node, for an interior node whose next node is burnable (Task 11d fix round 4:
    /// an interior node drains to it), with the course's own chain there: `max(level − burn,
    /// 0 m)`, and at least `DEPRESSION_EPS_M` above the chain at the node after, from the
    /// course's end upstream (how this course alone would route it).
    succ: Option<(u32, f32)>,
    /// The river graph's bed: the running minimum, sloped across held stretches.
    bed: f32,
    anchor: DVec3,
}

/// The levels along one course's nodes (upstream first, with their anchors), never below 0 m on
/// a node at or above sea level:
/// - routing's level is the running minimum of `elevation`;
/// - the bed is that level, except across a held stretch (the nodes after one that set the
///   minimum, up to the next that lowers it): there it falls linearly by path length (`dist`
///   between consecutive nodes) from the held level toward the next lower one, so it never
///   rises and never stands above the level. After the last lowering it stays level.
///
/// Routing's elevation is built from `level − TODAY_RIVER_BURN_M` over every course at once
/// ([`BurntNodes::chain_step`]).
///
/// `add` gets each node that `burnable` allows; the others still feed the running minimum.
fn course_levels(
    nodes: &[(u32, DVec3)],
    elevation: impl Fn(u32) -> f32,
    burnable: impl Fn(u32) -> bool,
    dist: impl Fn(u32, u32) -> f64,
    mut add: impl FnMut(CourseLevel),
) {
    let e: Vec<f32> = nodes.iter().map(|&(v, _)| elevation(v)).collect();
    // The running minimum, and the nodes that lowered it (the first included).
    let mut level = Vec::with_capacity(e.len());
    let mut setters = Vec::new();
    let mut run = f32::INFINITY;
    for (k, &ek) in e.iter().enumerate() {
        if ek < run {
            run = ek;
            setters.push(k);
        }
        level.push(run);
    }
    // Path length from the first node.
    let mut path = Vec::with_capacity(e.len());
    let mut m = 0.0;
    for (k, &(v, _)) in nodes.iter().enumerate() {
        if k > 0 {
            m += dist(nodes[k - 1].0, v);
        }
        path.push(m);
    }
    let burn = TODAY_RIVER_BURN_M as f32;
    let eps = DEPRESSION_EPS_M as f32;
    let mut own_chain = vec![0.0f32; e.len()];
    for k in (0..e.len()).rev() {
        let raw = (level[k] - burn).max(0.0);
        own_chain[k] = own_chain
            .get(k + 1)
            .map_or(raw, |&down| raw.max(down + eps));
    }
    let last = nodes.iter().rposition(|&(v, _)| burnable(v));
    let mut next = 0;
    for (k, &(v, anchor)) in nodes.iter().enumerate() {
        while next < setters.len() && setters[next] <= k {
            next += 1;
        }
        let land = e[k] >= 0.0;
        let clamp = |x: f32| if land { x.max(0.0) } else { x };
        let held = level[k];
        let bed = match setters.get(next) {
            Some(&t) if setters[next - 1] != k => {
                let s = setters[next - 1];
                let target = clamp(level[t]);
                let from = clamp(held);
                let frac = (path[k] - path[s]) / (path[t] - path[s]);
                (f64::from(from) + (f64::from(target) - f64::from(from)) * frac) as f32
            }
            _ => clamp(held),
        };
        if burnable(v) {
            let interior = last.is_some_and(|l| k < l);
            add(CourseLevel {
                node: v,
                level: clamp(held),
                base: level[k] - burn,
                own: e[k],
                last: last == Some(k),
                succ: nodes
                    .get(k + 1)
                    .map(|&(s, _)| (s, own_chain[k + 1]))
                    .filter(|&(s, _)| interior && burnable(s)),
                bed: bed.min(clamp(held)),
                anchor,
            });
        }
    }
}

/// One burnt node as the pass leaves it, for measuring the burn (the reality check).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CourseBed {
    pub node: u32,
    /// The course (index into the table) that gave the node its level.
    pub course: usize,
    /// The era's own elevation there, before any burn.
    pub elevation_m: f32,
    /// The river graph's bed there: the lowest level any course gave it.
    pub bed_m: f32,
    /// Whether the course had passed a node below sea level upstream while this node stands
    /// above it, so its bed is pinned at 0 m.
    pub pinned_at_sea_level: bool,
}

/// The beds the pass gives `rivers` over `source` (a present-day era: data lakes) with the lake
/// level table `levels`, the straits burnt first as the pass does ([`super::straits`]). Not
/// time-boxed: for checks.
pub fn course_beds<S: NodeSource>(
    lattice: &CubeLattice,
    rivers: &[TodayRiver],
    source: &S,
    levels: &[super::levels::LakeLevel],
) -> Vec<CourseBed> {
    use super::params::ICE_NODE_FRACTION;
    use super::straits::{shipped_straits, strait_levels};
    let mut sample: HashMap<u32, (f32, bool)> = HashMap::new();
    let mut sampled = |v: u32| {
        *sample.entry(v).or_insert_with(|| {
            let s = source.node(lattice.dir(v));
            (s.elevation_m, f64::from(s.ice) >= ICE_NODE_FRACTION)
        })
    };
    // The mapped lakes' surfaces at nodes, as the pass reads them.
    let table = table_surfaces(levels, source);
    let mut lakes: HashMap<u32, Option<f32>> = HashMap::new();
    let mut lake = |u: u32| {
        *lakes.entry(u).or_insert_with(|| {
            source
                .mapped(lattice.dir(u))
                .map(|l| lake_surface(l, &table))
        })
    };
    // The straits' burn, as the pass's.
    let mut strait: HashMap<u32, f32> = HashMap::new();
    for s in shipped_straits() {
        for (v, level) in strait_levels(lattice, s) {
            if let Some(shift) = source.present_shift_m(lattice.dir(v)) {
                let l = strait.entry(v).or_insert(f32::INFINITY);
                *l = l.min(level + shift);
            }
        }
    }
    let mut out: Vec<CourseBed> = Vec::new();
    let mut at: HashMap<u32, usize> = HashMap::new();
    for (k, river) in rivers.iter().enumerate() {
        let mut course = RiverNodes::new(lattice, river);
        while !course.step(lattice, u32::MAX).0 {}
        let nodes = course.finish_with_anchors();
        let own: Vec<(f32, bool)> = nodes.iter().map(|&(v, _)| sampled(v)).collect();
        let routed = |v: u32, e: f32| strait.get(&v).map_or(e, |&l| e.min(l));
        let index: HashMap<u32, usize> = nodes
            .iter()
            .enumerate()
            .map(|(i, &(v, _))| (v, i))
            .collect();
        let (lat, lon) = river.points[0];
        let start = source
            .mapped(geo_to_render(latlon_to_geo(lat, lon)))
            .map(|l| lake_surface(l, &table));
        let mut below_sea = false;
        let mut levels: Vec<(u32, f32, bool)> = Vec::new();
        for (i, &(v, _)) in nodes.iter().enumerate() {
            let near = lake_surface_near(lattice, v, &mut lake);
            let e = course_height(routed(v, own[i].0), near, start.filter(|_| i == 0));
            let pinned = below_sea && e >= 0.0;
            below_sea |= e < 0.0;
            levels.push((v, e, pinned));
        }
        course_levels(
            &nodes,
            |v| levels[index[&v]].1,
            |v| !own[index[&v]].1 && source.todays_terrain(lattice.dir(v)),
            |a, b| lattice.distance_m(a, b),
            |c| {
                let (v, level) = (c.node, c.bed);
                let i = index[&v];
                let bed = CourseBed {
                    node: v,
                    course: k,
                    elevation_m: own[i].0,
                    bed_m: level,
                    pinned_at_sea_level: levels[i].2,
                };
                match at.get(&v) {
                    Some(&j) if out[j].bed_m <= level => {}
                    Some(&j) => out[j] = bed,
                    None => {
                        at.insert(v, out.len());
                        out.push(bed);
                    }
                }
            },
        );
    }
    out
}

/// How one interior node of a course drains, against its course successor (Task 11d fix round
/// 4: every interior node's receiver is its successor, apart from these counted exceptions).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CourseStep {
    /// It drains to its successor.
    Follows,
    /// A node several courses pass through, draining to another course's successor there (the
    /// node's owner: `BurntNodes`).
    SharedNode,
    /// It is ocean: past its course's open-sea end (`route`'s corridor).
    Ocean,
    /// Its successor is ocean past its course's open-sea end: a course that left the coast and
    /// came back to land, which meets the sea where routing takes it.
    PastSeaEnd,
    /// Its successor is not burnt (ice, or not today's terrain).
    NotBurnt,
    /// It lies in a lake and carries no river.
    InLake,
    /// It carries no river: too little water.
    NoRiver,
    /// A river node redirected to its basin's exit, which is not its neighbour.
    BasinExit,
    /// A river node in a basin whose rewrite routes it a step closer to the basin's lake or
    /// sink, where its successor is none ([`super::EraHydrology::basin_routed`]).
    BasinRoute,
    /// A river node ending in a dry sink.
    Sink,
    /// Anything else: a river node draining to a neighbour that is no course's successor there.
    Other,
}

/// Each interior node of each of `rivers` (every burnable node but a course's last), in table
/// and course order: `(course, node, successor, how it drains)`, over the pass's result `h`
/// for `source` (a present-day era with today's terrain).
pub fn course_steps<S: NodeSource>(
    h: &super::EraHydrology,
    rivers: &[TodayRiver],
    source: &S,
) -> Vec<(usize, u32, u32, CourseStep)> {
    use super::params::ICE_NODE_FRACTION;
    let lattice = &h.lattice;
    let mut burnable_at: HashMap<u32, bool> = HashMap::new();
    let mut burnable = |v: u32| {
        *burnable_at.entry(v).or_insert_with(|| {
            let d = lattice.dir(v);
            f64::from(source.node(d).ice) < ICE_NODE_FRACTION && source.todays_terrain(d)
        })
    };
    let courses: Vec<Vec<u32>> = rivers
        .iter()
        .map(|r| today_river_nodes(lattice, r))
        .collect();
    // Each course's interior nodes and their successors (None: not burnt).
    let mut interior: Vec<(usize, u32, Option<u32>)> = Vec::new();
    for (k, nodes) in courses.iter().enumerate() {
        let Some(last) = nodes.iter().rposition(|&v| burnable(v)) else {
            continue;
        };
        for i in 0..last {
            if burnable(nodes[i]) {
                let next = nodes[i + 1];
                interior.push((k, nodes[i], burnable(next).then_some(next)));
            }
        }
    }
    let mut successors: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(_, v, s) in &interior {
        if let Some(s) = s {
            successors.entry(v).or_default().push(s);
        }
    }
    interior
        .iter()
        .map(|&(k, v, s)| {
            let Some(s) = s else {
                return (k, v, v, CourseStep::NotBurnt);
            };
            let step = if h.is_ocean(v) {
                CourseStep::Ocean
            } else if let Some(i) = h.rivers.index_of(v) {
                let n = h.rivers.nodes()[i as usize];
                let to = match n.down {
                    super::Down::River(t) => Some(h.rivers.nodes()[t as usize].node),
                    super::Down::Ocean(j) | super::Down::Lake(_, j) | super::Down::Ice(j) => {
                        Some(j)
                    }
                    super::Down::Sink(_) => None,
                };
                match to {
                    Some(j) if j == s => CourseStep::Follows,
                    Some(j) if successors[&v].contains(&j) => CourseStep::SharedNode,
                    Some(_) if h.is_ocean(s) => CourseStep::PastSeaEnd,
                    Some(_) if h.basin_routed(v) => CourseStep::BasinRoute,
                    Some(j) if !lattice.neighbours(v).contains(&Some(j)) => CourseStep::BasinExit,
                    Some(_) => CourseStep::Other,
                    None => CourseStep::Sink,
                }
            } else if h.lake_of(v).is_some() {
                CourseStep::InLake
            } else {
                CourseStep::NoRiver
            };
            (k, v, s, step)
        })
        .collect()
}

/// A node's successor slot with none.
const NO_SUCC: u32 = u32::MAX;

/// Per-node levels gathered from many courses: each node once, in the order first burnt, with
/// the lowest routing level any course gave it, and the lowest bed with that bed's course's
/// anchor (ties: the first).
///
/// **Routing's elevation** (Task 11d fix round 4) is built over every course at once, so the
/// courses' order agrees where they meet. Each interior node's **successor** is its next node on
/// one of the courses that pass it as an interior node: the course whose own chain is lowest at
/// that next node (`max(level − TODAY_RIVER_BURN_M, 0 m)`, at least `DEPRESSION_EPS_M` above the
/// node after, from that course's own end), ties to the first in the table. At a fork, or where
/// a distributary leaves a stem, that is the branch whose own course falls lowest there, as
/// steepest descent picked it when each course routed on its own chain (Task 11d fix round 2).
/// Then from the courses' ends upstream along the successors, the **land chain** is
/// `max(base, 0 m, chain(successor) + DEPRESSION_EPS_M)`, where `base` is the lowest
/// `level − TODAY_RIVER_BURN_M` any course gave the node. A node at or above sea level routes at
/// its chain; one below the sea at `base`, unless routing finds it an interior node joined to the
/// ocean (the corridor), when it takes the chain. A successor that would close a loop is dropped
/// (the node where the walk, in the order nodes were first burnt, meets its own path keeps none).
#[derive(Default)]
pub(crate) struct BurntNodes {
    /// Each node and the lowest level any course gave it.
    list: Vec<(u32, f32)>,
    /// Each node's routing elevation (`list`'s order), once [`BurntNodes::chain_step`] is done.
    routes: Vec<f32>,
    /// Each node's lowest `level − burn` (`list`'s order).
    base: Vec<f32>,
    /// Each node's own elevation as read by its courses (the highest; `list`'s order).
    own: Vec<f32>,
    /// Each node's successor and its course's own chain there (`list`'s order).
    succ: Vec<(u32, f32)>,
    beds: Vec<(f32, DVec3)>,
    at: HashMap<u32, u32>,
    /// The nodes that end a course (its last burnable node), which alone may meet the sea.
    ends: Vec<u32>,
    /// The nodes inside some course (any but its last).
    interior: Vec<u32>,
    /// Each course's interior nodes below the sea, in course order: `(node, own elevation)`.
    corridor: Vec<Vec<(u32, f32)>>,
    /// The land chain (`list`'s order), while it is built: NaN where not yet known.
    chain: Vec<f32>,
    /// The next node to chain from, while the chain is built.
    chained: usize,
}

impl BurntNodes {
    /// The river graph's bed at `v`, if any course burnt it.
    pub(crate) fn get(&self, v: u32) -> Option<f32> {
        self.at.get(&v).map(|&k| self.beds[k as usize].0)
    }

    /// The anchor of `v`, if any course burnt it.
    pub(crate) fn anchor(&self, v: u32) -> Option<DVec3> {
        self.at.get(&v).map(|&k| self.beds[k as usize].1)
    }

    /// The burnt nodes and their beds, in the order first burnt.
    #[cfg(test)]
    pub(crate) fn beds(&self) -> Vec<(u32, f32)> {
        self.list
            .iter()
            .zip(&self.beds)
            .map(|(&(v, _), &(bed, _))| (v, bed))
            .collect()
    }

    /// The burnt nodes and their levels, in the order first burnt.
    #[cfg(test)]
    pub(crate) fn list(&self) -> &[(u32, f32)] {
        &self.list
    }

    /// The burnt nodes and their routing elevations, in the order first burnt.
    #[cfg(test)]
    pub(crate) fn routes(&self) -> Vec<(u32, f32)> {
        self.list
            .iter()
            .map(|&(v, _)| v)
            .zip(self.routes.iter().copied())
            .collect()
    }

    /// The nodes inside some course (any node but a course's last), sorted: routing never
    /// drains them into the ocean (Task 11d fix rounds 2 and 3: a course meets the sea only at
    /// its own end, and a node that ends one course but lies inside another is inland).
    pub(crate) fn inland(&self) -> Vec<u32> {
        let mut out = self.interior.clone();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Each course's interior nodes below the sea, in course order, `(node, land-chain
    /// elevation, own elevation)`: routing keeps them out of the ocean up to the first open-sea
    /// node (Task 11d fix round 3). Once the chain is built.
    pub(crate) fn corridor(&self) -> Vec<Vec<(u32, f32, f32)>> {
        self.corridor
            .iter()
            .filter(|c| !c.is_empty())
            .map(|c| {
                c.iter()
                    .map(|&(v, e)| (v, self.chain[self.at[&v] as usize], e))
                    .collect()
            })
            .collect()
    }

    /// Each interior node with a successor and that successor, sorted by node (Task 11d fix
    /// round 4: routing drains the node there). Once the chain is built: no loops.
    pub(crate) fn successors(&self) -> Vec<(u32, u32)> {
        let mut out: Vec<(u32, u32)> = self
            .list
            .iter()
            .zip(&self.succ)
            .filter(|&(_, &(s, _))| s != NO_SUCC)
            .map(|(&(v, _), &(s, _))| (v, s))
            .collect();
        out.sort_unstable();
        out
    }

    /// Starts the next course's corridor.
    pub(crate) fn begin_course(&mut self) {
        if self.corridor.last().is_none_or(|c| !c.is_empty()) {
            self.corridor.push(Vec::new());
        }
    }

    /// The nodes that end some course, sorted.
    pub(crate) fn ends(&self) -> Vec<u32> {
        let mut ends = self.ends.clone();
        ends.sort_unstable();
        ends.dedup();
        ends
    }

    fn add(&mut self, c: CourseLevel) {
        if c.last {
            self.ends.push(c.node);
        } else {
            self.interior.push(c.node);
        }
        if !c.last && c.own < 0.0 {
            if self.corridor.is_empty() {
                self.corridor.push(Vec::new());
            }
            self.corridor
                .last_mut()
                .expect("a course")
                .push((c.node, c.own));
        }
        match self.at.entry(c.node) {
            Entry::Occupied(o) => {
                let k = *o.get() as usize;
                let l = &mut self.list[k].1;
                *l = l.min(c.level);
                self.base[k] = self.base[k].min(c.base);
                self.own[k] = self.own[k].max(c.own);
                if let Some(s) = c.succ
                    && s.1 < self.succ[k].1
                {
                    self.succ[k] = s;
                }
                if c.bed < self.beds[k].0 {
                    self.beds[k] = (c.bed, c.anchor);
                }
            }
            Entry::Vacant(e) => {
                e.insert(self.list.len() as u32);
                self.list.push((c.node, c.level));
                self.base.push(c.base);
                self.own.push(c.own);
                self.succ.push(c.succ.unwrap_or((NO_SUCC, f32::INFINITY)));
                self.beds.push((c.bed, c.anchor));
            }
        }
    }

    /// Builds the land chain and the routing elevations from about `budget` more nodes (a walk
    /// along successors is not split); returns whether it is done, and the units used (a unit
    /// per node walked from, and one per further node a walk chains: independent of the slicing).
    pub(crate) fn chain_step(&mut self, budget: u32) -> (bool, u32) {
        let n = self.list.len();
        if self.chain.len() != n {
            self.chain = vec![f32::NAN; n];
            self.routes = vec![0.0; n];
        }
        let eps = DEPRESSION_EPS_M as f32;
        let mut used = 0u32;
        let mut path: Vec<usize> = Vec::new();
        // Nodes on the current walk, so a loop is seen.
        let mut on_path: HashMap<usize, ()> = HashMap::new();
        while self.chained < n && used < budget {
            let start = self.chained;
            self.chained += 1;
            used += 1;
            if !self.chain[start].is_nan() {
                continue;
            }
            path.clear();
            on_path.clear();
            let mut k = start;
            // Walk down the successors to a node already chained, a root, or a loop.
            let mut below = loop {
                path.push(k);
                on_path.insert(k, ());
                let s = self.succ[k].0;
                if s == NO_SUCC {
                    break None;
                }
                let j = self.at[&s] as usize;
                if on_path.contains_key(&j) {
                    // A loop: this node keeps no successor.
                    self.succ[k] = (NO_SUCC, f32::INFINITY);
                    break None;
                }
                if !self.chain[j].is_nan() {
                    break Some(self.chain[j]);
                }
                k = j;
            };
            for &k in path.iter().rev() {
                let floor = self.base[k].max(0.0);
                let c = below.map_or(floor, |down| floor.max(down + eps));
                self.chain[k] = c;
                self.routes[k] = if self.own[k] >= 0.0 { c } else { self.base[k] };
                below = Some(c);
                used += u32::from(k != start);
            }
        }
        (self.chained == n, used.max(1))
    }

    pub(crate) fn bytes(&self) -> usize {
        8 * self.list.capacity()
            + 32 * self.beds.capacity()
            + 12 * self.at.capacity()
            + 4 * self.ends.capacity()
            + 4 * self.interior.capacity()
            + self
                .corridor
                .iter()
                .map(|c| 24 + 8 * c.capacity())
                .sum::<usize>()
            + 4 * (self.routes.capacity()
                + self.base.capacity()
                + self.own.capacity()
                + self.chain.capacity())
            + 8 * self.succ.capacity()
    }
}

/// Which courses a pass burns.
#[derive(Clone, Debug)]
pub(crate) enum TodayTable {
    /// The compiled-in table, read a river at a time.
    Shipped,
    /// Tests' own courses.
    #[cfg_attr(not(test), allow(dead_code))]
    Given(Vec<TodayRiver>),
}

/// Today's river courses, burnt resumably: each course is read, its nodes found and its levels
/// gathered ([`BurntNodes`]) while the elevation stays the era's own, then every burnt node is
/// lowered ([`TODAY_RIVER_BURN_M`] below its level, never across sea level).
pub(crate) struct TodayBurn {
    reader: TodayRiverReader<'static>,
    /// The next of [`TodayTable::Given`]'s courses.
    next: usize,
    /// The course whose nodes are being found, and the surface of the mapped lake its first
    /// point lies in.
    course: Option<(RiverNodes, Option<f32>)>,
    burnt: BurntNodes,
    /// Every course is read; the land chain is being built.
    chaining: bool,
    /// The chain is built; the burnt nodes are being lowered from this one on.
    applying: Option<usize>,
    /// The level table's lakes by map id ([`table_surfaces`]).
    table: Vec<(u16, f32)>,
}

/// How near a mapped lake's node a course node reads that lake's surface, in mean spacings: a
/// node's footprint reaches about half a spacing, so 1.5 is one spacing from the footprint.
const LAKE_REACH_SPACINGS: f64 = 1.5;

/// The highest surface of the mapped lakes (`lake`) at `v` or at a lattice neighbour within
/// [`LAKE_REACH_SPACINGS`] of it (Task 11d fix round 4): a course node there reads its height as
/// at least that surface, so a deep lake's bed (Baikal's lies 1,186 m below the sea), mixed into
/// a shore node's sample, never sets a course's level.
fn lake_surface_near(
    lattice: &CubeLattice,
    v: u32,
    lake: impl FnMut(u32) -> Option<f32>,
) -> Option<f32> {
    let reach = LAKE_REACH_SPACINGS * lattice.mean_spacing_m();
    std::iter::once(Some(v))
        .chain(lattice.neighbours(v))
        .flatten()
        .filter(|&u| u == v || lattice.distance_m(v, u) <= reach)
        .filter_map(lake)
        .reduce(f32::max)
}

/// The lake level table's lakes as mapped lakes: each table point's map id and its table level,
/// sorted by id ([`super::levels`]). A course reads such a lake at its table level, as the pass
/// fills it: the map's own baked surface can be its bed's (Baikal's reads −328 m).
pub(crate) fn table_surfaces<S: NodeSource>(
    levels: &[super::levels::LakeLevel],
    source: &S,
) -> Vec<(u16, f32)> {
    let mut out: Vec<(u16, f32)> = levels
        .iter()
        .filter_map(|l| {
            let m = source.mapped(geo_to_render(latlon_to_geo(l.lat, l.lon)))?;
            Some((m.id, l.level_m))
        })
        .collect();
    out.sort_by_key(|e| e.0);
    out.dedup_by_key(|e| e.0);
    out
}

/// A mapped lake's surface for a course: its table level where the level table holds it
/// (`table`, from [`table_surfaces`]), else the map's.
fn lake_surface(lake: MappedLake, table: &[(u16, f32)]) -> f32 {
    table
        .binary_search_by_key(&lake.id, |e| e.0)
        .map_or(lake.surface_m, |k| table[k].1)
}

/// A course node's height for its levels: its own `e`, and at least the surface of a mapped lake
/// within reach ([`lake_surface_near`]), or of the lake its course leaves (`start`, for its first
/// node).
fn course_height(e: f32, near: Option<f32>, start: Option<f32>) -> f32 {
    [near, start].into_iter().flatten().fold(e, f32::max)
}

/// Units for reading one course: a unit a point, plus one.
fn course_read_units(river: &TodayRiver) -> u32 {
    1 + river.points.len() as u32
}

/// Units for gathering one node's level.
const LEVEL_UNITS: u32 = 2;

impl TodayBurn {
    /// The burn, with the level table's lakes by map id ([`table_surfaces`]).
    pub(crate) fn new(table: Vec<(u16, f32)>) -> Self {
        Self {
            table,
            reader: TodayRiverReader::new(TODAY_RIVERS_JSON),
            next: 0,
            course: None,
            burnt: BurntNodes::default(),
            chaining: false,
            applying: None,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.burnt.bytes() + self.course.as_ref().map_or(0, |c| c.0.bytes())
    }

    /// The burnt nodes and their levels, for the river graph's beds.
    pub(crate) fn into_beds(self) -> BurntNodes {
        self.burnt
    }

    /// Up to about `budget` units (a course's reading and its levels are not split); returns
    /// whether every burnt node is lowered, and the units used (at least 1). The units do not
    /// depend on how the work is sliced. `mapped` holds the mapped lakes' nodes, ascending.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn step<S: NodeSource>(
        &mut self,
        table: &TodayTable,
        lattice: &CubeLattice,
        elevation: &mut [f32],
        mapped: &[(u32, MappedLake)],
        is_ice: &dyn Fn(u32) -> bool,
        source: &S,
        budget: u32,
    ) -> (bool, u32) {
        let mut used = 0;
        while used < budget {
            if let Some(start) = self.applying {
                let burnt = &self.burnt;
                let end = start
                    .saturating_add((budget - used) as usize)
                    .min(burnt.list.len());
                for k in start..end {
                    let e = &mut elevation[burnt.list[k].0 as usize];
                    let route = burnt.routes[k];
                    // Never across sea level, so the burn changes no ocean (`route` is never below
                    // 0 m on land), except beside a mapped lake, where a node the course reads as
                    // land at the lake's surface is land. On land it is set: the course's fall of
                    // DEPRESSION_EPS_M a node may stand a little above the node's own height.
                    *e = if burnt.own[k] >= 0.0 {
                        route
                    } else {
                        e.min(route)
                    };
                }
                used += (end - start) as u32;
                self.applying = Some(end);
                if end == burnt.list.len() {
                    return (true, used);
                }
                continue;
            }
            if self.chaining {
                let (done, u) = self.burnt.chain_step(budget - used);
                used += u;
                if done {
                    self.chaining = false;
                    self.applying = Some(0);
                }
                continue;
            }
            if let Some((course, _)) = &mut self.course {
                let (done, u) = course.step(lattice, budget - used);
                used += u;
                if !done {
                    continue;
                }
                let (course, start) = self.course.take().expect("a course");
                let nodes = course.finish_with_anchors();
                used += LEVEL_UNITS * nodes.len() as u32;
                let burnable = |v: u32| !is_ice(v) && source.todays_terrain(lattice.dir(v));
                let table = &self.table;
                let lake = |u: u32| {
                    mapped
                        .binary_search_by_key(&u, |&(i, _)| i)
                        .ok()
                        .map(|k| lake_surface(mapped[k].1, table))
                };
                let first = nodes.first().map(|&(v, _)| v);
                let burnt = &mut self.burnt;
                burnt.begin_course();
                course_levels(
                    &nodes,
                    |v| {
                        let near = lake_surface_near(lattice, v, lake);
                        let start = start.filter(|_| Some(v) == first);
                        course_height(elevation[v as usize], near, start)
                    },
                    burnable,
                    |a, b| lattice.distance_m(a, b),
                    |c| burnt.add(c),
                );
                continue;
            }
            let river = match table {
                TodayTable::Shipped => self
                    .reader
                    .next_river()
                    .expect("the shipped rivers parse (their tests say so)"),
                TodayTable::Given(rivers) => {
                    self.next += 1;
                    rivers.get(self.next - 1).cloned()
                }
            };
            match river {
                Some(river) => {
                    used += course_read_units(&river);
                    let (lat, lon) = river.points[0];
                    let start = source
                        .mapped(geo_to_render(latlon_to_geo(lat, lon)))
                        .map(|l| lake_surface(l, &self.table));
                    self.course = Some((RiverNodes::new(lattice, &river), start));
                }
                None => {
                    used += 1;
                    if self.burnt.list.is_empty() {
                        return (true, used);
                    }
                    self.chaining = true;
                }
            }
        }
        (false, used)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{geo_to_latlon, render_to_geo};

    /// Task 11d fix round 4: a course node reads a mapped lake's surface at its own node or a
    /// neighbour within reach, and a course's first node the lake its first point lies in.
    #[test]
    fn a_course_node_reads_a_lake_within_reach_at_its_surface() {
        let lattice = CubeLattice { n: 300 };
        let v = lattice.nearest(geo_to_render(latlon_to_geo(10.0, 10.0)));
        let u = lattice.neighbours(v)[0].expect("a neighbour");
        let lake = |x: u32| (x == u).then_some(120.0f32);
        assert_eq!(lake_surface_near(&lattice, v, lake), Some(120.0));
        assert_eq!(lake_surface_near(&lattice, u, lake), Some(120.0));
        // Two steps away the lake is out of reach.
        let far = lattice
            .neighbours(v)
            .into_iter()
            .flatten()
            .flat_map(|w| lattice.neighbours(w).into_iter().flatten())
            .find(|&w| lattice.distance_m(w, u) > 2.0 * lattice.mean_spacing_m())
            .expect("a node two steps off");
        assert_eq!(lake_surface_near(&lattice, far, lake), None);
        // A lake raises a node read below it; it never lowers one.
        assert_eq!(course_height(-50.0, Some(120.0), None), 120.0);
        assert_eq!(course_height(300.0, Some(120.0), None), 300.0);
        assert_eq!(course_height(-300.0, None, Some(120.0)), 120.0);
        assert_eq!(course_height(-300.0, None, None), -300.0);
    }

    #[test]
    fn the_shipped_table_parses() {
        assert!(
            TODAY_RIVERS_JSON.len() <= 1_000_000,
            "{}",
            TODAY_RIVERS_JSON.len()
        );
        let rivers = shipped_today_rivers();
        assert!(rivers.len() >= 1000, "{}", rivers.len());
        assert!(rivers.iter().any(|r| r.name == "Indus"));
        assert!(rivers.iter().any(|r| r.name == "Jhelum"));
        // The reader alone reads the same rivers.
        let mut reader = TodayRiverReader::new(TODAY_RIVERS_JSON);
        let mut k = 0;
        while let Some(r) = reader.next_river().unwrap() {
            assert_eq!(&r, &rivers[k]);
            k += 1;
        }
        assert_eq!(k, rivers.len());
        assert_eq!(reader.next_river(), Ok(None));
    }

    #[test]
    fn bad_river_tables_are_errors() {
        let ok = r#"{"comment": "the \"rivers\" here", "rivers": [
            {"name": "a", "scalerank": 3, "points": [[1, 2], [1.5, 2]]},
            {"name": "", "scalerank": 7, "points": [[1, 2], [1.5, 2.5], [2, 3]]}
        ], "source": "s"}"#;
        let got = parse_today_rivers(ok).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].points, vec![(1.0, 2.0), (1.5, 2.5), (2.0, 3.0)]);
        assert_eq!(parse_today_rivers(r#"{"rivers": []}"#).unwrap(), vec![]);
        for bad in [
            "{}",
            r#"{"rivers": {}}"#,
            r#"{"rivers": [{"name": "a", "scalerank": 3, "points": [[1, 2]]}]}"#,
            r#"{"rivers": [{"name": "a", "scalerank": 3, "points": [[1, 2], [95, 2]]}]}"#,
            r#"{"rivers": [{"name": "a", "scalerank": 3, "points": [[1, 2], [1, 2, 3]]}]}"#,
            r#"{"rivers": [{"scalerank": 3, "points": [[1, 2], [1, 3]]}]}"#,
            r#"{"rivers": [{"name": "a", "points": [[1, 2], [1, 3]]}]}"#,
            r#"{"rivers": [{"name": "a", "scalerank": 3, "points": [[1, 2], [1, 3]]}"#,
        ] {
            assert!(parse_today_rivers(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn river_nodes_run_node_to_node() {
        let lattice = CubeLattice { n: 900 };
        let mut total = 0;
        for river in shipped_today_rivers() {
            let nodes = today_river_nodes(&lattice, river);
            assert!(!nodes.is_empty(), "{}", river.name);
            for w in nodes.windows(2) {
                assert!(
                    lattice.neighbours(w[0]).contains(&Some(w[1])),
                    "{}: {w:?}",
                    river.name
                );
            }
            for w in nodes.windows(3) {
                let k = lattice
                    .neighbours(w[0])
                    .iter()
                    .position(|&u| u == Some(w[2]));
                assert!(
                    k.is_none_or(|k| k % 2 == 0),
                    "{}: an L-shaped corner at {w:?}",
                    river.name
                );
            }
            let mut sorted = nodes.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), nodes.len(), "{}: a repeat", river.name);
            total += nodes.len();
        }
        assert!(total > 25_000, "{total}");
        // Stepping in small blocks finds the same nodes.
        let river = shipped_today_rivers()
            .iter()
            .find(|r| r.name == "Indus")
            .unwrap();
        let mut blocks = RiverNodes::new(&lattice, river);
        let mut steps = 0;
        while !blocks.step(&lattice, 3).0 {
            steps += 1;
        }
        assert!(steps > 10);
        assert_eq!(blocks.finish(), today_river_nodes(&lattice, river));
        // A loop back to an earlier node is cut: out 50 km east and back again.
        let there_and_back = TodayRiver {
            name: "loop".into(),
            points: vec![(10.0, 20.0), (10.0, 20.45), (10.0, 20.0), (10.5, 20.0)],
        };
        let nodes = today_river_nodes(&lattice, &there_and_back);
        assert_eq!(
            nodes[0],
            lattice.nearest(geo_to_render(latlon_to_geo(10.0, 20.0)))
        );
        for w in nodes.windows(2) {
            assert!(lattice.neighbours(w[0]).contains(&Some(w[1])), "{w:?}");
        }
        let lon = |v: u32| geo_to_latlon(render_to_geo(lattice.dir(v))).1;
        assert!(
            nodes.iter().all(|&v| lon(v) < 20.1),
            "the eastward loop is gone"
        );
        // A hairpin keeps its apex: the Jhelum turns south at Muzaffarabad (34.335, 73.454).
        let jhelum = shipped_today_rivers()
            .iter()
            .find(|r| r.name == "Jhelum" && r.points[0].0 > 34.0)
            .unwrap();
        let apex = lattice.nearest(geo_to_render(latlon_to_geo(34.335, 73.454)));
        assert!(today_river_nodes(&lattice, jhelum).contains(&apex));
    }

    /// Task 11d fix round 1 (the owner: "Hand-add delta channels"): each cited channel runs node
    /// to node (as every course, above) to within 10 km of its cited mouth, and the three that
    /// continue a Natural Earth course start on its last point.
    #[test]
    fn the_hand_added_delta_channels_reach_their_mouths() {
        let lattice = CubeLattice { n: 900 };
        let rivers = shipped_today_rivers();
        let km = |a: DVec3, b: DVec3| a.angle_between(b) * 6_371.0;
        let at = |lat: f64, lon: f64| geo_to_render(latlon_to_geo(lat, lon)).0;
        for (name, mouth, continues) in [
            ("Yangtze estuary", (31.39361, 121.98306), true),
            ("Hau (Bassac)", (9.51667, 106.21667), true),
            ("Tien", (10.19, 106.75), true),
            ("Rio Grande (Orinoco)", (8.60722, -60.41083), false),
        ] {
            let river = rivers
                .iter()
                .find(|r| r.name == name)
                .unwrap_or_else(|| panic!("no {name}"));
            let nodes = today_river_nodes(&lattice, river);
            assert!(nodes.len() >= 5, "{name}: {nodes:?}");
            let last = lattice.dir(nodes[nodes.len() - 1]).0;
            assert!(km(last, at(mouth.0, mouth.1)) < 10.0, "{name}");
            let first = river.points[0];
            let joins = rivers
                .iter()
                .any(|r| r.name != name && r.points[r.points.len() - 1] == first);
            assert_eq!(joins, continues, "{name}");
        }
    }

    #[test]
    fn the_lower_indus_runs_upstream_first() {
        // The Indus's longest part reaches the sea.
        let indus = shipped_today_rivers()
            .iter()
            .filter(|r| r.name == "Indus")
            .max_by_key(|r| r.points.len())
            .unwrap();
        let (first, last) = (indus.points[0], indus.points[indus.points.len() - 1]);
        assert!(first.0 > last.0 + 5.0, "{first:?} to {last:?}");
        assert!(last.0 < 25.0, "it ends by the sea: {last:?}");
        // By Sehwan (26.4, 67.9), west of where the lattice ran it.
        assert!(
            indus
                .points
                .iter()
                .any(|&(lat, lon)| (lat - 26.4).abs() < 0.15 && (lon - 67.9).abs() < 0.15),
            "{:?}",
            indus.points
        );
    }
}
