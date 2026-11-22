// The globe surfaces (spec §7.1, §9), one material per shader def:
//   PLATE_PIECES  rigid plate pieces of the deep-time globe
//   OCEAN_SHELL   the continuous ocean beneath them
//   RECENT_GLOBE  the fixed geographic globe of the ICE-6G_C path
//   COMPOSITE     the bridge crossfade of the recent and deep views
//
// The sampling rules mirror timefly_core::globe; validity arrives as CPU-decided flags.

import bevy_pbr::render::mesh_functions;
import bevy_pbr::render::view_transformations::position_world_to_clip;

struct GlobeParams {
    // World direction toward the sun.
    sun_dir: vec4<f32>,
    // Deep: (younger anomaly m, older anomaly m, older weight, cube size).
    // Recent: (0, 0, older weight, cube size). Composite: (0, 0, deep weight, 0).
    endpoint: vec4<f32>,
    // (radius km, exaggeration, ocean fallback m, ocean shell depth km).
    shape: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> params: GlobeParams;

@if(COMPOSITE) {
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var recent_view: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var deep_view: texture_2d<f32>;
}
@else {
@group(constants::MATERIAL_BIND_GROUP) @binding(1) var frame_a: texture_cube<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(2) var sampler_a: sampler;
@group(constants::MATERIAL_BIND_GROUP) @binding(3) var frame_b: texture_cube<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(4) var sampler_b: sampler;
}

// ---------------------------------------------------------------------------------------------
// Painterly palette (approved art direction B), authored in sRGB.

fn srgb(r: f32, g: f32, b: f32) -> vec3<f32> {
    let c = vec3<f32>(r, g, b) / 255.0;
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

fn land_albedo(elev_m: f32) -> vec3<f32> {
    let lowland = srgb(111.0, 143.0, 76.0);   // #6f8f4c
    let dry = srgb(226.0, 173.0, 85.0);       // #e2ad55
    let highland = srgb(199.0, 163.0, 111.0); // #c7a36f
    let peak = srgb(185.0, 147.0, 184.0);     // #b993b8
    var c = mix(lowland, dry, smoothstep(250.0, 1100.0, elev_m));
    c = mix(c, highland, smoothstep(1500.0, 2800.0, elev_m));
    c = mix(c, peak, smoothstep(3200.0, 5200.0, elev_m));
    return c;
}

fn water_albedo(elev_m: f32) -> vec3<f32> {
    let shallow = srgb(63.0, 143.0, 160.0); // #3f8fa0
    let deep = srgb(31.0, 63.0, 102.0);     // #1f3f66 at -4000 m
    let t = clamp(-elev_m / 4000.0, 0.0, 1.0);
    return mix(shallow, deep, sqrt(t));
}

fn ice_albedo() -> vec3<f32> {
    return srgb(238.0, 244.0, 250.0); // #eef4fa
}

fn hash3(p: vec3<f32>) -> f32 {
    return fract(sin(dot(p, vec3<f32>(127.1, 311.7, 74.7))) * 43758.5453);
}

fn value_noise(p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let n000 = hash3(i);
    let n100 = hash3(i + vec3<f32>(1.0, 0.0, 0.0));
    let n010 = hash3(i + vec3<f32>(0.0, 1.0, 0.0));
    let n110 = hash3(i + vec3<f32>(1.0, 1.0, 0.0));
    let n001 = hash3(i + vec3<f32>(0.0, 0.0, 1.0));
    let n101 = hash3(i + vec3<f32>(1.0, 0.0, 1.0));
    let n011 = hash3(i + vec3<f32>(0.0, 1.0, 1.0));
    let n111 = hash3(i + vec3<f32>(1.0, 1.0, 1.0));
    let x00 = mix(n000, n100, u.x);
    let x10 = mix(n010, n110, u.x);
    let x01 = mix(n001, n101, u.x);
    let x11 = mix(n011, n111, u.x);
    return mix(mix(x00, x10, u.y), mix(x01, x11, u.y), u.z);
}

// Low-amplitude brush noise in [-1, 1], anchored to the surface (plate-fixed or geographic).
fn brush(d: vec3<f32>) -> f32 {
    let broad = value_noise(d * 36.0);
    let streak = value_noise(d * vec3<f32>(150.0, 55.0, 150.0));
    return (broad * 0.6 + streak * 0.4) * 2.0 - 1.0;
}

// Soft ramped diffuse with hue-shifted shadows toward #5b3a52, plus brush noise.
fn painterly(albedo: vec3<f32>, n: vec3<f32>, sun: vec3<f32>, anchor: vec3<f32>) -> vec3<f32> {
    let ndl = dot(n, sun);
    let ramp = smoothstep(-0.25, 0.65, ndl);
    let shadow_tint = srgb(91.0, 58.0, 82.0); // #5b3a52
    let shadow = mix(albedo * 0.35, shadow_tint, 0.5) * mix(0.55, 1.0, smoothstep(-0.7, -0.1, ndl));
    var c = mix(shadow, albedo, ramp);
    c = c * (1.0 + 0.06 * brush(anchor));
    return c;
}

fn tangent_frame(p: vec3<f32>) -> mat2x3<f32> {
    var up = vec3<f32>(0.0, 1.0, 0.0);
    if abs(p.y) > 0.9 {
        up = vec3<f32>(1.0, 0.0, 0.0);
    }
    let t1 = normalize(cross(up, p));
    let t2 = cross(p, t1);
    return mat2x3<f32>(t1, t2);
}

// Normal of the displaced sphere from central differences of the radial lift `dh` (km) over a
// step of `eps` radians in each tangent direction.
fn normal_from_lifts(p: vec3<f32>, frame: mat2x3<f32>, dh1: f32, dh2: f32, eps: f32) -> vec3<f32> {
    let scale = 1.0 / (2.0 * eps * params.shape.x);
    return normalize(p - frame[0] * (dh1 * scale) - frame[1] * (dh2 * scale));
}

fn quat_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

fn lift_km(elev_m: f32) -> f32 {
    return max(elev_m, 0.0) * params.shape.y / 1000.0;
}

fn sun() -> vec3<f32> {
    return normalize(params.sun_dir.xyz);
}

// ---------------------------------------------------------------------------------------------
// Rigid plate pieces.

@if(PLATE_PIECES) {
struct Plate {
    current: vec4<f32>,
    source_a: vec4<f32>,
    source_b: vec4<f32>,
    flags: u32,
    tie_bias_km: f32,
    pad_a: u32,
    pad_b: u32,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(5) var<storage> plates: array<Plate>;
@group(constants::MATERIAL_BIND_GROUP) @binding(6) var<storage> list_flags: array<u32>;

const PLATE_CURRENT_VALID: u32 = 1u;
const PLATE_SOURCE0_VALID: u32 = 2u;
const PLATE_SOURCE1_VALID: u32 = 4u;
const LIST_VALID_NOW: u32 = 1u;
const LIST_VALID_SOURCE0: u32 = 2u;
const LIST_VALID_SOURCE1: u32 = 4u;

// timefly_core::globe::endpoint_available + endpoint_weights.
fn endpoint_weights(plate_flags: u32, list: u32) -> vec2<f32> {
    let a = (plate_flags & PLATE_SOURCE0_VALID) != 0u && (list & LIST_VALID_SOURCE0) != 0u;
    let b = (plate_flags & PLATE_SOURCE1_VALID) != 0u && (list & LIST_VALID_SOURCE1) != 0u;
    let w = params.endpoint.z;
    if a && b {
        return vec2<f32>(1.0 - w, w);
    }
    if a {
        return vec2<f32>(1.0, 0.0);
    }
    if b {
        return vec2<f32>(0.0, 1.0);
    }
    return vec2<f32>(0.0, 0.0);
}

// timefly_core::globe::blend_endpoints_m: each endpoint samples the plate-fixed direction
// rotated into its source frame, with its sea-level anomaly applied as elev - anomaly.
fn plate_elevation(plate: Plate, weights: vec2<f32>, p: vec3<f32>) -> f32 {
    var e = 0.0;
    if weights.x > 0.0 {
        let d = quat_rotate(plate.source_a, p);
        e += weights.x * (textureSampleLevel(frame_a, sampler_a, d, 0.0).r - params.endpoint.x);
    }
    if weights.y > 0.0 {
        let d = quat_rotate(plate.source_b, p);
        e += weights.y * (textureSampleLevel(frame_b, sampler_b, d, 0.0).r - params.endpoint.y);
    }
    return e;
}

struct PieceVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    // (plate index, cell interval list, corner interval list)
    @location(1) piece: vec3<u32>,
}

struct PieceOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) plate_dir: vec3<f32>,
    @location(1) @interpolate(flat) piece: vec2<u32>,
}

@vertex
fn vertex(v: PieceVertex) -> PieceOut {
    var out: PieceOut;
    out.plate_dir = v.position;
    out.piece = v.piece.xy;
    let plate = plates[v.piece.x];
    // timefly_core::globe::piece_visible. All three vertices share the flags, so the whole
    // triangle lands outside the clip volume and is dropped.
    let visible = (plate.flags & PLATE_CURRENT_VALID) != 0u
        && (list_flags[v.piece.y] & LIST_VALID_NOW) != 0u;
    if !visible {
        out.clip = vec4<f32>(2.0, 2.0, 2.0, 1.0);
        return out;
    }
    // Displacement uses the canonical corner's availability, so every duplicate of a corner
    // lifts the same distance.
    let w = endpoint_weights(plate.flags, list_flags[v.piece.z]);
    var lift = 0.0;
    if w.x + w.y > 0.0 {
        lift = lift_km(plate_elevation(plate, w, v.position));
    }
    let radius = params.shape.x + lift + plate.tie_bias_km;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = world_from_local * vec4<f32>(quat_rotate(plate.current, v.position) * radius, 1.0);
    out.clip = position_world_to_clip(world.xyz);
    return out;
}

@fragment
fn fragment(in: PieceOut) -> @location(0) vec4<f32> {
    let plate = plates[in.piece.x];
    let w = endpoint_weights(plate.flags, list_flags[in.piece.y]);
    if w.x + w.y <= 0.0 {
        discard;
    }
    let p = normalize(in.plate_dir);
    let elev = plate_elevation(plate, w, p);
    // Below sea level: not land; the ocean shell shows through.
    if elev <= 0.0 {
        discard;
    }
    // Central differences with the centre sample's endpoint decision (the nearest valid samples).
    let eps = 1.0 / max(params.endpoint.w, 1.0);
    let frame = tangent_frame(p);
    let dh1 = lift_km(plate_elevation(plate, w, normalize(p + frame[0] * eps)))
        - lift_km(plate_elevation(plate, w, normalize(p - frame[0] * eps)));
    let dh2 = lift_km(plate_elevation(plate, w, normalize(p + frame[1] * eps)))
        - lift_km(plate_elevation(plate, w, normalize(p - frame[1] * eps)));
    let n = quat_rotate(plate.current, normal_from_lifts(p, frame, dh1, dh2, eps));
    return vec4<f32>(painterly(land_albedo(elev), n, sun(), p), 1.0);
}
}

// ---------------------------------------------------------------------------------------------
// Ocean shell and recent globe share a plain position vertex.

@if(OCEAN_SHELL || RECENT_GLOBE) {
struct SphereVertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

struct SphereOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) dir: vec3<f32>,
}
}

@if(OCEAN_SHELL) {
// Geographic bathymetry from the source grids (published frames), blended like the endpoints.
fn shell_elevation(d: vec3<f32>) -> f32 {
    let w = params.endpoint.z;
    let a = textureSampleLevel(frame_a, sampler_a, d, 0.0).r - params.endpoint.x;
    let b = textureSampleLevel(frame_b, sampler_b, d, 0.0).r - params.endpoint.y;
    let e = mix(a, b, w);
    // Land-valued samples (a deep-time gap) fall back to open-ocean depth.
    return select(e, params.shape.z, e > 0.0);
}

@vertex
fn vertex(v: SphereVertex) -> SphereOut {
    var out: SphereOut;
    out.dir = v.position;
    let radius = params.shape.x - params.shape.w;
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = world_from_local * vec4<f32>(v.position * radius, 1.0);
    out.clip = position_world_to_clip(world.xyz);
    return out;
}

@fragment
fn fragment(in: SphereOut) -> @location(0) vec4<f32> {
    let d = normalize(in.dir);
    let color = painterly(water_albedo(shell_elevation(d)), d, sun(), d);
    return vec4<f32>(color, 1.0);
}
}

@if(RECENT_GLOBE) {
// (topography m, land fraction, ice fraction), coverage blended between the two snapshots.
fn recent_sample(d: vec3<f32>) -> vec3<f32> {
    let a = textureSampleLevel(frame_a, sampler_a, d, 0.0).rgb;
    let b = textureSampleLevel(frame_b, sampler_b, d, 0.0).rgb;
    return mix(a, b, params.endpoint.z);
}

// Positive topography on land is displaced; open ocean and floating ice stay at sea level.
fn recent_lift_km(s: vec3<f32>) -> f32 {
    return lift_km(s.x) * smoothstep(0.35, 0.65, s.y);
}

@vertex
fn vertex(v: SphereVertex) -> SphereOut {
    var out: SphereOut;
    out.dir = v.position;
    let radius = params.shape.x + recent_lift_km(recent_sample(v.position));
    let world_from_local = mesh_functions::get_world_from_local(v.instance_index);
    let world = world_from_local * vec4<f32>(v.position * radius, 1.0);
    out.clip = position_world_to_clip(world.xyz);
    return out;
}

@fragment
fn fragment(in: SphereOut) -> @location(0) vec4<f32> {
    let d = normalize(in.dir);
    let s = recent_sample(d);
    let land = smoothstep(0.4, 0.6, s.y);
    let ice = smoothstep(0.4, 0.6, s.z);
    let water = water_albedo(min(s.x, -1.0));
    var albedo = mix(water, land_albedo(max(s.x, 0.0)), land);
    // Grounded ice shades the elevated land; floating ice overlays water at sea level.
    albedo = mix(albedo, ice_albedo(), ice * land);
    albedo = mix(albedo, mix(ice_albedo(), water, 0.2), ice * (1.0 - land));
    let eps = 1.0 / max(params.endpoint.w, 1.0);
    let frame = tangent_frame(d);
    let dh1 = recent_lift_km(recent_sample(normalize(d + frame[0] * eps)))
        - recent_lift_km(recent_sample(normalize(d - frame[0] * eps)));
    let dh2 = recent_lift_km(recent_sample(normalize(d + frame[1] * eps)))
        - recent_lift_km(recent_sample(normalize(d - frame[1] * eps)));
    let n = normal_from_lifts(d, frame, dh1, dh2, eps);
    return vec4<f32>(painterly(albedo, n, sun(), d), 1.0);
}
}

// ---------------------------------------------------------------------------------------------
// Bridge crossfade: both completed views, same camera, mixed in linear colour.

@if(COMPOSITE) {
struct CompositeVertex {
    @location(0) position: vec3<f32>,
}

struct CompositeOut {
    @builtin(position) clip: vec4<f32>,
}

@vertex
fn vertex(v: CompositeVertex) -> CompositeOut {
    var out: CompositeOut;
    // Reversed Z: just in front of the far plane, so the atmosphere still draws over it.
    out.clip = vec4<f32>(v.position.xy, 1e-7, 1.0);
    return out;
}

@fragment
fn fragment(in: CompositeOut) -> @location(0) vec4<f32> {
    let px = vec2<i32>(in.clip.xy);
    let recent = textureLoad(recent_view, px, 0).rgb;
    let deep = textureLoad(deep_view, px, 0).rgb;
    return vec4<f32>(mix(recent, deep, params.endpoint.z), 1.0);
}
}
