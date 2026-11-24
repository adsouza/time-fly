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
//   and the snowline; green-grey microbial mats on Barren-era shores; a few percent of value
//   and hue jitter. The brush is noise.wesl at 400 m, 60 m and 9 m world scales, each faded out
//   as it shrinks below a couple of pixels.
// - light: Bevy PBR, then a soft three-band ramp on the diffuse illumination and a hue shift of
//   shadowed light toward the sky's cool, era-tinted zenith (TerrainLook, binding 106).

import package::terrain_cdlod::{
    TerrainBuffer, TerrainParams, cdlod_vertex, grid_index_of, terrain_normal,
};
import package::noise::gradient_noise3;
import package::planet::planet_position_m;
import package::snowline::{snowline_m, snowline_raise_from_byte};
import bevy_pbr::render::mesh_functions::get_tag;
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
    // xyz: the prevailing wind's 3D axis W (tangent at the centre); dune phase is dot(p, W).
    wind: vec4<f32>,
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

// What `paint` returns: the albedo (linear), the snow cover (0..1, which lights differently),
// and the sand cover and dune gradient (zero until the dunes land).
struct Paint {
    albedo: vec3<f32>,
    snow: f32,
    sand: f32,
    dune_gradient: vec2<f32>,
}

// The painted albedo (linear) at xz for the true height, the flat-terrain slope (rise over run)
// and the pixel footprint (m).
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

    // Rock above a slope of 0.6 (31 degrees), with a ragged edge.
    let rock = smoothstep(0.5, 0.7, slope + 0.12 * n60 + 0.06 * n9);
    c = mix(c, surf.rock, rock);

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
    p.sand = 0.0;
    p.dune_gradient = vec2<f32>(0.0);
    return p;
}

// The ramp: illumination l (1 = a surface facing the sun; level ground in the region's sun is
// about 0.55 with the ambient) pulled toward three soft bands: shade, half-light and full light.
// Level ground sits inside the middle band, so slopes turned to or from the sun step out of it.
fn ramp(l: f32, floor_level: f32) -> f32 {
    let banded = floor_level + 0.42 * smoothstep(0.22, 0.38, l) + 0.42 * smoothstep(0.70, 0.86, l);
    let bands = banded + max(l - 1.0, 0.0);
    return mix(l, bands, look.light.y);
}

// Repaints PBR light: the ramp on the illumination, then a hue shift of shadowed light.
// Snow (0..1) lifts the ramp's floor to look.relief.z and takes only look.relief.w of the hue.
fn paint_light(lit: vec3<f32>, albedo: vec3<f32>, snow: f32) -> vec3<f32> {
    let l = dot(lit, LUMA) / max(dot(albedo, LUMA), 1e-4) / max(look.light.x, 1e-6);
    var c = lit * (ramp(l, mix(0.16, look.relief.z, snow)) / max(l, 1e-4));
    let shade = 1.0 - smoothstep(0.12, 0.6, l);
    c = c * mix(vec3<f32>(1.0), look.shadow.rgb, shade * look.shadow.a * mix(1.0, look.relief.w, snow));
    return c;
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
    let shading = gradient * look.relief.x + curve;
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
    let wireframe = params.debug.x > 0.5;
    if wireframe {
        albedo = mix(albedo, lod_tint(in.lod), 0.55);
    }
    pbr_input.material.base_color = vec4<f32>(albedo, 1.0) * pbr_input.material.base_color;

    var out: FragmentOutput;
    let lit = apply_pbr_lighting(pbr_input);
    out.color = vec4<f32>(paint_light(lit.rgb, pbr_input.material.base_color.rgb, painted.snow), lit.a);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    if wireframe {
        let line = 1.0 - smoothstep(0.5, 1.5, edge_pixels(in.grid));
        out.color = vec4<f32>(mix(out.color.rgb, vec3<f32>(0.02, 0.02, 0.03), line * 0.85), out.color.a);
    }
    return out;
}
