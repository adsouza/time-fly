# Rivers and lakes: performance

Task 10 (benches) of the rivers and lakes milestone. Native numbers were measured on 2026-11-26
by the Task 10 implementer; the Chrome (WebGPU) numbers are the owner's run and are pending.

| | |
|---|---|
| Machine | Apple M4, 16 GB, macOS 27.2.1 (26B131) |
| Adapter and backend | Apple M4 (Metal), wgpu 30.0.2 through Bevy 0.20.1 |
| Build | `f846c1a` for every native run, the `release` profile (no LTO). The binaries' labels read a stale SHA (worktree), so `grep -a -c 'gpu time: pass timestamps'` and `grep -a -c 'Indus tributaries'` (both new in this task) finding 1 in each binary is the proof |
| Assets | the `rivers-root` scratch root (the main checkout's `assets/data`, this branch's shaders) |

## Summary

| What | Result |
|---|---|
| `--journey-bench`, the route set, native, display-paced | Run 1 (first launch) **FAIL**, hop memory only: the first hop +71.0 MiB. **Run 2 (judged) PASS**: hop memory worst **+31.4 MiB** (gate +64), every other gate passes |
| The hop from a boulder-free region into a rapids region (`--region=-38.5,-76.0,0`) | PASS twice: +38.4 and +51.2 MiB (the sessions' first hops) |
| `--region-bench`, nine regions, native, display-paced | **PASS** both runs (R11 32.07 ms, warm generation max 434 ms, memory cycles +0.0%) |
| Cloud pass, GPU timestamps, offscreen 2560 × 1440 | **+2.68 ms** at density 0.5, **+2.87 ms** at 1.0 (M3's ioreg estimate: 1.8 ms) |
| The water on the Indus, GPU timestamps | about +0.1 ms over dry land, the rivers under +0.1 ms: within noise |
| The continental pass, native release | 0.93–1.05 s wall in the 0 ka and 21 ka eras, 0.40–0.62 s deep: at or just over the 1,000 ms native target (reported) |
| The pass's longest single step, time-boxed at the web's 6 ms slice (native release, real data) | was 28 ms (the step sizing), 12.8 ms (`rivers→done`, the graph build), 6.8 ms (routing's largest ocean) and 2.6 ms (one basin's resolve); **now 1.1–1.3 ms** at 0 ka and 21 ka after fix round 1, the same result, the same total time |
| Tiles per region (the river read margin) | 2–4× in six of nine bench regions (Beringia 2 → 8) |
| Boulder placement on the swap frame, native | 0.05–0.86 ms for 37–5,897 boulders |
| Everything on the web | **pending: the owner's Chrome run** (steps at the end) |

## Concerns

1. **The first hop of a first launch can exceed +64 MiB** (run 1: +71.0 MiB; Task 7's paced run
   read +60.7). It is a one-time rise in non-malloc ("internal") memory that later hops reuse;
   run 2's first hop read +31.4 MiB. The judged run passes; whether first launches need work
   (trim the boulder pool on a replace, or stage the river grids like the water) is the
   controller's call.
2. **The continental pass's long steps: fixed in fix round 1.** A web slice overruns its 6 ms
   budget by up to one step, and on real data the time-boxed run's longest step was 28 ms. The
   log now names the stage (`longest_step_stage=`), which found four causes; each is now
   metered (see "The continental pass's steps" below). The time-boxed longest step is 1.1–1.3 ms
   natively at 0 ka and 21 ka; Chrome's `longest_step_ms` gives the web figure.
3. **The region bench's first-entry line double-counted the pass wait** in these native runs
   (`generation_ms` subtracted `hydro_wait_ms` from a `millis` that already excludes it): four
   regions read 0 ms and Japan 1,477 instead of 1,911. Natively those lines are reported only, so
   no verdict changed; it is fixed in the commit after the runs, before the web build, and the
   tables above use the session logs' own numbers. On the web the first-entry gate reads it.
4. **Windowed GPU frame times are the passes that still run, not the scene** (1.5–1.8 ms): the
   window behind the Claude app gets no main pass. Offscreen tour numbers are the real ones.
5. **The pass cache keeps two eras**, so the region bench recomputes the 0 ka pass after the deep
   eras (about 0.93 s natively, longer on the web), and its 170 MiB working set stays resident
   for a while after (the memory cycles' first samples, 941 MiB).

## GPU frame time (new in this task)

`crates/timefly/src/diagnostics/gpu_time.rs` (`GpuTimePlugin`, always on in the game) times
every frame on the GPU from pass timestamps, where the device has `Features::TIMESTAMP_QUERY`
(Bevy's default settings request every feature the adapter offers, so that is "where the adapter
has it"; the M4 does, Chrome exposes `timestamp-query`). Bevy's own `RenderDiagnosticsPlugin`
needs `TIMESTAMP_QUERY_INSIDE_ENCODERS`, which Apple-silicon Metal and WebGPU don't offer.

**How.** A one-pixel pass first in the render graph writes the begin timestamp; after the last
pass, one one-pixel pass per output target (the window's swap-chain texture, or a camera's
image) loads that target and writes an end timestamp. A frame's span is
`max(ends) − max(begin, previous frame's end)`, so a frame is timed only when the frame before
it was read: after a frame that found no free slot (or whose reading was dropped), the begin may
have run inside that frame, so that reading is dropped and counted (fix round 1). The queries
are resolved once the queue reports the frame's work done, and read back through a ring of eight
buffers, so nothing waits on the GPU and a reading arrives a few frames after its frame.
Readings with a zero, reversed or non-finite pair are dropped and counted too.

**Why it is built that way: measured on the M4.** Apple-silicon GPUs run render passes that
don't depend on each other at the same time, so naive marker passes don't bracket a frame:

| Design (frames of N busy full-screen passes, 1.65 ms each) | N = 1 | N = 4 | N = 16 |
|---|---|---|---|
| End pass on its own 1 × 1 target | 0.04 ms | 1.66 ms | 1.67 ms |
| End pass in its own command buffer, same submit | 0.04 ms | 1.66 ms | 1.66 ms |
| **End pass loading the frame's target** (shipped) | **1.67 ms** | **6.56 ms** | **26.3 ms** |
| Frames back to back, span from the begin pass alone | — | 12.9 ms (double) | 50.6 ms (double) |
| **Back to back, span from max(begin, previous end)** (shipped) | — | **6.53 ms** | — |

Two more Metal findings: a resolve in the frame's own command buffer read 0 or the previous
frame's end (it races the last pass's end-of-fragment sample), so resolves wait for the queue's
work-done callback; and a clear-only pass left its end-of-pass sample unwritten, so the marker
passes draw one pixel.

**The test.** `tests/region_gpu.rs`, `the_gpu_frame_time_is_measured_on_this_gpu` (ignored;
`cargo test -p timefly --test region_gpu -- --ignored`): frames of 0, 1, 4 and 16 busy passes
read 0.05, 1.54, 6.54 and 26.15 ms (medians). Since fix round 1 it also submits 20 frames of
four passes back to back, all at once, so 12–13 of 26 frames find no free slot: the 9–10
readings read a median of 6.52–6.71 ms and a worst within 7% of it (three runs). Mutating either
rule (the end pass on its own target; the span from the begin alone) fails it, and so does
timing a frame whose predecessor wasn't read (`read_frame`'s unit test pins that rule; on the M4
the readings after an untimed frame didn't come out doubled in this test, so the back-to-back
check is a guard). Waited frames can read slower than packed ones (8.5 vs 6.9 ms in one run):
the GPU idles between waited frames and clocks down.

**Caveats.**
- GPU time is busy time at whatever clock the GPU chose. A display-paced run idles the GPU
  between frames and it may clock down, so the same scene reads more GPU milliseconds paced than
  GPU-bound. Compare scenes within one pacing, interleaved, as below.
- A reading arrives 2–4 frames after its frame; the benches assign it to the segment or route
  being flown when it arrives.
- **Chrome** rounds `timestamp-query` to 100 µs unless the "WebGPU Developer Features" flag
  (`chrome://flags/#enable-webgpu-developer-features`) is on. Each Chrome run below says which.
  The overlap rules above were found on Metal directly; Dawn (Chrome's WebGPU on Metal) may
  schedule passes differently, so compare Chrome's GPU medians against its frame times (a GPU
  time far under a GPU-bound frame time means the markers didn't bracket the frame there).

## Cloud pass cost: measured (replaces M3's ioreg estimate)

**Result: about 2.7 ms of GPU time a frame at density 0.5 and 2.9 ms at 1.0, at 2560 × 1440.**
M3 estimated 1.8 ms from `ioreg` utilisation and noted that utilisation near 93% busy may
understate the time; the timestamps say it did, by about 0.9 ms. The pass runs only during
transitions (the gather, the hold, the clear), and the owner accepted about 2 ms in M3.

**Method.** As M3: `examples/region_tour.rs` in `time` mode with `--offscreen` (the region camera
draws into a 2560 × 1440 image), the Alps (46.5° N, 10° E, present), 8 km above the centre,
6 s settle, then 30 s timed. Three scenes interleaved over three passes, each a fresh
`TIMEFLY_LOG_DIR`, after a warm-up run of the new binary (Metal's shader cache):

```sh
T=<scratch>/region_tour; R=<root with assets/>
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 46.5,10,0 cloud-off time 8000
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 46.5,10,0 cloud-05 cloudtime 8000
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 46.5,10,0 cloud-10 cloudtime1 8000
```

**Pacing.** Every run was display-paced: frame-time medians 16.62–16.76 ms (60 Hz), vsync off
requested. So the GPU idled between frames; the GPU times are at the clocks it chose for that.

| GPU frame time, median (p95) ms | Pass 1 | Pass 2 | Pass 3 | Mean of medians | Cost |
|---|---|---|---|---|---|
| Cloud off | 12.78 (13.35) | 12.67 (13.30) | 12.84 (13.28) | 12.76 | — |
| Density 0.5 | 15.36 (15.61) | 15.38 (15.58) | 15.59 (16.01) | 15.44 | **+2.68 ms** |
| Density 1.0 | 15.45 (15.74) | 15.60 (15.89) | 15.85 (16.22) | 15.63 | **+2.87 ms** |

Each run read 1,798–1,803 frames, none dropped.

## The water's GPU cost on the Indus: measured (native)

**Result: the water costs about 0.1–0.2 ms of GPU time a frame at 2560 × 1440 over the Indus
tributaries, and its rivers no measurable extra (under 0.1 ms, below the run-to-run spread).**
The water's mesh is drawn everywhere; over dry ground each fragment reads `blended_water` once
and discards (Task 8's fix round), and a river's pixels are a thin share of the view.

**Method.** `region_tour` `time --offscreen`, 2560 × 1440, the Indus tributaries bench region
(32.5° N, 74.5° E, present), 2 km above the centre, looking north along a river that runs to
the horizon (the shaded shot `indus-tributaries-rivers.png` in this folder: the same pose at 1280 × 720). Three
scenes, interleaved with the cloud runs above over three passes:

- rivers on (the game as it ships);
- `--no-rivers`: the continental pass off, so no rivers or lakes; the water still draws and
  discards over dry land (new tour flag);
- `--no-water`: the water hidden (new tour flag).

| GPU frame time, median (mean) ms | Pass 1 | Pass 2 | Pass 3 | Mean of means |
|---|---|---|---|---|
| Rivers on | 12.53 (12.62) | 13.04 (12.93) | 12.66 (12.70) | 12.75 |
| No rivers (water over dry land only) | 12.86 (12.86) | 12.46 (12.60) | 12.66 (12.68) | 12.71 |
| No water | 12.62 (12.67) | 12.56 (12.62) | 12.47 (12.58) | 12.62 |

Pacing: display-paced, frame-time medians 16.66–16.71 ms. The spread within a scene across
passes (up to 0.5 ms in medians, 0.3 ms in means) is larger than either difference, so: water
over dry land ≈ +0.1 ms, the rivers ≈ +0.04 ms; both are within noise. A pose with more river
on screen (a big braided reach) would cost more; Chrome's numbers are the owner's run.

**Distant thin rivers stay visible** (Task 8's minor): in the shaded shot the river is a
continuous line to the horizon, about 1 px wide in the distance, with no gaps.

## Tiles, the continental pass and boulders per bench region (native)

`region_tour … shaded 3000` at each bench region's centre, 640 × 360, rivers on and with
`--no-rivers`, one run each with a fresh `TIMEFLY_LOG_DIR`; the numbers are the session log's
`RegionGenerated` event, `continental pass done` line and `region boulders placed` line. Every
pass ran once per session (a cold cache), so each region waited for it.

| Region | Tiles fetched, rivers on / off | Generation (ms, excl. the pass wait), on / off | Pass wall / CPU (ms) | Pass's longest step | Boulders (placement) |
|---|---|---|---|---|---|
| Pangaea interior (250 Ma) | 0 / 0 | 399 / 384 | 582 / 570 | 6.3 ms | 0 |
| Red Sea (0 ka) | **6 / 2** | 517 / 485 | 983 / 949 | **30.7 ms** | 0 |
| Himalayas (0 ka) | **4 / 1** | 468 / 366 | 966 / 944 | **30.6 ms** | **5,897 (0.79 ms)** |
| Indus tributaries (0 ka) | 2 / 2 | 482 / 416 | 968 / 944 | **30.9 ms** | 1,089 (0.27 ms) |
| Sundaland (21 ka) | **4 / 2** | 450 / 351 | 1,033 / 1,010 | **28.3 ms** | 0 |
| Japan (1 Ma) | **9 / 4** | 2,016 / 1,968 | 416 / 408 | 8.7 ms | 37 (0.11 ms) |
| Beringia (21 ka) | **8 / 2** | 484 / 400 | 1,049 / 1,015 | **30.0 ms** | 0 |
| Rockies ice margin (21 ka) | **6 / 2** | 684 / 634 | 1,033 / 1,004 | **27.9 ms** | 432 (0.16 ms) |
| Panama (3 Ma) | 0 / 0 | 416 / 368 | 433 / 416 | 8.1 ms | 65 (0.05 ms) |

- **Tile-count growth (Task 5's river read margin, 116 km instead of 5 km):** two to four times
  the tiles in six of the nine regions (Beringia 2 → 8, Japan 4 → 9). Pangaea and Panama read
  no tiles either way (era frames, the overview), and the Indus tributaries' footprint already
  spans its two tiles. Each tile is about 120 KiB, so a first entry fetches 0.2–0.6 MiB more;
  the region bench's first entries on the web carry it (pending).
- **The continental pass, native release:** 950–1,050 ms wall in the 0 ka and 21 ka eras (0.89
  → 1.40 s in the dev profile after Task 8b), 420–580 ms in the deep eras: **at or just over the
  1,000 ms native target** (reported, not gated). Its own session: the 0 ka pass at the Indus
  read 950 ms wall, 927 ms CPU, 200 ms of sampling, 4,860,000 nodes, 121,257 river nodes, 861
  lakes, 170 MiB working set, 5.7 MiB result.
- **The pass's longest step in these tour sessions: 28–31 ms at 0 ka and 21 ka** (6–9 ms in the
  deep eras). This is the native task's step, `NATIVE_STEP_UNITS` (2¹⁸) units at a time on a
  background thread, which never holds a frame. The web runs the time-boxed path, whose steps
  were as long (28 ms) and are now 1.1–1.3 ms: see "The continental pass's steps".

### The continental pass's steps (fix round 1)

A web slice (6 ms, `WEB_HYDRO_BUDGET_MS`) runs steps until its budget is spent, so it overruns
by up to one step. The time-boxed path, measured natively on real data
(`TIMEFLY_REAL_ASSET_ROOT=<root>/assets cargo test -p timefly --release --test region_loading
real_continental_pass_timing -- --ignored --nocapture`), with the stage now named in
`PassOutput::longest_step_stage` and the `continental pass done` line (`longest_step_stage=`):

| Fix, in order | Longest step at 0 ka (6 ms slices) | Its stage |
|---|---|---|
| Before (Task 10 as committed) | 27.9 ms | `runoff→walk` |
| Steps at most double and are capped (2¹⁴ units): a unit costs 3–110 ns inside one stage, so steps sized on cheap units overran when they turned costly | 12.8 ms | `rivers→done` |
| The river graph and the ocean bits built in steps (`GraphBuild`, a `build` stage) | 6.8 ms | `route` |
| Routing's largest ocean component marked in blocks (it was one unit) | 2.5 ms | `walk` |
| A basin over 1,024 nodes gathers its inputs over several walk steps, 4 units a node (a 13,459-node basin took 2.6 ms) | 1.9 ms | `sample` |
| Steps aim at an eighth of the slice, with a 2¹³ cap | **1.10–1.21 ms** (21 ka: 1.09–1.30 ms) | `runoff→walk`, `sample`, `sample→route` |

The result is unchanged: the test asserts the time-boxed hydrology equals `run_parallel`'s at
0 ka and 21 ka, the core's any-step-size test runs every step size from 1 up, and the golden
fixtures are untouched. Total time-boxed CPU is unchanged too (1.22 s at 0 ka, 1.36 s at 21 ka;
194 and 217 slices of 6 ms; longest slice 6.8–7.0 ms). The remaining long steps hold a stage
change (routing's finish, the lake-level placement, new full-length buffers: about 0.5–0.7 ms).
On the web, about 2–3 times slower, that is 3–4 ms at most, inside a 6 ms slice. `Today::place`
(the present-day overlay) was never the long step: the walk's `today` steps stay under the cap.
- **Boulder placement on the swap frame:** 0.05–0.79 ms natively for 37–5,897 boulders (about
  0.13 ms per thousand), so about 2 ms at the 16,384 cap natively.
- **The region CPU footprint** (Task 6's +15 MiB per region) is read from the region bench's
  memory lines, pending its paced run (below).

## Native hop memory, display-paced (the journey bench)

**Result: PASS on run 2 (judged): worst hop +31.4 MiB against the +64 MiB gate.** Run 1 (the
binary's first launch) failed on its first hop alone, +71.0 MiB; every other hop of both runs
read +20 to +31 MiB. The hop from a boulder-free region into a rapids region passed both times
(+38.4 and +51.2 MiB on its two runs, under the gate).

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace \
  BEVY_ASSET_ROOT=<root> <scratch>/timefly --journey-bench --target-fps 60 --window 2560x1440
# The boulder-free → rapids hop: the Pacific off Chile (no boulders), hopping east into the
# Chilean lakes region (840–875 boulders); the pool starts empty in a fresh session.
TIMEFLY_LOG_DIR="$(mktemp -d)" TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace \
  BEVY_ASSET_ROOT=<root> <scratch>/timefly --journey-bench --region=-38.5,-76.0,0 --target-fps 60 --window 2560x1440
```

**Pacing.** Every run was display-paced: flight medians 16.68–16.69 ms on the 60 Hz display.
The window opened behind the Claude app on the 1920 × 1080 display, so the 2560 × 1440 request
rendered **1920 × 951** (as M3's Task 11 runs). See the GPU caveat below.

| Run | Pacing (flight median) | Hop memory, worst | Hops (MiB over the in-region baseline) | Result |
|---|---|---|---|---|
| Route set, run 1 (first launch) | display-paced, 16.68 ms | **+71.0 MiB (FAIL)** | Andes 1 **+71.0**, Andes 2 +23.1, Pangaea +22.0 / +22.0, Grand Canyon +24.9 / +26.1, Fuji +29.5 / +27.5, Andes +22.3 / +22.3 | FAIL (exit 3), hop memory only |
| **Route set, run 2 (judged)** | display-paced, 16.69 ms | **+31.4 MiB** | Andes +31.4 / +29.8, Pangaea +26.4 / +20.1, Grand Canyon +30.5 / +30.1, Fuji +29.3 / +26.8, Andes +22.3 / +22.3 | **PASS** (exit 0) |
| Chile, run 1 | display-paced, 16.69 ms | +45.7 MiB | cycle 1 **+38.4** (0 → 840 boulders) / +45.7; cycle 2 +26.4 / +22.4; cycle 3 +22.3 / +20.5 | PASS (exit 0) |
| Chile, run 2 | display-paced, 16.68 ms | +51.2 MiB | cycle 1 **+51.2** (0 → rapids) / +22.3; cycle 2 +22.5 / +23.0; cycle 3 +21.5 / +21.7 | PASS (exit 0) |

Every other gate passed in every run: holds (dives 428–729 ms, hops 68–72 ms, climbs 33–34 ms
against 3,000 ms), flight per route (display-paced rule), swap worst frame 31.35 ms in run 2
(33.68 ms in run 1; gate 100 ms), route checks (Fuji's coast 65–66% land, the cube corner 97–101
km), journey memory cycles (Andes +4.5% in run 1, −1.2% in run 2). Sites: the canyon 1,102 m
deep, Fuji 3,025 m.

**Run 1's first hop.** From the hop-memory trace (`timefly::hop_memory=trace`): the footprint
went from 805.7 MiB before the hop to 876.7 MiB at the swap; "internal" (non-malloc) memory
rose 58.7 MiB and graphics 22.3 MiB, while malloc's in-use bytes stayed flat (258 → 256 MiB).
The rise stayed: hop 2's baseline was 854.6 MiB, and every later hop peaked at the same
~877–878 MiB. So run 1's first hop pays a one-time high-water rise of about 50 MiB that later
hops reuse, about 20 MiB more than M3's first hop (+37.5 MiB worst, Task 11). Run 2's first hop
read +31.4 MiB. This is the same first-hop pattern M3 recorded, now closer to the gate: with
Task 7's +60.7 MiB and this run 1's +71.0 MiB, the first hop of a first launch can exceed +64 MiB.
The judged run passes; the controller decides whether the first-launch case needs work (see
"Concerns").

**The boulder pool.** Task 9's pool only grows. In the Chile runs the session's first boulders
arrive with a hop (0 → 840): +38.4 and +51.2 MiB on the two runs. These are the sessions' first
hops, which carry the one-time rise above; one chunk of growth (about 0.9 MiB) is not separable
from it. A hop into a Himalayas-size field (5,897 boulders, six
chunks; about +5 MiB estimated) wasn't flown: no boulder-free region sits one eastward hop from
one (the region west of the Himalayas has 4,506).

**GPU frame times from these windowed runs are not the scene's cost.** The windowed benches read
1.45–1.82 ms of GPU time a frame in every region and route, against 12.5–13 ms offscreen at
2560 × 1440 for the same kind of terrain. M3 found that a window the compositor isn't showing
(here, behind the Claude app) gets no main pass, only the shadow and compute passes; these
numbers fit that. So the windowed runs' GPU lines measure the passes that still run, and the
memory figures may sit lower than with the main pass drawn (its targets). The owner's visible
runs (Chrome below, or a native run with the window in front) give the real GPU frame times.

## Region bench, display-paced (native)

**Result: PASS on both runs.** Nine regions now (the Indus tributaries after the Himalayas).

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace \
  BEVY_ASSET_ROOT=<root> <scratch>/timefly --region-bench --target-fps 60 --window 2560x1440
```

Pacing: every flight display-paced, medians 16.64–16.73 ms; rendered 1920 × 951 (the window
behind the Claude app, on the 1080p display).

| | Run 1 | Run 2 (judged) |
|---|---|---|
| R14 flight, per region | all PASS (Pangaea and the Red Sea reported only, PASS) | all PASS (likewise) |
| R11 first-region worst frame (native: reported) | 33.60 ms | 32.07 ms |
| Himalayas load-window worst frame | 33.08 ms | 34.48 ms |
| Warm generation (10 re-entries of the Himalayas; ≤ 3,000 ms) | median 432 ms, max 433 ms | median 432 ms, max 434 ms |
| Memory cycles (≤ +10%) | +0.1% | +0.0% |
| Exit | 0 | 0 |

**First entries, run 2** (the region's own generation, from the session log's `RegionGenerated`;
reported natively, gated at 8,000 ms on the web). The summary's own `first entries` line in
these runs is wrong for the four regions that waited for their era's pass: it subtracted the wait
a second time (`generation_ms`; Task 4's `millis` already excludes it), reading 0 ms for them and
1,477 ms for Japan. Fixed after the runs (see "Concerns"); the true numbers:

| Region | Own generation | Pass wait | Tiles | Flight median / p95 / worst (ms) |
|---|---|---|---|---|
| Pangaea interior | 368 ms | 634 ms | 0 | 16.71 / 18.33 / 83.73 (reported only) |
| Red Sea | 467 ms | 950 ms | 6 | 16.65 / 18.21 / 32.53 (reported only) |
| Himalayas | 450 ms | 0 | 4 | 16.64 / 18.05 / 19.12 |
| Indus tributaries | 466 ms | 0 | 2 | 16.65 / 18.14 / 19.65 |
| Sundaland | 416 ms | 1,016 ms | 4 | 16.67 / 18.35 / 19.87 |
| Japan | 1,911 ms | 434 ms | 9 | 16.71 / 18.37 / 21.56 |
| Beringia | 466 ms | 0 | 8 | 16.70 / 18.38 / 52.71 |
| Rockies ice margin | 649 ms | 0 | 6 | 16.71 / 18.29 / 20.40 |
| Panama | 368 ms | 465 ms | 0 | 16.71 / 18.36 / 26.03 |

Against M3 (Task 11, the same 1920 × 951, display-paced; M3's first entries had no pass wait to
leave out): Himalayas 450 vs 332 ms, Sundaland 416 vs 332, Beringia 466 vs 382, Rockies 649 vs
616, Panama 368 vs 332, Japan 1,911 vs 1,865; Pangaea 368 vs 549 and the Red Sea 467 vs 482 are
lower. Warm generation 432 vs 316 ms. The rise is the rivers in the generation (Task 6 measured
+52 ms of prep) and the extra tiles. Every flight p95 is 18.05–18.38 ms, as M3's 17.93–18.25 ms.

**The continental passes in the region bench** (wall / CPU ms, native release): 250 Ma 618 / 599,
0 ka 933 / 905, 21 ka 999 / 970, 1 Ma 415 / 402, 3 Ma 432 / 430, 5 Ma 466 / 435 (a prefetch on
the globe), and 0 ka again 932 / 905: the cache keeps two eras (`HYDRO_CACHE_ERAS`), so the
Himalayas' era was evicted by the deep eras and recomputed before the memory cycles.

**Memory.** The memory cycles start at about 941 MiB on the globe (M3: 641 MiB) and fall to
about 776 MiB from cycle 5 or 6 on. The trace shows why: right after the second 0 ka pass,
"internal" memory rose by about 190 MiB (323 → 510 MiB), the pass's 170 MiB working set
(`working_bytes`), freed but still resident, and macOS returned it during the cycles; graphics
memory stayed at 428–429 MiB throughout. So it is transient, and the cycles gate passes (it
compares the later cycles with the first). The in-region minus on-globe difference is about
111 MiB in every cycle (M3: 133 MiB); Task 6's +15 MiB a region isn't separable here, given the
undrawn main pass (above) and the pass's resident working set. The hop peaks (+20–31 MiB after
the first hop) bound what a region adds during a swap.

## Chrome WebGPU: pending (the owner's run)

**Not yet run.** Every web number in this section is pending the owner's Chrome run.

**The build.** `web-release` (wasm32, no features: system spans off, R29) of this task's last
code: the commit that adds this document ("fix: the region bench's generation figure no longer
subtracts the pass wait twice", after `f846c1a`), built 2026-11-26 as
`timefly-task10.wasm` in the SDD scratch folder (`<scratchpad>/task10/bin/`):
- 72,052,207 bytes, sha256 `523540a22d0dd465ab0878860bdb052c542d39c148b8c1f27f4b876e770fac58`;
- `grep -a -c 'gpu time: pass timestamps'` and `grep -a -c 'Indus tributaries'` each find 1
  (both new in this task), so the build is this branch's.

To rebuild it: `cargo build --profile web-release -p timefly --target wasm32-unknown-unknown`
(the wasm lands in `<target dir>/wasm32-unknown-unknown/web-release/timefly.wasm`).

**Before the run.** Decide whether to turn on "WebGPU Developer Features"
(`chrome://flags/#enable-webgpu-developer-features`, then relaunch Chrome). With it on, GPU
timestamps are exact; with it off they are rounded to 100 µs, which is fine for medians of
several milliseconds but not for sub-millisecond differences. Record which it was.

**To serve it** (from a folder whose `assets/` holds `data` from the main checkout and this
branch's `shaders` and `credits.txt`, such as the `rivers-root` scratch root;
`wasm-server-runner` serves `./assets`):

```sh
cd <root with assets/{data,shaders,credits.txt}>
wasm-server-runner <the wasm file above>
```

**The runs.** Each in an Incognito window, in front, hands off, no other tabs (not the Claude
app's browser pane, which throttles `requestAnimationFrame`, R23):

1. `http://127.0.0.1:1334/?region-bench` (about 15 minutes; nine regions now). Gates as M3: R14
   at 30 fps, R11 ≤ 250 ms, generation ≤ 8,000 ms (warm and every first entry), linear memory
   flat across cycles.
2. `http://127.0.0.1:1334/?journey-bench` (about 5 minutes). Holds reported against 8 s, flight
   gated, swaps reported, M3's per-route linear-memory rule.
3. The water and the cloud's GPU cost, by the timestamps: `?region=32.5,74.5,0` (the Indus
   tributaries), fly low (about 2 km) along a river for a minute, then the same over dry ground
   away from the rivers. This is a manual check: the web has no `region_tour`. Look at the GPU
   frame times the region bench prints for "Indus tributaries" as the main number.
4. Watch the Indus tributaries stop of the region bench: distant thin rivers should stay
   visible toward the horizon (Task 8's minor), as they do natively (below).

Copy each log from DevTools with M2's one-liner (`region-m2/performance.md`, "Chrome WebGPU,
real Chrome (gated)"):

```js
copy(Object.keys(localStorage).filter(k => k.startsWith('timefly.log.')).map(k => localStorage.getItem(k)).filter(t => t.includes('region bench summary')).pop())
```

with `'journey bench summary'` for the journey bench. The full log (not only the summary) has the
lines below; search the copied log for them.

**What to record from the logs** (each against its rule; anything over its budget gets
time-sliced, as the plan says):

| What | Log line to find | Native reference | Web rule |
|---|---|---|---|
| Region bench, journey bench: every gate | the summaries | below | M3's gates, unchanged |
| GPU frame time per region and route | `gpu frame time (<name>): median … / p95 … / worst … ms over N frames (timestamp-query)`, and the summary's `gpu time:` line (readings dropped) | below | reported only |
| The continental pass's wall time per era | `continental pass (<age>): … ms wall, … ms CPU, … slices on web (target <= 5000 ms, reported)` | 950–1,050 ms wall (0 ka and 21 ka, release); 0.89 → 1.40 s (dev, Task 8b) | ≤ 5,000 ms, reported |
| The largest present-day overlay unit (`Today::place`) and every other pass step | `continental pass done … longest_step_ms=… longest_step_stage=… longest_slice_ms=…` (the stage names which part of the pass the step was in) | time-boxed at 6 ms: 1.1–1.3 ms at 0 ka and 21 ka (`runoff→walk`, `sample`), release, fix round 1 | each step within `WEB_HYDRO_BUDGET_MS` (6 ms): a step longer than that overruns its slice |
| The apron prep step (Task 6: 2.7 ms native) | `region generated in time-boxed slices … longest_ms=…` (one per region) | — | within the 6 ms slice |
| Boulder placement on the swap frame | `region boulders placed boulders=N ms="…"` (on each region change; the Indus tributaries and the journey routes' rapids) | 0.27 ms for 1,089 (Indus tributaries), 0.79 ms for 5,897 (Himalayas), release | inside the swap frame's budget; M3's swap worst frame is reported on the web (≤ 100 ms natively). Up to 16,384 boulders: about 4 ms natively |
| The water's GPU cost over rivers and over dry land | the GPU frame times of step 3 and of the Indus tributaries stop | below | reported |
| Distant thin rivers | step 4, by eye | visible natively | reported |
| Wasm linear memory across the region bench's nine regions | the region blocks' `memory (wasm linear memory …)` lines | — | M3's rule (flat across cycles) |
