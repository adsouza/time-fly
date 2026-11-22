# Build disk hygiene

## Why

Bevy builds are huge. Every profile (debug, release), target triple (wasm32) and
feature combination keeps its own full copy of every dependency in `target/`, and
cargo never removes stale artifacts. On a laptop with a few GB free this fills the disk.

## Profile choices (root `Cargo.toml`)

- `dev`: `opt-level = 1`, `debug = "line-tables-only"` for workspace crates.
- `dev` dependencies: `opt-level = 3`, `debug = false` (no debuginfo for third-party code,
  the largest single saving).
- `release`: `lto = "thin"`, `codegen-units = 1`.

## The guard

`tools/dev/target-guard.sh` keeps `<repo>/target` under a budget. It only ever operates
on that directory (refuses if it is a symlink or missing).

| Variable | Default | Meaning |
|---|---|---|
| `TIMEFLY_TARGET_BUDGET_MB` | 15360 | max size of `target/` |
| `TIMEFLY_MIN_FREE_MB` | 5120 | min free space on the disk |

If both limits hold it does nothing. Otherwise it prunes, re-measuring after each step:

1. incremental-compilation sessions not modified in 24 h;
2. `cargo sweep --time 2`, if `cargo-sweep` is installed;
3. whole build dirs (`release`, `wasm32-unknown-unknown`, `scratch-*`, ...) not modified in
   24 h, oldest first, never the most recently modified one;
4. `cargo clean`, only if free space is still below the minimum.

Steps 3 and 4 are skipped while a `cargo` or `rustc` process is running.

It runs automatically: `.claude/settings.json` registers a `PostToolUse` hook on `Bash`
that calls `target-guard.sh --hook` (acts only on commands containing `cargo`, reports to
Claude Code only when it pruned or limits are still exceeded, never blocks).

```sh
tools/dev/target-guard.sh --status   # sizes and limits, deletes nothing
tools/dev/target-guard.sh            # prune now, with a report
tools/dev/test-target-guard.sh       # self-test of the guard
```

## Rule for development

Never build feature-combination or profile variants into the main target dirs ad hoc.
Use `CARGO_TARGET_DIR=target/scratch-<name>` and delete it when done. Verify wasm with
`cargo check --target wasm32-unknown-unknown` except in the web end-to-end checks.

Optional, for finer pruning: `cargo install cargo-sweep` (the guard uses it when present).

## Toolchain

The project uses one toolchain: rustup's `1.98.1` (`rust-toolchain.toml`; it includes the
`wasm32-unknown-unknown` std). Homebrew's standalone `cargo`/`rustc` is the same release, but
its version string ends in "(Homebrew)", so cargo fingerprints it as a different compiler:
alternating between the two rebuilds everything and leaves duplicate artifacts in `target/`.

rustup's proxies live in the keg-only `/opt/homebrew/opt/rustup/bin`, which is not on `PATH`.
Run every cargo command for this project with it first:

```sh
PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build -p timefly
```

`target-guard.sh` does this itself. To make it the default in this directory, add
`PATH_add /opt/homebrew/opt/rustup/bin` to your (git-ignored) direnv `.envrc`.
Check with `cargo -vV` (no "(Homebrew)") and `rustc --print sysroot` (inside `~/.rustup/toolchains/1.98.1-*`).
