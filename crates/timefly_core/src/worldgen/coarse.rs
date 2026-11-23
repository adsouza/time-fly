//! Coarse fields: the real data a region amplifies (design spec §5.2), sampled at era-frame
//! directions. "Elevation" is always effective elevation relative to the era's sea level.
//!
//! - [`RecentField`] (ICE-6G frames, up to 26 ka): ETOPO present-day elevation plus the ICE-6G
//!   change `Topo(era) − Topo(present)`, with the ICE-6G ice fraction. Geographic lookup.
//! - [`DeepField`] (a PaleoDEM bracket, 30 ka on): the globe's CPU reference,
//!   [`rendered_surface`], so a region agrees with the globe by construction. The modern-terrain
//!   bridge frame reads the ETOPO tiles instead of its cube frame (ruling R5).
//! - [`CrossfadeField`] (between the last ICE-6G frame and the bridge frame): the held last
//!   recent result crossfaded into the deep one, as the globe's `FrameSelection::Bridge` does.
//!
//! [`snapshot_field`] picks the right one for an age from [`Timeline::select_frames`].

use std::f64::consts::FRAC_PI_2;
use std::fmt;

use bevy_math::{DQuat, DVec3};
use tracing::instrument;

use crate::cube::bilinear_taps;
use crate::formats::{Grid, GridKind};
use crate::geo::{RenderDir, geo_to_latlon, render_to_geo};
use crate::geodynamics::{CrustModel, PlateId, PlateIndex, ResolvedPosition};
use crate::globe::{
    DeepSampling, FrameSample, OCEAN_GAP_FALLBACK_M, PlateRotations, RenderedSurface,
    plate_rotations, rendered_surface, shell_surface_m,
};
use crate::surface::{DeepBracket, FrameSelection, FrameSource, Timeline};
use crate::worldgen::frame::EARTH_RADIUS_M;
use crate::worldgen::tiles::{TILE_SPACING_M, TileSet};

/// Source spacing of the deep (PaleoDEM) frames: about 1°.
pub const DEEP_SPACING_M: f64 = 111_000.0;

/// The canonical crust-space position that seeds a sample's detail (parent spec §5.2).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum LatticeKey {
    /// Present-day geography (the recent eras' fixed frame).
    Geographic(DVec3),
    /// Tracked crust: the plate and the direction in its fixed frame.
    Tracked {
        plate_id: PlateId,
        plate_fixed: DVec3,
    },
    /// No valid plate: the era-frame direction, keyed with the snapshot age by the generator.
    EraFixed(DVec3),
}

impl LatticeKey {
    pub fn kind(&self) -> KeyKind {
        match *self {
            LatticeKey::Geographic(_) => KeyKind::Geographic,
            LatticeKey::Tracked { plate_id, .. } => KeyKind::Tracked(plate_id),
            LatticeKey::EraFixed(_) => KeyKind::EraFixed,
        }
    }
}

/// A [`LatticeKey`] without its position: what a coarse node passes on to the texels near it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Geographic,
    Tracked(PlateId),
    EraFixed,
}

/// The coarse truth at one era-frame direction.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CoarseSample {
    /// Effective elevation (m) relative to the era's sea level; positive is land.
    pub elevation_m: f64,
    /// Ice fraction in [0, 1].
    pub ice: f32,
    /// Present-day Köppen-Geiger code (1–30) where the data gives one.
    pub koppen: Option<u8>,
    pub lattice: LatticeKey,
}

/// A coarse field: the data a region amplifies.
pub trait CoarseField: Sync {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample;

    /// Spacing (m) of the underlying source samples: detail adds only finer scales.
    fn source_spacing_m(&self) -> f64;

    /// Latitude (degrees) in the era frame. Both the recent (geographic) and deep (era) frames
    /// put north at render +Y.
    fn paleolat(&self, era_dir: RenderDir) -> f64 {
        era_dir.0.normalize().y.clamp(-1.0, 1.0).asin().to_degrees()
    }

    /// The detail key at `era_dir`, given the kind of key of the nearest coarse node (which
    /// decides the plate). The default covers geographic and era-fixed keys; a field that
    /// produces tracked keys overrides it to rotate `era_dir` into the plate's fixed frame.
    fn lattice_key(&self, era_dir: RenderDir, nearest: KeyKind) -> LatticeKey {
        match nearest {
            KeyKind::Geographic => LatticeKey::Geographic(era_dir.0),
            KeyKind::Tracked(_) | KeyKind::EraFixed => LatticeKey::EraFixed(era_dir.0),
        }
    }
}

/// Why a field could not be built from its frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldError {
    /// A frame grid is not a square cube map with the channels its source needs. From
    /// [`snapshot_field`], `frame` is the timeline index; from a field's own constructor, the
    /// position among its arguments (0 younger, 1 older, 2 present).
    BadFrame { frame: usize },
}

impl fmt::Display for FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldError::BadFrame { frame } => write!(
                f,
                "frame {frame} is not a square cube map with the channels its source needs"
            ),
        }
    }
}

impl std::error::Error for FieldError {}

fn check_cube(grid: &Grid, channels: u8, frame: usize) -> Result<(), FieldError> {
    let ok = grid.kind == GridKind::Cube
        && grid.width == grid.height
        && grid.width > 0
        && grid.channels >= channels;
    ok.then_some(()).ok_or(FieldError::BadFrame { frame })
}

/// The arc (m) of a cube texel at a face centre: a cube frame's source spacing.
pub fn cube_spacing_m(cube_size: u32) -> f64 {
    FRAC_PI_2 / f64::from(cube_size.max(1)) * EARTH_RADIUS_M
}

/// Seamless bilinear values of the given channels of a cube grid at `dir`.
fn cube_bilinear<const N: usize>(grid: &Grid, dir: RenderDir, channels: [u8; N]) -> [f64; N] {
    let mut out = [0.0; N];
    for ((face, col, row), w) in bilinear_taps(dir, grid.height) {
        if w == 0.0 {
            continue;
        }
        for (k, &ch) in channels.iter().enumerate() {
            out[k] += w * f64::from(grid.texel_f32(face as usize, row, col, ch));
        }
    }
    out
}

fn lat_lon(dir: RenderDir) -> (f64, f64) {
    geo_to_latlon(render_to_geo(dir))
}

fn koppen_code(code: u8) -> Option<u8> {
    (1..=30).contains(&code).then_some(code)
}

/// The recent eras (ICE-6G_C, up to 26 ka): `etopo + (Topo_era − Topo_present)`, where
/// `Topo_era` blends the bracketing frames by the older weight (ruling R6). Where a tile is
/// missing the frames' own `Topo_era` stands in, at the cube's resolution.
#[derive(Copy, Clone, Debug)]
pub struct RecentField<'a> {
    tiles: &'a TileSet,
    /// `[younger, older]` ICE-6G frames: R = signed Topo (m), G = land, B = ice fraction.
    era: [&'a Grid; 2],
    older_weight: f64,
    /// The present-day ICE-6G frame (timeline frame 0).
    present: &'a Grid,
    /// Some tile the region needs is missing: report the cube's spacing.
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

impl CoarseField for RecentField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
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
        let (elevation_m, koppen) = match self.tiles.sample(lat, lon) {
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

    fn source_spacing_m(&self) -> f64 {
        if self.fallback {
            cube_spacing_m(self.era[0].height)
        } else {
            TILE_SPACING_M
        }
    }
}

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
    /// bridge frame).
    fn endpoint_sample(&self, e: usize, dir: RenderDir) -> FrameSample {
        let [cube_elevation, coverage] = cube_bilinear(self.frames[e], dir, [0, 1]);
        let elevation_m = if self.etopo[e] {
            let (lat, lon) = lat_lon(dir);
            self.tiles
                .sample(lat, lon)
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
    fn koppen(&self, plate: Option<(PlateId, DVec3)>, era: DVec3) -> Option<u8> {
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
        koppen_code(self.tiles.sample(lat, lon)?.1)
    }
}

impl CoarseField for DeepField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        let era = era_dir.0.normalize();
        // The globe's input as given, so the result is bit-identical to the globe's.
        let surface = rendered_surface(
            self.crust,
            era_dir,
            &self.sampling,
            &self.rotations,
            |e, d| self.endpoint_sample(e, d),
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
                        self.endpoint_sample(0, RenderDir(era)),
                        self.endpoint_sample(1, RenderDir(era)),
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
            koppen: self.koppen(plate, era),
            lattice,
        }
    }

    fn source_spacing_m(&self) -> f64 {
        if !self.fallback && self.etopo_weight() >= 0.5 {
            TILE_SPACING_M
        } else {
            DEEP_SPACING_M
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

/// Between the last ICE-6G frame and the bridge frame: the held last recent result crossfaded
/// into the deep result by `deep_weight` (the globe's `FrameSelection::Bridge`).
#[derive(Clone, Debug)]
pub struct CrossfadeField<'a> {
    pub recent: RecentField<'a>,
    pub deep: DeepField<'a>,
    pub deep_weight: f64,
}

impl CoarseField for CrossfadeField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        let r = self.recent.sample(era_dir);
        let d = self.deep.sample(era_dir);
        let w = self.deep_weight;
        CoarseSample {
            elevation_m: r.elevation_m + (d.elevation_m - r.elevation_m) * w,
            ice: r.ice + (d.ice - r.ice) * w as f32,
            koppen: r.koppen.or(d.koppen),
            // Detail follows the dominant side.
            lattice: if w < 0.5 { r.lattice } else { d.lattice },
        }
    }

    fn source_spacing_m(&self) -> f64 {
        if self.deep_weight < 0.5 {
            self.recent.source_spacing_m()
        } else {
            self.deep.source_spacing_m()
        }
    }

    fn lattice_key(&self, era_dir: RenderDir, nearest: KeyKind) -> LatticeKey {
        if self.deep_weight < 0.5 {
            self.recent.lattice_key(era_dir, nearest)
        } else {
            self.deep.lattice_key(era_dir, nearest)
        }
    }
}

/// The coarse field of a snapshot, as [`Timeline::select_frames`] picks it.
#[derive(Clone, Debug)]
pub enum SnapshotField<'a> {
    Recent(RecentField<'a>),
    Deep(DeepField<'a>),
    Bridge(CrossfadeField<'a>),
}

impl CoarseField for SnapshotField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        match self {
            SnapshotField::Recent(f) => f.sample(era_dir),
            SnapshotField::Deep(f) => f.sample(era_dir),
            SnapshotField::Bridge(f) => f.sample(era_dir),
        }
    }

    fn source_spacing_m(&self) -> f64 {
        match self {
            SnapshotField::Recent(f) => f.source_spacing_m(),
            SnapshotField::Deep(f) => f.source_spacing_m(),
            SnapshotField::Bridge(f) => f.source_spacing_m(),
        }
    }

    fn lattice_key(&self, era_dir: RenderDir, nearest: KeyKind) -> LatticeKey {
        match self {
            SnapshotField::Recent(f) => f.lattice_key(era_dir, nearest),
            SnapshotField::Deep(f) => f.lattice_key(era_dir, nearest),
            SnapshotField::Bridge(f) => f.lattice_key(era_dir, nearest),
        }
    }
}

/// The timeline frames [`snapshot_field`] reads at `age_myr`, ascending: the selection's
/// frames, plus the present-day ICE-6G frame (0) whenever a recent field is involved.
pub fn frames_needed(timeline: &Timeline, age_myr: f64) -> Vec<usize> {
    let mut out = match timeline.select_frames(age_myr) {
        FrameSelection::Recent(b) => vec![0, b.younger, b.older],
        FrameSelection::Deep(d) => vec![d.frames.younger, d.frames.older],
        FrameSelection::Bridge { recent, deep, .. } => {
            vec![0, recent, deep.frames.younger, deep.frames.older]
        }
    };
    out.sort_unstable();
    out.dedup();
    out
}

/// Builds the coarse field for `age_myr`. `frame(i)` returns timeline frame `i`'s CPU grid; it
/// is called only for [`frames_needed`]. `fallback` says some tile the region needs is missing.
#[instrument(level = "debug", skip_all, fields(age_myr), err)]
pub fn snapshot_field<'a>(
    timeline: &Timeline,
    crust: &'a CrustModel,
    tiles: &'a TileSet,
    fallback: bool,
    age_myr: f64,
    frame: impl Fn(usize) -> &'a Grid,
) -> Result<SnapshotField<'a>, FieldError> {
    for i in frames_needed(timeline, age_myr) {
        let channels = match timeline.frames()[i].source {
            FrameSource::Ice6g => 3,
            FrameSource::PaleoDem { .. } => 2,
        };
        check_cube(frame(i), channels, i)?;
    }
    let recent = |b: crate::surface::Bracket| {
        RecentField::new(
            tiles,
            [frame(b.younger), frame(b.older)],
            b.older_weight,
            frame(0),
            fallback,
        )
    };
    let deep = |d: &DeepBracket| {
        DeepField::new(
            crust,
            timeline,
            age_myr,
            d,
            [frame(d.frames.younger), frame(d.frames.older)],
            tiles,
            fallback,
        )
    };
    Ok(match timeline.select_frames(age_myr) {
        FrameSelection::Recent(b) => SnapshotField::Recent(recent(b)?),
        FrameSelection::Deep(d) => SnapshotField::Deep(deep(&d)?),
        FrameSelection::Bridge {
            recent: held,
            deep: d,
            deep_weight,
        } => SnapshotField::Bridge(CrossfadeField {
            recent: recent(crate::surface::Bracket {
                younger: held,
                older: held,
                older_weight: 0.0,
            })?,
            deep: deep(&d)?,
            deep_weight,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worldgen::testkit::*;
    use crate::worldgen::tiles::tests::tile_from;

    fn single_tile(lat: f64, lon: f64, elevation_m: f32, koppen: u8) -> TileSet {
        let (r, c) = crate::worldgen::tiles::tile_of(lat, lon);
        let mut set = TileSet::new();
        set.insert(r, c, tile_from(r, c, |_, _| (elevation_m, koppen)))
            .unwrap();
        set
    }

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

    /// Timeline-indexed frames: ICE-6G placeholders, then PaleoDEMs from `paleo(frame)`.
    fn frames(paleo: impl Fn(usize, RenderDir) -> (f32, bool)) -> Vec<Grid> {
        (0..7)
            .map(|i| {
                if i < 4 {
                    ice_frame(0.0, 0.0)
                } else {
                    paleo_frame(|d| paleo(i, d))
                }
            })
            .collect()
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
    fn bridge_crossfade_blends_recent_into_deep() {
        let crust = rift(0);
        let s = timeline();
        let centre = dir(0.0, 0.0);
        let tiles = single_tile(0.0, 0.0, 100.0, 1);
        // ICE-6G: present Topo 100 m, the last frame (26 ka) 220 m (sea 120 m lower).
        let mut fr = frames(|_, _| (7.0, true));
        fr[0] = ice_frame(100.0, 0.0);
        fr[3] = ice_frame(220.0, 0.5);
        let age = 0.028;
        let field = snapshot_field(&s, &crust, &tiles, false, age, |i| &fr[i]).unwrap();
        let SnapshotField::Bridge(cf) = &field else {
            panic!("{field:?}")
        };
        let w = (age - 0.026076) / (0.03 - 0.026076);
        assert!((cf.deep_weight - w).abs() < 1e-12);
        let recent = 100.0 + 220.0 - 100.0;
        let deep = 100.0 - s.sea_level_anomaly_m(age);
        let got = field.sample(centre);
        let expected = recent + (deep - recent) * w;
        assert!(
            (got.elevation_m - expected).abs() < 1e-6,
            "{got:?} vs {expected}"
        );
        assert!((f64::from(got.ice) - 0.5 * (1.0 - w)).abs() < 1e-6);
        assert_eq!(got.koppen, Some(1));
        assert_eq!(field.source_spacing_m(), 1852.0);
        assert_eq!(frames_needed(&s, age), vec![0, 3, 4]);
        assert_eq!(frames_needed(&s, 0.021), vec![0, 1, 2]);
        assert_eq!(frames_needed(&s, 1.0), vec![4, 5]);
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
                    f.endpoint_sample(e, dd)
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

    #[test]
    fn bad_frames_are_errors_not_panics() {
        let crust = rift(0);
        let s = timeline();
        let tiles = TileSet::new();
        let mut fr = frames(|_, _| (0.0, true));
        fr[5].channels = 1;
        let err = snapshot_field(&s, &crust, &tiles, false, 1.0, |i| &fr[i]).unwrap_err();
        assert_eq!(err, FieldError::BadFrame { frame: 5 });
        fr[0].kind = GridKind::LatLon;
        let err = snapshot_field(&s, &crust, &tiles, false, 0.01, |i| &fr[i]).unwrap_err();
        assert_eq!(err, FieldError::BadFrame { frame: 0 });
    }
}
