//! The deep eras' coarse field: the globe's rendered surface, with the modern-terrain bridge
//! frame read from the tiles (ruling R5).

use bevy_math::{DQuat, DVec3};

use crate::formats::Grid;
use crate::geo::RenderDir;
use crate::geodynamics::{CrustModel, PlateId, PlateIndex, ResolvedPosition};
use crate::globe::{
    DeepSampling, FrameSample, OCEAN_GAP_FALLBACK_M, PlateRotations, RenderedSurface,
    plate_rotations, rendered_surface, shell_surface_m,
};
use crate::surface::{DeepBracket, FrameSource, Timeline};
use crate::worldgen::tiles::{SourceLevel, TILE_SPACING_M, TileSet};

use super::{
    CoarseField, CoarseSample, DEEP_SPACING_M, FieldError, KeyKind, LatticeKey, check_cube,
    cube_bilinear, koppen_code, lat_lon,
};

/// The deep eras: the plate renderer's CPU reference at the snapshot's PaleoDEM bracket. Each
/// endpoint is read through its own sampler: the modern-terrain bridge frame reads the ETOPO
/// tiles (falling back to its cube frame where a tile is missing), every other endpoint its
/// cube frame. Sea level applies per endpoint as `elev − anomaly`, inside the globe functions.
#[derive(Clone, Debug)]
pub struct DeepField<'a> {
    crust: &'a CrustModel,
    tiles: &'a TileSet,
    sampling: DeepSampling,
    /// Indexed by plate index.
    rotations: Vec<PlateRotations>,
    /// Each plate's era-to-plate-fixed rotation at the snapshot age, indexed by plate index:
    /// exactly the rotation [`CrustModel::resolve`] inverts.
    to_plate: Vec<Option<DQuat>>,
    /// `(plate id, plate index)` sorted by id.
    by_id: Vec<(PlateId, PlateIndex)>,
    /// `[younger, older]` cube frames (R = elevation, G = coverage).
    frames: [&'a Grid; 2],
    /// Which endpoints are the modern-terrain bridge frame.
    etopo: [bool; 2],
    fallback: bool,
}

impl<'a> DeepField<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        crust: &'a CrustModel,
        timeline: &Timeline,
        age_myr: f64,
        bracket: &DeepBracket,
        frames: [&'a Grid; 2],
        tiles: &'a TileSet,
        fallback: bool,
    ) -> Result<DeepField<'a>, FieldError> {
        check_cube(frames[0], 2, 0)?;
        check_cube(frames[1], 2, 1)?;
        let sampling = DeepSampling::from_bracket(timeline, age_myr, bracket);
        let table = crust.rotations();
        let plates = table.plate_ids.len() as u32;
        let rotations = (0..plates)
            .map(|p| plate_rotations(table, PlateIndex(p), &sampling))
            .collect();
        let to_plate = (0..plates)
            .map(|p| {
                table
                    .rotation_at(PlateIndex(p), age_myr)
                    .map(|q| q.as_dquat().normalize().inverse())
            })
            .collect();
        let mut by_id: Vec<(PlateId, PlateIndex)> = (0..plates)
            .map(|p| (table.plate_ids[p as usize], PlateIndex(p)))
            .collect();
        by_id.sort_by_key(|&(id, _)| id);
        let bridge = timeline.bridge_frame_index();
        let etopo = sampling.endpoints.map(|e| {
            e.frame == bridge
                && timeline.frames()[e.frame].source
                    == (FrameSource::PaleoDem {
                        source_rotation_age_myr: 0.0,
                    })
        });
        Ok(DeepField {
            crust,
            tiles,
            sampling,
            rotations,
            to_plate,
            by_id,
            frames,
            etopo,
            fallback,
        })
    }

    pub fn sampling(&self) -> &DeepSampling {
        &self.sampling
    }

    /// The combined weight of the endpoints read from the ETOPO tiles.
    pub fn etopo_weight(&self) -> f64 {
        let w = self.sampling.older_weight;
        let weights = if self.sampling.endpoints[0].frame == self.sampling.endpoints[1].frame {
            [1.0, 0.0]
        } else {
            [1.0 - w, w]
        };
        (0..2).filter(|&e| self.etopo[e]).map(|e| weights[e]).sum()
    }

    /// Endpoint `e` at a direction in its published frame (present-day geography for the
    /// bridge frame, which reads a tile, the overview or its cube, by `level`).
    fn endpoint_sample(&self, e: usize, dir: RenderDir, level: SourceLevel) -> FrameSample {
        let [cube_elevation, coverage] = cube_bilinear(self.frames[e], dir, [0, 1]);
        let elevation_m = if self.etopo[e] {
            let (lat, lon) = lat_lon(dir);
            self.tiles
                .lookup(lat, lon, level)
                .map_or(cube_elevation, |(h, _)| f64::from(h))
        } else {
            cube_elevation
        };
        FrameSample {
            elevation_m,
            covered: coverage > 0.5,
        }
    }

    fn plate_index(&self, id: PlateId) -> Option<PlateIndex> {
        self.by_id
            .binary_search_by_key(&id, |&(i, _)| i)
            .ok()
            .map(|k| self.by_id[k].1)
    }

    fn plate_fixed(&self, id: PlateId, era: DVec3) -> Option<DVec3> {
        let q = self.to_plate[self.plate_index(id)?.0 as usize]?;
        Some((q * era).normalize())
    }

    fn tracked_or_era_fixed(&self, era: DVec3) -> LatticeKey {
        // No visible land: the lowest valid source ID, as the globe's resolve rule says.
        match self
            .crust
            .resolve(RenderDir(era), self.sampling.age_myr, |_| None)
        {
            ResolvedPosition::Tracked(pos) => match self.plate_fixed(pos.plate_id, era) {
                Some(plate_fixed) => LatticeKey::Tracked {
                    plate_id: pos.plate_id,
                    plate_fixed,
                },
                None => LatticeKey::EraFixed(era),
            },
            ResolvedPosition::EraFixed { .. } => LatticeKey::EraFixed(era),
        }
    }

    /// The present-day Köppen code where the ETOPO endpoint dominates: read where that
    /// endpoint was sampled (the plate's source position for plate land, the era direction for
    /// the ocean shell).
    fn koppen(
        &self,
        plate: Option<(PlateId, DVec3)>,
        era: DVec3,
        level: SourceLevel,
    ) -> Option<u8> {
        if self.etopo_weight() < 0.5 {
            return None;
        }
        let e = if self.etopo[0] { 0 } else { 1 };
        let dir = match plate {
            Some((id, plate_fixed)) => {
                let source = self.rotations[self.plate_index(id)?.0 as usize].sources[e]?;
                (source.as_dquat().normalize() * plate_fixed).normalize()
            }
            None => era,
        };
        let (lat, lon) = lat_lon(RenderDir(dir));
        koppen_code(self.tiles.lookup(lat, lon, level)?.1)
    }

    fn sample_with(&self, era_dir: RenderDir, level: SourceLevel) -> CoarseSample {
        let era = era_dir.0.normalize();
        // The globe's input as given, so the result is bit-identical to the globe's.
        let surface = rendered_surface(
            self.crust,
            era_dir,
            &self.sampling,
            &self.rotations,
            |e, d| self.endpoint_sample(e, d, level),
        );
        let (elevation_m, lattice) = match surface {
            RenderedSurface::Land {
                plate_id,
                elevation_m,
            } => {
                let lattice = match self.plate_fixed(plate_id, era) {
                    Some(plate_fixed) => LatticeKey::Tracked {
                        plate_id,
                        plate_fixed,
                    },
                    None => LatticeKey::EraFixed(era),
                };
                (elevation_m, lattice)
            }
            RenderedSurface::GapLand { elevation_m } => {
                (elevation_m, self.tracked_or_era_fixed(era))
            }
            RenderedSurface::Ocean => {
                let shell = shell_surface_m(
                    &self.sampling,
                    [
                        self.endpoint_sample(0, RenderDir(era), level),
                        self.endpoint_sample(1, RenderDir(era), level),
                    ],
                );
                // The globe shows water here, whatever the shell's blend says.
                let depth = if shell.is_nan() {
                    OCEAN_GAP_FALLBACK_M
                } else {
                    shell.min(0.0)
                };
                (depth, self.tracked_or_era_fixed(era))
            }
        };
        let plate = match lattice {
            LatticeKey::Tracked {
                plate_id,
                plate_fixed,
            } if matches!(surface, RenderedSurface::Land { .. }) => Some((plate_id, plate_fixed)),
            _ => None,
        };
        CoarseSample {
            elevation_m,
            ice: 0.0,
            koppen: self.koppen(plate, era, level),
            lattice,
        }
    }
}

impl CoarseField for DeepField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        self.sample_with(era_dir, SourceLevel::Tiles)
    }

    fn sample_overview(&self, era_dir: RenderDir) -> CoarseSample {
        self.sample_with(era_dir, SourceLevel::Overview)
    }

    fn source_spacing_m(&self) -> f64 {
        if self.etopo_weight() < 0.5 {
            DEEP_SPACING_M
        } else if !self.fallback {
            TILE_SPACING_M
        } else {
            self.tiles.overview_spacing_m().unwrap_or(DEEP_SPACING_M)
        }
    }

    fn lattice_key(&self, era_dir: RenderDir, nearest: KeyKind) -> LatticeKey {
        match nearest {
            KeyKind::Tracked(plate_id) => match self.plate_fixed(plate_id, era_dir.0) {
                Some(plate_fixed) => LatticeKey::Tracked {
                    plate_id,
                    plate_fixed,
                },
                None => LatticeKey::EraFixed(era_dir.0),
            },
            KeyKind::Geographic => LatticeKey::Geographic(era_dir.0),
            KeyKind::EraFixed => LatticeKey::EraFixed(era_dir.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::FrameSelection;
    use crate::worldgen::testkit::*;

    /// The deep field at `age` on the rift, with PaleoDEM frames from `paleo` and constant tiles.
    fn deep_at<'a>(
        crust: &'a CrustModel,
        frames: &'a [Grid],
        tiles: &'a TileSet,
        age: f64,
    ) -> DeepField<'a> {
        let s = timeline();
        let d = match s.select_frames(age) {
            FrameSelection::Deep(d) => d,
            other => panic!("{other:?}"),
        };
        DeepField::new(
            crust,
            &s,
            age,
            &d,
            [&frames[d.frames.younger], &frames[d.frames.older]],
            tiles,
            false,
        )
        .unwrap()
    }

    #[test]
    fn bridge_frame_endpoint_reads_etopo_with_anomaly() {
        // At 30 ka the deep bracket's younger endpoint is the modern frame, at weight 0: the
        // elevation is ETOPO (not the cube frame's 7 m) minus the -50 m anomaly.
        let crust = rift(0);
        let centre = dir(0.0, 0.0); // render +Z: plate 202
        let tiles = single_tile(0.0, 0.0, 100.0, 1);
        let fr = frames(|_, _| (7.0, true));
        let f = deep_at(&crust, &fr, &tiles, 0.03);
        assert_eq!(f.sampling().endpoints[0].frame, 4);
        assert_eq!(f.sampling().older_weight, 0.0);
        assert_eq!(f.etopo_weight(), 1.0);
        let s = f.sample(centre);
        assert!((s.elevation_m - 150.0).abs() < 1e-9, "{s:?}");
        assert_eq!(s.koppen, Some(1));
        assert!(matches!(
            s.lattice,
            LatticeKey::Tracked {
                plate_id: PlateId(202),
                ..
            }
        ));
        assert_eq!(f.source_spacing_m(), 1852.0);
        // A missing tile falls back to the bridge frame's cube: 7 m + 50 m.
        let empty = TileSet::new();
        let f = deep_at(&crust, &fr, &empty, 0.03);
        assert!((f.sample(centre).elevation_m - 57.0).abs() < 1e-3);
        // Past halfway to 4.47 Ma the PaleoDEM dominates: 111 km spacing, no Köppen.
        let f = deep_at(&crust, &fr, &tiles, 3.0);
        assert!(f.etopo_weight() < 0.5);
        assert_eq!(f.source_spacing_m(), DEEP_SPACING_M);
        assert_eq!(f.sample(centre).koppen, None);
    }

    #[test]
    fn deep_field_is_the_rendered_surface() {
        // At 100 Ma plate 101 has turned onto plate 202's side; the rift's gap (render -X) is
        // ocean, with uncovered frame land showing there as gap land when present.
        let crust = rift(0);
        let fr = frames(|i, d| {
            let land = if i == 6 && d.0.x < -0.6 { 800.0 } else { 500.0 };
            (if d.0.y < -0.7 { -3000.0 } else { land }, d.0.x > -0.6)
        });
        let tiles = TileSet::new();
        let f = deep_at(&crust, &fr, &tiles, 100.0);
        let sampling = *f.sampling();
        let rotations: Vec<PlateRotations> = (0..2)
            .map(|p| plate_rotations(crust.rotations(), PlateIndex(p), &sampling))
            .collect();
        let (mut land, mut gap, mut ocean) = (0, 0, 0);
        for lat in (-85..=85).step_by(10) {
            for lon in (-180..180).step_by(15) {
                let d = dir(f64::from(lat), f64::from(lon));
                let rs = rendered_surface(&crust, d, &sampling, &rotations, |e, dd| {
                    f.endpoint_sample(e, dd, SourceLevel::Tiles)
                });
                let c = f.sample(d);
                assert_eq!(
                    c.elevation_m > 0.0,
                    rs.is_land(),
                    "{lat},{lon}: {c:?} {rs:?}"
                );
                assert_eq!(c.ice, 0.0);
                match rs {
                    RenderedSurface::Land {
                        plate_id,
                        elevation_m,
                    } => {
                        land += 1;
                        assert_eq!(c.elevation_m, elevation_m);
                        let LatticeKey::Tracked {
                            plate_id: id,
                            plate_fixed,
                        } = c.lattice
                        else {
                            panic!("{c:?}")
                        };
                        assert_eq!(id, plate_id);
                        // The key maps back to the era direction at the snapshot age.
                        let idx = crust.rotations().index_of(id).unwrap();
                        let q = crust.rotations().rotation_at(idx, 100.0).unwrap();
                        let back = q.as_dquat().normalize() * plate_fixed;
                        assert!((back - d.0).length() < 1e-9);
                    }
                    RenderedSurface::GapLand { elevation_m } => {
                        gap += 1;
                        assert_eq!(c.elevation_m, elevation_m);
                    }
                    RenderedSurface::Ocean => {
                        ocean += 1;
                        assert!(c.elevation_m <= 0.0);
                    }
                }
            }
        }
        assert!(land > 0 && gap > 0 && ocean > 0, "{land} {gap} {ocean}");
    }
}
