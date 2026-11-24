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

Design targets: ≤ 3 s native (parallel), ≤ 8 s web. On the web the generation is not just
slower code: it runs in time-boxed slices of 6 ms per frame (`WEB_GEN_BUDGET_MS`), so at 60 Hz
it gets 6 of every 16.7 ms and the wall time is about 2.8 × its CPU time (about 1.4 × with the
12 ms slices taken while the fade overlay covers the view, `WEB_GEN_LARGE_BUDGET_MS`; a first
entry from the globe generates behind the visible globe, so it gets 6 ms). The 8 s target
therefore leaves about 2.9 s of wasm CPU time, not 8 s: these synthetic regions (under 1 s
single-threaded) keep about 3× headroom for wasm's slowdown, but the real ETOPO-dominant deep
regions take 0.9–3.4 s single-threaded natively (the Aegean worst, see the final fix wave
below), which at a 1.5–2.5× wasm slowdown projects to about 12–20 s on the web for the Aegean's
first entry. The web region bench gates every region's first entry against 8 s for that reason.
(Before ruling R8, `new` ran before any row as one untimed step; it is now incremental.) Lattice nodes take 8 bytes each
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

The runs below through the final fix wave bench the **Alps** as region 1: they are the
pre-swap baseline. Since ruling R24 region 1 is the Himalayas (see the latest run at the end of
this section).

`--region-bench` (web `?region-bench`) waits for the interactive globe, then enters the Alps
(46.5° N, 10° E, present), Scandinavia (63° N, 15° E, 21 ka), the Pangaea interior (era-frame
22° S, 18° W, 250 Ma) and, since ruling R22, the Aegean (era-frame 38° N, 24.5° E, 1 Ma) in
turn. In each it flies a 60 s scripted path (`bench/region/path.rs`): a
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

**Resolution (ruling R21).** These M4 runs are at **1920 × 951**, the display's size: the main
display was a 1920 × 1080 monitor, so macOS clamped the requested 2560 × 1440 window. M1's
numbers were taken at 2560 × 1440 on a larger display. The milestone's 2560 × 1440 desktop
measurement is taken on the owner's Linux machine (spec §15), as an explicit step of
`docs/testing/linux-vulkan.md` section 7:

```sh
WGPU_BACKEND=vulkan time-fly --region-bench --target-fps 60 --window 2560x1440
```

At 1920 × 951 the GPU was not the limit (p95 18.2–18.7 ms, no missed frames).

The first two runs below predate the Aegean region (three regions each); the fix-round run
after them has all four. Two identical three-region runs. Run 1 was the first launch of the new binary; run 2 followed it.

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

### Four regions with the Aegean (ruling R22), 2026-11-23

Release build of `4e682af` plus the fix-round changes, same M4 and display, **1920 × 951**
(R21). The Aegean's coast search finds a coast 4.9 km from the centre, inside the inner field,
and the run along it stays within 1.0 km of a coastline (sampled every 375 m along the run, ring
search in 250 m steps).

| Region | Generation (first entry) | Tiles fetched | Load: request to first path frame | Load worst frame | Flight median / p95 / worst | Missed (> 25 ms) | Result |
|---|---|---|---|---|---|---|---|
| Alps | 683 ms (cold) | 2 (270,951 B) | 2.53 s | 84.7 ms | 16.70 / 18.30 / 21.18 ms | 0 of 3,601 | PASS |
| Scandinavia | 432 ms | 2 (270,945 B) | 2.27 s | 24.4 ms | 16.70 / 18.33 / 31.91 ms | 2 of 3,600 (0.06%) | PASS |
| Pangaea | 332 ms | 0 | 2.17 s | 23.7 ms | 16.69 / 18.31 / 20.78 ms | 0 of 3,600 | PASS |
| **Aegean** | **2,216 ms** | 2 (305,816 B) | 4.03 s | 24.5 ms | 16.70 / 18.28 / 20.18 ms | 0 of 3,601 | PASS |

- **Aegean generation:** 2,216 ms natively on its first entry, the slowest real case (the
  ETOPO-dominant two-level deep path), still under the 3 s native target. On the web's
  time-boxed path it is the region to watch against the 8 s target.
- **Aegean flight:** the coast run (900 frames along the shore, water, foam and beach in view)
  had a median of 16.72 ms and a worst frame of 19.40 ms, no slower than the other segments.
- **First-region worst frame (R11):** 84.7 ms. The new binary's shaders were unchanged, so
  Metal's cache from the earlier runs still held them.
- **Warm generation:** median 449 ms, max 467 ms. **Memory cycles:** 721.4, 721.5, 709.1,
  709.2, 709.2, 721.8, 722.0, 722.0, 709.7, 709.9 MiB on the globe; worst of 2–10 +0.1% of
  cycle 1 (limit +10%); in the region 798.3–800.4 MiB. Summary `result: PASS`, exit 0.

### Final fix wave, native, 2026-11-23

Release build of `952ea91`'s tree (the binary reports `7a10889`, the commit before; the build
was made from the working tree that became `952ea91`), same M4, display and command, at
**1920 × 951** (R21). Summary block, verbatim:

```text
regions (R14 frame rate): Alps PASS, Scandinavia PASS, Pangaea interior PASS, Aegean PASS
first-region worst frame (R11 gate <= 250 ms, request to first path frame, overview decode included): 24.73 ms -> PASS (native: reported only, the gate applies in Chrome WebGPU)
warm generation (10 re-entries of Alps, target <= 3000 ms): median 449 ms, max 467 ms -> PASS
first entries (cold tiles; reported only natively): Alps 681 ms, Scandinavia 416 ms, Pangaea interior 332 ms, Aegean 2382 ms
memory cycles (phys_footprint, includes GPU allocations on Apple silicon, on the globe after each leave): 1: 744.1 MiB, 2: 744.2 MiB, 3: 756.8 MiB, 4: 756.9 MiB, 5: 757.0 MiB, 6: 744.6 MiB, 7: 744.7 MiB, 8: 744.8 MiB, 9: 757.4 MiB, 10: 757.5 MiB; worst of cycles 2-10 is +1.8% of cycle 1 (limit +10%) -> PASS
  in the region: 1: 834.3 MiB, 2: 834.4 MiB, 3: 834.5 MiB, 4: 833.8 MiB, 5: 833.9 MiB, 6: 834.8 MiB, 7: 834.8 MiB, 8: 835.0 MiB, 9: 834.3 MiB, 10: 834.4 MiB
result: PASS
```

| Region | Generation (first entry) | Tiles fetched | Load: request to first path frame | Load worst frame | Flight median / p95 / worst | Missed (> 25 ms) | Result |
|---|---|---|---|---|---|---|---|
| Alps | 681 ms (cold) | 2 (270,951 B) | 2.53 s | 24.7 ms | 16.71 / 18.26 / 20.30 ms | 0 of 3,600 | PASS |
| Scandinavia | 416 ms | 2 (270,945 B) | 2.25 s | 24.3 ms | 16.70 / 18.27 / 31.29 ms | 1 of 3,601 (0.03%) | PASS |
| Pangaea | 332 ms | 0 | 2.17 s | 24.9 ms | 16.71 / 18.29 / 22.01 ms | 0 of 3,601 | PASS |
| Aegean | 2,382 ms | 2 (305,816 B) | 4.22 s | 22.9 ms | 16.71 / 18.31 / 20.05 ms | 0 of 3,600 | PASS |

- **First-region worst frame (R11) and the pipeline warm-up.** The region shaders were made cold
  for one pair of runs by a temporary no-op edit to `terrain.wesl`, `terrain_prepass.wesl`,
  `water.wesl` and `sky.wesl` (Metal's shader cache is per user and shared, so it was not
  cleared; a changed source misses it). Without the warm-up: **1,151.5 ms** (as Task 10's
  1,125.8 ms on a new binary). With it: **27.0 ms**. With the warm cache: 84.7–85.3 ms before,
  24.7–25.2 ms after. The globe's first interactive frame came 2.2 s after launch with the cold
  region shaders (0.8 s without the warm-up), and 0.4–0.9 s with a warm cache.
- **Memory.** The first version drew the stand-in to the window and cast its shadows during
  loading: it left about 110 MiB resident for the session (817–830 MiB on the globe after
  leaves). The stand-in now draws into a 1 × 1 target and casts shadows only once the globe is
  interactive: 744–757 MiB, against 705–720 MiB without a warm-up; in the region 834 MiB
  against 798 MiB.
- **Aegean generation:** 2,382 ms (2,216 ms before the wave): the two-level path now takes the
  plate's own terms within 150 m of sea level (finding 4).
- **Scrub bench** (M1, the same binary, `--scrub-bench`): PASS; see the final fix report.

#### 2560 × 1440 on the M4 (`5396955`)

The main display became a 3840 × 2160 monitor during the session, so the requested 2560 × 1440
window fit. That display did not pace `AutoNoVsync` frames, so the uncapped rule (p95 ≤ 16.67 ms)
decided. All four regions PASS, summary `result: PASS`:

| Region | Generation (first entry) | Load worst frame | Flight median / p95 / worst | Stalls (> 33.3 ms) | Result |
|---|---|---|---|---|---|
| Alps | 725 ms (cold) | 33.8 ms (R11 line) | 10.35 / 13.00 / 28.69 ms | 0 | PASS |
| Scandinavia | 440 ms | 16.9 ms | 11.19 / 15.74 / 87.96 ms | 5 | PASS |
| Pangaea | 344 ms | 16.8 ms | 11.03 / 14.36 / 48.64 ms | 6 | PASS |
| Aegean | 2,647 ms | 27.7 ms | 7.14 / 9.49 / 26.57 ms | 0 | PASS |

Warm generation median 486 ms, max 537 ms. Memory cycles 1,142.7–1,153.8 MiB on the globe
(+1.0%), 1,227.8–1,234.4 MiB in the region (larger window, so not comparable with the
1920 × 951 runs). An earlier attempt was invalid: the display changed mid-run and the window
resized (2338 × 1237 to 2567 × 1237).

While the bench runs, a translucent banner at the top centre shows the current stop (region,
age, `k of 9` with the pan counted), `Next: …` on the globe, and at the end the result.

#### With the Red Sea (R30), native M4, 2560 × 1440 (`07658ba`, 2026-11-23)

The rerun after jobs 8 and 9 (native spans unchanged; the Red Sea second). This time the 4K
display paced every flight at 60 Hz (medians 16.66–16.67 ms), so the display-paced rule decided
everywhere, and **every gate passed**, Pangaea included: its earlier FAIL (below) came from a
pacing switch halfway through its flight, which did not recur, so no banner-suppressed run was
needed.

| Stop | Generation (first entry) | Tiles / frames fetched | Load worst frame | Flight median / p95 / worst | Missed (> 25 ms) | Result |
|---|---|---|---|---|---|---|
| Pangaea interior | 582 ms | 0 / 2 | 18.1 ms (R11 line) | 16.67 / 17.36 / 39.45 ms | 7 of 3,601 (0.19%) | PASS |
| Red Sea | 515 ms | 2 (275,711 B) / 1 | 18.5 ms | 16.66 / 17.44 / 28.28 ms | 4 (0.11%) | PASS |
| Himalayas | 330 ms | 1 / 0 | 32.6 ms | 16.66 / 17.09 / 35.01 ms | 2 (0.06%) | PASS |
| Sundaland | 330 ms | 2 / 2 | 31.5 ms | 16.67 / 17.03 / 18.83 ms | 0 | PASS |
| Japan | 1,998 ms | 4 / 2 | 32.6 ms | 16.66 / 17.15 / 27.30 ms | 3 (0.08%) | PASS |
| Beringia | 398 ms | 2 / 1 | 25.9 ms | 16.66 / 17.61 / 27.52 ms | 9 (0.24%) | PASS |
| globe pan (8 s) | — | — | — | 16.67 / 17.53 / 18.06 ms (480 frames) | — | PASS |
| Rockies ice margin | 631 ms | 2 / **0** | 18.9 ms | 16.67 / 17.09 / 19.50 ms | 0 | PASS |
| Panama | 330 ms | 0 / 1 | 17.9 ms | 16.67 / 17.40 / 32.65 ms | 4 (0.11%) | PASS |

The Red Sea's coast run followed a coast 6.8 km from the centre and stayed within 5.8 km of it.
Frame reuse: the Himalayas (after the Red Sea, both 0 ka) and the Rockies (after Beringia)
fetched none; Beringia fetched one, as before (Japan's 1 Ma frames evict one 21 ka slot of the
4-slot cache), so inserting the Red Sea changed nothing there.

```text
===== Time Fly region bench summary =====
build: 0.1.0 (07658ba, release, aarch64-apple-darwin; system-spans)
regions (R14 frame rate): Pangaea interior PASS, Red Sea PASS, Himalayas PASS, Sundaland PASS, Japan PASS, Beringia PASS, Rockies ice margin PASS, Panama PASS
first-region worst frame (R11 gate <= 250 ms, request to first path frame, overview decode included): 18.12 ms -> PASS (native: reported only, the gate applies in Chrome WebGPU)
Himalayas load-window worst frame (cold tiles; reported only): 32.63 ms
warm generation (10 re-entries of Himalayas, target <= 3000 ms): median 314 ms, max 331 ms -> PASS
first entries (cold tiles; reported only natively): Pangaea interior 582 ms, Red Sea 515 ms, Himalayas 330 ms, Sundaland 330 ms, Japan 1998 ms, Beringia 398 ms, Rockies ice margin 631 ms, Panama 330 ms
memory cycles (phys_footprint, includes GPU allocations on Apple silicon, on the globe after each leave): 1: 1093.1 MiB, 2: 1077.0 MiB, 3: 1077.4 MiB, 4: 1087.5 MiB, 5: 1077.8 MiB, 6: 1077.5 MiB, 7: 1088.9 MiB, 8: 1079.4 MiB, 9: 1079.1 MiB, 10: 1089.3 MiB; worst of cycles 2-10 is -0.3% of cycle 1 (limit +10%) -> PASS
  in the region: 1: 1170.0 MiB, 2: 1170.7 MiB, 3: 1170.8 MiB, 4: 1164.0 MiB, 5: 1171.2 MiB, 6: 1171.2 MiB, 7: 1165.4 MiB, 8: 1172.8 MiB, 9: 1172.9 MiB, 10: 1165.9 MiB
result: PASS
===== end region bench summary =====
```

#### Final bench set, native M4, 2560 × 1440 (`5022c32`, 2026-11-23)

The single run after the Chrome-bench round and jobs 2–7 (Himalayas, Beringia and the Rockies
with the pan, Japan, Panama and Sundaland in the owner's order, the banner, the dry snowline,
clustering off). Main display 4K, so the window got 2560 × 1440; `AutoNoVsync` was not paced
for most of the run, so the uncapped rule (p95 ≤ 16.67 ms) decided.

| Stop | Generation (first entry) | Tiles / frames fetched | Load worst frame | Flight median / p95 / worst | Missed (> 25 ms) | Result |
|---|---|---|---|---|---|---|
| Pangaea interior | 583 ms | 0 / 2 | 30.1 ms (R11 line) | 11.62 / **16.96** / 29.92 ms | 3 of 4,631 | **FAIL** (p95 0.29 ms over) |
| Himalayas | 317 ms | 1 (123,550 B) / 1 | 18.3 ms | 11.06 / 13.05 / 21.93 ms | 0 | PASS |
| Sundaland | 311 ms | 2 / 2 | 19.2 ms | 9.89 / 11.59 / 21.94 ms | 0 | PASS |
| Japan | 2,108 ms | 4 / 2 | 20.1 ms | 8.21 / 11.54 / 33.10 ms | 2 | PASS |
| Beringia | 377 ms | 2 / 1 | 20.2 ms | 8.13 / 11.37 / 23.09 ms | 0 | PASS |
| globe pan (8 s) | — | — | — | 2.11 / 3.47 / 27.76 ms (3,345 frames) | — | PASS |
| Rockies ice margin | 643 ms | 2 / **0** | 19.3 ms | 10.59 / 13.39 / 22.77 ms | 0 | PASS |
| Panama | 311 ms | 0 / 1 | 17.2 ms | 6.80 / 8.24 / 19.96 ms | 0 | PASS |

- **Pangaea's FAIL is a pacing switch, not a hitch.** Its low pass and climb ran display-paced
  (medians 16.67 and 16.59 ms) and the second half uncapped (9.84 and 10.69 ms); the whole
  flight's median (11.62 ms) put it under the uncapped rule, whose p95 then caught the paced
  half. No stall (> 33.3 ms), 0.06% missed; under the display-paced rule it passes. Earlier
  2560 × 1440 runs had Pangaea at p95 14.36 ms.
- **Frame reuse:** the Rockies (after Beringia, same 21 ka) fetched no frame. Beringia fetched
  one: Japan's two 1 Ma frames between Sundaland and Beringia pushed one 21 ka frame out of the
  4-slot frame cache.
- No host stall anywhere; the banner did not affect a gate.

```text
===== Time Fly region bench summary =====
build: 0.1.0 (5022c32, release, aarch64-apple-darwin; system-spans)
regions (R14 frame rate): Pangaea interior FAIL, Himalayas PASS, Sundaland PASS, Japan PASS, Beringia PASS, Rockies ice margin PASS, Panama PASS
first-region worst frame (R11 gate <= 250 ms, request to first path frame, overview decode included): 30.10 ms -> PASS (native: reported only, the gate applies in Chrome WebGPU)
Himalayas load-window worst frame (cold tiles; reported only): 18.34 ms
warm generation (10 re-entries of Himalayas, target <= 3000 ms): median 311 ms, max 318 ms -> PASS
first entries (cold tiles; reported only natively): Pangaea interior 583 ms, Himalayas 317 ms, Sundaland 311 ms, Japan 2108 ms, Beringia 377 ms, Rockies ice margin 643 ms, Panama 311 ms
memory cycles (phys_footprint, includes GPU allocations on Apple silicon, on the globe after each leave): 1: 1093.2 MiB, 2: 1085.9 MiB, 3: 1093.5 MiB, 4: 1094.2 MiB, 5: 1094.3 MiB, 6: 1101.8 MiB, 7: 1094.8 MiB, 8: 1102.0 MiB, 9: 1102.2 MiB, 10: 1094.9 MiB; worst of cycles 2-10 is +0.8% of cycle 1 (limit +10%) -> PASS
  in the region: 1: 1169.8 MiB, 2: 1170.6 MiB, 3: 1170.1 MiB, 4: 1178.3 MiB, 5: 1178.7 MiB, 6: 1178.4 MiB, 7: 1178.8 MiB, 8: 1181.0 MiB, 9: 1178.6 MiB, 10: 1179.3 MiB
result: FAIL
===== end region bench summary =====
```

#### Bench regions since rulings R24–R26 (real bake, single-threaded)

`TIMEFLY_GENERATE=1 TIMEFLY_CANDIDATES=... cargo test --release -p timefly_core --test
real_bake_region bench_region_candidates -- --ignored --nocapture` (61 × 61 coarse samples of
the inner field; coast by the bench's 36-ray search; generation single-threaded, warm files):

| Region | Centre, age | Tiles | Land | Anomaly land | Coast | Ice | Max (inner) | Snowline | Plates | Generation |
|---|---|---|---|---|---|---|---|---|---|---|
| Pangaea interior | −22, −18, 250 Ma | 0 | | | none in 1,150 km | | | | | (unchanged) |
| Red Sea (R30) | 20, 37.5, 0 ka | 2 | 0.42 | 0.000 | 8 km | 0.00 | 1,593 m | 5,500 m | — | 0.64 s |
| Himalayas | 28, 86.9, 0 ka | 1 | 1.00 | 0.000 | 684 km | 0.00 | 7,576 m | 4,940 m (26.5% above) | — | 0.55 s |
| Sundaland | 5, 107, 21 ka | 2 | 0.97 | 0.948 | 40 km | 0.00 | 558 m | 4,513 m | — | 0.52 s |
| Japan | 36, 138.5, 1 Ma | 4 | 0.74 | — | 80 km | 0.00 | 2,588 m | 3,811 m | 610 | 2.36 s |
| Beringia | 62, −175, 21 ka | 2 | 0.85 | 0.850 | 80 km | 0.00 | 355 m | 1,573 m | — | 0.60 s |
| Rockies ice margin | 48.5, −113.5, 21 ka | 2 | 1.00 | 0.000 | 724 km | 0.67 | 3,524 m | 2,518 m (13% above) | — | 0.83 s |
| Panama | 9, −79.5, 3 Ma | 0 | 0.15 | — | 14 km | 0.00 | 113 m | 5,398 m | 203, 230 | 0.79 s |
| Aegean (left the bench) | 38, 24.5, 1 Ma | 2 | 0.29 | — | 6 km | 0.00 | 1,127 m | 3,671 m | 307, 308, 504, 510 | 3.27–3.33 s |

**The bench no longer holds the slowest known generation.** The Aegean (3.3 s single-threaded,
2.38 s natively in the final-fix-wave bench) is not benched any more; the slowest bench region is
Japan at 2.36 s, 72% of it (Panama 0.79 s, Sundaland 0.52 s). The web gate's worst case is
therefore unbenched; the Aegean's number is kept here for reference (owner decision R26, raised
by the controller).

#### The two-level deep path on the real bake (finding 4)

`cargo test --release -p timefly_core --test real_bake_region modern_path_on_the_real_bake -- --ignored --nocapture`
(single-threaded generation, then land/sea against the globe's reference where
|reference| > 150 m, about 8,300–12,900 checks per region):

| Age | East Mediterranean | Alps | Scandinavia | Aegean |
|---|---|---|---|---|
| 50 ka | 2.23 s | 1.30 s | 1.08 s | 3.44 s |
| 100 ka | 2.34 s | 1.35 s | 1.11 s | 3.44 s |
| 1 Ma | 2.20 s | 1.26 s | 0.95 s | 2.93 s |
| 2 Ma | 2.51 s | 1.48 s | 0.96 s | 3.25 s |

0 sign misses at every age (8 before the fix: 2 at 1 Ma, 6 at 2 Ma).

### Chrome WebGPU, in-app pane (indicative, ruling R23), 2026-11-23

Build `4f36589`, Chrome 156 in the Claude desktop app's browser pane, 1280 × 720, target 30 fps,
two runs. **Indicative only:** the pane paused `requestAnimationFrame` for 20–45 s at a time
(stalls ended by user input) while `document.visibilityState` stayed `visible`.

- **R11 first-region worst frame:** 218 ms and 225 ms: PASS (≤ 250 ms), the overview decode
  included.
- **Alps, Scandinavia, Pangaea:** flights PASS; first entries 2.1–3.2 s: PASS.
- **Aegean first entry: FAIL** against the 8 s web target: 613 and 628 time-boxed slices
  (`longest_ms=8.5`), a stall-free floor of about 628 × 16.7 ms ≈ 10.5 s. First entries
  generate on the globe (`NavPhase::Globe` for the bench, `Descending` for a double-click),
  where the overlay is clear, so the budget stayed 6 ms.
- **A load-timeout false positive:** during a pane stall the memory cycles' Alps re-entry
  timed out a frame (`data/globe/frames/f0000.tfgrid: the load timed out after 20 s`) although
  the GET returned 200: the timeout compared wall-clock seconds.

**Fixes (Chrome-bench fix round).** (1) A region generating while none is on screen (a first
entry on the globe or descending, or a fade held at full cover) takes the 12 ms budget
(`WEB_GEN_LARGE_BUDGET_MS`, `LargeGenBudget`); flying keeps 6 ms. Natively, the time-boxed path
on the Aegean at 1 Ma (`cargo test --release -p timefly --test region_loading -- --ignored
aegean_time_boxed_slices --nocapture`, release, warm tiles): 523 slices at 6 ms (4.3 s of
work), 282 at 12 ms (4.2 s). Scaling Chrome's 628 slices by 282/523 gives about 340 slices,
**about 5.7 s at 60 Hz**, inside 8 s if each 12 ms slice plus the globe's frame fits a 16.7 ms
frame. (2) Loads time out after 20 s of running frames, on a clock that adds each frame's real
delta clamped to 0.25 s. (3) Each region block reports host stalls (frames over 1,000 ms,
"page paused or throttled?") for the load window and the flight; no pass rule reads them.

**Done:** the gated re-run in real Chrome passed every gate (see "Chrome WebGPU, real Chrome (gated)" below).

### Web memory: trace spans (ruling R29) and cluster buffers (ruling R28), 2026-11-23

Measured by the controller in Chrome WebGPU (in a region): the renderer process grew about
45–50 MB/s, to 5.6 GB in 105 s (the owner saw 9 GB), while the wasm memory (263 MiB) and the
JS heap (16 MiB) stayed flat.

**The cause: Bevy's trace spans on wasm (R29).** The default feature `system-spans` enabled
`bevy/trace` in every web build (M1's and the Pages site included). On wasm, bevy_log installs
`tracing_wasm::WASMLayer`, which writes a `performance.mark` and a `performance.measure` for
every span enter and exit: measured at 60 fps, 131,581 marks and 131,581 measures a second (1.12
million of each after about 10 s), each with a long span name, all kept by the browser: about
50 MB/s. A minimal WebGPU control page in the same pane stayed flat at 22 MB, and with the game
running, `performance.clearMarks()` and `clearMeasures()` every 0.5 s flattened the renderer
(1,688 → 1,725 MB over 30 s). Spans are now native-only: `system-spans` enables `bevy_internal`'s
`trace` and `debug` through a native-only dependency, so no wasm build enables `bevy_ecs`
`trace` (`cargo tree -e features --target wasm32-unknown-unknown -p timefly -i bevy_ecs` shows
none; the native tree still has it), the web build line no longer claims `system-spans`, and
`tools/dev/check-wasm-spans.sh` guards it in the Pages workflow.

**Secondary: per-frame cluster buffers (R28).** Hooking `GPUDevice.createBuffer` also showed
4.6 new buffers a frame, 26.7 MB/s of buffer allocations, and no `destroy()` call:

| Buffer | Per frame |
|---|---|
| `StorageBuffer<GpuClusterableObjectIndexListsStorage>` (mapped) | 262,144 B |
| `StorageBuffer<GpuClusterOffsetsAndCountsStorage>` (mapped) | 117,504 B |
| `UninitBufferVec<[UVec4; 2]>` | 117,504 B |
| `UninitBufferVec<ClusterableObjectZSlice>` | 12,288 B |
| "clustering Z slicing metadata buffer" | 48 B |

Bevy 0.20 builds every view's cluster buffers afresh each frame; wgpu 30's WebGPU buffer `Drop`
does not destroy them. This churn was real but not what grew the renderer (clearing the
performance marks alone flattened it). The game has no clusterable objects, so every `Camera3d`
has `ClusterConfig::None` and GPU clustering is off (`GlobalClusterSettings::gpu_clustering =
None`). `ClusterConfig::None` alone would still have allocated the GPU path's 262,144-byte index
list each frame (its size is the clustering capacity, not the cluster count); the CPU path left
running still makes each active view's cluster buffers every frame, at their minimum size.
Native renders are unchanged (region_tour shots of the Himalayas, the Rockies and Pangaea
pixel-identical before and after).

**After the fix (controller, `8d7c427`, in-app pane, visible, 60 fps, in the Alps region at
0 ka):**
- The page's renderer held at 530–561 MB over 62 s; before, it grew 45–55 MB/s.
- The GPU process was steady at about 840 MB, and the JS heap at 12 MB.
- The page had 346 performance marks in all: about 5 a second while loading, then none while flying.

The real-Chrome bench below also shows the wasm memory flat across all 10 memory cycles.

The bench's web memory gate reads only the wasm linear memory, so it could see neither
(plan appendix).

### Chrome WebGPU, real Chrome (gated), 2026-11-23

**Result: PASS on every gate.**

| | |
|---|---|
| Who ran it | the owner, in an isolated Chrome window in front, hands off |
| Browser and machine | Chrome 158 on the M4 (Metal behind WebGPU) |
| Build | `8d7c427` (wasm32, `web-release`, no features: system spans off, R29) |
| Render size | 3200 × 1800 (a 1600 × 900 window at 2×) |
| Session | `44e29db1`; the log was copied from the page's localStorage |

| Stop | First entry | Tiles / frames fetched | Load worst | Flight median / p95 / worst | Missed (> 50 ms) | Result |
|---|---|---|---|---|---|---|
| Pangaea interior (250 Ma) | 1,427 ms | 0 / 2 | **231.67 ms** (R11 ≤ 250: PASS) | 16.95 / 19.59 / 27.33 ms | 0 | PASS |
| Red Sea (0 ka) | 1,474 ms | 2 / 1 | 29.18 ms | 16.57 / 18.25 / 19.83 ms | 0 | PASS |
| Himalayas (0 ka) | 1,191 ms | 1 / 0 | 28.22 ms (reported only) | 17.40 / 19.45 / 22.45 ms | 0 | PASS |
| Sundaland (21 ka) | 1,177 ms | 2 / 2 | 27.74 ms | 16.74 / 18.56 / 21.61 ms | 0 | PASS |
| Japan (1 Ma) | **4,453 ms** | 4 / 2 | 25.43 ms | 16.60 / 18.60 / 23.27 ms | 0 | PASS |
| Beringia (21 ka) | 1,287 ms | 2 / 1 | 28.97 ms | 16.61 / 18.50 / 20.67 ms | 0 | PASS |
| Globe pan (8 s, 480 frames) | — | — | — | 16.62 / 18.32 / 19.43 ms | — | PASS |
| Rockies ice margin (21 ka) | 1,886 ms | 2 / 0 | 29.98 ms | 16.99 / 19.58 / 27.54 ms | 0 | PASS |
| Panama (3 Ma) | 1,192 ms | 0 / 1 | 32.58 ms | 16.57 / 18.39 / 19.99 ms | 0 | PASS |

Summary gates:
- First entries are all within 8,000 ms.
  - The slowest is Japan at 4.45 s, generated with the large budget while descending (fix 1).
  - The in-app pane's Aegean took 15.1 s with 6 ms slices before that fix. The Aegean is no longer benched (R26), and its projection at 12 ms was about 5.7 s.
- Warm generation, over 10 re-entries of the Himalayas: median 1,185 ms, max 1,189 ms (≤ 8,000: PASS).
- Memory cycles: wasm linear memory 294.4 MiB in every cycle, on the globe and in the region (+0.0%: PASS).
- JS heap peak: 34.8 MiB.
- No host stalls, and no stall over 66.7 ms, in any flight, load window or the pan.
- Every flight and the pan were display-paced at about 60 Hz, against the 30 fps target.

The owner watched the whole run and reported that it "looked smooth the entire time".

To repeat it, build and serve from `crates/timefly`:

```sh
cd crates/timefly
PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo run --profile web-release -p timefly \
  --target wasm32-unknown-unknown
```

Open `http://127.0.0.1:1334/?region-bench` in a Chrome window with no other tabs. Keep the window
in front and leave the page alone for about 11 minutes, until the banner reads `Region bench
finished: PASS` (or `FAIL`).

Don't use the Claude desktop app's browser pane for gated numbers (R23): it can pause
`requestAnimationFrame` for tens of seconds while the page still reports itself visible.

**What the console prints:** one `===== Time Fly region bench: …` block per region, then a
`===== Time Fly region bench summary =====` block. The target is 30 fps under the display-paced
rule. Lines to look for:
- **R11:** `first-region worst frame (R11 gate <= 250 ms, …)`, in the first region's block
  (Pangaea) and in the summary. On the web it decides the result.
- **Generation:** target ≤ 8,000 ms, for the warm re-entries and for every region's first entry
  (each block's `first-entry generation (web gate <= 8000 ms)` line and the summary's `first
  entries` line). A region over it fails.
- **Memory cycles:** these read the wasm linear memory. It never shrinks, so the check is that it
  stops growing. The JS heap (`performance.memory`, Chrome only) is reported beside it.

**To copy the whole log**, run this in DevTools → Console before closing the window (a Guest
window deletes its storage when it closes):

```js
copy(Object.keys(localStorage).filter(k => k.startsWith('timefly.log.')).map(k => localStorage.getItem(k)).filter(t => t.includes('region bench summary')).pop())
```

Checked by the implementer with curl against that server (`web-release` build, wasm-bindgen
output 64.97 MB): `/?region-bench`, `assets/data/tiles/manifest.json` (353,054 B),
`assets/data/tiles/overview.tfgrid` (12,430,497 B), `assets/data/tiles/r08c38.tfgrid`,
`r05c39.tfgrid` and `assets/data/globe/manifest.json` all return 200.

### Art pass (native rerun, the dune path, Chrome: R11 waived), 2026-11-23

The art pass (`docs/superpowers/plans/2026-11-23-art-pass.md`, reference renders in
`docs/testing/art-pass/`) adds work to the terrain and water shaders:
- shaded snow and a deep-shade fix;
- the foam fade;
- sand dunes and stony plain;
- swell displacement, light bands and swash;
- rock banding.

The design's gate is that the native and Chrome region benches still pass every M2 gate.

#### Native M4, art pass (`fe37635`), 1920 × 951

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=crates/timefly \
  target/release/timefly --region-bench --target-fps 60 --window 2560x1440
```

- **Build:** the release build of `fe37635` (`release` profile: no LTO, debug info).
- **Resolution:** the main display was the 1920 × 1080 monitor again, so the requested 2560 × 1440
  window was clamped to **1920 × 951** (ruling R21), as in the Task 10 runs.
- **Not like for like.** The `07658ba` comparison run was at 2560 × 1440 on the 4K display: 1.9×
  the pixels here.
- **Pacing:** the display paced `AutoNoVsync` at 60 Hz throughout (medians 16.67–16.69 ms), so
  the display-paced rule decided everywhere.
- **Result:** every gate passed. Exit 0, `result: PASS`.

| Stop | Generation (first entry) | Tiles / frames fetched | Load worst frame | Flight median / p95 / worst | Missed (> 25 ms) | Result |
|---|---|---|---|---|---|---|
| Pangaea interior | 548 ms | 0 / 2 | 23.4 ms (R11 line) | 16.68 / 18.33 / 21.10 ms | 0 of 3,601 | PASS |
| Red Sea | 482 ms | 2 (275,711 B) / 1 | 22.4 ms | 16.67 / 18.36 / **56.44 ms** | 2 (0.06%) | PASS |
| Himalayas | 333 ms | 1 / 0 | 22.8 ms | 16.68 / 18.41 / 20.91 ms | 0 | PASS |
| Sundaland | 332 ms | 2 / 2 | 22.2 ms | 16.69 / 18.38 / 20.48 ms | 0 | PASS |
| Japan | 1,884 ms | 4 / 2 | 24.6 ms | 16.68 / 18.40 / 36.55 ms | 3 (0.08%) | PASS |
| Beringia | 382 ms | 2 / 1 | 23.8 ms | 16.69 / 18.29 / 20.38 ms | 0 | PASS |
| globe pan (8 s) | — | — | — | 16.73 / 18.48 / 20.04 ms (481 frames) | — | PASS |
| Rockies ice margin | 616 ms | 2 / 0 | 22.6 ms | 16.69 / 18.33 / 20.22 ms | 0 | PASS |
| Panama | 332 ms | 0 / 1 | 22.0 ms | 16.69 / 18.32 / 20.70 ms | 0 | PASS |

**The Red Sea stop against `07658ba`:**

| | `07658ba` (M2, 2560 × 1440) | `fe37635` (art pass, 1920 × 951) |
|---|---|---|
| Generation (first entry) | 515 ms | 482 ms |
| Load worst frame | 18.5 ms | 22.4 ms |
| Flight median / p95 / worst | 16.66 / 17.44 / 28.28 ms | 16.67 / 18.36 / 56.44 ms |
| Missed (> 25 ms) | 4 (0.11%) | 2 (0.06%) |
| Stalls (> 33.3 ms) | 0 | 1 |
| Coast run | coast 6.8 km from the centre | the same coast, 6.8 km |

- **Both are display-paced at 60 Hz, with medians on the interval.**
- **The one stall** (56.44 ms) came in the Red Sea's low pass at 300 m. The other segments' worst
  frames were 20.12 ms (climb), 20.50 ms (coast run) and 24.93 ms (apron sweep).
  - It was a single frame: the display-paced rule counts it as one of the 2 missed frames, well
    under 1 %.
  - The run doesn't say what it was. It may be the low pass's first frames with the swell
    displacement on (camera below 1 km). It may also be one of the occasional single hitches
    every M2 run has had, such as Pangaea's 39.45 ms in `07658ba` and Japan's 36.55 ms here.
  - A rerun would tell. The web bench is the one that decides (below).
- **p95 is about 0.9 ms higher on every stop**, not only the Red Sea: Pangaea and the Himalayas
  rise as much as the art-pass-heavy stops. So it is this display's pacing jitter (the
  1920 × 1080 monitor; the Task 10 runs on it had p95 18.2–18.7 ms), not the art pass.

Summary block, verbatim:

```text
===== Time Fly region bench summary =====
build: 0.1.0 (fe37635, release, aarch64-apple-darwin; system-spans)
system spans: on
adapter: Apple M4 (Metal)
os: macOS 27.2.1 (26B131), aarch64
regions (R14 frame rate): Pangaea interior PASS, Red Sea PASS, Himalayas PASS, Sundaland PASS, Japan PASS, Beringia PASS, Rockies ice margin PASS, Panama PASS
first-region worst frame (R11 gate <= 250 ms, request to first path frame, overview decode included): 23.39 ms -> PASS (native: reported only, the gate applies in Chrome WebGPU)
Himalayas load-window worst frame (cold tiles; reported only): 22.82 ms
warm generation (10 re-entries of Himalayas, target <= 3000 ms): median 316 ms, max 333 ms -> PASS
first entries (cold tiles; reported only natively): Pangaea interior 548 ms, Red Sea 482 ms, Himalayas 333 ms, Sundaland 332 ms, Japan 1884 ms, Beringia 382 ms, Rockies ice margin 616 ms, Panama 332 ms
memory cycles (phys_footprint, includes GPU allocations on Apple silicon, on the globe after each leave): 1: 663.7 MiB, 2: 652.7 MiB, 3: 652.7 MiB, 4: 665.4 MiB, 5: 665.5 MiB, 6: 653.0 MiB, 7: 665.6 MiB, 8: 665.8 MiB, 9: 653.3 MiB, 10: 653.6 MiB; worst of cycles 2-10 is +0.3% of cycle 1 (limit +10%) -> PASS
  in the region: 1: 784.1 MiB, 2: 786.3 MiB, 3: 786.4 MiB, 4: 785.8 MiB, 5: 785.9 MiB, 6: 786.7 MiB, 7: 786.0 MiB, 8: 786.2 MiB, 9: 787.0 MiB, 10: 787.3 MiB
result: PASS
===== end region bench summary =====
```

**Every M2 gate:**
- R14 frame rate: all 8 regions and the pan pass.
- R11 first-region worst frame: 23.39 ms, against ≤ 250 ms (native reports it only).
- Warm generation: median 316 ms, max 333 ms, against ≤ 3,000 ms.
- Memory cycles: +0.3 %, against ≤ +10 %.
- First entries are reported only natively. The slowest is Japan at 1,884 ms (1,998 ms in
  `07658ba`).

#### The dune path (ruling R8)

The bench never shows a sand sea: the Red Sea stop's desert is stony plain. So the dune path was
measured separately, over the Sahara erg. `examples/region_tour.rs` has a `time` mode for this.
It works like this:
- the region camera draws to the window, with vsync off as in the benches;
- the camera is held at a fixed height above the centre, with the same pose as
  `tools/dev/region-shots.sh`'s shots (looking north, pitched down 20°);
- after a 6 s settle it records 30 s of frame deltas.

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" cargo run --release -p timefly --example region_tour -- \
  /tmp/unused 2560x1440 23,10,0 sahara time 400
```

- **Scenes:**
  - the Sahara at 23° N, 10° E, 0 ka: the sand sea fills the view at 400 m and at 8 km (see
    `docs/testing/art-pass/after/sahara-0ka-low-shaded.png` and `sahara-0ka-shaded.png`);
  - the reference is the Himalayas bench centre (28° N, 86.9° E, 0 ka): no desert, so no dune or
    stony-plain work. It has snow, rock and forest, with the same water-free view.
- **Setup:** the same M4, release build and window request as the bench (clamped by the 1920 ×
  1080 display).
- **Runs:** each scene and height ran four times, interleaved (Sahara 400 m, Himalayas 400 m,
  Sahara 8 km, Himalayas 8 km, then again).
- **GPU utilisation:** the display paces every frame at 60 Hz, so frame deltas can't show a
  sub-interval GPU cost. The last two passes therefore also sampled the GPU's own utilisation
  counters every 0.25 s (`ioreg -c IOAccelerator`, `PerformanceStatistics` "Device Utilization
  %"), inside each 30 s window. The idle desktop read 0.1 %.

Frame times, 30 s each, about 1,800 frames per run (ms):

| Scene | Mean | Median | p95 | Worst (of 4 runs) |
|---|---|---|---|---|
| Sahara erg, 400 m | 16.67 | 16.64–16.74 | 18.25–18.80 | 21.25 |
| Himalayas, 400 m | 16.67 | 16.69–16.74 | 18.20–18.51 | 22.50 |
| Sahara erg, 8 km | 16.67 | 16.61–16.74 | 17.76–18.53 | 24.43 |
| Himalayas, 8 km | 16.67 | 16.56–16.69 | 17.77–19.59 | 20.44 |

GPU device utilisation (passes 3 and 4), with the equivalent GPU time per 16.67 ms frame:

| Scene | Pass 3 | Pass 4 | ≈ GPU ms per frame |
|---|---|---|---|
| Sahara erg, 400 m | 12.3 % | 12.9 % | 2.05–2.15 |
| Himalayas, 400 m | 12.3 % | 12.1 % | 2.02–2.05 |
| Sahara erg, 8 km | 11.0 % | 12.4 % | 1.83–2.07 |
| Himalayas, 8 km | 11.0 % | 10.9 % | 1.82–1.83 |

- **Reading:** the dune path costs between nothing and about 0.25 ms of GPU time per frame at
  this size.
  - In each pass, Sahara minus Himalayas is 0.0 and +0.8 points at 400 m, and 0.0 and +1.5 points
    at 8 km.
  - Run-to-run spread for one scene is up to 1.4 points (the Sahara at 8 km).
  - So the difference is within the noise.
- **Frames:** paced at 60 Hz with no missed frame (> 25 ms) in any of the 16 runs.
- **Caveats:**
  - Utilisation is a share of busy time at whatever clock the GPU chose. It is an estimate, not
    a timestamp.
  - At 3200 × 1800 in Chrome (the gated web run's render size), the cost scales with about 3×
    the pixels.
  - So the web number for a sand sea is still unmeasured. A Chrome flight over the erg (or an
    erg bench stop, which needs the owner: R8) would settle it.

#### Chrome WebGPU, real Chrome, art pass merged with main (`5434520`), 2026-11-24

**Result: FAIL, on R11 alone** (first-region worst frame 256.08 ms against ≤ 250 ms). Every
other gate passes. **The owner waived R11 for the art pass (2026-11-24)**; the frame cost and
the low-pass stalls below stay open as follow-ups.

| | |
|---|---|
| Who ran it | Claude opened the window, the owner kept it in front, hands off |
| Browser and machine | Chrome 158, an Incognito window, on the M4 (Metal behind WebGPU) |
| Build | the art-pass branch after main was merged in, `5434520` (wasm32, `web-release`, no features, wasm-bindgen output 61.37 MB) |
| Build label | the log says `20c9097`, the commit before the merge: `build.rs` misses commits in a worktree. The `.wasm` holds main's `invalid tile generation digest` string and the art pass's `ramp_level`, so the code is the merge's |
| Render size | 3200 × 1800 (a 1600 × 900 window at 2×), as in the `8d7c427` run |
| Session | `76afd028`; the log was copied from the page's localStorage |
| Host stalls | none (no frame over 1 s in any load window, flight or the pan) |

Against the M2 gated run (`8d7c427`, same size and machine):

| Stop | First entry | Load worst | Flight median / p95 / worst | Missed (> 50 ms) | Result |
|---|---|---|---|---|---|
| Pangaea interior (250 Ma) | 1,491 ms (was 1,427) | **256.08 ms** (was 231.67; R11 ≤ 250: **FAIL**) | **32.14** / 41.88 / **267.94** ms (was 16.95 / 19.59 / 27.33) | 7 (0.38%), 2 stalls over 66.7 ms (was 0) | PASS |
| Red Sea (0 ka) | 1,475 ms (was 1,474) | 28.40 ms | 22.10 / 34.37 / 40.11 ms (was 16.57 / 18.25 / 19.83) | 0 | PASS |
| Himalayas (0 ka) | 1,194 ms | 30.89 ms (reported only) | 25.12 / 30.08 / 35.15 ms (was 17.40 / 19.45 / 22.45) | 0 | PASS |
| Sundaland (21 ka) | 1,177 ms | 33.31 ms | 23.55 / 28.39 / 31.78 ms (was 16.74 / 18.56 / 21.61) | 0 | PASS |
| Japan (1 Ma) | 4,455 ms (was 4,453) | 32.29 ms | 21.15 / 26.38 / 32.38 ms (was 16.60 / 18.60 / 23.27) | 0 | PASS |
| Beringia (21 ka) | 1,284 ms | 31.75 ms | 21.96 / 27.43 / 31.71 ms (was 16.61 / 18.50 / 20.67) | 0 | PASS |
| Globe pan (8 s, 481 frames) | — | — | 16.68 / 18.33 / 20.42 ms (was 16.62 / 18.32 / 19.43) | — | PASS |
| Rockies ice margin (21 ka) | 1,895 ms | 33.64 ms | 24.33 / 29.73 / 33.18 ms (was 16.99 / 19.58 / 27.54) | 0 | PASS |
| Panama (3 Ma) | 1,192 ms | 33.23 ms | 20.32 / 23.92 / 27.32 ms (was 16.57 / 18.39 / 19.99) | 0 | PASS |

Summary block, verbatim:

```text
regions (R14 frame rate): Pangaea interior PASS, Red Sea PASS, Himalayas PASS, Sundaland PASS, Japan PASS, Beringia PASS, Rockies ice margin PASS, Panama PASS
first-region worst frame (R11 gate <= 250 ms, request to first path frame, overview decode included): 256.08 ms -> FAIL (decides the result)
Himalayas load-window worst frame (cold tiles; reported only): 30.89 ms
warm generation (10 re-entries of Himalayas, target <= 8000 ms): median 1185 ms, max 1193 ms -> PASS
first entries (cold tiles, web gate <= 8000 ms each): Pangaea interior 1491 ms PASS, Red Sea 1475 ms PASS, Himalayas 1194 ms PASS, Sundaland 1177 ms PASS, Japan 4455 ms PASS, Beringia 1284 ms PASS, Rockies ice margin 1895 ms PASS, Panama 1192 ms PASS -> PASS
memory cycles (wasm linear memory, which grows but never shrinks, on the globe after each leave): 1: 278.9 MiB, 2: 278.9 MiB, 3: 278.9 MiB, 4: 278.9 MiB, 5: 278.9 MiB, 6: 278.9 MiB, 7: 278.9 MiB, 8: 278.9 MiB, 9: 278.9 MiB, 10: 278.9 MiB; worst of cycles 2-10 is +0.0% of cycle 1 (limit +10%) -> PASS
  in the region: 1: 278.9 MiB, 2: 278.9 MiB, 3: 278.9 MiB, 4: 278.9 MiB, 5: 278.9 MiB, 6: 278.9 MiB, 7: 278.9 MiB, 8: 278.9 MiB, 9: 278.9 MiB, 10: 278.9 MiB
result: FAIL
```

What changed, and what it says:

- **Generation is unchanged** (CPU work): first entries within 64 ms of M2's, warm generation
  1,185 ms median both times. Memory is lower: 278.9 MiB (was 294.4 MiB). JS heap peak 33.9 MiB.
- **Every region flight lost 60 Hz.** At M2 every flight was display-paced at about 60 Hz
  (medians 16.6–17.4 ms). Now the medians are 20.3–32.1 ms, while the globe pan, which draws
  no region, still holds 16.68 ms. So the extra cost is the art pass's region shading at
  3200 × 1800, about 3× the native run's 1920 × 951 pixels, which stayed paced at 60 Hz.
  Every flight still passes the 30 fps target.
- **Pangaea is the expensive stop.** A 250 Ma hot desert is dunes nearly everywhere. Its
  median is 32.14 ms against the 35.00 ms limit. Its climb segment's median is 37.72 ms,
  under 30 fps for that stretch. It passes only because the rule judges the whole flight.
- **R11, 256.08 ms.** It was 231.67 ms. The frame includes the overview decode. The rise is
  about the same as the rise in one Pangaea frame (17 to 32 ms), which fits the decode frame
  also paying one art-pass frame's GPU time. That is a likely reading, not a measured one.
- **Two stalls in Pangaea's low pass** (worst 267.94 ms; M2's worst was 27.33 ms). The log
  does not say what they were. The native run's one stall (56.44 ms, Red Sea) was also in a
  low pass. Something done the first time the camera goes below about 1 km (swell, near-field
  dunes) is the first suspect: a pipeline compiled late would cost this much on WebGPU.

To repeat it, serve from `crates/timefly` (`cargo run --profile web-release -p timefly --target
wasm32-unknown-unknown`), open `http://127.0.0.1:1334/?region-bench` in a 1600 × 900 Chrome
window with no other tabs, keep it in front for about 10 minutes, and copy the log with the
`copy(...)` one-liner under "Chrome WebGPU, real Chrome (gated)" above. Don't use the Claude
desktop app's browser pane for the gated numbers (R23).

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
