# Milestone 1 performance: scrub bench, load and memory

Measured by `--scrub-bench` (Task 10): after the first interactive frame and a 1 s settle, the
bench scrubs the full range in slider space, present to 540 Ma in 20 s and back in 20 s (both
ways through the 26-30 ka bridge), records every frame time and logs one block. Native runs
use `PresentMode::AutoNoVsync` and exit after the block; the web prints the same block to the
browser console.

```sh
PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo run --release -p timefly -- --scrub-bench --window 2560x1440
# web: open the wasm-server-runner URL with ?scrub-bench (see "Chrome WebGPU" below)
```

## Native Metal, Apple M4 (2026-11-22)

- **Machine:** Apple M4, macOS 27.2.1 (26B131), Metal backend (adapter log: `Apple M4`,
  `Metal`), 4K 60 Hz display.
- **Build:** `0.1.0 (c249528, release, aarch64-apple-darwin; system-spans)`; the span-less
  variant is the same source built with `--no-default-features` (its SHA reads 0faa344, a
  docs-only commit on top).
- **Render resolution:** 2560x1440 physical (`--window 2560x1440`), plus one 1280x720 run.
- **Data revision:** manifest version 2, 137 frames (28 ICE-6G_C incl. the reused 0 ka slice,
  109 PaleoDEM), 136 images, cube size 128; 17,177,338 bytes on disk without `bake.log`
  (`manifest.json` SHA-256 prefix `013f81f437f19736`).

### Frame times (20 s full-range scrub each way)

| Run | Spans | Direction | Frames | Median | p95 | Worst | > 33.3 ms |
|---|---|---|---|---|---|---|---|
| spans2 (warm) | on | older | 1199 | 16.63 ms | 18.33 ms | 27.17 ms | 0 |
| spans2 (warm) | on | younger | 1201 | 16.61 ms | 19.07 ms | 21.53 ms | 0 |
| nospans2 (warm) | off | older | 1199 | 16.61 ms | 19.05 ms | 24.43 ms | 0 |
| nospans2 (warm) | off | younger | 1201 | 16.64 ms | 18.66 ms | 23.90 ms | 0 |
| spans1 (first launch of a new build) | on | older | 1198 | 16.56 ms | 18.31 ms | 83.18 ms | 1 (bridge) |
| spans1 | on | younger | 1201 | 16.55 ms | 18.68 ms | 22.65 ms | 0 |
| nospans1 (first launch of a new build) | off | older | 1198 | 16.69 ms | 19.05 ms | 62.29 ms | 3 |
| nospans1 | off | younger | 1201 | 16.67 ms | 18.88 ms | 21.48 ms | 0 |
| small, 1280x720 (warm) | on | older | 1199 | 16.51 ms | 19.07 ms | 27.42 ms | 0 |
| small, 1280x720 (warm) | on | younger | 1201 | 16.57 ms | 18.83 ms | 22.35 ms | 0 |

Bridge frames (26-30 ka, both directions together): 20-22 per run, median 16.2-16.8 ms, worst
21.5-27.9 ms warm; the first launch of a new build had one 83 ms (spans1) frame on the first
bridge entry, and nospans1 three > 33 ms frames in the older direction.

**Reading these numbers.** Presentation on this Mac is paced at the display's 60 Hz even with
`AutoNoVsync` (windowed Metal goes through the compositor), so the frame deltas sit on
16.67 ms with about +-2 ms of pacing jitter, at both 2560x1440 and 1280x720. The p95 of
18.3-19.1 ms is that jitter, not dropped frames (no warm frame above 27.4 ms, none above
33.3 ms). The bench's strict rule (p95 <= 16.67 ms) therefore reports FAIL here although the
globe keeps up with the display; the M4 numbers are informational (the 60 fps desktop target is
checked on Linux in Task 11, where `AutoNoVsync` gives uncapped frames). Headroom above 60 fps
cannot be read from a 60 Hz-paced run; resolution does not change the result (GPU is not the
limit at 2560x1440). First launches of a new binary hitch once or a few times (Metal's shader
cache and the file cache are cold); relaunches do not.

**Plate fragments write depth with early-z off** (Task 9): no measurable cost on the M4 at
2560x1440 within the 60 Hz pacing; the deep-time direction is not slower than the recent one.

### System spans overhead

`system-spans` (`bevy/trace` + `bevy/debug`) on vs off, warm runs: p95 18.33/19.07 ms vs
19.05/18.66 ms, the same within jitter (well under the 5% threshold), so it stays a default
feature. The CPU time of the whole 41.5 s run (`/usr/bin/time -l`) shows the real cost: user
14.25-14.57 s with spans vs 13.76-14.01 s without (about +3-4% user CPU, +2% user+sys).

### Cold load

From launch (diagnostics start) to the first interactive frame:

| Trial | Build | Load |
|---|---|---|
| 1 | first launch after building (spans) | 3.95 s |
| 2 | first launch after rebuilding (spans) | 4.30 s |
| 3 | first launch after building (no spans) | 0.53 s |
| relaunches | spans / no spans / 1280x720 | 0.40 s / 0.39 s / 0.39 s |

Trials 1 and 2 ran right after a build finished: the data load alone took ~1 s (vs 0.08 s
otherwise) and the first frames were slow, consistent with a cold file cache, Metal compiling
shaders with an empty cache, and macOS scanning a new binary on first launch. The page cache
could not be purged without `sudo`, so the three trials are "first launch of a new binary".

### Memory

| Measure | 2560x1440 | 1280x720 |
|---|---|---|
| Decoded frame textures (calculated: 27 `Rgba16Float` + 109 `Rg16Float` cube maps at 128) | 61.1 MiB | 61.1 MiB |
| Bridge targets, persistent (2 x `Rgba8UnormSrgb`, calculated) | 28.1 MiB | 7.0 MiB |
| Bridge cameras' MSAA colour + depth + main textures while active (2 x 40 B/px, calculated) | 281.2 MiB | 70.3 MiB |
| Process `phys_footprint` at first interactive frame (includes GPU memory on Apple silicon) | 1011-1072 MiB | 658 MiB |
| Peak `phys_footprint` (bench / `time -l`) | 1012-1078 / 1061-1120 MB | 663 MiB |
| Max resident set (`time -l`, CPU-side pages) | 395-507 MB | 439 MB |

The footprint does not rise when the scrub enters the bridge (within 3 MiB) and does not fall
outside it: the bridge cameras render during GPU warm-up, so their textures are allocated
before the first interactive frame and stay resident. The resolution-dependent part
(1011-1072 vs 658 MiB for 2.76 Mpx more) is about 130 B/px, which matches the calculated
140 B/px: window surface (3 x 4 B), the globe camera's 4x MSAA colour + depth + main textures
(40 B/px), the persistent bridge targets (8 B/px) and the bridge cameras' textures
(80 B/px). So at 2560x1440 the bridge costs about 309 MiB of the ~1 GiB footprint. Options if
this is too much for the web: render the bridge cameras without MSAA (saves 2 x 32 B/px), at
reduced resolution, or allocate them only on bridge entry and release them after.

How it was measured: `proc_pid_rusage(RUSAGE_INFO_V4)` sampled each frame by the bench
(`ri_phys_footprint`, `ri_lifetime_max_phys_footprint`) and `/usr/bin/time -l` around the run.

## Chrome WebGPU (to be run by the controller)

Not measured by the implementer (no browser). Steps, on this Mac with the same bake:

1. From **`crates/timefly`** (wasm-server-runner serves `./assets` from the current directory;
   from the repo root every asset 404s and the page shows the failure overlay):
   `cd crates/timefly && PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo run --release -p timefly --target wasm32-unknown-unknown`
   (it prints the URL, `http://127.0.0.1:1334`). Checked by the implementer with curl: the page,
   `/?scrub-bench`, `assets/data/globe/manifest.json`, a frame and the shaders all return 200.
   The release wasm is 100.6 MB uncompressed (the release profile keeps line tables and there
   is no `wasm-opt` pass); the dev server sends it as 17.3 MB gzip / 14.6 MB brotli.
2. Chrome 158+, a fresh profile (`--user-data-dir=$(mktemp -d)`), `chrome://gpu` showing WebGPU
   hardware accelerated. Record the Chrome version.
3. Open DevTools (Network: "Disable cache"; Performance monitor), load the URL. The page must
   show "Loading Earth's history..." until the globe appears. The console's init log names the
   backend (`BrowserWebGpu`) and adapter.
4. Cold load x3: for each trial clear the cache (DevTools "Empty cache and hard reload" or a
   new profile), reload, and read "first interactive globe frame" against navigation start
   (DevTools Performance or the bench block's cold-load line). In the Network panel record
   transferred bytes for the app (`*.wasm`, `*.js`) and the data (`assets/data/globe/*`)
   separately, and the network conditions (localhost, no throttling).
5. Frame times: open `http://127.0.0.1:1334/?scrub-bench`. After ~42 s the console prints the
   `===== Time Fly scrub bench =====` block (target 30 fps, p95 <= 33.3 ms). Note console
   errors during the scrub.
6. Memory: Chrome Task Manager (Shift+Esc) with "GPU memory" and "Memory footprint" columns:
   record the tab (renderer) and GPU process at idle before loading, at the first frame and
   the peak during the scrub. The calculated texture residency is in the bench block
   (decoded textures and bridge targets for the canvas's physical size, which the block
   prints: the default 1280x720 canvas is 2560x1440 physical at devicePixelRatio 2).
7. Exercise the same visual and input checks as `docs/testing/mac-manual-checks.md` 6-10.
