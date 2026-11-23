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
| Deep 1 Ma (ETOPO-dominant, 1,852 m), 46°N 8°E, before fix round 1 | 6.01 s | 6.35 s |
| Deep 1 Ma, at a cube corner, before fix round 1 | 10.91 s | 11.26 s |
| Deep 1 Ma, 46°N 8°E, two-level path (fix round 1) | 0.46 s | 0.81 s |
| Deep 1 Ma, at a cube corner, two-level path (fix round 1) | 0.79 s | 1.14 s |

Fix round 1: ETOPO-dominant deep snapshots (50 ka, 100 ka, 1 Ma) used to resolve plates at
every 463 m node (about 4 µs each). They now run the globe's reference only on a 27.75 km
support lattice and read the ETOPO term per node (about 0.2 µs), so they cost about the same as
recent regions. Times are single-threaded with a cold support cache.

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
| Deep 1 Ma, mid-latitude (fix round 1, cold cache) | 0.36 ms | 0.66 ms | 1.24 ms | 1.37 ms |
| Deep 1 Ma, cube corner (fix round 1, cold cache) | 0.45 ms | 0.77 ms | 1.38 ms | 1.37 ms |

For the 6 ms web budget: loop `step(256)` until the frame's budget is spent (each call is at most
about 1 ms deep, natively), or call `step(1024)` once per frame (at most about 4 ms deep). Wasm is
slower than native, and real plate footprints cost more per deep node than the synthetic ones.

## Region generation in the shell (Task 5)

`cargo test -p timefly --test region_loading -- --ignored real_region_timing --nocapture`

Measured on 2026-11-22 (Apple M4, 10 cores, dev profile: workspace crates at opt-level 1,
dependencies at 3; rerunning with the workspace crates at opt-level 3 gave the same times within
3%). The real bake and tiles under `crates/timefly/assets/`, headless (`MinimalPlugins`), full
resolution (1025² inner + 513² apron). Times run from `RegionRequested` to the grid being ready
(`RegionGenerated.millis`): frame loads, tile loads and checks, the prep, and the rows. One app
runs the cases in order, so the tile cache and the overview carry over.

Native generation runs the prep (`RegionPrep`, sequential by design) on one
`AsyncComputeTaskPool` task, then the rows as 16-row blocks across the pool (`scope`). Bevy's
default pool options give async compute 3 threads on this machine; the scope's calling thread
helps too.

| Region | Total | Prep | Rows (parallel) | Tiles fetched |
|---|---|---|---|---|
| Alps 46.5°N 10°E, present, cold (first region of the session) | 747 ms | 359 ms | 138 ms | 2 (270,951 B) + overview |
| Alps, present, warm cache | 496 ms | 354 ms | 137 ms | 0 |
| Alps, present, warm, sequential (one thread, for comparison) | 724 ms | — | — | 0 |
| Scandinavia 63°N 15°E, 21 ka | 444 ms | 300 ms | 135 ms | 2 (270,945 B) |
| Alps, 1 Ma (deep, ETOPO-dominant two-level path) | 761 ms | 608 ms | 147 ms | 0 (cached) |
| Alps, 1 Ma, warm, sequential | 1,005 ms | — | — | 0 |
| Pangaea interior 0°N 20°E, 250 Ma | 317 ms | 130 ms | 185 ms | 0 (needs none) |

- **Parallel rows:** about 2.6× faster than one thread (360 ms → 138 ms of rows at the
  present-day Alps). The deep field's support cache (`RwLock<HashMap>`) shows no contention at
  this thread count: the 1 Ma rows take 147 ms against 138 ms for the present-day ones. The
  prep stays sequential and is now most of the time (70–80%), so more row threads would gain
  little; a parallel prep would be the next step if generation ever needs to be faster.
- **Cold start:** the first region of a session costs about 250 ms more: the tile manifest
  (353,054 B, 2,592 entries) and the overview (12,430,497 B, SHA-256 checked, decoded to
  37 MB of f16) load once and stay for the session.
- **Main thread:** finding the needed tiles (a probe field's `tiles_needed`) takes 0.1 ms for
  recent snapshots and 2.7 ms for the 1 Ma deep one; everything else runs on the pool.
- **Target:** ≤ 3 s native. Every case is under 0.8 s.

**Tile bytes for a mid-latitude region (web):** the present-day Alps region reads 2 tiles,
270,951 B. The first region of a session also fetches the tile manifest (353,054 B) and the
overview (12,430,497 B), about 13.1 MB in all; later regions fetch only the tiles their inner
field reads that are not cached (the cache keeps up to 96). Frames come on top: 1–3 globe frame
files per region, about 0.2–0.5 MB each (already in the browser's HTTP cache from the globe).

**Time-boxed web path, on native:** `budgeted_run_is_time_boxed` (257² + 129², tiny bake,
6 ms budget) finishes in 8 slices, the longest 6.05 ms: one slice overshoots its budget by at
most one prep step or one row. Wasm timings belong to Task 10's web benchmark.
