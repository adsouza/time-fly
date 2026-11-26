//! `--hydro-check[=OUT_DIR]`: the rivers' reality check, headless. These tests run the binary on
//! the committed tiny bake with a tiny reference (`tests/assets/hydro_reference`: a coarse
//! lattice, one box whose link follows the tiny terrain's own river, Lake Victoria from the
//! shipped map, one outlet). The real check runs on the production bake by hand
//! (`docs/testing/rivers/reality-check.md`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use timefly::diagnostics::session::{
    FAILED_RESULT_EXIT_CODE, PreviousOutcome, inspect_newest_session,
};
use timefly::hydro_check::REFERENCE_ENV;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn tiny_reference() -> PathBuf {
    manifest_dir().join("tests/assets/hydro_reference")
}

fn temp_dir(test: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("hydro_check-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    root
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the check on the tiny bake (`tests/assets`) against `reference`, SVGs into `out`.
fn run(logs: &Path, reference: &Path, out: &Path) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_timefly"))
        .arg(format!("--hydro-check={}", out.display()))
        .env("BEVY_ASSET_ROOT", manifest_dir().join("tests"))
        .env(REFERENCE_ENV, reference)
        .env("TIMEFLY_LOG_DIR", logs)
        .env_remove("TIMEFLY_LOG")
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .output()
        .expect("run the timefly binary");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

#[test]
fn hydro_check_runs_on_the_tiny_bake() {
    let root = temp_dir("pass");
    let out = root.join("out");
    let r = run(&root.join("logs"), &tiny_reference(), &out);
    let all = format!("stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert_eq!(r.code, Some(0), "{all}");
    for line in [
        "pass at 0 ka: ",
        "lattice n = 64",
        "Beyer climate: true",
        "box tiny: relaxed 100.0% within 50 km, 1/1 links matched",
        "lake Lake Victoria (-1, 33): level 1133.0 m, the lake map's fine surface 1133.0 m",
        "outlet tiny river: ",
        "passes 0.0 km from the equator",
        "ends in the ocean at (-0.90, -0.90)",
        "arid basins (< 250 mm/yr): game 0 of 0 large lakes in the boxes [], reference 0 of 1 - PASS",
        "hydro check PASS",
    ] {
        assert!(r.stdout.contains(line), "missing {line:?} in {all}");
    }
    let svg = fs::read_to_string(out.join("hydro-check-tiny.svg")).expect("the box's SVG");
    assert!(svg.contains("#1f6feb") && svg.contains("#d1242f") && svg.contains("100 km"));
    let previous = inspect_newest_session(&root.join("logs/data-checks")).expect("a session");
    assert_eq!(previous.outcome, PreviousOutcome::Clean);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_failed_bar_exits_with_the_failed_result_code() {
    // Lake Volta is a reservoir, which the lake map drops: no lake stands there.
    let root = temp_dir("fail");
    let reference = root.join("reference");
    fs::create_dir_all(&reference).unwrap();
    for name in ["tiny.json", "spec.json"] {
        fs::copy(tiny_reference().join(name), reference.join(name)).unwrap();
    }
    let path = reference.join("spec.json");
    let mut spec: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    spec["lakes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"name": "Lake Volta", "lat": 7.5, "lon": -0.2}));
    fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let r = run(&root.join("logs"), &reference, &root.join("out"));
    let all = format!("stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert_eq!(r.code, Some(i32::from(FAILED_RESULT_EXIT_CODE)), "{all}");
    assert!(
        r.stderr
            .contains("lake Lake Volta (7.5, -0.2): no lake there - FAIL"),
        "{all}"
    );
    assert!(r.stderr.contains("hydro check FAIL"), "{all}");
    // A failed result is not a crash.
    let previous = inspect_newest_session(&root.join("logs/data-checks")).expect("a session");
    assert_eq!(previous.outcome, PreviousOutcome::Clean);

    // A missing reference fixture: the check cannot run, exit 1.
    fs::remove_file(reference.join("tiny.json")).unwrap();
    let r = run(&root.join("logs"), &reference, &root.join("out"));
    assert_eq!(
        r.code,
        Some(1),
        "stdout:\n{}\nstderr:\n{}",
        r.stdout,
        r.stderr
    );
    assert!(
        r.stderr.contains("hydro check could not run"),
        "{}",
        r.stderr
    );
    fs::remove_dir_all(root).unwrap();
}
