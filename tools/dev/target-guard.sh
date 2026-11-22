#!/usr/bin/env bash
# Keeps <repo>/target from filling the disk. See docs/dev/build-disk.md.
#
#   target-guard.sh            prune if over budget / low on disk, print a report
#   target-guard.sh --hook     Claude Code PostToolUse hook (JSON on stdin); never blocks
#   target-guard.sh --status   print sizes and limits, delete nothing
#
# Env: TIMEFLY_TARGET_BUDGET_MB (default 15360), TIMEFLY_MIN_FREE_MB (default 5120).
# Test-only: TIMEFLY_FAKE_FREE_MB, TIMEFLY_GUARD_ROOT, TIMEFLY_RUSTUP_BIN.

MODE=manual
case "${1:-}" in
  --hook) MODE=hook ;;
  --status) MODE=status ;;
  "") ;;
  *) echo "usage: $0 [--hook|--status]" >&2; exit 0 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${TIMEFLY_GUARD_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TARGET="$ROOT/target"
warn() { [ "$MODE" = hook ] || echo "target-guard: $*" >&2; }
# Numeric env values only; fall back to the default otherwise (never fail).
num_or() { # num_or <name> <value> <default>
  case "$2" in
    ""|*[!0-9]*) [ -z "$2" ] || warn "$1='$2' is not a number; using default $3"; echo "$3" ;;
    *) echo "$2" ;;
  esac
}
BUDGET_MB="$(num_or TIMEFLY_TARGET_BUDGET_MB "${TIMEFLY_TARGET_BUDGET_MB:-}" 15360)"
MIN_FREE_MB="$(num_or TIMEFLY_MIN_FREE_MB "${TIMEFLY_MIN_FREE_MB:-}" 5120)"
STALE_MIN=1440   # 24 h

# One toolchain: use rustup's proxies (keg-only, not on PATH) when present.
RUSTUP_BIN="${TIMEFLY_RUSTUP_BIN-/opt/homebrew/opt/rustup/bin}"
if [ -n "$RUSTUP_BIN" ] && [ -d "$RUSTUP_BIN" ]; then
  PATH="$RUSTUP_BIN:$PATH"
fi

# --- hook pre-filter: only react to cargo commands --------------------------
if [ "$MODE" = hook ]; then
  input="$(cat)"
  cmd=""
  if command -v jq >/dev/null 2>&1; then
    cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null)"
  elif command -v python3 >/dev/null 2>&1; then
    cmd="$(printf '%s' "$input" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("tool_input",{}).get("command",""))
except Exception: pass' 2>/dev/null)"
  fi
  case "$cmd" in *cargo*) ;; *) exit 0 ;; esac
fi

if [ -L "$TARGET" ]; then warn "$TARGET is a symlink; refusing to touch it."; exit 0; fi
if [ ! -d "$TARGET" ]; then warn "$TARGET does not exist; nothing to do."; exit 0; fi

# --- measurements (KB internally) -------------------------------------------
target_kb() { du -sk "$TARGET" 2>/dev/null | awk '{print $1+0}'; }
free_kb() {
  if [ -n "${TIMEFLY_FAKE_FREE_MB:-}" ]; then echo $(($(num_or TIMEFLY_FAKE_FREE_MB "$TIMEFLY_FAKE_FREE_MB" 0) * 1024)); return; fi
  df -Pk "$ROOT" 2>/dev/null | awk 'NR==2 {print $4+0}'
}
mb() { echo $(($1 / 1024)); }
limits_ok() {
  [ "$(target_kb)" -le $((BUDGET_MB * 1024)) ] && [ "$(free_kb)" -ge $((MIN_FREE_MB * 1024)) ]
}
describe_state() {
  echo "target: $(mb "$(target_kb)") MB (budget $BUDGET_MB MB), free disk: $(mb "$(free_kb)") MB (minimum $MIN_FREE_MB MB)"
}

if [ "$MODE" = status ]; then
  echo "target-guard status for $TARGET"
  describe_state
  if limits_ok; then echo "within limits"; else echo "OVER LIMITS"; fi
  [ -d "$TARGET" ] && du -sk "$TARGET"/*/ 2>/dev/null | sort -rn | awk '{printf "  %8d MB  %s\n", $1/1024, $2}'
  exit 0
fi

if limits_ok; then exit 0; fi

BEFORE="$(describe_state)"
REMOVED=()
SKIPPED=()

build_running() { pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; }

# remove <path>: only ever inside $TARGET, never through symlinks.
remove() {
  case "$1" in "$TARGET"/*) ;; *) return 1 ;; esac
  [ -L "$1" ] && return 1
  rm -rf -- "$1" && REMOVED+=("${1#"$ROOT"/}")
}
fresh() { [ -n "$(find "$1" -mmin -"$STALE_MIN" -print -quit 2>/dev/null)" ]; }
newest_mtime() {   # newest mtime (epoch) of a dir and its subdirs down to depth 2
  if [ "$(uname)" = Darwin ] || [ "$(uname)" = FreeBSD ]; then
    find "$1" -maxdepth 2 -type d -exec stat -f %m {} + 2>/dev/null
  else
    find "$1" -maxdepth 2 -type d -exec stat -c %Y {} + 2>/dev/null
  fi | sort -n | tail -1
}

# Step 1: stale incremental-compilation sessions.
for d in "$TARGET"/*/incremental/* "$TARGET"/*/*/incremental/*; do
  [ -d "$d" ] && [ ! -L "$d" ] || continue
  fresh "$d" && continue
  remove "$d"
  limits_ok && break
done

# Step 2: cargo sweep, if installed.
if ! limits_ok && command -v cargo-sweep >/dev/null 2>&1; then
  (cd "$ROOT" && env CARGO_TARGET_DIR="$TARGET" cargo sweep --time 2 >/dev/null 2>&1) && REMOVED+=("cargo sweep --time 2")
fi

# Step 3: whole build dirs, least recently modified first, never the newest.
if ! limits_ok; then
  if build_running; then
    SKIPPED+=("removing build dirs (cargo/rustc is running)")
  else
    list=""
    for d in "$TARGET"/*/; do
      d="${d%/}"
      [ -d "$d" ] && [ ! -L "$d" ] || continue
      list="$list$(newest_mtime "$d")	$d
"
    done
    ordered="$(printf '%s' "$list" | sort -n)"
    newest="$(printf '%s\n' "$ordered" | tail -1 | cut -f2-)"
    while IFS="$(printf '\t')" read -r _ d; do
      [ -n "$d" ] && [ "$d" != "$newest" ] || continue
      fresh "$d" && continue
      remove "$d"
      limits_ok && break
    done <<EOF2
$ordered
EOF2
  fi
fi

# Step 4: last resort, only for low free disk space.
if [ "$(free_kb)" -lt $((MIN_FREE_MB * 1024)) ]; then
  if build_running; then
    SKIPPED+=("cargo clean (cargo/rustc is running)")
  elif command -v cargo >/dev/null 2>&1; then
    (cd "$ROOT" && env -u CARGO_TARGET_DIR cargo clean --target-dir "$TARGET" >/dev/null 2>&1) && REMOVED+=("cargo clean")
  else
    SKIPPED+=("cargo clean (cargo not found)")
  fi
fi

AFTER="$(describe_state)"
STILL_OVER=0; limits_ok || STILL_OVER=1

join() { local IFS=,; echo "$*"; }
SUMMARY="target-guard: "
if [ "${#REMOVED[@]}" -gt 0 ]; then SUMMARY+="removed $(join "${REMOVED[@]}"). "; fi
if [ "${#SKIPPED[@]}" -gt 0 ]; then SUMMARY+="Skipped: $(join "${SKIPPED[@]}"). "; fi
SUMMARY+="Now: ${AFTER}."
[ "$STILL_OVER" = 1 ] && SUMMARY+=" Still over limits."

if [ "$MODE" = hook ]; then
  [ "${#REMOVED[@]}" -eq 0 ] && [ "$STILL_OVER" = 0 ] && exit 0
  CONTEXT="$SUMMARY"
  if [ "$STILL_OVER" = 1 ]; then
    CONTEXT+=" To free space: close other cargo/rustc processes and re-run tools/dev/target-guard.sh, delete target/scratch-* dirs, or run cargo clean. See docs/dev/build-disk.md."
  fi
  if command -v jq >/dev/null 2>&1; then
    jq -n --arg s "$SUMMARY" --arg c "$CONTEXT" \
      '{systemMessage:$s, hookSpecificOutput:{hookEventName:"PostToolUse", additionalContext:$c}}'
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json,sys
print(json.dumps({"systemMessage":sys.argv[1],"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":sys.argv[2]}}))' "$SUMMARY" "$CONTEXT"
  fi
  exit 0
fi

echo "target-guard report for $TARGET"
echo "before: $BEFORE"
if [ "${#REMOVED[@]}" -gt 0 ]; then
  echo "removed:"; for r in "${REMOVED[@]}"; do echo "  $r"; done
else
  echo "removed: nothing"
fi
for s in "${SKIPPED[@]}"; do echo "skipped: $s"; done
echo "after:  $AFTER"
[ "$STILL_OVER" = 1 ] && echo "WARNING: still over limits."
exit 0
