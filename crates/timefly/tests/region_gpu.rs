//! GPU golden tests for the region shaders. Need a GPU adapter:
//! `cargo test -p timefly --test region_gpu -- --ignored`.
//!
//! - `wesl_noise_matches_golden` compiles `assets/shaders/noise.wesl` (plain WGSL, no imports)
//!   with a small compute entry point on a headless wgpu device, runs it over the golden inputs
//!   that the Rust implementation also checks (`timefly_core/tests/fixtures/noise_golden.json`)
//!   and compares the values.
//! - `wesl_morph_matches_rust` does the same with `assets/shaders/terrain_cdlod.wesl`, the vertex
//!   math every terrain pass runs, against `timefly_core::terrain`'s `morph_vertex`,
//!   `blended_height_m` and `curvature_drop_m`, fed through the shell's own buffer packing.
//! - `wesl_normal_matches_rust` runs `terrain_normal` (the per-pixel lighting normal, with the
//!   curvature tilt) against `timefly_core::terrain::surface_normal`.
//! - `wesl_snowline_matches_rust` runs `snowline.wesl` (with ruling R18's glacial lowering and
//!   ruling R27's dryness raise decoded from the surface byte) against
//!   `timefly_core::worldgen::biome::snowline_with_raise_m`.
//! - `wesl_planet_position_matches_rust` runs `planet.wesl`'s `planet_position_m` against
//!   `EARTH_RADIUS_M * FrameBasis::local_to_dir`.
//! - `wesl_water_matches_rust` runs `terrain_cdlod.wesl`'s `blended_water` (the water shader's
//!   sampler, with its crossing guard) against `timefly_core::terrain::blended_water`, over
//!   random water fields with rivers read through the shell's own `water_image` textures.
//! - `the_cloud_veils_geometry_nearer_than_its_plane` draws `cloud.wesl` (its Bevy and package
//!   imports stood in for) over an opaque quad nearer than 1.2 × the eye-to-fly distance, with
//!   the game's blend and reverse-Z depth test, and checks the quad comes out veiled exactly as
//!   far ground does (M3 Task 7's near-plane depth rule).
//! - `the_gpu_frame_time_is_measured_on_this_gpu` times frames of 0, 1, 4 and 16 busy full-screen
//!   passes with the game's `GpuTimer` (pass timestamps): positive, close to linear in the
//!   passes, and not inflated when frames run back to back (rivers and lakes Task 10).

use bevy::math::{DVec3, Vec3};
use bevy::tasks::block_on;
use timefly::terrain::material::{TerrainParams, patch_instances, terrain_buffer_bytes};
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};
use timefly_core::terrain::{
    LodSettings, blended_height_m, curvature_drop_m, morph_factor, morph_vertex, select_patches,
    surface_normal,
};
use timefly_core::worldgen::generate::{Field, RegionGrid, RegionSpec};
use timefly_core::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};
use wgpu::util::DeviceExt;

const NOISE: &str = include_str!("../assets/shaders/noise.wesl");
const GOLDEN: &str = include_str!("../../timefly_core/tests/fixtures/noise_golden.json");
const TOLERANCE: f32 = 1e-5;

const ENTRY: &str = r#"
@group(0) @binding(0) var<storage, read> cells: array<vec4<i32>>;   // (cell.xyz, seed as bits)
@group(0) @binding(1) var<storage, read> fracs: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= arrayLength(&out)) {
        return;
    }
    let c = cells[i];
    out[i] = gradient_noise3(c.xyz, fracs[i].xyz, bitcast<u32>(c.w));
}
"#;

#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn wesl_noise_matches_golden() {
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).expect("golden json");
    let rows = golden["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 256);
    let mut cells: Vec<[i32; 4]> = Vec::new();
    let mut fracs: Vec<[f32; 4]> = Vec::new();
    let mut want: Vec<f32> = Vec::new();
    for r in rows {
        let c: Vec<i64> = r["cell"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap())
            .collect();
        let f: Vec<f32> = r["frac"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_f64().unwrap() as f32)
            .collect();
        let seed = r["seed"].as_u64().unwrap() as u32;
        cells.push([c[0] as i32, c[1] as i32, c[2] as i32, seed as i32]);
        fracs.push([f[0], f[1], f[2], 0.0]);
        want.push(r["value"].as_f64().unwrap() as f32);
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    println!("adapter: {:?}", adapter.get_info());
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("noise golden"),
        source: wgpu::ShaderSource::Wgsl(format!("{NOISE}\n{ENTRY}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("noise golden"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });

    let input = |label, data: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: wgpu::BufferUsages::STORAGE,
        })
    };
    let cells_buf = input("cells", bytemuck::cast_slice(&cells));
    let fracs_buf = input("fracs", bytemuck::cast_slice(&fracs));
    let bytes = (want.len() * 4) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: cells_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: fracs_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(want.len().div_ceil(64) as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let got: Vec<f32> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();

    let mut worst = 0.0f32;
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        let d = (g - w).abs();
        worst = worst.max(d);
        assert!(d <= TOLERANCE, "row {i}: gpu {g} vs rust {w} (diff {d})");
    }
    println!(
        "largest Rust/WESL difference over {} rows: {worst:e}",
        want.len()
    );
}

// ---------------------------------------------------------------------------------------------
// CDLOD morph and heights

const CDLOD: &str = include_str!("../assets/shaders/terrain_cdlod.wesl");
/// The floor of the xz tolerance (ruling R13).
const XZ_TOLERANCE_M: f32 = 1e-3;
/// The height: the Rust reference takes a few f64 steps that f32 can only follow to rounding.
const HEIGHT_TOLERANCE_M: f32 = 0.5;
/// Vertices checked, over [`MORPH_CAMERAS`] cameras (a multiple of their count).
const MORPH_VERTICES: usize = 520;

/// Cameras: low over the centre, over the blend edge, high and far out, at M2's 60 km ceiling,
/// and the chase camera over a fly at the cloud base (M3: about 350 km up).
const MORPH_CAMERAS: [[f32; 3]; 5] = [
    [1_234.5, 150.0, -2_345.25],
    [-131_000.0, 2_000.0, 140_500.0],
    [149_000.0, 8_000.0, -98_765.0],
    [-60_000.0, 60_000.0, 20_000.0],
    [90_000.0, 351_300.0, 140_900.0],
];

const MORPH_ENTRY: &str = r#"
@group(0) @binding(0) var<storage, read> terrain: TerrainBuffer;
@group(0) @binding(1) var inner_height: texture_2d<f32>;
@group(0) @binding(2) var apron_height: texture_2d<f32>;
@group(0) @binding(3) var<storage, read> queries: array<vec4<u32>>;   // (i, j, slot, 0)
@group(0) @binding(4) var<storage, read_write> out: array<vec4<f32>>; // (x, z, height, y)

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = id.x;
    if (n >= arrayLength(&out)) {
        return;
    }
    let q = queries[n];
    let params = terrain.params;
    // The grid mesh's own vertex position, through the vertex shader's snap.
    let position = vec3<f32>(f32(q.x), 0.0, f32(q.y)) / params.lattice.w;
    let v = cdlod_vertex(grid_index_of(position, params), terrain.patches[q.z], params, inner_height, apron_height);
    out[n] = vec4<f32>(v.world.x, v.world.z, v.height, v.world.y);
}
"#;

/// SplitMix64: a small deterministic generator, so the test needs no extra crate.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u32) -> u32 {
        (self.next() % u64::from(n)) as u32
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Rough terrain at full resolution: kilometre-scale ripples plus texel-scale noise, so the
/// bilinear weights and both fields are exercised (about −3 to +4 km).
fn rough_field(res: u32, size_m: f64, seed: u64) -> Field {
    let mut rng = SplitMix(seed);
    let mut elevation_m = Vec::with_capacity((res * res) as usize);
    for row in 0..res {
        for col in 0..res {
            let (x, z) = (col as f32, row as f32);
            let h = 500.0
                + 2_500.0 * (x / 7.3).sin() * (z / 5.1).cos()
                + 600.0 * (x * 1.7 + z * 2.3).sin()
                + 200.0 * (rng.unit() - 0.5);
            elevation_m.push(h);
        }
    }
    Field::dry(res, size_m, elevation_m, vec![[0; 4]; (res * res) as usize])
}

fn r32_texture(device: &wgpu::Device, queue: &wgpu::Queue, field: &Field) -> wgpu::TextureView {
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("heights"),
            size: wgpu::Extent3d {
                width: field.res,
                height: field.res,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        bytemuck::cast_slice(&field.elevation_m),
    );
    texture.create_view(&Default::default())
}

/// How far the shader's morphed coordinate may sit from `morph_vertex`'s: max(1 mm, two f32 ulps
/// of the position measured from the root corner, `coordinate − root`).
///
/// Ruling R13 allows one ulp "at the coordinate" because the GPU may fuse `… × quad + root` into
/// one rounding. But the rounding Rust does there is of the product, `(cell − odd·k·2^lod) ×
/// quad`, which is the position from the root corner at −1,200 km, up to 2,400 km: one ulp of it
/// is 1/8 m even where the coordinate itself is small. Metal does fuse it (the test counts how
/// many differences match the fused sequence exactly). The second ulp covers the morph factor:
/// WGSL allows division and `sqrt` a few ulps of error, and `k × 2^lod × quad` magnifies that.
fn xz_tolerance_m(coordinate: f32, root: f32) -> f32 {
    XZ_TOLERANCE_M.max(2.0 * ulp(coordinate - root))
}

/// One f32 ulp at `v`.
fn ulp(v: f32) -> f32 {
    let v = v.abs();
    f32::from_bits(v.to_bits() + 1) - v
}

#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn wesl_morph_matches_rust() {
    let center = geo_to_render(latlon_to_geo(46.5, 10.0));
    let grid = RegionGrid {
        spec: RegionSpec::new(center, YearsBp(0.0), 1),
        inner: rough_field(REGION_RES, REGION_SIZE_M, 1),
        apron: rough_field(APRON_RES, APRON_SIZE_M, 2),
        boulders: Vec::new(),
    };
    let settings = LodSettings::region();

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    println!("adapter: {:?}", adapter.get_info());
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cdlod parity"),
        source: wgpu::ShaderSource::Wgsl(format!("{CDLOD}\n{MORPH_ENTRY}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("cdlod parity"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let inner = r32_texture(&device, &queue, &grid.inner);
    let apron = r32_texture(&device, &queue, &grid.apron);

    let mut rng = SplitMix(0x7F1E_0007);
    let per_camera = MORPH_VERTICES / MORPH_CAMERAS.len();
    let (mut worst_xz, mut worst_xz_ulps, mut worst_h, mut worst_y) =
        (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut partial, mut odd, mut apron_far, mut checked) = (0, 0, 0, 0);
    let (mut differing, mut fused_matches) = (0, 0);
    let quad = settings.finest_quad_m() as f32;
    let root = (-settings.extent_m() / 2.0) as f32;
    for camera in MORPH_CAMERAS {
        let patches = select_patches(DVec3::from(Vec3::from(camera)), &settings);
        let instances = patch_instances(&patches, &settings);
        let params = TerrainParams::new(&grid, &settings).with_camera(Vec3::from(camera));
        let buffer_bytes = terrain_buffer_bytes(&params, &instances, instances.len());
        // Random vertices, half of them on a patch edge (where neighbours must agree).
        let queries: Vec<[u32; 4]> = (0..per_camera)
            .map(|k| {
                let slot = rng.below(instances.len() as u32);
                let (mut i, mut j) = (rng.below(33), rng.below(33));
                if k % 2 == 0 {
                    if rng.below(2) == 0 {
                        i = [0, 32][rng.below(2) as usize];
                    } else {
                        j = [0, 32][rng.below(2) as usize];
                    }
                }
                [i, j, slot, 0]
            })
            .collect();

        let storage = |label, data: &[u8]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: data,
                usage: wgpu::BufferUsages::STORAGE,
            })
        };
        let terrain_buf = storage("terrain", &buffer_bytes);
        let query_buf = storage("queries", bytemuck::cast_slice(&queries));
        let bytes = (queries.len() * 16) as u64;
        let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("out"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: terrain_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&inner),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&apron),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: query_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: out_buf.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(queries.len().div_ceil(64) as u32, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
        queue.submit([encoder.finish()]);
        read_buf
            .slice(..)
            .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("poll");
        let got: Vec<[f32; 4]> =
            bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
                .to_vec();

        for (q, g) in queries.iter().zip(&got) {
            let patch = &patches[q[2] as usize];
            let [x, z] = morph_vertex([q[0], q[1]], patch, camera, &settings);
            // The morph factor and cells, as `morph_vertex` computes them.
            let cell = patch.origin_cells(&settings);
            let step = 1u32 << patch.lod;
            let p = |axis: usize| (cell[axis] + q[axis] * step) as f32 * quad + root;
            let (px, pz) = (p(0) - camera[0], p(1) - camera[2]);
            let k = morph_factor(
                (px * px + camera[1] * camera[1] + pz * pz).sqrt(),
                patch.morph_range_m,
            );
            for (axis, (gpu, rust)) in [(g[0], x), (g[1], z)].into_iter().enumerate() {
                let d = (gpu - rust).abs();
                let tolerance = xz_tolerance_m(rust, root);
                assert!(
                    d <= tolerance,
                    "xz: gpu {gpu} vs rust {rust} (diff {d} > {tolerance}) at {q:?} of {patch:?}, camera {camera:?}"
                );
                worst_xz = worst_xz.max(d);
                worst_xz_ulps = worst_xz_ulps.max(d / ulp(rust - root));
                if d > 0.0 {
                    differing += 1;
                    // The Rust steps with only the last `× quad + root` fused into one rounding.
                    let c = (cell[axis] + q[axis] * step) as f32;
                    let slid = c - (q[axis] & 1) as f32 * k * step as f32;
                    let fused = (f64::from(slid) * f64::from(quad) + f64::from(root)) as f32;
                    fused_matches += usize::from(gpu == fused);
                }
            }
            // Heights at the GPU's own xz, so an ulp of xz on a steep slope is not counted
            // against the height function.
            let (gx, gz) = (f64::from(g[0]), f64::from(g[1]));
            let h = blended_height_m(&grid, gx, gz);
            let dh = (g[2] - h).abs();
            assert!(
                dh <= HEIGHT_TOLERANCE_M,
                "height: gpu {} vs rust {h} at ({gx}, {gz})",
                g[2]
            );
            worst_h = worst_h.max(dh);
            let horizontal =
                ((gx - f64::from(camera[0])).powi(2) + (gz - f64::from(camera[2])).powi(2)).sqrt();
            let y = h - curvature_drop_m(horizontal) as f32;
            let dy = (g[3] - y).abs();
            assert!(
                dy <= HEIGHT_TOLERANCE_M,
                "y: gpu {} vs rust {y} at ({gx}, {gz})",
                g[3]
            );
            worst_y = worst_y.max(dy);

            // Coverage of the cases that matter.
            partial += usize::from(k > 0.0 && k < 1.0);
            odd += usize::from(q[0] % 2 == 1 || q[1] % 2 == 1);
            apron_far += usize::from(x.abs().max(z.abs()) > 524_288.0);
            checked += 1;
        }
    }
    println!(
        "{checked} vertices ({partial} partly morphed, {odd} with an odd index, {apron_far} beyond 524 km): \
         worst xz {worst_xz:e} m ({worst_xz_ulps} ulp of the position from the root corner), \
         height {worst_h:e} m, displaced y {worst_y:e} m; {differing} coordinates differ, \
         {fused_matches} of them exactly as a fused `× quad + root`"
    );
    assert_eq!(checked, MORPH_VERTICES);
    assert!(partial >= 10, "only {partial} partly morphed vertices");
    assert!(odd >= MORPH_VERTICES / 2, "only {odd} odd vertices");
    assert!(apron_far >= 10, "only {apron_far} far apron vertices");
}

const NORMAL_ENTRY: &str = r#"
@group(0) @binding(0) var<storage, read> terrain: TerrainBuffer;
@group(0) @binding(1) var inner_height: texture_2d<f32>;
@group(0) @binding(2) var apron_height: texture_2d<f32>;
@group(0) @binding(3) var<storage, read> queries: array<vec4<f32>>;   // (x, z, 0, 0)
@group(0) @binding(4) var<storage, read_write> out: array<vec4<f32>>; // normal, 0

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = id.x;
    if (n >= arrayLength(&out)) {
        return;
    }
    let q = queries[n];
    out[n] = vec4<f32>(terrain_normal(inner_height, apron_height, terrain.params, q.xy), 0.0);
}
"#;

/// Per-component agreement of the GPU and Rust normals. Each height is within about 0.05 m
/// (the morph test's bound); over a 2-texel baseline of at least 586 m that tilts the normal by
/// under 2e-4.
const NORMAL_TOLERANCE: f32 = 2.0e-4;

#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn wesl_normal_matches_rust() {
    let center = geo_to_render(latlon_to_geo(46.5, 10.0));
    let grid = RegionGrid {
        spec: RegionSpec::new(center, YearsBp(0.0), 1),
        inner: rough_field(REGION_RES, REGION_SIZE_M, 3),
        apron: rough_field(APRON_RES, APRON_SIZE_M, 4),
        boulders: Vec::new(),
    };
    let settings = LodSettings::region();
    let camera = Vec3::new(-20_000.0, 3_000.0, 35_000.0);
    let params = TerrainParams::new(&grid, &settings).with_camera(camera);
    let buffer_bytes = terrain_buffer_bytes(&params, &[], 1);

    // Points near the camera, across the blend, and far out in the apron (up to 1,600 km).
    let mut rng = SplitMix(0x7F1E_0008);
    let mut queries: Vec<[f32; 4]> = Vec::new();
    for k in 0..512 {
        let reach = [20_000.0, 160_000.0, 600_000.0, 1_600_000.0][k % 4];
        let x = (rng.unit() * 2.0 - 1.0) * reach;
        let z = (rng.unit() * 2.0 - 1.0) * reach;
        queries.push([x.clamp(-1.19e6, 1.19e6), z.clamp(-1.19e6, 1.19e6), 0.0, 0.0]);
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    println!("adapter: {:?}", adapter.get_info());
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("normal parity"),
        source: wgpu::ShaderSource::Wgsl(format!("{CDLOD}\n{NORMAL_ENTRY}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("normal parity"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let inner = r32_texture(&device, &queue, &grid.inner);
    let apron = r32_texture(&device, &queue, &grid.apron);
    let storage = |label, data: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: wgpu::BufferUsages::STORAGE,
        })
    };
    let terrain_buf = storage("terrain", &buffer_bytes);
    let query_buf = storage("queries", bytemuck::cast_slice(&queries));
    let bytes = (queries.len() * 16) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: terrain_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&inner),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&apron),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: query_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(queries.len().div_ceil(64) as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let got: Vec<[f32; 4]> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();

    let (mut worst, mut worst_tilt_deg) = (0.0f32, 0.0f64);
    for (q, g) in queries.iter().zip(&got) {
        let want = surface_normal(
            &grid,
            [f64::from(camera.x), f64::from(camera.z)],
            f64::from(q[0]),
            f64::from(q[1]),
        );
        for c in 0..3 {
            let diff = (g[c] - want[c]).abs();
            worst = worst.max(diff);
            assert!(
                diff <= NORMAL_TOLERANCE,
                "({}, {}) component {c}: gpu {:?} vs rust {want:?} (diff {diff})",
                q[0],
                q[1],
                &g[..3]
            );
        }
        // The far points carry the curvature tilt: a flat-terrain normal would be off by d/R.
        let d = f64::from((q[0] - camera.x).hypot(q[1] - camera.z));
        worst_tilt_deg = worst_tilt_deg.max((d / 6_371_000.0).atan().to_degrees());
    }
    println!(
        "{} normals: worst component diff {worst:.2e}; largest curvature tilt checked {worst_tilt_deg:.1} deg",
        queries.len()
    );
    assert!(worst_tilt_deg > 10.0);
}

const SNOWLINE: &str = include_str!("../assets/shaders/snowline.wesl");

const SNOWLINE_ENTRY: &str = r#"
@group(0) @binding(0) var<storage, read> queries: array<vec4<f32>>;   // (lat, anomaly, byte, -)
@group(0) @binding(1) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = id.x;
    if (n >= arrayLength(&out)) {
        return;
    }
    let q = queries[n];
    out[n] = snowline_m(q.x, q.y, snowline_raise_from_byte(u32(q.z)));
}
"#;

#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn wesl_snowline_matches_rust() {
    use timefly_core::worldgen::biome::snowline_with_raise_m;
    use timefly_core::worldgen::generate::{SURFACE_SHORE, decode_snowline_raise};
    let lats = [
        -90.0f32, -63.0, -46.5, -20.0, 0.0, 10.0, 20.0, 20.5, 30.0, 46.5, 60.0, 63.0, 80.0, 85.0,
        90.0,
    ];
    let anomalies = [
        50.0f32, 8.96, 0.0, -10.0, -60.0, -120.0, -125.0, -144.0, -150.0, -400.0,
    ];
    // Surface byte 3 values: no raise, steps through the range, the shore bit set (ruling R27).
    let bytes = [0u8, 1, 37, 64, 126, 127, SURFACE_SHORE | 90, 255];
    let queries: Vec<[f32; 4]> = lats
        .iter()
        .flat_map(|&lat| anomalies.iter().map(move |&a| (lat, a)))
        .flat_map(|(lat, a)| bytes.iter().map(move |&b| [lat, a, f32::from(b), 0.0]))
        .collect();

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("snowline parity"),
        source: wgpu::ShaderSource::Wgsl(format!("{SNOWLINE}\n{SNOWLINE_ENTRY}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("snowline parity"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let query_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("queries"),
        contents: bytemuck::cast_slice(&queries),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let bytes = (queries.len() * 4) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: query_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(queries.len().div_ceil(64) as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let got: Vec<f32> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();

    // f32 against f64 on values up to 6,500 m: a few thousandths of a metre.
    let mut worst = 0.0f64;
    for (q, g) in queries.iter().zip(&got) {
        let want = snowline_with_raise_m(
            f64::from(q[0]),
            f64::from(q[1]),
            decode_snowline_raise(q[2] as u8),
        );
        let diff = (f64::from(*g) - want).abs();
        worst = worst.max(diff);
        assert!(
            diff < 0.01,
            "lat {} anomaly {} byte {}: gpu {g} vs rust {want}",
            q[0],
            q[1],
            q[2]
        );
    }
    println!("{} snowlines: worst diff {worst:.2e} m", queries.len());
}

const PLANET: &str = include_str!("../assets/shaders/planet.wesl");

const PLANET_ENTRY: &str = r#"
struct Query {
    xz: vec2<f32>,
    pad: vec2<f32>,
    center: vec4<f32>,
    north: vec4<f32>,
    east: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> queries: array<Query>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = id.x;
    if (n >= arrayLength(&out)) {
        return;
    }
    let q = queries[n];
    out[n] = vec4<f32>(
        planet_position_m(q.xz, q.center.xyz, q.north.xyz, q.east.xyz, q.center.w),
        0.0,
    );
}
"#;

/// `planet_position_m` (`planet.wesl`) against `EARTH_RADIUS_M * FrameBasis::local_to_dir`, for
/// frames at (0, 0), (45, 10), (89, 0) and (36.1, -179.9) with x and z over +-1.7e6 m. The bound
/// is 2 m, which covers f32 rounding at Earth's radius (one ulp there is 0.5 m).
/// Measured maximum error: 0.906 m (324 positions, Apple GPU/Metal).
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn wesl_planet_position_matches_rust() {
    use timefly_core::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};
    let frames: Vec<RegionFrame> = [(0.0, 0.0), (45.0, 10.0), (89.0, 0.0), (36.1, -179.9)]
        .iter()
        .map(|&(lat, lon)| RegionFrame {
            center: geo_to_render(latlon_to_geo(lat, lon)),
        })
        .collect();
    let steps = [
        -1.7e6f64, -1.2e6, -5.0e5, -1.0e4, 0.0, 3.3e3, 4.0e5, 9.0e5, 1.7e6,
    ];
    // (xz, pad, center.xyzw, north.xyz0, east.xyz0) as f32s.
    let mut queries: Vec<[f32; 16]> = Vec::new();
    let mut truth: Vec<DVec3> = Vec::new();
    for f in &frames {
        let basis = f.basis();
        for &x in &steps {
            for &z in &steps {
                let c = basis.center;
                let (n, e) = (basis.north, basis.east);
                queries.push([
                    x as f32,
                    z as f32,
                    0.0,
                    0.0,
                    c.x as f32,
                    c.y as f32,
                    c.z as f32,
                    EARTH_RADIUS_M as f32,
                    n.x as f32,
                    n.y as f32,
                    n.z as f32,
                    0.0,
                    e.x as f32,
                    e.y as f32,
                    e.z as f32,
                    0.0,
                ]);
                // Truth from the same f32-rounded x and z. The GPU also gets the f32 centre,
                // north and east, whose rounding (about 0.4 m at Earth's radius) is part of the
                // error measured against the 0.906 m bound.
                let xf = f64::from(x as f32);
                let zf = f64::from(z as f32);
                truth.push(basis.local_to_dir(xf, zf).0 * EARTH_RADIUS_M);
            }
        }
    }

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("planet parity"),
        source: wgpu::ShaderSource::Wgsl(format!("{PLANET}\n{PLANET_ENTRY}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("planet parity"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let query_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("queries"),
        contents: bytemuck::cast_slice(&queries),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let bytes = (queries.len() * 16) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: query_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(queries.len().div_ceil(64) as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let got: Vec<f32> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();

    let mut worst = 0.0f64;
    for (i, want) in truth.iter().enumerate() {
        let g = DVec3::new(
            f64::from(got[i * 4]),
            f64::from(got[i * 4 + 1]),
            f64::from(got[i * 4 + 2]),
        );
        let diff = (g - *want).length();
        worst = worst.max(diff);
        assert!(
            diff < 2.0,
            "query {i} ({}, {}): gpu {g} vs rust {want}: {diff} m",
            queries[i][0],
            queries[i][1]
        );
    }
    println!("{} positions: worst diff {worst:.3} m", queries.len());
}

// ---------------------------------------------------------------------------------------------
// The cloud over near geometry (M3 Task 7's frag_depth rule, observed on the GPU)

const CLOUD: &str = include_str!("../assets/shaders/cloud.wesl");
/// The cloud's imports, stood in for: Bevy's view uniform (only the matrices the cloud reads)
/// at group 0, and the material at group 1.
const CLOUD_VIEW: &str = r#"
struct View {
    clip_from_view: mat4x4<f32>,
    view_from_clip: mat4x4<f32>,
    world_from_view: mat4x4<f32>,
}
@group(0) @binding(0) var<uniform> view: View;
"#;
/// Near geometry: a full-screen opaque red triangle at a fixed clip depth, writing depth.
const NEAR_QUAD: &str = r#"
@group(0) @binding(0) var<uniform> quad: vec4<f32>; // x: clip depth

@vertex
fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(p[i], quad.x, 1.0);
}

@fragment
fn fragment() -> @location(0) vec4<f32> {
    return vec4<f32>(1.0, 0.0, 0.0, 1.0);
}
"#;
const VEIL_SIZE: u32 = 256;
/// The eye-to-fly distance (world units) and the cloud's density for the check.
const VEIL_FLY_DISTANCE: f32 = 10.0;
const VEIL_DENSITY: f32 = 0.5;

/// `cloud.wesl` as plain WGSL: the imports replaced by `noise.wesl` and [`CLOUD_VIEW`].
fn cloud_wgsl() -> String {
    let imports = [
        "import bevy_pbr::render::mesh_view_bindings::view;",
        "import package::noise::gradient_noise3;",
    ];
    for import in imports {
        assert_eq!(
            CLOUD.matches(import).count(),
            1,
            "cloud.wesl imports {import}"
        );
    }
    let body: Vec<&str> = CLOUD
        .lines()
        .filter(|l| !l.trim_start().starts_with("import "))
        .collect();
    let body = body.join("\n");
    assert_eq!(body.matches("constants::MATERIAL_BIND_GROUP").count(), 1);
    let body = body.replace("constants::MATERIAL_BIND_GROUP", "1");
    assert!(
        !body
            .lines()
            .any(|l| !l.trim_start().starts_with("//") && l.contains("::")),
        "cloud.wesl has an import this test doesn't stand in for"
    );
    format!("{NOISE}\n{CLOUD_VIEW}\n{body}")
}

/// Renders the red quad at `quad_view_depth`, then the cloud at [`VEIL_DENSITY`] for a fly
/// [`VEIL_FLY_DISTANCE`] ahead, as the game's pipeline draws it (blended, depth-tested with
/// reverse-Z `GreaterEqual`, never writing depth), and reads the colour back (RGBA8).
fn render_cloud_over_quad(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    quad_view_depth: f32,
) -> Vec<[u8; 4]> {
    use bevy::camera::{CameraProjection, PerspectiveProjection};
    use bevy::math::{Mat4, Vec4};
    use timefly::cloud::{CloudParams, DEPTH_PAST_FLY, tilt_shift};

    let (w, h) = (VEIL_SIZE, VEIL_SIZE);
    // Bevy's own projection (infinite reverse-Z), as the game's cameras use it.
    let clip_from_view = PerspectiveProjection {
        fov: 45f32.to_radians(),
        aspect_ratio: w as f32 / h as f32,
        near: 0.1,
        ..Default::default()
    }
    .get_clip_from_view();
    let clip_depth = |view_depth: f32| {
        let c = clip_from_view * Vec4::new(0.0, 0.0, -view_depth, 1.0);
        c.z / c.w
    };
    let mats: Vec<f32> = [clip_from_view, clip_from_view.inverse(), Mat4::IDENTITY]
        .iter()
        .flat_map(|m| m.to_cols_array())
        .collect();
    let params = CloudParams {
        state: Vec4::new(
            VEIL_DENSITY,
            VEIL_FLY_DISTANCE * DEPTH_PAST_FLY,
            tilt_shift(VEIL_DENSITY),
            w as f32 / h as f32,
        ),
        // No hole: the fly isn't drawn here.
        fly: Vec4::ZERO,
        motion: Vec4::ZERO,
        sun: Vec4::new(0.3, 0.6, -0.74, 0.0).normalize(),
        sun_color: Vec4::new(1.0, 0.95, 0.9, 1.0),
        zenith: Vec4::new(0.2, 0.35, 0.7, 1.0),
        horizon: Vec4::new(0.6, 0.7, 0.85, 1.0),
    };
    let packed: Vec<f32> = [
        params.state,
        params.fly,
        params.motion,
        params.sun,
        params.sun_color,
        params.zenith,
        params.horizon,
    ]
    .iter()
    .flat_map(|v| v.to_array())
    .collect();

    let uniform = |label, data: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: wgpu::BufferUsages::UNIFORM,
        })
    };
    let view_buf = uniform("view", bytemuck::cast_slice(&mats));
    let cloud_buf = uniform("cloud", bytemuck::cast_slice(&packed));
    let quad_buf = uniform(
        "quad",
        bytemuck::cast_slice(&[clip_depth(quad_view_depth), 0.0, 0.0, 0.0]),
    );
    let triangle: [[f32; 3]; 3] = [[-1.0, -1.0, 0.0], [3.0, -1.0, 0.0], [-1.0, 3.0, 0.0]];
    let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cloud triangle"),
        contents: bytemuck::cast_slice(&triangle),
        usage: wgpu::BufferUsages::VERTEX,
    });

    let color_format = wgpu::TextureFormat::Rgba8Unorm;
    let depth_format = wgpu::TextureFormat::Depth32Float;
    let target = |format, usage| {
        device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        })
    };
    let color = target(
        color_format,
        wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = target(depth_format, wgpu::TextureUsages::RENDER_ATTACHMENT);
    let color_view = color.create_view(&Default::default());
    let depth_view = depth.create_view(&Default::default());

    let quad_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("near quad"),
        source: wgpu::ShaderSource::Wgsl(NEAR_QUAD.into()),
    });
    let cloud_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cloud"),
        source: wgpu::ShaderSource::Wgsl(cloud_wgsl().into()),
    });
    let depth_state = |write: bool, compare| wgpu::DepthStencilState {
        format: depth_format,
        depth_write_enabled: Some(write),
        depth_compare: Some(compare),
        stencil: Default::default(),
        bias: Default::default(),
    };
    let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("near quad"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &quad_module,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: Default::default(),
        depth_stencil: Some(depth_state(true, wgpu::CompareFunction::GreaterEqual)),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &quad_module,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(color_format.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    // As `CloudMaterial` specializes Bevy's blended mesh pipeline: no culling, depth-tested,
    // never writing depth. Copied by hand from `CloudMaterial::specialize` and `alpha_mode`
    // (`src/cloud.rs`): keep the two in step.
    let cloud_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cloud"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &cloud_module,
            entry_point: Some("vertex"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 12,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3],
            })],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(depth_state(false, wgpu::CompareFunction::GreaterEqual)),
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &cloud_module,
            entry_point: Some("fragment"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let group = |pipeline: &wgpu::RenderPipeline, index: u32, buffer: &wgpu::Buffer| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(index),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        })
    };
    let quad_group = group(&quad_pipeline, 0, &quad_buf);
    let view_group = group(&cloud_pipeline, 0, &view_buf);
    let cloud_group = group(&cloud_pipeline, 1, &cloud_buf);

    let row_bytes = w * 4;
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(row_bytes * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cloud over near geometry"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &depth_view,
                // Reverse-Z: 0 is infinitely far.
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(0.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        pass.set_pipeline(&quad_pipeline);
        pass.set_bind_group(0, &quad_group, &[]);
        pass.draw(0..3, 0..1);
        pass.set_pipeline(&cloud_pipeline);
        pass.set_bind_group(0, &view_group, &[]);
        pass.set_bind_group(1, &cloud_group, &[]);
        pass.set_vertex_buffer(0, vertices.slice(..));
        pass.draw(0..3, 0..1);
    }
    encoder.copy_texture_to_buffer(
        color.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &read_buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let pixels: Vec<[u8; 4]> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();
    pixels
}

/// Ground nearer than [`DEPTH_PAST_FLY`] × the eye-to-fly distance, under a half-dense cloud, is
/// veiled exactly as far ground is, never drawn sharp in front of the cloud (M3 Task 7: the
/// cloud writes the near plane's depth at every density above clear).
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn the_cloud_veils_geometry_nearer_than_its_plane() {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    println!("adapter: {:?}", adapter.get_info());
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");

    // Far ground (well past the cloud's plane) is the reference: the cloud always covered it.
    let far = render_cloud_over_quad(&device, &queue, VEIL_FLY_DISTANCE * 10.0);
    let veiled = |p: &[u8; 4]| p[1] >= 4 && p[2] >= 4;
    let far_veiled = far.iter().filter(|p| veiled(p)).count();
    assert_eq!(
        far_veiled,
        far.len(),
        "the half-dense cloud leaves far ground bare in {} of {} pixels",
        far.len() - far_veiled,
        far.len()
    );
    let min_green = far.iter().map(|p| p[1]).min().unwrap_or(0);
    println!("far ground under the cloud: least green {min_green} (bare ground is 0)");

    // Nearer than the fly, and between the fly and the cloud's old plane at 1.2 x.
    let plane = VEIL_FLY_DISTANCE * timefly::cloud::DEPTH_PAST_FLY;
    for factor in [0.5, 1.1] {
        let depth = VEIL_FLY_DISTANCE * factor;
        assert!(depth < plane);
        let near = render_cloud_over_quad(&device, &queue, depth);
        let sharp = near.iter().filter(|p| **p == [255, 0, 0, 255]).count();
        let differing = near
            .iter()
            .zip(&far)
            .filter(|(n, f)| n.iter().zip(f.iter()).any(|(a, b)| a.abs_diff(*b) > 1))
            .count();
        println!(
            "ground at {factor} x the fly distance ({depth} of plane {plane}): {sharp} sharp, {differing} differing from far ground, of {} pixels",
            near.len()
        );
        assert_eq!(
            sharp,
            0,
            "ground at {factor} x the fly distance is drawn sharp in front of the cloud in {sharp} of {} pixels",
            near.len()
        );
        assert_eq!(
            differing,
            0,
            "ground at {factor} x the fly distance is veiled differently from far ground in {differing} of {} pixels",
            near.len()
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The water texture (Task 7), as the water shader (Task 8) reads it

const WATER_ENTRY: &str = r#"
@group(0) @binding(0) var water: texture_2d<u32>;
@group(0) @binding(1) var<storage, read_write> out: array<vec4<f32>>; // two per texel

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(water);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let w = textureLoad(water, vec2<i32>(id.xy), 0);
    let river = unpack2x16float(w.z); // (ribbon, offset)
    let flow = unpack2x16float(w.w);  // (+x, +z)
    let n = (id.y * size.x + id.x) * 2u;
    out[n] = vec4<f32>(bitcast<f32>(w.x), bitcast<f32>(w.y), river.x, river.y);
    out[n + 1u] = vec4<f32>(flow.x, flow.y, 0.0, 0.0);
}
"#;

/// `water_image`'s `Rgba32Uint` texels decode on the GPU, with `bitcast` and
/// `unpack2x16float`, to the grid's level, `level − height`, ribbon, offset and flow `(east,
/// −north)` in each texel's own frame: the layout the water shader reads.
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn the_water_texture_decodes_on_the_gpu() {
    use timefly::terrain::material::water_image;
    use timefly_core::worldgen::generate::RIVER_NONE;
    use timefly_core::worldgen::generate::water::{decode_river, encode_river};
    let res = 37;
    let mut field = rough_field(res, REGION_SIZE_M, 11);
    let mut rng = SplitMix(5);
    for i in 0..(res * res) as usize {
        let h = field.elevation_m[i];
        // A third dry, a third standing water, a third rivers.
        match i % 3 {
            0 => field.water_level_m[i] = h - 64.0,
            1 => field.water_level_m[i] = h + 300.0 * rng.unit(),
            _ => {
                field.water_level_m[i] = h + 4.0 * rng.unit();
                field.river[i] = encode_river(
                    200.0 * (rng.unit() - 0.5),
                    6.0 * (rng.unit() - 0.5),
                    6.0 * (rng.unit() - 0.5),
                    300.0 * (rng.unit() - 0.5),
                );
            }
        }
    }
    assert!(field.river.contains(&RIVER_NONE));
    let image = water_image(&field);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let texture = device.create_texture_with_data(
        &queue,
        &wgpu::TextureDescriptor {
            label: Some("water"),
            size: wgpu::Extent3d {
                width: res,
                height: res,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        image.data.as_ref().expect("image data"),
    );
    let view = texture.create_view(&Default::default());
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("water decode"),
        source: wgpu::ShaderSource::Wgsl(WATER_ENTRY.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("water decode"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bytes = u64::from(res * res) * 32;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(res.div_ceil(8), res.div_ceil(8), 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let got: Vec<f32> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();

    for i in 0..(res * res) as usize {
        let g = &got[i * 8..i * 8 + 8];
        let level = field.water_level_m[i];
        let [ribbon, east, north, offset] = decode_river(field.river[i]);
        assert_eq!(g[0].to_bits(), level.to_bits(), "texel {i} level");
        assert_eq!(g[1], level - field.elevation_m[i], "texel {i} thickness");
        assert_eq!(
            [g[2], g[3]],
            [ribbon, offset],
            "texel {i} ribbon and offset"
        );
        assert_eq!([g[4], g[5]], [east, -north], "texel {i} flow");
    }
    println!("{} water texels decode on the GPU", res * res);
}

// ---------------------------------------------------------------------------------------------
// The water shader's sampler (Task 8)

const WATER_PARITY_ENTRY: &str = r#"
@group(0) @binding(0) var<storage, read> terrain: TerrainBuffer;
@group(0) @binding(1) var inner_water: texture_2d<u32>;
@group(0) @binding(2) var apron_water: texture_2d<u32>;
@group(0) @binding(3) var<storage, read> queries: array<vec4<f32>>;   // (x, z, 0, 0)
@group(0) @binding(4) var<storage, read_write> out: array<vec4<f32>>; // two per query

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = id.x;
    if (n >= arrayLength(&queries)) {
        return;
    }
    let w = blended_water(inner_water, apron_water, terrain.params, queries[n].xy);
    out[2u * n] = vec4<f32>(w.level, w.thickness, w.ribbon, 0.0);
    out[2u * n + 1u] = vec4<f32>(w.flow, 0.0, 0.0);
}
"#;

/// Points checked per case.
const WATER_POINTS: usize = 4_096;

/// A field's water texture on the GPU, as the shell uploads it.
fn water_texture(device: &wgpu::Device, queue: &wgpu::Queue, field: &Field) -> wgpu::TextureView {
    let image = timefly::terrain::material::water_image(field);
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("water"),
            size: wgpu::Extent3d {
                width: field.res,
                height: field.res,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Uint,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        image.data.as_ref().expect("image data"),
    );
    texture.create_view(&Default::default())
}

/// Random water over `rough_field` terrain: inside `calm_m` (max(|x|, |z|)) a mix of dry
/// texels, standing water and two rivers 1.5 km apart converging side by side (opposite offset
/// signs across the divide, the crossing guard's case), plus scattered river texels with random
/// offsets; from `calm_m` to `still_m` one uniform lake, so the blend between the fields mixes
/// equal values and only the blend's arithmetic is compared there; beyond `still_m` random
/// again. (A field's rivers meander nowhere: only the sampler is under test.)
fn wet_rough_field(res: u32, size_m: f64, seed: u64, calm_m: f64, still_m: f64) -> Field {
    use timefly_core::worldgen::generate::water::encode_river;
    let mut field = rough_field(res, size_m, seed);
    let mut rng = SplitMix(seed ^ 0xAB);
    let (west, east) = (1_234.5, 2_734.5);
    for row in 0..res {
        for col in 0..res {
            let i = (row * res + col) as usize;
            let (x, z) = field.local_of(row, col);
            let a = x.abs().max(z.abs());
            let h = field.elevation_m[i];
            if (calm_m..still_m).contains(&a) {
                field.elevation_m[i] = 30.0;
                field.water_level_m[i] = 40.0;
                continue;
            }
            let near = (x - west).abs().min((x - east).abs());
            if near < 1_500.0 {
                let o = if x < (west + east) / 2.0 {
                    x - west
                } else {
                    x - east
                } as f32;
                let half = 4.0 + 30.0 * rng.unit();
                field.water_level_m[i] = h + 4.0 * rng.unit() - 1.0;
                let speed = 6.0 * rng.unit();
                field.river[i] = encode_river(half - o.abs(), speed, -0.5 * speed, o);
                continue;
            }
            match rng.below(4) {
                0 => field.water_level_m[i] = h - 64.0,
                1 => field.water_level_m[i] = h + 30.0 * rng.unit(),
                2 => {
                    field.water_level_m[i] = h + 4.0 * rng.unit() - 1.0;
                    let o = 4.0 * size_m as f32 / res as f32 * (rng.unit() - 0.5);
                    field.river[i] = encode_river(
                        40.0 * rng.unit() - o.abs(),
                        6.0 * (rng.unit() - 0.5),
                        6.0 * (rng.unit() - 0.5),
                        o,
                    );
                }
                _ => field.water_level_m[i] = h - 64.0 + 70.0 * rng.unit(),
            }
        }
    }
    field
}

/// `sample_clamped`'s four texel indices at `(x, z)` (the test's own copy).
fn tap_indices(field: &Field, x: f64, z: f64) -> [usize; 4] {
    let last = f64::from(field.res - 1);
    let spacing = field.spacing_m();
    let u = ((x + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let v = ((z + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let (c0, r0) = (u.floor().min(last - 1.0), v.floor().min(last - 1.0));
    let n = field.res as usize;
    let i = r0 as usize * n + c0 as usize;
    [i, i + 1, i + n, i + n + 1]
}

/// Whether a field's level at `(x, z)` comes from its standing water alone
/// (`timefly_core::terrain::blended_water`'s rule: no tap is a river texel, some tap is wet),
/// and if so the wet taps' share `d` of the bilinear weight (0 when none has weight), and
/// whether some tap is dry (a shore cell).
fn standing_share(field: &Field, x: f64, z: f64) -> Option<(f32, bool)> {
    use timefly_core::terrain::DRY_BELOW_M;
    use timefly_core::worldgen::generate::water::RIVER_NONE;
    let taps = tap_indices(field, x, z);
    let river = |i: usize| field.river[i][0] != RIVER_NONE[0] || field.river[i][3] != 0;
    let wet = |i: usize| field.water_level_m[i] - field.elevation_m[i] > DRY_BELOW_M;
    if taps.iter().any(|&i| river(i)) || !taps.iter().any(|&i| wet(i)) {
        return None;
    }
    let last = f64::from(field.res - 1);
    let spacing = field.spacing_m();
    let u = ((x + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let v = ((z + field.size_m / 2.0) / spacing).clamp(0.0, last);
    let (fu, fv) = (u - u.floor().min(last - 1.0), v - v.floor().min(last - 1.0));
    let w = [
        (1.0 - fu) * (1.0 - fv),
        fu * (1.0 - fv),
        (1.0 - fu) * fv,
        fu * fv,
    ];
    let d: f64 = (0..4).filter(|&k| wet(taps[k])).map(|k| w[k]).sum();
    Some((d as f32, taps.iter().any(|&i| !wet(i))))
}

/// The largest magnitude and the spread (max − min) of `value` over both fields' taps at
/// `(x, z)`.
fn tap_range(
    grid: &RegionGrid,
    x: f64,
    z: f64,
    value: impl Fn(&Field, usize) -> f32,
) -> (f32, f32) {
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for f in [&grid.inner, &grid.apron] {
        for i in tap_indices(f, x, z) {
            let v = value(f, i);
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    (lo.abs().max(hi.abs()), hi - lo)
}

/// Runs `blended_water` on the GPU at `points` over `grid`'s water textures: per point
/// `[level, thickness, ribbon, flow.x, flow.y]`.
fn gpu_blended_water(grid: &RegionGrid, points: &[[f32; 4]]) -> Vec<[f32; 5]> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    let (device, queue) =
        block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("water parity"),
        source: wgpu::ShaderSource::Wgsl(format!("{CDLOD}\n{WATER_PARITY_ENTRY}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("water parity"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let inner = water_texture(&device, &queue, &grid.inner);
    let apron = water_texture(&device, &queue, &grid.apron);
    let settings = LodSettings::region();
    let params = TerrainParams::new(grid, &settings);
    let buffer_bytes = terrain_buffer_bytes(&params, &[], 1);
    let storage = |label, data: &[u8]| {
        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: data,
            usage: wgpu::BufferUsages::STORAGE,
        })
    };
    let terrain_buf = storage("terrain", &buffer_bytes);
    let query_buf = storage("queries", bytemuck::cast_slice(points));
    let bytes = (points.len() * 32) as u64;
    let out_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("out"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: terrain_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&inner),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&apron),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: query_buf.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: out_buf.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(points.len().div_ceil(64) as u32, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&out_buf, 0, &read_buf, 0, bytes);
    queue.submit([encoder.finish()]);
    read_buf
        .slice(..)
        .map_async(wgpu::MapMode::Read, |r| r.expect("map"));
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll");
    let got: Vec<[f32; 8]> =
        bytemuck::cast_slice(&read_buf.slice(..).get_mapped_range().expect("mapped range"))
            .to_vec();
    got.iter().map(|g| [g[0], g[1], g[2], g[4], g[5]]).collect()
}

/// Worst differences of one case: (level in ulps of its taps, thickness, ribbon, flow in m or
/// m/s), and how many points went through the crossing guard, the blend, rivers.
#[derive(Debug, Default)]
struct WaterParity {
    level_ulps: f32,
    thickness: f32,
    ribbon: f32,
    flow: f32,
    guarded: usize,
    blended: usize,
    in_river: usize,
    /// Points whose level came from a shore cell's wet taps alone (some taps dry).
    shore: usize,
}

/// Compares the GPU's `blended_water` with the Rust reference at `points`. `weight_error` bounds
/// how far the GPU's f32 bilinear weights may sit from the reference's (0 where the field sizes
/// make them exact); each quantity's tolerance is `ulps` ulps of its taps' largest magnitude,
/// plus the weight error and the blend weight's f32 rounding (1e-6) times its taps' spread.
fn check_water_parity(grid: &RegionGrid, points: &[[f32; 4]], weight_error: f32) -> WaterParity {
    use timefly_core::terrain::{CROSSING_REACH_TEXELS, blended_water};
    use timefly_core::worldgen::generate::water::decode_river;
    let got = gpu_blended_water(grid, points);
    let mut parity = WaterParity::default();
    let river = |f: &Field, i: usize| decode_river(f.river[i]);
    let e1 = grid.inner.size_m / 2.0;
    let e0 = e1 - 4.0 * grid.apron.spacing_m();
    for (q, g) in points.iter().zip(&got) {
        let (x, z) = (f64::from(q[0]), f64::from(q[1]));
        let r = blended_water(grid, x, z);
        let slack = weight_error + 1e-6;
        let tol = |ulps: f32, (scale, spread): (f32, f32)| ulps * ulp(scale) + slack * spread;
        let level = tap_range(grid, x, z, |f, i| f.water_level_m[i]);
        let heights = tap_range(grid, x, z, |f, i| f.elevation_m[i]);
        let half = tap_range(grid, x, z, |f, i| river(f, i)[0] + river(f, i)[3].abs());
        let offset = tap_range(grid, x, z, |f, i| river(f, i)[3]);
        let ribbon = tap_range(grid, x, z, |f, i| river(f, i)[0]);
        let east = tap_range(grid, x, z, |f, i| river(f, i)[1]);
        let north = tap_range(grid, x, z, |f, i| river(f, i)[2]);

        // Standing water's level is `n / d` over the wet taps: two more ulps for the division,
        // and the weight error divided by the wet taps' share `d`.
        let shares: Vec<(f32, bool)> = [&grid.inner, &grid.apron]
            .iter()
            .filter_map(|f| standing_share(f, x, z))
            .collect();
        let standing = shares
            .iter()
            .map(|&(d, _)| 2.0 * ulp(level.0) + slack * level.1 / d.max(1e-6))
            .fold(0.0f32, f32::max);
        let d = (g[0] - r.level_m).abs();
        assert!(
            d <= tol(2.0, level) + standing,
            "level at ({x}, {z}): gpu {} vs rust {} ({} ulps)",
            g[0],
            r.level_m,
            d / ulp(level.0)
        );
        parity.level_ulps = parity.level_ulps.max(d / ulp(level.0));
        // The GPU interpolates each texel's `level − h`; the reference subtracts the blended
        // height from the blended level: a few ulps of either apart.
        let both = (level.0.max(heights.0), level.1 + heights.1);
        let d = (g[1] - r.thickness_m).abs();
        assert!(
            d <= tol(4.0, both) + 2.0 * standing,
            "thickness at ({x}, {z}): gpu {} vs rust {}",
            g[1],
            r.thickness_m
        );
        parity.thickness = parity.thickness.max(d);
        let parts = (
            half.0.max(offset.0).max(ribbon.0),
            half.1 + offset.1 + ribbon.1,
        );
        let d = (g[2] - r.ribbon_m).abs();
        assert!(
            d <= tol(4.0, parts),
            "ribbon at ({x}, {z}): gpu {} vs rust {}",
            g[2],
            r.ribbon_m
        );
        parity.ribbon = parity.ribbon.max(d);
        // The GPU's flow is (east, −north).
        for (gpu, rust, range) in [(g[3], r.flow[0], east), (g[4], -r.flow[1], north)] {
            let d = (gpu - rust).abs();
            assert!(
                d <= tol(2.0, range),
                "flow at ({x}, {z}): gpu {gpu} vs rust {rust}"
            );
            parity.flow = parity.flow.max(d);
        }
        // Coverage.
        let guarded = [&grid.inner, &grid.apron].iter().any(|f| {
            let o = tap_indices(f, x, z).map(|i| river(f, i)[3]);
            let (lo, hi) = o
                .iter()
                .fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
            lo < 0.0 && hi > 0.0 && hi - lo > CROSSING_REACH_TEXELS * f.spacing_m() as f32
        });
        parity.guarded += usize::from(guarded);
        let a = x.abs().max(z.abs());
        parity.blended += usize::from(a > e0 && a < e1);
        parity.in_river += usize::from(r.ribbon_m > 0.0 && r.ribbon_m < 100.0);
        parity.shore += usize::from(shares.iter().any(|&(d, dry)| dry && d > 0.0));
    }
    parity
}

/// Query points: a third in the inner field (half of them within 60 m of a river or the divide
/// between them), a sixth in
/// the blend, the rest over the apron and beyond its edge; each coordinate a multiple of
/// `step_m`.
fn water_points(grid: &RegionGrid, seed: u64, step_m: f64) -> Vec<[f32; 4]> {
    let mut rng = SplitMix(seed);
    let e1 = grid.inner.size_m / 2.0;
    let e0 = e1 - 4.0 * grid.apron.spacing_m();
    let snap = |v: f64| ((v / step_m).round() * step_m) as f32;
    let mut span = |lo: f64, hi: f64| lo + (hi - lo) * f64::from(rng.unit());
    (0..WATER_POINTS)
        .map(|k| {
            let (x, z) = match k % 6 {
                // Across the two rivers (wet_rough_field's), and the divide between them.
                0 => {
                    let centre = [1_234.5, 2_734.5, 1_984.5][k / 6 % 3];
                    (centre + span(-60.0, 60.0), span(-e0, e0))
                }
                1 => (span(-e0, e0), span(-e0, e0)),
                2 => {
                    let a = span(e0, e1);
                    let b = span(-e1, e1);
                    if k % 12 == 2 { (a, b) } else { (b, -a) }
                }
                _ => {
                    let edge = grid.apron.size_m / 2.0 + 20_000.0;
                    (span(-edge, edge), span(-edge, edge))
                }
            };
            [snap(x), snap(z), 0.0, 0.0]
        })
        .collect()
}

/// The water shader's sampler mirrors `blended_water`, crossing guard included. First over
/// fields whose sizes make the bilinear weights exact in f32 (spacings of 256 m and 2,048 m,
/// points on a 1/8 m grid): the level within 2 ulps of its taps, the rest within a few. Then
/// over the region's own sizes, whose f32 weights sit a few ulps of the texel coordinate from
/// the reference's (WGSL's division may be 2.5 ulps off): within that times each quantity's
/// local spread.
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn wesl_water_matches_rust() {
    let center = geo_to_render(latlon_to_geo(34.2, 74.3));
    let dyadic = RegionGrid {
        spec: RegionSpec::new(center, YearsBp(0.0), 1),
        inner: wet_rough_field(1025, 262_144.0, 3, 118_000.0, 1.0e9),
        apron: wet_rough_field(1025, 2_097_152.0, 4, 0.0, 137_000.0),
        boulders: Vec::new(),
    };
    let points = water_points(&dyadic, 0xD1AD, 0.125);
    let exact = check_water_parity(&dyadic, &points, 0.0);
    println!("dyadic fields: {exact:?}");

    let standard = RegionGrid {
        spec: RegionSpec::new(center, YearsBp(0.0), 1),
        inner: wet_rough_field(REGION_RES, REGION_SIZE_M, 5, 128_000.0, 1.0e9),
        apron: wet_rough_field(APRON_RES, APRON_SIZE_M, 6, 0.0, 155_000.0),
        boulders: Vec::new(),
    };
    let points = water_points(&standard, 0x57A2, 1.0 / 64.0);
    // Three ulps of a texel coordinate below 1,025 for each weight.
    let weight_error = 2.0 * 3.0 * ulp(1024.0);
    let region = check_water_parity(&standard, &points, weight_error);
    println!("region fields: {region:?}");

    for (name, p) in [("dyadic", &exact), ("region", &region)] {
        assert!(
            p.guarded >= 50,
            "{name}: only {} points through the guard",
            p.guarded
        );
        assert!(
            p.blended >= 300,
            "{name}: only {} points in the blend",
            p.blended
        );
        assert!(
            p.in_river >= 100,
            "{name}: only {} points in a river",
            p.in_river
        );
        assert!(
            p.shore >= 100,
            "{name}: only {} points on standing water's shore",
            p.shore
        );
    }
}

/// A full-screen triangle whose fragments each run a long dependent loop: a pass with a known,
/// sizeable GPU cost.
const BUSY_PASS: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    var v = p.x * 0.001 + p.y * 0.002;
    for (var k = 0; k < 128; k++) {
        v = fract(sin(v * 12.9898 + f32(k)) * 43758.5453);
    }
    return vec4<f32>(v, v, v, 1.0);
}
"#;

/// One frame of `passes` busy passes between `timer`'s marker passes, submitted; `wait` waits
/// for the GPU after it. Returns the readings that came in.
fn submit_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    timer: &mut timefly::diagnostics::gpu_time::GpuTimer,
    pipeline: &wgpu::RenderPipeline,
    view: &wgpu::TextureView,
    passes: u32,
    wait: bool,
) -> (bool, Vec<Option<f32>>) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let timed = timer.begin(&mut encoder);
    for _ in 0..passes {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("busy"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.draw(0..3, 0..1);
    }
    timer.end(
        &mut encoder,
        device,
        &[(view, wgpu::TextureFormat::Rgba8Unorm)],
    );
    queue.submit([encoder.finish()]);
    timer.submitted(queue);
    let poll = if wait {
        wgpu::PollType::wait_indefinitely()
    } else {
        wgpu::PollType::Poll
    };
    device.poll(poll).expect("poll");
    (timed, timer.harvest())
}

/// Frames timed by a fresh [`GpuTimer`](timefly::diagnostics::gpu_time::GpuTimer), each with
/// `passes` busy passes, the busy target as the frame's output, each waiting for its work.
/// A primer frame comes first (a frame with no read predecessor is dropped), and empty frames
/// after until every reading is in. The readings, in frame order.
fn timed_frames(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    view: &wgpu::TextureView,
    passes: &[u32],
) -> Vec<f32> {
    let mut timer =
        timefly::diagnostics::gpu_time::GpuTimer::new(device, queue.get_timestamp_period());
    let mut readings = Vec::new();
    // A frame's reading arrives with the frame after it (its resolve waits for its work).
    let frames = std::iter::once(0)
        .chain(passes.iter().copied())
        .chain([0, 0]);
    for (i, n) in frames.enumerate() {
        let (timed, got) = submit_frame(device, queue, &mut timer, pipeline, view, n, true);
        assert!(timed, "a free slot for frame {i}");
        readings.extend(got);
    }
    assert_eq!(
        readings.first(),
        Some(&None),
        "the primer has no predecessor"
    );
    readings
        .into_iter()
        .skip(1)
        .take(passes.len())
        .map(|r| r.expect("a valid reading: begin and end written, end after begin"))
        .collect()
}

/// `frames` frames of `passes` busy passes each, back to back (no wait), as when GPU-bound:
/// frames that find no free slot go untimed. Then more of the same, waiting, until the readings
/// are in. Every valid reading.
fn back_to_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &wgpu::RenderPipeline,
    view: &wgpu::TextureView,
    passes: u32,
    frames: usize,
) -> (Vec<f32>, usize) {
    let mut timer =
        timefly::diagnostics::gpu_time::GpuTimer::new(device, queue.get_timestamp_period());
    let mut readings = Vec::new();
    let mut untimed = 0;
    for i in 0..frames + 6 {
        let (timed, got) = submit_frame(
            device,
            queue,
            &mut timer,
            pipeline,
            view,
            passes,
            i >= frames,
        );
        untimed += usize::from(!timed);
        readings.extend(got);
    }
    (readings.into_iter().flatten().collect(), untimed)
}

/// Rivers and lakes Task 10: the GPU frame time read from pass-level timestamps (one-pixel
/// marker passes around the frame, `TIMESTAMP_QUERY` only) is positive, grows with the GPU work
/// between them, and is not inflated when frames run back to back (on Apple silicon a marker
/// pass with no dependency overlaps other passes).
#[test]
#[ignore = "needs a GPU adapter; run with --ignored"]
fn the_gpu_frame_time_is_measured_on_this_gpu() {
    use timefly::diagnostics::gpu_time::GpuTimer;
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a GPU adapter");
    println!("adapter: {:?}", adapter.get_info());
    assert!(
        adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
        "this adapter offers no TIMESTAMP_QUERY: the game reports `gpu time: unavailable` here"
    );
    let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        ..Default::default()
    }))
    .expect("device");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("busy pass"),
        source: wgpu::ShaderSource::Wgsl(BUSY_PASS.into()),
    });
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("busy pass"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(format.into())],
        }),
        multiview_mask: None,
        cache: None,
    });
    let view = device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("busy target"),
            size: wgpu::Extent3d {
                width: 1024,
                height: 1024,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default());
    println!(
        "timestamp period {} ns",
        GpuTimer::new(&device, queue.get_timestamp_period()).period_ns()
    );
    // Warm up (pipeline compile, clocks), then the median of three frames per pass count.
    timed_frames(&device, &queue, &pipeline, &view, &[4; 3]);
    let median = |passes: u32| {
        let mut ms = timed_frames(&device, &queue, &pipeline, &view, &[passes; 3]);
        ms.sort_by(f32::total_cmp);
        println!("{passes} busy passes, waited: {ms:?} ms");
        ms[1]
    };
    let (none, one, four, sixteen) = (median(0), median(1), median(4), median(16));
    println!("medians: 0 {none} ms, 1 {one} ms, 4 {four} ms, 16 {sixteen} ms");
    assert!(
        (0.0..1.0).contains(&none),
        "the timer's own passes: {none} ms"
    );
    assert!(one > 0.0, "{one} ms");
    // Close to linear in the work: the end waits for the last busy pass.
    assert!(four > 3.0 * one, "4 passes {four} ms vs 1 pass {one} ms");
    assert!(
        sixteen > 3.0 * four,
        "16 passes {sixteen} ms vs 4 passes {four} ms"
    );
    // Twenty frames back to back (review, fix round 1): when the ring runs out some frames go
    // untimed, and a frame after an untimed one could start inside it and read up to double.
    // Those are dropped, so no reading is: the worst stays with the median (a doubled reading
    // is about 2×), and the median with the waited figure. That one only loosely: waited, the
    // GPU idles between frames and may clock down (one run read 8.5 ms waited, 6.9 packed).
    let (mut packed, untimed) = back_to_back(&device, &queue, &pipeline, &view, 4, 20);
    packed.sort_by(f32::total_cmp);
    let (mid, worst) = (packed[packed.len() / 2], packed[packed.len() - 1]);
    println!(
        "4 busy passes back to back: {} readings, {untimed} frames untimed, median {mid} ms, worst {worst} ms: {packed:?}",
        packed.len()
    );
    // The test submits every frame at once (Bevy keeps a frame or two in flight), so most go
    // untimed: the case under test, which must happen here.
    assert!(
        untimed > 0,
        "every frame found a slot: no chain break was exercised"
    );
    assert!(packed.len() >= 6, "{} readings of 26 frames", packed.len());
    assert!(
        (mid - four).abs() <= 0.35 * four,
        "back-to-back median {mid} ms vs waited {four} ms"
    );
    assert!(
        worst <= 1.3 * mid,
        "back-to-back worst {worst} ms vs their median {mid} ms"
    );
}
