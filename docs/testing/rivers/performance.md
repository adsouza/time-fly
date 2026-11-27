# Rivers and lakes: performance

Task 10 (benches) and Task 12 (verification) of the rivers and lakes milestone. **Task 12's
final numbers at HEAD come first**; Task 10's and Task 11c's records follow unchanged. The
Chrome (WebGPU) numbers are the controller's run in the owner's Chrome and are pending.

## Task 12: final verification at HEAD (2026-11-26/27)

Every native number in this section is HEAD `9814212` (Task 11c's fix round, the last code
commit), the `release` profile, copied out of the shared target dir. The binary's label reads
`9814212`, and `grep -a -c 'fn white_water_foam'` (Task 11c's fix round; the shaders are
embedded) finds 1 in it. The sections after this one are Task 10's and Task 11c's records, kept
as they were.

| | |
|---|---|
| Machine, adapter | Apple M4, 16 GB, macOS 27.2.1 (26B131); Apple M4 (Metal), wgpu 30.0.2 through Bevy 0.20.1 |
| Display | **changed during the runs.** Until about 03:50 UTC the main display was a 3840 × 2160 monitor at 1× (the 2560 × 1440 window rendered in full); from then on a 1920 × 1080 display (the window rendered **1920 × 951**, as in M3 and Task 10). Each bench line below says which |
| Pacing | mixed: a window the compositor showed ran **uncapped** on the 4K monitor (`AutoNoVsync` honoured), and display-paced while hidden. The benches judge each run by its own pacing (R14's two rules); like-for-like comparisons are noted where pacing differs |
| Other load | another session built and ran tests (`ci-test` profile) in the shared target dir from about 03:2x to 03:38 UTC, during region bench run 1 (logged from 03:35:47; the disk fell 2.4 GB from 03:22 to 03:33). Run 1's late regions are not clean measurements |
| Runs | `--keep-going` everywhere, so every gate reports. Each run a fresh `TIMEFLY_LOG_DIR`, `TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace`, `BEVY_ASSET_ROOT` the `rivers-root` scratch root. Region bench run 1 and 2 launched directly; the journey and scrub benches through a scratch `.app` wrapper and `open -n -W` so the window came to the front (raising a bare binary with System Events failed: no Accessibility grant). Run 2 of each bench is the one judged |

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace \
  BEVY_ASSET_ROOT=<root> <scratch>/timefly --region-bench --keep-going --target-fps 60 --window 2560x1440
# the same with --journey-bench and --scrub-bench
```

### Summary

| Gate | Result |
|---|---|
| `--region-bench` run 2 (judged) | **PASS** (exit 0). Every region's R14 passes (uncapped p95 rule for seven, Pangaea and the Red Sea reported only); R11 41.45 ms; warm generation median 531, max 540 ms (≤ 3,000); memory cycles +0.7% (≤ +10%) |
| `--region-bench` run 1 (first launch) | FAIL (exit 3): R14 in Sundaland, Beringia, the Rockies (and its globe pan) and Panama, during the other session's build and a display change mid-run; everything else passes |
| `--journey-bench` run 2 (judged) | **FAIL (exit 3) on hop memory alone: the first hop +78.1 MiB** against +64; every other hop +22.0 to +45.4 MiB; every other gate passes (holds, flight, swap worst frame 33.75 ms, route checks, journey memory +3.5%) |
| `--journey-bench` run 1 | FAIL (exit 3) on hop memory alone: the first hop +72.0 MiB; the others +44.4 to +54.8 MiB (uncapped, which reads higher, as Task 7 found) |
| Hop-memory escalation (plan Step 3) | (1) `APRON_RIVER_MIN_DISCHARGE_M3S` 200 → 1,000, two runs: **still FAIL** (worst +71.0 and +64.5 MiB, again on hops whose baseline fell into a trough). (2) Dropping the CPU river array after upload: already in place since Task 7 (`free_rivers`). **The gate is the owner's call** (below) |
| `--scrub-bench` (one run) | **PASS** (display-paced; older 16.70 / 18.47 / 32.12 ms, 0.08% missed; younger 16.68 / 18.41 / 22.52 ms) |
| The continental pass, native (target ≤ 1,000 ms, reported) | 0 ka 1,033–1,449 ms wall; 21 ka 1,099–1,231 ms; deep eras 433–755 ms: **over the native target at 0 ka and 21 ka** (Task 10: 930–1,050 ms), under it deep |
| White water's GPU cost (budget +1.0 ms) | **−0.01 ms** (bench pose) and **+0.04 ms** (rapid close-up) against `12ef525`, GPU timestamps, display-paced, window shown: within noise |
| The reality check (`--hydro-check`) | outlets **22/22**, lakes **8/8**, Amazon box PASS (91.8%), ridged joins 0/4,714, arid PASS; **Indus box FAIL on "every link" (74/84)**, as since Task 11 (the owner's) |
| Chrome WebGPU | **PENDING: the controller's run** (the build is ready, below) |

### Region bench

| | Run 1 (first launch) | Run 2 (judged) | Task 10 run 2 | M3 (Task 11 run 2) |
|---|---|---|---|---|
| Pacing, resolution | display-paced 2560 × 1440 for six regions, then uncapped; the last three at 1920 × 951 | uncapped from the Red Sea on (Pangaea paced), 2560 × 1440, the last three 1920 × 951 | display-paced, 1920 × 951 | display-paced, 1920 × 951 |
| R14 per region | FAIL in Sundaland (98 stalls), Beringia (36), the Rockies (uncapped p95 17.24 ms; its globe pan 49 ms worst), Panama (uncapped p95 16.80 ms); the rest PASS | all PASS | all PASS | all PASS |
| R11 first-region worst frame (native: reported) | 32.52 ms | 41.45 ms | 32.07 ms | 42.60 ms |
| Himalayas load-window worst frame | 34.72 ms | 33.10 ms | 34.48 ms | 23.90 ms |
| Warm generation (10 Himalayas re-entries) | median 466, max 514 ms | median 531, max 540 ms | median 432, max 434 ms | median 316, max 332 ms |
| Memory cycles (≤ +10%) | −9.5% | +0.7% | +0.0% | — |
| First entries (the region's own generation; pass wait excluded) | Pangaea 366, Red Sea 468, Himalayas 464, Indus 466, Sundaland 433, Japan 2,067, Beringia 532, Rockies 714, Panama 417 ms | Pangaea 402, Red Sea 506, Himalayas 508, Indus 488, Sundaland 468, Japan 2,098, Beringia 502, Rockies 855, Panama 433 ms | 368, 467, 450, 466, 416, 1,911, 466, 649, 368 ms | 549, 482, 332, —, 332, 1,865, 382, 616, 332 ms |

**Against M3 and Task 10.** Flight gates hold in the judged run. Region generation (pass wait
excluded) is up on Task 10 by 5–30% in run 2 (Himalayas 508 vs 450 ms, warm 531 vs 432 ms) and
on M3 by about 50% in the recent eras (Himalayas 508 vs 332). Task 10 explained M3 → Task 10 (the
rivers' prep and the wider tile margin). Task 10 → now adds Task 11e's tributary corridor search
(its own measure: +5–7 ms a region) and the wider reach (tile margin 120 → 133 km), the bigger
rocks of 11c, and the merged M3 work; and run 2 here ran uncapped, so the render thread competed
with generation for the CPU, where Task 10's ran paced. The 3,000 ms warm-generation gate keeps a
wide margin. Run 1's first entries (paced for most regions) sit within 3–10% of Task 10's.

**The continental passes** (wall / CPU ms, native release; the cache keeps two eras, so the last
0 ka pass is the Himalayas' recomputed after the deep eras):

| Era | Run 1 | Run 2 | Journey run 1 | Journey run 2 | Task 10 |
|---|---|---|---|---|---|
| 250 Ma | 616 / 597 | 658 / 649 | 755 / 750 | 633 / 600 | 618 / 599 |
| 0 ka | 1,033 / 1,004, then 1,154 / 1,144 | 1,144 / 1,133, then 1,364 / 1,361 | 1,449 / 1,441 | 1,393 / 1,387 | 933 / 905 |
| 21 ka | 1,099 / 1,072 | 1,231 / 1,222 | — | — | 999 / 970 |
| 1 Ma | 433 / 423 | 447 / 440 | — | — | 415 / 402 |
| 3 Ma | 462 / 460 | 511 / 511 | — | — | 432 / 430 |
| 5 Ma | 486 / 473 | 479 / 469 | — | — | 466 / 435 |

The recent passes rose from about 0.93 s (Task 10) to 1.03–1.45 s: Tasks 11b–11g added the
burnt courses, their order, the lake overlay and the ridged steps (11d measured 1.33–1.45 s in
the dev profile; `--hydro-check` read 1.01 s here, beside a render batch). The 1 s native target
is reported, not gated; the journey runs' 1.39–1.45 s were under the uncapped window's load.

**`EraHydrology` sizes** (the `continental pass done` line, region bench run 2):

| Era | `result_bytes` (`EraHydrology::bytes()`) | `working_bytes()` | Nodes | River nodes | Lakes | Longest step (native task) |
|---|---|---|---|---|---|---|
| 0 ka | 6,964,996 B (6.6 MiB) | 180,411,688 B (172 MiB) | 4,860,000 | 120,571 | 849 | 40.0 ms (`route`) |
| 21 ka | 7,749,996 B (7.4 MiB) | 108,253,732 B (103 MiB) | 4,860,000 | 138,671 | 1,368 | 39.4 ms (`route`) |
| 250 Ma | 1,075,708 B (1.0 MiB) | 4,743,344 B (4.5 MiB) | 194,400 | 24,211 | 349 | 7.3 ms (`route`) |

(The native task's steps are 2¹⁸ units on a background thread and never hold a frame; the
web's time-boxed steps are the Chrome run's `longest_step_ms`.)

### Journey bench and hop memory

| | Run 1 | Run 2 (judged) | Task 10 run 2 |
|---|---|---|---|
| Pacing, resolution | uncapped (median 8.28 ms), window shown, 1920 × 951 | display-paced (16.23 ms); the window was shown for the Andes, then hidden (GPU 1.5 ms a frame), 1920 × 951 | display-paced, 1920 × 951 |
| Holds (≤ 3,000 ms) | dives 430–769 ms, hops 29–46 ms, climbs 24–36 ms: PASS | dives 446–731 ms, hops 39–72 ms, climbs 33–35 ms: PASS | PASS |
| Flight per route | PASS (uncapped rule) | PASS (display-paced rule) | PASS |
| Swap worst frame (≤ 100 ms) | 19.03 ms | 33.75 ms | 31.35 ms |
| **Hop memory (≤ +64 MiB)** | **FAIL: first hop +72.0**; then +48.1, +44.7, +44.4, +54.8, +49.0, +54.5, +52.0, +44.6, +45.2 | **FAIL: first hop +78.1**; then +45.4, +22.4, +22.0, +24.7, +27.4, +29.2, +28.5, +22.3, +22.3 | PASS, worst +31.4 (run 1: first hop +71.0, FAIL) |
| Route checks; journey memory cycles | PASS; Andes +4.7% | PASS; Andes +3.5% | PASS |

**The first hop: a baseline trough, not more memory.** From the hop-memory trace
(`timefly::hop_memory=trace`, cycle 1, MiB):

| Run 2, cycle 1 | Footprint at the hop's start (the baseline) | Peak (after the swap) | Rise | Internal | Graphics | malloc in use |
|---|---|---|---|---|---|---|
| Flying in the first region, before the hop | 1,296.9 | | | 610.8 | 668.5 | 275.9 |
| Hop 1 | **1,028.0** | **1,106.3** | **+78.3** | 364.9 → 392.2 | 666.6 → 711.5 | 305.2 → 282.1 |
| Hop 2 | 1,060.8 | **1,106.3** | +45.5 | 369.0 → 392.6 | 666.2 → 710.6 | 305.2 → 282.9 |

Both hops peak at the same footprint, 1,106.3 MiB. Hop 1's baseline is 33 MiB lower because
macOS returned about 270 MiB of internal memory (the 0 ka pass's 172 MiB working set among it, freed but
resident) just as the hop began: the baseline is sampled in that trough,
and the regrowth to the steady level counts as the hop's rise. Run 1 shows the same (hop 1
1,051.3 → 1,123.5, +72.2; hop 2 1,078.8 → 1,127.1, +48.3). The swap's own cost is the later
hops' +22 to +45 MiB: graphics +45 MiB (the new region's textures and staging under the held
cloud) and internal +23 to +27. Task 7 recorded the same trough ("hop 1's baseline is a trough
after the first dive"), and Task 10's run 1 failed the same way (+71.0).

**The escalation list (plan Step 3), in order:**
1. **Raise `APRON_RIVER_MIN_DISCHARGE_M3S`** (200 → 1,000 m³/s, a scratch build, not committed;
   two journey runs, display-paced with the window hidden throughout, 1920 × 951): **still FAIL.**
   Run 1: worst **+71.0 MiB** (Mt Fuji hop 1), the first hop +53.0, the rest +20.1 to +63.1.
   Run 2: worst **+64.5 MiB** (Andes hop 2), the first hop +50.7, the rest +17.4 to +29.0. In
   run 2 the failing hop is the same trough: its baseline fell from 904.1 to 806.5 MiB as the
   hop began, and it peaked at 871.0, below hop 1's 924.5. Graphics memory read about 130 MiB
   lower in both (no main pass drawn while hidden), so these runs are not like-for-like with
   HEAD's run 2; the change does not remove the failure either way. The scratch build was not
   committed (`params.rs` restored and checked with `git diff`).
2. **Drop the CPU river array after upload:** already done (Task 7, `free_rivers` in
   `terrain/mod.rs`), so nothing to try.

**So the hop-memory gate is the owner's call**, with these facts: the swap itself stays within
+22–55 MiB; only the first hop after the first dive exceeds +64 MiB, by a baseline sampled in the
trough after macOS returns the pass's working set; the gate was not loosened. Options the owner
has seen before: take the baseline as the in-region footprint before the hop's lift (a
measurement change), or keep the gate and count first hops as reported only.

**`MAX_BOULDERS` stays 16,384.** Task 11c's ruling let it rise to 32,768 only if this task's hop
memory stayed under the gate; it did not.

### Scrub bench (once, as the plan asks)

`--scrub-bench`, display-paced, 1920 × 951: **PASS**. Cold load 0.57 s; data 137 frames, 136
images, decoded textures 61.1 MiB; bridge targets 13.9 MiB persistent + 139.3 MiB while
bridging; footprint at first frame 857.3 MiB, scrub peak outside the bridge 1,377.2 MiB, in the
bridge 1,255.2 MiB. Older (present → 540 Ma): 1,200 frames, median 16.70, p95 18.47, worst
32.12 ms, 1 missed (0.08%). Younger: 1,201 frames, 16.68 / 18.41 / 22.52 ms, 0 missed. The
bridge: 22 frames, worst 18.59 ms. The overview now decodes on the globe (Task 4's ruling) and
the scrub's frame gates still pass.

### The climate bake (Task 1)

The Beyer et al. 2020 grids were fetched by HTTP range requests: 39.6 MB in 66 requests for the
bake itself (plus a 1.35 MB probe and a 39.6 MB verification re-run: 80.6 MB in all, under the
owner's 100 MB pre-approval), from the 1,333,236,636-byte `LateQuaternary_Environment.nc`, never
held whole. The output: 35 grids (BIO1 and BIO12, 1°, F16), 2.49 MB, in the main checkout's
`crates/timefly/assets/data/climate` (git-ignored; packaging is a follow-up, ledgered in Task 1).

### White water's GPU cost, re-measured with GPU timestamps (Task 12)

**Result: no measurable cost.** Against the parent of Task 11c (`12ef525`, no white water),
the shipped white water (`9814212`, Task 11c with its fix round) changes the GPU frame time by
**−0.01 ms** at the Indus bench pose and **+0.04 ms** at the rapid close-up (means of the
per-run means; by medians −0.20 and +0.02 ms). The budget is +1.0 ms. Both are inside the
run-to-run spread (up to 0.5 ms between passes of one build), so neither budget cut is needed.
This replaces Task 11c fix round 1's uncapped frame-median fallback (+0.09 and +0.24 ms), recorded below.

**Method.** As Task 11c round 0: `region_tour time --offscreen`, 2560 × 1440, 6 s settle then
30 s timed, Task 10's GPU frame timer (pass timestamps). The parent binary is Task 11c's copy
(`region_tour_before`, built at `12ef525`: `grep -a -c 'fn white_water_foam'` finds 0), HEAD's
is this task's release build (finds 1). Builds alternated pose by pose over four passes, each run
a fresh `TIMEFLY_LOG_DIR`; pass 0 is the warm-up of the new binary (Metal's shader cache) and is
left out of the means.

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 32.5,74.5,0 bench-<build> time 2000
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 32.5735,75.8714,0 rapid-<build> time 150
```

**Pacing and readings.** Every run was display-paced (frame medians 16.66–16.68 ms on the 60 Hz
4K display, 1,800–1,801 frames) and every run's GPU timer read all 1,800–1,801 frames: the
window was on screen this time (the bench display is now a 3840 × 2160 monitor at 1×, so the
2560 × 1440 window fits). The script's attempt to raise the window with System Events had no
effect (no Accessibility grant); the readings show the main pass was drawn (12–13 ms, not the
1.5–1.8 ms of an occluded window).

| GPU frame time, median (mean) ms | Pass 0 (warm-up) | Pass 1 | Pass 2 | Pass 3 | Mean of means, passes 1–3 | Change |
|---|---|---|---|---|---|---|
| Indus bench pose, parent `12ef525` | 12.53 (12.88) | 13.19 (13.03) | 13.26 (13.09) | 13.35 (13.21) | 13.11 | — |
| Indus bench pose, HEAD `9814212` | 12.94 (12.97) | 13.37 (13.36) | 12.53 (12.83) | 13.29 (13.12) | 13.10 | **−0.01 ms** |
| Rapid close-up, parent | 12.44 (12.29) | 12.40 (12.24) | 12.25 (12.27) | 11.92 (12.07) | 12.19 | — |
| Rapid close-up, HEAD | 12.42 (12.25) | 12.23 (12.28) | 12.15 (12.12) | 12.24 (12.29) | 12.23 | **+0.04 ms** |

GPU p95s were 13.5–14.8 ms and worsts 14.8–15.9 ms in every run, with no pattern by build.

### The reality check at HEAD (the record for the PR)

`--hydro-check` on HEAD `9814212` (release, native, the real bake; run beside the render batch,
so its pass time is under load). Full method and history: `reality-check.md`. It exits 3 (FAIL)
on the Indus box's "every link" bar alone, as since Task 11 (the owner's decision, open); every
other bar passes. The summary lines:

```text
pass at 0 ka: 1.01 s (0.18 s sampling), lattice n = 900, 120571 river nodes, 849 lakes, Beyer climate: true
box indus: relaxed 82.7% within 10 km, 74/84 links matched, worst 81.3 km (needs 80% and every link); coarse graph 81.7% within 10 km, 73/84 links matched, worst 82.4 km - FAIL
box amazon (reference below 300 m): relaxed 91.8% within 25 km, 457/482 links matched, worst 145.9 km (needs 80%); coarse graph 91.4% within 25 km, 454/482 links matched, worst 142.6 km - PASS
lakes: 8 of 8 PASS, each off by 0.0 m (5 m allowed): Caspian -28.0 (table), Dead Sea -427.0, Titicaca 3808.5, Great Salt Lake 1278.0, Victoria 1133.0 (the map's fine surface), Michigan 176.0, Huron 176.0, Erie 173.0 (table)
outlets: 22 of 22 PASS (Danube, Jhelum, Indus, Amazon 152 km of 160, Ganges, Brahmaputra, Yangtze, Yellow River, Pearl, Mekong, Nile, Congo, Niger, Mississippi, Hudson, Yenisei, Ob, Orinoco, Paraná, Lena, Volga, Mackenzie)
endorheic lakes' surplus (informative, not routed): 1 [lake 5 (Terminal, -28.0 m, 374667 km2) at (42.02, 50.52): surplus 3478 m3/s of 12882 in]
arid basins (< 250 mm/yr): game 0 of 3 large lakes in the boxes [...], reference 0 of 2 - PASS
ridged joins: 0 of 4714 joining steps climb more than 100 m over the 60″ tiles (0 joins are no lattice step) - PASS
today's courses (informative): 31985 burnt nodes, terrain above the bed max 1362 m, p99 439 m; 4611 over 50 m
today's courses' order (informative): 31633 interior nodes [Follows 28628, SharedNode 92, Ocean 5, InLake 73, NoRiver 2738, BasinRoute 94, Other 3]
discharge table (informative): 1 of 18 gauges flagged (ratio outside 1/3 to 3): the Nile at Aswan, game 14089 m3/s against 2757 (x5.11; channel losses are a follow-up)
hydro check FAIL
```

The lake and outlet lines are condensed above from one line each in the output (the run's full
output is in the Task 12 report). The Indus box's ten missed links are as in
`reality-check.md` (the Shyok and the Arghandab are not in Natural Earth's 1:10m rivers).
Every number above matches Task 11e's record digit for digit (Indus 82.7%, 74/84; Amazon 91.8%;
849 lakes; 22/22), so Tasks 11c and M3's merge changed no hydrology.

### Chrome WebGPU (Task 12, Step 2): PENDING, the controller's run in the owner's Chrome

**The build** (Task 12): `web-release` (wasm32, no features) of HEAD `9814212`, built 2026-11-26:

- `timefly-task12.wasm`, 73,887,711 bytes, sha256
  `1f352c216bf85569ba76941417b6286bb206462a905d9ae1542b6708c430f943`;
- `grep -a -c 'fn white_water_foam'` finds 1 (Task 11c's fix round, the last code commit; the
  shaders are embedded), and `grep -a -c 'Indus tributaries'` finds 1.

Serve it as in Task 10's Chrome section below (`cd <root with assets/>; wasm-server-runner <wasm>`, then
`http://127.0.0.1:1334/?region-bench` and `?journey-bench` in an Incognito window, in front,
hands off; add `&keep-going` to get every verdict).

| What | Web result | Rule |
|---|---|---|
| Chrome version, WebGPU adapter, "WebGPU Developer Features" on or off | **PENDING (controller)** | recorded |
| `?region-bench` summary lines, every gate | **PENDING (controller)** | M3's gates |
| `?journey-bench` summary lines, every gate | **PENDING (controller)** | M3's gates; holds reported against 8 s |
| The continental pass per era: wall and CPU (`continental pass (<age>): …`) | **PENDING (controller)**: 250 Ma ___, 0 ka ___, 21 ka ___, 1 Ma ___, 3 Ma ___, 5 Ma ___ | ≤ 5,000 ms, reported |
| The pass's longest step and slice (`continental pass done … longest_step_ms=… longest_slice_ms=…`) | **PENDING (controller)** | each step within 6 ms |
| Boulder placement on the swap frame (`region boulders placed`), Himalayas and Indus | **PENDING (controller)** | reported |
| GPU frame time per region (`gpu frame time (<name>)`) | **PENDING (controller)** | reported |
| Task 8's renders in Chrome (Manaus, the Indus gorge, Titicaca, the Caspian, Qattara) | **PENDING (controller)** | by eye, against the Metal shots in `manual-checks.md` |

If the pass misses 5 s, the plan's escalation, in order, each ledgered: sample the ICE-6G terms on
a coarser sub-lattice; raise `WEB_HYDRO_BUDGET_MS` to 8 if the globe's frame gate still passes;
lower `HYDRO_RECENT_N` to 768.

## Task 10's record (2026-11-26, `f846c1a`)

| | |
|---|---|
| Machine | Apple M4, 16 GB, macOS 27.2.1 (26B131) |
| Adapter and backend | Apple M4 (Metal), wgpu 30.0.2 through Bevy 0.20.1 |
| Build | `f846c1a` for every native run, the `release` profile (no LTO). The binaries' labels read a stale SHA (worktree), so `grep -a -c 'gpu time: pass timestamps'` and `grep -a -c 'Indus tributaries'` (both new in this task) finding 1 in each binary is the proof |
| Assets | the `rivers-root` scratch root (the main checkout's `assets/data`, this branch's shaders) |

### Summary

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
| White water (Task 11c): water and scatter passes, 2560 × 1440 | **+0.07 ms** at the Indus bench pose, −0.32 ms at a rapid close-up (budget +1.0 ms); fix round 1, by uncapped frame medians: +0.09 ms and +0.24 ms |
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

## White water (Task 11c): GPU cost and boulder counts (native)

**Result: within the +1.0 ms budget.** At Task 10's Indus bench pose the water and scatter
passes together add **+0.07 ms** of GPU time a frame (means; +0.14 ms by medians); at a full
rapid close-up (150 m up, clustered rocks and foam collars in view) the after build read
**0.32 ms less** than the parent (means; 0.40 ms by medians). Neither budget cut was needed: the
collars' reach stays 3,000 m and the standing waves keep their single octave.

**Method.** Task 10's GPU frame timer (`GpuFrameTime`, pass timestamps; its readings time the
whole frame, so the difference between builds is the water's and the scatter's change: nothing
else differs), `region_tour time --offscreen` at 2560 × 1440, 6 s settle then 30 s timed. The
parent `12ef525` and the Task 11c build, both `release` and copied out (proved by
`grep -a -c STREAK_CONTRAST`: 0 in the parent, 3 in the new one), alternated over three passes
after each had rendered once (Metal's shader cache), each run with a fresh `TIMEFLY_LOG_DIR`:

```sh
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 32.5,74.5,0 bench-<build> time 2000
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=$R $T --offscreen <out> 2560x1440 32.5735,75.8714,0 rapid-<build> time 150
```

**Pacing.** Every run was display-paced: frame-time medians 16.65–16.67 ms, 1,800–1,801 frames
each. So the GPU idled between frames, at whatever clocks it chose (see the caveats above).

| GPU frame time, median (mean) ms | Pass 1 | Pass 2 | Pass 3 | Mean of means | Change |
|---|---|---|---|---|---|
| Indus bench pose, parent | 12.92 (12.94) | 13.12 (12.95) | 12.86 (12.94) | 12.94 | — |
| Indus bench pose, Task 11c | 13.22 (13.03) | 13.20 (12.97) | 12.91 (13.02) | 13.01 | **+0.07 ms** |
| Rapid close-up, parent | 11.90 (11.95) | 11.92 (11.95) | 12.48 (12.24) | 12.05 | — |
| Rapid close-up, Task 11c | 11.67 (11.72) | 11.76 (11.74) | 11.66 (11.73) | 11.73 | **−0.32 ms** |

The close-up's drop is consistent across the passes but not explained by the change: the new
water skips its white-water noise wherever `rapid` is 0 (the parent ran it on every river pixel
and multiplied it by 0), which saves work on the slow reaches in view, but a third of a
millisecond is more than that alone suggests; paced clocks may also differ. Either way the
change is not a cost. The collar albedo was brightened by one multiply after these runs (the
committed build); its cost is the same.

### Fix round 1: thinning by direction, and the owner's look

**Boulders thinned by direction (the review's Important).** Round 0 cut a region's boulders at
`MAX_BOULDERS` in sub-segment order, a region-dependent prefix, so overlapping regions held
different rocks. Now a cluster is kept, whole, exactly when a hash of its `(river, k, c)` falls
under `BOULDER_KEEP_FRACTION = 0.45`, and the cap is only a guard that warns with the region and
the overflow. The owner's "bigger rocks" (`BOULDER_RADIUS_M` 0.7–5 m, was 0.4–3; breakers up to
8 m) came in the same round. Per bench region (`region_tour … shaded 3000`, 640 × 360, native
release, the `region boulders placed` line):

| Bench region | Boulders | Share of `MAX_BOULDERS` | Collars | Placement on the swap frame | Cap guard |
|---|---|---|---|---|---|
| Pangaea interior (250 Ma) | 0 | 0% | 0 | 0.12 ms | silent |
| Red Sea (0 ka) | 0 | 0% | 0 | 0.08 ms | silent |
| Himalayas (0 ka) | 11,159 | **68%** | 4,353 | 1.91 ms | silent |
| Indus tributaries (0 ka) | 1,463 | 9% | 625 | 0.39 ms | silent |
| Sundaland (21 ka) | 0 | 0% | 0 | 0.12 ms | silent |
| Japan (1 Ma) | 70 | 0.4% | 5 | 0.13 ms | silent |
| Beringia (21 ka) | 0 | 0% | 0 | 0.11 ms | silent |
| Rockies ice margin (21 ka) | 644 | 4% | 280 | 0.26 ms | silent |
| Panama (3 Ma) | 161 | 1% | 1 | 0.06 ms | silent |

The Himalayas, the densest, hold 68% of the cap (under the ruling's 75%); round 0 cut 8,580 of
its 24,964 there. `MAX_BOULDERS` stays 16,384. The swap frame's placement there fell from 4.72 ms
(round 0 placed every boulder twice to find the collars) to 1.91 ms: boulders and collars are now
placed in one pass, a collar only at a foam site.

**The owner's "less white" (foam on the crests and in the rocks' wakes).** Measured as the share
of near-white pixels (every channel above 140, spread under 60) among the water pixels of the
rapid close-up's crop (`32.5735,75.8714,0`, AGL 150, 3840 × 2160, the 45° view, crop
`1300,150 – 2500,825`; script `foamfrac.py` in the task's scratch): **55% at `d37d8e6`, 21% after**.
A full rapid's foam fades to `FOAM_MEAN = 0.28` where unresolved (was about 0.97), so from afar
rapids now read pale rather than white.

**GPU cost, re-measured.** The GPU timer gave no readings in this session's runs, and every run
ran uncapped (frame medians 9.5–12.7 ms; the window was not being shown), so the fallback is
the frame-time medians, interleaved over three passes against the parent `12ef525` (GPU-bound
uncapped, an upper bound on the GPU's share):

| Frame time, median ms | Pass 1 | Pass 2 | Pass 3 | Mean | Change |
|---|---|---|---|---|---|
| Indus bench pose, parent | 12.24 | 12.66 | 12.69 | 12.53 | — |
| Indus bench pose, fix round 1 | 12.56 | 12.62 | 12.69 | 12.62 | **+0.09 ms** |
| Rapid close-up, parent | 9.51 | 9.54 | 9.67 | 9.57 | — |
| Rapid close-up, fix round 1 | 9.70 | 9.74 | 9.98 | 9.81 | **+0.24 ms** |

Both are within the +1.0 ms budget.

### Fix round 2: rocks only where the water is drawn

The owner found rocks and collars on a grass bank. A rock is now kept only where a spread of
texel lattices draw water over it (`water::hold_in_drawn_water`), and none in the band where
the inner field blends into the apron. The keep fraction rose from 0.45 to 0.6 to offset the
drop. Per bench region (the same command as above):

| Bench region | Boulders | Share of `MAX_BOULDERS` | Collars | Placement | Prep (region job) |
|---|---|---|---|---|---|
| Pangaea interior (250 Ma) | 0 | 0% | 0 | 0.13 ms | 102 ms |
| Red Sea (0 ka) | 0 | 0% | 0 | 0.12 ms | 278 ms |
| Himalayas (0 ka) | 7,320 | **45%** | 2,850 | 1.15 ms | 536 ms (217 ms at `d37d8e6`) |
| Indus tributaries (0 ka) | 930 | 6% | 396 | 0.27 ms | 272 ms (219 ms at `d37d8e6`) |
| Sundaland (21 ka) | 0 | 0% | 0 | 0.13 ms | 180 ms |
| Japan (1 Ma) | 5 | 0% | 3 | 0.11 ms | 1,737 ms |
| Beringia (21 ka) | 0 | 0% | 0 | 0.11 ms | 214 ms |
| Rockies ice margin (21 ka) | 520 | 3% | 202 | 0.22 ms | 450 ms |
| Panama (3 Ma) | 0 | 0% | 0 | 0.04 ms | 104 ms |

The check reads 64 more texels for each kept cluster, on the region job's prep thread. That
costs the Himalayas about +320 ms of prep, which is not on a frame. The cap guard stayed silent
everywhere.

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
