//! The climate the continental pass reads (rivers and lakes design §4.1, "Input"): the owner's
//! compiled-in table ([`climate_table`]) and the baked Beyer et al. 2020 grids
//! (`data/climate/`, Task 1), listed by a [`ClimateManifest`] that loads once the globe data is
//! ready.
//!
//! A missing or corrupt manifest, or a grid that will not load, means the zonal climate for
//! every age for the rest of the session, with one `warn` ([`ClimateIndex::fail`]): an older
//! bake or a failed fetch on the web still gets rivers, from the deep biome rule.

use std::fmt;
use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext, LoadState};
use bevy::prelude::*;
use serde::Deserialize;
use timefly_core::deep_time::YearsBp;
use timefly_core::hydrology::climate::{
    BIOME_ORDER, CLIMATE_GRID_COLS, CLIMATE_GRID_ROWS, Climate, ClimateTable, climate_bracket,
};

use crate::clock::LoadClock;
use crate::data::{GlobeData, describe_load_error};
use crate::region::frames::{CpuGrid, CpuGridSettings};
use crate::region::{LOAD_TIMEOUT_S, load_timed_out};

/// The climate manifest, relative to the asset root.
pub const CLIMATE_MANIFEST_PATH: &str = "data/climate/manifest.json";
/// The climate manifest version this build reads.
pub const CLIMATE_MANIFEST_VERSION: u32 = 1;
/// The owner's climate table, compiled in (Task 11's Python and `timefly_core`'s tests read the
/// same file).
pub const CLIMATE_TABLE_JSON: &str = include_str!("../../../../data/hydrology/climate_table.json");

#[derive(Deserialize)]
struct TableFile {
    pet_mm: Vec<(f32, f32)>,
    lake_evaporation_mm: Vec<(f32, f32)>,
    turc_pike_n: f32,
    ice_melt_runoff_mm: f32,
    biome_climate: std::collections::HashMap<String, (f32, f32)>,
}

/// Parses a climate table: breakpoint lists of `[°C, mm/yr]`, the Turc-Pike exponent, the ice
/// melt runoff and `biome_climate` rows of `[precipitation mm/yr, temperature °C]` keyed by the
/// [`BIOME_ORDER`] names.
pub fn parse_climate_table(json: &str) -> Result<ClimateTable, String> {
    let file: TableFile = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut biome_climate = [Climate {
        precip_mm: 0.0,
        temp_c: 0.0,
    }; 12];
    for (k, biome) in BIOME_ORDER.iter().enumerate() {
        let name = format!("{biome:?}");
        let &(precip_mm, temp_c) = file
            .biome_climate
            .get(&name)
            .ok_or(format!("no biome_climate row for {name}"))?;
        biome_climate[k] = Climate { precip_mm, temp_c };
    }
    for (key, points) in [
        ("pet_mm", &file.pet_mm),
        ("lake_evaporation_mm", &file.lake_evaporation_mm),
    ] {
        if points.is_empty() || points.windows(2).any(|w| w[0].0 >= w[1].0) {
            return Err(format!("{key} is not an ascending breakpoint list"));
        }
    }
    Ok(ClimateTable {
        pet_mm: file.pet_mm,
        lake_evaporation_mm: file.lake_evaporation_mm,
        turc_pike_n: file.turc_pike_n,
        ice_melt_runoff_mm: file.ice_melt_runoff_mm,
        biome_climate,
    })
}

/// The shipped climate table. Its test guarantees it parses.
pub fn climate_table() -> ClimateTable {
    parse_climate_table(CLIMATE_TABLE_JSON).expect("the shipped climate table parses")
}

/// One baked age's grid in the manifest.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ClimateFile {
    pub age_ka: f64,
    /// The file name inside `data/climate/`.
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
}

impl ClimateFile {
    /// The loader settings that check the file against this entry.
    pub fn settings(&self) -> CpuGridSettings {
        CpuGridSettings {
            bytes: Some(self.bytes),
            sha256: Some(self.sha256.clone()),
        }
    }

    /// The grid's asset path.
    pub fn path(&self) -> String {
        format!("data/climate/{}", self.file)
    }
}

/// The parsed `data/climate/manifest.json`: the baked ages (ka, ascending) and their files.
#[derive(Asset, TypePath, Clone, Debug, PartialEq)]
pub struct ClimateManifest {
    pub ages_ka: Vec<f64>,
    pub files: Vec<ClimateFile>,
}

#[derive(Deserialize)]
struct ManifestFile {
    version: u32,
    grid: GridShape,
    ages_ka: Vec<f64>,
    files: Vec<ClimateFile>,
}

#[derive(Deserialize)]
struct GridShape {
    rows: u32,
    cols: u32,
}

/// Why the climate manifest could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClimateManifestError(pub String);

impl fmt::Display for ClimateManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClimateManifestError {}

impl ClimateManifest {
    /// Parses and checks a manifest: version [`CLIMATE_MANIFEST_VERSION`], the 1° grid, ages
    /// finite and strictly ascending, one plain file name per age.
    pub fn parse(bytes: &[u8]) -> Result<ClimateManifest, ClimateManifestError> {
        let err = |s: String| ClimateManifestError(s);
        let file: ManifestFile = serde_json::from_slice(bytes)
            .map_err(|e| err(format!("invalid climate manifest: {e}")))?;
        if file.version != CLIMATE_MANIFEST_VERSION {
            return Err(err(format!(
                "climate manifest version {} is not supported (expected {CLIMATE_MANIFEST_VERSION})",
                file.version
            )));
        }
        if (file.grid.rows, file.grid.cols) != (CLIMATE_GRID_ROWS, CLIMATE_GRID_COLS) {
            return Err(err(format!(
                "a {} x {} climate grid is not supported (expected {CLIMATE_GRID_ROWS} x \
                 {CLIMATE_GRID_COLS})",
                file.grid.rows, file.grid.cols
            )));
        }
        let ascending = file.ages_ka.windows(2).all(|w| w[0] < w[1]);
        if file.ages_ka.is_empty() || !ascending || file.ages_ka.iter().any(|a| !a.is_finite()) {
            return Err(err(
                "the climate ages are not finite and ascending".to_string()
            ));
        }
        let matches = file.files.len() == file.ages_ka.len()
            && file
                .files
                .iter()
                .zip(&file.ages_ka)
                .all(|(f, &a)| f.age_ka == a);
        if !matches {
            return Err(err("the climate files do not match the ages".to_string()));
        }
        if let Some(f) = file
            .files
            .iter()
            .find(|f| f.file.is_empty() || f.file.contains(['/', '\\']) || f.file.starts_with('.'))
        {
            return Err(err(format!("{:?} is not a plain file name", f.file)));
        }
        Ok(ClimateManifest {
            ages_ka: file.ages_ka,
            files: file.files,
        })
    }

    /// The files around `age` and the older one's weight, or `None` past the baked range
    /// ([`climate_bracket`]), where the zonal rule applies.
    pub fn bracket(&self, age: YearsBp) -> Option<([&ClimateFile; 2], f64)> {
        let (younger, older, w) = climate_bracket(&self.ages_ka, age)?;
        Some(([&self.files[younger], &self.files[older]], w))
    }
}

#[derive(Default, TypePath)]
pub struct ClimateManifestLoader;

impl AssetLoader for ClimateManifestLoader {
    type Asset = ClimateManifest;
    type Settings = ();
    type Error = ClimateManifestError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<ClimateManifest, ClimateManifestError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| ClimateManifestError(format!("could not read: {e}")))?;
        ClimateManifest::parse(&bytes)
    }

    fn extensions(&self) -> &[&str] {
        &["json"]
    }
}

#[derive(Default)]
enum ManifestState {
    #[default]
    Idle,
    Loading {
        handle: Handle<ClimateManifest>,
        started_s: f64,
    },
    Ready(Arc<ClimateManifest>),
    /// Zonal for every age this session.
    Failed,
}

/// The session's climate data: the manifest, loaded once the globe data is ready, and whether
/// the climate has failed (then every pass uses the zonal rule).
#[derive(Resource, Default)]
pub struct ClimateIndex {
    state: ManifestState,
    warned: bool,
}

impl ClimateIndex {
    /// Whether the manifest is ready or the climate unavailable for the session.
    pub fn settled(&self) -> bool {
        matches!(self.state, ManifestState::Ready(_) | ManifestState::Failed)
    }

    /// The manifest, unless the climate failed.
    pub fn manifest(&self) -> Option<&Arc<ClimateManifest>> {
        match &self.state {
            ManifestState::Ready(m) => Some(m),
            _ => None,
        }
    }

    /// The climate is unavailable for the rest of the session: every pass from now on uses the
    /// zonal rule. Warns once per session, naming the first reason.
    pub fn fail(&mut self, path: &str, why: &str) {
        self.state = ManifestState::Failed;
        if !self.warned {
            self.warned = true;
            warn!(
                path,
                error = why,
                "climate data unavailable; the continental pass uses the zonal climate for every \
                 age"
            );
        }
    }
}

/// Starts the climate manifest's load once the globe data is ready and takes it in. A failure or
/// a timeout ([`LOAD_TIMEOUT_S`]) is the session's one climate `warn`.
pub(super) fn poll_climate_manifest(
    mut index: ResMut<ClimateIndex>,
    server: Res<AssetServer>,
    mut manifests: ResMut<Assets<ClimateManifest>>,
    globe: Option<Res<GlobeData>>,
    settings: Res<super::HydroSettings>,
    clock: Res<LoadClock>,
) {
    let now_s = clock.elapsed_s;
    match &index.state {
        ManifestState::Idle if globe.is_some() && settings.enabled => {
            index.state = ManifestState::Loading {
                handle: server.load(CLIMATE_MANIFEST_PATH),
                started_s: now_s,
            };
        }
        ManifestState::Loading { handle, started_s } => match server.load_state(handle) {
            LoadState::Loaded => match manifests.remove(handle) {
                Some(manifest) => {
                    debug!(ages = manifest.ages_ka.len(), "climate manifest loaded");
                    index.state = ManifestState::Ready(Arc::new(manifest));
                }
                None => index.fail(CLIMATE_MANIFEST_PATH, "the manifest vanished after loading"),
            },
            LoadState::Failed(e) => {
                let why = describe_load_error(CLIMATE_MANIFEST_PATH, &e);
                index.fail(CLIMATE_MANIFEST_PATH, why.trim_end());
            }
            _ if load_timed_out(*started_s, now_s, None) => index.fail(
                CLIMATE_MANIFEST_PATH,
                &format!("the load timed out after {LOAD_TIMEOUT_S:.0} s"),
            ),
            _ => {}
        },
        _ => {}
    }
}

/// Loads a climate grid checked against its manifest entry.
pub(super) fn load_grid(server: &AssetServer, file: &ClimateFile) -> Handle<CpuGrid> {
    let settings = file.settings();
    server
        .load_builder()
        .with_settings(move |s: &mut CpuGridSettings| *s = settings.clone())
        .load::<CpuGrid>(file.path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use timefly_core::worldgen::biome::BiomeClass;

    #[test]
    fn the_shipped_climate_table_parses() {
        let t = climate_table();
        assert_eq!(t.turc_pike_n, 2.0);
        assert_eq!(t.ice_melt_runoff_mm, 300.0);
        assert_eq!(t.pet_mm.len(), 6);
        assert_eq!(t.pet_mm[1], (-10.0, 120.0));
        assert_eq!(t.lake_evaporation_mm.last(), Some(&(30.0, 2200.0)));
        let c = |precip_mm, temp_c| Climate { precip_mm, temp_c };
        assert_eq!(t.biome(BiomeClass::HotDesert), c(60.0, 26.0));
        assert_eq!(t.biome(BiomeClass::Boreal), c(550.0, -2.0));
        assert_eq!(t.biome(BiomeClass::Alpine), c(900.0, -2.0));
    }

    #[test]
    fn bad_climate_tables_are_errors() {
        assert!(parse_climate_table("{").is_err());
        let no_alpine = CLIMATE_TABLE_JSON.replace("\"Alpine\"", "\"Alpinee\"");
        assert!(
            parse_climate_table(&no_alpine)
                .unwrap_err()
                .contains("Alpine")
        );
        let unsorted = CLIMATE_TABLE_JSON.replace("[-10, 120]", "[-30, 120]");
        assert!(
            parse_climate_table(&unsorted)
                .unwrap_err()
                .contains("pet_mm")
        );
    }

    fn manifest_json(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut v: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/assets/data/climate/manifest.json"
        ))
        .unwrap();
        edit(&mut v);
        serde_json::to_vec(&v).unwrap()
    }

    #[test]
    fn the_climate_manifest_parses_and_brackets_ages() {
        let m = ClimateManifest::parse(&manifest_json(|_| {})).unwrap();
        assert_eq!(m.ages_ka, [0.0, 10.0, 21.0, 26.0]);
        assert!(m.files[2].path().starts_with("data/climate/bio_21ka."));
        assert_eq!(m.files[0].settings().bytes, Some(1553));
        let ([y, o], w) = m.bracket(YearsBp(21.0e3)).unwrap();
        assert_eq!((y.age_ka, o.age_ka, w), (21.0, 21.0, 0.0));
        let ([y, o], w) = m.bracket(YearsBp(15.0e3)).unwrap();
        assert_eq!((y.age_ka, o.age_ka), (10.0, 21.0));
        assert!((w - 5.0 / 11.0).abs() < 1e-12, "{w}");
        // Past the baked range of Beyer et al. (120 ka): the zonal rule.
        assert!(m.bracket(YearsBp(1.0e6)).is_none());
    }

    #[test]
    fn bad_climate_manifests_are_errors() {
        let bad = |edit: fn(&mut serde_json::Value), says: &str| {
            let e = ClimateManifest::parse(&manifest_json(edit)).unwrap_err();
            assert!(e.0.contains(says), "{e}");
        };
        bad(|v| v["version"] = 2.into(), "version 2");
        bad(|v| v["grid"]["rows"] = 90.into(), "90 x 360");
        bad(|v| v["ages_ka"][1] = 30.0.into(), "ascending");
        bad(|v| v["files"][1]["age_ka"] = 11.0.into(), "do not match");
        bad(
            |v| v["files"][0]["file"] = "../x.tfgrid".into(),
            "plain file name",
        );
        assert!(ClimateManifest::parse(b"{").is_err());
    }
}
