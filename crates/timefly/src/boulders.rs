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
//!   in the water, its top less than half a metre under it or above it) gets a collar, the
//!   second scatter kind ([`ScatterKind::FoamCollar`]): one flat quad ([`foam_collar_mesh`]) at
//!   the drawn water level, scaled by the boulder's radius and turned so its `+z` runs
//!   downstream along the boulder's flow ([`foam_collar_instance`]). Its tag carries the drawn
//!   rock's radius at the waterline (as a share of its radius), its V wake's length
//!   (`Boulder::wake_radii`, which grows with the flow's speed), the flow's speed and the seed
//!   ([`collar_tag_seed`]). `scatter.wesl` shapes it by [`collar_shape`]: round a rock through
//!   the surface, foam piled on its upstream face (the pillow), a V of two arms trailing
//!   downstream and churn behind it; over a rock just under the surface, a dimmer boil just
//!   downstream of it. Fix round 5 ("softer, whiter wakes", animated): the shapes are feathered
//!   and stay put but pulse ([`collar_pulses`]), while the foam inside them ([`collar_foam`],
//!   streaks along the flow) is carried downstream at the water's speed; the coverage
//!   ([`collar_coverage`]) is written as a dithered sample mask (the material stays
//!   alpha-masked, drawn after the opaque pass), and the foam is clipped where the region draws
//!   no water (the material binds the region's water, [`ScatterWater`]). It takes the water's
//!   own depth pull plus [`FOAM_COLLAR_PULL_M`], so the water at its level never hides it.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::mesh::PrimitiveTopology;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use timefly_core::geo::RenderDir;
use timefly_core::terrain::{LodSettings, blended_height_m, blended_surface_m};
use timefly_core::worldgen::frame::FrameBasis;
use timefly_core::worldgen::generate::RegionGrid;
use timefly_core::worldgen::generate::water::Boulder;

use crate::scatter::{
    ScatterInstance, ScatterKind, ScatterLook, ScatterMaterial, ScatterPool, ScatterSet,
    ScatterWater, scatter_material,
};
use crate::terrain::material::{PaletteKind, palette_image};
use crate::terrain::view::full_sun_illumination;
use crate::terrain::{TerrainScene, TerrainSceneSystems};
use crate::water::{WATER_DEPTH_PULL, WATER_F0, WATER_PULL_M, WHITE_WATER, scene_water};
use bevy::render::storage::ShaderBuffer;

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
/// reaches upstream and how far downstream of the boulder's centre. It holds [`collar_shape`]
/// with the longest wake (`COLLAR_WAKE_MAX_RADII`): the V's arms reach `±(1.7 + 0.6 × 16)`.
pub const FOAM_COLLAR_EXTENT: [f32; 3] = [11.4, 3.6, 16.2];
/// A boil (the collar of a rock just under the surface) is this much dimmer than the foam
/// round a rock through it.
pub const FOAM_BOIL_SHADE: f32 = 0.8;
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
    // Counter-clockwise seen from above; the normal straight up, exactly (normalising the
    // triangles' cross product can leave it an ulp short).
    flat_mesh(&corners, &[[0, 2, 1], [0, 3, 2]])
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0f32, 1.0, 0.0]; 6])
}

/// `smoothstep(e0, e1, x)` as WGSL computes it.
fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The still pulses: `collar_shape` at these draws its full extent.
pub const COLLAR_STILL: Vec3 = Vec3::ONE;

/// How white a foam collar is (0 to 1, soft-edged) at `p` (radii; `x` across, `y` downstream)
/// round a rock whose waterline is `w` of its radius (0 for a rock just under), its V wake
/// `wake` radii long, pulsed by `pulses` = (surge, breathe, bloom) ([`collar_pulses`], each at
/// most 1, so the shape never leaves its still extent, [`COLLAR_STILL`]). `scatter.wesl`'s
/// `collar_shape`. Fix round 5 ("softer, whiter wakes"): every part ramps over most of its
/// width.
/// - A rock through the surface (`w > 0` and a wake): the pillow, piled on its upstream face
///   from just inside the waterline to 0.9 radii out abreast and 2.5 radii out upstream (times
///   the surge), feathered over 0.6 radii; the V, two arms from its flanks spreading downstream
///   at about 19° (`|x| = 0.8 w + 0.3 + 0.35 y`, 1.2 radii wide and widening, feathered over
///   all but their core), fading out over the last half of the (breathing) wake; and churn
///   behind it, widening, for most of the wake.
/// - A rock just under: a boil 1.5 radii round, 1 radius downstream of it, and a short trail,
///   both blooming with `bloom`.
pub fn collar_shape(p: Vec2, w: f32, wake: f32, pulses: Vec3) -> f32 {
    let step = smooth;
    let rho = p.length();
    let up = (-p.y / rho.max(1e-3)).clamp(0.0, 1.0);
    let breaks = w > 0.0 && wake > 0.0;
    let inner = 0.8 * w;
    let edge = (w + 0.9 + 1.6 * up) * pulses.x;
    let pillow = (1.0 - step(edge - 0.6, edge, rho)) * step(inner - 0.15, inner, rho);
    let along = p.y.max(0.0);
    let arm = 0.8 * w + 0.3 + 0.35 * along;
    let arm_half = 0.6 + 0.25 * along;
    let reach = (wake * pulses.y).max(1e-3);
    let fade = step(-0.3, 0.5, p.y) * (1.0 - step(0.5 * reach, reach, p.y));
    let arms = (1.0 - step(0.1 * arm_half, arm_half, (p.x.abs() - arm).abs())) * fade;
    let core = w + 0.8 + 0.2 * along;
    let churn = (1.0 - step(0.2 * core, core, p.x.abs()))
        * step(-0.3, 0.4, p.y)
        * (1.0 - step(0.4 * reach, 0.95 * reach, p.y));
    let boil = (1.0 - step(0.4, 1.5, (p - Vec2::new(0.0, 1.0)).length())) * pulses.z;
    let trail = (1.0 - step(0.1, 0.6, p.x.abs()))
        * step(0.5, 1.2, p.y)
        * (1.0 - step(1.4, 2.9, p.y))
        * pulses.z;
    if breaks {
        pillow.max(arms.max(churn))
    } else {
        boil.max(trail)
    }
}

/// The periods (s) of [`collar_pulses`]' sines: the surge's two, then the breath's two. Each
/// divides Bevy's hour-long time wrap ([`crate::art::TIME_WRAP_S`]), so nothing jumps there.
/// `scatter.wesl` literals.
pub const COLLAR_PULSE_PERIODS_S: [f32; 4] = [1.2, 3.0, 1.8, 2.4];
/// A boil's bloom period (s), picked by bits 2–3 of its seed; each divides the time wrap.
/// `scatter.wesl` literals.
pub const BOIL_PERIODS_S: [f32; 4] = [2.0, 2.4, 3.0, 3.6];
/// The periods (s) of [`collar_foam`]'s fine and coarse flow-map layers (each divides the time
/// wrap). `scatter.wesl` literals.
pub const COLLAR_FOAM_PERIODS_S: [f32; 2] = [1.0, 1.6];

/// A collar's pulses at time `t` (s) for its seed (low byte): (surge, breathe, bloom). The
/// pillow surges between 0.85 and 1 and the wake breathes between 0.8 and 1, each two sines of
/// [`COLLAR_PULSE_PERIODS_S`]; a boil blooms between 0.35 and 1 over one of
/// [`BOIL_PERIODS_S`]. `scatter.wesl`'s `collar_pulses`.
pub fn collar_pulses(t: f32, seed: u32) -> Vec3 {
    let tau = std::f32::consts::TAU;
    let [q1, q2, b1, b2] = COLLAR_PULSE_PERIODS_S;
    let phase = (seed & 255) as f32 / 256.0;
    let q = 0.6 * (tau * (t / q1 + phase)).sin() + 0.4 * (tau * (t / q2 + 2.3 * phase)).sin();
    let b =
        0.55 * (tau * (t / b1 + 1.7 * phase)).sin() + 0.45 * (tau * (t / b2 + 0.4 * phase)).sin();
    let period = BOIL_PERIODS_S[((seed >> 2) & 3) as usize];
    let bloom = 0.35 + 0.65 * (0.5 + 0.5 * (tau * (t / period + phase)).sin());
    Vec3::new(0.925 + 0.075 * q, 0.9 + 0.1 * b, bloom)
}

/// `scatter.wesl`'s `foam_noise`: gradient noise at `q` (cells), its third axis `z`.
fn foam_noise(q: Vec2, z: f32, seed: u32) -> f32 {
    let c = q.floor();
    let zc = z.floor();
    timefly_core::worldgen::noise::gradient_noise3(
        [c.x as i32, c.y as i32, zc as i32],
        [q.x - c.x, q.y - c.y, z - zc],
        seed,
    )
}

/// One flow-map layer of [`collar_foam`]: noise at `m` (m; `x` across, `y` downstream)
/// stretched `scale` (m across, m along), scrolled downstream at `speed` (m/s) over `period`
/// (s) and restarted, two phases half a period apart cross-faded (each weight 0 at its own
/// restart). `scatter.wesl`'s `foam_layer`.
pub fn foam_layer(m: Vec2, speed: f32, t: f32, period: f32, scale: Vec2, seed: u32) -> f32 {
    let a = (t / period).fract();
    let b = (a + 0.5).fract();
    let wa = 1.0 - (2.0 * a - 1.0).abs();
    let wb = 1.0 - (2.0 * b - 1.0).abs();
    let na = foam_noise((m - Vec2::new(0.0, speed * period * a)) / scale, 0.0, seed);
    let nb = foam_noise((m - Vec2::new(0.0, speed * period * b)) / scale, 7.5, seed);
    (wa * na + wb * nb) / (wa * wa + wb * wb).sqrt()
}

/// A collar's foam texture at `m` (m from the rock; `x` across, `y` downstream), carried
/// downstream with the water at `speed` (m/s): fine streaks 4:1 along the flow at the water's
/// speed and coarse clumps at three quarters of it, each its own flow-map layer and each
/// collar's own (offset by its seed). `scatter.wesl`'s `collar_foam`.
pub fn collar_foam(m: Vec2, speed: f32, t: f32, seed: u32) -> f32 {
    let q = m + Vec2::new(seed as f32 * 3.7, seed as f32 * 1.9);
    let [fine_s, coarse_s] = COLLAR_FOAM_PERIODS_S;
    let fine = foam_layer(q, speed, t, fine_s, Vec2::new(0.35, 1.4), 211);
    let coarse = foam_layer(q, 0.75 * speed, t, coarse_s, Vec2::new(1.2, 4.8), 223);
    0.65 * fine + 0.5 * coarse
}

/// The share of a pixel a collar covers (0 to 1) where its shape is `shape` and its foam
/// `foam`: dark water shows between the streaks, most in the fringe, so an edge feathers out
/// over the shape's own ramps. `scatter.wesl`'s `collar_coverage`, written as a dithered
/// sample mask.
pub fn collar_coverage(shape: f32, foam: f32) -> f32 {
    ((shape - 0.12) / 0.6 * (0.85 + 1.0 * foam)).clamp(0.0, 1.0)
}

/// A collar's tag holds its flow's speed in steps of this (m/s), up to 255 of them. A
/// `scatter.wesl` constant.
pub const COLLAR_SPEED_STEP: f32 = 0.05;
/// A collar's coverage is dithered over this many samples (the region camera's MSAA). A
/// `scatter.wesl` constant.
pub const COLLAR_SAMPLES: u32 = 4;

/// A collar instance's seed field (24 bits): its V wake's length in the top byte (a share of
/// `COLLAR_WAKE_MAX_RADII`, rounded to 1/255), its flow's speed in [`COLLAR_SPEED_STEP`]s in
/// the next, and the boulder's seed's low byte.
pub fn collar_tag_seed(wake_radii: f32, speed_mps: f32, seed: u32) -> u32 {
    let max = timefly_core::terrain::drawn::COLLAR_WAKE_MAX_RADII as f32;
    let share = if wake_radii.is_nan() {
        0.0
    } else {
        (wake_radii / max).clamp(0.0, 1.0)
    };
    let speed = if speed_mps.is_nan() {
        0.0
    } else {
        (speed_mps / COLLAR_SPEED_STEP).round().clamp(0.0, 255.0)
    };
    ((share * 255.0).round() as u32) << 16 | (speed as u32) << 8 | seed & 0xFF
}

/// A collar instance's V wake (radii), as `scatter.wesl` decodes it.
pub fn collar_wake(c: &ScatterInstance) -> f32 {
    let max = timefly_core::terrain::drawn::COLLAR_WAKE_MAX_RADII as f32;
    ((c.seed() >> 16) & 0xFF) as f32 / 255.0 * max
}

/// A collar instance's flow speed (m/s), as `scatter.wesl` decodes it.
pub fn collar_speed(c: &ScatterInstance) -> f32 {
    ((c.seed() >> 8) & 0xFF) as f32 * COLLAR_SPEED_STEP
}

/// The flow at `dir` (`[east, north]` in `dir`'s own frame, m/s) as a unit vector in the
/// region's local `(x, z)` axes of `basis` (`timefly_core::terrain::drawn::flow_local`, which
/// the core's collar footprint uses too). `None` where there is no flow.
pub fn flow_local(basis: &FrameBasis, dir: bevy::math::DVec3, flow_mps: [f32; 2]) -> Option<Vec2> {
    timefly_core::terrain::drawn::flow_local(basis, dir, flow_mps).map(|v| v.as_vec2())
}

/// The foam collar of boulder `b`, drawn as `rock` (its [`boulder_instance`]): at a foam site
/// standing in drawn water, a collar at the drawn level over its centre, scaled by its radius
/// and turned so its `+z` runs along the flow (by its seed where there is none). Its tag holds
/// the drawn rock's radius at the waterline as a share of its radius (0 for a rock just under
/// the water: a boil), and [`collar_tag_seed`] of its wake, its flow's speed and its seed.
pub fn foam_collar_instance(
    grid: &RegionGrid,
    basis: &FrameBasis,
    b: &Boulder,
    rock: &ScatterInstance,
) -> Option<ScatterInstance> {
    // Every foam site the generator kept (the shader clips the foam at the drawn water).
    if !b.collar || b.foam_site().is_none() {
        return None;
    }
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
        tag: ScatterInstance::tag_of(
            if y < 1.0 { share } else { 0.0 },
            collar_tag_seed(b.wake_radii, b.flow_mps[0].hypot(b.flow_mps[1]), b.seed),
        ),
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

/// The collars' look: the foam mode, the water's white water colour (linear) and the longest
/// wake (radii, to decode the tag's), the water's depth pull plus [`FOAM_COLLAR_PULL_M`], the
/// boulders' shade floor and the boils' shade ([`FOAM_BOIL_SHADE`]).
pub fn foam_collar_look() -> ScatterLook {
    let white = WHITE_WATER.to_linear();
    let max = timefly_core::terrain::drawn::COLLAR_WAKE_MAX_RADII as f32;
    ScatterLook {
        texel: UVec4::new(0, 0, ScatterKind::FoamCollar.shader_mode(), 0),
        colour: Vec4::new(white.red, white.green, white.blue, max),
        pull: Vec4::new(
            WATER_PULL_M,
            WATER_DEPTH_PULL,
            EARTH_RADIUS_M,
            FOAM_COLLAR_PULL_M,
        ),
        light: Vec4::new(
            BOULDER_SHADE_FLOOR * full_sun_illumination(),
            FOAM_BOIL_SHADE,
            0.0,
            0.0,
        ),
    }
}

/// The collars' material (shared with the pipeline warm-up so its key matches): the scatter
/// material binding the region's `water`, alpha-masked (drawn in the alpha-mask phase with the
/// water: no sorting; its coverage is a sample mask), rough as the water's foam and with its
/// reflectance.
pub fn foam_collar_material(palette: &Handle<Image>, water: &ScatterWater) -> ScatterMaterial {
    let mut m = scatter_material(palette, foam_collar_look(), water);
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
    /// The foam collars' material (it binds the region's water, rebound with each scene).
    pub collar_material: Handle<ScatterMaterial>,
    /// The scene grid whose boulders the sets hold.
    pub grid: Option<Arc<RegionGrid>>,
    /// Dry stand-in water, bound while the scene has no water ready (a replace's swap frame,
    /// the frame before its new water) and with no scene: the `u_int` water slots can't take
    /// the default image, and an old region's water must not stay bound.
    pub stand_in: ScatterWater,
    /// The water both materials bind now.
    pub bound: ScatterWater,
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
    mut buffers: Option<ResMut<Assets<ShaderBuffer>>>,
    mut materials: ResMut<Assets<ScatterMaterial>>,
) {
    let palette = images.add(palette_image());
    // Stand-ins until the first region's scene, and whenever it has no water ready.
    let water = ScatterWater::placeholder(&mut images, buffers.as_deref_mut());
    // The look is a stand-in until the first region's.
    let look = ScatterLook {
        texel: UVec4::ZERO,
        colour: Vec4::ZERO,
        pull: Vec4::ZERO,
        light: Vec4::ZERO,
    };
    let material = materials.add(scatter_material(&palette, look, &water));
    let shapes = boulder_meshes().map(|m| meshes.add(m)).to_vec();
    let set = commands
        .spawn((
            Name::new("Region boulders"),
            ScatterSet::empty(ScatterKind::Boulders),
            ScatterPool::new(shapes, material.clone(), BOULDER_VISIBLE_M),
        ))
        .id();
    // The collars' look is the same in every region: one material for good, its water rebound
    // with each scene.
    let collar_material = materials.add(foam_collar_material(&palette, &water));
    let collars = commands
        .spawn((
            Name::new("Region foam collars"),
            ScatterSet::empty(ScatterKind::FoamCollar),
            ScatterPool::new(
                vec![meshes.add(foam_collar_mesh())],
                collar_material.clone(),
                FOAM_COLLAR_VISIBLE_M,
            ),
        ))
        .id();
    commands.insert_resource(RegionBoulders {
        set,
        material,
        collars,
        collar_material,
        grid: None,
        stand_in: water.clone(),
        bound: water,
    });
}

/// Follows the terrain scene: both materials bind its water once it is ready and the dry
/// stand-in otherwise, and a new grid's boulders and collars replace the sets (and the
/// boulders' look); no scene empties them.
///
/// The water follows the handles, not the grid: a replace takes the scene's water on its swap
/// frame (the grid already new) and makes the new water a frame later. The default image a
/// taken handle leaves is a float one, which in a `u_int` slot is a wgpu validation error that
/// quits the app. Binding the stand-in (uploaded at startup, so the bind group is rebuilt at
/// once) also lets the old water's textures go on the swap frame, as the water's own material
/// does (Task 7's hop memory).
fn sync_boulders(
    mut commands: Commands,
    scene: Option<Res<TerrainScene>>,
    boulders: Option<ResMut<RegionBoulders>>,
    images: Res<Assets<Image>>,
    mut materials: ResMut<Assets<ScatterMaterial>>,
) {
    let Some(mut boulders) = boulders else { return };
    let water = scene
        .as_ref()
        .and_then(|s| {
            scene_water(s, &images)
                .map(|[inner, apron]| ScatterWater::of(&s.buffer, &inner, &apron))
        })
        .unwrap_or_else(|| boulders.stand_in.clone());
    if water != boulders.bound {
        for handle in [&boulders.material, &boulders.collar_material] {
            if let Some(mut m) = materials.get_mut(handle) {
                water.bind(&mut m.extension);
            }
        }
        boulders.bound = water;
    }
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
            collar: false,
            wake_radii: 0.0,
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
            collar: true,
            wake_radii: 5.0,
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
        assert_eq!(c.seed() & 0xFF, 9);
        assert!(
            (collar_speed(&c) - 4.0).abs() < 1e-3,
            "{}",
            collar_speed(&c)
        );
        assert!(
            (collar_wake(&c) - 5.0).abs() < 16.0 / 255.0,
            "{}",
            collar_wake(&c)
        );
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
        // Where the generator found the drawn water doesn't hold its footprint: none.
        let held_off = Boulder { collar: false, ..b };
        assert!(foam_collar_instance(&water, &basis, &held_off, &rock).is_none());
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

    /// Fix rounds 4 and 5: round a rock through the surface, the pillow piles on its upstream
    /// face and the V trails downstream, nothing upstream past the pillow, nothing abreast past
    /// it, and none of the V beyond its wake; over a rock just under, a boil just downstream.
    /// Mirrored in `scatter.wesl`, line for line (with the pulses, the foam and the coverage).
    #[test]
    fn collar_pillow_is_upstream_and_the_v_wake_downstream() {
        let (w, wake, still) = (0.8, 10.0, COLLAR_STILL);
        let at = |x: f32, y: f32, w: f32, wake: f32| collar_shape(Vec2::new(x, y), w, wake, still);
        assert!(at(0.0, -(w + 1.0), w, wake) > 0.9, "pillow");
        assert!(at(w + 0.1, 0.0, w, wake) > 0.9, "abreast");
        assert_eq!(at(w + 1.6, -0.05, w, wake), 0.0, "past the pillow abreast");
        assert_eq!(at(0.0, -(w + 2.8), w, wake), 0.0, "upstream");
        // The V: on its arms (|x| = 0.8 w + 0.3 + 0.35 y), clear between them past the churn,
        // and gone by the end of the wake.
        for y in [1.0f32, 2.0, 3.0] {
            let arm = 0.8 * w + 0.3 + 0.35 * y;
            for side in [-1.0, 1.0] {
                assert!(at(side * arm, y, w, wake) > 0.9, "an arm at {y}");
            }
        }
        assert_eq!(at(0.0, 9.7, w, wake), 0.0, "between the arms");
        assert!(at(0.0, 1.0, w, wake) > 0.9, "churn behind it");
        let arm_end = 0.8 * w + 0.3 + 0.35 * wake;
        assert_eq!(at(arm_end, wake, w, wake), 0.0);
        assert_eq!(at(2.3, -2.3, w, wake), 0.0, "no V upstream");
        // A rock just under (w = 0): a boil downstream of it, none upstream nor far down.
        assert!(at(0.0, 1.0, 0.0, wake) > 0.9, "the boil");
        assert_eq!(at(0.0, -0.6, 0.0, wake), 0.0);
        assert_eq!(at(0.0, 3.0, 0.0, wake), 0.0);
        assert_eq!(at(1.6, 1.5, 0.0, wake), 0.0, "no V");
        // A rock through the surface with no wake left: a boil.
        assert_eq!(at(0.0, 1.0, w, 0.0), at(0.0, 1.0, 0.0, 0.0));
        // The shader's copy, line for line.
        let src = include_str!("../assets/shaders/scatter.wesl");
        for line in COLLAR_WESL.lines() {
            assert!(src.contains(line), "scatter.wesl lacks: {line}");
        }
        assert!(src.contains(&format!(
            "const COLLAR_SPEED_STEP: f32 = {COLLAR_SPEED_STEP:?};"
        )));
        assert!(src.contains(&format!("const COLLAR_SAMPLES: u32 = {COLLAR_SAMPLES}u;")));
    }

    /// Fix round 5: the shapes stay put. Over 10 s and several seeds the pulses stay in their
    /// bounds (surge 0.85–1, breathe 0.8–1, bloom 0.35–1) and swing within them; the pulsed
    /// shape never exceeds the still one; and the pillow's, the arms' and the churn's cores stay
    /// white throughout. A boil blooms and fades on its own 2–4 s cycle.
    #[test]
    fn collar_shapes_stay_put_while_they_pulse() {
        let (w, wake) = (0.8, 10.0);
        let points: Vec<Vec2> = (-30..=30)
            .flat_map(|i| (-30..=120).map(move |j| Vec2::new(i as f32 * 0.2, j as f32 * 0.1)))
            .collect();
        let cores = [
            Vec2::new(0.0, -(w + 1.0)),
            Vec2::new(0.8 * w + 0.3 + 0.7, 2.0),
            Vec2::new(0.0, 1.0),
        ];
        for seed in [0u32, 77, 200, 255] {
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for k in 0..200 {
                let pulses = collar_pulses(k as f32 * 0.05, seed);
                lo = lo.min(pulses);
                hi = hi.max(pulses);
                for &p in points.iter().step_by(7) {
                    for ww in [0.0, w] {
                        assert!(
                            collar_shape(p, ww, wake, pulses)
                                <= collar_shape(p, ww, wake, COLLAR_STILL) + 1e-6,
                            "seed {seed}, {p}"
                        );
                    }
                }
                for c in cores {
                    assert!(collar_shape(c, w, wake, pulses) > 0.9, "core {c} at {k}");
                }
            }
            assert!(lo.x >= 0.85 - 1e-6 && hi.x <= 1.0 + 1e-6, "surge {lo} {hi}");
            assert!(
                lo.y >= 0.8 - 1e-6 && hi.y <= 1.0 + 1e-6,
                "breathe {lo} {hi}"
            );
            assert!(lo.z >= 0.35 - 1e-6 && hi.z <= 1.0 + 1e-6, "bloom {lo} {hi}");
            assert!(
                hi.x - lo.x > 0.08 && hi.y - lo.y > 0.1 && hi.z - lo.z > 0.5,
                "seed {seed}: swings {lo} {hi}"
            );
        }
    }

    /// Fix round 5 ("softer"): every edge ramps. Across an arm, at the pillow's rim and at the
    /// wake's end the shape climbs from 10% to 90% over at least 0.3 radii, and the coverage
    /// takes values between nothing and full across it (no step).
    /// Bevy wraps `globals.time` every hour: the collars' animation must not jump there.
    #[test]
    fn collar_animation_is_continuous_across_the_time_wrap() {
        use crate::art::{TIME_WRAP_S, assert_divides_wrap};
        for p in COLLAR_PULSE_PERIODS_S
            .into_iter()
            .chain(BOIL_PERIODS_S)
            .chain(COLLAR_FOAM_PERIODS_S)
        {
            assert_divides_wrap(p);
        }
        // Every boil period is used (bits 2–3 of the seed pick it).
        let used: BTreeSet<u32> = (0..256u32)
            .map(|s| BOIL_PERIODS_S[((s >> 2) & 3) as usize].to_bits())
            .collect();
        assert_eq!(used.len(), BOIL_PERIODS_S.len());
        let (before, after) = (TIME_WRAP_S - 0.005, 0.005);
        for seed in 0..256 {
            let jump = (collar_pulses(before, seed) - collar_pulses(after, seed)).abs();
            assert!(jump.max_element() < 0.03, "seed {seed}: pulses jump {jump}");
            for k in 0..8 {
                let m = Vec2::new(0.7 * k as f32 - 2.0, 1.3 * k as f32);
                let jump =
                    (collar_foam(m, 4.0, before, seed) - collar_foam(m, 4.0, after, seed)).abs();
                assert!(jump < 0.05, "seed {seed}, {m}: foam jumps {jump}");
            }
        }
    }

    #[test]
    fn collar_edges_are_soft() {
        let (w, wake) = (0.8, 10.0);
        let width = |line: &dyn Fn(f32) -> Vec2| {
            let v: Vec<f32> = (0..=400)
                .map(|k| collar_shape(line(k as f32 * 0.01), w, wake, COLLAR_STILL))
                .collect();
            let first = |th: f32| v.iter().position(|&s| s <= th).unwrap() as f32 * 0.01;
            let mid = v.iter().filter(|&&s| s > 0.2 && s < 0.6).count();
            (first(0.1) - first(0.9), mid)
        };
        let arm = 0.8 * w + 0.3 + 0.35 * 3.0;
        for (name, line) in [
            (
                "arm",
                Box::new(move |d: f32| Vec2::new(arm + d, 3.0)) as Box<dyn Fn(f32) -> Vec2>,
            ),
            (
                "pillow",
                Box::new(move |d: f32| Vec2::new(0.0, -(w + 0.8) - d)),
            ),
            (
                "wake's end",
                Box::new(move |d: f32| Vec2::new(arm + 0.35 * 2.0, 5.0 + d * 2.0)),
            ),
        ] {
            let (ramp, mid) = width(&*line);
            println!("{name}: ramps over {ramp:.2} radii");
            assert!(ramp >= 0.3, "{name}: ramps over {ramp}");
            assert!(mid >= 10, "{name}: {mid} partial samples");
        }
        // Coverage feathers: half the shape covers a share, not all or nothing.
        let c = collar_coverage(0.5, 0.0);
        assert!(c > 0.2 && c < 0.6, "{c}");
        assert_eq!(collar_coverage(0.12, 1.0), 0.0);
        assert_eq!(collar_coverage(1.0, 0.5), 1.0);
        // Dark water through the wake: where the streaks are low the core is only partly
        // covered.
        assert!(collar_coverage(1.0, -0.6) < 0.4);
    }

    /// Fix round 5 ("animated"): the foam inside a collar is carried downstream at the water's
    /// speed. Over a 0.1 s step the texture moved `v × 0.1` m downstream matches itself (its
    /// correlation with the shifted field is near 1), much better than the field left in place,
    /// and better than one moved at half the speed.
    #[test]
    fn collar_foam_is_carried_downstream_with_the_flow() {
        let (v, t, dt) = (5.0f32, 3.21, 0.1);
        let corr = |shift: f32| {
            let mut pairs = Vec::new();
            for i in 0..40 {
                for j in 0..80 {
                    let m = Vec2::new(i as f32 * 0.17 - 3.0, j as f32 * 0.13);
                    pairs.push((
                        collar_foam(m, v, t, 41),
                        collar_foam(m + Vec2::new(0.0, shift), v, t + dt, 41),
                    ));
                }
            }
            let n = pairs.len() as f32;
            let (ma, mb) = pairs
                .iter()
                .fold((0.0, 0.0), |(x, y), &(a, b)| (x + a / n, y + b / n));
            let (mut c, mut va, mut vb) = (0.0, 0.0, 0.0);
            for &(a, b) in &pairs {
                c += (a - ma) * (b - mb);
                va += (a - ma) * (a - ma);
                vb += (b - mb) * (b - mb);
            }
            c / (va * vb).sqrt()
        };
        let (moved, half, still) = (corr(v * dt), corr(0.5 * v * dt), corr(0.0));
        println!("carried {moved:.3}, at half speed {half:.3}, left in place {still:.3}");
        assert!(moved > 0.9, "{moved}");
        assert!(
            moved > still + 0.2 && moved > half + 0.05,
            "{moved} {half} {still}"
        );
    }

    /// The V wake lengthens with the flow's speed (`collar_wake_radii`, up to its longest), and
    /// the shape follows it: a point 9.5 radii down the arm is white behind a fast rock, not a
    /// slow one; the tag carries the wake, the speed and the seed to the shader.
    #[test]
    fn the_wake_lengthens_with_speed() {
        use timefly_core::terrain::drawn::{COLLAR_WAKE_MAX_RADII, collar_wake_radii};
        let mut last = 0.0;
        for v in [3.0, 4.0, 5.0, 6.0] {
            let wake = collar_wake_radii(v);
            assert!(wake > last, "{v} m/s: {wake}");
            last = wake;
        }
        assert_eq!(collar_wake_radii(3.0), 9.0);
        assert_eq!(collar_wake_radii(6.0), 15.0);
        assert_eq!(collar_wake_radii(20.0), COLLAR_WAKE_MAX_RADII);
        let w = 0.8;
        let p = |y: f32| Vec2::new(0.8 * w + 0.3 + 0.35 * y, y);
        let (slow, fast) = (collar_wake_radii(3.0) as f32, collar_wake_radii(6.0) as f32);
        assert_eq!(collar_shape(p(9.5), w, slow, COLLAR_STILL), 0.0);
        assert!(collar_shape(p(9.5), w, fast, COLLAR_STILL) > 0.5);
        // The tag round trip.
        let max = COLLAR_WAKE_MAX_RADII as f32;
        for (wake, speed) in [
            (0.0, 0.0),
            (1.4, 3.0),
            (7.0, 4.55),
            (16.0, 6.0),
            (16.0, 12.75),
        ] {
            let c = ScatterInstance {
                translation: Vec3::ZERO,
                scale: 1.0,
                yaw: 0.0,
                tag: ScatterInstance::tag_of(0.5, collar_tag_seed(wake, speed, 0xABCD_1234)),
            };
            assert!((collar_wake(&c) - wake).abs() <= 0.5 * max / 255.0 + 1e-6);
            assert!((collar_speed(&c) - speed).abs() <= 0.5 * COLLAR_SPEED_STEP + 1e-6);
            assert_eq!(c.seed() & 0xFF, 0x34);
        }
    }

    /// A boil collar (dimmer, no V) appears only over a rock just under the surface; a rock
    /// through it gets the pillow and V; a deeper one gets none.
    #[test]
    fn a_boil_marks_only_rocks_just_under() {
        let water = plane_grid(3.0, Vec::new());
        let basis = water.spec.frame().basis();
        let (x, z) = (120.0, -340.0);
        let through = rock_at(&water, x, z, 1.5, 0.75, [5.0, 0.0], 9);
        let rock = boulder_instance(&water, &basis, &through).unwrap();
        let c = foam_collar_instance(&water, &basis, &through, &rock).unwrap();
        assert!(c.wet() > 0.0 && collar_wake(&c) > 4.0, "pillow and V");
        // Its top 0.3 m under: a boil (no waterline).
        let under = Boulder {
            level_m: 3.3,
            ..rock_at(&water, x, z, 1.5, 1.0, [5.0, 0.0], 9)
        };
        let rock = boulder_instance(&water, &basis, &under).unwrap();
        let c = foam_collar_instance(&water, &basis, &under, &rock).unwrap();
        assert_eq!(c.wet(), 0.0, "a boil");
        // 0.6 m under: none.
        let deeper = Boulder {
            level_m: 3.6,
            ..under
        };
        assert!(foam_collar_instance(&water, &basis, &deeper, &rock).is_none());
        // Dimmer.
        let look = foam_collar_look();
        assert_eq!(look.light.y, FOAM_BOIL_SHADE);
        const { assert!(FOAM_BOIL_SHADE < 1.0) };
        let src = include_str!("../assets/shaders/scatter.wesl");
        assert!(src.contains("let shade = select(look.light.y, 1.0, w > 0.0 && wake > 0.0);"));
    }

    /// The collar's shape (still, its largest) stays within `COLLAR_RING_RADII` but for the V,
    /// which stays within its wake downstream and `COLLAR_WAKE_HALF_RADII + COLLAR_WAKE_SPREAD ×
    /// y` either side; and the quad holds it all. Fix round 5: the shader clips the foam at the
    /// drawn water itself, so the core no longer checks this footprint against the lattices.
    #[test]
    fn collar_shape_stays_in_the_checked_footprint() {
        use timefly_core::terrain::drawn::{
            COLLAR_RING_RADII, COLLAR_WAKE_HALF_RADII, COLLAR_WAKE_MAX_RADII, COLLAR_WAKE_SPREAD,
        };
        let ring = COLLAR_RING_RADII as f32;
        let [across, up, down] = FOAM_COLLAR_EXTENT;
        for wake in [0.0, 1.4, 6.0, COLLAR_WAKE_MAX_RADII as f32] {
            for w in [0.0, 0.3, 0.7, 1.0] {
                for i in -115..=115 {
                    for j in -35..=170 {
                        let p = Vec2::new(i as f32 * 0.1, j as f32 * 0.1);
                        let half =
                            (COLLAR_WAKE_HALF_RADII + COLLAR_WAKE_SPREAD * f64::from(p.y)) as f32;
                        let in_wake = p.y >= 0.0 && p.y <= wake && p.x.abs() <= half;
                        let shape = collar_shape(p, w, wake, COLLAR_STILL);
                        if p.length() > ring && !in_wake {
                            assert_eq!(shape, 0.0, "w {w}, wake {wake}, {p}");
                        }
                        if shape > 0.0 {
                            assert!(
                                p.x.abs() <= across && p.y >= -up && p.y <= down,
                                "outside the quad: {p}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// `collar_shape`, `collar_pulses`, `foam_layer`, `collar_foam` and `collar_coverage` as
    /// `scatter.wesl` writes them.
    const COLLAR_WESL: &str = "fn collar_shape(p: vec2<f32>, w: f32, wake: f32, surge: f32, breathe: f32, bloom: f32) -> f32 {
    let rho = length(p);
    let up = clamp(-p.y / max(rho, 1e-3), 0.0, 1.0);
    let breaks = w > 0.0 && wake > 0.0;
    let inner = 0.8 * w;
    let edge = (w + 0.9 + 1.6 * up) * surge;
    let pillow = (1.0 - smoothstep(edge - 0.6, edge, rho)) * smoothstep(inner - 0.15, inner, rho);
    let along = max(p.y, 0.0);
    let arm = 0.8 * w + 0.3 + 0.35 * along;
    let arm_half = 0.6 + 0.25 * along;
    let reach = max(wake * breathe, 1e-3);
    let fade = smoothstep(-0.3, 0.5, p.y) * (1.0 - smoothstep(0.5 * reach, reach, p.y));
    let arms = (1.0 - smoothstep(0.1 * arm_half, arm_half, abs(abs(p.x) - arm))) * fade;
    let core = w + 0.8 + 0.2 * along;
    let churn = (1.0 - smoothstep(0.2 * core, core, abs(p.x))) * smoothstep(-0.3, 0.4, p.y)
        * (1.0 - smoothstep(0.4 * reach, 0.95 * reach, p.y));
    let boil = (1.0 - smoothstep(0.4, 1.5, length(p - vec2<f32>(0.0, 1.0)))) * bloom;
    let trail = (1.0 - smoothstep(0.1, 0.6, abs(p.x))) * smoothstep(0.5, 1.2, p.y)
        * (1.0 - smoothstep(1.4, 2.9, p.y)) * bloom;
    return select(max(boil, trail), max(pillow, max(arms, churn)), breaks);
}
fn collar_pulses(t: f32, seed: u32) -> vec3<f32> {
    let phase = f32(seed & 255u) / 256.0;
    let q = 0.6 * sin(TAU * (t / 1.2 + phase)) + 0.4 * sin(TAU * (t / 3.0 + 2.3 * phase));
    let b = 0.55 * sin(TAU * (t / 1.8 + 1.7 * phase)) + 0.45 * sin(TAU * (t / 2.4 + 0.4 * phase));
    let k = (seed >> 2u) & 3u;
    let period = select(select(2.0, 2.4, k == 1u), select(3.0, 3.6, k == 3u), k >= 2u);
    let bloom = 0.35 + 0.65 * (0.5 + 0.5 * sin(TAU * (t / period + phase)));
    return vec3<f32>(0.925 + 0.075 * q, 0.9 + 0.1 * b, bloom);
}
fn foam_layer(m: vec2<f32>, speed: f32, t: f32, period: f32, scale: vec2<f32>, seed: u32) -> f32 {
    let a = fract(t / period);
    let b = fract(a + 0.5);
    let wa = 1.0 - abs(2.0 * a - 1.0);
    let wb = 1.0 - abs(2.0 * b - 1.0);
    let na = foam_noise((m - vec2<f32>(0.0, speed * period * a)) / scale, 0.0, seed);
    let nb = foam_noise((m - vec2<f32>(0.0, speed * period * b)) / scale, 7.5, seed);
    return (wa * na + wb * nb) * inverseSqrt(wa * wa + wb * wb);
}
fn collar_foam(m: vec2<f32>, speed: f32, t: f32, seed: u32) -> f32 {
    let q = m + vec2<f32>(f32(seed) * 3.7, f32(seed) * 1.9);
    let fine = foam_layer(q, speed, t, 1.0, vec2<f32>(0.35, 1.4), 211u);
    let coarse = foam_layer(q, 0.75 * speed, t, 1.6, vec2<f32>(1.2, 4.8), 223u);
    return 0.65 * fine + 0.5 * coarse;
}
fn collar_coverage(shape: f32, foam: f32) -> f32 {
    return clamp((shape - 0.12) / 0.6 * (0.85 + 1.0 * foam), 0.0, 1.0);
}";

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
        let longest = timefly_core::terrain::drawn::COLLAR_WAKE_MAX_RADII as f32;
        assert!(down > longest, "room for the wake");
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

    /// The water both scatter materials bind (slots 102–104).
    fn bound_water(app: &App) -> [ScatterWater; 2] {
        let b = app.world().resource::<RegionBoulders>();
        let materials = app.world().resource::<Assets<ScatterMaterial>>();
        [&b.material, &b.collar_material].map(|h| {
            let e = &materials.get(h).unwrap().extension;
            ScatterWater::of(&e.patches, &e.inner_water, &e.apron_water)
        })
    }

    /// The scene's water, once both textures exist (the water's own rule).
    fn ready_scene_water(app: &App) -> Option<ScatterWater> {
        let scene = app.world().get_resource::<TerrainScene>()?;
        let images = app.world().resource::<Assets<Image>>();
        crate::water::scene_water(scene, images)
            .map(|[inner, apron]| ScatterWater::of(&scene.buffer, &inner, &apron))
    }

    /// A replace takes the scene's water on its swap frame and makes the new water a frame
    /// later. The scatter materials' `u_int` water slots must never take the default image (a
    /// float one: wgpu's `InvalidTextureSampleType`, which quits the app): they hold the dry
    /// stand-in until the scene's water is ready, then the scene's, and the stand-in again once
    /// the region is torn down (no old region's water kept alive on the globe).
    #[test]
    fn scatter_water_never_binds_the_default_image() {
        let mut app = boulder_app();
        app.update();
        // Before any region: the stand-in, in both materials.
        let [stand_in, collar_stand_in] = bound_water(&app);
        assert_eq!(stand_in, collar_stand_in);
        assert_ne!(stand_in.inner_water, Handle::default());
        assert_ne!(stand_in.apron_water, Handle::default());
        let check = |app: &App, frame: &str| {
            for w in bound_water(app) {
                assert_ne!(
                    w.inner_water,
                    Handle::default(),
                    "{frame}: slot 103 default"
                );
                assert_ne!(
                    w.apron_water,
                    Handle::default(),
                    "{frame}: slot 104 default"
                );
                match ready_scene_water(app) {
                    Some(scene) => assert!(
                        w == scene || w == stand_in,
                        "{frame}: neither the scene's water nor the stand-in"
                    ),
                    None => assert_eq!(w, stand_in, "{frame}: not the stand-in while unready"),
                }
            }
        };
        let (a, b) = (rapids_grid(5, 0.0), rapids_grid(3, 40_000.0));
        enter(&mut app, &a);
        for k in 0..4 {
            app.update();
            check(&app, &format!("enter a, frame {k}"));
        }
        let water_a = ready_scene_water(&app).expect("a's water is ready");
        assert_eq!(bound_water(&app), [water_a.clone(), water_a.clone()]);
        // The hop: a replace at the same settings.
        enter(&mut app, &b);
        let mut unready = 0;
        for k in 0..8 {
            app.update();
            check(&app, &format!("hop to b, frame {k}"));
            let scene = app.world().resource::<TerrainScene>();
            if scene.inner_water == Handle::default() {
                unready += 1;
            }
            for w in bound_water(&app) {
                assert_ne!(
                    w.inner_water, water_a.inner_water,
                    "a's water kept, frame {k}"
                );
                assert_ne!(
                    w.apron_water, water_a.apron_water,
                    "a's water kept, frame {k}"
                );
            }
        }
        assert!(
            unready > 0,
            "the fixture must reach a swap frame with no scene water"
        );
        let water_b = ready_scene_water(&app).expect("b's water is ready");
        assert_ne!(water_b, water_a);
        assert_eq!(bound_water(&app), [water_b.clone(), water_b.clone()]);
        // Torn down: the stand-in again, no region's water held.
        app.world_mut().remove_resource::<ActiveRegion>();
        for k in 0..3 {
            app.update();
            check(&app, &format!("teardown, frame {k}"));
        }
        assert!(app.world().get_resource::<TerrainScene>().is_none());
        assert_eq!(bound_water(&app), [stand_in.clone(), stand_in]);
    }
}
