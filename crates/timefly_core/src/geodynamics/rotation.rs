//! Plate rotation table as read from a `.tfrot` file, with interpolation between samples.

use bevy_math::Quat;

use super::{PlateId, PlateIndex};

/// Rotations of every plate at every stored time, in the render frame.
///
/// `quats` and `valid` are stored `[time][plate]`, flattened: entry `(t, p)` is at
/// `t * plate_ids.len() + p`. Quaternions are `[x, y, z, w]` and rotate a plate-fixed direction
/// into the era frame; `valid[i] == false` marks a missing circuit, whose quaternion must not be
/// used. The bake samples every time at which any plate's validity changes, so two adjacent valid
/// samples always belong to one continuous span.
#[derive(Clone, Debug, PartialEq)]
pub struct RotationTable {
    pub plate_ids: Vec<PlateId>,
    pub times_myr: Vec<f64>,
    pub quats: Vec<[f32; 4]>,
    pub valid: Vec<bool>,
}

impl RotationTable {
    /// Dense index of a source plate ID in this table.
    pub fn index_of(&self, plate_id: PlateId) -> Option<PlateIndex> {
        self.plate_ids
            .iter()
            .position(|&p| p == plate_id)
            .map(|i| PlateIndex(i as u32))
    }

    /// The plate's rotation at `age_myr`: an exact valid sample, or a slerp between the two
    /// bracketing samples when both are valid. `None` for a missing circuit on either side, an
    /// age outside the table (or NaN), or an unknown index.
    pub fn rotation_at(&self, plate_index: PlateIndex, age_myr: f64) -> Option<Quat> {
        let plates = self.plate_ids.len();
        let plate = plate_index.0 as usize;
        if plate >= plates {
            return None;
        }
        let times = &self.times_myr;
        let (&first, &last) = (times.first()?, times.last()?);
        if !(age_myr >= first && age_myr <= last) {
            return None;
        }
        let sample = |t: usize| {
            let i = t * plates + plate;
            match self.valid.get(i) {
                Some(true) => self.quats.get(i).map(|q| Quat::from_array(*q)),
                _ => None,
            }
        };
        let hi = times.partition_point(|&t| t < age_myr); // first time >= age
        if times[hi] == age_myr {
            return sample(hi);
        }
        let (a, b) = (sample(hi - 1)?, sample(hi)?);
        let s = (age_myr - times[hi - 1]) / (times[hi] - times[hi - 1]);
        Some(a.slerp(b, s as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_math::Vec3;

    /// A table from `times` and, per plate, one optional rotation per time (`None` = missing).
    fn table(times: &[f64], plates: &[(u32, Vec<Option<Quat>>)]) -> RotationTable {
        let mut quats = Vec::new();
        let mut valid = Vec::new();
        for t in 0..times.len() {
            for (_, samples) in plates {
                let q = samples[t];
                quats.push(q.unwrap_or(Quat::IDENTITY).to_array());
                valid.push(q.is_some());
            }
        }
        RotationTable {
            plate_ids: plates.iter().map(|(id, _)| PlateId(*id)).collect(),
            times_myr: times.to_vec(),
            quats,
            valid,
        }
    }

    fn about_y(deg: f32) -> Quat {
        Quat::from_rotation_y(deg.to_radians())
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-6
    }

    #[test]
    fn index_of_maps_source_ids_to_dense_indices() {
        let t = table(
            &[0.0],
            &[(701, vec![Some(Quat::IDENTITY)]), (101, vec![None])],
        );
        assert_eq!(t.index_of(PlateId(701)), Some(PlateIndex(0)));
        assert_eq!(t.index_of(PlateId(101)), Some(PlateIndex(1)));
        assert_eq!(t.index_of(PlateId(999)), None);
    }

    #[test]
    fn known_90_degree_rotation_and_midpoint_slerp() {
        let t = table(
            &[0.0, 10.0],
            &[(101, vec![Some(Quat::IDENTITY), Some(about_y(90.0))])],
        );
        let p = PlateIndex(0);
        let q10 = t.rotation_at(p, 10.0).unwrap();
        assert!(close(q10 * Vec3::Z, Vec3::X)); // +90 deg about +Y takes +Z to +X
        let q5 = t.rotation_at(p, 5.0).unwrap();
        let h = std::f32::consts::FRAC_1_SQRT_2;
        assert!(close(q5 * Vec3::Z, Vec3::new(h, 0.0, h)));
        let q2 = t.rotation_at(p, 2.5).unwrap();
        let a = 22.5f32.to_radians();
        assert!(close(q2 * Vec3::Z, Vec3::new(a.sin(), 0.0, a.cos())));
    }

    #[test]
    fn exact_valid_identity_is_a_rotation_not_a_gap() {
        let t = table(
            &[0.0, 10.0],
            &[(101, vec![Some(Quat::IDENTITY), Some(Quat::IDENTITY)])],
        );
        assert_eq!(t.rotation_at(PlateIndex(0), 0.0), Some(Quat::IDENTITY));
        assert_eq!(t.rotation_at(PlateIndex(0), 10.0), Some(Quat::IDENTITY));
        let mid = t.rotation_at(PlateIndex(0), 5.0).unwrap();
        assert!(close(mid * Vec3::X, Vec3::X));
    }

    #[test]
    fn invalid_sample_is_missing_even_with_an_identity_quaternion() {
        let t = table(&[0.0, 10.0], &[(101, vec![None, Some(Quat::IDENTITY)])]);
        assert_eq!(t.rotation_at(PlateIndex(0), 0.0), None);
        assert_eq!(t.rotation_at(PlateIndex(0), 10.0), Some(Quat::IDENTITY));
    }

    #[test]
    fn no_interpolation_across_a_missing_span() {
        let t = table(
            &[0.0, 10.0, 20.0, 30.0],
            &[(
                101,
                vec![
                    Some(Quat::IDENTITY),
                    None,
                    Some(about_y(40.0)),
                    Some(about_y(60.0)),
                ],
            )],
        );
        let p = PlateIndex(0);
        assert!(t.rotation_at(p, 0.0).is_some());
        for age in [0.001, 5.0, 10.0, 15.0, 19.999] {
            assert_eq!(t.rotation_at(p, age), None, "age {age}");
        }
        assert!(t.rotation_at(p, 20.0).is_some());
        let q = t.rotation_at(p, 25.0).unwrap(); // one continuous span: 20..30
        assert!(close(q * Vec3::Z, about_y(50.0) * Vec3::Z));
    }

    #[test]
    fn out_of_range_age_or_plate_is_missing() {
        let t = table(
            &[0.0, 10.0],
            &[(101, vec![Some(Quat::IDENTITY), Some(Quat::IDENTITY)])],
        );
        let p = PlateIndex(0);
        for age in [-0.5, 10.5, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(t.rotation_at(p, age), None, "age {age}");
        }
        assert_eq!(t.rotation_at(PlateIndex(1), 5.0), None);
    }
}
