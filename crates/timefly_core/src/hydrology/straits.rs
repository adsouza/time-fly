//! Seaways and river gorges narrower than the lattice (rivers Task 8b, round 2, and Task 11):
//! the Bosporus, without which the Black Sea reads as an inland basin, and gorges a river leaves
//! its basin by (the Danube's Iron Gate, without which the Pannonian basin spills north through
//! the Moravian Gate). Compiled in from `data/hydrology/straits.json`, which Task 11's Python
//! reference (`timefly_bake.hydro_ref`) also reads.
//!
//! Where the terrain is today's ([`NodeSource::data_lakes`](super::NodeSource)), the pass lowers
//! each entry's nodes ([`strait_levels`]) to `min(e, level + shift)`, `shift` being how far the
//! era's node stands from the present-day overview there
//! ([`NodeSource::present_shift_m`](super::NodeSource)). A strait's level is its sill everywhere
//! (an era whose sea stood below the sill, the last glacial maximum's −120 m against the
//! Bosporus's −35 m, keeps the strait dry); a gorge's is its river's surface, given at each point
//! of its polyline and interpolated between them, so the burnt channel falls as the river does
//! and never cuts below it.

use std::sync::OnceLock;

use bevy_math::DVec3;
use serde_json::Value;

use super::CubeLattice;
use crate::geo::{RenderDir, geo_to_render, latlon_to_geo};

/// The shipped table's JSON.
pub const STRAITS_JSON: &str = include_str!("../../../../data/hydrology/straits.json");

/// One entry: its centreline (lat, lon degrees, from one side to the other: a gorge from
/// upstream) and its level (m, present-day sea level 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Strait {
    pub name: String,
    pub points: Vec<(f64, f64)>,
    /// A strait's sill; a gorge's lowest level.
    pub sill_m: f32,
    /// A gorge's level at each point; empty for a strait.
    pub levels: Vec<f32>,
}

/// Parses `{ "straits": [{ "name", "kind"?, "points", "sill_m"?, "source", "note"? }] }`. `kind`
/// is `"strait"` (the default: `points` are `[lat, lon]` and `sill_m` is required) or `"gorge"`
/// (`points` are `[lat, lon, level_m]`, no `sill_m`).
pub fn parse_straits(json: &str) -> Result<Vec<Strait>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let list = v["straits"].as_array().ok_or("no \"straits\" array")?;
    list.iter()
        .enumerate()
        .map(|(k, s)| {
            let name = s["name"]
                .as_str()
                .ok_or(format!("strait {k}: no \"name\""))?
                .to_owned();
            let gorge = match s["kind"].as_str() {
                None | Some("strait") => false,
                Some("gorge") => true,
                Some(other) => return Err(format!("strait {k}: unknown kind {other:?}")),
            };
            let mut levels = Vec::new();
            let points = s["points"]
                .as_array()
                .ok_or(format!("strait {k}: no \"points\""))?
                .iter()
                .map(|p| {
                    let (lat, lon) = (p[0].as_f64(), p[1].as_f64());
                    let len = p.as_array().map_or(0, Vec::len);
                    if len != if gorge { 3 } else { 2 } {
                        return Err(format!(
                            "strait {k}: point {p} needs {}",
                            if gorge {
                                "[lat, lon, level_m]"
                            } else {
                                "[lat, lon]"
                            }
                        ));
                    }
                    if gorge {
                        let level = p[2]
                            .as_f64()
                            .filter(|x| x.is_finite())
                            .ok_or(format!("strait {k}: bad level in {p}"))?;
                        levels.push(level as f32);
                    }
                    match (lat, lon) {
                        (Some(lat), Some(lon))
                            if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) =>
                        {
                            Ok((lat, lon))
                        }
                        _ => Err(format!("strait {k}: bad point {p}")),
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            let sill_m = if gorge {
                levels.iter().copied().fold(f32::INFINITY, f32::min)
            } else {
                s["sill_m"]
                    .as_f64()
                    .filter(|x| x.is_finite())
                    .ok_or(format!("strait {k}: no finite \"sill_m\""))? as f32
            };
            if points.len() < 2 || s["source"].as_str().is_none_or(str::is_empty) {
                return Err(format!("strait {k}: needs two points and a source"));
            }
            Ok(Strait {
                name,
                points,
                sill_m,
                levels,
            })
        })
        .collect()
}

/// The shipped table, parsed once. Its tests guarantee it parses.
pub fn shipped_straits() -> &'static [Strait] {
    static STRAITS: OnceLock<Vec<Strait>> = OnceLock::new();
    STRAITS.get_or_init(|| parse_straits(STRAITS_JSON).expect("the shipped straits parse"))
}

/// The lattice nodes along an entry's centreline, in order, without repeats, each with its
/// level: the nearest node to points a quarter of the mean spacing apart, so consecutive nodes
/// are neighbours. A node takes the lowest level of the centreline's points nearest to it.
pub fn strait_levels(lattice: &CubeLattice, strait: &Strait) -> Vec<(u32, f32)> {
    let dirs: Vec<DVec3> = strait
        .points
        .iter()
        .map(|&(lat, lon)| geo_to_render(latlon_to_geo(lat, lon)).0.normalize())
        .collect();
    let step = 0.25 * lattice.mean_spacing_m() / crate::worldgen::frame::EARTH_RADIUS_M;
    let mut out: Vec<(u32, f32)> = Vec::new();
    for (w, pair) in dirs.windows(2).enumerate() {
        let n = (pair[0].angle_between(pair[1]) / step).ceil().max(1.0) as u32;
        for k in 0..=n {
            let t = f64::from(k) / f64::from(n);
            let d = RenderDir(pair[0].lerp(pair[1], t).normalize());
            let v = lattice.nearest(d);
            let level = match strait.levels.get(w..w + 2) {
                Some(&[a, b]) => (f64::from(a) + t * (f64::from(b) - f64::from(a))) as f32,
                _ => strait.sill_m,
            };
            match out.iter_mut().find(|o| o.0 == v) {
                Some(o) => o.1 = o.1.min(level),
                None => out.push((v, level)),
            }
        }
    }
    out
}

/// The nodes of [`strait_levels`] alone.
pub fn strait_nodes(lattice: &CubeLattice, strait: &Strait) -> Vec<u32> {
    strait_levels(lattice, strait)
        .into_iter()
        .map(|(v, _)| v)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrology::HydroGraph;

    #[test]
    fn the_shipped_straits_parse_and_run_node_to_node() {
        let straits = shipped_straits();
        assert_eq!(straits.len(), 4);
        let bosporus = &straits[0];
        assert_eq!(
            (bosporus.name.as_str(), bosporus.sill_m),
            ("Bosporus", -35.0)
        );
        let lattice = CubeLattice { n: 900 };
        let nodes = strait_nodes(&lattice, bosporus);
        assert!(nodes.len() >= 4, "{nodes:?}");
        for w in nodes.windows(2) {
            assert!(lattice.neighbours(w[0]).contains(&Some(w[1])), "{w:?}");
        }
    }

    #[test]
    fn the_iron_gate_runs_from_the_pannonian_plain_to_below_its_dam() {
        let gate = shipped_straits()
            .iter()
            .find(|s| s.name == "Iron Gate")
            .expect("the Iron Gate entry");
        assert!(!gate.levels.is_empty(), "a gorge carries its levels");
        let lattice = CubeLattice { n: 900 };
        let nodes = strait_levels(&lattice, gate);
        assert!(nodes.len() >= 10, "{nodes:?}");
        for w in nodes.windows(2) {
            assert!(lattice.neighbours(w[0].0).contains(&Some(w[1].0)), "{w:?}");
            // The river's surface never rises downstream.
            assert!(w[1].1 <= w[0].1, "{w:?}");
        }
        // The reservoir above the dam, the river below it.
        assert_eq!(nodes[0].1, 66.0);
        assert_eq!(nodes[nodes.len() - 1].1, 41.0);
        // It starts on the Pannonian side (Golubac) and ends below the dam (Drobeta).
        let (lat0, lon0) =
            crate::geo::geo_to_latlon(crate::geo::render_to_geo(lattice.dir(nodes[0].0)));
        let last = nodes[nodes.len() - 1].0;
        let (lat1, lon1) = crate::geo::geo_to_latlon(crate::geo::render_to_geo(lattice.dir(last)));
        assert!(
            lon0 < 21.8 && lon1 > 22.6,
            "({lat0}, {lon0}) to ({lat1}, {lon1})"
        );
    }

    #[test]
    fn the_danube_bend_runs_from_esztergom_to_budapest() {
        let bend = shipped_straits()
            .iter()
            .find(|s| s.name == "Danube Bend")
            .expect("the Danube Bend entry");
        let lattice = CubeLattice { n: 900 };
        let nodes = strait_levels(&lattice, bend);
        assert!(nodes.len() >= 4, "{nodes:?}");
        for w in nodes.windows(2) {
            assert!(lattice.neighbours(w[0].0).contains(&Some(w[1].0)), "{w:?}");
            assert!(w[1].1 <= w[0].1, "{w:?}");
        }
        let latlon = |v: u32| crate::geo::geo_to_latlon(crate::geo::render_to_geo(lattice.dir(v)));
        let (start, end) = (latlon(nodes[0].0), latlon(nodes[nodes.len() - 1].0));
        // West of the Bend at Esztergom, south of it at Budapest.
        assert!(start.1 < 18.75 && end.0 < 47.55, "{start:?} to {end:?}");
        assert!((101.0..=105.0).contains(&nodes[nodes.len() - 1].1));
    }

    #[test]
    fn the_upper_indus_runs_from_tibet_into_ladakh() {
        let indus = shipped_straits()
            .iter()
            .find(|s| s.name == "Upper Indus")
            .expect("the Upper Indus entry");
        let lattice = CubeLattice { n: 900 };
        let nodes = strait_levels(&lattice, indus);
        assert!(nodes.len() >= 30, "{}", nodes.len());
        for w in nodes.windows(2) {
            assert!(lattice.neighbours(w[0].0).contains(&Some(w[1].0)), "{w:?}");
            assert!(w[1].1 <= w[0].1, "{w:?}");
        }
        let latlon = |v: u32| crate::geo::geo_to_latlon(crate::geo::render_to_geo(lattice.dir(v)));
        let (start, end) = (latlon(nodes[0].0), latlon(nodes[nodes.len() - 1].0));
        // From Tibet (east of Demchok, 79.45 E) to below Leh (77.58 E), falling north-west.
        assert!(
            start.1 > 80.0 && end.1 < 77.5 && end.0 > start.0,
            "{start:?} {end:?}"
        );
        assert!(nodes[0].1 > 4000.0 && nodes[nodes.len() - 1].1 < 3300.0);
    }

    #[test]
    fn a_gorge_takes_its_levels_along_the_polyline() {
        let json = r#"{"straits": [{"name": "g", "kind": "gorge", "source": "s",
            "points": [[10.0, 20.0, 100], [10.0, 21.0, 50], [10.0, 22.0, 0]]}]}"#;
        let gorge = &parse_straits(json).unwrap()[0];
        assert_eq!(gorge.levels, vec![100.0, 50.0, 0.0]);
        assert_eq!(gorge.sill_m, 0.0);
        let lattice = CubeLattice { n: 900 };
        let nodes = strait_levels(&lattice, gorge);
        // Each node takes the lowest level near it: the first a little below the head's.
        let first = nodes.first().unwrap().1;
        assert!((97.0..100.0).contains(&first), "{first}");
        assert_eq!(nodes.last().unwrap().1, 0.0);
        // Interpolated along the way: about halfway is about 50 m.
        let mid = nodes[nodes.len() / 2].1;
        assert!((mid - 50.0).abs() < 5.0, "{mid}");
        assert!(nodes.windows(2).all(|w| w[1].1 <= w[0].1));
        // A strait's nodes all stand at its sill.
        let strait = &shipped_straits()[0];
        assert!(
            strait_levels(&lattice, strait)
                .iter()
                .all(|&(_, l)| l == strait.sill_m)
        );
        assert_eq!(
            strait_levels(&lattice, strait)
                .iter()
                .map(|&(v, _)| v)
                .collect::<Vec<_>>(),
            strait_nodes(&lattice, strait)
        );
    }

    #[test]
    fn bad_strait_tables_are_errors() {
        // A gorge needs a level at every point; a strait has none and needs its sill.
        let gorge = r#"{"straits": [{"name": "x", "kind": "gorge", "source": "s",
            "points": [[1, 2, 10], [1.5, 2]]}]}"#;
        assert!(parse_straits(gorge).is_err());
        let strait = r#"{"straits": [{"name": "x", "source": "s", "sill_m": -3,
            "points": [[1, 2, 10], [1.5, 2, 5]]}]}"#;
        assert!(parse_straits(strait).is_err());
        let kind = r#"{"straits": [{"name": "x", "kind": "canal", "source": "s", "sill_m": -3,
            "points": [[1, 2], [1.5, 2]]}]}"#;
        assert!(parse_straits(kind).is_err());
        assert!(parse_straits("{}").is_err());
        let one =
            r#"{"straits": [{"name": "x", "points": [[1, 2]], "sill_m": -3, "source": "s"}]}"#;
        assert!(parse_straits(one).is_err());
        let far = r#"{"straits": [{"name": "x", "points": [[1, 2], [95, 2]], "sill_m": -3, "source": "s"}]}"#;
        assert!(parse_straits(far).is_err());
        let ok = r#"{"straits": [{"name": "x", "points": [[1, 2], [1.5, 2]], "sill_m": -3, "source": "s"}]}"#;
        assert_eq!(parse_straits(ok).unwrap()[0].points.len(), 2);
    }
}
