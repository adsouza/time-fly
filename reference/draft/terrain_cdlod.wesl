// terrain_cdlod.wesl
// Target: Bevy 0.20 (WebGPU + Vulkan compatible WESL/WGSL)
// Purpose: Continuous Distance-Dependent LOD (CDLOD) macro terrain vertex/fragment shader
//          with grid-snapped geomorphing, finite-difference normals, inner-voxel-window
//          discard clipping, and depth-aware underwater fog/caustics.

import bevy_pbr::{
    mesh_view_bindings::view,
    forward_io::VertexOutput,
    lighting,
}

struct TerrainUniforms {
    // Camera world position (xyz) + sea level Y (w)
    camera_pos_and_sea_level: vec4<f32>,
    // World-space center of the active sliding voxel window (xz) + inner clip radius (z) + outer blend radius (w)
    voxel_window: vec4<f32>,
    // World size of the heightmap (xy), max vertical elevation scale (z), heightmap texel size (w)
    terrain_scale: vec4<f32>,
    // Morph start ratio (e.g. 0.70), morph end ratio (e.g. 0.98), grid resolution (e.g. 64.0), time in seconds (w)
    morph_and_time: vec4<f32>,
}

struct PatchInstance {
    // Patch world origin (xz), patch world width (z), LOD ring base morph end distance (w)
    patch_rect: vec4<f32>,
    // Grid vertex spacing at this LOD level (x), morph range span (y), reserved (zw)
    lod_params: vec4<f32>,
}

@group(2) @binding(0) var<uniform> terrain: TerrainUniforms;
@group(2) @binding(1) var heightmap_tex: texture_2d<f32>;
@group(2) @binding(2) var heightmap_sampler: sampler;
@group(2) @binding(3) var<storage, read> instances: array<PatchInstance>;

struct TerrainVertexInput {
    // Normalized grid coordinates in [0.0, 1.0] for an N x N static patch mesh
    @location(0) grid_uv: vec2<f32>,
    @builtin(instance_index) instance_idx: u32,
}

struct TerrainVertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) heightmap_uv: vec2<f32>,
    @location(2) morph_factor: f32,
}

fn world_to_height_uv(world_xz: vec2<f32>) -> vec2<f32> {
    // Map centered world coordinates [-size/2, +size/2] to [0.0, 1.0]
    return (world_xz / terrain.terrain_scale.xy) + vec2<f32>(0.5);
}

fn sample_height(world_xz: vec2<f32>) -> f32 {
    let uv = clamp(world_to_height_uv(world_xz), vec2<f32>(0.0), vec2<f32>(1.0));
    let h = textureSampleLevel(heightmap_tex, heightmap_sampler, uv, 0.0).r;
    return h * terrain.terrain_scale.z;
}

// Smoothly snaps fine grid coordinates onto the next coarser 2x2 parent grid cell
fn morph_grid_position(world_xz: vec2<f32>, grid_spacing: f32, morph_factor: f32) -> vec2<f32> {
    let parent_spacing = grid_spacing * 2.0;
    let snapped_parent = floor((world_xz / parent_spacing) + vec2<f32>(0.5)) * parent_spacing;
    return mix(world_xz, snapped_parent, morph_factor);
}

@vertex
fn vertex(in: TerrainVertexInput) -> TerrainVertexOutput {
    let inst = instances[in.instance_idx];
    let patch_origin = inst.patch_rect.xy;
    let patch_size = inst.patch_rect.z;
    let morph_end_dist = inst.patch_rect.w;
    let grid_spacing = inst.lod_params.x;
    let morph_span = max(inst.lod_params.y, 0.001);

    // 1. Unmorphed world XZ position on the fine grid
    let base_xz = patch_origin + in.grid_uv * patch_size;

    // 2. Evaluate preliminary height and camera distance for CDLOD morph factor
    let base_y = sample_height(base_xz);
    let cam_pos = terrain.camera_pos_and_sea_level.xyz;
    let dist = distance(cam_pos, vec3<f32>(base_xz.x, base_y, base_xz.y));

    let morph_start = morph_end_dist - morph_span;
    let morph_factor = clamp((dist - morph_start) / morph_span, 0.0, 1.0);

    // 3. Geomorph XZ toward parent grid cell and re-sample height to eliminate T-junctions
    let morphed_xz = morph_grid_position(base_xz, grid_spacing, morph_factor);
    let morphed_y = sample_height(morphed_xz);
    let world_pos = vec3<f32>(morphed_xz.x, morphed_y, morphed_xz.y);

    var out: TerrainVertexOutput;
    out.clip_position = view.clip_from_world * vec4<f32>(world_pos, 1.0);
    out.world_pos = world_pos;
    out.heightmap_uv = world_to_height_uv(morphed_xz);
    out.morph_factor = morph_factor;
    return out;
}

// Compute terrain normal via central finite differences over the heightmap
fn compute_terrain_normal(world_xz: vec2<f32>, step_size: f32) -> vec3<f32> {
    let h_l = sample_height(world_xz - vec2<f32>(step_size, 0.0));
    let h_r = sample_height(world_xz + vec2<f32>(step_size, 0.0));
    let h_d = sample_height(world_xz - vec2<f32>(0.0, step_size));
    let h_u = sample_height(world_xz + vec2<f32>(0.0, step_size));
    return normalize(vec3<f32>(h_l - h_r, 2.0 * step_size, h_d - h_u));
}

// Procedural dual-layer caustic shimmer for submerged macro seafloor
fn compute_caustics(world_xz: vec2<f32>, time_sec: f32) -> f32 {
    let p1 = world_xz * 0.18 + vec2<f32>(time_sec * 0.35, -time_sec * 0.25);
    let p2 = world_xz * 0.23 + vec2<f32>(-time_sec * 0.28, time_sec * 0.31);
    let wave1 = sin(p1.x + cos(p1.y * 1.3));
    let wave2 = cos(p2.y + sin(p2.x * 1.2));
    return pow(clamp(0.5 + 0.5 * (wave1 * wave2), 0.0, 1.0), 4.0);
}

@fragment
fn fragment(in: TerrainVertexOutput) -> @location(0) vec4<f32> {
    // 1. Inner-Voxel-Window Clipping
    // Discard macro terrain fragments inside the active micro-voxel radius so read-only
    // procedural voxels render cleanly without z-fighting. Leave a small overlap margin
    // (e.g. inner_clip_radius < outer_blend_radius) where voxel density has already converged
    // to H(x, z) for a watertight seam.
    let voxel_center_xz = terrain.voxel_window.xy;
    let inner_clip_radius = terrain.voxel_window.z;
    let dist_to_voxel_center = distance(in.world_pos.xz, voxel_center_xz);
    if (dist_to_voxel_center < inner_clip_radius) {
        discard;
    }

    // 2. Surface Normal & Slope/Elevation Material Blending
    let texel_world_step = terrain.terrain_scale.x * terrain.terrain_scale.w;
    let normal = compute_terrain_normal(in.world_pos.xz, max(texel_world_step, 0.5));
    let slope = 1.0 - clamp(normal.y, 0.0, 1.0);

    let sea_level = terrain.camera_pos_and_sea_level.w;
    let altitude = in.world_pos.y - sea_level;

    let sand_color = vec3<f32>(0.72, 0.67, 0.52);
    let grass_color = vec3<f32>(0.22, 0.38, 0.19);
    let rock_color = vec3<f32>(0.36, 0.35, 0.33);
    let snow_color = vec3<f32>(0.88, 0.90, 0.93);

    // Blend beach sand near sea level into grass, steep rock, and high-altitude snow
    let beach_mask = 1.0 - smoothstep(-2.0, 4.5, altitude);
    var albedo = mix(grass_color, sand_color, beach_mask);
    albedo = mix(albedo, rock_color, smoothstep(0.28, 0.58, slope));
    albedo = mix(albedo, snow_color, smoothstep(180.0, 260.0, altitude) * (1.0 - smoothstep(0.45, 0.70, slope)));

    // 3. Directional Key Light + Hemisphere Ambient
    let sun_dir = normalize(vec3<f32>(0.45, 0.80, 0.38));
    let n_dot_l = max(dot(normal, sun_dir), 0.0);
    let sky_ambient = mix(vec3<f32>(0.14, 0.16, 0.18), vec3<f32>(0.34, 0.42, 0.52), normal.y * 0.5 + 0.5);
    var lit_color = albedo * (sky_ambient + vec3<f32>(1.05, 0.98, 0.90) * n_dot_l);

    // 4. Underwater Seafloor Caustics & Depth Attenuation
    if (altitude < 0.0) {
        let depth = abs(altitude);
        let time_sec = terrain.morph_and_time.w;
        let caustics = compute_caustics(in.world_pos.xz, time_sec) * exp(-depth * 0.045) * n_dot_l;
        lit_color += vec3<f32>(0.18, 0.34, 0.38) * caustics;

        // Beer-Lambert style depth absorption + underwater fog tint
        let water_tint = vec3<f32>(0.04, 0.22, 0.30);
        let absorption = clamp(1.0 - exp(-depth * 0.035), 0.0, 0.92);
        lit_color = mix(lit_color, water_tint, absorption);
    }

    // 5. Atmospheric Distance Fog (or Underwater Camera Fog)
    let cam_pos = terrain.camera_pos_and_sea_level.xyz;
    let cam_dist = distance(cam_pos, in.world_pos);
    let is_cam_underwater = cam_pos.y < sea_level;
    let fog_color = select(vec3<f32>(0.68, 0.78, 0.88), vec3<f32>(0.03, 0.18, 0.26), is_cam_underwater);
    let fog_density = select(0.00035, 0.018, is_cam_underwater);
    let fog_factor = 1.0 - exp(-cam_dist * fog_density);
    lit_color = mix(lit_color, fog_color, clamp(fog_factor, 0.0, 1.0));

    return vec4<f32>(lit_color, 1.0);
}
