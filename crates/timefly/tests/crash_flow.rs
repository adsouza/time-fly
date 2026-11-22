//! Process tests for crash detection: each runs the built game with a hidden
//! `--diagnostics-probe` and a temporary `TIMEFLY_LOG_DIR`. None opens a window.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use timefly::diagnostics::session::{PreviousOutcome, inspect_newest_session};

/// A fresh, empty log root under cargo's per-test temp dir.
fn log_root(test: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("crash_flow-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

/// A probe never opens a window and finishes in well under a second; anything slower is a
/// binary that ignored the probe (and may have opened a window), so it is killed.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

struct Output {
    status: ExitStatus,
    stderr: String,
}

fn probe(root: &Path, mode: &str) -> Output {
    probe_with_env(root, mode, &[])
}

fn probe_with_env(root: &Path, mode: &str, env: &[(&str, &str)]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_timefly"))
        .arg(format!("--diagnostics-probe={mode}"))
        .env("TIMEFLY_LOG_DIR", root)
        .env_remove("TIMEFLY_LOG")
        .env_remove("RUST_LOG")
        .envs(env.iter().copied())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the timefly binary");
    let mut stderr_pipe = child.stderr.take().expect("stderr");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr_pipe.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for timefly") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("probe {mode} still running after {PROBE_TIMEOUT:?}; killed it");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stderr: reader.join().unwrap_or_default(),
    }
}

fn session_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn only_session(root: &Path) -> PathBuf {
    let dirs = session_dirs(root);
    assert_eq!(dirs.len(), 1, "expected one session dir, found {dirs:?}");
    dirs.into_iter().next().unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn describe(out: &Output) -> String {
    format!("status {:?}\n--- stderr\n{}", out.status, out.stderr)
}

#[test]
fn panic_is_reported_next_run() {
    let root = log_root("panic");
    let out = probe(&root, "panic");
    assert!(!out.status.success(), "{}", describe(&out));

    let previous = inspect_newest_session(&root).expect("a session");
    assert_eq!(
        previous.outcome,
        PreviousOutcome::Crashed {
            during_startup: true,
            has_crash_report: true
        }
    );

    let crash = read(&only_session(&root).join("crash.txt"));
    assert!(crash.contains("diagnostics probe: panic"), "{crash}");
    assert!(
        crash.contains("probe step 2"),
        "recent events missing:\n{crash}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn abort_is_detected_without_report() {
    let root = log_root("abort");
    let out = probe(&root, "abort");
    assert!(!out.status.success(), "{}", describe(&out));

    let previous = inspect_newest_session(&root).expect("a session");
    assert!(
        matches!(
            previous.outcome,
            PreviousOutcome::Crashed {
                has_crash_report: false,
                ..
            }
        ),
        "{:?}",
        previous.outcome
    );
    let log = read(&only_session(&root).join("session.log"));
    assert!(log.contains("diagnostics probe started"), "{log}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn error_exit_is_a_crash() {
    // Bevy returns `AppExit::Error` from `run()` after a render-thread panic or a wgpu error.
    let root = log_root("error-exit");
    let out = probe(&root, "error-exit");
    assert!(!out.status.success(), "{}", describe(&out));
    assert_eq!(
        inspect_newest_session(&root).expect("a session").outcome,
        PreviousOutcome::Crashed {
            during_startup: true,
            has_crash_report: false
        }
    );
    let log = read(&only_session(&root).join("session.log"));
    assert!(log.contains("the app exited with an error"), "{log}");
    let _ = fs::remove_dir_all(&root);
}

/// Without a graphics adapter the game says so and stops before Bevy (which would panic in
/// renderer init); the session is clean, so the next launch shows no crash notice. The windowed
/// probe makes the run reach the GPU check without showing the dialog, and `WGPU_BACKEND=dx12`
/// (never compiled in off Windows) leaves wgpu no adapter, as on a machine without drivers.
#[cfg(not(windows))]
#[test]
fn no_gpu_adapter_is_a_clean_stop_with_a_message() {
    let root = log_root("no-gpu");
    let out = probe_with_env(&root, "panic-after-frame", &[("WGPU_BACKEND", "dx12")]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert!(
        out.stderr
            .contains("No compatible GPU/driver found (Vulkan/Metal)"),
        "{}",
        describe(&out)
    );
    assert_eq!(
        inspect_newest_session(&root).expect("a session").outcome,
        PreviousOutcome::Clean
    );
    let session = only_session(&root);
    assert!(!session.join("crash.txt").exists());
    let log = read(&session.join("session.log"));
    assert!(log.contains("No suitable graphics adapter found"), "{log}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn clean_exit_is_not_a_crash() {
    let root = log_root("clean");
    let out = probe(&root, "clean");
    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        inspect_newest_session(&root).expect("a session").outcome,
        PreviousOutcome::Clean
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn error_dumps_flight_recorder() {
    let root = log_root("error");
    let out = probe(&root, "error");
    assert!(out.status.success(), "{}", describe(&out));

    let log = read(&only_session(&root).join("session.log"));
    let marker = log
        .find("--- context before error ---")
        .unwrap_or_else(|| panic!("no marker in:\n{log}"));
    let after = &log[marker..];
    // Debug events are below the session file's level, so they appear only in the dump.
    let step = after
        .find("probe step 2")
        .unwrap_or_else(|| panic!("no debug events after the marker:\n{log}"));
    assert!(after[step..].contains("diagnostics probe: error"), "{log}");
    assert!(
        !log[..marker].contains("probe step 2"),
        "debug events leaked into the plain log:\n{log}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn logs_are_redacted() {
    let home = std::env::var("HOME").expect("HOME");
    let root = log_root("redact");
    let out = probe(&root, "panic");
    assert!(!out.status.success(), "{}", describe(&out));

    let dir = only_session(&root);
    let mut checked = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        assert!(
            !text.contains(&home),
            "{} contains the home path:\n{text}",
            path.display()
        );
        checked += 1;
    }
    assert!(
        checked >= 3,
        "expected session.json, session.log and crash.txt"
    );
    // The probe logged a path under the home directory; it must appear as `~`.
    assert!(
        read(&dir.join("session.log")).contains("~/"),
        "no redacted path in the session log"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn prunes_to_ten_sessions() {
    let root = log_root("prune");
    for _ in 0..12 {
        let out = probe(&root, "clean");
        assert!(out.status.success(), "{}", describe(&out));
    }
    assert_eq!(session_dirs(&root).len(), 10);
    let _ = fs::remove_dir_all(&root);
}
