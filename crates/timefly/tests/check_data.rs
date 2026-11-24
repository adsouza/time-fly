//! `--check-data[=DIR]`: loads a bake through the real loader in a window-less app (no GPU),
//! then the region tiles beside it, and exits 0 when both load, 1 when either fails. CI runs it
//! on the production bake; these tests run it on the committed tiny bake and fixture tiles.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use timefly::check_data::{
    CHECKED_TILES, DataSummary, TileSummary, check_globe_data, check_tiles, tiles_dir_beside,
};
use timefly::diagnostics::session::{PreviousOutcome, inspect_newest_session};

fn tiny_globe() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets/data/globe")
}

fn tiny_tiles() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets/data/tiles")
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
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
fn the_fixture_tiles_check_ok() {
    assert_eq!(
        tiles_dir_beside(&tiny_globe()),
        tiny_tiles().parent().unwrap().join("tiles")
    );
    let summary = check_tiles(&tiny_tiles(), Duration::from_secs(20)).expect("tiles load");
    assert_eq!(summary.listed, 4);
    // The fixture overview: one node per degree, both edges included.
    assert_eq!(summary.overview, (361, 181));
    assert_eq!(summary.loaded.len(), CHECKED_TILES);
    let fixtures = [(17, 35), (17, 36), (18, 35), (18, 36)];
    assert!(
        summary.loaded.iter().all(|t| fixtures.contains(t)),
        "{summary:?}"
    );
    // Largest first.
    let size = |&(r, c): &(u8, u8)| {
        fs::metadata(tiny_tiles().join(format!("r{r:02}c{c:02}.tfgrid")))
            .unwrap()
            .len()
    };
    assert!(
        summary
            .loaded
            .windows(2)
            .all(|w| size(&w[0]) >= size(&w[1]))
    );
    let text = summary.to_string();
    assert!(
        text.starts_with("4 tiles listed, overview 361x181, loaded 3 ("),
        "{text}"
    );
}

#[test]
fn a_corrupt_or_missing_tile_fails_the_tile_check() {
    let summary: TileSummary = check_tiles(&tiny_tiles(), Duration::from_secs(20)).unwrap();
    let (r, c) = summary.loaded[0];
    let name = format!("r{r:02}c{c:02}.tfgrid");

    let corrupt = temp_dir("corrupt-tile");
    copy_dir(&tiny_tiles(), &corrupt);
    let mut bytes = fs::read(corrupt.join(&name)).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    fs::write(corrupt.join(&name), bytes).unwrap();
    let error = check_tiles(&corrupt, Duration::from_secs(20)).expect_err("corrupt");
    assert!(error.contains(&name), "{error}");

    let missing = temp_dir("missing-tile");
    copy_dir(&tiny_tiles(), &missing);
    fs::remove_file(missing.join(&name)).unwrap();
    let error = check_tiles(&missing, Duration::from_secs(20)).expect_err("missing");
    assert!(
        error.contains(&name) && error.contains("not found"),
        "{error}"
    );

    let error = check_tiles(&temp_dir("no-tiles"), Duration::from_secs(20)).expect_err("none");
    assert!(
        error.contains("manifest.json") && error.contains("not found"),
        "{error}"
    );
}

#[test]
fn checker_validates_the_published_generations_overview_layout() {
    let root = temp_dir("generation-shape");
    let generation = "a".repeat(64);
    let payloads = root.join(&generation);
    copy_dir(&tiny_tiles(), &payloads);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(payloads.join("manifest.json")).unwrap()).unwrap();
    manifest["version"] = 2.into();
    manifest["generation"] = generation.into();
    let path = root.join("manifest.json");
    fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    check_tiles(&root, Duration::from_secs(20)).unwrap();

    let tile = &manifest["tiles"][0];
    let name = format!(
        "r{:02}c{:02}.tfgrid",
        tile["row"].as_u64().unwrap(),
        tile["col"].as_u64().unwrap()
    );
    fs::copy(payloads.join(name), payloads.join("overview.tfgrid")).unwrap();
    manifest["overview"] = tile.clone();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let error = check_tiles(&root, Duration::from_secs(20)).unwrap_err();
    assert!(
        error.contains("overview.tfgrid") && error.contains("301"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn checker_rejects_a_single_channel_tile_with_valid_dimensions_and_hash() {
    let root = temp_dir("tile-channels");
    copy_dir(&tiny_tiles(), &root);
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let tile = &mut manifest["tiles"][0];
    let name = format!(
        "r{:02}c{:02}.tfgrid",
        tile["row"].as_u64().unwrap(),
        tile["col"].as_u64().unwrap()
    );
    let mut bytes = fs::read(root.join(&name)).unwrap();
    // One f32 consumes the same payload bytes as the fixture's two f16 channels.
    bytes[9] = 2;
    bytes[10] = 1;
    tile["sha256"] = timefly_core::sha256::sha256_hex(&bytes).into();
    fs::write(root.join(&name), bytes).unwrap();
    fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let error = check_tiles(&root, Duration::from_secs(20)).unwrap_err();
    assert!(
        error.contains(&name) && error.contains("2-channel"),
        "{error}"
    );
    fs::remove_dir_all(root).unwrap();
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
    assert!(
        run.stdout
            .contains("region tiles OK: 4 tiles listed, overview 361x181, loaded 3 ("),
        "{}",
        run.stdout
    );
    let previous = inspect_newest_session(&logs.join("data-checks")).expect("a session");
    assert_eq!(previous.outcome, PreviousOutcome::Clean);
}

#[test]
fn the_binary_exits_1_when_the_tiles_beside_the_globe_are_missing() {
    let logs = temp_dir("binary-no-tiles-logs");
    let bake = temp_dir("binary-no-tiles-bake");
    copy_dir(&tiny_globe(), &bake.join("data/globe"));
    let run = run_binary(
        &logs,
        &format!("--check-data={}", bake.join("data/globe").display()),
    );
    assert_eq!(
        run.code,
        Some(1),
        "stdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    assert!(run.stdout.contains("globe data OK"), "{}", run.stdout);
    assert!(
        run.stderr.contains("region tiles FAILED") && run.stderr.contains("manifest.json"),
        "stderr:\n{}",
        run.stderr
    );
    let previous = inspect_newest_session(&logs.join("data-checks")).expect("a session");
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
    let previous = inspect_newest_session(&logs.join("data-checks")).expect("a session");
    assert_eq!(previous.outcome, PreviousOutcome::Clean);
}

#[test]
fn data_check_preserves_pending_game_crash() {
    let logs = temp_dir("pending-crash");
    let crash = run_binary(&logs, "--diagnostics-probe=panic");
    assert_ne!(crash.code, Some(0));
    let before = inspect_newest_session(&logs).unwrap();
    let check = run_binary(&logs, &format!("--check-data={}", tiny_globe().display()));
    assert_eq!(check.code, Some(0), "{}", check.stderr);
    assert!(!check.stderr.contains("didn't close normally"));
    let after = inspect_newest_session(&logs).unwrap();
    assert_eq!(after.dir, before.dir);
    assert!(matches!(
        after.outcome,
        PreviousOutcome::Crashed {
            during_startup: true,
            ..
        }
    ));
}
