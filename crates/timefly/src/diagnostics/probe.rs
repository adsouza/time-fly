//! The hidden `--diagnostics-probe` (web: `?diagnostics-probe=`): starts diagnostics, logs a
//! few events, then crashes or exits the way the mode asks. Tests and the manual checks use it.

use bevy::diagnostic::FrameCount;
use bevy::prelude::*;

use super::{DiagnosticsHandle, DiagnosticsPlugin, Session, lock, log_plugin};

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

/// Adds the systems for a probe that runs in the real, windowed app.
pub(crate) fn add_windowed_probe(app: &mut App, probe: Option<ProbeMode>) {
    if probe == Some(ProbeMode::PanicAfterFrame) {
        app.add_systems(Startup, || log_probe_events(ProbeMode::PanicAfterFrame))
            .add_systems(Update, panic_after_first_frame);
    }
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
        super::session::web::save_log(&session.shared);
        std::process::abort();
    }
    session.finish();
    AppExit::Success
}
