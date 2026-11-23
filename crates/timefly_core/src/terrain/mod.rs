//! Region terrain geometry (design spec §6.3): CDLOD patch selection, the vertex morph, the
//! inner/apron height blend and the curvature drop.
//!
//! Pure functions only. [`morph_vertex`], [`blended_height_m`] and [`curvature_drop_m`] are the
//! Rust reference for the terrain vertex shader (`assets/shaders/terrain.wesl`), which mirrors
//! them operation for operation; their doc comments give the exact formulas.
//!
//! Units: region local metres, `+x` east, `−z` north, `+y` up, sea level at `y = 0`.

mod cdlod;

pub use cdlod::{LodSettings, Patch, morph_factor, morph_vertex, select_patches};

use crate::worldgen::frame::EARTH_RADIUS_M;
use crate::worldgen::generate::{Field, RegionGrid};

/// Quads per patch side (33 × 33 vertices).
pub const PATCH_QUADS: u32 = 32;
/// Where a ring's morph starts, as a fraction of the ring (see [`LodSettings::morph_range_m`]).
pub const MORPH_START: f32 = 0.70;
/// Where a ring's morph ends, as a fraction of the ring.
pub const MORPH_END: f32 = 0.98;
/// Apron texels inside the inner field's edge over which the inner field fades into the apron.
pub const BLEND_APRON_TEXELS: f64 = 4.0;

/// Terrain height (m above sea level) at local `(x, z)` metres: the inner field near the centre,
/// the apron outside it, and a smoothstep between the two.
///
/// Exact formula (mirrored in `terrain.wesl`):
/// - `e1 = inner.size_m / 2`, `e0 = e1 − 4 × apron.spacing_m()` (131,250 m at the standard
///   resolutions), `a = max(|x|, |z|)`;
/// - `s = smoothstep(e0, e1, a)` with `t = clamp((a − e0) / (e1 − e0), 0, 1)` and
///   `s = t² (3 − 2t)`, computed in f64 and rounded to f32;
/// - `hi = bilinear(inner, x, z)`, `ha = bilinear(apron, x, z)`, each as [`Field::height_at`]
///   but with the texel coordinate clamped to the field (see `sample_clamped`), so both are
///   defined everywhere;
/// - `h = hi + (ha − hi) × s` in f32.
///
/// So `h` is the inner field exactly for `a ≤ e0` and the apron (to f32 rounding) for `a ≥ e1`;
/// beyond the apron it holds the apron's edge value.
pub fn blended_height_m(grid: &RegionGrid, x_m: f64, z_m: f64) -> f32 {
    let e1 = grid.inner.size_m / 2.0;
    let e0 = e1 - BLEND_APRON_TEXELS * grid.apron.spacing_m();
    let s = smoothstep(e0, e1, x_m.abs().max(z_m.abs())) as f32;
    let hi = sample_clamped(&grid.inner, x_m, z_m);
    let ha = sample_clamped(&grid.apron, x_m, z_m);
    hi + (ha - hi) * s
}

/// How far the ground has dropped below the tangent plane at a horizontal distance from the
/// camera: `d² / (2R)` with `R = 6,371,000 m` (the small-angle sagitta).
pub fn curvature_drop_m(horizontal_distance_m: f64) -> f64 {
    horizontal_distance_m * horizontal_distance_m / (2.0 * EARTH_RADIUS_M)
}

/// `t = clamp((x − e0) / (e1 − e0), 0, 1)`, then `t² (3 − 2t)`: WGSL's `smoothstep`.
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Bilinear height at local `(x, z)` metres with the texel coordinate clamped to the field:
/// `u = clamp((x + size/2) / spacing, 0, res − 1)`, `c0 = min(floor(u), res − 2)`, `fu = u − c0`
/// (f64, then the weight rounded to f32), likewise `v`, `r0`, `fv` from `z`; then in f32
/// `top = h(r0, c0) + (h(r0, c0+1) − h(r0, c0)) × fu`, `bottom` the same on row `r0 + 1`, and
/// `top + (bottom − top) × fv`. Inside the field this is [`Field::height_at`].
fn sample_clamped(field: &Field, x_m: f64, z_m: f64) -> f32 {
    let last = f64::from(field.res - 1);
    let spacing = field.spacing_m();
    let u = ((x_m + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let v = ((z_m + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let (c0, r0) = (u.floor().min(last - 1.0), v.floor().min(last - 1.0));
    let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
    let (c0, r0) = (c0 as usize, r0 as usize);
    let n = field.res as usize;
    let h = |r: usize, c: usize| field.elevation_m[r * n + c];
    let top = h(r0, c0) + (h(r0, c0 + 1) - h(r0, c0)) * fu;
    let bottom = h(r0 + 1, c0) + (h(r0 + 1, c0 + 1) - h(r0 + 1, c0)) * fu;
    top + (bottom - top) * fv
}

#[cfg(test)]
mod tests;
