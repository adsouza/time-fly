# Milestone 3 (the fly and zoom-and-swap): performance

Measured on 2026-11-24 and 25 for Task 11. The benches ran on `4b79f08`, Task 10's last commit.
The cloud cost re-measure and the web build used `e3bd6e9`, Task 11's code commit, which drops the
tilt-shift (see "Cloud pass cost").

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
| `--journey-bench` | FAIL (exit 3), hop memory only | **FAIL** (exit 3), **hop memory only**: open, owner decision pending |
| Cloud pass cost (offscreen, 2560 × 1440; budget ≤ 1 ms) | about 1.8 ms with the tilt-shift | about 2.0–2.2 ms without it: over the 1 ms budget, **accepted by the owner** (2026-11-25) |

**Cloud pass: accepted.** On 2026-11-25 the owner accepted the pass at about 2 ms, so the
noise stays as it is. Dropping the tilt-shift (Task 5's ruling) saved nothing, because the cost
is the noise itself: six evaluations a pixel. If it ever needs to be cheaper, each of these is a
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
  the noise is the cost (see "Cloud pass cost"). The owner accepted that cost on 2026-11-25.
  - The benches ran on `4b79f08`, before that change. It touches only the cloud, which is
    hidden outside transitions, and it doesn't make the pass cheaper, so they weren't re-run.
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
| Hop memory peak (≤ baseline + 40 MiB) | worst **+43.3 MiB** | worst **+48.9 MiB** | **FAIL: open, owner decision pending** |
| Journey memory cycles (≤ +10%) | 675.5, 719.9, 670.3 MiB: +6.6% | 666.1, 657.0, 657.5 MiB: −1.3% | PASS |

- **Holds.** A dive holds 0.54–0.73 s at full cover natively: the region generates in about
  0.3–0.5 s, and the first dive pays the cold tiles. A hop holds about 2 frames: the neighbour is
  preloaded well before the edge, so it is always ready when the cloud closes. The hop lift ran
  every time (about 20 km above the ground up to 60 km).
- **Swap frames.** Hops cost 32–35 ms (the terrain's new textures upload on the swap frame, by the
  plan's ruling), dives 22–29 ms, climbs 18–19 ms. All under the white-out, and a third of the
  100 ms gate.

### Hop memory: open, owner decision pending

Per hop, in MiB over the in-region baseline (the last flying frame before the hop), peak from
the preload's start until the fly is flying again:

| | Cycle 1, hop 1 | Cycle 1, hop 2 | Cycle 2, hop 1 | Cycle 2, hop 2 | Cycle 3, hop 1 | Cycle 3, hop 2 |
|---|---|---|---|---|---|---|
| Run 1 | **+43.3** | +27.7 | +31.8 | +29.5 | +23.9 | +36.3 |
| Run 2 | +30.0 | +26.7 | **+48.9** | **+42.3** | +11.0 | +3.3 |

Three of the twelve hops go over the 40 MiB gate. Every cycle flies the same route (the Alps and
much the same two neighbours east of them), so geography doesn't explain the spread.

The journey memory cycles (on the globe after each climb) read −1.3% in run 2 and +6.6% in run 1.
Run 1's cycle 2 sat 44 MiB above cycle 1 (675.5, 719.9, 670.3 MiB) and came back down in cycle 3.
Over three cycles that shows no growth trend, but it doesn't prove the memory flat.

**What the peak is made of** (code reading and arithmetic, no extra runs):

| Part | Size | When it is resident |
|---|---|---|
| The neighbour's grid (`RegionGrid`: inner 1025² × (4 B height + 4 B surface), apron 513² × 8 B) | 10.5 MB = **10.0 MiB** | from the end of the preload's generation until the swap, alongside the active grid |
| The generator's working set on the job's worker: the node lattice (`PackedNode`, 8 B a node) | about 2.3 M nodes for a recent mid-latitude region (M2's Task 4: 54 tiles) = **17.5 MiB**; up to 4.1 M nodes at a cube corner = **31.4 MiB** | during the preload's generation only; freed when the job finishes |
| The rest of the job (the coast-distance field, 513² × f32; new source tiles, 301² × 2 × f16 each, 0–2 per hop) | about **1–2 MiB** | during the preload; the tiles stay in the 96-tile cache |
| The new terrain textures at the swap (`replace_fields`: inner height `R32Float` 4.2 MB, inner surface `Rgba8Unorm` 4.2 MB, apron 1.05 MB each) | 10.5 MB = **10.0 MiB** on the GPU | from the swap frame on. They are `RENDER_WORLD` only, so the CPU copy goes once uploaded, but the upload passes through wgpu's staging memory (about the same 10 MiB) until the frame's commands complete |
| The old region's textures | **10.0 MiB** on the GPU | the old handles are replaced on the swap frame, but the asset is dropped on the next frame, the render world frees the texture after the next extract, and Metal releases it once the frames in flight finish: 2–3 frames of overlap |
| The water | none | it rebinds the terrain's new textures; it uploads nothing of its own |

The old grid itself goes on the swap frame, when `ActiveRegion` and the terrain scene both let go
of it. (During the hold the scene still holds it, as Task 8 noted, but the neighbour isn't
swapped in until the hold ends.)

So the parts that can overlap inside one peak window add up to:
- during the preload: the neighbour grid as it fills (10.0) + the lattice (17.5–31.4) + 1–2 =
  **about 29–43 MiB**;
- at the swap: the neighbour grid (10.0) + the new textures (10.0) + the staging copy (up to
  10.0) + the old textures not yet released (10.0) = **about 30–40 MiB**.

Design §8's estimate of "about 10 MiB of grid data, plus about 10 MiB of GPU textures" left out
the generator's working set and the overlap at the swap. The gate's 40 MiB sits right at the top
of these sums.

**The sums don't explain the whole peak.** The worst computed sum is about 43 MiB, and the
observed worst is 48.9 MiB. So at least 6 MiB of the worst peak is unaccounted for by the parts
above.

**Why the spread is so wide (3 to 49 MiB): a hypothesis, not measured.** `phys_footprint` counts
dirty pages that the process holds, the GPU driver's allocations included. Neither allocator hands
every free straight back:
- macOS malloc keeps freed medium and large blocks in its magazines and regions for reuse;
- Metal's driver pools freed texture and staging memory in its heaps.

So a hop's peak shows only the part of the working set the process doesn't already hold from an
earlier hop or entry. In cycle 3 of run 2, the earlier cycles' freed lattice and texture memory
was still held and was reused, and the peak read only +3.3 and +11.0 MiB. When the allocator has
just returned pages (for example after the globe's climb), the same work reads +43 to +49 MiB.
Page and heap granularity (16 KiB pages, multi-MiB heap blocks) adds a few MiB of noise either
way. The baseline is the same kind of reading, so it moves with it.

Nothing here was measured, and run 1 cuts against it: its cycle 3 read +23.9 and +36.3 MiB, not
small. Reuse may explain part of the spread; it doesn't explain it all.

**Options for the owner:**
1. **Raise the gate.** About +64 MiB would sit above every observed hop (worst +48.9 MiB) and the
   computed sum (about 43 MiB), with about 15 MiB to spare. But the peak isn't fully explained
   (at least 6 MiB unaccounted for), so the margin is a judgement, not a proof. Cheapest.
2. **Release the old region's GPU resources earlier.** For example, free the old textures on the
   swap frame (or stop the job's lattice from overlapping the swap). This cuts about 10 MiB off
   the swap's sum, but the preload's working set still dominates.
3. **Measure the peak differently.** Take the median of the hops, or leave out cycle 1, or count
   only the bytes the hop allocates (grid, lattice, textures) instead of the process footprint.
   This would show the hop's own cost without the allocator's noise. Leaving out cycle 1 alone
   wouldn't pass run 2 (cycle 2 read +48.9).
4. **Pre-upload the neighbour's textures** before the swap (the alternative to the plan's ruling
   "the terrain is uploaded at the swap frame, not before"). This moves the upload, its staging
   copy and the overlap with the old textures out of the swap frame. It also lowers the hop's
   32–35 ms swap frame, but it doesn't lower the peak by itself: the new textures would be
   resident earlier, alongside the old.
5. **Investigate first: log the grid, lattice and texture allocations against the footprint
   during one hop.** This is the way to close the unexplained part of the peak (at least 6 MiB)
   and to test the allocator-reuse hypothesis. It would also give option 1 a measured number,
   and options 2–4 a measured saving.

## Cloud pass cost (Task 5's ≤ 1 ms, Task 7's early-Z loss)

**Result: about 1.8 ms a frame at 2560 × 1440, over the 1 ms budget, so the tilt-shift was
dropped (Task 5's ruling). Dropping it didn't lower the cost: about 2.0–2.2 ms after.** The cost
is the pass itself, six gradient-noise evaluations a pixel over 3.7 Mpx; the tilt-shift only
scaled octaves that were computed anyway.

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
  hops after the first.
- Watch the cloud during the dives and hops: `cloud.wesl` has no GPU or naga test on WebGPU (the
  ledger's Task 7 note). Check that near ground under a gathering cloud is veiled, not sharp in
  front of it (the Chrome side of the near-geometry check above).

Copy each log from DevTools with M2's one-liner (`performance.md` of `region-m2`, "Chrome WebGPU,
real Chrome (gated)"), with `'journey bench summary'` in place of `'region bench summary'` for
the journey bench.
