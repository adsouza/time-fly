//! Diagnostics: tracing aspects, session logs and the crash notice (spec §20).
//!
//! [`start`] runs first in `main`, before the Bevy `App` exists: it finds out how the previous
//! session ended, opens this session's record, log and lock, and installs the panic hook. It
//! writes only to stderr. [`DiagnosticsPlugin`] then hands the log layers to Bevy's `LogPlugin`
//! (the one global subscriber) and logs what `start` found.

pub mod aspects;
pub mod notice;
pub mod recorder;
pub mod report;
pub mod session;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bevy::diagnostic::FrameCount;
use bevy::log::tracing_subscriber::{EnvFilter, Layer as _};
use bevy::log::{BoxedFmtLayer, BoxedLayer, DEFAULT_FILTER, Level, LogPlugin};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::window::PrimaryWindow;

use crate::args::LaunchArgs;
use notice::{NoticeRoute, PendingNotice};
use recorder::{Recorder, RecorderLayer};
use report::Home;
use session::{
    BuildInfo, PlatformInfo, PreviousOutcome, PreviousSession, SessionRecord, SessionStatus,
    UtcTime,
};

/// Session directories kept on native (spec §20).
pub const KEEP_SESSIONS: usize = 10;

/// Hidden `--diagnostics-probe` modes, for tests and the manual checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeMode {
    /// Panics before opening a window.
    Panic,
    /// Opens the window, renders a frame, then panics.
    PanicAfterFrame,
    /// Calls `std::process::abort()`.
    Abort,
    /// Logs an error and exits cleanly.
    Error,
    /// Exits cleanly.
    Clean,
}

impl ProbeMode {
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "panic" => Self::Panic,
            "panic-after-frame" => Self::PanicAfterFrame,
            "abort" => Self::Abort,
            "error" => Self::Error,
            "clean" => Self::Clean,
            _ => return None,
        })
    }

    /// Probes that never open a window run a headless `App`.
    pub fn is_headless(self) -> bool {
        self != Self::PanicAfterFrame
    }
}

/// Last known game state, for crash reports. Systems write this resource; it is copied to the
/// panic hook's copy only when it changes. Later tasks fill in their fields.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct DiagnosticsContext {
    pub game_age_years: Option<f64>,
    pub camera_distance_km: Option<f64>,
    pub camera_direction: Option<[f32; 3]>,
    pub load_state: Option<String>,
    pub frame_selection: Option<String>,
    pub adapter: Option<String>,
    pub backend: Option<String>,
    pub window_size: Option<(u32, u32)>,
}

impl DiagnosticsContext {
    /// One line per known value, for the crash report.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(age) = self.game_age_years {
            lines.push(format!("game age: {age} years"));
        }
        if let Some(d) = self.camera_distance_km {
            lines.push(format!("camera distance: {d} km"));
        }
        if let Some([x, y, z]) = self.camera_direction {
            lines.push(format!("camera direction: ({x}, {y}, {z})"));
        }
        if let Some(s) = &self.load_state {
            lines.push(format!("load state: {s}"));
        }
        if let Some(s) = &self.frame_selection {
            lines.push(format!("frame selection: {s}"));
        }
        if let Some(s) = &self.adapter {
            lines.push(format!("adapter: {s}"));
        }
        if let Some(s) = &self.backend {
            lines.push(format!("backend: {s}"));
        }
        if let Some((w, h)) = self.window_size {
            lines.push(format!("window: {w}x{h}"));
        }
        if lines.is_empty() {
            lines.push("(nothing recorded yet)".to_string());
        }
        lines
    }
}

/// What [`start`] found, logged by [`DiagnosticsPlugin`] once the subscriber exists.
#[derive(Debug, Default)]
pub(crate) struct Findings {
    pub log_root: Option<PathBuf>,
    pub session_dir: Option<PathBuf>,
    pub previous: Option<PreviousSession>,
    pub pruned: Vec<String>,
    pub problems: Vec<String>,
}

/// State shared by the session, the log layers, the panic hook and the systems.
pub(crate) struct Shared {
    pub args: LaunchArgs,
    pub record: Mutex<SessionRecord>,
    pub findings: Findings,
    pub recorder: Mutex<Recorder>,
    pub context: Mutex<DiagnosticsContext>,
    pub home: Home,
    pub started: Instant,
    /// The in-game notice, when the previous session crashed and nothing before the `App`
    /// showed a notice.
    pub pending_notice: Mutex<Option<PendingNotice>>,
    pub notice_route: Mutex<NoticeRoute>,
    #[cfg(not(target_arch = "wasm32"))]
    pub log_writer: Option<tracing_appender::non_blocking::NonBlocking>,
}

/// Locks a mutex, ignoring poisoning: diagnostics must keep working after a panic.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub fn session_dir(&self) -> Option<&Path> {
        self.findings.session_dir.as_deref()
    }

    /// Changes the session record and saves it.
    pub fn update_record(&self, change: impl FnOnce(&mut SessionRecord)) {
        let mut record = lock(&self.record);
        change(&mut record);
        self.save_record(&record);
    }

    fn save_record(&self, record: &SessionRecord) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(dir) = self.session_dir()
            && let Err(e) = session::store::write_record(dir, record)
        {
            eprintln!("Time Fly: could not save the session record: {e}");
        }
        #[cfg(target_arch = "wasm32")]
        session::web::save_record(record);
    }

    pub fn previous_outcome(&self) -> PreviousOutcome {
        self.findings
            .previous
            .as_ref()
            .map_or(PreviousOutcome::Nothing, |p| p.outcome)
    }
}

/// This run of the game. Keep it alive until the `App` returns, then call [`Session::finish`].
pub struct Session {
    shared: Arc<Shared>,
    #[cfg(not(target_arch = "wasm32"))]
    guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

impl Session {
    pub fn probe(&self) -> Option<ProbeMode> {
        self.shared.args.diagnostics_probe
    }

    pub fn previous_outcome(&self) -> PreviousOutcome {
        self.shared.previous_outcome()
    }

    /// This session's directory, when native logging is available.
    pub fn session_dir(&self) -> Option<&Path> {
        self.shared.session_dir()
    }

    /// Marks the session clean and flushes the session log. On the web the page keeps running
    /// after `main` returns, so the session stays open and `pagehide` marks it clean.
    pub fn finish(self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut this = self;
            this.shared
                .update_record(|r| r.status = SessionStatus::Clean);
            this.flush_logs();
        }
        #[cfg(target_arch = "wasm32")]
        std::mem::forget(self);
    }

    /// Drops the log worker guard, which writes out everything logged so far.
    #[cfg(not(target_arch = "wasm32"))]
    fn flush_logs(&mut self) {
        drop(self.guard.take());
    }
}

fn now_unix_millis() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as u64
    }
}

/// Eight hex digits, unique enough to tell sessions apart.
fn short_id(now_ms: u64) -> String {
    #[cfg(not(target_arch = "wasm32"))]
    let bits = {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(now_ms);
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    #[cfg(target_arch = "wasm32")]
    let bits = (js_sys::Math::random() * f64::from(u32::MAX)) as u64 ^ now_ms;
    format!("{:08x}", bits as u32)
}

fn home() -> Home {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dirs) = directories::BaseDirs::new() {
        let raw = dirs.home_dir();
        let canonical = std::fs::canonicalize(raw).ok();
        return Home::new(
            &raw.to_string_lossy(),
            canonical.as_deref().map(|c| c.to_string_lossy()).as_deref(),
        );
    }
    Home::default()
}

/// Opens this session's diagnostics. Runs before the Bevy `App` exists; see the module docs.
pub fn start(args: LaunchArgs) -> Session {
    let now = now_unix_millis();
    let started = UtcTime::from_unix_millis(now);
    let record = SessionRecord {
        id: short_id(now),
        started_utc: started.rfc3339(),
        build: BuildInfo::current(),
        platform: PlatformInfo::current(),
        status: SessionStatus::Running,
        reached_first_frame: false,
        acknowledged: false,
    };
    let home = home();

    #[cfg(not(target_arch = "wasm32"))]
    let (findings, log_writer, guard) = session::store::open(&record, &started, &home);
    #[cfg(target_arch = "wasm32")]
    let findings = session::web::open(&record);

    let shared = Arc::new(Shared {
        args,
        record: Mutex::new(record),
        findings,
        recorder: Mutex::new(Recorder::default()),
        context: Mutex::new(DiagnosticsContext::default()),
        home,
        started: Instant::now(),
        pending_notice: Mutex::new(None),
        notice_route: Mutex::new(NoticeRoute::None),
        #[cfg(not(target_arch = "wasm32"))]
        log_writer,
    });
    for problem in &shared.findings.problems {
        eprintln!("Time Fly diagnostics: {problem}");
    }
    report::install_panic_hook(shared.clone());
    #[cfg(target_arch = "wasm32")]
    session::web::install_pagehide(shared.clone());

    let (route, pending) = notice::before_app(&shared);
    *lock(&shared.notice_route) = route;
    *lock(&shared.pending_notice) = pending;

    Session {
        shared,
        #[cfg(not(target_arch = "wasm32"))]
        guard,
    }
}

/// The handle systems and log layers use.
#[derive(Resource, Clone)]
pub(crate) struct DiagnosticsHandle(pub Arc<Shared>);

/// Global filter: everything any layer wants. `timefly*` at `debug` feeds the flight recorder;
/// the console and session-file layers narrow it down.
fn global_filter() -> String {
    let mut filter = format!("{DEFAULT_FILTER}timefly=debug");
    if let Some(extra) = env_directives("TIMEFLY_LOG") {
        filter.push(',');
        filter.push_str(&extra);
    }
    filter
}

fn env_directives(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn filter(defaults: &str, extra: Option<String>) -> EnvFilter {
    let directives = match extra {
        Some(extra) => format!("{defaults},{extra}"),
        None => defaults.to_string(),
    };
    EnvFilter::builder().parse_lossy(directives)
}

/// `LogPlugin` configured with the diagnostics layers.
pub fn log_plugin() -> LogPlugin {
    LogPlugin {
        filter: global_filter(),
        level: Level::INFO,
        custom_layer: diagnostics_layers,
        fmt_layer: console_layer,
        ..default()
    }
}

/// The session file (native) and the flight recorder.
fn diagnostics_layers(app: &mut App) -> Option<BoxedLayer> {
    let shared = app.world().get_resource::<DiagnosticsHandle>()?.0.clone();
    let mut layers: Vec<BoxedLayer> = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(writer) = shared.log_writer.clone() {
        // `TIMEFLY_LOG` replaces the session file's levels.
        let levels =
            env_directives("TIMEFLY_LOG").unwrap_or_else(|| "warn,timefly=info".to_string());
        layers.push(
            bevy::log::tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter(&levels, None))
                .boxed(),
        );
    }
    layers.push(
        RecorderLayer::new(shared)
            .with_filter(filter("info,timefly=debug", None))
            .boxed(),
    );
    Some(Box::new(layers))
}

/// The console: `info`, or what `RUST_LOG` asks for.
fn console_layer(_app: &mut App) -> Option<BoxedFmtLayer> {
    Some(Box::new(
        bevy::log::tracing_subscriber::fmt::Layer::default()
            .with_writer(std::io::stderr)
            .with_filter(filter("info", env_directives("RUST_LOG"))),
    ))
}

/// Adds the diagnostics systems and resources. Add it before `DefaultPlugins`, so the log
/// layers exist when `LogPlugin` builds.
pub struct DiagnosticsPlugin {
    shared: Arc<Shared>,
    windowed: bool,
}

impl DiagnosticsPlugin {
    pub fn new(session: &Session) -> Self {
        Self {
            shared: session.shared.clone(),
            windowed: true,
        }
    }

    /// For apps without a window: no first-frame tracking, no in-game notice.
    pub fn headless(session: &Session) -> Self {
        Self {
            shared: session.shared.clone(),
            windowed: false,
        }
    }
}

impl Plugin for DiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DiagnosticsHandle(self.shared.clone()))
            .init_resource::<DiagnosticsContext>()
            .add_systems(Startup, log_findings)
            .add_systems(
                Last,
                sync_context.run_if(resource_changed::<DiagnosticsContext>),
            );
        if !self.windowed {
            return;
        }
        app.add_systems(Update, (update_render_context, mark_first_frame));
        if let Some(pending) = lock(&self.shared.pending_notice).take() {
            notice::add_in_game_notice(app, pending);
        }
        if self.shared.args.diagnostics_probe == Some(ProbeMode::PanicAfterFrame) {
            app.add_systems(Startup, || log_probe_events(ProbeMode::PanicAfterFrame))
                .add_systems(Update, panic_after_first_frame);
        }
        #[cfg(target_arch = "wasm32")]
        app.add_systems(Update, session::web::save_log_periodically);
    }
}

fn log_findings(handle: Res<DiagnosticsHandle>) {
    let shared = &handle.0;
    let findings = &shared.findings;
    {
        let record = lock(&shared.record);
        info!(
            session = %record.id,
            build = %record.build.summary(),
            platform = %record.platform.summary(),
            "session started"
        );
    }
    match (&findings.session_dir, &findings.log_root) {
        (Some(dir), _) => info!(dir = %dir.display(), "session log directory"),
        (None, Some(root)) => {
            warn!(root = %root.display(), "no session directory; logging to the console only")
        }
        (None, None) => {}
    }
    for name in &findings.pruned {
        info!(session = %name, "pruned an old session");
    }
    for problem in &findings.problems {
        warn!("{problem}");
    }
    for arg in &shared.args.unknown {
        warn!(arg = %arg, "ignoring an unknown launch argument");
    }
    if let Some(previous) = &findings.previous {
        match previous.outcome {
            PreviousOutcome::Crashed {
                during_startup,
                has_crash_report,
            } => warn!(
                log = %notice::log_display(previous),
                during_startup,
                has_crash_report,
                notice = ?*lock(&shared.notice_route),
                "the previous session didn't close normally"
            ),
            PreviousOutcome::OtherInstanceRunning => {
                info!(dir = %previous.dir.display(), "another Time Fly instance is running")
            }
            PreviousOutcome::Clean | PreviousOutcome::Nothing => {
                debug!(outcome = ?previous.outcome, "previous session")
            }
        }
    }
}

/// Copies the context resource to the panic hook's copy, and records the adapter in the
/// session record once the renderer reports it.
fn sync_context(context: Res<DiagnosticsContext>, handle: Res<DiagnosticsHandle>) {
    *lock(&handle.0.context) = context.clone();
    let adapter_changed = {
        let record = lock(&handle.0.record);
        record.platform.adapter != context.adapter || record.platform.backend != context.backend
    };
    if adapter_changed {
        handle.0.update_record(|r| {
            r.platform.adapter = context.adapter.clone();
            r.platform.backend = context.backend.clone();
        });
        info!(adapter = ?context.adapter, backend = ?context.backend, "graphics adapter");
    }
}

/// Window size and adapter, written only when they change.
fn update_render_context(
    windows: Query<&Window, With<PrimaryWindow>>,
    adapter: Option<Res<RenderAdapterInfo>>,
    mut context: ResMut<DiagnosticsContext>,
) {
    let size = windows
        .single()
        .ok()
        .map(|w| (w.physical_width(), w.physical_height()));
    if context.window_size != size {
        context.window_size = size;
    }
    if let Some(adapter) = adapter.filter(|a| a.is_changed()) {
        let name = Some(adapter.name.clone());
        let backend = Some(format!("{:?}", adapter.backend));
        if context.adapter != name || context.backend != backend {
            context.adapter = name;
            context.backend = backend;
        }
    }
}

/// Records the first frame after the window opens. Task 9 moves this to the first interactive
/// globe frame.
fn mark_first_frame(
    frames: Option<Res<FrameCount>>,
    windows: Query<(), With<PrimaryWindow>>,
    handle: Res<DiagnosticsHandle>,
    mut done: Local<bool>,
) {
    // Frame 2: with pipelined rendering, frame 0 has been rendered by then.
    if *done || frames.is_none_or(|f| f.0 < 2) || windows.is_empty() {
        return;
    }
    *done = true;
    handle.0.update_record(|r| r.reached_first_frame = true);
    info!("first frame reached");
}

fn panic_after_first_frame(handle: Res<DiagnosticsHandle>, frames: Option<Res<FrameCount>>) {
    let reached = lock(&handle.0.record).reached_first_frame;
    if reached && frames.is_some_and(|f| f.0 >= 5) {
        panic!("diagnostics probe: panic after the first frame");
    }
}

/// A few events at each level, including a path under the home directory (redaction check).
fn log_probe_events(mode: ProbeMode) {
    info!(mode = ?mode, "diagnostics probe started");
    for step in 1..=3 {
        debug!(step, "probe step {step}");
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dirs) = directories::BaseDirs::new() {
        let path = dirs.home_dir().join("Time Fly probe").join("example.txt");
        info!(path = %path.display(), "probe path under the home directory");
    }
}

fn probe_system(mode: ProbeMode) -> impl FnMut(Local<bool>) {
    move |mut done: Local<bool>| {
        if std::mem::replace(&mut *done, true) {
            return;
        }
        log_probe_events(mode);
        match mode {
            ProbeMode::Panic => panic!("diagnostics probe: panic before the window opens"),
            ProbeMode::Error => error!("diagnostics probe: error"),
            ProbeMode::Abort | ProbeMode::Clean | ProbeMode::PanicAfterFrame => {}
        }
    }
}

/// Runs a window-less probe in a headless `App` (`MinimalPlugins` plus the real log layers),
/// then ends the process the way the probe asks.
pub fn run_headless_probe(session: Session, mode: ProbeMode) -> AppExit {
    let mut app = App::new();
    app.add_plugins(DiagnosticsPlugin::headless(&session))
        .add_plugins((MinimalPlugins, log_plugin()))
        .add_systems(Update, probe_system(mode));
    app.finish();
    app.cleanup();
    for _ in 0..3 {
        app.update();
    }
    drop(app);
    if mode == ProbeMode::Abort {
        // Write out what was logged, then die without unwinding or marking the session clean.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut session = session;
            session.flush_logs();
        }
        #[cfg(target_arch = "wasm32")]
        session::web::save_log(&session.shared);
        std::process::abort();
    }
    session.finish();
    AppExit::Success
}
