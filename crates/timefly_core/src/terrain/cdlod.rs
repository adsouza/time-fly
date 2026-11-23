//! CDLOD patch selection and the vertex morph (design spec §6.3, parent spec §7.2).
//!
//! **Selection.** A quadtree over the square of side [`LodSettings::extent_m`] centred on the
//! region, rooted at LOD `levels − 1`. A node at LOD `L > 0` is split into its four children when
//! its footprint comes within LOD `L − 1`'s range of the camera; otherwise it is selected whole.
//! The selected patches therefore tile the square exactly, with no gaps or overlaps.
//!
//! **Distance.** Every distance here is from the camera to a point on the sea-level plane
//! (`y = 0`): `√(dx² + cy² + dz²)`. It ignores terrain height, so a vertex's morph factor depends
//! only on its grid position and the camera, never on which patch computes it. Selection uses
//! the same metric (the nearest point of a node's footprint), so the two agree.
//!
//! **Morph.** Each vertex has a whole-number index on the finest grid (its *cell*): the patch's
//! origin cell plus its patch index times `2^lod`. An odd patch index slides toward the even
//! index below it by the morph factor, in index space, so at full morph the patch is exactly its
//! parent's grid (the odd vertices collapse onto even ones; the shader re-samples the height at
//! the morphed position, as the draft does). Sliding to "the midpoint of the even neighbours" in
//! `xz` would not move the vertex at all, since that midpoint is its own position. At full
//! morph the surface over the odd vertex's original position is the midpoint of the coarse
//! edge, as in standard CDLOD. Positions are computed from cells with one f32 operation
//! sequence, so a vertex shared by two patches is bit-identical in both.
//! This replaces the draft's `floor(world / parent + 0.5)` world-space snap (spec §7.2), which is
//! ambiguous at exactly .5 in floating point.
//!
//! **Why the shared edges agree.** Between same-LOD patches, a shared vertex has the same cell
//! and the same morph factor. Between LOD `L` and `L + 1` (the most they differ by), the shared
//! edge lies outside LOD `L`'s range, where the fine patch is fully morphed (its morph ends at
//! `0.98 × range` at LOD 0, `0.99 × range` above), and inside the coarse patch's no-morph zone (its morph starts at
//! `1.7 × range(L)`, beyond the fine patch's farthest point when the base range is at least
//! `2√2 / 0.7 ≈ 4.04 ×` the finest patch: the fine patch's parent comes within `range(L)` and
//! spans a diagonal of `2√2 ×` the fine patch). Both then sit on the coarse grid. The same
//! bound keeps neighbouring LODs within one of each other.

use bevy_math::DVec3;

use super::{MORPH_END, MORPH_START, PATCH_QUADS};
use crate::worldgen::APRON_SIZE_M;

/// LOD levels in a region: the coarsest patch covers the whole apron.
const REGION_LEVELS: u32 = 10;
/// The finest LOD's outer range, in multiples of the finest patch (19,921.875 m).
///
/// Shared edges need at least `2√2 / MORPH_START ≈ 4.04` (see the module docs), and the bound
/// is nearly tight: at 4.04 a sweep put coarse shared-edge vertices at 0.9996 of their morph
/// start. 4.25 leaves 2% (about 700 m at LOD 0) and selects at most 532 patches over a dense
/// sweep of the inner field at 150 m and 2 km; 4.5 already selects 622.
const REGION_BASE_RANGE_PATCHES: f64 = 4.25;

/// The quadtree's shape and the LOD ranges.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LodSettings {
    /// Quads per patch side.
    pub patch_quads: u32,
    /// LOD levels; LOD 0 is the finest, `levels − 1` the root.
    pub levels: u32,
    /// Side of a LOD 0 patch (m).
    pub finest_patch_m: f64,
    /// LOD 0's outer range (m); LOD `L`'s is `base_range_m × 2^L`.
    pub base_range_m: f64,
    /// Morph start, as a fraction of each ring (below 1, under `morph_end`).
    pub morph_start: f32,
    /// Morph end, as a fraction of each ring (below 1).
    pub morph_end: f32,
}

impl LodSettings {
    /// The region's settings: [`PATCH_QUADS`] quads, 10 levels, a 4,687.5 m finest patch
    /// (146.484375 m quads; ten levels then tile [`APRON_SIZE_M`] exactly), a 19,921.875 m base
    /// range, and morph fractions [`MORPH_START`] / [`MORPH_END`].
    pub fn region() -> LodSettings {
        let finest_patch_m = APRON_SIZE_M / f64::from(1u32 << (REGION_LEVELS - 1));
        LodSettings {
            patch_quads: PATCH_QUADS,
            levels: REGION_LEVELS,
            finest_patch_m,
            base_range_m: REGION_BASE_RANGE_PATCHES * finest_patch_m,
            morph_start: MORPH_START,
            morph_end: MORPH_END,
        }
    }

    /// Side of the selected square (the root patch), centred on the region.
    pub fn extent_m(&self) -> f64 {
        self.patch_size_m(self.levels.max(1) - 1)
    }

    /// Side of a finest-grid quad: one cell.
    pub fn finest_quad_m(&self) -> f64 {
        self.finest_patch_m / f64::from(self.patch_quads)
    }

    /// Side of a patch at `lod`.
    pub fn patch_size_m(&self, lod: u32) -> f64 {
        self.finest_patch_m * pow2(lod)
    }

    /// LOD `lod`'s outer range (m): beyond it, the next coarser LOD takes over.
    pub fn range_m(&self, lod: u32) -> f64 {
        self.base_range_m * pow2(lod)
    }

    /// `[start, end]` of LOD `lod`'s morph (m): the fractions `morph_start` and `morph_end` of
    /// its ring, which runs from the previous LOD's range (0 for LOD 0) to its own. With doubling
    /// ranges that is `[0.85, 0.99] × range` above LOD 0 and `[0.70, 0.98] × range` at LOD 0.
    pub fn morph_range_m(&self, lod: u32) -> [f32; 2] {
        let outer = self.range_m(lod);
        let inner = if lod == 0 { 0.0 } else { self.range_m(lod - 1) };
        let at = |fraction: f32| (inner + (outer - inner) * f64::from(fraction)) as f32;
        [at(self.morph_start), at(self.morph_end)]
    }

    /// The camera-relative distance below which a node at `lod` is split.
    fn split_range_m(&self, lod: u32) -> f64 {
        self.range_m(lod - 1)
    }
}

fn pow2(n: u32) -> f64 {
    2f64.powi(n as i32)
}

/// One selected patch: a square of `patch_quads²` quads at one LOD.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Patch {
    /// The patch's minimum `(x, z)` corner (its north-west corner), local metres.
    pub origin_m: [f64; 2],
    pub size_m: f64,
    pub lod: u32,
    /// `[start, end]` camera distances (m) of this LOD's morph ([`LodSettings::morph_range_m`]).
    pub morph_range_m: [f32; 2],
}

impl Patch {
    /// The origin as whole cells of the finest grid, counted from the selected square's
    /// minimum corner. Exact: origins are multiples of the finest quad.
    pub fn origin_cells(&self, settings: &LodSettings) -> [u32; 2] {
        let half = settings.extent_m() / 2.0;
        let quad = settings.finest_quad_m();
        self.origin_m.map(|c| ((c + half) / quad).round() as u32)
    }
}

/// The patches covering the square of side [`LodSettings::extent_m`] centred on the region
/// (±[`APRON_SIZE_M`]` / 2` with [`LodSettings::region`]), for a camera at local metres
/// `camera_m` (`y` above sea level). See the module docs for the rule.
pub fn select_patches(camera_m: DVec3, settings: &LodSettings) -> Vec<Patch> {
    let mut out = Vec::new();
    let half = settings.extent_m() / 2.0;
    select_node(
        camera_m,
        settings,
        [-half, -half],
        settings.levels.max(1) - 1,
        &mut out,
    );
    out
}

fn select_node(
    camera_m: DVec3,
    settings: &LodSettings,
    origin_m: [f64; 2],
    lod: u32,
    out: &mut Vec<Patch>,
) {
    let size_m = settings.patch_size_m(lod);
    if lod > 0 && footprint_distance_m(camera_m, origin_m, size_m) < settings.split_range_m(lod) {
        let h = size_m / 2.0;
        for (dx, dz) in [(0.0, 0.0), (h, 0.0), (0.0, h), (h, h)] {
            let child = [origin_m[0] + dx, origin_m[1] + dz];
            select_node(camera_m, settings, child, lod - 1, out);
        }
    } else {
        out.push(Patch {
            origin_m,
            size_m,
            lod,
            morph_range_m: settings.morph_range_m(lod),
        });
    }
}

/// Distance from the camera to the nearest point of a square footprint on the sea-level plane.
fn footprint_distance_m(camera_m: DVec3, origin_m: [f64; 2], size_m: f64) -> f64 {
    let gap = |c: f64, lo: f64| (lo - c).max(c - (lo + size_m)).max(0.0);
    let dx = gap(camera_m.x, origin_m[0]);
    let dz = gap(camera_m.z, origin_m[1]);
    DVec3::new(dx, camera_m.y, dz).length()
}

/// The morph factor at a camera distance: `clamp((d − start) / (end − start), 0, 1)` in f32.
/// Needs `end > start`.
pub fn morph_factor(distance_m: f32, morph_range_m: [f32; 2]) -> f32 {
    let [start, end] = morph_range_m;
    ((distance_m - start) / (end - start)).clamp(0.0, 1.0)
}

/// A patch vertex's morphed world `(x, z)` (local metres), for `grid_index` in
/// `0..=patch_quads` on each axis and a camera at `camera_m` (local metres, `y` above sea
/// level). The terrain shader mirrors this exactly; every step is f32 unless noted:
///
/// ```text
/// cell   = origin_cells + grid_index × 2^lod                  (u32, exact)
/// p      = f32(cell) × quad + root                            (quad = finest_quad_m,
///                                                              root = −extent_m / 2)
/// d      = sqrt((dx·dx + cy·cy) + dz·dz),  dx = p.x − cx, dz = p.z − cz
/// k      = morph_factor(d, morph_range_m)
/// odd    = f32(grid_index & 1)                                (per axis)
/// result = (f32(cell) − odd × k × f32(2^lod)) × quad + root   (per axis)
/// ```
///
/// Operations run in this order with no fused multiply-add. With [`LodSettings::region`],
/// `quad` (146.484375) and `root` (−1,200,000) are exact in f32; shared vertices agree whether
/// or not they are, because each depends only on its cell. The camera's `y` enters only the
/// distance: see the module docs.
pub fn morph_vertex(
    grid_index: [u32; 2],
    patch: &Patch,
    camera_m: [f32; 3],
    settings: &LodSettings,
) -> [f32; 2] {
    let origin = patch.origin_cells(settings);
    let step = 1u32 << patch.lod;
    let quad = settings.finest_quad_m() as f32;
    let root = (-settings.extent_m() / 2.0) as f32;
    let cell = [
        (origin[0] + grid_index[0] * step) as f32,
        (origin[1] + grid_index[1] * step) as f32,
    ];
    let x = cell[0] * quad + root;
    let z = cell[1] * quad + root;
    let dx = x - camera_m[0];
    let dz = z - camera_m[2];
    let cy = camera_m[1];
    let d = (dx * dx + cy * cy + dz * dz).sqrt();
    let k = morph_factor(d, patch.morph_range_m);
    let morph = |axis: usize| {
        let odd = (grid_index[axis] & 1) as f32;
        (cell[axis] - odd * k * step as f32) * quad + root
    };
    [morph(0), morph(1)]
}
