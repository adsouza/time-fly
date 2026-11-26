// The region's water (M2 design §6.5; rivers and lakes design §4.4): the sea, the lakes and the
// rivers, drawn from the fields' water textures, curved like the terrain, lit through Bevy's
// StandardMaterial PBR path (sun, terrain shadows, fog, tonemapping).
//
// The mesh is a polar grid in metres from the camera. The vertex stage puts it at the drawn
// surface (the water level, or the ground where that is higher, `blended_surface_m`), adds the
// camera's xz and the curvature drop d² / 2R (as cdlod_vertex does), then pulls the vertex toward
// the camera along its view ray: depth changes, the pixel does not, so the water wins depth ties
// with the terrain at its shore. The fragment stage reads the water at its surface point
// (blended_water, the WESL copy of timefly_core::terrain::blended_water) and discards it where
// the water's thickness over the ground is not positive, or outside a river's true width (its
// ribbon below 0), so the shoreline is the fields' own zero contour at rest.
//
// Rivers (ribbon below RIBBON_NONE_M): their ripples and white water scroll downstream along
// the flow, two phases half a period apart (`flow_phase_weights`), so the water moves and never
// pulses; rapids (`rapid_from_speed`) scroll faster, ripple harder and break white. Their tint
// is silty brown when slow, clear when fast (`river_tint`). Lakes and the sea (ribbon at least
// RIBBON_NONE_M) keep the swell, the swash and the shore's foam below.
//
// Waves (art pass §5.4, waves.wesl):
// - Swell: below 1 km the vertex stage raises and lowers the surface by three directional
//   components around the region's wind. Each fades where the mesh has too few vertices per
//   wavelength, all fade with the camera's height and, near the shore, with the floor's depth.
//   The fragment stage tilts the normal by the same waves at every height, plus three slow,
//   long normal-only bands, so moving light shows on the sea from high up.
// - Swash: in the shallows the water level rises and falls (`swash_level_m`), so the edge runs
//   a fixed reach up the beach and back, with a foam line riding it. The vertex stage pulls the
//   shallows toward the camera by the swash's height too, so water laid over the beach still
//   wins depth ties with the terrain there.

import package::terrain_cdlod::{
    TerrainBuffer, TerrainParams, WaterSample, blended_water, fog_position, texel_step,
};
import package::noise::gradient_noise3;
import package::waves::{
    TAU, SWASH_MAX_AMPLITUDE_M, SWASH_PROBE_M, swash_amplitude_m, swash_level_m, swash_reach_factor,
    wave_phase, swell_spacing_weight,
};
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
    // x: constant depth pull (m); y, z: camera heights (m) where the shallow tint starts and
    // finishes calming.
    pull: vec4<f32>,
    // rgb: the shallow colour seen from altitude.
    shallow_far: vec4<f32>,
    // x: swell amplitude per component (m); y, z: camera heights (m) where the displacement
    // starts and finishes fading out; w: vertices per wavelength for full displacement.
    swell: vec4<f32>,
    // x: swash period (s); y: the swash phase's noise scale (m); z: the depth (m) below which
    // the swash moves the water level. The reach and the cap are waves.wesl constants.
    swash: vec4<f32>,
    // xy: the region-local wind direction (the way it blows).
    wind: vec4<f32>,
    // The mesh's vertex spacing: x: ring step near the camera (m); y: ring growth with radius;
    // z: segments around.
    mesh: vec4<f32>,
    // x: each band's largest slope; y: the bands' patch noise scale (m); z, w: floor depths (m)
    // over which every wave comes in from the shore.
    bands: vec4<f32>,
    // The rivers' flow map: x: period (s); y: ripple scale (m); z: white water's foam
    // strength; w: the rapids' extra scroll.
    flow: vec4<f32>,
    // rgb: a fast river's clear colour; w: the speed (m/s) at or below which it is fully silty.
    river_clear: vec4<f32>,
    // rgb: a slow river's silty colour; w: the speed (m/s) from which it is clear.
    river_silty: vec4<f32>,
    // Three displaced swell components, then three normal-only bands: (dir.x, dir.z,
    // wavelength m, period s), as `wave_phase` takes them.
    waves: array<vec4<f32>, 6>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> terrain: TerrainBuffer;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var inner_water: texture_2d<u32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var apron_water: texture_2d<u32>;
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

// `RIBBON_NONE_M` in timefly_core::hydrology::params: the ribbon of water that is not a river.
const RIBBON_NONE_M: f32 = 1000.0;
// `RAPID_SPEED_BASE_MS`, `RAPID_SPEED_GAIN_MS` in timefly_core::hydrology::params.
const RAPID_SPEED_BASE_MPS: f32 = 3.0;
const RAPID_SPEED_GAIN_MPS: f32 = 3.0;
// `RAPID_NORMAL_GAIN` in water.rs: rapids ripple this much harder.
const RAPID_NORMAL_GAIN: f32 = 3.0;
// `WHITE_WATER` in water.rs, linear.
const WHITE_WATER: vec3<f32> = vec3<f32>(0.8276, 0.8689, 0.8481);
// Flow speeds (m/s) over which water turns from standing to river: rivers run at 0.3 m/s or
// more (LOWLAND_SPEED_MS), standing water has none, and where a river meets a lake the flow
// fades to 0 exactly where the ribbon reaches RIBBON_NONE_M, so the look changes without a seam.
const RIVER_SPEED_MPS: vec2<f32> = vec2<f32>(0.05, 0.3);
// A river's ripples' largest slope (before the rapids' gain).
const RIVER_RIPPLE_SLOPE: f32 = 0.06;

// The water at xz (`blended_water` over this material's water textures).
fn water_at(params: TerrainParams, xz: vec2<f32>) -> WaterSample {
    return blended_water(inner_water, apron_water, params, xz);
}

// The floor's elevation gradient under standing water (uphill, rise over run) at xz: minus the
// gradient of the water's thickness (its level is flat), by a central difference over one texel
// of the dominant field, so it varies smoothly across texel edges.
fn floor_gradient(params: TerrainParams, xz: vec2<f32>) -> vec2<f32> {
    let e = texel_step(params, xz);
    let dx = water_at(params, xz + vec2<f32>(e, 0.0)).thickness
        - water_at(params, xz - vec2<f32>(e, 0.0)).thickness;
    let dz = water_at(params, xz + vec2<f32>(0.0, e)).thickness
        - water_at(params, xz - vec2<f32>(0.0, e)).thickness;
    return -vec2<f32>(dx, dz) / (2.0 * e);
}

// `rapid_from_speed` in timefly_core: how much of a rapid a river flowing at this speed is.
fn rapid_from_speed(speed_mps: f32) -> f32 {
    return clamp((speed_mps - RAPID_SPEED_BASE_MPS) / RAPID_SPEED_GAIN_MPS, 0.0, 1.0);
}

// `flow_phase_weights` in water.rs: the two flow phases' triangle weights, half a period apart,
// each 0 where its phase restarts.
fn flow_phase_weights(t_s: f32, jitter: f32) -> vec2<f32> {
    let p = fract(t_s / look.flow.x + jitter);
    let q = fract(p + 0.5);
    return vec2<f32>(1.0 - abs(2.0 * p - 1.0), 1.0 - abs(2.0 * q - 1.0));
}

// `river_tint` in water.rs: clear to silty by the speed; the clear part darkens with depth.
fn river_tint(depth_m: f32, speed_mps: f32) -> vec3<f32> {
    let silt = 1.0 - smoothstep(look.river_clear.w, look.river_silty.w, speed_mps);
    let base = mix(look.river_clear.rgb, look.river_silty.rgb, silt);
    let t = 1.0 - exp(-max(depth_m, 0.0) / look.shallow.w);
    return mix(base, look.deep.rgb, t * (1.0 - silt));
}

// Land vertices this far above the sea (m) still carry the swash's depth lift, so the
// triangles that straddle the shore interpolate it.
const SWASH_LIFT_LAND_M: f32 = 25.0;

// `WATER_HORIZON_MARGIN` in water.rs: the mesh reaches this fraction past the horizon.
const WATER_HORIZON_MARGIN: f32 = 0.05;

@vertex
fn vertex(in: Vertex) -> WaterVertexOutput {
    let params = terrain.params;
    // `water_radius_m`: from high up the horizon is farther than the mesh's radius
    // (`look.mesh.w`), and the mesh stretches past it, so its rim never shows.
    let h = max(params.camera.y, 0.0);
    let reach = max(look.mesh.w, sqrt(h * (2.0 * params.lattice.z + h)) * (1.0 + WATER_HORIZON_MARGIN));
    let rel = in.position.xz * (reach / look.mesh.w);
    let xz = params.camera.xz + rel;
    let drop = dot(rel, rel) / (2.0 * params.lattice.z);
    // The drawn surface: the water's level, or the ground where that is higher. Dry vertices sit
    // on the ground rather than at their texels' level (NO_WATER_DROP_M below it), so between a
    // river's vertex and a dry one the mesh never dips under the river's banks.
    let w = water_at(params, xz);
    let level = w.level - min(w.thickness, 0.0);
    // Low down, on standing water: the swell's height, and the swash's amplitude for the depth
    // lift below. Higher up, and on rivers, neither shows.
    // `water_vertex_spacing_m`: the ring step or the arc between segments.
    let r = length(rel);
    let spacing = max(max(look.mesh.x, r * look.mesh.y), TAU * r / look.mesh.z);
    var swell = 0.0;
    var lift = 0.0;
    let height_fade = 1.0 - smoothstep(look.swell.y, look.swell.z, params.camera.y - w.level);
    if height_fade > 0.0 && w.ribbon >= RIBBON_NONE_M {
        let depth = w.thickness;
        var sum = 0.0;
        for (var i = 0u; i < 3u; i++) {
            let w = look.waves[i];
            sum += swell_spacing_weight(spacing, w.z, look.swell.w) * sin(wave_phase(w, xz, globals.time));
        }
        swell = look.swell.x * height_fade * smoothstep(look.bands.z, look.bands.w, depth) * sum;
        if depth > -SWASH_LIFT_LAND_M && depth < look.swash.z {
            lift = height_fade * swash_amplitude_m(length(floor_gradient(params, xz)));
        }
    }
    let surface = vec3<f32>(xz.x, level + swell - drop, xz.y);
    let ray = surface - view.world_position;
    let distance = length(ray);
    // The pull of water `lift` metres higher (along the ray, `lift / sin` of its slope), so water
    // the swash lays over the beach still wins depth ties with the terrain beneath it.
    let lifted = lift * distance / max(abs(ray.y), 1e-3);
    let pull = min(look.pull.x + look.sky_horizon.w * distance + lifted, 0.5 * distance);
    let pulled = surface - ray * (pull / max(distance, 1e-3));
    var out: WaterVertexOutput;
    out.position = position_world_to_clip(pulled);
    out.world_position = vec4<f32>(surface, 1.0);
    out.instance_index = in.instance_index;
    return out;
}

// `water_tint` in water.rs: shallow to deep by 1 − e^(−depth / scale). From altitude the shallow
// colour calms toward `shallow_far` (no turquoise rim); up close it is the clear-water one.
fn water_tint(depth_m: f32) -> vec3<f32> {
    let t = 1.0 - exp(-max(depth_m, 0.0) / look.shallow.w);
    let calm = smoothstep(look.pull.y, look.pull.z, view.world_position.y);
    return mix(mix(look.shallow.rgb, look.shallow_far.rgb, calm), look.deep.rgb, t);
}

// Gradient noise at a world-space scale: integer cell plus fraction, as noise.wesl expects.
// Region-local metres stay well inside f32's exact range for these quotients.
fn brush(xz: vec2<f32>, scale_m: f32, z: f32, seed: u32) -> f32 {
    let q = xz / scale_m;
    let c = floor(q);
    let zc = floor(z);
    return gradient_noise3(vec3<i32>(i32(c.x), i32(c.y), i32(zc)), vec3<f32>(q - c, z - zc), seed);
}

// The thinnest water (m) the shore's foam band ends at (FOAM_MIN_DEPTH_M in water.rs).
const FOAM_MIN_DEPTH_M: f32 = 0.02;
// The swash's foam line: water thinner than this fraction of the amplitude, and never thinner
// than SWASH_LINE_MIN_M (SWASH_LINE_FRACTION, SWASH_LINE_MIN_M in water.rs).
const SWASH_LINE_FRACTION: f32 = 0.35;
const SWASH_LINE_MIN_M: f32 = 0.03;

// `octave_weight` in water.rs: fades a pattern out as it shrinks below a couple of pixels.
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
    let w = water_at(params, xz);
    let depth = w.thickness;
    // No swash lifts the water over ground this high; and outside a river's true width (drawn
    // at least about a pixel wide, so a thin river stays a line from afar) there is no water.
    if depth <= -SWASH_MAX_AMPLITUDE_M || w.ribbon + 0.5 * footprint < 0.0 {
        discard;
    }
    let river = w.ribbon < RIBBON_NONE_M;
    // How much of the river look applies: all of it on a river, none on standing water, ramped
    // across the cell where a river meets a lake (`RIVER_SPEED_MPS`).
    let river_w = select(0.0, smoothstep(RIVER_SPEED_MPS.x, RIVER_SPEED_MPS.y, length(w.flow)), river);
    let t = globals.time;

    // Swash: in the shallows the level rises and falls by the reach times the floor's slope,
    // with a phase that drifts along the coast. It applies only where the shore lies within
    // SWASH_PROBE_M along the slope (`swash_reach_factor`), and eases in from SWASH_DEPTH_M to
    // half that, so the water's thickness has no seam there.
    // The floor's slope is read wherever the swash or the foam band can reach.
    var floor_slope_m = 0.0;
    var amplitude = 0.0;
    var level = 0.0;
    if river_w < 1.0 && depth < max(look.swash.z, look.deep.w) {
        let gradient = floor_gradient(params, xz);
        floor_slope_m = length(gradient);
        amplitude = swash_amplitude_m(floor_slope_m);
        if depth < look.swash.z {
            // Toward the shore: downhill from land, uphill from the sea.
            let toward = gradient / max(floor_slope_m, 1e-9) * select(-1.0, 1.0, depth > 0.0);
            let probe = water_at(params, xz + toward * SWASH_PROBE_M).thickness;
            let phase = 2.0 * TAU * brush(xz, look.swash.y, 0.0, 101u);
            let ease = 1.0 - smoothstep(0.5 * look.swash.z, look.swash.z, depth);
            level = swash_level_m(floor_slope_m, t, look.swash.x, phase) * ease
                * swash_reach_factor(depth, probe, amplitude) * (1.0 - river_w);
        }
    }
    // The water's thickness over the floor; land wherever the floor is above the water level.
    let water = depth + level;
    if water <= 0.0 {
        discard;
    }

    // Normal: the curvature tilt (as the terrain's), a slow, faint ripple, and the waves: the
    // swell (the slope of the displaced height) and the long bands, calmed toward the shore and
    // faded where a wave shrinks below a couple of pixels.
    let rel = xz - params.camera.xz;
    let w30 = octave_weight(footprint, 30.0);
    let w200 = octave_weight(footprint, 200.0);
    let ripple = vec2<f32>(
        brush(xz, 200.0, t * 0.05, 71u) * w200 + 0.5 * brush(xz, 30.0, t * 0.2, 73u) * w30,
        brush(xz, 200.0, t * 0.05, 79u) * w200 + 0.5 * brush(xz, 30.0, t * 0.2, 83u) * w30,
    ) * 0.04;
    var wave_slope = vec2<f32>(0.0);
    for (var i = 0u; i < 3u; i++) {
        let w = look.waves[i];
        let a = look.swell.x * TAU / w.z * octave_weight(footprint, w.z);
        wave_slope += a * cos(wave_phase(w, xz, t)) * w.xy;
    }
    let band_patch = clamp(0.6 + brush(xz, look.bands.y, 0.0, 103u), 0.0, 1.0);
    for (var i = 3u; i < 6u; i++) {
        let w = look.waves[i];
        let a = look.bands.x * band_patch * octave_weight(footprint, w.z);
        wave_slope += a * cos(wave_phase(w, xz, t)) * w.xy;
    }
    wave_slope *= smoothstep(look.bands.z, look.bands.w, depth);

    // Rivers: ripples and white water carried downstream by the flow (region +x, +z), sampled
    // at xz - flow × phase × period for each of the two phases, faster in rapids. The phase is
    // jittered over 400 m so the restarts don't sweep the river in step.
    var flow_tilt = vec2<f32>(0.0);
    var white = 0.0;
    if river {
        let rapid = rapid_from_speed(length(w.flow));
        let jitter = brush(xz, 400.0, 0.0, 139u);
        let weights = flow_phase_weights(t, jitter);
        let p = fract(t / look.flow.x + jitter);
        let q = fract(p + 0.5);
        let reach = w.flow * look.flow.x * (1.0 + look.flow.w * rapid);
        let x0 = xz - reach * p;
        let x1 = xz - reach * q;
        let s = look.flow.y;
        let ws = octave_weight(footprint, s);
        let wl = octave_weight(footprint, 4.0 * s);
        let r0 = vec2<f32>(
            brush(x0, s, 0.0, 151u) * ws + 0.6 * brush(x0, 4.0 * s, 0.0, 157u) * wl,
            brush(x0, s, 0.0, 163u) * ws + 0.6 * brush(x0, 4.0 * s, 0.0, 167u) * wl,
        );
        let r1 = vec2<f32>(
            brush(x1, s, 0.0, 151u) * ws + 0.6 * brush(x1, 4.0 * s, 0.0, 157u) * wl,
            brush(x1, s, 0.0, 163u) * ws + 0.6 * brush(x1, 4.0 * s, 0.0, 167u) * wl,
        );
        flow_tilt = (weights.x * r0 + weights.y * r1) * RIVER_RIPPLE_SLOPE
            * (1.0 + RAPID_NORMAL_GAIN * rapid) * river_w;
        // White water: bright, broken foam, churning as it goes; where its noise can't be
        // resolved it fades to its mean, so rapids still read white from afar.
        let wf = octave_weight(footprint, 2.0 * s);
        let wc = octave_weight(footprint, 8.0 * s);
        let n0 = brush(x0, 2.0 * s, t * 0.5, 173u) * wf + 0.7 * brush(x0, 8.0 * s, t * 0.2, 179u) * wc;
        let n1 = brush(x1, 2.0 * s, t * 0.5, 173u) * wf + 0.7 * brush(x1, 8.0 * s, t * 0.2, 179u) * wc;
        let n = weights.x * n0 + weights.y * n1;
        let broken = smoothstep(0.15 - 0.5 * rapid, 0.35 - 0.3 * rapid, n);
        white = look.flow.z * rapid * broken * river_w;
    }
    let lake_tilt = (ripple - wave_slope) * (1.0 - river_w);
    let tilt = rel / params.lattice.z + lake_tilt + flow_tilt;
    let normal = normalize(vec3<f32>(tilt.x, 1.0, tilt.y));

    // Foam: a band where the water is less than FOAM_DEPTH_M thick, kept within FOAM_WIDTH_M of
    // the edge on gentle coasts (`foam_band_depth_m`), broken up by noise, faded to nothing
    // where its texture can't be resolved; and a line riding the swash's moving edge, where the
    // water is thinnest (`swash_line_width_m`: about a third of the reach wide on a gentle
    // beach), faded before it narrows below a couple of pixels on steep ones.
    let band_depth = clamp(look.swash.w * floor_slope_m, FOAM_MIN_DEPTH_M, look.deep.w);
    let band = 1.0 - smoothstep(0.075 * band_depth, band_depth, water);
    let w25 = octave_weight(footprint, 25.0);
    let w6 = octave_weight(footprint, 6.0);
    let n = brush(xz, 25.0, t * 0.15, 89u) * w25 + 0.6 * brush(xz, 6.0, t * 0.3, 97u) * w6;
    let line_thickness = max(SWASH_LINE_FRACTION * amplitude, SWASH_LINE_MIN_M);
    let line_width = line_thickness / max(floor_slope_m, 1e-4);
    let line = (1.0 - smoothstep(0.0, line_thickness, water)) * octave_weight(footprint, line_width);
    let shore_foam = max(
        smoothstep(0.45, 0.6, band * (0.8 + n)),
        smoothstep(0.5, 0.75, line * (0.9 + n)),
    ) * w25 * (1.0 - river_w);
    let foam = max(shore_foam, white);

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
    let own = mix(water_tint(water), river_tint(water, length(w.flow)), river_w);
    let foam_colour = mix(vec3<f32>(0.85, 0.88, 0.86), WHITE_WATER, river_w);
    let albedo = mix(own, foam_colour, foam);
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
    pbr_input.world_position = vec4<f32>(fog_position(params, pbr_input.world_position.xyz), 1.0);
    out.color = main_pass_post_lighting_processing(pbr_input, color);
    return out;
}
