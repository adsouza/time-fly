//! The deep eras' coarse field: the globe's rendered surface, with the modern-terrain bridge
//! frame read from the tiles (ruling R5).

use std::sync::Arc;

use bevy_math::{DQuat, DVec3};

use crate::formats::Grid;
use crate::geo::RenderDir;
use crate::geodynamics::{CrustModel, PlateId, PlateIndex, ResolvedPosition};
use crate::globe::{
    DeepSampling, FrameSample, OCEAN_GAP_FALLBACK_M, PlateRotations, RenderedSurface,
    plate_rotations, rendered_surface, shell_surface_m,
};
use crate::surface::{DeepBracket, FrameSource, Timeline};
use crate::worldgen::frame::RegionFrame;
use crate::worldgen::tiles::{SourceLevel, TILE_SPACING_M, TileSet};

use super::modern::{PresentRead, SupportCache, support_cube_size};

/// How far from plate land an era-fixed crack may lie and still show the frames' land
/// ([`DeepField::sliver_land_m`]).
pub const SLIVER_RADIUS_M: f64 = 50_000.0;
/// The rings and bearings probed for plate land around a crack.
const SLIVER_RING_M: [f64; 2] = [SLIVER_RADIUS_M / 2.0, SLIVER_RADIUS_M];
const SLIVER_BEARINGS: u32 = 8;

use super::{
    CoarseField, CoarseSample, DEEP_SPACING_M, FieldError, KeyKind, LatticeKey, check_cube,
    cube_bilinear, koppen_code, lat_lon,
};

/// The deep eras: the plate renderer's CPU reference at the snapshot's PaleoDEM bracket. Each
/// endpoint is read through its own sampler: the modern-terrain bridge frame reads the ETOPO
/// data (a tile, the overview, else its cube frame), every other endpoint its cube frame. Sea
/// level applies per endpoint as `elev − anomaly`, inside the globe functions.
///
/// - Where the PaleoDEM dominates (source spacing 111 km), samples are the reference itself,
///   read at overview level: tiles would add nothing at that spacing, and the field reads none.
/// - Where the bridge frame dominates (1,852 m), samples take the two-level path of
///   the `modern` module: the reference on a 27.75 km support lattice, the ETOPO term per
///   sample at the plate's present-day position. [`CoarseField::tiles_needed`] lists the tiles
///   those reads touch.
#[derive(Clone, Debug)]
pub struct DeepField<'a> {
    pub(super) crust: &'a CrustModel,
    pub(super) tiles: &'a TileSet,
    pub(super) sampling: DeepSampling,
    /// Indexed by plate index.
    pub(super) rotations: Vec<PlateRotations>,
    /// Each plate's era-to-plate-fixed rotation at the snapshot age, indexed by plate index:
    /// exactly the rotation [`CrustModel::resolve`] inverts.
    pub(super) to_plate: Vec<Option<DQuat>>,
    /// Each plate's read of the ETOPO endpoint from an era direction, indexed by plate index.
    pub(super) present: Vec<PresentRead>,
    /// `(plate id, plate index)` sorted by id.
    by_id: Vec<(PlateId, PlateIndex)>,
    /// `[younger, older]` cube frames (R = elevation, G = coverage).
    pub(super) frames: [&'a Grid; 2],
    /// Which endpoints are the modern-terrain bridge frame.
    pub(super) etopo: [bool; 2],
    fallback: bool,
    /// The two-level path's support lattice size and node cache.
    pub(super) support_n: u32,
    pub(super) support: Arc<SupportCache>,
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
        let rotations: Vec<PlateRotations> = (0..plates)
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
        let star = if etopo[0] { 0 } else { 1 };
        let present = rotations
            .iter()
            .map(|r: &PlateRotations| PresentRead::of(r.sources[star], r.current))
            .collect();
        Ok(DeepField {
            crust,
            tiles,
            sampling,
            rotations,
            to_plate,
            present,
            by_id,
            frames,
            etopo,
            fallback,
            support_n: support_cube_size(),
            support: Arc::new(SupportCache::default()),
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
    pub(super) fn endpoint_sample(
        &self,
        e: usize,
        dir: RenderDir,
        level: SourceLevel,
    ) -> FrameSample {
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

    pub(super) fn plate_index(&self, id: PlateId) -> Option<PlateIndex> {
        self.by_id
            .binary_search_by_key(&id, |&(i, _)| i)
            .ok()
            .map(|k| self.by_id[k].1)
    }

    fn plate_fixed(&self, id: PlateId, era: DVec3) -> Option<DVec3> {
        let q = self.to_plate[self.plate_index(id)?.0 as usize]?;
        Some((q * era).normalize())
    }

    pub(super) fn tracked_or_era_fixed(&self, era: DVec3) -> LatticeKey {
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

    /// The region's value where the globe shows the ocean shell: the shell's depth, except in
    /// a thin era-fixed sliver ([`sliver_land_m`](Self::sliver_land_m)), which shows the frames'
    /// own land.
    pub(super) fn ocean_value_m(
        &self,
        era: RenderDir,
        key: &LatticeKey,
        level: SourceLevel,
    ) -> f64 {
        self.ocean_value(era, key, level).0
    }

    /// [`ocean_value_m`](Self::ocean_value_m) and whether the sliver rule gave it.
    pub(super) fn ocean_value(
        &self,
        era: RenderDir,
        key: &LatticeKey,
        level: SourceLevel,
    ) -> (f64, bool) {
        let samples = [
            self.endpoint_sample(0, era, level),
            self.endpoint_sample(1, era, level),
        ];
        if matches!(key, LatticeKey::EraFixed(_))
            && let Some(land) = self.sliver_land(era, samples, level)
        {
            return (land, true);
        }
        let shell = shell_surface_m(&self.sampling, samples);
        // The globe shows water here, whatever the shell's blend says.
        let depth = if shell.is_nan() {
            OCEAN_GAP_FALLBACK_M
        } else {
            shell.min(0.0)
        };
        (depth, false)
    }

    /// The frames' own land at an era direction the globe shows as the ocean shell's crack fill,
    /// or `None`. That is the case when no plate is valid there (an era-fixed point), the
    /// endpoints' blend (sea level applied, coverage ignored) is land, and plate land lies within
    /// [`SLIVER_RADIUS_M`].
    ///
    /// Why: plate footprints are cube cells in each plate's own frame, so after relative motion
    /// they leave thin straight slivers between plates where no plate is valid. The frames'
    /// coverage mask (a 128-cube texel, filtered) still says "covered" there, so the globe's
    /// shell clamps the frames' land to a −200 m crack fill: a 1-pixel line on the globe, but a
    /// long straight channel through dry land in a region (the 10 Ma Task 8 shot). In the real
    /// bake such points are 0.04–1% of the globe and 94–100% of them lie within 50 km of plate
    /// land. Wider gaps keep the globe's rule.
    pub fn sliver_land_m(&self, era_dir: RenderDir) -> Option<f64> {
        let era = RenderDir(era_dir.0.normalize());
        let level = SourceLevel::Overview;
        let surface = rendered_surface(
            self.crust,
            era_dir,
            &self.sampling,
            &self.rotations,
            |e, d| self.endpoint_sample(e, d, level),
        );
        if surface != RenderedSurface::Ocean
            || !matches!(self.tracked_or_era_fixed(era.0), LatticeKey::EraFixed(_))
        {
            return None;
        }
        let samples = [
            self.endpoint_sample(0, era, level),
            self.endpoint_sample(1, era, level),
        ];
        self.sliver_land(era, samples, level)
    }

    fn sliver_land(
        &self,
        era: RenderDir,
        samples: [FrameSample; 2],
        level: SourceLevel,
    ) -> Option<f64> {
        // The shell's blend with every sample treated as uncovered: the frames' own surface.
        let own = shell_surface_m(
            &self.sampling,
            samples.map(|s| FrameSample {
                covered: false,
                ..s
            }),
        );
        if own.is_nan() || own <= 0.0 {
            return None;
        }
        let (north, east) = RegionFrame { center: era }.north_east();
        let near_plate_land = SLIVER_RING_M.iter().any(|&r| {
            (0..SLIVER_BEARINGS).any(|k| {
                let t = f64::from(k) * std::f64::consts::TAU / f64::from(SLIVER_BEARINGS);
                let angle = r / crate::worldgen::frame::EARTH_RADIUS_M;
                let tangent = north * t.cos() + east * t.sin();
                let d = RenderDir((era.0 * angle.cos() + tangent * angle.sin()).normalize());
                matches!(
                    rendered_surface(self.crust, d, &self.sampling, &self.rotations, |e, dd| {
                        self.endpoint_sample(e, dd, level)
                    }),
                    RenderedSurface::Land { .. }
                )
            })
        });
        near_plate_land.then_some(own)
    }

    pub(super) fn sample_with(&self, era_dir: RenderDir, level: SourceLevel) -> CoarseSample {
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
                let key = self.tracked_or_era_fixed(era);
                (self.ocean_value_m(RenderDir(era), &key, level), key)
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
        if self.is_modern() {
            self.modern_sample(era_dir, SourceLevel::Tiles)
        } else {
            self.sample_with(era_dir, SourceLevel::Overview)
        }
    }

    fn sample_overview(&self, era_dir: RenderDir) -> CoarseSample {
        if self.is_modern() {
            self.modern_sample(era_dir, SourceLevel::Overview)
        } else {
            self.sample_with(era_dir, SourceLevel::Overview)
        }
    }

    fn tiles_needed(&self, frame: &RegionFrame) -> Vec<(u8, u8)> {
        if self.is_modern() {
            self.modern_tiles_needed(frame)
        } else {
            Vec::new()
        }
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

    /// The rift at 1 Ma (78% modern terrain): PaleoDEMs from a smooth function, ETOPO from a
    /// smooth overview, so the two-level path runs at overview level everywhere.
    fn modern_fixture() -> (CrustModel, Vec<Grid>, TileSet) {
        let crust = rift(0);
        let fr = frames(|_, d| {
            let p = d.0 * 3.0;
            (
                (900.0 * (p.x.sin() + p.y.cos() * p.z.cos()) - 400.0) as f32,
                d.0.y > -0.8,
            )
        });
        let mut tiles = TileSet::new();
        tiles
            .set_overview(crate::worldgen::tiles::tests::overview_from(
                2,
                |lat, lon| {
                    let e = 600.0 * (lat * 0.11).sin() * (lon * 0.07).cos() + 150.0;
                    (e as f32, 26)
                },
            ))
            .unwrap();
        (crust, fr, tiles)
    }

    #[test]
    fn modern_path_reproduces_the_reference_at_support_nodes() {
        let (crust, fr, tiles) = modern_fixture();
        let f = deep_at(&crust, &fr, &tiles, 1.0);
        assert!(f.is_modern() && f.etopo_weight() > 0.7);
        let n = f.support_n;
        let (mut checked, mut land) = (0, 0);
        for face in crate::cube::CubeFace::ALL {
            for row in (0..n).step_by(23) {
                for col in (0..n).step_by(23) {
                    let d = crate::cube::texel_center_dir(face, col, row, n);
                    let reference = f.sample_with(d, SourceLevel::Overview);
                    let modern = f.modern_sample(d, SourceLevel::Overview);
                    // Land on both or neither: plate water uses the plate's own (negative)
                    // value, the globe's shell its depth, so only the sign must agree there.
                    assert_eq!(
                        modern.elevation_m > 0.0,
                        reference.elevation_m > 0.0,
                        "{d:?}: {modern:?} vs {reference:?}"
                    );
                    if reference.elevation_m > 0.0 {
                        land += 1;
                        let diff = (modern.elevation_m - reference.elevation_m).abs();
                        assert!(diff < 1e-6, "{d:?}: {modern:?} vs {reference:?}");
                        assert_eq!(modern.lattice.kind(), reference.lattice.kind());
                    }
                    checked += 1;
                }
            }
        }
        assert!(land > 50 && checked - land > 50, "{land} of {checked}");
    }

    #[test]
    fn modern_path_tracks_the_reference_between_nodes() {
        // Between support nodes the PaleoDEM part is bilinear on a quarter of its spacing and the
        // ETOPO part is read in full: on interior plate land the two-level sample stays close to
        // the reference (which reads both endpoints directly).
        let (crust, fr, tiles) = modern_fixture();
        let f = deep_at(&crust, &fr, &tiles, 1.0);
        let (mut worst, mut count, mut edge_worst) = (0.0f64, 0, 0.0f64);
        for lat in (-600..=600).step_by(7) {
            for lon in (-1800..1800).step_by(13) {
                let d = dir(f64::from(lat) / 10.0, f64::from(lon) / 10.0);
                let reference = f.sample_with(d, SourceLevel::Overview);
                if reference.elevation_m < 200.0 {
                    continue; // away from coasts and plate water
                }
                let modern = f.modern_sample(d, SourceLevel::Overview);
                let carriers: Vec<_> = crate::cube::bilinear_taps(d, f.support_n)
                    .iter()
                    .map(|&((face, col, row), _)| f.support(face, col, row).carrier)
                    .collect();
                let diff = (modern.elevation_m - reference.elevation_m).abs();
                if carriers.iter().all(|&c| c.is_some() && c == carriers[0]) {
                    worst = worst.max(diff);
                    count += 1;
                } else {
                    // Within one support cell of a plate edge or gap the taps mix carriers.
                    edge_worst = edge_worst.max(diff);
                }
            }
        }
        println!(
            "interior worst {worst:.2} m over {count} points; plate-edge worst {edge_worst:.1} m"
        );
        assert!(count > 1000, "{count}");
        assert!(worst < 1.0, "worst {worst} m over {count} points");
        assert!(edge_worst < 500.0, "edge worst {edge_worst} m");
    }

    #[test]
    fn modern_path_agrees_on_land_and_sea_off_nodes() {
        // Everywhere, plate edges, gaps and coasts included: where the reference is clearly land
        // or sea (|elevation| > 150 m) the two-level sample is on the same side. At every
        // standard snapshot on the two-level path (50 ka, 100 ka, 1 Ma) and near its oldest end
        // (2 Ma, ETOPO weight 0.56; it ends at 0.5, about 2.25 Ma).
        let (crust, fr, tiles) = modern_fixture();
        for age in [0.05, 0.1, 1.0, 2.0] {
            let f = deep_at(&crust, &fr, &tiles, age);
            assert!(f.is_modern(), "{age} Ma: ETOPO weight {}", f.etopo_weight());
            let (mut checked, mut edge, mut worst_miss) = (0, 0, 0.0f64);
            let mut misses = Vec::new();
            for lat in (-890..=890).step_by(9) {
                for lon in (-1800..1800).step_by(11) {
                    let d = dir(f64::from(lat) / 10.0, f64::from(lon) / 10.0);
                    let reference = f.sample_with(d, SourceLevel::Overview).elevation_m;
                    if reference.abs() <= 150.0 {
                        continue;
                    }
                    let modern = f.modern_sample(d, SourceLevel::Overview).elevation_m;
                    let carriers: Vec<_> = crate::cube::bilinear_taps(d, f.support_n)
                        .iter()
                        .map(|&((face, col, row), _)| f.support(face, col, row).carrier)
                        .collect();
                    if !carriers.iter().all(|&c| c == carriers[0]) {
                        edge += 1;
                    }
                    if (modern > 0.0) != (reference > 0.0) {
                        worst_miss = worst_miss.max(reference.abs());
                        misses.push((lat, lon, reference, modern));
                    }
                    checked += 1;
                }
            }
            println!(
                "{age} Ma: {checked} points ({edge} at plate edges or gaps), {} sign misses",
                misses.len()
            );
            assert!(checked > 20_000 && edge > 100, "{age} Ma: {checked} {edge}");
            assert!(
                misses.is_empty(),
                "{age} Ma: {} misses, worst |ref| {worst_miss}: {:?}",
                misses.len(),
                &misses[..misses.len().min(8)]
            );
        }
    }
}
