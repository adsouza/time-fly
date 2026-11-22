# Milestone 1 — Globe & Time Slider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A painterly globe that animates real continental drift as you drag a log-scaled time slider from today back to 540 Ma, with ice sheets and sea level for the last ~26 ka. Development and acceptance checks run on this Mac (Apple M4): natively on Metal and in Chrome on WebGPU. CI-built x86-64 Linux packages (.deb and Flatpak) add a manual Vulkan acceptance run on the owner's Linux machine, which is for testing only, not development.

**Architecture:**
- **Offline:** a Python bake tool (pygplates + numpy) turns PaleoDEMs and ICE-6G_C into geographic cube maps, and exports the matching PALEOMAP plate footprints and rotation validity. Each PaleoDEM records the age of its source reconstruction independently of its timeline age.
- **Runtime:** a Bevy 0.20 app renders independent rigid plate pieces over an ocean shell. Rotate each plate-fixed sample into the geographic frames of the two source rasters, then blend their valid samples as the piece moves. A fixed geographic sphere displays recent topography, land cover and ice cover.
- **Functional core, imperative shell (spec §19):** `timefly_core` is the functional core: pure Rust with no Bevy app and no I/O, organized by bounded context (deep time, geodynamics, paleo-surface, plus the geo/cube shared kernel and format parsers). It holds the time model, coordinate conventions, binary formats, rotation interpolation, forward/inverse crust lookups and frame selection, and is unit-tested apart from rendering. The Bevy crate is the imperative shell.
- **Diagnostics (spec §20):** tracing applied at seams, per-session logs, a flight recorder, and a next-run crash notice on every platform. Task 2 builds it first so every later task is instrumented.

**Tech Stack:**
- Rust: Bevy 0.20.1 (WESL shaders, `bevy_ui_widgets`, `trace`), `bevy_math` 0.20 (glam 0.33), `ruzstd` 0.9, `half` 2, `serde`/`serde_json`, `bytemuck`, `tracing` 0.1, `tracing-appender` 0.2; native `directories` 6, `fd-lock` 4, `opener` 0.9, `rfd` 0.18; wasm `web-sys` 0.3.
- Python 3.13 via `uv` (pinned in `tools/bake/.python-version`: `pygplates` 1.0.0 wheels stop at cp313): `pygplates` 1.0, `numpy`, `netCDF4`, `zstandard`, `pytest`.

**Spec:** `docs/superpowers/specs/2026-11-21-time-fly-design.md`. This plan covers **§14 step 1 (Globe + slider)**, including the crust lookup contract for later worlds, the reference-year convention for cultural data, and the architecture and diagnostics foundations of §19–20. The cultural CSV import belongs to Milestone 5.

## Global Constraints

- Bevy `0.20` with feature `zstd_rust` (not `zstd`) and `ktx2`, `basis-universal`. On `wasm32`, add feature `webgpu`.
- All shaders are `.wesl`.
- WebGPU baseline is the ceiling: no mesh shaders, no `MeshletMesh`, no 64-bit atomics, no unbounded `binding_array`, no `std::thread` in core loops. Use only 32-bit atomics.
- No texture-format feature beyond baseline. Use filterable `Rg16Float` for PaleoDEM and `Rgba16Float` for ICE-6G_C. Both bind as float cube textures.
- `cargo check --target wasm32-unknown-unknown -p timefly` must pass at the end of every task that touches `crates/`. Verify against the same production bake on Metal and Chrome WebGPU on this Mac in Task 10, and on Vulkan with the CI-built Linux packages (.deb and Flatpak) in Task 11.
- Geographic convention (pygplates): `x = cos(lat)cos(lon)`, `y = cos(lat)sin(lon)`, `z = sin(lat)`. Render frame (Bevy, Y-up): `render = (geo.y, geo.z, geo.x)`. Quaternions convert by the same permutation of `(x, y, z)`; `w` is unchanged.
- Timeline and source reconstruction ages are **Myr (f64)** at manifest and runtime API boundaries; the time model uses `YearsBp(f64)`. `YearsBp::from_myr(m) = m * 1e6`. The game reference year is 2026 CE and source ages in 1950 BP gain 76 years on import. Preserve published PaleoDEM reconstruction times for rotation lookup. Binary formats store ages as f64. Packed GPU times may use f32 with boundary behavior checked against the CPU.
- 1 world unit = 1 km. `EARTH_RADIUS_KM = 6371.0`. Elevation is in metres.
- Sea-level anomalies are sea level relative to present (negative = lower sea). Apply them everywhere, on CPU and GPU, as `effective_elev_m = elev_m - anomaly_m`, so a −120 m anomaly lifts a −100 m shelf to +20 m.
- Raw downloaded data lives in `data/raw/` (git-ignored). Baked output lives in `crates/timefly/assets/data/globe/` (git-ignored; reproducible with the bake CLI).
- Every downloaded dataset is recorded in `data/README.md` with source URL, licence and citation, and cited in `crates/timefly/assets/credits.txt`.
- **Downloads need the user's explicit OK** (state the filename, source and approximate size).
- Every commit message ends with the trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Architecture (spec §19).** `timefly_core` is the functional core: no Bevy `App` or ECS, no I/O (bytes in, values out), no clocks, globals or unseeded randomness. `timefly` is the imperative shell. Its systems follow gather → decide → apply and keep decisions in pure functions: in the core, or pure `fn`s in the shell module for presentation-only logic such as UI fade. Each pure function is unit-tested directly. The bake tool keeps pure array transforms apart from I/O (`cli`, `fetch`, loaders, writers). Newtypes separate `PlateId` from `PlateIndex` and `GeoDir` from `RenderDir`; elsewhere names carry unit suffixes. Aggregates (`CrustModel`, `Timeline`) are built only through validating constructors. Names follow the spec's ubiquitous language.
- **Observability (spec §20).** Shell boundary functions (loaders, file I/O, GPU resource creation, plugin setup) get `#[instrument(level = "debug", skip_all, fields(...), err)]`; `err` logs returned errors. Bevy `trace` spans cover systems. Domain events and states are registered only through Task 2's `add_domain_event` / `add_logged_state`. The core gets attributes only on coarse entry points; per-sample functions are never instrumented, and core function bodies never log. Levels: `error` = user-visible failure, `warn` = a fallback was used, `info` = lifecycle and domain events, `debug` = boundary calls, `trace` = detail. Every public bake function carries `@traced` (enforced by a test). Never log personal data, and route paths through redaction.
- **Build-disk hygiene (owner request).** The dev profile keeps no dependency debug info (workspace crates keep line tables). `tools/dev/target-guard.sh` runs after every cargo command through a Claude Code hook and prunes `target/` above 15 GB or below 5 GB free (Task 1b). Never build feature or profile variants into the main target dirs ad hoc: use `CARGO_TARGET_DIR=target/scratch-<name>` and delete it afterwards. Verify wasm with `cargo check` except in the web end-to-end checks.
- Never push without the owner's explicit OK. Task 11 needs one push and one workflow run; ask before each.

## Review Focus

1. **Slider at the extremes:** age exactly 0, exactly 540 Ma, or a slider value outside [0, 1]. It must clamp without NaN, panic or out-of-range frame index. Tests: Task 1 `slider_clamps`, Task 8 `bracket_clamps_at_ends`.
2. **Antimeridian and poles:** sampling at lon ±180° and lat ±90°, and cube-face edges, must not produce a seam or out-of-bounds index. Tests: Task 3 `dir_to_texel_handles_axes_and_edges`, Task 6 `bilinear_wraps_longitude_and_clamps_latitude`.
3. **Plate boundaries, missing circuits and missing crust:** Task 5 tests forward/inverse lookup and expiry. Task 9 tests a synthetic two-plate rift and overlap, then compares a real bake against source coastlines. A deep-time gap exposes the ocean shell.
4. **Corrupt, truncated or missing data files** (common on flaky web loads) must return errors, log clearly and show a "data failed to load" message, never panic. Tests: Task 4 `rejects_bad_magic_and_truncation`, Task 8 `failed_load_enters_failed_state`.
5. **Fast scrubbing across the complete timeline:** Task 8 checks preloaded handles. Task 10 measures cold load, peak memory and scrub frame times with the full bake on Chrome WebGPU and Metal on this Mac. Task 11 repeats the scrub benchmark on Linux Vulkan.
6. **Recent coastlines and floating ice:** Task 7 tests signed bathymetry, exposed shelf and distinct grounded and floating ice cases. Task 10 checks the real-data result around the Last Glacial Maximum and across the bridge to deep time.
7. **Sea-level sign:** a wrong sign floods shelves exactly when they should be dry. Test: Task 8 `sea_level_offset_exposes_shelf`.
8. **Crash flow:** a panic, an abort and an error each leave what the next run needs. Tests: Task 2 `crash_flow.rs`. Manual checks: Task 2 Step 7, Task 11 Step 7.

---

## File Structure

```
.gitignore
.github/workflows/linux-package.yml  CI: bake + x86-64 Linux .deb and Flatpak
Cargo.toml                         workspace
.cargo/config.toml                 wasm runner
crates/timefly_core/               functional core: pure Rust, no Bevy App, no I/O
  Cargo.toml
  src/lib.rs
  src/deep_time.rs                 deep time: YearsBp, slider mapping, snapshots, labels
  src/geo.rs                       shared kernel: GeoDir/RenderDir, lat/lon <-> geo <-> render
  src/cube.rs                      shared kernel: cube-map texel <-> direction convention
  src/geodynamics/mod.rs           PlateId, PlateIndex, CrustPos
  src/geodynamics/rotation.rs      RotationTable interpolation and validity
  src/geodynamics/crust.rs         PlateFootprints, CrustModel aggregate, forward/inverse lookup
  src/surface/mod.rs, timeline.rs  paleo-surface: Timeline aggregate, bracket, frame selection, sea level
  src/formats.rs                   .tfgrid / .tfrot / .tfplates parsers (pure: bytes in, values out)
  tests/fixtures/                  files written by the Python tool (cross-language tests)
crates/timefly/                    the game: imperative shell
  Cargo.toml
  build.rs                         BuildInfo (version, git SHA, profile, target)
  src/main.rs
  src/lib.rs                       app() builder
  src/time.rs                      TimePlugin (GlobeTime, stepping)
  src/data.rs                      GlobeDataPlugin (manifest, loaders, load state; maps Timeline image slots to handles)
  src/diagnostics/                 session, recorder, report, notice, aspects (spec §20)
  src/globe/mod.rs                 GlobePlugin
  src/globe/mesh.rs                fixed globe and independently rotated plate pieces
  src/globe/material.rs            GlobeMaterial + AtmosphereMaterial + update systems
  src/camera.rs                    orbit camera
  src/bench.rs                     scrub benchmark mode (--scrub-bench)
  src/ui/mod.rs, slider.rs, credits.rs, status.rs
  assets/shaders/globe.wesl         fixed globe, plate pieces and ocean shell
  assets/shaders/atmosphere.wesl
  assets/credits.txt
tools/bake/                        Python bake tool (uv project)
  pyproject.toml
  timefly_bake/__init__.py, log.py, geo.py, cube.py, formats.py, plates.py, paleodem.py, ice6g.py, sealevel.py, cli.py, fetch.py
  tests/test_*.py
data/README.md                     sources, licences, citations, download steps
data/sources.json                  pinned raw-data URLs + SHA-256 (used by fetch.py locally and in CI)
packaging/linux/, packaging/flatpak/  .desktop, AppStream, icon, staging + .deb scripts, Flatpak manifest
docs/testing/linux-vulkan.md       owner's manual Linux test checklist
data/culture/civilizations.csv     owner's CSV (copied; used in a later milestone)
reference/draft/                   owner's draft architecture + shaders (later milestones)
```

---

### Task 1: Workspace scaffold + time model

**Files:**
- Create: `.gitignore`, `Cargo.toml`, `.cargo/config.toml`, `crates/timefly_core/{Cargo.toml,src/lib.rs,src/deep_time.rs}`, `crates/timefly/{Cargo.toml,src/main.rs,src/lib.rs}`, `tools/bake/pyproject.toml`, `tools/bake/timefly_bake/__init__.py`, `tools/bake/tests/test_smoke.py`, `data/README.md`
- Copy: `~/Downloads/bevy-hybrid-lod-architecture-v4.md`, `terrain_cdlod.wesl`, `voxel_surface_nets.wesl` → `reference/draft/`; `~/Downloads/Chronology of global human civilizations - Sheet1.csv` → `data/culture/civilizations.csv`
- Test: `crates/timefly_core/src/deep_time.rs` (unit tests module)

**Interfaces:**
- Produces (in `timefly_core::deep_time`):
  - `pub struct YearsBp(pub f64)` (`Copy, Clone, Debug, PartialEq, PartialOrd`)
  - `YearsBp::from_myr(m: f64) -> YearsBp`, `YearsBp::myr(self) -> f64`
  - `pub const MAX_YEARS_BP: f64 = 540.0e6`, `pub const SLIDER_OFFSET_YEARS: f64 = 50.0`
  - `pub const PRESENT_YEAR_CE: i32 = 2026`, `pub const BP_REFERENCE_YEAR_CE: i32 = 1950`, `YearsBp::from_1950_bp(years: f64) -> YearsBp`
  - `pub fn slider_to_years(s: f64) -> YearsBp`, `pub fn years_to_slider(y: YearsBp) -> f64`
  - `pub const STANDARD_SNAPSHOTS: [f64; 24]` (years BP, ascending)
  - `pub fn next_snapshot(y: YearsBp) -> YearsBp` (strictly older; clamps at 540 Ma), `pub fn prev_snapshot(y: YearsBp) -> YearsBp` (strictly younger; clamps at 0)
  - `pub fn format_years_bp(y: YearsBp) -> String`
- Produces: `timefly::app() -> bevy::app::App` (DefaultPlugins only for now) in `crates/timefly/src/lib.rs`; `main.rs` calls `timefly::app().run()`.

- [ ] **Step 1: Create the repo and workspace**

The repo already exists (initial commit, GitHub remote `origin`); don't re-initialize it. `.gitignore` contains: `target/`, `data/raw/`, `crates/timefly/assets/data/`, `.superpowers/`, `tools/bake/.venv/`, `__pycache__/`, `.DS_Store`.

Root `Cargo.toml` is a workspace with members `crates/timefly_core` and `crates/timefly`, `resolver = "3"`, and the draft's profiles (dev deps at opt-level 3, dev at 1, release with `lto = "thin"` and `codegen-units = 1`).

`crates/timefly/Cargo.toml` (edition 2024):
- deps: `bevy = { version = "0.20", features = ["ktx2", "basis-universal", "zstd_rust", "bevy_ui_widgets"] }`, `timefly_core = { path = "../timefly_core" }`, `serde = { version = "1", features = ["derive"] }`, `serde_json = "1"`, `bytemuck = { version = "1.19", features = ["derive"] }`
- wasm32 target section: `bevy` with feature `webgpu`, plus `console_error_panic_hook = "0.1"`

`crates/timefly_core/Cargo.toml`: `bevy_math = "0.20"`, `ruzstd = "0.9"`, `half = "2"`.

`.cargo/config.toml`: `[target.wasm32-unknown-unknown] runner = "wasm-server-runner"`.

`tools/bake/pyproject.toml`: project `timefly-bake`, Python `>=3.12`, deps `numpy`, `netCDF4`, `zstandard`, `pygplates>=1.0`; dev dep `pytest`; script `timefly-bake = "timefly_bake.cli:main"`.

`data/README.md`: a table with one row per dataset (filled in Tasks 5–7), plus one line per copied reference file.

- [ ] **Step 2: Write the failing time-model tests** in `crates/timefly_core/src/deep_time.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn close(a: f64, b: f64, rel: f64) -> bool { (a - b).abs() <= rel * b.abs().max(1.0) }

    #[test] fn slider_endpoints() {
        assert_eq!(slider_to_years(0.0).0, 0.0);
        assert!(close(slider_to_years(1.0).0, 540.0e6, 1e-9));
    }
    #[test] fn slider_midpoints_are_log_spaced() {
        assert!(close(slider_to_years(0.25).0, 2816.328442963292, 1e-9));
        assert!(close(slider_to_years(0.5).0, 164266.77485880742, 1e-9));
        assert!(close(slider_to_years(0.75).0, 9419666.908675905, 1e-9));
    }
    #[test] fn slider_round_trips_and_known_ages() {
        for s in [0.0, 0.1, 0.37, 0.8, 1.0] { assert!((years_to_slider(slider_to_years(s)) - s).abs() < 1e-9); }
        assert!((years_to_slider(YearsBp(66.0e6)) - 0.8702126358373475).abs() < 1e-9);
        assert!((years_to_slider(YearsBp(21_000.0)) - 0.3731158781360922).abs() < 1e-9);
    }
    #[test] fn slider_clamps() {
        assert_eq!(slider_to_years(-0.5).0, 0.0);
        assert!(close(slider_to_years(1.5).0, 540.0e6, 1e-9));
        assert_eq!(years_to_slider(YearsBp(-10.0)), 0.0);
        assert_eq!(years_to_slider(YearsBp(1.0e12)), 1.0);
    }
    #[test] fn bp_reference_year() {
        assert_eq!(YearsBp::from_1950_bp(0.0).0, 76.0);
        assert_eq!(YearsBp::from_1950_bp(21_000.0).0, 21_076.0);
    }
    #[test] fn snapshot_stepping() {
        assert_eq!(next_snapshot(YearsBp(0.0)).0, 100.0);
        assert_eq!(next_snapshot(YearsBp(150.0)).0, 500.0);
        assert_eq!(prev_snapshot(YearsBp(150.0)).0, 100.0);
        assert_eq!(prev_snapshot(YearsBp(0.0)).0, 0.0);
        assert_eq!(next_snapshot(YearsBp(540.0e6)).0, 540.0e6);
    }
    #[test] fn labels() {
        assert_eq!(format_years_bp(YearsBp(0.0)), "Present");
        assert_eq!(format_years_bp(YearsBp(100.0)), "100 years ago");
        assert_eq!(format_years_bp(YearsBp(9_999.0)), "9,999 years ago");
        assert_eq!(format_years_bp(YearsBp(21_000.0)), "21 thousand years ago");
        assert_eq!(format_years_bp(YearsBp(1_500_000.0)), "1.5 million years ago");
        assert_eq!(format_years_bp(YearsBp(5_000_000.0)), "5 million years ago");
        assert_eq!(format_years_bp(YearsBp(66.0e6)), "66 million years ago");
        assert_eq!(format_years_bp(YearsBp(540.0e6)), "540 million years ago");
    }
}
```

- [ ] **Step 3: Run it to confirm it fails**

Run: `cargo test -p timefly_core`
Expected: compile errors (functions not defined).

- [ ] **Step 4: Implement `timefly_core::deep_time`**

Slider mapping, with `Y0 = SLIDER_OFFSET_YEARS` and `YM = MAX_YEARS_BP`. Clamp `s` to [0, 1] and `y` to [0, YM] first:

```
years(s) = Y0 * ((YM + Y0) / Y0)^s - Y0
s(y)     = ln((y + Y0) / Y0) / ln((YM + Y0) / Y0)
```

`STANDARD_SNAPSHOTS = [0, 100, 500, 1e3, 2e3, 5e3, 1e4, 2.1e4, 5e4, 1e5, 1e6, 5e6, 1e7, 2e7, 3.5e7, 5e7, 6.6e7, 1e8, 1.5e8, 2e8, 2.5e8, 3e8, 4e8, 5.4e8]`.

Label rules:
- `< 0.5` → "Present"
- `< 10_000` → rounded integer with comma thousands separator + " years ago"
- `< 1e6` → `round(y/1e3)` + " thousand years ago"
- `< 1e7` → one decimal of `y/1e6` with a trailing ".0" trimmed + " million years ago"
- otherwise → `round(y/1e6)` + " million years ago"

- [ ] **Step 5: Run the tests**

Run: `cargo test -p timefly_core`
Expected: 7 passed.

The reviewed row mapping and cultural import stay in Milestone 5. Copy the source CSV unchanged. The later importer uses the reference years above and the spec’s explicit row conventions. Its date tests include approximate 12,000 BP → 12,076 game years, 1 BCE → 2,026, and 1 CE → 2,025.

- [ ] **Step 6: Verify the game shell builds on both targets and the Python project runs**

`tools/bake/tests/test_smoke.py` imports `timefly_bake` and asserts `timefly_bake.__version__ == "0.1.0"`.

Run each command:
- `cargo build -p timefly` → succeeds
- `cargo check -p timefly --target wasm32-unknown-unknown` → succeeds (if Bevy needs an extra wasm feature such as `web`, add it to the wasm32 section and note why in a comment)
- `cd tools/bake && uv sync && uv run pytest -q` → `1 passed`
- `wasm-server-runner --version` → succeeds (installed by the owner)

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: scaffold workspace, bake tool and time model

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 1b: Build-disk hygiene

Added at the owner's request on 2026-11-22. The full brief lives in the execution workspace (`task-1b-brief.md`). It adds `tools/dev/target-guard.sh` (prunes `target/` when it exceeds 15 GB or free space drops below 5 GB, only inside `target/`), a test script, a project `.claude/settings.json` `PostToolUse` hook that runs the guard after cargo commands, and `docs/dev/build-disk.md`.

---

### Task 2: Diagnostics: tracing aspects, session logs and crash notice

Every later task is instrumented through what this task builds, so it comes before the domain code. Spec §20 is the requirement.

**Files:**
- Create: `crates/timefly/build.rs`, `crates/timefly/src/diagnostics/{mod,session,recorder,report,notice,aspects}.rs`, `crates/timefly/tests/crash_flow.rs`, `tools/bake/timefly_bake/log.py`, `tools/bake/tests/test_log.py`, `.github/ISSUE_TEMPLATE/crash_report.yml`
- Modify: root `Cargo.toml` (release profile), `crates/timefly/{Cargo.toml,src/main.rs,src/lib.rs}`, `crates/timefly_core/Cargo.toml`

**Dependencies** (checked on crates.io and in the Bevy 0.20 source, 2026-11-22):
- `bevy` gains feature `trace` behind a crate feature `system-spans` (default on). Task 10 measures its cost.
- Use `bevy::log::tracing_subscriber` (re-exported by `bevy_log`) instead of a separate `tracing-subscriber` dependency, so the versions can't drift. Add `tracing = "0.1"` and `tracing-appender = "0.2"`.
- Native only: `directories = "6"`, `fd-lock = "4"`, `opener = { version = "0.9", features = ["reveal"] }`, `rfd = "0.18"`.
- wasm32 only: `wasm-bindgen = "0.2"` and `web-sys` features `Window`, `Document`, `Element`, `HtmlElement`, `HtmlAnchorElement`, `Storage`, `Location`, `Blob`, `BlobPropertyBag`, `Url` (`console_error_panic_hook` is already present).
- `timefly_core`: `tracing = "0.1"`, for `#[instrument]` attributes only.
- Root release profile: `debug = "line-tables-only"`, so backtraces carry file and line.

**Pure parts** (no I/O; unit tested):
- `SessionRecord { id, started_utc, build: BuildInfo, platform: PlatformInfo, status: Running | Clean | Crashed { panicked: bool }, reached_first_frame: bool, acknowledged: bool }`, serialized as JSON.
- `previous_outcome(record: Option<&SessionRecord>, lock_free: bool, has_crash_report: bool) -> PreviousOutcome` returns `Nothing`, `Clean`, `OtherInstanceRunning` or `Crashed { during_startup: bool, has_crash_report: bool }`. `Running` with a held lock is another instance, not a crash. An acknowledged crash returns `Nothing`.
- `redact(text, home) -> String` replaces the home directory (raw and canonical forms) with `~`.
- `Recorder`: a ring of formatted events capped at 4,096 events or 2 MiB, dropping the oldest first. `snapshot() -> Vec<String>`.
- `format_crash_report(panic, build, platform, context, recent) -> String`.
- `BuildInfo` from `build.rs`: package version, git short SHA (`TIMEFLY_GIT_SHA` when CI sets it, else `git rev-parse --short HEAD`, else `unknown`), profile, target triple and enabled features.

**Shell:**
- `diagnostics::start(args) -> Session` runs first in `main`, before the Bevy `App` exists:
  1. Resolve the log root. macOS: `~/Library/Logs/Time Fly/`. Linux: `$XDG_STATE_HOME/time-fly/logs/` (default `~/.local/state`). Windows: `%LOCALAPPDATA%\Time Fly\logs\`. `TIMEFLY_LOG_DIR` overrides it for tests and CI. The web uses a `timefly.` namespace in `localStorage`.
  2. Read the newest previous session, try its lock, and compute `previous_outcome`.
  3. Create `<root>/<utc timestamp>_<short id>/`. Hold an `fd-lock` write lock on its `session.lock` for the life of the process, write `session.json` as `Running`, and prune to the 10 newest session directories (never the current one).
  4. Install the panic hook. Chain the previous hook, and on wasm also call `console_error_panic_hook::hook`. The hook writes `crash.txt` synchronously with `format_crash_report`, then marks the session `Crashed { panicked: true }`.
- `DiagnosticsPlugin` supplies `LogPlugin::custom_layer` (verified in Bevy 0.20) with two layers:
  - **Session file:** `tracing-appender` non-blocking with `lossy(false)`, through a redacting writer. Levels: `info` for `timefly*`, `warn` for dependencies; `TIMEFLY_LOG` overrides them. `Session` keeps the `WorkerGuard`; dropping it at exit flushes the file.
  - **Flight recorder:** `debug` for `timefly*`, `info` otherwise. At the first `ERROR` event (then at most once a minute) it appends its contents to the session log under a `--- context before error ---` marker.
- **Diagnostics context:** a mutex-guarded `DiagnosticsContext` (game age, camera distance and direction, load state, frame selection, adapter and backend, window size). A system updates it only when a value changes, and the panic hook reads it.
- **Aspects** (`aspects.rs`): `App::add_domain_event::<E: Message + Debug>()` registers the message plus a system that logs each one at `info`. `App::add_logged_state::<S: States + Debug>()` logs every transition. Later tasks register events and states only through these.
- **Clean exit:** `main` runs `timefly::app(&session).run()`, then `session.finish()`, which marks the session `Clean` and drops the guard. A system sets `reached_first_frame` on the first frame after the window opens; Task 9 moves this to the first interactive globe frame. On the web, `pagehide` marks the session `Clean` and saves the recorder. While running, a 2 s timer saves the recorder (capped at 1 MiB; keep the newest 3 web sessions).
- **Notice** (`notice.rs`), when `previous_outcome` is `Crashed`:
  - Always log a `warn` with the previous log path, and print it to stderr.
  - Native, crash before the first frame: show an `rfd::MessageDialog` before the `App` starts, with buttons **Show log** (`opener::reveal`) and **Not now**. If the dialog can't open (for example a Linux desktop without a portal), fall through to the in-game notice.
  - Native, otherwise: an in-game overlay after the first frame with the text, the log path, and buttons **Show log** (`opener::reveal`), **Report problem** (`opener::open(REPORT_URL)`) and **Dismiss**.
  - Web: an HTML overlay built with `web-sys` before the `App` starts, so it doesn't depend on WebGPU. Buttons: **Download log** (a Blob URL named `timefly-<session id>.log`), **Report problem** and **Dismiss**.
  - Any button marks the previous session `acknowledged`.
  - Text: "Time Fly didn't close normally last time. A log of that session is saved at <path>. Please send it to the developer so the problem can be fixed. It contains game events and your OS and graphics card, and no personal information."
  - `REPORT_URL` is one constant (the repository is public): `https://github.com/adsouza/time-fly/issues/new?template=crash_report.yml&title=Crash%20report%20<session-id>`. A pure `report_url(session_id, platform_summary) -> String` also appends `&session_id=<id>&platform=<summary>`; issue forms fill a field from a query parameter named after its `id`. Percent-encode every value.
- **Issue form** (`.github/ISSUE_TEMPLATE/crash_report.yml`, about 20 lines): `name: Crash report`, `labels: [crash]`, and these fields:
  - `session_id`: input, required.
  - `what_happened`: textarea, "What were you doing when it happened?"
  - `platform`: input, "OS and graphics card".
  - `log`: textarea. "Drag the log file from **Show log** or **Download log** into this box." GitHub accepts drag-and-drop attachments in textareas.

  GitHub applies the `crash` label only if it already exists. Ask the owner to create it, or run `gh label create crash` with their OK.
- **Probe flag:** a hidden `--diagnostics-probe=<mode>` (web: `?diagnostics-probe=<mode>`) starts diagnostics and logs a few events, then:
  - `panic`: panics before opening a window.
  - `panic-after-frame`: opens the window, renders a frame, then panics.
  - `abort`: calls `std::process::abort()`.
  - `error`: logs an error and exits cleanly.
  - `clean`: exits cleanly.
  Tests and the manual checks use it. Window-less probes run a headless `App` (`MinimalPlugins` plus `LogPlugin` with the same custom layer), so they exercise the real subscriber.
- **One subscriber.** Bevy's `LogPlugin` installs the global subscriber, so nothing else may set one. `start` returns its findings (previous outcome, pruned sessions, paths) and writes only to stderr. `DiagnosticsPlugin` logs those findings once the subscriber exists.

**Python (`timefly_bake/log.py`):**
- `configure(out_dir: Path | None, level="INFO")`: human-readable stderr, plus `bake.log` in `out_dir` at DEBUG. Task 7's CLI calls it.
- `@traced`: logs entry with summarized arguments (arrays as shape and dtype, paths redacted, long strings cut), exit with duration and a result summary, and exceptions with traceback before re-raising. Preserves `functools.wraps` metadata and sets `__traced__ = True`.

- [ ] **Step 1: Write the failing Rust unit tests** for the pure parts:
  - the `previous_outcome` truth table: no record; clean; running with a free lock; running with a held lock; crashed with and without a report; before vs after the first frame; acknowledged.
  - `redact` with Unix and Windows-style homes, and text without the home left unchanged.
  - the recorder's count and byte caps, and its ordering.
  - `format_crash_report` containing the message, location, build, context and recent events.
  - `report_url` percent-encoding a session ID and a platform summary that contains spaces and parentheses.
  - log-root resolution per OS, with and without `TIMEFLY_LOG_DIR`.
- [ ] **Step 2: Write the failing process tests** in `crates/timefly/tests/crash_flow.rs`. Each runs the built binary (`env!("CARGO_BIN_EXE_timefly")`) with a temporary `TIMEFLY_LOG_DIR`, and none opens a window:
  - `panic_is_reported_next_run`: probe `panic`, then compute the outcome in the test. Expect `Crashed { during_startup: true, has_crash_report: true }`, and a `crash.txt` holding the panic message and the probe's recent events.
  - `abort_is_detected_without_report`: probe `abort`. Expect `Crashed { has_crash_report: false }`, with the probe's events in the session log.
  - `clean_exit_is_not_a_crash`: probe `clean`. Expect `Clean`.
  - `error_dumps_flight_recorder`: probe `error`. The session log holds the context marker followed by the debug events that preceded the error.
  - `logs_are_redacted`: the probe logs a path under the home directory; no log file contains the literal home path.
  - `prunes_to_ten_sessions`: twelve clean probes leave ten session directories.
- [ ] **Step 3: Write the failing Python tests** in `tests/test_log.py`:
  - entry, exit and duration records;
  - an ndarray summarized as shape and dtype, never values;
  - an exception logged with traceback and re-raised;
  - the return value unchanged;
  - `test_every_public_function_is_traced`: import every `timefly_bake` module and assert that each public function defined there has `__traced__` (excluding `log` itself). It keeps enforcing coverage as later tasks add modules.
- [ ] **Step 4: Run the focused checks.** Expected: fail.
- [ ] **Step 5: Implement** the modules. Wire `diagnostics::start` into `main.rs` and `DiagnosticsPlugin` into `app()`.
- [ ] **Step 6: Run the focused checks and the wasm build check.** Expected: pass.
- [ ] **Step 7: Check the notices by hand on this Mac.**
  - Run `cargo run -p timefly -- --diagnostics-probe=panic`, then `cargo run -p timefly`. Expect the system dialog, and **Show log** revealing `crash.txt` in Finder.
  - Repeat with `panic-after-frame`. Expect the in-game overlay. After any button, the next launch shows no notice.
  - Repeat with `abort`. Expect a notice pointing at the session log.
  - Web: open the `wasm-server-runner` URL with `?diagnostics-probe=panic`, then reload without it. Expect the HTML overlay, and **Download log** saving the file.
  - Click **Report problem** in both the native and web notices. Expect the crash-report form to open with the title, session ID and platform filled in. The form only appears on GitHub once this task's commit is pushed, so do this part after Task 11's first push, or check it against a pushed branch with the owner's OK.
- [ ] **Step 8: Commit** — `feat: diagnostics, session logs and crash notice`.

---

### Task 3: Coordinate frames and the cube-map convention (Rust + Python)

**Files:**
- Create: `crates/timefly_core/src/geo.rs`, `crates/timefly_core/src/cube.rs`, `crates/timefly_core/src/geodynamics/mod.rs`, `tools/bake/timefly_bake/geo.py`, `tools/bake/timefly_bake/cube.py`
- Test: unit tests in both Rust files; `tools/bake/tests/test_geo_cube.py`

**Interfaces:**
- Produces (Rust, `timefly_core::geo`; `DVec3`/`DQuat` from `bevy_math`):
  - Frame-tagged value objects: `pub struct GeoDir(pub DVec3)` and `pub struct RenderDir(pub DVec3)` (`Copy`, `Debug`, `PartialEq`). Mixing frames is a type error; unwrap with `.0` only for arithmetic inside one frame.
  - `latlon_to_geo(lat_deg: f64, lon_deg: f64) -> GeoDir`, `geo_to_latlon(v: GeoDir) -> (f64, f64)` (lon in (-180, 180])
  - `geo_to_render(v: GeoDir) -> RenderDir`, `render_to_geo(v: RenderDir) -> GeoDir`, `geo_quat_to_render(q: DQuat) -> DQuat`
- Produces (Rust, `timefly_core::geodynamics`): `pub struct PlateId(pub u32)` (stable source ID), `pub struct PlateIndex(pub u32)` (dense, per bake), and `pub struct CrustPos { pub plate_id: PlateId, pub lat_deg: f64, pub lon_deg: f64 }` (plate-fixed frame). A plate ID is stable across bakes. Add forward and inverse lookups in Task 5. Canonicalize longitude to `(-180°, 180°]`, using zero longitude at the poles.
- Produces (Rust, `timefly_core::cube`):
  - `#[repr(u8)] pub enum CubeFace { PosX = 0, NegX, PosY, NegY, PosZ, NegZ }` with `CubeFace::ALL: [CubeFace; 6]`
  - `texel_center_dir(face: CubeFace, col: u32, row: u32, n: u32) -> RenderDir` (normalized)
  - `dir_to_texel(dir: RenderDir, n: u32) -> (CubeFace, u32, u32)` (returns face, col, row)
- Produces (Python): `geo.latlon_to_geo(lat, lon) -> ndarray[...,3]`, `geo.geo_to_latlon(v) -> (lat, lon)`, `geo.render_to_geo(v)`, `geo.geo_to_render(v)`, `geo.quat_rotate(q_xyzw, v)` (all vectorised numpy); `cube.texel_center_dirs(n) -> ndarray (6, n, n, 3)` indexed `[face, row, col]`, render frame; `cube.dirs_to_texels(dirs, n) -> (face, col, row)` int arrays (same order as Rust's `dir_to_texel`; index numpy arrays as `arr[face, row, col]`).

**The convention** (the standard cube-map table, shared by WebGPU, Vulkan and D3D). For texel `(col, row)` on an `n×n` face, `sc = 2*(col+0.5)/n - 1` and `tc = 2*(row+0.5)/n - 1`. Row 0 is the top of the face. The unnormalized direction is:

| face | direction |
|---|---|
| +X | ( 1, -tc, -sc) |
| -X | (-1, -tc,  sc) |
| +Y | ( sc,  1,  tc) |
| -Y | ( sc, -1, -tc) |
| +Z | ( sc, -tc,  1) |
| -Z | (-sc, -tc, -1) |

The inverse picks the major axis (`|x| >= |y| && |x| >= |z|` → X, else `|y| >= |z|` → Y, else Z), inverts the table row for that face, then sets `col = clamp(floor((sc+1)/2*n), 0, n-1)` and the same for `row` from `tc`.

- [ ] **Step 1: Write the failing tests (Rust)**

```rust
// cube.rs tests
#[test] fn golden_texel_dirs() {
    let r = 1.0 / 1.5f64.sqrt();
    let d = texel_center_dir(CubeFace::PosZ, 0, 0, 2);
    assert!((d.0 - DVec3::new(-0.5 * r, 0.5 * r, r)).length() < 1e-12);
    let d = texel_center_dir(CubeFace::PosX, 1, 0, 2);
    assert!((d.0 - DVec3::new(r, 0.5 * r, -0.5 * r)).length() < 1e-12);
}
#[test] fn texel_round_trip_all_faces() {
    for f in CubeFace::ALL { for row in 0..8 { for col in 0..8 {
        assert_eq!(dir_to_texel(texel_center_dir(f, col, row, 8), 8), (f, col, row));
    }}}
}
#[test] fn dir_to_texel_handles_axes_and_edges() {
    assert_eq!(dir_to_texel(RenderDir(DVec3::Y), 4).0, CubeFace::PosY);
    assert_eq!(dir_to_texel(RenderDir(-DVec3::Z), 4).0, CubeFace::NegZ);
    let (_, c, r) = dir_to_texel(RenderDir(DVec3::new(1.0, 1.0, 1.0).normalize()), 4); // corner tie
    assert!(c < 4 && r < 4);
}
// geo.rs tests
#[test] fn latlon_frames() {
    assert!((geo_to_render(latlon_to_geo(0.0, 0.0)).0 - DVec3::Z).length() < 1e-12);   // lon 0 faces +Z
    assert!((geo_to_render(latlon_to_geo(0.0, 90.0)).0 - DVec3::X).length() < 1e-12);  // east is +X
    assert!((geo_to_render(latlon_to_geo(90.0, 0.0)).0 - DVec3::Y).length() < 1e-12);  // north is +Y
    let (lat, lon) = geo_to_latlon(latlon_to_geo(-33.9, 151.2));
    assert!((lat + 33.9).abs() < 1e-9 && (lon - 151.2).abs() < 1e-9);
}
#[test] fn quat_permutation_matches_vector_permutation() {
    let q = DQuat::from_axis_angle(DVec3::new(0.3, -0.5, 0.8).normalize(), 1.1);
    let v = latlon_to_geo(12.0, 34.0);
    assert!((geo_to_render(GeoDir(q * v.0)).0 - geo_quat_to_render(q) * geo_to_render(v).0).length() < 1e-12);
}
```

- [ ] **Step 2: Write the failing tests (Python)** in `tests/test_geo_cube.py`

Assert the same values: `texel_center_dirs(2)[4, 0, 0] ≈ (-0.408248290, 0.408248290, 0.816496581)` and `[0, 0, 1] ≈ (0.816496581, 0.408248290, -0.408248290)` (face index 4 = +Z, 0 = +X; index `[face, row, col]`). Also check a round trip `dirs_to_texels(texel_center_dirs(8), 8)` for all texels, and the three `latlon_frames` cases.

- [ ] **Step 3: Run both suites to confirm they fail**

Run: `cargo test -p timefly_core` and `cd tools/bake && uv run pytest -q`
Expected: failures (not defined).

- [ ] **Step 4: Implement `geo.rs`, `cube.rs`, `geo.py` and `cube.py`** using the convention table above.

- [ ] **Step 5: Run both suites**

Expected: all pass.

- [ ] **Step 6: Commit** — `feat: shared coordinate frames and cube-map convention` (with trailer).

---

### Task 4: Binary formats `.tfgrid` / `.tfrot` / `.tfplates` (Python writer, Rust reader)

**Files:**
- Create: `tools/bake/timefly_bake/formats.py`, `crates/timefly_core/src/formats.rs`, `crates/timefly_core/src/geodynamics/{rotation,crust}.rs` (bare data structs only), `tools/bake/tests/test_formats.py`, `tools/bake/scripts/write_fixtures.py`
- Create (generated, committed): `crates/timefly_core/tests/fixtures/{cube2_rg16f.tfgrid,rot_small.tfrot,footprints_small.tfplates}`
- Test: `crates/timefly_core/tests/formats_fixtures.rs`

**Layouts** (all little-endian; the payload is one zstd frame):

```
.tfgrid
  magic     8 bytes  b"TFGRID01"
  kind      u8       0 = cube, 1 = lat-lon
  dtype     u8       0 = f16, 1 = u16, 2 = f32
  channels  u8
  reserved  u8       0
  width     u32      cube: face edge n; lat-lon: columns
  height    u32      cube: n;          lat-lon: rows
  zlen      u32      compressed payload length
  payload   zstd( array[face][row][col][channel] )   faces in CubeFace order (+X,-X,+Y,-Y,+Z,-Z);
                                                       lat-lon: [row][col][channel]

.tfrot
  magic     8 bytes  b"TFROT002"
  n_plates  u32
  n_times   u32
  zlen      u32
  payload   zstd( plate_ids u32[n_plates],
                  times_myr f64[n_times] (ascending, starting at 0),
                  quats f32[n_times][n_plates][4] (render frame, x y z w),
                  valid u8[n_times][n_plates] (0 = missing circuit, 1 = valid) )

.tfplates
  magic     8 bytes  b"TFPLAT01"
  cube_size u32      face edge n
  n_entries u32
  zlen      u32      compressed payload length
  payload   zstd( offsets u32[6 * n * n + 1] (cube cell order [face][row][col]),
                  entries { plate_index: u32, younger_myr: f64, older_myr: f64 }[n_entries] )   packed, 20 bytes each
```

**Interfaces:**
- Produces (Python): `write_tfgrid(path, kind: str ("cube"|"latlon"), data: ndarray)`. The dtype is inferred from `data.dtype` (float16 / uint16 / float32). The shape is `(6, n, n, c)` for cube or `(h, w, c)` for lat-lon. Also `read_tfgrid(path) -> (dict, ndarray)`, `write_tfrot(path, plate_ids, times_myr, quats_render_xyzw, valid)` and `write_tfplates(path, cube_size, offsets, entries)`.
- Produces (Rust, `timefly_core::formats`):
  - `pub enum GridKind { Cube, LatLon }`, `pub enum DType { F16, U16, F32 }`
  - `pub struct Grid { pub kind: GridKind, pub dtype: DType, pub channels: u8, pub width: u32, pub height: u32, pub data: Vec<u8> }`, where `data` is the decompressed little-endian bytes
  - `impl Grid { pub fn texel_u16(&self, face: usize, row: u32, col: u32, ch: u8) -> u16; pub fn texel_f32(&self, face: usize, row: u32, col: u32, ch: u8) -> f32 }`. `texel_f32` converts f16 via `half`.
  - `pub enum FormatError { BadMagic, Truncated, UnknownKind(u8), UnknownDType(u8), SizeMismatch { expected: usize, got: usize }, Zstd(String) }` (implements `std::error::Error`)
  - `pub fn read_tfgrid(bytes: &[u8]) -> Result<Grid, FormatError>`
  - `pub fn read_tfrot(bytes: &[u8]) -> Result<RotationTable, FormatError>`. Define `pub struct RotationTable { pub plate_ids: Vec<PlateId>, pub times_myr: Vec<f64>, pub quats: Vec<[f32; 4]>, pub valid: Vec<bool> }` in `rotation.rs`; Task 5 adds methods. Store source IDs as `u32` and quaternions and validity in matching `[time][plate]` order.
  - `pub fn read_tfplates(bytes: &[u8]) -> Result<PlateFootprints, FormatError>`. Define `pub struct PlateFootprints { pub cube_size: u32, pub offsets: Vec<u32>, pub entries: Vec<FootprintEntry> }` and `pub struct FootprintEntry { pub plate_index: PlateIndex, pub younger_myr: f64, pub older_myr: f64 }` in `crust.rs`; Task 5 adds cell and plate lookups. Offsets begin at zero, end at `n_entries`, and never decrease. Each cell's entries are sorted by dense plate index, then by younger bound. Intervals are closed, within 0–540 Ma. Check index bounds against the rotation table when loading both files.

- [ ] **Step 1: Write the failing Python round-trip test**

Write a cube `(6, 2, 2, 2)` float16 array whose value at `[f, r, c, ch]` is `f*100 + r*10 + c + ch*0.5`, then read it back. Assert the header (`kind == "cube"`, `width == 2`, `channels == 2`) and that the data are equal. Write a `.tfrot` with `plate_ids = [101, 202, 303]`, `times = [0, 1]`, `quats[1, 2] = (0, 0.7071068, 0, 0.7071068)` and `valid[1, 1] = 0`. Write a 2×2 `.tfplates` fixture with an empty cell and a cell with two plate memberships.

- [ ] **Step 2: Run** `uv run pytest -q tests/test_formats.py`. Expected: fails.

- [ ] **Step 3: Implement `formats.py`.** Then write `scripts/write_fixtures.py` to write the three Step 1 files into `crates/timefly_core/tests/fixtures/` and run it.

- [ ] **Step 4: Write the failing Rust fixture tests** in `crates/timefly_core/tests/formats_fixtures.rs`

```rust
#[test] fn reads_python_cube_fixture() {
    let g = read_tfgrid(include_bytes!("fixtures/cube2_rg16f.tfgrid")).unwrap();
    assert_eq!((g.width, g.height, g.channels), (2, 2, 2));
    assert_eq!(g.texel_f32(4, 1, 0, 1), 410.5); // face 4, row 1, col 0, channel 1
}
#[test] fn reads_python_rot_fixture() {
    let t = read_tfrot(include_bytes!("fixtures/rot_small.tfrot")).unwrap();
    assert_eq!(t.plate_ids, vec![PlateId(101), PlateId(202), PlateId(303)]);
    assert_eq!(t.times_myr, vec![0.0, 1.0]);
    assert!((t.quats[1 * 3 + 2][1] - 0.7071068).abs() < 1e-6);
    assert!(!t.valid[1 * 3 + 1]);
}
#[test] fn rejects_bad_magic_and_truncation() {
    let good = include_bytes!("fixtures/cube2_rg16f.tfgrid");
    let mut bad = good.to_vec(); bad[0] = b'X';
    assert!(matches!(read_tfgrid(&bad), Err(FormatError::BadMagic)));
    assert!(matches!(read_tfgrid(&good[..good.len() - 3]), Err(FormatError::Truncated)));
    assert!(read_tfgrid(&good[..10]).is_err());
}
```

Also test both footprint memberships, rejecting invalid offsets, and rejecting validity bytes outside `0..=1`.

- [ ] **Step 5: Run** `cargo test -p timefly_core`. Expected: fails.
- [ ] **Step 6: Implement `formats.rs`** with `ruzstd::decoding::StreamingDecoder`. The three readers are coarse core entry points: give each `#[instrument(level = "debug", skip_all, fields(len = bytes.len()), err)]`. Validate sizes with checked arithmetic, cap decompression at the expected length, and reject mis-sized payloads, invalid time ranges, and non-finite values in active rotation records.
- [ ] **Step 7: Run both suites.** Expected: all pass.
- [ ] **Step 8: Commit** — `feat: globe binary formats with cross-language fixtures`.

---

### Task 5: Plate footprints, rotation validity and crust lookup

**Files:**
- Create: `tools/bake/timefly_bake/plates.py`
- Modify: `crates/timefly_core/src/geodynamics/{rotation,crust}.rs` (created in Task 4), `data/README.md`
- Test: `tools/bake/tests/test_plates.py`, unit tests in `rotation.rs` and `crust.rs`

**Data (ask the user before downloading):** use the rotation and geometry release linked from the [EarthByte PaleoDEM resource](https://www.earthbyte.org/paleodem-resource-scotese-and-wright-2018/). If using smaller standalone rotation and static-polygon archives, confirm they match the release and anchor used for the PaleoDEMs. Pin the download URLs, file names, checksums, citation, redistribution terms and reconstruction anchor in `data/README.md`.

**Interfaces:**

- Python `assign_plates(dirs_render, polygons_path) -> PlateFootprints` returns every containing polygon's source plate ID and valid interval per cube cell. Group memberships by plate, union overlapping intervals for that plate, clip to the modelled age range, and write candidate lists in stable sorted order. Keep different plates in the same cell and disjoint intervals for the same plate.
- Python `export_rotations(rotation_path, plate_ids, times_myr) -> (quats_render_xyzw, valid)` uses anchor plate 0 after checking alignment with the source. Call `RotationModel.get_rotation(..., use_identity_for_missing_plate_ids=False)` and mark `None` invalid. A returned identity quaternion can be valid. Export the 1 Myr samples from 0 to 540 Ma plus source reconstruction ages and rotation-circuit boundaries within that range. Preserve missing spans between samples, including a missing midpoint between valid samples.
- Rust `RotationTable::index_of(plate_id: PlateId) -> Option<PlateIndex>` maps a stable source ID to a dense index. `rotation_at(plate_index: PlateIndex, age_myr: f64) -> Option<Quat>` returns a valid exact sample or slerps between valid samples in one continuous span. A missing circuit or an out-of-range age yields `None`. Pass this validity through to GPU data for current and source-frame rotations.
- Rust `PlateFootprints::contains(plate_index: PlateIndex, plate_fixed_dir: RenderDir, age_myr: f64) -> bool` tests the cell's membership and interval.
- Rust aggregate `CrustModel::new(rotations: RotationTable, footprints: PlateFootprints) -> Result<CrustModel, CrustModelError>` checks that every footprint plate index exists in the rotation table. It is the only way to build a `CrustModel`. `CrustModel::forward(pos: CrustPos, age_myr: f64) -> Option<RenderDir>` requires both an eligible rotation and footprint.
- Rust `CrustModel::resolve(era_dir: RenderDir, age_myr: f64, surface_for_candidate) -> ResolvedPosition` inverse-rotates the direction for each valid plate, tests the resulting cell, and ranks candidates. `surface_for_candidate` supplies the visible land elevation from the same frame selection and sampling rules used by the globe. Pick the highest visible land, then the lowest source ID at equal elevation. If no candidate has visible land, pick the lowest valid source ID. With no valid candidate, return `EraFixed { age, lat_deg, lon_deg }`. Otherwise return `Tracked(CrustPos)`. Use a stable iteration order. For the fixed recent globe, both lookup directions use age-0 rotations for the orientation and the requested age for validity. In that path, the sampled geographic surface is common to all candidates.
- For later dives, snap to the child world's target snapshot before resolving the direction. On later time changes, use the stored `CrustPos` and forward lookup. An era-fixed location or a failed forward lookup returns the traveller to the globe. All region and local generators use this same resolver and a canonical lattice for the terrain seed at each sample. M1 provides the contract and tests without adding descent.

- [ ] **Step 1: Write the failing Rust tests.** Cover `CrustModel::new` rejecting a footprint index missing from the rotation table, known 90° rotation and midpoint slerp, exact valid identity, no interpolation across a missing span, forward/inverse round trip (including the antimeridian), selection by visible height with both overlap tie rules, an expired footprint, an era-fixed gap, and identical resolved sample keys from overlapping world windows. Include a cell with a gap between two valid intervals.
- [ ] **Step 2: Write the failing Python tests.** Use two overlapping synthetic polygons with different valid intervals and one plate with a missing circuit. Assert both memberships survive the bake, a genuine zero rotation is valid, and a missing circuit remains missing. Include a north-pole rotation with a known nonzero angle and verify the geographic-to-render quaternion permutation.
- [ ] **Step 3: Run the focused Rust and Python checks.** Expected: fail.
- [ ] **Step 4: Implement the footprints and lookup.** Use `pygplates.FeatureCollection` and polygon containment. Prune by polygon bounds before point-in-polygon checks. Bake the footprint index at the same cube resolution and with the same face convention as Tasks 3 and 4. Keep the CPU and GPU footprint acceptance rules equivalent.
- [ ] **Step 5: Run the focused checks and wasm build check.** Expected: pass.
- [ ] **Step 6: Commit** — `feat: preserve plate validity and resolve crust positions`.

---

### Task 6: PaleoDEM geographic frames and age metadata

**Files:**
- Create: `tools/bake/timefly_bake/paleodem.py`
- Modify: `data/README.md`
- Test: `tools/bake/tests/test_paleodem.py`

**Data (ask the user before downloading):** the 1° PaleoDEM netCDF [archive listed by EarthByte](https://www.earthbyte.org/paleodem-resource-scotese-and-wright-2018/) (listed at 8.9 MB). Citation: Scotese, C.R. & Wright, N. (2018), *PALEOMAP Paleodigital Elevation Models (PaleoDEMs) for the Phanerozoic*, PALEOMAP Project ([Zenodo record](https://zenodo.org/records/5460860)). Inspect the netCDF schema and use [Table 1 of the source report](https://www.earthbyte.org/webdav/ftp/Data_Collections/Scotese_Wright_2018_PaleoDEM/Scotese_Wright2018_PALEOMAP_PaleoDEMs.pdf) to identify the reconstruction age associated with each chosen frame. Record the verified file/age mapping in `data/README.md` or its referenced data table.

**Interfaces:**
- `LatLonGrid` holds ascending latitudes, periodic longitudes and a float32 sample grid. `load_paleodem(path)` reads it and normalizes axis order. `sample_bilinear(grid, lat, lon)` wraps longitude, clamps latitude, and handles any duplicate seam column.
- `list_paleodem_files(directory, age_mapping)` returns strictly ascending timeline ages with a file path and `source_rotation_age_myr` for each. Verify a present-day source and use the terminal PaleoDEM reconstructed at 540 Ma for the 540 Ma slider stop. Retain the original age labels in the source mapping and record any difference between the timeline age and the reconstruction age. Use a filename age only after checking what it encodes.
- `bake_paleo_frame(dirs_render, grid) -> ndarray (6, n, n, 2)` samples the full geographic grid at each cube texel centre. Channel R is signed elevation in metres. G was reserved at zero here; Task 9 (fix round 2, ruling R13) bakes it as the plate-coverage mask: 1 where a plate footprint valid at the frame's source reconstruction age covers that texel, else 0. Keep geographic coverage intact, including any areas that the plate model cannot track.
- At runtime a plate sample direction `p` becomes `rotation_at(plate, source_rotation_age_myr) * p` for each endpoint. An endpoint requires both a valid rotation and a footprint at that source reconstruction age. Blend when both endpoints are valid. Use the valid endpoint when only one qualifies. When neither qualifies, expose the ocean shell at that sample. Independently require the piece's current age rotation and footprint before it draws.

- [ ] **Step 1: Write the failing tests.**
  - `bilinear_wraps_longitude_and_clamps_latitude`: on a 3×4 grid (lat −60, 0, 60 and lon −135, −45, 45, 135), sampling at lon 180 gives the mean of the two edge columns. Sampling at lat 89 gives the 60° row.
  - `geographic_cube_preserves_signed_elevation`: a synthetic coastline has values on both sides of sea level, including cells with no plate membership.
  - `source_age_and_timeline_age_are_distinct`: a source grid placed at 66 Ma on the slider can carry 65 Ma as its reconstruction age. Unmapped or duplicate timeline frames fail with the filename.
- [ ] **Step 2: Run** `uv run pytest -q tests/test_paleodem.py`. Expected: fail.
- [ ] **Step 3: Implement `paleodem.py`** with vectorized numpy and `netCDF4.Dataset`.
- [ ] **Step 4: Run the focused Python checks.** Expected: pass.
- [ ] **Step 5: Commit** — `feat: bake geographic PaleoDEM frames with source ages`.

---

### Task 7: Quaternary layer (ICE-6G_C + sea level), manifest and bake CLI

**Files:**
- Create: `tools/bake/timefly_bake/{ice6g,sealevel,cli}.py`
- Modify: `data/README.md`
- Test: `tools/bake/tests/{test_quaternary,test_cli}.py`

**Data (ask the user before downloading):**
- **ICE-6G_C (VM5a):** take the 27 integer-ka slices (0–26 ka) from W.R. Peltier's 1° files, `https://www.atmosp.physics.utoronto.ca/~peltier/datasets/Ice6G_C_VM5a/I6_C.VM5a_1deg.<k>.nc.gz`, k = 0…26 (15.7 MB in total). *(Owner decision 2026-11-22, ruling R4: chosen over the PMIP4 10-arcminute series, ~1 GB, because a 128-texel cube face is ~0.7° per texel anyway.)* The source publishes no checksums: record each file's SHA-256 when it is downloaded and pin it in `data/sources.json` (Task 11), which the fetcher verifies. Inspect the netCDF attributes to confirm variable names, axes and elevation datum. Use signed `Topo` for relief and bathymetry, `sftlf` for land and `sftgif` for ice. Read masks as percentages and normalize to 0–1. Cite the two papers the files' `Acknowledgement` attribute asks for. The files state no licence; the owner approved distributing the baked derivatives with citation and credit (2026-11-22). Confirm the source BP reference or record the 1950 assumption.
- **Sea level, source 0–798 ka BP:** use Spratt & Lisiecki (2016) from [NOAA NCEI](https://www.ncei.noaa.gov/access/metadata/landing-page/bin/iso?id=noaa-ocean-19982), file `spratt2016-noaa.txt`. Use calendar ka BP and the long-PC1 sea-level anomaly in metres. Apply the game reference offset to these dates as well.

**Frame list and bridge:**

| Source | Timeline ages (Myr) | Channels | Sea-level treatment |
|---|---|---|---|
| `ice6g` | 0 (reuse youngest source), then 0.000076 … 0.026076 | R = signed `Topo` (m), G = land coverage, B = ice coverage, A = reserved | `Topo` is relative to the slice's sea level, so no extra offset |
| `paleodem` | 0.030 (present-day source, reconstruction age 0), then the selected older source timeline ages (check Table 1 for any source between 30 ka and 5 Ma) | R = signed elevation (m), G = plate-coverage mask (Task 9) | Use the sea-level curve, as `elev − anomaly`, only for the modern-terrain approximation starting at 0.030 Myr |

For the recent globe, blend signed elevation and both coverage masks linearly. Use land coverage for the land/water mix and ice coverage for a separate ice overlay. Grounded ice already contributes to `Topo`. Floating ice stays at sea level over bathymetry. Zero elevation alone has no effect on land classification. Between the final ICE-6G_C frame (26.076 ka in production) and the PaleoDEM bridge frame (30 ka in production), hold the final recent frame and crossfade its rendered globe into the deep-time result at the bridge frame. Read both bounds from the manifest; never hard-code them. From the bridge frame to the first older PaleoDEM, blend the bridge frame toward that older frame. Apply the sea-level curve only to the bridge sample, as `elev − anomaly`, before blending. After the curve's oldest knot, taper its last offset linearly to zero at the first older PaleoDEM. Published older snapshots retain their own sea level. Document the loss of reconstructed ice outside the ICE-6G_C coverage.

**Manifest** `manifest.json`:

```json
{ "version": 2, "cube_size": 128,
  "frames": [
    { "age_myr": 0.0, "file": "frames/f0000.tfgrid", "source": "ice6g" },
    { "age_myr": 0.000076, "file": "frames/f0000.tfgrid", "source": "ice6g" },
    { "age_myr": 0.030, "file": "frames/f0027.tfgrid", "source": "paleodem", "source_rotation_age_myr": 0.0 }
  ],
  "footprints": "plates.tfplates",
  "rotations": "rotations.tfrot",
  "sea_level_curve": [[0.000076, -1.5], [0.001076, -1.8]],
  "attribution": ["…one line per dataset…"] }
```

The example omits intermediate frames. Files are numbered by first occurrence in age order. Reuse the age-zero ICE-6G_C image for the two youngest timeline entries. The rotation table includes every required source rotation age and covers 0–540 Myr.

**Interfaces:**
- `ice6g.load_ice6g(path)` returns the three normalized source grids. `ice6g.bake_ice_frame(dirs_render, topo, land, ice)` returns a geographic `(6, n, n, 4)` float16 array.
- `sealevel.load_spratt2016(path)` returns ascending game ages and sea-level anomalies. Keep the reference-year conversion consistent with Task 1.
- `cli.main(argv=None)` accepts `--raw DIR --out DIR [--cube-size 128] [--log-level LEVEL]`, calls `log.configure(out)`, and writes the manifest, textures, footprints, rotation/validity table and `bake.log`.

- [ ] **Step 1: Write the failing tests.**
  - `ice_frame_preserves_ocean_shelf_and_floating_ice`: assert signed depth and zero land coverage over open water, positive topography and land coverage on an exposed shelf, nonzero ice coverage over otherwise open ocean for floating ice, and grounded ice on land. Add a zero-elevation open-ocean case and a mask blend between two frames.
  - `sealevel_parses_and_sorts`: a text fixture parses to ascending timeline ages, adding exactly 76 years to the raw BP dates.
  - `cli_builds_manifest_from_tiny_inputs`: generate and commit a tiny raw tree and its generator under `tools/bake/tests/fixtures/`. Include two synthetic plates, PaleoDEMs at 0 and 540 Ma, an ICE-6G_C grid at 0 ka, and a sea-level file. The synthetic raw tree provides an explicit age mapping. Assert the tiny manifest has ages `[0.0, 0.000076, 0.030, 540.0]`, three unique image files, source reconstruction ages for the PaleoDEMs, and valid references across the formats.
- [ ] **Step 2: Run the focused Python checks.** Expected: fail.
- [ ] **Step 3: Implement** the modules and CLI. Print progress per frame and report footprint and rotation coverage.
- [ ] **Step 4: Run the focused checks.** Expected: pass.
- [ ] **Step 5: Run the real bake** after the downloads are approved and verified.

  Run `cd tools/bake && uv run timefly-bake --raw ../../data/raw --out ../../crates/timefly/assets/data/globe`. Verify all expected integer-ka slices and a deep-time endpoint at 540 Ma. Record raw inputs, output frame counts and sizes, and the compressed transfer estimate in `data/README.md`. Keep the ~20 MB data asset target and carry the actual full-bake size into Task 10.
- [ ] **Step 6: Commit** — `feat: quaternary layer, globe manifest and bake CLI`. Baked production output is git-ignored.

---

### Task 8: Runtime data loading, time state and frame selection

**Files:**
- Create: `crates/timefly_core/src/surface/{mod,timeline}.rs`, `crates/timefly/src/{data,time}.rs`
- Modify: `crates/timefly/src/lib.rs` to register `TimePlugin` and `GlobeDataPlugin`
- Test: unit tests in `timefly_core/src/surface/timeline.rs` (pure frame selection) and `time.rs`, and `crates/timefly/tests/data_loading.rs`

**Interfaces:**
- `GlobeTime { years: YearsBp }` defaults to present. `TimePlugin` steps to the next older snapshot on `[` and the next younger snapshot on `]`. The step decision is the pure `next_snapshot` / `prev_snapshot`; the system only reads keys and applies it. Every change emits the domain event `TimeChanged { from: YearsBp, to: YearsBp, cause: TimeChangeCause }` (`Key`, `Slider`, `Bench`), registered with `add_domain_event`.
- Core (`timefly_core::surface`): `FrameMeta { age_myr: f64, source: FrameSource, image_slot: u32 }` with `FrameSource::Ice6g` or `FrameSource::PaleoDem { source_rotation_age_myr: f64 }`. The aggregate `Timeline::new(frames, sea_level_curve) -> Result<Timeline, TimelineError>` validates ascending ages, source order (every `ice6g` frame before every `paleodem` frame), source rotation ages, and the presence of a bridge. `bracket` and `select_frames` below are pure and return frame indices; they never see image handles.
- Shell: `GlobeData` holds the cube size, the `Timeline`, `images: Vec<Handle<Image>>` indexed by `image_slot`, the `CrustModel`, and attribution.
- `bracket(ages_myr, age_myr)` returns the two nearest bracketing indices and the weight of the older frame, clamping endpoints. Exact times may use the same index for both slots.
- `Timeline::select_frames(&self, age_myr) -> FrameSelection` returns `Recent` with an ICE-6G_C bracket, `Deep` with a PaleoDEM bracket, or `Bridge` containing the final recent frame, first deep result and crossfade weight. The bridge spans from the last `ice6g` frame age to the first `paleodem` frame age, both read from the manifest. At the bridge endpoints the output equals the adjacent path. For the deep bracket, include the offset for the modern-terrain endpoint as described in Task 7. Apply that offset to its signed elevation before any valid endpoint blend. Keep the offsets for older PaleoDEMs at zero.
- `GlobeDataPlugin` loads manifest v2, builds the `Timeline` and `CrustModel` through their validating constructors and checks cube sizes. Any validation error enters `Failed` with the error in the log. The `.tfgrid` image loader maps two f16 channels to `Rg16Float` and four to `Rgba16Float`, using six array layers and `TextureViewDimension::Cube`. Resolve repeated paths to one image handle and release the decoded CPU image bytes after successful upload where Bevy permits. Retain small structured footprint and rotation data for CPU queries.
- `GlobeLoadState` has `Loading`, `Ready` and `Failed`, registered with `add_logged_state`. `Ready` allows the render plugins to construct the globe after CPU assets have loaded. The loading overlay remains until the renderer confirms that the required GPU assets and globe pipelines are ready. Report failed assets and shader initialization errors with a useful diagnostic. CPU-only tests exercise the asset-load state.

- [ ] **Step 1: Write the failing unit tests.** In `bracket_clamps_at_ends`, cover times at, before and after both extremes. Also cover exact age and interior interpolation, recent-frame selection at 0 and at both manifest-derived bridge endpoints (26.076 and 30 ka for a production-shaped synthetic frame list; 0.000076 and 0.030 Myr for the tiny fixture's list), intermediate bridge weights, applying the sea-level offset to the modern-terrain endpoint only, `sea_level_offset_exposes_shelf` (a −100 m bridge sample with a −120 m anomaly yields +20 m), and a continuous taper to zero at the first older PaleoDEM. Test keyboard steps by updating an app with `TimePlugin`.
- [ ] **Step 2: Write the failing tests in `tests/data_loading.rs`.** Use a small asset-test app with the asset and state plugins needed by `GlobeDataPlugin` and the committed tiny bake from Step 4. Use a bounded time limit for asynchronous loading.
  - `loads_tiny_bake_to_ready`: four timeline entries, three image handles, matching geographic cube and footprint sizes, valid plate data.
  - `failed_load_enters_failed_state`: point the manifest in a temporary asset directory at a missing file and check the failure path. Also test a footprint index outside the rotation table and a mismatched image format.
  - `scrubbing_reuses_preloaded_images`: after loading, make 50 frame selections through the complete fixture range. Confirm every selected handle was preloaded and the selection does not create images. This is a functional check. Task 10 measures actual runtime cost.
- [ ] **Step 3: Run the focused app checks.** Expected: fail.
- [ ] **Step 4: Implement** the runtime loaders and resource interfaces. Generate and commit the tiny bake: `cd tools/bake && uv run timefly-bake --raw tests/fixtures/raw_tiny --out ../../crates/timefly/tests/assets/data/globe --cube-size 8`. Keep the fixture small and independent of the production download.
- [ ] **Step 5: Run the focused checks and wasm build check.** Expected: pass.
- [ ] **Step 6: Commit** — `feat: load globe data and select frames by source`.

---

### Task 9: Globe pieces, source sampling and drift rendering

**Files:**
- Create: `crates/timefly/src/globe/{mod,mesh,material}.rs`, `crates/timefly/assets/shaders/{globe,atmosphere}.wesl`
- Modify: `crates/timefly/src/lib.rs`
- Test: `mesh.rs` and `material.rs`, a two-plate render fixture, a real-bake coastline diagnostic

**Mesh and overlap rules:**
- Use `EARTH_RADIUS_KM = 6371.0` and initial elevation exaggeration `20.0` *(call)*. Build the fixed geographic sphere and ocean shell as cube-spheres. Use the footprint grid as the starting tessellation for plate pieces.
- Build per-plate geometry from the footprint cells. Emit each occupied cell once per candidate plate, with shared vertices where plate and validity intervals agree. Duplicate vertices at plate and interval boundaries. Both triangles of a cell carry flat attributes for the same dense plate index and validity interval list. Use identical corner directions across cube faces and neighboring cells so adjacent active cells within a plate meet.
- Hide a primitive outside its footprint interval or without a current rotation. Apply the source-frame validity checks in Task 6. At coincident corners of active cells on the same plate, use one canonical corner direction and one endpoint availability decision so displacement agrees even when incident cells have different valid intervals. Use the footprint cell containing the sample under the Task 3 cube tie rule to make that decision.
- Draw a continuous ocean shell beneath the deep-time land to cover separation gaps and unsupported terrain. For ocean tint, sample geographic bathymetry from the source grids and clamp land-valued gap samples to an ocean fallback. Render valid land pieces using depth, with stable source plate ID precedence for ties. Guarantee stable ordering in the draw setup and test it. Suppress geometry for below-sea-level samples so they expose the ocean. Align the CPU overlap resolver with the same surface ranking and ties.

**Materials and shader:**
- Use Bevy's WESL custom-material bind-group convention. Bind both frame textures, cube size, blend and per-endpoint sea-level settings, plus storage for the current and both source-frame rotations with valid flags (sea level applied as `elev − anomaly`, matching the CPU), and the interval lists. Use flat plate and interval attributes. Set resources only when the selected age changes.
- **Plate vertex stage:** compute the displaced position from the valid endpoint elevations at the unrotated plate-fixed sample, then apply the current rotation. **Fragment stage:** sample with the interpolated plate-fixed direction using the same endpoint selection, signed-height and footprint rules. Use the rotated normal and central differences within the valid footprint. Use the nearest valid sample or radial normal where the stencil crosses a plate border.
- **Recent path:** use the fixed geographic sphere and sample both recent textures directly. Displace positive topography and apply land and ice coverage as in Task 7. Open ocean remains water, grounded ice shades elevated land, and floating ice overlays water at sea level.
- **Bridge:** render the completed recent and deep globe views into separate targets during the short source transition and crossfade in linear color. Use the same camera and blend weights for both. Render the atmosphere once.
- **Painterly style:** colour water by depth from `#3f8fa0` to `#1f3f66` at −4000 m. Use the approved palette (B) for lowlands `#6f8f4c`, dry and mid elevations `#e2ad55`, highlands `#c7a36f`, peak haze `#b993b8` and ice `#eef4fa`. Blend hue-shifted shadows toward `#5b3a52` with a smooth diffuse ramp. Add low-amplitude brush noise. Use a day-to-terminator atmosphere rim (`#9cc3e6` to `#f4c58d`) above the terrain and fade it on the night side.
- **GlobePlugin:** spawn the globe, atmosphere and directional light on data readiness. Report the first interactive rendered frame to the loading UI and to diagnostics (`reached_first_frame`). Publish the adapter name and backend to the diagnostics context when the renderer initializes. Emit the domain event `FrameSelectionChanged` (registered with `add_domain_event`) when the selected frame indices or bridge state change. Keep the endpoint-selection and footprint-acceptance rules as pure functions in `timefly_core` that the shader mirrors, and compare the two in the fixture tests.

- [ ] **Step 1: Write the failing tests.** Assert valid mesh winding, unit directions, matching cube edges, one plate per triangle, and deterministic depth tie order. Use a small two-plate fixture occupying adjacent and overlapping cells with nonzero relative motion. Assert independent rigid motion during separation, a filled ocean gap, the expected overlap winner, and hidden expired or invalid geometry. Include a pair of neighboring valid cells on one plate whose source endpoint availability differs. Test current and source quaternion selection separately, including a PaleoDEM whose timeline and source ages differ.
- [ ] **Step 2: Run the focused app checks.** Expected: fail.
- [ ] **Step 3: Implement** the mesh, materials, shaders and `GlobePlugin`. Render the tiny fixture and verify its gap and overlap in the actual shader path.
- [ ] **Step 4: Run the focused app checks and wasm build check.** Expected: pass.
- [ ] **Step 5: Check the real bake against the source coasts.** In the published geographic frame of each selected PaleoDEM, compare a resulting land/water mask from the plate renderer to the source mask, including **present, about 66 Ma, about 250 Ma, and 540 Ma**. Use the 30 ka bridge's present-day PaleoDEM with zero diagnostic sea-level offset for the modern comparison. For older comparisons use each frame's source reconstruction age for both placement and sampling, then also check the normal slider-time output for that frame. Report missing land and added land separately by area, with a one-source-cell shoreline tolerance. Record model coverage and inspect any discrepancies beyond that tolerance before accepting the bake. Fix any missing major landmass, spurious land bridge, or systematic geographic offset before sign-off. Save comparable source and output views for review.
- [ ] **Step 6: Commit** — `feat: render rigid plate pieces and painterly globe`.

---

### Task 10: Time slider, orbit camera, status and credits UI

**Files:**
- Create: `crates/timefly/src/camera.rs`, `crates/timefly/src/bench.rs`, `crates/timefly/src/ui/{mod.rs,slider.rs,status.rs,credits.rs}`, `crates/timefly/assets/credits.txt`
- Modify: `crates/timefly/src/lib.rs`
- Test: unit tests in `slider.rs`, `credits.rs`, `bench.rs`

**Interfaces:**
- Consumes: `GlobeTime`, `slider_to_years`, `years_to_slider`, `format_years_bp`, `GlobeLoadState`, `EARTH_RADIUS_KM`, Task 2's diagnostics context
- Pure functions (unit-tested directly; the systems only gather input, call them and apply the result): `widget_value_for(years) -> f64`, `years_for(widget_value) -> YearsBp`, `ui_opacity(idle_secs) -> f32`, `orbit_step(state, input) -> OrbitState`, `scrub_schedule(elapsed_secs) -> YearsBp`, and `frame_time_stats(&[f32]) -> Stats`.
- Produces:
  - **`SliderUiPlugin`:** a `bevy_ui_widgets` slider along the bottom (full width minus 16 px gutters), range 0..1, with a label above it showing `format_years_bp`.
    - **Orientation: time runs left to right.** The left end is 540 Ma and the right end is the present. The core's `s` (0 = present) is mirrored at the UI edge: `widget_value = 1 - years_to_slider(years)`.
    - Dragging writes `GlobeTime.years = slider_to_years(1 - widget_value)` and emits `TimeChanged` with cause `Slider`.
    - Changes from elsewhere (keys) move the thumb to `1 - years_to_slider(years)`.
    - With this mapping, the last ~12,000 years fill roughly the right third of the slider.
    - The UI fades to 30% opacity after 3 s without input, and returns to 100% on hover, drag or key press.
  - **`OrbitCameraPlugin`:**
    - Drag rotates around the origin. The scroll wheel zooms between distances `EARTH_RADIUS_KM * 1.15` and `EARTH_RADIUS_KM * 6.0`, starting at `* 3.0` facing lon 0.
    - Use Bevy 0.20's `pan_orbit_camera` controller (feature `pan_orbit_camera`) if it supports orbiting a fixed target with distance limits. Otherwise write `orbit_camera_system` yourself.
  - **`StatusUiPlugin`:** show "Loading Earth's history…" until the first interactive globe frame. Show a failure message if the bake fails to load or the renderer cannot initialize.
  - **`CreditsPlugin`:** `Esc` toggles a scrollable overlay showing `include_str!("../../assets/credits.txt")`. `credits.txt` lists every dataset's citation and licence from `data/README.md`.
  - **`ScrubBenchPlugin`:** enabled by `--scrub-bench` on native (optional `--target-fps N`, `--window WxH`) or `?scrub-bench` in the page URL on the web (read via `web-sys` `Location`; add `web-sys = { version = "0.3", features = ["Window", "Location"] }` to the wasm32 deps).
    - After the first interactive frame, drive `GlobeTime` through a deterministic 20 s full-range scrub in each direction, crossing the bridge.
    - Then log one block: session ID and log path, adapter name, backend, OS, render resolution, data revision (frame count, cube size), cold-load time to first interactive frame, and median / p95 / worst frame times with pass or fail against the target. On native, exit after logging.
    - This is the same measurement the owner runs on Linux in Task 11, so the output is plain text that can be pasted back.

- [ ] **Step 1: Write the failing tests**
  - `slider_drag_sets_time`: in an app with the plugin, setting the widget value to 0.5 gives `GlobeTime.years.0 ≈ 164266.77`. Setting it to 1.0 gives `0.0` (present at the right). Setting it to 0.0 gives `540e6`.
  - `external_time_moves_thumb`: setting `GlobeTime = 66 Ma` and updating gives a widget value ≈ 0.1298 (= 1 − 0.8702). `GlobeTime = 21 ka` gives ≈ 0.6269.
  - `ui_fades_when_idle`: advancing the virtual time 3.1 s with no input gives an opacity resource or component of 0.3, and a key press brings it back to 1.0.
  - `esc_toggles_credits`: pressing `Escape` makes the overlay visible; pressing it again hides it.
  - `scrub_bench_reports_percentiles`: feeding a known frame-time series gives the expected median, p95 and worst, and pass/fail flips at the target. The CLI parser accepts `--scrub-bench --target-fps 60 --window 2560x1440` and rejects a malformed `--window`.
- [ ] **Step 2: Run** `cargo test -p timefly`. Expected: fail.
- [ ] **Step 3: Implement** the four plugins and `credits.txt`.
- [ ] **Step 4: Run** the tests plus the wasm check. Expected: pass.
- [ ] **Step 5: Native Metal end-to-end check (this Mac).** With the full production bake, run `cargo run --release -p timefly`. Confirm the adapter log reports Metal on the Apple M4. Check present-day coastlines, ice and the Bering land bridge near 21 ka, Pangaea near 250 Ma, and the 540 Ma endpoint. Verify the bridge crossfade, full-range scrubbing, orbit limits, age label and credits. Save comparable screenshots. Then run `cargo run --release -p timefly -- --scrub-bench --window 2560x1440` and keep its output. Run the bench once more with `--no-default-features` (no `system-spans`), built with `CARGO_TARGET_DIR=target/scratch-nospans` and deleted afterwards, and record the span overhead. If spans raise p95 by more than 5%, make `system-spans` dev-only and note it. The M4 numbers are informational; the 60 fps desktop target is checked on Linux in Task 11.
- [ ] **Step 6: Chrome WebGPU end-to-end and performance check.** With the same full bake, run `cargo run --release -p timefly --target wasm32-unknown-unknown` using `wasm-server-runner` and open the URL in Chrome on this Mac (Apple M4, Chrome 158 or later; `chrome://gpu` should show WebGPU hardware accelerated). The M4 is faster than the spec's M1-class reference, so a pass here is provisional: record the headroom. Confirm the browser WebGPU backend in the initialization log. Exercise the same visual and input checks and verify that the loading overlay persists until the first interactive frame. For both the Metal and Chrome runs, record:
  - Adapter, OS, build, browser version where applicable, render resolution and data revision, including frame count and cube size.
  - Cold-load time from app launch or navigation to the first interactive frame. Run three cold trials, clearing the browser cache for each and recording any network conditions. In Chrome, report transferred bytes for the app and data separately. Also record the sum of decoded texture allocations and peak client memory through startup and scrubbing. Use an isolated browser session and record renderer and shared GPU process peaks separately with their idle baselines. If tooling lacks per-tab GPU memory, record the available process measurement and the calculated texture residency.
  - Median, 95th-percentile and worst frame times for a 20-second full-range scrub in each direction, starting after loading. Include the 26–30 ka bridge and track frame-boundary stalls and console errors. Meet the spec's 30 fps browser target at the 95th percentile (≤33.3 ms). Use `?scrub-bench` for the browser frame-time numbers.
  - Use the full-bake measurements to check the ~20 MB compressed data target and assess the preload strategy. At cube size 128, one `Rg16Float` frame is 384 KiB and one `Rgba16Float` frame is 768 KiB. A bake with 109 PaleoDEM textures including the bridge and 27 ICE-6G_C textures is about 61 MiB of GPU texels before upload copies, CPU retention and render targets. Document the actual results in `data/README.md`. If size or scrub targets are missed, adjust the asset or preload strategy and rerun both checks. Record any unresolved startup or memory trade-off for milestone approval.
- [ ] **Step 7: Commit** — `feat: time slider, orbit camera, status and credits UI`.

---

### Task 11: Linux x86-64 CI packages (.deb and Flatpak) and manual Vulkan check

The owner's Linux machine is for testing only. CI builds two installable packages from the same release binary and baked data: a Debian package (`.deb`) and a single-file Flatpak bundle (`.flatpak`). The owner installs each, runs a short checklist and pastes the benchmark output back. *(Owner request, 2026-11-22: both formats.)*

**Files:**
- Create: `data/sources.json`, `tools/bake/timefly_bake/fetch.py`, `tools/bake/tests/test_fetch.py`, `.github/workflows/linux-package.yml`, `docs/testing/linux-vulkan.md`
- Create (packaging): `packaging/linux/io.github.adsouza.TimeFly.desktop`, `packaging/linux/io.github.adsouza.TimeFly.metainfo.xml`, `packaging/linux/io.github.adsouza.TimeFly.svg` (a simple generated icon), `packaging/linux/stage.sh`, `packaging/linux/build-deb.sh`, `packaging/flatpak/io.github.adsouza.TimeFly.yml`
- Modify: `data/README.md`, `tools/bake/pyproject.toml` (script `timefly-fetch`)

**Shared conventions:**
- App ID `io.github.adsouza.TimeFly` (reverse-DNS of the public GitHub repo). Command `time-fly`. Package name `time-fly`.
- The game loads assets from the directory beside its executable. Both packages install the binary and `assets/` together in one directory and expose `time-fly` on PATH through a small wrapper script (`exec <dir>/time-fly "$@"`), so asset lookup never depends on symlink resolution. Verify Bevy 0.20's asset base-path rule before relying on it.
- The staged tree excludes `bake.log` and anything else under the asset folder that the game doesn't load.
- Logs: the `.deb` build writes to `~/.local/state/time-fly/logs/`; the Flatpak writes inside its sandbox, `~/.var/app/io.github.adsouza.TimeFly/.local/state/time-fly/logs/`. The crash notice shows the actual path either way.

**Interfaces:**
- `data/sources.json`: one entry per raw file approved and pinned in Tasks 5–7: `url`, `path` (relative to `data/raw/`), `sha256`, `bytes`, `licence`. It must agree with the table in `data/README.md`.
- `fetch.main(argv=None)`: `--sources FILE --raw DIR`. It skips a file whose checksum already matches, downloads missing files to a temp name, verifies size and SHA-256, then renames. A mismatch deletes the temp file and exits non-zero naming the file. No other network access.
- `packaging/linux/stage.sh BINARY ASSETS_DIR OUT_DIR`: builds the install tree shared by both packages: `lib/time-fly/{time-fly,assets/}`, `bin/time-fly` (wrapper), `share/applications/…desktop`, `share/icons/hicolor/scalable/apps/…svg`, `share/metainfo/…metainfo.xml`, `share/doc/time-fly/{copyright,README}` (licences and citations from `credits.txt`/`data/README.md`; README has the run commands and log locations). It works on any host, so its layout can be checked on this Mac.
- `packaging/linux/build-deb.sh STAGED_DIR VERSION OUT_DIR`: maps the staged tree to `/opt/time-fly` (binary + assets), `/usr/bin/time-fly` (wrapper) and `/usr/share/…`, writes `DEBIAN/control` (Package `time-fly`, Architecture `amd64`, Version `<cargo version>~git<short sha>`, Depends from `dpkg-shlibdeps` on the binary plus `libvulkan1`, Recommends for the libraries winit loads at runtime: `libwayland-client0 | libx11-6`, `libxkbcommon0`, `libxcursor1`, `libxrandr2`, `libxi6`), and runs `dpkg-deb --root-owner-group --build`.
- `packaging/flatpak/io.github.adsouza.TimeFly.yml`: `runtime: org.freedesktop.Platform`, `sdk: org.freedesktop.Sdk` at the current supported freedesktop branch (verify which branch is current when implementing), `command: time-fly`, one `simple` module that copies the staged tree into `/app` (no compilation inside flatpak-builder; the binary comes from the CI build job), `finish-args`: `--share=ipc`, `--socket=wayland`, `--socket=fallback-x11`, `--device=dri`, `--talk-name=org.freedesktop.FileManager1` (for **Show log**). The bundle is made with `flatpak build-bundle … --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo` so installing it pulls the runtime from Flathub.
- `.github/workflows/linux-package.yml`:
  - Triggers: `workflow_dispatch` and tags `v*` only, to save CI minutes. Pin third-party actions to major versions. Use no secrets.
  - Job `build` on `ubuntu-22.04` (x86-64; older glibc so the binary runs on more distros): checkout → apt `pkg-config libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev desktop-file-utils appstream` → the Rust toolchain from `rust-toolchain.toml` (via rustup) → Rust cache → `astral-sh/setup-uv` → `actions/cache` for `data/raw/` keyed on the hash of `data/sources.json` → `timefly-fetch` → `timefly-bake` (production cube size) → `cargo build --release -p timefly` (with `TIMEFLY_GIT_SHA` set) → `timefly --check-data=crates/timefly/assets/data/globe` (loads the bake through the game's reader, window-less, no GPU; exits 1 if it fails) → `stage.sh` → `desktop-file-validate` and `appstreamcli validate --no-net` → `build-deb.sh` → upload the `.deb`, and the staged tree as an intermediate artifact.
  - Job `test`, in parallel with `build`: `uv run pytest -q`, `cargo test -p timefly_core` and `cargo test -p timefly` (CPU-only; the GPU globe test is `#[ignore]`) as gates.
  - Job `flatpak` (needs `build` and `test`): runs in the freedesktop flatpak-builder container matching the manifest's runtime branch (e.g. the `flatpak/flatpak-github-actions` image), downloads the staged tree, builds with the manifest and produces `time-fly-<short sha>.flatpak`; uploads it.
  - Artifacts: `time-fly_<version>_amd64.deb` and `time-fly-<sha>.flatpak` (14-day retention), plus `bake.log` and test output as a separate artifact uploaded even when the run fails.

- [ ] **Step 1: Write the failing fetch tests.** Serve a fixture file from a local `http.server` thread. Cover a fresh download, a skip when the checksum already matches, a checksum mismatch that leaves no file behind and exits non-zero, and a `sources.json` entry whose path escapes `data/raw/` (rejected).
- [ ] **Step 2: Run** `uv run pytest -q tests/test_fetch.py`. Expected: fail.
- [ ] **Step 3: Implement** `fetch.py` and write `data/sources.json` from the files already approved, downloaded and verified in Tasks 5–7. Run `uv run timefly-fetch --sources ../../data/sources.json --raw ../../data/raw` locally. Expected: every file skipped as already verified.
- [ ] **Step 4: Write the packaging files, workflow and checklist.** On this Mac, run `stage.sh` with the macOS release binary and the baked assets into a temp dir and list the tree (check `bake.log` is absent and the wrapper resolves assets). `.deb` and Flatpak builds and the desktop/AppStream validators run in CI. Check that redistributing the baked derivatives is allowed by every licence recorded in `data/README.md` (the owner approved the ICE-6G_C derivatives with citation and credit on 2026-11-22; record that). If another licence forbids it, stop and ask the owner.
- [ ] **Step 5: Commit** — `ci: Linux .deb and Flatpak packages with pinned data fetch`.
- [ ] **Step 6: Run CI (owner's OK required for each).** Ask the owner before pushing the branch to `origin`. Ask again before dispatching with `gh workflow run linux-package.yml --ref <branch>`. Watch the run with `gh run watch`. Expected: green, with one `.deb` and one `.flatpak` artifact. Fix failures locally, commit, and ask before pushing again.
- [ ] **Step 7: Owner's manual Vulkan check.** `docs/testing/linux-vulkan.md` tells the owner to download both artifacts (`gh run download <run-id>` or the Actions page), run `vulkaninfo --summary` and note the GPU and driver, then for each package:
  - **`.deb`:** `sudo apt install ./time-fly_<version>_amd64.deb`; launch from the app menu once and from a terminal as `WGPU_BACKEND=vulkan time-fly`.
  - **Flatpak:** `flatpak remote-add --if-not-exists --user flathub https://dl.flathub.org/repo/flathub.flatpakrepo`, `flatpak install --user ./time-fly-<sha>.flatpak`, then `flatpak run --env=WGPU_BACKEND=vulkan io.github.adsouza.TimeFly`. (On NVIDIA's proprietary driver, Flatpak needs the matching `org.freedesktop.Platform.GL.nvidia` extension, which Flatpak normally installs from Flathub automatically.)
  - Check the Task 10 Step 5 visual list (coastlines, 21 ka ice and Bering bridge, 250 Ma Pangaea, 540 Ma, bridge crossfade, orbit limits, label, credits).
  - Run the bench (`time-fly --scrub-bench --target-fps 60 --window 2560x1440`, or the `flatpak run …` equivalent) and paste the output block back.
  - Run the crash probe (`--diagnostics-probe=panic`, then a normal launch). Expect a notice pointing at that package's log location; send the file back.
  - Uninstall afterwards: `sudo apt remove time-fly` / `flatpak uninstall --user io.github.adsouza.TimeFly`.

  Record the owner's results in `data/README.md` alongside the Mac numbers. Milestone 1 is accepted when both packages install and run on Vulkan on a hardware GPU, the visual checks pass in both, and the `.deb` bench block ends `result: PASS` at 60 fps (the Flatpak block is recorded too). Under ruling R14 the bench judges a display-paced run (frames paced by vsync or the compositor even though `AutoNoVsync` was requested) by missed frames: at most 1% longer than 1.5× the 16.67 ms interval in each direction, with each median at most 5% above the interval; only a run that is really uncapped is judged by p95 ≤ 16.67 ms. The raw p95 is always reported. If the target is missed, treat it as a Task 10 Step 6 strategy change and rerun.

---

## Known limitations (accepted for Milestone 1)

- Beyond ICE-6G_C coverage, the Quaternary bridge uses modern terrain, changing sea level where the curve provides it, and no reconstructed ice overlay. Standard stops between the 30 ka bridge and the first older PaleoDEM are approximations: 50 ka, 100 ka and, unless Table 1 lists a source near it, 1 Ma.
- The plate boundaries are quantized to the baked cube grid; the bake grows footprints by about 2 cells to close boundary cracks. Where no plate covers a frame's land, the ocean shell shows that frame's own land, drawn flat with no relief. At frame ages this matches the source exactly; between frames that land crossfades instead of drifting, and thin slivers can appear at plate edges 1–2 Myr from a frame. Overlapping land follows the depth and tie rules. The coastline diagnostic gates major coverage errors.
- There's no biome colouring yet (elevation palette only). Climate and biome data arrive with region worlds (Milestone 2).
- There are no clouds and no day/night city lights; both are polish items.
- There's no creature yet; the orbit camera stands in for it until zoom-and-swap (Milestone 3).
- Browser performance is measured on an Apple M4, which is faster than the spec's M1-class reference, so the 30 fps browser result is provisional.
- A native crash that isn't a Rust panic (driver fault, abort, out-of-memory kill) is detected on the next run and its session log is kept, but there's no crash report or minidump for it, and the last buffered log lines may be lost.
- There is no WebGL2 fallback. The plate renderer needs storage buffers, which WebGL2 lacks.
- There's no session lock on the web. Opening the game in a second tab while the first is still open makes the first tab's session look crashed, so the second tab shows the crash notice about it.
- The in-game **Report problem** button opens GitHub's issue form only after this branch is merged to `main`: GitHub reads issue templates from the default branch.
- Without a usable GPU the game says so instead of starting: on native a dialog ("No compatible GPU/driver found (Vulkan/Metal)", with the log path) and exit code 1; on the web an HTML message asking for a browser with WebGPU. Either way the session is recorded as clean, so the next launch shows no crash notice. The native check asks wgpu for an adapter without the window's surface, so an adapter that exists but can't present to the window still reaches Bevy's renderer and fails there as before. A device lost after start is still recorded as a crash.

---

## Appendix: Rulings and deviations

The execution ledger is git-ignored, so the decisions that changed what was built are recorded here. Each line gives what was decided, why, and the cost if it proves wrong.

**Controller rulings**

- **R1:** One launch-argument parser, `crates/timefly/src/args.rs` (`parse_launch_args` for native argv, `parse_query_args` for the web URL query), shared by the diagnostics probe, the scrub bench and later `--check-data`. Why: two parsers would diverge. Cost if wrong: moving a little code.
- **R2:** Implementers never download data; the controller listed every file with size and licence and asked the owner once (approved 2026-11-22). Why: downloads need the owner's explicit permission. Cost if wrong: one pause.
- **R3:** Everything automatable is automated; the controller runs web checks in a browser and non-interactive native checks; interactive native GUI checks (the rfd dialog and overlay buttons) go to `docs/testing/mac-manual-checks.md` for the owner. Why: native dialogs can't be clicked reliably from here. Cost if wrong: a 5-minute owner checklist.
- **R4:** ICE-6G_C comes from the author's 1° files, not the PMIP4 10′ series (owner approved). Why: a 128-texel cube face is ~0.7° per texel; 15.7 MB against ~1 GB. Cost if wrong: refetch the 10′ set.
- **R5:** One toolchain: rustup with Rust 1.98.1 pinned in `rust-toolchain.toml`, rustup's directory first on `PATH`. Why: Homebrew's and rustup's differing version strings forced full rebuilds and duplicate artifacts. Cost if wrong: one rebuild.
- **R6:** Bevy's `basis-universal` feature is native-only. Why: its C++ transcoder doesn't build for `wasm32-unknown-unknown`, and the globe uses `.tfgrid`, not Basis. Cost if wrong: the web build can't transcode Basis textures (none in Milestone 1).
- **R7:** The `target/` guard hook wasn't live in the execution session, so the controller ran `tools/dev/target-guard.sh` after each task and implementers after large builds. Why: `.claude/` didn't exist when the session started. Cost if wrong: none.
- **R8:** `diagnostics/session.rs` (1,066 lines) was split into `session/{mod,store,web}.rs` plus `probe.rs`. Why: the plan asks for focused files. Cost if wrong: churn.
- **R9:** The `system-spans` feature enables `bevy/trace` and `bevy/debug`. Why: spans need real system names for Task 10's span-cost measurement. Cost if wrong: a slightly larger build.
- **R10:** `CrustModel::resolve` is not instrumented. Why: spec §5.2 calls it per terrain sample, and the per-sample rule beats the plan's "coarse entry point" wording. Cost if wrong: one attribute.
- **R11:** The bridge crossfade's deep side applies the sea-level anomaly at the current age, not a fixed 30 ka. Why: it is continuous at both ends and physically closer. Cost if wrong: a one-line flip.
- **R12:** The bake dilates plate footprints: unowned cells within k cells of an owned cell (k = 2 at cube 128, scaled with cube size; a CLI flag tunes or disables it) join the nearest plate (breadth-first, ties to the lowest plate ID) with its validity intervals. Why: cube quantization left ocean cracks through continental interiors; plate pieces hide below-sea-level samples, so dilated ocean cells still show ocean. Cost if wrong: slightly fatter plates, overlap handled by the depth rules.
- **R13:** Each PaleoDEM frame's G channel is a plate-coverage mask; where it is 0 the ocean shell shows that frame's own land (crossfaded like elevation). Why: 7 Mkm² of land was missing at 66 Ma, and spec §7.1 makes the ocean shell the gap cover. Cost if wrong: gap land crossfades instead of drifting between frames.
- **R14:** The scrub bench judges a display-paced run by missed frames (at most 1% longer than 1.5× the target interval) and a median at most 5% above the interval; only a truly uncapped run uses p95 ≤ the interval, and raw p95 is always reported. Why: spec "60 fps" means no missed frames, and p95 ≤ 16.67 ms is unmeetable under 60 Hz pacing. Cost if wrong: a looser gate.
- **R15:** A `web-release` profile (release without debuginfo, `strip = true`, opt-level 3 kept). Why: the wasm shrank from 100.6 MB to 65.8 MB raw (7.7 MB brotli). Cost if wrong: browser stack traces show function indices, not names.
- **R16:** Bridge-camera MSAA memory (about 309 MiB at 2560×1440) is left as is for now. Why: the browser-pane run at devicePixelRatio 1 was fine; revisit after a desktop-Chrome Retina run. Cost if wrong: high memory in Retina browsers while bridging.
- **R17:** No manifest version bump for the G channel's new meaning. Why: CI and the packages always bake fresh. Cost if wrong: a stale local bake shows ghost land between frames.
- **Unnumbered (Task 1):** The age label picks its unit from the rounded value, with a singular "1 year ago". Why: the plan's thresholds on unrounded values gave labels such as "1000 thousand years ago". Cost if wrong: trivial.

**Owner decisions (2026-11-22)**

- **ICE-6G_C:** use the author's 1° files (R4). They state no licence, only a request to cite two papers; the owner approved distributing the baked derivatives with both citations in the credits and package copyright file, never the raw files (recorded in `data/README.md`).
- **Licence:** AGPL-3.0-or-later.
- **Maintainer:** "Antonio D'souza <275832+adsouza@users.noreply.github.com>" in the package metadata.
- **Linux packages:** both a `.deb` and a single-file Flatpak bundle (app ID `io.github.adsouza.TimeFly`) from one release build.
- **Compressed debug sections:** CI alone links the release binary with `--compress-debug-sections=zlib` (about 123 MB download / 230 MB installed, against about 80 / 576 MB uncompressed); local builds are unaffected.
- **Push and CI:** pushing `milestone-1` to `origin` and running `linux-package.yml`, fix-and-rerun pushes included, is approved; never push `main`, never force-push.
- **TLS for the ICE-6G_C host:** its server omits the Sectigo intermediate, so the fetcher downloads exactly that intermediate through the leaf's AIA URL and retries with partial chains disallowed (the chain must end at a system root); hostname checks, required certificates and the SHA-256 pins are unchanged, and a self-signed AIA certificate is refused.
- **Browser bench:** run in the Claude desktop app's browser pane, visible, on this Mac (the Chrome extension's browser was on another machine and couldn't reach localhost). Cold-cache trials and Chrome memory in desktop Chrome remain optional owner checks.

**Other deviations from the plan text**

- Task 7 reads the archives in place (no manual unpacking); `ages.py` and `--paleodem-ages` / `--paleodem-step-myr` were added.
- The bake tool needs Python 3.13 (`pygplates` 1.0.0 wheels stop at cp313).
- CI got a parallel `test` job, caches and a temporary push trigger on `milestone-1` (the workflow had to run once before it could be dispatched); the trigger was removed after the final CI verification, leaving `workflow_dispatch` and `v*` tags.
- Final review fixes: the pre-flight GPU check (see Known limitations), hidden `--check-data[=DIR]` run in CI on the production bake, `cargo test -p timefly` in CI, the scrub bench's FAIL exit code (3, a clean session) and FAIL block when the globe fails, and per-frame time changes logged below `info`.
