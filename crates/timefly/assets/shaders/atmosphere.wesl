// Painterly atmosphere rim (spec §7.1, §9): a shell above the highest exaggerated terrain, its
// rim coloured from day (#9cc3e6) to terminator (#f4c58d) and faded on the night side.

import bevy_pbr::render::mesh_functions;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::mesh_view_bindings::view;

struct GlobeParams {
    sun_dir: vec4<f32>,
    endpoint: vec4<f32>,
    // (radius km, exaggeration, ocean fallback m, ocean shell depth km); the shell radius is
    // radius * endpoint.w.
    shape: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> params: GlobeParams;

fn srgb(r: f32, g: f32, b: f32) -> vec3<f32> {
    let c = vec3<f32>(r, g, b) / 255.0;
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

struct ShellVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

struct ShellOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
}

@vertex
fn vertex(v: ShellVertex) -> ShellOut {
    var out: ShellOut;
    let radius = params.shape.x * params.endpoint.w;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = world_from_local * vec4<f32>(v.position * radius, 1.0);
    out.world = world.xyz;
    out.clip = position_world_to_clip(world.xyz);
    return out;
}

@fragment
fn fragment(in: ShellOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world);
    let v = normalize(view.world_position - in.world);
    let facing = clamp(dot(n, v), 0.0, 1.0);
    let rim = pow(1.0 - facing, 3.5);
    let day = dot(n, normalize(params.sun_dir.xyz));
    let terminator = srgb(244.0, 197.0, 141.0); // #f4c58d
    let daylight = srgb(156.0, 195.0, 230.0);   // #9cc3e6
    let color = mix(terminator, daylight, smoothstep(0.0, 0.45, day));
    let lit = smoothstep(-0.3, 0.12, day);
    let alpha = clamp(rim * 1.1, 0.0, 0.9) * lit;
    return vec4<f32>(color, alpha);
}
