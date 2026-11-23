// assets/shaders/voxel_surface_nets.wesl
// WebGPU + Vulkan compatible compute shader for read-only sliding-window voxel meshing.
// Uses Bevy 0.20 WESL module conventions with pure 32-bit atomics and flat storage buffers.

struct ChunkUniforms {
    // World-space origin of the chunk's (0, 0, 0) voxel corner
    chunk_origin: vec3<f32>,
    // Size of a single voxel cell in meters (scales with LOD tier)
    voxel_size: f32,
    // Grid dimensions per axis (e.g., 34 = 32 active cells + 1-voxel padding on each side)
    grid_dim: u32,
    // World-space camera anchor for radial detail fade-out
    window_center: vec3<f32>,
    // Inner radius where full 3D voxel detail is active
    inner_radius: f32,
    // Outer radius where 3D noise attenuates to 0.0 to stitch flush with macro heightmap
    outer_radius: f32,
    // Max vertex/index capacity allocated for this chunk slot in the pool
    max_vertices: u32,
    max_indices: u32,
};

struct VoxelVertex {
    position: vec3<f32>,
    _pad0: f32,
    normal: vec3<f32>,
    material_id: u32,
};

struct DrawCounters {
    vertex_count: atomic<u32>,
    index_count: atomic<u32>,
};

@group(0) @binding(0) var<uniform> params: ChunkUniforms;
@group(0) @binding(1) var<storage, read_write> density_grid: array<f32>;
@group(0) @binding(2) var<storage, read_write> cell_vertex_indices: array<u32>;
@group(0) @binding(3) var<storage, read_write> out_vertices: array<VoxelVertex>;
@group(0) @binding(4) var<storage, read_write> out_indices: array<u32>;
@group(0) @binding(5) var<storage, read_write> counters: DrawCounters;

// Optional: Macro heightmap texture sampled via textureLoad / bilinear math
// so compute shaders do not require non-filtering sampler workarounds.
@group(0) @binding(6) var macro_heightmap: texture_2d<f32>;

fn linear_index(x: u32, y: u32, z: u32) -> u32 {
    let d = params.grid_dim;
    return x + y * d + z * d * d;
}

// Deterministic 3D hash/gradient noise (must mirror CPU-side grounding evaluation)
fn hash31(p: vec3<f32>) -> f32 {
    var p3 = fract(p * 0.1031);
    p3 += dot(p3, p3.zyx + 31.32);
    return fract((p3.x + p3.y) * p3.z);
}

fn value_noise_3d(x: vec3<f32>) -> f32 {
    let i = floor(x);
    let f = fract(x);
    let u = f * f * (3.0 - 2.0 * f);

    return mix(
        mix(
            mix(hash31(i + vec3<f32>(0.0, 0.0, 0.0)), hash31(i + vec3<f32>(1.0, 0.0, 0.0)), u.x),
            mix(hash31(i + vec3<f32>(0.0, 1.0, 0.0)), hash31(i + vec3<f32>(1.0, 1.0, 0.0)), u.x),
            u.y
        ),
        mix(
            mix(hash31(i + vec3<f32>(0.0, 0.0, 1.0)), hash31(i + vec3<f32>(1.0, 0.0, 1.0)), u.x),
            mix(hash31(i + vec3<f32>(0.0, 1.0, 1.0)), hash31(i + vec3<f32>(1.0, 1.0, 1.0)), u.x),
            u.y
        ),
        u.z
    ) * 2.0 - 1.0;
}

fn fbm_3d(p: vec3<f32>) -> f32 {
    var value = 0.0;
    var amp = 0.5;
    var freq = 1.0;
    for (var i = 0; i < 4; i = i + 1) {
        value += amp * value_noise_3d(p * freq);
        freq *= 2.03;
        amp *= 0.5;
    }
    return value;
}

// Sample macro terrain height H(x, z) for seamless terrain-to-voxel coupling
fn sample_macro_height(world_xz: vec2<f32>) -> f32 {
    // Replace with textureLoad bilinear interpolation into `macro_heightmap`
    // using world-to-UV terrain bounds.
    let wave = sin(world_xz.x * 0.01) * cos(world_xz.y * 0.01) * 24.0;
    return wave;
}

// Signed density field: > 0.0 is solid rock/seabed, < 0.0 is air or water
fn evaluate_density(world_pos: vec3<f32>) -> f32 {
    let macro_h = sample_macro_height(world_pos.xz);

    // Radial attenuation mask so outer voxel edges merge flush with macro heightmap
    let dist_xz = distance(world_pos.xz, params.window_center.xz);
    let blend = 1.0 - smoothstep(params.inner_radius, params.outer_radius, dist_xz);

    // 3D overhangs, sea arches, and reef structures
    let detail_noise = fbm_3d(world_pos * 0.045) * 14.0;

    return (macro_h - world_pos.y) + detail_noise * blend;
}

fn estimate_normal(world_pos: vec3<f32>) -> vec3<f32> {
    let eps = params.voxel_size * 0.5;
    let dx = evaluate_density(world_pos + vec3<f32>(eps, 0.0, 0.0)) - evaluate_density(world_pos - vec3<f32>(eps, 0.0, 0.0));
    let dy = evaluate_density(world_pos + vec3<f32>(0.0, eps, 0.0)) - evaluate_density(world_pos - vec3<f32>(0.0, eps, 0.0));
    let dz = evaluate_density(world_pos + vec3<f32>(0.0, 0.0, eps)) - evaluate_density(world_pos - vec3<f32>(0.0, 0.0, eps));
    // Negative gradient points outward from solid (>0) toward air/water (<0)
    return normalize(-vec3<f32>(dx, dy, dz) + vec3<f32>(0.0, 1e-5, 0.0));
}

// PASS 1: Evaluate signed density across the padded chunk grid
@compute @workgroup_size(4, 4, 4)
fn sample_density_pass(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.grid_dim || gid.y >= params.grid_dim || gid.z >= params.grid_dim) {
        return;
    }

    let world_pos = params.chunk_origin + vec3<f32>(gid) * params.voxel_size;
    let idx = linear_index(gid.x, gid.y, gid.z);
    density_grid[idx] = evaluate_density(world_pos);
    cell_vertex_indices[idx] = 0xFFFFFFFFu;
}

// PASS 2: Naive Surface Nets vertex placement for sign-changing cells
@compute @workgroup_size(4, 4, 4)
fn generate_vertices_pass(@builtin(global_invocation_id) gid: vec3<u32>) {
    let max_cell = params.grid_dim - 1u;
    if (gid.x >= max_cell || gid.y >= max_cell || gid.z >= max_cell) {
        return;
    }

    // Sample 8 corners of the cell
    var corners: array<f32, 8>;
    var mask = 0u;
    for (var i = 0u; i < 8u; i = i + 1u) {
        let ox = i & 1u;
        let oy = (i >> 1u) & 1u;
        let oz = (i >> 2u) & 1u;
        let d = density_grid[linear_index(gid.x + ox, gid.y + oy, gid.z + oz)];
        corners[i] = d;
        if (d > 0.0) {
            mask |= (1u << i);
        }
    }

    // Skip completely empty (0) or completely solid (255) cells
    if (mask == 0u || mask == 255u) {
        return;
    }

    // Average zero-crossing positions along the 12 cube edges
    var accum_pos = vec3<f32>(0.0);
    var crossings = 0.0;

    // 12 edges defined by corner index pairs
    let edge_a = array<u32, 12>(0u, 2u, 4u, 6u, 0u, 1u, 4u, 5u, 0u, 1u, 2u, 3u);
    let edge_b = array<u32, 12>(1u, 3u, 5u, 7u, 2u, 3u, 6u, 7u, 4u, 5u, 6u, 7u);

    for (var e = 0u; e < 12u; e = e + 1u) {
        let a = edge_a[e];
        let b = edge_b[e];
        let da = corners[a];
        let db = corners[b];

        if ((da > 0.0) != (db > 0.0)) {
            let t = clamp(da / (da - db), 0.0, 1.0);
            let pa = vec3<f32>(f32(a & 1u), f32((a >> 1u) & 1u), f32((a >> 2u) & 1u));
            let pb = vec3<f32>(f32(b & 1u), f32((b >> 1u) & 1u), f32((b >> 2u) & 1u));
            accum_pos += mix(pa, pb, t);
            crossings += 1.0;
        }
    }

    let local_pos = accum_pos / max(crossings, 1.0);
    let world_pos = params.chunk_origin + (vec3<f32>(gid) + local_pos) * params.voxel_size;

    // 32-bit atomic allocation (strictly WebGPU + Vulkan compliant)
    let v_idx = atomicAdd(&counters.vertex_count, 1u);
    if (v_idx >= params.max_vertices) {
        return;
    }

    cell_vertex_indices[linear_index(gid.x, gid.y, gid.z)] = v_idx;

    // Material ID: e.g., 0 = rock/cliff, 1 = seabed/sand below water level (y < 0.0)
    let mat_id = select(0u, 1u, world_pos.y < 0.0);

    out_vertices[v_idx] = VoxelVertex(
        world_pos,
        0.0,
        estimate_normal(world_pos),
        mat_id
    );
}

// PASS 3: Emit quads (2 triangles) for interior edges crossing the isosurface
@compute @workgroup_size(4, 4, 4)
fn generate_indices_pass(@builtin(global_invocation_id) gid: vec3<u32>) {
    let max_active = params.grid_dim - 2u;
    if (gid.x == 0u || gid.y == 0u || gid.z == 0u ||
        gid.x >= max_active || gid.y >= max_active || gid.z >= max_active) {
        return;
    }

    let d0 = density_grid[linear_index(gid.x, gid.y, gid.z)];

    // Check 3 positive axis edges (+X, +Y, +Z) owned by this grid point
    // 1. +X edge: connects cells (x, y-1, z-1), (x, y, z-1), (x, y, z), (x, y-1, z)
    let dx = density_grid[linear_index(gid.x + 1u, gid.y, gid.z)];
    if ((d0 > 0.0) != (dx > 0.0)) {
        emit_quad(
            cell_vertex_indices[linear_index(gid.x, gid.y - 1u, gid.z - 1u)],
            cell_vertex_indices[linear_index(gid.x, gid.y, gid.z - 1u)],
            cell_vertex_indices[linear_index(gid.x, gid.y, gid.z)],
            cell_vertex_indices[linear_index(gid.x, gid.y - 1u, gid.z)],
            d0 > 0.0
        );
    }

    // 2. +Y edge: connects cells (x-1, y, z-1), (x-1, y, z), (x, y, z), (x, y, z-1)
    let dy = density_grid[linear_index(gid.x, gid.y + 1u, gid.z)];
    if ((d0 > 0.0) != (dy > 0.0)) {
        emit_quad(
            cell_vertex_indices[linear_index(gid.x - 1u, gid.y, gid.z - 1u)],
            cell_vertex_indices[linear_index(gid.x - 1u, gid.y, gid.z)],
            cell_vertex_indices[linear_index(gid.x, gid.y, gid.z)],
            cell_vertex_indices[linear_index(gid.x, gid.y, gid.z - 1u)],
            d0 > 0.0
        );
    }

    // 3. +Z edge: connects cells (x-1, y-1, z), (x, y-1, z), (x, y, z), (x-1, y, z)
    let dz = density_grid[linear_index(gid.x, gid.y, gid.z + 1u)];
    if ((d0 > 0.0) != (dz > 0.0)) {
        emit_quad(
            cell_vertex_indices[linear_index(gid.x - 1u, gid.y - 1u, gid.z)],
            cell_vertex_indices[linear_index(gid.x, gid.y - 1u, gid.z)],
            cell_vertex_indices[linear_index(gid.x, gid.y, gid.z)],
            cell_vertex_indices[linear_index(gid.x - 1u, gid.y, gid.z)],
            d0 > 0.0
        );
    }
}

fn emit_quad(v0: u32, v1: u32, v2: u32, v3: u32, flip_winding: bool) {
    if (v0 == 0xFFFFFFFFu || v1 == 0xFFFFFFFFu || v2 == 0xFFFFFFFFu || v3 == 0xFFFFFFFFu) {
        return;
    }

    let i_idx = atomicAdd(&counters.index_count, 6u);
    if (i_idx + 6u > params.max_indices) {
        return;
    }

    if (flip_winding) {
        out_indices[i_idx + 0u] = v0;
        out_indices[i_idx + 1u] = v1;
        out_indices[i_idx + 2u] = v2;
        out_indices[i_idx + 3u] = v0;
        out_indices[i_idx + 4u] = v2;
        out_indices[i_idx + 5u] = v3;
    } else {
        out_indices[i_idx + 0u] = v0;
        out_indices[i_idx + 1u] = v2;
        out_indices[i_idx + 2u] = v1;
        out_indices[i_idx + 3u] = v0;
        out_indices[i_idx + 4u] = v3;
        out_indices[i_idx + 5u] = v2;
    }
}
