//! Present-day elevation and climate tiles (ETOPO 2022 and Köppen-Geiger, baked by Task 1).
//!
//! Each tile is a 5° × 5° `LatLon` grid of 301 × 301 samples on the 60″ lattice, with both edges
//! included, so neighbouring tiles (also across the antimeridian) share their edge samples.
//! Channel 0 is elevation in metres, channel 1 the Köppen code (0 = ocean or no data, 1–30 =
//! Beck classes). Row 0 is the tile's north edge: sample `(i, j)` sits at
//! `lat = lat_n − i/60`, `lon = lon_w + j/60`, where tile `(row, col)` has `lat_n = 90 − 5·row`
//! and `lon_w = −180 + 5·col`.
//!
//! The **overview** (ruling R7) is one global grid on a coarser node lattice (5′ in the bake:
//! 2161 × 4321), rows from 90°N to 90°S and columns from −180° to +180°, both edges included.
//! Region aprons read only the overview; inner fields read tiles, then the overview where a tile
//! is missing ([`TileSet::lookup`]).

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::formats::{Grid, GridKind};
use bevy_math::DQuat;

use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use crate::worldgen::REGION_SIZE_M;
use crate::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};

/// Degrees per tile side.
pub const TILE_DEG: f64 = 5.0;
/// Samples per tile side, both edges included.
pub const TILE_SAMPLES: u32 = 301;
/// Tile rows (north to south) and columns (west to east).
pub const TILE_ROWS: u8 = 36;
pub const TILE_COLS: u8 = 72;
/// Sample spacing of the tiles: 60″ of latitude, in metres.
pub const TILE_SPACING_M: f64 = 1852.0;
/// [`TileSet::needed_for`] widens the inner field by this much on every side, for the coastline
/// clamp's corner samples and the coarse lattice's overhang past the field's edge.
pub const TILE_MARGIN_M: f64 = 5_000.0;

/// Samples per degree.
const PER_DEG: f64 = (TILE_SAMPLES - 1) as f64 / TILE_DEG;

/// Which data a lookup may read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SourceLevel {
    /// A loaded tile, else the overview (inner fields).
    Tiles,
    /// The overview only (aprons, and lattices as coarse as the apron's).
    Overview,
}

/// A tile grid that is not a 2-channel 301 × 301 `LatLon` grid, a tile index off the map, or an
/// overview that is not a global 2-channel node lattice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileError {
    BadIndex { row: u8, col: u8 },
    BadShape { row: u8, col: u8 },
    BadOverview { width: u32, height: u32 },
}

impl fmt::Display for TileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TileError::BadIndex { row, col } => {
                write!(f, "tile r{row:02}c{col:02} is outside the 36 x 72 tile map")
            }
            TileError::BadShape { row, col } => write!(
                f,
                "tile r{row:02}c{col:02} is not a 2-channel 301 x 301 lat-lon grid"
            ),
            TileError::BadOverview { width, height } => write!(
                f,
                "the overview ({width} x {height}) is not a 2-channel global lat-lon node lattice \
                 (180k + 1 rows by 360k + 1 columns)"
            ),
        }
    }
}

impl std::error::Error for TileError {}

/// The tile `(row, col)` holding a latitude and longitude (degrees). Latitudes clamp to
/// [−90, 90]; longitudes wrap, with 180° in column 0 (−180°).
pub fn tile_of(lat_deg: f64, lon_deg: f64) -> (u8, u8) {
    let row = ((90.0 - lat_deg.clamp(-90.0, 90.0)) / TILE_DEG).floor();
    let col = ((wrap_lon(lon_deg) + 180.0) / TILE_DEG).floor();
    (
        row.clamp(0.0, f64::from(TILE_ROWS - 1)) as u8,
        col.clamp(0.0, f64::from(TILE_COLS - 1)) as u8,
    )
}

/// Longitude wrapped into [−180, 180).
fn wrap_lon(lon_deg: f64) -> f64 {
    (lon_deg + 180.0).rem_euclid(360.0) - 180.0
}

/// The decoded tiles a region needs, keyed by `(row, col)`, and the global overview. Grids are
/// shared (`Arc`), so a caller's cache can hand them over without copying.
#[derive(Clone, Debug, Default)]
pub struct TileSet {
    tiles: HashMap<(u8, u8), Arc<Grid>>,
    overview: Option<Arc<Grid>>,
}

/// Bilinear channel 0 and nearest channel 1 (a Köppen code) of a lat-lon grid at fractional
/// row `i` and column `j`, both already clamped to the grid.
fn sample_nodes(grid: &Grid, i: f64, j: f64) -> (f32, u8) {
    let (last_i, last_j) = (f64::from(grid.height - 1), f64::from(grid.width - 1));
    let (i0, j0) = (i.floor().min(last_i - 1.0), j.floor().min(last_j - 1.0));
    let (fi, fj) = ((i - i0) as f32, (j - j0) as f32);
    let (i0, j0) = (i0 as u32, j0 as u32);
    let e = |r: u32, c: u32| grid.texel_f32(0, r, c, 0);
    let top = e(i0, j0) + (e(i0, j0 + 1) - e(i0, j0)) * fj;
    let bottom = e(i0 + 1, j0) + (e(i0 + 1, j0 + 1) - e(i0 + 1, j0)) * fj;
    let elevation = top + (bottom - top) * fi;
    let code = grid.texel_f32(0, i.round() as u32, j.round() as u32, 1);
    let code = if (0.0..=30.0).contains(&code) {
        code.round() as u8
    } else {
        0
    };
    (elevation, code)
}

impl TileSet {
    pub fn new() -> TileSet {
        TileSet::default()
    }

    /// Adds (or replaces) a tile after checking its index and shape.
    pub fn insert(
        &mut self,
        row: u8,
        col: u8,
        grid: impl Into<Arc<Grid>>,
    ) -> Result<(), TileError> {
        if row >= TILE_ROWS || col >= TILE_COLS {
            return Err(TileError::BadIndex { row, col });
        }
        let grid = grid.into();
        let shape_ok = grid.kind == GridKind::LatLon
            && grid.channels == 2
            && grid.width == TILE_SAMPLES
            && grid.height == TILE_SAMPLES;
        if !shape_ok {
            return Err(TileError::BadShape { row, col });
        }
        self.tiles.insert((row, col), grid);
        Ok(())
    }

    pub fn contains(&self, row: u8, col: u8) -> bool {
        self.tiles.contains_key(&(row, col))
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// Sets the overview after checking that it is a 2-channel `LatLon` node lattice covering
    /// the globe with both edges: `180k + 1` rows by `360k + 1` columns (k = 12 in the bake).
    pub fn set_overview(&mut self, grid: impl Into<Arc<Grid>>) -> Result<(), TileError> {
        let grid = grid.into();
        let (w, h) = (grid.width, grid.height);
        let per_deg = h.saturating_sub(1) / 180;
        let ok = grid.kind == GridKind::LatLon
            && grid.channels == 2
            && per_deg >= 1
            && h == 180 * per_deg + 1
            && w == 360 * per_deg + 1;
        if !ok {
            return Err(TileError::BadOverview {
                width: w,
                height: h,
            });
        }
        self.overview = Some(grid);
        Ok(())
    }

    pub fn has_overview(&self) -> bool {
        self.overview.is_some()
    }

    /// The overview's node spacing in metres (along a meridian), if there is one.
    pub fn overview_spacing_m(&self) -> Option<f64> {
        let per_deg = f64::from((self.overview.as_ref()?.height - 1) / 180);
        Some(EARTH_RADIUS_M * (1.0f64).to_radians() / per_deg)
    }

    /// The tiles of `needed` that are not loaded. `needed` is what the field will read,
    /// [`CoarseField::tiles_needed`](crate::worldgen::coarse::CoarseField::tiles_needed); a
    /// non-empty result means the region falls back (`fallback = true`).
    pub fn missing(&self, needed: &[(u8, u8)]) -> Vec<(u8, u8)> {
        needed
            .iter()
            .copied()
            .filter(|&(r, c)| !self.contains(r, c))
            .collect()
    }

    /// Every tile the inner field ([`REGION_SIZE_M`]) around `frame` can read where the data is
    /// read in the era frame itself (the recent eras), widened by [`TILE_MARGIN_M`] on every
    /// side, sorted. At most one row of 72 tiles when the field contains a pole, and fewer
    /// elsewhere. Callers ask the field instead
    /// ([`CoarseField::tiles_needed`](crate::worldgen::coarse::CoarseField::tiles_needed)),
    /// which also covers reads moved by plate rotations.
    pub fn needed_for(frame: &RegionFrame) -> Vec<(u8, u8)> {
        Self::covering(frame, REGION_SIZE_M, DQuat::IDENTITY)
    }

    /// [`needed_for`](Self::needed_for) for reads at `q · d` instead of `d`: the inner field
    /// carried rigidly by the rotation `q` (a plate's era-to-present rotation).
    pub fn needed_for_rotated(frame: &RegionFrame, q: DQuat) -> Vec<(u8, u8)> {
        Self::covering(frame, REGION_SIZE_M, q)
    }

    /// The tiles the square of side `size_m` (plus the margin), carried by the rotation `q`,
    /// touches. The image's latitude and longitude extremes lie on its boundary unless it
    /// contains a pole, in which case it takes whole rows of tiles.
    fn covering(frame: &RegionFrame, size_m: f64, q: DQuat) -> Vec<(u8, u8)> {
        let half = size_m / 2.0 + TILE_MARGIN_M;
        let basis = frame.basis();
        let q = q.normalize();
        let image = |d: RenderDir| RenderDir((q * d.0).normalize());
        let (_, centre_lon) = geo_to_latlon(render_to_geo(image(RenderDir(basis.center))));
        // Boundary samples every ~1 km (at least 64 per edge).
        let steps = ((2.0 * half / 1000.0).ceil() as usize).max(64);
        let (mut lat_min, mut lat_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut rel_min, mut rel_max) = (f64::INFINITY, f64::NEG_INFINITY);
        for k in 0..=steps {
            let t = -half + 2.0 * half * k as f64 / steps as f64;
            for (x, z) in [(t, -half), (t, half), (-half, t), (half, t)] {
                let (lat, lon) = geo_to_latlon(render_to_geo(image(basis.local_to_dir(x, z))));
                lat_min = lat_min.min(lat);
                lat_max = lat_max.max(lat);
                let rel = wrap_lon(lon - centre_lon);
                rel_min = rel_min.min(rel);
                rel_max = rel_max.max(rel);
            }
        }
        let inside = |lat: f64| {
            let pole = geo_to_render(latlon_to_geo(lat, 0.0));
            basis
                .dir_to_local(RenderDir((q.inverse() * pole.0).normalize()))
                .is_some_and(|(x, z)| x.abs() <= half && z.abs() <= half)
        };
        // Pad by a sample, so a point a hair outside the sampled extremes still finds its tile.
        let pad = 1.0 / PER_DEG;
        let mut all_columns = false;
        if inside(90.0) {
            lat_max = 90.0;
            all_columns = true;
        }
        if inside(-90.0) {
            lat_min = -90.0;
            all_columns = true;
        }
        let (row_top, _) = tile_of(lat_max + pad, 0.0);
        let (row_bottom, _) = tile_of(lat_min - pad, 0.0);
        let mut cols: Vec<u8> = Vec::new();
        if all_columns || rel_max - rel_min + 2.0 * pad >= 360.0 - TILE_DEG {
            cols.extend(0..TILE_COLS);
        } else {
            let first = centre_lon + rel_min - pad;
            let last = centre_lon + rel_max + pad;
            let mut lon = first;
            loop {
                let (_, c) = tile_of(0.0, lon.min(last));
                if !cols.contains(&c) {
                    cols.push(c);
                }
                if lon >= last {
                    break;
                }
                lon += TILE_DEG / 2.0;
            }
        }
        let mut out: Vec<(u8, u8)> = (row_top..=row_bottom)
            .flat_map(|r| cols.iter().map(move |&c| (r, c)))
            .collect();
        out.sort_unstable();
        out
    }

    /// Bilinear elevation (m) and the nearest Köppen code at a latitude and longitude (degrees),
    /// or `None` if the tile is missing or the position is not finite. Codes outside 0–30 read 0.
    pub fn sample(&self, lat_deg: f64, lon_deg: f64) -> Option<(f32, u8)> {
        if !(lat_deg.is_finite() && lon_deg.is_finite()) {
            return None;
        }
        let lat = lat_deg.clamp(-90.0, 90.0);
        let lon = wrap_lon(lon_deg);
        let (row, col) = tile_of(lat, lon);
        let grid = self.tiles.get(&(row, col))?;
        let lat_n = 90.0 - TILE_DEG * f64::from(row);
        let lon_w = -180.0 + TILE_DEG * f64::from(col);
        let last = f64::from(TILE_SAMPLES - 1);
        let i = ((lat_n - lat) * PER_DEG).clamp(0.0, last);
        let j = ((lon - lon_w) * PER_DEG).clamp(0.0, last);
        Some(sample_nodes(grid, i, j))
    }

    /// [`sample`](Self::sample) from the overview, or `None` without one.
    pub fn sample_overview(&self, lat_deg: f64, lon_deg: f64) -> Option<(f32, u8)> {
        if !(lat_deg.is_finite() && lon_deg.is_finite()) {
            return None;
        }
        let grid = self.overview.as_ref()?;
        let per_deg = f64::from((grid.height - 1) / 180);
        let i = (90.0 - lat_deg.clamp(-90.0, 90.0)) * per_deg;
        let j = (wrap_lon(lon_deg) + 180.0) * per_deg;
        Some(sample_nodes(
            grid,
            i.clamp(0.0, f64::from(grid.height - 1)),
            j.clamp(0.0, f64::from(grid.width - 1)),
        ))
    }

    /// The lookup order of a level: [`SourceLevel::Tiles`] reads a loaded tile, else the
    /// overview; [`SourceLevel::Overview`] reads only the overview. `None` means the caller
    /// falls back to its cube frames.
    pub fn lookup(&self, lat_deg: f64, lon_deg: f64, level: SourceLevel) -> Option<(f32, u8)> {
        match level {
            SourceLevel::Tiles => self
                .sample(lat_deg, lon_deg)
                .or_else(|| self.sample_overview(lat_deg, lon_deg)),
            SourceLevel::Overview => self.sample_overview(lat_deg, lon_deg),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::formats::DType;

    /// A tile whose elevation and Köppen code are functions of the sample's (lat, lon).
    pub(crate) fn tile_from(row: u8, col: u8, f: impl Fn(f64, f64) -> (f32, u8)) -> Grid {
        let lat_n = 90.0 - TILE_DEG * f64::from(row);
        let lon_w = -180.0 + TILE_DEG * f64::from(col);
        let n = TILE_SAMPLES;
        let mut data = Vec::with_capacity((n * n * 4) as usize);
        for i in 0..n {
            for j in 0..n {
                let (e, k) = f(
                    lat_n - f64::from(i) / PER_DEG,
                    lon_w + f64::from(j) / PER_DEG,
                );
                data.extend_from_slice(&half::f16::from_f32(e).to_le_bytes());
                data.extend_from_slice(&half::f16::from_f32(f32::from(k)).to_le_bytes());
            }
        }
        Grid {
            kind: GridKind::LatLon,
            dtype: DType::F16,
            channels: 2,
            width: n,
            height: n,
            data,
        }
    }

    fn frame_at(lat: f64, lon: f64) -> RegionFrame {
        RegionFrame {
            center: geo_to_render(latlon_to_geo(lat, lon)),
        }
    }

    #[test]
    fn tile_indices_cover_the_map() {
        assert_eq!(tile_of(90.0, -180.0), (0, 0));
        assert_eq!(tile_of(89.9, 179.9), (0, 71));
        assert_eq!(tile_of(-90.0, 0.0), (35, 36));
        assert_eq!(tile_of(0.0, 180.0), (18, 0), "180 E is -180");
        assert_eq!(tile_of(0.01, -0.01), (17, 35));
        assert_eq!(tile_of(-0.01, 0.0), (18, 36));
        assert_eq!(tile_of(10.0, 540.0), (16, 0));
    }

    #[test]
    fn insert_rejects_bad_tiles() {
        let mut set = TileSet::new();
        assert_eq!(
            set.insert(36, 0, tile_from(0, 0, |_, _| (0.0, 0))),
            Err(TileError::BadIndex { row: 36, col: 0 })
        );
        let mut small = tile_from(0, 0, |_, _| (0.0, 0));
        small.width = 300;
        assert_eq!(
            set.insert(0, 0, small),
            Err(TileError::BadShape { row: 0, col: 0 })
        );
        assert!(set.is_empty());
        set.insert(3, 4, tile_from(3, 4, |_, _| (0.0, 0))).unwrap();
        assert!(set.contains(3, 4) && set.len() == 1);
    }

    #[test]
    fn sample_is_bilinear_with_nearest_koppen() {
        // Elevation linear in lat and lon (integers below 2048 at samples, so exact in f16), code
        // by longitude.
        let f = |lat: f64, lon: f64| {
            (
                ((lat - 5.0) * 240.0 + (lon - 5.0) * 60.0) as f32,
                if lon < 7.5 { 3 } else { 17 },
            )
        };
        let mut set = TileSet::new();
        set.insert(16, 37, tile_from(16, 37, f)).unwrap(); // lat 5..10 N, lon 5..10 E
        let (lat, lon) = (7.25 + 0.3 / 60.0, 7.5 + 0.6 / 60.0);
        let (e, k) = set.sample(lat, lon).unwrap();
        let expected = ((lat - 5.0) * 240.0 + (lon - 5.0) * 60.0) as f32;
        assert!((e - expected).abs() < 0.5, "{e} vs {expected}");
        assert_eq!(k, 17, "nearest sample is east of 7.5");
        let (_, k) = set.sample(lat, 7.5 - 0.6 / 60.0).unwrap();
        assert_eq!(k, 3);
        assert_eq!(set.sample(12.0, 7.0), None, "missing tile");
        assert_eq!(set.sample(f64::NAN, 7.0), None);
    }

    #[test]
    fn tiles_wrap_the_antimeridian() {
        let f = |_: f64, lon: f64| ((lon.abs() * 10.0) as f32, 1);
        let mut set = TileSet::new();
        set.insert(18, 0, tile_from(18, 0, f)).unwrap();
        set.insert(18, 71, tile_from(18, 71, f)).unwrap();
        // Just west of 180 reads column 71, just east (and 180 itself) column 0; they meet at 1800.
        let west = set.sample(-2.0, 179.999).unwrap().0;
        let east = set.sample(-2.0, -179.999).unwrap().0;
        let at = set.sample(-2.0, 180.0).unwrap().0;
        assert!(
            (west - 1800.0).abs() < 2.0 && (east - 1800.0).abs() < 2.0,
            "{west} {east}"
        );
        assert_eq!(at, set.sample(-2.0, -180.0).unwrap().0);
    }

    #[test]
    fn needed_tiles_cover_the_square() {
        let cases = [
            (0.0, 0.0),
            (2.5, 2.5),
            (45.3, 179.9),
            (-12.0, -179.9),
            (70.0, 20.0),
            (89.5, 33.0),
            (-89.5, -150.0),
            (90.0, 0.0),
        ];
        for (lat, lon) in cases {
            // The inner field through `needed_for`; the apron-sized square through the same
            // covering rule (the apron itself reads only the overview).
            for size in [
                crate::worldgen::REGION_SIZE_M,
                crate::worldgen::APRON_SIZE_M,
            ] {
                let frame = frame_at(lat, lon);
                let needed = TileSet::covering(&frame, size, DQuat::IDENTITY);
                if size == crate::worldgen::REGION_SIZE_M {
                    assert_eq!(needed, TileSet::needed_for(&frame));
                }
                let half = size / 2.0 + TILE_MARGIN_M * 0.99;
                let n = 80;
                for a in 0..=n {
                    for b in 0..=n {
                        let x = -half + 2.0 * half * f64::from(a) / f64::from(n);
                        let z = -half + 2.0 * half * f64::from(b) / f64::from(n);
                        let (plat, plon) = geo_to_latlon(render_to_geo(frame.local_to_dir(x, z)));
                        let t = tile_of(plat, plon);
                        assert!(
                            needed.binary_search(&t).is_ok(),
                            "centre ({lat},{lon}) size {size}: ({plat},{plon}) in {t:?} missing"
                        );
                    }
                }
            }
        }
        // A mid-latitude inner field spans at most two tiles per axis.
        assert!(TileSet::needed_for(&frame_at(37.0, -122.0)).len() <= 4);
    }

    #[test]
    fn needed_tiles_stay_bounded_near_the_poles() {
        let mut worst = 0;
        for k in 0..=80 {
            let lat = 80.0 + f64::from(k) * 0.125;
            for lon in [-180.0, -97.0, 0.0, 33.0, 179.9] {
                for sign in [1.0, -1.0] {
                    let n = TileSet::needed_for(&frame_at(sign * lat, lon)).len();
                    worst = worst.max(n);
                }
            }
        }
        assert!(worst <= 80, "{worst} tiles");
        assert_eq!(TileSet::needed_for(&frame_at(90.0, 0.0)).len(), 72);
    }

    /// A global overview at `per_deg` nodes per degree, from a function of (lat, lon).
    pub(crate) fn overview_from(per_deg: u32, f: impl Fn(f64, f64) -> (f32, u8)) -> Grid {
        let (h, w) = (180 * per_deg + 1, 360 * per_deg + 1);
        let mut data = Vec::with_capacity((h * w * 4) as usize);
        for i in 0..h {
            for j in 0..w {
                let lat = 90.0 - f64::from(i) / f64::from(per_deg);
                let lon = -180.0 + f64::from(j) / f64::from(per_deg);
                let (e, k) = f(lat, if j == w - 1 { -180.0 } else { lon });
                data.extend_from_slice(&half::f16::from_f32(e).to_le_bytes());
                data.extend_from_slice(&half::f16::from_f32(f32::from(k)).to_le_bytes());
            }
        }
        Grid {
            kind: GridKind::LatLon,
            dtype: DType::F16,
            channels: 2,
            width: w,
            height: h,
            data,
        }
    }

    #[test]
    fn overview_shape_is_checked() {
        let mut set = TileSet::new();
        assert!(!set.has_overview() && set.overview_spacing_m().is_none());
        let mut bad = overview_from(1, |_, _| (0.0, 0));
        bad.width -= 1;
        assert_eq!(
            set.set_overview(bad),
            Err(TileError::BadOverview {
                width: 360,
                height: 181
            })
        );
        set.set_overview(overview_from(2, |_, _| (0.0, 0))).unwrap();
        let spacing = set.overview_spacing_m().unwrap();
        assert!((spacing - 55_597.0).abs() < 1.0, "{spacing}");
        // The bake's 5' grid: 2161 x 4321 and about 9.27 km.
        let per_deg = (2161 - 1) / 180;
        assert_eq!((180 * per_deg + 1, 360 * per_deg + 1), (2161, 4321));
    }

    #[test]
    fn overview_is_bilinear_and_wraps() {
        // Elevation linear in latitude, plus a longitude term that matches across 180.
        let f = |lat: f64, lon: f64| {
            (
                (lat * 4.0 + lon.abs()) as f32,
                if lat > 0.0 { 5 } else { 6 },
            )
        };
        let mut set = TileSet::new();
        set.set_overview(overview_from(4, f)).unwrap();
        let (e, k) = set.sample_overview(10.1, 20.05).unwrap();
        assert!((e - (10.1 * 4.0 + 20.05) as f32).abs() < 0.05, "{e}");
        assert_eq!(k, 5);
        assert_eq!(set.sample_overview(-30.0, 0.0).unwrap().1, 6);
        let west = set.sample_overview(3.0, 179.99).unwrap().0;
        let east = set.sample_overview(3.0, -179.99).unwrap().0;
        assert!((west - east).abs() < 0.1, "{west} {east}");
        assert!(
            set.sample_overview(90.0, 0.0).is_some() && set.sample_overview(-90.0, 7.0).is_some()
        );
    }

    #[test]
    fn lookup_reads_tiles_then_the_overview() {
        let mut set = TileSet::new();
        set.insert(16, 37, tile_from(16, 37, |_, _| (100.0, 3)))
            .unwrap(); // 5..10 N, 5..10 E
        assert_eq!(set.lookup(7.0, 7.0, SourceLevel::Tiles), Some((100.0, 3)));
        assert_eq!(set.lookup(7.0, 7.0, SourceLevel::Overview), None);
        assert_eq!(
            set.lookup(12.0, 7.0, SourceLevel::Tiles),
            None,
            "no tile, no overview"
        );
        set.set_overview(overview_from(1, |_, _| (300.0, 9)))
            .unwrap();
        assert_eq!(set.lookup(7.0, 7.0, SourceLevel::Tiles), Some((100.0, 3)));
        assert_eq!(
            set.lookup(7.0, 7.0, SourceLevel::Overview),
            Some((300.0, 9))
        );
        assert_eq!(set.lookup(12.0, 7.0, SourceLevel::Tiles), Some((300.0, 9)));
        // Missing inner tiles are what makes a region fall back.
        let needed = TileSet::needed_for(&frame_at(7.0, 7.0));
        assert!(
            set.missing(&needed).is_empty(),
            "{:?}",
            set.missing(&needed)
        );
        assert_eq!(TileSet::new().missing(&needed), needed);
    }

    #[test]
    fn rotated_coverage_follows_the_rotation() {
        // Reads carried 3 degrees east by a rotation about the pole land in the tiles 3 degrees
        // east, and every carried point of the square is covered.
        let frame = frame_at(-31.0, 116.0);
        let q = DQuat::from_rotation_y(3f64.to_radians());
        let needed = TileSet::needed_for_rotated(&frame, q);
        let half = REGION_SIZE_M / 2.0 + TILE_MARGIN_M * 0.99;
        for a in 0..=40 {
            for b in 0..=40 {
                let x = -half + 2.0 * half * f64::from(a) / 40.0;
                let z = -half + 2.0 * half * f64::from(b) / 40.0;
                let d = q * frame.local_to_dir(x, z).0;
                let (lat, lon) = geo_to_latlon(render_to_geo(RenderDir(d)));
                assert!(
                    needed.binary_search(&tile_of(lat, lon)).is_ok(),
                    "({lat},{lon})"
                );
            }
        }
        assert_eq!(
            TileSet::needed_for_rotated(&frame, DQuat::IDENTITY),
            TileSet::needed_for(&frame)
        );
        assert_ne!(needed, TileSet::needed_for(&frame));
        // A rotation that carries the field over a pole still takes whole rows only.
        let polar = TileSet::needed_for_rotated(
            &frame_at(60.0, 0.0),
            DQuat::from_rotation_x(-30f64.to_radians()),
        );
        assert!(polar.len() <= 80, "{}", polar.len());
    }
}
