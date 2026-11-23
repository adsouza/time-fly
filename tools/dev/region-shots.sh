#!/usr/bin/env bash
# Renders the region look screenshots (M2 Task 8) with examples/region_tour.rs: the three bench
# regions and one region per vegetation era, from 8 km above each centre looking north.
#
#   tools/dev/region-shots.sh [out_dir] [WxH] [name-filter]
#
# Each run gets a fresh TIMEFLY_LOG_DIR (a crash record from one run must not block the next).
# Centres are era-frame LAT,LON (what a globe click gives) and ages in years.
set -euo pipefail
cd "$(dirname "$0")/../.."
out="${1:-docs/testing/region-m2}"
size="${2:-1280x720}"
filter="${3:-}"

# name  LAT,LON,AGE_YEARS
shots=(
  "bench-alps-0ka           46.5,10,0"
  "alps-21ka-glacial        46.5,10,21000"
  "bench-scandinavia-21ka   63,15,21000"
  "bench-pangaea-250ma      -22,-18,250000000"
  "era-540ma-barren         -16,0,540000000"
  "era-400ma-early-plants   -28,-27,400000000"
  "era-300ma-first-forests  -13,-48,300000000"
  "era-100ma-flowering      47,-42,100000000"
  "era-10ma-modern          35,36,10000000"
  "sahara-0ka               23,10,0"
)

cargo build -q -p timefly --example region_tour
for entry in "${shots[@]}"; do
  read -r name region <<<"$entry"
  if [[ -n "$filter" && "$name" != *"$filter"* ]]; then
    continue
  fi
  logs="$(mktemp -d "${TMPDIR:-/tmp}/timefly-shots.XXXXXX")"
  echo "== $name ($region)"
  TIMEFLY_LOG_DIR="$logs" cargo run -q -p timefly --example region_tour -- \
    "$out" "$size" "$region" "$name" shaded
done
