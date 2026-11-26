// The region's gradient sky dome (M2 design §6.4, spec §9): a unit sphere around the camera,
// drawn at infinite depth so every terrain fragment covers it.
//
// Colour: the horizon tint at the geometric horizon (which dips below level by sqrt(2h / R) as
// the camera climbs), easing to the zenith tint overhead, plus a warm glow and a soft disc toward
// the sun. The colours are in the same linear, exposure-applied units as the lit terrain and the
// fog, and tonemap the same way, so the sky's horizon meets the fogged far terrain.

import bevy_pbr::render::mesh_view_bindings::view;
import bevy_pbr::render::view_transformations::position_world_to_clip;
@if(TONEMAP_IN_SHADER) import bevy_core_pipeline::tonemapping::tone_mapping;

struct SkyParams {
    zenith: vec4<f32>,
    horizon: vec4<f32>,
    // xyz: direction toward the sun; w: glow strength.
    sun: vec4<f32>,
    sun_color: vec4<f32>,
    // Below the horizon: rgb the fog's colour, w its sun glow.
    fog: vec4<f32>,
    // Below the horizon: rgb the sun's colour times its illuminance, w the fog's sun exponent.
    fog_sun: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(0) var<uniform> sky: SkyParams;

const EARTH_RADIUS_M: f32 = 6371000.0;

struct SkyVertex {
    @location(0) position: vec3<f32>,
}

struct SkyOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) dir: vec3<f32>,
}

@vertex
fn vertex(v: SkyVertex) -> SkyOut {
    var out: SkyOut;
    // A direction, placed a kilometre out and then pushed to the far plane (reversed-Z: 0).
    let world = view.world_position + v.position * 1000.0;
    let clip = position_world_to_clip(world);
    out.clip = vec4<f32>(clip.xy, 0.0, clip.w);
    out.dir = v.position;
    return out;
}

@fragment
fn fragment(in: SkyOut) -> @location(0) vec4<f32> {
    let dir = normalize(in.dir);
    // The geometric horizon's elevation (radians, negative) for the camera's height.
    let height = max(view.world_position.y, 1.0);
    let dip = -sqrt(2.0 * height / EARTH_RADIUS_M);
    let up = clamp((dir.y - dip) / (1.0 - dip), 0.0, 1.0);
    // Ease out of the horizon quickly, so the haze is a band, not half the sky.
    let t = 1.0 - pow(1.0 - up, 3.0);
    var color = mix(sky.horizon.rgb, sky.zenith.rgb, t);
    // The sun: a broad warm glow and a soft disc.
    let s = max(dot(dir, normalize(sky.sun.xyz)), 0.0);
    let glow = sky.sun.w * (0.6 * pow(s, 12.0) + 0.4 * pow(s, 3.0) * (1.0 - t));
    let disc = smoothstep(0.9995, 0.99985, s) * 6.0;
    color = color + sky.sun_color.rgb * (glow + disc);
    // Below the horizon (beyond the region's apron): fully fogged ground, as Bevy's fog colours
    // the terrain there (its colour plus the sun's scattering), so the apron's edge, seen from
    // high up, melts into it.
    let scattering = pow(s, sky.fog_sun.w) * sky.fog_sun.rgb * view.exposure;
    let fogged = sky.fog.rgb + scattering * sky.sun_color.rgb * sky.fog.w;
    color = select(color, fogged, dir.y < dip);
    var out = vec4<f32>(color, 1.0);
    @if(TONEMAP_IN_SHADER)
    out = tone_mapping(out, view.color_grading);
    return out;
}
