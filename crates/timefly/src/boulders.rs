//! The boulders in a region's rapids (rivers and lakes design §4.2, §4.4; Task 9), drawn as the
//! first instanced scatter ([`crate::scatter`]).
//!
//! - **What.** The region generator lists the inner field's boulders (`RegionGrid::boulders`):
//!   each a direction, the channel bed's point height under it, a radius, a seed and the share
//!   of its height under the water. [`BoulderPlugin`] turns them into one [`ScatterSet`] of
//!   [`ScatterInstance`]s whenever the terrain scene takes a new grid (a build, a replace once
//!   any held swap is released), and empties it when the scene goes.
//! - **Where.** The bed height is a point value that regions share, but the terrain draws
//!   293 m texels, where a channel tens of metres wide isn't carved at all, so a boulder at its
//!   bed height could float or vanish underground. So each boulder is placed against what is
//!   drawn ([`boulder_instance`]): in drawn water, as deep under the drawn level as under its
//!   own; elsewhere, set [`BOULDER_EMBED`] of its height into the drawn ground
//!   ([`drawn_ground_m`], the terrain's finest triangles, unmorphed, which draw every boulder
//!   within [`BOULDER_VISIBLE_M`] below [`BOULDER_MAX_ELEVATION_M`]; higher ones aren't
//!   drawn); never more than half under the drawn ground. Its tag's wetness is
//!   the share under the drawn water, so the waterline the shader darkens is where the water
//!   meets it.
//! - **How it looks.** [`boulder_meshes`]: four jittered icospheres, one subdivision (80
//!   triangles), flat-shaded, spanning `y ∈ [−1, 1]`; each boulder is one of them scaled by its
//!   radius and turned by its seed. Its colour is the terrain palette's rock for the region's
//!   look ([`boulder_look`]), jittered in hue and value by the seed, and wet-dark at and above
//!   the waterline.
//! - **Foam collars** (Task 11c). Every boulder at a foam site (`Boulder::foam_site`: standing
//!   in the water, its top less than half a metre under it or above it) that stands in drawn
//!   water gets a collar, the second scatter kind ([`ScatterKind::FoamCollar`]): one flat quad
//!   ([`foam_collar_mesh`]) at the drawn water level, scaled by the boulder's radius and turned
//!   so its `+z` runs downstream along the boulder's flow ([`foam_collar_instance`]). Its tag
//!   carries the drawn rock's radius at the waterline (as a share of its radius) and the seed.
//!   `scatter.wesl` cuts it out (alpha-masked: no sorting): a ring hugging the rock, thicker on
//!   its upstream face, and a wake about [`FOAM_COLLAR_WAKE_RADII`] radii long, its edge broken
//!   by noise that churns with time and runs downstream. It takes the water's own depth pull
//!   plus [`FOAM_COLLAR_PULL_M`], so the water at its level never hides it.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::PrimitiveTopology;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use timefly_core::geo::RenderDir;
use timefly_core::terrain::{LodSettings, blended_height_m, blended_surface_m};
use timefly_core::worldgen::frame::{FrameBasis, RegionFrame};
use timefly_core::worldgen::generate::RegionGrid;
use timefly_core::worldgen::generate::water::Boulder;

use crate::scatter::{
    ScatterInstance, ScatterKind, ScatterLook, ScatterMaterial, ScatterPool, ScatterSet,
    scatter_material,
};
use crate::terrain::material::{PaletteKind, palette_image};
use crate::terrain::view::full_sun_illumination;
use crate::terrain::{TerrainScene, TerrainSceneSystems};
use crate::water::{WATER_DEPTH_PULL, WATER_F0, WATER_PULL_M, WHITE_WATER};

/// Boulders are drawn out to this distance from the camera (m).
pub const BOULDER_VISIBLE_M: f32 = 6_000.0;
/// The boulder meshes ([`boulder_meshes`]).
pub const BOULDER_SHAPES: usize = 4;
/// Each mesh vertex's distance from the centre varies by this share (±half of it).
pub const BOULDER_JITTER: f32 = 0.35;
/// A boulder not in drawn water is set this share of its height into the drawn ground.
pub const BOULDER_EMBED: f32 = 0.2;
/// The seed turns a boulder's colour by up to this much about the grey axis (± radians)...
pub const BOULDER_HUE_JITTER_RAD: f32 = 0.15;
/// ...and brightens or darkens it by up to this share.
pub const BOULDER_VALUE_JITTER: f32 = 0.15;
/// A wet boulder's colour darkens by this share under the waterline...
pub const BOULDER_WET_DARKEN: f32 = 0.45;
/// ...fading out over this share of its height above it (the splash).
pub const BOULDER_WET_BAND: f32 = 0.2;
/// Boulders farther than this from sea level (m) aren't drawn: within it, every drawn
/// boulder lies in the terrain's finest LOD, unmorphed, which [`drawn_ground_m`] assumes (the
/// CDLOD measures distance as `√(h² + camera_y²)`, so a high boulder seen from
/// [`BOULDER_VISIBLE_M`] away can reach LOD 0's morph at 13,945 m). No river has rapids this
/// high on today's Earth.
pub const BOULDER_MAX_ELEVATION_M: f32 = 7_500.0;
/// The least light a boulder's face shows, as a share of full sun: the terrain light ramp's
/// shade band, so a face turned from the sun reads as the terrain's shade, never black.
pub const BOULDER_SHADE_FLOOR: f32 = 0.16;
/// Foam collars are drawn out to this distance from the camera (m); farther, a collar is a
/// pixel or two and the water's own white water stands for it.
pub const FOAM_COLLAR_VISIBLE_M: f32 = 3_000.0;
/// The collar quad's extent, in boulder radii: half its width across the flow, how far it
/// reaches upstream and how far downstream of the boulder's centre.
pub const FOAM_COLLAR_EXTENT: [f32; 3] = [2.0, 2.0, 4.5];
/// The wake trails about this many radii downstream of the rock: a short tail (fix round 1,
/// the owner's look; was 4).
pub const FOAM_COLLAR_WAKE_RADII: f32 = 3.0;
/// A collar is pulled this much (m) nearer the camera than the water's own pull, so it wins
/// over the water at its level, also where the water's mesh interpolates a little above it.
pub const FOAM_COLLAR_PULL_M: f32 = 0.3;

/// The four boulder shapes: jittered icospheres of one subdivision (80 triangles), flat
/// normals, positions and normals only, spanning `y ∈ [−1, 1]` exactly (so a tag's wetness is a
/// share of the drawn height), wound to face outward.
pub fn boulder_meshes() -> [Mesh; BOULDER_SHAPES] {
    let (sphere, faces) = icosphere();
    std::array::from_fn(|k| {
        // Per shape: a stretch along x and z and a squash in y, then a radius per vertex.
        let stretch = [
            Vec3::new(1.0, 0.85, 1.15),
            Vec3::new(1.2, 0.8, 1.0),
            Vec3::new(1.05, 0.9, 1.1),
            Vec3::new(1.25, 0.75, 1.2),
        ][k];
        let mut points: Vec<Vec3> = sphere
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let h = jitter_hash(k as u32, i as u32);
                *p * stretch * (1.0 + BOULDER_JITTER * (h - 0.5))
            })
            .collect();
        // Span y in [-1, 1] exactly: centre the height, then scale every axis alike.
        let (lo, hi) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
        let (mid, scale) = ((lo + hi) / 2.0, 2.0 / (hi - lo));
        for p in &mut points {
            *p = Vec3::new(p.x, p.y - mid, p.z) * scale;
            // The extremes land on ±1 exactly (no rounding drift).
            if (p.y - 1.0).abs() < 1e-5 {
                p.y = 1.0;
            } else if (p.y + 1.0).abs() < 1e-5 {
                p.y = -1.0;
            }
        }
        flat_mesh(&points, &faces)
    })
}

/// A unit icosahedron subdivided once (42 vertices, 80 faces), each face wound to face outward.
fn icosphere() -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let t = (1.0 + 5.0f32.sqrt()) / 2.0;
    let mut points: Vec<Vec3> = [
        (-1.0, t, 0.0),
        (1.0, t, 0.0),
        (-1.0, -t, 0.0),
        (1.0, -t, 0.0),
        (0.0, -1.0, t),
        (0.0, 1.0, t),
        (0.0, -1.0, -t),
        (0.0, 1.0, -t),
        (t, 0.0, -1.0),
        (t, 0.0, 1.0),
        (-t, 0.0, -1.0),
        (-t, 0.0, 1.0),
    ]
    .iter()
    .map(|&(x, y, z)| Vec3::new(x, y, z).normalize())
    .collect();
    let base: [[u32; 3]; 20] = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    let mut midpoints = std::collections::HashMap::new();
    let mut mid = |a: u32, b: u32, points: &mut Vec<Vec3>| -> u32 {
        *midpoints.entry((a.min(b), a.max(b))).or_insert_with(|| {
            points.push(((points[a as usize] + points[b as usize]) / 2.0).normalize());
            points.len() as u32 - 1
        })
    };
    let mut faces = Vec::with_capacity(80);
    for [a, b, c] in base {
        let (ab, bc, ca) = (
            mid(a, b, &mut points),
            mid(b, c, &mut points),
            mid(c, a, &mut points),
        );
        faces.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
    }
    for f in &mut faces {
        let [a, b, c] = f.map(|i| points[i as usize]);
        if (b - a).cross(c - a).dot(a + b + c) < 0.0 {
            f.swap(1, 2);
        }
    }
    (points, faces)
}

/// A hash of `(shape, vertex)` as a fraction in [0, 1).
fn jitter_hash(shape: u32, vertex: u32) -> f32 {
    let mut h = shape.wrapping_mul(0x9E37_79B1) ^ vertex.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7FEB_352D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846C_A68B);
    h ^= h >> 16;
    (h >> 8) as f32 / 16_777_216.0
}

/// A flat-shaded triangle list: three vertices a face, each with the face's normal. Kept in the
/// main world too (the default usages: 240 vertices a shape), so every pooled slot spawned later
/// gets its bounds from it.
fn flat_mesh(points: &[Vec3], faces: &[[u32; 3]]) -> Mesh {
    let mut positions = Vec::with_capacity(faces.len() * 3);
    let mut normals = Vec::with_capacity(faces.len() * 3);
    for f in faces {
        let [a, b, c] = f.map(|i| points[i as usize]);
        let n = (b - a).cross(c - a).normalize();
        positions.extend([a, b, c].map(|p| p.to_array()));
        normals.extend([n.to_array(); 3]);
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
}

/// The drawn ground at local `(x, z)` metres within the terrain's finest LOD (everything within
/// [`BOULDER_VISIBLE_M`] of the camera, unmorphed): the triangles of the finest grid
/// ([`LodSettings::finest_quad_m`] quads from the root's corner, each split on its
/// `(i, j)–(i + 1, j + 1)` diagonal as `grid_mesh` splits it) over [`blended_height_m`] at
/// their corners.
pub fn drawn_ground_m(grid: &RegionGrid, x_m: f64, z_m: f64) -> f32 {
    let settings = LodSettings::region();
    let quad = settings.finest_quad_m();
    let root = -settings.extent_m() / 2.0;
    let (u, v) = ((x_m - root) / quad, (z_m - root) / quad);
    let (i, j) = (u.floor(), v.floor());
    let (fu, fv) = ((u - i) as f32, (v - j) as f32);
    let at =
        |di: f64, dj: f64| blended_height_m(grid, root + (i + di) * quad, root + (j + dj) * quad);
    let (a, b, c, d) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0), at(1.0, 1.0));
    if fu >= fv {
        a + fu * (b - a) + fv * (d - b)
    } else {
        a + fv * (c - a) + fu * (d - c)
    }
}

/// Where boulder `b` is drawn in a region with `basis` and fields `grid` (see the module docs):
/// its mesh's centre, radius, yaw and tag; `None` at the region's antipode, or with its centre
/// farther than [`BOULDER_MAX_ELEVATION_M`] from sea level.
pub fn boulder_instance(
    grid: &RegionGrid,
    basis: &FrameBasis,
    b: &Boulder,
) -> Option<ScatterInstance> {
    let (x, z) = basis.dir_to_local(RenderDir(b.dir))?;
    let r = b.radius_m;
    let height = 2.0 * r;
    let ground = drawn_ground_m(grid, x, z);
    // The water is drawn where its level stands over the bilinear ground (`blended_water`'s
    // thickness), at that level.
    let level = blended_surface_m(grid, x, z);
    let in_water = level > blended_height_m(grid, x, z);
    let bottom = if in_water && b.wet > 0.0 {
        level - b.wet * height
    } else {
        let rd = f64::from(r);
        [(rd, 0.0), (-rd, 0.0), (0.0, rd), (0.0, -rd)]
            .iter()
            .map(|&(dx, dz)| drawn_ground_m(grid, x + dx, z + dz))
            .fold(ground, f32::min)
            - BOULDER_EMBED * height
    }
    .max(ground - r);
    if (bottom + r).abs() > BOULDER_MAX_ELEVATION_M {
        return None;
    }
    let wet = if in_water {
        (level - bottom) / height
    } else {
        0.0
    };
    Some(ScatterInstance {
        translation: Vec3::new(x as f32, bottom + r, z as f32),
        scale: r,
        yaw: boulder_yaw(b.seed),
        tag: ScatterInstance::tag_of(wet, b.seed),
    })
}

/// A boulder's turn about `+y` from its whole seed (radians, in [0, 2π)).
fn boulder_yaw(seed: u32) -> f32 {
    jitter_hash(seed, 0x5CA7_7E12) * std::f32::consts::TAU
}

/// Every boulder of `grid`, as drawn in its own frame.
pub fn boulder_instances(grid: &RegionGrid) -> Arc<[ScatterInstance]> {
    let basis = grid.spec.frame().basis();
    grid.boulders
        .iter()
        .filter_map(|b| boulder_instance(grid, &basis, b))
        .collect()
}

/// The boulders' look in `grid`'s region: the palette's rock for the region's look (era and
/// biome), the jitters and the wetness, the water's depth pull, and the terrain's shade floor.
pub fn boulder_look(grid: &RegionGrid) -> ScatterLook {
    let (era, biome) = crate::sky::region_look(grid);
    ScatterLook {
        texel: UVec4::new(
            biome as u32 * 3 + PaletteKind::Rock as u32,
            era as u32,
            ScatterKind::Boulders.shader_mode(),
            0,
        ),
        colour: Vec4::new(
            BOULDER_HUE_JITTER_RAD,
            BOULDER_VALUE_JITTER,
            BOULDER_WET_DARKEN,
            BOULDER_WET_BAND,
        ),
        pull: Vec4::new(WATER_PULL_M, WATER_DEPTH_PULL, EARTH_RADIUS_M, 0.0),
        light: Vec4::new(BOULDER_SHADE_FLOOR * full_sun_illumination(), 0.0, 0.0, 0.0),
    }
}

/// The foam collar's quad: `x ∈ ±FOAM_COLLAR_EXTENT[0]` across, `z` from
/// `−FOAM_COLLAR_EXTENT[1]` (upstream) to `FOAM_COLLAR_EXTENT[2]` (downstream), at `y = 0`,
/// facing up; positions and normals only, as the boulders' meshes.
pub fn foam_collar_mesh() -> Mesh {
    let [w, up, down] = FOAM_COLLAR_EXTENT;
    let corners = [
        Vec3::new(-w, 0.0, -up),
        Vec3::new(w, 0.0, -up),
        Vec3::new(w, 0.0, down),
        Vec3::new(-w, 0.0, down),
    ];
    // Counter-clockwise seen from above.
    flat_mesh(&corners, &[[0, 2, 1], [0, 3, 2]])
}

/// A foam collar's shape at `p` (radii; `x` across, `y` downstream) round a rock whose
/// waterline is `w` of its radius (0 for a rock just under), its tail `wake` radii long: 1 in
/// the ring (from just inside the waterline to 0.45 radii out, 0.35 more on the upstream face,
/// where the water piles) and along the tail, 0 elsewhere. A rock just under gets a boil of
/// 0.7 radii and a short tail. `scatter.wesl`'s `collar_shape`, which breaks its edge with noise.
pub fn collar_shape(p: Vec2, w: f32, wake: f32) -> f32 {
    let step = |e0: f32, e1: f32, x: f32| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let rho = p.length();
    let up = (-p.y / rho.max(1e-3)).clamp(0.0, 1.0);
    let breaks = w > 0.0;
    let inner = 0.8 * w;
    let edge = if breaks { w + 0.45 } else { 0.7 } + 0.35 * up;
    let ring = (1.0 - step(edge - 0.15, edge, rho)) * step(inner - 0.1, inner, rho);
    let half = if breaks { 0.8 * w + 0.15 } else { 0.45 } * (1.0 + 0.1 * p.y.max(0.0));
    let tail = if breaks { wake } else { 0.6 };
    let trail = (1.0 - step(0.6 * half, half, p.x.abs()))
        * step(-0.2, 0.4, p.y)
        * (1.0 - step(0.5 * tail, tail, p.y));
    ring.max(trail)
}

/// The flow at `dir` (`[east, north]` in `dir`'s own frame, m/s) as a unit vector in the
/// region's local `(x, z)` axes of `basis`: carried through the frames exactly, not by a sign
/// flip (they turn apart away from the region's centre). `None` where there is no flow.
pub fn flow_local(basis: &FrameBasis, dir: bevy::math::DVec3, flow_mps: [f32; 2]) -> Option<Vec2> {
    let (north, east) = RegionFrame {
        center: RenderDir(dir),
    }
    .north_east();
    let v = east * f64::from(flow_mps[0]) + north * f64::from(flow_mps[1]);
    let speed = v.length();
    if speed.is_nan() || speed <= 1e-6 {
        return None;
    }
    // 10 m along the flow.
    let a = 10.0 / (f64::from(EARTH_RADIUS_M));
    let ahead = (dir * a.cos() + v / speed * a.sin()).normalize();
    let (x0, z0) = basis.dir_to_local(RenderDir(dir))?;
    let (x1, z1) = basis.dir_to_local(RenderDir(ahead))?;
    Vec2::new((x1 - x0) as f32, (z1 - z0) as f32).try_normalize()
}

/// The foam collar of boulder `b`, drawn as `rock` (its [`boulder_instance`]): at a foam site
/// standing in drawn water, a collar at the drawn level over its centre, scaled by its radius
/// and turned so its `+z` runs along the flow (by its seed where there is none). Its tag holds
/// the drawn rock's radius at the waterline as a share of its radius (0 for a rock just under
/// the water) and the seed.
pub fn foam_collar_instance(
    grid: &RegionGrid,
    basis: &FrameBasis,
    b: &Boulder,
    rock: &ScatterInstance,
) -> Option<ScatterInstance> {
    b.foam_site()?;
    let (x, z) = (f64::from(rock.translation.x), f64::from(rock.translation.z));
    let level = blended_surface_m(grid, x, z);
    if level <= blended_height_m(grid, x, z) {
        return None;
    }
    let r = rock.scale;
    // The drawn rock's waterline, in its own height: -1 at its bottom, 1 at its top.
    let y = (level - rock.translation.y) / r;
    if y > 1.0 + timefly_core::hydrology::params::FOAM_SITE_DEPTH_M / r {
        return None;
    }
    let share = (1.0 - y.clamp(-1.0, 1.0).powi(2)).sqrt();
    let yaw = flow_local(basis, b.dir, b.flow_mps)
        .map_or_else(|| boulder_yaw(b.seed), |f| f.x.atan2(f.y));
    Some(ScatterInstance {
        translation: Vec3::new(rock.translation.x, level, rock.translation.z),
        scale: r,
        yaw: yaw.rem_euclid(std::f32::consts::TAU),
        tag: ScatterInstance::tag_of(if y < 1.0 { share } else { 0.0 }, b.seed),
    })
}

/// Every boulder's foam collar in `grid`, in boulder order (only foam sites are placed).
pub fn foam_collar_instances(grid: &RegionGrid) -> Arc<[ScatterInstance]> {
    region_scatter(grid).1
}

/// `grid`'s boulders and their foam collars, as drawn, in boulder order: each boulder placed
/// once, and a collar placed only at a foam site (fix round 1: placing every boulder twice
/// took 4.7 ms of the Himalayas' swap frame).
pub fn region_scatter(grid: &RegionGrid) -> (Arc<[ScatterInstance]>, Arc<[ScatterInstance]>) {
    let basis = grid.spec.frame().basis();
    let mut rocks = Vec::with_capacity(grid.boulders.len());
    let mut collars = Vec::new();
    for b in &grid.boulders {
        let Some(rock) = boulder_instance(grid, &basis, b) else {
            continue;
        };
        if b.foam_site().is_some()
            && let Some(collar) = foam_collar_instance(grid, &basis, b, &rock)
        {
            collars.push(collar);
        }
        rocks.push(rock);
    }
    (rocks.into(), collars.into())
}

/// The collars' look: the foam mode, the water's white water colour (linear) and the wake's
/// length, the water's depth pull plus [`FOAM_COLLAR_PULL_M`], and the boulders' shade floor.
pub fn foam_collar_look() -> ScatterLook {
    let white = WHITE_WATER.to_linear();
    ScatterLook {
        texel: UVec4::new(0, 0, ScatterKind::FoamCollar.shader_mode(), 0),
        colour: Vec4::new(white.red, white.green, white.blue, FOAM_COLLAR_WAKE_RADII),
        pull: Vec4::new(
            WATER_PULL_M,
            WATER_DEPTH_PULL,
            EARTH_RADIUS_M,
            FOAM_COLLAR_PULL_M,
        ),
        light: Vec4::new(BOULDER_SHADE_FLOOR * full_sun_illumination(), 0.0, 0.0, 0.0),
    }
}

/// The collars' material (shared with the pipeline warm-up so its key matches): the scatter
/// material, alpha-masked (drawn in the alpha-mask phase with the water: no sorting), rough as
/// the water's foam and with its reflectance.
pub fn foam_collar_material(palette: &Handle<Image>) -> ScatterMaterial {
    let mut m = scatter_material(palette, foam_collar_look());
    m.base.alpha_mode = AlphaMode::Mask(0.5);
    m.base.perceptual_roughness = 0.9;
    m.base.reflectance = (WATER_F0 / 0.16).sqrt();
    m
}

/// The Earth's radius (m) the scatter's curvature drop uses, as the terrain's.
const EARTH_RADIUS_M: f32 = (crate::globe::EARTH_RADIUS_KM * 1000.0) as f32;

/// The boulders' and their foam collars' set entities, the boulders' material and the grid
/// they draw.
#[derive(Resource, Debug)]
pub struct RegionBoulders {
    pub set: Entity,
    pub material: Handle<ScatterMaterial>,
    /// The foam collars' set entity (its pool holds the collar mesh and material).
    pub collars: Entity,
    /// The scene grid whose boulders the sets hold.
    pub grid: Option<Arc<RegionGrid>>,
}

pub struct BoulderPlugin;

impl Plugin for BoulderPlugin {
    fn build(&self, app: &mut App) {
        add_boulder_systems(app);
    }
}

/// The systems (the scatter's material plugin comes from `ScatterPlugin`), for headless tests.
pub(crate) fn add_boulder_systems(app: &mut App) {
    app.add_systems(Startup, spawn_boulder_set)
        .add_systems(Update, sync_boulders.after(TerrainSceneSystems));
}

/// The boulders' set entity with its pool: the four meshes, the palette, and one material.
fn spawn_boulder_set(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<ScatterMaterial>>,
) {
    let palette = images.add(palette_image());
    // The look is a stand-in until the first region's.
    let look = ScatterLook {
        texel: UVec4::ZERO,
        colour: Vec4::ZERO,
        pull: Vec4::ZERO,
        light: Vec4::ZERO,
    };
    let material = materials.add(scatter_material(&palette, look));
    let shapes = boulder_meshes().map(|m| meshes.add(m)).to_vec();
    let set = commands
        .spawn((
            Name::new("Region boulders"),
            ScatterSet::empty(ScatterKind::Boulders),
            ScatterPool::new(shapes, material.clone(), BOULDER_VISIBLE_M),
        ))
        .id();
    // The collars' look is the same in every region: one material for good.
    let collars = commands
        .spawn((
            Name::new("Region foam collars"),
            ScatterSet::empty(ScatterKind::FoamCollar),
            ScatterPool::new(
                vec![meshes.add(foam_collar_mesh())],
                materials.add(foam_collar_material(&palette)),
                FOAM_COLLAR_VISIBLE_M,
            ),
        ))
        .id();
    commands.insert_resource(RegionBoulders {
        set,
        material,
        collars,
        grid: None,
    });
}

/// Follows the terrain scene's grid: a new grid's boulders and collars replace the sets (and
/// the boulders' look), and no scene empties them.
fn sync_boulders(
    mut commands: Commands,
    scene: Option<Res<TerrainScene>>,
    boulders: Option<ResMut<RegionBoulders>>,
    mut materials: ResMut<Assets<ScatterMaterial>>,
) {
    let Some(mut boulders) = boulders else { return };
    let grid = scene.map(|s| s.grid.clone());
    let same = match (&grid, &boulders.grid) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    let (set, collars) = match &grid {
        Some(g) => {
            let start = Instant::now();
            let (instances, collars) = region_scatter(g);
            if let Some(mut m) = materials.get_mut(&boulders.material) {
                m.extension.look = boulder_look(g);
            }
            info!(
                boulders = instances.len(),
                collars = collars.len(),
                ms = format!("{:.2}", start.elapsed().as_secs_f64() * 1e3),
                "region boulders placed"
            );
            (
                ScatterSet {
                    kind: ScatterKind::Boulders,
                    instances,
                },
                ScatterSet {
                    kind: ScatterKind::FoamCollar,
                    instances: collars,
                },
            )
        }
        None => {
            info!("region boulders removed");
            (
                ScatterSet::empty(ScatterKind::Boulders),
                ScatterSet::empty(ScatterKind::FoamCollar),
            )
        }
    };
    commands.entity(boulders.set).insert(set);
    commands.entity(boulders.collars).insert(collars);
    boulders.grid = grid;
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use bevy::asset::AssetPlugin;
    use bevy::render::storage::ShaderBuffer;
    use timefly_core::deep_time::YearsBp;
    use timefly_core::geo::{geo_to_render, latlon_to_geo};
    use timefly_core::worldgen::frame::RegionFrame;
    use timefly_core::worldgen::generate::{Field, RegionSpec};
    use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

    use super::*;
    use crate::globe::GlobeCamera;
    use crate::region::ActiveRegion;
    use crate::scatter::{ScatterSlot, add_scatter_systems};
    use crate::terrain::material::{TerrainMaterial, palette_texel_index};
    use crate::terrain::{TerrainSettings, add_terrain_systems};
    use bevy::camera::visibility::VisibilityRange;

    /// Mesh positions as exact keys (every vertex comes from one shared position).
    fn key(p: [f32; 3]) -> [u32; 3] {
        p.map(f32::to_bits)
    }

    #[test]
    fn the_boulder_meshes_are_low_poly_and_closed() {
        let meshes = boulder_meshes();
        assert_eq!(meshes.len(), BOULDER_SHAPES);
        let mut shapes = BTreeSet::new();
        for (k, mesh) in meshes.iter().enumerate() {
            assert_eq!(mesh.primitive_topology(), PrimitiveTopology::TriangleList);
            let attributes: Vec<_> = mesh.attributes().map(|(a, _)| a.id).collect();
            assert_eq!(
                attributes,
                [Mesh::ATTRIBUTE_POSITION.id, Mesh::ATTRIBUTE_NORMAL.id],
                "positions and normals only"
            );
            let positions = mesh
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .and_then(|a| a.as_float3())
                .unwrap();
            let normals = mesh
                .attribute(Mesh::ATTRIBUTE_NORMAL)
                .and_then(|a| a.as_float3())
                .unwrap();
            let corners: Vec<usize> = match mesh.indices() {
                Some(i) => i.iter().collect(),
                None => (0..positions.len()).collect(),
            };
            let triangles = corners.len() / 3;
            assert!(
                (20..=80).contains(&triangles),
                "shape {k}: {triangles} triangles"
            );
            // Closed: each edge between two shared positions is crossed once each way, by two
            // triangles wound alike.
            let mut edges: HashMap<([u32; 3], [u32; 3]), usize> = HashMap::new();
            for t in corners.chunks(3) {
                let p = [positions[t[0]], positions[t[1]], positions[t[2]]];
                for e in 0..3 {
                    *edges.entry((key(p[e]), key(p[(e + 1) % 3]))).or_default() += 1;
                }
                // Flat, outward: every corner's normal is the face's.
                let [a, b, c] = p.map(Vec3::from_array);
                let face = (b - a).cross(c - a).normalize();
                assert!(
                    face.dot((a + b + c) / 3.0) > 0.0,
                    "shape {k}: an inward face"
                );
                for &i in t {
                    assert!(Vec3::from_array(normals[i]).dot(face) > 0.9999, "shape {k}");
                }
            }
            for (&(a, b), &n) in &edges {
                assert_eq!(n, 1, "shape {k}: a directed edge used {n} times");
                assert_eq!(edges.get(&(b, a)), Some(&1), "shape {k}: an open edge");
            }
            assert_eq!(edges.len(), 3 * triangles);
            // Spanning y in [-1, 1], so the tag's wetness is a share of the drawn height.
            let (lo, hi) = positions.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
                (lo.min(p[1]), hi.max(p[1]))
            });
            assert!(
                (lo + 1.0).abs() < 1e-6 && (hi - 1.0).abs() < 1e-6,
                "shape {k}: {lo}..{hi}"
            );
            // Roughly round: no vertex far from the unit sphere.
            for p in positions {
                let r = Vec3::from_array(*p).length();
                assert!((0.6..=1.8).contains(&r), "shape {k}: radius {r}");
            }
            let mut sorted: Vec<_> = positions.iter().map(|&p| key(p)).collect();
            sorted.sort();
            shapes.insert(sorted);
        }
        assert_eq!(shapes.len(), BOULDER_SHAPES, "four different shapes");
    }

    /// A region of plane ground `h = 1000 + 0.01 x − 0.02 z` (m), dry, or under standing water
    /// `depth_m` deep where `depth_m > 0`.
    fn plane_grid(depth_m: f32, boulders: Vec<Boulder>) -> RegionGrid {
        let field = |res: u32, size_m: f64| {
            let spacing = size_m / f64::from(res - 1);
            let mut elevation = Vec::new();
            for row in 0..res {
                for col in 0..res {
                    let x = -size_m / 2.0 + f64::from(col) * spacing;
                    let z = -size_m / 2.0 + f64::from(row) * spacing;
                    elevation.push((1000.0 + 0.01 * x - 0.02 * z) as f32);
                }
            }
            let mut f = Field::dry(
                res,
                size_m,
                elevation,
                vec![[6, 0, 0, 0]; (res * res) as usize],
            );
            if depth_m > 0.0 {
                for (w, h) in f.water_level_m.iter_mut().zip(&f.elevation_m) {
                    *w = h + depth_m;
                }
            }
            f
        };
        RegionGrid {
            spec: RegionSpec {
                inner_res: 33,
                apron_res: 17,
                ..RegionSpec::new(geo_to_render(latlon_to_geo(34.2, 74.3)), YearsBp(0.0), 1)
            },
            inner: field(33, REGION_SIZE_M),
            apron: field(17, APRON_SIZE_M),
            boulders,
        }
    }

    fn boulder_at(
        grid: &RegionGrid,
        x: f64,
        z: f64,
        radius_m: f32,
        wet: f32,
        seed: u32,
    ) -> Boulder {
        Boulder {
            dir: grid.spec.frame().local_to_dir(x, z).0,
            base_m: 0.0,
            radius_m,
            seed,
            wet,
            level_m: 0.0,
            flow_mps: [0.0; 2],
        }
    }

    /// A boulder of radius `r` whose core water stands `wet` of its height up it (so its top is
    /// `(1 − wet) × 2r` above the level), flowing `flow` (east, north) m/s.
    fn rock_at(
        grid: &RegionGrid,
        x: f64,
        z: f64,
        r: f32,
        wet: f32,
        flow: [f32; 2],
        seed: u32,
    ) -> Boulder {
        Boulder {
            level_m: wet * 2.0 * r,
            flow_mps: flow,
            ..boulder_at(grid, x, z, r, wet, seed)
        }
    }

    #[test]
    fn foam_collars_ring_rocks_at_the_surface() {
        let water = plane_grid(3.0, Vec::new());
        let basis = water.spec.frame().basis();
        let (x, z) = (120.0, -340.0);
        let level = blended_surface_m(&water, x, z);
        // A rock breaking the surface, flowing east: a collar at the drawn level over its centre,
        // its radius, turned so +z runs east (+x), its tag the waterline's share of the radius.
        let b = rock_at(&water, x, z, 1.5, 0.75, [4.0, 0.0], 9);
        assert!(b.foam_site().is_some());
        let rock = boulder_instance(&water, &basis, &b).unwrap();
        let c = foam_collar_instance(&water, &basis, &b, &rock).unwrap();
        assert_eq!(
            c.translation,
            Vec3::new(rock.translation.x, level, rock.translation.z)
        );
        assert_eq!(c.scale, 1.5);
        let downstream = c.transform().rotation * Vec3::Z;
        assert!(downstream.abs_diff_eq(Vec3::X, 1e-3), "{downstream}");
        // The waterline is half way up the top half: sqrt(1 − 0.5²) of the radius.
        let y = (level - rock.translation.y) / 1.5;
        assert!((y - 0.5).abs() < 1e-3, "{y}");
        assert!(
            (c.wet() - 0.75f32.sqrt()).abs() < 1.0 / 255.0,
            "{}",
            c.wet()
        );
        assert_eq!(c.seed(), 9);
        // Flowing north: +z runs north, which is region -z.
        let n = rock_at(&water, x, z, 1.5, 0.75, [0.0, 3.5], 9);
        let c = foam_collar_instance(&water, &basis, &n, &rock).unwrap();
        let downstream = c.transform().rotation * Vec3::Z;
        assert!(downstream.abs_diff_eq(Vec3::NEG_Z, 1e-3), "{downstream}");
        // A rock just under the water (its top 0.3 m down) gets a collar with no ring hole; one
        // a metre under gets none; a dry rock (no drawn water) gets none.
        let under = Boulder {
            level_m: 3.3,
            ..rock_at(&water, x, z, 1.5, 1.0, [4.0, 0.0], 9)
        };
        assert!(under.foam_site().is_some());
        let rock = boulder_instance(&water, &basis, &under).unwrap();
        assert_eq!(
            foam_collar_instance(&water, &basis, &under, &rock)
                .unwrap()
                .wet(),
            0.0
        );
        let deep = Boulder {
            level_m: 4.0,
            ..under
        };
        assert!(deep.foam_site().is_none());
        assert!(foam_collar_instance(&water, &basis, &deep, &rock).is_none());
        let dry = plane_grid(0.0, Vec::new());
        let rock = boulder_instance(&dry, &basis, &b).unwrap();
        assert!(foam_collar_instance(&dry, &basis, &b, &rock).is_none());
        // No flow: turned by its seed instead.
        let still = rock_at(&water, x, z, 1.5, 0.75, [0.0, 0.0], 9);
        let rock = boulder_instance(&water, &basis, &still).unwrap();
        assert_eq!(
            foam_collar_instance(&water, &basis, &still, &rock)
                .unwrap()
                .yaw,
            boulder_yaw(9)
        );
    }

    /// The flow is carried into region axes through the frames: at the centre east is +x and
    /// north −z; 140 km east at 34° N, a due-north flow turns off the region's −z by the
    /// meridians' convergence, `(d / R) · tan(latitude)` (about 0.85°), which a sign flip misses.
    #[test]
    fn collar_flow_follows_the_region_axes() {
        let grid = plane_grid(3.0, Vec::new());
        let frame = grid.spec.frame();
        let basis = frame.basis();
        let at = |x: f64, z: f64| frame.local_to_dir(x, z).0;
        let f = flow_local(&basis, at(0.0, 0.0), [2.0, 0.0]).unwrap();
        assert!(f.abs_diff_eq(Vec2::X, 1e-6), "{f}");
        let f = flow_local(&basis, at(0.0, 0.0), [0.0, 2.0]).unwrap();
        assert!(f.abs_diff_eq(Vec2::NEG_Y, 1e-6), "{f}");
        let f = flow_local(&basis, at(140_000.0, 0.0), [0.0, 2.0]).unwrap();
        let turn = f.x.atan2(-f.y).to_degrees();
        let lat = 34.2f64.to_radians();
        let want = (140_000.0 / 6_371_000.0 * lat.tan()).to_degrees() as f32;
        assert!(
            (turn.abs() - want).abs() < 0.1,
            "turned {turn}°, want about {want}°"
        );
        assert!(flow_local(&basis, at(0.0, 0.0), [0.0, 0.0]).is_none());
    }

    /// Fix round 1: a clear ring round a rock through the surface, a short tail downstream, and
    /// nothing upstream past the pile nor abreast past the ring. Mirrored in `scatter.wesl`.
    #[test]
    fn collar_shape_rings_the_rock_and_trails_downstream() {
        let wake = FOAM_COLLAR_WAKE_RADII;
        let w = 0.8;
        // The ring, all round, just outside the waterline.
        for k in 0..16 {
            let a = std::f32::consts::TAU * k as f32 / 16.0;
            let p = Vec2::new(a.cos(), a.sin()) * (w + 0.2);
            assert!(collar_shape(p, w, wake) > 0.9, "the ring at {p}");
        }
        // Thicker upstream (-y): the pile.
        assert!(collar_shape(Vec2::new(0.0, -(w + 0.65)), w, wake) > 0.5);
        assert_eq!(collar_shape(Vec2::new(w + 0.65, 0.0), w, wake), 0.0);
        // The tail downstream, then nothing.
        assert!(
            collar_shape(Vec2::new(0.0, w + 1.0), w, wake) > 0.8,
            "the tail"
        );
        assert!(
            collar_shape(Vec2::new(0.0, 0.6 * wake), w, wake) > 0.3,
            "the tail's end"
        );
        assert_eq!(collar_shape(Vec2::new(0.0, wake + 0.5), w, wake), 0.0);
        assert_eq!(collar_shape(Vec2::new(0.0, -2.0), w, wake), 0.0, "upstream");
        assert_eq!(
            collar_shape(Vec2::new(1.9, 1.0), w, wake),
            0.0,
            "abreast of the tail"
        );
        // A rock just under (w = 0): a smaller boil, no tail beyond it.
        assert!(collar_shape(Vec2::new(0.0, 0.3), 0.0, wake) > 0.5);
        assert_eq!(collar_shape(Vec2::new(0.0, 2.5), 0.0, wake), 0.0);
        // The shader's copy, line for line.
        let src = include_str!("../assets/shaders/scatter.wesl");
        for line in COLLAR_SHAPE_WESL.lines() {
            assert!(src.contains(line), "scatter.wesl lacks: {line}");
        }
        const { assert!(FOAM_COLLAR_WAKE_RADII <= 3.0, "a short tail") };
    }

    /// `collar_shape` as `scatter.wesl` writes it.
    const COLLAR_SHAPE_WESL: &str = "fn collar_shape(p: vec2<f32>, w: f32, wake: f32) -> f32 {
    let rho = length(p);
    let up = clamp(-p.y / max(rho, 1e-3), 0.0, 1.0);
    let breaks = w > 0.0;
    let inner = 0.8 * w;
    let edge = select(0.7, w + 0.45, breaks) + 0.35 * up;
    let ring = (1.0 - smoothstep(edge - 0.15, edge, rho)) * smoothstep(inner - 0.1, inner, rho);
    let half = select(0.45, 0.8 * w + 0.15, breaks) * (1.0 + 0.1 * max(p.y, 0.0));
    let tail = select(0.6, wake, breaks);
    let trail = (1.0 - smoothstep(0.6 * half, half, abs(p.x)))
        * smoothstep(-0.2, 0.4, p.y)
        * (1.0 - smoothstep(0.5 * tail, tail, p.y));
    return max(ring, trail);";

    #[test]
    fn the_foam_collar_mesh_is_one_upward_quad() {
        let mesh = foam_collar_mesh();
        assert_eq!(mesh.primitive_topology(), PrimitiveTopology::TriangleList);
        let ids: Vec<_> = mesh.attributes().map(|(a, _)| a.id).collect();
        let rock = boulder_meshes();
        let rock_ids: Vec<_> = rock[0].attributes().map(|(a, _)| a.id).collect();
        assert_eq!(ids, rock_ids, "the boulders' vertex layout");
        let p = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| a.as_float3())
            .unwrap();
        let n = mesh
            .attribute(Mesh::ATTRIBUTE_NORMAL)
            .and_then(|a| a.as_float3())
            .unwrap();
        assert_eq!(p.len(), 6, "two triangles");
        for t in p.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(Vec3::from_array);
            assert!((b - a).cross(c - a).y > 0.0, "counter-clockwise from above");
        }
        assert!(n.iter().all(|&n| n == [0.0, 1.0, 0.0]));
        let (lo, hi) = p.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), &q| {
            (lo.min(Vec3::from(q)), hi.max(Vec3::from(q)))
        });
        let [w, up, down] = FOAM_COLLAR_EXTENT;
        assert_eq!((lo, hi), (Vec3::new(-w, 0.0, -up), Vec3::new(w, 0.0, down)));
        assert!(down > FOAM_COLLAR_WAKE_RADII + 1.0, "room for the wake");
    }

    #[test]
    fn boulders_rest_on_the_drawn_surface() {
        // The drawn ground: the finest triangles over the bilinear heights, exact at their
        // corners. A checkerboard of ±1,000 m texels twists the bilinear surface, so the
        // triangles differ from it inside a quad.
        let mut checker = plane_grid(0.0, Vec::new());
        for f in [&mut checker.inner, &mut checker.apron] {
            let res = f.res as usize;
            for (k, h) in f.elevation_m.iter_mut().enumerate() {
                *h += if (k / res + k % res).is_multiple_of(2) {
                    1_000.0
                } else {
                    -1_000.0
                };
            }
        }
        let settings = LodSettings::region();
        let quad = settings.finest_quad_m();
        let root = -settings.extent_m() / 2.0;
        let mut twisted = 0;
        for (i, j) in [(8_190.0, 8_190.0), (8_000.0, 8_300.0), (8_211.0, 8_170.0)] {
            let (x, z) = (root + i * quad, root + j * quad);
            let h = |dx: f64, dz: f64| blended_height_m(&checker, x + dx * quad, z + dz * quad);
            let drawn = |dx: f64, dz: f64| drawn_ground_m(&checker, x + dx * quad, z + dz * quad);
            assert_eq!(drawn(0.0, 0.0), h(0.0, 0.0));
            let (a, b, c, d) = (h(0.0, 0.0), h(1.0, 0.0), h(0.0, 1.0), h(1.0, 1.0));
            let mid = drawn(0.5, 0.5);
            assert!(
                (mid - (a + d) / 2.0).abs() < 1e-3,
                "{mid} vs the diagonal's {}",
                (a + d) / 2.0
            );
            // Below the diagonal the triangle (a, b, d); above it (a, c, d).
            let lower = drawn(0.75, 0.25);
            assert!((lower - (a + 0.75 * (b - a) + 0.25 * (d - b))).abs() < 1e-3);
            let upper = drawn(0.25, 0.75);
            assert!((upper - (a + 0.75 * (c - a) + 0.25 * (d - c))).abs() < 1e-3);
            if (mid - h(0.5, 0.5)).abs() > 0.01 {
                twisted += 1;
            }
        }
        assert!(
            twisted >= 2,
            "the checkerboard must tell triangles from bilinear"
        );
        let dry = plane_grid(0.0, Vec::new());
        let basis = dry.spec.frame().basis();
        let (x, z) = (1_234.0, -2_345.0);
        let ground = drawn_ground_m(&dry, x, z);
        // Dry ground: set BOULDER_EMBED of its height in, below the lowest ground under it;
        // dry, whatever its own wetness said (no water is drawn there).
        for wet in [0.0, 0.6] {
            let b = boulder_at(&dry, x, z, 2.0, wet, 77);
            let i = boulder_instance(&dry, &basis, &b).unwrap();
            let bottom = i.translation.y - i.scale;
            assert_eq!(i.scale, 2.0);
            assert!(
                bottom < ground && bottom > ground - 2.0,
                "{bottom} on {ground}"
            );
            let lowest = [(2.0, 0.0), (-2.0, 0.0), (0.0, 2.0), (0.0, -2.0)]
                .iter()
                .map(|&(dx, dz)| drawn_ground_m(&dry, x + dx, z + dz))
                .fold(ground, f32::min);
            assert!((bottom - (lowest - BOULDER_EMBED * 4.0)).abs() < 1e-3);
            assert_eq!(i.wet(), 0.0);
            assert_eq!(i.seed(), 77);
            assert!(
                (i.translation.x - x as f32).abs() < 0.01
                    && (i.translation.z - z as f32).abs() < 0.01
            );
        }
        // In drawn water 5 m deep: as deep under the drawn level as under its own, so the
        // waterline on it is its wetness.
        let wet = plane_grid(5.0, Vec::new());
        let level = blended_surface_m(&wet, x, z);
        assert!(level > blended_height_m(&wet, x, z) + 4.0);
        let b = boulder_at(&wet, x, z, 1.5, 0.4, 5);
        let i = boulder_instance(&wet, &basis, &b).unwrap();
        assert!((i.translation.y - 1.5 - (level - 0.4 * 3.0)).abs() < 1e-3);
        assert!((i.wet() - 0.4).abs() < 0.5 / 255.0 + 1e-6);
        // ...but never more than half under the drawn ground: in water 1 m deep a boulder 90 %
        // under rises to half buried, and its waterline with it.
        let shallow = plane_grid(1.0, Vec::new());
        let ground = drawn_ground_m(&shallow, x, z);
        let level = blended_surface_m(&shallow, x, z);
        let b = boulder_at(&shallow, x, z, 1.5, 0.9, 5);
        let i = boulder_instance(&shallow, &basis, &b).unwrap();
        let bottom = i.translation.y - 1.5;
        assert!(
            (bottom - (ground - 1.5)).abs() < 1e-3,
            "{bottom} vs {ground}"
        );
        assert!((i.wet() - (level - bottom) / 3.0).abs() < 0.5 / 255.0 + 1e-6);
        // Its yaw comes from its seed: the same boulder turns the same way in any region.
        let again = boulder_instance(&shallow, &basis, &b).unwrap();
        assert_eq!(again.yaw, i.yaw);
        let other = boulder_instance(&shallow, &basis, &Boulder { seed: 6, ..b }).unwrap();
        assert_ne!(other.yaw, i.yaw);
        assert!((0.0..std::f32::consts::TAU).contains(&i.yaw));
    }

    /// `drawn_ground_m` is the finest grid, unmorphed: true wherever a boulder can be drawn.
    /// The CDLOD's selection and morph both measure `D = √(h² + camera_y²)` on the sea-level
    /// plane. A boulder at elevation `e` is drawn within BOULDER_VISIBLE_M of the camera, so
    /// the corners of its quad lie within `D ≤ BOULDER_VISIBLE_M + |e| + r + quad·√2`, which
    /// must stay under LOD 0's morph start; boulders above BOULDER_MAX_ELEVATION_M aren't drawn.
    #[test]
    fn boulders_draw_inside_the_unmorphed_finest_lod() {
        use timefly_core::terrain::{morph_factor, select_patches};
        let settings = LodSettings::region();
        let quad = settings.finest_quad_m();
        let morph = settings.morph_range_m(0);
        let reach = f64::from(BOULDER_VISIBLE_M)
            + f64::from(BOULDER_MAX_ELEVATION_M)
            + timefly_core::hydrology::params::BOULDER_MAX_RADIUS_M
            + quad * std::f64::consts::SQRT_2;
        assert!(
            reach < f64::from(morph[0]),
            "boulders reach D = {reach} m; LOD 0 morphs from {} m",
            morph[0]
        );
        // The real selection and morph at the extreme cameras: on the visibility sphere around
        // a boulder at the lowest and highest drawn elevations.
        let (bx, bz) = (123.4, -56.7);
        let root = -settings.extent_m() / 2.0;
        let (i, j) = (((bx - root) / quad).floor(), ((bz - root) / quad).floor());
        for e in [
            -f64::from(BOULDER_MAX_ELEVATION_M),
            0.0,
            f64::from(BOULDER_MAX_ELEVATION_M),
        ] {
            for k in 0..=12 {
                let a = (k as f64 / 12.0 - 0.5) * std::f64::consts::PI;
                let r = f64::from(BOULDER_VISIBLE_M);
                let camera = bevy::math::DVec3::new(bx + r * a.cos(), e + r * a.sin(), bz);
                let patches = select_patches(camera, &settings);
                let covering = patches
                    .iter()
                    .find(|p| {
                        (p.origin_m[0]..p.origin_m[0] + p.size_m).contains(&bx)
                            && (p.origin_m[1]..p.origin_m[1] + p.size_m).contains(&bz)
                    })
                    .expect("a patch over the boulder");
                assert_eq!(covering.lod, 0, "camera {camera}");
                for (di, dj) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
                    let (x, z) = (root + (i + di) * quad, root + (j + dj) * quad);
                    let d =
                        ((x - camera.x).powi(2) + camera.y.powi(2) + (z - camera.z).powi(2)).sqrt();
                    assert_eq!(morph_factor(d as f32, morph), 0.0, "camera {camera}, d {d}");
                }
            }
        }
        // A boulder above the highest drawn elevation isn't drawn; one just below it is.
        let lifted = |lift: f32| {
            let mut g = plane_grid(0.0, Vec::new());
            for f in [&mut g.inner, &mut g.apron] {
                f.elevation_m.iter_mut().for_each(|h| *h += lift);
                f.water_level_m.iter_mut().for_each(|h| *h += lift);
            }
            g
        };
        let high = lifted(BOULDER_MAX_ELEVATION_M - 1_000.0 + 10.0);
        let low = lifted(BOULDER_MAX_ELEVATION_M - 1_000.0 - 10.0);
        let basis = high.spec.frame().basis();
        let b = boulder_at(&high, 0.0, 0.0, 1.0, 0.0, 3);
        // The plane is 1,000 m at the centre.
        assert!(boulder_instance(&high, &basis, &b).is_none());
        assert!(boulder_instance(&low, &basis, &b).is_some());
    }

    #[test]
    fn the_boulder_look_is_the_regions_rock() {
        let grid = plane_grid(0.0, Vec::new());
        let look = boulder_look(&grid);
        let (era, biome) = crate::sky::region_look(&grid);
        let index = look.texel.y as usize * crate::terrain::material::PALETTE_SIZE.x as usize
            + look.texel.x as usize;
        assert_eq!(index, palette_texel_index(era, biome, PaletteKind::Rock));
        assert_eq!(
            look.colour,
            Vec4::new(
                BOULDER_HUE_JITTER_RAD,
                BOULDER_VALUE_JITTER,
                BOULDER_WET_DARKEN,
                BOULDER_WET_BAND
            )
        );
        assert_eq!(look.pull.x, crate::water::WATER_PULL_M);
        assert_eq!(look.pull.y, crate::water::WATER_DEPTH_PULL);
        assert_eq!(look.pull.z, 6_371_000.0);
        assert_eq!(look.light.x, BOULDER_SHADE_FLOOR * full_sun_illumination());
    }

    fn boulder_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<Mesh>()
            .init_asset::<ShaderBuffer>()
            .init_asset::<TerrainMaterial>()
            .init_asset::<ScatterMaterial>();
        add_terrain_systems(&mut app);
        add_scatter_systems(&mut app);
        add_boulder_systems(&mut app);
        app.world_mut()
            .spawn((GlobeCamera, Camera::default(), IsDefaultUiCamera));
        app
    }

    fn enter(app: &mut App, grid: &Arc<RegionGrid>) {
        app.insert_resource(ActiveRegion {
            grid: grid.clone(),
            frame: RegionFrame {
                center: grid.spec.center,
            },
        });
    }

    fn drawn_collars(app: &App) -> Arc<[ScatterInstance]> {
        let set = app.world().resource::<RegionBoulders>().collars;
        app.world()
            .get::<ScatterSet>(set)
            .unwrap()
            .instances
            .clone()
    }

    /// The slot entities of the collars' pool.
    fn collar_slots(app: &App) -> Vec<Entity> {
        let set = app.world().resource::<RegionBoulders>().collars;
        app.world()
            .get::<ScatterPool>(set)
            .unwrap()
            .slots()
            .to_vec()
    }

    /// A grid under 3 m of water holding `n` rocks at the surface (and as many deep ones,
    /// which get no collar), flowing east.
    fn rapids_grid(n: u32, east_m: f64) -> Arc<RegionGrid> {
        let mut g = plane_grid(3.0, Vec::new());
        g.spec.center = g.spec.frame().local_to_dir(east_m, 0.0);
        let boulders = (0..n)
            .flat_map(|k| {
                let x = 7.0 * f64::from(k % 400) - 1_400.0;
                let z = 9.0 * f64::from(k / 400) - 300.0;
                let surface = rock_at(&g, x, z, 1.0, 0.8, [4.0, 0.5], k);
                let deep = Boulder {
                    level_m: 5.0,
                    ..rock_at(&g, x + 3.0, z, 1.0, 1.0, [4.0, 0.5], 50_000 + k)
                };
                [surface, deep]
            })
            .collect();
        Arc::new(RegionGrid { boulders, ..g })
    }

    #[test]
    fn collars_follow_the_active_region() {
        let mut app = boulder_app();
        let (a, b) = (rapids_grid(5, 0.0), rapids_grid(3, 40_000.0));
        app.update();
        assert!(drawn_collars(&app).is_empty(), "no region, no collars");
        enter(&mut app, &a);
        app.update();
        app.update();
        assert_eq!(&drawn_collars(&app)[..], &foam_collar_instances(&a)[..]);
        assert_eq!(drawn_collars(&app).len(), 5, "one per rock at the surface");
        // A held swap keeps the collars the terrain draws.
        app.world_mut().resource_mut::<TerrainSettings>().hold_swap = true;
        enter(&mut app, &b);
        app.update();
        assert_eq!(&drawn_collars(&app)[..], &foam_collar_instances(&a)[..]);
        // Released: the new region's, in its own frame.
        app.world_mut().resource_mut::<TerrainSettings>().hold_swap = false;
        app.update();
        app.update();
        assert_eq!(&drawn_collars(&app)[..], &foam_collar_instances(&b)[..]);
        assert_eq!(drawn_collars(&app).len(), 3);
        // Torn down: empty, and the slots gone.
        app.world_mut().remove_resource::<ActiveRegion>();
        app.update();
        app.update();
        assert!(drawn_collars(&app).is_empty());
        assert!(collar_slots(&app).is_empty());
    }

    #[test]
    fn collars_share_one_mesh_and_material() {
        let mut app = boulder_app();
        enter(&mut app, &rapids_grid(1_500, 0.0));
        for _ in 0..4 {
            app.update();
        }
        let set = app.world().resource::<RegionBoulders>().collars;
        let pool = app.world().get::<ScatterPool>(set).unwrap();
        assert_eq!(pool.meshes().len(), 1, "one collar mesh");
        let (mesh, material) = (pool.meshes()[0].clone(), pool.material().clone());
        let slots = collar_slots(&app);
        assert_eq!(slots.len(), 2_048);
        for e in &slots {
            let entity = app.world().entity(*e);
            assert_eq!(entity.get::<Mesh3d>().unwrap().0, mesh);
            assert_eq!(
                entity.get::<MeshMaterial3d<ScatterMaterial>>().unwrap().0,
                material
            );
            let range = entity.get::<VisibilityRange>().unwrap();
            assert!(range.is_abrupt());
            assert!(range.is_visible_at_all(FOAM_COLLAR_VISIBLE_M - 1.0));
            assert!(range.is_culled(FOAM_COLLAR_VISIBLE_M + 1.0));
        }
        // The collars' material: alpha-masked foam over the shared palette, apart from the
        // boulders' (whose look changes per region).
        let boulders = app.world().resource::<RegionBoulders>().material.clone();
        assert_ne!(material, boulders);
        let materials = app.world().resource::<Assets<ScatterMaterial>>();
        assert_eq!(materials.len(), 2, "the boulders' and the collars'");
        let m = materials.get(&material).unwrap();
        assert_eq!(m.base.alpha_mode, AlphaMode::Mask(0.5));
        assert_eq!(m.extension.look, foam_collar_look());
        assert_eq!(m.extension.look.texel.z, crate::scatter::SCATTER_MODE_FOAM);
        assert_eq!(
            m.extension.palette,
            materials.get(&boulders).unwrap().extension.palette
        );
        let mesh = app.world().resource::<Assets<Mesh>>().get(&mesh).unwrap();
        assert_eq!(
            mesh.attribute(Mesh::ATTRIBUTE_POSITION).unwrap().len(),
            foam_collar_mesh()
                .attribute(Mesh::ATTRIBUTE_POSITION)
                .unwrap()
                .len()
        );
    }

    #[test]
    fn collar_sets_reuse_their_pool() {
        let mut app = boulder_app();
        let grids = [
            rapids_grid(1_500, 0.0),
            rapids_grid(600, 40_000.0),
            rapids_grid(1_900, 80_000.0),
        ];
        enter(&mut app, &grids[0]);
        for _ in 0..4 {
            app.update();
        }
        let pool = collar_slots(&app);
        assert_eq!(pool.len(), 2_048, "1,500 collars in whole chunks");
        for g in &grids[1..] {
            enter(&mut app, g);
            for _ in 0..4 {
                app.update();
            }
            assert_eq!(&drawn_collars(&app)[..], &foam_collar_instances(g)[..]);
            assert_eq!(collar_slots(&app), pool, "the same slot entities");
        }
        let world = app.world_mut();
        let collar_set = world.resource::<RegionBoulders>().collars;
        let mut q = world.query::<(&ScatterSlot, &ChildOf, &Visibility)>();
        let shown = q
            .iter(world)
            .filter(|(_, c, v)| c.parent() == collar_set && **v != Visibility::Hidden)
            .count();
        assert_eq!(shown, 1_900);
    }

    fn drawn_set(app: &App) -> Arc<[ScatterInstance]> {
        let set = app.world().resource::<RegionBoulders>().set;
        app.world()
            .get::<ScatterSet>(set)
            .unwrap()
            .instances
            .clone()
    }

    fn slots(app: &mut App) -> usize {
        let world = app.world_mut();
        world.query::<&ScatterSlot>().iter(world).count()
    }

    #[test]
    fn boulders_follow_the_active_region() {
        let mut app = boulder_app();
        let a = Arc::new({
            let g = plane_grid(3.0, Vec::new());
            let boulders = (0..5)
                .map(|k| boulder_at(&g, 100.0 * f64::from(k), -50.0, 1.0, 0.5, k))
                .collect();
            RegionGrid { boulders, ..g }
        });
        let b = Arc::new({
            let mut g = plane_grid(0.0, Vec::new());
            g.spec.center = g.spec.frame().local_to_dir(40_000.0, 0.0);
            let boulders = (0..3)
                .map(|k| boulder_at(&g, -30.0 * f64::from(k), 70.0, 2.0, 0.0, 100 + k))
                .collect();
            RegionGrid { boulders, ..g }
        });
        app.update();
        assert!(drawn_set(&app).is_empty(), "no region, no boulders");
        enter(&mut app, &a);
        app.update();
        app.update();
        assert_eq!(&drawn_set(&app)[..], &boulder_instances(&a)[..]);
        assert_eq!(drawn_set(&app).len(), 5);
        assert_eq!(slots(&mut app), 1_024);
        // The look is the region's.
        let material = app.world().resource::<RegionBoulders>().material.clone();
        let look = |app: &App| {
            app.world()
                .resource::<Assets<ScatterMaterial>>()
                .get(&material)
                .unwrap()
                .extension
                .look
        };
        assert_eq!(look(&app), boulder_look(&a));
        // A held swap keeps the boulders the terrain draws.
        app.world_mut().resource_mut::<TerrainSettings>().hold_swap = true;
        enter(&mut app, &b);
        app.update();
        assert_eq!(&drawn_set(&app)[..], &boulder_instances(&a)[..]);
        // Released: the new region's, in its own frame, through the same pool.
        app.world_mut().resource_mut::<TerrainSettings>().hold_swap = false;
        app.update();
        app.update();
        assert_eq!(&drawn_set(&app)[..], &boulder_instances(&b)[..]);
        assert_eq!(drawn_set(&app).len(), 3);
        assert_eq!(slots(&mut app), 1_024);
        assert_eq!(look(&app), boulder_look(&b));
        // Torn down: empty, and the slots gone.
        app.world_mut().remove_resource::<ActiveRegion>();
        app.update();
        app.update();
        assert!(drawn_set(&app).is_empty());
        assert_eq!(slots(&mut app), 0);
        // The pool's meshes are the four shapes, its material the palette's rock.
        let set = app.world().resource::<RegionBoulders>().set;
        let pool = app.world().get::<ScatterPool>(set).unwrap();
        assert_eq!(pool.meshes().len(), BOULDER_SHAPES);
        let palette = &app
            .world()
            .resource::<Assets<ScatterMaterial>>()
            .get(&material)
            .unwrap()
            .extension
            .palette;
        let image = app
            .world()
            .resource::<Assets<Image>>()
            .get(palette)
            .unwrap();
        assert_eq!(image.data, palette_image().data);
    }
}
