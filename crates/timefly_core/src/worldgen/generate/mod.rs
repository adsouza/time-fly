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
//!
//! **Water (rivers and lakes, Task 6).** Given the era's continental pass
//! ([`EraHydrology`]), each field's rivers are refined ([`rivers`]) in [`RegionPrep`]'s rivers
//! phases, and every texel's height is carved and its water chosen by [`water`]'s rule: the
//! stored height, a water level and a river texel, plus boulders in the inner field's rapids.
//! All of it is a function of the direction and the pass, so overlap stays exact. Without a
//! pass the heights and surface codes are M3's, bit for bit.

use std::ops::Range;

use tracing::instrument;

use crate::deep_time::YearsBp;
use crate::geo::RenderDir;
use crate::hydrology::params::{
    APRON_RIVER_MIN_DISCHARGE_M3S, MAX_BOULDERS, NO_WATER_DROP_M, RIVER_MIN_DISCHARGE_M3S,
    SUBPOINT_M,
};
use crate::hydrology::{EraHydrology, LakeTap};
use crate::worldgen::biome::{
    BiomeClass, VegetationEra, biome_from_koppen, effective_biome, snowline_raise_m,
    snowline_with_raise_m, vegetation_era, zonal_biome,
};
use crate::worldgen::coarse::{CoarseField, KeyKind};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis, RegionFrame};
use crate::worldgen::tiles::SourceLevel;
use crate::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};

mod detail;
mod join;
mod lattice;
pub mod rivers;
pub mod water;

pub use detail::coastline_clamp;
use detail::{Detail, octaves_for, smoothstep};
use lattice::{Coarse, Lattice};
use rivers::{RegionRivers, RiverBuilder, RiverHit};
use water::Water;
pub use water::{Boulder, FoamSite, RIVER_NONE, foam_sites};

/// Surface byte 3's top bit: the texel is in the shoreline band (Task 8 tints Barren-era shores
/// with it).
pub const SURFACE_SHORE: u8 = 0x80;
/// Surface byte 3's low 7 bits: the snowline's dryness raise (ruling R27) in steps of
/// [`MAX_SNOWLINE_RAISE_M`] / 127, so the shader paints snow at the same line as the generator.
pub const SURFACE_RAISE_STEPS: u8 = 0x7F;
/// The largest dryness raise either term gives.
pub const MAX_SNOWLINE_RAISE_M: f64 = 1000.0;

/// A dryness raise (m) as surface byte 3's low bits (nearest step, clamped).
pub fn encode_snowline_raise(raise_m: f64) -> u8 {
    let q = (raise_m / MAX_SNOWLINE_RAISE_M * f64::from(SURFACE_RAISE_STEPS)).round();
    if q.is_nan() {
        0
    } else {
        q.clamp(0.0, f64::from(SURFACE_RAISE_STEPS)) as u8
    }
}

/// The dryness raise (m) surface byte 3 holds. Mirrored in `terrain.wesl`'s `texel_surface`.
pub fn decode_snowline_raise(byte: u8) -> f64 {
    f64::from(byte & SURFACE_RAISE_STEPS) / f64::from(SURFACE_RAISE_STEPS) * MAX_SNOWLINE_RAISE_M
}
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
    /// weighted to the bridge endpoint's share in deep brackets that read it, 0 in older ones.
    /// It lowers the snowline in glacial eras (ruling R18, [`snowline_m`]); the heights already
    /// carry the sea level through the coarse field.
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
    /// Effective elevation (m): sea level is 0. Carved by the rivers where there are any.
    pub elevation_m: Vec<f32>,
    /// `[biome class, vegetation era, ice × 255, shore flag]`.
    pub surface: Vec<[u8; 4]>,
    /// The water surface (m): sea level 0, a lake's level or a river's surface; where there is
    /// no water, [`NO_WATER_DROP_M`] below the ground ([`water`]'s rule).
    pub water_level_m: Vec<f32>,
    /// f16 bits of `[ribbon_m, flow_east_mps, flow_north_mps, offset_m]` where the water is a
    /// river, [`RIVER_NONE`] elsewhere ([`water`]'s encoding).
    pub river: Vec<[u16; 4]>,
    /// One bit per texel, set on a river texel: kept by [`free_rivers`](Self::free_rivers)
    /// when it drops `river`, so readers still tell rivers from standing water
    /// ([`is_river`](Self::is_river)). Empty until then.
    pub river_mask: Vec<u64>,
}

impl Field {
    /// A field with no water: every water level [`NO_WATER_DROP_M`] below the ground, no river.
    pub fn dry(res: u32, size_m: f64, elevation_m: Vec<f32>, surface: Vec<[u8; 4]>) -> Field {
        let water_level_m = elevation_m
            .iter()
            .map(|&h| h - NO_WATER_DROP_M as f32)
            .collect();
        let river = vec![RIVER_NONE; elevation_m.len()];
        Field {
            res,
            size_m,
            elevation_m,
            surface,
            water_level_m,
            river,
            river_mask: Vec::new(),
        }
    }

    pub fn spacing_m(&self) -> f64 {
        self.size_m / f64::from(self.res - 1)
    }

    /// Drops the river texels (8 B a texel) once the shell has them on the GPU, keeping one bit
    /// a texel for [`is_river`](Self::is_river). Readers then see [`RIVER_NONE`] everywhere
    /// ([`river_at`](Self::river_at)); the levels, and the fly's floor, are unchanged.
    pub fn free_rivers(&mut self) {
        if self.river.is_empty() {
            return;
        }
        let mut mask = vec![0u64; self.river.len().div_ceil(64)];
        for i in (0..self.river.len()).filter(|&i| self.is_river(i)) {
            mask[i / 64] |= 1 << (i % 64);
        }
        self.river_mask = mask;
        self.river = Vec::new();
    }

    /// Whether texel `i` is a river texel: its ribbon or offset bits are not [`RIVER_NONE`]'s
    /// (the flow is not looked at, as the water texture's word 2 holds only those two). Read
    /// from the mask once the river texels are freed.
    pub fn is_river(&self, i: usize) -> bool {
        match self.river.get(i) {
            Some(r) => r[0] != RIVER_NONE[0] || r[3] != RIVER_NONE[3],
            None => self
                .river_mask
                .get(i / 64)
                .is_some_and(|w| w >> (i % 64) & 1 == 1),
        }
    }

    /// Texel `i`'s river bits: [`RIVER_NONE`] once [`free_rivers`](Self::free_rivers) ran.
    pub fn river_at(&self, i: usize) -> [u16; 4] {
        self.river.get(i).copied().unwrap_or(RIVER_NONE)
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
    /// The boulders in the inner field's rapids.
    pub boulders: Vec<Boulder>,
}

impl RegionGrid {
    /// The bytes its fields and boulders hold, by capacity.
    pub fn bytes(&self) -> usize {
        [&self.inner, &self.apron]
            .iter()
            .map(|f| {
                f.elevation_m.capacity() * 4
                    + f.surface.capacity() * 4
                    + f.water_level_m.capacity() * 4
                    + f.river.capacity() * 8
                    + f.river_mask.capacity() * 8
            })
            .sum::<usize>()
            + self.boulders.capacity() * std::mem::size_of::<Boulder>()
    }
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
    pub water_level_m: Vec<f32>,
    pub river: Vec<[u16; 4]>,
}

impl FieldRows {
    /// The bytes its rows hold, by capacity.
    pub fn bytes(&self) -> usize {
        self.elevation_m.capacity() * 4
            + self.surface.capacity() * 4
            + self.water_level_m.capacity() * 4
            + self.river.capacity() * 8
    }
}

/// Everything a field stores at one texel, as [`RegionGenerator::sample_texel_in`] gives it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Texel {
    /// The stored (carved) height.
    pub h: f32,
    pub surface: [u8; 4],
    pub water_level_m: f32,
    pub river: [u16; 4],
}

/// Per-field constants.
#[derive(Copy, Clone, Debug)]
struct Layer {
    res: u32,
    size_m: f64,
    /// Index into the lattices.
    lattice: usize,
    octaves: u32,
    /// The texel spacing, `size / (res − 1)`.
    texel_m: f64,
    /// The rivers' sub-point spacing: `max(SUBPOINT_M, texel / 2)`, so texel queries stay within
    /// twice it.
    subpoint_m: f64,
    /// The smallest discharge the field's rivers refine.
    min_q: f32,
}

/// Where a [`RegionPrep`] is.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    /// Planning which nodes lattice `i` touches.
    Plan(usize),
    /// Sampling lattice `i`'s nodes.
    Fill(usize),
    /// Refining field `k`'s rivers (0 inner, 1 apron), in sub-point units.
    Rivers(usize),
    /// The inner field's boulders, one unit each.
    Boulders,
    /// The apron's coarse land mask, texel by texel.
    Mask,
    /// The coast-distance transform, in a step of its own.
    Chamfer,
    Done,
}

/// A region generator under construction (ruling R8). [`new`](Self::new) samples nothing;
/// [`step`](Self::step) does about `max_nodes` units of work (one unit is a planning point, a
/// lattice node, a river sub-point, a boulder or an apron mask texel; a river node's relaxation
/// counts four and may finish a step a few units over), except the final coast-distance step,
/// which runs alone; [`finish`](Self::finish) completes whatever is left. The result is
/// identical for any step sizes.
pub struct RegionPrep<'a, F: CoarseField> {
    spec: RegionSpec,
    field: &'a F,
    hydro: Option<&'a EraHydrology>,
    basis: FrameBasis,
    spacing_m: f64,
    lattices: Vec<Lattice>,
    layers: [Layer; 2],
    detail: Detail,
    phase: Phase,
    /// Each field's rivers while they are refined, then once they are done.
    builders: [Option<RiverBuilder<'a>>; 2],
    rivers: [Option<RegionRivers>; 2],
    /// The inner field's boulder sites still to read (`None` before they are listed), and the
    /// boulders read so far.
    boulder_sites: Option<Vec<water::BoulderCluster>>,
    boulders: Vec<Boulder>,
    boulders_dropped: u32,
    /// The drawn-water lattices' texels read for the current sub-segment's clusters.
    drawn_cache: water::DrawnCache,
    /// The chamfer's input: 0 at sea, infinity on land, filled texel by texel.
    coast: Vec<f32>,
    units_done: u64,
}

impl<'a, F: CoarseField> RegionPrep<'a, F> {
    /// Sizes the lattices. Cheap: no field samples. `hydro` is the era's continental pass; with
    /// `None` the region has no rivers, lakes or boulders, and its heights are M3's.
    pub fn new(
        spec: RegionSpec,
        field: &'a F,
        hydro: Option<&'a EraHydrology>,
    ) -> RegionPrep<'a, F> {
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
        let layer = |res: u32, size_m: f64, lattice: usize, octaves: u32, min_q: f64| {
            let texel_m = size_m / f64::from(res - 1);
            Layer {
                res,
                size_m,
                lattice,
                octaves,
                texel_m,
                subpoint_m: SUBPOINT_M.max(texel_m / 2.0),
                min_q: min_q as f32,
            }
        };
        let layers = [
            layer(
                spec.inner_res,
                REGION_SIZE_M,
                inner_lattice,
                inner_octaves,
                RIVER_MIN_DISCHARGE_M3S,
            ),
            layer(
                spec.apron_res,
                APRON_SIZE_M,
                0,
                apron_octaves,
                APRON_RIVER_MIN_DISCHARGE_M3S,
            ),
        ];
        RegionPrep {
            spec,
            field,
            hydro,
            basis,
            spacing_m,
            lattices,
            layers,
            detail: Detail::new(spacing_m, spec.seed, spec.snapshot),
            phase: Phase::Plan(0),
            builders: [None, None],
            rivers: [None, None],
            boulder_sites: None,
            boulders: Vec::new(),
            boulders_dropped: 0,
            drawn_cache: water::DrawnCache::default(),
            coast: Vec::with_capacity((spec.apron_res * spec.apron_res) as usize),
            units_done: 0,
        }
    }

    fn sampler(&self) -> Sampler<'_, F> {
        Sampler {
            field: self.field,
            spacing_m: self.spacing_m,
            lattices: &self.lattices,
            layers: &self.layers,
            detail: &self.detail,
            hydro: self.hydro,
            rivers: &self.rivers,
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
                            Phase::Rivers(0)
                        };
                    }
                }
                Phase::Rivers(k) => {
                    if self.rivers_step(k, budget, &mut used_any) {
                        self.phase = if k == 0 {
                            Phase::Rivers(1)
                        } else {
                            Phase::Boulders
                        };
                    }
                }
                Phase::Boulders => {
                    if self.boulders_step(budget, &mut used_any) {
                        self.phase = Phase::Mask;
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

    /// Field `k`'s rivers, within `budget`; true once they are done (at once without a pass).
    fn rivers_step(&mut self, k: usize, budget: &mut u32, used_any: &mut bool) -> bool {
        let Some(hydro) = self.hydro else {
            return true;
        };
        if self.rivers[k].is_some() {
            return true;
        }
        let layer = self.layers[k];
        let Some(builder) = self.builders[k].as_mut() else {
            // The gather reads no terrain: one unit.
            self.builders[k] = Some(RiverBuilder::new(
                hydro,
                &self.spec.frame(),
                layer.size_m / 2.0,
                layer.subpoint_m,
                layer.min_q,
            ));
            *budget -= 1;
            *used_any = true;
            return false;
        };
        let lattice = &self.lattices[layer.lattice];
        let (field, detail) = (self.field, &self.detail);
        let floor_at = |d: RenderDir| lattice.elevation(field, d);
        let key_at =
            |d: RenderDir| detail.noise_input(&field.lattice_key(d, lattice.kind_at(field, d)));
        let before = builder.units_done();
        let done = builder.step(&floor_at, &key_at, *budget);
        let used = builder.units_done() - before;
        *budget -= u32::try_from(used).unwrap_or(u32::MAX).min(*budget);
        *used_any |= used > 0;
        if done {
            let builder = self.builders[k].take().expect("a builder ran");
            self.rivers[k] = Some(builder.finish());
        }
        done
    }

    /// The inner field's boulders, a cluster at a time, one unit of `budget` per bed read (a
    /// cluster may overrun the budget by its last few); true once they are done. A cluster reads
    /// its centre's bed, spreads its members by the water there
    /// ([`BoulderCluster::spread_m`](water::BoulderCluster::spread_m)), reads theirs and is
    /// sized by the deepest ([`water::size_cluster`]); its boulders inside the square are kept.
    /// Once all are placed, the [`MAX_BOULDERS`] guard applies.
    fn boulders_step(&mut self, budget: &mut u32, used_any: &mut bool) -> bool {
        let Some(rivers) = &self.rivers[0] else {
            return true;
        };
        let half = self.layers[0].size_m / 2.0;
        if self.boulder_sites.is_none() {
            // Hashes only, no terrain: listed with the first cluster's units.
            let mut clusters = water::boulder_clusters(rivers, &self.basis, half);
            clusters.reverse();
            self.boulders =
                Vec::with_capacity(clusters.iter().map(|c| c.members.len()).sum::<usize>());
            self.boulder_sites = Some(clusters);
            self.boulders_dropped = 0;
        }
        while *budget > 0 {
            let Some(cluster) = self.boulder_sites.as_mut().and_then(Vec::pop) else {
                break;
            };
            for b in self.cluster_boulders(&cluster) {
                let inside = self
                    .basis
                    .dir_to_local(RenderDir(b.dir))
                    .is_some_and(|(x, z)| x.abs() <= half && z.abs() <= half);
                if inside {
                    self.boulders.push(b);
                }
            }
            let units = u32::try_from(cluster.members.len()).unwrap_or(u32::MAX);
            *budget -= units.min(*budget);
            *used_any = true;
        }
        let done = self.boulder_sites.as_ref().is_some_and(Vec::is_empty);
        if done {
            // The guard: the keep fraction keeps every bench region well under it.
            self.boulders_dropped = water::cap_boulders(&mut self.boulders, MAX_BOULDERS as usize);
        }
        done
    }

    /// A cluster's boulders that the drawn water holds (all its members, in or out of the
    /// square): its centre's bed read first, its members spread by the water there, then sized
    /// by the deepest; then fitted to the water every lattice of [`water::DRAWN_LATTICES`]
    /// draws, and given a collar where they draw its footprint wet
    /// ([`water::hold_in_drawn_water`], Task 11c fix rounds 2–4). The lattices' texels are kept
    /// for the next cluster of the same sub-segment.
    fn cluster_boulders(&mut self, cluster: &water::BoulderCluster) -> Vec<Boulder> {
        let mut cache = std::mem::take(&mut self.drawn_cache);
        cache.start((cluster.river, cluster.k));
        let bed = |dir: bevy_math::DVec3| {
            let (carved, water) = self.sampler().texel(FieldKind::Inner, RenderDir(dir));
            water::BoulderBed {
                base_m: carved.h as f32,
                level_m: water.level_m,
                river: water.river,
            }
        };
        let centre = bed(cluster.centre());
        let spread = cluster.spread_m(centre.depth_m());
        let dirs = cluster.member_dirs(spread);
        let beds: Vec<water::BoulderBed> = std::iter::once(centre)
            .chain(dirs[1..].iter().map(|&d| bed(d)))
            .collect();
        let sized = water::size_cluster(cluster, spread, &dirs, &beds);
        let texel_m = self.layers[0].size_m / f64::from(self.layers[0].res - 1);
        let held = {
            let mut lattices =
                water::DrawnLattices::new(cluster.origin(), texel_m, &bed, &mut cache);
            let with_beds = sized.into_iter().zip(beds.iter().copied()).collect();
            water::hold_in_drawn_water(&mut lattices, cluster, with_beds)
        };
        self.drawn_cache = cache;
        held
    }

    /// Work units done so far (planning points, lattice nodes, river sub-points, boulders and
    /// mask texels): a progress measure. The final coast-distance step adds none.
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
            hydro: self.hydro,
            basis: self.basis,
            spacing_m: self.spacing_m,
            lattices: self.lattices,
            layers: self.layers,
            coast_km: self.coast,
            era: vegetation_era(spec.snapshot),
            detail: self.detail,
            rivers: self.rivers,
            boulders: self.boulders,
            boulders_dropped: self.boulders_dropped,
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
    hydro: Option<&'a EraHydrology>,
    basis: FrameBasis,
    spacing_m: f64,
    lattices: Vec<Lattice>,
    layers: [Layer; 2],
    /// Distance to the coast (km) over the apron's texels, from its coarse land mask.
    coast_km: Vec<f32>,
    era: VegetationEra,
    detail: Detail,
    rivers: [Option<RegionRivers>; 2],
    boulders: Vec<Boulder>,
    boulders_dropped: u32,
    next_row: [u32; 2],
    done: [Vec<FieldRows>; 2],
}

impl<'a, F: CoarseField> RegionGenerator<'a, F> {
    /// Builds the generator in one go: [`RegionPrep`] run to completion.
    pub fn new(
        spec: RegionSpec,
        field: &'a F,
        hydro: Option<&'a EraHydrology>,
    ) -> RegionGenerator<'a, F> {
        RegionPrep::new(spec, field, hydro).finish()
    }

    fn sampler(&self) -> Sampler<'_, F> {
        Sampler {
            field: self.field,
            spacing_m: self.spacing_m,
            lattices: &self.lattices,
            layers: &self.layers,
            detail: &self.detail,
            hydro: self.hydro,
            rivers: &self.rivers,
        }
    }

    /// A field's refined rivers (`None` without a pass).
    pub fn rivers(&self, kind: FieldKind) -> Option<&RegionRivers> {
        self.rivers[kind.index()].as_ref()
    }

    /// Boulders past [`MAX_BOULDERS`](crate::hydrology::params::MAX_BOULDERS) that the region
    /// dropped: the shell warns when there are any.
    pub fn boulders_dropped(&self) -> u32 {
        self.boulders_dropped
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

    /// The size in bytes of each allocation holding lattice nodes.
    pub fn node_allocation_bytes(&self) -> Vec<usize> {
        self.lattices
            .iter()
            .flat_map(|l| l.blocks.iter().flatten())
            .flat_map(|b| b.nodes.allocation_bytes())
            .collect()
    }

    /// The bytes the precomputation holds while the generator lives: the lattices' stored nodes,
    /// the coast-distance field, the refined rivers and the boulders, by capacity. For memory
    /// diagnostics.
    pub fn working_bytes(&self) -> usize {
        let nodes: usize = self.node_allocation_bytes().iter().sum();
        let rivers: usize = self.rivers.iter().flatten().map(RegionRivers::bytes).sum();
        nodes
            + self.coast_km.capacity() * std::mem::size_of::<f32>()
            + rivers
            + self.boulders.capacity() * std::mem::size_of::<Boulder>()
    }

    /// The inner field's height and surface at an era-frame direction. Grid texels equal this at
    /// their own directions, and any region with the same snapshot field, seed and inner
    /// resolution gives the same result at the same direction.
    pub fn sample_point(&self, era_dir: RenderDir) -> (f32, [u8; 4]) {
        self.sample_point_in(FieldKind::Inner, era_dir)
    }

    /// [`sample_point`](Self::sample_point) for either field.
    pub fn sample_point_in(&self, kind: FieldKind, era_dir: RenderDir) -> (f32, [u8; 4]) {
        let t = self.point(kind, era_dir);
        (t.h, t.surface)
    }

    /// Everything either field stores at an era-frame direction: grid texels equal this at their
    /// own directions, and any region with the same snapshot field, seed, resolutions and pass
    /// gives the same bits.
    pub fn sample_texel_in(&self, kind: FieldKind, era_dir: RenderDir) -> Texel {
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
            water_level_m: Vec::with_capacity(count),
            river: Vec::with_capacity(count),
        };
        for row in range {
            for col in 0..layer.res {
                let (x, z) = local_of(layer.res, layer.size_m, row, col);
                let t = self.point(kind, self.basis.local_to_dir(x, z));
                out.elevation_m.push(t.h);
                out.surface.push(t.surface);
                out.water_level_m.push(t.water_level_m);
                out.river.push(t.river);
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
                water_level_m: Vec::with_capacity(total),
                river: Vec::with_capacity(total),
                river_mask: Vec::new(),
            };
            for b in blocks {
                field.elevation_m.extend(b.elevation_m);
                field.surface.extend(b.surface);
                field.water_level_m.extend(b.water_level_m);
                field.river.extend(b.river);
            }
            assert!(
                field.elevation_m.len() == total
                    && field.surface.len() == total
                    && field.water_level_m.len() == total
                    && field.river.len() == total,
                "{kind:?} blocks hold {} texels, expected {total}",
                field.elevation_m.len()
            );
            field
        };
        let mut grid = RegionGrid {
            spec: self.spec,
            inner: join(FieldKind::Inner, inner),
            apron: join(FieldKind::Apron, apron),
            boulders: Vec::new(),
        };
        // Not where the river fades into the apron's (Task 11c fix round 2).
        grid.boulders = crate::terrain::drawn::region_boulders(&grid, &self.boulders);
        grid
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
    #[cfg(test)]
    fn coarse_and_corners(&self, kind: FieldKind, dir: RenderDir) -> (Coarse, [f64; 4]) {
        self.sampler().coarse_and_corners(kind, dir)
    }

    /// The inner field's boulders (they go into the grid at [`assemble`](Self::assemble)).
    pub fn boulders(&self) -> &[Boulder] {
        &self.boulders
    }

    fn point(&self, kind: FieldKind, era_dir: RenderDir) -> Texel {
        let (carved, water) = self.sampler().texel(kind, era_dir);
        Texel {
            h: carved.h as f32,
            surface: self.surface(carved.dir, carved.h, &carved.coarse),
            water_level_m: water.level_m,
            river: water.river,
        }
    }

    fn surface(&self, dir: RenderDir, h: f64, coarse: &Coarse) -> [u8; 4] {
        let lat = self.field.paleolat(dir);
        let coast_km = f64::from(self.coast_km_at(dir));
        // The dryness raise (ruling R27), at the step the surface byte stores, so the generator
        // and the shader draw the same snowline.
        let raise_byte = encode_snowline_raise(snowline_raise_m(coarse.koppen, coast_km));
        let raise_m = decode_snowline_raise(raise_byte);
        let biome = match coarse.koppen.and_then(biome_from_koppen) {
            Some(b) => b,
            None => zonal_biome(lat, h, coast_km, self.spec.sea_level_anomaly_m),
        };
        let biome = effective_biome(self.era, biome);
        let snow = if h > 0.0 {
            let line = snowline_with_raise_m(lat, self.spec.sea_level_anomaly_m, raise_m);
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
            shore | raise_byte,
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

/// What a texel reads, borrowed from a [`RegionPrep`] or a [`RegionGenerator`].
struct Sampler<'s, F: CoarseField> {
    field: &'s F,
    spacing_m: f64,
    lattices: &'s [Lattice],
    layers: &'s [Layer; 2],
    detail: &'s Detail,
    hydro: Option<&'s EraHydrology>,
    rivers: &'s [Option<RegionRivers>; 2],
}

/// A texel's terrain after the lakes and rivers.
struct Carved {
    dir: RenderDir,
    /// The stored height: M3's `h0` without a pass.
    h: f64,
    coarse: Coarse,
    sea: bool,
    lake: Option<LakeTap>,
    hit: Option<RiverHit>,
}

impl<F: CoarseField> Sampler<'_, F> {
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

    /// The texel at a direction: its terrain ([`water`]'s `h0`, `h1` and carve) and its water.
    fn texel(&self, kind: FieldKind, era_dir: RenderDir) -> (Carved, Water) {
        let dir = RenderDir(era_dir.0.normalize());
        let layer = self.layers[kind.index()];
        let (coarse, c) = self.coarse_and_corners(kind, dir);
        let s = self.spacing_m;
        let gx = ((c[0] + c[2]) - (c[1] + c[3])) / (2.0 * s);
        let gz = ((c[0] + c[1]) - (c[2] + c[3])) / (2.0 * s);
        let slope = gx.hypot(gz);
        let detail = self.blended_detail_m(dir, &coarse, slope, layer.octaves);
        let h0 = coastline_clamp(coarse.elevation_m + detail, c);
        let h0 = if h0.is_finite() { h0 } else { -1.0 };
        let below_sea = c.iter().any(|&x| x < 0.0);
        let mut carved = Carved {
            dir,
            h: h0,
            coarse,
            sea: below_sea,
            lake: None,
            hit: None,
        };
        if let Some(hydro) = self.hydro {
            carved.lake = hydro.lake_tap(dir);
            let mut h1 = water::lake_bed_m(h0, carved.lake.as_ref());
            // The lake's dam where its reach ends: no water, and ground above its level.
            if let Some(lake) = carved
                .lake
                .filter(|l| water::lake_edge(hydro, dir, l.lake, layer.texel_m))
            {
                h1 = water::dam_m(h1, &lake);
                carved.lake = None;
            }
            let (h, hit) = match &self.rivers[kind.index()] {
                Some(rivers) => {
                    let t = rivers.texel(dir, h1, layer.texel_m);
                    (t.h_m, t.hit)
                }
                None => (h1, None),
            };
            carved.h = if h.is_finite() { h } else { -1.0 };
            carved.hit = hit;
            carved.sea = below_sea && water::ocean_near(hydro, dir);
        }
        let water = water::choose_water(
            carved.h as f32,
            carved.sea,
            carved.lake.as_ref(),
            carved.hit.as_ref(),
        );
        (carved, water)
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
