//! Plate rotation table as read from a `.tfrot` file. Interpolation and lookups arrive with the
//! plate-motion model.

use super::PlateId;

/// Rotations of every plate at every stored time, in the render frame.
///
/// `quats` and `valid` are stored `[time][plate]`, flattened: entry `(t, p)` is at
/// `t * plate_ids.len() + p`. Quaternions are `[x, y, z, w]`; `valid[i] == false` marks a missing
/// circuit, whose quaternion must not be used.
#[derive(Clone, Debug, PartialEq)]
pub struct RotationTable {
    pub plate_ids: Vec<PlateId>,
    pub times_myr: Vec<f64>,
    pub quats: Vec<[f32; 4]>,
    pub valid: Vec<bool>,
}
