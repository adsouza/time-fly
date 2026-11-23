#!/usr/bin/env bash
# Renders the region look screenshots (M2 Task 8) with examples/region_tour.rs: the bench regions
# (the pre-R24 ones too) and one region per vegetation era, from 8 km above each centre looking
# north.
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
  "bench-himalayas-0ka      28,86.9,0"
  "bench-sundaland-21ka     5,107,21000"
  "bench-japan-1ma          36,138.5,1000000"
  "bench-beringia-21ka      62,-175,21000"
  "bench-rockies-21ka       48.5,-113.5,21000"
  "bench-panama-3ma         9,-79.5,3000000"
  "bench-red-sea-0ka        20,37.5,0"
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
