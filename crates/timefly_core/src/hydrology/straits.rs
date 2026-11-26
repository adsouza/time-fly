//! Seaways narrower than the lattice (rivers Task 8b, round 2): the Bosporus, without which the
//! Black Sea reads as an inland basin. Compiled in from `data/hydrology/straits.json`, which
//! Task 11's Python also reads.
//!
//! Where the terrain is today's ([`NodeSource::data_lakes`](super::NodeSource)), the pass lowers
//! each strait's nodes ([`strait_nodes`]) to `min(e, sill_m + shift)`, `shift` being how far the
//! era's node stands from the present-day overview there
//! ([`NodeSource::present_shift_m`](super::NodeSource)). An era whose sea stood below the sill
//! (the last glacial maximum's −120 m against the Bosporus's −35 m) keeps the strait dry.

use std::sync::OnceLock;

use bevy_math::DVec3;
use serde_json::Value;

use super::CubeLattice;
use crate::geo::{RenderDir, geo_to_render, latlon_to_geo};

/// The shipped table's JSON.
pub const STRAITS_JSON: &str = include_str!("../../../../data/hydrology/straits.json");

/// One strait: its centreline (lat, lon degrees, from one sea to the other) and its sill depth
/// (m, present-day sea level 0).
#[derive(Clone, Debug, PartialEq)]
pub struct Strait {
    pub name: String,
    pub points: Vec<(f64, f64)>,
    pub sill_m: f32,
}

/// Parses `{ "straits": [{ "name", "points": [[lat, lon], ...], "sill_m", "source", "note" }] }`.
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
            let sill_m = s["sill_m"]
                .as_f64()
                .filter(|x| x.is_finite())
                .ok_or(format!("strait {k}: no finite \"sill_m\""))?
                as f32;
            let points = s["points"]
                .as_array()
                .ok_or(format!("strait {k}: no \"points\""))?
                .iter()
                .map(|p| {
                    let (lat, lon) = (p[0].as_f64(), p[1].as_f64());
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
            if points.len() < 2 || s["source"].as_str().is_none_or(str::is_empty) {
                return Err(format!("strait {k}: needs two points and a source"));
            }
            Ok(Strait {
                name,
                points,
                sill_m,
            })
        })
        .collect()
}

/// The shipped table, parsed once. Its tests guarantee it parses.
pub fn shipped_straits() -> &'static [Strait] {
    static STRAITS: OnceLock<Vec<Strait>> = OnceLock::new();
    STRAITS.get_or_init(|| parse_straits(STRAITS_JSON).expect("the shipped straits parse"))
}

/// The lattice nodes along a strait's centreline, in order, without repeats: the nearest node
/// to points a quarter of the mean spacing apart, so consecutive nodes are neighbours.
pub fn strait_nodes(lattice: &CubeLattice, strait: &Strait) -> Vec<u32> {
    let dirs: Vec<DVec3> = strait
        .points
        .iter()
        .map(|&(lat, lon)| geo_to_render(latlon_to_geo(lat, lon)).0.normalize())
        .collect();
    let step = 0.25 * lattice.mean_spacing_m() / crate::worldgen::frame::EARTH_RADIUS_M;
    let mut out: Vec<u32> = Vec::new();
    for w in dirs.windows(2) {
        let n = (w[0].angle_between(w[1]) / step).ceil().max(1.0) as u32;
        for k in 0..=n {
            let t = f64::from(k) / f64::from(n);
            let d = RenderDir(w[0].lerp(w[1], t).normalize());
            let v = lattice.nearest(d);
            if out.last() != Some(&v) && !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrology::HydroGraph;

    #[test]
    fn the_shipped_straits_parse_and_run_node_to_node() {
        let straits = shipped_straits();
        assert_eq!(straits.len(), 1);
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
    fn bad_strait_tables_are_errors() {
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
