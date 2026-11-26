//! The hidden `--bake-ridged-steps[=OUT_FILE]` (native, rivers and lakes Task 11g): bakes the
//! ridged steps ([`timefly_core::hydrology::ridged`]) at the recent lattice from the region
//! tiles (ETOPO 2022 60″) and the overview under the game's `assets/data/tiles`, for the shipped
//! course table at [`JOIN_RIDGE_M`], and writes them (by default to the source tree's
//! `data/hydrology/ridged_steps.bin`). Deterministic: the same tiles, overview and table give
//! the same bytes. Exits 0 when written, 1 when it cannot run or the file would pass its size
//! limit ([`MAX_BYTES`]).

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::math::DVec3;
use bevy::prelude::*;
use timefly_core::formats::{Grid, read_tfgrid};
use timefly_core::geo::{RenderDir, geo_to_latlon, render_to_geo};
use timefly_core::hydrology::CubeLattice;
use timefly_core::hydrology::params::{HYDRO_RECENT_N, JOIN_RIDGE_M};
use timefly_core::hydrology::ridged::{
    RidgedSteps, course_nodes, ridged_steps, shipped_table_hash,
};
use timefly_core::hydrology::today_rivers::shipped_today_rivers;
use timefly_core::worldgen::tiles::{TileSet, tile_of};

use crate::args::RidgedBake;
use crate::diagnostics::Session;
use crate::region::tiles::{OVERVIEW_PATH, TILE_MANIFEST_PATH, TileManifest, tile_path};

/// The source tree's file (a dev build's).
pub const DEFAULT_OUT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../data/hydrology/ridged_steps.bin"
);
/// The file's hard size limit (the owner's ruling, 2026-11-26).
pub const MAX_BYTES: usize = 256 * 1024;

fn read_grid(path: &Path) -> Result<Arc<Grid>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    read_tfgrid(&bytes)
        .map(Arc::new)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The 60″ region tiles under an asset folder, read as they are reached, and the overview: the
/// bake's terrain, and the reality check's for its ridged-joins bar.
pub struct FineTerrain {
    root: PathBuf,
    manifest: TileManifest,
    tiles: RefCell<TileSet>,
    tried: RefCell<HashSet<(u8, u8)>>,
    error: RefCell<Option<String>>,
}

fn latlon(d: DVec3) -> (f64, f64) {
    geo_to_latlon(render_to_geo(RenderDir(d)))
}

impl FineTerrain {
    /// The overview and the tile manifest under `root` (an asset folder holding `data/`).
    pub fn open(root: &Path) -> Result<FineTerrain, String> {
        let manifest_path = root.join(TILE_MANIFEST_PATH);
        let manifest = TileManifest::parse(
            &fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?,
        )
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
        let mut tiles = TileSet::new();
        tiles
            .set_overview(read_grid(&root.join(manifest.payload_path(OVERVIEW_PATH)))?)
            .map_err(|e| format!("overview: {e}"))?;
        Ok(FineTerrain {
            root: root.to_path_buf(),
            manifest,
            tiles: RefCell::new(tiles),
            tried: RefCell::new(HashSet::new()),
            error: RefCell::new(None),
        })
    }

    /// The 60″ elevation at `d` (the overview where the manifest has no tile).
    pub fn at(&self, d: DVec3) -> f64 {
        let (lat, lon) = latlon(d);
        let key = tile_of(lat, lon);
        if self.tried.borrow_mut().insert(key) && self.manifest.tiles.contains_key(&key) {
            let path = self
                .root
                .join(self.manifest.payload_path(&tile_path(key.0, key.1)));
            let read = read_grid(&path).and_then(|g| {
                self.tiles
                    .borrow_mut()
                    .insert(key.0, key.1, g)
                    .map_err(|e| format!("tile r{:02}c{:02}: {e}", key.0, key.1))
            });
            if let Err(e) = read {
                *self.error.borrow_mut() = Some(e);
            }
        }
        let t = self.tiles.borrow();
        t.sample(lat, lon)
            .or_else(|| t.sample_overview(lat, lon))
            .map_or(0.0, |s| f64::from(s.0))
    }

    /// The overview at `d`: the pass's unburnt elevation of a present-day node at its centre.
    pub fn overview_at(&self, d: DVec3) -> f64 {
        let (lat, lon) = latlon(d);
        self.tiles
            .borrow()
            .sample_overview(lat, lon)
            .map_or(0.0, |s| f64::from(s.0))
    }

    /// The first tile that failed to read, if any.
    pub fn error(&self) -> Option<String> {
        self.error.borrow().clone()
    }

    /// The tiles tried so far.
    pub fn tiles_tried(&self) -> usize {
        self.tried.borrow().len()
    }
}

/// What a bake found.
pub struct RidgedBakeReport {
    pub steps: RidgedSteps,
    pub course_nodes: usize,
    pub tiles_read: usize,
}

/// Bakes the ridged steps from the tiles under `root` (an asset folder holding `data/`).
pub fn bake_ridged_steps(root: &Path) -> Result<RidgedBakeReport, String> {
    let terrain = FineTerrain::open(root)?;
    let lattice = CubeLattice { n: HYDRO_RECENT_N };
    let unburnt = |v: u32| terrain.overview_at(lattice.dir(v).0);
    let fine_at = |d: DVec3| terrain.at(d);
    let course = course_nodes(&lattice, shipped_today_rivers());
    let steps = ridged_steps(&lattice, &course, &unburnt, &fine_at, JOIN_RIDGE_M);
    if let Some(e) = terrain.error() {
        return Err(e);
    }
    Ok(RidgedBakeReport {
        steps: RidgedSteps {
            n: lattice.n,
            ridge_m: JOIN_RIDGE_M as f32,
            table_hash: shipped_table_hash(),
            steps,
        },
        course_nodes: course.len(),
        tiles_read: terrain.tiles_tried(),
    })
}

pub fn run_ridged_bake(session: Session, bake: &RidgedBake) -> AppExit {
    let root = bevy::asset::io::file::FileAssetReader::get_base_path().join("assets");
    let out = PathBuf::from(bake.out.as_deref().unwrap_or(DEFAULT_OUT));
    let started = std::time::Instant::now();
    let result = bake_ridged_steps(&root).and_then(|report| {
        let bytes = report.steps.to_bytes();
        if bytes.len() > MAX_BYTES {
            return Err(format!(
                "{} steps make {} bytes, over the {MAX_BYTES}-byte limit",
                report.steps.steps.len(),
                bytes.len()
            ));
        }
        fs::write(&out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
        Ok(format!(
            "ridged steps: {} steps into {} course nodes at n = {}, threshold {} m, table hash \
             {:016x}; {} tiles read; {} bytes to {} in {:.1} s",
            report.steps.steps.len(),
            report.course_nodes,
            report.steps.n,
            report.steps.ridge_m,
            report.steps.table_hash,
            report.tiles_read,
            bytes.len(),
            out.display(),
            started.elapsed().as_secs_f64()
        ))
    });
    let exit = match result {
        Ok(line) => {
            info!("{line}");
            println!("{line}");
            AppExit::Success
        }
        Err(error) => {
            error!(%error, "ridged-steps bake could not run");
            session.eprint(&format!("ridged-steps bake could not run: {error}"));
            AppExit::from_code(1)
        }
    };
    session.finish_clean(&exit);
    exit
}
