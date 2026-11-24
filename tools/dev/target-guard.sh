#!/usr/bin/env bash
# Keeps <repo>/target from filling the disk. See docs/dev/build-disk.md.
#
#   target-guard.sh            prune if over budget / low on disk, print a report
#   target-guard.sh --hook     Claude Code PostToolUse hook (JSON on stdin); never blocks.
#                              Prunes only after cargo runs; warns after any command when
#                              free space is low.
#   target-guard.sh --status   print sizes and limits, delete nothing
#
# Env: TIMEFLY_TARGET_BUDGET_MB (default 15360), TIMEFLY_MIN_FREE_MB (default 5120),
#      TIMEFLY_WARN_FREE_MB (default 10240).
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
WARN_FREE_MB="$(num_or TIMEFLY_WARN_FREE_MB "${TIMEFLY_WARN_FREE_MB:-}" 10240)"
STALE_MIN=1440   # 24 h
CULPRIT_MIN_BYTES=524288000   # 500 MB: files this big directly in $TMPDIR are listed

# One toolchain: use rustup's proxies (keg-only, not on PATH) when present.
RUSTUP_BIN="${TIMEFLY_RUSTUP_BIN-/opt/homebrew/opt/rustup/bin}"
if [ -n "$RUSTUP_BIN" ] && [ -d "$RUSTUP_BIN" ]; then
  PATH="$RUSTUP_BIN:$PATH"
fi

# --- shell command parsing ---------------------------------------------------
# command_segments <command>: one line per simple command, its words tab-separated with
# quotes removed, starting at the program actually run. Splits on && || ; | & ( ) ` $(
# and newlines outside quotes; drops comments; skips leading VAR=value assignments,
# `env [opts] [VAR=value...]`, `time [-p]`, `nice [-n N]`, `command [-p]`, `direnv exec <dir>`
# and the keywords ! { if then else elif do while until. `command -v/-V` (a lookup) yields
# no line. A rough tokenizer: unusual syntax may hide a command, never invent one.
command_segments() {
  TF_CMD="$1" awk '
    function endword() { if (inword) { w[++n] = word }; word = ""; inword = 0 }
    function endseg(   k, out, j) {
      endword()
      k = 1
      while (k <= n) {
        if (w[k] ~ /^[A-Za-z_][A-Za-z0-9_]*=/) { k++; continue }
        if (w[k] == "env") {
          k++
          while (k <= n && w[k] ~ /^-/) {
            if (w[k] == "-u" || w[k] == "-P" || w[k] == "-C" || w[k] == "-S") k++
            k++
          }
          continue
        }
        if (w[k] == "time") { k++; if (w[k] == "-p") k++; continue }
        if (w[k] == "nice") {
          k++
          if (w[k] == "-n") k += 2; else if (w[k] ~ /^-/) k++
          continue
        }
        if (w[k] == "command") {
          k++
          if (w[k] == "-v" || w[k] == "-V") { k = n + 1; break }
          if (w[k] == "-p") k++
          continue
        }
        if (w[k] == "direnv" && w[k + 1] == "exec") { k += 3; continue }
        if (w[k] ~ /^(!|[{]|if|then|else|elif|do|while|until)$/) { k++; continue }
        break
      }
      if (k <= n) {
        out = w[k]
        for (j = k + 1; j <= n; j++) out = out "\t" w[j]
        print out
      }
      n = 0
    }
    BEGIN {
      s = ENVIRON["TF_CMD"]; L = length(s); q = ""; n = 0; word = ""; inword = 0
      for (i = 1; i <= L; i++) {
        c = substr(s, i, 1); nx = substr(s, i + 1, 1)
        if (q == "\047") { if (c == "\047") q = ""; else word = word c; continue }
        if (q == "\"") {
          if (c == "\"") q = ""
          else if (c == "\\" && index("\"\\$`", nx)) { word = word nx; i++ }
          else word = word c
          continue
        }
        if (c == "\\") {
          if (nx == "\n") { i++; continue }
          word = word nx; inword = 1; i++; continue
        }
        if (c == "\047" || c == "\"") { q = c; inword = 1; continue }
        if (c == "#" && !inword) { while (i <= L && substr(s, i, 1) != "\n") i++; endseg(); continue }
        if (c == " " || c == "\t") { endword(); continue }
        if (c == "\n" || c == ";" || c == "(" || c == ")" || c == "`") { endseg(); continue }
        if (c == "$" && nx == "(") { i++; endseg(); continue }
        if (c == "&") {
          if (substr(word, length(word), 1) ~ /[<>]/ || nx == ">") { word = word c; inword = 1; continue }
          if (nx == "&") i++
          endseg(); continue
        }
        if (c == "|") { if (nx == "|" || nx == "&") i++; endseg(); continue }
        word = word c; inword = 1
      }
      endseg()
    }'
}
# runs_cargo <command>: true when some simple command runs cargo itself.
runs_cargo() {
  command_segments "$1" | awk -F '\t' '$1 == "cargo" || $1 ~ /\/cargo$/ { f = 1 } END { exit !f }'
}

# --- hook input --------------------------------------------------------------
# hook_command: the Bash command from the hook JSON on stdin; empty if unparsable.
hook_command() {
  local input; input="$(cat)"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$input" | jq -r '.tool_input.command // empty' 2>/dev/null
  elif command -v python3 >/dev/null 2>&1; then
    printf '%s' "$input" | python3 -c 'import json,sys
try: print(json.load(sys.stdin).get("tool_input",{}).get("command",""))
except Exception: pass' 2>/dev/null
  fi
}

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

# --- low free space warning (never deletes anything) --------------------------
file_bytes() {
  case "$(uname)" in Darwin|FreeBSD) stat -f %z "$1" ;; *) stat -c %s "$1" ;; esac 2>/dev/null
}
# culprits: up to 5 "<MB> MB <path>" lines for regular files >= 500 MB directly in $TMPDIR,
# largest first (apparent size). xctrace leaves its raw instruments*.ktrace there.
scan_dir() { local d="${TMPDIR:-/tmp}"; d="${d%/}"; echo "${d:-/}"; }
culprits() {
  local dir f
  dir="$(scan_dir)"
  [ -d "$dir" ] || return 0
  find "$dir" -maxdepth 1 -type f -size +511999k 2>/dev/null | while IFS= read -r f; do
    printf '%s\t%s\n' "$(file_bytes "$f")" "$f"
  done | awk -F '\t' -v min="$CULPRIT_MIN_BYTES" '$1 + 0 >= min' | sort -t "$(printf '\t')" -k1,1nr | head -5 |
    awk -F '\t' '{ printf "%d MB %s\n", $1 / 1048576, $2 }'
}
# low_free_warning: when free space is under TIMEFLY_WARN_FREE_MB, set WARN_SYS (short) and
# WARN_CTX (free space, target/ size, likely culprits) and succeed; otherwise fail.
low_free_warning() {
  local free t list
  free="$(free_kb)"
  [ "$free" -lt $((WARN_FREE_MB * 1024)) ] || return 1
  if [ -d "$TARGET" ] && [ ! -L "$TARGET" ]; then t="$(mb "$(target_kb)") MB"; else t="absent"; fi
  list="$(culprits | awk 'NR > 1 { printf "; " } { printf "%s", $0 }')"
  WARN_SYS="target-guard: low disk space, $(mb "$free") MB free (warning below $WARN_FREE_MB MB)."
  WARN_CTX="$WARN_SYS target/: $t (minimum free $MIN_FREE_MB MB, target budget $BUDGET_MB MB)."
  if [ -n "$list" ]; then
    WARN_CTX+=" Largest files directly in $(scan_dir) (likely culprits; xctrace leaves instruments*.ktrace there): $list."
  else
    WARN_CTX+=" No files of 500 MB or more directly in $(scan_dir)."
  fi
  WARN_CTX+=" The guard never deletes anything outside target/: tell the owner what is using the space, and delete files outside target/ only with the owner's approval. See docs/dev/build-disk.md."
}
# emit_post <systemMessage> <additionalContext>: PostToolUse hook output.
emit_post() {
  if command -v jq >/dev/null 2>&1; then
    jq -n --arg s "$1" --arg c "$2" \
      '{systemMessage:$s, hookSpecificOutput:{hookEventName:"PostToolUse", additionalContext:$c}}'
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json,sys
print(json.dumps({"systemMessage":sys.argv[1],"hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":sys.argv[2]}}))' "$1" "$2"
  fi
}
# hook_done: in hook mode, report low free space (if any) and exit.
hook_done() {
  if [ "$MODE" = hook ] && low_free_warning; then emit_post "$WARN_SYS" "$WARN_CTX"; fi
  exit 0
}

# --- hook pre-filter: prune only after cargo runs; warn after any command --------
if [ "$MODE" = hook ]; then
  cmd="$(hook_command)"
  runs_cargo "$cmd" || hook_done
fi

if [ -L "$TARGET" ]; then warn "$TARGET is a symlink; refusing to touch it."; hook_done; fi
if [ ! -d "$TARGET" ]; then warn "$TARGET does not exist; nothing to do."; hook_done; fi

if [ "$MODE" = status ]; then
  echo "target-guard status for $TARGET"
  describe_state
  if limits_ok; then echo "within limits"; else echo "OVER LIMITS"; fi
  [ -d "$TARGET" ] && du -sk "$TARGET"/*/ 2>/dev/null | sort -rn | awk '{printf "  %8d MB  %s\n", $1/1024, $2}'
  exit 0
fi

if limits_ok; then hook_done; fi
PHYSICAL_TARGET="$(cd -P -- "$TARGET" && pwd -P)" || hook_done

BEFORE="$(describe_state)"
REMOVED=()
SKIPPED=()

build_running() { pgrep -x cargo >/dev/null 2>&1 || pgrep -x rustc >/dev/null 2>&1; }

# remove <path>: only ever inside $TARGET, never through symlinks.
remove() {
  case "$1" in "$TARGET"/*) ;; *) return 1 ;; esac
  local parent=${1%/*} leaf=${1##*/}
  # Stay in the verified physical parent during removal. A symlinked ancestor
  # leads to a different directory, even when the last component looks ordinary.
  (
    cd -P -- "$parent" || exit 1
    [ "$(pwd -P)" = "$PHYSICAL_TARGET${parent#"$TARGET"}" ] || exit 1
    [ ! -L "./$leaf" ] && rm -rf -- "./$leaf"
  ) && REMOVED+=("${1#"$ROOT"/}")
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
  [ "${#REMOVED[@]}" -eq 0 ] && [ "$STILL_OVER" = 0 ] && hook_done
  CONTEXT="$SUMMARY"
  if [ "$STILL_OVER" = 1 ]; then
    CONTEXT+=" To free space: close other cargo/rustc processes and re-run tools/dev/target-guard.sh, delete target/scratch-* dirs, or run cargo clean. See docs/dev/build-disk.md."
  fi
  if low_free_warning; then SUMMARY+=" $WARN_SYS"; CONTEXT+=" $WARN_CTX"; fi
  emit_post "$SUMMARY" "$CONTEXT"
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
