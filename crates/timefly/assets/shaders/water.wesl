// The region's sea (M2 design §6.5): a plane at sea level, curved like the terrain, lit through
// Bevy's StandardMaterial PBR path (sun, terrain shadows, fog, tonemapping).
//
// The mesh is a polar grid in metres from the camera. The vertex stage adds the camera's xz and
// the curvature drop d² / 2R (as cdlod_vertex does), then pulls the vertex toward the camera along
// its view ray: depth changes, the pixel does not, so the sea wins depth ties with a terrain mesh
// lying near y = 0. The fragment stage reads the sea-floor depth from the terrain's own height
// textures (blended_height, the WESL copy of blended_height_m) and discards fragments over land,
// so the shoreline is the height field's zero contour.

import package::terrain_cdlod::{TerrainBuffer, blended_height};
import package::noise::gradient_noise3;
import bevy_pbr::render::mesh_view_bindings::{view, globals};
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing};
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

struct WaterLook {
    // rgb: shallow colour; w: depth scale (m).
    shallow: vec4<f32>,
    // rgb: deep colour; w: foam depth (m).
    deep: vec4<f32>,
    // rgb: sky zenith; w: Schlick f0.
    sky_zenith: vec4<f32>,
    // rgb: sky horizon; w: depth pull (fraction of the view distance).
    sky_horizon: vec4<f32>,
    // x: constant depth pull (m).
    pull: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> terrain: TerrainBuffer;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var inner_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var apron_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var<uniform> look: WaterLook;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

struct WaterVertexOutput {
    @builtin(position) position: vec4<f32>,
    // The true surface point (before the depth pull).
    @location(0) world_position: vec4<f32>,
    @location(1) @interpolate(flat) instance_index: u32,
}

@vertex
fn vertex(in: Vertex) -> WaterVertexOutput {
    let params = terrain.params;
    let rel = in.position.xz;
    let xz = params.camera.xz + rel;
    let drop = dot(rel, rel) / (2.0 * params.lattice.z);
    let surface = vec3<f32>(xz.x, -drop, xz.y);
    let ray = surface - view.world_position;
    let distance = length(ray);
    let pull = min(look.pull.x + look.sky_horizon.w * distance, 0.5 * distance);
    let pulled = surface - ray * (pull / max(distance, 1e-3));
    var out: WaterVertexOutput;
    out.position = position_world_to_clip(pulled);
    out.world_position = vec4<f32>(surface, 1.0);
    out.instance_index = in.instance_index;
    return out;
}

// `water_tint` in water.rs: shallow to deep by 1 − e^(−depth / scale).
fn water_tint(depth_m: f32) -> vec3<f32> {
    let t = 1.0 - exp(-max(depth_m, 0.0) / look.shallow.w);
    return mix(look.shallow.rgb, look.deep.rgb, t);
}

// Gradient noise at a world-space scale: integer cell plus fraction, as noise.wesl expects.
// Region-local metres stay well inside f32's exact range for these quotients.
fn brush(xz: vec2<f32>, scale_m: f32, z: f32, seed: u32) -> f32 {
    let q = xz / scale_m;
    let c = floor(q);
    let zc = floor(z);
    return gradient_noise3(vec3<i32>(i32(c.x), i32(c.y), i32(zc)), vec3<f32>(q - c, z - zc), seed);
}

// Fades a noise octave out as it shrinks below a couple of pixels.
fn octave_weight(footprint_m: f32, scale_m: f32) -> f32 {
    return 1.0 - smoothstep(0.25, 0.6, footprint_m / scale_m);
}

@fragment
fn fragment(in: WaterVertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let params = terrain.params;
    let xz = in.world_position.xz;
    // Derivatives first, while every invocation of the quad is still running.
    let footprint = length(fwidth(xz));
    if max(abs(xz.x), abs(xz.y)) > params.apron.x * 0.5 {
        discard;
    }
    let depth = -blended_height(inner_height, apron_height, params, xz);
    if depth <= 0.0 {
        discard;
    }

    // Normal: the curvature tilt (as the terrain's), plus a slow, faint ripple.
    let rel = xz - params.camera.xz;
    let t = globals.time;
    let w30 = octave_weight(footprint, 30.0);
    let w200 = octave_weight(footprint, 200.0);
    let ripple = vec2<f32>(
        brush(xz, 200.0, t * 0.05, 71u) * w200 + 0.5 * brush(xz, 30.0, t * 0.2, 73u) * w30,
        brush(xz, 200.0, t * 0.05, 79u) * w200 + 0.5 * brush(xz, 30.0, t * 0.2, 83u) * w30,
    ) * 0.04;
    let normal = normalize(vec3<f32>(rel.x / params.lattice.z + ripple.x, 1.0, rel.y / params.lattice.z + ripple.y));

    // Foam: a band where the floor is less than FOAM_DEPTH_M deep, broken up by noise, thinned
    // to a softer average where its texture would alias.
    let band = 1.0 - smoothstep(0.3, look.deep.w, depth);
    let w25 = octave_weight(footprint, 25.0);
    let w6 = octave_weight(footprint, 6.0);
    let n = brush(xz, 25.0, t * 0.15, 89u) * w25 + 0.6 * brush(xz, 6.0, t * 0.3, 97u) * w6;
    let foam = smoothstep(0.45, 0.6, band * (0.8 + n)) * mix(0.4, 1.0, w25);

    var mesh: VertexOutput;
    mesh.position = in.position;
    mesh.world_position = in.world_position;
    mesh.world_normal = normal;
    @if(VERTEX_UVS_A)
    mesh.uv = xz;
    @if(VERTEX_UVS_B)
    mesh.uv_b = xz;
    @if(VERTEX_TANGENTS)
    mesh.world_tangent = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    @if(VERTEX_COLORS)
    mesh.color = vec4<f32>(1.0);
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    mesh.instance_index = in.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    mesh.visibility_range_dither = 0;

    var pbr_input = pbr_input_from_standard_material(mesh, is_front);
    let albedo = mix(water_tint(depth), vec3<f32>(0.85, 0.88, 0.86), foam);
    pbr_input.material.base_color = vec4<f32>(albedo, 1.0) * pbr_input.material.base_color;
    pbr_input.material.perceptual_roughness = mix(pbr_input.material.perceptual_roughness, 0.9, foam);
    var color = apply_pbr_lighting(pbr_input);

    // Schlick Fresnel toward the sky along the reflected ray (no foam reflection).
    let v = normalize(view.world_position - in.world_position.xyz);
    let cos_theta = clamp(dot(normal, v), 0.0, 1.0);
    let f0 = look.sky_zenith.w;
    let fresnel = f0 + (1.0 - f0) * pow(1.0 - cos_theta, 5.0);
    let r = reflect(-v, normal);
    let sky = mix(look.sky_horizon.rgb, look.sky_zenith.rgb, 1.0 - pow(1.0 - clamp(r.y, 0.0, 1.0), 3.0));
    color = vec4<f32>(mix(color.rgb, sky, fresnel * (1.0 - foam)), 1.0);

    var out: FragmentOutput;
    out.color = main_pass_post_lighting_processing(pbr_input, color);
    return out;
}
