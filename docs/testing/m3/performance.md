# Milestone 3 (the fly and zoom-and-swap): performance

Measured on 2026-11-24 and 25 for Task 11. The benches ran on `4b79f08`, Task 10's last commit.
The cloud cost re-measure and the web build used `e3bd6e9`, Task 11's code commit, which drops the
tilt-shift (see "Cloud pass cost"). On 2026-11-25 the owner asked for the tilt-shift back, and it
is back in the game (about 1.8 ms, inside the owner's accepted 2 ms).

| | |
|---|---|
| Machine | Apple M4, 16 GB, macOS 27.2.1 (26B131) |
| Adapter and backend | Apple M4 (Metal), wgpu 30.0.2 through Bevy 0.20.1 |
| Build | `4b79f08`, the `release` profile (no LTO, line tables). The binary's label reads `4b79f08`, and `grep -a -c 'journey bench summary'` finds the journey bench's string, which isn't on `main` |
| Display | the main display is a 1920 × 1080 monitor at 60 Hz, so the requested 2560 × 1440 window was clamped to a **1920 × 951** render (as under M2's ruling R21). The cloud pass's cost was measured offscreen at 2560 × 1440 (below) |
| Pacing | `AutoNoVsync` requested; the display paced every flight at 60 Hz (medians 16.6–16.7 ms), so R14's display-paced rule decided everywhere |
| Runs | each bench twice, each with a fresh `TIMEFLY_LOG_DIR`. Run 1 was the binary's first launch (Metal's shader cache misses); **run 2 is the one judged**. Every session ended clean |

```sh
cargo build --release -p timefly
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=<a root whose assets/ has data, shaders, credits.txt> \
  target/release/timefly --region-bench --target-fps 60 --window 2560x1440
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT=<same> \
  target/release/timefly --journey-bench --target-fps 60 --window 2560x1440
```

## Summary

| Bench | Run 1 | Run 2 (judged) |
|---|---|---|
| `--region-bench` | PASS (exit 0) | **PASS** (exit 0) |
| `--journey-bench` | FAIL (exit 3), hop memory only | **FAIL** (exit 3), **hop memory only** |
| `--journey-bench` after the hop-memory work (`ec9293e`, gate +64 MiB; the Alps; 2560 × 1440 uncapped) | PASS (exit 0): hop memory worst +34.1 MiB | PASS (exit 0): hop memory worst +29.5 MiB |
| `--journey-bench`, the owner's route set (`d3b38ff`; 2560 × 1440) | FAIL (exit 3) as run: Pangaea's flight, uncapped p95 18.40 ms; hop memory worst **+48.0 MiB**. Re-judged under the fix-round rules (one pacing per run; Pangaea's flight reported only, owner 2026-11-25): **PASS** | **PASS** (exit 0, display-paced): hop memory worst +30.2 MiB; every gate passes; unchanged when re-judged |
| Cloud pass cost (offscreen, 2560 × 1440; budget ≤ 1 ms) | about 1.8 ms with the tilt-shift | about 2.0–2.2 ms without it: over the 1 ms budget, **accepted by the owner** (2026-11-25) |

**Hop memory: fixed in part, gate raised.** A per-frame memory trace found that most of the hop
peak, and its +3 to +54 MiB spread, was malloc keeping each region's freed generator lattice
resident; the lattice now lives in fixed-size chunks that the next region reuses. The first hop
of a run still reads up to +37.5 MiB, too close to +40 to gate on, so the native gate is the
owner's +64 MiB (2026-11-25). See "Hop memory" below. The journey bench was re-run for this on a
newer build and a different display (a 4K monitor, so the 2560 × 1440 window rendered in full,
uncapped rather than display-paced): its flight and swap numbers there aren't comparable with
Task 11's table below. One of the eight runs, `k2`, failed the flight gate's uncapped rule (p95
16.86 ms against 16.67; every other run 12.1–16.5 ms); see "Hop memory" for the runs.

**Cloud pass: accepted.** On 2026-11-25 the owner accepted the pass at about 2 ms, so the
noise stays as it is, and the tilt-shift is back at the owner's request. Dropping it (Task 5's
ruling) had saved nothing, because the cost is the noise itself: six evaluations a pixel. If it ever needs to be cheaper, each of these is a
look change:
- drop to three octaves (`CLOUD_OCTAVES` allows 3–4);
- skip the two sun taps (the lighting's `shape(toward)`);
- shade the cloud at half resolution.
Or accept the cost: the pass runs only during transitions.

Every other gate passes in both runs: the eight regions' flight, the globe pan, warm
generation, memory cycles, the 12 holds, flight outside the cloud, the swap worst frame and the
journey memory cycles.

- **Cloud pass:** about 1.8 ms a frame at 2560 × 1440, over Task 5's 1 ms budget. So Task 11
  dropped the tilt-shift, as Task 5's ruling said. Without it the pass costs about 2.0–2.2 ms:
  the noise is the cost (see "Cloud pass cost"). The owner accepted that cost on 2026-11-25
  and asked for the tilt-shift's softer edges back, so it is restored: the pass is at about
  1.8 ms again.
  - The benches ran on `4b79f08`, which has the tilt-shift, so they match the restored cloud.
- **Near geometry under the cloud** (Task 7's rule), observed on Metal: PASS (a new GPU test).
- **Chrome WebGPU:** pending; the controller runs it.
- The owner's checks are in `manual-checks.md` and the deferred minors in `deferred.md`, both in
  this folder.

## Region bench

Run 2's summary block, verbatim:

```text
===== Time Fly region bench summary =====
build: 0.1.0 (4b79f08, release, aarch64-apple-darwin; system-spans)
system spans: on
dunes: on
adapter: Apple M4 (Metal)
os: macOS 27.2.1 (26B131), aarch64
regions: all 8
regions (R14 frame rate): Pangaea interior PASS, Red Sea PASS, Himalayas PASS, Sundaland PASS, Japan PASS, Beringia PASS, Rockies ice margin PASS, Panama PASS
first-region worst frame (R11 gate <= 250 ms, request to first path frame, overview decode included): 42.60 ms -> PASS (native: reported only, the gate applies in Chrome WebGPU)
Himalayas load-window worst frame (cold tiles; reported only): 23.90 ms
warm generation (10 re-entries of Himalayas, target <= 3000 ms): median 316 ms, max 332 ms -> PASS
first entries (cold tiles; reported only natively): Pangaea interior 549 ms, Red Sea 482 ms, Himalayas 332 ms, Sundaland 332 ms, Japan 1865 ms, Beringia 382 ms, Rockies ice margin 616 ms, Panama 332 ms
memory cycles (phys_footprint, includes GPU allocations on Apple silicon, on the globe after each leave): 1: 640.7 MiB, 2: 640.8 MiB, 3: 641.1 MiB, 4: 641.2 MiB, 5: 641.2 MiB, 6: 641.3 MiB, 7: 641.5 MiB, 8: 641.6 MiB, 9: 641.7 MiB, 10: 641.8 MiB; worst of cycles 2-10 is +0.2% of cycle 1 (limit +10%) -> PASS
  in the region: 1: 773.6 MiB, 2: 773.7 MiB, 3: 774.0 MiB, 4: 774.1 MiB, 5: 774.2 MiB, 6: 774.2 MiB, 7: 774.5 MiB, 8: 774.5 MiB, 9: 774.6 MiB, 10: 774.7 MiB
result: PASS
===== end region bench summary =====
```

Run 1's summary: `result: PASS`. Its R11 frame was 290.05 ms, Metal compiling the region and
cloud pipelines for a binary it hadn't seen (reported only natively; run 2 read 42.60 ms). Warm
generation was a median of 316 ms and a max of 331 ms, and the memory cycles +0.2%.

### Per region, run 2, against M2

The like-for-like M2 baseline is the art pass's native run (`fe37635`, M2's last native bench
set, also clamped to 1920 × 951 on this display, display-paced). Task 6's run 2 (`8edbe0b`, mid
M3, after the bench pitches were retuned) is shown for its worst frames.

| Region | First entry (M3 / art pass) | Load: request to first path frame, worst | Flight median / p95 / worst, M3 run 2 | Missed (> 25 ms) | Art pass median / p95 / worst | Task 6 run 2 worst |
|---|---|---|---|---|---|---|
| Pangaea interior | 549 / 548 ms | 3.60 s, 42.60 ms (R11) | 16.66 / 17.93 / 20.22 ms | 0 | 16.68 / 18.33 / 21.10 ms | 20.35 ms |
| Red Sea | 482 / 482 ms | 3.53 s, 24.42 ms | 16.67 / 18.21 / **47.02** ms | 4 (0.09%) | 16.67 / 18.36 / **56.44** ms | **65.21** ms |
| Himalayas | 332 / 333 ms | 3.38 s, 23.90 ms | 16.67 / 18.25 / 20.47 ms | 0 | 16.68 / 18.41 / 20.91 ms | 20.30 ms |
| Sundaland | 332 / 332 ms | 3.38 s, 22.51 ms | 16.67 / 18.16 / 19.65 ms | 0 | 16.69 / 18.38 / 20.48 ms | 19.99 ms |
| Japan | 1,865 / 1,884 ms | 4.90 s, 25.68 ms | 16.66 / 18.16 / 19.22 ms | 0 | 16.68 / 18.40 / **36.55** ms | **59.03** ms |
| Beringia | 382 / 382 ms | 3.42 s, 22.23 ms | 16.68 / 17.99 / 19.58 ms | 0 | 16.69 / 18.29 / 20.38 ms | 25.47 ms |
| Rockies ice margin | 616 / 616 ms | 3.63 s, 23.71 ms | 16.67 / 18.10 / 21.45 ms | 0 | 16.69 / 18.33 / 20.22 ms | 28.60 ms |
| Panama | 332 / 332 ms | 3.35 s, 22.96 ms | 16.68 / 18.15 / 19.80 ms | 0 | 16.69 / 18.32 / 20.70 ms | (uncapped; run 1 21.80 ms) |
| Globe pan (8 s) | — | — | 16.69 / 18.06 / 18.99 ms (480 frames) | — | 16.73 / 18.48 / 20.04 ms | 18.12 ms |

The M3 flight is 75 s (the high pass at 60–75 s is new, Task 6); the art pass's was 60 s.

**Flight gates hold, with no regression.** Every M3 median is on the 60 Hz interval and every
p95 is 0.1–0.4 ms *lower* than the art pass's. Missed frames: 4 in all eight flights (Red Sea,
0.09%), against 5 in the art pass's.

**Single-frame stalls in the older segments.** Run 2 has one: a 47.02 ms frame in the Red Sea's
climb. The other 39 segments' worst frames are 18.7–21.5 ms. In run 1 the Himalayas had one in
the coast run (66.74 ms) and one in the low pass (38.03 ms), and the Red Sea a 32.33 ms frame in
its climb. Task 6's run 2 had them in the Red Sea's apron sweep (65.21 ms) and Japan's coast run
(59.03 ms), and the art pass in the Red Sea's low pass (56.44 ms) and Japan (36.55 ms). They land
in a different segment each run, and no segment repeats one, so **the retuned pitches (Task 6)
added no stall**: these are the occasional one-off hitches every M2 run has had. The owner
doesn't weigh them; judged by medians, p95 and missed frames, every segment passes.

**The edge segments aren't strictly comparable.** Task 9 (ruling P5) pulled the path in to
`FIELD_LIMIT` 0.5 × `EDGE_M` (coast runs to 0.45 × `EDGE_M`), so the bench never comes within
the preload distance of the edge. The coast run and apron sweep now fly a little further from
the edge than in M2, looking at much the same view.

**Generation and memory: no regression.**
- First entries are within 1% of the art pass's in every region (Japan 1,865 ms against 1,884).
- Warm generation: median 316 ms, max 332 ms (art pass 316 / 333 ms).
- Memory cycles: +0.2% (art pass +0.3%). The footprint is lower: 640.7–641.8 MiB on the globe
  (art pass 652.7–665.8) and 773.6–774.7 MiB in the region (784.1–787.3).
- The load window (request to first path frame) is longer: 3.35–4.90 s, against 2.17–2.53 s
  in M2's runs. A `--region` entry now goes through the cloud and the fly emerges for
  `EMERGE_S` (2 s) before the path starts; the load's worst frames stay at 22–26 ms after the
  first region.

## Journey bench

*(Task 11's runs below flew the bench's first route, the Alps. Since 2026-11-25 the bench flies
the owner's route set instead; see "Journey route set" below. The Alps numbers are the
"before".)*

`--journey-bench`: from the globe, dive into the Alps (46.5° N, 10.0° E, present), descend to
about 20 km above the ground, fly east over the edge and hop twice, then climb back out with
`Backspace`. Three cycles. Run 2's summary block, verbatim:

```text
===== Time Fly journey bench summary =====
build: 0.1.0 (4b79f08, release, aarch64-apple-darwin; system-spans)
system spans: on
dunes: on
adapter: Apple M4 (Metal)
os: macOS 27.2.1 (26B131), aarch64
render resolution: 1920x951
bake: manifest v2, sha256 013f81f437f19736
region: Alps (46.5, 10 at 0 years), 3 cycles of a dive, 2 hops and a climb
hold (from the first frame drawn at cloud density exactly 1 to the first frame after it drawn below 1): native gate <= 3000 ms each
hold: dive 725 ms (cycle 1) -> PASS
hold: hop 1 32 ms (cycle 1) -> PASS
hold: hop 2 32 ms (cycle 1) -> PASS
hold: climb 35 ms (cycle 1) -> PASS
hold: dive 538 ms (cycle 2) -> PASS
hold: hop 1 33 ms (cycle 2) -> PASS
hold: hop 2 32 ms (cycle 2) -> PASS
hold: climb 36 ms (cycle 2) -> PASS
hold: dive 537 ms (cycle 3) -> PASS
hold: hop 1 31 ms (cycle 3) -> PASS
hold: hop 2 33 ms (cycle 3) -> PASS
hold: climb 33 ms (cycle 3) -> PASS
present mode: AutoNoVsync requested; display refresh 60.0 Hz; median 16.66 ms -> display-paced
target: 60 fps (interval 16.67 ms)
pass rule (display-paced): missed frames (>25.0 ms) <= 1% and median <= 17.50 ms (interval +5%)
flight (outside the cloud): frames 10980, median 16.66 ms, p95 18.69 ms, worst 23.78 ms, stalls (>33.3 ms = 2x interval) 0 -> PASS
  missed frames (>25.0 ms = 1.5x interval): 0 (0.00%)
  host stalls in the flight (frames > 1000 ms; page paused or throttled?): 0
swap worst frame (native gate <= 100 ms; the swap frame and the 2 after it): 34.66 ms; dive 28.71 ms (cycle 1), hop 1 33.01 ms (cycle 1), hop 2 33.59 ms (cycle 1), climb 19.16 ms (cycle 1), dive 23.54 ms (cycle 2), hop 1 34.01 ms (cycle 2), hop 2 31.97 ms (cycle 2), climb 18.51 ms (cycle 2), dive 22.48 ms (cycle 3), hop 1 34.66 ms (cycle 3), hop 2 34.12 ms (cycle 3), climb 18.95 ms (cycle 3) -> PASS
hop lift (reported only; from the height above the ground before the lift to the hop's highest height above sea level, the lift rising to 60 km): hop 1 21.1 km -> 60.0 km (cycle 1), hop 2 21.1 km -> 60.0 km (cycle 1), hop 1 19.4 km -> 60.0 km (cycle 2), hop 2 19.8 km -> 60.0 km (cycle 2), hop 1 19.8 km -> 60.2 km (cycle 3), hop 2 20.5 km -> 60.0 km (cycle 3)
hop memory peak (phys_footprint, includes GPU allocations on Apple silicon; gate: peak <= in-region baseline before the hop +40 MiB): worst +48.9 MiB over the baseline; hop 1 +30.0 MiB (cycle 1), hop 2 +26.7 MiB (cycle 1), hop 1 +48.9 MiB (cycle 2), hop 2 +42.3 MiB (cycle 2), hop 1 +11.0 MiB (cycle 3), hop 2 +3.3 MiB (cycle 3) -> FAIL
journey memory cycles (phys_footprint, includes GPU allocations on Apple silicon, on the globe after each climb): 1: 666.1 MiB, 2: 657.0 MiB, 3: 657.5 MiB; worst of cycles 2-3 is -1.3% of cycle 1 (limit +10%) -> PASS
result: FAIL
===== end journey bench summary =====
```

| Gate | Run 1 | Run 2 (judged) | Result |
|---|---|---|---|
| Hold per transition (native ≤ 3,000 ms) | dives 721, 540, 538 ms; hops and climbs 31–35 ms | dives 725, 538, 537 ms; hops and climbs 31–36 ms | PASS |
| Flight outside the cloud (R14, display-paced) | 11,442 frames, 16.68 / 18.33 / 36.80 ms, 1 stall, 1 missed (0.01%) | 10,980 frames, 16.66 / 18.69 / 23.78 ms, 0 stalls, 0 missed | PASS |
| Swap worst frame (native ≤ 100 ms) | worst 36.15 ms | worst 34.66 ms | PASS |
| Hop memory peak (≤ baseline + 40 MiB) | worst **+43.3 MiB** | worst **+48.9 MiB** | **FAIL** at `4b79f08`; **PASS** after the lattice fix (`ec9293e`) and the +64 MiB gate: worst +29.5 MiB in the judged run, see below |
| Journey memory cycles (≤ +10%) | 675.5, 719.9, 670.3 MiB: +6.6% | 666.1, 657.0, 657.5 MiB: −1.3% | PASS |

- **Holds.** A dive holds 0.54–0.73 s at full cover natively: the region generates in about
  0.3–0.5 s, and the first dive pays the cold tiles. A hop holds about 2 frames: the neighbour is
  preloaded well before the edge, so it is always ready when the cloud closes. The hop lift ran
  every time (about 20 km above the ground up to 60 km).
- **Swap frames.** Hops cost 32–35 ms (the terrain's new textures upload on the swap frame, by the
  plan's ruling), dives 22–29 ms, climbs 18–19 ms. All under the white-out, and a third of the
  100 ms gate.

### Hop memory: measured; the lattice fixed, the gate raised to +64 MiB

**Outcome.** The worst hop read +48.9 MiB in Task 11. Measured with a per-frame memory trace,
about half of that peak was malloc keeping a freed generator lattice resident, and that freed
memory is what made the peaks swing from +3 to +54 MiB. Storing the lattice in fixed-size
chunks (`ec9293e`) lets each region reuse the last region's lattice pages: a hop now reads about
+22 MiB, and the spread is gone. But the first hop of a run still reads +31 to +37.5 MiB (the
swap's upload, plus pages for the terrain's image data that no freed block yet fits), only
2.5 MiB under +40 at worst. That isn't a margin a gate can hold, so the gate is now the owner's
**+64 MiB** (`HOP_MEMORY_LIMIT_MIB`), with the fix kept. Judged run: worst +29.5 MiB, PASS.

**How it was measured.** `458d07b` adds an off-by-default trace
(`TIMEFLY_LOG=warn,timefly=info,timefly::hop_memory=trace`, `crate::bench::journey::trace`). On
every region frame it logs, next to navigation's phase:
- the footprint and its parts from `task_info(TASK_VM_INFO)`: `phys_footprint` (what the bench
  gates), `internal` (dirty anonymous pages: malloc's) and the graphics ledger (Metal's textures
  and buffers);
- `malloc_zone_statistics`: bytes in live blocks and bytes malloc holds;
- what it can attribute: the neighbour's grid, the job's stage, the terrain's image data on the
  CPU and the field textures in the render world.

A native generation also logs from its worker thread at each step: the lattice and coast field
built, the row blocks done, the grid assembled, the generator dropped. Three trace runs went
into this: two before the fix (`t1`, `t2`) and one after (`f1`). The bench's own numbers in the
trace runs match its untraced runs.

**What the allocator does (the cause of the spread).** On macOS 27.2 a freed `malloc` block
keeps its pages dirty and counted in `phys_footprint`. malloc stops counting it as held, but
the pages stay until a request of *the same size* reuses them; sleeping doesn't release them,
and `malloc_zone_pressure_relief` returns 0. In a C test, a freed 20 MiB block was reused by
the next 20 MiB request, but a 22 MiB request took 22 MiB of new pages; 30 frees of distinct
sizes left 580 MiB resident. `MallocLargeCache=0` makes frees return at once, which points at
malloc's large-block cache.

The generator's lattice was one allocation per cube-face block, and its size changes with
every region (19.5–24.5 MiB on this route). So whether a hop's lattice took new pages depended
on whether an earlier region had freed a block at least as large of the same size: the
allocator-reuse hypothesis, confirmed, with size as the reason. The other big buffers have fixed
sizes (the grid's fields and the terrain's image data are 1025² × 4 B or 513² × 4 B, the row
blocks 16 rows), so they are reused from the second hop on.

**Before the fix: the worst hop and the smallest** (MiB over the baseline, run `t1`, display-paced
at 1920 × 951; read from the trace):

Worst, cycle 1 hop 2, **+53.5**:

| Step | Footprint | Of which |
|---|---|---|
| Lattice and coast field built (22.0 MiB) | +21.9 | all new pages: no freed block of that size to reuse |
| Row blocks (10.0 MiB) | +21.9 | +0: reused freed pages |
| Grid assembled (10.0 MiB) | +29.4 | +7.5 new |
| Generator dropped | +29.4 | the lattice's 22 MiB freed, but its pages stay counted |
| Swap frame: the terrain's image data (10 MiB on the CPU) | +37.9 | +8.1 new |
| Swap + 1 | +39.8 | +1.9 |
| Swap + 2: the upload | **+53.5** | internal +8.3 more (staging), graphics +16.0, other −10.2 |
| Back to flying | +37.5 | 28 MiB of freed pages still counted (in use: +9.0) |

Smallest, cycle 3 hop 2, **+10.8**: the same route's lattice from cycle 2 had left a block of
the same size, so the lattice, rows and grid took +0.6 MiB in all; the image data reused freed
pages too. The peak is only the upload's transient, and the frame sample caught the graphics
ledger at +10.2, not the +16–22 it reaches on other hops.

The bench's per-frame sample adds its own noise: it reads the footprint once a frame on the
main thread while the render thread uploads. The trace's render-world samples saw up to +48.0
MiB in hops whose frame samples read +33.5.

**After the fix: the same breakdown** (run `f1`, uncapped at 2560 × 1440; this run also had the
early texture free described below, which changed nothing measurable):

| Step | Cycle 1 hop 1 (the run's worst, +32.7) | Cycle 2 hop 2 (typical, +23.0) |
|---|---|---|
| Lattice built (20.5 / 24.5 MiB) | +0.3: the dive's lattice chunks reused | +0.3 |
| Rows, grid, generator dropped | +3.5: mostly reused freed pages | +1.2 |
| Swap frame: the image data | +10.6 (+8.0 new: the first hop of a run) | +1.1 (reused) |
| Swap + 1 | +14.6 | +3.5 |
| Swap + 2 and + 3: the upload | **+32.7**: internal +21.6, graphics +21.1, other −10.0 | **+23.0**: internal +11.1, graphics +21.8, other −10.0 |
| Swap + 5 | +10.7 | +0.9 |
| Back to flying | +11.3 | +1.1 |

**What every hop's peak is now.** The peak is the swap's upload, for two or three frames:
- about +10 MiB of internal pages, the upload's staging (wgpu writes the 10 MiB of field data
  through a staging buffer), freed three frames later;
- the graphics ledger +21–22 MiB, while the footprint's remainder (not internal, not graphics)
  falls by 10 MiB in the same frames: about +12 MiB net. The likeliest reading is the new
  textures plus the staging buffer, with the old textures' 10 MiB leaving another ledger; the
  trace doesn't separate those, so this is not verified;
- on the first hop of a run only, the image data's 10 MiB as new pages (about +8 MiB measured);
  later hops reuse the previous upload's.

So a hop reads about +22 MiB, the first hop of a run about +31 to +37.5. The two regions' grids
overlap only from the preload to the swap, as the design says, and since the neighbour's grid
appears to reuse the dive's freed image data (the same sizes), it adds almost nothing.

**Why +64 MiB, not +40.** Over eight runs with the fix (`f1`, `j1`, `j2`, `k1`, `k2`, `k3` and
the two at +64), the first hop of a run read +32.7, +31.0, +34.3, +2.1, +33.3, +37.5, +34.1 and
+29.5 MiB; every later hop read +2.0 to +35.9, most of them about +22. The first hop's +37.5 is
2.5 MiB under +40. And without any page reuse at all the parts add up to about +44 MiB: the
neighbour's grid 10, the image data 10, the upload's staging 10, the textures net 12, the
lattice's growth over the last region's 2. That case needs the freed pages gone before the hop's
baseline is read (the OS could reclaim them under memory pressure); the runs never showed it.
+64 MiB sits about 26 MiB above the worst hop measured and 20 MiB above that sum. The bench
still samples the footprint once a frame on the main thread, as before; the render thread's own
samples were higher in the traces (up to +48.0 MiB in a hop whose frame samples read +33.5), so
the gate reads what the bench has always read, not the true instantaneous peak.

**What else could lower it** (not done; each is bigger than a contained change):
- free the old grid before the terrain's new image data is allocated on the swap frame, so the
  image data reuses the old grid's blocks (the same sizes): about 8–10 MiB off the first hop.
  The water and sky hold the old grid until after the terrain uploads, so this reorders the
  swap frame;
- upload the fields over several frames (Bevy's bytes-per-frame limit): less staging at once,
  but the new terrain would appear a few frames after the swap, against the plan's ruling that
  it uploads on the swap frame.

**Tried and dropped: freeing the old textures a frame earlier.** Running
`Assets::<Image>::track_assets` again in `PostUpdate` frees a replaced region's old field
textures in the swap frame's extraction instead of the next one. It worked in the render world
(the old and new textures no longer overlapped there: the field-texture count stayed at 14
through the swap instead of 14 → 18 → 14), but the footprint didn't change. The graphics ledger
peaked at +21–22 MiB with it and without it: Metal frees textures when the frames in flight
finish, whichever frame Bevy drops them in. The change and its test were reverted.

**The journey memory cycles** are flatter too: +0.2%, −3.0%, +1.2%, +0.2%, +1.1%, +1.3%, +0.5%
and +1.0% in the eight runs after the fix. Before it, `t1` read +16.4% (a FAIL) and Task 11's
run 1 +6.6%; the likeliest cause is the same retained lattice pages (not traced per cycle).

| Run | Fix | Render | Hop peaks, cycle 1 hop 1, 1 h 2, 2 h 1, 2 h 2, 3 h 1, 3 h 2 (MiB) | Worst |
|---|---|---|---|---|
| Task 11 run 1 | before | 1920 × 951 paced | +43.3, +27.7, +31.8, +29.5, +23.9, +36.3 | +43.3 |
| Task 11 run 2 | before | 1920 × 951 paced | +30.0, +26.7, +48.9, +42.3, +11.0, +3.3 | +48.9 |
| `t1` (trace) | before | 1920 × 951 paced | +33.3, +53.5, +16.2, +17.7, +22.0, +10.8 | +53.5 |
| `t2` (trace) | before | 2560 × 1440 uncapped | +9.6, +39.6, +40.5, +36.8, +17.9, +22.9 | +40.5 |
| `f1` (trace) | lattice + early texture free | 2560 × 1440 uncapped | +32.7, +26.4, +22.0, +23.0, +21.8, +22.2 | +32.7 |
| `j1` | lattice + early texture free | 2560 × 1440 uncapped | +31.0, +35.9, +4.0, +22.2, +30.8, +22.2 | +35.9 |
| `j2` | lattice + early texture free | 2560 × 1440 uncapped | +34.3, +23.9, +23.0, +23.2, +21.8, +22.3 | +34.3 |
| `k1` | lattice | 2560 × 1440 uncapped | +2.1, +23.4, +11.7, +22.3, +21.8, +22.9 | +23.4 |
| `k2` | lattice | 2560 × 1440 uncapped | +33.3, +5.8, +34.5, +23.8, +23.7, +22.0 | +34.5 |
| `k3` | lattice | 2560 × 1440 uncapped | +37.5, +26.4, +21.8, +23.4, +23.8, +9.8 | +37.5 |
| run 1 (gate +64) | lattice | 2560 × 1440 uncapped | +34.1, +32.9, +22.0, +25.2, +21.8, +23.0 | +34.1 |
| **run 2 (gate +64, judged)** | lattice | 2560 × 1440 uncapped | +29.5, +2.0, +22.0, +22.9, +22.1, +18.7 | **+29.5** |

**The other gates in these runs.** Every hold, swap frame and journey memory cycle passed in all
eight runs with the fix (holds: dives 541–1,004 ms, hops 15–34 ms; swap frames at most 66.86 ms,
in `j1` while the workspace tests were building; cycles −3.0% to +1.3%). Flight outside the
cloud passed in seven: `k2` read p95 16.86 ms against the uncapped rule's 16.67, with its third
cycle's holds and swap frames slower than every other run's (hops 33–34 ms, swap frames 24–25
ms, against 23–27 and 12–14 ms), which reads as other load on the machine rather than the
change. The two runs at +64 MiB passed everything: run 1 p95 16.37 ms, run 2 (judged) 12.77 ms,
swap worst 38.84 and 28.56 ms.

**Generation is about 3–5% slower** with the chunked lattice: on the same display, the first
dive's prep took 314–317 ms (runs `k1`, `k2`) against 307 ms before (`t2`), and every
generation's rows 154–164 ms against 148–153 ms (the rows read every node through the chunk
index). One `--region-bench` run with the lattice fix (`r1`, 2560 × 1440; the display paced some
regions and not others) passed every gate: warm generation median 333 ms, max 346 ms (Task 11:
316 / 332 ms); first entries 338–2,124 ms (Task 11: 332–1,865 ms; reported only natively);
memory cycles −0.0%; all eight regions PASS. Part of that is the larger, uncapped render
competing with the generator's threads; the journey comparison above, at the same render size,
puts the lattice's share at about 5% of the rows.

### Journey route set (owner, 2026-11-25)

The Alps route flew near-repeat hops, none at a cube corner (where the generator's lattice is
largest) or over a coast. The owner asked for geographic diversity, a cube corner, a coastline,
the Grand Canyon, then "Andes and Fuji". A run (`d3b38ff`, about 7 minutes natively, 5 journeys)
flies:

| Route | Start, heading | What its hops reach (checked against the code and the 1′ data) |
|---|---|---|
| Andes | Aconcagua (32.65° S, 70.01° W; 6,040 m in the data), north | both hops over the range: 35% and 43% of their inner fields above 3,000 m (11% and 1% flying south) |
| Pangaea interior | the bench region (22° S, 18° W era frame, 250 Ma), north | deep time; its lattice is 1.5 MiB (the deep generator) |
| Grand Canyon | 33.85° N, 112.1° W, north | hop 1's neighbour centred on the canyon (36.1° N, 112.1° W). The grid reads a floor of 1,044–1,047 m and rims of 2,508 m (north) and 2,145 m (south) within 15 km: **about 1,100 m deep** (the 1′ data smooths the inner gorge; the river is at about 730 m) |
| Mt Fuji | 35.36° N, 141.48° E (the Pacific east of the Bōsō peninsula), west | hop 1 crosses the Kantō coast onto Fuji: the neighbour's inner field is 67% land (gated: 5–95%). **Fuji reads as a peak**: 3,024 m at 35.36° N, 138.73° E, 3,101–3,117 m within 10 km, a 20 km ring averaging 710 m (the 1′ data tops out at 3,164 m; the real summit is 3,776 m). Hop 2's neighbour is 104–106 km from the **cube corner** at 35.26° N, 135° E (gated: ≤ 150 km) |
| Andes again | as the first | so memory compares the Andes with itself |

The corner: the cube's faces are planes of the render frame (`crates/timefly_core/src/cube.rs`),
so the corners are the directions `(±1, ±1, ±1) / √3`; render `(x, y, z)` is geo `(y, z, x)`
(`geo.rs`), so they lie at ±35.26° latitude and ±45°, ±135° longitude. The lattices measured by
the trace (`n1`): Fuji's dive 31.0 MiB, hop 1 32.0, hop 2 (the corner hop) **35.0 MiB**; the
Andes 14.0–16.5, the Grand Canyon 17.5–20.0.

The scripted dive used to stop descending 2° short of a route's start (it dove at 44.6° N,
9.1° E for the Alps' 46.5° N, 10° E); it now steers in to within 0.25° while descending
(`SETTLE_DEG`), so the hops land where they are aimed: the dives measured within 0.3° of each
start.

Per hop, MiB over the baseline (`n1` traced; all four runs on `d3b38ff`'s code):

| Run | Andes h1, h2 | Pangaea h1, h2 | Grand Canyon h1, h2 | Mt Fuji h1, h2 (corner) | Andes again h1, h2 | Worst |
|---|---|---|---|---|---|---|
| `n1` (trace) | +30.1, +37.8 | +21.9, +22.2 | +22.0, +22.3 | +24.6, +25.4 | +24.5, +22.2 | +37.8 |
| `n2` | +32.5, +23.0 | +22.3, +22.2 | +27.1, +25.3 | +25.6, +24.9 | +21.8, +21.9 | +32.5 |
| `o1` (run 1) | +37.3, +26.4 | +23.0, +22.1 | +30.8, +23.0 | +23.9, +27.4 | +21.9, **+48.0** | **+48.0** |
| **`o2` (run 2, judged)** | +30.2, +16.4 | +14.9, +16.9 | +21.0, +26.5 | +27.1, +8.8 | +1.9, +2.4 | **+30.2** |

**The gate decision on the route set: +64 MiB stands.** `o1`'s second Andes visit read +48.0
MiB on its second hop, 8 MiB over +40. Between the two Andes visits the globe's footprint fell
110 MiB (1,043.7 to 933.3 MiB): the OS took back retained pages, so that hop paid for its pages
anew, the no-reuse case estimated above at about +44 MiB. The corner hop itself read +8.8 to
+27.4: its 35 MiB lattice reused the chunks of Fuji's dive (31 MiB) and first hop (32 MiB), as
neighbouring regions' lattices are close in size; the dive pays the jump, not the hop. With no
reuse at all at the corner the parts would add up to about +77 MiB (lattice 35, grid 10, image
data 10, staging 10, textures net 12), over even +64; that needs the OS to reclaim the pages
between the corner region's dive and its hop, which no run showed.

**Flight is judged per route, under one pacing per run** (each route's frames, as the region
bench judges each region, R14; the pacing rule is the run's, classified once from every flight
frame pooled, so a heavy route whose own median sits near the interval can't flip itself into
the lenient display-paced rule). **Pangaea's flight is reported, not gated** (owner, 2026-11-25:
"Accept it"): its line still prints with its would-be verdict, marked "reported only", and
the exit code ignores it; every other route's flight gates, and Pangaea's holds, swaps, hop
memory and route checks stay gated. The runs below were judged by the first version, which
classified each route on its own frames; re-judged under the final rules (exact: every input is
in the summary), `o1` passes (its pooled pacing was uncapped and so was each route's; only
Pangaea failed) and `o2` is unchanged (paced pooled and per route). Pooled, the route set's frames failed the uncapped p95 rule in
`n1` and `n2` (16.96 and 16.87 ms) though the routes render very differently: from `n1`'s
trace, Pangaea's median was 16.56 ms (p95 18.00), the Andes' 13.4–13.8, the Grand Canyon's 12.5,
Fuji's 8.4. Per route, `o2` passed everywhere (display-paced, medians 16.67–16.68 ms); `o1`,
uncapped, failed on **Pangaea alone: p95 18.40 ms** (median 14.16), the other routes 12.41–16.43.
Pangaea's sand seas (the dunes) are the heaviest terrain at 2560 × 1440; the region bench passes
it display-paced (p95 20.64 ms in `r1`), and Task 11's 1920 × 951 runs passed it. Whether the
display paced a run changed between runs on this 4K monitor, so an uncapped run can fail on
Pangaea. Not changed here: it's the terrain's cost, not the transitions', and the owner accepted
it (Pangaea's flight is reported only). The gate is judged under whatever pacing the run shows:
display-paced, every route's missed frames ≤ 1% and median ≤ 17.50 ms; uncapped, every route's
p95 ≤ 16.67 ms.

Other gates, `o1` / `o2`: holds (dives 404–724 ms, hops 20–35 ms, climbs 27–40 ms) and swap
frames (worst 31.98 / 36.61 ms) pass; route checks pass in all four runs; memory cycles: the
Andes against itself −10.6% (the reclaim) / +4.0%.

On the web the run becomes about 10 minutes (5 journeys at about 2 minutes each, against about
5 minutes for Task 11's 3 Alps cycles); not run here.

## Cloud pass cost (Task 5's ≤ 1 ms, Task 7's early-Z loss)

**Result: about 1.8 ms a frame at 2560 × 1440, over the 1 ms budget, so the tilt-shift was
dropped (Task 5's ruling). Dropping it didn't lower the cost: about 2.0–2.2 ms after.** The cost
is the pass itself, six gradient-noise evaluations a pixel over 3.7 Mpx; the tilt-shift only
scaled octaves that were computed anyway. **Then (2026-11-25) the owner accepted about 2 ms and
asked for the tilt-shift back, so it is restored**: the `4b79f08` row below (about 1.8 ms) is
the cloud as it now ships.

**Method.** `examples/region_tour.rs` in `time` mode with `--offscreen`, so the region camera
draws into a 2560 × 1440 image whatever the window (a window the compositor isn't showing gets
no main pass). The Alps (46.5° N, 10° E, present), 8 km above the centre, looking north at the
start pitch; 6 s settle, then 30 s timed. Three scenes, interleaved over four passes:

- `time`: the cloud off;
- `cloudtime`: the cloud held at density 0.5 throughout (the pass at its mid-transition, with the
  tilt-shift on before the change);
- `cloudtime1` (new in Task 11): held at 1.0, opaque.

The display paces every frame at 60 Hz, so frame times can't show the cost (every run: mean
16.66–16.67 ms, median 16.65–16.78 ms). Instead `ioreg -c IOAccelerator` was sampled every 0.25 s
("Device Utilization %" from `PerformanceStatistics`), as in M2's dune measurement, and averaged
over each 30 s window (about 96 samples). GPU time a frame ≈ utilisation × 16.67 ms. Nothing else
used the GPU during the runs.

| | Cloud off | Density 0.5 | Density 1.0 | Cost at 0.5 | Cost at 1.0 |
|---|---|---|---|---|---|
| With the tilt-shift (`4b79f08`), passes 1–4 | 82.0, 81.4, 82.1, 82.8 % | 92.9, 93.2, 93.2, 93.2 % | 93.0, 92.3, 93.2, 93.7 % | +11.0 points ≈ **1.84 ms** | +11.0 points ≈ **1.83 ms** |
| Without it (`e3bd6e9`), passes 1–4 | 81.2, 82.1, 82.8, 82.4 % | 93.4, 94.5, 94.9, 93.9 % | 92.9, 95.6, 96.0, 96.1 % | +12.0 points ≈ **2.01 ms** | +13.0 points ≈ **2.17 ms** |

- **Over budget either way.** The pass costs about 1.8–2.2 ms at 2560 × 1440. At the 1920 × 951
  the benches render (half the pixels) that is about 0.9–1.1 ms.
- **The tilt-shift was not the cost.** Without it every pixel adds the two fine octaves at full
  weight, so the cost rises a little (within the run-to-run spread of 1–3 points).
- **Early-Z.** Writing `frag_depth` turns off early depth rejection for this pass (Task 7). The
  number above includes that. It costs next to nothing here: outside the fly's small hole the
  cloud writes the near plane, in front of everything, so early-Z would have rejected almost no
  fragment anyway.
- **Caveats.** Utilisation is a share of busy time at whatever clock the GPU chose; near 93–96%
  busy it may understate the true time. It is an estimate, not a timestamp.
- **What it costs in play.** The cloud is hidden when clear, so this is paid only during
  transitions: the gather, the hold and the clear. Every flight and swap frame in the benches
  above stayed on 60 Hz natively. Chrome renders at a larger size (3200 × 1800 in M2's gated
  runs), where it would cost proportionally more: the Chrome journey run will tell.
- **Owner's decision (2026-11-25):** about 2 ms is acceptable. The cheaper options under
  "Summary" stay on file in case a later milestone needs the time.

## The cloud over near geometry, observed on Metal

`tests/region_gpu.rs`, `the_cloud_veils_geometry_nearer_than_its_plane` (ignored; run with
`cargo test -p timefly --test region_gpu -- --ignored` on a machine with a GPU):

- **What it draws.** `cloud.wesl` itself, with its imports stood in for (Bevy's view uniform and
  the shared `noise.wesl`), over an opaque red quad, on a headless wgpu device. It uses the
  game's pipeline state: blended, depth-tested with reverse-Z `GreaterEqual`, never writing
  depth.
- **The setup.** The cloud is at density 0.5 for a fly 10 units away, so its old plane is at
  1.2 × 10 = 12. The quad sits at 5 (nearer than the fly) and at 11 (between the fly and the
  plane, the gap the ledger flagged). The reference is the same cloud over ground at 100.
- **What it checks.** No pixel of the near quad is left sharp (pure quad colour), and every pixel
  matches the reference within one 8-bit step. Near ground is veiled exactly as far ground is.

On the M4 (Metal): far ground least green 28 (bare ground is 0); ground at 5 and at 11: 0 sharp
and 0 differing pixels of 65,536. **PASS.**

**RED, by mutation.** The shader is already right, so the test was shown to fail for the right
reason by reverting the rule: `out.depth = select(in.clip.z, …)` (the cloud's own plane instead
of the near plane). Then: "ground at 0.5 x the fly distance is drawn sharp in front of the cloud
in 65536 of 65536 pixels". The shader was restored from a copy, and the test passed again.

The Chrome WebGPU side of this check (the same shader through Tint) is the controller's, with the
Chrome runs below.

## Chrome WebGPU — pending (controller runs this)

**Not yet run.** The controller runs both benches in the owner's Chrome (an Incognito window, in
front, hands off; not the Claude app's browser pane, which throttles `requestAnimationFrame`, R23)
and fills this section in.

**The build.** `web-release` of `e3bd6e9` (Task 11's code commit), built 2026-11-25.
- `timefly.wasm`: 70,819,154 bytes, sha256 `9ddc20270b9e746b…`; wasm-bindgen output 62.16 MB.
- `grep -a -c 'journey bench summary'` finds 1. The journey bench is this branch's, so the build
  is too.
- The shaders are served from the tree, and the served `cloud.wesl` has no tilt-shift.

**To serve it** (from a folder whose `assets/` holds `data` from the main checkout and this
branch's `shaders` and `credits.txt`; `wasm-server-runner` serves `./assets`):

```sh
cargo build --profile web-release -p timefly --target wasm32-unknown-unknown
cd <root with assets/{data,shaders,credits.txt}>
wasm-server-runner <target dir>/wasm32-unknown-unknown/web-release/timefly.wasm
```

From the main checkout after the merge, `cd crates/timefly && cargo run --profile web-release
-p timefly --target wasm32-unknown-unknown` does both, as in M2. Checked with curl: `/`,
`/?region-bench`, `/?journey-bench`, `assets/data/tiles/manifest.json` (353,054 B),
`assets/data/tiles/overview.tfgrid` (12,430,497 B), `assets/data/globe/manifest.json` and
`assets/shaders/cloud.wesl` all return 200.

**What to open:**
- `http://127.0.0.1:1334/?region-bench` (about 13 minutes). Gates: R14 at 30 fps, R11
  ≤ 250 ms, generation ≤ 8,000 ms, linear memory flat across cycles.
- `http://127.0.0.1:1334/?journey-bench` (about 5 minutes). Holds are reported against 8 s,
  flight frames are gated, swap frames are reported, and linear memory must stay flat across the
  hops of a route's repeat visit (the second Andes journey; a route's first visit may grow it).
- Watch the cloud during the dives and hops: `cloud.wesl` has no GPU or naga test on WebGPU (the
  ledger's Task 7 note). Check that near ground under a gathering cloud is veiled, not sharp in
  front of it (the Chrome side of the near-geometry check above).

Copy each log from DevTools with M2's one-liner (`performance.md` of `region-m2`, "Chrome WebGPU,
real Chrome (gated)"), with `'journey bench summary'` in place of `'region bench summary'` for
the journey bench.
