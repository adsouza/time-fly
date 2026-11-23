//! GPU golden test for the region shaders. Needs a GPU adapter:
//! `cargo test -p timefly --test region_gpu -- --ignored`.
//!
//! Compiles `assets/shaders/noise.wesl` (plain WGSL, no imports) with a small compute entry point
//! on a headless wgpu device, runs it over the golden inputs that the Rust implementation also
//! checks (`timefly_core/tests/fixtures/noise_golden.json`) and compares the values.

use bevy::tasks::block_on;
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
