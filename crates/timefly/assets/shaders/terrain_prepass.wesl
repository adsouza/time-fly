// The region terrain's prepass vertex shader: depth, normal and motion-vector prepasses and
// every shadow-map pass. It displaces through terrain_cdlod.wesl exactly as terrain.wesl does,
// from the main camera in the patch buffer (not this pass's view), so depth and shadows sit on
// the drawn surface. It writes Bevy's prepass VertexOutput, so StandardMaterial's prepass
// fragment shader runs unchanged after it when a pass needs one.

import package::terrain_cdlod::{TerrainBuffer, cdlod_vertex, grid_index_of, terrain_normal};
import bevy_pbr::render::mesh_functions::get_tag;
import bevy_pbr::render::view_transformations::position_world_to_clip;
import bevy_pbr::prepass::io::VertexOutput;

@group(constants::MATERIAL_BIND_GROUP) @binding(100) var<storage, read> terrain: TerrainBuffer;
@group(constants::MATERIAL_BIND_GROUP) @binding(101) var inner_height: texture_2d<f32>;
@group(constants::MATERIAL_BIND_GROUP) @binding(102) var apron_height: texture_2d<f32>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
}

@vertex
fn vertex(in: Vertex) -> VertexOutput {
    let params = terrain.params;
    let inst = terrain.patches[get_tag(in.instance_index)];
    let grid_index = grid_index_of(in.position, params);
    let v = cdlod_vertex(grid_index, inst, params, inner_height, apron_height);
    var out: VertexOutput;
    out.world_position = vec4<f32>(v.world, 1.0);
    out.position = position_world_to_clip(v.world);
@if(UNCLIPPED_DEPTH_ORTHO_EMULATION) {
    out.unclipped_depth = out.position.z;
    out.position.z = min(out.position.z, 1.0);
}
    @if(VERTEX_UVS_A)
    out.uv = vec2<f32>(grid_index) / params.lattice.w;
    @if(VERTEX_UVS_B)
    out.uv_b = vec2<f32>(grid_index) / params.lattice.w;
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS)
    out.world_normal = terrain_normal(inner_height, apron_height, params, v.world.xz);
    @if(NORMAL_PREPASS_OR_DEFERRED_PREPASS && VERTEX_TANGENTS)
    out.world_tangent = vec4<f32>(1.0, 0.0, 0.0, 1.0);
    @if(VERTEX_COLORS)
    out.color = vec4<f32>(1.0);
    // The patch buffer holds only this frame, so a vertex's last position is not known; the
    // terrain itself does not move.
    @if(MOTION_VECTOR_PREPASS)
    out.previous_world_position = out.world_position;
    @if(VERTEX_OUTPUT_INSTANCE_INDEX)
    out.instance_index = in.instance_index;
    @if(VISIBILITY_RANGE_DITHER)
    out.visibility_range_dither = 0;
    return out;
}
