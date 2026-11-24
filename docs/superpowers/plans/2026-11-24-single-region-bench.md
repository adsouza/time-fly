# Single-region bench Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `--region-bench --region=LAT,LON,AGE_YEARS` (web: `?region-bench&region=…`) benches only that place: one 60 s flight, no pan, no memory cycles.

**Architecture:** `RegionBenchConfig` gains the list of regions to fly. The full bench passes `BENCH_REGIONS` and a single-region run passes one `BenchRegion`. Every read of the constant list, `stop_count()` and `cycle_region()` goes through the bench's own list. `cycles: 0` sends the last region straight to the summary.

**Tech Stack:** Rust, Bevy 0.20 (native and wasm32).

**Spec:** `docs/superpowers/specs/2026-11-24-single-region-bench-design.md`

## Global Constraints

- Triggered only by both flags together. `--region-bench` alone is unchanged (all 8 stops, pan, 10 cycles). `--region` alone is unchanged (enter that region and play).
- The chosen region's name is `"Chosen region"`. `pan_from_previous: false`. `cycles: 0`.
- The summary's line after the header: `regions: single (LAT, LON at AGE years)` or `regions: all 8`, with LAT/LON/AGE printed as the block titles print them (`{}` of the f64s).
- Warm generation and memory cycles in a single run print `not measured (single-region run)` and never fail the result.
- R11 (≤ 250 ms first-region worst frame), the first-entry gate and R14 apply to the chosen region as to the first stop.
- The name-keyed special case "Himalayas load-window worst frame (reported only)" applies only to the full bench's stop.
- Repo rules as in `docs/superpowers/plans/2026-11-24-no-dunes.md`'s Global Constraints: the worktree and branch, explicit staging, the commit identity and trailer, no push, the toolchain PATH and shared CARGO_TARGET_DIR, a fresh TIMEFLY_LOG_DIR and no timeout kill.

## Review Focus

1. **`--region` with an invalid value plus `--region-bench`.** `parse_region` rejects it, so `args.region` is `None`, and the full bench runs. Expected: that's what happens, and the value is in `unknown`. Test in Task 1.
2. **Web order.** `?region=20,37.5,0&region-bench` and `?region-bench&region=20,37.5,0` both give a single-region run. Test in Task 1.
3. **No coast near the chosen place.** The path's "none within N km" branch must work for a single region, as it does for Pangaea. Covered by the native run in Task 1 (Pangaea's coordinates).
4. **Banner count.** "1 of 1", never "1 of 9". Test in Task 1.
5. **The run ends.** With `cycles: 0` the bench reaches the summary and, natively, exits. Test in Task 1.

---

### Task 1: The bench flies the config's regions

**Files:**
- Modify: `crates/timefly/src/bench/region/mod.rs` (config, state, every `BENCH_REGIONS` / `stop_count()` / `cycle_region()` use, summary line, tests)
- Modify: `crates/timefly/src/bench/region/banner.rs` (counts from the bench's list)
- Modify: `crates/timefly/src/lib.rs:138-150` (build the single-region config when both flags are set)
- Modify: `crates/timefly/src/args.rs` (the `region_bench` field doc: with `--region`, only that region)
- Modify: `docs/testing/region-m2/performance.md` (one paragraph, how to run a single region)

**Interfaces:**
- Produces: `RegionBenchConfig { regions: Vec<BenchRegion>, .. }`. `RegionBenchConfig::new(target_fps, exit_when_done)` keeps building the full bench.
- Produces: `pub fn single(target_fps: u32, exit_when_done: bool, arg: RegionArg) -> RegionBenchConfig` on `RegionBenchConfig`.
- Produces: `RegionBench::stop_count(&self) -> usize` and `RegionBench::cycle_region(&self) -> Option<usize>`, replacing the free functions. `cycle_region` is `None` when the list has no `CYCLE_REGION`.
- Produces: `fn regions_line(config: &RegionBenchConfig) -> String`.

- [ ] **Step 1: Write the failing tests**

In `bench/region/mod.rs`'s tests:

```rust
#[test]
fn single_config_flies_one_region_without_cycles() {
    let arg = RegionArg { lat_deg: 20.0, lon_deg: 37.5, years: 0.0 };
    let c = RegionBenchConfig::single(30, true, arg);
    assert_eq!(c.regions.len(), 1);
    assert_eq!(c.regions[0].name, "Chosen region");
    assert_eq!(c.regions[0].arg, arg);
    assert!(!c.regions[0].pan_from_previous);
    assert_eq!(c.cycles, 0);
    let bench = RegionBench::new(c.clone());
    assert_eq!(bench.stop_count(), 1);
    assert_eq!(bench.cycle_region(), None);
    assert_eq!(regions_line(&c), "regions: single (20, 37.5 at 0 years)");
    assert_eq!(regions_line(&RegionBenchConfig::new(30, true)), "regions: all 8");
}
```

The existing test-app harness for the bench (the tests that shorten `path_secs` and `cycles`) also gets `a_single_region_run_reaches_the_summary`. Run a `single(..)` config with the short path. Assert that it reaches `Stage::Done` and that its summary contains `regions: single`, `warm generation (target <= ` … `not measured (single-region run)` and `memory cycles: not measured (single-region run)`. Assert it has exactly one region block. Also assert the banner text during the flight ends with `1 of 1`.

In `args.rs`'s tests:

```rust
#[test]
fn region_bench_with_a_region_in_either_order() {
    for q in ["?region=20,37.5,0&region-bench", "?region-bench&region=20,37.5,0"] {
        let p = parse_query_args(q);
        assert!(p.region_bench && p.region.is_some(), "{q}");
    }
    let bad = parse_launch_args(args(&["--region-bench", "--region=200,0,0"]));
    assert!(bad.region_bench && bad.region.is_none());
}
```

- [ ] **Step 2: Run them to see them fail**

Run: `cargo test -p timefly --lib single_config_flies_one_region_without_cycles a_single_region_run_reaches_the_summary region_bench_with_a_region_in_either_order`
Expected: compile errors (`single`, `regions`, `regions_line` and the methods don't exist). The args test may already pass. Note it and keep it: it pins Review Focus 1 and 2.

- [ ] **Step 3: Implement**

- Move every `BENCH_REGIONS` read in `mod.rs` and `banner.rs` to `bench.config.regions`. Replace `stop_count()` and `cycle_region()` with the methods. Where `cycle_region()` was used to pick the cycles' region, `None` means there are no cycles to run.
- From the last region, `Next` goes to `Summary` when `config.cycles == 0`, without entering a cycle.
- The summary pushes `regions_line(&bench.config)` first after the header. With `cycles == 0`, the warm and memory lines read `not measured (single-region run)`.
- Keep the Himalayas "reported only" line keyed to the full bench: it applies when the region's name is `"Himalayas"`, which `"Chosen region"` never is.
- `lib.rs`: `if args.region_bench { let config = match args.region { Some(r) => RegionBenchConfig::single(target_fps, exit_when_done, r), None => RegionBenchConfig::new(target_fps, exit_when_done) }; … }`.
- `performance.md`: under the region bench's run instructions, add "To bench one place: `--region-bench --region=LAT,LON,AGE_YEARS` (web `?region-bench&region=…`): one flight, no pan or memory cycles, about a minute".

- [ ] **Step 4: Run the tests, then the crate**

Run: `cargo test -p timefly --lib`, `cargo test -q --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
Expected: all pass, including the existing bench tests (the full bench is unchanged).

- [ ] **Step 5: One real run**

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=crates/timefly \
  cargo run --release -p timefly -- --region-bench --region=-22,-18,250000000
```

Expected: one `===== Time Fly region bench: Chosen region (-22, -18 at 250000000 years) =====` block, with `coast: none within …`. The summary has `regions: single (-22, -18 at 250000000 years)` and both `not measured (single-region run)` lines. The process exits by itself in about a minute.

- [ ] **Step 6: Commit**

```bash
git add crates/timefly/src/bench/region/mod.rs crates/timefly/src/bench/region/banner.rs crates/timefly/src/lib.rs crates/timefly/src/args.rs docs/testing/region-m2/performance.md
git commit -m "feat: --region-bench with --region benches only that region"
```
