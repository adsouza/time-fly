#!/usr/bin/env bash
# shellcheck disable=SC2016  # single-quoted bash -c bodies are intentional
# Self-contained tests for tools/dev/target-guard.sh. Builds fake roots in a temp
# dir; never touches the real target/.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="$SCRIPT_DIR/target-guard.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/tfguard.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

PASS=0
FAIL=0
ok()   { PASS=$((PASS + 1)); echo "  ok   $1"; }
bad()  { FAIL=$((FAIL + 1)); echo "  FAIL $1"; }
check() { # check <description> <command...>
  local d="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$d"; else bad "$d"; fi
}

# Fake tools first on PATH: cargo records its calls; pgrep says "nothing running".
FAKEBIN="$TMP/fakebin"
CALLS="$TMP/cargo-calls"
mkdir -p "$FAKEBIN"
printf '#!/bin/sh\necho "$* | CTD=${CARGO_TARGET_DIR-unset}" >> "%s"\n' "$CALLS" > "$FAKEBIN/cargo"
printf '#!/bin/sh\nexit 1\n' > "$FAKEBIN/pgrep"
chmod +x "$FAKEBIN/cargo" "$FAKEBIN/pgrep"

export TIMEFLY_RUSTUP_BIN=/nonexistent   # keep the guard from prepending the real rustup
export PATH="$FAKEBIN:$PATH"
unset TIMEFLY_FAKE_FREE_MB TIMEFLY_TARGET_BUDGET_MB TIMEFLY_MIN_FREE_MB

OLD=200001010000     # touch -t stamps

# mkfile <path> <MB>
mkfile() { mkdir -p "$(dirname "$1")"; dd if=/dev/zero of="$1" bs=1048576 count="$2" 2>/dev/null; }
# age <path> <stamp>  (recursively, children first so dir mtimes end up as stamped)
age() { find "$1" -exec touch -t "$2" {} +; }
newroot() { ROOT="$TMP/root.$1"; rm -rf "$ROOT"; mkdir -p "$ROOT/target"; : > "$CALLS"; }
guard() { TIMEFLY_GUARD_ROOT="$ROOT" "$GUARD" "$@"; }

echo "1. under both limits: silent, nothing deleted"
newroot 1
mkfile "$ROOT/target/debug/a.bin" 1
mkfile "$ROOT/target/debug/incremental/s/x" 1
age "$ROOT/target/debug/incremental/s" $OLD
out=$(TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard 2>&1)
check "no output" test -z "$out"
check "stale incremental still present" test -d "$ROOT/target/debug/incremental/s"
check "cargo not called" test ! -s "$CALLS"

echo "2. over budget: only the stale incremental session is removed"
newroot 2
mkfile "$ROOT/target/debug/incremental/stale/x" 3
mkfile "$ROOT/target/debug/incremental/fresh/x" 3
mkfile "$ROOT/target/debug/deps/d.bin" 1
mkfile "$ROOT/target/release/incremental/stale2/x" 0
age "$ROOT/target/debug/incremental/stale" $OLD
age "$ROOT/target/release/incremental/stale2" $OLD
out=$(TIMEFLY_TARGET_BUDGET_MB=5 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard 2>&1)
check "stale session removed" test ! -e "$ROOT/target/debug/incremental/stale"
check "fresh session kept" test -d "$ROOT/target/debug/incremental/fresh"
check "deps kept" test -f "$ROOT/target/debug/deps/d.bin"
check "report mentions removal" grep -qi 'removed' <<<"$out"
check "no cargo clean/sweep" test ! -s "$CALLS"

newroot ancestor-link
mkfile "$ROOT/target/debug/a.bin" 2
mkfile "$TMP/external/incremental/stale/x" 1
age "$TMP/external/incremental/stale" $OLD
ln -s "$TMP/external" "$ROOT/target/release"
TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=0 guard >/dev/null
check "symlinked profile keeps external incremental session" test -f "$TMP/external/incremental/stale/x"

echo "3. still over after step 1: least recently modified stale profile removed, newest kept"
newroot 3
mkfile "$ROOT/target/release/r.bin" 4;                 age "$ROOT/target/release" 202001010000
mkfile "$ROOT/target/wasm32-unknown-unknown/w.bin" 4;  age "$ROOT/target/wasm32-unknown-unknown" 202101010000
mkfile "$ROOT/target/debug/d.bin" 4
TIMEFLY_TARGET_BUDGET_MB=9 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "oldest (release) removed" test ! -e "$ROOT/target/release"
check "next-oldest kept once under budget" test -d "$ROOT/target/wasm32-unknown-unknown"
check "newest (debug) kept" test -d "$ROOT/target/debug"
TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "second stale dir removed when still over" test ! -e "$ROOT/target/wasm32-unknown-unknown"
check "newest never removed even if still over" test -d "$ROOT/target/debug"
check "fresh (non-stale) dir is never removed in step 3" bash -c '
  cd "$0" && mkdir -p target/scratch-x && echo 0123456789 > target/scratch-x/f && touch target/scratch-x target/scratch-x/f &&
  sleep 1 && touch target/debug target/debug/d.bin &&
  TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 TIMEFLY_GUARD_ROOT="$0" "$1" >/dev/null 2>&1;
  test -d target/scratch-x' "$ROOT" "$GUARD"

echo "3b. newest is protected even when every dir is stale"
newroot 3b
mkfile "$ROOT/target/release/r.bin" 2;                age "$ROOT/target/release" 202001010000
mkfile "$ROOT/target/wasm32-unknown-unknown/w.bin" 2; age "$ROOT/target/wasm32-unknown-unknown" 202101010000
mkfile "$ROOT/target/scratch-a/s.bin" 2;              age "$ROOT/target/scratch-a" 202201010000
mkfile "$ROOT/target/debug/d.bin" 2;                  age "$ROOT/target/debug" 202301010000
TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "newest stale dir (debug) survives" test -d "$ROOT/target/debug"
check "all older dirs removed" bash -c 'cd "$1/target" && test ! -e release && test ! -e wasm32-unknown-unknown && test ! -e scratch-a' _ "$ROOT"

echo "4. nothing outside target/ is touched; symlinked target is refused"
newroot 4
mkfile "$ROOT/keep.txt" 1
mkfile "$ROOT/other/x.bin" 1
mkfile "$ROOT/target/release/r.bin" 4; age "$ROOT/target" $OLD
before=$(cd "$ROOT" && find . -path ./target -prune -o -type f -print | sort | xargs cksum)
TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
after=$(cd "$ROOT" && find . -path ./target -prune -o -type f -print | sort | xargs cksum)
check "outside files unchanged" test "$before" = "$after"
newroot 4b
rm -rf "$ROOT/target"
mkfile "$TMP/elsewhere/release/r.bin" 4; age "$TMP/elsewhere" $OLD
ln -s "$TMP/elsewhere" "$ROOT/target"
out=$(TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard 2>&1); rc=$?
check "symlink: exit 0" test "$rc" -eq 0
check "symlink: warns" grep -qi 'symlink' <<<"$out"
check "symlink: target contents intact" test -f "$TMP/elsewhere/release/r.bin"
newroot 4c
rmdir "$ROOT/target"
out=$(guard 2>&1); rc=$?
check "missing target: exit 0 with warning" bash -c 'test "$1" -eq 0 && test -n "$2"' _ "$rc" "$out"

echo "4d. CARGO_TARGET_DIR outside the repo is never what cargo clean/sweep act on"
newroot 4d
mkdir -p "$TMP/outside-target"
mkfile "$ROOT/target/debug/d.bin" 1
CARGO_TARGET_DIR="$TMP/outside-target" TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=10 guard >/dev/null 2>&1
check "clean called with --target-dir <root>/target" grep -qF -- "clean --target-dir $ROOT/target " "$CALLS"
check "CARGO_TARGET_DIR not in cargo clean env" grep -q 'CTD=unset' "$CALLS"
printf '#!/bin/sh\nexit 0\n' > "$FAKEBIN/cargo-sweep"; chmod +x "$FAKEBIN/cargo-sweep"
mkfile "$ROOT/target/release/r.bin" 4; age "$ROOT/target/release" $OLD
: > "$CALLS"
CARGO_TARGET_DIR="$TMP/outside-target" TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "sweep pointed at <root>/target" grep -qF "sweep --time 2 | CTD=$ROOT/target" "$CALLS"
rm -f "$FAKEBIN/cargo-sweep"

echo "4e. hook mode is silent on warnings; bad env values never fail"
newroot 4e
rmdir "$ROOT/target"
out=$(echo '{"tool_input":{"command":"cargo build"}}' | guard --hook 2>&1); rc=$?
check "hook, missing target: silent, exit 0" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
mkdir -p "$TMP/real"; ln -s "$TMP/real" "$ROOT/target"
out=$(echo '{"tool_input":{"command":"cargo build"}}' | guard --hook 2>&1); rc=$?
check "hook, symlinked target: silent, exit 0" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
newroot 4f
mkfile "$ROOT/target/debug/d.bin" 1
out=$(echo '{"tool_input":{"command":"cargo build"}}' | TIMEFLY_TARGET_BUDGET_MB=abc TIMEFLY_MIN_FREE_MB=1x2 TIMEFLY_FAKE_FREE_MB=zz guard --hook 2>&1); rc=$?
check "hook, non-numeric env: exit 0" test "$rc" -eq 0
check "hook, non-numeric env: valid-or-empty output, no shell errors" bash -c '! grep -qi "syntax\|arithmetic\|error" <<<"$1"' _ "$out"
out=$(TIMEFLY_TARGET_BUDGET_MB=abc TIMEFLY_MIN_FREE_MB=1x2 guard --status 2>&1); rc=$?
check "manual, non-numeric env: exit 0 and warns" bash -c 'test "$1" -eq 0 && grep -q "not a number" <<<"$2"' _ "$rc" "$out"
check "manual, non-numeric env: falls back to defaults" grep -q 'budget 15360 MB' <<<"$out"

echo "5. --hook"
newroot 5
mkfile "$ROOT/target/debug/incremental/stale/x" 3
mkfile "$ROOT/target/debug/incremental/fresh/x" 3
age "$ROOT/target/debug/incremental/stale" $OLD
export TIMEFLY_TARGET_BUDGET_MB=5 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000
out=$(echo '{"tool_name":"Bash","tool_input":{"command":"ls -la"}}' | guard --hook 2>&1); rc=$?
check "non-cargo command: no output, exit 0" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
check "non-cargo command: nothing pruned" test -d "$ROOT/target/debug/incremental/stale"
out=$(echo '{"tool_name":"Bash","tool_input":{"command":"cargo build -p timefly"}}' | guard --hook 2>&1); rc=$?
check "cargo command: exit 0" test "$rc" -eq 0
check "cargo command: pruned" test ! -e "$ROOT/target/debug/incremental/stale"
check "cargo command: valid JSON" python3 -c 'import json,sys; json.load(sys.stdin)' <<<"$out"
check "cargo command: has systemMessage and hookSpecificOutput" python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["systemMessage"]
h=d["hookSpecificOutput"]
assert h["hookEventName"]=="PostToolUse" and h["additionalContext"]' <<<"$out"
out=$(echo '{"tool_name":"Bash","tool_input":{"command":"cargo test"}}' | guard --hook 2>&1)
check "cargo command, nothing to do: silent" test -z "$out"
unset TIMEFLY_TARGET_BUDGET_MB TIMEFLY_MIN_FREE_MB TIMEFLY_FAKE_FREE_MB

echo "6. --status deletes nothing"
newroot 6
mkfile "$ROOT/target/release/r.bin" 4; age "$ROOT/target" $OLD
out=$(TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard --status 2>&1)
check "files intact" test -f "$ROOT/target/release/r.bin"
check "prints budget" grep -qi 'budget' <<<"$out"
check "cargo not called" test ! -s "$CALLS"

echo "7. cargo clean / cargo sweep / running builds"
newroot 7
mkfile "$ROOT/target/debug/d.bin" 1
TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "enough free space: cargo clean not called" test ! -s "$CALLS"
TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=10 guard >/dev/null 2>&1
check "free space stays low: cargo clean called" grep -q '^clean' "$CALLS"
check "cargo sweep not invoked when absent" bash -c '! grep -q sweep "$1"' _ "$CALLS"
: > "$CALLS"
printf '#!/bin/sh\nexit 0\n' > "$FAKEBIN/pgrep"; chmod +x "$FAKEBIN/pgrep"   # a build is running
out=$(TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=10 guard 2>&1)
check "build running: cargo clean skipped" test ! -s "$CALLS"
check "build running: skip is reported" grep -qi 'skipped' <<<"$out"
mkfile "$ROOT/target/release/r.bin" 4; age "$ROOT/target/release" $OLD
TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "build running: stale profile dir kept" test -d "$ROOT/target/release"
printf '#!/bin/sh\nexit 1\n' > "$FAKEBIN/pgrep"; chmod +x "$FAKEBIN/pgrep"
printf '#!/bin/sh\nexit 0\n' > "$FAKEBIN/cargo-sweep"; chmod +x "$FAKEBIN/cargo-sweep"
: > "$CALLS"
TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "cargo-sweep on PATH: 'cargo sweep --time 2' invoked" grep -q '^sweep --time 2' "$CALLS"
rm -f "$FAKEBIN/cargo-sweep"

# hook_json <command>: the PostToolUse/PreToolUse payload for a Bash command.
hook_json() { python3 -c 'import json,sys; print(json.dumps({"tool_name":"Bash","tool_input":{"command":sys.argv[1]}}))' "$1"; }

echo "8. --hook prunes only after real cargo invocations, not mentions"
# cargo_case <yes|no> <command>: over budget with a stale session; pruned only for cargo runs.
cargo_case() {
  newroot cmd
  mkfile "$ROOT/target/debug/incremental/stale/x" 1
  age "$ROOT/target/debug/incremental/stale" $OLD
  hook_json "$2" | TIMEFLY_TARGET_BUDGET_MB=0 TIMEFLY_MIN_FREE_MB=0 TIMEFLY_WARN_FREE_MB=0 TIMEFLY_FAKE_FREE_MB=1000 \
    guard --hook >/dev/null 2>&1
  local label; label="$(printf '%s' "$2" | tr '\n' '~')"
  if [ "$1" = yes ]; then
    check "cargo run:   $label" test ! -e "$ROOT/target/debug/incremental/stale"
  else
    check "not cargo:   $label" test -d "$ROOT/target/debug/incremental/stale"
  fi
}
NL='
'
cargo_case yes 'cargo build'
cargo_case yes '  cargo test -p timefly'
cargo_case yes 'cd crates && cargo test'
cargo_case yes 'false || cargo build'
cargo_case yes 'echo hi; cargo build'
cargo_case yes 'echo x | cargo run'
cargo_case yes '(cargo build)'
cargo_case yes "echo a${NL}cargo build"
cargo_case yes 'v=$(cargo metadata --format-version 1)'
cargo_case yes 'FOO=1 PATH="/a b:$PATH" cargo build'
cargo_case yes 'env CARGO_TARGET_DIR=target/scratch-x cargo build'
cargo_case yes 'env -u CARGO_TARGET_DIR cargo clean'
cargo_case yes 'time cargo build'
cargo_case yes 'nice cargo build'
cargo_case yes 'nice -n 10 cargo build'
cargo_case yes 'command cargo build'
cargo_case yes 'direnv exec . cargo build'
cargo_case yes '/opt/homebrew/opt/rustup/bin/cargo build'
cargo_case yes './bin/cargo build'
cargo_case yes 'PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo build -p timefly 2>&1 | tail -5'
cargo_case no  "pgrep -fl 'cargo|rustc'"
cargo_case no  'grep cargo x'
cargo_case no  'ls ~/.cargo/bin'
cargo_case no  'cat Cargo.toml'
cargo_case no  'echo "run cargo test"'
cargo_case no  'echo cargo'
cargo_case no  'command -v cargo'
cargo_case no  'which cargo'
cargo_case no  '# cargo build'
cargo_case no  'cargo-sweep --help'
cargo_case no  'git commit -m "fix cargo build"'
cargo_case no  "bash -c 'echo nothing' # cargo"

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
