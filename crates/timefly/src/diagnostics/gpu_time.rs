//! GPU frame time from pass-level timestamps (rivers and lakes Task 10, owner 2026-11-25).
//!
//! **Why not Bevy's plugin.** `RenderDiagnosticsPlugin` writes timestamps inside command
//! encoders, which needs `Features::TIMESTAMP_QUERY_INSIDE_ENCODERS`. Apple-silicon Metal and
//! WebGPU don't offer that, so there the plugin records CPU time only.
//!
//! **What this does instead.** [`GpuTimePlugin`] adds two render-graph systems: one first in
//! the graph ([`RenderGraphSystems::Begin`]) and one last (after [`RenderGraphSystems::Render`],
//! before the submit). Each records one-pixel render passes whose `timestamp_writes` put the
//! frame's begin and end timestamps into a `QuerySet`; pass timestamps need only
//! `Features::TIMESTAMP_QUERY`. The end passes load the frame's output targets, and a frame's
//! span starts no earlier than the previous frame's end, so on a GPU that overlaps independent
//! passes (Apple silicon) the timestamps still bracket the frame ([`GpuTimer`] has the
//! measurements behind this).
//!
//! **Readback.** Once the queue reports a frame's work done, a later frame's begin encoder
//! resolves its queries and copies them into one of [`RING`] readback buffers; after that
//! submit the buffer is mapped asynchronously, and a later frame reads it
//! ([`GpuTimer::harvest`], in submission order) and frees it ([`ReadbackRing`]). Nothing ever
//! waits on the GPU: a frame that finds no free slot is not timed.
//!
//! **When it runs.** Only when the device has `Features::TIMESTAMP_QUERY`. Bevy's default
//! `WgpuSettings` (priority `Functionality`) request every feature the adapter offers, so the
//! feature is on exactly when the adapter has it, on Metal and in Chrome alike; this plugin
//! changes no device settings, so no other feature the game needs is dropped. Without it the
//! plugin adds no render systems and [`GpuFrameTime::available`] stays false; the benches then
//! print `gpu time: unavailable`.
//!
//! **Convention.** A frame's GPU time is `(end − begin) × period` ([`frame_ms_from_timestamps`];
//! the queue's timestamp period is in ns, 1 on WebGPU), where `begin` is
//! [`frame_start`]`(begin, previous end)`; a frame whose predecessor wasn't read is dropped
//! ([`read_frame`]). A frame whose end precedes its begin,
//! whose value is not finite, or that has a zero timestamp (a query the GPU never wrote resolves
//! to 0) is dropped and counted in [`GpuFrameTime::dropped`].
//!
//! **Chrome** exposes `timestamp-query` but rounds each timestamp to 100 µs unless the "WebGPU
//! Developer Features" flag (`chrome://flags/#enable-webgpu-developer-features`) is on.
//!
//! The main world gets each reading as a [`GpuFrameMeasured`] message, a few frames after the
//! frame it times (the readback's latency), and [`GpuFrameTime`] keeps the last one.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bevy::prelude::*;
use bevy::render::extract_resource::{ExtractResource, ExtractResourcePlugin};
use bevy::render::renderer::{
    PendingCommandBuffers, RenderDevice, RenderGraph, RenderGraphSystems, RenderQueue,
};
use bevy::render::view::ViewTargetAttachments;
use bevy::render::{RenderApp, RenderStartup};

/// Readback buffers in flight at most (a frame's reading arrives within this many frames, or
/// that frame goes untimed).
pub const RING: usize = 8;

/// The GPU frame time, in the main world.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct GpuFrameTime {
    /// The latest frame's GPU time (ms), once one has been read.
    pub last_ms: Option<f32>,
    /// The queue's timestamp period (ns per tick).
    pub period_ns: f32,
    /// Whether the device has `TIMESTAMP_QUERY`, so frames are timed.
    pub available: bool,
    /// Readings dropped: reversed, zero or non-finite timestamps, or a failed map.
    pub dropped: u32,
}

impl GpuFrameTime {
    /// No timestamps on this device (or no renderer).
    pub fn unavailable() -> Self {
        GpuFrameTime {
            last_ms: None,
            period_ns: 1.0,
            available: false,
            dropped: 0,
        }
    }
}

/// Benchmark that owns the image extracted from this main-world frame.
#[derive(Resource, ExtractResource, Copy, Clone, Debug, Default, PartialEq, Eq)]
#[extract_app(RenderApp)]
pub enum GpuBenchmarkScope {
    #[default]
    Unmeasured,
    Region(usize),
    Journey(u32),
}

/// One frame's GPU time (ms), sent when its readback arrives.
#[derive(Message, Copy, Clone, Debug, PartialEq)]
pub struct GpuFrameMeasured {
    pub frame: u64,
    pub scope: GpuBenchmarkScope,
    pub ms: f32,
}

/// A frame's GPU time (ms) from its begin and end timestamps and the period (ns per tick); `None`
/// for a reversed pair, a zero (unwritten) timestamp or a non-finite result.
pub fn frame_ms_from_timestamps(begin: u64, end: u64, period_ns: f32) -> Option<f32> {
    if begin == 0 || end == 0 || end < begin || !period_ns.is_finite() || period_ns <= 0.0 {
        return None;
    }
    let ms = (end - begin) as f64 * f64::from(period_ns) / 1.0e6;
    let ms = ms as f32;
    ms.is_finite().then_some(ms)
}

/// A slot's state. Each callback moves its slot out of the state set just before it was
/// registered.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SlotState {
    Free = 0,
    /// Acquired this frame; its timestamp passes are being recorded.
    Recording = 1,
    /// Submitted; waiting for the queue to finish the frame's work.
    Running = 2,
    /// The frame's work is done, so its timestamps are written.
    Done = 3,
    /// Its resolve and copy are recorded in a later frame, not yet submitted.
    Resolving = 4,
    /// The copy is submitted and `map_async` called; the callback hasn't run.
    Mapping = 5,
    Mapped = 6,
    /// The map failed.
    Failed = 7,
    /// Handed out by `pop_ready`, not yet released.
    Reading = 8,
}

impl SlotState {
    const ALL: [SlotState; 9] = [
        SlotState::Free,
        SlotState::Recording,
        SlotState::Running,
        SlotState::Done,
        SlotState::Resolving,
        SlotState::Mapping,
        SlotState::Mapped,
        SlotState::Failed,
        SlotState::Reading,
    ];

    fn from_u8(v: u8) -> SlotState {
        SlotState::ALL
            .get(usize::from(v))
            .copied()
            .unwrap_or(SlotState::Failed)
    }
}

/// The slots' bookkeeping: which slot a frame may use, which finished frames to resolve, and
/// which readings are ready, in the order the frames were submitted. The states are shared with
/// the GPU callbacks.
#[derive(Debug)]
pub struct ReadbackRing {
    states: Arc<[AtomicU8; RING]>,
    /// Slots acquired and not yet released, oldest first.
    in_flight: VecDeque<usize>,
}

impl Default for ReadbackRing {
    fn default() -> Self {
        ReadbackRing {
            states: Arc::new(std::array::from_fn(|_| {
                AtomicU8::new(SlotState::Free as u8)
            })),
            in_flight: VecDeque::new(),
        }
    }
}

impl ReadbackRing {
    pub fn state(&self, slot: usize) -> SlotState {
        SlotState::from_u8(self.states[slot].load(Ordering::Acquire))
    }

    fn set(&self, slot: usize, state: SlotState) {
        self.states[slot].store(state as u8, Ordering::Release);
    }

    /// A callback that moves `slot` to `state`.
    fn setter(&self, slot: usize) -> impl Fn(SlotState) + Send + 'static {
        let states = self.states.clone();
        move |state| states[slot].store(state as u8, Ordering::Release)
    }

    /// A free slot for this frame (marked `Recording`), or `None` when every slot is in flight.
    pub fn acquire(&mut self) -> Option<usize> {
        // Only a slot no frame holds: one still running, resolving, mapping, or mapped and
        // unread, is never reused.
        let slot = (0..RING).find(|s| !self.in_flight.contains(s))?;
        self.set(slot, SlotState::Recording);
        self.in_flight.push_back(slot);
        Some(slot)
    }

    /// Gives back a slot whose frame was never submitted.
    pub fn cancel(&mut self, slot: usize) {
        if self.state(slot) == SlotState::Recording {
            self.in_flight.retain(|s| *s != slot);
            self.set(slot, SlotState::Free);
        }
    }

    /// Marks `slot` as submitted and returns the queue's work-done callback, which marks it done.
    pub fn running(&self, slot: usize) -> impl FnOnce() + Send + 'static {
        self.set(slot, SlotState::Running);
        let set = self.setter(slot);
        move || set(SlotState::Done)
    }

    /// The finished frames' slots, oldest first, marked `Resolving` for this frame to resolve.
    pub fn take_done(&mut self) -> Vec<usize> {
        let done: Vec<usize> = self
            .in_flight
            .iter()
            .copied()
            .filter(|&s| self.state(s) == SlotState::Done)
            .collect();
        for &slot in &done {
            self.set(slot, SlotState::Resolving);
        }
        done
    }

    /// The slots whose resolve this frame recorded.
    pub fn resolving(&self) -> Vec<usize> {
        self.in_flight
            .iter()
            .copied()
            .filter(|&s| self.state(s) == SlotState::Resolving)
            .collect()
    }

    /// Marks `slot` as mapping and returns the map callback, which marks it mapped (or failed).
    pub fn mapping(&self, slot: usize) -> impl FnOnce(bool) + Send + 'static {
        self.set(slot, SlotState::Mapping);
        let set = self.setter(slot);
        move |ok| {
            set(if ok {
                SlotState::Mapped
            } else {
                SlotState::Failed
            });
        }
    }

    /// The oldest slot in flight not yet handed out, if its map has finished: `(slot, mapped)`,
    /// marked as being read until [`release`](Self::release). A later slot that finished first
    /// waits, so readings come out in submission order.
    pub fn pop_ready(&mut self) -> Option<(usize, bool)> {
        let &slot = self
            .in_flight
            .iter()
            .find(|&&s| self.state(s) != SlotState::Reading)?;
        let mapped = match self.state(slot) {
            SlotState::Mapped => true,
            SlotState::Failed => false,
            _ => return None,
        };
        self.set(slot, SlotState::Reading);
        Some((slot, mapped))
    }

    /// Frees a slot taken from [`pop_ready`](Self::pop_ready) once it is read and unmapped.
    pub fn release(&mut self, slot: usize) {
        self.in_flight.retain(|s| *s != slot);
        self.set(slot, SlotState::Free);
    }
}

/// End timestamps a frame records at most: one per output target ([`GpuTimer::end`]).
pub const MAX_ENDS: usize = 4;
/// Queries per slot: the begin, then up to [`MAX_ENDS`] ends.
const QUERIES_PER_SLOT: usize = 1 + MAX_ENDS;
/// Bytes of a timestamp.
const TIMESTAMP_BYTES: u64 = 8;

/// The timestamp passes' shader: a triangle, scissored to one pixel, so each pass has vertex
/// and fragment work for its stage-boundary timestamp (a clear-only pass left its end-of-pass
/// timestamp unwritten on Metal).
const TIMESTAMP_PASS_SHADER: &str = r#"
@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 1.0);
}
"#;

/// The begin pass's own target format.
const BEGIN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The timestamp queries and readback buffers (render world). Works on a plain wgpu device, so
/// the GPU test drives it without Bevy.
///
/// Measured on Apple-silicon Metal (M4), where render passes with no data dependency overlap
/// freely, three rules make the marker passes bracket the frame:
/// - **The end waits for the frame's output.** Each end pass loads one of the frame's output
///   targets (write mask empty, one pixel), so it runs after every pass that wrote it, and
///   writes its timestamp at the end of the pass. The frame ends at the latest of them. (An end
///   pass on its own target read 1.7 ms for 26 ms of work.)
/// - **The span starts at the begin or at the last frame's end, whichever is later.** The begin
///   pass (its own target, timestamp at the start of the pass) can't be made to wait for the
///   frame's work, nor the work for it: with frames back to back it starts inside the previous
///   frame (a span read double), so a frame's span is `end − max(begin, previous end)`
///   ([`frame_start`]). When the GPU idles between frames that is the begin; when it is busy,
///   the previous frame's end.
/// - **Resolve after the work is done.** A frame's queries are resolved in a later frame's
///   encoder, once the queue reports its work done: a resolve in the frame's own command buffer
///   read 0 or the previous frame's end.
///
#[derive(Resource)]
pub struct GpuTimer {
    query_set: wgpu::QuerySet,
    /// Slot `k` resolves at `k × QUERY_RESOLVE_BUFFER_ALIGNMENT`.
    resolve: wgpu::Buffer,
    readback: Vec<wgpu::Buffer>,
    begin_target: wgpu::TextureView,
    module: wgpu::ShaderModule,
    /// The begin pass's pipeline, then an end pipeline per target format.
    begin_pipeline: wgpu::RenderPipeline,
    end_pipelines: Vec<(wgpu::TextureFormat, wgpu::RenderPipeline)>,
    ring: ReadbackRing,
    /// End timestamps each slot's frame recorded.
    ends: [usize; RING],
    /// Each slot's frame number ([`begin`](Self::begin) calls so far, timed or not).
    frame_of: [u64; RING],
    scope_of: [GpuBenchmarkScope; RING],
    frames: u64,
    /// The last reading's frame number and end timestamp.
    last_end: Option<(u64, u64)>,
    period_ns: f32,
    /// The slot this frame records into, from [`begin`](Self::begin).
    current: Option<usize>,
    /// Whether this frame's end timestamps are recorded.
    ended: bool,
}

impl GpuTimer {
    /// Needs `Features::TIMESTAMP_QUERY` on `device`.
    pub fn new(device: &wgpu::Device, period_ns: f32) -> Self {
        let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("gpu time"),
            ty: wgpu::QueryType::Timestamp,
            count: (QUERIES_PER_SLOT * RING) as u32,
        });
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gpu time resolve"),
            size: wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT * RING as u64,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = (0..RING)
            .map(|_| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("gpu time readback"),
                    size: TIMESTAMP_BYTES * QUERIES_PER_SLOT as u64,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let begin_target = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("gpu time begin target"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: BEGIN_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("gpu time pass"),
            source: wgpu::ShaderSource::Wgsl(TIMESTAMP_PASS_SHADER.into()),
        });
        let begin_pipeline =
            timestamp_pipeline(device, &module, BEGIN_FORMAT, wgpu::ColorWrites::ALL);
        GpuTimer {
            query_set,
            resolve,
            readback,
            begin_target,
            module,
            begin_pipeline,
            end_pipelines: Vec::new(),
            ring: ReadbackRing::default(),
            ends: [0; RING],
            frame_of: [0; RING],
            scope_of: [GpuBenchmarkScope::Unmeasured; RING],
            frames: 0,
            last_end: None,
            period_ns,
            current: None,
            ended: false,
        }
    }

    pub fn period_ns(&self) -> f32 {
        self.period_ns
    }

    /// A one-pixel pass over `view` that writes query `index` at its start (`begin`) or end.
    fn timestamp_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        pipeline: &wgpu::RenderPipeline,
        load: wgpu::LoadOp<wgpu::Color>,
        index: u32,
        begin: bool,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gpu time"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: Some(wgpu::RenderPassTimestampWrites {
                query_set: &self.query_set,
                beginning_of_pass_write_index: begin.then_some(index),
                end_of_pass_write_index: (!begin).then_some(index),
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_scissor_rect(0, 0, 1, 1);
        pass.draw(0..3, 0..1);
    }

    /// Starts a frame: resolves the finished frames' timestamps into their readback buffers,
    /// then (if a slot is free) records the begin pass. Returns whether anything was recorded
    /// into `encoder`.
    pub fn begin(&mut self, encoder: &mut wgpu::CommandEncoder) -> bool {
        self.begin_scoped(encoder, GpuBenchmarkScope::Unmeasured)
    }

    fn begin_scoped(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        scope: GpuBenchmarkScope,
    ) -> bool {
        // A frame that began and was never submitted gives its slot back.
        if let Some(slot) = self.current.take() {
            self.ring.cancel(slot);
        }
        self.ended = false;
        self.frames += 1;
        let done = self.ring.take_done();
        for &slot in &done {
            let at = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT * slot as u64;
            let first = (QUERIES_PER_SLOT * slot) as u32;
            let count = 1 + self.ends[slot] as u32;
            encoder.resolve_query_set(&self.query_set, first..first + count, &self.resolve, at);
            encoder.copy_buffer_to_buffer(
                &self.resolve,
                at,
                &self.readback[slot],
                0,
                TIMESTAMP_BYTES * u64::from(count),
            );
        }
        let Some(slot) = self.ring.acquire() else {
            return !done.is_empty();
        };
        self.current = Some(slot);
        self.frame_of[slot] = self.frames;
        self.scope_of[slot] = scope;
        self.timestamp_pass(
            encoder,
            &self.begin_target,
            &self.begin_pipeline,
            wgpu::LoadOp::Clear(wgpu::Color::BLACK),
            (QUERIES_PER_SLOT * slot) as u32,
            true,
        );
        true
    }

    /// Ends the frame begun by [`begin`](Self::begin): one end pass per output target (the
    /// first [`MAX_ENDS`]), each loading it so it runs after the passes that wrote it; with no
    /// target, one pass on the begin pass's own target. False when the frame isn't being
    /// timed.
    pub fn end(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        targets: &[(&wgpu::TextureView, wgpu::TextureFormat)],
    ) -> bool {
        let Some(slot) = self.current.filter(|_| !self.ended) else {
            return false;
        };
        let first = (QUERIES_PER_SLOT * slot) as u32 + 1;
        let targets = &targets[..targets.len().min(MAX_ENDS)];
        for &(_, format) in targets {
            if !self.end_pipelines.iter().any(|(f, _)| *f == format) {
                let pipeline =
                    timestamp_pipeline(device, &self.module, format, wgpu::ColorWrites::empty());
                self.end_pipelines.push((format, pipeline));
            }
        }
        if targets.is_empty() {
            self.timestamp_pass(
                encoder,
                &self.begin_target,
                &self.begin_pipeline,
                wgpu::LoadOp::Load,
                first,
                false,
            );
            self.ends[slot] = 1;
        } else {
            for (i, &(view, format)) in targets.iter().enumerate() {
                let Some((_, pipeline)) = self.end_pipelines.iter().find(|(f, _)| *f == format)
                else {
                    continue;
                };
                self.timestamp_pass(
                    encoder,
                    view,
                    pipeline,
                    wgpu::LoadOp::Load,
                    first + i as u32,
                    false,
                );
            }
            self.ends[slot] = targets.len();
        }
        self.ended = true;
        true
    }

    /// After the frame's submit: asks the queue to report the timed frame's work done, and maps
    /// the readback buffers this frame copied into (asynchronously).
    pub fn submitted(&mut self, queue: &wgpu::Queue) {
        for slot in self.ring.resolving() {
            let done = self.ring.mapping(slot);
            self.readback[slot]
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| done(result.is_ok()));
        }
        if let Some(slot) = self.current.take() {
            if std::mem::take(&mut self.ended) {
                queue.on_submitted_work_done(self.ring.running(slot));
            } else {
                self.ring.cancel(slot);
            }
        }
    }

    /// The readings whose maps have finished, oldest first: `Some(ms)`, or `None` for a dropped
    /// one. Never waits.
    pub fn harvest(&mut self) -> Vec<Option<f32>> {
        self.harvest_frames()
            .into_iter()
            .map(|r| r.map(|f| f.ms))
            .collect()
    }

    fn harvest_frames(&mut self) -> Vec<Option<GpuFrameMeasured>> {
        let mut out = Vec::new();
        while let Some((slot, mapped)) = self.ring.pop_ready() {
            let reading = mapped.then(|| {
                let buffer = &self.readback[slot];
                // A range that won't map reads as zeros, which drops the frame.
                let words: Vec<u64> = match buffer.slice(..).get_mapped_range() {
                    Ok(bytes) => bytes
                        .as_chunks::<{ TIMESTAMP_BYTES as usize }>()
                        .0
                        .iter()
                        .take(1 + self.ends[slot])
                        .map(|b| u64::from_le_bytes(*b))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                buffer.unmap();
                let begin = words.first().copied().unwrap_or(0);
                let end = words.iter().skip(1).copied().max().unwrap_or(0);
                let (ms, last) = read_frame(
                    begin,
                    end,
                    self.frame_of[slot],
                    self.last_end,
                    self.period_ns,
                );
                self.last_end = last;
                ms
            });
            self.ring.release(slot);
            out.push(reading.flatten().map(|ms| GpuFrameMeasured {
                ms,
                frame: self.frame_of[slot],
                scope: self.scope_of[slot],
            }));
        }
        out
    }
}

/// One frame's reading from its begin and latest end timestamps, frame number `frame`, and the
/// last reading's `(frame, end)`: the frame's GPU time (ms), and the `(frame, end)` to keep for
/// the next frame.
///
/// The span starts at [`frame_start`]`(begin, previous end)`, so only a frame whose predecessor
/// was read can be timed: after a frame that found no free slot, or whose reading was dropped,
/// the begin may have run inside that frame (a span read up to double; review, fix round 1),
/// so the reading is dropped. Its end is still kept, when it is valid, to link the next frame.
pub fn read_frame(
    begin: u64,
    end: u64,
    frame: u64,
    last_end: Option<(u64, u64)>,
    period_ns: f32,
) -> (Option<f32>, Option<(u64, u64)>) {
    let previous = last_end.filter(|(f, _)| *f + 1 == frame).map(|(_, e)| e);
    // Also checks the pair itself: a zero or reversed pair keeps nothing to link.
    if frame_ms_from_timestamps(begin, end, period_ns).is_none() {
        return (None, None);
    }
    let ms = previous
        .and_then(|p| frame_ms_from_timestamps(frame_start(begin, Some(p)), end, period_ns));
    (ms, Some((frame, end)))
}

/// When a frame's GPU span starts: its begin timestamp, or the previous frame's end when that is
/// later (the GPU was still busy with it), when the previous frame was read.
pub fn frame_start(begin: u64, previous_end: Option<u64>) -> u64 {
    previous_end.map_or(begin, |end| begin.max(end))
}

/// A timestamp pass's pipeline for `format`.
fn timestamp_pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    write_mask: wgpu::ColorWrites,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("gpu time pass"),
        layout: None,
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Readings passed from the render world to the main world (`None` = dropped).
#[derive(Resource, Clone, Default)]
struct GpuTimeShared(Arc<Mutex<Vec<Option<GpuFrameMeasured>>>>);

impl GpuTimeShared {
    fn push(&self, readings: Vec<Option<GpuFrameMeasured>>) {
        if !readings.is_empty() {
            let mut shared = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            shared.extend(readings);
        }
    }

    fn take(&self) -> Vec<Option<GpuFrameMeasured>> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Measures every frame's GPU time where the device has `TIMESTAMP_QUERY` (see the module docs).
pub struct GpuTimePlugin;

impl Plugin for GpuTimePlugin {
    fn build(&self, app: &mut App) {
        let shared = GpuTimeShared::default();
        app.init_resource::<GpuBenchmarkScope>()
            .add_plugins(ExtractResourcePlugin::<GpuBenchmarkScope>::default())
            .insert_resource(GpuFrameTime::unavailable())
            .insert_resource(shared.clone())
            .add_message::<GpuFrameMeasured>()
            .add_systems(PreUpdate, receive_gpu_times);
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.insert_resource(shared);
        }
    }

    fn finish(&self, app: &mut App) {
        let render = app.get_sub_app(RenderApp);
        let features = render
            .and_then(|r| r.world().get_resource::<RenderDevice>())
            .map_or(wgpu::Features::empty(), RenderDevice::features);
        let period_ns = render
            .and_then(|r| r.world().get_resource::<RenderQueue>())
            .map_or(1.0, |q| q.get_timestamp_period());
        install(app, features, period_ns);
    }
}

/// Adds the render-graph systems when `features` has `TIMESTAMP_QUERY` (and a render app
/// exists), and records the answer in [`GpuFrameTime`]. Returns whether frames will be timed.
pub(crate) fn install(app: &mut App, features: wgpu::Features, period_ns: f32) -> bool {
    let available =
        features.contains(wgpu::Features::TIMESTAMP_QUERY) && app.get_sub_app(RenderApp).is_some();
    app.insert_resource(GpuFrameTime {
        period_ns,
        available,
        ..GpuFrameTime::unavailable()
    });
    if !available {
        info!("gpu time: unavailable (the device has no TIMESTAMP_QUERY)");
        return false;
    }
    info!(period_ns, "gpu time: measuring frames with pass timestamps");
    app.sub_app_mut(RenderApp)
        .add_systems(RenderStartup, init_gpu_timer)
        .add_systems(
            RenderGraph,
            (
                gpu_time_begin
                    .in_set(RenderGraphSystems::Begin)
                    .before(RenderGraphSystems::Render),
                gpu_time_end
                    .after(RenderGraphSystems::Render)
                    .before(RenderGraphSystems::Submit),
                gpu_time_map
                    .in_set(RenderGraphSystems::Finish)
                    .after(RenderGraphSystems::Submit),
            ),
        );
    true
}

fn init_gpu_timer(mut commands: Commands, device: Res<RenderDevice>, queue: Res<RenderQueue>) {
    commands.insert_resource(GpuTimer::new(
        device.wgpu_device(),
        queue.get_timestamp_period(),
    ));
}

/// First in the graph: hands finished readings to the main world, then writes the frame's
/// begin timestamp.
fn gpu_time_begin(
    timer: Option<ResMut<GpuTimer>>,
    device: Res<RenderDevice>,
    shared: Res<GpuTimeShared>,
    scope: Res<GpuBenchmarkScope>,
    mut pending: ResMut<PendingCommandBuffers>,
) {
    let Some(mut timer) = timer else { return };
    // Natively map callbacks run when the device is polled (a submit also polls); on the web
    // the browser runs them.
    #[cfg(not(target_arch = "wasm32"))]
    let _ = device.poll(wgpu::PollType::Poll);
    shared.push(timer.harvest_frames());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu time begin"),
    });
    if timer.begin_scoped(&mut encoder, *scope) {
        pending.push_encoder(encoder, "gpu_time_begin");
    }
}

/// Last in the graph, before the submit: writes the end timestamps, after the frame's output
/// targets.
fn gpu_time_end(
    timer: Option<ResMut<GpuTimer>>,
    device: Res<RenderDevice>,
    outputs: Option<Res<ViewTargetAttachments>>,
    mut pending: ResMut<PendingCommandBuffers>,
) {
    let Some(mut timer) = timer else { return };
    let targets: Vec<(&wgpu::TextureView, wgpu::TextureFormat)> = outputs
        .as_deref()
        .map(|o| o.values().map(|a| (&*a.view, a.view_format)).collect())
        .unwrap_or_default();
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gpu time end"),
    });
    if timer.end(&mut encoder, device.wgpu_device(), &targets) {
        pending.push_encoder(encoder, "gpu_time_end");
    }
}

/// After the submit: watches the frame's work and maps the buffers it copied into.
fn gpu_time_map(timer: Option<ResMut<GpuTimer>>, queue: Res<RenderQueue>) {
    if let Some(mut timer) = timer {
        timer.submitted(&queue);
    }
}

/// Main world: takes the render world's readings into [`GpuFrameTime`] and [`GpuFrameMeasured`].
fn receive_gpu_times(
    shared: Res<GpuTimeShared>,
    mut time: ResMut<GpuFrameTime>,
    mut measured: MessageWriter<GpuFrameMeasured>,
) {
    for reading in shared.take() {
        match reading {
            Some(frame) => {
                time.last_ms = Some(frame.ms);
                measured.write(frame);
            }
            None => time.dropped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_ms_from_timestamps_converts_with_the_period_and_drops_bad_pairs() {
        // WebGPU: nanosecond ticks.
        assert_eq!(frame_ms_from_timestamps(1_000, 4_121_000, 1.0), Some(4.12));
        // A coarser period scales the ticks.
        let ms = frame_ms_from_timestamps(10, 10 + 100_000, 41.666_67).unwrap();
        assert!((ms - 4.166_667).abs() < 1e-5, "{ms}");
        // Equal timestamps are a zero-length frame, not an error.
        assert_eq!(frame_ms_from_timestamps(5, 5, 1.0), Some(0.0));
        // Dropped: the end before the begin, an unwritten (zero) timestamp, a bad period.
        assert_eq!(frame_ms_from_timestamps(4_121_000, 1_000, 1.0), None);
        assert_eq!(frame_ms_from_timestamps(0, 4_121_000, 1.0), None);
        assert_eq!(frame_ms_from_timestamps(1_000, 0, 1.0), None);
        assert_eq!(frame_ms_from_timestamps(1, 2, f32::NAN), None);
        assert_eq!(frame_ms_from_timestamps(1, 2, f32::INFINITY), None);
        assert_eq!(frame_ms_from_timestamps(1, 2, 0.0), None);
        assert_eq!(frame_ms_from_timestamps(1, 2, -1.0), None);
        // Huge spans stay finite in f64 before the cast.
        assert!(frame_ms_from_timestamps(1, u64::MAX, 1.0).is_some());
    }

    #[test]
    fn a_frame_starts_no_earlier_than_the_previous_frames_end() {
        // The GPU idled between frames: the begin.
        assert_eq!(frame_start(1_000, Some(900)), 1_000);
        // Back to back: the begin ran inside the previous frame, which ended later.
        assert_eq!(frame_start(1_000, Some(4_000)), 4_000);
        // The previous frame wasn't read.
        assert_eq!(frame_start(1_000, None), 1_000);
    }

    /// Review finding (fix round 1): a frame whose predecessor wasn't read (it found no free
    /// slot, or its reading was dropped) can't tell whether its begin ran inside that frame,
    /// so its reading is dropped; its end still links the next frame.
    #[test]
    fn a_frame_after_an_unread_frame_is_dropped_but_links_the_next() {
        // Frame 7 read, frame 8 untimed (no slot), frame 9's begin ran inside frame 8.
        let last = Some((7, 10_000_000));
        let (ms, last) = read_frame(11_000_000, 19_000_000, 9, last, 1.0);
        assert_eq!(ms, None, "8 ms of frames 8 and 9 together, not frame 9's");
        assert_eq!(last, Some((9, 19_000_000)));
        // Frame 10 follows frame 9: it starts at 9's end.
        let (ms, last) = read_frame(18_000_000, 23_000_000, 10, last, 1.0);
        assert_eq!(ms, Some(4.0));
        assert_eq!(last, Some((10, 23_000_000)));
        // The first frame of a session has no predecessor either.
        assert_eq!(read_frame(1_000, 2_000_000, 1, None, 1.0).0, None);
        // An unwritten end breaks the chain.
        let (ms, last) = read_frame(24_000_000, 0, 11, last, 1.0);
        assert_eq!((ms, last), (None, None));
        // A reversed pair is dropped too, and keeps nothing to link.
        let (ms, last) = read_frame(30, 20, 12, Some((11, 10)), 1.0);
        assert_eq!((ms, last), (None, None));
    }

    #[test]
    fn the_readback_ring_never_hands_out_a_slot_still_being_mapped() {
        let mut ring = ReadbackRing::default();
        let slots: Vec<usize> = (0..RING).filter_map(|_| ring.acquire()).collect();
        assert_eq!(slots, (0..RING).collect::<Vec<_>>());
        assert_eq!(ring.acquire(), None, "every slot is recording");
        // Submitted: each waits for its frame's work.
        let mut work_done: Vec<_> = slots.iter().map(|&s| Some(ring.running(s))).collect();
        assert_eq!(ring.acquire(), None, "every slot is running");
        assert!(ring.take_done().is_empty());
        // Frames 0 and 1 finish; a later frame resolves them, then maps them.
        work_done[1].take().unwrap()();
        work_done[0].take().unwrap()();
        assert_eq!(ring.take_done(), [0, 1]);
        assert!(ring.take_done().is_empty(), "taken once");
        assert_eq!(ring.resolving(), [0, 1]);
        assert_eq!(ring.acquire(), None, "resolving slots are not free");
        let c0 = ring.mapping(0);
        let c1 = ring.mapping(1);
        assert!(ring.resolving().is_empty());
        assert_eq!(ring.state(0), SlotState::Mapping);
        assert_eq!(ring.acquire(), None, "every slot is mapping or running");
        assert_eq!(ring.pop_ready(), None);
        // Slot 1's map finishes first: it waits behind slot 0, and nothing is handed out.
        c1(true);
        assert_eq!(ring.state(1), SlotState::Mapped);
        assert_eq!(ring.pop_ready(), None, "readings keep their order");
        assert_eq!(ring.acquire(), None, "a mapped slot is not free until read");
        c0(true);
        assert_eq!(ring.pop_ready(), Some((0, true)));
        assert_eq!(ring.acquire(), None, "popped but not yet released");
        ring.release(0);
        assert_eq!(ring.acquire(), Some(0), "the released slot only");
        assert_eq!(ring.acquire(), None, "slots 2 and 3 are still running");
        assert_eq!(ring.pop_ready(), Some((1, true)));
        ring.release(1);
        // A failed map comes out as such, in turn.
        work_done[2].take().unwrap()();
        work_done[3].take().unwrap()();
        assert_eq!(ring.take_done(), [2, 3]);
        let (c2, c3) = (ring.mapping(2), ring.mapping(3));
        c3(false);
        c2(true);
        assert_eq!(ring.pop_ready(), Some((2, true)));
        assert_eq!(ring.pop_ready(), Some((3, false)));
        // Slot 0, acquired again after them, is still recording.
        assert_eq!(ring.pop_ready(), None);
        ring.cancel(0);
        assert_eq!(
            ring.state(0),
            SlotState::Free,
            "an unsubmitted frame gives its slot back"
        );
        assert_eq!(ring.acquire(), Some(0));
    }

    /// An app with an empty render sub-app holding Bevy's render-graph schedule.
    fn app_with_render_graph() -> App {
        let mut app = App::new();
        let mut render = bevy::app::SubApp::new();
        render.add_schedule(RenderGraph::base_schedule());
        app.insert_sub_app(RenderApp, render);
        app.add_plugins(GpuTimePlugin);
        app
    }

    fn render_graph_systems(app: &App) -> usize {
        app.get_sub_app(RenderApp)
            .unwrap()
            .world()
            .resource::<Schedules>()
            .get(RenderGraph)
            .unwrap()
            .systems_len()
    }

    #[test]
    fn without_timestamp_query_the_plugin_adds_no_nodes_and_reports_unavailable() {
        let mut app = app_with_render_graph();
        assert!(!install(&mut app, wgpu::Features::empty(), 1.0));
        assert_eq!(render_graph_systems(&app), 0);
        let time = app.world().resource::<GpuFrameTime>();
        assert!(!time.available);
        assert_eq!(time.last_ms, None);
        assert_eq!(
            crate::bench::gpu_frame_line("Indus tributaries", &[], false),
            "gpu time: unavailable"
        );

        // With the feature: the begin, end and map systems.
        let mut app = app_with_render_graph();
        assert!(install(
            &mut app,
            wgpu::Features::TIMESTAMP_QUERY,
            41.666_67
        ));
        assert_eq!(render_graph_systems(&app), 3);
        let time = app.world().resource::<GpuFrameTime>();
        assert!(time.available);
        assert_eq!(time.period_ns, 41.666_67);

        // No renderer at all: unavailable.
        let mut bare = App::new();
        bare.add_plugins(GpuTimePlugin);
        assert!(!install(&mut bare, wgpu::Features::TIMESTAMP_QUERY, 1.0));
        assert!(!bare.world().resource::<GpuFrameTime>().available);
    }

    #[test]
    fn readings_reach_the_main_world_in_order_and_drops_are_counted() {
        let mut app = App::new();
        app.add_plugins(GpuTimePlugin);
        let first = GpuFrameMeasured {
            frame: 8,
            scope: GpuBenchmarkScope::Unmeasured,
            ms: 4.0,
        };
        let second = GpuFrameMeasured {
            frame: 10,
            scope: GpuBenchmarkScope::Journey(0),
            ms: 5.5,
        };
        app.insert_resource(GpuBenchmarkScope::Journey(1));
        app.world()
            .resource::<GpuTimeShared>()
            .push(vec![Some(first), None, Some(second)]);
        app.update();
        let time = app.world().resource::<GpuFrameTime>();
        assert_eq!((time.last_ms, time.dropped), (Some(5.5), 1));
        let seen: Vec<_> = app
            .world()
            .resource::<Messages<GpuFrameMeasured>>()
            .iter_current_update_messages()
            .copied()
            .collect();
        assert_eq!(seen, [first, second]);
    }
}
