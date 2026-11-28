//! The present-day lake map (rivers Task 8b, owner ruling 2026-11-25 "check against today"):
//! Natural Earth 1:10m lakes, reservoirs dropped, rasterised at cell centres onto a 3′
//! equirectangular grid, with each lake's surface level from the median of the ETOPO 2022 60″
//! cells inside it. Baked by `tools/bake` (`timefly_bake.lakemap`, whose docstring gives the
//! format) to `data/hydrology/lake_map.bin` and compiled in; Task 11's Python reads the same file.
//!
//! The pass reads it through [`NodeSource::mapped_surface_m`](super::NodeSource), only where the
//! terrain is today's ([`NodeSource::data_lakes`](super::NodeSource)).

use std::sync::OnceLock;

/// The shipped map's bytes.
pub const LAKE_MAP: &[u8] = include_bytes!("../../../../data/hydrology/lake_map.bin");

const MAGIC: &[u8; 4] = b"TFLM";
const VERSION: u32 = 1;
const HEADER_BYTES: usize = 20;
const RUN_BYTES: usize = 6;

/// A decoded lake map: per row, runs of cells `[start, end)` holding one lake.
#[derive(Clone, Debug, PartialEq)]
pub struct LakeMap {
    width: u32,
    height: u32,
    /// Lake id `k + 1` has `levels[k]` (m above present sea level).
    levels: Vec<f32>,
    /// Row `r`'s runs are `runs[row_start[r]..row_start[r + 1]]`.
    row_start: Vec<u32>,
    /// `(col_start, col_end exclusive, lake id)`, ascending `col_start` within a row.
    runs: Vec<(u16, u16, u16)>,
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().expect("four bytes")))
        .ok_or_else(|| format!("truncated at byte {at}"))
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

impl LakeMap {
    /// Decodes a lake map file, checking its structure.
    pub fn parse(bytes: &[u8]) -> Result<LakeMap, String> {
        if bytes.get(..4) != Some(MAGIC.as_slice()) {
            return Err("not a lake map (bad magic)".into());
        }
        let version = u32_at(bytes, 4)?;
        if version != VERSION {
            return Err(format!("lake map version {version}, expected {VERSION}"));
        }
        let (width, height, lakes) = (u32_at(bytes, 8)?, u32_at(bytes, 12)?, u32_at(bytes, 16)?);
        if width == 0 || width > 65_535 || height == 0 || lakes > 65_535 {
            return Err(format!("bad size {width} x {height}, {lakes} lakes"));
        }
        let mut at = HEADER_BYTES;
        let levels = (0..lakes as usize)
            .map(|k| u32_at(bytes, at + 4 * k).map(f32::from_bits))
            .collect::<Result<Vec<f32>, _>>()?;
        at += 4 * lakes as usize;
        let row_start = (0..=height as usize)
            .map(|r| u32_at(bytes, at + 4 * r))
            .collect::<Result<Vec<u32>, _>>()?;
        at += 4 * (height as usize + 1);
        let n = *row_start.last().expect("height + 1 entries") as usize;
        if row_start[0] != 0 || row_start.windows(2).any(|w| w[0] > w[1]) {
            return Err("row starts do not ascend from 0".into());
        }
        if bytes.len() != at + RUN_BYTES * n {
            return Err(format!(
                "{} bytes, expected {} for {n} runs",
                bytes.len(),
                at + RUN_BYTES * n
            ));
        }
        let runs: Vec<(u16, u16, u16)> = (0..n)
            .map(|k| {
                let p = at + RUN_BYTES * k;
                (u16_at(bytes, p), u16_at(bytes, p + 2), u16_at(bytes, p + 4))
            })
            .collect();
        for r in 0..height as usize {
            let row = &runs[row_start[r] as usize..row_start[r + 1] as usize];
            let mut last_end = 0;
            for &(s, e, id) in row {
                if s < last_end
                    || e <= s
                    || u32::from(e) > width
                    || id == 0
                    || u32::from(id) > lakes
                {
                    return Err(format!("bad run ({s}, {e}, {id}) in row {r}"));
                }
                last_end = e;
            }
        }
        Ok(LakeMap {
            width,
            height,
            levels,
            row_start,
            runs,
        })
    }

    /// The number of lakes.
    pub fn lakes(&self) -> usize {
        self.levels.len()
    }

    /// The lake id (1-based) of the cell holding (lat, lon) degrees, if a lake holds it:
    /// a binary search of the row's runs.
    pub fn lake_at(&self, lat: f64, lon: f64) -> Option<u16> {
        let step = 360.0 / f64::from(self.width);
        let row = ((90.0 - lat) / step)
            .floor()
            .clamp(0.0, f64::from(self.height - 1)) as usize;
        let col = ((lon + 180.0) / step)
            .floor()
            .rem_euclid(f64::from(self.width)) as u32;
        let col = col.min(self.width - 1) as u16;
        let runs = &self.runs[self.row_start[row] as usize..self.row_start[row + 1] as usize];
        let k = runs.partition_point(|&(s, _, _)| s <= col);
        let &(_, end, id) = runs.get(k.checked_sub(1)?)?;
        (col < end).then_some(id)
    }

    /// Lake `id`'s surface level (m above present sea level). Panics on an id the map lacks.
    pub fn level(&self, id: u16) -> f32 {
        self.levels[usize::from(id) - 1]
    }

    /// The surface level (m above present sea level) of the lake holding (lat, lon), if any.
    pub fn level_at(&self, lat: f64, lon: f64) -> Option<f32> {
        self.lake_at(lat, lon)
            .map(|id| self.levels[usize::from(id) - 1])
    }

    /// The lake id at cell (`row`, `col`), if a lake holds it.
    fn id_at_cell(&self, row: usize, col: u32) -> Option<u16> {
        let runs = &self.runs[self.row_start[row] as usize..self.row_start[row + 1] as usize];
        let col = col as u16;
        let k = runs.partition_point(|&(s, _, _)| s <= col);
        let &(_, end, id) = runs.get(k.checked_sub(1)?)?;
        (col < end).then_some(id)
    }

    /// Cell (`row`, `col`)'s centre (lat, lon) in degrees.
    fn cell_centre(&self, row: usize, col: u32) -> (f64, f64) {
        let step = 360.0 / f64::from(self.width);
        (
            90.0 - (row as f64 + 0.5) * step,
            -180.0 + (f64::from(col) + 0.5) * step,
        )
    }

    /// Each lake's footprint and its shoreline ring (Task 11d fix round 5, the map's audit): its
    /// cell count, the mean of its cells' centres, and the cells outside every lake that touch
    /// it (8-neighbours, longitudes wrapping), as centres, in row and column order. Lake `id`
    /// is entry `id − 1`.
    pub fn rings(&self) -> Vec<LakeRing> {
        let n = self.levels.len();
        let mut cells = vec![0u32; n];
        let mut sum = vec![(0.0f64, 0.0f64); n];
        let mut ring: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
        let w = self.width;
        for r in 0..self.height as usize {
            let runs = &self.runs[self.row_start[r] as usize..self.row_start[r + 1] as usize];
            for &(s, e, id) in runs {
                let k = usize::from(id) - 1;
                for c in u32::from(s)..u32::from(e) {
                    let (lat, lon) = self.cell_centre(r, c);
                    cells[k] += 1;
                    sum[k].0 += lat;
                    sum[k].1 += lon;
                }
                let (s, e) = (u32::from(s), u32::from(e));
                let rows = r.saturating_sub(1)..=(r + 1).min(self.height as usize - 1);
                for rr in rows {
                    // The run's own row needs only its two ends; the rows beside it, its span
                    // and a cell either side.
                    let cols: Vec<u32> = if rr == r {
                        vec![(s + w - 1) % w, e % w]
                    } else {
                        (s + w - 1..=e + w).map(|c| c % w).collect()
                    };
                    for c in cols {
                        if self.id_at_cell(rr, c).is_none() {
                            ring[k].push((rr as u32, c));
                        }
                    }
                }
            }
        }
        ring.iter_mut()
            .enumerate()
            .map(|(k, cs)| {
                cs.sort_unstable();
                cs.dedup();
                let m = f64::from(cells[k].max(1));
                LakeRing {
                    id: (k + 1) as u16,
                    cells: cells[k],
                    centre: (sum[k].0 / m, sum[k].1 / m),
                    ring: cs
                        .iter()
                        .map(|&(r, c)| self.cell_centre(r as usize, c))
                        .collect(),
                }
            })
            .collect()
    }

    /// The bytes the map holds, by capacity.
    pub fn bytes(&self) -> usize {
        4 * (self.levels.capacity() + self.row_start.capacity()) + RUN_BYTES * self.runs.capacity()
    }
}

/// One lake's footprint and shoreline ring ([`LakeMap::rings`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LakeRing {
    pub id: u16,
    /// The footprint's cells.
    pub cells: u32,
    /// The mean of its cells' centres (lat, lon) in degrees.
    pub centre: (f64, f64),
    /// The centres of the cells outside every lake that touch the footprint.
    pub ring: Vec<(f64, f64)>,
}

/// The shipped map, decoded once. Its tests guarantee it decodes.
pub fn shipped_lake_map() -> &'static LakeMap {
    static MAP: OnceLock<LakeMap> = OnceLock::new();
    MAP.get_or_init(|| LakeMap::parse(LAKE_MAP).expect("the shipped lake map decodes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_map_holds_todays_lakes_at_their_fine_surface() {
        let map = shipped_lake_map();
        assert!(LAKE_MAP.len() <= 512 * 1024, "{} bytes", LAKE_MAP.len());
        assert!(map.lakes() > 1000, "{} lakes", map.lakes());
        // Titicaca, Victoria and the Great Salt Lake; the fine data's level at each.
        for (lat, lon, level) in [
            (-15.8, -69.4, 3812.0),
            (-1.0, 33.0, 1133.0),
            (41.1, -112.5, 1278.0),
            // The Dead Sea at the fine surface, not the 10 km overview's -276 m.
            (31.5, 35.5, -427.0),
        ] {
            let got = map.level_at(lat, lon).expect("a lake there");
            assert!(
                (got - level).abs() <= 5.0,
                "({lat}, {lon}): {got} vs {level}"
            );
        }
        // Manaus (the Amazon's flats) and Skardu (the Indus valley) hold none.
        assert_eq!(map.level_at(-3.1, -60.0), None);
        assert_eq!(map.level_at(35.3, 75.6), None);
        // Longitudes wrap; the poles clamp.
        assert_eq!(map.lake_at(-1.0, 33.0), map.lake_at(-1.0, 33.0 - 360.0));
        assert_eq!(map.lake_at(90.0, 0.0), None);
        assert_eq!(map.lake_at(-90.0, 180.0), None);
    }

    /// A 4 x 2 map with lakes 1 (level 10) and 2 (level -5).
    fn tiny() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"TFLM");
        for x in [1u32, 4, 2, 2] {
            b.extend_from_slice(&x.to_le_bytes());
        }
        for l in [10.0f32, -5.0] {
            b.extend_from_slice(&l.to_le_bytes());
        }
        for s in [0u32, 2, 3] {
            b.extend_from_slice(&s.to_le_bytes());
        }
        // Row 0: cols 0 (lake 1) and 2..4 (lake 2); row 1: cols 1..3 (lake 1).
        for (s, e, id) in [(0u16, 1u16, 1u16), (2, 4, 2), (1, 3, 1)] {
            for v in [s, e, id] {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    /// Task 11d fix round 5: each lake's ring is the cells outside every lake that touch it,
    /// across the longitude seam.
    #[test]
    fn a_lakes_ring_is_the_land_around_its_footprint() {
        let map = LakeMap::parse(&tiny()).unwrap();
        let rings = map.rings();
        assert_eq!(rings.len(), 2);
        // Cells are 90 degrees; the only non-lake cells are (0, 1), (1, 0) and (1, 3).
        let free = vec![(45.0, -45.0), (-45.0, -135.0), (-45.0, 135.0)];
        assert_eq!((rings[0].id, rings[0].cells), (1, 3));
        assert_eq!(rings[0].ring, free);
        // Lake 2 (cols 2 and 3 of row 0) touches (1, 0) across the seam.
        assert_eq!((rings[1].id, rings[1].cells), (2, 2));
        assert_eq!(rings[1].ring, free);
        assert_eq!(rings[1].centre, (45.0, 90.0));
    }

    #[test]
    fn lookups_follow_the_runs_and_bad_files_are_errors() {
        let map = LakeMap::parse(&tiny()).unwrap();
        // Cells are 90 degrees: row 0 is the north half.
        let at = |lat, lon| map.level_at(lat, lon);
        assert_eq!(at(45.0, -135.0), Some(10.0));
        assert_eq!(at(45.0, -45.0), None);
        assert_eq!(at(45.0, 45.0), Some(-5.0));
        assert_eq!(at(45.0, 135.0), Some(-5.0));
        assert_eq!(at(-45.0, -135.0), None);
        assert_eq!(at(-45.0, -45.0), Some(10.0));
        assert_eq!(at(-45.0, 45.0), Some(10.0));
        assert_eq!(at(-45.0, 135.0), None);

        let good = tiny();
        assert!(LakeMap::parse(&good[..good.len() - 1]).is_err());
        let mut bad = good.clone();
        bad[0] = b'X';
        assert!(LakeMap::parse(&bad).is_err());
        // An id past the lake count, and overlapping runs.
        let mut bad = good.clone();
        let last = bad.len() - 2;
        bad[last] = 3;
        assert!(LakeMap::parse(&bad).unwrap_err().contains("bad run"));
        let mut bad = good;
        let second_start = bad.len() - 6 - 6;
        bad[second_start] = 0;
        assert!(LakeMap::parse(&bad).unwrap_err().contains("bad run"));
    }
}
