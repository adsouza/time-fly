//! The region generator (design spec §5.3–§5.5): coarse truth, amplified.
//!
//! `h = coastline_clamp(coarse + D, corners)` at every texel, where:
//! - `coarse` is the coarse field upsampled bilinearly from a lattice of coarse nodes;
//! - `D` is detail noise seeded by the texel's canonical crust-space position (its
//!   [`LatticeKey`](crate::worldgen::coarse::LatticeKey)), with only the octaves finer than the source data;
//! - `corners` are the coarse field at `(±spacing/2, ±spacing/2)` along the texel's own east and
//!   north. Where all four agree on land or sea, `h` keeps their side of sea level
//!   ([`coastline_clamp`]), so detail draws fractal coastlines only inside the data's own
//!   uncertainty band and never lifts open ocean.
//!
//! **The coarse lattice is global, not the region's.** Nodes are the texel centres of a cube map
//! whose face-centre texel spans `max(spacing/4, texel)`, and upsampling is
//! [`bilinear_taps`](crate::cube::bilinear_taps) on that cube. Every texel is therefore a pure function of its era-frame
//! direction, the snapshot field, the seed and the field's resolution: overlapping regions agree
//! exactly, and a grid texel equals [`RegionGenerator::sample_point`] at its direction. A region
//! only decides which nodes it precomputes; any other node is computed on demand.
//!
//! **Source levels (ruling R7).** The apron's lattice reads present-day data only from the
//! global overview (never tiles), so apron texels stay a function of direction whatever tiles
//! are loaded. A separate inner lattice reads tiles. When both fields share one lattice (the
//! source is coarser than the apron's texels, as in deep time) it is the overview-level one.
//!
//! **Incremental construction (ruling R8).** [`RegionPrep`] does the precompute in bounded
//! steps: planning which nodes the region touches, sampling them, the apron's coarse land mask,
//! and finally the coast-distance transform in a step of its own. [`RegionGenerator::new`] is
//! all of it at once.
//!
//! Why a lattice at all: the deep source is about 110 km, and resolving plates (every candidate
//! plate's rotation, footprint and endpoint samples) at every 293 m texel would be wasted work.
//! The detail and the lattice key are per texel; only the plate comes from the nearest node.

use std::ops::Range;

use tracing::instrument;

use crate::deep_time::YearsBp;
use crate::geo::RenderDir;
use crate::worldgen::biome::{
    BiomeClass, VegetationEra, biome_from_koppen, effective_biome, snowline_m, vegetation_era,
    zonal_biome,
};
use crate::worldgen::coarse::{CoarseField, KeyKind};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis, RegionFrame};
use crate::worldgen::tiles::SourceLevel;
use crate::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};

mod detail;
mod lattice;

pub use detail::coastline_clamp;
use detail::{Detail, octaves_for, smoothstep};
use lattice::{Coarse, Lattice};

/// Surface byte 3 for a texel in the shoreline band (Task 8 tints Barren-era shores with it).
pub const SURFACE_SHORE: u8 = 255;
/// Heights within this many metres of sea level count as shoreline.
pub const SHORE_BAND_M: f64 = 15.0;
/// Coast distance where the apron holds no coast at all.
pub const COAST_UNKNOWN_KM: f32 = 1.0e6;
/// Snow fades in over this many metres either side of the snowline.
const SNOW_RAMP_M: f64 = 150.0;

/// Everything that decides a region: its centre, snapshot, seed and grid resolutions. Sizes are
/// [`REGION_SIZE_M`] and [`APRON_SIZE_M`]; resolutions below 2 are raised to 2.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RegionSpec {
    /// Centre direction in the era frame.
    pub center: RenderDir,
    /// The snapshot age. The coarse field must be built for the same age
    /// (`snapshot_field(.., snapshot.myr(), ..)`); the generator uses it for the vegetation era
    /// and the era-fixed detail seed.
    pub snapshot: YearsBp,
    pub seed: u32,
    pub inner_res: u32,
    pub apron_res: u32,
    /// The snapshot's sea-level anomaly (m; negative when the sea stood lower):
    /// [`snapshot_sea_level_anomaly_m`](crate::worldgen::coarse::snapshot_sea_level_anomaly_m),
    /// 0 in deep eras. It lowers the snowline in glacial eras (ruling R18, [`snowline_m`]); the
    /// heights already carry the sea level through the coarse field.
    pub sea_level_anomaly_m: f64,
}

impl RegionSpec {
    /// A spec at the standard resolutions ([`REGION_RES`], [`APRON_RES`]), with no sea-level
    /// anomaly.
    pub fn new(center: RenderDir, snapshot: YearsBp, seed: u32) -> RegionSpec {
        RegionSpec {
            center,
            snapshot,
            seed,
            inner_res: REGION_RES,
            apron_res: APRON_RES,
            sea_level_anomaly_m: 0.0,
        }
    }

    pub fn frame(&self) -> RegionFrame {
        RegionFrame {
            center: RenderDir(self.center.0.normalize()),
        }
    }
}

/// One square grid centred on the region, row-major with row 0 at the north edge
/// (`z = −size/2`) and column 0 at the west edge (`x = −size/2`).
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub res: u32,
    pub size_m: f64,
    /// Effective elevation (m): sea level is 0.
    pub elevation_m: Vec<f32>,
    /// `[biome class, vegetation era, ice × 255, shore flag]`.
    pub surface: Vec<[u8; 4]>,
}

impl Field {
    pub fn spacing_m(&self) -> f64 {
        self.size_m / f64::from(self.res - 1)
    }

    /// Local `(x, z)` metres of a texel.
    pub fn local_of(&self, row: u32, col: u32) -> (f64, f64) {
        local_of(self.res, self.size_m, row, col)
    }

    /// Bilinear height at local `(x, z)` metres, or `None` outside the field.
    pub fn height_at(&self, x_m: f64, z_m: f64) -> Option<f32> {
        let last = f64::from(self.res - 1);
        let u = (x_m + self.size_m / 2.0) / self.spacing_m();
        let v = (z_m + self.size_m / 2.0) / self.spacing_m();
        if !(0.0..=last).contains(&u) || !(0.0..=last).contains(&v) {
            return None;
        }
        let (c0, r0) = (u.floor().min(last - 1.0), v.floor().min(last - 1.0));
        let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
        let (c0, r0) = (c0 as usize, r0 as usize);
        let n = self.res as usize;
        let h = |r: usize, c: usize| self.elevation_m[r * n + c];
        let top = h(r0, c0) + (h(r0, c0 + 1) - h(r0, c0)) * fu;
        let bottom = h(r0 + 1, c0) + (h(r0 + 1, c0 + 1) - h(r0 + 1, c0)) * fu;
        Some(top + (bottom - top) * fv)
    }
}

fn local_of(res: u32, size_m: f64, row: u32, col: u32) -> (f64, f64) {
    let step = size_m / f64::from(res - 1);
    (
        -size_m / 2.0 + f64::from(col) * step,
        -size_m / 2.0 + f64::from(row) * step,
    )
}

/// A generated region: the 300 km inner field and the 2,400 km apron around it.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionGrid {
    pub spec: RegionSpec,
    pub inner: Field,
    pub apron: Field,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Inner,
    Apron,
}

impl FieldKind {
    fn index(self) -> usize {
        match self {
            FieldKind::Inner => 0,
            FieldKind::Apron => 1,
        }
    }
}

/// A block of consecutive rows of one field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FieldRows {
    pub elevation_m: Vec<f32>,
    pub surface: Vec<[u8; 4]>,
}

/// Per-field constants.
#[derive(Copy, Clone, Debug)]
struct Layer {
    res: u32,
    size_m: f64,
    /// Index into the lattices.
    lattice: usize,
    octaves: u32,
}

/// Where a [`RegionPrep`] is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Planning which nodes lattice `i` touches.
    Plan(usize),
    /// Sampling lattice `i`'s nodes.
    Fill(usize),
    /// The apron's coarse land mask, texel by texel.
    Mask,
    /// The coast-distance transform, in a step of its own.
    Chamfer,
    Done,
}

/// A region generator under construction (ruling R8). [`new`](Self::new) samples nothing;
/// [`step`](Self::step) does at most `max_nodes` units of work (one unit is a planning point, a
/// lattice node or an apron mask texel), except the final coast-distance step, which runs alone;
/// [`finish`](Self::finish) completes whatever is left. The result is identical for any step
/// sizes.
pub struct RegionPrep<'a, F: CoarseField> {
    spec: RegionSpec,
    field: &'a F,
    basis: FrameBasis,
    spacing_m: f64,
    lattices: Vec<Lattice>,
    layers: [Layer; 2],
    phase: Phase,
    /// The chamfer's input: 0 at sea, infinity on land, filled texel by texel.
    coast: Vec<f32>,
    units_done: u64,
}

impl<'a, F: CoarseField> RegionPrep<'a, F> {
    /// Sizes the lattices. Cheap: no field samples.
    pub fn new(spec: RegionSpec, field: &'a F) -> RegionPrep<'a, F> {
        let spec = RegionSpec {
            inner_res: spec.inner_res.max(2),
            apron_res: spec.apron_res.max(2),
            ..spec
        };
        let basis = spec.frame().basis();
        let spacing_m = field.source_spacing_m().max(1.0);
        let layer_params = |res: u32, size_m: f64| {
            let texel_m = size_m / f64::from(res - 1);
            let lattice_m = (spacing_m / 4.0).max(texel_m);
            let n = (2.0 * EARTH_RADIUS_M / lattice_m)
                .ceil()
                .clamp(1.0, f64::from(1u32 << 24)) as u32;
            // Corners reach half a spacing past the edge; taps one node beyond that.
            let half_m = size_m / 2.0 + spacing_m / 2.0 + 2.0 * lattice_m;
            (n, octaves_for(spacing_m, texel_m), half_m)
        };
        let (apron_n, apron_octaves, apron_half) = layer_params(spec.apron_res, APRON_SIZE_M);
        let (inner_n, inner_octaves, inner_half) = layer_params(spec.inner_res, REGION_SIZE_M);
        let mut lattices = vec![Lattice::new(apron_n, SourceLevel::Overview, apron_half)];
        let inner_lattice = if inner_n == apron_n {
            0
        } else {
            lattices.push(Lattice::new(inner_n, SourceLevel::Tiles, inner_half));
            1
        };
        let layers = [
            Layer {
                res: spec.inner_res,
                size_m: REGION_SIZE_M,
                lattice: inner_lattice,
                octaves: inner_octaves,
            },
            Layer {
                res: spec.apron_res,
                size_m: APRON_SIZE_M,
                lattice: 0,
                octaves: apron_octaves,
            },
        ];
        RegionPrep {
            spec,
            field,
            basis,
            spacing_m,
            lattices,
            layers,
            phase: Phase::Plan(0),
            coast: Vec::with_capacity((spec.apron_res * spec.apron_res) as usize),
            units_done: 0,
        }
    }

    /// Does up to `max_nodes` units of work (at least one); returns true when construction is
    /// complete. The coast-distance transform runs in a step of its own, whatever `max_nodes`.
    pub fn step(&mut self, max_nodes: u32) -> bool {
        let start = max_nodes.max(1);
        let mut budget = start;
        let done = self.run(&mut budget);
        self.units_done += u64::from(start - budget);
        done
    }

    fn run(&mut self, budget: &mut u32) -> bool {
        let mut used_any = false;
        while *budget > 0 {
            match self.phase {
                Phase::Plan(i) => {
                    let used = self.lattices[i].plan(&self.basis, *budget);
                    *budget -= used;
                    used_any |= used > 0;
                    if self.lattices[i].planned() {
                        self.phase = if i + 1 < self.lattices.len() {
                            Phase::Plan(i + 1)
                        } else {
                            Phase::Fill(0)
                        };
                    }
                }
                Phase::Fill(i) => {
                    let used = self.lattices[i].fill(self.field, *budget);
                    *budget -= used;
                    used_any |= used > 0;
                    if self.lattices[i].filled() {
                        self.phase = if i + 1 < self.lattices.len() {
                            Phase::Fill(i + 1)
                        } else {
                            Phase::Mask
                        };
                    }
                }
                Phase::Mask => {
                    let res = self.spec.apron_res;
                    let total = (res * res) as usize;
                    while *budget > 0 && self.coast.len() < total {
                        let k = self.coast.len() as u32;
                        let (x, z) = local_of(res, APRON_SIZE_M, k / res, k % res);
                        let dir = self.basis.local_to_dir(x, z);
                        let land = self.lattices[0].elevation(self.field, dir) > 0.0;
                        self.coast.push(if land { f32::INFINITY } else { 0.0 });
                        *budget -= 1;
                        used_any = true;
                    }
                    if self.coast.len() == total {
                        self.phase = Phase::Chamfer;
                    }
                }
                Phase::Chamfer => {
                    if used_any {
                        return false;
                    }
                    let mask = std::mem::take(&mut self.coast);
                    self.coast = chamfer_km(mask, self.spec.apron_res);
                    self.phase = Phase::Done;
                    return true;
                }
                Phase::Done => return true,
            }
        }
        self.phase == Phase::Done
    }

    /// Work units done so far (planning points, lattice nodes and mask texels): a progress
    /// measure. The final coast-distance step adds none.
    pub fn units_done(&self) -> u64 {
        self.units_done
    }

    /// Completes any remaining work and returns the generator.
    #[instrument(level = "debug", skip_all, fields(inner_res = self.spec.inner_res, apron_res = self.spec.apron_res))]
    pub fn finish(mut self) -> RegionGenerator<'a, F> {
        while !self.step(u32::MAX) {}
        let spec = self.spec;
        RegionGenerator {
            spec,
            field: self.field,
            basis: self.basis,
            spacing_m: self.spacing_m,
            lattices: self.lattices,
            layers: self.layers,
            coast_km: self.coast,
            era: vegetation_era(spec.snapshot),
            detail: Detail::new(self.spacing_m, spec.seed, spec.snapshot),
            next_row: [0, 0],
            done: [Vec::new(), Vec::new()],
        }
    }
}

/// Generates a region's fields from a coarse field. Built by [`RegionPrep`] (or `new`, which is
/// a whole [`RegionPrep`] at once): the coarse lattice nodes, their plates and the coast
/// distance are precomputed. [`rows`](Self::rows) then produces any block of rows of either
/// field, independently and in any order (Task 5 runs blocks in parallel on native and
/// time-boxed on the web); [`assemble`](Self::assemble) joins blocks into a [`RegionGrid`].
/// [`generate_rows`](Self::generate_rows) and [`finish`](Self::finish) are the sequential
/// convenience: inner rows first, then the apron.
pub struct RegionGenerator<'a, F: CoarseField> {
    spec: RegionSpec,
    field: &'a F,
    basis: FrameBasis,
    spacing_m: f64,
    lattices: Vec<Lattice>,
    layers: [Layer; 2],
    /// Distance to the coast (km) over the apron's texels, from its coarse land mask.
    coast_km: Vec<f32>,
    era: VegetationEra,
    detail: Detail,
    next_row: [u32; 2],
    done: [Vec<FieldRows>; 2],
}

impl<'a, F: CoarseField> RegionGenerator<'a, F> {
    /// Builds the generator in one go: [`RegionPrep`] run to completion.
    pub fn new(spec: RegionSpec, field: &'a F) -> RegionGenerator<'a, F> {
        RegionPrep::new(spec, field).finish()
    }

    pub fn spec(&self) -> &RegionSpec {
        &self.spec
    }

    /// The coarse field's source spacing (m), which sets the coastline corners and the octaves.
    pub fn source_spacing_m(&self) -> f64 {
        self.spacing_m
    }

    /// Rows in a field.
    pub fn rows_in(&self, kind: FieldKind) -> u32 {
        self.layers[kind.index()].res
    }

    /// The inner field's height and surface at an era-frame direction. Grid texels equal this at
    /// their own directions, and any region with the same snapshot field, seed and inner
    /// resolution gives the same result at the same direction.
    pub fn sample_point(&self, era_dir: RenderDir) -> (f32, [u8; 4]) {
        self.point(FieldKind::Inner, era_dir)
    }

    /// [`sample_point`](Self::sample_point) for either field.
    pub fn sample_point_in(&self, kind: FieldKind, era_dir: RenderDir) -> (f32, [u8; 4]) {
        self.point(kind, era_dir)
    }

    /// Rows `range` of a field (clamped to the field). Pure: any block, any order, any thread.
    pub fn rows(&self, kind: FieldKind, range: Range<u32>) -> FieldRows {
        let layer = self.layers[kind.index()];
        let range = range.start.min(layer.res)..range.end.min(layer.res);
        let count = (range.end - range.start) as usize * layer.res as usize;
        let mut out = FieldRows {
            elevation_m: Vec::with_capacity(count),
            surface: Vec::with_capacity(count),
        };
        for row in range {
            for col in 0..layer.res {
                let (x, z) = local_of(layer.res, layer.size_m, row, col);
                let (h, s) = self.point(kind, self.basis.local_to_dir(x, z));
                out.elevation_m.push(h);
                out.surface.push(s);
            }
        }
        out
    }

    /// Joins row blocks, each field's in order, into the region grid.
    ///
    /// Panics if a field's blocks do not hold exactly its `res × res` texels.
    pub fn assemble(&self, inner: Vec<FieldRows>, apron: Vec<FieldRows>) -> RegionGrid {
        let join = |kind: FieldKind, blocks: Vec<FieldRows>| {
            let layer = self.layers[kind.index()];
            let total = layer.res as usize * layer.res as usize;
            let mut field = Field {
                res: layer.res,
                size_m: layer.size_m,
                elevation_m: Vec::with_capacity(total),
                surface: Vec::with_capacity(total),
            };
            for b in blocks {
                field.elevation_m.extend(b.elevation_m);
                field.surface.extend(b.surface);
            }
            assert!(
                field.elevation_m.len() == total && field.surface.len() == total,
                "{kind:?} blocks hold {} texels, expected {total}",
                field.elevation_m.len()
            );
            field
        };
        RegionGrid {
            spec: self.spec,
            inner: join(FieldKind::Inner, inner),
            apron: join(FieldKind::Apron, apron),
        }
    }

    /// Rows not yet produced by [`generate_rows`](Self::generate_rows), over both fields.
    pub fn rows_remaining(&self) -> u32 {
        (0..2).map(|k| self.layers[k].res - self.next_row[k]).sum()
    }

    /// Produces up to `max_rows` more rows: the inner field first, then the apron.
    pub fn generate_rows(&mut self, max_rows: u32) {
        let mut budget = max_rows;
        for kind in [FieldKind::Inner, FieldKind::Apron] {
            let k = kind.index();
            let left = self.layers[k].res - self.next_row[k];
            let take = left.min(budget);
            if take > 0 {
                let start = self.next_row[k];
                let block = self.rows(kind, start..start + take);
                self.done[k].push(block);
                self.next_row[k] += take;
                budget -= take;
            }
        }
    }

    /// Produces any remaining rows and returns the grid.
    pub fn finish(mut self) -> RegionGrid {
        self.generate_rows(u32::MAX);
        let [inner, apron] = std::mem::take(&mut self.done);
        self.assemble(inner, apron)
    }

    /// The coarse value and the four coastline corners at a direction.
    fn coarse_and_corners(&self, kind: FieldKind, dir: RenderDir) -> (Coarse, [f64; 4]) {
        let lattice = &self.lattices[self.layers[kind.index()].lattice];
        let coarse = lattice.interp(self.field, dir);
        let (north, east) = RegionFrame { center: dir }.north_east();
        let off = self.spacing_m / 2.0 / EARTH_RADIUS_M;
        let corner = |sx: f64, sz: f64| {
            let d = RenderDir((dir.0 + (east * sx + north * sz) * off).normalize());
            lattice.elevation(self.field, d)
        };
        // [north-east, north-west, south-east, south-west]
        let corners = [
            corner(1.0, 1.0),
            corner(-1.0, 1.0),
            corner(1.0, -1.0),
            corner(-1.0, -1.0),
        ];
        (coarse, corners)
    }

    fn point(&self, kind: FieldKind, era_dir: RenderDir) -> (f32, [u8; 4]) {
        let dir = RenderDir(era_dir.0.normalize());
        let layer = self.layers[kind.index()];
        let (coarse, c) = self.coarse_and_corners(kind, dir);
        let s = self.spacing_m;
        let gx = ((c[0] + c[2]) - (c[1] + c[3])) / (2.0 * s);
        let gz = ((c[0] + c[1]) - (c[2] + c[3])) / (2.0 * s);
        let slope = gx.hypot(gz);
        let detail = self.blended_detail_m(dir, &coarse, slope, layer.octaves);
        let h = coastline_clamp(coarse.elevation_m + detail, c);
        let h = if h.is_finite() { h } else { -1.0 };
        (h as f32, self.surface(dir, h, &coarse))
    }

    /// The detail at a texel. Detail is seeded per lattice key (a plate's fixed frame, or the
    /// era-fixed key), so two keys give independent noise: where the cell's four taps carry
    /// different keys (a plate boundary, a footprint crack), each key's detail is weighted by
    /// its taps' bilinear weights, so detail crossfades over one lattice cell instead of
    /// stepping by hundreds of metres along the boundary (the 10 Ma Task 8 shot's straight dark
    /// lines). Cells with one key take that key's detail alone.
    fn blended_detail_m(&self, dir: RenderDir, coarse: &Coarse, slope: f64, octaves: u32) -> f64 {
        let detail = |kind| {
            let key = self.field.lattice_key(dir, kind);
            self.detail
                .detail_m(&key, coarse.elevation_m, slope, octaves)
        };
        let first = coarse.taps[0].0;
        if octaves == 0 || coarse.taps.iter().all(|&(k, _)| k == first) {
            return detail(coarse.kind);
        }
        let mut kinds: Vec<(KeyKind, f64)> = Vec::with_capacity(4);
        for &(k, w) in &coarse.taps {
            match kinds.iter_mut().find(|(kk, _)| *kk == k) {
                Some((_, total)) => *total += w,
                None => kinds.push((k, w)),
            }
        }
        let total: f64 = kinds.iter().map(|&(_, w)| w).sum();
        if total <= 0.0 {
            return detail(coarse.kind);
        }
        kinds
            .iter()
            .filter(|&&(_, w)| w > 0.0)
            .map(|&(k, w)| w * detail(k))
            .sum::<f64>()
            / total
    }

    fn surface(&self, dir: RenderDir, h: f64, coarse: &Coarse) -> [u8; 4] {
        let lat = self.field.paleolat(dir);
        let biome = match coarse.koppen.and_then(biome_from_koppen) {
            Some(b) => b,
            None => zonal_biome(
                lat,
                h,
                f64::from(self.coast_km_at(dir)),
                self.spec.sea_level_anomaly_m,
            ),
        };
        let biome = effective_biome(self.era, biome);
        let snow = if h > 0.0 {
            let line = snowline_m(lat, self.spec.sea_level_anomaly_m);
            smoothstep(line - SNOW_RAMP_M, line + SNOW_RAMP_M, h)
        } else {
            0.0
        };
        let cap = if biome == BiomeClass::IceCap {
            1.0
        } else {
            0.0
        };
        let ice = coarse.ice.max(snow).max(cap).clamp(0.0, 1.0);
        let shore = if h > -SHORE_BAND_M && h <= SHORE_BAND_M {
            SURFACE_SHORE
        } else {
            0
        };
        [
            biome as u8,
            self.era as u8,
            (ice * 255.0).round() as u8,
            shore,
        ]
    }

    /// Distance to the coast (km) at a direction, bilinear from the apron's transform.
    fn coast_km_at(&self, dir: RenderDir) -> f32 {
        let res = self.layers[1].res;
        let Some((x, z)) = self.basis.dir_to_local(dir) else {
            return COAST_UNKNOWN_KM;
        };
        let step = APRON_SIZE_M / f64::from(res - 1);
        let last = f64::from(res - 1);
        let u = (x + APRON_SIZE_M / 2.0) / step;
        let v = (z + APRON_SIZE_M / 2.0) / step;
        if !(0.0..=last).contains(&u) || !(0.0..=last).contains(&v) {
            return COAST_UNKNOWN_KM;
        }
        let (c0, r0) = (u.floor().min(last - 1.0), v.floor().min(last - 1.0));
        let (fu, fv) = ((u - c0) as f32, (v - r0) as f32);
        let (c0, r0, n) = (c0 as usize, r0 as usize, res as usize);
        let d = |r: usize, c: usize| self.coast_km[r * n + c];
        let top = d(r0, c0) + (d(r0, c0 + 1) - d(r0, c0)) * fu;
        let bottom = d(r0 + 1, c0) + (d(r0 + 1, c0 + 1) - d(r0 + 1, c0)) * fu;
        top + (bottom - top) * fv
    }
}

/// A two-pass chamfer distance transform (weights 1 and √2) over the apron's coarse land mask
/// (`mask`: 0 at sea, infinity on land): each land texel's distance (km) to the nearest sea
/// texel; 0 at sea; [`COAST_UNKNOWN_KM`] everywhere if the apron holds no sea.
///
/// Known limitation, accepted for M2 (ruling R9): the distance comes from each region's own
/// apron window, so the deep-era zonal biome near a threshold can differ between overlapping
/// regions. Heights and every other surface byte are window-independent.
fn chamfer_km(mut d: Vec<f32>, res: u32) -> Vec<f32> {
    let n = res as usize;
    let diag = std::f32::consts::SQRT_2;
    let at = |d: &[f32], r: isize, c: isize| -> f32 {
        if r < 0 || c < 0 || r >= n as isize || c >= n as isize {
            f32::INFINITY
        } else {
            d[r as usize * n + c as usize]
        }
    };
    for r in 0..n as isize {
        for c in 0..n as isize {
            let best = at(&d, r, c)
                .min(at(&d, r - 1, c - 1) + diag)
                .min(at(&d, r - 1, c) + 1.0)
                .min(at(&d, r - 1, c + 1) + diag)
                .min(at(&d, r, c - 1) + 1.0);
            d[r as usize * n + c as usize] = best;
        }
    }
    for r in (0..n as isize).rev() {
        for c in (0..n as isize).rev() {
            let best = at(&d, r, c)
                .min(at(&d, r + 1, c + 1) + diag)
                .min(at(&d, r + 1, c) + 1.0)
                .min(at(&d, r + 1, c - 1) + diag)
                .min(at(&d, r, c + 1) + 1.0);
            d[r as usize * n + c as usize] = best;
        }
    }
    let km_per_texel = (APRON_SIZE_M / f64::from(res - 1) / 1000.0) as f32;
    d.into_iter()
        .map(|t| {
            if t.is_finite() {
                t * km_per_texel
            } else {
                COAST_UNKNOWN_KM
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
