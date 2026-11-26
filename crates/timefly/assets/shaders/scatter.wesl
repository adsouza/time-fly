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

import bevy_pbr::render::mesh_functions::{get_world_from_local, get_tag, mesh_normal_local_to_world};
import bevy_pbr::render::mesh_view_bindings::view;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing};
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

// `ScatterLook` in scatter.rs.
struct ScatterLook {
    // xy: the palette texel of the kind's colour; zw: unused.
    texel: vec4<u32>,
    // x: hue jitter (± radians); y: value jitter (± share); z: wet darkening (share); w: the wet
    // band above the waterline (share of the height).
    colour: vec4<f32>,
    // x: the water's constant pull (m); y: its share of the view distance; z: Earth radius (m).
    pull: vec4<f32>,
    // x: the least light (exposure applied) a surface shows: the terrain ramp's shade floor.
    light: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var palette: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var<uniform> look: ScatterLook;

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
    let ray = world - camera;
    let distance = length(ray);
    let above = select(0.0, 1.0, wet > 0.0 && height > wet);
    let pull = min(look.pull.x + look.pull.y * distance, 0.5 * distance) * above;
    let pulled = world - ray * (pull / max(distance, 1e-3));
    var out: ScatterVertexOutput;
    out.position = position_world_to_clip(pulled);
    out.world_position = vec4<f32>(world, 1.0);
    out.world_normal = mesh_normal_local_to_world(in.normal, in.instance_index);
    out.height = height;
    out.instance_index = in.instance_index;
    return out;
}

// `colour` turned by `angle` radians about the grey axis (Rodrigues' rotation).
fn turn_hue(colour: vec3<f32>, angle: f32) -> vec3<f32> {
    let k = vec3<f32>(0.57735027);
    let c = cos(angle);
    let s = sin(angle);
    return colour * c + cross(k, colour) * s + k * dot(k, colour) * (1.0 - c);
}

@fragment
fn fragment(in: ScatterVertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
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
