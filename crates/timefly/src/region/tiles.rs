//! Present-day tiles for regions: the tile manifest, tile asset paths and a bounded cache of
//! decoded tiles.
//!
//! Every tile and the overview load through the asset server with the same relative path on
//! every platform (under the manifest’s generation directory for version 2): bundled files on desktop, fetched from the
//! Pages site on the web. The loader checks each file's size and SHA-256 against the manifest
//! ([`FileEntry::settings`]).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use bevy::asset::io::Reader;
use bevy::asset::{AssetLoader, LoadContext};
use bevy::prelude::*;
use serde::Deserialize;
use timefly_core::formats::Grid;
use timefly_core::worldgen::tiles::{TILE_COLS, TILE_DEG, TILE_ROWS, TILE_SAMPLES, TileSet};
use tracing::instrument;

use super::frames::CpuGridSettings;

/// The tile manifest, relative to the asset root.
pub const TILE_MANIFEST_PATH: &str = "data/tiles/manifest.json";
/// The global overview grid (ruling R7), relative to the asset root.
pub const OVERVIEW_PATH: &str = "data/tiles/overview.tfgrid";
pub const TILE_MANIFEST_VERSION: u32 = 2;
/// The cache's normal limit. A region that reads more than `TILE_CACHE_TILES −
/// TILE_CACHE_SLACK` tiles (near a pole, with moving plates) raises it to its own needs plus the
/// slack while it is current: see [`TileCache::capacity_for`].
pub const TILE_CACHE_TILES: usize = 96;
pub const TILE_CACHE_SLACK: usize = 16;

/// The asset path of tile `(row, col)`.
pub fn tile_path(row: u8, col: u8) -> String {
    format!("data/tiles/r{row:02}c{col:02}.tfgrid")
}

/// A file's manifest entry.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct FileEntry {
    pub bytes: u64,
    pub sha256: String,
}

impl FileEntry {
    /// The loader settings that check a download against this entry.
    pub fn settings(&self) -> CpuGridSettings {
        CpuGridSettings {
            bytes: Some(self.bytes),
            sha256: Some(self.sha256.clone()),
        }
    }
}

/// The parsed `data/tiles/manifest.json`.
#[derive(Asset, TypePath, Clone, Debug, PartialEq)]
pub struct TileManifest {
    generation: Option<String>,
    pub overview: Option<FileEntry>,
    pub tiles: HashMap<(u8, u8), FileEntry>,
}

#[derive(Deserialize)]
struct ManifestFile {
    generation: Option<String>,
    version: u32,
    tile_deg: f64,
    samples: u32,
    #[serde(default)]
    overview: Option<FileEntry>,
    tiles: Vec<TileFile>,
}

#[derive(Deserialize)]
struct TileFile {
    row: u8,
    col: u8,
    #[serde(flatten)]
    entry: FileEntry,
}

/// Why the tile manifest could not be used.
#[derive(Clone, Debug, PartialEq)]
pub enum TileManifestError {
    Io(String),
    Json(String),
    Version(u32),
    InvalidGeneration,
    Layout { tile_deg: f64, samples: u32 },
    BadTile { row: u8, col: u8 },
}

impl fmt::Display for TileManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileManifestError::Io(e) => write!(f, "could not read: {e}"),
            TileManifestError::Json(e) => write!(f, "invalid tile manifest: {e}"),
            TileManifestError::InvalidGeneration => write!(f, "invalid tile generation digest"),
            TileManifestError::Version(v) => write!(
                f,
                "tile manifest version {v} is not supported (supported versions: 1 and {TILE_MANIFEST_VERSION})"
            ),
            TileManifestError::Layout { tile_deg, samples } => write!(
                f,
                "tiles of {tile_deg}° with {samples} samples are not supported \
                 (expected {TILE_DEG}° with {TILE_SAMPLES})"
            ),
            TileManifestError::BadTile { row, col } => {
                write!(f, "tile r{row:02}c{col:02} is outside the tile map")
            }
        }
    }
}

impl std::error::Error for TileManifestError {}

impl TileManifest {
    /// Resolve a standard tile asset path in the generation selected by this manifest.
    pub fn payload_path(&self, standard: &str) -> String {
        match &self.generation {
            Some(id) => format!(
                "data/tiles/{id}/{}",
                standard.rsplit('/').next().unwrap_or(standard)
            ),
            None => standard.to_owned(),
        }
    }

    /// Parses and checks a manifest.
    #[instrument(level = "debug", skip_all, fields(len = bytes.len()), err)]
    pub fn parse(bytes: &[u8]) -> Result<TileManifest, TileManifestError> {
        let file: ManifestFile =
            serde_json::from_slice(bytes).map_err(|e| TileManifestError::Json(e.to_string()))?;
        if !matches!(file.version, 1 | TILE_MANIFEST_VERSION) {
            return Err(TileManifestError::Version(file.version));
        }
        match (file.version, file.generation.as_deref()) {
            (1, None) => {}
            (TILE_MANIFEST_VERSION, Some(id))
                if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return Err(TileManifestError::InvalidGeneration),
        }
        if file.tile_deg != TILE_DEG || file.samples != TILE_SAMPLES {
            return Err(TileManifestError::Layout {
                tile_deg: file.tile_deg,
                samples: file.samples,
            });
        }
        let mut tiles = HashMap::with_capacity(file.tiles.len());
        for t in file.tiles {
            if t.row >= TILE_ROWS || t.col >= TILE_COLS {
                return Err(TileManifestError::BadTile {
                    row: t.row,
                    col: t.col,
                });
            }
            tiles.insert((t.row, t.col), t.entry);
        }
        Ok(TileManifest {
            generation: file.generation,
            overview: file.overview,
            tiles,
        })
    }
}

#[derive(Default, TypePath)]
pub struct TileManifestLoader;

impl AssetLoader for TileManifestLoader {
    type Asset = TileManifest;
    type Settings = ();
    type Error = TileManifestError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<TileManifest, TileManifestError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| TileManifestError::Io(e.to_string()))?;
        TileManifest::parse(&bytes)
    }

    fn extensions(&self) -> &[&str] {
        &["json"]
    }
}

#[derive(Debug)]
struct CachedTile {
    grid: Arc<Grid>,
    last_used: u64,
}

/// Decoded tiles, least recently used first out, plus the overview (which the cache keeps for
/// the session: one file, needed by every region). Holds [`TILE_CACHE_TILES`] tiles, or a
/// region's whole needed set plus [`TILE_CACHE_SLACK`] when that is larger; tiles the current
/// region needs are never evicted.
#[derive(Resource, Default, Debug)]
pub struct TileCache {
    tiles: HashMap<(u8, u8), CachedTile>,
    overview: Option<Arc<Grid>>,
    clock: u64,
}

impl TileCache {
    /// The limit while a region needing `needed` tiles is current.
    pub fn capacity_for(needed: usize) -> usize {
        TILE_CACHE_TILES.max(needed + TILE_CACHE_SLACK)
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn contains(&self, key: (u8, u8)) -> bool {
        self.tiles.contains_key(&key)
    }

    pub fn overview(&self) -> Option<&Arc<Grid>> {
        self.overview.as_ref()
    }

    /// Sets the overview after checking its shape ([`TileSet::set_overview`]).
    pub fn set_overview(
        &mut self,
        grid: Arc<Grid>,
    ) -> Result<(), timefly_core::worldgen::tiles::TileError> {
        TileSet::new().set_overview(grid.clone())?;
        self.overview = Some(grid);
        Ok(())
    }

    /// Adds a tile after checking its shape ([`TileSet::insert`]), then evicts the least
    /// recently used tiles outside `needed` (the current region's set) down to
    /// [`capacity_for`](Self::capacity_for).
    pub fn insert(
        &mut self,
        key: (u8, u8),
        grid: Arc<Grid>,
        needed: &[(u8, u8)],
    ) -> Result<(), timefly_core::worldgen::tiles::TileError> {
        TileSet::new().insert(key.0, key.1, grid.clone())?;
        self.clock += 1;
        self.tiles.insert(
            key,
            CachedTile {
                grid,
                last_used: self.clock,
            },
        );
        self.evict(needed);
        Ok(())
    }

    fn evict(&mut self, needed: &[(u8, u8)]) {
        let capacity = Self::capacity_for(needed.len());
        while self.tiles.len() > capacity {
            let victim = self
                .tiles
                .iter()
                .filter(|(key, _)| !needed.contains(key))
                .min_by_key(|(_, tile)| tile.last_used)
                .map(|(key, _)| *key);
            match victim {
                Some(key) => {
                    self.tiles.remove(&key);
                }
                None => break,
            }
        }
    }

    /// The tile set for a region: every cached tile of `needed` (marked as just used) and the
    /// overview. Tiles of `needed` that are not cached are simply absent; the caller compares
    /// with [`TileSet::missing`].
    pub fn tile_set(&mut self, needed: &[(u8, u8)]) -> TileSet {
        self.clock += 1;
        let mut set = TileSet::new();
        for key in needed {
            if let Some(tile) = self.tiles.get_mut(key) {
                tile.last_used = self.clock;
                set.insert(key.0, key.1, tile.grid.clone())
                    .expect("cached tiles were checked on insert");
            }
        }
        if let Some(overview) = &self.overview {
            set.set_overview(overview.clone())
                .expect("the overview was checked on insert");
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use timefly_core::formats::{DType, GridKind};
    use timefly_core::geo::{geo_to_render, latlon_to_geo};
    use timefly_core::worldgen::frame::RegionFrame;

    use super::*;

    fn tile_grid() -> Arc<Grid> {
        let n = TILE_SAMPLES;
        Arc::new(Grid {
            kind: GridKind::LatLon,
            dtype: DType::F16,
            channels: 2,
            width: n,
            height: n,
            data: vec![0; (n * n * 4) as usize],
        })
    }

    fn frame(lat: f64, lon: f64) -> RegionFrame {
        RegionFrame {
            center: geo_to_render(latlon_to_geo(lat, lon)),
        }
    }

    #[test]
    fn tile_cache_is_bounded() {
        let grid = tile_grid();
        let mut cache = TileCache::default();
        // 30 regions around the globe, both poles and the antimeridian among them. Every needed
        // tile is fetched when it is not cached, as the region plugin does.
        let mut fetched = 0;
        for k in 0..30 {
            let (lat, lon) = match k {
                0 => (89.5, 0.0),
                1 => (-89.0, 120.0),
                2 => (0.0, 179.9),
                _ => (
                    -75.0 + 150.0 * f64::from(k) / 29.0,
                    -180.0 + 37.0 * f64::from(k),
                ),
            };
            let needed = TileSet::needed_for(&frame(lat, lon));
            for &key in &needed {
                if !cache.contains(key) {
                    cache.insert(key, grid.clone(), &needed).unwrap();
                    fetched += 1;
                }
            }
            let set = cache.tile_set(&needed);
            assert!(set.missing(&needed).is_empty(), "region {k} lost a tile");
            assert!(cache.len() <= TILE_CACHE_TILES, "{} tiles", cache.len());
        }
        assert!(
            fetched > TILE_CACHE_TILES,
            "the test never filled the cache"
        );
    }

    #[test]
    fn cache_evicts_least_recently_used_outside_the_needed_set() {
        let grid = tile_grid();
        let mut cache = TileCache::default();
        let first: Vec<(u8, u8)> = (0..TILE_CACHE_TILES)
            .map(|i| (10 + (i / 72) as u8, (i % 72) as u8))
            .collect();
        for &key in &first {
            cache.insert(key, grid.clone(), &first).unwrap();
        }
        assert_eq!(cache.len(), TILE_CACHE_TILES);
        // Touch the oldest tile; the next oldest goes first.
        cache.tile_set(&[first[0]]);
        let next = [(30, 0)];
        cache.insert(next[0], grid.clone(), &next).unwrap();
        assert_eq!(cache.len(), TILE_CACHE_TILES);
        assert!(cache.contains(first[0]));
        assert!(!cache.contains(first[1]));
        assert!(cache.contains(next[0]));
    }

    #[test]
    fn a_large_needed_set_raises_the_limit_and_is_never_evicted() {
        let grid = tile_grid();
        let mut cache = TileCache::default();
        let old: Vec<(u8, u8)> = (0..72).map(|c| (20, c)).collect();
        for &key in &old {
            cache.insert(key, grid.clone(), &old).unwrap();
        }
        // A polar region with moving plates: two full rows.
        let needed: Vec<(u8, u8)> = (0..72).flat_map(|c| [(0, c), (1, c)]).collect();
        for &key in &needed {
            cache.insert(key, grid.clone(), &needed).unwrap();
        }
        assert_eq!(cache.len(), needed.len() + TILE_CACHE_SLACK);
        assert!(needed.iter().all(|&k| cache.contains(k)));
        assert_eq!(TileCache::capacity_for(10), TILE_CACHE_TILES);
        assert_eq!(TileCache::capacity_for(144), 160);
    }

    #[test]
    fn cache_rejects_bad_shapes() {
        let mut cache = TileCache::default();
        let mut bad = (*tile_grid()).clone();
        bad.channels = 1;
        assert!(cache.insert((0, 0), Arc::new(bad), &[]).is_err());
        assert!(cache.is_empty());
        assert!(cache.set_overview(tile_grid()).is_err());
        assert!(cache.overview().is_none());
    }

    #[test]
    fn manifest_parses_and_checks_layout() {
        let json = br#"{"version":1,"tile_deg":5,"samples":301,
            "sources":{"etopo":"a","koppen":"b"},
            "overview":{"bytes":10,"sha256":"ab"},
            "tiles":[{"row":17,"col":35,"bytes":5,"sha256":"cd"}]}"#;
        let m = TileManifest::parse(json).unwrap();
        assert_eq!(
            m.overview,
            Some(FileEntry {
                bytes: 10,
                sha256: "ab".into()
            })
        );
        assert_eq!(m.tiles[&(17, 35)].bytes, 5);
        assert_eq!(m.tiles[&(17, 35)].settings().sha256.as_deref(), Some("cd"));
        assert_eq!(tile_path(3, 7), "data/tiles/r03c07.tfgrid");

        let unsupported = br#"{"version":3,"tile_deg":5,"samples":301,"tiles":[]}"#;
        assert_eq!(
            TileManifest::parse(unsupported).unwrap_err(),
            TileManifestError::Version(3)
        );
        let layout = br#"{"version":1,"tile_deg":1,"samples":61,"tiles":[]}"#;
        assert!(matches!(
            TileManifest::parse(layout),
            Err(TileManifestError::Layout { .. })
        ));
        let off_map = br#"{"version":1,"tile_deg":5,"samples":301,
            "tiles":[{"row":36,"col":0,"bytes":5,"sha256":"cd"}]}"#;
        assert_eq!(
            TileManifest::parse(off_map).unwrap_err(),
            TileManifestError::BadTile { row: 36, col: 0 }
        );
        assert!(matches!(
            TileManifest::parse(b"{"),
            Err(TileManifestError::Json(_))
        ));
    }
}
