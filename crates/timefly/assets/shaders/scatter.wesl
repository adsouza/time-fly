// Instanced scatter in region worlds (rivers and lakes design §4.4, Task 9): boulders now,
// vegetation later. Each instance is a pooled entity sharing its shape's mesh and its kind's
// material, so Bevy draws every shape as one instanced batch; the entity's MeshTag carries the
// per-instance data (`ScatterInstance::tag_of` in scatter.rs):
//   tag = (wet × 255) << 24 | seed & 0xFFFFFF,
// with `wet` the share of the instance's height under the water (0 at the bottom, 1 at the top:
// meshes span y in [-1, 1]).
//
// Vertex: the instance's transform, then the curvature drop d² / 2R for the horizontal distance
// d from the camera (as cdlod_vertex drops the terrain); the part above the waterline is pulled
// toward the camera along the view ray by the water's own pull (water.wesl), depth only, so the
// water, which wins ties by that pull, never hides a rock top standing above it. The scatter
// draws only in the main pass (no shadows, no prepass), so the view is the region camera.
//
// Fragment: the kind's colour from the terrain palette (look.texel), turned about the grey axis
// and brightened or darkened by the seed (look.colour.xy), darkened where wet (look.colour.z)
// below the waterline and over a band above it (look.colour.w), then Bevy's PBR lighting with
// the shade floored as the terrain's light ramp floors it (look.light.x), and fog.
//
// Foam collars (look.texel.z == SCATTER_MODE_FOAM, Task 11c): a flat quad at the water level,
// its local +z downstream, in boulder radii; the tag's top byte is the drawn rock's radius at the
// waterline as a share of its radius (0 for a rock just under the water), its next byte the V
// wake's length as a share of look.colour.w radii, the next its flow's speed (COLLAR_SPEED_STEP
// steps) and the low byte the seed. The vertex stage pulls every vertex by the water's pull plus
// look.pull.w, so the water at its level never hides it. The fragment (fix round 5, "softer,
// whiter wakes", animated) shapes it by `collar_shape`: round a rock through the surface, foam
// piled on its upstream face (the pillow), a V of two arms trailing downstream as long as its
// wake, and churn behind it; over a rock just under the surface, a boil just downstream of it,
// dimmer (look.light.y). The shapes stay put but pulse (`collar_pulses`: the pillow surges, the
// wake breathes, a boil blooms), while the foam inside them (`collar_foam`, streaks 4:1 along the
// flow) is carried downstream at the water's speed. Its coverage (`collar_coverage`, soft at the
// edges, broken by the streaks) is written as a dithered sample mask: the material stays
// alpha-masked (no sorting) and drawn after the opaque pass. It is clipped where the region draws
// no water (the water's own `blended_water` over the same textures), and thins out where a
// radius shrinks to a few pixels. Its colour is the water's white water (look.colour.rgb), lit
// as the water's foam is (rough, the water's reflectance), with the boulders' shade floor.

import bevy_pbr::render::mesh_functions::{get_world_from_local, get_tag, mesh_normal_local_to_world};
import bevy_pbr::render::mesh_view_bindings::{view, globals};
import package::noise::gradient_noise3;
import package::terrain_cdlod::{TerrainBuffer, blended_water};
import package::waves::TAU;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing};
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

// `ScatterLook` in scatter.rs.
struct ScatterLook {
    // xy: the palette texel of the kind's colour; z: the mode (SCATTER_MODE_*); w: unused.
    texel: vec4<u32>,
    // Solid: x: hue jitter (± radians); y: value jitter (± share); z: wet darkening (share); w:
    // the wet band above the waterline (share of the height). Foam: rgb: its colour (linear);
    // w: the longest wake (radii), which the tag's wake byte is a share of.
    colour: vec4<f32>,
    // x: the water's constant pull (m); y: its share of the view distance; z: Earth radius (m);
    // w: foam only, the extra pull (m) in front of the water.
    pull: vec4<f32>,
    // x: the least light (exposure applied) a surface shows: the terrain ramp's shade floor;
    // y: foam only, a boil's shade.
    light: vec4<f32>,
}

// `SCATTER_MODE_FOAM` in scatter.rs.
const SCATTER_MODE_FOAM: u32 = 1u;

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var palette: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> look: ScatterLook;
// The region's terrain buffer and water textures, as the water's material binds them: a collar
// draws only where the water does.
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var<storage, read> terrain: TerrainBuffer;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var inner_water: texture_2d<u32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(104) var apron_water: texture_2d<u32>;

// `COLLAR_SPEED_STEP` in boulders.rs: a collar's tag holds its flow's speed in these steps (m/s).
const COLLAR_SPEED_STEP: f32 = 0.05;
// `COLLAR_SAMPLES` in boulders.rs: a collar's coverage is dithered over this many samples (the
// region camera's MSAA; with fewer, the extra bits are ignored).
const COLLAR_SAMPLES: u32 = 4u;

// The fragment's output: the colour, and the samples it covers (a collar's coverage).
struct ScatterFragmentOutput {
    @location(0) color: vec4<f32>,
    @builtin(sample_mask) mask: u32,
}

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
}

struct ScatterVertexOutput {
    @builtin(position) position: vec4<f32>,
    // The dropped surface point (before the depth pull).
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    // The height within the instance: 0 at its bottom, 1 at its top.
    @location(2) height: f32,
    @location(3) @interpolate(flat) instance_index: u32,
    // The mesh's own xz (a collar's: radii across and downstream).
    @location(4) local: vec2<f32>,
    // The instance's scale (a collar's: its boulder's radius, m).
    @location(5) @interpolate(flat) scale: f32,
}

// A 32-bit integer hash (PCG's output permutation), for the seed's jitter.
fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// A hash of `seed` as a value in [-1, 1].
fn signed_unit(seed: u32) -> f32 {
    return f32(pcg(seed) >> 8u) / 8388607.5 - 1.0;
}

@vertex
fn vertex(in: Vertex) -> ScatterVertexOutput {
    let world_from_local = get_world_from_local(in.instance_index);
    var world = (world_from_local * vec4<f32>(in.position, 1.0)).xyz;
    let camera = view.world_position;
    let d = world.xz - camera.xz;
    world.y -= dot(d, d) / (2.0 * look.pull.z);
    let tag = get_tag(in.instance_index);
    let wet = f32(tag >> 24u) / 255.0;
    let height = (in.position.y + 1.0) * 0.5;
    // Above the waterline of a wet instance: the water's pull, so its depth matches the water's.
    // A foam collar, at the water's level, takes it everywhere, plus look.pull.w.
    let ray = world - camera;
    let distance = length(ray);
    let foam = look.texel.z == SCATTER_MODE_FOAM;
    let above = select(select(0.0, 1.0, wet > 0.0 && height > wet), 1.0, foam);
    let pull = min(look.pull.x + look.pull.y * distance + look.pull.w, 0.5 * distance) * above;
    let pulled = world - ray * (pull / max(distance, 1e-3));
    var out: ScatterVertexOutput;
    out.position = position_world_to_clip(pulled);
    out.world_position = vec4<f32>(world, 1.0);
    out.world_normal = mesh_normal_local_to_world(in.normal, in.instance_index);
    out.height = height;
    out.instance_index = in.instance_index;
    out.local = in.position.xz;
    out.scale = length(world_from_local[0].xyz);
    return out;
}

// Gradient noise at `q` (in cells), its third axis `z`: integer cell plus fraction, as
// noise.wesl expects.
fn foam_noise(q: vec2<f32>, z: f32, seed: u32) -> f32 {
    let c = floor(q);
    let zc = floor(z);
    return gradient_noise3(vec3<i32>(i32(c.x), i32(c.y), i32(zc)), vec3<f32>(q - c, z - zc), seed);
}

// `collar_shape` in boulders.rs: how white a collar is (0 to 1, soft-edged) at `p` (radii, x
// across, y downstream) round a rock whose waterline is `w` of its radius, its V wake `wake` radii
// long, the pillow surged by `surge`, the wake breathed by `breathe` and a boil bloomed by `bloom`
// (`collar_pulses`; all at most 1, so the shape never leaves its still extent). Through the
// surface: the pillow on its upstream face, the V's two arms spreading downstream at about 19°
// until the wake's end, and churn behind it. Just under (w = 0, or no wake): a boil just
// downstream of it and a short trail.
fn collar_shape(p: vec2<f32>, w: f32, wake: f32, surge: f32, breathe: f32, bloom: f32) -> f32 {
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

// `collar_pulses` in boulders.rs: (surge, breathe, bloom) at time `t` (s) for a collar of seed
// `seed`: the pillow surges between 0.85 and 1 and the wake breathes between 0.8 and 1, each two
// sines (`COLLAR_PULSE_PERIODS_S`); a boil blooms between 0.35 and 1 over a period its seed picks
// (`BOIL_PERIODS_S`). Every period divides Bevy's hour-long time wrap.
fn collar_pulses(t: f32, seed: u32) -> vec3<f32> {
    let phase = f32(seed & 255u) / 256.0;
    let q = 0.6 * sin(TAU * (t / 1.2 + phase)) + 0.4 * sin(TAU * (t / 3.0 + 2.3 * phase));
    let b = 0.55 * sin(TAU * (t / 1.8 + 1.7 * phase)) + 0.45 * sin(TAU * (t / 2.4 + 0.4 * phase));
    let k = (seed >> 2u) & 3u;
    let period = select(select(2.0, 2.4, k == 1u), select(3.0, 3.6, k == 3u), k >= 2u);
    let bloom = 0.35 + 0.65 * (0.5 + 0.5 * sin(TAU * (t / period + phase)));
    return vec3<f32>(0.925 + 0.075 * q, 0.9 + 0.1 * b, bloom);
}

// One flow-map layer of `collar_foam`: noise at `m` (m, x across, y downstream) stretched
// `scale` (m across, m along), scrolled downstream at `speed` (m/s) over `period` (s) and
// restarted, two phases half a period apart cross-faded so the restart never shows.
fn foam_layer(m: vec2<f32>, speed: f32, t: f32, period: f32, scale: vec2<f32>, seed: u32) -> f32 {
    let a = fract(t / period);
    let b = fract(a + 0.5);
    let wa = 1.0 - abs(2.0 * a - 1.0);
    let wb = 1.0 - abs(2.0 * b - 1.0);
    let na = foam_noise((m - vec2<f32>(0.0, speed * period * a)) / scale, 0.0, seed);
    let nb = foam_noise((m - vec2<f32>(0.0, speed * period * b)) / scale, 7.5, seed);
    return (wa * na + wb * nb) * inverseSqrt(wa * wa + wb * wb);
}

// `collar_foam` in boulders.rs: the collar's foam texture at `m` (m from the rock, x across,
// y downstream), carried downstream with the water at `speed` (m/s): fine streaks 4:1 along the
// flow at the water's speed and coarse clumps at three quarters of it, each its own flow-map
// layer, each collar's own (offset by its seed).
fn collar_foam(m: vec2<f32>, speed: f32, t: f32, seed: u32) -> f32 {
    let q = m + vec2<f32>(f32(seed) * 3.7, f32(seed) * 1.9);
    let fine = foam_layer(q, speed, t, 1.0, vec2<f32>(0.35, 1.4), 211u);
    let coarse = foam_layer(q, 0.75 * speed, t, 1.6, vec2<f32>(1.2, 4.8), 223u);
    return 0.65 * fine + 0.5 * coarse;
}

// `collar_coverage` in boulders.rs: the share of a pixel a collar covers (0 to 1): its shape
// streaked by its foam (dark water shows between the streaks, most in the shape's fringe), so the
// edge feathers out over the shape's own soft ramps.
fn collar_coverage(shape: f32, foam: f32) -> f32 {
    return clamp((shape - 0.12) / 0.6 * (0.85 + 1.0 * foam), 0.0, 1.0);
}

@fragment
fn fragment(in: ScatterVertexOutput, @builtin(front_facing) is_front: bool) -> ScatterFragmentOutput {
    if look.texel.z == SCATTER_MODE_FOAM {
        return foam_fragment(in, is_front);
    }
    var out: ScatterFragmentOutput;
    out.color = solid_fragment(in, is_front).color;
    out.mask = 0xFFFFFFFFu;
    return out;
}

// Interleaved gradient noise at a pixel: a dither in [0, 1).
fn pixel_dither(xy: vec2<f32>) -> f32 {
    return fract(52.982919 * fract(dot(xy, vec2<f32>(0.06711056, 0.00583715))));
}

// A foam collar's fragment (see the header): its coverage as a sample mask (dithered, so a soft
// edge feathers out rather than stepping), clipped where the region draws no water, the foam's
// colour, lit as the water's foam.
fn foam_fragment(in: ScatterVertexOutput, is_front: bool) -> ScatterFragmentOutput {
    // Derivatives first, while every invocation of the quad is still running.
    let footprint = length(fwidth(in.world_position.xz)) / max(in.scale, 1e-3);
    let tag = get_tag(in.instance_index);
    let w = f32(tag >> 24u) / 255.0;
    let wake = f32((tag >> 16u) & 255u) / 255.0 * look.colour.w;
    let speed = f32((tag >> 8u) & 255u) * COLLAR_SPEED_STEP;
    let seed = tag & 255u;
    let t = globals.time;
    let pulses = collar_pulses(t, seed);
    let shape = collar_shape(in.local, w, wake, pulses.x, pulses.y, pulses.z)
        * (1.0 - smoothstep(0.3, 0.8, footprint));
    if shape <= 0.12 {
        discard;
    }
    // Only where the region's own water is drawn (the water shader's discard).
    let water = blended_water(inner_water, apron_water, terrain.params, in.world_position.xz);
    if !(water.thickness > 0.0 && water.ribbon >= 0.0) {
        discard;
    }
    let coverage = collar_coverage(shape, collar_foam(in.local * in.scale, speed, t, seed));
    // Coverage as samples (alpha-to-coverage by hand, in the alpha-mask phase): `n` of
    // COLLAR_SAMPLES, dithered per pixel.
    let n = u32(floor(coverage * f32(COLLAR_SAMPLES) + pixel_dither(in.position.xy)));
    if n == 0u {
        discard;
    }
    // A boil, over a rock just under the surface, is dimmer.
    let shade = select(look.light.y, 1.0, w > 0.0 && wake > 0.0);
    let albedo = look.colour.rgb * shade;
    var mesh: VertexOutput;
    mesh.position = in.position;
    mesh.world_position = in.world_position;
    mesh.world_normal = vec3<f32>(0.0, 1.0, 0.0);
    @if(VERTEX_UVS_A)
    mesh.uv = in.world_position.xz;
    @if(VERTEX_UVS_B)
    mesh.uv_b = in.world_position.xz;
    @if(VERTEX_TANGENTS)
    mesh.world_tangent = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    @if(VERTEX_COLORS)
    mesh.color = vec4<f32>(1.0);
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    mesh.instance_index = in.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    mesh.visibility_range_dither = 0;

    var pbr_input = pbr_input_from_standard_material(mesh, is_front);
    pbr_input.material.base_color = vec4<f32>(albedo, 1.0);
    var color = apply_pbr_lighting(pbr_input);
    color = vec4<f32>(max(color.rgb, albedo * look.light.x), 1.0);
    var out: ScatterFragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, color);
    out.mask = (1u << min(n, 31u)) - 1u;
    return out;
}

// `colour` turned by `angle` radians about the grey axis (Rodrigues' rotation).
fn turn_hue(colour: vec3<f32>, angle: f32) -> vec3<f32> {
    let k = vec3<f32>(0.57735027);
    let c = cos(angle);
    let s = sin(angle);
    return colour * c + cross(k, colour) * s + k * dot(k, colour) * (1.0 - c);
}

// A solid instance's fragment (boulders).
fn solid_fragment(in: ScatterVertexOutput, is_front: bool) -> FragmentOutput {
    let tag = get_tag(in.instance_index);
    let wet = f32(tag >> 24u) / 255.0;
    let seed = tag & 0xFFFFFFu;
    let base = textureLoad(palette, vec2<i32>(look.texel.xy), 0).rgb;
    let hue = signed_unit(seed) * look.colour.x;
    let value = 1.0 + signed_unit(seed ^ 0x9E3779u) * look.colour.y;
    // Wet: dark under the waterline, fading out over the band above it; a dry instance not at all.
    let band = max(look.colour.w, 1e-3);
    let damp = select(0.0, 1.0 - smoothstep(wet, wet + band, in.height), wet > 0.0);
    let albedo = max(turn_hue(base, hue), vec3<f32>(0.0)) * value * (1.0 - look.colour.z * damp);

    var mesh: VertexOutput;
    mesh.position = in.position;
    mesh.world_position = in.world_position;
    mesh.world_normal = normalize(in.world_normal);
    @if(VERTEX_UVS_A)
    mesh.uv = in.world_position.xz;
    @if(VERTEX_UVS_B)
    mesh.uv_b = in.world_position.xz;
    @if(VERTEX_TANGENTS)
    mesh.world_tangent = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    @if(VERTEX_COLORS)
    mesh.color = vec4<f32>(1.0);
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    mesh.instance_index = in.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    mesh.visibility_range_dither = 0;

    var pbr_input = pbr_input_from_standard_material(mesh, is_front);
    pbr_input.material.base_color = vec4<f32>(albedo, 1.0) * pbr_input.material.base_color;
    var color = apply_pbr_lighting(pbr_input);
    color = vec4<f32>(max(color.rgb, albedo * look.light.x), color.a);
    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, color);
    return out;
}
