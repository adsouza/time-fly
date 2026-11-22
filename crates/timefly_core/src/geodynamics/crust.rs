//! Plate footprints and the crust model: which plates cover each cube-map cell and when, and
//! the lookups between plate-fixed crust positions and era-frame directions (spec §5.2).

use std::fmt;

use tracing::instrument;

use super::{CrustPos, PlateId, PlateIndex, RotationTable};
use crate::cube::dir_to_texel;
use crate::geo::{RenderDir, geo_to_latlon, geo_to_render, latlon_to_geo, render_to_geo};

/// Footprint intervals lie within the 540 Ma the model covers.
const MAX_AGE_MYR: f64 = 540.0;
/// How far a valid quaternion's length may stray from 1 (it is stored as `f32`).
const UNIT_TOLERANCE: f32 = 1e-3;

/// One plate's membership of a cell. The interval is closed and measured in Ma (millions of
/// years before present), so `younger_myr <= older_myr`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FootprintEntry {
    pub plate_index: PlateIndex,
    pub younger_myr: f64,
    pub older_myr: f64,
}

/// The closed-interval acceptance rule of a footprint entry: `younger_myr <= age_myr <= older_myr`.
/// False for a NaN age. Shared by [`PlateFootprints::contains`] and the renderer's per-age
/// validity flags, so the CPU and GPU decide boundaries identically (in f64, on the CPU).
pub fn interval_accepts(younger_myr: f64, older_myr: f64, age_myr: f64) -> bool {
    younger_myr <= age_myr && age_myr <= older_myr
}

/// Cube-map cell memberships in compressed-row form: cell `c` (`[face][row][col]` order) owns
/// `entries[offsets[c]..offsets[c + 1]]`, sorted by plate index and then by younger bound.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateFootprints {
    pub cube_size: u32,
    pub offsets: Vec<u32>,
    pub entries: Vec<FootprintEntry>,
}

impl PlateFootprints {
    /// Whether the plate covers `plate_fixed_dir` at `age_myr`.
    ///
    /// The acceptance rule, which the GPU path must mirror exactly: the direction selects one cell
    /// with [`dir_to_texel`] (no filtering, the shared cube tie rule), and the cell accepts the
    /// plate if any of its entries for that plate has `younger_myr <= age_myr <= older_myr`.
    /// A non-finite direction or age is never contained.
    pub fn contains(
        &self,
        plate_index: PlateIndex,
        plate_fixed_dir: RenderDir,
        age_myr: f64,
    ) -> bool {
        let n = self.cube_size as usize;
        if n == 0 || !plate_fixed_dir.0.is_finite() {
            return false;
        }
        let (face, col, row) = dir_to_texel(plate_fixed_dir, self.cube_size);
        let cell = (face as usize * n + row as usize) * n + col as usize;
        let (Some(&start), Some(&end)) = (self.offsets.get(cell), self.offsets.get(cell + 1))
        else {
            return false;
        };
        self.entries
            .get(start as usize..end as usize)
            .is_some_and(|cell| {
                cell.iter().any(|e| {
                    e.plate_index == plate_index
                        && interval_accepts(e.younger_myr, e.older_myr, age_myr)
                })
            })
    }
}

/// Why a [`CrustModel`] could not be built.
#[derive(Clone, Debug, PartialEq)]
pub enum CrustModelError {
    /// A footprint names a plate index beyond the rotation table.
    MissingPlate { plate_index: PlateIndex },
    /// The rotation table breaks a structural rule.
    Rotations(&'static str),
    /// The footprints break a structural rule.
    Footprints(&'static str),
}

impl fmt::Display for CrustModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CrustModelError::MissingPlate { plate_index } => write!(
                f,
                "footprint plate index {} is not in the rotation table",
                plate_index.0
            ),
            CrustModelError::Rotations(why) => write!(f, "invalid rotation table: {why}"),
            CrustModelError::Footprints(why) => write!(f, "invalid plate footprints: {why}"),
        }
    }
}

impl std::error::Error for CrustModelError {}

/// A plate that passed the rotation and footprint checks for a direction, offered to the
/// surface callback of [`CrustModel::resolve`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Candidate {
    pub plate_id: PlateId,
    pub plate_index: PlateIndex,
    /// The direction in this plate's fixed frame.
    pub plate_fixed_dir: RenderDir,
}

/// Where an era-frame direction lies: on tracked crust, or at an era-fixed point where no plate
/// is valid (keyed by snapshot age and canonical geographic coordinates).
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ResolvedPosition {
    Tracked(CrustPos),
    EraFixed {
        age_myr: f64,
        lat_deg: f64,
        lon_deg: f64,
    },
}

/// Which rotation orients the lookup; validity is always evaluated at the requested age.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Orientation {
    /// The rotation at the requested age (the deep-time globe and its worlds).
    Era,
    /// The age-0 rotation (the fixed recent globe in geographic coordinates).
    Present,
}

/// Rotations and footprints checked against each other; the only way to look positions up.
#[derive(Clone, Debug)]
pub struct CrustModel {
    rotations: RotationTable,
    footprints: PlateFootprints,
    /// Dense indices in ascending source-ID order: the stable candidate order.
    by_plate_id: Vec<PlateIndex>,
}

impl CrustModel {
    /// Validates both tables and that every footprint plate index exists in the rotation table.
    #[instrument(
        level = "debug",
        skip_all,
        fields(
            plates = rotations.plate_ids.len(),
            times = rotations.times_myr.len(),
            cube_size = footprints.cube_size,
            entries = footprints.entries.len()
        ),
        err
    )]
    pub fn new(
        rotations: RotationTable,
        footprints: PlateFootprints,
    ) -> Result<CrustModel, CrustModelError> {
        check_rotations(&rotations)?;
        check_footprints(&footprints)?;
        let plates = rotations.plate_ids.len();
        if let Some(e) = footprints
            .entries
            .iter()
            .find(|e| e.plate_index.0 as usize >= plates)
        {
            return Err(CrustModelError::MissingPlate {
                plate_index: e.plate_index,
            });
        }
        let mut by_plate_id: Vec<PlateIndex> = (0..plates as u32).map(PlateIndex).collect();
        by_plate_id.sort_by_key(|i| rotations.plate_ids[i.0 as usize]);
        Ok(CrustModel {
            rotations,
            footprints,
            by_plate_id,
        })
    }

    pub fn rotations(&self) -> &RotationTable {
        &self.rotations
    }

    pub fn footprints(&self) -> &PlateFootprints {
        &self.footprints
    }

    /// The era-frame direction of a crust position at `age_myr`, or `None` unless the plate has
    /// both a valid rotation and a footprint covering the position at that age.
    pub fn forward(&self, pos: CrustPos, age_myr: f64) -> Option<RenderDir> {
        self.forward_oriented(pos, age_myr, Orientation::Era)
    }

    /// [`forward`](Self::forward) for the fixed recent globe: oriented by the age-0 rotation,
    /// with validity still evaluated at `age_myr`.
    pub fn forward_recent(&self, pos: CrustPos, age_myr: f64) -> Option<RenderDir> {
        self.forward_oriented(pos, age_myr, Orientation::Present)
    }

    /// Resolves an era-frame direction at `age_myr` to tracked crust or an era-fixed point.
    ///
    /// Every plate with a valid rotation is tried in ascending source-ID order: the direction is
    /// inverse-rotated into the plate's fixed frame and kept if the plate's footprint covers it.
    /// `surface_for_candidate` returns each candidate's visible land elevation (`None`, or NaN,
    /// for no visible land), sampled with the globe's frame selection and sampling rules. The
    /// highest visible land wins, ties going to the lower source ID; without visible land the
    /// lowest valid source ID wins. With no candidate the result is era-fixed.
    pub fn resolve(
        &self,
        era_dir: RenderDir,
        age_myr: f64,
        surface_for_candidate: impl FnMut(&Candidate) -> Option<f64>,
    ) -> ResolvedPosition {
        self.resolve_oriented(era_dir, age_myr, surface_for_candidate, Orientation::Era)
    }

    /// [`resolve`](Self::resolve) for the fixed recent globe: oriented by the age-0 rotations,
    /// with validity still evaluated at `age_myr`. The geographic surface is common to all
    /// candidates there, so the callback normally returns the same value for each.
    pub fn resolve_recent(
        &self,
        geo_dir: RenderDir,
        age_myr: f64,
        surface_for_candidate: impl FnMut(&Candidate) -> Option<f64>,
    ) -> ResolvedPosition {
        self.resolve_oriented(
            geo_dir,
            age_myr,
            surface_for_candidate,
            Orientation::Present,
        )
    }

    /// The orienting rotation of a plate, provided it is valid at `age_myr`.
    fn orientation(
        &self,
        plate_index: PlateIndex,
        age_myr: f64,
        orientation: Orientation,
    ) -> Option<bevy_math::DQuat> {
        let at_age = self.rotations.rotation_at(plate_index, age_myr)?;
        let q = match orientation {
            Orientation::Era => at_age,
            Orientation::Present => self.rotations.rotation_at(plate_index, 0.0)?,
        };
        Some(q.as_dquat().normalize())
    }

    fn forward_oriented(
        &self,
        pos: CrustPos,
        age_myr: f64,
        orientation: Orientation,
    ) -> Option<RenderDir> {
        let index = self.rotations.index_of(pos.plate_id)?;
        let q = self.orientation(index, age_myr, orientation)?;
        let fixed = geo_to_render(latlon_to_geo(pos.lat_deg, pos.lon_deg));
        if !self.footprints.contains(index, fixed, age_myr) {
            return None;
        }
        Some(RenderDir((q * fixed.0).normalize()))
    }

    fn resolve_oriented(
        &self,
        era_dir: RenderDir,
        age_myr: f64,
        mut surface_for_candidate: impl FnMut(&Candidate) -> Option<f64>,
        orientation: Orientation,
    ) -> ResolvedPosition {
        let era = era_dir.0.normalize();
        // (candidate, visible land elevation); replaced only by a strictly better candidate.
        let mut best: Option<(Candidate, Option<f64>)> = None;
        for &plate_index in &self.by_plate_id {
            let Some(q) = self.orientation(plate_index, age_myr, orientation) else {
                continue;
            };
            let plate_fixed_dir = RenderDir((q.inverse() * era).normalize());
            if !self
                .footprints
                .contains(plate_index, plate_fixed_dir, age_myr)
            {
                continue;
            }
            let candidate = Candidate {
                plate_id: self.rotations.plate_ids[plate_index.0 as usize],
                plate_index,
                plate_fixed_dir,
            };
            let land = surface_for_candidate(&candidate).filter(|h| !h.is_nan());
            let better = match (&best, land) {
                (None, _) => true,
                (Some((_, None)), Some(_)) => true,
                (Some((_, Some(top))), Some(h)) => h > *top,
                _ => false,
            };
            if better {
                best = Some((candidate, land));
            }
        }
        match best {
            Some((c, _)) => {
                let (lat, lon) = geo_to_latlon(render_to_geo(c.plate_fixed_dir));
                ResolvedPosition::Tracked(CrustPos::new(c.plate_id, lat, lon))
            }
            None => {
                let (lat_deg, lon_deg) = geo_to_latlon(render_to_geo(RenderDir(era)));
                ResolvedPosition::EraFixed {
                    age_myr,
                    lat_deg,
                    lon_deg,
                }
            }
        }
    }
}

fn check_rotations(r: &RotationTable) -> Result<(), CrustModelError> {
    let bad = CrustModelError::Rotations;
    let records = r
        .times_myr
        .len()
        .checked_mul(r.plate_ids.len())
        .ok_or(bad("size overflow"))?;
    if r.quats.len() != records || r.valid.len() != records {
        return Err(bad(
            "quats and valid must have one record per time and plate",
        ));
    }
    if r.times_myr.first() != Some(&0.0)
        || !r.times_myr.iter().all(|t| t.is_finite())
        || r.times_myr.windows(2).any(|w| w[0] >= w[1])
    {
        return Err(bad(
            "times must be finite, strictly ascending and start at 0",
        ));
    }
    let mut ids: Vec<PlateId> = r.plate_ids.clone();
    ids.sort_unstable();
    if ids.windows(2).any(|w| w[0] == w[1]) {
        return Err(bad("duplicate plate id"));
    }
    let unit = |q: &[f32; 4]| {
        q.iter().all(|v| v.is_finite())
            && (bevy_math::Quat::from_array(*q).length() - 1.0).abs() <= UNIT_TOLERANCE
    };
    if r.quats.iter().zip(&r.valid).any(|(q, &ok)| ok && !unit(q)) {
        return Err(bad("valid quaternion is not unit length"));
    }
    Ok(())
}

fn check_footprints(f: &PlateFootprints) -> Result<(), CrustModelError> {
    let bad = CrustModelError::Footprints;
    if f.cube_size == 0 {
        return Err(bad("zero cube size"));
    }
    let n = f.cube_size as usize;
    let cells = n
        .checked_mul(n)
        .and_then(|c| c.checked_mul(6))
        .ok_or(bad("size overflow"))?;
    if f.offsets.len() != cells + 1
        || f.offsets[0] != 0
        || f.offsets[cells] as usize != f.entries.len()
        || f.offsets.windows(2).any(|w| w[0] > w[1])
    {
        return Err(bad(
            "offsets must cover every cell, start at 0, end at the entry count and not decrease",
        ));
    }
    let in_range = |v: f64| (0.0..=MAX_AGE_MYR).contains(&v); // false for NaN
    if f.entries
        .iter()
        .any(|e| !in_range(e.younger_myr) || !in_range(e.older_myr) || e.younger_myr > e.older_myr)
    {
        return Err(bad("interval outside 0..=540 Ma or reversed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cube::{CubeFace, texel_center_dir};
    use crate::geo::{RenderDir, geo_to_render, latlon_to_geo};
    use crate::geodynamics::{CrustPos, PlateId, RotationTable};
    use bevy_math::{DVec3, Quat};

    const N: u32 = 8;
    const TIMES: [f64; 4] = [0.0, 10.0, 20.0, 30.0];

    /// Plates of the test model, in dense-index order (deliberately not sorted by source ID):
    ///
    /// | index | id  | rotation                                  | footprint (plate-fixed)       |
    /// |-------|-----|-------------------------------------------|-------------------------------|
    /// | 0     | 500 | 0 deg at 0 Ma, +90 deg about +Y from 10 Ma | `abs(z) > 0.5`, 0-540 Ma      |
    /// | 1     | 200 | identity                                  | `y > 0.5`, 0-540 Ma           |
    /// | 2     | 100 | identity                                  | `y > 0.5`, 0-540 Ma           |
    /// | 3     | 50  | identity, missing circuit from 20 Ma      | `y > 0.5`, 0-540 Ma           |
    /// | 4     | 700 | identity                                  | `y < -0.5`, 0-10 and 20-30 Ma |
    fn model() -> CrustModel {
        let ids = [500, 200, 100, 50, 700];
        let mut quats = Vec::new();
        let mut valid = Vec::new();
        for &t in &TIMES {
            for (i, _) in ids.iter().enumerate() {
                let q = if i == 0 && t >= 10.0 {
                    Quat::from_rotation_y(90f32.to_radians())
                } else {
                    Quat::IDENTITY
                };
                quats.push(q.to_array());
                valid.push(!(i == 3 && t >= 20.0));
            }
        }
        let rotations = RotationTable {
            plate_ids: ids.iter().map(|&id| PlateId(id)).collect(),
            times_myr: TIMES.to_vec(),
            quats,
            valid,
        };
        let footprints = footprints(N, |d| {
            let mut e = Vec::new();
            if d.z.abs() > 0.5 {
                e.push((0, 0.0, 540.0));
            }
            if d.y > 0.5 {
                e.extend([(1, 0.0, 540.0), (2, 0.0, 540.0), (3, 0.0, 540.0)]);
            }
            if d.y < -0.5 {
                e.extend([(4, 0.0, 10.0), (4, 20.0, 30.0)]);
            }
            e
        });
        CrustModel::new(rotations, footprints).unwrap()
    }

    /// Footprints whose cell memberships are `cell(texel centre)`, as `(index, younger, older)`.
    fn footprints(n: u32, cell: impl Fn(DVec3) -> Vec<(u32, f64, f64)>) -> PlateFootprints {
        let mut offsets = vec![0];
        let mut entries = Vec::new();
        for face in CubeFace::ALL {
            for row in 0..n {
                for col in 0..n {
                    for (i, y, o) in cell(texel_center_dir(face, col, row, n).0) {
                        entries.push(FootprintEntry {
                            plate_index: PlateIndex(i),
                            younger_myr: y,
                            older_myr: o,
                        });
                    }
                    offsets.push(entries.len() as u32);
                }
            }
        }
        PlateFootprints {
            cube_size: n,
            offsets,
            entries,
        }
    }

    fn dir(lat: f64, lon: f64) -> RenderDir {
        geo_to_render(latlon_to_geo(lat, lon))
    }

    fn no_land(_: &Candidate) -> Option<f64> {
        None
    }

    fn tracked_id(r: ResolvedPosition) -> u32 {
        match r {
            ResolvedPosition::Tracked(p) => p.plate_id.0,
            other => panic!("expected tracked crust, got {other:?}"),
        }
    }

    fn rotations_with(n_plates: usize) -> RotationTable {
        RotationTable {
            plate_ids: (0..n_plates as u32).map(PlateId).collect(),
            times_myr: vec![0.0],
            quats: vec![[0.0, 0.0, 0.0, 1.0]; n_plates],
            valid: vec![true; n_plates],
        }
    }

    #[test]
    fn new_rejects_a_footprint_index_missing_from_the_rotation_table() {
        let fits = footprints(1, |_| vec![(1, 0.0, 10.0)]);
        assert!(CrustModel::new(rotations_with(2), fits.clone()).is_ok());
        assert_eq!(
            CrustModel::new(rotations_with(1), fits).err(),
            Some(CrustModelError::MissingPlate {
                plate_index: PlateIndex(1)
            })
        );
    }

    #[test]
    fn new_rejects_malformed_tables() {
        let fp = || footprints(1, |_| vec![]);
        let mut short = rotations_with(2);
        short.valid.pop();
        assert!(CrustModel::new(short, fp()).is_err());
        let mut unsorted = rotations_with(1);
        unsorted.times_myr = vec![0.0, 5.0, 5.0];
        unsorted.quats = vec![[0.0, 0.0, 0.0, 1.0]; 3];
        unsorted.valid = vec![true; 3];
        assert!(CrustModel::new(unsorted, fp()).is_err());
        let mut not_unit = rotations_with(1);
        not_unit.quats[0] = [0.0, 0.0, 0.0, 2.0];
        assert!(CrustModel::new(not_unit, fp()).is_err());
        let mut dup = rotations_with(2);
        dup.plate_ids = vec![PlateId(7), PlateId(7)];
        assert!(CrustModel::new(dup, fp()).is_err());
        let mut bad_offsets = fp();
        bad_offsets.offsets.pop();
        assert!(CrustModel::new(rotations_with(1), bad_offsets).is_err());
        let reversed = footprints(1, |_| vec![(0, 20.0, 10.0)]);
        assert!(CrustModel::new(rotations_with(1), reversed).is_err());
    }

    #[test]
    fn contains_tests_cell_membership_and_closed_intervals_with_a_gap() {
        // cube_size 1: one cell per face. +X cell: plate 0 over 0-10 and 20-30 Ma, plate 2 always.
        let fp = footprints(1, |d| {
            if d.x > 0.9 {
                vec![(0, 0.0, 10.0), (0, 20.0, 30.0), (2, 0.0, 540.0)]
            } else {
                vec![]
            }
        });
        let x = RenderDir(DVec3::X);
        let p0 = PlateIndex(0);
        for (age, inside) in [
            (0.0, true),
            (5.0, true),
            (10.0, true),
            (15.0, false),
            (20.0, true),
            (25.0, true),
            (30.0, true),
            (31.0, false),
            (f64::NAN, false),
        ] {
            assert_eq!(fp.contains(p0, x, age), inside, "age {age}");
        }
        assert!(fp.contains(PlateIndex(2), x, 15.0));
        assert!(!fp.contains(PlateIndex(1), x, 5.0));
        assert!(!fp.contains(p0, RenderDir(-DVec3::X), 5.0));
    }

    #[test]
    fn non_finite_direction_is_never_contained() {
        let everywhere = footprints(1, |_| vec![(0, 0.0, 540.0)]);
        for d in [DVec3::NAN, DVec3::new(f64::INFINITY, 0.0, 0.0)] {
            assert!(!everywhere.contains(PlateIndex(0), RenderDir(d), 5.0));
        }
        let m = model();
        assert!(matches!(
            m.resolve(RenderDir(DVec3::ZERO), 0.0, no_land),
            ResolvedPosition::EraFixed { .. }
        ));
    }

    #[test]
    fn forward_applies_the_rotation_when_rotation_and_footprint_are_valid() {
        let m = model();
        let pos = CrustPos::new(PlateId(500), 0.0, 0.0); // render +Z
        let at = |age| m.forward(pos, age).unwrap().0;
        assert!((at(0.0) - DVec3::Z).length() < 1e-6);
        assert!((at(10.0) - DVec3::X).length() < 1e-6);
        let h = std::f64::consts::FRAC_1_SQRT_2;
        assert!((at(5.0) - DVec3::new(h, 0.0, h)).length() < 1e-6);
        assert_eq!(m.forward(pos, 31.0), None); // beyond the table
        assert_eq!(m.forward(CrustPos::new(PlateId(999), 0.0, 0.0), 5.0), None);
        assert_eq!(m.forward(CrustPos::new(PlateId(500), 0.0, 90.0), 5.0), None); // off footprint
        assert_eq!(m.forward(CrustPos::new(PlateId(50), 80.0, 0.0), 25.0), None); // missing circuit
    }

    #[test]
    fn forward_and_inverse_round_trip_including_the_antimeridian() {
        let m = model();
        for (lat, lon, age) in [
            (0.0, 10.0, 0.0),
            (10.0, 179.5, 5.0),
            (-5.0, 180.0, 5.0),
            (20.0, -179.0, 20.0),
            (0.0, 180.0, 30.0),
        ] {
            let pos = CrustPos::new(PlateId(500), lat, lon);
            let era = m.forward(pos, age).expect("forward");
            match m.resolve(era, age, no_land) {
                ResolvedPosition::Tracked(back) => {
                    assert_eq!(back.plate_id, PlateId(500));
                    assert!(back.lon_deg > -180.0 && back.lon_deg <= 180.0);
                    let err = (latlon_to_geo(back.lat_deg, back.lon_deg).0
                        - latlon_to_geo(lat, lon).0)
                        .length();
                    assert!(err < 1e-9, "({lat}, {lon}) at {age}: error {err}");
                }
                other => panic!("({lat}, {lon}) at {age}: {other:?}"),
            }
        }
    }

    #[test]
    fn overlap_prefers_the_highest_visible_land() {
        let m = model();
        let north = RenderDir(DVec3::Y);
        let r = m.resolve(north, 25.0, |c| match c.plate_id.0 {
            200 => Some(500.0),
            100 => Some(300.0),
            _ => None,
        });
        assert_eq!(tracked_id(r), 200);
        // Land beats no land, even for a higher source ID.
        let r = m.resolve(north, 5.0, |c| (c.plate_id.0 == 200).then_some(10.0));
        assert_eq!(tracked_id(r), 200);
    }

    #[test]
    fn overlap_ties_break_by_lowest_valid_source_id() {
        let m = model();
        let north = RenderDir(DVec3::Y);
        // Equal visible land: lowest ID wins (50 has no circuit at 25 Ma).
        assert_eq!(tracked_id(m.resolve(north, 25.0, |_| Some(300.0))), 100);
        // No visible land anywhere: lowest valid ID wins.
        assert_eq!(tracked_id(m.resolve(north, 25.0, no_land)), 100);
        assert_eq!(tracked_id(m.resolve(north, 5.0, no_land)), 50);
        // A NaN surface counts as no visible land.
        assert_eq!(
            tracked_id(m.resolve(north, 25.0, |c| (c.plate_id.0 == 200).then_some(f64::NAN))),
            100
        );
    }

    #[test]
    fn candidates_are_offered_in_ascending_source_id_order() {
        let m = model();
        let mut seen = Vec::new();
        m.resolve(RenderDir(DVec3::Y), 5.0, |c| {
            seen.push(c.plate_id.0);
            None
        });
        assert_eq!(seen, vec![50, 100, 200]);
    }

    #[test]
    fn expired_footprint_is_not_tracked() {
        let m = model();
        let pos = CrustPos::new(PlateId(700), -80.0, 30.0);
        assert!(m.forward(pos, 5.0).is_some());
        assert_eq!(m.forward(pos, 15.0), None);
        assert_eq!(
            m.resolve(RenderDir(-DVec3::Y), 15.0, no_land),
            ResolvedPosition::EraFixed {
                age_myr: 15.0,
                lat_deg: -90.0,
                lon_deg: 0.0
            }
        );
    }

    #[test]
    fn footprint_valid_again_after_a_gap() {
        let m = model();
        let pos = CrustPos::new(PlateId(700), -80.0, 30.0);
        assert!(m.forward(pos, 25.0).is_some());
        assert_eq!(
            tracked_id(m.resolve(RenderDir(-DVec3::Y), 25.0, no_land)),
            700
        );
    }

    #[test]
    fn era_fixed_where_no_plate_is_valid() {
        let m = model();
        assert_eq!(
            m.resolve(dir(0.0, -90.0), 0.0, no_land),
            ResolvedPosition::EraFixed {
                age_myr: 0.0,
                lat_deg: 0.0,
                lon_deg: -90.0
            }
        );
        // At 20 Ma plate 500 has turned away from the antimeridian: canonical longitude is 180.
        match m.resolve(dir(0.0, 180.0), 20.0, no_land) {
            ResolvedPosition::EraFixed {
                age_myr,
                lat_deg,
                lon_deg,
            } => {
                assert_eq!(age_myr, 20.0);
                assert!(lat_deg.abs() < 1e-12);
                assert_eq!(lon_deg, 180.0);
            }
            other => panic!("expected era-fixed, got {other:?}"),
        }
    }

    #[test]
    fn overlapping_world_windows_resolve_identical_sample_keys() {
        // Two windows sample one canonical 2-degree lattice; each resolves its own samples.
        let m = model();
        let step = 2.0;
        let lattice_dir = |i: i32, j: i32| {
            dir(
                -90.0 + (f64::from(i) + 0.5) * step,
                -180.0 + (f64::from(j) + 0.5) * step,
            )
        };
        let surface = |c: &Candidate| Some(f64::from(c.plate_id.0 % 3) * 100.0);
        let window = |is: std::ops::Range<i32>, js: std::ops::Range<i32>| {
            let mut out = std::collections::BTreeMap::new();
            for i in is {
                for j in js.clone() {
                    out.insert((i, j), m.resolve(lattice_dir(i, j), 20.0, surface));
                }
            }
            out
        };
        let a = window(40..90, 0..120);
        let b = window(60..90, 30..180);
        let mut kinds = (0, 0);
        for (key, ra) in &a {
            if let Some(rb) = b.get(key) {
                assert_eq!(ra, rb, "lattice point {key:?}");
                match ra {
                    ResolvedPosition::Tracked(_) => kinds.0 += 1,
                    ResolvedPosition::EraFixed { .. } => kinds.1 += 1,
                }
            }
        }
        assert!(
            kinds.0 > 0 && kinds.1 > 0,
            "overlap covers both kinds: {kinds:?}"
        );
    }

    #[test]
    fn recent_globe_uses_present_orientation_and_requested_age_validity() {
        let m = model();
        let pos = CrustPos::new(PlateId(500), 0.0, 0.0);
        let d = m.forward_recent(pos, 20.0).unwrap();
        assert!((d.0 - DVec3::Z).length() < 1e-6); // age-0 orientation
        assert!((m.forward(pos, 20.0).unwrap().0 - DVec3::X).length() < 1e-6);
        let missing = CrustPos::new(PlateId(50), 80.0, 0.0);
        assert!(m.forward_recent(missing, 5.0).is_some());
        assert_eq!(m.forward_recent(missing, 25.0), None); // validity at the requested age
        let expired = CrustPos::new(PlateId(700), -80.0, 30.0);
        assert_eq!(m.forward_recent(expired, 15.0), None);

        let z = RenderDir(DVec3::Z);
        assert_eq!(tracked_id(m.resolve_recent(z, 20.0, no_land)), 500);
        assert!(matches!(
            m.resolve(z, 20.0, no_land),
            ResolvedPosition::EraFixed { .. }
        ));
        assert_eq!(
            tracked_id(m.resolve_recent(RenderDir(DVec3::Y), 25.0, no_land)),
            100
        );
    }
}
