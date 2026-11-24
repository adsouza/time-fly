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

echo "4d. CARGO_TARGET_DIR outside the repo is never what the last resort/sweep act on"
newroot 4d
mkfile "$TMP/outside-target/debug/o.bin" 1
mkfile "$ROOT/target/debug/d.bin" 4
CARGO_TARGET_DIR="$TMP/outside-target" TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=98 guard >/dev/null 2>&1
check "last resort removed <root>/target/debug" test ! -e "$ROOT/target/debug"
check "outside CARGO_TARGET_DIR untouched" test -f "$TMP/outside-target/debug/o.bin"
check "cargo not called for the last resort" test ! -s "$CALLS"
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
mkfile "$ROOT/target/debug/d.bin" 2
TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=1000 guard >/dev/null 2>&1
check "enough free space: nothing removed, cargo not called" bash -c 'test -f "$1/target/debug/d.bin" && test ! -s "$2"' _ "$ROOT" "$CALLS"
TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=99 guard >/dev/null 2>&1
check "free space stays low: cargo clean never called" bash -c '! grep -q clean "$1"' _ "$CALLS"
check "free space stays low: last resort removed the build dir" test ! -e "$ROOT/target/debug"
check "cargo sweep not invoked when absent" bash -c '! grep -q sweep "$1"' _ "$CALLS"
: > "$CALLS"
mkfile "$ROOT/target/debug/d.bin" 2
printf '#!/bin/sh\nexit 0\n' > "$FAKEBIN/pgrep"; chmod +x "$FAKEBIN/pgrep"   # a build is running
out=$(TIMEFLY_TARGET_BUDGET_MB=50 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=99 guard 2>&1)
check "build running: last resort skipped" bash -c 'test -f "$1/target/debug/d.bin" && test ! -s "$2"' _ "$ROOT" "$CALLS"
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
check "non-cargo warning does not measure target/ (no du)" bash -c '! grep -q "target/: [0-9]" <<<"$1"' _ "$ctx"
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
check "cargo command warning gives target/ size" grep -q 'target/: 2 MB' <<<"$(ctx_of "$out")"
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
: > "$TOOLLOG"
out=$(hook_json 'ls -la' | TIMEFLY_WARN_FREE_MB=999999999 TMPDIR="$CUL" guard --hook 2>/dev/null)
check "below-threshold warning path: warns" grep -q 'low disk space' <<<"$out"
check "below-threshold warning path: exactly one df call" test "$(grep -c '^df$' "$TOOLLOG")" -eq 1
check "below-threshold warning path: no du call" bash -c '! grep -q "^du$" "$1"' _ "$TOOLLOG"
rm -f "$FAKEBIN/du" "$FAKEBIN/df"
check "cheap path: under 0.5 s" python3 -c '
import os,subprocess,sys,time
env=dict(os.environ, TIMEFLY_WARN_FREE_MB="0", TIMEFLY_GUARD_ROOT=sys.argv[2])
best=min((lambda t0: (subprocess.run([sys.argv[1],"--hook"],input=b"{\"tool_input\":{\"command\":\"ls -la\"}}",env=env,capture_output=True), time.monotonic()-t0)[1])(time.monotonic()) for _ in range(3))
assert best < 0.5, best' "$GUARD" "$ROOT"
printf '#!/bin/sh\nexit 1\n' > "$FAKEBIN/df"; chmod +x "$FAKEBIN/df"   # df fails
out=$(hook_json 'cargo build' | TIMEFLY_WARN_FREE_MB=999999 guard --hook 2>&1); rc=$?
check "df failing: exit 0, silent, nothing deleted" bash -c 'test "$1" -eq 0 && test -z "$2" && test -d "$3/target/debug"' _ "$rc" "$out" "$ROOT"
out=$(hook_json 'xctrace record --time-limit 5s --template X' | guard --pre-hook 2>&1); rc=$?
check "df failing: pre-hook allows a bounded recording" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
rm -f "$FAKEBIN/df"
unset TIMEFLY_TARGET_BUDGET_MB TIMEFLY_MIN_FREE_MB

echo "10. --pre-hook denies runaway xctrace recordings, allows everything else"
newroot 10
# decision <hook output>: deny / allow / other, checking the PreToolUse shape.
decision() { python3 -c '
import json,sys
raw=sys.stdin.read()
if not raw.strip(): print("allow"); sys.exit()
h=json.loads(raw)["hookSpecificOutput"]
assert h["hookEventName"]=="PreToolUse" and h["permissionDecisionReason"]
print(h["permissionDecision"])' <<<"$1" 2>/dev/null || echo other; }
reason_of() { python3 -c 'import json,sys; print(json.load(sys.stdin)["hookSpecificOutput"]["permissionDecisionReason"])' <<<"$1" 2>/dev/null; }
pre() { hook_json "$2" | TIMEFLY_FAKE_FREE_MB="$1" TMPDIR="$CUL" guard --pre-hook 2>/dev/null; }
# xcase <deny|allow> <free MB> <command>
xcase() {
  local out rc label
  out="$(pre "$2" "$3")"; rc=$?
  label="$(printf '%s' "$3" | tr '\n' '~')"
  if [ "$1" = deny ]; then
    check "deny  ($2 MB free): $label" bash -c 'test "$1" -eq 0 && test "$2" = deny' _ "$rc" "$(decision "$out")"
  else
    check "allow ($2 MB free): $label" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
  fi
}
xcase deny  20000 "xctrace record --template 'Time Profiler' --launch -- ./game"
xcase deny  20000 "xcrun xctrace record --template 'Metal System Trace' --attach 123 --output /tmp/x.trace"
xcase deny  20000 "/usr/bin/xcrun xctrace record --template X --all-processes"
xcase deny  20000 "xcrun -sdk macosx xctrace record --template X --all-processes"
xcase deny  20000 "/Applications/Xcode.app/Contents/Developer/usr/bin/xctrace record --template X"
xcase deny  20000 "cd /tmp && xctrace record --template X --launch -- ls"
xcase deny  20000 "TMPDIR=/tmp/x nice xctrace record --template X"
xcase deny  20000 "xctrace record --template X; sleep 1 --time-limit 5s"
xcase deny  20000 "xctrace record --template X --launch -- ./game --time-limit 5s"
xcase deny  5000  "xctrace record --time-limit 30s --template X --launch -- ./game"
xcase allow 20000 "xctrace record --time-limit 30s --template X --launch -- ./game"
xcase allow 20000 "xcrun xctrace record --template X --time-limit=30s --all-processes"
xcase allow 20000 "xctrace list devices"
xcase allow 20000 "xcrun xctrace export --input a.trace --toc"
xcase allow 20000 "xctrace help record"
xcase allow 20000 'echo "xctrace record"'
xcase allow 20000 "grep -n 'xctrace record' notes.md"
xcase allow 20000 "man xctrace"
xcase allow 20000 "cargo build"
xcase allow 5000  "ls -la"
out=$(hook_json "xctrace record --time-limit 30s --template X" | TIMEFLY_XCTRACE_MIN_FREE_MB=1000 TIMEFLY_FAKE_FREE_MB=5000 guard --pre-hook 2>&1)
check "TIMEFLY_XCTRACE_MIN_FREE_MB lowers the free-space floor" test -z "$out"
out=$(hook_json "xctrace record --template X" | TIMEFLY_XCTRACE_MIN_FREE_MB=1000 TIMEFLY_FAKE_FREE_MB=5000 guard --pre-hook 2>&1)
check "no --time-limit is denied whatever the floor" test "$(decision "$out")" = deny
r="$(reason_of "$(pre 20000 "xctrace record --template X --all-processes")")"
check "reason: says --time-limit is missing" grep -q -- '--time-limit' <<<"$r"
check "reason: says where the raw trace goes" grep -q 'instruments\*\.ktrace' <<<"$r"
check "reason: names TMPDIR" grep -q "$CUL" <<<"$r"
check "reason: cites the incident" grep -q '2026-11-24' <<<"$r"
check "reason: suggests ioreg sampling" grep -q 'ioreg' <<<"$r"
r="$(reason_of "$(pre 5000 "xctrace record --time-limit 30s --template X")")"
check "low-free reason: gives free space and floor" bash -c 'grep -q "5000 MB free" <<<"$1" && grep -q "10240 MB" <<<"$1"' _ "$r"
check "low-free reason: says to free space first" grep -qi 'free space first' <<<"$r"
check "low-free reason: does not claim --time-limit is missing" bash -c '! grep -qi "no --time-limit" <<<"$1"' _ "$r"
for bad in 'not json' '' '{"tool_input":{}}' '{"tool_input":{"command":null}}'; do
  out=$(printf '%s' "$bad" | TIMEFLY_FAKE_FREE_MB=1 guard --pre-hook 2>&1); rc=$?
  check "fail open on input '$bad'" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
done
NOPY="$TMP/nopy"; mkdir -p "$NOPY"
for t in bash cat dirname awk df du find sort head uname stat tr; do ln -s "$(command -v "$t")" "$NOPY/$t"; done
out=$(hook_json "xctrace record --template X" | PATH="$NOPY" TIMEFLY_FAKE_FREE_MB=1 TIMEFLY_GUARD_ROOT="$ROOT" "$GUARD" --pre-hook 2>&1); rc=$?
check "fail open without jq or python3" bash -c 'test "$1" -eq 0 && test -z "$2"' _ "$rc" "$out"
check ".claude/settings.json wires --pre-hook as a PreToolUse Bash hook" python3 -c '
import json,sys
h=json.load(open(sys.argv[1]))["hooks"]
pre=[x for e in h["PreToolUse"] if e["matcher"]=="Bash" for x in e["hooks"]]
assert any(x["type"]=="command" and x["command"].endswith("tools/dev/target-guard.sh --pre-hook") for x in pre)
post=[x for e in h["PostToolUse"] if e["matcher"]=="Bash" for x in e["hooks"]]
assert any(x["command"].endswith("tools/dev/target-guard.sh --hook") for x in post)' "$SCRIPT_DIR/../../.claude/settings.json"

echo "11. last resort for low free space: whole build dirs in a fixed order, only as many as needed"
# five fresh 4 MB build dirs: fresh, so the 24 h rule would protect all of them in step 3
profiles() {
  newroot "$1"
  for d in release wasm32-unknown-unknown scratch-a doc debug; do mkfile "$ROOT/target/$d/x.bin" 4; done
}
lr() { TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB="$1" TMPDIR="$CUL" guard 2>&1; }
left() { (cd "$ROOT/target" && for d in release wasm32-unknown-unknown scratch-a doc debug; do [ -e "$d" ] && printf '%s ' "$d"; done); }
profiles 11a; out="$(lr 95)"   # 5 MB short
check "5 MB short: release, then the triple dir; rest kept" test "$(left)" = "scratch-a doc debug "
check "report names what was deleted" bash -c 'grep -q "target/release" <<<"$1" && grep -q "target/wasm32-unknown-unknown" <<<"$1"' _ "$out"
check "report says why (last resort, free space under the minimum)" grep -qi 'last resort.*free' <<<"$out"
check "cargo clean never called" bash -c '! grep -q clean "$1"' _ "$CALLS"
profiles 11b; lr 89 >/dev/null   # 11 MB short
check "11 MB short: then scratch-*" test "$(left)" = "doc debug "
profiles 11c; lr 85 >/dev/null   # 15 MB short
check "15 MB short: then other dirs, debug kept" test "$(left)" = "debug "
profiles 11d; out="$(lr 82)"     # 18 MB short, 20 MB in target/
check "18 MB short: every build dir goes, debug last" test -z "$(left)"
check "every dir gone is reported as a full clean" grep -qi 'every build dir' <<<"$out"
profiles 11e
mkfile "$ROOT/target/debug/incremental/stale/x" 1; age "$ROOT/target/debug/incremental/stale" $OLD
age "$ROOT/target/release" $OLD
out="$(lr 60)"                   # 40 MB short, only 21 MB in target/
check "short by more than target/ holds: nothing in target/ deleted" bash -c 'test "$(cd "$1/target" && ls | tr "\n" " ")" = "debug doc release scratch-a wasm32-unknown-unknown " && test -d "$1/target/debug/incremental/stale"' _ "$ROOT"
check "...and says deleting builds would not fix it" grep -qi 'nothing in target/ was deleted' <<<"$out"
check "...and lists the culprits outside target/" grep -q "instrumentsA.ktrace" <<<"$out"
check "...and the culprits are left in place" test -f "$CUL/instrumentsA.ktrace"
profiles 11f
rm -rf "$ROOT/target/release"; mkfile "$TMP/ext-release/x.bin" 4; ln -s "$TMP/ext-release" "$ROOT/target/release"
lr 95 >/dev/null
check "symlinked build dir: neither followed nor removed" bash -c 'test -L "$1/target/release" && test -f "$2/ext-release/x.bin"' _ "$ROOT" "$TMP"
check "symlinked build dir: the next ones go instead" test "$(left)" = "release doc debug "
profiles 11g
out=$(hook_json 'cargo build' | TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=95 TMPDIR="$CUL" guard --hook 2>&1)
check "hook: last resort runs after a cargo command" test "$(left)" = "scratch-a doc debug "
check "hook: reports what and why" python3 -c '
import json,sys
d=json.load(sys.stdin); c=d["hookSpecificOutput"]["additionalContext"]
assert "target/release" in d["systemMessage"] and "last resort" in c.lower(), c' <<<"$out"
profiles 11h
hook_json 'ls -la' | TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=95 TMPDIR="$CUL" guard --hook >/dev/null 2>&1
check "hook: no last resort after a non-cargo command" test "$(left)" = "release wasm32-unknown-unknown scratch-a doc debug "

echo "12. failed commands (PostToolUseFailure) get the same hook, answered under their own event"
# hook_json_ev <event> <command>
hook_json_ev() { python3 -c 'import json,sys; print(json.dumps({"hook_event_name":sys.argv[1],"tool_name":"Bash","tool_input":{"command":sys.argv[2]},"error":"exit 1","is_interrupt":False}))' "$1" "$2"; }
event_of() { python3 -c 'import json,sys; print(json.load(sys.stdin)["hookSpecificOutput"]["hookEventName"])' <<<"$1" 2>/dev/null; }
newroot 12
mkfile "$ROOT/target/debug/d.bin" 1
out=$(hook_json_ev PostToolUseFailure 'ls nope' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "failure event, low free: warns" grep -q '3000 MB free' <<<"$(ctx_of "$out")"
check "failure event, low free: hookEventName PostToolUseFailure" test "$(event_of "$out")" = PostToolUseFailure
mkfile "$ROOT/target/debug/incremental/stale/x" 1; age "$ROOT/target/debug/incremental/stale" $OLD
out=$(hook_json_ev PostToolUseFailure 'cargo test' | TIMEFLY_TARGET_BUDGET_MB=1 TIMEFLY_MIN_FREE_MB=0 TIMEFLY_WARN_FREE_MB=0 TIMEFLY_FAKE_FREE_MB=1000 guard --hook 2>&1)
check "failure event, failed cargo command: pruned" test ! -e "$ROOT/target/debug/incremental/stale"
check "failure event, prune report: hookEventName PostToolUseFailure" test "$(event_of "$out")" = PostToolUseFailure
out=$(hook_json_ev PostToolUse 'ls' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "PostToolUse event: answered as PostToolUse" test "$(event_of "$out")" = PostToolUse
out=$(hook_json_ev SomethingElse 'ls' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "unknown event: answered as PostToolUse" test "$(event_of "$out")" = PostToolUse
out=$(hook_json 'ls' | TIMEFLY_FAKE_FREE_MB=3000 TMPDIR="$CUL" guard --hook 2>&1)
check "missing event: answered as PostToolUse" test "$(event_of "$out")" = PostToolUse
check ".claude/settings.json wires --hook under PostToolUseFailure for Bash" python3 -c '
import json,sys
h=json.load(open(sys.argv[1]))["hooks"]
f=[x for e in h["PostToolUseFailure"] if e["matcher"]=="Bash" for x in e["hooks"]]
assert any(x["type"]=="command" and x["command"].endswith("tools/dev/target-guard.sh --hook") for x in f)' "$SCRIPT_DIR/../../.claude/settings.json"

echo "13. huge commands without the keyword skip the tokenizer"
newroot 13
BIG="$TMP/big.json"
python3 -c 'import json,sys; json.dump({"tool_name":"Bash","tool_input":{"command":"echo \"some text; more | words && (x)\" \n" * 12000}}, open(sys.argv[1],"w"))' "$BIG"
check "fixture is ~400 KB" test "$(wc -c < "$BIG")" -gt 400000
for m in --pre-hook --hook; do
  check "$m: 400 KB command without the keyword in under 1 s" python3 -c '
import os,subprocess,sys,time
env=dict(os.environ, TIMEFLY_WARN_FREE_MB="0", TIMEFLY_GUARD_ROOT=sys.argv[3])
t0=time.monotonic()
r=subprocess.run([sys.argv[1],sys.argv[2]],stdin=open(sys.argv[4],"rb"),env=env,capture_output=True)
dt=time.monotonic()-t0
assert r.returncode==0 and not r.stdout, r
assert dt < 1.0, dt' "$GUARD" "$m" "$ROOT" "$BIG"
done

echo "14. xctrace record behind prefixes, nested shells and command substitutions"
newroot 14
xcase deny  20000 "nohup xctrace record --template X --all-processes &"
xcase deny  20000 "timeout 60 xctrace record --template X --all-processes"
xcase deny  20000 "timeout -s INT 60 xctrace record --template X"
xcase deny  20000 "timeout --signal=INT -k 5 60 xcrun xctrace record --template X"
xcase deny  20000 "exec xctrace record --template X"
xcase deny  20000 "sudo xctrace record --template X --all-processes"
xcase deny  20000 "sudo -u root -E xctrace record --template X"
xcase deny  20000 "bash -c 'xctrace record --template X --all-processes'"
xcase deny  20000 "sh -c \"cd /tmp && xcrun xctrace record --template X\""
xcase deny  20000 "zsh -lc 'xctrace record --template X'"
xcase deny  20000 "/bin/bash -c 'nohup xctrace record --template X &'"
xcase deny  20000 "bash -c 'cd /tmp${NL}xctrace record --template X'"
xcase deny  20000 'out="$(xctrace record --template X --all-processes)"'
xcase deny  20000 'echo "trace: $(xcrun xctrace record --template X)"'
xcase deny  20000 'out="`xctrace record --template X`"'
xcase deny  20000 "bash -c 'out=\"\$(xctrace record --template X)\"'"
xcase allow 20000 "nohup xctrace record --time-limit 30s --template X &"
xcase allow 20000 "timeout 60 xctrace record --time-limit 30s --template X"
xcase allow 20000 "bash -c 'xctrace record --time-limit 30s --template X'"
xcase allow 20000 'out="$(xctrace record --time-limit 30s --template X)"'
xcase allow 20000 "bash -c 'grep xctrace record.log'"
xcase allow 20000 "rg 'xctrace record' docs"
xcase allow 20000 "git log --grep 'xctrace record'"
xcase allow 20000 'echo "$(xctrace list devices)"'
xcase allow 20000 "xcrun simctl list devices"
xcase allow 20000 "echo 'out=\$(xctrace record --template X)'"
cargo_case yes 'timeout 600 cargo test'
cargo_case yes 'nohup cargo build &'
cargo_case yes "bash -c 'cargo build'"
cargo_case yes 'meta="$(cargo metadata --format-version 1)"'
cargo_case no  "bash -c 'echo cargo'"
cargo_case no  "echo 'x=\$(cargo build)'"

echo "15. a lock keeps concurrent guard runs from pruning at once"
profiles 15a; mkdir "$ROOT/target/.guard-lock"
out="$(lr 95)"
check "lock held: nothing deleted" test "$(left)" = "release wasm32-unknown-unknown scratch-a doc debug "
check "lock held: says so" grep -qi 'lock' <<<"$out"
check "lock held: someone else's lock is left alone" test -d "$ROOT/target/.guard-lock"
out=$(hook_json 'cargo build' | TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=95 TMPDIR="$CUL" guard --hook 2>&1)
check "lock held, hook: nothing deleted" test "$(left)" = "release wasm32-unknown-unknown scratch-a doc debug "
check "lock held, hook: still warns about low free space" grep -q '95 MB free' <<<"$(ctx_of "$out")"
profiles 15b; mkdir "$ROOT/target/.guard-lock"; touch -t $OLD "$ROOT/target/.guard-lock"
lr 95 >/dev/null
check "stale lock (> 10 min): taken over, pruning runs" test "$(left)" = "scratch-a doc debug "
check "stale lock: released afterwards" test ! -e "$ROOT/target/.guard-lock"
profiles 15c; lr 95 >/dev/null
check "manual run: lock released" test ! -e "$ROOT/target/.guard-lock"
profiles 15d
hook_json 'cargo build' | TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=95 TMPDIR="$CUL" guard --hook >/dev/null 2>&1
check "hook run: pruned and lock released" bash -c 'test "$1" = "scratch-a doc debug " && test ! -e "$2/target/.guard-lock"' _ "$(left)" "$ROOT"
profiles 15e   # mkdir of the lock fails (e.g. ENOSPC) and no lock exists: prune anyway
printf '#!/bin/sh\ncase "$*" in *.guard-lock*) exit 1 ;; esac\nexec %s "$@"\n' "$(command -v mkdir)" > "$FAKEBIN/mkdir"; chmod +x "$FAKEBIN/mkdir"
lr 95 >/dev/null
rm -f "$FAKEBIN/mkdir"
check "lock cannot be created (no lock present): pruning still runs" test "$(left)" = "scratch-a doc debug "

echo "16. last resort stops when deleting doesn't return space (snapshots, open files)"
profiles 16a
out="$(TIMEFLY_FAKE_FREE_STATIC=1 lr 95)"
check "deletions not returned: only the first dir (release) removed" test "$(left)" = "wasm32-unknown-unknown scratch-a doc debug "
check "says the space isn't being returned" grep -qi 'not being returned\|was not returned' <<<"$out"
check "suggests tmutil listlocalsnapshots /" grep -q 'tmutil listlocalsnapshots /' <<<"$out"
profiles 16b
out=$(hook_json 'cargo build' | TIMEFLY_FAKE_FREE_STATIC=1 TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TIMEFLY_FAKE_FREE_MB=95 TMPDIR="$CUL" guard --hook 2>&1)
check "hook: same stop, reported to Claude" bash -c 'test "$1" = "wasm32-unknown-unknown scratch-a doc debug " && grep -q "tmutil listlocalsnapshots" <<<"$2"' _ "$(left)" "$(ctx_of "$out")"

echo "17. heredoc bodies are text, not commands (at every nesting level)"
newroot 17
Q="'"
xcase allow 20000 "git commit -m \"\$(cat <<${Q}EOF${Q}${NL}target-guard: deny \`xctrace record\` without --time-limit${NL}EOF${NL})\""
xcase allow 20000 "gh pr create --title x --body \"\$(cat <<${Q}EOF${Q}${NL}- Blocks \`xctrace record\` unless bounded.${NL}EOF${NL})\""
xcase allow 20000 "git commit -m \"\$(cat <<${Q}EOF${Q}${NL}Summary${NL}${NL}xctrace record is now blocked${NL}EOF${NL})\""
xcase allow 20000 "git commit -F - <<${Q}EOF${Q}${NL}xctrace record is now blocked${NL}EOF"
xcase allow 20000 "cat > notes.md <<${Q}EOF${Q}${NL}Never run \`xctrace record\` bare.${NL}xctrace record --template X${NL}EOF"
xcase allow 20000 "git commit -m \"\$(cat <<${Q}EOF${Q}${NL}1) Fix \"quotes\" (and parens${NL}xctrace record --template X${NL}EOF${NL})\""
xcase allow 20000 "cat <<\"EOF\"${NL}xctrace record --template X${NL}EOF"
xcase allow 20000 "cat <<-EOF${NL}	xctrace record --template X${NL}	EOF"
xcase allow 20000 "cat <<EOF${NL}xctrace record --template X"
xcase deny  20000 "xctrace record --template X <<EOF${NL}hello${NL}EOF"
xcase deny  20000 "cat <<EOF${NL}hello${NL}EOF${NL}xctrace record --template X"
xcase deny  20000 "cat <<-EOF${NL}	hello${NL}	EOF${NL}xctrace record --template X"
xcase deny  20000 "cat <<EOF >a${NL}x${NL}EOF${NL}cat <<${Q}END${Q}${NL}y${NL}END${NL}xctrace record --template X"
xcase deny  20000 "cat <<< \"here string\"${NL}xctrace record --template X"
xcase deny  20000 "out=\"\$(cat <<EOF${NL}x${NL}EOF${NL}xctrace record --template X)\""
xcase deny  20000 "bash -c ${Q}cat <<EOF${NL}x${NL}EOF${NL}xctrace record --template X${Q}"
cargo_case no  "git commit -m \"\$(cat <<${Q}EOF${Q}${NL}Run \`cargo build\` first${NL}EOF${NL})\""
cargo_case no  "git commit -F - <<${Q}EOF${Q}${NL}cargo build fixed${NL}EOF"
cargo_case yes "cat <<EOF${NL}notes${NL}EOF${NL}cargo build"

echo "18. a failed removal is reported as such, and the last resort moves on"
profiles 18a
chmod 555 "$ROOT/target/release"   # rm -rf can't empty it (fixture only)
out="$(lr 95)"
chmod -R u+w "$ROOT/target"
check "failed removal: release kept, next dirs removed until enough" test "$(left)" = "release doc debug "
check "failed removal: reported" grep -qi 'could not delete target/release' <<<"$out"
check "failed removal: not misreported as space not returned" bash -c '! grep -qi "not being returned" <<<"$1"' _ "$out"

echo "19. stale-lock takeover is atomic (mv); losing the race skips pruning"
profiles 19a; mkdir "$ROOT/target/.guard-lock"; touch -t $OLD "$ROOT/target/.guard-lock"
printf '#!/bin/sh\ncase "$*" in *.guard-lock*) exit 1 ;; esac\nexec %s "$@"\n' "$(command -v mv)" > "$FAKEBIN/mv"; chmod +x "$FAKEBIN/mv"
out="$(lr 95)"
rm -f "$FAKEBIN/mv"
check "takeover mv fails (another run won): nothing deleted" test "$(left)" = "release wasm32-unknown-unknown scratch-a doc debug "
check "takeover mv fails: says the lock is held" grep -qi 'lock' <<<"$out"
profiles 19b; mkdir "$ROOT/target/.guard-lock"; touch -t $OLD "$ROOT/target/.guard-lock"
lr 95 >/dev/null
check "takeover: pruned, no lock or renamed stale lock left" bash -c 'test "$1" = "scratch-a doc debug " && test -z "$(ls -A "$2/target" | grep guard-lock)"' _ "$(left)" "$ROOT"

echo "20. a partly failed removal still counts: stop as soon as free space is back"
has() { (cd "$ROOT/target" && for d in release wasm32-unknown-unknown scratch-a doc debug; do [ -f "$d/x.bin" ] && printf '%s ' "$d"; done); }
profiles 20a
mkfile "$ROOT/target/release/locked/y" 0; chmod 555 "$ROOT/target/release/locked"   # rm -rf fails partway
out="$(lr 97)"                                                                         # 3 MB short
chmod -R u+w "$ROOT/target"
check "partial failure that frees enough: x.bin gone, next dir kept" test "$(has)" = "wasm32-unknown-unknown scratch-a doc debug "
check "partial failure: reported as could not delete" grep -qi 'could not delete target/release' <<<"$out"
profiles 20b
chmod 555 "$ROOT/target"   # every dir's contents go, the dirs themselves can't
out="$(TIMEFLY_FAKE_FREE_MB=95 TIMEFLY_TARGET_BUDGET_MB=1000 TIMEFLY_MIN_FREE_MB=100 TMPDIR="$CUL" guard 2>&1)"
chmod u+w "$ROOT/target"
check "read-only target/: stops once free space is back (two dirs emptied, three kept)" test "$(has)" = "scratch-a doc debug "
check "read-only target/: failures are not called 'space not returned'" bash -c '! grep -qi "not being returned" <<<"$1"' _ "$out"

echo "21. << inside arithmetic is a shift, not a heredoc"
newroot 21
xcase deny  20000 "echo \$((1 << 2))${NL}xctrace record --template X"
xcase deny  20000 "(( n <<= 1 ))${NL}xctrace record --template X"
xcase deny  20000 "x=\"\$(echo \$((1<<2))${NL}xctrace record --template X)\""
xcase deny  20000 "echo \"\$(( (1 + 2) << 3 ))\"${NL}xctrace record --template X"
xcase allow 20000 "echo \$((1 << 2))${NL}cat <<EOF${NL}xctrace record --template X${NL}EOF"
xcase allow 20000 "x=\"\$(echo \$((1<<2)); cat <<EOF${NL}xctrace record --template X${NL}EOF${NL})\""

echo
echo "passed: $PASS  failed: $FAIL"
[ "$FAIL" -eq 0 ]
