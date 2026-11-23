#!/usr/bin/env bash
# Tests check-wasm-spans.sh's verdict on fixed listings, then runs it on the real tree.
set -euo pipefail
cd "$(dirname "$0")/../.."
check=tools/dev/check-wasm-spans.sh
bad='bevy_ecs v0.20.1
├── bevy_ecs feature "default"
└── bevy_ecs feature "trace"
    └── bevy_app feature "trace"'
good='bevy_ecs v0.20.1
├── bevy_ecs feature "default"
└── bevy_ecs feature "multi_threaded"'
if "$check" --stdin <<<"$bad" 2>/dev/null; then
  echo "FAIL: a tree with bevy_ecs trace passed" >&2; exit 1
fi
"$check" --stdin <<<"$good" >/dev/null || { echo "FAIL: a clean tree failed" >&2; exit 1; }
"$check"
echo "test-check-wasm-spans: ok"
