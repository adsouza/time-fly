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

## Region bench (Task 10)

`--region-bench` (web `?region-bench`) waits for the interactive globe, then enters the Alps
(46.5° N, 10° E, present), Scandinavia (63° N, 15° E, 21 ka) and the Pangaea interior (era-frame
22° S, 18° W, 250 Ma) in turn. In each it flies a 60 s scripted path (`bench/region/path.rs`): a
low pass at 300 m above the ground, a climb to 30 km, a run along the coast (or, when the
nearest coast lies in the apron, along the field's edge looking at it), and a sweep toward the
apron. Then it re-enters the Alps ten times for the warm-generation and memory cycles. One block
per region and a summary block; exit 0 on PASS, 3 on FAIL.

```sh
cargo build --release -p timefly
TIMEFLY_LOG_DIR=<fresh dir> BEVY_ASSET_ROOT=crates/timefly \
  target/release/timefly --region-bench --target-fps 60 --window 2560x1440
```

### Native, Apple M4 (Metal), 2026-11-23

Release build of `a27355a` plus the Task 10 changes (thin LTO), macOS 27.2.1, 60 Hz display.
`AutoNoVsync` was requested, but frames stayed paced to the display (median 16.70 ms), so ruling
R14's display-paced rule decided.

**Resolution caveat.** The main display was a 1920 × 1080 monitor that day, so macOS clamped the
requested 2560 × 1440 window to **1920 × 951** physical pixels. M1's numbers were taken at
2560 × 1440 on a larger display. A rerun at 2560 × 1440 (the built-in Retina display, or a
1440p monitor) is still owed; the GPU was not the limit at this size (p95 18.2–18.7 ms with no
missed frames).

Two identical runs. Run 1 was the first launch of the new binary; run 2 followed it.

| Region | Generation (first entry) | Tiles fetched | Load: request to first path frame | Load worst frame | Flight median / p95 / worst | Missed (> 25 ms) | Result |
|---|---|---|---|---|---|---|---|
| Alps, run 1 | 683 ms (cold: manifest and overview too) | 2 (270,951 B) | 3.25 s | **1,125.8 ms** | 16.70 / 18.23 / 20.67 ms | 0 of 3,600 | PASS |
| Alps, run 2 | 633 ms | 2 (270,951 B) | 2.46 s | 83.1 ms | 16.69 / 18.69 / 20.26 ms | 0 of 3,601 | PASS |
| Scandinavia, run 1 | 415 ms | 2 (270,945 B) | 2.25 s | 34.2 ms | 16.70 / 18.21 / 23.91 ms | 0 of 3,600 | PASS |
| Scandinavia, run 2 | 418 ms | 2 (270,945 B) | 2.25 s | 22.3 ms | 16.70 / 18.60 / 21.86 ms | 0 of 3,600 | PASS |
| Pangaea, run 1 | 333 ms | 0 | 2.15 s | 24.4 ms | 16.70 / 18.21 / 19.67 ms | 0 of 3,601 | PASS |
| Pangaea, run 2 | 332 ms | 0 | 2.17 s | 24.4 ms | 16.69 / 18.68 / 20.70 ms | 0 of 3,601 | PASS |

The load window holds the 1 s settle at the start pose and the 0.8 s entering fade. Each path
segment (low pass, climb, coast run, apron sweep) had a median of 16.65–16.73 ms and a worst
frame of at most 23.9 ms, with no stalls (> 33.3 ms). The coast search found the Ligurian coast
208 km from the Alps centre, a coast 467 km from central Sweden at 21 ka (both in the apron, so
the run follows the field's edge facing them), and none within 1,150 km of the Pangaea centre.

**First-region worst frame (ruling R11).** Run 1: 1,125.8 ms; run 2: 83.1 ms. In run 1 the log
shows `view switched region_view=true` at the fade's midpoint and nothing for 1.1 s after: the
first frame drawn by the region camera, when Metal compiled the terrain, water and sky pipelines
for a binary it had not seen. Run 2 found them in Metal's shader cache. The overview decode is
not the cause natively (it runs on the asset loader's thread; the tiles and overview were ready
0.25 s after the request). R11 gates Chrome WebGPU, where the overview also decodes on the main
thread and the pipelines compile through Tint on every cold page load, so the browser number is
the one that decides.

**Warm generation** (the ten Alps re-entries, tiles and overview cached): median 449 ms, max
464 ms (run 1) and 467 ms (run 2). Target ≤ 3,000 ms: PASS.

**Memory cycles** (phys_footprint, includes GPU allocations on Apple silicon; sampled on the
globe 1.5 s after each leave, and in the region 1 s after entering):

| Run | On the globe after leaves 1–10 (MiB) | Worst of 2–10 vs 1 | In the region (MiB) |
|---|---|---|---|
| 1 | 697.4, 710.0, 697.6, 697.6, 697.7, 710.2, 710.3, 697.9, 697.9, 710.8 | +1.9% | 786.9–788.2 |
| 2 | 719.5, 719.5, 719.8, 732.4, 732.5, 732.6, 720.1, 720.2, 720.3, 733.0 | +1.9% | 809.4–810.5 |

Memory returns to the first cycle's level within 2% (limit 10%): PASS. The two levels about
12.6 MiB apart alternate without a trend (an allocator or GPU heap that is kept or released),
and the in-region footprint is flat across the ten entries. Peak footprint over a run:
863–868 MiB. Summary: `result: PASS`, exit 0, both runs.

### Chrome WebGPU (controller, ruling R20)

_To be run by the controller in the in-app browser pane._ Build and serve from
`crates/timefly`:

```sh
cd crates/timefly
PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo run --profile web-release -p timefly \
  --target wasm32-unknown-unknown
```

Open `http://127.0.0.1:1334/?region-bench` and leave the page alone for about four minutes. The
console prints three `===== Time Fly region bench: …` blocks and one `===== Time Fly region
bench summary =====` block, each as one message (target 30 fps, display-paced rule). The R11
line, `first-region worst frame (R11 gate <= 250 ms, …)`, is in the Alps block and the summary,
and on the web it decides the result. Generation target ≤ 8,000 ms (warm). The memory cycles use
the wasm linear memory (it never shrinks, so the check is that it stops growing); the JS heap
(`performance.memory`, Chrome only) is reported beside it.

Checked by the implementer with curl against that server (`web-release` build, wasm-bindgen
output 64.97 MB): `/?region-bench`, `assets/data/tiles/manifest.json` (353,054 B),
`assets/data/tiles/overview.tfgrid` (12,430,497 B), `assets/data/tiles/r08c38.tfgrid`,
`r05c39.tfgrid` and `assets/data/globe/manifest.json` all return 200.

## Sizes: packages and Pages (Task 10)

| Item | Bytes |
|---|---|
| Region tiles shipped (`assets/data/tiles`, no `bake.log`): 2,592 tiles + overview + manifest | 256,798,552 (244,015,001 + 12,430,497 + 353,054) |
| Globe bake shipped (`assets/data/globe`, no `bake.log`) | 17,177,338 |
| Staged install tree, measured with `stage.sh` on this bake (binary aside) | 267 MiB by `du` (globe 17 MiB, tiles 250 MiB) |
| Web app (wasm-bindgen output, `web-release`) | about 65.0 MB |

- **Linux packages:** the tiles are already zstd-compressed, so xz gains almost nothing on
  them. Expect each package to grow by about 257 MB: the `.deb` from about 123 MB download /
  230 MB installed to about 380 MB / 490 MB, and the Flatpak bundle by about the same 257 MB.
  The CI run will give the exact sizes (`du -sh staged` in the log).
- **GitHub Pages:** wasm 65.0 MB + JS and page about 0.1 MB + globe 17.2 MB + tiles 256.8 MB +
  shaders and credits 0.06 MB ≈ **339 MB**, a third of the 1 GB site limit. The largest file is
  the wasm (65 MB, under the 100 MB file limit); the largest data file is the overview
  (12.4 MB). `pages.yml` now checks both limits before uploading.
