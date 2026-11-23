# Region worlds (M2): performance and size

## Tile bake (Task 1)

`cd tools/bake && uv run --locked timefly-bake tiles --raw ../../data/raw --out ../../crates/timefly/assets/data/tiles`

Measured on 2026-11-22 (Apple M4, 16 GB, Python 3.13).

| Measure | Value |
|---|---|
| Tiles | 2,592 (36 × 72), each 301 × 301 × 2 float16, zstd 19 |
| Total tile bytes | 244,015,001 (about 244.0 MB; `du` reports 239 MiB on disk) |
| Largest tile | 172,862 bytes |
| Wall time | 178.5 s (peak resident set 3.3 GB); 186.9 s with the overview (rerun 2026-11-22, 3.4 GB) |
| Overview (ruling R7) | `overview.tfgrid`, 2161 × 4321 × 2 float16 (5′ node lattice), 12,430,497 bytes (about 12.4 MB) |
| Budget | design estimate 150–250 MB; stop threshold 300 MB; Pages site limit 1 GB |

The total is inside the design's 150–250 MB estimate and about a quarter of the Pages limit
(the 17 MB globe set shares it). A region needs at most 4 tiles (about 0.7 MB at the largest tile
size) on the web.

Inputs: `etopo/ETOPO_2022_v1_60s_N90W180_surface.nc` (cell-registered, averaged onto nodes) and
the 1991–2020 30″ GeoTIFF of `koppen/koppen_geiger_tif.zip` (2 × 2 mode onto nodes); see
`data/README.md`.

## Region generation (Task 4)

`cargo test --release -p timefly_core --lib generation_timing -- --ignored --nocapture`

Measured on 2026-11-22 (Apple M4, release profile, one thread). Synthetic data: the deep case
is a 150-plate crust model (two plates with footprints, 148 more that every lookup still
tries, as in the real bake's 149) at 100 Ma; the recent cases read constant tiles and ICE-6G
frames at 21 ka.

| Region (1025² inner + 513² apron) | `new` (lattice + coast distance) | Total |
|---|---|---|
| Deep, 10°N 60°E | 0.09 s | 0.56 s |
| Recent, 46°N 8°E (54 tiles, 2.29 M lattice nodes) | 0.28 s | 0.65 s |
| Recent, at a cube corner (48 tiles, 4.11 M lattice nodes, the worst case) | 0.57 s | 0.96 s |

Design targets: ≤ 3 s native (parallel), ≤ 8 s web. Single-threaded native is already under
1 s, so the web budget has about 8× headroom for wasm's slowdown. `new` runs before any row
and is not time-sliced: on the web it is one 0.1–0.6 s step. Lattice nodes take 8 bytes each
(33 MB at the worst case), freed when the generator drops.

Tiles a region reads (`TileSet::needed_for`, inner field only, 5 km margin): 2–4 up to 60°
latitude, 6 at 70°, 10 at 80° and all 72 of the polar row within 150 km of a pole (never more
than 72 in a sweep of 80°–90°). Since ruling R7 the apron reads only the 12.4 MB overview, which
is fetched once; before it, the apron needed 30–360 tiles.

### Incremental construction (ruling R8)

`RegionPrep::step(max_nodes)` does at most `max_nodes` units (a planning point, a lattice node
or an apron mask texel); the coast-distance transform runs alone in the final step. Same run as
above, `RegionPrep::new` takes under 0.01 ms:

| Region | step(256): slowest | step(1024): slowest | step(4096): slowest | Final transform step |
|---|---|---|---|---|
| Deep (150 plates, about 3.9 µs per node) | 1.0 ms | 4.0 ms | 15.4 ms | 0.73 ms |
| Recent, mid-latitude (about 0.15 µs per node) | 0.05 ms | 0.16 ms | 0.57 ms | 0.73 ms |
| Recent, cube corner | 0.06 ms | 0.18 ms | 0.70 ms | 0.74 ms |

For the 6 ms web budget: loop `step(256)` until the frame's budget is spent (each call is at most
about 1 ms deep, natively), or call `step(1024)` once per frame (at most about 4 ms deep). Wasm is
slower than native, and real plate footprints cost more per deep node than the synthetic ones.
