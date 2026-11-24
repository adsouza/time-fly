#!/usr/bin/env bash
# Keeps <repo>/target from filling the disk. See docs/dev/build-disk.md.
#
#   target-guard.sh            prune if over budget / low on disk, print a report
#   target-guard.sh --hook     Claude Code PostToolUse hook (JSON on stdin); never blocks.
#                              Prunes only after cargo runs; warns after any command when
#                              free space is low.
#   target-guard.sh --pre-hook Claude Code PreToolUse hook (JSON on stdin): denies
#                              `xctrace record` without --time-limit or on low free space
#   target-guard.sh --status   print sizes and limits, delete nothing
#
# Env: TIMEFLY_TARGET_BUDGET_MB (default 15360), TIMEFLY_MIN_FREE_MB (default 5120),
#      TIMEFLY_WARN_FREE_MB (default 10240), TIMEFLY_XCTRACE_MIN_FREE_MB (default 10240).
# Test-only: TIMEFLY_FAKE_FREE_MB (free space in MB; grows as the guard deletes in target/),
#            TIMEFLY_FAKE_FREE_STATIC=1 (deletions don't raise the fake free space),
#            TIMEFLY_GUARD_ROOT, TIMEFLY_RUSTUP_BIN.

MODE=manual
case "${1:-}" in
  --hook) MODE=hook ;;
  --pre-hook) MODE=prehook ;;
  --status) MODE=status ;;
  "") ;;
  *) echo "usage: $0 [--hook|--pre-hook|--status]" >&2; exit 0 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="${TIMEFLY_GUARD_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TARGET="$ROOT/target"
warn() { case "$MODE" in hook|prehook) ;; *) echo "target-guard: $*" >&2 ;; esac; }
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
XCTRACE_MIN_FREE_MB="$(num_or TIMEFLY_XCTRACE_MIN_FREE_MB "${TIMEFLY_XCTRACE_MIN_FREE_MB:-}" 10240)"
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
# `env [opts] [VAR=value...]`, `time [-p]`, `nice [-n N]`, `command [-p]`, `direnv exec <dir>`,
# `nohup`, `exec [opts]`, `sudo [opts]`, `timeout [opts] DURATION` and the keywords
# ! { if then else elif do while until. `command -v/-V` (a lookup) yields no line.
# Newlines and tabs inside words are encoded as \036 and \035 so each segment stays one line.
# Nested commands, i.e. the script of `sh|bash|zsh|dash|ksh -c SCRIPT` and the body of a
# `$(...)` or backtick substitution inside double quotes, come out as extra lines: \034 then
# the (encoded) nested command; all_segments tokenizes those in turn.
# Heredoc bodies (<<WORD, <<'WORD', <<"WORD", <<-WORD; not the <<< here-string) are text and
# are dropped up to their terminator line; an unterminated heredoc drops the rest. Inside
# $(( )) and (( )) arithmetic, << and <<= are shifts, not heredocs.
# A rough tokenizer: unusual syntax may hide a command, never invent one.
command_segments() {
  TF_CMD="$1" awk '
    function enc(t) { gsub(/\n/, "\036", t); gsub(/\t/, "\035", t); return t }
    function endword() { if (inword) { w[++n] = enc(word) }; word = ""; inword = 0 }
    # heredoc operator "<<" at j (not "<<<"): sets HW (delimiter, quotes removed) and HS
    # (1 for <<-); returns the index of the last character of the operator and word.
    function hd_parse(j,   k, ch, w, qq) {
      k = j + 2; HS = 0
      if (substr(s, k, 1) == "-") { HS = 1; k++ }
      while (substr(s, k, 1) == " " || substr(s, k, 1) == "\t") k++
      w = ""; qq = ""
      for (; k <= L; k++) {
        ch = substr(s, k, 1)
        if (qq != "") { if (ch == qq) qq = ""; else w = w ch; continue }
        if (ch == "\047" || ch == "\"") { qq = ch; continue }
        if (ch == "\\") { k++; w = w substr(s, k, 1); continue }
        if (ch ~ /[ \t\n;|&()<>]/) break
        w = w ch
      }
      HW = w
      return k - 1
    }
    # heredoc body after the newline at j: skips lines up to and including the one equal to
    # w (leading tabs stripped if strip); returns the index of the last character skipped.
    function hd_skip(j, w, strip,   k, line) {
      while (j < L) {
        for (k = j + 1; k <= L && substr(s, k, 1) != "\n"; k++) ;
        line = substr(s, j + 1, k - j - 1)
        if (strip) sub(/^\t+/, "", line)
        j = (k <= L) ? k : L
        if (line == w) return j
      }
      return L
    }
    # substitution body starting at st inside double quotes; returns the index of its end.
    # For $(...), finds the matching ")" outside quotes and heredoc bodies; arith says the
    # body is the inside of $((...)). astk holds the paren depth each (( opened at.
    function capsub(st, tick, arith,   j, ch, depth, q2, nh2, h, cw, cs, ar2, astk) {
      if (tick) {
        j = index(substr(s, st), "`"); j = j ? st + j - 1 : L + 1
      } else {
        depth = 1; q2 = ""; nh2 = 0; ar2 = 0
        if (arith) { ar2 = 1; astk[1] = 0 }
        for (j = st; j <= L; j++) {
          ch = substr(s, j, 1)
          if (q2 == "\047") { if (ch == "\047") q2 = ""; continue }
          if (q2 == "\"") { if (ch == "\\") j++; else if (ch == "\"") q2 = ""; continue }
          if (ch == "\\") { j++; continue }
          if (ch == "\047" || ch == "\"") { q2 = ch; continue }
          if (ch == "<" && substr(s, j + 1, 1) == "<") {
            if (ar2) { j++; continue }   # shift
            if (substr(s, j + 2, 1) == "<") { j += 2; continue }
            j = hd_parse(j)
            if (HW != "") { cw[++nh2] = HW; cs[nh2] = HS }
            continue
          }
          if (ch == "\n" && nh2) {
            for (h = 1; h <= nh2; h++) j = hd_skip(j, cw[h], cs[h])
            nh2 = 0
            continue
          }
          if (ch == "(" && substr(s, j + 1, 1) == "(") { astk[++ar2] = depth; depth += 2; j++; continue }
          if (ch == "(") depth++
          else if (ch == ")") {
            depth--
            if (ar2 && depth <= astk[ar2]) ar2--
            if (depth == 0) break
          }
        }
      }
      subs[++ns] = enc(substr(s, st, j - st))
      return j
    }
    function endseg(   k, out, j, prog, hasc) {
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
        if (w[k] == "nohup") { k++; continue }
        if (w[k] == "exec") { k++; while (w[k] ~ /^-/) { if (w[k] == "-a") k++; k++ }; continue }
        if (w[k] == "sudo") {
          k++
          while (w[k] ~ /^-/) {
            if (w[k] == "--") { k++; break }
            if (w[k] ~ /^-[ugChprtUDTR]$/) k++
            k++
          }
          continue
        }
        if (w[k] == "timeout" || w[k] == "gtimeout") {
          k++
          while (w[k] ~ /^-/) { if (w[k] == "-s" || w[k] == "-k") k++; k++ }
          k++   # the duration
          continue
        }
        if (w[k] ~ /^(!|[{]|if|then|else|elif|do|while|until)$/) { k++; continue }
        break
      }
      if (k <= n) {
        out = w[k]
        for (j = k + 1; j <= n; j++) out = out "\t" w[j]
        print out
        prog = w[k]; sub(/.*\//, "", prog)
        if (prog ~ /^(sh|bash|zsh|dash|ksh)$/) {
          hasc = 0
          for (j = k + 1; j <= n && w[j] ~ /^[-+]/; j++) {
            if (w[j] ~ /^-[A-Za-z]*c[A-Za-z]*$/) hasc = 1
            if (w[j] ~ /^[-+]o$/) j++
          }
          if (hasc && j <= n) subs[++ns] = w[j]
        }
      }
      n = 0
    }
    BEGIN {
      s = ENVIRON["TF_CMD"]; L = length(s); q = ""; n = 0; ns = 0; nh = 0; ar = 0; word = ""; inword = 0
      for (i = 1; i <= L; i++) {
        c = substr(s, i, 1); nx = substr(s, i + 1, 1)
        if (q == "\047") { if (c == "\047") q = ""; else word = word c; continue }
        if (q == "\"") {
          if (c == "\"") q = ""
          else if (c == "\\" && index("\"\\$`", nx)) { word = word nx; i++ }
          else if (c == "$" && nx == "(") { e = capsub(i + 2, 0, substr(s, i + 2, 1) == "("); word = word substr(s, i, e - i + 1); i = e }
          else if (c == "`") { e = capsub(i + 1, 1, 0); word = word substr(s, i, e - i + 1); i = e }
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
        if (c == "$" && nx == "(" && substr(s, i + 2, 1) == "(") { ar++; i += 2; endseg(); continue }
        if (c == "(" && nx == "(") { ar++; i++; endseg(); continue }
        if (c == ")" && nx == ")" && ar > 0) { ar--; i++; endseg(); continue }
        if (c == "<" && nx == "<") {
          if (ar > 0) { word = word "<<"; inword = 1; i++; continue }   # shift
          if (substr(s, i + 2, 1) == "<") { word = word "<<<"; inword = 1; i += 2; continue }
          endword(); i = hd_parse(i)
          if (HW != "") { hw[++nh] = HW; hs[nh] = HS }
          continue
        }
        if (c == "\n" && nh) {
          endseg()
          for (h = 1; h <= nh; h++) i = hd_skip(i, hw[h], hs[h])
          nh = 0
          continue
        }
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
      for (j = 1; j <= ns; j++) print "\034" subs[j]
    }'
}
# all_segments <command> [depth]: command_segments plus, recursively (depth <= 4), the
# segments of every nested command it reports.
all_segments() {
  local depth="${2:-0}" line mark
  mark="$(printf '\034')"
  command_segments "$1" | while IFS= read -r line; do
    case "$line" in
      "$mark"*)
        [ "$depth" -lt 4 ] || continue
        all_segments "$(printf '%s' "${line#"$mark"}" | tr '\036\035' '\n\t')" $((depth + 1)) ;;
      *) printf '%s\n' "$line" ;;
    esac
  done
}
# runs_cargo <command>: true when some simple command runs cargo itself.
runs_cargo() {
  all_segments "$1" | awk -F '\t' '$1 == "cargo" || $1 ~ /\/cargo$/ { f = 1 } END { exit !f }'
}

# xctrace_records <command>: one line per simple command that runs `xctrace record`
# (directly, by path, or through xcrun): "limit" if it passes --time-limit (before any
# `--`, after which the arguments belong to the launched program), else "nolimit".
xctrace_records() {
  all_segments "$1" | awk -F '\t' '
    {
      k = 1
      if ($1 == "xcrun" || $1 ~ /\/xcrun$/) {
        k = 2
        while (k <= NF && $k ~ /^-/) {
          if ($k ~ /^--?(f|find|show-sdk-path|show-sdk-version|show-sdk-platform-path)$/) next
          if ($k ~ /^--?(sdk|toolchain)$/) k++
          k++
        }
      }
      if ($k != "xctrace" && $k !~ /\/xctrace$/) next
      if ($(k + 1) != "record") next
      lim = "nolimit"
      for (j = k + 2; j <= NF; j++) {
        if ($j == "--") break
        if ($j == "--time-limit" || $j ~ /^--time-limit=/) lim = "limit"
      }
      print lim
    }'
}

# --- hook input --------------------------------------------------------------
# read_hook_input: parse the hook JSON on stdin into HOOK_CMD (the Bash command) and
# HOOK_EVENT (PostToolUse or PostToolUseFailure; anything else reads as PostToolUse).
# Both are empty/default if the input is unparsable or neither jq nor python3 exists.
read_hook_input() {
  local input out nl='
'
  input="$(cat)"
  # "<event>\n<command>\n." keeps the command's trailing newlines through $(...).
  if command -v jq >/dev/null 2>&1; then
    out="$(printf '%s' "$input" | jq -r '"\(.hook_event_name // "")\n\(.tool_input.command // "")\n."' 2>/dev/null)"
  elif command -v python3 >/dev/null 2>&1; then
    out="$(printf '%s' "$input" | python3 -c 'import json,sys
try:
    d=json.load(sys.stdin); t=d.get("tool_input") or {}
    sys.stdout.write("%s\n%s\n.\n" % (d.get("hook_event_name") or "", t.get("command") or ""))
except Exception: pass' 2>/dev/null)"
  fi
  HOOK_EVENT=""; HOOK_CMD=""
  case "$out" in *"$nl"*)
    HOOK_EVENT="${out%%"$nl"*}"
    HOOK_CMD="${out#*"$nl"}"; HOOK_CMD="${HOOK_CMD%.}"; HOOK_CMD="${HOOK_CMD%"$nl"}" ;;
  esac
  case "$HOOK_EVENT" in PostToolUse|PostToolUseFailure) ;; *) HOOK_EVENT=PostToolUse ;; esac
}
HOOK_EVENT=PostToolUse
CARGO_RUN=0

# --- measurements (KB internally) -------------------------------------------
target_kb() { du -sk "$TARGET" 2>/dev/null | awk '{ s = $1 + 0 } END { print s + 0 }'; }
# Fake free space (tests): TIMEFLY_FAKE_FREE_MB plus whatever was deleted from target/
# since FAKE_START_KB was taken, so "stop as soon as there is enough" is testable.
free_kb() {
  if [ -n "${TIMEFLY_FAKE_FREE_MB:-}" ]; then
    local base=$(($(num_or TIMEFLY_FAKE_FREE_MB "$TIMEFLY_FAKE_FREE_MB" 0) * 1024))
    if [ -n "${FAKE_START_KB:-}" ] && [ "${TIMEFLY_FAKE_FREE_STATIC:-}" != 1 ]; then echo $((base + FAKE_START_KB - $(target_kb))); else echo "$base"; fi
    return
  fi
  # Unknown (df failed) reads as plenty: never delete or block on a guess.
  df -Pk "$ROOT" 2>/dev/null | awk 'NR == 2 { v = $4 + 0; ok = 1 } END { if (ok) print v; else print "1125899906842624" }'
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
# low_free_warning [force]: when free space is under TIMEFLY_WARN_FREE_MB (or "force"), set
# WARN_SYS (short) and WARN_CTX (free space, target/ size, likely culprits) and succeed;
# otherwise fail.
low_free_warning() {
  local free t list
  free="$(free_kb)"
  [ "${1:-}" = force ] || [ "$free" -lt $((WARN_FREE_MB * 1024)) ] || return 1
  # target/ is measured (du) only after cargo runs: every other command costs one df.
  t=""
  if [ "$CARGO_RUN" = 1 ]; then
    if [ -d "$TARGET" ] && [ ! -L "$TARGET" ]; then t=" target/: $(mb "$(target_kb)") MB"; else t=" target/: absent"; fi
  fi
  list="$(culprits | awk 'NR > 1 { printf "; " } { printf "%s", $0 }')"
  WARN_SYS="target-guard: low disk space, $(mb "$free") MB free (warning below $WARN_FREE_MB MB)."
  WARN_CTX="$WARN_SYS$t (minimum free $MIN_FREE_MB MB, target budget $BUDGET_MB MB)."
  if [ -n "$list" ]; then
    WARN_CTX+=" Largest files directly in $(scan_dir) (likely culprits; xctrace leaves instruments*.ktrace there): $list."
  else
    WARN_CTX+=" No files of 500 MB or more directly in $(scan_dir)."
  fi
  WARN_CTX+=" The guard never deletes anything outside target/: tell the owner what is using the space, and delete files outside target/ only with the owner's approval. See docs/dev/build-disk.md."
}
# emit_post <systemMessage> <additionalContext>: PostToolUse / PostToolUseFailure output,
# answered under the event that called us (HOOK_EVENT).
emit_post() {
  if command -v jq >/dev/null 2>&1; then
    jq -n --arg s "$1" --arg c "$2" --arg e "$HOOK_EVENT" \
      '{systemMessage:$s, hookSpecificOutput:{hookEventName:$e, additionalContext:$c}}'
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json,sys
print(json.dumps({"systemMessage":sys.argv[1],"hookSpecificOutput":{"hookEventName":sys.argv[3],"additionalContext":sys.argv[2]}}))' "$1" "$2" "$HOOK_EVENT"
  fi
}
# culprit_lines: the culprit list as one sentence, for the manual report.
culprit_lines() {
  local list; list="$(culprits | awk 'NR > 1 { printf "; " } { printf "%s", $0 }')"
  if [ -n "$list" ]; then echo "Largest files directly in $(scan_dir): $list. The guard never deletes anything outside target/; delete them only with the owner's approval."
  else echo "No files of 500 MB or more directly in $(scan_dir)."; fi
}
# hook_done: in hook mode, report low free space (if any) and exit.
hook_done() {
  if [ "$MODE" = hook ] && low_free_warning; then emit_post "$WARN_SYS" "$WARN_CTX"; fi
  exit 0
}

# emit_pre_deny <systemMessage> <reason>: PreToolUse "deny" hook output. Only reached after
# read_hook_input parsed a command, which needs jq or python3, so one of them exists.
emit_pre_deny() {
  if command -v jq >/dev/null 2>&1; then
    jq -n --arg s "$1" --arg r "$2" \
      '{systemMessage:$s, hookSpecificOutput:{hookEventName:"PreToolUse", permissionDecision:"deny", permissionDecisionReason:$r}}'
  else
    python3 -c 'import json,sys
print(json.dumps({"systemMessage":sys.argv[1],"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":sys.argv[2]}}))' "$1" "$2"
  fi
}

# --- PreToolUse: block runaway xctrace recordings; allow everything else ---------
if [ "$MODE" = prehook ]; then
  read_hook_input; cmd="$HOOK_CMD"
  # Cheap keyword check first: the tokenizer is slow on huge commands.
  case "$cmd" in *xctrace*) ;; *) exit 0 ;; esac
  recs="$(xctrace_records "$cmd")"
  [ -n "$recs" ] || exit 0
  free="$(free_kb)"
  case "$free" in ""|*[!0-9]*) free="" ;; esac
  why=""; low=""
  case "$recs" in *nolimit*) why="it has no --time-limit" ;; esac
  if [ -n "$free" ] && [ "$free" -lt $((XCTRACE_MIN_FREE_MB * 1024)) ]; then
    low="only $(mb "$free") MB free, under TIMEFLY_XCTRACE_MIN_FREE_MB ($XCTRACE_MIN_FREE_MB MB)"
    why="${why:+$why and }$low"
  fi
  [ -n "$why" ] || exit 0
  reason="target-guard blocked this xctrace record: $why."
  reason+=" xctrace streams its raw trace to \$TMPDIR/instruments*.ktrace (here $(scan_dir)), not to --output, and a stopped or aborted recording leaves it there."
  reason+=" On 2026-11-24 two aborted recordings left 3.4 + 4.3 GB of .ktrace there, the disk filled (ENOSPC) and Bash could no longer start."
  reason+=" To proceed:"
  case "$recs" in *nolimit*) reason+=" add --time-limit (e.g. --time-limit 30s) to every xctrace record;" ;; esac
  [ -n "$low" ] && reason+=" free space first (check df -h; look for instruments*.ktrace and other large files in $(scan_dir) and delete them only with the owner's approval);"
  reason+=" for GPU cost prefer sampling ioreg instead (ioreg -c IOAccelerator, PerformanceStatistics \"Device Utilization %\" every 0.25 s; see docs/dev/build-disk.md)."
  emit_pre_deny "target-guard: blocked xctrace record ($why)." "$reason"
  exit 0
fi

# --- hook pre-filter: prune only after cargo runs; warn after any command --------
if [ "$MODE" = hook ]; then
  read_hook_input; cmd="$HOOK_CMD"
  # Cheap keyword check first: the tokenizer is slow on huge commands.
  case "$cmd" in *cargo*) runs_cargo "$cmd" || hook_done ;; *) hook_done ;; esac
  CARGO_RUN=1
fi

if [ -L "$TARGET" ]; then warn "$TARGET is a symlink; refusing to touch it."; hook_done; fi
if [ ! -d "$TARGET" ]; then warn "$TARGET does not exist; nothing to do."; hook_done; fi

[ -n "${TIMEFLY_FAKE_FREE_MB:-}" ] && FAKE_START_KB="$(target_kb)"

if [ "$MODE" = status ]; then
  echo "target-guard status for $TARGET"
  describe_state
  if limits_ok; then echo "within limits"; else echo "OVER LIMITS"; fi
  [ -d "$TARGET" ] && du -sk "$TARGET"/*/ 2>/dev/null | sort -rn | awk '{printf "  %8d MB  %s\n", $1/1024, $2}'
  exit 0
fi

if limits_ok; then hook_done; fi
PHYSICAL_TARGET="$(cd -P -- "$TARGET" && pwd -P)" || hook_done

# One pruning run at a time: concurrent hooks would race over the same dirs. A lock older
# than 10 minutes is a leftover from a killed run; it is taken over atomically by renaming
# it (only one run's mv can succeed), and a run that loses that race skips pruning. If the
# lock can't even be created and none exists (mkdir fails on a full disk), prune unlocked
# rather than not at all.
LOCK="$TARGET/.guard-lock"
take_lock() {
  mkdir "$LOCK" 2>/dev/null || return 1
  trap 'rmdir "$LOCK" 2>/dev/null' EXIT
  trap 'exit 1' HUP INT TERM
}
lock_held() { warn "another target-guard run holds $LOCK; skipped pruning."; hook_done; }
if ! take_lock; then
  if [ -e "$LOCK" ] || [ -L "$LOCK" ]; then
    [ -n "$(find "$LOCK" -maxdepth 0 -type d -mmin +10 2>/dev/null)" ] || lock_held
    mv "$LOCK" "$LOCK.stale.$$" 2>/dev/null || lock_held
    rm -rf -- "$LOCK.stale.$$"
    take_lock || lock_held
  fi
fi

BEFORE="$(describe_state)"
REMOVED=()
SKIPPED=()
NOTES=()
FAILED=()
MIN_FREE_KB=$((MIN_FREE_MB * 1024))
free_ok() { [ "$(free_kb)" -ge "$MIN_FREE_KB" ]; }
under_budget() { [ "$(target_kb)" -le $((BUDGET_MB * 1024)) ]; }
# If free space is short of the minimum by more than all of target/ holds, deleting
# builds cannot fix it: then only the budget may drive deletions.
SHORT_KB=$((MIN_FREE_KB - $(free_kb)))
FREE_FIXABLE=1
[ "$SHORT_KB" -gt 0 ] && [ "$SHORT_KB" -gt "$(target_kb)" ] && FREE_FIXABLE=0
need_prune() {
  under_budget || return 0
  [ "$FREE_FIXABLE" = 1 ] && ! free_ok
}

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
    [ ! -L "./$leaf" ] && rm -rf -- "./$leaf" 2>/dev/null
  ) && { REMOVED+=("${1#"$ROOT"/}"); return 0; }
  FAILED+=("${1#"$ROOT"/}")
  return 1
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
  need_prune || break
  remove "$d"
done

# Step 2: cargo sweep, if installed.
if need_prune && command -v cargo-sweep >/dev/null 2>&1; then
  (cd "$ROOT" && env CARGO_TARGET_DIR="$TARGET" cargo sweep --time 2 >/dev/null 2>&1) && REMOVED+=("cargo sweep --time 2")
fi

# Step 3: whole build dirs, least recently modified first, never the newest.
if need_prune; then
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
      need_prune || break
    done <<EOF2
$ordered
EOF2
  fi
fi

# last_resort_dirs: build dirs in deletion order: release, <triple>/ dirs, scratch-*,
# any others, debug last. One path per line; symlinks are skipped by the caller.
last_resort_dirs() {
  local d n pass
  for pass in release triple scratch other debug; do
    for d in "$TARGET"/*/; do
      d="${d%/}"; n="${d##*/}"
      [ -d "$d" ] || continue
      case "$pass:$n" in
        release:release|debug:debug|scratch:scratch-*) echo "$d" ;;
        triple:release|triple:debug|triple:scratch-*|other:release|other:debug|other:scratch-*) ;;
        triple:*-*-*) echo "$d" ;;
        other:*-*-*) ;;
        other:*) echo "$d" ;;
      esac
    done
  done
}

# Step 4: last resort, only for low free disk space: whole build dirs, fresh or not, in
# a fixed order, stopping as soon as free space is back above the minimum.
HOPELESS=0
if ! free_ok; then
  if [ "$FREE_FIXABLE" = 0 ]; then
    HOPELESS=1
    NOTES+=("free disk is $(mb $((MIN_FREE_KB - $(free_kb)))) MB short of the minimum, more than all of target/ ($(mb "$(target_kb)") MB) could give back, so nothing in target/ was deleted for free space: deleting builds would not fix it, the space is used elsewhere")
  elif build_running; then
    SKIPPED+=("last resort: deleting whole build dirs for free space (cargo/rustc is running)")
  else
    LR_FREE="$(mb "$(free_kb)")"
    LR_COUNT=0
    LR_STUCK=""
    while IFS= read -r d; do
      [ -n "$d" ] && [ -d "$d" ] && [ ! -L "$d" ] || continue
      # If a deletion gives back less than half its size, the blocks are still held
      # (APFS local snapshots, open files): deleting more builds would not help.
      size_kb="$(du -sk "$d" 2>/dev/null | awk '{ s = $1 + 0 } END { print s + 0 }')"
      free_before="$(free_kb)"
      # A failed rm -rf is usually partial and may still have freed enough. Either way no
      # "space not returned" check: we don't know how much it was meant to free.
      remove "$d" || { free_ok && break; continue; }
      LR_COUNT=$((LR_COUNT + 1))
      free_ok && break
      if [ $(($(free_kb) - free_before)) -lt $((size_kb / 2)) ]; then
        LR_STUCK="${d#"$ROOT"/}"
        break
      fi
    done <<EOF3
$(last_resort_dirs)
EOF3
    if [ "$LR_COUNT" -gt 0 ]; then
      NOTE="last resort: free disk was $LR_FREE MB, under the minimum $MIN_FREE_MB MB, so whole build dirs were deleted in the order release, <triple>, scratch-*, others, debug until free space was back above the minimum"
      [ -z "$(last_resort_dirs | while IFS= read -r d; do [ -L "$d" ] || echo "$d"; done)" ] &&
        NOTE+="; every build dir had to go (the same as cargo clean)"
      NOTES+=("$NOTE")
    fi
    [ -n "$LR_STUCK" ] && NOTES+=("deleting $LR_STUCK did not raise free space by even half its size, so the deleted space is not being returned (likely APFS local snapshots or a process holding the files open); stopped deleting. Check with tmutil listlocalsnapshots / and lsof +L1, and tell the owner")
  fi
fi

AFTER="$(describe_state)"
STILL_OVER=0; limits_ok || STILL_OVER=1

join() { local IFS=,; echo "$*"; }
SUMMARY="target-guard: "
if [ "${#REMOVED[@]}" -gt 0 ]; then SUMMARY+="removed $(join "${REMOVED[@]}"). "; fi
if [ "${#SKIPPED[@]}" -gt 0 ]; then SUMMARY+="Skipped: $(join "${SKIPPED[@]}"). "; fi
if [ "${#FAILED[@]}" -gt 0 ]; then SUMMARY+="Could not delete $(join "${FAILED[@]}"). "; fi
for n in "${NOTES[@]}"; do SUMMARY+="Note: $n. "; done
SUMMARY+="Now: ${AFTER}."
[ "$STILL_OVER" = 1 ] && SUMMARY+=" Still over limits."

if [ "$MODE" = hook ]; then
  [ "${#REMOVED[@]}" -eq 0 ] && [ "${#FAILED[@]}" -eq 0 ] && [ "$STILL_OVER" = 0 ] && hook_done
  CONTEXT="$SUMMARY"
  if [ "$STILL_OVER" = 1 ]; then
    CONTEXT+=" To free space: close other cargo/rustc processes and re-run tools/dev/target-guard.sh, or delete target/scratch-* dirs. See docs/dev/build-disk.md."
  fi
  force=""; [ "$HOPELESS" = 1 ] && force=force
  if low_free_warning $force; then SUMMARY+=" $WARN_SYS"; CONTEXT+=" $WARN_CTX"; fi
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
for f in "${FAILED[@]}"; do echo "could not delete $f"; done
for n in "${NOTES[@]}"; do echo "note: $n"; done
[ "$HOPELESS" = 1 ] && culprit_lines
echo "after:  $AFTER"
[ "$STILL_OVER" = 1 ] && echo "WARNING: still over limits."
exit 0
