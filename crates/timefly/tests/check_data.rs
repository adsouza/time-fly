//! `--check-data[=DIR]`: loads a bake through the real loader in a window-less app (no GPU) and
//! exits 0 when it reaches `Ready`, 1 when it fails. CI runs it on the production bake; these
//! tests run it on the committed tiny bake.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use timefly::check_data::{DataSummary, check_globe_data};
use timefly::diagnostics::session::{PreviousOutcome, inspect_newest_session};

fn tiny_globe() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets/data/globe")
}

fn temp_dir(test: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("check_data-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn the_tiny_bake_checks_ready() {
    let summary = check_globe_data(&tiny_globe(), Duration::from_secs(20)).expect("Ready");
    assert_eq!(
        summary,
        DataSummary {
            frames: 4,
            images: 3,
            plates: summary.plates,
            cube_size: 8,
            revision: summary.revision.clone(),
        }
    );
    assert!(summary.plates > 0);
    assert!(
        summary.revision.starts_with("manifest v2, sha256 "),
        "{}",
        summary.revision
    );
}

#[test]
fn a_missing_bake_fails_with_the_reason() {
    let empty = temp_dir("missing");
    let error = check_globe_data(&empty, Duration::from_secs(20)).expect_err("Failed");
    assert!(error.contains("manifest.json"), "{error}");
    assert!(error.contains("not found"), "{error}");
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_binary(log_root: &Path, arg: &str) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_timefly"))
        .arg(arg)
        .env("TIMEFLY_LOG_DIR", log_root)
        .env_remove("TIMEFLY_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run the timefly binary");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn the_binary_checks_a_bake_and_exits_0() {
    let logs = temp_dir("binary-ok");
    let run = run_binary(&logs, &format!("--check-data={}", tiny_globe().display()));
    assert_eq!(
        run.code,
        Some(0),
        "stdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.stdout.contains("globe data OK: 4 frames, 3 images,"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("cube size 8"), "{}", run.stdout);
    assert!(
        run.stdout.contains("manifest v2, sha256 "),
        "{}",
        run.stdout
    );
    let previous = inspect_newest_session(&logs).expect("a session");
    assert_eq!(previous.outcome, PreviousOutcome::Clean);
}

#[test]
fn the_binary_exits_1_on_a_broken_bake_without_recording_a_crash() {
    let logs = temp_dir("binary-fail-logs");
    let empty = temp_dir("binary-fail-bake");
    let run = run_binary(&logs, &format!("--check-data={}", empty.display()));
    assert_eq!(
        run.code,
        Some(1),
        "stdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    assert!(
        run.stderr.contains("globe data FAILED"),
        "stderr:\n{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("manifest.json"),
        "stderr:\n{}",
        run.stderr
    );
    // A failed check is a result, not a crash: the next launch shows no crash notice.
    let previous = inspect_newest_session(&logs).expect("a session");
    assert_eq!(previous.outcome, PreviousOutcome::Clean);
}
