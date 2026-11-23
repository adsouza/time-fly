//! Present-day elevation and climate tiles (ETOPO 2022 and Köppen-Geiger, baked by Task 1).
//!
//! Each tile is a 5° × 5° `LatLon` grid of 301 × 301 samples on the 60″ lattice, with both edges
//! included, so neighbouring tiles (also across the antimeridian) share their edge samples.
//! Channel 0 is elevation in metres, channel 1 the Köppen code (0 = ocean or no data, 1–30 =
//! Beck classes). Row 0 is the tile's north edge: sample `(i, j)` sits at
//! `lat = lat_n − i/60`, `lon = lon_w + j/60`, where tile `(row, col)` has `lat_n = 90 − 5·row`
//! and `lon_w = −180 + 5·col`.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::formats::{Grid, GridKind};
use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};
use crate::worldgen::frame::RegionFrame;

/// Degrees per tile side.
pub const TILE_DEG: f64 = 5.0;
/// Samples per tile side, both edges included.
pub const TILE_SAMPLES: u32 = 301;
/// Tile rows (north to south) and columns (west to east).
pub const TILE_ROWS: u8 = 36;
pub const TILE_COLS: u8 = 72;
/// Sample spacing of the tiles: 60″ of latitude, in metres.
pub const TILE_SPACING_M: f64 = 1852.0;
/// [`TileSet::needed_for`] widens the square by this much on every side, for the coastline
/// clamp's corner samples and the coarse lattice's overhang past the field's edge.
pub const TILE_MARGIN_M: f64 = 5_000.0;

/// Samples per degree.
const PER_DEG: f64 = (TILE_SAMPLES - 1) as f64 / TILE_DEG;

/// A tile grid that is not a 2-channel 301 × 301 `LatLon` grid, or a tile index off the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TileError {
    BadIndex { row: u8, col: u8 },
    BadShape { row: u8, col: u8 },
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

/// The decoded tiles a region needs, keyed by `(row, col)`. Tiles are shared (`Arc`), so a
/// caller's tile cache can hand them over without copying.
#[derive(Clone, Debug, Default)]
pub struct TileSet {
    tiles: HashMap<(u8, u8), Arc<Grid>>,
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

    /// Every tile a square field of side `size_m` around `frame`'s centre can read, widened by
    /// [`TILE_MARGIN_M`] on every side, sorted. The square's latitude and longitude extremes lie
    /// on its boundary unless it contains a pole, in which case it takes whole rows of tiles.
    pub fn needed_for(frame: &RegionFrame, size_m: f64) -> Vec<(u8, u8)> {
        let half = size_m / 2.0 + TILE_MARGIN_M;
        let basis = frame.basis();
        let (_, centre_lon) = geo_to_latlon(render_to_geo(RenderDir(basis.center)));
        // Boundary samples every ~1 km (at least 64 per edge).
        let steps = ((2.0 * half / 1000.0).ceil() as usize).max(64);
        let (mut lat_min, mut lat_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut rel_min, mut rel_max) = (f64::INFINITY, f64::NEG_INFINITY);
        for k in 0..=steps {
            let t = -half + 2.0 * half * k as f64 / steps as f64;
            for (x, z) in [(t, -half), (t, half), (-half, t), (half, t)] {
                let (lat, lon) = geo_to_latlon(render_to_geo(basis.local_to_dir(x, z)));
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
                .dir_to_local(pole)
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
        let (i0, j0) = (i.floor().min(last - 1.0), j.floor().min(last - 1.0));
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
        Some((elevation, code))
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
            for size in [
                crate::worldgen::REGION_SIZE_M,
                crate::worldgen::APRON_SIZE_M,
            ] {
                let frame = frame_at(lat, lon);
                let needed = TileSet::needed_for(&frame, size);
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
        assert!(
            TileSet::needed_for(&frame_at(37.0, -122.0), crate::worldgen::REGION_SIZE_M).len() <= 4
        );
    }
}
