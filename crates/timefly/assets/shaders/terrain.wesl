// The region terrain's main pass (M2 design §6.3): CDLOD patches displaced in the vertex stage,
// lit through Bevy's StandardMaterial PBR path (lights, shadows, fog, tonemapping).
//
// Every patch entity shares one grid mesh, whose positions are (i, 0, j) / quads, and one
// material. The entity's MeshTag indexes the patch buffer. The vertex math lives in
// terrain_cdlod.wesl, which terrain_prepass.wesl also calls, so shadows match.
//
// Task 7's fragment colour is a plain stand-in (height bands, ice from the surface code); Task 8
// replaces it with the painterly palette.

import package::terrain_cdlod::{
    TerrainBuffer, TerrainParams, cdlod_vertex, grid_index_of, terrain_normal,
};
import bevy_pbr::render::mesh_functions::get_tag;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::render::pbr_fragment::pbr_input_from_standard_material;
import bevy_pbr::render::pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing};
import bevy_pbr::render::forward_io::{VertexOutput, FragmentOutput};

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> terrain: TerrainBuffer;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var inner_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var apron_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(103) var inner_surface: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(104) var apron_surface: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(105) var palette: texture_2d<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

struct TerrainVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) world_position: vec4<f32>,
    @location(1) world_normal: vec3<f32>,
    // The unmorphed patch grid index, interpolated: integer lines are the triangle edges.
    @location(2) grid: vec2<f32>,
    @location(3) @interpolate(flat) lod: u32,
    @location(4) morph: f32,
    @location(5) @interpolate(flat) instance_index: u32,
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
    out.world_normal = terrain_normal(inner_height, apron_height, params, v.world.xz);
    out.grid = vec2<f32>(grid_index);
    out.lod = inst.cells.z;
    out.morph = v.k;
    out.instance_index = in.instance_index;
    return out;
}

// Surface code texel of the field that covers xz (the inner field inside it).
fn surface_code(params: TerrainParams, xz: vec2<f32>) -> vec4<f32> {
    let inside = max(abs(xz.x), abs(xz.y)) < params.inner.x * 0.5;
    let field = select(params.apron, params.inner, inside);
    let last = field.z - 1.0;
    let uv = clamp(round((xz + vec2<f32>(field.x * 0.5)) / field.y), vec2<f32>(0.0), vec2<f32>(last));
    if inside {
        return textureLoad(inner_surface, vec2<i32>(uv), 0);
    }
    return textureLoad(apron_surface, vec2<i32>(uv), 0);
}

// Task 7's stand-in albedo (linear): sea floor, lowland, upland, rock, snow; ice from the code.
fn stand_in_albedo(height: f32, normal: vec3<f32>, code: vec4<f32>) -> vec3<f32> {
    let sea_floor = vec3<f32>(0.10, 0.16, 0.20);
    let shore = vec3<f32>(0.42, 0.38, 0.27);
    let lowland = vec3<f32>(0.16, 0.24, 0.09);
    let upland = vec3<f32>(0.28, 0.25, 0.16);
    let rock = vec3<f32>(0.24, 0.23, 0.22);
    let snow = vec3<f32>(0.82, 0.84, 0.88);
    var c = mix(sea_floor, shore, smoothstep(-60.0, 0.0, height));
    c = mix(c, lowland, smoothstep(0.0, 40.0, height));
    c = mix(c, upland, smoothstep(600.0, 1800.0, height));
    c = mix(c, rock, smoothstep(0.45, 0.7, 1.0 - normal.y) * step(0.0, height));
    c = mix(c, snow, smoothstep(2600.0, 3200.0, height) * smoothstep(0.5, 0.75, normal.y));
    return mix(c, snow, code.z);
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
    // Per-pixel normals, so the lighting does not depend on the patch's LOD.
    let normal = terrain_normal(inner_height, apron_height, params, xz);

    var mesh: VertexOutput;
    mesh.position = in.position;
    mesh.world_position = in.world_position;
    mesh.world_normal = normal;
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
    // The drop moves the surface below the blended height; colour by the terrain's own height.
    let d = xz - params.camera.xz;
    let height = in.world_position.y + dot(d, d) / (2.0 * params.lattice.z);
    var albedo = stand_in_albedo(height, normal, surface_code(params, xz));
    let wireframe = params.debug.x > 0.5;
    if wireframe {
        albedo = mix(albedo, lod_tint(in.lod), 0.55);
    }
    pbr_input.material.base_color = vec4<f32>(albedo, 1.0) * pbr_input.material.base_color;

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    if wireframe {
        let line = 1.0 - smoothstep(0.5, 1.5, edge_pixels(in.grid));
        out.color = vec4<f32>(mix(out.color.rgb, vec3<f32>(0.02, 0.02, 0.03), line * 0.85), out.color.a);
    }
    return out;
}
