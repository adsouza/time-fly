//! Region terrain geometry (design spec §6.3): CDLOD patch selection, the vertex morph, the
//! inner/apron height blend and the curvature drop.
//!
//! Pure functions only. [`morph_vertex`], [`blended_height_m`], [`curvature_drop_m`] and
//! [`surface_normal`] are the Rust reference for the terrain shaders (`terrain_cdlod.wesl`),
//! which mirror them operation for operation; their doc comments give the exact formulas.
//!
//! Units: region local metres, `+x` east, `−z` north, `+y` up, sea level at `y = 0`.

mod cdlod;

pub use cdlod::{LodSettings, Patch, morph_factor, morph_vertex, select_patches};

use crate::worldgen::frame::EARTH_RADIUS_M;
use crate::worldgen::generate::water::decode_river;
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
    let s = blend_weight(grid, x_m, z_m);
    let hi = sample_clamped(&grid.inner, x_m, z_m);
    let ha = sample_clamped(&grid.apron, x_m, z_m);
    hi + (ha - hi) * s
}

/// The apron's weight `s` in [`blended_height_m`]'s blend.
fn blend_weight(grid: &RegionGrid, x_m: f64, z_m: f64) -> f32 {
    let e1 = grid.inner.size_m / 2.0;
    let e0 = e1 - BLEND_APRON_TEXELS * grid.apron.spacing_m();
    smoothstep(e0, e1, x_m.abs().max(z_m.abs())) as f32
}

/// The water at a point, as drawn.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct WaterSample {
    /// The water surface (m).
    pub level_m: f32,
    /// `level − `[`blended_height_m`]: positive where the water stands over the ground.
    pub thickness_m: f32,
    /// Positive inside a river's channel: `w/2` minus the distance to its centreline. About
    /// [`RIBBON_NONE_M`](crate::hydrology::params::RIBBON_NONE_M) or more over standing water.
    pub ribbon_m: f32,
    /// The river's flow (m/s), `[east, north]` in the texel's own frame.
    pub flow: [f32; 2],
}

/// The water at local `(x, z)` metres, blended exactly as [`blended_height_m`]: each field's
/// texels are read with the same clamped bilinear weights and the two fields mixed with the
/// same `s`, quantity by quantity. Mirrored by the water shader (Task 8).
///
/// Per field: the water level, the flow, the signed centreline offset `o` and the half-width
/// `hw = ribbon + |o|` of each texel are interpolated (the river texel's f16 values, decoded);
/// after the blend, `ribbon = hw − |o|`. A sub-texel river therefore keeps its true width
/// between texels on either side of it, where interpolating the ribbon itself would lose it
/// (`ribbon` is `w/2 − |o|`, not linear across the centreline).
pub fn blended_water(grid: &RegionGrid, x_m: f64, z_m: f64) -> WaterSample {
    let s = blend_weight(grid, x_m, z_m);
    let read = |f: &Field| {
        let level = sample_field(f, x_m, z_m, |i| f.water_level_m[i]);
        let decoded = |i: usize| decode_river(f.river[i]);
        let hw = sample_field(f, x_m, z_m, |i| {
            let [ribbon, _, _, offset] = decoded(i);
            ribbon + offset.abs()
        });
        let offset = sample_field(f, x_m, z_m, |i| decoded(i)[3]);
        let east = sample_field(f, x_m, z_m, |i| decoded(i)[1]);
        let north = sample_field(f, x_m, z_m, |i| decoded(i)[2]);
        [level, hw, offset, east, north]
    };
    let (i, a) = (read(&grid.inner), read(&grid.apron));
    let mix = |k: usize| i[k] + (a[k] - i[k]) * s;
    let level_m = mix(0);
    WaterSample {
        level_m,
        thickness_m: level_m - blended_height_m(grid, x_m, z_m),
        ribbon_m: mix(1) - mix(2).abs(),
        flow: [mix(3), mix(4)],
    }
}

/// The drawn surface at local `(x, z)` metres: the higher of the ground ([`blended_height_m`])
/// and the water ([`blended_water`]'s level). The fly's floor: it reads only the levels, not the
/// river texels, since the fly steps call it once per substep.
pub fn blended_surface_m(grid: &RegionGrid, x_m: f64, z_m: f64) -> f32 {
    let s = blend_weight(grid, x_m, z_m);
    let level = |f: &Field| sample_field(f, x_m, z_m, |i| f.water_level_m[i]);
    let (i, a) = (level(&grid.inner), level(&grid.apron));
    blended_height_m(grid, x_m, z_m).max(i + (a - i) * s)
}

/// How far the ground has dropped below the tangent plane at a horizontal distance from the
/// camera: `d² / (2R)` with `R = 6,371,000 m` (the small-angle sagitta).
pub fn curvature_drop_m(horizontal_distance_m: f64) -> f64 {
    horizontal_distance_m * horizontal_distance_m / (2.0 * EARTH_RADIUS_M)
}

/// The unit normal of the drawn surface at local `(x, z)` metres, for a camera at horizontal
/// position `camera_xz`: the blended height minus the curvature drop, so far terrain tilts away
/// from the camera by about `d / R` (9° at 1,000 km).
///
/// Exact formula (mirrored by `terrain_normal` in `terrain_cdlod.wesl`):
/// - `e` is one texel of the field that dominates at `(x, z)`: the inner spacing where
///   `max(|x|, |z|) < e1` (the blend's end), the apron spacing elsewhere;
/// - `west, east, north, south` are [`blended_height_m`] at `(x ∓ e, z)` and `(x, z ∓ e)`;
/// - `(dx, dz) = (x, z) − camera_xz`, in f32;
/// - `n = normalize(west − east + 2e·dx/R, 2e, north − south + 2e·dz/R)`: central differences
///   of `h − (dx² + dz²) / 2R`, whose slope is `−d/R` along the line from the camera.
pub fn surface_normal(grid: &RegionGrid, camera_xz: [f64; 2], x_m: f64, z_m: f64) -> [f32; 3] {
    let e1 = grid.inner.size_m / 2.0;
    let e = if x_m.abs().max(z_m.abs()) < e1 {
        grid.inner.spacing_m()
    } else {
        grid.apron.spacing_m()
    };
    let west = blended_height_m(grid, x_m - e, z_m);
    let east = blended_height_m(grid, x_m + e, z_m);
    let north = blended_height_m(grid, x_m, z_m - e);
    let south = blended_height_m(grid, x_m, z_m + e);
    let (e, r) = (e as f32, EARTH_RADIUS_M as f32);
    let dx = (x_m - camera_xz[0]) as f32;
    let dz = (z_m - camera_xz[1]) as f32;
    let n = [
        west - east + 2.0 * e * dx / r,
        2.0 * e,
        north - south + 2.0 * e * dz / r,
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    n.map(|c| c / len)
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
    sample_field(field, x_m, z_m, |i| field.elevation_m[i])
}

/// [`sample_clamped`] of any per-texel value `at(row × res + col)`.
fn sample_field(field: &Field, x_m: f64, z_m: f64, at: impl Fn(usize) -> f32) -> f32 {
    let last = f64::from(field.res - 1);
    let spacing = field.spacing_m();
    let u = ((x_m + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let v = ((z_m + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let (c0, r0) = (u.floor().min(last - 1.0), v.floor().min(last - 1.0));
    let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
    let (c0, r0) = (c0 as usize, r0 as usize);
    let n = field.res as usize;
    let h = |r: usize, c: usize| at(r * n + c);
    let top = h(r0, c0) + (h(r0, c0 + 1) - h(r0, c0)) * fu;
    let bottom = h(r0 + 1, c0) + (h(r0 + 1, c0 + 1) - h(r0 + 1, c0)) * fu;
    top + (bottom - top) * fv
}

#[cfg(test)]
mod tests;
