//! Diagnostics: tracing aspects, session logs and the crash notice (spec §20).
//!
//! [`start`] runs first in `main`, before the Bevy `App` exists: it finds out how the previous
//! session ended, opens this session's record, log and lock, and installs the panic hook. It
//! writes only to stderr. [`DiagnosticsPlugin`] then hands the log layers to Bevy's `LogPlugin`
//! (the one global subscriber) and logs what `start` found.

pub mod aspects;
pub mod notice;
pub mod probe;
pub mod recorder;
pub mod report;
pub mod session;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Duration;

use bevy::log::tracing_subscriber::{EnvFilter, Layer as _};
use bevy::log::{BoxedFmtLayer, BoxedLayer, DEFAULT_FILTER, Level, LogPlugin};
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapterInfo;
use bevy::window::PrimaryWindow;

use crate::args::LaunchArgs;
use aspects::DiagnosticsAppExt;
use notice::{NoticeRoute, PendingNotice};
pub use probe::{ProbeMode, run_headless_probe};
use recorder::{Recorder, RecorderLayer};
use report::Home;
use session::{
    BuildInfo, PlatformInfo, PreviousOutcome, PreviousSession, SessionRecord, SessionStatus,
    UtcTime,
};

/// Session directories kept on native (spec §20).
pub const KEEP_SESSIONS: usize = 10;

/// How long the panic hook waits for a lock another thread holds.
pub(crate) const HOOK_LOCK_WAIT: Duration = Duration::from_millis(200);

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
    /// Problems found before the log subscriber existed; `DiagnosticsPlugin` logs them.
    pub problems: Mutex<Vec<String>>,
    /// Set once `DiagnosticsPlugin` has logged the findings: problems go straight to `warn!`.
    pub logging_ready: AtomicBool,
    #[cfg(not(target_arch = "wasm32"))]
    pub log_writer: Option<tracing_appender::non_blocking::NonBlocking>,
}

/// Locks a mutex, ignoring poisoning: diagnostics must keep working after a panic.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Tries to lock a mutex for up to `wait`, ignoring poisoning. For the panic hook: another
/// thread holding the lock briefly is waited for, but a lock held by the panicking thread
/// itself gives up instead of deadlocking.
pub(crate) fn try_lock_for<T>(mutex: &Mutex<T>, wait: Duration) -> Option<MutexGuard<'_, T>> {
    let deadline = Instant::now() + wait;
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) if Instant::now() >= deadline => return None,
            Err(TryLockError::WouldBlock) => std::thread::yield_now(),
        }
    }
}

impl Shared {
    pub fn session_dir(&self) -> Option<&Path> {
        self.findings.session_dir.as_deref()
    }

    /// Changes the session record and saves it. Problems are reported after the lock is
    /// released, so logging never happens while holding it.
    pub fn update_record(&self, change: impl FnOnce(&mut SessionRecord)) {
        let result = {
            let mut record = lock(&self.record);
            change(&mut record);
            self.save_record(&record)
        };
        if let Err(e) = result {
            self.problem(format!("could not save the session record: {e}"));
        }
    }

    /// [`Shared::update_record`] for the panic hook: gives up rather than deadlock if the record
    /// lock is held (possibly by the panicking thread). Returns whether it saved.
    pub fn try_update_record(&self, change: impl FnOnce(&mut SessionRecord)) -> Result<(), String> {
        let mut record = try_lock_for(&self.record, HOOK_LOCK_WAIT)
            .ok_or_else(|| "the session record was locked".to_string())?;
        change(&mut record);
        self.save_record(&record)
    }

    fn save_record(&self, record: &SessionRecord) -> Result<(), String> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(dir) = self.session_dir() {
            session::store::write_record(dir, record).map_err(|e| e.to_string())?;
        }
        #[cfg(target_arch = "wasm32")]
        session::web::save_record(record)?;
        Ok(())
    }

    /// Prints to stderr with the home directory replaced by `~`.
    pub fn eprint(&self, message: &str) {
        eprintln!("{}", report::redact(message, &self.home));
    }

    /// Reports a diagnostics problem: a `warn!` once logging is up; before that, stderr plus a
    /// note that `DiagnosticsPlugin` logs once the subscriber exists.
    pub fn problem(&self, message: String) {
        let mut problems = lock(&self.problems);
        if self.logging_ready.load(Ordering::Acquire) {
            drop(problems);
            warn!("{message}");
        } else {
            self.eprint(&format!("Time Fly diagnostics: {message}"));
            problems.push(message);
        }
    }

    /// Switches [`Shared::problem`] to `warn!` and returns the problems collected so far.
    fn take_problems_for_logging(&self) -> Vec<String> {
        let mut problems = lock(&self.problems);
        self.logging_ready.store(true, Ordering::Release);
        std::mem::take(&mut *problems)
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

    /// The launch arguments.
    pub fn args(&self) -> &LaunchArgs {
        &self.shared.args
    }

    pub fn previous_outcome(&self) -> PreviousOutcome {
        self.shared.previous_outcome()
    }

    /// This session's directory, when native logging is available.
    pub fn session_dir(&self) -> Option<&Path> {
        self.shared.session_dir()
    }

    /// Records how the `App` ended ([`session::finish_status`]: clean only for a successful
    /// exit of a session that hadn't crashed) and flushes the session log. On the web the page
    /// keeps running after `main` returns, so the session stays open and `pagehide` marks it
    /// clean.
    pub fn finish(self, exit: &AppExit) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut this = self;
            // `record_app_exits` has already recorded and logged an error exit; this is the
            // success path and a safety net (`finish_status` never downgrades a crash).
            this.shared
                .update_record(|r| r.status = session::finish_status(r.status, exit));
            this.flush_logs();
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = exit;
            std::mem::forget(self);
        }
    }

    /// Ends a session the game stopped on purpose ([`session::stopped_status`]): records it clean
    /// whatever `exit` is, so the next launch shows no crash notice, and writes out the log
    /// (on the web, saves the flight recorder to browser storage).
    pub fn finish_clean(self, exit: &AppExit) {
        let _ = exit;
        self.shared
            .update_record(|r| r.status = session::stopped_status(r.status));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut this = self;
            this.flush_logs();
        }
        #[cfg(target_arch = "wasm32")]
        {
            if let Err(e) = session::web::save_log(&self.shared) {
                self.shared.problem(e);
            }
            std::mem::forget(self);
        }
    }

    /// Prints to stderr with the home directory replaced by `~`.
    pub fn eprint(&self, message: &str) {
        self.shared.eprint(message);
    }

    /// Where this session's log is, as shown to the player (home directory as `~`).
    pub fn log_location(&self) -> String {
        SessionInfo::of(&self.shared).log
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

    let mut findings = findings;
    let problems = std::mem::take(&mut findings.problems);
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
        problems: Mutex::new(Vec::new()),
        logging_ready: AtomicBool::new(false),
        #[cfg(not(target_arch = "wasm32"))]
        log_writer,
    });
    for problem in problems {
        shared.problem(problem);
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

/// This session's identity, for reports such as the scrub benchmark.
#[derive(Resource, Clone, Debug)]
pub struct SessionInfo {
    pub id: String,
    /// Where this session's log is (a path on native, home directory as `~`; browser storage
    /// on the web).
    pub log: String,
    /// [`BuildInfo::summary`] of the running build.
    pub build: String,
    /// When diagnostics started, just after launch.
    pub started: Instant,
}

impl SessionInfo {
    fn of(shared: &Shared) -> Self {
        let (id, build) = {
            let record = lock(&shared.record);
            (record.id.clone(), record.build.summary())
        };
        let log = match shared.session_dir() {
            Some(dir) => report::redact(
                &dir.join(session::LOG_FILE).display().to_string(),
                &shared.home,
            ),
            None if cfg!(target_arch = "wasm32") => {
                "browser localStorage (timefly.sessions; Download log saves it)".to_string()
            }
            None => "console only".to_string(),
        };
        SessionInfo {
            id,
            log,
            build,
            started: shared.started,
        }
    }
}

/// Logs `message` as an error through the real log layers (console, session log, flight
/// recorder) when the game's `App` will not run: a window-less `App` with the diagnostics logs
/// the session findings and the message, then ends. Only once per process (the log subscriber
/// is global).
pub fn log_error_without_app(session: &Session, message: &str) {
    let message = message.to_string();
    let mut app = App::new();
    app.add_plugins(DiagnosticsPlugin::headless(session))
        .add_plugins((MinimalPlugins, log_plugin()))
        .add_systems(Update, move || error!("{message}"));
    app.finish();
    app.cleanup();
    app.update();
}

/// Web: shows `text` in an HTML panel over the page, styled like the crash notice.
#[cfg(target_arch = "wasm32")]
pub fn show_web_message(session: &Session, text: &str) {
    notice::show_message(&session.shared, text);
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
fn console_layer(app: &mut App) -> Option<BoxedFmtLayer> {
    let home = app
        .world()
        .get_resource::<DiagnosticsHandle>()
        .map(|h| h.0.home.clone())
        .unwrap_or_default();
    Some(Box::new(
        bevy::log::tracing_subscriber::fmt::Layer::default()
            .with_writer(report::RedactingStderr(home))
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
            .insert_resource(SessionInfo::of(&self.shared))
            .init_resource::<DiagnosticsContext>()
            .add_systems(Startup, log_findings)
            .add_systems(
                Last,
                (
                    sync_context.run_if(resource_changed::<DiagnosticsContext>),
                    record_app_exits,
                ),
            );
        if !self.windowed {
            return;
        }
        app.add_domain_event::<FirstInteractiveFrame>()
            .add_systems(Update, update_render_context)
            .add_systems(PostUpdate, mark_first_frame);
        if let Some(pending) = lock(&self.shared.pending_notice).take() {
            notice::add_in_game_notice(app, pending);
        }
        probe::add_windowed_probe(app, self.shared.args.diagnostics_probe);
        #[cfg(target_arch = "wasm32")]
        app.add_systems(Update, session::web::save_log_periodically);
    }
}

fn log_findings(handle: Res<DiagnosticsHandle>) {
    let shared = &handle.0;
    let findings = &shared.findings;
    // Copy values out first: never log while holding a lock the panic hook needs.
    let (id, build, platform) = {
        let record = lock(&shared.record);
        (
            record.id.clone(),
            record.build.summary(),
            record.platform.summary(),
        )
    };
    let notice_route = *lock(&shared.notice_route);
    info!(session = %id, build = %build, platform = %platform, "session started");
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
    for problem in shared.take_problems_for_logging() {
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
                notice = ?notice_route,
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

/// Records an error exit as a crash as soon as it is requested, without waiting for `run()` to
/// return: on the web `run()` returns at once and `Session::finish` never sees the exit. Bevy
/// requests `AppExit::Error` after a render-thread panic and for wgpu validation, out-of-memory
/// and device-lost errors, on native and web alike.
fn record_app_exits(mut exits: MessageReader<AppExit>, handle: Res<DiagnosticsHandle>) {
    for exit in exits.read() {
        if let AppExit::Error(code) = exit {
            if code.get() == session::FAILED_RESULT_EXIT_CODE {
                info!(code = code.get(), "the app exited with a failed result");
            } else {
                error!(code = code.get(), "the app exited with an error");
            }
            handle
                .0
                .update_record(|r| r.status = session::finish_status(r.status, exit));
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

/// Domain event: the first interactive globe frame was drawn (written by the globe).
#[derive(Message, Copy, Clone, Debug, PartialEq, Eq)]
pub struct FirstInteractiveFrame {
    pub frame: u32,
}

/// Records the first interactive globe frame: from then on a crash is no longer "during
/// startup".
fn mark_first_frame(
    mut frames: MessageReader<FirstInteractiveFrame>,
    handle: Res<DiagnosticsHandle>,
) {
    if frames.read().last().is_some() {
        handle.0.update_record(|r| r.reached_first_frame = true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Shared` without files: `save_record` has no session directory to write to.
    fn test_shared() -> Arc<Shared> {
        Arc::new(Shared {
            args: LaunchArgs::default(),
            record: Mutex::new(SessionRecord {
                id: "0123abcd".into(),
                started_utc: "2026-11-22T09:33:00.123Z".into(),
                build: BuildInfo::current(),
                platform: PlatformInfo::current(),
                status: SessionStatus::Running,
                reached_first_frame: true,
                acknowledged: false,
            }),
            findings: Findings::default(),
            recorder: Mutex::new(Recorder::default()),
            context: Mutex::new(DiagnosticsContext::default()),
            home: Home::default(),
            started: Instant::now(),
            pending_notice: Mutex::new(None),
            notice_route: Mutex::new(NoticeRoute::None),
            problems: Mutex::new(Vec::new()),
            logging_ready: AtomicBool::new(true),
            #[cfg(not(target_arch = "wasm32"))]
            log_writer: None,
        })
    }

    #[test]
    fn session_info_redacts_the_log_path() {
        // The scrub bench prints `SessionInfo::log` with `println!`, past the redacting
        // writers, and the no-GPU dialog shows it.
        let mut shared = test_shared();
        let shared_mut = Arc::get_mut(&mut shared).unwrap();
        shared_mut.home = Home::new("/Users/someone", None);
        shared_mut.findings.session_dir =
            Some(PathBuf::from("/Users/someone/Library/Logs/Time Fly/s1"));
        assert_eq!(
            SessionInfo::of(&shared).log,
            format!("~/Library/Logs/Time Fly/s1/{}", session::LOG_FILE)
        );
    }

    /// An app whose system requests `exit` in `Update`; `run()` is never called.
    fn status_after_exit(exit: Option<AppExit>) -> SessionStatus {
        let shared = test_shared();
        let mut app = App::new();
        app.insert_resource(DiagnosticsHandle(shared.clone()))
            .add_systems(Last, record_app_exits)
            .add_systems(Update, move |mut writer: MessageWriter<AppExit>| {
                if let Some(exit) = exit.clone() {
                    writer.write(exit);
                }
            });
        app.update();
        lock(&shared.record).status
    }

    #[test]
    fn the_first_interactive_globe_frame_ends_startup() {
        let shared = test_shared();
        shared.update_record(|r| r.reached_first_frame = false);
        let mut app = App::new();
        app.insert_resource(DiagnosticsHandle(shared.clone()))
            .add_message::<FirstInteractiveFrame>()
            .add_systems(PostUpdate, mark_first_frame);
        app.update();
        assert!(
            !lock(&shared.record).reached_first_frame,
            "frames alone do not count"
        );
        app.world_mut()
            .resource_mut::<Messages<FirstInteractiveFrame>>()
            .write(FirstInteractiveFrame { frame: 7 });
        app.update();
        assert!(lock(&shared.record).reached_first_frame);
    }

    #[test]
    fn an_error_exit_is_recorded_as_a_crash_without_run_returning() {
        assert_eq!(
            status_after_exit(Some(AppExit::error())),
            SessionStatus::Crashed { panicked: false }
        );
    }

    #[test]
    fn a_success_exit_or_no_exit_leaves_the_session_running() {
        // `Session::finish` (native) or `pagehide` (web) marks it clean later.
        assert_eq!(
            status_after_exit(Some(AppExit::Success)),
            SessionStatus::Running
        );
        assert_eq!(status_after_exit(None), SessionStatus::Running);
    }

    #[test]
    fn an_error_exit_keeps_an_earlier_panic() {
        let shared = test_shared();
        shared.update_record(|r| r.status = SessionStatus::Crashed { panicked: true });
        let mut app = App::new();
        app.insert_resource(DiagnosticsHandle(shared.clone()))
            .add_systems(Last, record_app_exits);
        app.world_mut().write_message(AppExit::error());
        app.update();
        assert_eq!(
            lock(&shared.record).status,
            SessionStatus::Crashed { panicked: true }
        );
    }

    #[test]
    fn try_lock_for_gives_up_on_a_lock_this_thread_holds() {
        let mutex = Mutex::new(1);
        let held = lock(&mutex);
        assert!(try_lock_for(&mutex, Duration::from_millis(20)).is_none());
        drop(held);
        assert_eq!(
            try_lock_for(&mutex, Duration::from_millis(20)).map(|g| *g),
            Some(1)
        );
    }

    #[test]
    fn try_lock_for_waits_for_another_thread() {
        let mutex = Arc::new(Mutex::new(0));
        let held = mutex.clone();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut guard = lock(&held);
            locked_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(30));
            *guard = 7;
        });
        locked_rx.recv().unwrap();
        assert_eq!(
            try_lock_for(&mutex, Duration::from_secs(5)).map(|g| *g),
            Some(7)
        );
        worker.join().unwrap();
    }

    #[test]
    fn try_lock_for_ignores_poisoning() {
        let mutex = Arc::new(Mutex::new(3));
        let poisoner = mutex.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert_eq!(
            try_lock_for(&mutex, Duration::from_millis(20)).map(|g| *g),
            Some(3)
        );
    }
}
