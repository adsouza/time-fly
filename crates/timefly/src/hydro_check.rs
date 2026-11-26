//! The hidden `--hydro-check[=OUT_DIR]` (native, rivers and lakes Task 11, design §7): the
//! reality check. It loads the bake headless (the globe through the real loader, then the
//! overview, the region tiles over the boxes and the 0 ka climate from their files), runs the
//! continental pass at 0 ka (timed), and judges the result against the 1′ reference fixtures
//! (`tools/bake/timefly_bake/hydro_ref.py`) and the places below.
//!
//! The bars ([`CheckSpec::standard`]):
//!
//! - **Boxes:** the game's rivers with a drainage area of at least [`LINK_AREA_KM2`], as relaxed
//!   (valley-searched, pre-meander) centrelines over the inner-level terrain, against the
//!   reference's links ([`centreline_agreement`]): the Indus within 10 km for 80% of the
//!   reference's length with every link matched, the Amazon's flats (reference below 300 m)
//!   within 25 km for 80%. The coarse graph's numbers (straight node-to-node segments) are
//!   printed beside them, not judged.
//! - **Lakes:** at each point, `lake_tap(..).level_m` within [`LAKE_TOLERANCE_M`] of the lake
//!   level table's level (a table lake within [`TABLE_LAKE_KM`]), else of the lake map's baked
//!   surface there.
//! - **Outlets:** the river of the largest discharge within [`OUTLET_START_KM`] of a point flows
//!   within [`OUTLET_VIA_KM`] of its real gorge (when one is named) and ends in the ocean within
//!   [`OUTLET_SEA_KM`] of its real mouth ([`trace_outlet`]).
//! - **Arid basins:** no lake of at least `LARGE_LAKE_KM2` touching a box whose basin (the lake
//!   and the river nodes upstream of it) averages under [`ARID_PRECIP_MM`] of precipitation, in
//!   the game's network or the reference's.
//!
//! It writes `hydro-check-<box>.svg` per box (reference blue, game red, equirectangular, a
//! [`SCALE_BAR_KM`] scale bar) to `OUT_DIR` (the working directory without one), prints one line
//! per bar and exits 0 on PASS, [`FAILED_RESULT_EXIT_CODE`] on FAIL and 1 when it cannot run.
//!
//! The reference folder is [`REFERENCE_DIR`] unless [`REFERENCE_ENV`] names another. A folder
//! holding [`SPEC_FILE`] takes its places and bars from it instead of the standard ones (the
//! tests' tiny bake).

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use bevy::math::DVec3;
use bevy::prelude::*;
use serde::Deserialize;
use timefly_core::deep_time::YearsBp;
use timefly_core::formats::{Grid, read_tfgrid};
use timefly_core::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use timefly_core::hydrology::climate::{BeyerClimate, ClimateSource, ZonalClimate};
use timefly_core::hydrology::compare::{
    Agreement, Reference, centreline_agreement, parse_reference, trace_outlet,
};
use timefly_core::hydrology::lakemap::shipped_lake_map;
use timefly_core::hydrology::levels::shipped_lake_levels;
use timefly_core::hydrology::params::{HYDRO_RECENT_N, LARGE_LAKE_KM2, SUBPOINT_M};
use timefly_core::hydrology::{Down, EraHydrology, HydroGraph, LatticeSizes};
use timefly_core::worldgen::coarse::{
    CoarseField, frames_needed, snapshot_field, snapshot_sea_level_anomaly_m,
};
use timefly_core::worldgen::frame::EARTH_RADIUS_M;
use timefly_core::worldgen::generate::rivers::relaxed_centreline;
use timefly_core::worldgen::tiles::{TileSet, tile_of};

use crate::args::HydroCheck;
use crate::check_data::{CHECK_TIMEOUT, data_check_app, settle};
use crate::data::GlobeData;
use crate::diagnostics::Session;
use crate::diagnostics::session::FAILED_RESULT_EXIT_CODE;
use crate::hydro::climate::{CLIMATE_MANIFEST_PATH, ClimateManifest, climate_table};
use crate::hydro::job::{HydroInputs, run_parallel};
use crate::region::tiles::{OVERVIEW_PATH, TILE_MANIFEST_PATH, TileManifest, tile_path};

/// The committed reference fixtures (a dev build's source tree).
pub const REFERENCE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/hydro_reference"
);
/// Names another reference folder.
pub const REFERENCE_ENV: &str = "TIMEFLY_HYDRO_REFERENCE";
/// A reference folder's own places and bars ([`CheckSpec`] as JSON).
pub const SPEC_FILE: &str = "spec.json";
/// Rivers compared: a drainage area of at least this, on both sides.
pub const LINK_AREA_KM2: f64 = 10_000.0;
/// A lake level within this of the expected one passes.
pub const LAKE_TOLERANCE_M: f64 = 5.0;
/// A lake point takes the lake level table's level when one of its points is this close.
pub const TABLE_LAKE_KM: f64 = 25.0;
/// An outlet's river starts at the largest within this of its point.
pub const OUTLET_START_KM: f64 = 30.0;
/// ... passes within this of its gorge (about a lattice spacing and a half) ...
pub const OUTLET_VIA_KM: f64 = 15.0;
/// ... and ends in the ocean within this of its mouth.
pub const OUTLET_SEA_KM: f64 = 150.0;
/// A basin averaging less precipitation than this (mm/yr) holds no large lake.
pub const ARID_PRECIP_MM: f64 = 250.0;
/// The SVGs' scale bar and resolution.
pub const SCALE_BAR_KM: f64 = 100.0;
const SVG_PX_PER_DEG: f64 = 60.0;
const SVG_STEP_KM: f64 = 2.0;

/// A box and its bar.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct BoxBar {
    pub name: String,
    /// The reference fixture, in the reference folder.
    pub file: String,
    pub tolerance_km: f64,
    /// The share of the reference's counted length that must lie within the tolerance.
    pub min_share: f64,
    /// Whether every reference link must match.
    pub every_link: bool,
    /// Counts only reference points below this elevation (m), when set.
    #[serde(default)]
    pub below_m: Option<f64>,
}

/// A lake to check, at a point inside it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LakePoint {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

/// A river's real course: a point on it, its gorge (if checked) and its mouth, `(lat, lon)`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OutletBar {
    pub name: String,
    pub from: (f64, f64),
    #[serde(default)]
    pub via: Option<(String, (f64, f64))>,
    pub sea: (f64, f64),
}

/// What the check judges.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct CheckSpec {
    /// The recent lattice's face size (the tests' tiny bake takes a coarse one).
    #[serde(default)]
    pub recent_n: Option<u32>,
    pub boxes: Vec<BoxBar>,
    pub lakes: Vec<LakePoint>,
    pub outlets: Vec<OutletBar>,
}

impl CheckSpec {
    /// The plan's bars (rivers and lakes Task 11) and the controller's outlets (2026-11-25).
    pub fn standard() -> CheckSpec {
        let lake = |name: &str, lat, lon| LakePoint {
            name: name.into(),
            lat,
            lon,
        };
        let outlet = |name: &str, from, via: Option<(&str, (f64, f64))>, sea| OutletBar {
            name: name.into(),
            from,
            via: via.map(|(n, p)| (n.to_string(), p)),
            sea,
        };
        CheckSpec {
            recent_n: None,
            boxes: vec![
                BoxBar {
                    name: "indus".into(),
                    file: "indus.json".into(),
                    tolerance_km: 10.0,
                    min_share: 0.8,
                    every_link: true,
                    below_m: None,
                },
                BoxBar {
                    name: "amazon".into(),
                    file: "amazon.json".into(),
                    tolerance_km: 25.0,
                    min_share: 0.8,
                    every_link: false,
                    below_m: Some(300.0),
                },
            ],
            lakes: vec![
                lake("Caspian Sea", 42.0, 50.5),
                lake("Dead Sea", 31.5, 35.5),
                lake("Lake Titicaca", -15.8, -69.4),
                lake("Great Salt Lake", 41.1, -112.5),
                lake("Lake Victoria", -1.0, 33.0),
            ],
            outlets: vec![
                // From Budapest, through the Iron Gate, to the delta.
                outlet(
                    "Danube",
                    (47.50, 19.05),
                    Some(("the Iron Gate", (44.67, 22.5))),
                    (45.2, 29.7),
                ),
                // From Srinagar, out of the Kashmir valley at Baramulla, to the Indus delta.
                outlet(
                    "Jhelum",
                    (34.08, 74.80),
                    Some(("Baramulla", (34.2, 74.35))),
                    (24.0, 67.4),
                ),
                // From Skardu to the Arabian Sea.
                outlet("Indus", (35.3, 75.6), None, (24.0, 67.4)),
                // From Iquitos to the Atlantic.
                outlet("Amazon", (-3.75, -73.25), None, (-0.5, -49.5)),
            ],
        }
    }
}

/// The great-circle distance between two unit vectors (km).
fn km_between(a: DVec3, b: DVec3) -> f64 {
    2.0 * (0.5 * (a.normalize() - b.normalize()).length())
        .min(1.0)
        .asin()
        * EARTH_RADIUS_M
        * 1e-3
}

fn dir(lat: f64, lon: f64) -> DVec3 {
    geo_to_render(latlon_to_geo(lat, lon)).0
}

fn latlon(d: DVec3) -> (f64, f64) {
    geo_to_latlon(render_to_geo(RenderDir(d)))
}

/// One bar's verdict, its line, and lines of detail under it.
#[derive(Clone, Debug, PartialEq)]
pub struct BarResult {
    pub pass: bool,
    pub line: String,
    pub details: Vec<String>,
}

/// The game's river segments of at least [`LINK_AREA_KM2`] near `reference`'s box, as relaxed
/// centrelines over `floor_at` and as straight node-to-node segments.
pub fn box_rivers(
    hydro: &EraHydrology,
    reference: &Reference,
    floor_at: &impl Fn(RenderDir) -> f64,
) -> (Vec<Vec<DVec3>>, Vec<Vec<DVec3>>) {
    let lattice = &hydro.lattice;
    let nodes = hydro.rivers.nodes();
    let (n, w, s, e) = reference.bbox;
    let margin = 0.5;
    let near = |d: DVec3| {
        let (lat, lon) = latlon(d);
        (s - margin..=n + margin).contains(&lat) && (w - margin..=e + margin).contains(&lon)
    };
    let (mut relaxed, mut coarse) = (Vec::new(), Vec::new());
    for (k, node) in nodes.iter().enumerate() {
        if f64::from(node.area_km2) < LINK_AREA_KM2 {
            continue;
        }
        let end = match node.down {
            Down::River(j) => nodes[j as usize].node,
            Down::Ocean(j) | Down::Lake(_, j) | Down::Ice(j) | Down::Sink(j) => j,
        };
        let (a, b) = (lattice.dir(node.node).0, lattice.dir(end).0);
        if !(near(a) || near(b)) {
            continue;
        }
        relaxed.push(relaxed_centreline(hydro, k as u32, SUBPOINT_M, floor_at));
        coarse.push(vec![a, b]);
    }
    (relaxed, coarse)
}

/// The box bar on the relaxed rivers, with the coarse graph's numbers beside it.
pub fn judge_box(
    bar: &BoxBar,
    reference: &Reference,
    relaxed: &[Vec<DVec3>],
    coarse: &[Vec<DVec3>],
) -> BarResult {
    let filter = |p: &timefly_core::hydrology::compare::RefPoint| {
        p.area_km2 >= LINK_AREA_KM2 && bar.below_m.is_none_or(|top| p.elevation_m < top)
    };
    let r = centreline_agreement(relaxed, &reference.links, bar.tolerance_km, filter);
    let c = centreline_agreement(coarse, &reference.links, bar.tolerance_km, filter);
    let pass = r.links > 0
        && r.share_within >= bar.min_share
        && (!bar.every_link || r.matched_links == r.links);
    let describe = |a: &Agreement| {
        format!(
            "{:.1}% within {} km, {}/{} links matched, worst {:.1} km",
            100.0 * a.share_within,
            bar.tolerance_km,
            a.matched_links,
            a.links,
            a.worst_km
        )
    };
    let scope = bar
        .below_m
        .map_or(String::new(), |m| format!(" (reference below {m} m)"));
    let needs = format!(
        "{:.0}%{}",
        100.0 * bar.min_share,
        if bar.every_link {
            " and every link"
        } else {
            ""
        }
    );
    // The reference links the relaxed rivers miss, longest first.
    let mut missed: Vec<(f64, String)> = Vec::new();
    for link in &reference.links {
        let one = centreline_agreement(
            relaxed,
            std::slice::from_ref(link),
            bar.tolerance_km,
            filter,
        );
        if one.links == 0 || one.matched_links == 1 {
            continue;
        }
        let (a, b) = (link.points[0], link.points[link.points.len() - 1]);
        let km: f64 = link
            .points
            .windows(2)
            .map(|w| km_between(w[0].dir, w[1].dir))
            .sum();
        missed.push((
            km,
            format!(
                "  missed link ({:.2}, {:.2}) to ({:.2}, {:.2}): {km:.0} km, {:.0}-{:.0} km2, \
                 {:.0} m3/s, {:.0}% within, worst {:.1} km",
                a.lat_deg,
                a.lon_deg,
                b.lat_deg,
                b.lon_deg,
                a.area_km2,
                b.area_km2,
                link.discharge_m3s,
                100.0 * one.share_within,
                one.worst_km
            ),
        ));
    }
    missed.sort_by(|x, y| y.0.total_cmp(&x.0));
    BarResult {
        pass,
        line: format!(
            "box {}{scope}: relaxed {} (needs {needs}); coarse graph {} - {}",
            bar.name,
            describe(&r),
            describe(&c),
            verdict(pass)
        ),
        details: missed.into_iter().map(|(_, line)| line).collect(),
    }
}

fn verdict(pass: bool) -> &'static str {
    if pass { "PASS" } else { "FAIL" }
}

/// The level a lake point should read: the lake level table's (a point within
/// [`TABLE_LAKE_KM`]), else the lake map's baked surface; and where it came from.
pub fn expected_level(lake: &LakePoint) -> Option<(f32, &'static str)> {
    let at = dir(lake.lat, lake.lon);
    if let Some(t) = shipped_lake_levels()
        .iter()
        .find(|t| km_between(dir(t.lat, t.lon), at) <= TABLE_LAKE_KM)
    {
        return Some((t.level_m, "the lake level table"));
    }
    let map = shipped_lake_map();
    map.lake_at(lake.lat, lake.lon)
        .map(|id| (map.level(id), "the lake map's fine surface"))
}

pub fn judge_lake(hydro: &EraHydrology, lake: &LakePoint) -> BarResult {
    let got = hydro.lake_tap(RenderDir(dir(lake.lat, lake.lon)));
    let (pass, line) = match (got, expected_level(lake)) {
        (Some(tap), Some((want, from))) => {
            let diff = f64::from(tap.level_m) - f64::from(want);
            (
                diff.abs() <= LAKE_TOLERANCE_M,
                format!(
                    "level {:.1} m, {from} {:.1} m (off {:+.1} m, {LAKE_TOLERANCE_M} m allowed)",
                    tap.level_m, want, diff
                ),
            )
        }
        (None, _) => (false, "no lake there".to_string()),
        (Some(tap), None) => (
            false,
            format!("level {:.1} m, but no expected level", tap.level_m),
        ),
    };
    BarResult {
        pass,
        line: format!(
            "lake {} ({}, {}): {line} - {}",
            lake.name,
            lake.lat,
            lake.lon,
            verdict(pass)
        ),
        details: Vec::new(),
    }
}

pub fn judge_outlet(hydro: &EraHydrology, outlet: &OutletBar) -> BarResult {
    let via = outlet.via.as_ref().map(|(_, (lat, lon))| dir(*lat, *lon));
    let from = dir(outlet.from.0, outlet.from.1);
    let (pass, line) = match trace_outlet(hydro, from, OUTLET_START_KM, via) {
        None => (false, format!("no river within {OUTLET_START_KM} km")),
        Some(t) => {
            let sea_km = km_between(t.end_dir, dir(outlet.sea.0, outlet.sea.1));
            let (lat, lon) = latlon(t.end_dir);
            let ocean = matches!(t.end, Down::Ocean(_));
            let via_ok = t.via_km.is_none_or(|km| km <= OUTLET_VIA_KM);
            let via_text = match (&outlet.via, t.via_km) {
                (Some((name, _)), Some(km)) => {
                    format!("passes {km:.1} km from {name} ({OUTLET_VIA_KM} km allowed), ")
                }
                _ => String::new(),
            };
            let end = match t.end {
                Down::Ocean(_) => "the ocean",
                Down::Lake(..) => "a lake",
                Down::Ice(_) => "the ice",
                Down::Sink(_) => "a dry sink",
                Down::River(_) => "a river",
            };
            (
                ocean && via_ok && sea_km <= OUTLET_SEA_KM,
                format!(
                    "{} river nodes through {} lakes, {via_text}ends in {end} at ({lat:.2}, \
                     {lon:.2}), {sea_km:.0} km from its mouth ({OUTLET_SEA_KM} km allowed)",
                    t.nodes, t.lakes
                ),
            )
        }
    };
    BarResult {
        pass,
        line: format!("outlet {}: {line} - {}", outlet.name, verdict(pass)),
        details: Vec::new(),
    }
}

/// The game's lakes of at least `LARGE_LAKE_KM2` touching a box: `(lake, area, basin mean
/// precipitation)`, the basin being the lake's nodes and the river nodes upstream of it,
/// weighted by area.
pub fn large_lakes(
    hydro: &EraHydrology,
    boxes: &[Reference],
    precip_at: &impl Fn(u32) -> f64,
) -> Vec<(u16, f64, f64)> {
    let lattice = &hydro.lattice;
    let mut footprints: Vec<Vec<u32>> = vec![Vec::new(); hydro.lakes.len()];
    for v in 0..lattice.len() {
        if let Some(l) = hydro.lake_of(v) {
            footprints[l as usize].push(v);
        }
    }
    let rivers = hydro.rivers.nodes();
    let mut out = Vec::new();
    for (l, footprint) in footprints.iter().enumerate() {
        if hydro.lakes[l].area_km2 < LARGE_LAKE_KM2 {
            continue;
        }
        let inside = footprint.iter().any(|&v| {
            let (lat, lon) = latlon(lattice.dir(v).0);
            boxes.iter().any(|b| b.contains(lat, lon))
        });
        if !inside {
            continue;
        }
        let mut basin: Vec<u32> = footprint.clone();
        let mut stack: Vec<u32> = (0..rivers.len() as u32)
            .filter(|&k| matches!(rivers[k as usize].down, Down::Lake(x, _) if usize::from(x) == l))
            .collect();
        while let Some(k) = stack.pop() {
            basin.push(rivers[k as usize].node);
            stack.extend_from_slice(hydro.rivers.upstream(k));
        }
        let (mut sum, mut weight) = (0.0, 0.0);
        for &v in &basin {
            let a = lattice.area_m2(v);
            sum += a * precip_at(v);
            weight += a;
        }
        out.push((l as u16, hydro.lakes[l].area_km2, sum / weight));
    }
    out
}

pub fn judge_arid(game: &[(u16, f64, f64)], references: &[(String, Reference)]) -> BarResult {
    let wet = |p: f64| p >= ARID_PRECIP_MM;
    let game_bad = game.iter().filter(|g| !wet(g.2)).count();
    let mut reference_large = 0;
    let mut reference_bad = 0;
    for (_, r) in references {
        for lake in r.lakes.iter().filter(|l| l.area_km2 >= LARGE_LAKE_KM2) {
            reference_large += 1;
            reference_bad += usize::from(!wet(lake.basin_precip_mm));
        }
    }
    let pass = game_bad == 0 && reference_bad == 0;
    let list: Vec<String> = game
        .iter()
        .map(|(l, a, p)| format!("lake {l} {a:.0} km2 at {p:.0} mm/yr"))
        .collect();
    BarResult {
        pass,
        line: format!(
            "arid basins (< {ARID_PRECIP_MM} mm/yr): game {game_bad} of {} large lakes in the \
             boxes [{}], reference {reference_bad} of {reference_large} - {}",
            game.len(),
            list.join(", "),
            verdict(pass)
        ),
        details: Vec::new(),
    }
}

/// The box's SVG: reference blue, game red, equirectangular, with a scale bar.
pub fn box_svg(name: &str, reference: &Reference, game: &[Vec<DVec3>], line: &str) -> String {
    let (n, w, s, e) = reference.bbox;
    let k = SVG_PX_PER_DEG;
    let (width, height) = ((e - w) * k, (n - s) * k);
    let xy = |lat: f64, lon: f64| ((lon - w) * k, (n - lat) * k);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" \
         viewBox=\"0 0 {width:.0} {height:.0}\">"
    );
    let _ = writeln!(
        out,
        "<title>hydro check {name}: reference blue, game red</title>"
    );
    let _ = writeln!(
        out,
        "<rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/>"
    );
    let mut path = |points: &mut dyn Iterator<Item = (f64, f64)>, colour: &str, px: f64| {
        let d: Vec<String> = points
            .map(|(lat, lon)| {
                let (x, y) = xy(lat, lon);
                format!("{x:.1},{y:.1}")
            })
            .collect();
        if d.len() >= 2 {
            let _ = writeln!(
                out,
                "<polyline fill=\"none\" stroke=\"{colour}\" stroke-width=\"{px}\" \
                 points=\"{}\"/>",
                d.join(" ")
            );
        }
    };
    for link in &reference.links {
        path(
            &mut link.points.iter().map(|p| (p.lat_deg, p.lon_deg)),
            "#1f6feb",
            2.0,
        );
    }
    for line in game {
        // Thinned to points about SVG_STEP_KM apart: the relaxed lines' 500 m sub-points would
        // make the Amazon's file megabytes.
        let mut kept: Vec<DVec3> = Vec::new();
        for (k, &d) in line.iter().enumerate() {
            let last = k + 1 == line.len();
            if kept
                .last()
                .is_none_or(|&p| last || km_between(p, d) >= SVG_STEP_KM)
            {
                kept.push(d);
            }
        }
        path(&mut kept.iter().map(|&d| latlon(d)), "#d1242f", 1.2);
    }
    let lat_mid = 0.5 * (n + s);
    let bar_px = SCALE_BAR_KM / (EARTH_RADIUS_M * 1e-3 * lat_mid.to_radians().cos()) * 180.0
        / std::f64::consts::PI
        * k;
    let (x0, y0) = (20.0, height - 20.0);
    let _ = writeln!(
        out,
        "<line x1=\"{x0}\" y1=\"{y0}\" x2=\"{:.1}\" y2=\"{y0}\" stroke=\"#000000\" \
         stroke-width=\"3\"/>",
        x0 + bar_px
    );
    let _ = writeln!(
        out,
        "<text x=\"{x0}\" y=\"{:.0}\" font-family=\"sans-serif\" font-size=\"14\">{SCALE_BAR_KM} \
         km</text>",
        y0 - 8.0
    );
    let _ = writeln!(
        out,
        "<text x=\"20\" y=\"24\" font-family=\"sans-serif\" font-size=\"14\">{}</text>",
        line.replace('&', "&amp;").replace('<', "&lt;")
    );
    out.push_str("</svg>\n");
    out
}

/// The reference folder: [`REFERENCE_ENV`] or [`REFERENCE_DIR`].
pub fn reference_dir() -> PathBuf {
    std::env::var_os(REFERENCE_ENV).map_or_else(|| PathBuf::from(REFERENCE_DIR), PathBuf::from)
}

/// The folder's [`SPEC_FILE`], or the standard spec without one.
pub fn load_spec(dir: &Path) -> Result<CheckSpec, String> {
    let path = dir.join(SPEC_FILE);
    if !path.exists() {
        return Ok(CheckSpec::standard());
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn read_grid(path: &Path) -> Result<Arc<Grid>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    read_tfgrid(&bytes)
        .map(Arc::new)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The tiles covering a box and a degree around it (a box off the antimeridian).
fn tiles_for(bbox: (f64, f64, f64, f64)) -> Vec<(u8, u8)> {
    let (n, w, s, e) = bbox;
    let (top, left) = tile_of((n + 1.0).min(89.9), w - 1.0);
    let (bottom, right) = tile_of((s - 1.0).max(-89.9), e + 1.0);
    let mut out = Vec::new();
    for r in top..=bottom {
        for c in left..=right {
            out.push((r, c));
        }
    }
    out
}

/// What a run measured.
pub struct HydroCheckReport {
    pub lines: Vec<String>,
    pub pass: bool,
    pub svgs: Vec<PathBuf>,
}

/// Runs the check on the bake under `root` (an asset folder holding `data/`), writing SVGs to
/// `out_dir`. `Err` when it cannot run at all.
pub fn hydro_check(
    root: &Path,
    out_dir: &Path,
    session: Option<&Session>,
) -> Result<HydroCheckReport, String> {
    let ref_dir = reference_dir();
    let spec = load_spec(&ref_dir)?;
    let mut references = Vec::new();
    for bar in &spec.boxes {
        let path = ref_dir.join(&bar.file);
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        references.push((
            bar.name.clone(),
            parse_reference(&text).map_err(|e| format!("{}: {e}", path.display()))?,
        ));
    }

    // The globe, through the real loader.
    let globe_dir = root.join("data/globe");
    let mut app = data_check_app(&globe_dir, session);
    settle(&mut app, CHECK_TIMEOUT).map_err(|e| format!("globe data: {e}"))?;
    let data = app.world().resource::<GlobeData>();
    let (timeline, crust) = (data.timeline.clone(), data.crust.clone());
    let mut frames: Vec<Option<Arc<Grid>>> = vec![None; timeline.frames().len()];
    for i in frames_needed(&timeline, 0.0) {
        frames[i] = Some(read_grid(&globe_dir.join(data.frame_path(i).path()))?);
    }

    // The overview, and the tiles over the boxes for the relaxation's terrain.
    let manifest_path = root.join(TILE_MANIFEST_PATH);
    let manifest = TileManifest::parse(
        &fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut overview_only = TileSet::new();
    overview_only
        .set_overview(read_grid(&root.join(manifest.payload_path(OVERVIEW_PATH)))?)
        .map_err(|e| format!("overview: {e}"))?;
    let mut tiles = overview_only.clone();
    let mut wanted: Vec<(u8, u8)> = references
        .iter()
        .flat_map(|(_, r)| tiles_for(r.bbox))
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    for (r, c) in wanted {
        if manifest.tiles.contains_key(&(r, c)) {
            let grid = read_grid(&root.join(manifest.payload_path(&tile_path(r, c))))?;
            tiles
                .insert(r, c, grid)
                .map_err(|e| format!("tile r{r:02}c{c:02}: {e}"))?;
        }
    }

    // The 0 ka climate.
    let climate_path = root.join(CLIMATE_MANIFEST_PATH);
    let climate = ClimateManifest::parse(
        &fs::read(&climate_path).map_err(|e| format!("{}: {e}", climate_path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", climate_path.display()))?;
    let age = YearsBp(0.0);
    let ([younger, older], w) = climate
        .bracket(age)
        .ok_or("the climate manifest has no 0 ka bracket")?;
    let grids = [
        read_grid(&root.join(younger.path()))?,
        read_grid(&root.join(older.path()))?,
    ];

    let sizes = LatticeSizes {
        recent_n: spec.recent_n.unwrap_or(HYDRO_RECENT_N),
        ..LatticeSizes::default()
    };
    let inputs = HydroInputs {
        age,
        sizes,
        timeline: timeline.clone(),
        crust: crust.clone(),
        tiles: overview_only,
        frames: frames.clone(),
        climate: Some((grids.clone(), w)),
        table: climate_table(),
    };
    let pass_out =
        run_parallel(&inputs, &AtomicBool::new(false)).map_err(|e| format!("the pass: {e}"))?;
    drop(app);
    let hydro = &pass_out.hydrology;
    let mut lines = vec![format!(
        "pass at 0 ka: {:.2} s ({:.2} s sampling), lattice n = {}, {} river nodes, {} lakes, \
         Beyer climate: {}",
        pass_out.cpu.as_secs_f64(),
        pass_out.sampling.as_secs_f64(),
        hydro.lattice.n,
        hydro.rivers.nodes().len(),
        hydro.lakes.len(),
        pass_out.beyer
    )];

    let field = snapshot_field(&timeline, &crust, &tiles, false, 0.0, |i| {
        frames[i].as_deref().expect("frames_needed were read")
    })
    .map_err(|e| format!("the field: {e}"))?;
    let floor_at = |d: RenderDir| field.sample(d).elevation_m;
    let table = climate_table();
    let beyer = BeyerClimate::new(
        [&grids[0], &grids[1]],
        w,
        ZonalClimate {
            table: &table,
            sea_level_anomaly_m: snapshot_sea_level_anomaly_m(&timeline, 0.0),
        },
    );

    let mut pass = true;
    let mut svgs = Vec::new();
    fs::create_dir_all(out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    for (bar, (_, reference)) in spec.boxes.iter().zip(&references) {
        let (relaxed, coarse) = box_rivers(hydro, reference, &floor_at);
        let result = judge_box(bar, reference, &relaxed, &coarse);
        pass &= result.pass;
        let svg = out_dir.join(format!("hydro-check-{}.svg", bar.name));
        fs::write(&svg, box_svg(&bar.name, reference, &relaxed, &result.line))
            .map_err(|e| format!("{}: {e}", svg.display()))?;
        svgs.push(svg);
        lines.push(result.line);
        lines.extend(result.details);
    }
    for lake in &spec.lakes {
        let result = judge_lake(hydro, lake);
        pass &= result.pass;
        lines.push(result.line);
    }
    for outlet in &spec.outlets {
        let result = judge_outlet(hydro, outlet);
        pass &= result.pass;
        lines.push(result.line);
    }
    let precip_at = |v: u32| {
        let d = hydro.lattice.dir(v);
        let e = field.sample_overview(d).elevation_m as f32;
        f64::from(beyer.climate(d, e, 0.0).precip_mm)
    };
    let boxes: Vec<Reference> = references.iter().map(|(_, r)| r.clone()).collect();
    let arid = judge_arid(&large_lakes(hydro, &boxes, &precip_at), &references);
    pass &= arid.pass;
    lines.push(arid.line);
    lines.push(format!("hydro check {}", verdict(pass)));
    Ok(HydroCheckReport { lines, pass, svgs })
}

/// Runs `--hydro-check`: prints the lines (stdout on PASS, stderr on FAIL), logs them, and exits
/// 0 on PASS, [`FAILED_RESULT_EXIT_CODE`] on FAIL and 1 when it cannot run. Either way the
/// session ends clean.
pub fn run_hydro_check(session: Session, check: &HydroCheck) -> AppExit {
    let root = bevy::asset::io::file::FileAssetReader::get_base_path().join("assets");
    let out_dir = PathBuf::from(check.out_dir.as_deref().unwrap_or("."));
    let exit = match hydro_check(&root, &out_dir, Some(&session)) {
        Ok(report) => {
            for line in &report.lines {
                info!("{line}");
                if report.pass {
                    println!("{line}");
                } else {
                    session.eprint(line);
                }
            }
            for svg in &report.svgs {
                println!("wrote {}", svg.display());
            }
            if report.pass {
                AppExit::Success
            } else {
                AppExit::from_code(FAILED_RESULT_EXIT_CODE)
            }
        }
        Err(error) => {
            error!(%error, "hydro check could not run");
            session.eprint(&format!("hydro check could not run: {error}"));
            AppExit::from_code(1)
        }
    };
    session.finish_clean(&exit);
    exit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_spec_holds_the_plans_bars() {
        let s = CheckSpec::standard();
        assert_eq!(s.recent_n, None);
        let indus = &s.boxes[0];
        assert_eq!(
            (
                indus.tolerance_km,
                indus.min_share,
                indus.every_link,
                indus.below_m
            ),
            (10.0, 0.8, true, None)
        );
        let amazon = &s.boxes[1];
        assert_eq!(
            (
                amazon.tolerance_km,
                amazon.min_share,
                amazon.every_link,
                amazon.below_m
            ),
            (25.0, 0.8, false, Some(300.0))
        );
        assert_eq!(s.lakes.len(), 5);
        // The Caspian reads the table; the other four the map's fine surfaces.
        let (_, from) = expected_level(&s.lakes[0]).unwrap();
        assert_eq!(from, "the lake level table");
        for lake in &s.lakes[1..] {
            let (level, from) = expected_level(lake).unwrap_or_else(|| panic!("{lake:?}"));
            assert_eq!(from, "the lake map's fine surface", "{lake:?}");
            assert!(level.is_finite());
        }
        let names: Vec<&str> = s.outlets.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Danube", "Jhelum", "Indus", "Amazon"]);
    }

    #[test]
    fn a_spec_file_parses() {
        let json = r#"{"recent_n": 64,
            "boxes": [{"name": "tiny", "file": "tiny.json", "tolerance_km": 50,
                       "min_share": 0.5, "every_link": false}],
            "lakes": [{"name": "x", "lat": 1.0, "lon": 2.0}],
            "outlets": [{"name": "r", "from": [1, 2], "via": ["g", [1.5, 2.5]], "sea": [0, 0]}]}"#;
        let spec: CheckSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.recent_n, Some(64));
        assert_eq!(spec.boxes[0].below_m, None);
        assert_eq!(spec.outlets[0].via, Some(("g".to_string(), (1.5, 2.5))));
    }

    #[test]
    fn the_svg_draws_both_networks_and_a_scale_bar() {
        let r = parse_reference(
            r#"{"box": [1, 0, 0, 2], "links": [{"lonlat": [[0.5, 0.5], [1.5, 0.5]],
                "area_km2": [1e4, 2e4], "elevation_m": [10, 5], "discharge_m3s": 30}]}"#,
        )
        .unwrap();
        let svg = box_svg("t", &r, &[vec![dir(0.4, 0.5), dir(0.4, 1.5)]], "a < b & c");
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("width=\"120\" height=\"60\""), "{svg}");
        assert!(svg.contains("stroke=\"#1f6feb\"") && svg.contains("stroke=\"#d1242f\""));
        assert!(svg.contains("points=\"30.0,30.0 90.0,30.0\""), "{svg}");
        assert!(svg.contains("100 km") && svg.contains("a &lt; b &amp; c"));
        // 100 km at 0.5 degrees of latitude is 0.8994 degrees of longitude: 54.0 px from x = 20.
        assert!(svg.contains("x2=\"74.0\""), "{svg}");
    }
}
