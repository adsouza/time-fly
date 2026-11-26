//! The hop-memory trace (M3), off by default. With
//! `TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace` a native run logs to the session
//! file, at `trace` under [`TARGET`]:
//!
//! - one `frame` line per region frame: the process's memory split ([`memory::split`]: the
//!   footprint, malloc's dirty pages, the graphics ledger, malloc's bytes in use and held) and
//!   the bytes it can attribute (the neighbour's grid, the job's stage, the terrain's image data
//!   on the CPU, the field textures in the render world);
//! - a `generation` line at each step of a native region generation, from its worker thread,
//!   with the bytes the step holds (`crate::diagnostics::hop_memory::trace_generation`, which
//!   product code calls);
//! - a `render world` line whenever the render world's field textures change.
//!
//! It is how `docs/testing/m3/performance.md` explains the hop's memory peak. With the target
//! off, the systems return at once and nothing is sampled.

use std::sync::{Arc, Mutex};

use bevy::diagnostic::FrameCount;
use bevy::prelude::*;
use bevy::render::render_asset::RenderAssets;
use bevy::render::texture::GpuImage;
use bevy::render::{Render, RenderApp, RenderSystems};
use tracing::trace;

use crate::bench::memory;
pub use crate::diagnostics::hop_memory::{TARGET, enabled};
use crate::region::navigate::{NavPhase, Navigation};
use crate::region::{ActiveRegion, PendingRegion, RegionJob};
use crate::terrain::{TerrainScene, TerrainSceneSystems};

/// A texture or image this large or larger counts as one of the terrain's fields (the apron's
/// are 513² × 4 B, about 1.05 MB; nothing else in a region is as large).
const FIELD_BYTES: u64 = 1_000_000;

/// The render world's textures, written in the render world, read in the main world.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct GpuImageTotals {
    count: usize,
    bytes: u64,
    fields: usize,
    field_bytes: u64,
    /// The main-world frame the render world last counted for.
    frame: u32,
}

#[derive(Resource, Clone, Default)]
struct GpuImageStats(Arc<Mutex<GpuImageTotals>>);

/// The trace's systems: the main world's sampler, after the terrain scene has seen the frame's
/// region (so a swap frame shows its new textures), and the render world's count.
pub fn add_trace(app: &mut App) {
    let stats = GpuImageStats::default();
    app.insert_resource(stats.clone())
        .add_systems(Update, sample_frame.after(TerrainSceneSystems));
    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
        render_app.insert_resource(stats).add_systems(
            Render,
            count_gpu_images.in_set(RenderSystems::PrepareResources),
        );
    }
}

/// A texture's bytes: every mip level of every layer.
fn texture_bytes(image: &GpuImage) -> u64 {
    let d = &image.texture_descriptor;
    let format = d.format;
    let (bw, bh) = format.block_dimensions();
    let block = u64::from(format.block_copy_size(None).unwrap_or(4));
    (0..d.mip_level_count)
        .map(|level| {
            let w = (d.size.width >> level).max(1).div_ceil(bw);
            let h = (d.size.height >> level).max(1).div_ceil(bh);
            u64::from(w) * u64::from(h) * u64::from(d.size.depth_or_array_layers) * block
        })
        .sum()
}

fn count_gpu_images(
    images: Res<RenderAssets<GpuImage>>,
    stats: Res<GpuImageStats>,
    frame: Option<Res<FrameCount>>,
) {
    if !enabled() {
        return;
    }
    let mut totals = GpuImageTotals {
        frame: frame.map_or(0, |f| f.0),
        ..default()
    };
    for (_, image) in images.iter() {
        let bytes = texture_bytes(image);
        totals.count += 1;
        totals.bytes += bytes;
        if bytes >= FIELD_BYTES {
            totals.fields += 1;
            totals.field_bytes += bytes;
        }
    }
    let Ok(mut shared) = stats.0.lock() else {
        return;
    };
    if (shared.fields, shared.field_bytes) != (totals.fields, totals.field_bytes) {
        trace!(
            target: TARGET,
            frame = totals.frame,
            from = shared.fields,
            to = totals.fields,
            field_bytes = totals.field_bytes,
            memory = ?memory::split(),
            "render world: field textures changed"
        );
    }
    *shared = totals;
}

/// A short name for the phase.
fn phase_name(phase: &NavPhase) -> &'static str {
    match phase {
        NavPhase::Globe => "globe",
        NavPhase::Flying => "flying",
        NavPhase::Hopping { swapped: false, .. } => "hop",
        NavPhase::Hopping { swapped: true, .. } => "hop-swapped",
        NavPhase::Diving { .. } => "diving",
        NavPhase::Emerging { .. } => "emerging",
        NavPhase::Climbing { .. } => "climbing",
        _ => "other",
    }
}

/// A grid's identity in the trace: the low bits of its address.
fn grid_id(grid: &Arc<timefly_core::worldgen::generate::RegionGrid>) -> String {
    format!("{:x}", (Arc::as_ptr(grid) as usize) & 0xff_ffff)
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn sample_frame(
    frame: Res<FrameCount>,
    nav: Option<Res<Navigation>>,
    pending: Option<Res<PendingRegion>>,
    active: Option<Res<ActiveRegion>>,
    scene: Option<Res<TerrainScene>>,
    job: Option<Res<RegionJob>>,
    images: Res<Assets<Image>>,
    gpu: Res<GpuImageStats>,
) {
    if !enabled() {
        return;
    }
    let Some(nav) = nav else { return };
    if nav.phase == NavPhase::Globe {
        return;
    }
    let (cpu_fields, cpu_field_bytes) = images
        .iter()
        .filter_map(|(_, i)| i.data.as_ref().map(|d| d.len() as u64))
        .filter(|&b| b >= FIELD_BYTES)
        .fold((0usize, 0u64), |(n, sum), b| (n + 1, sum + b));
    let gpu = gpu.0.lock().map(|g| *g).unwrap_or_default();
    let neighbour = pending.as_ref().and_then(|p| p.region.as_ref());
    trace!(
        target: TARGET,
        frame = frame.0,
        phase = phase_name(&nav.phase),
        density = nav.density,
        memory = ?memory::split(),
        neighbour_grid = neighbour.map_or(0, |r| r.grid.bytes()),
        neighbour = neighbour.map_or_else(String::new, |r| grid_id(&r.grid)),
        active = active.as_ref().map_or_else(String::new, |r| grid_id(&r.grid)),
        scene = scene.as_ref().map_or_else(String::new, |s| grid_id(&s.grid)),
        job = job.as_ref().map_or("", |j| j.describe()),
        cpu_fields,
        cpu_field_bytes,
        gpu_fields = gpu.fields,
        gpu_field_bytes = gpu.field_bytes,
        gpu_images = gpu.count,
        gpu_bytes = gpu.bytes,
        gpu_frame = gpu.frame,
        "frame"
    );
}
