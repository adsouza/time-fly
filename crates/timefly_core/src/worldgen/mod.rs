//! Region-world generation (spec §8.3): terrain heights and brush noise for the ground-level view.
//!
//! Pure functions only. `noise` is mirrored one-to-one in `assets/shaders/noise.wesl`.

pub mod noise;
