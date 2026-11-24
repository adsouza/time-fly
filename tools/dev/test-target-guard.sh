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
unset TIMEFLY_FAKE_FREE_MB TIMEFLY_TARGET_BUDGET_MB TIMEFLY_MIN_FREE_MB TIMEFLY_WARN_FREE_MB TIMEFLY_XCTRACE_MIN_FREE_MB

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
out=$(echo '{"tool_input":{"command":"cargo build"}}' | TIMEFLY_WARN_FREE_MB=0 guard --hook 2>&1); rc=$?
check "hook, missing target: silent, exit 0" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
mkdir -p "$TMP/real"; ln -s "$TMP/real" "$ROOT/target"
out=$(echo '{"tool_input":{"command":"cargo build"}}' | TIMEFLY_WARN_FREE_MB=0 guard --hook 2>&1); rc=$?
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
export TIMEFLY_TARGET_BUDGET_MB=5 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 TIMEFLY_WARN_FREE_MB=500
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
unset TIMEFLY_TARGET_BUDGET_MB TIMEFLY_MIN_FREE_MB TIMEFLY_FAKE_FREE_MB TIMEFLY_WARN_FREE_MB

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

echo "9. --hook warns about low free space after any command, deletes nothing outside target/"
# sparse <path> <MB>: apparent size only, no real disk used.
sparse() { mkdir -p "$(dirname "$1")"; dd if=/dev/zero of="$1" bs=1048576 seek="$2" count=0 2>/dev/null; }
ctx_of() { python3 -c 'import json,sys; print(json.load(sys.stdin)["hookSpecificOutput"]["additionalContext"])' <<<"$1" 2>/dev/null; }
CUL="$TMP/tmpdir"
sparse "$CUL/instrumentsA.ktrace" 900
sparse "$CUL/instrumentsB.ktrace" 700
sparse "$CUL/c.bin" 600
sparse "$CUL/d.bin" 550
sparse "$CUL/e.bin" 520
sparse "$CUL/f.bin" 510           # 6th largest: over the limit of 5
sparse "$CUL/small.bin" 499       # under 500 MB
sparse "$CUL/sub/nested.bin" 800  # not directly in TMPDIR
newroot 9
mkfile "$ROOT/target/debug/d.bin" 2
export TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100
out=$(hook_json 'ls -la' | TIMEFLY_FAKE_FREE_MB=20000 TMPDIR="$CUL" guard --hook 2>&1); rc=$?
check "free above the default 10240 MB threshold: silent, exit 0" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
out=$(hook_json 'ls -la' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1); rc=$?
check "low free, non-cargo command: exit 0" test "$rc" -eq 0
check "low free: PostToolUse JSON with systemMessage and additionalContext" python3 -c '
import json,sys
d=json.load(sys.stdin)
assert d["systemMessage"]
h=d["hookSpecificOutput"]
assert h["hookEventName"]=="PostToolUse" and h["additionalContext"]' <<<"$out"
ctx="$(ctx_of "$out")"
check "warning gives free space" grep -q '3000 MB free' <<<"$ctx"
check "warning gives target/ size" grep -q 'target/: 2 MB' <<<"$ctx"
check "warning lists the ktrace culprit with its size" grep -q "900 MB $CUL/instrumentsA.ktrace" <<<"$ctx"
check "culprits largest first" python3 -c '
import sys
c=sys.stdin.read(); names=["instrumentsA.ktrace","instrumentsB.ktrace","c.bin","d.bin","e.bin"]
pos=[c.find("/"+n) for n in names]
assert all(p>=0 for p in pos) and pos==sorted(pos), pos' <<<"$ctx"
check "at most 5 culprits" bash -c '! grep -q "f\.bin" <<<"$1"' _ "$ctx"
check "files under 500 MB not listed" bash -c '! grep -q "small\.bin" <<<"$1"' _ "$ctx"
check "files in subdirectories not listed" bash -c '! grep -q "nested\.bin" <<<"$1"' _ "$ctx"
check "says the guard never deletes outside target/" grep -qi 'never deletes anything outside target/' <<<"$ctx"
check "says the owner must approve deleting them" grep -qi 'owner' <<<"$ctx"
check "culprits left in place" bash -c 'for f in instrumentsA.ktrace instrumentsB.ktrace c.bin f.bin small.bin sub/nested.bin; do test -f "$1/$f" || exit 1; done' _ "$CUL"
check "target/ untouched by a non-cargo command" test -f "$ROOT/target/debug/d.bin"
out=$(hook_json 'ls -la' | TIMEFLY_WARN_FREE_MB=2000 TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "TIMEFLY_WARN_FREE_MB sets the threshold" test -z "$out"
mkdir -p "$TMP/cul2"; sparse "$TMP/cul2/x500.bin" 500; sparse "$TMP/cul2/y499.bin" 499
ctx="$(ctx_of "$(hook_json 'ls' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$TMP/cul2/" guard --hook 2>/dev/null)")"
check "a file of exactly 500 MB is listed" grep -q "500 MB $TMP/cul2/x500.bin" <<<"$ctx"
mkdir -p "$TMP/cul-empty"
ctx="$(ctx_of "$(hook_json 'ls' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$TMP/cul-empty" guard --hook 2>/dev/null)")"
check "no culprits: says none found" grep -qi 'no files of 500 MB or more' <<<"$ctx"
out=$(hook_json 'ls' | env -u TMPDIR TIMEFLY_FAKE_FREE_MB=3000 TIMEFLY_GUARD_ROOT="$ROOT" "$GUARD" --hook 2>&1); rc=$?
check "TMPDIR unset: exit 0, JSON only" bash -c 'test "$1" -eq 0 && python3 -c "import json,sys; json.load(sys.stdin)" <<<"$2"' _ "$rc" "$out"
out=$(hook_json 'cargo test' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "cargo command with nothing to prune: still warns" grep -q '3000 MB free' <<<"$(ctx_of "$out")"
rm -rf "$ROOT/target"
out=$(hook_json 'cargo test' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "cargo command, missing target/: still warns" grep -q 'target/: absent' <<<"$(ctx_of "$out")"
mkdir -p "$ROOT/target/debug"
# Cheap path: one df, no du, quick.
TOOLLOG="$TMP/toollog"
for t in du df; do
  printf '#!/bin/sh\necho %s >> "%s"\nexec %s "$@"\n' "$t" "$TOOLLOG" "$(command -v "$t")" > "$FAKEBIN/$t"
  chmod +x "$FAKEBIN/$t"
done
: > "$TOOLLOG"
hook_json 'ls -la' | TIMEFLY_WARN_FREE_MB=0 guard --hook >/dev/null 2>&1
check "cheap path: exactly one df call" test "$(grep -c '^df$' "$TOOLLOG")" -eq 1
check "cheap path: no du call" bash -c '! grep -q "^du$" "$1"' _ "$TOOLLOG"
rm -f "$FAKEBIN/du" "$FAKEBIN/df"
check "cheap path: under 0.5 s" python3 -c '
import os,subprocess,sys,time
env=dict(os.environ, TIMEFLY_WARN_FREE_MB="0", TIMEFLY_GUARD_ROOT=sys.argv[2])
best=min((lambda t0: (subprocess.run([sys.argv[1],"--hook"],input=b"{\"tool_input\":{\"command\":\"ls -la\"}}",env=env,capture_output=True), time.monotonic()-t0)[1])(time.monotonic()) for _ in range(3))
assert best < 0.5, best' "$GUARD" "$ROOT"
unset TIMEFLY_TARGET_BUDGET_MB TIMEFLY_MIN_FREE_MB

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
