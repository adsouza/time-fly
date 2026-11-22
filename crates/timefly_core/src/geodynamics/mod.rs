//! Plate identity types shared by the bake output and the runtime. Forward and inverse lookups
//! arrive with the plate-motion model.

pub mod crust;
pub mod rotation;

pub use crust::{FootprintEntry, PlateFootprints};
pub use rotation::RotationTable;

/// Stable source identifier of a plate, constant across bakes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlateId(pub u32);

/// Dense index of a plate within one bake; not stable across bakes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PlateIndex(pub u32);

/// A position on the crust, in the plate-fixed frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CrustPos {
    pub plate_id: PlateId,
    pub lat_deg: f64,
    pub lon_deg: f64,
}

impl CrustPos {
    /// Builds a position with longitude canonicalized to (-180, 180], and zero at the poles.
    pub fn new(plate_id: PlateId, lat_deg: f64, lon_deg: f64) -> CrustPos {
        let lon_deg = if lat_deg.abs() >= 90.0 {
            0.0
        } else {
            let wrapped = (lon_deg + 180.0).rem_euclid(360.0) - 180.0; // [-180, 180)
            if wrapped == -180.0 { 180.0 } else { wrapped }
        };
        CrustPos {
            plate_id,
            lat_deg,
            lon_deg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crust_pos_canonicalizes_longitude() {
        let p = |lat, lon| CrustPos::new(PlateId(101), lat, lon).lon_deg;
        assert_eq!(p(10.0, -180.0), 180.0);
        assert_eq!(p(10.0, 190.0), -170.0);
        assert_eq!(p(10.0, 540.0), 180.0);
        assert_eq!(p(10.0, 0.0), 0.0);
        assert_eq!(p(90.0, 45.0), 0.0);
        assert_eq!(p(-90.0, -45.0), 0.0);
    }
}
