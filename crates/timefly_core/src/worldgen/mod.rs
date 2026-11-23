//! Region-world generation (spec §8.3): terrain heights and brush noise for the ground-level view.
//!
//! Pure functions only. `noise` is mirrored one-to-one in `assets/shaders/noise.wesl`.

pub mod biome;
pub mod frame;
pub mod noise;

/// Side of a region's inner field, in metres.
pub const REGION_SIZE_M: f64 = 300_000.0;
/// Samples per side of the inner field.
pub const REGION_RES: u32 = 1025;
/// Side of the outer apron, in metres.
pub const APRON_SIZE_M: f64 = 2_400_000.0;
/// Samples per side of the outer apron.
pub const APRON_RES: u32 = 513;
