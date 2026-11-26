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
//! - **Lakes:** at each point, `lake_tap(..).level_m`, and every pass lake holding nodes of the
//!   point's map lake (Task 11f), within [`LAKE_TOLERANCE_M`] of the lake level table's level (a
//!   table point within [`TABLE_LAKE_KM`], or in the point's map lake), else of the lake map's
//!   baked surface there.
//! - **Outlets:** the river of the largest discharge within [`OUTLET_START_KM`] of a point flows
//!   within [`OUTLET_VIA_KM`] of each of its named places ([`OutletBar::vias`]) and ends in the
//!   ocean (or, for [`OutletEnd::Lake`], a lake with no outflow: the Volga's Caspian) within
//!   [`OUTLET_SEA_KM`] of its real mouth, or its own allowance ([`OutletBar::sea_km`]; the
//!   Amazon's is 160 km) ([`trace_outlet`], [`judge_trace`]). Task 11d added the world's major
//!   rivers, every point cited ([`OutletBar::source`]).
//! - **Arid basins:** no lake of at least `LARGE_LAKE_KM2` touching a box whose basin (the lake
//!   and the river nodes upstream of it) averages under [`ARID_PRECIP_MM`] of precipitation, in
//!   the game's network or the reference's.
//!
//! It also prints a discharge table (informative, never a bar; Task 11d): at each
//! [`DischargeGauge`], the game's discharge ([`game_discharge`]) against the published long-term
//! mean, flagged outside a ratio of [`DISCHARGE_FLAG_RATIO`] ([`discharge_row`]).
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
    Agreement, OutletTrace, Reference, centreline_agreement, parse_reference, trace_outlet,
    trace_outlet_path,
};
use timefly_core::hydrology::lakemap::{LakeMap, LakeRing, shipped_lake_map};
use timefly_core::hydrology::levels::shipped_lake_levels;
use timefly_core::hydrology::params::{HYDRO_RECENT_N, LARGE_LAKE_KM2, SUBPOINT_M};
use timefly_core::hydrology::today_rivers::{
    CourseBed, CourseStep, course_beds, course_steps, shipped_today_rivers,
};
use timefly_core::hydrology::{
    CubeLattice, Down, EraHydrology, HydroGraph, LakeKind, LatticeSizes, SnapshotSource,
    hydro_lattice_for,
};
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

/// What a river's real end is: the ocean, or a lake with no outlet (the Volga's Caspian).
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutletEnd {
    #[default]
    Ocean,
    Lake,
}

/// A river's real course: a point on it, the places it passes (in order, each checked) and its
/// mouth, `(lat, lon)`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct OutletBar {
    pub name: String,
    pub from: (f64, f64),
    #[serde(default)]
    pub vias: Vec<(String, (f64, f64))>,
    pub sea: (f64, f64),
    /// This river's own allowance at its mouth (km), in place of [`OUTLET_SEA_KM`].
    #[serde(default)]
    pub sea_km: Option<f64>,
    /// What the river must end in.
    #[serde(default)]
    pub ends_in: OutletEnd,
    /// Where its points come from.
    #[serde(default)]
    pub source: String,
}

/// A gauge for the discharge table (informative): the published long-term mean discharge at a
/// point on a river.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct DischargeGauge {
    pub river: String,
    pub gauge: String,
    pub at: (f64, f64),
    pub mean_m3s: f64,
    pub source: String,
}

impl OutletBar {
    /// How far from its mouth the river may end (km).
    pub fn sea_allowance_km(&self) -> f64 {
        self.sea_km.unwrap_or(OUTLET_SEA_KM)
    }
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
    #[serde(default)]
    pub gauges: Vec<DischargeGauge>,
}

impl CheckSpec {
    /// The plan's bars (rivers and lakes Task 11) and the controller's outlets (2026-11-25).
    pub fn standard() -> CheckSpec {
        let lake = |name: &str, lat, lon| LakePoint {
            name: name.into(),
            lat,
            lon,
        };
        let outlet = |name: &str, from, vias: &[(&str, (f64, f64))], sea, source: &str| OutletBar {
            name: name.into(),
            from,
            vias: vias.iter().map(|&(n, p)| (n.to_string(), p)).collect(),
            sea,
            sea_km: None,
            ends_in: OutletEnd::Ocean,
            source: format!("English Wikipedia, read 2026-11-26: {source}"),
        };
        let gauge = |river: &str, gauge: &str, at, mean_m3s, source: &str| DischargeGauge {
            river: river.into(),
            gauge: gauge.into(),
            at,
            mean_m3s,
            source: source.into(),
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
                // Task 11f: the map bakes the Great Lakes at their bed. Each point lies in its
                // table lake's own map lake, far from its table point, near where a bed
                // fragment was; the bar judges the whole map lake.
                lake("Lake Michigan (Green Bay)", 44.7, -87.85),
                lake("Lake Huron (by the St. Marys' mouth)", 45.95, -83.9),
                lake("Lake Erie (its western basin)", 41.7, -82.9),
            ],
            // Every point is an English Wikipedia article's coordinates (its own, or its
            // infobox's mouth), read 2026-11-26 (the owner's ruling: cited outlet points); each
            // entry's `source` names them, and docs/testing/rivers/reality-check.md lists them.
            outlets: vec![
                outlet(
                    "Danube",
                    (47.49250, 19.05139),
                    &[("the Iron Gate", (44.67111, 22.52972))],
                    (45.21750, 29.76139),
                    "from \"Budapest\"; via \"Iron Gates\"; mouth \"Danube\" (infobox)",
                ),
                outlet(
                    "Jhelum",
                    (34.09000, 74.79000),
                    &[("Baramulla", (34.20869, 74.34350))],
                    (23.99500, 67.43500),
                    "from \"Srinagar\"; via \"Baramulla\"; mouth \"Indus River\" (infobox)",
                ),
                outlet(
                    "Indus",
                    (35.29028, 75.64444),
                    &[],
                    (23.99500, 67.43500),
                    "from \"Skardu\"; mouth \"Indus River\" (infobox)",
                ),
                // Its allowance is 160 km (owner, 2026-11-26): its estuary is about 300 km across.
                OutletBar {
                    sea_km: Some(160.0),
                    ..outlet(
                        "Amazon",
                        (-3.750, -73.250),
                        &[],
                        (0.70778, -50.08944),
                        "from \"Iquitos\"; mouth \"Amazon River\" (infobox: the North Channel)",
                    )
                },
                // Task 11d: the world's major river systems (owner, 2026-11-26).
                outlet(
                    "Ganges",
                    (26.449923, 80.331874),
                    &[
                        ("Varanasi", (25.31888889, 83.01277778)),
                        ("the Farakka Barrage", (24.80444444, 87.93305556)),
                    ],
                    (22.0, 90.9),
                    "from \"Kanpur\"; via \"Varanasi\", \"Farakka Barrage\"; mouth \"Ganges\" \
                     (infobox)",
                ),
                // The Brahmaputra's own mouth is its confluence with the Ganges; it reaches the
                // sea by the Ganges's mouth (as the Jhelum takes the Indus's).
                outlet(
                    "Brahmaputra",
                    (29.185, 92.51666667),
                    &[
                        ("Pasighat", (28.07, 95.33)),
                        ("Guwahati", (26.17222222, 91.74583333)),
                    ],
                    (22.0, 90.9),
                    "from \"Zangmu Dam\" (the Yarlung Tsangpo above the Great Bend); via \
                     \"Pasighat\", \"Guwahati\"; mouth \"Ganges\" (infobox)",
                ),
                outlet(
                    "Yangtze",
                    (29.5637, 106.5504),
                    &[
                        ("the Three Gorges", (31.01777778, 110.22861111)),
                        ("Yichang", (30.692, 111.287)),
                        ("Wuhan", (30.595, 114.2975)),
                    ],
                    (31.39361111, 121.98305556),
                    "from \"Chongqing\"; via \"Three Gorges\", \"Yichang\", \"Wuhan\"; mouth \
                     \"Yangtze\" (infobox)",
                ),
                // Zhengzhou's own point stands 15 km from the river: Huayuankou, its riverside
                // district and gauge, is the via.
                outlet(
                    "Yellow River",
                    (35.989, 101.471),
                    &[
                        ("Lanzhou", (36.0606, 103.8268)),
                        ("Zhengzhou (Huayuankou)", (34.87694444, 113.67277778)),
                    ],
                    (37.763, 119.162),
                    "from \"Guide County\"; via \"Lanzhou\", \"Huayuankou, Henan\"; mouth \
                     \"Yellow River\" (infobox)",
                ),
                outlet(
                    "Pearl",
                    (23.4, 110.08333333),
                    &[("Wuzhou", (23.4767, 111.279))],
                    (22.76666667, 113.63333333),
                    "from \"Guiping\"; via \"Wuzhou\"; mouth \"Pearl River\" (the article's \
                     coordinates: its infobox gives none)",
                ),
                outlet(
                    "Mekong",
                    (17.98, 102.63),
                    &[("Phnom Penh", (11.56944444, 104.92111111))],
                    (10.19, 106.75),
                    "from \"Vientiane\"; via \"Phnom Penh\"; mouth \"Mekong\" (infobox)",
                ),
                outlet(
                    "Nile",
                    (0.44361111, 33.185),
                    &[
                        ("Khartoum", (15.6, 32.5)),
                        ("Aswan", (24.08888889, 32.89972222)),
                    ],
                    (31.5125, 31.82555556),
                    "from \"Nalubaale Hydroelectric Power Station\" (the Owen Falls Dam at Jinja); via \"Khartoum\", \"Aswan\"; mouth \"Ras El Bar\" \
                     (31 30 45 N, 31 49 32 E, read 2026-11-26: \"at the mouth of the Damietta Nile branch\"; \
                     fix round 4, the controller's ruling as the owner chose for the Orinoco; the \
                     \"Nile\" infobox's 30 10 21 N, 31 8 24 E is the delta's head)",
                ),
                outlet(
                    "Congo",
                    (-2.95, 25.95),
                    &[
                        ("Kisangani", (0.51666667, 25.2)),
                        ("Kinshasa", (-4.32194444, 15.31194444)),
                    ],
                    (-6.075, 12.45),
                    "from \"Kindu\"; via \"Kisangani\", \"Kinshasa\"; mouth \"Congo River\" \
                     (infobox)",
                ),
                // Timbuktu stands 13 km from the river: Kabara, its port, is the via.
                outlet(
                    "Niger",
                    (12.63916667, -8.00277778),
                    &[
                        ("the Inner Niger Delta", (15.2, -4.1)),
                        ("Timbuktu (Kabara)", (16.70695, -2.9853)),
                        ("Lokoja", (7.80194444, 6.74416667)),
                    ],
                    (5.32222222, 6.46916667),
                    "from \"Bamako\"; via \"Inner Niger Delta\", \"Kabara, Mali\", \"Lokoja\"; \
                     mouth \"Niger River\" (infobox)",
                ),
                outlet(
                    "Mississippi",
                    (44.98194444, -93.26916667),
                    &[
                        ("St. Louis", (38.62722222, -90.19777778)),
                        ("New Orleans", (29.9761, -90.0783)),
                    ],
                    (29.15111111, -89.25333333),
                    "from \"Minneapolis\"; via \"St. Louis\", \"New Orleans\"; mouth \
                     \"Mississippi River\" (infobox)",
                ),
                outlet(
                    "Hudson",
                    (43.31222222, -73.64833333),
                    &[("Albany", (42.6525, -73.75722222))],
                    (40.69666667, -74.02833333),
                    "from \"Glens Falls, New York\"; via \"Albany, New York\"; mouth \
                     \"Hudson River\" (infobox)",
                ),
                outlet(
                    "Yenisei",
                    (51.71666667, 94.45),
                    &[("Krasnoyarsk", (56.00888889, 92.87194444))],
                    (71.83333333, 82.66666667),
                    "from \"Kyzyl\"; via \"Krasnoyarsk\"; mouth \"Yenisei\" (infobox: the \
                     Yenisey Gulf)",
                ),
                outlet(
                    "Ob",
                    (53.34861111, 83.77638889),
                    &[("Novosibirsk", (55.05, 82.95))],
                    (66.53388889, 71.39472222),
                    "from \"Barnaul\"; via \"Novosibirsk\"; mouth \"Ob\" (infobox: the Gulf \
                     of Ob)",
                ),
                outlet(
                    "Orinoco",
                    (7.65, -66.16666667),
                    &[("Ciudad Bolívar", (8.13793, -63.548266))],
                    (8.60722222, -60.41083333),
                    "from \"Caicara del Orinoco\"; via \"Ciudad Bolívar\"; mouth \"Barima Point\" on the \
                     Boca Grande, the Rio Grande's mouth (fix round 1, the owner's ruling; \
                     Britannica places the Boca Grande at Barima Point)",
                ),
                outlet(
                    "Paraná",
                    (-27.48333333, -58.81666667),
                    &[("Rosario", (-32.9575, -60.63944444))],
                    (-34.00138889, -58.39361111),
                    "from \"Corrientes\"; via \"Rosario\"; mouth \"Paraná River\" (infobox: \
                     the Paraná Guazú into the Río de la Plata)",
                ),
                outlet(
                    "Lena",
                    (57.78333333, 108.08333333),
                    &[("Yakutsk", (62.03, 129.73))],
                    (72.4087, 126.6847),
                    "from \"Kirensk\"; via \"Yakutsk\"; mouth \"Lena River\" (infobox)",
                ),
                // Its sea is the Caspian, a lake with no outlet.
                OutletBar {
                    ends_in: OutletEnd::Lake,
                    ..outlet(
                        "Volga",
                        (56.32694444, 44.0075),
                        &[("Volgograd", (48.70861111, 44.51472222))],
                        (45.695, 47.8975),
                        "from \"Nizhny Novgorod\"; via \"Volgograd\"; mouth \"Volga\" \
                         (infobox: the Volga Delta on the Caspian)",
                    )
                },
                outlet(
                    "Mackenzie",
                    (61.35472222, -117.66),
                    &[("Fort Simpson", (61.86083333, -121.35277778))],
                    (68.93972222, -136.17277778),
                    "from \"Fort Providence\"; via \"Fort Simpson\"; mouth \"Mackenzie \
                     River\" (infobox)",
                ),
            ],
            // Task 11d's discharge table (informative): one gauge per river, the published
            // long-term mean and its citation, as the river's English Wikipedia article gives
            // it (read 2026-11-26) unless named otherwise. The gauges' points are their
            // articles' coordinates.
            gauges: vec![
                gauge(
                    "Ganges",
                    "the Farakka Barrage",
                    (24.80444444, 87.93305556),
                    16_648.0,
                    "\"Ganges\", infobox: Kumar, Singh & Sharma (2005), Water Resources of India, \
                     Current Science 89(5), 794-811",
                ),
                gauge(
                    "Brahmaputra",
                    "Guwahati",
                    (26.17222222, 91.74583333),
                    18_850.4,
                    "\"Brahmaputra River\", infobox (1971-2000): Tilman, Rivers Network, Ganga \
                     (Ganges)-Brahmaputra",
                ),
                // 711.1 km3/yr at 365.25 days a year.
                gauge(
                    "Yangtze",
                    "Wuhan (Hankou)",
                    (30.595, 114.2975),
                    22_534.0,
                    "\"Yangtze\", infobox (1980-2020, 711.1 km3/yr): Yang et al. (2023), Frontiers \
                     in Marine Science 10, 1201533, doi:10.3389/fmars.2023.1201533",
                ),
                // The article names no gauge: GRDC station 2180800.
                gauge(
                    "Yellow River",
                    "Huayuankou",
                    (34.87694444, 113.67277778),
                    1_438.0,
                    "UNH/GRDC Composite Runoff Fields, station 2180800 Huayuankou (1946-1988), \
                     compositerunoff.sr.unh.edu; point \"Huayuankou, Henan\"",
                ),
                gauge(
                    "Pearl",
                    "Wuzhou",
                    (23.4767, 111.279),
                    7_410.0,
                    "\"Xi River\", infobox: NASA GES DISC (archived 2008-08-28)",
                ),
                gauge(
                    "Mekong",
                    "Kratié",
                    (12.48, 106.03),
                    13_200.0,
                    "\"Mekong\", Table 2 (1960-2004): Mekong River Commission (2005), Overview of \
                     the Hydrology of the Mekong Basin; point \"Kratié (town)\"",
                ),
                gauge(
                    "Nile",
                    "Aswan",
                    (24.08888889, 32.89972222),
                    2_757.0,
                    "\"Nile\", infobox: Senay et al. (2014), Water Resources Research 50(11), \
                     8625-8650, doi:10.1002/2013WR015231, Table 4",
                ),
                gauge(
                    "Congo",
                    "Kinshasa-Brazzaville",
                    (-4.32194444, 15.31194444),
                    40_500.0,
                    "\"Congo River\", infobox (1902-2019): Laraque et al. (2020), Water 12(9), 2613, \
                     doi:10.3390/w12092613",
                ),
                gauge(
                    "Niger",
                    "Lokoja",
                    (7.80194444, 6.74416667),
                    6_743.2,
                    "\"Niger River\", infobox (2000-2025): Niger-HYCOS, Bilan hydrologique du \
                     bassin du Niger annee 2023/2024",
                ),
                // 200,500 ft3/s.
                gauge(
                    "Mississippi",
                    "St. Louis",
                    (38.62722222, -90.19777778),
                    5_677.5,
                    "\"Mississippi River\", infobox (1933-2024): USGS Water-Year Summary for Site \
                     07010000",
                ),
                // 17,400 ft3/s.
                gauge(
                    "Hudson",
                    "Green Island",
                    (42.74416667, -73.69138889),
                    492.7,
                    "\"Hudson River\", infobox: USGS, Hudson River at Green Island NY (01358000); \
                     point \"Green Island, New York\"",
                ),
                gauge(
                    "Yenisei",
                    "Igarka",
                    (67.46666667, 86.58333333),
                    19_499.0,
                    "\"Yenisei\", infobox (1971-2015): Stadnyk et al. (2021), Changing freshwater \
                     contributions to the Arctic, Elementa 9(1), doi:10.1525/elementa.2020.00098",
                ),
                gauge(
                    "Ob",
                    "Salekhard",
                    (66.53333333, 66.6),
                    12_889.0,
                    "\"Ob\", infobox (1971-2015): Stadnyk et al. (2021), \
                     doi:10.1525/elementa.2020.00098",
                ),
                gauge(
                    "Orinoco",
                    "Ciudad Bolívar",
                    (8.13793, -63.548266),
                    32_760.0,
                    "\"Orinoco\", infobox (1926-2011): Cordova & Sanabria, La geografia del agua",
                ),
                gauge(
                    "Paraná",
                    "Corrientes",
                    (-27.48333333, -58.81666667),
                    18_979.0,
                    "\"Paraná River\", infobox (1971-2010): Transboundary Diagnostic Analysis for \
                     the La Plata Basin",
                ),
                gauge(
                    "Lena",
                    "Kyusyur",
                    (70.68333333, 127.36666667),
                    17_773.0,
                    "\"Lena River\", infobox (1971-2015): Stadnyk et al. (2021), \
                     doi:10.1525/elementa.2020.00098",
                ),
                gauge(
                    "Volga",
                    "Volgograd",
                    (48.70861111, 44.51472222),
                    8_228.3,
                    "\"Volga\", infobox: Tilman, Rivers Network, Volga",
                ),
                gauge(
                    "Mackenzie",
                    "Tsiigehtchic (Arctic Red River)",
                    (67.44055556, -133.74527778),
                    9_211.0,
                    "\"Mackenzie River\", infobox (1971-2015): NOAA Arctic Report Card, River \
                     Discharge",
                ),
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
/// [`TABLE_LAKE_KM`] of a table point, or in the map lake holding one: Task 11f, the map bakes
/// the table's lakes at their bed), else the lake map's baked surface; and where it came from.
pub fn expected_level(lake: &LakePoint) -> Option<(f32, &'static str)> {
    let at = dir(lake.lat, lake.lon);
    let map = shipped_lake_map();
    let id = map.lake_at(lake.lat, lake.lon);
    if let Some(t) = shipped_lake_levels().iter().find(|t| {
        km_between(dir(t.lat, t.lon), at) <= TABLE_LAKE_KM
            || id.is_some_and(|id| map.lake_at(t.lat, t.lon) == Some(id))
    }) {
        return Some((t.level_m, "the lake level table"));
    }
    id.map(|id| (map.level(id), "the lake map's fine surface"))
}

/// Judges a lake point: the pass's lake there ([`EraHydrology::lake_tap`]) and every pass lake
/// holding nodes of the point's map lake (`holders`, [`map_lake_holders`]; Task 11f: a tap takes
/// the highest of four nodes' lakes, so it never sees a bed fragment beside the table lake)
/// stand within [`LAKE_TOLERANCE_M`] of the expected level.
pub fn judge_lake(
    hydro: &EraHydrology,
    lake: &LakePoint,
    holders: &[(u16, u16, u32)],
) -> BarResult {
    let got = hydro.lake_tap(RenderDir(dir(lake.lat, lake.lon)));
    let (pass, line) = match (got, expected_level(lake)) {
        (Some(tap), Some((want, from))) => {
            let diff = f64::from(tap.level_m) - f64::from(want);
            let (whole, whole_line) = match shipped_lake_map().lake_at(lake.lat, lake.lon) {
                Some(id) => map_lake_stands_at(want, holders_of(holders, id), &|l| {
                    hydro.lakes[usize::from(l)].level_m
                }),
                None => (true, String::new()),
            };
            (
                diff.abs() <= LAKE_TOLERANCE_M && whole,
                format!(
                    "level {:.1} m, {from} {:.1} m (off {:+.1} m, {LAKE_TOLERANCE_M} m allowed)\
                     {whole_line}",
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

/// Whether every pass lake holding nodes of a map lake (`held`: `(map id, pass lake, nodes)`)
/// stands within [`LAKE_TOLERANCE_M`] of `want` (`level(lake)`), and the line's tail naming
/// them, worst first.
pub fn map_lake_stands_at(
    want: f32,
    held: &[(u16, u16, u32)],
    level: &dyn Fn(u16) -> f32,
) -> (bool, String) {
    if held.is_empty() {
        return (true, String::new());
    }
    let mut by: Vec<(f64, u16, u32)> = held
        .iter()
        .map(|&(_, l, n)| (f64::from(level(l)) - f64::from(want), l, n))
        .collect();
    by.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()).then(a.1.cmp(&b.1)));
    let pass = by.iter().all(|b| b.0.abs() <= LAKE_TOLERANCE_M);
    let named: Vec<String> = by
        .iter()
        .map(|&(d, l, n)| format!("{l} at {:.1} m ({n} nodes)", f64::from(want) + d))
        .collect();
    (
        pass,
        format!("; its map lake's nodes lie in lakes [{}]", named.join(", ")),
    )
}

/// Traces `outlet`'s river ([`trace_outlet`]) once to its end and once per via, and judges it
/// ([`judge_trace`]).
pub fn judge_outlet(hydro: &EraHydrology, outlet: &OutletBar) -> BarResult {
    let from = dir(outlet.from.0, outlet.from.1);
    let trace = trace_outlet(hydro, from, OUTLET_START_KM, None);
    let via_km: Vec<Option<f64>> = outlet
        .vias
        .iter()
        .map(|(_, (lat, lon))| {
            trace_outlet(hydro, from, OUTLET_START_KM, Some(dir(*lat, *lon))).and_then(|t| t.via_km)
        })
        .collect();
    judge_trace(outlet, trace.as_ref(), &via_km)
}

/// The outlet bar on a trace: every via within [`OUTLET_VIA_KM`] (`via_km`, one per via, in
/// order), and an end of the outlet's kind ([`OutletBar::ends_in`]) within its allowance of its
/// mouth.
pub fn judge_trace(
    outlet: &OutletBar,
    trace: Option<&OutletTrace>,
    via_km: &[Option<f64>],
) -> BarResult {
    let (pass, line) = match trace {
        None => (false, format!("no river within {OUTLET_START_KM} km")),
        Some(t) => {
            let sea_km = km_between(t.end_dir, dir(outlet.sea.0, outlet.sea.1));
            let (lat, lon) = latlon(t.end_dir);
            let end_ok = match outlet.ends_in {
                OutletEnd::Ocean => matches!(t.end, Down::Ocean(_)),
                OutletEnd::Lake => matches!(t.end, Down::Lake(..)),
            };
            let mut via_ok = true;
            let mut via_text = String::new();
            for ((name, _), km) in outlet.vias.iter().zip(via_km) {
                match km {
                    Some(km) => {
                        via_ok &= *km <= OUTLET_VIA_KM;
                        let _ = write!(
                            via_text,
                            "passes {km:.1} km from {name} ({OUTLET_VIA_KM} km allowed), "
                        );
                    }
                    None => {
                        via_ok = false;
                        let _ = write!(via_text, "never measured from {name}, ");
                    }
                }
            }
            let end = match t.end {
                Down::Ocean(_) => "the ocean",
                Down::Lake(..) => "a lake",
                Down::Ice(_) => "the ice",
                Down::Sink(_) => "a dry sink",
                Down::River(_) => "a river",
            };
            let wants = match (outlet.ends_in, end_ok) {
                (_, true) => "",
                (OutletEnd::Ocean, false) => " (needs the ocean)",
                (OutletEnd::Lake, false) => " (needs a lake)",
            };
            (
                end_ok && via_ok && sea_km <= outlet.sea_allowance_km(),
                format!(
                    "{} river nodes through {} lakes, {via_text}ends in {end}{wants} at ({lat:.2}, \
                     {lon:.2}), {sea_km:.0} km from its mouth ({} km allowed)",
                    t.nodes,
                    t.lakes,
                    outlet.sea_allowance_km()
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

/// A ratio of game to published discharge outside `[1 / DISCHARGE_FLAG_RATIO,
/// DISCHARGE_FLAG_RATIO]` is flagged in the discharge table.
pub const DISCHARGE_FLAG_RATIO: f64 = 3.0;

/// The game's discharge at a gauge: the largest of the river nodes within [`OUTLET_START_KM`]
/// (m³/s), as an outlet's start.
pub fn game_discharge(hydro: &EraHydrology, at: (f64, f64)) -> Option<f64> {
    let nodes = hydro.rivers.nodes();
    let mut near = Vec::new();
    hydro.rivers.near(
        &hydro.lattice,
        RenderDir(dir(at.0, at.1)),
        OUTLET_START_KM * 1e3,
        &mut near,
    );
    near.iter()
        .map(|&k| f64::from(nodes[k as usize].discharge_m3s))
        .reduce(f64::max)
}

/// The discharge table's row (informative, never a bar): whether it is flagged (no river at the
/// gauge, or a ratio outside [`DISCHARGE_FLAG_RATIO`]), and its line.
pub fn discharge_row(gauge: &DischargeGauge, game_m3s: Option<f64>) -> (bool, String) {
    let head = format!(
        "discharge {} at {} ({:.2}, {:.2})",
        gauge.river, gauge.gauge, gauge.at.0, gauge.at.1
    );
    match game_m3s {
        None => (
            true,
            format!(
                "{head}: no river within {OUTLET_START_KM} km, published {:.0} m3/s - FLAG",
                gauge.mean_m3s
            ),
        ),
        Some(q) => {
            let ratio = q / gauge.mean_m3s;
            let flagged = !(1.0 / DISCHARGE_FLAG_RATIO..=DISCHARGE_FLAG_RATIO).contains(&ratio);
            (
                flagged,
                format!(
                    "{head}: game {q:.0} m3/s, published {:.0} m3/s, ratio {ratio:.2} - {}",
                    gauge.mean_m3s,
                    if flagged { "FLAG" } else { "ok" }
                ),
            )
        }
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

/// One set of polylines on a map SVG: `(lat, lon)` points, a colour and a stroke width (px).
pub struct SvgLayer {
    pub colour: &'static str,
    pub width_px: f64,
    pub lines: Vec<Vec<(f64, f64)>>,
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;")
}

/// An equirectangular map of `bbox` (n, w, s, e) at [`SVG_PX_PER_DEG`]: the layers in order,
/// labelled points, a [`SCALE_BAR_KM`] scale bar and `line` as a caption.
pub fn map_svg(
    title: &str,
    bbox: (f64, f64, f64, f64),
    layers: &[SvgLayer],
    marks: &[(String, (f64, f64))],
    line: &str,
) -> String {
    let (n, w, s, e) = bbox;
    let k = SVG_PX_PER_DEG;
    let (width, height) = ((e - w) * k, (n - s) * k);
    let xy = |lat: f64, lon: f64| ((lon - w) * k, (n - lat) * k);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width:.0}\" height=\"{height:.0}\" \
         viewBox=\"0 0 {width:.0} {height:.0}\">"
    );
    let _ = writeln!(out, "<title>{}</title>", escape(title));
    let _ = writeln!(
        out,
        "<rect width=\"100%\" height=\"100%\" fill=\"#ffffff\"/>"
    );
    for layer in layers {
        for points in &layer.lines {
            let d: Vec<String> = points
                .iter()
                .map(|&(lat, lon)| {
                    let (x, y) = xy(lat, lon);
                    format!("{x:.1},{y:.1}")
                })
                .collect();
            if d.len() >= 2 {
                let _ = writeln!(
                    out,
                    "<polyline fill=\"none\" stroke=\"{}\" stroke-width=\"{}\" points=\"{}\"/>",
                    layer.colour,
                    layer.width_px,
                    d.join(" ")
                );
            }
        }
    }
    for (label, (lat, lon)) in marks {
        let (x, y) = xy(*lat, *lon);
        let _ = writeln!(
            out,
            "<circle cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"4\" fill=\"#000000\"/>\n<text x=\"{:.1}\" \
             y=\"{:.1}\" font-family=\"sans-serif\" font-size=\"12\">{}</text>",
            x + 6.0,
            y - 6.0,
            escape(label)
        );
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
        escape(line)
    );
    out.push_str("</svg>\n");
    out
}

/// Points about [`SVG_STEP_KM`] apart along `line` (the last kept), as `(lat, lon)`.
fn thinned(line: &[DVec3]) -> Vec<(f64, f64)> {
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
    kept.iter().map(|&d| latlon(d)).collect()
}

/// The box's SVG: reference blue, game red, equirectangular, with a scale bar.
pub fn box_svg(name: &str, reference: &Reference, game: &[Vec<DVec3>], line: &str) -> String {
    let reference_lines = reference
        .links
        .iter()
        .map(|link| link.points.iter().map(|p| (p.lat_deg, p.lon_deg)).collect())
        .collect();
    // Thinned: the relaxed lines' 500 m sub-points would make the Amazon's file megabytes.
    let game_lines = game.iter().map(|l| thinned(l)).collect();
    map_svg(
        &format!("hydro check {name}: reference blue, game red"),
        reference.bbox,
        &[
            SvgLayer {
                colour: "#1f6feb",
                width_px: 2.0,
                lines: reference_lines,
            },
            SvgLayer {
                colour: "#d1242f",
                width_px: 1.2,
                lines: game_lines,
            },
        ],
        &[],
        line,
    )
}

/// An outlet's SVG (Task 11d fix round 1): the trace's path through the lattice's node centres
/// (red), today's burnt courses near it (green, clipped to the map) and the bar's cited start,
/// vias and mouth (black), with a scale bar.
pub fn outlet_svg(
    outlet: &OutletBar,
    path: &[DVec3],
    courses: &[Vec<(f64, f64)>],
    line: &str,
) -> String {
    let path: Vec<(f64, f64)> = path.iter().map(|&d| latlon(d)).collect();
    let mut marks = vec![(format!("{} start", outlet.name), outlet.from)];
    marks.extend(outlet.vias.iter().map(|(name, at)| (name.clone(), *at)));
    marks.push((format!("{} mouth", outlet.name), outlet.sea));
    let all = path.iter().chain(marks.iter().map(|(_, p)| p));
    let (mut n, mut w, mut s, mut e) = (f64::MIN, f64::MAX, f64::MAX, f64::MIN);
    for &(lat, lon) in all {
        n = n.max(lat);
        s = s.min(lat);
        w = w.min(lon);
        e = e.max(lon);
    }
    let pad = 0.5;
    let bbox = (n + pad, w - pad, s - pad, e + pad);
    let inside = |&(lat, lon): &(f64, f64)| {
        (bbox.2..=bbox.0).contains(&lat) && (bbox.1..=bbox.3).contains(&lon)
    };
    // Each course cut to its runs inside the map.
    let mut clipped: Vec<Vec<(f64, f64)>> = Vec::new();
    for course in courses {
        let mut run: Vec<(f64, f64)> = Vec::new();
        for p in course {
            if inside(p) {
                run.push(*p);
            } else if !run.is_empty() {
                clipped.push(std::mem::take(&mut run));
            }
        }
        if !run.is_empty() {
            clipped.push(run);
        }
    }
    map_svg(
        &format!(
            "hydro check outlet {}: the trace red, burnt courses green, cited points black",
            outlet.name
        ),
        bbox,
        &[
            SvgLayer {
                colour: "#2da44e",
                width_px: 1.2,
                lines: clipped,
            },
            SvgLayer {
                colour: "#d1242f",
                width_px: 2.0,
                lines: vec![path],
            },
        ],
        &marks,
        line,
    )
}

/// A file-name slug: lower-case ASCII letters and digits, the rest `-`.
pub fn slug(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            'á' | 'à' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            _ => '-',
        })
        .collect()
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
    let holders = map_lake_holders(hydro, shipped_lake_map());
    for lake in &spec.lakes {
        let result = judge_lake(hydro, lake, &holders);
        pass &= result.pass;
        lines.push(result.line);
    }
    let courses: Vec<Vec<(f64, f64)>> = shipped_today_rivers()
        .iter()
        .map(|r| r.points.clone())
        .collect();
    for outlet in &spec.outlets {
        let result = judge_outlet(hydro, outlet);
        pass &= result.pass;
        let from = dir(outlet.from.0, outlet.from.1);
        if let Some(path) = trace_outlet_path(hydro, from, OUTLET_START_KM) {
            let svg = out_dir.join(format!("hydro-check-outlet-{}.svg", slug(&outlet.name)));
            fs::write(&svg, outlet_svg(outlet, &path, &courses, &result.line))
                .map_err(|e| format!("{}: {e}", svg.display()))?;
            svgs.push(svg);
        }
        lines.push(result.line);
    }
    // Informative (Task 11d): the game's discharge at a gauge per river against its published
    // long-term mean.
    let mut flagged = 0;
    for gauge in &spec.gauges {
        let (flag, line) = discharge_row(gauge, game_discharge(hydro, gauge.at));
        flagged += usize::from(flag);
        lines.push(line);
    }
    if !spec.gauges.is_empty() {
        lines.push(format!(
            "discharge table (informative): {flagged} of {} gauges flagged (ratio outside 1/{} to \
             {})",
            spec.gauges.len(),
            DISCHARGE_FLAG_RATIO,
            DISCHARGE_FLAG_RATIO
        ));
    }
    lines.push(surplus_line(hydro));
    let precip_at = |v: u32| {
        let d = hydro.lattice.dir(v);
        let e = field.sample_overview(d).elevation_m as f32;
        f64::from(beyer.climate(d, e, 0.0).precip_mm)
    };
    let boxes: Vec<Reference> = references.iter().map(|(_, r)| r.clone()).collect();
    let arid = judge_arid(&large_lakes(hydro, &boxes, &precip_at), &references);
    pass &= arid.pass;
    lines.push(arid.line);
    // Informative (Task 11b, fix round 1): how far today's burnt courses' beds lie below the
    // era's own terrain.
    let (_, mode) = hydro_lattice_for(&timeline, 0.0, sizes);
    let source = SnapshotSource::new(&field, &tiles, &timeline, 0.0, mode);
    let rivers = shipped_today_rivers();
    let names: Vec<&str> = rivers.iter().map(|r| r.name.as_str()).collect();
    let beds = course_beds(&hydro.lattice, rivers, &source, shipped_lake_levels());
    lines.extend(burn_depth_lines(&hydro.lattice, &beds, &names));
    // Informative (Task 11d fix round 4): whether each course's interior nodes drain to their
    // course successors.
    let steps = course_steps(hydro, rivers, &source);
    lines.extend(course_step_lines(&hydro.lattice, &steps, &names));
    // Informative (Task 11d fix round 5): the lake map's surfaces against their shores.
    let map = shipped_lake_map();
    let shore_height = |lat: f64, lon: f64| field.sample_overview(dir_render(lat, lon)).elevation_m;
    let game_lakes = |id: u16| -> String {
        let found: Vec<String> = hydro
            .lakes
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                let (la, lo) = latlon(hydro.lattice.dir(l.exit).0);
                map.lake_at(la, lo) == Some(id)
            })
            .map(|(k, l)| format!("{k} {:?} {:.1} m", l.kind, l.level_m))
            .collect();
        found.join(", ")
    };
    let standing = table_lake_levels(&holders, &|l| {
        let lake = &hydro.lakes[usize::from(l)];
        (lake.kind, lake.level_m)
    });
    let stands_at = |id: u16| {
        standing
            .binary_search_by_key(&id, |s| s.0)
            .ok()
            .map(|k| standing[k].1)
    };
    lines.extend(lake_map_audit_lines(
        map,
        &shore_height,
        &stands_at,
        &game_lakes,
    ));
    // Every lake, one line each (Task 11d fix round 2): for diffing lakes between runs.
    let tsv = out_dir.join("hydro-check-lakes.tsv");
    fs::write(&tsv, lakes_tsv(hydro)).map_err(|e| format!("{}: {e}", tsv.display()))?;
    lines.push(format!("hydro check {}", verdict(pass)));
    Ok(HydroCheckReport { lines, pass, svgs })
}

/// A lake-map lake whose baked surface stands more than this below the lowest cell of its
/// shoreline ring is listed by the audit (Task 11d fix round 5).
pub const LAKE_MAP_GAP_M: f64 = 20.0;

/// Lakes the audit always reports, flagged or not (Task 11d fix round 5, the controller): name
/// and a point in each.
pub const AUDITED_LAKES: &[(&str, (f64, f64))] = &[
    ("Baikal", (53.5, 108.0)),
    ("Tanganyika", (-6.0, 29.6)),
    ("Malawi", (-12.0, 34.6)),
    ("Superior", (47.7, -87.5)),
    ("Michigan", (44.0, -87.0)),
    ("Huron", (44.8, -82.4)),
    ("Erie", (42.2, -81.2)),
    ("Ontario", (43.7, -77.9)),
    ("Great Slave", (61.7, -114.0)),
    ("Great Bear", (66.0, -121.0)),
    ("Ladoga", (60.8, 31.5)),
    ("Issyk-Kul", (42.45, 77.25)),
];

/// The pass's lakes holding each lake-map lake's lattice nodes (a node's centre in its map
/// cell): `(map id, pass lake, nodes)`, sorted.
pub fn map_lake_holders(hydro: &EraHydrology, map: &LakeMap) -> Vec<(u16, u16, u32)> {
    let lattice = &hydro.lattice;
    let mut held: Vec<(u16, u16)> = Vec::new();
    for v in 0..lattice.len() {
        let Some(l) = hydro.lake_of(v) else {
            continue;
        };
        let (la, lo) = latlon(lattice.dir(v).0);
        if let Some(id) = map.lake_at(la, lo) {
            held.push((id, l));
        }
    }
    held.sort_unstable();
    held.chunk_by(|a, b| a == b)
        .map(|c| (c[0].0, c[0].1, c.len() as u32))
        .collect()
}

/// Map lake `id`'s entries in [`map_lake_holders`]' list.
pub fn holders_of(holders: &[(u16, u16, u32)], id: u16) -> &[(u16, u16, u32)] {
    let from = holders.partition_point(|h| h.0 < id);
    let to = holders.partition_point(|h| h.0 <= id);
    &holders[from..to]
}

/// The level each lake-map lake stands at where table lakes alone hold its nodes (the one
/// holding the most; ties: the lower index), `(map id, level)` sorted by id, from
/// [`map_lake_holders`] and each pass lake's `(kind, level)`. At 0 ka the pass's lakes other
/// than the table's are the map's, kind `Data`: a map lake any of them holds is left out (Task
/// 11f: a bed fragment beside its table lake still stands at the bed).
pub fn table_lake_levels(
    holders: &[(u16, u16, u32)],
    lake: &dyn Fn(u16) -> (LakeKind, f32),
) -> Vec<(u16, f64)> {
    holders
        .chunk_by(|a, b| a.0 == b.0)
        .filter(|by_id| by_id.iter().all(|h| lake(h.1).0 != LakeKind::Data))
        .map(|by_id| {
            let best = by_id
                .iter()
                .max_by(|a, b| a.2.cmp(&b.2).then(b.1.cmp(&a.1)))
                .expect("a chunk is never empty");
            (by_id[0].0, f64::from(lake(best.1).1))
        })
        .collect()
}

fn dir_render(lat: f64, lon: f64) -> RenderDir {
    geo_to_render(latlon_to_geo(lat, lon))
}

/// Informative (Task 11d fix round 5), never a failure: the lake map's lakes whose surface
/// stands more than [`LAKE_MAP_GAP_M`] below their shoreline ring's lowest cell
/// ([`LakeMap::rings`], the heights from `height_at(lat, lon)`), the largest gap first, each
/// with `game(id)` (the pass's lakes whose exit lies in it); then [`AUDITED_LAKES`], flagged or
/// not. The surface is the level `stands_at(id)` gives (a table lake's, where the pass joined
/// the map lake to one: Task 11f), else the baked one.
pub fn lake_map_audit_lines(
    map: &LakeMap,
    height_at: &dyn Fn(f64, f64) -> f64,
    stands_at: &dyn Fn(u16) -> Option<f64>,
    game: &dyn Fn(u16) -> String,
) -> Vec<String> {
    let rings = map.rings();
    // Each lake's (surface, ring min, ring median), where it has a ring.
    let shore: Vec<Option<(f64, f64, f64)>> = rings
        .iter()
        .map(|r| {
            let mut h: Vec<f64> = r.ring.iter().map(|&(la, lo)| height_at(la, lo)).collect();
            h.sort_by(f64::total_cmp);
            let (&min, med) = (h.first()?, h[h.len() / 2]);
            let surface = stands_at(r.id).unwrap_or(f64::from(map.level(r.id)));
            Some((surface, min, med))
        })
        .collect();
    let named = |id: u16| {
        AUDITED_LAKES
            .iter()
            .find(|(_, (la, lo))| map.lake_at(*la, *lo) == Some(id))
            .map(|(n, _)| (*n).to_string())
    };
    let line = |r: &LakeRing, (surface, min, med): (f64, f64, f64)| {
        let name = named(r.id).unwrap_or_else(|| format!("map lake {}", r.id));
        let baked = f64::from(map.level(r.id));
        let stands = if stands_at(r.id).is_some() {
            format!(", stands at the table's {surface:.1} m")
        } else {
            String::new()
        };
        format!(
            "  {name} at ({:.2}, {:.2}), {} cells: baked {baked:.1} m{stands}, ring lowest \
             {min:.1} m (median {med:.1} m), {:.0} m below; the pass's lakes there [{}]",
            r.centre.0,
            r.centre.1,
            r.cells,
            min - surface,
            game(r.id)
        )
    };
    let mut flagged: Vec<(&LakeRing, (f64, f64, f64))> = rings
        .iter()
        .zip(&shore)
        .filter_map(|(r, s)| s.map(|s| (r, s)))
        .filter(|(_, (surface, min, _))| min - surface > LAKE_MAP_GAP_M)
        .collect();
    flagged.sort_by(|a, b| (b.1.1 - b.1.0).total_cmp(&(a.1.1 - a.1.0)));
    let mut lines = vec![format!(
        "lake map audit (informative): {} of {} lakes stand more than {LAKE_MAP_GAP_M} m below \
         their shoreline ring's lowest cell",
        flagged.len(),
        rings.len()
    )];
    lines.extend(flagged.iter().map(|&(r, s)| line(r, s)));
    lines.push("lake map audit, named lakes:".into());
    for (name, (la, lo)) in AUDITED_LAKES {
        match map.lake_at(*la, *lo) {
            Some(id) => match shore[usize::from(id) - 1] {
                Some(s) => lines.push(format!(
                    "{}{}",
                    line(&rings[usize::from(id) - 1], s),
                    if s.1 - s.0 > LAKE_MAP_GAP_M {
                        " - FLAGGED"
                    } else {
                        " - ok"
                    }
                )),
                None => lines.push(format!("  {name}: map lake {id} has no ring")),
            },
            None => lines.push(format!("  {name} ({la}, {lo}): no lake in the map")),
        }
    }
    lines
}

/// Informative (Task 11d fix round 2): each endorheic table lake's surplus, the inflow beyond
/// its evaporation capacity, which the pass reports and does not route.
pub fn surplus_line(hydro: &EraHydrology) -> String {
    let held: Vec<String> = hydro
        .lakes
        .iter()
        .enumerate()
        .filter(|(_, l)| l.surplus_m3s > 0.0)
        .map(|(k, l)| {
            let (lat, lon) = latlon(hydro.lattice.dir(l.exit).0);
            format!(
                "lake {k} ({:?}, {:.1} m, {:.0} km2) at ({lat:.2}, {lon:.2}): surplus {:.0} m3/s of \
                 {:.0} in",
                l.kind, l.level_m, l.area_km2, l.surplus_m3s, l.inflow_m3s
            )
        })
        .collect();
    format!(
        "endorheic lakes' surplus (informative, not routed): {} [{}]",
        held.len(),
        held.join("; ")
    )
}

/// Every lake as a tab-separated line: index, kind, level, area, inflow, evaporation, outflow
/// and its exit's (lat, lon), under a header.
pub fn lakes_tsv(hydro: &EraHydrology) -> String {
    let mut out = String::from(
        "lake\tkind\tlevel_m\tarea_km2\tinflow_m3s\tevaporation_m3s\toutflow_m3s\tlat\tlon\n",
    );
    for (k, l) in hydro.lakes.iter().enumerate() {
        let (lat, lon) = latlon(hydro.lattice.dir(l.exit).0);
        let _ = writeln!(
            out,
            "{k}\t{:?}\t{:.1}\t{:.0}\t{:.1}\t{:.1}\t{:.1}\t{lat:.3}\t{lon:.3}",
            l.kind, l.level_m, l.area_km2, l.inflow_m3s, l.evaporation_m3s, l.outflow_m3s
        );
    }
    out
}

/// The burn's depth below the terrain over `beds` (`e − bed`): its maximum, 99th percentile and
/// worst five nodes with their courses, and the nodes pinned at 0 m below a course's passage
/// through a node below sea level.
pub fn burn_depth_lines(lattice: &CubeLattice, beds: &[CourseBed], names: &[&str]) -> Vec<String> {
    let mut depth: Vec<(f32, &CourseBed)> =
        beds.iter().map(|b| (b.elevation_m - b.bed_m, b)).collect();
    depth.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.node.cmp(&b.1.node)));
    let Some(&(max, _)) = depth.first() else {
        return vec!["today's courses: nothing burnt".into()];
    };
    let p99 = depth[(depth.len() - 1) / 100].0;
    let name = |b: &CourseBed| match names.get(b.course) {
        Some(n) if !n.is_empty() => (*n).to_string(),
        _ => format!("course {}", b.course),
    };
    let mut lines = vec![format!(
        "today's courses (informative): {} burnt nodes, terrain above the bed max {max:.0} m, \
         p99 {p99:.0} m; {} over 50 m",
        beds.len(),
        depth.iter().filter(|d| d.0 > 50.0).count()
    )];
    for &(d, b) in depth.iter().take(5) {
        let (lat, lon) = latlon(lattice.dir(b.node).0);
        lines.push(format!(
            "  {d:.0} m below the terrain: {} at ({lat:.2}, {lon:.2}), node {}, terrain {:.0} m, \
             bed {:.0} m",
            name(b),
            b.node,
            b.elevation_m,
            b.bed_m
        ));
    }
    let pinned: Vec<&CourseBed> = beds.iter().filter(|b| b.pinned_at_sea_level).collect();
    // Every pinned course (Task 11d fix round 4: each must be explained), by name, then course.
    let mut courses: Vec<(String, usize, Vec<&CourseBed>)> = Vec::new();
    for &b in &pinned {
        match courses.iter_mut().find(|c| c.1 == b.course) {
            Some(c) => c.2.push(b),
            None => courses.push((name(b), b.course, vec![b])),
        }
    }
    courses.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    lines.push(format!(
        "  pinned at 0 m after a node below sea level: {} nodes on {} courses{}",
        pinned.len(),
        courses.len(),
        if courses.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                courses
                    .iter()
                    .map(|c| c.0.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    ));
    for (n, _, at) in &courses {
        let (lat, lon) = latlon(lattice.dir(at[0].node).0);
        lines.push(format!(
            "    {n}: {} nodes, the first at ({lat:.2}, {lon:.2}), terrain {:.1} m",
            at.len(),
            at[0].elevation_m
        ));
    }
    lines
}

/// How today's courses' interior nodes drain against their course successors
/// ([`course_steps`]): the count of each kind, and the first nodes of the unexplained kind
/// ([`CourseStep::Other`]) with their courses.
pub fn course_step_lines(
    lattice: &CubeLattice,
    steps: &[(usize, u32, u32, CourseStep)],
    names: &[&str],
) -> Vec<String> {
    let mut counts: Vec<(CourseStep, usize)> = Vec::new();
    for &(_, _, _, step) in steps {
        match counts.iter_mut().find(|c| c.0 == step) {
            Some(c) => c.1 += 1,
            None => counts.push((step, 1)),
        }
    }
    counts.sort();
    let mut lines = vec![format!(
        "today's courses' order (informative): {} interior nodes [{}]",
        steps.len(),
        counts
            .iter()
            .map(|(s, n)| format!("{s:?} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    )];
    for &(k, v, s, _) in steps.iter().filter(|e| e.3 == CourseStep::Other).take(12) {
        let (lat, lon) = latlon(lattice.dir(v).0);
        let (slat, slon) = latlon(lattice.dir(s).0);
        let name = match names.get(k) {
            Some(n) if !n.is_empty() => (*n).to_string(),
            _ => format!("course {k}"),
        };
        lines.push(format!(
            "  leaves its course: {name} at ({lat:.2}, {lon:.2}), node {v}, successor ({slat:.2}, \
             {slon:.2})"
        ));
    }
    lines
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
        assert_eq!(s.lakes.len(), 8);
        // The Caspian reads the table; the next four the map's fine surfaces; the Great Lakes
        // the table again (the_great_lakes_stand_at_their_table_levels).
        let (_, from) = expected_level(&s.lakes[0]).unwrap();
        assert_eq!(from, "the lake level table");
        for lake in &s.lakes[1..5] {
            let (level, from) = expected_level(lake).unwrap_or_else(|| panic!("{lake:?}"));
            assert_eq!(from, "the lake map's fine surface", "{lake:?}");
            assert!(level.is_finite());
        }
        let names: Vec<&str> = s.outlets.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names[..4], ["Danube", "Jhelum", "Indus", "Amazon"]);
    }

    #[test]
    fn a_spec_file_parses() {
        let json = r#"{"recent_n": 64,
            "boxes": [{"name": "tiny", "file": "tiny.json", "tolerance_km": 50,
                       "min_share": 0.5, "every_link": false}],
            "lakes": [{"name": "x", "lat": 1.0, "lon": 2.0}],
            "outlets": [{"name": "r", "from": [1, 2], "vias": [["g", [1.5, 2.5]]], "sea": [0, 0]},
                        {"name": "l", "from": [1, 2], "sea": [0, 0], "ends_in": "lake",
                         "source": "s"}],
            "gauges": [{"river": "r", "gauge": "g", "at": [1, 2], "mean_m3s": 10,
                        "source": "s"}]}"#;
        let spec: CheckSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.recent_n, Some(64));
        assert_eq!(spec.boxes[0].below_m, None);
        assert_eq!(spec.outlets[0].vias, [("g".to_string(), (1.5, 2.5))]);
        assert_eq!(
            (spec.outlets[0].ends_in, spec.outlets[0].source.as_str()),
            (OutletEnd::Ocean, "")
        );
        assert_eq!(spec.outlets[1].ends_in, OutletEnd::Lake);
        assert!(spec.outlets[1].vias.is_empty());
        assert_eq!(spec.gauges[0].mean_m3s, 10.0);
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

    #[test]
    fn burn_depth_lines_report_the_deepest_beds() {
        let lattice = CubeLattice { n: 30 };
        let bed = |node, course, e: f32, b: f32, pinned| CourseBed {
            node,
            course,
            elevation_m: e,
            bed_m: b,
            pinned_at_sea_level: pinned,
        };
        let beds: Vec<CourseBed> = (0..200)
            .map(|k| bed(k, 0, 10.0 + k as f32, 10.0, false))
            .chain([
                bed(500, 1, 900.0, 100.0, false),
                bed(501, 2, 5.0, 0.0, true),
            ])
            .collect();
        let lines = burn_depth_lines(&lattice, &beds, &["a", "Deep", ""]);
        assert!(lines[0].contains("202 burnt nodes"), "{}", lines[0]);
        assert!(lines[0].contains("max 800 m"), "{}", lines[0]);
        assert!(lines[0].contains("p99 198 m"), "{}", lines[0]);
        assert!(lines[0].contains("150 over 50 m"), "{}", lines[0]);
        assert!(
            lines[1].starts_with("  800 m below the terrain: Deep at"),
            "{}",
            lines[1]
        );
        assert_eq!(lines.len(), 8);
        assert!(
            lines[6].contains("1 nodes on 1 courses (course 2)"),
            "{}",
            lines[6]
        );
        assert!(
            lines[7].starts_with("    course 2: 1 nodes, the first at ("),
            "{}",
            lines[7]
        );
        assert_eq!(
            burn_depth_lines(&lattice, &[], &[]),
            ["today's courses: nothing burnt"]
        );
    }

    /// Task 11d fix round 4 (the controller's ruling, as the owner chose for the Orinoco): the
    /// Nile's mouth is where its Damietta branch meets the sea, at Ras El Bar, not the delta's
    /// head, and its allowance stays 150 km.
    #[test]
    fn the_nile_ends_at_the_damietta_mouth() {
        let spec = CheckSpec::standard();
        let nile = spec
            .outlets
            .iter()
            .find(|o| o.name == "Nile")
            .expect("a Nile bar");
        assert_eq!(nile.sea, (31.5125, 31.82555556));
        assert!(nile.source.contains("\"Ras El Bar\""), "{}", nile.source);
        assert_eq!(nile.sea_allowance_km(), 150.0);
    }

    /// Task 11d fix round 5: the lake map audit lists a lake baked far below its shore, the
    /// largest gap first, and reports the named lakes either way.
    #[test]
    fn the_lake_map_audit_lists_lakes_baked_below_their_shores() {
        let map = shipped_lake_map();
        // Every shore at 500 m: lakes baked under 480 m are flagged, Victoria (1133 m) is not.
        let lines = lake_map_audit_lines(map, &|_, _| 500.0, &|_| None, &|id| format!("game {id}"));
        let count: usize = lines[0]
            .split_whitespace()
            .nth(4)
            .and_then(|n| n.parse().ok())
            .expect("a count");
        assert!(count > 100, "{}", lines[0]);
        let baikal = lines
            .iter()
            .find(|l| l.starts_with("  Baikal at") && l.ends_with("FLAGGED"))
            .expect("Baikal is flagged");
        assert!(baikal.contains("ring lowest 500.0 m"), "{baikal}");
        assert!(baikal.contains("[game "), "{baikal}");
        // The largest gap comes first: the lowest baked surface under a level shore.
        let gap = |l: &str| -> f64 {
            let below = l.split(" m below").next().expect("a gap");
            below
                .rsplit(", ")
                .next()
                .expect("a gap")
                .parse()
                .expect("a number")
        };
        let gaps: Vec<f64> = lines[1..=count].iter().map(|l| gap(l)).collect();
        assert!(gaps.windows(2).all(|w| w[0] >= w[1]), "{gaps:?}");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.ends_with(" - ok") || l.ends_with("FLAGGED"))
                .count(),
            AUDITED_LAKES.len()
        );
        // With every shore below the sea's floor, nothing is flagged.
        let none = lake_map_audit_lines(map, &|_, _| -1e4, &|_| None, &|_| String::new());
        assert!(none[0].contains(" 0 of "), "{}", none[0]);
        // Task 11f: a map lake the pass joins to a table lake is judged at the table's level.
        let baikal_id = map.lake_at(53.5, 108.0).expect("Baikal is mapped");
        let baikal = |stands: &dyn Fn(u16) -> Option<f64>| {
            lake_map_audit_lines(map, &|_, _| 400.0, stands, &|_| String::new())
                .into_iter()
                // The named lakes' line, which ends with its verdict.
                .find(|l| {
                    l.starts_with("  Baikal at") && (l.ends_with("FLAGGED") || l.ends_with(" - ok"))
                })
                .expect("Baikal is named")
        };
        assert!(baikal(&|_| None).ends_with("FLAGGED"));
        let joined = baikal(&|id| (id == baikal_id).then_some(455.5));
        assert!(joined.ends_with(" - ok"), "{joined}");
        assert!(
            joined.contains("baked -328.0 m, stands at the table's 455.5 m"),
            "{joined}"
        );
    }

    /// Task 11f: the map bakes Michigan, Huron and Erie at their bed (and their fragments were
    /// separate data lakes there); the lake bar reads each at its table level, at a point far
    /// from its table point in its own map lake, where the fragments lay.
    #[test]
    fn the_great_lakes_stand_at_their_table_levels() {
        let s = CheckSpec::standard();
        let levels = shipped_lake_levels();
        let map = shipped_lake_map();
        for name in ["Lake Michigan", "Lake Huron", "Lake Erie"] {
            let p = s
                .lakes
                .iter()
                .find(|l| l.name.starts_with(name))
                .unwrap_or_else(|| panic!("{name} is in the lake bar"));
            let t = levels
                .iter()
                .find(|t| t.name == name)
                .expect("a table entry");
            assert_eq!(
                expected_level(p),
                Some((t.level_m, "the lake level table")),
                "{p:?}"
            );
            // The map lake decides, not the 25 km rule.
            let at = dir(p.lat, p.lon);
            assert!(
                levels
                    .iter()
                    .all(|t| km_between(dir(t.lat, t.lon), at) > TABLE_LAKE_KM),
                "{p:?}"
            );
            // The hazard: the point's map lake is the table lake's own, baked far below it.
            let id = map.lake_at(p.lat, p.lon).expect("in a map lake");
            assert_eq!(map.lake_at(t.lat, t.lon), Some(id), "{p:?}");
            assert!(
                f64::from(t.level_m - map.level(id)) > LAKE_TOLERANCE_M,
                "{p:?}: baked {}",
                map.level(id)
            );
        }
        // The bar and the audit judge the whole map lake: a bed fragment beside the table lake
        // (Michigan's, at 96.4 m, the pass's lake 376 before Task 11f) fails it.
        let lakes = |l: u16| match l {
            2 => (LakeKind::Open, 176.0),
            _ => (LakeKind::Data, 96.4),
        };
        let level = |l: u16| lakes(l).1;
        let split = [(7, 2, 1100), (7, 376, 23), (9, 376, 1)];
        let (pass, line) = map_lake_stands_at(176.0, holders_of(&split, 7), &level);
        assert!(!pass, "{line}");
        assert!(
            line.starts_with(
                "; its map lake's nodes lie in lakes [376 at 96.4 m (23 nodes), 2 at 176.0 m"
            ),
            "{line}"
        );
        assert_eq!(table_lake_levels(&split, &lakes), []);
        let joined = [(7, 2, 1123), (9, 376, 1)];
        assert!(map_lake_stands_at(176.0, holders_of(&joined, 7), &level).0);
        assert_eq!(table_lake_levels(&joined, &lakes), [(7, 176.0)]);
        assert_eq!(map_lake_stands_at(1.0, &[], &level), (true, String::new()));
    }

    /// Owner, 2026-11-26: the Amazon alone may end 160 km from its mouth; the rest keep
    /// `OUTLET_SEA_KM`.
    #[test]
    fn the_amazon_alone_allows_160_km_from_its_mouth() {
        let spec = CheckSpec::standard();
        for o in &spec.outlets {
            let want = if o.name == "Amazon" {
                160.0
            } else {
                OUTLET_SEA_KM
            };
            assert_eq!(o.sea_allowance_km(), want, "{}", o.name);
        }
        assert_eq!(OUTLET_SEA_KM, 150.0);
    }

    /// The great-circle distance from `p` to the minor arc `a`-`b` (km).
    fn km_to_arc(p: DVec3, a: DVec3, b: DVec3) -> f64 {
        let (p, a, b) = (p.normalize(), a.normalize(), b.normalize());
        let ends = km_between(p, a).min(km_between(p, b));
        let n = a.cross(b);
        if n.length() < 1e-15 {
            return ends;
        }
        let n = n.normalize();
        let s = p.dot(n);
        let foot = p - s * n;
        if a.cross(foot).dot(n) >= 0.0 && foot.cross(b).dot(n) >= 0.0 {
            s.abs().min(1.0).asin() * EARTH_RADIUS_M * 1e-3
        } else {
            ends
        }
    }

    /// The nearest burnt course to a point: its name and distance (km).
    fn nearest_course(lat: f64, lon: f64) -> (f64, String) {
        let p = dir(lat, lon);
        let mut best = (f64::INFINITY, String::new());
        for r in shipped_today_rivers() {
            for w in r.points.windows(2) {
                let km = km_to_arc(p, dir(w[0].0, w[0].1), dir(w[1].0, w[1].1));
                if km < best.0 {
                    best = (km, r.name.clone());
                }
            }
        }
        best
    }

    /// Task 11d: the world's major rivers have outlet bars and gauges with cited sources, and
    /// every start, via and gauge lies on land (outside the lake map's lakes) within its
    /// tolerance of a burnt course; a river ending in a lake has a lake within its allowance of
    /// its mouth.
    #[test]
    fn the_major_rivers_have_cited_outlets() {
        let spec = CheckSpec::standard();
        let major = [
            "Ganges",
            "Brahmaputra",
            "Yangtze",
            "Yellow River",
            "Pearl",
            "Mekong",
            "Nile",
            "Congo",
            "Niger",
            "Mississippi",
            "Hudson",
            "Yenisei",
            "Ob",
            "Orinoco",
            "Paraná",
            "Lena",
            "Volga",
            "Mackenzie",
        ];
        let named = |n: &str| spec.outlets.iter().find(|o| o.name == n);
        for river in major {
            let o = named(river).unwrap_or_else(|| panic!("no outlet bar for {river}"));
            assert!(
                spec.gauges.iter().filter(|g| g.river == river).count() == 1,
                "{river}: one gauge"
            );
            assert_eq!(o.sea_allowance_km(), OUTLET_SEA_KM, "{river}");
        }
        let vias = |o: &str| named(o).unwrap().vias.len();
        assert_eq!(
            [
                vias("Ganges"),
                vias("Brahmaputra"),
                vias("Nile"),
                vias("Niger")
            ],
            [2, 2, 2, 3]
        );
        let map = shipped_lake_map();
        let mut problems = Vec::new();
        let mut check = |what: String, (lat, lon): (f64, f64), tolerance_km: f64| {
            let (km, course) = nearest_course(lat, lon);
            if km > tolerance_km {
                problems.push(format!(
                    "{what} ({lat}, {lon}): {km:.1} km from {course:?}, over {tolerance_km} km"
                ));
            }
            if map.lake_at(lat, lon).is_some() {
                problems.push(format!("{what} ({lat}, {lon}): in a lake"));
            }
        };
        for o in &spec.outlets {
            assert!(!o.source.is_empty(), "{}: no source", o.name);
            check(format!("{} start", o.name), o.from, OUTLET_START_KM);
            for (name, at) in &o.vias {
                check(format!("{} via {name}", o.name), *at, OUTLET_VIA_KM);
            }
            if o.ends_in == OutletEnd::Lake {
                // Its mouth is where its burnt course ends, and its lake is a lake of the level
                // table (the Caspian), which the pass fills to its table level, named in its
                // source.
                let (km, course) = nearest_course(o.sea.0, o.sea.1);
                assert!(
                    km <= o.sea_allowance_km(),
                    "{}: mouth {km:.1} km from {course:?}",
                    o.name
                );
                let lake = shipped_lake_levels().iter().find(|t| {
                    let first = t.name.split(' ').next().unwrap_or(&t.name);
                    o.source.contains(first)
                });
                assert!(lake.is_some(), "{}: names no level-table lake", o.name);
            }
        }
        for g in &spec.gauges {
            assert!(!g.source.is_empty() && g.mean_m3s > 0.0, "{g:?}");
            assert!(named(&g.river).is_some(), "{}: no outlet bar", g.river);
            check(
                format!("{} gauge {}", g.river, g.gauge),
                g.at,
                OUTLET_START_KM,
            );
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    /// Task 11d fix round 1: the misroute pictures for the owner.
    #[test]
    fn an_outlet_svg_draws_the_trace_the_courses_and_the_cited_points() {
        let bar = OutletBar {
            name: "R".into(),
            from: (1.0, 1.0),
            vias: vec![("V & W".into(), (0.5, 1.5))],
            sea: (0.0, 2.0),
            sea_km: None,
            ends_in: OutletEnd::Ocean,
            source: "s".into(),
        };
        let path = vec![dir(1.0, 1.0), dir(0.5, 1.5), dir(0.0, 2.0)];
        // A course leaving the map and coming back: two runs.
        let courses = vec![vec![
            (1.0, 1.0),
            (0.5, 1.5),
            (10.0, 10.0),
            (0.2, 1.8),
            (0.1, 1.9),
        ]];
        let svg = outlet_svg(&bar, &path, &courses, "a < b");
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
        // The points and half a degree around them: 2 x 2 degrees at 60 px.
        assert!(svg.contains("width=\"120\" height=\"120\""), "{svg}");
        assert_eq!(svg.matches("stroke=\"#2da44e\"").count(), 2, "{svg}");
        assert_eq!(svg.matches("stroke=\"#d1242f\"").count(), 1, "{svg}");
        assert!(
            svg.contains("points=\"30.0,30.0 60.0,60.0 90.0,90.0\""),
            "{svg}"
        );
        assert_eq!(svg.matches("<circle").count(), 3);
        for label in ["R start", "V &amp; W", "R mouth", "100 km", "a &lt; b"] {
            assert!(svg.contains(label), "{label}: {svg}");
        }
        assert_eq!(slug("Paraná River"), "parana-river");
    }

    #[test]
    fn the_discharge_table_flags_ratios_outside_three() {
        let g = DischargeGauge {
            river: "R".into(),
            gauge: "G".into(),
            at: (1.0, 2.0),
            mean_m3s: 900.0,
            source: "s".into(),
        };
        let flag = |q: Option<f64>| discharge_row(&g, q).0;
        // 1/3 and 3 themselves are inside; just past them is not.
        assert!(!flag(Some(300.0)) && !flag(Some(900.0)) && !flag(Some(2700.0)));
        assert!(flag(Some(299.0)) && flag(Some(2701.0)) && flag(Some(0.0)));
        assert!(flag(None));
        let (_, line) = discharge_row(&g, Some(1800.0));
        assert_eq!(
            line,
            "discharge R at G (1.00, 2.00): game 1800 m3/s, published 900 m3/s, ratio 2.00 - ok"
        );
        let (_, line) = discharge_row(&g, Some(90.0));
        assert!(line.ends_with("ratio 0.10 - FLAG"), "{line}");
        let (_, line) = discharge_row(&g, None);
        assert!(
            line.contains("no river within 30 km") && line.ends_with("FLAG"),
            "{line}"
        );
    }

    /// Task 11d: the Volga's sea is the Caspian, a lake. A bar names what its river must end in;
    /// each of several vias is judged.
    #[test]
    fn an_outlet_ends_in_the_water_its_bar_names() {
        let lattice = CubeLattice { n: 30 };
        let mouth = (45.7, 47.9);
        let end = lattice.nearest(RenderDir(dir(mouth.0, mouth.1)));
        let trace = |down| OutletTrace {
            start: 0,
            nodes: 9,
            lakes: 0,
            via_km: None,
            end: down,
            end_dir: lattice.dir(end).0,
        };
        let bar = |ends_in| OutletBar {
            name: "V".into(),
            from: (56.0, 44.0),
            vias: vec![("A".into(), (50.0, 45.0)), ("B".into(), (48.7, 44.5))],
            sea: mouth,
            sea_km: None,
            ends_in,
            source: "s".into(),
        };
        let ok = [Some(1.0), Some(OUTLET_VIA_KM)];
        let lake = trace(Down::Lake(0, end));
        let ocean = trace(Down::Ocean(end));
        assert!(judge_trace(&bar(OutletEnd::Lake), Some(&lake), &ok).pass);
        assert!(judge_trace(&bar(OutletEnd::Ocean), Some(&ocean), &ok).pass);
        let r = judge_trace(&bar(OutletEnd::Ocean), Some(&lake), &ok);
        assert!(
            !r.pass && r.line.contains("ends in a lake (needs the ocean)"),
            "{}",
            r.line
        );
        let r = judge_trace(&bar(OutletEnd::Lake), Some(&ocean), &ok);
        assert!(!r.pass && r.line.contains("(needs a lake)"), "{}", r.line);
        assert!(!judge_trace(&bar(OutletEnd::Lake), Some(&trace(Down::Sink(end))), &ok).pass);
        // Every via is judged: the second one off by a metre fails.
        let r = judge_trace(
            &bar(OutletEnd::Lake),
            Some(&lake),
            &[Some(1.0), Some(15.001)],
        );
        assert!(!r.pass, "{}", r.line);
        assert!(
            r.line
                .contains("passes 1.0 km from A (15 km allowed), passes 15.0 km from B"),
            "{}",
            r.line
        );
        assert!(!judge_trace(&bar(OutletEnd::Lake), Some(&lake), &[Some(1.0), None]).pass);
        // Far from its mouth.
        let far = OutletTrace {
            end_dir: dir(40.0, 50.0),
            ..lake
        };
        assert!(!judge_trace(&bar(OutletEnd::Lake), Some(&far), &ok).pass);
        assert!(!judge_trace(&bar(OutletEnd::Lake), None, &ok).pass);
    }
}
