//! Plate footprints as read from a `.tfplates` file: which plates cover each cube-map cell, and
//! over which time intervals. Cell and plate lookups arrive with the plate-motion model.

use super::PlateIndex;

/// One plate's membership of a cell. The interval is closed and measured in Ma (millions of
/// years before present), so `younger_myr <= older_myr`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct FootprintEntry {
    pub plate_index: PlateIndex,
    pub younger_myr: f64,
    pub older_myr: f64,
}

/// Cube-map cell memberships in compressed-row form: cell `c` (`[face][row][col]` order) owns
/// `entries[offsets[c]..offsets[c + 1]]`, sorted by plate index and then by younger bound.
#[derive(Clone, Debug, PartialEq)]
pub struct PlateFootprints {
    pub cube_size: u32,
    pub offsets: Vec<u32>,
    pub entries: Vec<FootprintEntry>,
}
