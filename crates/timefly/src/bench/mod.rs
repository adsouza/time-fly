//! The scrub benchmark (`--scrub-bench`, web `?scrub-bench`).
//!
//! After the first interactive frame and a short settle, it drives [`GlobeTime`] through a
//! deterministic full-range scrub in each direction ([`scrub_schedule`]: [`SCRUB_SECS`] from the
//! present to 540 Ma, then back), crossing the bridge both ways, and records every frame time.
//! It then logs one plain-text block (session, adapter, OS, resolution, bake, cold load, memory,
//! median / p95 / worst frame times and pass or fail) that can be pasted back from any machine.
//! On native it exits after logging.
//!
//! **Pass rule (ruling R14).** When frames are paced by the display (always on the web, and on
//! native whenever the median sits on the display's refresh interval even though an uncapped
//! present mode was asked for), frame deltas jitter around the interval, so a run passes when
//! at most [`MAX_MISSED_SHARE`] of frames are missed (longer than [`MISSED_INTERVALS`] target
//! intervals) and the median is no more than [`MEDIAN_TOLERANCE`] above the target interval. When
//! an uncapped present mode is really in effect, the run passes when p95 <= the target interval.
//! The block says which rule decided.
//!
//! If the globe fails instead of becoming interactive, it logs a FAIL block with the reason.
//! On native it exits after logging: 0 on PASS, [`FAILED_RESULT_EXIT_CODE`] on FAIL (a result,
//! not a crash, so the next launch shows no crash notice).
//!
//! The region bench (`--region-bench`, web `?region-bench`) lives in [`region`] and shares the
//! statistics, the pass rule, the block's header and the exit codes.

pub mod region;

use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::window::{Monitor, PrimaryMonitor, PrimaryWindow};
use timefly_core::deep_time::{YearsBp, slider_to_years};
use timefly_core::surface::FrameSource;

use crate::data::GlobeData;
use crate::diagnostics::session::FAILED_RESULT_EXIT_CODE;
use crate::diagnostics::{FirstInteractiveFrame, SessionInfo};
use crate::globe::GlobeRenderStatus;
use crate::time::{GlobeTime, TimeChangeCause, TimeChanged};

/// Seconds per scrub direction.
pub const SCRUB_SECS: f64 = 20.0;
/// Seconds between the first interactive frame and the start of the scrub.
pub const SETTLE_SECS: f64 = 1.0;
/// Default frame-rate targets (spec): 60 fps on desktop, 30 fps in the browser.
pub const NATIVE_TARGET_FPS: u32 = 60;
pub const WEB_TARGET_FPS: u32 = 30;
/// The bridge window, years BP (26-30 ka), reported separately.
pub const BRIDGE_YEARS: (f64, f64) = (26.0e3, 30.0e3);
/// A frame longer than this many target budgets counts as a stall.
pub const STALL_BUDGETS: f32 = 2.0;
/// Display-paced rule: a frame longer than this many target intervals is a missed frame...
pub const MISSED_INTERVALS: f32 = 1.5;
/// ...and at most this share of frames may be missed...
pub const MAX_MISSED_SHARE: f64 = 0.01;
/// ...and the median may exceed the target interval by at most this fraction. (A display faster
/// than the target paces below the interval, which passes.)
pub const MEDIAN_TOLERANCE: f32 = 0.05;
/// Refresh rates tried when the display's own rate is unknown.
const COMMON_REFRESH_HZ: [f32; 8] = [50.0, 60.0, 75.0, 90.0, 100.0, 120.0, 144.0, 165.0];

/// The age at `elapsed_secs` into the scrub: linear in slider position, present to 540 Ma over
/// [`SCRUB_SECS`], then back to the present; clamped outside `[0, 2 * SCRUB_SECS]`.
pub fn scrub_schedule(elapsed_secs: f64) -> YearsBp {
    let t = (elapsed_secs / SCRUB_SECS).clamp(0.0, 2.0);
    let s = if t <= 1.0 { t } else { 2.0 - t };
    slider_to_years(s)
}

/// Frame-time statistics, in milliseconds.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub frames: usize,
    pub median_ms: f32,
    pub p95_ms: f32,
    pub worst_ms: f32,
}

impl Stats {
    /// Whether the 95th percentile meets the target frame rate.
    pub fn passes(&self, target_fps: u32) -> bool {
        self.frames > 0 && self.p95_ms <= frame_budget_ms(target_fps)
    }
}

/// How frames reached the screen.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pacing {
    /// Paced by the display (vsync, the browser's `requestAnimationFrame`, or a compositor).
    DisplayPaced,
    /// Rendered as fast as possible.
    Uncapped,
}

/// Decides the pacing actually in effect. The web and vsync present modes are display-paced;
/// an uncapped request counts as uncapped only if the median frame time is not within
/// [`MEDIAN_TOLERANCE`] of the display's refresh interval (or, when the rate is unknown, of a
/// common one).
pub fn pacing(
    web: bool,
    uncapped_requested: bool,
    refresh_hz: Option<f32>,
    median_ms: f32,
) -> Pacing {
    if web || !uncapped_requested {
        return Pacing::DisplayPaced;
    }
    let on_interval = |hz: f32| {
        let interval = 1000.0 / hz;
        (median_ms - interval).abs() <= MEDIAN_TOLERANCE * interval
    };
    let paced = match refresh_hz.filter(|hz| *hz > 1.0) {
        Some(hz) => on_interval(hz),
        None => COMMON_REFRESH_HZ.into_iter().any(on_interval),
    };
    if paced {
        Pacing::DisplayPaced
    } else {
        Pacing::Uncapped
    }
}

/// Frames longer than [`MISSED_INTERVALS`] target intervals.
pub fn missed_frames(frame_ms: &[f32], target_fps: u32) -> usize {
    let limit = MISSED_INTERVALS * frame_budget_ms(target_fps);
    frame_ms.iter().filter(|&&ms| ms > limit).count()
}

/// Whether one direction's frames pass under `pacing` (see the module docs).
pub fn passes(frame_ms: &[f32], target_fps: u32, pacing: Pacing) -> bool {
    let stats = frame_time_stats(frame_ms);
    if stats.frames == 0 {
        return false;
    }
    match pacing {
        Pacing::Uncapped => stats.passes(target_fps),
        Pacing::DisplayPaced => {
            let missed = missed_frames(frame_ms, target_fps) as f64 / stats.frames as f64;
            missed <= MAX_MISSED_SHARE
                && stats.median_ms <= (1.0 + MEDIAN_TOLERANCE) * frame_budget_ms(target_fps)
        }
    }
}

/// Milliseconds per frame at `fps`.
pub fn frame_budget_ms(fps: u32) -> f32 {
    1000.0 / fps.max(1) as f32
}

/// Median, 95th percentile (nearest rank: the smallest value with at least that share of
/// frames at or below it) and worst of `frame_ms`.
pub fn frame_time_stats(frame_ms: &[f32]) -> Stats {
    let mut sorted: Vec<f32> = frame_ms.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return Stats::default();
    }
    sorted.sort_by(f32::total_cmp);
    let rank = |p: f64| {
        let r = (p * sorted.len() as f64).ceil() as usize;
        sorted[r.clamp(1, sorted.len()) - 1]
    };
    Stats {
        frames: sorted.len(),
        median_ms: rank(0.5),
        p95_ms: rank(0.95),
        worst_ms: sorted[sorted.len() - 1],
    }
}

/// Frames longer than [`STALL_BUDGETS`] target budgets.
pub fn stall_count(frame_ms: &[f32], target_fps: u32) -> usize {
    let limit = STALL_BUDGETS * frame_budget_ms(target_fps);
    frame_ms.iter().filter(|&&ms| ms > limit).count()
}

/// GPU texel bytes of the decoded frame textures (ICE-6G_C frames are `Rgba16Float`, PaleoDEM
/// frames `Rg16Float`; six cube faces each), counting each image once.
pub fn decoded_texture_bytes(data: &GlobeData) -> u64 {
    let face = u64::from(data.cube_size) * u64::from(data.cube_size) * 6;
    let mut seen = vec![false; data.images.len()];
    let mut bytes = 0;
    for frame in data.timeline.frames() {
        let slot = frame.image_slot as usize;
        if seen.get(slot).copied().unwrap_or(true) {
            continue;
        }
        seen[slot] = true;
        bytes += face
            * match frame.source {
                FrameSource::Ice6g => 8,
                FrameSource::PaleoDem { .. } => 4,
            };
    }
    bytes
}

/// Bytes of the two persistent bridge targets (`Rgba8UnormSrgb`) at a resolution, and of the
/// per-camera textures Bevy allocates while both bridge cameras render: 4x MSAA colour
/// (4 bytes x 4 samples), 4x MSAA `Depth32Float` (4 x 4) and the two resolved main textures
/// (4 + 4), i.e. 40 bytes per pixel per camera.
pub fn bridge_target_bytes(size: UVec2) -> (u64, u64) {
    let pixels = u64::from(size.x) * u64::from(size.y);
    (2 * pixels * 4, 2 * pixels * 40)
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

/// Which part of the run a frame belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    WaitingForGlobe,
    Settling,
    Scrubbing,
    Done,
}

/// The benchmark's configuration and measurements.
#[derive(Resource)]
pub struct ScrubBench {
    pub target_fps: u32,
    pub exit_when_done: bool,
    phase: Phase,
    phase_started: Option<Instant>,
    /// From diagnostics start (just after the app starts) to the first interactive frame.
    cold_load_secs: Option<f64>,
    /// Web only: from navigation (`performance.now()`) to the first interactive frame.
    navigation_load_secs: Option<f64>,
    older_ms: Vec<f32>,
    younger_ms: Vec<f32>,
    bridge_ms: Vec<f32>,
    memory: MemoryTrack,
    /// The block, once logged.
    report: Option<String>,
}

impl ScrubBench {
    pub fn new(target_fps: u32, exit_when_done: bool) -> Self {
        ScrubBench {
            target_fps,
            exit_when_done,
            phase: Phase::WaitingForGlobe,
            phase_started: None,
            cold_load_secs: None,
            navigation_load_secs: None,
            older_ms: Vec::new(),
            younger_ms: Vec::new(),
            bridge_ms: Vec::new(),
            memory: MemoryTrack::default(),
            report: None,
        }
    }

    /// The report block, once the run has ended.
    pub fn report(&self) -> Option<&str> {
        self.report.as_deref()
    }

    /// Logs the block (one call, so it pastes back in one piece; the browser console shows it
    /// as a single message), prints it on native, and ends the run.
    fn finish(&mut self, report: String, pass: bool, exit: &mut MessageWriter<AppExit>) {
        self.phase = Phase::Done;
        info!("{report}");
        #[cfg(not(target_arch = "wasm32"))]
        println!("{report}");
        self.report = Some(report);
        if self.exit_when_done {
            exit.write(bench_exit(pass));
        }
    }
}

/// How a native run exits: 0 on PASS, [`FAILED_RESULT_EXIT_CODE`] on FAIL.
pub fn bench_exit(pass: bool) -> AppExit {
    if pass {
        AppExit::Success
    } else {
        AppExit::from_code(FAILED_RESULT_EXIT_CODE)
    }
}

/// Process memory samples (bytes), where the platform reports them.
#[derive(Default, Debug)]
struct MemoryTrack {
    at_interactive: Option<u64>,
    peak_scrub_deep_or_recent: Option<u64>,
    peak_scrub_bridge: Option<u64>,
}

fn max_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

pub struct ScrubBenchPlugin {
    pub target_fps: u32,
    pub exit_when_done: bool,
}

impl Plugin for ScrubBenchPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(ScrubBench::new(self.target_fps, self.exit_when_done))
            .add_systems(PreUpdate, drive_scrub_bench);
    }
}

#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn drive_scrub_bench(
    mut bench: ResMut<ScrubBench>,
    real: Res<Time<Real>>,
    mut first: MessageReader<FirstInteractiveFrame>,
    mut time: ResMut<GlobeTime>,
    mut changes: MessageWriter<TimeChanged>,
    session: Option<Res<SessionInfo>>,
    data: Option<Res<GlobeData>>,
    adapter: Option<Res<RenderAdapterInfo>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    monitors: Query<&Monitor, With<PrimaryMonitor>>,
    status: Option<Res<GlobeRenderStatus>>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = Instant::now();
    let interactive = first.read().last().is_some();
    if bench.phase != Phase::Done
        && let Some(GlobeRenderStatus::Failed(reason)) = status.as_deref()
    {
        let report = failure_block(session.as_deref(), adapter.as_deref(), reason);
        bench.finish(report, false, &mut exit);
        return;
    }
    match bench.phase {
        Phase::WaitingForGlobe => {
            if interactive {
                bench.cold_load_secs = session
                    .as_ref()
                    .map(|s| now.duration_since(s.started).as_secs_f64());
                bench.navigation_load_secs = navigation_secs();
                bench.memory.at_interactive = memory::footprint_now();
                bench.phase = Phase::Settling;
                bench.phase_started = Some(now);
                info!(
                    cold_load_secs = ?bench.cold_load_secs,
                    navigation_load_secs = ?bench.navigation_load_secs,
                    "scrub bench: globe interactive, settling"
                );
            }
        }
        Phase::Settling => {
            let started = bench.phase_started.unwrap_or(now);
            if now.duration_since(started).as_secs_f64() >= SETTLE_SECS {
                bench.phase = Phase::Scrubbing;
                bench.phase_started = Some(now);
                info!("scrub bench: scrubbing");
            }
        }
        Phase::Scrubbing => {
            let elapsed = now
                .duration_since(bench.phase_started.unwrap_or(now))
                .as_secs_f64();
            // This frame's time belongs to the age shown since the last frame.
            let frame_ms = real.delta_secs() * 1000.0;
            let shown = time.years.0;
            let in_bridge = (BRIDGE_YEARS.0..=BRIDGE_YEARS.1).contains(&shown);
            let footprint = memory::footprint_now();
            if in_bridge {
                bench.bridge_ms.push(frame_ms);
                bench.memory.peak_scrub_bridge = max_opt(bench.memory.peak_scrub_bridge, footprint);
            } else {
                bench.memory.peak_scrub_deep_or_recent =
                    max_opt(bench.memory.peak_scrub_deep_or_recent, footprint);
            }
            if elapsed <= SCRUB_SECS {
                bench.older_ms.push(frame_ms);
            } else {
                bench.younger_ms.push(frame_ms);
            }
            if elapsed > 2.0 * SCRUB_SECS {
                let window = windows.single().ok();
                let display = DisplayInfo {
                    size: window.map(|w| UVec2::new(w.physical_width(), w.physical_height())),
                    present_mode: window.map(|w| w.present_mode),
                    refresh_hz: monitors
                        .iter()
                        .next()
                        .and_then(|m| m.refresh_rate_millihertz)
                        .map(|mhz| mhz as f32 / 1000.0),
                };
                let (report, pass) = report_block(
                    &bench,
                    session.as_deref(),
                    data.as_deref(),
                    adapter.as_deref(),
                    &display,
                );
                bench.finish(report, pass, &mut exit);
                return;
            }
            let to = scrub_schedule(elapsed);
            let from = time.years;
            if to != from {
                time.years = to;
                changes.write(TimeChanged {
                    from,
                    to,
                    cause: TimeChangeCause::Bench,
                });
            }
        }
        Phase::Done => {}
    }
}

/// Web: seconds since navigation started (`performance.now()`); `None` on native.
fn navigation_secs() -> Option<f64> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now() / 1000.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
}

/// The window and display the run was measured on.
struct DisplayInfo {
    size: Option<UVec2>,
    present_mode: Option<bevy::window::PresentMode>,
    refresh_hz: Option<f32>,
}

/// Formats one direction's lines: the raw statistics, then the missed-frame count.
fn direction_lines(name: &str, frames: &[f32], target_fps: u32, pacing: Pacing) -> [String; 2] {
    let s = frame_time_stats(frames);
    let missed = missed_frames(frames, target_fps);
    let share = if s.frames > 0 {
        100.0 * missed as f64 / s.frames as f64
    } else {
        0.0
    };
    [
        format!(
            "{name}: frames {}, median {:.2} ms, p95 {:.2} ms, worst {:.2} ms, stalls (>{:.1} ms = {STALL_BUDGETS}x interval) {} -> {}",
            s.frames,
            s.median_ms,
            s.p95_ms,
            s.worst_ms,
            STALL_BUDGETS * frame_budget_ms(target_fps),
            stall_count(frames, target_fps),
            if passes(frames, target_fps, pacing) {
                "PASS"
            } else {
                "FAIL"
            }
        ),
        format!(
            "  missed frames (>{:.1} ms = {MISSED_INTERVALS}x interval): {missed} ({share:.2}%)",
            MISSED_INTERVALS * frame_budget_ms(target_fps),
        ),
    ]
}

/// The block's opening lines: session, build, spans, adapter and OS.
fn header_lines(session: Option<&SessionInfo>, adapter: Option<&RenderAdapterInfo>) -> Vec<String> {
    header_lines_titled("===== Time Fly scrub bench =====", session, adapter)
}

/// [`header_lines`] under another title line.
fn header_lines_titled(
    title: &str,
    session: Option<&SessionInfo>,
    adapter: Option<&RenderAdapterInfo>,
) -> Vec<String> {
    let mut out = vec![title.to_string()];
    if let Some(s) = session {
        out.push(format!("session: {}  log: {}", s.id, s.log));
        out.push(format!("build: {}", s.build));
    }
    out.push(format!(
        "system spans: {}",
        // Spans run natively only (ruling R29).
        if cfg!(all(feature = "system-spans", not(target_arch = "wasm32"))) {
            "on"
        } else {
            "off"
        }
    ));
    match adapter {
        Some(a) => {
            let driver = format!("{} {}", a.driver, a.driver_info);
            let driver = driver.trim();
            out.push(if driver.is_empty() {
                format!("adapter: {} ({:?})", a.name, a.backend)
            } else {
                format!("adapter: {} ({:?}; driver {driver})", a.name, a.backend)
            });
        }
        None => out.push("adapter: unknown".to_string()),
    }
    out.push(format!("os: {}", memory::os_description()));
    out
}

/// The block for a run interrupted by a globe failure.
fn failure_block(
    session: Option<&SessionInfo>,
    adapter: Option<&RenderAdapterInfo>,
    reason: &str,
) -> String {
    let mut out = header_lines(session, adapter);
    out.push(format!("result: FAIL (the globe failed: {reason})"));
    out.push("===== end scrub bench =====".to_string());
    out.join("\n")
}

/// The block and whether the run passed.
fn report_block(
    bench: &ScrubBench,
    session: Option<&SessionInfo>,
    data: Option<&GlobeData>,
    adapter: Option<&RenderAdapterInfo>,
    display: &DisplayInfo,
) -> (String, bool) {
    use bevy::window::PresentMode;
    let fps = bench.target_fps;
    let all: Vec<f32> = bench
        .older_ms
        .iter()
        .chain(&bench.younger_ms)
        .copied()
        .collect();
    let median_all = frame_time_stats(&all).median_ms;
    let web = cfg!(target_arch = "wasm32");
    let uncapped_requested = matches!(
        display.present_mode,
        Some(PresentMode::AutoNoVsync | PresentMode::Immediate | PresentMode::Mailbox)
    );
    let pacing = pacing(web, uncapped_requested, display.refresh_hz, median_all);
    let bridge = frame_time_stats(&bench.bridge_ms);
    let pass = passes(&bench.older_ms, fps, pacing) && passes(&bench.younger_ms, fps, pacing);
    let size = display.size;
    let mut out = header_lines(session, adapter);
    if let Some(size) = size {
        out.push(format!("render resolution: {}x{}", size.x, size.y));
    }
    if let Some(d) = data {
        out.push(format!("bake: {}", d.revision.summary()));
        out.push(format!(
            "data: {} frames, {} images, cube size {}, decoded textures {} (calculated)",
            d.timeline.frames().len(),
            d.images.len(),
            d.cube_size,
            mib(decoded_texture_bytes(d))
        ));
    }
    if let Some(size) = size {
        let (persistent, transient) = bridge_target_bytes(size);
        out.push(format!(
            "bridge render targets: {} persistent + {} while bridging (calculated)",
            mib(persistent),
            mib(transient)
        ));
    }
    let secs = |v: Option<f64>| v.map_or("unknown".to_string(), |s| format!("{s:.2} s"));
    if web {
        out.push(format!(
            "cold load (navigation to first interactive frame): {}",
            secs(bench.navigation_load_secs)
        ));
        out.push(format!(
            "  app start (wasm running) to first interactive frame: {}",
            secs(bench.cold_load_secs)
        ));
    } else {
        out.push(format!(
            "cold load (launch to first interactive frame): {}",
            secs(bench.cold_load_secs)
        ));
    }
    let m = &bench.memory;
    let show = |v: Option<u64>| v.map_or("n/a".to_string(), mib);
    out.push(format!(
        "process memory ({}): at first frame {}, scrub peak outside bridge {}, scrub peak in bridge {}, lifetime peak {}",
        memory::FOOTPRINT_KIND,
        show(m.at_interactive),
        show(m.peak_scrub_deep_or_recent),
        show(m.peak_scrub_bridge),
        show(memory::footprint_peak()),
    ));
    let interval = frame_budget_ms(fps);
    out.push(format!(
        "present mode: {} requested{}; display refresh {}; median {:.2} ms -> {}",
        display
            .present_mode
            .map_or("unknown".to_string(), |m| format!("{m:?}")),
        if web {
            " (browser requestAnimationFrame)"
        } else {
            ""
        },
        display
            .refresh_hz
            .map_or("unknown".to_string(), |hz| format!("{hz:.1} Hz")),
        median_all,
        match pacing {
            Pacing::DisplayPaced => "display-paced",
            Pacing::Uncapped => "uncapped",
        }
    ));
    out.push(format!("target: {fps} fps (interval {interval:.2} ms)"));
    out.push(match pacing {
        Pacing::DisplayPaced => format!(
            "pass rule (display-paced): missed frames (>{:.1} ms) <= {:.0}% and median <= {:.2} ms (interval +{:.0}%)",
            MISSED_INTERVALS * interval,
            MAX_MISSED_SHARE * 100.0,
            (1.0 + MEDIAN_TOLERANCE) * interval,
            MEDIAN_TOLERANCE * 100.0
        ),
        Pacing::Uncapped => format!("pass rule (uncapped): p95 <= {interval:.2} ms"),
    });
    out.extend(direction_lines(
        "older   (present -> 540 Ma)",
        &bench.older_ms,
        fps,
        pacing,
    ));
    out.extend(direction_lines(
        "younger (540 Ma -> present)",
        &bench.younger_ms,
        fps,
        pacing,
    ));
    out.push(format!(
        "bridge (26-30 ka, both directions): frames {}, median {:.2} ms, worst {:.2} ms, stalls {}",
        bridge.frames,
        bridge.median_ms,
        bridge.worst_ms,
        stall_count(&bench.bridge_ms, fps)
    ));
    out.push(format!(
        "result: {} (decided by the {} rule)",
        if pass { "PASS" } else { "FAIL" },
        match pacing {
            Pacing::DisplayPaced => "display-paced missed-frame",
            Pacing::Uncapped => "uncapped p95",
        }
    ));
    out.push("===== end scrub bench =====".to_string());
    (out.join("\n"), pass)
}

/// Process memory and OS description, per platform.
mod memory {
    #[cfg(target_os = "macos")]
    pub const FOOTPRINT_KIND: &str = "phys_footprint, includes GPU allocations on Apple silicon";
    #[cfg(all(unix, not(target_os = "macos")))]
    pub const FOOTPRINT_KIND: &str = "resident set; GPU memory not included";
    #[cfg(not(unix))]
    pub const FOOTPRINT_KIND: &str = "not available here";

    #[cfg(target_os = "macos")]
    fn rusage() -> Option<libc::rusage_info_v4> {
        // SAFETY: `proc_pid_rusage` fills the struct for the flavour asked for; it is plain data.
        unsafe {
            let mut info: libc::rusage_info_v4 = std::mem::zeroed();
            let ok = libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&raw mut info).cast(),
            );
            (ok == 0).then_some(info)
        }
    }

    /// The process's current memory footprint in bytes.
    pub fn footprint_now() -> Option<u64> {
        #[cfg(target_os = "macos")]
        return rusage().map(|r| r.ri_phys_footprint);
        #[cfg(all(unix, not(target_os = "macos")))]
        return std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| {
                let pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
                // SAFETY: sysconf has no preconditions.
                let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
                Some(pages * page.max(0) as u64)
            });
        #[cfg(not(unix))]
        None
    }

    /// The process's peak memory footprint in bytes.
    pub fn footprint_peak() -> Option<u64> {
        #[cfg(target_os = "macos")]
        return rusage().map(|r| r.ri_lifetime_max_phys_footprint);
        #[cfg(all(unix, not(target_os = "macos")))]
        // SAFETY: getrusage fills a plain-data struct.
        return unsafe {
            let mut usage: libc::rusage = std::mem::zeroed();
            (libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0)
                .then(|| usage.ru_maxrss.max(0) as u64 * 1024)
        };
        #[cfg(not(unix))]
        None
    }

    /// The OS name and version.
    pub fn os_description() -> String {
        #[cfg(target_os = "macos")]
        {
            let version = sysctl_string(c"kern.osproductversion").unwrap_or_default();
            let build = sysctl_string(c"kern.osversion").unwrap_or_default();
            format!("macOS {version} ({build}), {}", std::env::consts::ARCH)
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let pretty = std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|s| {
                    s.lines().find_map(|l| {
                        l.strip_prefix("PRETTY_NAME=")
                            .map(|v| v.trim_matches('"').to_string())
                    })
                })
                .unwrap_or_else(|| std::env::consts::OS.to_string());
            let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string())
                .unwrap_or_default();
            format!("{pretty}, kernel {kernel}, {}", std::env::consts::ARCH)
        }
        #[cfg(target_arch = "wasm32")]
        {
            web_sys::window()
                .and_then(|w| w.navigator().user_agent().ok())
                .map_or_else(|| "web".to_string(), |ua| format!("web: {ua}"))
        }
        #[cfg(not(any(unix, target_arch = "wasm32")))]
        {
            std::env::consts::OS.to_string()
        }
    }

    /// Web: the JS heap in use (`performance.memory.usedJSHeapSize`, Chrome only) and the size
    /// of the wasm linear memory (which grows but never shrinks).
    #[cfg(target_arch = "wasm32")]
    pub fn web_memory() -> (Option<u64>, Option<u64>) {
        use wasm_bindgen::JsValue;
        let get = |object: &JsValue, key: &str| {
            js_sys::Reflect::get(object, &JsValue::from_str(key))
                .ok()
                .filter(|v| !v.is_undefined() && !v.is_null())
        };
        let bytes = |v: JsValue| v.as_f64().filter(|b| *b >= 0.0).map(|b| b as u64);
        let js_heap = web_sys::window()
            .and_then(|w| w.performance())
            .and_then(|p| get(&JsValue::from(p), "memory"))
            .and_then(|m| get(&m, "usedJSHeapSize"))
            .and_then(bytes);
        let linear = get(&wasm_bindgen::memory(), "buffer")
            .and_then(|b| get(&b, "byteLength"))
            .and_then(bytes);
        (js_heap, linear)
    }

    #[cfg(target_os = "macos")]
    fn sysctl_string(name: &std::ffi::CStr) -> Option<String> {
        let mut buf = [0u8; 128];
        let mut len = buf.len();
        // SAFETY: the buffer and its length are valid; sysctlbyname writes at most `len` bytes.
        let ok = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (ok == 0).then(|| {
            String::from_utf8_lossy(&buf[..len.min(buf.len())])
                .trim_end_matches('\0')
                .to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_bench_reports_percentiles() {
        // 1..=100 ms: nearest-rank median 50, p95 95, worst 100.
        let series: Vec<f32> = (1..=100).map(|v| v as f32).collect();
        let s = frame_time_stats(&series);
        assert_eq!(
            s,
            Stats {
                frames: 100,
                median_ms: 50.0,
                p95_ms: 95.0,
                worst_ms: 100.0
            }
        );
        // Order does not matter; non-finite values are dropped.
        let mut shuffled = series.clone();
        shuffled.reverse();
        shuffled.push(f32::NAN);
        assert_eq!(frame_time_stats(&shuffled), s);
        assert_eq!(frame_time_stats(&[]), Stats::default());

        // A smooth 60 fps run with a few spikes.
        let mut run = vec![16.0_f32; 95];
        run.extend([40.0; 5]);
        let s = frame_time_stats(&run);
        assert_eq!((s.median_ms, s.p95_ms, s.worst_ms), (16.0, 16.0, 40.0));
        assert!(s.passes(60));
        assert_eq!(stall_count(&run, 60), 5);
        // One more spike pushes the 95th percentile over the 16.67 ms budget.
        run[0] = 40.0;
        let s = frame_time_stats(&run);
        assert_eq!(s.p95_ms, 40.0);
        assert!(!s.passes(60));
        assert!(s.passes(20), "40 ms is within a 20 fps budget (50 ms)");
        assert!(!Stats::default().passes(60), "no frames never passes");
    }

    /// A 60 Hz display-paced run: deltas jitter +-2 ms around 16.67 ms, with `missed` frames
    /// of two intervals spread through it.
    fn paced_series(frames: usize, missed: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| {
                if missed > 0 && i % (frames / missed) == 0 && i / (frames / missed) < missed {
                    33.4
                } else {
                    16.67 + [-2.0, 1.9, -0.5, 0.4, 2.0, -1.8][i % 6]
                }
            })
            .collect()
    }

    #[test]
    fn display_paced_runs_pass_on_missed_frames_not_p95() {
        let clean = paced_series(1200, 0);
        let stats = frame_time_stats(&clean);
        assert!(stats.p95_ms > frame_budget_ms(60), "jitter puts p95 over");
        assert!(!stats.passes(60), "the p95 rule would fail it");
        assert_eq!(missed_frames(&clean, 60), 0);
        assert!(passes(&clean, 60, Pacing::DisplayPaced));

        let lossy = paced_series(1200, 24); // 2% missed
        assert_eq!(missed_frames(&lossy, 60), 24);
        assert!(!passes(&lossy, 60, Pacing::DisplayPaced));
        let ok = paced_series(1200, 12); // 1% missed: still passes
        assert!(passes(&ok, 60, Pacing::DisplayPaced));

        // A median more than 5% over the interval fails even without missed frames.
        let slow = vec![17.6_f32; 100];
        assert!(!passes(&slow, 60, Pacing::DisplayPaced));
        // A faster display (120 Hz) at a 60 fps target passes.
        assert!(passes(&vec![8.33_f32; 100], 60, Pacing::DisplayPaced));
        assert!(!passes(&[], 60, Pacing::DisplayPaced));

        // Uncapped runs keep the p95 rule.
        let mut fast = vec![9.0_f32; 95];
        fast.extend([16.0; 5]);
        assert!(passes(&fast, 60, Pacing::Uncapped));
        fast.extend([20.0; 10]);
        assert!(!passes(&fast, 60, Pacing::Uncapped));
    }

    #[test]
    fn pacing_is_detected_from_the_present_mode_and_the_median() {
        assert_eq!(pacing(true, true, None, 5.0), Pacing::DisplayPaced, "web");
        assert_eq!(
            pacing(false, false, None, 5.0),
            Pacing::DisplayPaced,
            "vsync"
        );
        // Asked for uncapped, but the median sits on the display's interval.
        assert_eq!(pacing(false, true, Some(60.0), 16.6), Pacing::DisplayPaced);
        assert_eq!(pacing(false, true, Some(60.0), 9.0), Pacing::Uncapped);
        assert_eq!(pacing(false, true, Some(144.0), 6.94), Pacing::DisplayPaced);
        // Unknown refresh: common rates.
        assert_eq!(pacing(false, true, None, 8.33), Pacing::DisplayPaced);
        assert_eq!(pacing(false, true, None, 11.9), Pacing::Uncapped);
    }

    #[test]
    fn scrub_bench_cli_flags() {
        let parsed = crate::args::parse_launch_args(
            [
                "--scrub-bench",
                "--target-fps",
                "60",
                "--window",
                "2560x1440",
            ]
            .map(String::from),
        );
        assert!(parsed.scrub_bench);
        assert_eq!(parsed.target_fps, Some(60));
        assert_eq!(parsed.window, Some((2560, 1440)));
        let bad = crate::args::parse_launch_args(["--window", "2560by1440"].map(String::from));
        assert_eq!(bad.window, None);
        assert_eq!(bad.unknown, vec!["--window=2560by1440".to_string()]);
    }

    #[test]
    fn schedule_scrubs_the_full_range_both_ways_through_the_bridge() {
        assert_eq!(scrub_schedule(0.0).0, 0.0);
        assert!((scrub_schedule(SCRUB_SECS).0 - 540.0e6).abs() < 1.0);
        assert_eq!(scrub_schedule(2.0 * SCRUB_SECS).0, 0.0);
        assert_eq!(scrub_schedule(-1.0).0, 0.0);
        assert_eq!(scrub_schedule(3.0 * SCRUB_SECS).0, 0.0);
        // Symmetric, monotonic each way.
        for t in [1.0, 5.0, 7.7, 13.0] {
            let (a, b) = (scrub_schedule(t).0, scrub_schedule(2.0 * SCRUB_SECS - t).0);
            assert!((a - b).abs() <= 1e-6 * a.max(1.0), "{t}: {a} vs {b}");
        }
        let ages: Vec<f64> = (0..=400)
            .map(|i| scrub_schedule(i as f64 * 0.05).0)
            .collect();
        assert!(ages.windows(2).all(|w| w[1] >= w[0]));
        // The bridge (26-30 ka) is crossed, and lasts several frames at 60 fps each way.
        let in_bridge = (0..1200)
            .map(|i| scrub_schedule(i as f64 / 60.0).0)
            .filter(|y| (BRIDGE_YEARS.0..=BRIDGE_YEARS.1).contains(y))
            .count();
        assert!(in_bridge >= 5, "{in_bridge}");
    }

    #[test]
    fn a_failed_run_exits_with_the_failed_result_code() {
        assert_eq!(bench_exit(true), AppExit::Success);
        assert_eq!(
            bench_exit(false),
            AppExit::from_code(crate::diagnostics::session::FAILED_RESULT_EXIT_CODE)
        );
    }

    fn bench_app(status: GlobeRenderStatus, exit_when_done: bool) -> App {
        let mut app = App::new();
        app.init_resource::<Time<Real>>()
            .init_resource::<GlobeTime>()
            .add_message::<FirstInteractiveFrame>()
            .add_message::<TimeChanged>()
            .insert_resource(status)
            .add_plugins(ScrubBenchPlugin {
                target_fps: 60,
                exit_when_done,
            });
        app
    }

    fn exits(app: &App) -> Vec<AppExit> {
        app.world()
            .resource::<Messages<AppExit>>()
            .iter_current_update_messages()
            .cloned()
            .collect()
    }

    #[test]
    fn a_failed_globe_ends_the_bench_with_a_fail_block() {
        let mut app = bench_app(
            GlobeRenderStatus::Failed("data/globe/manifest.json: file not found".into()),
            true,
        );
        app.update();
        assert_eq!(exits(&app), vec![bench_exit(false)]);
        let report = app.world().resource::<ScrubBench>().report().unwrap();
        assert!(
            report.starts_with("===== Time Fly scrub bench ====="),
            "{report}"
        );
        assert!(
            report.contains(
                "result: FAIL (the globe failed: data/globe/manifest.json: file not found)"
            ),
            "{report}"
        );
        assert!(report.ends_with("===== end scrub bench ====="), "{report}");
        // Reported once.
        app.update();
        assert!(exits(&app).is_empty());
    }

    #[test]
    fn a_pipeline_failure_during_scrubbing_fails_the_bench() {
        let mut app = bench_app(GlobeRenderStatus::Interactive { first_frame: 1 }, true);
        app.world_mut().resource_mut::<ScrubBench>().phase = Phase::Scrubbing;
        app.world_mut()
            .insert_resource(GlobeRenderStatus::Failed("composite failed".into()));
        app.update();
        assert_eq!(exits(&app), vec![bench_exit(false)]);
    }

    #[test]
    fn the_web_bench_reports_a_failed_globe_without_exiting() {
        let mut app = bench_app(GlobeRenderStatus::Failed("no adapter".into()), false);
        app.update();
        assert!(exits(&app).is_empty());
        assert!(app.world().resource::<ScrubBench>().report().is_some());
    }

    #[test]
    fn a_loading_globe_keeps_the_bench_waiting() {
        let mut app = bench_app(GlobeRenderStatus::LoadingData, true);
        app.update();
        assert!(exits(&app).is_empty());
        assert!(app.world().resource::<ScrubBench>().report().is_none());
    }

    #[test]
    fn bridge_target_memory() {
        let (persistent, transient) = bridge_target_bytes(UVec2::new(2560, 1440));
        assert_eq!(persistent, 2 * 2560 * 1440 * 4);
        assert_eq!(transient, 10 * persistent);
    }
}
