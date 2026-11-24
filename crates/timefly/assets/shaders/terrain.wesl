// The region terrain's main pass (M2 design §6.3, §6.4): CDLOD patches displaced in the vertex
// stage, painted in the fragment stage and lit through Bevy's StandardMaterial PBR path (lights,
// shadows, fog, tonemapping).
//
// Every patch entity shares one grid mesh, whose positions are (i, 0, j) / quads, and one
// material. The entity's MeshTag indexes the patch buffer. The vertex math lives in
// terrain_cdlod.wesl, which terrain_prepass.wesl also calls, so shadows match.
//
// The fragment (painterly naturalism, spec §9):
// - colour: the palette (binding 105) of the four surrounding surface-code texels, blended in
//   colour space; canopy over ground in brushy patches; rock on slopes; snow from the ice byte
//   and the snowline; green-grey microbial mats on Barren-era shores; in deserts, sand seas of
//   creeping transverse dunes (their slope tilts the shading normal) and stony plain with gravel
//   patches and mottling elsewhere; a few percent of value and hue jitter. The sand-sea mask, the
//   dune warp, the stony plain and the band warp are keyed to the planet-fixed position; the
//   dunes' crest phase is the along-wind distance from the region centre, so it depends on the
//   centre (a known limitation until M3 adds a global phase offset). The brush is noise.wesl
//   at 400 m, 60 m and 9 m world scales, each faded out as it shrinks below a couple of pixels.
// - light: Bevy PBR, then a soft three-band ramp on the diffuse illumination and a hue shift of
//   shadowed light toward the sky's cool, era-tinted zenith (TerrainLook, binding 106).

import package::terrain_cdlod::{
    TerrainBuffer, TerrainParams, blended_height, cdlod_vertex, fog_position, grid_index_of,
    terrain_normal,
};
import package::noise::{gradient_noise3, hash3};
import package::planet::planet_position_m;
import package::snowline::{snowline_m, snowline_raise_from_byte};
import bevy_pbr::render::mesh_functions::get_tag;
import bevy_pbr::render::mesh_view_bindings::globals;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing};
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

struct TerrainLook {
    // rgb: the hue shadowed light shifts toward (luminance 1); a: strength.
    shadow: vec4<f32>,
    // x: full-sun illumination; y: ramp amount; z: brush strength; w: centre latitude (deg).
    light: vec4<f32>,
    // x: relief exaggeration for shading; y: the era's sea-level anomaly (m), for the snowline;
    // z: the ramp floor for snow; w: the hue factor for shaded snow.
    relief: vec4<f32>,
    // xyz: the region centre's unit direction (era frame); w: Earth's radius (m).
    frame_center: vec4<f32>,
    // xyz: unit tangent toward north at the centre (local -z).
    frame_north: vec4<f32>,
    // xyz: unit tangent toward east at the centre (local +x).
    frame_east: vec4<f32>,
    // xyz: the prevailing wind's 3D axis W (tangent at the centre). The dune phase is the
    // along-wind distance from the centre, dot(p, W), taken from local metres (desert_at).
    // w: 1 when the dunes are drawn, 0 under --no-dunes (desert_at skips the dune work).
    wind: vec4<f32>,
    // x: main dune spacing (m); y: small dune spacing (m); z: main period (s); w: small period (s).
    dunes: vec4<f32>,
    // x: the sand-sea mask's noise scale (m); y: mean rock band thickness (m); z: the band warp's
    // amplitude (m); w: the slope (rise over run) from which dry bare
    // ground shows bands (BAND_FULL_SLOPE below is where they reach full weight).
    patterns: vec4<f32>,
    // xyz: the unit direction toward the region's sun (REGION_SUN_DIRECTION), for
    // bound_dune_tilt; w: 0.
    sun: vec4<f32>,
}

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> terrain: TerrainBuffer;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var inner_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var apron_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var inner_surface: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(104) var apron_surface: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(105) var palette: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(106) var<uniform> look: TerrainLook;

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);
// Linear colours.
const SNOW: vec3<f32> = vec3<f32>(0.80, 0.84, 0.90);
const MICROBIAL_MAT: vec3<f32> = vec3<f32>(0.16, 0.19, 0.13);
const BEACH_SAND: vec3<f32> = vec3<f32>(0.46, 0.38, 0.24);
// Metres per degree of latitude.
const METRES_PER_DEGREE: f32 = 111195.0;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

struct TerrainVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    // The unmorphed patch grid index, interpolated: integer lines are the triangle edges.
    @location(1) grid: vec2<f32>,
    @location(2) @interpolate(flat) lod: u32,
    @location(3) morph: f32,
    @location(4) @interpolate(flat) instance_index: u32,
}

@vertex
fn vertex(in: Vertex) -> TerrainVertexOutput {
    let params = terrain.params;
    let inst = terrain.patches[get_tag(in.instance_index)];
    let grid_index = grid_index_of(in.position, params);
    let v = cdlod_vertex(grid_index, inst, params, inner_height, apron_height);
    var out: TerrainVertexOutput;
    out.world_position = vec4<f32>(v.world, 1.0);
    out.position = position_world_to_clip(v.world);
    out.grid = vec2<f32>(grid_index);
    out.lod = inst.cells.z;
    out.morph = v.k;
    out.instance_index = in.instance_index;
    return out;
}

// The palette colours and flags of one surface-code texel.
struct Surface {
    ground: vec3<f32>,
    canopy: vec3<f32>,
    rock: vec3<f32>,
    // Canopy cover (0..1), ice (0..1), shore flag (0 or 1), Barren era (0 or 1).
    flags: vec4<f32>,
    // The snowline's dryness raise (m, ruling R27).
    snow_raise: f32,
    // Sand weight at the sea (ground texel alpha) and rock exposure (rock texel alpha), 0..1.
    sand: f32,
    exposure: f32,
}

fn texel_surface(code: vec4<f32>) -> Surface {
    let biome = min(u32(round(code.x * 255.0)), 11u);
    let era = min(u32(round(code.y * 255.0)), 5u);
    let at = vec2<i32>(i32(biome * 3u), i32(era));
    let canopy = textureLoad(palette, at + vec2<i32>(1, 0), 0);
    var s: Surface;
    s.canopy = canopy.rgb;
    let rock = textureLoad(palette, at + vec2<i32>(2, 0), 0);
    let ground = textureLoad(palette, at, 0);
    s.ground = ground.rgb;
    s.rock = rock.rgb;
    s.sand = ground.a;
    s.exposure = rock.a;
    // The fourth byte: the shore flag in its top bit, the snowline's dryness raise below it.
    let byte3 = u32(round(code.w * 255.0));
    let shore = select(0.0, 1.0, byte3 >= 128u);
    s.flags = vec4<f32>(canopy.a, code.z, shore, select(0.0, 1.0, era == 0u));
    s.snow_raise = snowline_raise_from_byte(byte3);
    return s;
}

fn mix_surface(a: Surface, b: Surface, t: f32) -> Surface {
    var s: Surface;
    s.ground = mix(a.ground, b.ground, t);
    s.canopy = mix(a.canopy, b.canopy, t);
    s.rock = mix(a.rock, b.rock, t);
    s.flags = mix(a.flags, b.flags, t);
    s.snow_raise = mix(a.snow_raise, b.snow_raise, t);
    s.sand = mix(a.sand, b.sand, t);
    s.exposure = mix(a.exposure, b.exposure, t);
    return s;
}

// The surface of one field at xz: its four surrounding texels' colours, blended bilinearly in
// colour space (class blending happens between colours, never between class indices).
fn field_surface(tex: texture_2d<f32>, field: vec4<f32>, xz: vec2<f32>) -> Surface {
    let last = field.z - 1.0;
    let uv = clamp((xz + vec2<f32>(field.x * 0.5)) / field.y, vec2<f32>(0.0), vec2<f32>(last));
    let c0 = min(floor(uv), vec2<f32>(last - 1.0));
    let f = uv - c0;
    let i = vec2<i32>(c0);
    let s00 = texel_surface(textureLoad(tex, i, 0));
    let s10 = texel_surface(textureLoad(tex, i + vec2<i32>(1, 0), 0));
    let s01 = texel_surface(textureLoad(tex, i + vec2<i32>(0, 1), 0));
    let s11 = texel_surface(textureLoad(tex, i + vec2<i32>(1, 1), 0));
    return mix_surface(mix_surface(s00, s10, f.x), mix_surface(s01, s11, f.x), f.y);
}

// The surface at xz, blended between the fields exactly as blended_height blends heights.
fn surface_at(params: TerrainParams, xz: vec2<f32>) -> Surface {
    let a = max(abs(xz.x), abs(xz.y));
    let s = smoothstep(params.inner.w, params.apron.w, a);
    if s <= 0.0 {
        return field_surface(inner_surface, params.inner, xz);
    }
    let outer = field_surface(apron_surface, params.apron, xz);
    if s >= 1.0 {
        return outer;
    }
    return mix_surface(field_surface(inner_surface, params.inner, xz), outer, s);
}

// Gradient noise at a world-space scale. noise.wesl wants an integer cell plus a fraction
// (lattice_split); the quotient of region-local metres (within +-1.7e6) by a 9 m scale or more
// stays below 2^18, where f32 still resolves a cell to 1/64, so floor and fraction are exact
// enough without f64.
fn brush(xz: vec2<f32>, scale_m: f32, seed: u32) -> f32 {
    let q = xz / scale_m;
    let c = floor(q);
    return gradient_noise3(vec3<i32>(i32(c.x), i32(c.y), 0), vec3<f32>(q - c, 0.37), seed);
}

// Fades a noise octave out as it shrinks below a couple of pixels, so it never aliases.
fn octave_weight(footprint_m: f32, scale_m: f32) -> f32 {
    return 1.0 - smoothstep(0.25, 0.6, footprint_m / scale_m);
}

// Gradient noise at a planet-fixed position p (m, from planet_position_m), so the pattern stays
// put whichever region centre the place is entered from (the sand-sea mask, the dune warp, the
// stony plain and the band warp; not the dunes' crest phase, see desert_at). |p| is about
// 6.4e6 m, so p / scale_m fits an i32 cell for any scale; at scales of 30 m or more the f32
// fraction resolves a cell to 1/64 or better, which is plenty for value noise.
fn planet_noise(p: vec3<f32>, scale_m: f32, seed: u32) -> f32 {
    let q = p / scale_m;
    let c = floor(q);
    return gradient_noise3(vec3<i32>(c), q - c, seed);
}

// The slope at which bands on dry, bare ground reach full weight (BAND_FULL_SLOPE in material.rs);
// they ramp in from look.patterns.w (0.06), so steeper ground reads stronger and level plains none.
const BAND_FULL_SLOPE: f32 = 0.2;

// Rock strata (spec §5.5). Each layer cuts the warped height into cells of its own thickness and
// gives every cell a random (value, hue) in -1..1; three layers at 0.4, 1 and 1.7 times
// look.patterns.y (14, 35 and 60 m) overlap into bands of mixed thickness (10-60 m), so the
// stack does not read as stripes. A layer fades out as the vertical extent of a pixel
// (`vertical_m`) approaches its thickness, so the bands never alias.
fn band_layer(h: f32, thickness_m: f32, seed: u32, vertical_m: f32) -> vec2<f32> {
    let cell = i32(floor(h / thickness_m));
    let a = f32(hash3(vec3<i32>(cell, 0, 0), seed) >> 8u) / 8388608.0 - 1.0;
    let b = f32(hash3(vec3<i32>(cell, 1, 0), seed + 1u) >> 8u) / 8388608.0 - 1.0;
    return vec2<f32>(a, b) * octave_weight(vertical_m, thickness_m);
}

// The strata's colour factor at the planet position p and true height: value +-20 % and a
// warm-cool hue of a few percent, per band. The height is warped by km-scale planet noise of
// amplitude look.patterns.z, so the layers dip and fold, the same way on every visit.
fn band_tint(p: vec3<f32>, height: f32, vertical_m: f32) -> vec3<f32> {
    let warp = look.patterns.z
        * (0.7 * planet_noise(p, 4000.0, 301u) + 0.3 * planet_noise(p, 1300.0, 303u));
    let h = height + warp;
    let t = look.patterns.y;
    let l0 = band_layer(h, 0.4 * t, 311u, vertical_m);
    let l1 = band_layer(h, t, 317u, vertical_m);
    let l2 = band_layer(h, 1.7 * t, 331u, vertical_m);
    let v = clamp(0.35 * l0.x + 0.45 * l1.x + 0.55 * l2.x, -1.0, 1.0);
    let w = clamp(0.35 * l0.y + 0.45 * l1.y + 0.55 * l2.y, -1.0, 1.0);
    return (1.0 + 0.2 * v)
        * mix(vec3<f32>(1.03, 1.0, 0.96), vec3<f32>(0.97, 1.0, 1.04), 0.5 + 0.5 * w);
}

// The sand seas (spec §5.3). The mask is planet-position noise at look.patterns.x (40 km) and a
// third of it; above SAND_SEA_EDGE's centre (0.14, the 70th percentile of the mask over the
// sphere) lies sand sea, so about 30 % of desert ground is erg and the rest stony plain.
// The seeds (213, 225) are arbitrary; this pair puts the Sahara review scenes' centre (23 N, 10 E)
// on an erg with stony plain in view, so both looks can be checked.
const SAND_SEA_DETAIL: f32 = 0.5;
const SAND_SEA_EDGE: vec2<f32> = vec2<f32>(0.10, 0.18);
// Dune heights (m): the main and small crests. Shading only; the geometry stays the height field.
const DUNE_HEIGHT_M: f32 = 25.0;
const DUNE_SMALL_HEIGHT_M: f32 = 6.0;
// The share of a dune's cycle taken by the windward face; the lee (slip face) takes the rest,
// so windward to lee is 4:1 and the lee is four times as steep.
const DUNE_WINDWARD: f32 = 0.8;
// The small crests run at this angle (radians) to the main ones, so the two octaves cross.
const DUNE_SMALL_ANGLE: f32 = 0.35;
// Forward-difference step (m) for the warp's gradient (the warp's finest scale is 600 m).
const DUNE_WARP_STEP_M: f32 = 40.0;
// Sand seas lie on flat ground: the erg fades out between these true slopes (rise over run) and
// the desert there is stony plain, so the dunes' tilt never rides on a hillside's. Mirrored as
// DUNE_SLOPE in material.rs.
const DUNE_SLOPE: vec2<f32> = vec2<f32>(0.03, 0.07);
// How far the dunes may turn a face away from the sun: this share of the way from facing straight
// up to the terminator, counting the terrain's own (exaggerated) tilt (bound_dune_tilt). At 0.25
// a slip face turned from the sun keeps about three quarters of level ground's sunlight, above
// the ramp's shade step, so it reads as soft shade and never as a black line (0.4-0.5 still
// dropped into the step at the Grand Canyon plateau edge). Mirrored as DUNE_AWAY_LIMIT in
// material.rs, whose bound_dune_tilt mirror is property-tested.
const DUNE_AWAY_LIMIT: f32 = 0.25;
const PI: f32 = 3.14159265;

// The static warp of the main crests, in main cycles: crests wander and fork. Planet-fixed and
// time-independent, so the creep (the phase alone moves) is exactly periodic.
fn dune_warp(p: vec3<f32>) -> f32 {
    return 0.75 * planet_noise(p, 5000.0, 227u) + 0.3 * planet_noise(p, 1800.0, 229u);
}

// The small crests' own warp, in small cycles.
fn dune_small_warp(p: vec3<f32>) -> f32 {
    return 0.6 * planet_noise(p, 600.0, 233u);
}

// The dune profile at phase u (any real; one cycle per crest): x the height (0 in the trough,
// 1 on the crest), y its derivative d/du. The windward face rises over DUNE_WINDWARD of the cycle
// and the slip face falls over the rest, both as half cosines, so crest and trough are smooth
// (no aliasing edge) and the lee is steeper by DUNE_WINDWARD / (1 - DUNE_WINDWARD).
fn dune_profile(u: f32) -> vec2<f32> {
    let f = fract(u);
    let lee = f >= DUNE_WINDWARD;
    let span = select(DUNE_WINDWARD, 1.0 - DUNE_WINDWARD, lee);
    let s = select(f, 1.0 - f, lee) / span;
    let slope = 0.5 * PI * sin(PI * s) / span;
    return vec2<f32>(0.5 - 0.5 * cos(PI * s), select(slope, -slope, lee));
}

// The profile's band-limited stand-in for when the slip face is too narrow to draw: one
// sinusoid per cycle with its crest where dune_profile's is, so the sharp profile can fade into
// it without the crests jumping.
fn dune_profile_soft(u: f32) -> vec2<f32> {
    let a = 2.0 * PI * (u - DUNE_WINDWARD);
    return vec2<f32>(0.5 + 0.5 * cos(a), -PI * sin(a));
}

// The desert's look at one pixel: the sand-sea weight, the dunes' normal tilt (per unit sand-sea
// weight, before the masks; like the fragment's `gradient`, minus the height's rise over run),
// the crest tone (-0.5 trough .. 0.5 crest, faded) and the stony plain's gravel and mottling.
struct Desert {
    sea: f32,
    gradient: vec2<f32>,
    tone: f32,
    gravel: f32,
    mottle: f32,
}

// xz is the region-local position, p its planet position; footprint the pixel's size (m).
fn desert_at(xz: vec2<f32>, p: vec3<f32>, footprint: f32, slope: f32) -> Desert {
    var d: Desert;
    let mask = planet_noise(p, look.patterns.x, 213u)
        + SAND_SEA_DETAIL * planet_noise(p, look.patterns.x / 3.0, 225u);
    let raw_sea = smoothstep(SAND_SEA_EDGE.x, SAND_SEA_EDGE.y, mask);
    d.sea = raw_sea * (1.0 - smoothstep(DUNE_SLOPE.x, DUNE_SLOPE.y, slope));
    d.gradient = vec2<f32>(0.0);
    d.tone = 0.0;
    d.gravel = 0.0;
    d.mottle = 0.0;

    // --no-dunes (wind.w = 0) skips all of it: one test of a uniform the whole frame shares, so
    // every pixel takes the same branch. The sand sea stays (the mask above) as flat, still sand.
    if look.wind.w > 0.5 {
        // The local frame: +x east, +z south, so a planet direction v moves region-local xz by
        // (dot(v, east), -dot(v, north)). W is fixed per region (tangent at the centre); across the
        // region it stays within a degree or two of the local tangent plane.
        let east = look.frame_east.xyz;
        let north = look.frame_north.xyz;
        let wind = look.wind.xyz;
        let t = globals.time;
        // The crest phase's along-wind distance, dot(p, W) for a tangent axis W at the centre. With
        // p = R (cos(a) centre + sin(a) / |xz| (east x - north z)) and a = |xz| / R, that is
        // dot(xz, W's local xz) times sin(a) / a, taken from local metres so f32 never has to resolve
        // metres against |p| of 6.4e6 m. It is the distance from the region centre, so the phase
        // depends on the centre: crests shift between visits from different centres (a known
        // limitation; M3 adds a per-region f64 phase offset from a global along-wind field).
        let a = length(xz) / look.frame_center.w;
        let sinc = select(sin(a) / a, 1.0 - a * a / 6.0, a < 1e-3);

        // Each octave's sharp profile fades out as its slip face (a fifth of a spacing, the sharpest
        // feature) shrinks below a couple of pixels. The main crests then carry on as a sinusoid,
        // which fades as a whole cycle gets that small, so from 40 km a sand sea is a faint
        // corrugation; the small crests are gone by then.
        let sharp_w = octave_weight(footprint, 0.2 * look.dunes.x);
        let main_w = octave_weight(footprint, 0.5 * look.dunes.x);
        let small_w = octave_weight(footprint, 0.2 * look.dunes.y);
        if d.sea > 0.0 && main_w > 0.0 {
            // The warp and its gradient (forward differences one step east and one step south).
            let pe = p + east * DUNE_WARP_STEP_M;
            let ps = p - north * DUNE_WARP_STEP_M;
            let w0 = dune_warp(p);
            let warp_grad = vec2<f32>(dune_warp(pe) - w0, dune_warp(ps) - w0) / DUNE_WARP_STEP_M;
            let wind_xz = vec2<f32>(dot(wind, east), -dot(wind, north));
            // Crest phase along the wind; only the phase moves, by one spacing per period.
            let u = dot(xz, wind_xz) * sinc / look.dunes.x + w0 - t / look.dunes.z;
            let big = mix(dune_profile_soft(u), dune_profile(u), sharp_w) * main_w;
            let big_grad = wind_xz / look.dunes.x + warp_grad;
            // The normal tilts away from the rise: minus height times dh/du times du/dxz.
            d.gradient = -DUNE_HEIGHT_M * big.y * big_grad;
            d.tone = big.x - 0.5 * main_w;

            if small_w > 0.0 {
                // The small crests: the wind turned by DUNE_SMALL_ANGLE in the tangent plane, the
                // main warp (so they follow the big crests' wander) plus their own.
                let side = cross(look.frame_center.xyz, wind);
                let wind2 = wind * cos(DUNE_SMALL_ANGLE) + side * sin(DUNE_SMALL_ANGLE);
                let wind2_xz = vec2<f32>(dot(wind2, east), -dot(wind2, north));
                let ratio = look.dunes.x / look.dunes.y;
                let s0 = dune_small_warp(p);
                let small_warp_grad = vec2<f32>(dune_small_warp(pe) - s0, dune_small_warp(ps) - s0)
                    / DUNE_WARP_STEP_M;
                let u2 = dot(xz, wind2_xz) * sinc / look.dunes.y + w0 * ratio + s0 - t / look.dunes.w;
                let small = dune_profile(u2);
                let small_grad = wind2_xz / look.dunes.y + warp_grad * ratio + small_warp_grad;
                d.gradient -= DUNE_SMALL_HEIGHT_M * small_w * small.y * small_grad;
                d.tone += 0.4 * (small.x - 0.5) * small_w;
            }
        }
    }

    // The stony plain: darker gravel patches (desert pavement) at 2.5 km and 700 m, and a
    // mottling at 150 m and 35 m.
    if d.sea < 1.0 {
        let g = planet_noise(p, 2500.0, 239u) * octave_weight(footprint, 2500.0)
            + 0.35 * planet_noise(p, 700.0, 241u) * octave_weight(footprint, 700.0);
        d.gravel = smoothstep(-0.25, 0.45, g);
        d.mottle = planet_noise(p, 150.0, 251u) * octave_weight(footprint, 150.0)
            + 0.6 * planet_noise(p, 35.0, 257u) * octave_weight(footprint, 35.0);
    }
    return d;
}

// What `paint` returns: the albedo (linear), the snow cover (0..1, which lights differently),
// the dune weight (sand sea, less rock and snow, 0..1) and the dunes' normal tilt in region-local
// xz (already weighted), which adds to the fragment's shading tilt.
struct Paint {
    albedo: vec3<f32>,
    snow: f32,
    sand: f32,
    dune_gradient: vec2<f32>,
}

// The painted albedo (linear) at xz for the height of the drawn surface (the morphing mesh, as
// the beach, mat, snow and desert thresholds have used since M2; the strata read the true
// surface), the flat-terrain slope (rise over run) and the pixel footprint (m).
fn paint(params: TerrainParams, xz: vec2<f32>, height: f32, slope: f32, footprint: f32) -> Paint {
    // The class boundaries come from 1.85 km climate data and read as stair-steps; warping the
    // lookup with two noise scales turns them into ragged, brushy edges (the heights are not
    // warped, so the land and sea stay where the data put them).
    let warp = vec2<f32>(brush(xz, 3000.0, 101u), brush(xz, 3000.0, 103u)) * 1100.0
        + vec2<f32>(brush(xz, 600.0, 107u), brush(xz, 600.0, 109u)) * 280.0
            * octave_weight(footprint, 600.0);
    let surf = surface_at(params, xz + warp);
    let cover = surf.flags.x;
    let ice = surf.flags.y;
    let shore = surf.flags.z;
    let barren = surf.flags.w;

    let n2k = brush(xz, 2500.0, 5u) * octave_weight(footprint, 2500.0);
    let n400 = brush(xz, 400.0, 11u) * octave_weight(footprint, 400.0);
    let n60 = brush(xz, 60.0, 23u) * octave_weight(footprint, 60.0);
    let n9 = brush(xz, 9.0, 37u) * octave_weight(footprint, 9.0);

    // Canopy over ground in brushy patches: where the patch noise falls under the cover.
    // The mask stays soft and never quite closes, so forests keep a few glades and light.
    let patches = 0.5 + 0.55 * (0.55 * n2k + 0.3 * n400 + 0.15 * n60);
    let canopy = 0.9 * smoothstep(-0.25, 0.25, cover - patches);
    var c = mix(surf.ground, surf.canopy, canopy);
    // Tree-scale breakup inside the canopy: crowns lit and shaded.
    c = c * (1.0 + canopy * 0.12 * (n60 + 0.8 * n9));

    // Sand along non-Barren shores, just above the sea.
    // (The shore flag is read through the warp, so the height bounds keep these at the coast.)
    let beach = shore * (1.0 - barren) * (1.0 - smoothstep(1.0, 9.0, height + 6.0 * n60));
    c = mix(c, BEACH_SAND, beach * 0.6);

    // Microbial mats: a green-grey tint on Barren-era shores, in patches.
    let near_sea = 1.0 - smoothstep(10.0, 20.0, abs(height));
    let mats = shore * barren * near_sea * smoothstep(-0.25, 0.25, n400 + 0.5 * n60);
    c = mix(c, MICROBIAL_MAT, mats * 0.55);

    // Rock above a slope of 0.6 (31 degrees), with a ragged edge (applied after the deserts).
    let rock = smoothstep(0.5, 0.7, slope + 0.12 * n60 + 0.06 * n9);

    // Rock strata (spec §5.5), applied before the snow so snow edges keep their full cover: on
    // cliffs, and on slopes of dry or bare ground (exposure >= 0.8) with a weight that grows from
    // look.patterns.w to BAND_FULL_SLOPE, scaled by how exposed the rock is. Not under canopy
    // (a true zero), on beaches, on microbial mats (an era signature) or on dune sand (below).
    // The weight also fades with the strata's visibility, so ground too far away to show bands
    // keeps its own colour instead of turning rock-coloured.
    let moderate = smoothstep(0.7, 0.8, surf.exposure)
        * smoothstep(look.patterns.w, BAND_FULL_SLOPE, slope);
    let vertical_m = slope * footprint;
    let visible = octave_weight(vertical_m, 1.7 * look.patterns.y);
    let band_ground = surf.exposure * max(rock, moderate) * (1.0 - min(canopy / 0.9, 1.0))
        * (1.0 - beach) * (1.0 - mats) * visible;

    // Deserts (spec §5.3): sand seas with creeping dunes, and stony plain elsewhere. The sand
    // weight blends like every other surface value, so desert edges stay soft. No dunes under
    // the sea (the water plane covers height < 0) or on beaches; rock and snow are masked below.
    var dune_sea = 0.0;
    var dune_gradient = vec2<f32>(0.0);
    let desert_ground = surf.sand * (1.0 - canopy) * (1.0 - beach) * smoothstep(0.0, 2.0, height);
    // The planet position, once, for the desert and the strata (pixels needing neither skip it).
    var pp = vec3<f32>(0.0);
    if desert_ground > 0.0 || band_ground > 0.0 {
        pp = planet_position_m(
            xz, look.frame_center.xyz, look.frame_north.xyz, look.frame_east.xyz, look.frame_center.w
        );
    }
    if desert_ground > 0.0 {
        let desert = desert_at(xz, pp, footprint, slope);
        let sea = desert.sea;
        dune_sea = desert_ground * sea;
        dune_gradient = desert.gradient;
        // Erg: the ground colour, lighter on crests and darker in the interdune troughs.
        let erg = 1.0 + 0.2 * desert.tone;
        // Stony plain: gravel patches toward the darker, greyer rock colour, and mottling.
        let plain = desert_ground * (1.0 - sea);
        let gravel = mix(c, surf.rock * 0.8, 0.35 * desert.gravel);
        let stony = gravel * (1.0 + 0.18 * desert.mottle);
        c = mix(c, stony, plain);
        c = c * mix(1.0, erg, dune_sea);
    }

    // The rock on steep slopes (its weight is above).
    c = mix(c, surf.rock, rock);

    // The strata. Pixels with no weight evaluate no noise and load no extra heights. The bands
    // are keyed to the true bilinear surface, not the morphing mesh height (`height`), so 14-60 m
    // strata don't slide across the LOD morph ring.
    let band_weight = band_ground * (1.0 - dune_sea);
    if band_weight > 0.0 {
        let true_height = blended_height(inner_height, apron_height, params, xz);
        c = mix(c, surf.rock * band_tint(pp, true_height, vertical_m), band_weight);
    }

    // Snow: the ice byte, sharpened per pixel by the snowline at the pixel's latitude (north is
    // -z), shed by cliffs, with a painted edge.
    let lat = look.light.w - xz.y / METRES_PER_DEGREE;
    let line = snowline_m(lat, look.relief.y, surf.snow_raise);
    let above = smoothstep(line - 150.0, line + 150.0, height + 140.0 * n400 + 40.0 * n60);
    let cliff = smoothstep(0.9, 1.4, slope);
    let snow_amount = max(ice * (1.0 - 0.6 * cliff), above * step(0.0, height) * (1.0 - cliff));
    let snow = smoothstep(0.35, 0.65, snow_amount + 0.12 * n60 + 0.06 * n9);
    c = mix(c, SNOW, snow);

    // Brush: value jitter, and a warm-cool hue jitter, a few percent each.
    let k = look.light.z;
    let value = 1.0 + k * (n400 + 0.7 * n60 + 0.45 * n9);
    let hue = 0.5 + 0.5 * (0.6 * n2k + 0.4 * n60);
    c = c * value * mix(vec3<f32>(1.035, 1.0, 0.955), vec3<f32>(0.965, 1.0, 1.045), hue);
    var p: Paint;
    p.albedo = max(c, vec3<f32>(0.0));
    p.snow = snow;
    p.sand = dune_sea * (1.0 - rock) * (1.0 - snow);
    p.dune_gradient = dune_gradient * p.sand;
    return p;
}

// The ramp: illumination l (1 = a surface facing the sun; level ground in the region's sun is
// about 0.55 with the ambient) pulled toward three soft bands: shade, half-light and full light.
// Level ground sits inside the middle band, so slopes turned to or from the sun step out of it.
//
// `floor_level` is the shade band's level (0.16 for everything but snow) and `lift` the height
// of the first step; the half-light band stays at 0.58 and full light at 1.0 (floor + lift = 0.58),
// so only the shade is lifted. For snow = 0 this is the original ramp.
fn ramp(l: f32, floor_level: f32, lift: f32) -> f32 {
    let banded = floor_level + lift * smoothstep(0.22, 0.38, l) + 0.42 * smoothstep(0.70, 0.86, l);
    let bands = banded + max(l - 1.0, 0.0);
    return mix(l, bands, look.light.y);
}

// Repaints PBR light: the ramp on the illumination, then a hue shift of shadowed light.
// Snow (0..1) lifts the ramp's shade floor to look.relief.z (the half-light and full-light bands stay put) and takes
// look.relief.w of the shadow hue.
fn paint_light(lit: vec3<f32>, albedo: vec3<f32>, snow: f32) -> vec3<f32> {
    let l = dot(lit, LUMA) / max(dot(albedo, LUMA), 1e-4) / max(look.light.x, 1e-6);
    let lift = mix(0.42, 0.58 - look.relief.z, snow);
    let ramp_level = ramp(l, mix(0.16, look.relief.z, snow), lift);
    // In deep shade `lit` is nearly all blue sky ambient, and scaling it up to the ramp's floor
    // would amplify that blue (vivid blotches on shaded cliffs). Below l = 0.15 the colour comes
    // from the albedo in full-sun light instead, at the same luminance; at and above 0.15 it is
    // the scaled `lit` exactly.
    let scaled = lit * (ramp_level / max(l, 1e-4));
    let in_sun_colour = albedo * (look.light.x * ramp_level);
    var c = mix(in_sun_colour, scaled, smoothstep(0.05, 0.15, l));
    let shade = 1.0 - smoothstep(0.12, 0.6, l);
    c = c * mix(vec3<f32>(1.0), look.shadow.rgb, shade * look.shadow.a * mix(1.0, look.relief.w, snow));
    return c;
}

// The dunes' tilt, bounded so that with the terrain's shading tilt `base` it never turns a face
// more than DUNE_AWAY_LIMIT of the way to the sun's terminator. Unnormalised, N.L is
// sun.y + dot(tilt, sun.xz), so the terminator is dot(tilt, sun.xz) = -sun.y. Only the dune
// tilt's component along sun.xz is limited, and only where it turns the face away; faces turned
// toward the sun, and faces the terrain alone already turns further, keep their tilt (the dunes
// then add nothing away from the sun). The sun is the region sun's direction from the look
// (Bevy's per-view light list is sorted by shadow flags and entity, so a second directional light
// on the terrain layer could take index 0). Mirrored by bound_dune_tilt in material.rs.
fn bound_dune_tilt(base: vec2<f32>, dune: vec2<f32>) -> vec2<f32> {
    let to_sun = look.sun.xyz;
    let across = to_sun.xz;
    let len2 = dot(across, across);
    if to_sun.y <= 0.0 || len2 < 1e-6 {
        return dune;
    }
    let lowest = -DUNE_AWAY_LIMIT * to_sun.y;
    let along = dot(dune, across);
    let allowed = max(along, min(0.0, lowest - dot(base, across)));
    return dune + across * ((allowed - along) / len2);
}

// Distance to the nearest triangle edge in pixels: grid lines and the (i, j)-(i+1, j+1) diagonal.
fn edge_pixels(grid: vec2<f32>) -> f32 {
    let f = fract(grid);
    let w = max(fwidth(grid), vec2<f32>(1e-6));
    let along = min(f, vec2<f32>(1.0) - f) / w;
    let diagonal = abs(f.x - f.y) / max(length(w), 1e-6);
    return min(min(along.x, along.y), diagonal);
}

// A colour per LOD for the wireframe view.
fn lod_tint(lod: u32) -> vec3<f32> {
    let h = f32(lod) * 0.13;
    return vec3<f32>(0.5) + 0.4 * cos(6.2831853 * (vec3<f32>(h) + vec3<f32>(0.0, 0.33, 0.67)));
}

@fragment
fn fragment(in: TerrainVertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    let params = terrain.params;
    let xz = in.world_position.xz;
    // Per-pixel normals (with the curvature tilt), so the lighting does not depend on the
    // patch's LOD. The vertex stage computes none.
    let normal = terrain_normal(inner_height, apron_height, params, xz);

    // The terrain's own gradient (the normal's, less the curvature's d/R) and its slope.
    let d = xz - params.camera.xz;
    let curve = d / params.lattice.z;
    let gradient = normal.xz / max(normal.y, 1e-3) - curve;
    let slope = length(gradient);

    // The paint first (it also says how much snow lies here), then the shading normal: the
    // gradient exaggerated, the curvature tilt kept. The drop moves the surface below the blended
    // height; colour by the terrain's own height.
    let height = in.world_position.y + dot(d, d) / (2.0 * params.lattice.z);
    let painted = paint(params, xz, height, slope, length(fwidth(xz)));
    // The dunes' tilt joins the shading tilt, so the PBR sun and the ramp light each dune face by
    // its own slope (which face is lit depends on where the sun stands against the wind). It is
    // bounded so a slip face turned from the sun stays in soft shade.
    let base_shading = gradient * look.relief.x + curve;
    let shading = base_shading + bound_dune_tilt(base_shading, painted.dune_gradient);
    var mesh: VertexOutput;
    mesh.position = in.position;
    mesh.world_position = in.world_position;
    mesh.world_normal = normalize(vec3<f32>(shading.x, 1.0, shading.y));
    @if(VERTEX_UVS_A)
    mesh.uv = in.grid / params.lattice.w;
    @if(VERTEX_UVS_B)
    mesh.uv_b = in.grid / params.lattice.w;
    @if(VERTEX_TANGENTS)
    mesh.world_tangent = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    @if(VERTEX_COLORS)
    mesh.color = vec4<f32>(1.0);
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    mesh.instance_index = in.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    mesh.visibility_range_dither = 0;

    var pbr_input = pbr_input_from_standard_material(mesh, is_front);
    var albedo = painted.albedo;
    let wireframe = params.shading.x > 0.5;
    if wireframe {
        albedo = mix(albedo, lod_tint(in.lod), 0.55);
    }
    pbr_input.material.base_color = vec4<f32>(albedo, 1.0) * pbr_input.material.base_color;

    var out: FragmentOutput;
    let lit = apply_pbr_lighting(pbr_input);
    out.color = vec4<f32>(paint_light(lit.rgb, pbr_input.material.base_color.rgb, painted.snow), lit.a);
    pbr_input.world_position = vec4<f32>(fog_position(params, pbr_input.world_position.xyz), 1.0);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    if wireframe {
        let line = 1.0 - smoothstep(0.5, 1.5, edge_pixels(in.grid));
        out.color = vec4<f32>(mix(out.color.rgb, vec3<f32>(0.02, 0.02, 0.03), line * 0.85), out.color.a);
    }
    return out;
}
