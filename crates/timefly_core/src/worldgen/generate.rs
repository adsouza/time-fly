//! The region generator (design spec §5.3–§5.5): coarse truth, amplified.
//!
//! `h = coastline_clamp(coarse + D, corners)` at every texel, where:
//! - `coarse` is the coarse field upsampled bilinearly from a lattice of coarse nodes;
//! - `D` is detail noise seeded by the texel's canonical crust-space position (its
//!   [`LatticeKey`]), with only the octaves finer than the source data;
//! - `corners` are the coarse field at `(±spacing/2, ±spacing/2)` along the texel's own east and
//!   north. Where all four agree on land or sea, `h` keeps their side of sea level
//!   ([`coastline_clamp`]), so detail draws fractal coastlines only inside the data's own
//!   uncertainty band and never lifts open ocean.
//!
//! **The coarse lattice is global, not the region's.** Nodes are the texel centres of a cube map
//! whose face-centre texel spans `max(spacing/4, texel)`, and upsampling is
//! [`bilinear_taps`] on that cube. Every texel is therefore a pure function of its era-frame
//! direction, the snapshot field, the seed and the field's resolution: overlapping regions agree
//! exactly, and a grid texel equals [`RegionGenerator::sample_point`] at its direction. A region
//! only decides which nodes `new` precomputes; any other node is computed on demand.
//!
//! Why a lattice at all: the deep source is about 110 km, and resolving plates (every candidate
//! plate's rotation, footprint and endpoint samples) at every 293 m texel would be wasted work.
//! The detail and the lattice key are per texel; only the plate comes from the nearest node.

use std::ops::Range;

use bevy_math::DVec3;
use tracing::instrument;

use crate::cube::{CubeFace, bilinear_taps, texel_center_dir};
use crate::deep_time::YearsBp;
use crate::geo::RenderDir;
use crate::worldgen::biome::{
    BiomeClass, VegetationEra, biome_from_koppen, effective_biome, snowline_m, vegetation_era,
    zonal_biome,
};
use crate::worldgen::coarse::{CoarseField, CoarseSample, KeyKind, LatticeKey};
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis, RegionFrame};
use crate::worldgen::noise::{fbm, hash3, ridged};
use crate::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};

/// Surface byte 3 for a texel in the shoreline band (Task 8 tints Barren-era shores with it).
pub const SURFACE_SHORE: u8 = 255;
/// Heights within this many metres of sea level count as shoreline.
pub const SHORE_BAND_M: f64 = 15.0;
/// Coast distance where the apron holds no coast at all.
pub const COAST_UNKNOWN_KM: f32 = 1.0e6;

/// Detail relief per metre of source spacing: the first octave's amplitude on plains (scaled
/// down on low ground and up to full at [`PLAINS_FULL_M`]) and in mountains.
const PLAINS_RELIEF: f64 = 0.004;
const PLAINS_FULL_M: f64 = 500.0;
const PLAINS_FLOOR: f64 = 0.1;
const MOUNTAIN_RELIEF: f64 = 0.02;
/// Ridged octaves fade in between these coarse elevations, or these coarse gradients (on land).
const MOUNTAIN_START_M: f64 = 1000.0;
const MOUNTAIN_FULL_M: f64 = 2000.0;
const SLOPE_START: f64 = 0.01;
const SLOPE_FULL: f64 = 0.03;
/// The long-run mean of [`ridged`] (0.678 over 400,000 random points on the sphere, the same for
/// any octave count), removed so detail does not shift the coarse trend.
const RIDGED_MEAN: f64 = 0.678;
/// Snow fades in over this many metres either side of the snowline.
const SNOW_RAMP_M: f64 = 150.0;
const MAX_OCTAVES: u32 = 16;

const PLATE_SALT: u32 = 0x9E37_79B9;
const ERA_SALT: u32 = 0x85EB_CA6B;
const RIDGE_SALT: u32 = 0xC2B2_AE35;

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
}

impl RegionSpec {
    /// A spec at the standard resolutions ([`REGION_RES`], [`APRON_RES`]).
    pub fn new(center: RenderDir, snapshot: YearsBp, seed: u32) -> RegionSpec {
        RegionSpec {
            center,
            snapshot,
            seed,
            inner_res: REGION_RES,
            apron_res: APRON_RES,
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

/// The coastline rule (design spec §5.3): where all four corner samples are land the height
/// stays at least +1 m; where all are sea, at most −1 m; mixed corners leave it unchanged.
pub fn coastline_clamp(h_m: f64, corners_m: [f64; 4]) -> f64 {
    if corners_m.iter().all(|&c| c > 0.0) {
        h_m.max(1.0)
    } else if corners_m.iter().all(|&c| c < 0.0) {
        h_m.min(-1.0)
    } else {
        h_m
    }
}

fn smoothstep(edge0: f64, edge1: f64, x: f64) -> f64 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A coarse node: elevation, ice (quantised to bytes, as the output is), Köppen code (0 = none)
/// and its key's kind. Precomputed and on-demand nodes go through the same quantisation, so a
/// texel never depends on which nodes a region happened to precompute.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Node {
    elevation_m: f32,
    ice: u8,
    koppen: u8,
    kind: KeyKind,
}

impl Node {
    fn from_sample(s: CoarseSample) -> Node {
        let e = s.elevation_m;
        // Never let a bad source value become NaN terrain: NaN is sea, infinities clamp.
        let e = if e.is_nan() {
            -1.0
        } else {
            e.clamp(-12_000.0, 12_000.0)
        };
        let ice = if s.ice.is_finite() {
            s.ice.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Node {
            elevation_m: e as f32,
            ice: (ice * 255.0).round() as u8,
            koppen: s.koppen.unwrap_or(0),
            kind: s.lattice.kind(),
        }
    }
}

/// A stored node: 8 bytes, with the key kind as an index into [`Lattice::kinds`]. A recent
/// region near a cube corner precomputes about 4 million nodes.
#[derive(Copy, Clone, Debug)]
struct PackedNode {
    elevation_m: f32,
    ice: u8,
    koppen: u8,
    kind: u16,
}

/// Precomputed nodes of one cube face, over a column/row rectangle.
#[derive(Clone, Debug)]
struct Block {
    c0: u32,
    r0: u32,
    w: u32,
    h: u32,
    nodes: Vec<PackedNode>,
}

impl Block {
    fn get(&self, col: u32, row: u32) -> Option<PackedNode> {
        let (c, r) = (col.wrapping_sub(self.c0), row.wrapping_sub(self.r0));
        (c < self.w && r < self.h).then(|| self.nodes[(r * self.w + c) as usize])
    }
}

/// The interpolated coarse field at a direction.
struct Coarse {
    elevation_m: f64,
    ice: f64,
    /// The highest-weight tap's key kind: the texel's plate.
    kind: KeyKind,
    /// The highest-weight tap with a Köppen code, if any.
    koppen: Option<u8>,
}

/// The global coarse lattice of one cube size, with the nodes a region needs precomputed.
#[derive(Clone, Debug)]
struct Lattice {
    n: u32,
    blocks: [Option<Block>; 6],
    /// The distinct key kinds of the stored nodes.
    kinds: Vec<KeyKind>,
}

impl Lattice {
    /// Precomputes every node the square of half-side `half_m` around the frame can tap.
    fn build<F: CoarseField>(field: &F, n: u32, basis: &FrameBasis, half_m: f64) -> Lattice {
        const POINTS: u32 = 128;
        let mut bbox: [Option<[u32; 4]>; 6] = [None; 6];
        for a in 0..=POINTS {
            for b in 0..=POINTS {
                let x = -half_m + 2.0 * half_m * f64::from(a) / f64::from(POINTS);
                let z = -half_m + 2.0 * half_m * f64::from(b) / f64::from(POINTS);
                for ((face, col, row), _) in bilinear_taps(basis.local_to_dir(x, z), n) {
                    let bb = &mut bbox[face as usize];
                    *bb = Some(match *bb {
                        None => [col, col, row, row],
                        Some([c0, c1, r0, r1]) => {
                            [c0.min(col), c1.max(col), r0.min(row), r1.max(row)]
                        }
                    });
                }
            }
        }
        // Between sample points the footprint can reach a little further: by one point step,
        // in nodes as small as a cube corner's (under half a face-centre node).
        let node_m = 2.0 * EARTH_RADIUS_M / f64::from(n);
        let step_m = 2.0 * half_m / f64::from(POINTS);
        let grow = (step_m / (0.45 * node_m)).ceil() as u32 + 2;
        let mut blocks: [Option<Block>; 6] = Default::default();
        let mut kinds: Vec<KeyKind> = Vec::new();
        for face in CubeFace::ALL {
            let Some([c0, c1, r0, r1]) = bbox[face as usize] else {
                continue;
            };
            let (c0, r0) = (c0.saturating_sub(grow), r0.saturating_sub(grow));
            let (c1, r1) = ((c1 + grow).min(n - 1), (r1 + grow).min(n - 1));
            let (w, h) = (c1 - c0 + 1, r1 - r0 + 1);
            let mut nodes = Vec::with_capacity((w * h) as usize);
            for row in r0..=r1 {
                for col in c0..=c1 {
                    let node = Node::from_sample(field.sample(texel_center_dir(face, col, row, n)));
                    // Kinds repeat in long runs (one plate, or geographic everywhere).
                    let kind = match kinds.iter().rposition(|&k| k == node.kind) {
                        Some(i) => i,
                        None => {
                            kinds.push(node.kind);
                            kinds.len() - 1
                        }
                    };
                    nodes.push(PackedNode {
                        elevation_m: node.elevation_m,
                        ice: node.ice,
                        koppen: node.koppen,
                        kind: u16::try_from(kind).expect("fewer than 65,536 plates"),
                    });
                }
            }
            blocks[face as usize] = Some(Block {
                c0,
                r0,
                w,
                h,
                nodes,
            });
        }
        Lattice { n, blocks, kinds }
    }

    fn node<F: CoarseField>(&self, field: &F, face: CubeFace, col: u32, row: u32) -> Node {
        match self.blocks[face as usize]
            .as_ref()
            .and_then(|b| b.get(col, row))
        {
            Some(p) => Node {
                elevation_m: p.elevation_m,
                ice: p.ice,
                koppen: p.koppen,
                kind: self.kinds[p.kind as usize],
            },
            None => Node::from_sample(field.sample(texel_center_dir(face, col, row, self.n))),
        }
    }

    fn interp<F: CoarseField>(&self, field: &F, dir: RenderDir) -> Coarse {
        let (mut e, mut ice) = (0.0, 0.0);
        let (mut best_w, mut kind) = (-1.0, KeyKind::EraFixed);
        let (mut koppen_w, mut koppen) = (-1.0, None);
        for ((face, col, row), w) in bilinear_taps(dir, self.n) {
            let node = self.node(field, face, col, row);
            e += w * f64::from(node.elevation_m);
            ice += w * f64::from(node.ice) / 255.0;
            if w > best_w {
                best_w = w;
                kind = node.kind;
            }
            if node.koppen != 0 && w > koppen_w {
                koppen_w = w;
                koppen = Some(node.koppen);
            }
        }
        Coarse {
            elevation_m: e,
            ice,
            kind,
            koppen,
        }
    }

    fn elevation<F: CoarseField>(&self, field: &F, dir: RenderDir) -> f64 {
        bilinear_taps(dir, self.n)
            .iter()
            .map(|&((face, col, row), w)| {
                w * f64::from(self.node(field, face, col, row).elevation_m)
            })
            .sum()
    }
}

/// Per-field constants.
#[derive(Copy, Clone, Debug)]
struct Layer {
    res: u32,
    size_m: f64,
    /// Index into `RegionGenerator::lattices`.
    lattice: usize,
    octaves: u32,
}

/// Generates a region's fields from a coarse field. `new` precomputes the coarse lattice nodes,
/// their plates and the coast distance; [`rows`](Self::rows) then produces any block of rows of
/// either field, independently and in any order (Task 5 runs blocks in parallel on native and
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
    era_fixed_seed: u32,
    next_row: [u32; 2],
    done: [Vec<FieldRows>; 2],
}

impl<'a, F: CoarseField> RegionGenerator<'a, F> {
    #[instrument(level = "debug", skip_all, fields(inner_res = spec.inner_res, apron_res = spec.apron_res))]
    pub fn new(spec: RegionSpec, field: &'a F) -> RegionGenerator<'a, F> {
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
            let octaves = if spacing_m >= 2.0 * texel_m {
                ((spacing_m / (2.0 * texel_m)).log2().floor() as u32 + 1).min(MAX_OCTAVES)
            } else {
                0
            };
            // Corners reach half a spacing past the edge; taps one node beyond that.
            let half_m = size_m / 2.0 + spacing_m / 2.0 + 2.0 * lattice_m;
            (n, octaves, half_m)
        };
        let (apron_n, apron_octaves, apron_half) = layer_params(spec.apron_res, APRON_SIZE_M);
        let (inner_n, inner_octaves, inner_half) = layer_params(spec.inner_res, REGION_SIZE_M);
        let mut lattices = vec![Lattice::build(field, apron_n, &basis, apron_half)];
        let inner_lattice = if inner_n == apron_n {
            0
        } else {
            lattices.push(Lattice::build(field, inner_n, &basis, inner_half));
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
        let coast_km = coast_distance_km(field, &lattices[0], &basis, spec.apron_res);
        let age_bits = spec.snapshot.0.to_bits();
        let era_fixed_seed = hash3(
            [age_bits as i32, (age_bits >> 32) as i32, 0],
            spec.seed ^ ERA_SALT,
        );
        RegionGenerator {
            spec,
            field,
            basis,
            spacing_m,
            lattices,
            layers,
            coast_km,
            era: vegetation_era(spec.snapshot),
            era_fixed_seed,
            next_row: [0, 0],
            done: [Vec::new(), Vec::new()],
        }
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
        let key = self.field.lattice_key(dir, coarse.kind);
        let detail = self.detail_m(&key, coarse.elevation_m, slope, layer.octaves);
        let h = coastline_clamp(coarse.elevation_m + detail, c);
        let h = if h.is_finite() { h } else { -1.0 };
        (h as f32, self.surface(dir, h, &coarse))
    }

    /// The detail noise `D` (m) for a texel: ridged octaves in mountains (high or steep land),
    /// fBm elsewhere, from the source spacing down to twice the texel.
    fn detail_m(&self, key: &LatticeKey, coarse_m: f64, slope: f64, octaves: u32) -> f64 {
        if octaves == 0 {
            return 0.0;
        }
        let (p, seed) = self.noise_input(key);
        let s = self.spacing_m;
        // Raw octave sum: fbm and ridged normalise by the total amplitude, which differs between
        // octave counts; undoing it keeps the apron's octaves a prefix of the inner field's.
        let total = 2.0 * (1.0 - 0.5f64.powi(octaves as i32));
        let mountain = if coarse_m > 0.0 {
            smoothstep(MOUNTAIN_START_M, MOUNTAIN_FULL_M, coarse_m).max(smoothstep(
                SLOPE_START,
                SLOPE_FULL,
                slope,
            ))
        } else {
            0.0
        };
        let mut d = 0.0;
        if mountain < 1.0 {
            let amp = PLAINS_RELIEF * s * (coarse_m.abs() / PLAINS_FULL_M).clamp(PLAINS_FLOOR, 1.0);
            let n = f64::from(fbm(p, s, octaves, 2.0, 0.5, seed)) * total;
            d += (1.0 - mountain) * amp * n;
        }
        if mountain > 0.0 {
            let n = (f64::from(ridged(p, s, octaves, seed ^ RIDGE_SALT)) - RIDGED_MEAN) * total;
            d += mountain * MOUNTAIN_RELIEF * s * n;
        }
        d
    }

    /// Noise position (m on the sphere) and seed of a lattice key.
    fn noise_input(&self, key: &LatticeKey) -> (DVec3, u32) {
        match *key {
            LatticeKey::Geographic(v) => (v.normalize() * EARTH_RADIUS_M, self.spec.seed),
            LatticeKey::Tracked {
                plate_id,
                plate_fixed,
            } => (
                plate_fixed.normalize() * EARTH_RADIUS_M,
                hash3([plate_id.0 as i32, 0, 0], self.spec.seed ^ PLATE_SALT),
            ),
            LatticeKey::EraFixed(v) => (v.normalize() * EARTH_RADIUS_M, self.era_fixed_seed),
        }
    }

    fn surface(&self, dir: RenderDir, h: f64, coarse: &Coarse) -> [u8; 4] {
        let lat = self.field.paleolat(dir);
        let biome = match coarse.koppen.and_then(biome_from_koppen) {
            Some(b) => b,
            None => zonal_biome(lat, h, f64::from(self.coast_km_at(dir))),
        };
        let biome = effective_biome(self.era, biome);
        let snow = if h > 0.0 {
            let line = snowline_m(lat);
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

/// A two-pass chamfer distance transform (weights 1 and √2) over the apron's coarse land mask:
/// each land texel's distance (km) to the nearest sea texel; 0 at sea; [`COAST_UNKNOWN_KM`]
/// everywhere if the apron holds no sea.
fn coast_distance_km<F: CoarseField>(
    field: &F,
    lattice: &Lattice,
    basis: &FrameBasis,
    res: u32,
) -> Vec<f32> {
    let n = res as usize;
    let mut d = vec![f32::INFINITY; n * n];
    for row in 0..res {
        for col in 0..res {
            let (x, z) = local_of(res, APRON_SIZE_M, row, col);
            if lattice.elevation(field, basis.local_to_dir(x, z)) <= 0.0 {
                d[row as usize * n + col as usize] = 0.0;
            }
        }
    }
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
