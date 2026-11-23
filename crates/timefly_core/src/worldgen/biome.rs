//! Biome classes, vegetation eras and surface palettes for region worlds (design spec §5.4,
//! parent spec §6.3 and §9).
//!
//! Present-day eras collapse Köppen-Geiger classes into twelve painterly biomes; deep eras use a
//! zonal rule on paleolatitude. The vegetation era says how much of the land is green, and the
//! palette table gives each `(era, biome)` pair its colours.

use crate::deep_time::YearsBp;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BiomeClass {
    TropicalRainforest,
    MonsoonSavanna,
    HotDesert,
    ColdDesert,
    Steppe,
    Mediterranean,
    TemperateBroadleaf,
    TemperateRainforest,
    Boreal,
    Tundra,
    IceCap,
    Alpine,
}

/// Collapses a Beck et al. (2018) Köppen-Geiger code (1 = Af ... 30 = EF) to a biome class.
/// Code 0 (ocean or no data) and anything above 30 give `None`.
pub fn biome_from_koppen(code: u8) -> Option<BiomeClass> {
    use BiomeClass::*;
    Some(match code {
        1 => TropicalRainforest,        // Af
        2 | 3 => MonsoonSavanna,        // Am, Aw
        4 => HotDesert,                 // BWh
        5 => ColdDesert,                // BWk
        6 | 7 => Steppe,                // BSh, BSk
        8..=10 => Mediterranean,        // Csa, Csb, Csc
        11..=14 => TemperateBroadleaf,  // Cwa, Cwb, Cwc, Cfa
        15 | 16 => TemperateRainforest, // Cfb, Cfc
        17 | 18 => TemperateBroadleaf,  // Dsa, Dsb
        19 | 20 => Boreal,              // Dsc, Dsd
        21 | 22 => TemperateBroadleaf,  // Dwa, Dwb
        23 | 24 => Boreal,              // Dwc, Dwd
        25 | 26 => TemperateBroadleaf,  // Dfa, Dfb
        27 | 28 => Boreal,              // Dfc, Dfd
        29 => Tundra,                   // ET
        30 => IceCap,                   // EF
        _ => return None,
    })
}

/// Most the snowline drops in a glacial era (m).
pub const MAX_GLACIAL_SNOWLINE_DROP_M: f64 = 1200.0;
/// The snowline drop per metre of negative sea-level anomaly: 1,000 m at the Last Glacial
/// Maximum's -120 m.
pub const SNOWLINE_DROP_PER_ANOMALY_M: f64 = 1000.0 / 120.0;

/// Height of the permanent snowline: 5,500 m up to 20 degrees of latitude, then 70 m lower per
/// degree, lowered in glacial eras, never below sea level.
///
/// Ruling R18: ICE-6G's 1-degree grid misses mountain glaciers (the Alps at 21 ka have zero ice
/// fraction), so a glacial era lowers the snowline by `1000 m × (anomaly / −120 m)`, clamped to
/// `0..=1,200 m`: no change for a positive anomaly. `sea_level_anomaly_m` is the era's (0 in deep
/// eras). Mirrored by `snowline_m` in `snowline.wesl`.
pub fn snowline_m(lat_deg: f64, sea_level_anomaly_m: f64) -> f64 {
    let drop = (-sea_level_anomaly_m * SNOWLINE_DROP_PER_ANOMALY_M)
        .clamp(0.0, MAX_GLACIAL_SNOWLINE_DROP_M);
    (5500.0 - 70.0 * (lat_deg.abs() - 20.0).max(0.0) - drop).max(0.0)
}

/// The zonal rule for deep time, from paleolatitude (degrees), effective elevation (m),
/// distance to the coast (km) and the era's sea-level anomaly (m, for [`snowline_m`]; 0 in deep
/// eras). Bands are `[lower, upper)` in |latitude|; a distance threshold belongs to its wetter
/// side ("within 500 km" includes 500).
pub fn zonal_biome(
    paleolat_deg: f64,
    elevation_m: f64,
    coast_distance_km: f64,
    sea_level_anomaly_m: f64,
) -> BiomeClass {
    let lat = paleolat_deg.abs();
    if elevation_m > snowline_m(lat, sea_level_anomaly_m) {
        return if lat > 60.0 {
            BiomeClass::IceCap
        } else {
            BiomeClass::Alpine
        };
    }
    let coast = coast_distance_km;
    if lat < 10.0 {
        if coast <= 1500.0 {
            BiomeClass::TropicalRainforest
        } else {
            BiomeClass::MonsoonSavanna
        }
    } else if lat < 30.0 {
        if coast > 500.0 {
            BiomeClass::HotDesert
        } else {
            BiomeClass::MonsoonSavanna
        }
    } else if lat < 45.0 {
        if coast <= 500.0 {
            BiomeClass::Mediterranean
        } else {
            BiomeClass::Steppe
        }
    } else if lat < 60.0 {
        if coast <= 1000.0 {
            BiomeClass::TemperateBroadleaf
        } else {
            BiomeClass::Steppe
        }
    } else if lat < 70.0 {
        BiomeClass::Boreal
    } else {
        BiomeClass::Tundra
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum VegetationEra {
    Barren,
    EarlyPlants,
    FirstForests,
    ConiferAge,
    FloweringPlants,
    Modern,
}

/// The era whose half-open age range `[young, old)` (Ma) contains `age`.
pub fn vegetation_era(age: YearsBp) -> VegetationEra {
    let ma = age.myr();
    if ma < 20.0 {
        VegetationEra::Modern
    } else if ma < 130.0 {
        VegetationEra::FloweringPlants
    } else if ma < 300.0 {
        VegetationEra::ConiferAge
    } else if ma < 385.0 {
        VegetationEra::FirstForests
    } else if ma < 470.0 {
        VegetationEra::EarlyPlants
    } else {
        VegetationEra::Barren
    }
}

/// Grasslands spread only in the last 20 Myr, so older eras swap steppe for shrubland and
/// savanna for open woodland.
pub fn effective_biome(era: VegetationEra, biome: BiomeClass) -> BiomeClass {
    if era == VegetationEra::Modern {
        return biome;
    }
    match biome {
        BiomeClass::Steppe => BiomeClass::Mediterranean,
        BiomeClass::MonsoonSavanna => BiomeClass::TemperateBroadleaf,
        other => other,
    }
}

/// Surface colours in linear RGB.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SurfacePalette {
    /// Soil, sand, snow: what shows between plants.
    pub ground: [f32; 3],
    /// Vegetation. Equal to `ground` where nothing grows.
    pub canopy: [f32; 3],
    /// Exposed rock on steep slopes.
    pub rock: [f32; 3],
}

type Srgb = [u8; 3];

/// Per-biome colours of a vegetated land, as sRGB bytes `(ground, canopy, rock)`, indexed by
/// `BiomeClass as usize`. Painterly naturalism: real biome colours (photos of Amazonia, the
/// Serengeti, the Sahara, the Great Plains, the Mediterranean, an English oak wood, the Pacific
/// Northwest, taiga, tundra, ice sheets and the Alps), slightly simplified and warmed.
const VEGETATED: [(Srgb, Srgb, Srgb); 12] = [
    ([92, 84, 52], [36, 96, 40], [110, 98, 88]), // TropicalRainforest
    ([176, 150, 82], [118, 132, 58], [140, 112, 84]), // MonsoonSavanna
    ([214, 176, 118], [160, 150, 86], [178, 122, 84]), // HotDesert
    ([172, 152, 128], [128, 122, 90], [132, 120, 112]), // ColdDesert
    ([186, 166, 100], [140, 150, 76], [150, 128, 100]), // Steppe
    ([170, 140, 96], [104, 112, 62], [156, 130, 104]), // Mediterranean
    ([96, 84, 56], [70, 112, 48], [116, 104, 92]), // TemperateBroadleaf
    ([70, 70, 48], [36, 86, 52], [96, 96, 90]),  // TemperateRainforest
    ([88, 84, 70], [38, 74, 58], [108, 106, 104]), // Boreal
    ([136, 128, 106], [110, 118, 86], [126, 122, 118]), // Tundra
    ([236, 242, 250], [222, 232, 246], [170, 184, 204]), // IceCap
    ([150, 142, 130], [108, 120, 92], [132, 128, 126]), // Alpine
];

/// Per-biome colours before land plants, as sRGB bytes `(ground, rock)`: sediment and bare rock,
/// warmed by iron oxides, with no green anywhere. Same indexing as `VEGETATED`. Ice stays white.
const BARREN: [(Srgb, Srgb); 12] = [
    ([150, 104, 80], [120, 96, 84]),   // TropicalRainforest: laterite red
    ([176, 140, 100], [140, 112, 90]), // MonsoonSavanna
    ([216, 176, 120], [180, 124, 86]), // HotDesert
    ([170, 150, 128], [134, 120, 110]), // ColdDesert
    ([180, 150, 110], [146, 122, 98]), // Steppe
    ([168, 138, 104], [150, 126, 104]), // Mediterranean
    ([150, 126, 100], [124, 108, 94]), // TemperateBroadleaf
    ([130, 110, 92], [110, 98, 90]),   // TemperateRainforest
    ([128, 114, 100], [112, 106, 102]), // Boreal
    ([150, 140, 126], [130, 124, 120]), // Tundra
    ([232, 240, 250], [176, 190, 208]), // IceCap
    ([144, 134, 124], [132, 126, 122]), // Alpine
];

/// How much of the vegetated palette an era shows (0 = bare sediment) and a multiplicative tint
/// on its canopy: swampy dark green for the first forests, cool blue-green conifers, warm modern
/// greens.
fn era_vegetation(era: VegetationEra) -> (f32, [f32; 3]) {
    match era {
        VegetationEra::Barren => (0.0, [1.0, 1.0, 1.0]),
        VegetationEra::EarlyPlants => (0.25, [0.9, 1.0, 0.85]),
        VegetationEra::FirstForests => (0.8, [0.85, 0.95, 0.8]),
        VegetationEra::ConiferAge => (0.9, [0.8, 0.9, 0.85]),
        VegetationEra::FloweringPlants => (1.0, [1.0, 1.0, 0.95]),
        VegetationEra::Modern => (1.0, [1.0, 1.0, 1.0]),
    }
}

fn to_linear(c: Srgb) -> [f32; 3] {
    c.map(|b| {
        let s = f32::from(b) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    })
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
}

/// Colours for an era and biome. Callers map the biome through [`effective_biome`] first; this
/// function takes it as given.
pub fn palette(era: VegetationEra, biome: BiomeClass) -> SurfacePalette {
    let (veg_ground, veg_canopy, veg_rock) = VEGETATED[biome as usize];
    let (bare_ground, bare_rock) = BARREN[biome as usize];
    let (bare_ground, bare_rock) = (to_linear(bare_ground), to_linear(bare_rock));
    let (amount, tint) = era_vegetation(era);
    let canopy = to_linear(veg_canopy);
    let canopy = [0, 1, 2].map(|i| canopy[i] * tint[i]);
    SurfacePalette {
        ground: mix(bare_ground, to_linear(veg_ground), amount),
        canopy: mix(bare_ground, canopy, amount),
        rock: mix(bare_rock, to_linear(veg_rock), amount),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_BIOMES: [BiomeClass; 12] = [
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
    const ALL_ERAS: [VegetationEra; 6] = [
        VegetationEra::Barren,
        VegetationEra::EarlyPlants,
        VegetationEra::FirstForests,
        VegetationEra::ConiferAge,
        VegetationEra::FloweringPlants,
        VegetationEra::Modern,
    ];

    #[test]
    fn koppen_collapse_table() {
        use BiomeClass::*;
        // Beck et al. 2018 legend order, codes 1..=30.
        let expected = [
            TropicalRainforest,  // 1 Af
            MonsoonSavanna,      // 2 Am
            MonsoonSavanna,      // 3 Aw
            HotDesert,           // 4 BWh
            ColdDesert,          // 5 BWk
            Steppe,              // 6 BSh
            Steppe,              // 7 BSk
            Mediterranean,       // 8 Csa
            Mediterranean,       // 9 Csb
            Mediterranean,       // 10 Csc
            TemperateBroadleaf,  // 11 Cwa
            TemperateBroadleaf,  // 12 Cwb
            TemperateBroadleaf,  // 13 Cwc
            TemperateBroadleaf,  // 14 Cfa
            TemperateRainforest, // 15 Cfb
            TemperateRainforest, // 16 Cfc
            TemperateBroadleaf,  // 17 Dsa
            TemperateBroadleaf,  // 18 Dsb
            Boreal,              // 19 Dsc
            Boreal,              // 20 Dsd
            TemperateBroadleaf,  // 21 Dwa
            TemperateBroadleaf,  // 22 Dwb
            Boreal,              // 23 Dwc
            Boreal,              // 24 Dwd
            TemperateBroadleaf,  // 25 Dfa
            TemperateBroadleaf,  // 26 Dfb
            Boreal,              // 27 Dfc
            Boreal,              // 28 Dfd
            Tundra,              // 29 ET
            IceCap,              // 30 EF
        ];
        assert_eq!(biome_from_koppen(0), None);
        for (i, want) in expected.iter().enumerate() {
            assert_eq!(
                biome_from_koppen(i as u8 + 1),
                Some(*want),
                "code {}",
                i + 1
            );
        }
        assert_eq!(biome_from_koppen(31), None);
        assert_eq!(biome_from_koppen(255), None);
    }

    #[test]
    fn vegetation_era_boundaries() {
        use VegetationEra::*;
        let era = |ma: f64| vegetation_era(YearsBp::from_myr(ma));
        assert_eq!(era(0.0), Modern);
        assert_eq!(era(19.999), Modern);
        assert_eq!(era(20.0), FloweringPlants);
        assert_eq!(era(129.999), FloweringPlants);
        assert_eq!(era(130.0), ConiferAge);
        assert_eq!(era(299.999), ConiferAge);
        assert_eq!(era(300.0), FirstForests);
        assert_eq!(era(384.999), FirstForests);
        assert_eq!(era(385.0), EarlyPlants);
        assert_eq!(era(469.999), EarlyPlants);
        assert_eq!(era(470.0), Barren);
        assert_eq!(era(540.0), Barren);
        // The standard snapshots.
        assert_eq!(era(400.0), EarlyPlants);
        assert_eq!(era(250.0), ConiferAge);
    }

    #[test]
    fn no_grass_before_20_ma() {
        use BiomeClass::*;
        for era in ALL_ERAS {
            for b in ALL_BIOMES {
                let got = effective_biome(era, b);
                if era == VegetationEra::Modern {
                    assert_eq!(got, b);
                } else {
                    assert_ne!(got, Steppe, "{era:?} {b:?}");
                    assert_ne!(got, MonsoonSavanna, "{era:?} {b:?}");
                    match b {
                        Steppe => assert_eq!(got, Mediterranean),
                        MonsoonSavanna => assert_eq!(got, TemperateBroadleaf),
                        _ => assert_eq!(got, b),
                    }
                }
            }
        }
    }

    #[test]
    fn no_green_before_470_ma() {
        for b in ALL_BIOMES {
            let p = palette(VegetationEra::Barren, b);
            for c in [p.ground, p.canopy, p.rock] {
                assert!(c[1] <= c[0].max(c[2]) + 0.02, "{b:?}: {c:?}");
            }
        }
    }

    #[test]
    fn palette_is_valid_linear_rgb_and_distinguishes_biomes() {
        for era in ALL_ERAS {
            for b in ALL_BIOMES {
                let p = palette(era, b);
                for c in [p.ground, p.canopy, p.rock] {
                    assert!(
                        c.iter().all(|v| (0.0..=1.0).contains(v)),
                        "{era:?} {b:?}: {c:?}"
                    );
                }
            }
        }
        // Vegetation shows up: a modern rainforest canopy is greener than bare rock of the same place.
        let modern = palette(VegetationEra::Modern, BiomeClass::TropicalRainforest);
        assert!(modern.canopy[1] > modern.canopy[0] && modern.canopy[1] > modern.canopy[2]);
        let ice = palette(VegetationEra::Modern, BiomeClass::IceCap);
        assert!(ice.ground.iter().all(|&v| v > 0.6));
    }

    #[test]
    fn zonal_rule_bands() {
        use BiomeClass::*;
        let z = |lat, e, c| zonal_biome(lat, e, c, 0.0);
        for lat in [0.0, 5.0, -9.9] {
            assert_eq!(z(lat, 100.0, 1500.0), TropicalRainforest);
            assert_eq!(z(lat, 100.0, 1500.1), MonsoonSavanna);
        }
        for lat in [10.0, 20.0, -29.9] {
            assert_eq!(z(lat, 100.0, 500.1), HotDesert);
            assert_eq!(z(lat, 100.0, 500.0), MonsoonSavanna);
        }
        for lat in [30.0, 40.0, -44.9] {
            assert_eq!(z(lat, 100.0, 500.0), Mediterranean);
            assert_eq!(z(lat, 100.0, 500.1), Steppe);
        }
        for lat in [45.0, 55.0, -59.9] {
            assert_eq!(z(lat, 100.0, 1000.0), TemperateBroadleaf);
            assert_eq!(z(lat, 100.0, 1000.1), Steppe);
        }
        for lat in [60.0, 65.0, -69.9] {
            assert_eq!(z(lat, 100.0, 0.0), Boreal);
            assert_eq!(z(lat, 100.0, 5000.0), Boreal);
        }
        for lat in [70.1, 80.0, -89.0] {
            assert_eq!(z(lat, 0.0, 0.0), Tundra);
        }
        // Above the snowline.
        assert_eq!(z(0.0, 5500.1, 0.0), Alpine);
        assert_eq!(z(0.0, 5500.0, 0.0), TropicalRainforest);
        assert_eq!(z(45.0, 5500.0 - 70.0 * 25.0 + 1.0, 0.0), Alpine);
        assert_eq!(z(61.0, snowline_m(61.0, 0.0) + 1.0, 0.0), IceCap);
        assert_eq!(z(-80.0, snowline_m(80.0, 0.0) + 1.0, 0.0), IceCap);
        assert_eq!(z(60.0, snowline_m(60.0, 0.0) + 1.0, 0.0), Alpine);
    }

    #[test]
    fn snowline() {
        assert_eq!(snowline_m(0.0, 0.0), 5500.0);
        assert_eq!(snowline_m(20.0, 0.0), 5500.0);
        assert_eq!(snowline_m(-20.0, 0.0), 5500.0);
        assert!((snowline_m(30.0, 0.0) - 4800.0).abs() < 1e-9);
        assert!((snowline_m(-30.0, 0.0) - 4800.0).abs() < 1e-9);
        assert!((snowline_m(60.0, 0.0) - 2700.0).abs() < 1e-9);
        assert!(snowline_m(90.0, 0.0) >= 0.0);
        assert_eq!(snowline_m(90.0, 0.0), 600.0);
    }

    /// Ruling R18: glacial eras lower the snowline by 1,000 m per -120 m of sea-level anomaly,
    /// clamped to 0..1,200 m, so mountain glaciers too small for ICE-6G's 1-degree grid show.
    #[test]
    fn snowline_drops_in_glacial_eras() {
        for lat in [0.0, 20.0, -35.0, 46.5, 60.0, 63.0] {
            let base = snowline_m(lat, 0.0);
            // -120 m: 1,000 m lower; -60 m: 500 m lower.
            assert!(
                (snowline_m(lat, -120.0) - (base - 1000.0)).abs() < 1e-9,
                "{lat}"
            );
            assert!(
                (snowline_m(lat, -60.0) - (base - 500.0)).abs() < 1e-9,
                "{lat}"
            );
            // -150 m would be 1,250 m: clamped to 1,200 m, and so is anything deeper.
            assert!(
                (snowline_m(lat, -150.0) - (base - 1200.0)).abs() < 1e-9,
                "{lat}"
            );
            assert!(
                (snowline_m(lat, -400.0) - (base - 1200.0)).abs() < 1e-9,
                "{lat}"
            );
            // Positive anomalies (warm eras) never raise it.
            assert_eq!(snowline_m(lat, 8.96), base, "{lat}");
            assert_eq!(snowline_m(lat, 50.0), base, "{lat}");
        }
        // The Alps at the Last Glacial Maximum: 3,645 m today, 2,645 m at -120 m.
        assert!((snowline_m(46.5, -120.0) - 2645.0).abs() < 1e-9);
        // Never below sea level.
        assert_eq!(snowline_m(85.0, -120.0), 0.0);
        // The zonal rule takes the anomaly too: alpine 1,000 m lower at -120 m.
        assert_eq!(
            zonal_biome(45.0, 3000.0, 0.0, 0.0),
            BiomeClass::TemperateBroadleaf
        );
        assert_eq!(zonal_biome(45.0, 3000.0, 0.0, -120.0), BiomeClass::Alpine);
    }
}
