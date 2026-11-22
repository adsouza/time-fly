//! Build facts for crash reports and session records (`BuildInfo`).

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=TIMEFLY_GIT_SHA");

    // CI sets TIMEFLY_GIT_SHA; local builds ask git, and fall back to "unknown".
    let sha = env::var("TIMEFLY_GIT_SHA")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--short", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());

    // Rebuild when HEAD moves: watch HEAD, the branch it points at, and packed refs.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) {
        let head = git_dir.join("HEAD");
        println!("cargo:rerun-if-changed={}", head.display());
        if let Some(reference) = std::fs::read_to_string(&head)
            .ok()
            .and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_string()))
        {
            let ref_path = git_dir.join(reference);
            if ref_path.exists() {
                println!("cargo:rerun-if-changed={}", ref_path.display());
            }
        }
        let packed = git_dir.join("packed-refs");
        if packed.exists() {
            println!("cargo:rerun-if-changed={}", packed.display());
        }
    }

    let mut features: Vec<String> = env::vars()
        .filter_map(|(k, _)| {
            k.strip_prefix("CARGO_FEATURE_")
                .map(|f| f.to_lowercase().replace('_', "-"))
        })
        .filter(|f| f != "default")
        .collect();
    features.sort();

    println!("cargo:rustc-env=TIMEFLY_GIT_SHA={sha}");
    println!(
        "cargo:rustc-env=TIMEFLY_BUILD_PROFILE={}",
        env::var("PROFILE").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=TIMEFLY_BUILD_TARGET={}",
        env::var("TARGET").unwrap_or_default()
    );
    println!(
        "cargo:rustc-env=TIMEFLY_BUILD_FEATURES={}",
        features.join(",")
    );
}
