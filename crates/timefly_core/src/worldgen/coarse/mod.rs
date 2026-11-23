//! Coarse fields: the real data a region amplifies (design spec §5.2), sampled at era-frame
//! directions. "Elevation" is always effective elevation relative to the era's sea level.
//!
//! - [`RecentField`] (ICE-6G frames, up to 26 ka): ETOPO present-day elevation plus the ICE-6G
//!   change `Topo(era) − Topo(present)`, with the ICE-6G ice fraction. Geographic lookup.
//! - [`DeepField`] (a PaleoDEM bracket, 30 ka on): the globe's CPU reference,
//!   [`rendered_surface`](crate::globe::rendered_surface), so a region agrees with the globe by construction. The modern-terrain
//!   bridge frame reads the ETOPO tiles instead of its cube frame (ruling R5).
//! - [`CrossfadeField`] (between the last ICE-6G frame and the bridge frame): the held last
//!   recent result crossfaded into the deep one, as the globe's `FrameSelection::Bridge` does.
//!
//! [`snapshot_field`] picks the right one for an age from [`Timeline::select_frames`].

mod deep;
mod modern;
mod recent;

pub use deep::DeepField;
pub use recent::RecentField;

use std::f64::consts::FRAC_PI_2;
use std::fmt;

use bevy_math::DVec3;
use tracing::instrument;

use crate::cube::bilinear_taps;
use crate::formats::{Grid, GridKind};
use crate::geo::{RenderDir, geo_to_latlon, render_to_geo};
use crate::geodynamics::{CrustModel, PlateId};
use crate::surface::{DeepBracket, FrameSelection, FrameSource, Timeline};
use crate::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};
use crate::worldgen::tiles::TileSet;

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
    /// The field at full detail: tiles where loaded (inner fields).
    fn sample(&self, era_dir: RenderDir) -> CoarseSample;

    /// The field as the apron sees it (ruling R7): present-day elevation from the overview, never
    /// from tiles, so an apron texel is a function of its direction alone. Fields that read no
    /// tiles keep the default.
    fn sample_overview(&self, era_dir: RenderDir) -> CoarseSample {
        self.sample(era_dir)
    }

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

    /// Every tile the inner field around `frame` reads, sorted. Callers load exactly these,
    /// and set the field's `fallback` when any is missing ([`TileSet::missing`]). The default is
    /// the era-frame square ([`TileSet::needed_for`]); fields whose reads move (a plate's
    /// present-day position in the deep eras) or that read no tiles override it. Independent of
    /// which tiles are loaded.
    fn tiles_needed(&self, frame: &RegionFrame) -> Vec<(u8, u8)> {
        TileSet::needed_for(frame)
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

/// Between the last ICE-6G frame and the bridge frame: the held last recent result crossfaded
/// into the deep result by `deep_weight` (the globe's `FrameSelection::Bridge`).
#[derive(Clone, Debug)]
pub struct CrossfadeField<'a> {
    pub recent: RecentField<'a>,
    pub deep: DeepField<'a>,
    pub deep_weight: f64,
}

impl CrossfadeField<'_> {
    fn blend(&self, r: CoarseSample, d: CoarseSample) -> CoarseSample {
        let w = self.deep_weight;
        CoarseSample {
            elevation_m: r.elevation_m + (d.elevation_m - r.elevation_m) * w,
            ice: r.ice + (d.ice - r.ice) * w as f32,
            koppen: r.koppen.or(d.koppen),
            // Detail follows the dominant side.
            lattice: if w < 0.5 { r.lattice } else { d.lattice },
        }
    }
}

impl CoarseField for CrossfadeField<'_> {
    fn sample(&self, era_dir: RenderDir) -> CoarseSample {
        self.blend(self.recent.sample(era_dir), self.deep.sample(era_dir))
    }

    fn sample_overview(&self, era_dir: RenderDir) -> CoarseSample {
        self.blend(
            self.recent.sample_overview(era_dir),
            self.deep.sample_overview(era_dir),
        )
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

    fn tiles_needed(&self, frame: &RegionFrame) -> Vec<(u8, u8)> {
        let mut out = self.recent.tiles_needed(frame);
        out.extend(self.deep.tiles_needed(frame));
        out.sort_unstable();
        out.dedup();
        out
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

    fn sample_overview(&self, era_dir: RenderDir) -> CoarseSample {
        match self {
            SnapshotField::Recent(f) => f.sample_overview(era_dir),
            SnapshotField::Deep(f) => f.sample_overview(era_dir),
            SnapshotField::Bridge(f) => f.sample_overview(era_dir),
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

    fn tiles_needed(&self, frame: &RegionFrame) -> Vec<(u8, u8)> {
        match self {
            SnapshotField::Recent(f) => f.tiles_needed(frame),
            SnapshotField::Deep(f) => f.tiles_needed(frame),
            SnapshotField::Bridge(f) => f.tiles_needed(frame),
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
/// is called only for [`frames_needed`]. `fallback` says some inner tile the region needs is
/// missing (`tiles.missing(&field.tiles_needed(&frame))` is not empty); the apron's use of the
/// overview is not one. `tiles_needed` does not depend on `fallback` or on the tiles loaded, so
/// a caller can ask a field built with `fallback = false`, load the tiles, then build the field
/// again with the right flag.
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
