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

use crate::hydrology::params::NO_WATER_DROP_M;
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
/// The most one river's signed centreline offset changes across a bilinear cell, in texels: the
/// cell's diagonal (√2, since a distance changes no faster than the point moves) plus slack for
/// the offsets' f16 rounding. Taps whose offsets differ in sign by more belong to different
/// rivers ([`blended_water`]'s crossing guard). Mirrored in `terrain_cdlod.wesl`.
pub const CROSSING_REACH_TEXELS: f32 = 1.5;
/// A texel whose water level stands more than this below its ground is dry: dry texels store
/// `h − NO_WATER_DROP_M` ([`blended_water`]'s standing-water rule). Mirrored in
/// `terrain_cdlod.wesl`.
pub const DRY_BELOW_M: f32 = -(NO_WATER_DROP_M as f32) / 2.0;

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
///
/// The crossing guard: where a field's four taps hold offsets of both signs that differ by
/// more than [`CROSSING_REACH_TEXELS`] texels, they are not one river seen from both banks (its
/// offset changes no faster than the distance between the taps) but two (side by side, or at a
/// confluence or a sharp bend). The interpolated offset would pass through 0 between them and
/// rebuild a ribbon of `hw` where no river is, so that field contributes its interpolated
/// ribbons instead, as `hw` with `o = 0`.
///
/// Standing water (Task 8b): where none of a field's four taps is a river texel (its bits other
/// than [`RIVER_NONE`](crate::worldgen::generate::water::RIVER_NONE)'s ribbon and offset), the level is interpolated over the taps that are
/// wet, `level − h > `[`DRY_BELOW_M`], alone ([`field_level`]): a lake's or the sea's flat
/// level, not one dropping toward the dry texels' `h − NO_WATER_DROP_M`. The shoreline is then
/// the terrain's own `h = L` contour. Rivers keep the plain blend.
pub fn blended_water(grid: &RegionGrid, x_m: f64, z_m: f64) -> WaterSample {
    let s = blend_weight(grid, x_m, z_m);
    let read = |f: &Field| {
        let level = field_level(f, x_m, z_m);
        let decoded = |i: usize| decode_river(f.river_at(i));
        let hw = sample_field(f, x_m, z_m, |i| {
            let [ribbon, _, _, offset] = decoded(i);
            ribbon + offset.abs()
        });
        let offset = sample_field(f, x_m, z_m, |i| decoded(i)[3]);
        let east = sample_field(f, x_m, z_m, |i| decoded(i)[1]);
        let north = sample_field(f, x_m, z_m, |i| decoded(i)[2]);
        let (lo, hi) = taps(f, x_m, z_m)
            .0
            .iter()
            .map(|&i| decoded(i)[3])
            .fold((f32::MAX, f32::MIN), |(lo, hi), o| (lo.min(o), hi.max(o)));
        let reach = CROSSING_REACH_TEXELS * f.spacing_m() as f32;
        if lo < 0.0 && hi > 0.0 && hi - lo > reach {
            let ribbon = sample_field(f, x_m, z_m, |i| decoded(i)[0]);
            return [level, ribbon, 0.0, east, north];
        }
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
/// and the water ([`blended_water`]'s level, by the same [`field_level`] rule: standing water
/// over its wet taps, rivers by the plain blend). The fly's floor; it reads the river mask, not
/// the river texels, which are freed after the upload ([`Field::is_river`]).
pub fn blended_surface_m(grid: &RegionGrid, x_m: f64, z_m: f64) -> f32 {
    let s = blend_weight(grid, x_m, z_m);
    let level = |f: &Field| field_level(f, x_m, z_m);
    let (i, a) = (level(&grid.inner), level(&grid.apron));
    blended_height_m(grid, x_m, z_m).max(i + (a - i) * s)
}

/// One field's water level at local `(x, z)`: the bilinear level of [`sample_clamped`], except
/// over standing water. A tap is wet when `level − h > `[`DRY_BELOW_M`]; a tap whose river
/// texel's ribbon and offset are not [`RIVER_NONE`](crate::worldgen::generate::water::RIVER_NONE)'s ([`Field::is_river`]) is a river. Where no
/// tap is a river and some taps are wet and some dry:
/// - if every wet tap holds the same level, that level (exactly: a lake is flat);
/// - else `n / d`, in f32: with the bilinear weights `w = [(1 − fu)(1 − fv), fu (1 − fv),
///   (1 − fu) fv, fu fv]` in tap order, `n` sums `w_k × level_k` and `d` sums `w_k` over the wet
///   taps, in tap order (all terms positive, so a small wet share loses no precision), when
///   `d > 0`.
///
/// With every tap wet, or none, the plain bilinear level.
fn field_level(f: &Field, x_m: f64, z_m: f64) -> f32 {
    let plain = sample_field(f, x_m, z_m, |i| f.water_level_m[i]);
    let (taps, fu, fv) = taps(f, x_m, z_m);
    let wet = |i: usize| f.water_level_m[i] - f.elevation_m[i] > DRY_BELOW_M;
    let wet_taps = taps.iter().filter(|&&i| wet(i)).count();
    if taps.iter().any(|&i| f.is_river(i)) || wet_taps == 0 || wet_taps == 4 {
        return plain;
    }
    let mut levels = taps
        .iter()
        .filter(|&&i| wet(i))
        .map(|&i| f.water_level_m[i]);
    let first = levels.next().expect("a wet tap");
    if levels.all(|l| l == first) {
        return first;
    }
    let w = [
        (1.0 - fu) * (1.0 - fv),
        fu * (1.0 - fv),
        (1.0 - fu) * fv,
        fu * fv,
    ];
    let (mut n, mut d) = (0.0f32, 0.0f32);
    for (k, &i) in taps.iter().enumerate().filter(|&(_, &i)| wet(i)) {
        n += w[k] * f.water_level_m[i];
        d += w[k];
    }
    if d > 0.0 { n / d } else { plain }
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
    let ([i00, i10, i01, i11], fu, fv) = taps(field, x_m, z_m);
    let top = at(i00) + (at(i10) - at(i00)) * fu;
    let bottom = at(i01) + (at(i11) - at(i01)) * fu;
    top + (bottom - top) * fv
}

/// [`sample_clamped`]'s four texel indices, `(r0, c0)`, `(r0, c0 + 1)`, `(r0 + 1, c0)` and
/// `(r0 + 1, c0 + 1)`, and its weights `fu`, `fv`.
fn taps(field: &Field, x_m: f64, z_m: f64) -> ([usize; 4], f32, f32) {
    let last = f64::from(field.res - 1);
    let spacing = field.spacing_m();
    let u = ((x_m + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let v = ((z_m + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let (c0, r0) = (u.floor().min(last - 1.0), v.floor().min(last - 1.0));
    let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
    let (c0, r0) = (c0 as usize, r0 as usize);
    let n = field.res as usize;
    let i = r0 * n + c0;
    ([i, i + 1, i + n, i + n + 1], fu, fv)
}

#[cfg(test)]
mod tests;
