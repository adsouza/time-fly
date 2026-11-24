#!/usr/bin/env bash
# Renders the region look screenshots (M2 Task 8) with examples/region_tour.rs: the bench regions
# (the pre-R24 ones too) and one region per vegetation era, from 8 km above each centre looking
# north.
#
#   tools/dev/region-shots.sh [out_dir] [WxH] [name-filter...]
#
# Any number of filters may follow; a scene runs if its name contains any of them.
#
# Each run gets a fresh TIMEFLY_LOG_DIR (a crash record from one run must not block the next).
# Centres are era-frame LAT,LON (what a globe click gives) and ages in years.
set -euo pipefail
cd "$(dirname "$0")/../.."
out="${1:-docs/testing/region-m2}"
size="${2:-1280x720}"
filters=("${@:3}")

# name  LAT,LON,AGE_YEARS  AGL_M (camera height above max(ground, 0) at the centre)
shots=(
  "bench-alps-0ka           46.5,10,0 8000"
  "alps-21ka-glacial        46.5,10,21000 8000"
  "bench-scandinavia-21ka   63,15,21000 8000"
  "bench-pangaea-250ma      -22,-18,250000000 8000"
  "era-540ma-barren         -16,0,540000000 8000"
  "era-400ma-early-plants   -28,-27,400000000 8000"
  "era-300ma-first-forests  -13,-48,300000000 8000"
  "era-100ma-flowering      47,-42,100000000 8000"
  "era-10ma-modern          35,36,10000000 8000"
  "sahara-0ka               23,10,0 8000"
  "bench-himalayas-0ka      28,86.9,0 8000"
  "bench-sundaland-21ka     5,107,21000 8000"
  "bench-japan-1ma          36,138.5,1000000 8000"
  "bench-beringia-21ka      62,-175,21000 8000"
  "bench-rockies-21ka       48.5,-113.5,21000 8000"
  "bench-panama-3ma         9,-79.5,3000000 8000"
  "bench-red-sea-0ka        20,37.5,0 8000"
  "grand-canyon-0ka         36.1,-112.1,0 8000"
  "grand-canyon-0ka-low     36.1,-112.1,0 1500"
  "red-sea-0ka-low          20,37.5,0 400"
  "sahara-0ka-low           23,10,0 400"
  "sahara-0ka-high          23,10,0 40000"
  "beringia-21ka-low        62,-175,21000 400"
  "panama-3ma-low           9,-79.5,3000000 400"
  "rockies-21ka-low         48.5,-113.5,21000 1500"
)

cargo build -q -p timefly --example region_tour
for entry in "${shots[@]}"; do
  read -r name region agl <<<"$entry"
  if ((${#filters[@]})); then
    match=0
    for f in "${filters[@]}"; do
      [[ "$name" == *"$f"* ]] && match=1
    done
    ((match)) || continue
  fi
  logs="$(mktemp -d "${TMPDIR:-/tmp}/timefly-shots.XXXXXX")"
  echo "== $name ($region, ${agl} m AGL)"
  TIMEFLY_LOG_DIR="$logs" cargo run -q -p timefly --example region_tour -- \
    "$out" "$size" "$region" "$name" shaded "$agl"
done
