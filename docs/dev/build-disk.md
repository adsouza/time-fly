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

`tools/dev/target-guard.sh` keeps `<repo>/target` under a budget and watches free disk space.
It only ever deletes inside that directory (refuses if it is a symlink or missing, never follows
symlinks inside it). It never deletes anything outside `target/`.

| Variable | Default | Meaning |
|---|---|---|
| `TIMEFLY_TARGET_BUDGET_MB` | 15360 | max size of `target/` |
| `TIMEFLY_MIN_FREE_MB` | 5120 | min free space on the disk |
| `TIMEFLY_WARN_FREE_MB` | 10240 | warn Claude after any Bash command when free space is below this |
| `TIMEFLY_XCTRACE_MIN_FREE_MB` | 10240 | block `xctrace record` when free space is below this |

If both limits hold it does nothing. Otherwise it prunes, re-measuring after each step:

1. incremental-compilation sessions not modified in 24 h;
2. `cargo sweep --time 2`, if `cargo-sweep` is installed;
3. whole build dirs (`release`, `wasm32-unknown-unknown`, `scratch-*`, ...) not modified in
   24 h, oldest first, never the most recently modified one;
4. last resort, only if free space is still below the minimum: whole build dirs, fresh or not,
   one at a time in the order `release`, `<triple>/` dirs, `scratch-*`, any others, `debug`
   last, stopping as soon as free space is back above the minimum. Only if every dir has to go
   does `target/` end up empty (the same as `cargo clean`; the guard no longer runs it).

Steps 3 and 4 are skipped while a `cargo` or `rustc` process is running. If free space is short
of the minimum by more than all of `target/` holds, deleting builds would not fix it: the guard
deletes nothing for free space (steps 1-3 still run for the budget) and instead lists the likely
culprits outside `target/` (below).

### Hooks

`.claude/settings.json` registers two hooks on `Bash`:

- `PostToolUse`: `target-guard.sh --hook`, after every command. It never blocks.
  - **Free space:** one `df` call; if free space is at least `TIMEFLY_WARN_FREE_MB` it exits
    silently. Below it, it tells Claude the free space, the size of `target/`, and the largest
    regular files (500 MB or more, at most 5) directly in `$TMPDIR`, where xctrace leaves its raw
    `instruments*.ktrace`. It deletes none of them: the owner must approve deleting anything
    outside `target/`.
  - **Pruning** (the steps above) runs only when the command actually runs cargo: as a command
    at the start or after `&&`, `||`, `;`, `|`, `(`, `$(` or a newline, optionally behind
    `VAR=value` assignments, `env`, `time`, `nice`, `command` or `direnv exec <dir>`, or as a path
    ending in `/cargo`. Mentions such as `pgrep -fl 'cargo|rustc'`, `grep cargo`,
    `ls ~/.cargo/bin` or `echo "run cargo test"` do not count. It reports to Claude Code only when
    it pruned, limits are still exceeded, or free space is low.
- `PreToolUse`: `target-guard.sh --pre-hook`, before every command. It denies `xctrace record`
  (also by path or through `xcrun`) without `--time-limit`, and any `xctrace record` while free
  space is under `TIMEFLY_XCTRACE_MIN_FREE_MB`; the reason says why and how to proceed. It allows
  everything else, and allows everything if the input can't be parsed or neither `jq` nor
  `python3` is available.

```sh
tools/dev/target-guard.sh --status   # sizes and limits, deletes nothing
tools/dev/target-guard.sh            # prune now, with a report
tools/dev/test-target-guard.sh       # self-test of the guard
```

### Incidents behind the current rules (2026-11-24)

- **xctrace filled the Mac.** `xctrace record` streams its raw trace to
  `$TMPDIR/instruments<random>.ktrace`, not to `--output`. Two aborted recordings left 3.4 + 4.3 GB
  there and the disk hit ENOSPC. The guard only ran after cargo commands and only looked inside
  `target/`. Hence the free-space warning after every command and the xctrace block. For GPU
  cost, prefer sampling `ioreg -c IOAccelerator` ("Device Utilization %", see
  `docs/testing/region-m2/performance.md`); if you do record, pass `--time-limit` and check
  `df -h` first.
- **A mention of cargo wiped every build.** With 3.8 GiB free, `pgrep -fl 'cargo|rustc'` matched
  the old `*cargo*` filter and the last resort ran a full `cargo clean` (10.6 GB: debug 5.8,
  release 4.8), when deleting `target/release` alone would have restored the 5 GB minimum. Hence
  the cargo-invocation check and the proportional last resort.

## Rule for development

Never build feature-combination or profile variants into the main target dirs ad hoc.
Use `CARGO_TARGET_DIR=target/scratch-<name>` and delete it when done. Verify wasm with
`cargo check --target wasm32-unknown-unknown` except in the web end-to-end checks.

Optional, for finer pruning: `cargo install cargo-sweep` (the guard uses it when present).

## Toolchain

The project uses one toolchain: rustup's `1.100.0` (`rust-toolchain.toml`; it includes the
`wasm32-unknown-unknown` std). Homebrew's standalone `cargo`/`rustc` may be the same release, but
its version string ends in "(Homebrew)", so cargo fingerprints it as a different compiler:
alternating between the two rebuilds everything and leaves duplicate artifacts in `target/`.

rustup's proxies live in the keg-only `/opt/homebrew/opt/rustup/bin`, which is not on `PATH`.
Run every cargo command for this project with it first:

```sh
PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build -p timefly
```

`target-guard.sh` does this itself. To make it the default in this directory, add
`PATH_add /opt/homebrew/opt/rustup/bin` to your (git-ignored) direnv `.envrc`.
Check with `cargo -vV` (no "(Homebrew)") and `rustc --print sysroot` (inside `~/.rustup/toolchains/1.100.0-*`).

To move to a new Rust release, run `cargo clean` first, then change `channel` in
`rust-toolchain.toml`. A new compiler invalidates every artifact, and the old ones would stay in
`target/` next to the new. CI's caches rebuild once, cold.
