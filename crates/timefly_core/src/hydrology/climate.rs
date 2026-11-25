//! The era's climate per lattice node, and the owner's table that turns it into runoff and lake
//! evaporation (rivers and lakes design §4.1, "Input").
//!
//! - Up to [`CLIMATE_MAX_KA`]: Beyer et al. 2020 (BIO1 and BIO12 on a 1° grid, baked by Task 1),
//!   bilinear in space and linear in age ([`BeyerClimate`], [`climate_bracket`]).
//! - Beyond it, or where Beyer has no value: the deep biome zonal rule, each biome mapped to a
//!   typical climate by the table ([`ZonalClimate`]).
//!
//! The table lives in `data/hydrology/climate_table.json`; the shell parses it into a
//! [`ClimateTable`] (`biome_climate` is keyed by the [`BiomeClass`] names in [`BIOME_ORDER`]).

use super::params::CLIMATE_MAX_KA;
use crate::deep_time::YearsBp;
use crate::formats::{Grid, GridKind};
use crate::geo::{RenderDir, geo_to_latlon, render_to_geo};
use crate::surface::bracket;
use crate::worldgen::biome::{BiomeClass, zonal_biome};

/// Seconds in a Julian year: converts mm/yr over an area to m³/s.
pub const SECONDS_PER_YEAR: f64 = 365.25 * 86_400.0;

/// The rows of [`ClimateTable::biome_climate`], in [`BiomeClass`] discriminant order. The JSON
/// table keys them by these variants' names.
pub const BIOME_ORDER: [BiomeClass; 12] = [
    BiomeClass::TropicalRainforest,
    BiomeClass::MonsoonSavanna,
    BiomeClass::HotDesert,
    BiomeClass::ColdDesert,
    BiomeClass::Steppe,
    BiomeClass::Mediterranean,
    BiomeClass::TemperateBroadleaf,
    BiomeClass::TemperateRainforest,
    BiomeClass::Boreal,
    BiomeClass::Tundra,
    BiomeClass::IceCap,
    BiomeClass::Alpine,
];

/// Annual precipitation (BIO12) and annual mean temperature (BIO1) at a node.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Climate {
    pub precip_mm: f32,
    pub temp_c: f32,
}

/// The owner's tuning table. Breakpoint lists are `(°C, mm/yr)`, ascending in temperature,
/// linear between breakpoints and clamped at the ends.
#[derive(Clone, Debug, PartialEq)]
pub struct ClimateTable {
    /// Potential evapotranspiration of land.
    pub pet_mm: Vec<(f32, f32)>,
    /// Evaporation from an open water surface.
    pub lake_evaporation_mm: Vec<(f32, f32)>,
    /// The Turc-Pike exponent `n`.
    pub turc_pike_n: f32,
    /// Runoff from an ice node (melt), mm/yr.
    pub ice_melt_runoff_mm: f32,
    /// A typical climate per biome, indexed by `BiomeClass as usize` ([`BIOME_ORDER`]).
    pub biome_climate: [Climate; 12],
}

/// Piecewise linear through `points`, clamped at the ends. 0 for an empty table; NaN for NaN.
fn piecewise(points: &[(f32, f32)], x: f32) -> f64 {
    let (Some(&(x0, y0)), Some(&(x1, y1))) = (points.first(), points.last()) else {
        return 0.0;
    };
    let x = f64::from(x);
    if x.is_nan() {
        return f64::NAN;
    }
    if x <= f64::from(x0) {
        return f64::from(y0);
    }
    if x >= f64::from(x1) {
        return f64::from(y1);
    }
    let k = points.partition_point(|&(px, _)| f64::from(px) <= x);
    let ((ax, ay), (bx, by)) = (points[k - 1], points[k]);
    let (ax, ay, bx, by) = (f64::from(ax), f64::from(ay), f64::from(bx), f64::from(by));
    ay + (by - ay) * (x - ax) / (bx - ax)
}

impl ClimateTable {
    /// Potential evapotranspiration (mm/yr) at `temp_c`.
    pub fn pet_at(&self, temp_c: f32) -> f64 {
        piecewise(&self.pet_mm, temp_c)
    }

    /// Open-water evaporation (mm/yr) at `temp_c`.
    pub fn lake_evaporation_at(&self, temp_c: f32) -> f64 {
        piecewise(&self.lake_evaporation_mm, temp_c)
    }

    /// Turc-Pike runoff (mm/yr): `P − P / (1 + (P/PET)^n)^(1/n)`, or `P` when PET is 0.
    /// Negative precipitation counts as none.
    pub fn runoff_mm(&self, c: Climate) -> f32 {
        let p = f64::from(c.precip_mm).max(0.0);
        if p == 0.0 {
            return 0.0;
        }
        let pet = self.pet_at(c.temp_c);
        if pet <= 0.0 {
            return p as f32;
        }
        let n = f64::from(self.turc_pike_n);
        let actual = p / (1.0 + (p / pet).powf(n)).powf(1.0 / n);
        (p - actual).max(0.0) as f32
    }

    /// Net evaporation from a lake surface (mm/yr): `max(0, E(T) − P)`.
    pub fn net_evaporation_mm(&self, c: Climate) -> f32 {
        (self.lake_evaporation_at(c.temp_c) - f64::from(c.precip_mm)).max(0.0) as f32
    }

    /// The table's typical climate for `biome`.
    pub fn biome(&self, biome: BiomeClass) -> Climate {
        self.biome_climate[biome as usize]
    }
}

/// The climate at a lattice node.
pub trait ClimateSource: Sync {
    /// The climate at `dir` (era frame), for a node at `elevation_m` (relative to the era's sea
    /// level) and `coast_km` from the ocean. `coast_km` is 0 unless [`needs_coast_km`] is true.
    ///
    /// [`needs_coast_km`]: ClimateSource::needs_coast_km
    fn climate(&self, dir: RenderDir, elevation_m: f32, coast_km: f32) -> Climate;

    /// Whether the pass must compute each node's distance to the coast first.
    fn needs_coast_km(&self) -> bool {
        false
    }
}

/// The deep zonal rule ([`zonal_biome`]) mapped through the table's biome rows.
#[derive(Copy, Clone, Debug)]
pub struct ZonalClimate<'a> {
    pub table: &'a ClimateTable,
    /// The sea-level anomaly the era's heights carry, for the snowline.
    pub sea_level_anomaly_m: f64,
}

impl ClimateSource for ZonalClimate<'_> {
    fn climate(&self, dir: RenderDir, elevation_m: f32, coast_km: f32) -> Climate {
        let paleolat = dir.0.normalize().y.clamp(-1.0, 1.0).asin().to_degrees();
        let biome = zonal_biome(
            paleolat,
            f64::from(elevation_m),
            f64::from(coast_km),
            self.sea_level_anomaly_m,
        );
        self.table.biome(biome)
    }

    fn needs_coast_km(&self) -> bool {
        true
    }
}

/// Rows and columns of a baked climate grid (Task 1): 1°, row 0 at 89.5° N, column 0 at
/// 179.5° W; channel 0 BIO1 (°C), channel 1 BIO12 (mm/yr); NaN where Beyer has no land value.
pub const CLIMATE_GRID_ROWS: u32 = 180;
pub const CLIMATE_GRID_COLS: u32 = 360;

/// Whether `grid` has the baked climate grid's shape.
pub fn is_climate_grid(grid: &Grid) -> bool {
    grid.kind == GridKind::LatLon
        && grid.height == CLIMATE_GRID_ROWS
        && grid.width == CLIMATE_GRID_COLS
        && grid.channels >= 2
        && grid.data.len()
            >= (grid.height as usize) * (grid.width as usize) * (grid.channels as usize) * 2
}

/// Beyer et al. 2020 between two baked ages: bilinear on the 1° grid, then linear in age. A tap
/// with no value (NaN) takes the zonal climate at the node instead, so a coast or an exposed
/// shelf blends smoothly into the fallback.
#[derive(Copy, Clone, Debug)]
pub struct BeyerClimate<'a> {
    /// `[younger, older]`.
    grids: [&'a Grid; 2],
    older_weight: f64,
    zonal: ZonalClimate<'a>,
}

impl<'a> BeyerClimate<'a> {
    /// Panics unless both grids pass [`is_climate_grid`]: the shell checks them first and falls
    /// back to [`ZonalClimate`].
    pub fn new(grids: [&'a Grid; 2], older_weight: f64, zonal: ZonalClimate<'a>) -> Self {
        assert!(
            grids.iter().all(|g| is_climate_grid(g)),
            "not a baked climate grid"
        );
        Self {
            grids,
            older_weight,
            zonal,
        }
    }
}

/// The four 1° cells around a latitude and longitude, `(row, col, weight)`. Longitude wraps;
/// latitude clamps to the outermost rows.
fn climate_taps(lat_deg: f64, lon_deg: f64) -> [(u32, u32, f64); 4] {
    let last_row = f64::from(CLIMATE_GRID_ROWS - 1);
    let cols = f64::from(CLIMATE_GRID_COLS);
    let y = (89.5 - lat_deg).clamp(0.0, last_row);
    let r0 = y.floor().min(last_row - 1.0);
    let fy = y - r0;
    let mut x = (lon_deg + 179.5).rem_euclid(cols);
    if !(0.0..cols).contains(&x) {
        x = 0.0;
    }
    let c0 = x.floor();
    let fx = x - c0;
    let (r0, c0) = (r0 as u32, c0 as u32);
    let c1 = (c0 + 1) % CLIMATE_GRID_COLS;
    [
        (r0, c0, (1.0 - fy) * (1.0 - fx)),
        (r0, c1, (1.0 - fy) * fx),
        (r0 + 1, c0, fy * (1.0 - fx)),
        (r0 + 1, c1, fy * fx),
    ]
}

impl ClimateSource for BeyerClimate<'_> {
    fn climate(&self, dir: RenderDir, elevation_m: f32, coast_km: f32) -> Climate {
        let (lat, lon) = geo_to_latlon(render_to_geo(dir));
        let taps = climate_taps(lat, lon);
        let mut zonal: Option<Climate> = None;
        let mut sample = |grid: &Grid| {
            let (mut p, mut t) = (0.0f64, 0.0f64);
            for (row, col, w) in taps {
                if w == 0.0 {
                    continue;
                }
                let tc = grid.texel_f32(0, row, col, 0);
                let pm = grid.texel_f32(0, row, col, 1);
                let (tc, pm) = if tc.is_nan() || pm.is_nan() {
                    let z = *zonal
                        .get_or_insert_with(|| self.zonal.climate(dir, elevation_m, coast_km));
                    (z.temp_c, z.precip_mm)
                } else {
                    (tc, pm)
                };
                t += w * f64::from(tc);
                p += w * f64::from(pm);
            }
            (p, t)
        };
        let (mut p, mut t) = sample(self.grids[0]);
        let w = self.older_weight;
        if w != 0.0 {
            let (po, to) = sample(self.grids[1]);
            p += (po - p) * w;
            t += (to - t) * w;
        }
        Climate {
            precip_mm: p.max(0.0) as f32,
            temp_c: t as f32,
        }
    }
}

/// The baked ages around `age` (`ages_ka` ascending): `(younger, older, older weight)`, clamped
/// at the ends, or `None` past [`CLIMATE_MAX_KA`] (or for a NaN age), where the zonal rule
/// applies.
pub fn climate_bracket(ages_ka: &[f64], age: YearsBp) -> Option<(usize, usize, f64)> {
    let ka = age.0 / 1000.0;
    if ages_ka.is_empty() || ka.is_nan() || ka > CLIMATE_MAX_KA {
        return None;
    }
    let b = bracket(ages_ka, ka);
    Some((b.younger, b.older, b.older_weight))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::formats::DType;
    use crate::worldgen::testkit::dir;
    use serde_json::Value;

    const TABLE: &str = include_str!("../../../../data/hydrology/climate_table.json");

    /// The shipped table, parsed the way the shell does.
    pub(crate) fn shipped_table() -> ClimateTable {
        let v: Value = serde_json::from_str(TABLE).unwrap();
        let points = |key: &str| -> Vec<(f32, f32)> {
            v[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| (p[0].as_f64().unwrap() as f32, p[1].as_f64().unwrap() as f32))
                .collect()
        };
        let biome_climate = BIOME_ORDER.map(|b| {
            let row = &v["biome_climate"][format!("{b:?}")];
            Climate {
                precip_mm: row[0].as_f64().unwrap() as f32,
                temp_c: row[1].as_f64().unwrap() as f32,
            }
        });
        ClimateTable {
            pet_mm: points("pet_mm"),
            lake_evaporation_mm: points("lake_evaporation_mm"),
            turc_pike_n: v["turc_pike_n"].as_f64().unwrap() as f32,
            ice_melt_runoff_mm: v["ice_melt_runoff_mm"].as_f64().unwrap() as f32,
            biome_climate,
        }
    }

    fn c(precip_mm: f32, temp_c: f32) -> Climate {
        Climate { precip_mm, temp_c }
    }

    #[test]
    fn biome_order_is_the_discriminant_order() {
        for (k, b) in BIOME_ORDER.iter().enumerate() {
            assert_eq!(*b as usize, k);
        }
    }

    #[test]
    fn the_shipped_table_turns_climates_into_runoff() {
        let t = shipped_table();
        assert_eq!(t.turc_pike_n, 2.0);
        assert_eq!(t.ice_melt_runoff_mm, 300.0);
        assert_eq!(t.pet_mm.len(), 6);
        assert_eq!(t.biome(BiomeClass::HotDesert), c(60.0, 26.0));
        assert_eq!(t.biome(BiomeClass::Boreal), c(550.0, -2.0));
        let sahara = t.runoff_mm(c(30.0, 28.0));
        let amazon = t.runoff_mm(c(2300.0, 26.0));
        let indus = t.runoff_mm(c(1000.0, 5.0));
        println!("Sahara {sahara} mm, Amazon {amazon} mm, upper Indus {indus} mm");
        assert!((0.0..1.0).contains(&sahara), "{sahara}");
        assert!((1000.0..=1200.0).contains(&amazon), "{amazon}");
        assert!((450.0..=700.0).contains(&indus), "{indus}");
        for temp in [-30.0, -20.0, -3.5, 0.0, 12.7, 26.0, 35.0] {
            let mut last = -1.0f32;
            for p in 0..=6000 {
                let r = t.runoff_mm(c(p as f32, temp));
                assert!(r >= last, "{p} mm at {temp} C: {r} < {last}");
                assert!(r <= p as f32 + 1e-3);
                last = r;
            }
        }
        // Below -20 C PET is 0: everything runs off.
        assert_eq!(t.runoff_mm(c(200.0, -25.0)), 200.0);
        assert_eq!(t.runoff_mm(c(-5.0, 10.0)), 0.0);
        // Net evaporation: E(26) = 1,920 mm, clamped and never negative.
        assert!((t.net_evaporation_mm(c(60.0, 26.0)) - 1860.0).abs() < 1e-3);
        assert_eq!(t.net_evaporation_mm(c(2500.0, 26.0)), 0.0);
        assert_eq!(t.lake_evaporation_at(45.0), 2200.0);
        assert_eq!(t.lake_evaporation_at(-40.0), 50.0);
        assert!((t.pet_at(5.0) - 475.0).abs() < 1e-9);
        // A NaN temperature reads NaN from the table, and gives no runoff or evaporation
        // rather than a panic.
        assert!(t.pet_at(f32::NAN).is_nan());
        assert_eq!(t.runoff_mm(c(500.0, f32::NAN)), 0.0);
        assert_eq!(t.net_evaporation_mm(c(500.0, f32::NAN)), 0.0);
    }

    /// A climate grid filled by `f(row, col) -> (temp, precip)`.
    pub(crate) fn climate_grid(f: impl Fn(u32, u32) -> (f32, f32)) -> Grid {
        let mut data = Vec::new();
        for row in 0..CLIMATE_GRID_ROWS {
            for col in 0..CLIMATE_GRID_COLS {
                let (t, p) = f(row, col);
                data.extend_from_slice(&half::f16::from_f32(t).to_le_bytes());
                data.extend_from_slice(&half::f16::from_f32(p).to_le_bytes());
            }
        }
        Grid {
            kind: GridKind::LatLon,
            dtype: DType::F16,
            channels: 2,
            width: CLIMATE_GRID_COLS,
            height: CLIMATE_GRID_ROWS,
            data,
        }
    }

    #[test]
    fn climate_interpolates_between_baked_ages() {
        let ages = [
            0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 12.0, 21.0, 26.0, 110.0, 120.0,
        ];
        let (y, o, w) = climate_bracket(&ages, YearsBp(8_250.0)).unwrap();
        assert_eq!((ages[y], ages[o]), (8.0, 9.0));
        assert!((w - 0.25).abs() < 1e-12, "{w}");
        assert_eq!(climate_bracket(&ages, YearsBp(130_000.0)), None);
        assert_eq!(climate_bracket(&ages, YearsBp(f64::NAN)), None);
        assert_eq!(
            climate_bracket(&ages, YearsBp(120_000.0)),
            Some((15, 15, 0.0))
        );
        assert_eq!(climate_bracket(&ages, YearsBp(0.0)), Some((0, 0, 0.0)));
        assert_eq!(
            climate_bracket(&[0.0, 10.0], YearsBp(60_000.0)),
            Some((1, 1, 0.0))
        );

        // 8 ka: 10 C, 1,000 mm everywhere; 9 ka: 14 C, 2,000 mm. 8.25 ka blends 0.75 / 0.25.
        let table = shipped_table();
        let zonal = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        let (young, old) = (
            climate_grid(|_, _| (10.0, 1000.0)),
            climate_grid(|_, _| (14.0, 2000.0)),
        );
        let beyer = BeyerClimate::new([&young, &old], w, zonal);
        let got = beyer.climate(dir(12.3, -71.9), 200.0, 0.0);
        assert!((got.precip_mm - 1250.0).abs() < 1e-3, "{got:?}");
        assert!((got.temp_c - 11.0).abs() < 1e-5, "{got:?}");
    }

    #[test]
    fn beyer_is_bilinear_and_falls_back_to_zonal_where_it_has_no_value() {
        let table = shipped_table();
        let zonal = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        // Precipitation = 10 x column + row, temperature = row: bilinear reproduces both at any
        // point away from the wrap and the poles.
        let g = climate_grid(|r, col| (r as f32, (10 * col + r) as f32));
        let beyer = BeyerClimate::new([&g, &g], 0.0, zonal);
        let (lat, lon) = (12.3, -71.9);
        let got = beyer.climate(dir(lat, lon), 0.0, 0.0);
        let (y, x) = (89.5 - lat, lon + 179.5);
        assert!((f64::from(got.temp_c) - y).abs() < 0.05, "{got:?}");
        assert!(
            (f64::from(got.precip_mm) - (10.0 * x + y)).abs() < 0.5,
            "{got:?}"
        );
        // Across the date line, columns 359 and 0 blend.
        let g = climate_grid(|_, col| (0.0, if col == 0 { 100.0 } else { 0.0 }));
        let beyer = BeyerClimate::new([&g, &g], 0.0, zonal);
        let east = beyer.climate(dir(0.0, 180.0), 0.0, 0.0);
        assert!((east.precip_mm - 50.0).abs() < 1e-3, "{east:?}");

        // A NaN cell: each tap on it takes the zonal value at the node, by its weight.
        let at = (2.0, 10.0); // lat 2: TropicalRainforest within 1,500 km of the coast
        let (row, col) = ((89.5 - at.0) as u32, (at.1 + 179.5) as u32);
        let g = climate_grid(|r, cc| {
            if (r, cc) == (row, col) {
                (f32::NAN, f32::NAN)
            } else {
                (20.0, 100.0)
            }
        });
        let beyer = BeyerClimate::new([&g, &g], 0.0, zonal);
        let d = dir(at.0, at.1);
        let z = zonal.climate(d, 100.0, 50.0);
        assert_eq!(z, table.biome(BiomeClass::TropicalRainforest));
        let got = beyer.climate(d, 100.0, 50.0);
        let taps = climate_taps(at.0, at.1);
        let wn: f64 = taps
            .iter()
            .filter(|&&(r, cc, _)| (r, cc) == (row, col))
            .map(|t| t.2)
            .sum();
        assert!(wn > 0.2, "the hazard: the NaN cell carries weight {wn}");
        let want = 100.0 * (1.0 - wn) + f64::from(z.precip_mm) * wn;
        assert!(
            (f64::from(got.precip_mm) - want).abs() < 0.05,
            "{got:?} vs {want}"
        );
        assert!(got.precip_mm.is_finite() && got.temp_c.is_finite());
    }

    #[test]
    fn zonal_climate_follows_the_biome_rule() {
        let table = shipped_table();
        let zonal = ZonalClimate {
            table: &table,
            sea_level_anomaly_m: 0.0,
        };
        assert!(zonal.needs_coast_km());
        // 20 degrees, 800 km inland: hot desert. Near the coast: monsoon savanna.
        assert_eq!(
            zonal.climate(dir(20.0, 40.0), 300.0, 800.0),
            table.biome(BiomeClass::HotDesert)
        );
        assert_eq!(
            zonal.climate(dir(-20.0, 40.0), 300.0, 100.0),
            table.biome(BiomeClass::MonsoonSavanna)
        );
        assert_eq!(
            zonal.climate(dir(80.0, 0.0), 6000.0, 100.0),
            table.biome(BiomeClass::IceCap)
        );
    }
}
