//! The hand-made lake level table (owner decision, 2026-11-25): present-day surface levels of
//! big deep lakes whose bed, not surface, ETOPO 2022 holds (the Caspian, Baikal and the Great
//! Lakes). Compiled in from `data/hydrology/lake_levels.json`, which Task 11's Python also reads.

use std::sync::OnceLock;

use serde_json::Value;

/// The shipped table's JSON.
pub const LAKE_LEVELS_JSON: &str = include_str!("../../../../data/hydrology/lake_levels.json");

/// One lake: a point inside it and its surface level.
#[derive(Clone, Debug, PartialEq)]
pub struct LakeLevel {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub level_m: f32,
    /// The overview's value at the point, where the table records it (always below the level).
    pub overview_m: Option<f32>,
    pub source: String,
    pub note: String,
    /// A lake with no outlet (the Caspian; Task 11d fix round 2): terminal whatever its balance,
    /// its nodes below its level seeding routing like the sea at that level. Set only with a
    /// cited `endorheic_source`.
    pub endorheic: bool,
}

/// Parses a lake level table: `{ "lakes": [{ "name", "lat", "lon", "level_m", "source",
/// "note", "overview_m"?, "endorheic"?, "endorheic_source"? }] }`; `endorheic: true` needs an
/// `endorheic_source`.
pub fn parse_lake_levels(json: &str) -> Result<Vec<LakeLevel>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let lakes = v["lakes"].as_array().ok_or("no \"lakes\" array")?;
    lakes
        .iter()
        .enumerate()
        .map(|(k, l)| {
            let text = |key: &str| {
                l[key]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(format!("lake {k}: no \"{key}\""))
            };
            let number = |key: &str| {
                l[key]
                    .as_f64()
                    .filter(|x| x.is_finite())
                    .ok_or(format!("lake {k}: no finite \"{key}\""))
            };
            let (lat, lon) = (number("lat")?, number("lon")?);
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err(format!("lake {k}: lat {lat}, lon {lon} out of range"));
            }
            let endorheic = l["endorheic"].as_bool().unwrap_or(false);
            if endorheic && text("endorheic_source").is_err() {
                return Err(format!(
                    "lake {k}: endorheic without an \"endorheic_source\""
                ));
            }
            Ok(LakeLevel {
                name: text("name")?,
                lat,
                lon,
                level_m: number("level_m")? as f32,
                overview_m: l["overview_m"].as_f64().map(|x| x as f32),
                source: text("source")?,
                note: text("note")?,
                endorheic,
            })
        })
        .collect()
}

/// The shipped table, parsed once. Its tests guarantee it parses.
pub fn shipped_lake_levels() -> &'static [LakeLevel] {
    static LEVELS: OnceLock<Vec<LakeLevel>> = OnceLock::new();
    LEVELS
        .get_or_init(|| parse_lake_levels(LAKE_LEVELS_JSON).expect("the shipped lake levels parse"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_lake_levels_parse_and_lie_above_the_overview() {
        let levels = shipped_lake_levels();
        let named: Vec<(&str, f32)> = levels
            .iter()
            .map(|l| (l.name.as_str(), l.level_m))
            .collect();
        assert_eq!(
            named,
            [
                ("Caspian Sea", -28.0),
                ("Lake Baikal", 455.5),
                ("Lake Superior", 183.4),
                ("Lake Michigan", 176.0),
                ("Lake Huron", 176.0),
                ("Lake Erie", 173.0),
                ("Lake Ontario", 74.0),
            ]
        );
        for l in levels {
            assert!(!l.source.is_empty(), "{l:?}");
            // The reason for the table: the data holds the bed, below the surface.
            let overview = l
                .overview_m
                .expect("each entry records the overview's value");
            assert!(l.level_m > overview, "{l:?}");
        }
        assert!(levels[0].note.contains("-29.4"));
        // Task 11d fix round 2: the Caspian alone is endorheic (the Dead Sea and the Great Salt
        // Lake are the lake map's, not the table's).
        let flagged: Vec<&str> = levels
            .iter()
            .filter(|l| l.endorheic)
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(flagged, ["Caspian Sea"]);
        // Michigan and Huron are one water body at one level.
        assert_eq!(levels[3].level_m, levels[4].level_m);
        // Task 11f: Baikal and the Great Lakes cite the page read and its own source.
        for l in &levels[1..] {
            assert!(l.source.contains("(read 2026-11-26)"), "{l:?}");
            assert!(
                l.source.contains("citing") || l.source.contains("uncited there"),
                "{l:?}"
            );
        }
    }

    #[test]
    fn bad_tables_are_errors() {
        assert!(parse_lake_levels("{").is_err());
        assert!(parse_lake_levels("{}").is_err());
        let missing =
            r#"{"lakes": [{"name": "x", "lat": 1.0, "lon": 2.0, "source": "", "note": ""}]}"#;
        assert!(parse_lake_levels(missing).unwrap_err().contains("level_m"));
        let far = r#"{"lakes": [{"name": "x", "lat": 91.0, "lon": 2.0, "level_m": 1.0, "source": "", "note": ""}]}"#;
        assert!(parse_lake_levels(far).is_err());
        let ok = r#"{"lakes": [{"name": "x", "lat": 1.0, "lon": 2.0, "level_m": 1.5, "source": "s", "note": ""}]}"#;
        assert_eq!(parse_lake_levels(ok).unwrap()[0].overview_m, None);
        assert!(!parse_lake_levels(ok).unwrap()[0].endorheic);
        let uncited = r#"{"lakes": [{"name": "x", "lat": 1.0, "lon": 2.0, "level_m": 1.5, "source": "s", "note": "", "endorheic": true}]}"#;
        assert!(
            parse_lake_levels(uncited)
                .unwrap_err()
                .contains("endorheic_source")
        );
    }
}
