//! The recent eras' coarse field: ETOPO plus the ICE-6G change.

use crate::formats::Grid;
use crate::geo::RenderDir;
use crate::worldgen::tiles::{SourceLevel, TILE_SPACING_M, TileSet};

use super::{
    CoarseField, CoarseSample, FieldError, LatticeKey, check_cube, cube_bilinear, cube_spacing_m,
    koppen_code, lat_lon,
};

/// The recent eras (ICE-6G_C, up to 26 ka): `etopo + (Topo_era − Topo_present)`, where
/// `Topo_era` blends the bracketing frames by the older weight (ruling R6). `etopo` comes from a
/// tile, else the overview (the apron reads only the overview, ruling R7); with neither the
/// frames' own `Topo_era` stands in, at the cube's resolution.
#[derive(Copy, Clone, Debug)]
pub struct RecentField<'a> {
    tiles: &'a TileSet,
    /// `[younger, older]` ICE-6G frames: R = signed Topo (m), G = land, B = ice fraction.
    era: [&'a Grid; 2],
    older_weight: f64,
    /// The present-day ICE-6G frame (timeline frame 0).
    present: &'a Grid,
    /// Some inner tile the region needs is missing: report the overview's spacing, or the
    /// cube's without an overview.
    fallback: bool,
}

impl<'a> RecentField<'a> {
    pub fn new(
        tiles: &'a TileSet,
        era: [&'a Grid; 2],
        older_weight: f64,
        present: &'a Grid,
        fallback: bool,
    ) -> Result<RecentField<'a>, FieldError> {
        check_cube(era[0], 3, 0)?;
        check_cube(era[1], 3, 1)?;
        check_cube(present, 3, 2)?;
        Ok(RecentField {
            tiles,
            era,
            older_weight,
            present,
            fallback,
        })
    }
}

impl RecentField<'_> {
    fn sample_with(&self, era_dir: RenderDir, level: SourceLevel) -> CoarseSample {
        let [topo_young, ice_young] = cube_bilinear(self.era[0], era_dir, [0, 2]);
        let [topo_old, ice_old] = if self.older_weight == 0.0 {
            [topo_young, ice_young]
        } else {
            cube_bilinear(self.era[1], era_dir, [0, 2])
        };
        let w = self.older_weight;
        let topo_era = topo_young + (topo_old - topo_young) * w;
        let ice = ice_young + (ice_old - ice_young) * w;
        let (lat, lon) = lat_lon(era_dir);
        let (elevation_m, koppen) = match self.tiles.lookup(lat, lon, level) {
            Some((etopo, code)) => {
                let [topo_present] = cube_bilinear(self.present, era_dir, [0]);
                (
                    f64::from(etopo) + topo_era - topo_present,
                    koppen_code(code),
                )
            }
            None => (topo_era, None),
        };
        CoarseSample {
            elevation_m,
            ice: ice.clamp(0.0, 1.0) as f32,
            koppen,
            lattice: LatticeKey::Geographic(era_dir.0),
        }
    }
}

impl CoarseField for RecentField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        self.sample_with(era_dir, SourceLevel::Tiles)
    }

    fn sample_overview(&self, era_dir: RenderDir) -> CoarseSample {
        self.sample_with(era_dir, SourceLevel::Overview)
    }

    fn source_spacing_m(&self) -> f64 {
        if self.fallback {
            self.tiles
                .overview_spacing_m()
                .unwrap_or_else(|| cube_spacing_m(self.era[0].height))
        } else {
            TILE_SPACING_M
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::FrameSelection;
    use crate::worldgen::testkit::*;

    #[test]
    fn recent_adds_ice_delta() {
        let d = dir(10.0, 20.0);
        let tiles = single_tile(10.0, 20.0, 100.0, 27);
        // An ice sheet: Topo rises 2,000 m over the present, full ice cover.
        let (era, present) = (ice_frame(2100.0, 1.0), ice_frame(100.0, 0.0));
        let f = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
        let s = f.sample(d);
        assert!((s.elevation_m - 2100.0).abs() < 1e-6, "{s:?}");
        assert_eq!(s.ice, 1.0);
        assert_eq!(s.koppen, Some(27));
        assert_eq!(s.lattice, LatticeKey::Geographic(d.0));
        assert_eq!(f.source_spacing_m(), 1852.0);
        // The M1 sea-level sign through ICE-6G: a -100 m shelf with the delta +120 is +20 m land.
        let tiles = single_tile(10.0, 20.0, -100.0, 0);
        let (era, present) = (ice_frame(20.0, 0.0), ice_frame(-100.0, 0.0));
        let f = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
        let s = f.sample(d);
        assert!((s.elevation_m - 20.0).abs() < 1e-6, "{s:?}");
        assert_eq!(s.koppen, None, "code 0 is ocean");
    }

    #[test]
    fn recent_blends_the_bracket() {
        // 21 ka lies between the 20.076 and 21.076 ka frames: Topo and ice blend by the weight.
        let d = dir(-30.0, 140.0);
        let tiles = single_tile(-30.0, 140.0, 300.0, 4);
        let (young, old, present) = (
            ice_frame(400.0, 0.0),
            ice_frame(1400.0, 1.0),
            ice_frame(300.0, 0.0),
        );
        let s = timeline();
        let b = match s.select_frames(0.021) {
            FrameSelection::Recent(b) => b,
            other => panic!("{other:?}"),
        };
        assert_eq!((b.younger, b.older), (1, 2));
        let w = b.older_weight;
        assert!(w > 0.9 && w < 1.0, "{w}");
        let f = RecentField::new(&tiles, [&young, &old], w, &present, false).unwrap();
        let got = f.sample(d);
        let expected = 300.0 + (400.0 + 1000.0 * w) - 300.0;
        assert!(
            (got.elevation_m - expected).abs() < 1e-3,
            "{got:?} vs {expected}"
        );
        assert!((f64::from(got.ice) - w).abs() < 1e-3);
    }

    #[test]
    fn missing_inner_tile_falls_back_to_the_overview() {
        let mut tiles = TileSet::new();
        tiles
            .set_overview(crate::worldgen::tiles::tests::overview_from(2, |_, _| {
                (300.0, 8)
            }))
            .unwrap();
        let (era, present) = (ice_frame(130.0, 0.0), ice_frame(100.0, 0.0));
        let f = RecentField::new(&tiles, [&era, &era], 0.0, &present, true).unwrap();
        let s = f.sample(dir(41.0, 12.0));
        assert!(
            (s.elevation_m - 330.0).abs() < 1e-6,
            "overview + delta: {s:?}"
        );
        assert_eq!(s.koppen, Some(8));
        assert_eq!(f.sample_overview(dir(41.0, 12.0)), s);
        assert_eq!(f.source_spacing_m(), tiles.overview_spacing_m().unwrap());
        // A loaded tile still wins for the inner field; the apron's view ignores it.
        tiles
            .insert(
                9,
                38,
                crate::worldgen::tiles::tests::tile_from(9, 38, |_, _| (10.0, 15)),
            )
            .unwrap(); // 40..45 N, 10..15 E
        let f = RecentField::new(&tiles, [&era, &era], 0.0, &present, false).unwrap();
        assert!((f.sample(dir(41.0, 12.0)).elevation_m - 40.0).abs() < 1e-6);
        assert!((f.sample_overview(dir(41.0, 12.0)).elevation_m - 330.0).abs() < 1e-6);
        assert_eq!(f.source_spacing_m(), 1852.0);
    }

    #[test]
    fn recent_without_a_tile_uses_the_frames() {
        let tiles = TileSet::new();
        let (era, present) = (ice_frame(-40.0, 0.25), ice_frame(10.0, 0.0));
        let f = RecentField::new(&tiles, [&era, &era], 0.0, &present, true).unwrap();
        let s = f.sample(dir(0.0, 0.0));
        assert!((s.elevation_m + 40.0).abs() < 1e-6, "Topo_era alone: {s:?}");
        assert_eq!(s.koppen, None);
        assert_eq!(f.source_spacing_m(), cube_spacing_m(8));
        assert!(cube_spacing_m(128) > 75_000.0 && cube_spacing_m(128) < 80_000.0);
    }
}
