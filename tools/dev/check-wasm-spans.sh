#!/usr/bin/env bash
# Fails if a wasm32 build of timefly would enable Bevy's trace spans (ruling R29: on wasm,
# tracing-wasm writes a performance mark and measure per span, which the browser never frees).
#
#   tools/dev/check-wasm-spans.sh            check the real wasm feature tree
#   tools/dev/check-wasm-spans.sh --stdin    check a `cargo tree -e features` listing on stdin
set -euo pipefail
cd "$(dirname "$0")/../.."
if [[ "${1:-}" == "--stdin" ]]; then
  tree="$(cat)"
else
  tree="$(cargo tree --locked -e features --target wasm32-unknown-unknown -p timefly -i bevy_ecs)"
fi
if grep -qE 'bevy_ecs feature "trace"' <<<"$tree"; then
  echo "check-wasm-spans: the wasm32 build enables bevy_ecs \"trace\" (ruling R29)." >&2
  grep -E 'feature "trace"' <<<"$tree" >&2 || true
  exit 1
fi
echo "check-wasm-spans: no bevy_ecs trace in the wasm32 feature tree"
