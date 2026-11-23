// CDLOD vertex math for the region terrain (M2 design §6.3), shared by terrain.wesl (the main
// pass) and terrain_prepass.wesl (depth, normal and shadow passes), so every pass displaces a
// vertex identically. Plain WGSL with no imports and no bindings: the GPU parity test
// (tests/region_gpu.rs) compiles it on its own with a compute entry point.
//
// It mirrors timefly_core::terrain operation for operation: `cdlod_morph` is `morph_vertex`,
// `blended_height` is `blended_height_m` and the drop in `cdlod_vertex` is `curvature_drop_m`.
// Heights are R32Float, which baseline WebGPU cannot filter, so they are read with textureLoad
// and interpolated by hand, as the Rust reference does.
//
// Units: region local metres, +x east, -z north, +y up, sea level at y = 0. Texture texel
// (col, row) holds field column col (west to east) and row row (north to south).

// The buffer header, written once per frame (`TerrainParams` in terrain/material.rs).
struct TerrainParams {
    // xyz: the main camera (local m); w: sea level (0, heights are effective elevation).
    // The camera comes from here, never from the view, so the shadow passes morph and drop
    // vertices exactly as the main pass does.
    camera: vec4<f32>,
    // x: finest quad (m); y: the root's minimum corner (m); z: Earth radius (m); w: quads per
    // patch side.
    lattice: vec4<f32>,
    // x: size (m); y: texel spacing (m); z: resolution; w: blend start e0 (m).
    inner: vec4<f32>,
    // x: size (m); y: texel spacing (m); z: resolution; w: blend end e1 (m).
    apron: vec4<f32>,
    // Reserved for M4's voxel-window clip (centre xz, inner clip radius, outer blend radius).
    // Unused: the draft's clipping is removed for now.
    voxel_window: vec4<f32>,
    // x > 0.5: wireframe view. yzw reserved.
    debug: vec4<f32>,
}

// One selected patch (`PatchInstance` in terrain/material.rs), indexed by the entity's MeshTag.
struct PatchInstance {
    // xy: origin in finest-grid cells from the root's minimum corner; z: LOD; w: 2^LOD.
    cells: vec4<u32>,
    // x: morph start (m); y: morph end (m); z: patch size (m); w: reserved.
    morph: vec4<f32>,
}

struct TerrainBuffer {
    params: TerrainParams,
    patches: array<PatchInstance>,
}

struct Morphed {
    xz: vec2<f32>,
    k: f32,
}

struct CdlodVertex {
    // The displaced position: morphed xz, blended height minus the curvature drop.
    world: vec3<f32>,
    // The blended terrain height at the morphed xz, before the drop.
    height: f32,
    // The morph factor.
    k: f32,
}

// The whole-number patch grid index of a mesh vertex, whose position is (i, 0, j) / quads.
// Snapping by index (spec §7.2), never by rounding world positions.
fn grid_index_of(position: vec3<f32>, params: TerrainParams) -> vec2<u32> {
    return vec2<u32>(round(position.xz * params.lattice.w));
}

// `morph_vertex`: cells are exact integers; odd indices slide toward the even index below
// (ruling R12) by the morph factor, in index space.
fn cdlod_morph(grid_index: vec2<u32>, inst: PatchInstance, params: TerrainParams) -> Morphed {
    let quad = params.lattice.x;
    let root = params.lattice.y;
    let cell = vec2<f32>(inst.cells.xy + grid_index * inst.cells.w);
    let p = cell * quad + root;
    let cam = params.camera.xyz;
    let dx = p.x - cam.x;
    let dz = p.y - cam.z;
    let cy = cam.y;
    let d = sqrt((dx * dx + cy * cy) + dz * dz);
    let k = clamp((d - inst.morph.x) / (inst.morph.y - inst.morph.x), 0.0, 1.0);
    let odd = vec2<f32>(grid_index & vec2<u32>(1u));
    let xz = (cell - odd * k * f32(inst.cells.w)) * quad + root;
    return Morphed(xz, k);
}

// Bilinear height of one field with the texel coordinate clamped to it (`sample_clamped`).
// field: (size m, spacing m, resolution, unused).
fn field_height(tex: texture_2d<f32>, field: vec4<f32>, xz: vec2<f32>) -> f32 {
    let last = field.z - 1.0;
    let uv = clamp((xz + vec2<f32>(field.x * 0.5)) / field.y, vec2<f32>(0.0), vec2<f32>(last));
    let c0 = min(floor(uv), vec2<f32>(last - 1.0));
    let f = uv - c0;
    let i = vec2<i32>(c0);
    let h00 = textureLoad(tex, i, 0).r;
    let h10 = textureLoad(tex, i + vec2<i32>(1, 0), 0).r;
    let h01 = textureLoad(tex, i + vec2<i32>(0, 1), 0).r;
    let h11 = textureLoad(tex, i + vec2<i32>(1, 1), 0).r;
    let top = h00 + (h10 - h00) * f.x;
    let bottom = h01 + (h11 - h01) * f.x;
    return top + (bottom - top) * f.y;
}

// `blended_height_m`: the inner field, the apron, and a smoothstep on max(|x|, |z|) between
// e0 and e1. Each side is skipped where its weight is zero.
fn blended_height(
    inner: texture_2d<f32>,
    apron: texture_2d<f32>,
    params: TerrainParams,
    xz: vec2<f32>,
) -> f32 {
    let a = max(abs(xz.x), abs(xz.y));
    let s = smoothstep(params.inner.w, params.apron.w, a);
    if s <= 0.0 {
        return field_height(inner, params.inner, xz);
    }
    let ha = field_height(apron, params.apron, xz);
    if s >= 1.0 {
        return ha;
    }
    let hi = field_height(inner, params.inner, xz);
    return hi + (ha - hi) * s;
}

// One texel of the field that dominates at xz: the inner field's inside the blend's end, the
// apron's beyond it.
fn texel_step(params: TerrainParams, xz: vec2<f32>) -> f32 {
    let a = max(abs(xz.x), abs(xz.y));
    return select(params.apron.y, params.inner.y, a < params.apron.w);
}

// The surface normal by central differences over one texel.
fn terrain_normal(
    inner: texture_2d<f32>,
    apron: texture_2d<f32>,
    params: TerrainParams,
    xz: vec2<f32>,
) -> vec3<f32> {
    let e = texel_step(params, xz);
    let west = blended_height(inner, apron, params, xz - vec2<f32>(e, 0.0));
    let east = blended_height(inner, apron, params, xz + vec2<f32>(e, 0.0));
    let north = blended_height(inner, apron, params, xz - vec2<f32>(0.0, e));
    let south = blended_height(inner, apron, params, xz + vec2<f32>(0.0, e));
    return normalize(vec3<f32>(west - east, 2.0 * e, north - south));
}

// The displaced vertex: morph, then the blended height at the morphed xz, then the curvature
// drop d² / 2R for the horizontal distance d from the camera (the region itself is flat).
fn cdlod_vertex(
    grid_index: vec2<u32>,
    inst: PatchInstance,
    params: TerrainParams,
    inner: texture_2d<f32>,
    apron: texture_2d<f32>,
) -> CdlodVertex {
    let m = cdlod_morph(grid_index, inst, params);
    let h = blended_height(inner, apron, params, m.xz);
    let d = m.xz - params.camera.xz;
    let drop = dot(d, d) / (2.0 * params.lattice.z);
    return CdlodVertex(vec3<f32>(m.xz.x, h - drop, m.xz.y), h, m.k);
}
