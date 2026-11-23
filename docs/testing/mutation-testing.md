# Property and mutation testing

Line coverage shows which code ran under test. Mutation testing shows whether a test would
notice if that code were wrong. Property tests are the main way to close the gaps it finds.

## Property tests

- **Rust:** `crates/timefly_core/tests/properties.rs` ([proptest](https://docs.rs/proptest)). It
  covers deep time, geo frames, the cube map, the binary readers, rotations, footprints, globe
  blending and the timeline. It runs with `cargo test`.
- **Python:** `tools/bake/tests/test_properties.py` ([Hypothesis](https://hypothesis.readthedocs.io)).
  It covers geo frames, the cube map, ages, `.tfgrid`/`.tfplates` writing, interval helpers,
  footprint dilation, lat-lon grids, bilinear sampling and log redaction. It runs with
  `uv run pytest`.

The cube and geo properties are the same on both sides, because the Rust runtime and the Python
bake must agree on those conventions.

When a property fails, proptest prints a minimal failing input. Keep it as a named example test
next to the code, as `format_years_bp` does for 42,498,528.9 years. Its first property run found
that age labelled "43 million years ago", because it was rounded twice.

## Mutation testing (Rust core)

```bash
cargo mutants -p timefly_core -j 6 --timeout 60
```

This takes about 8 minutes on an M4 and writes `mutants.out/`. Use `-o <dir>` to put it outside
the repo. Each mutant is built in a copy of the tree, but the core has no Bevy dependency, so
each copy is small.

| Run (2026-11-22) | Caught | Missed | Unviable | Timeout | Score |
|---|---|---|---|---|---|
| Before property tests | 639 | 75 | 63 | 1 | 89.5% |
| After, 9 equivalent mutants excluded | 705 | 0 | 63 | 1 | 100% |

Score is caught (timeouts included) divided by caught plus missed.

- **The timeout:** a `%`→`+` change in the SHA-256 padding loop never ends, so the test run hangs.
  It counts as detected.
- **Equivalent mutants:** these are listed with their reasons in `.cargo/mutants.toml`. Each
  pattern pins one `file:line:column`. After editing one of those lines, re-check its location
  with `cargo mutants -p timefly_core --list`.
- **A new survivor:** add a property or example test that kills it. Exclude it only when no input
  can tell the mutant apart from the original, and write down why.

The shell crate (`timefly`, 1,405 mutants) isn't run routinely. Each mutant relinks Bevy, so a full
run takes hours. Its pure modules (`args`, `diagnostics/session`, `bench` decisions) are the best
candidates for a targeted run with `--file`.

## Coverage

| Scope (2026-11-22) | Lines |
|---|---|
| Rust workspace, all tests | 79.9% |
| `timefly_core`, its own tests | 97.2% |
| Bake tool (branch coverage) | 92% |

Measure with:

```bash
cargo llvm-cov --workspace --summary-only
```

```bash
cd tools/bake && uv run --with pytest-cov pytest --cov=timefly_bake --cov-branch
```

Branch coverage for Rust needs a nightly toolchain. The workspace figure is held down by code
that only runs with a GPU or a native dialog:
- `globe/capture.rs` is 0%: its test needs a GPU and is ignored in CI.
- `globe/mod.rs` is 31%.
- `diagnostics/notice.rs` is 12%.
- `bench.rs` is 58%.
