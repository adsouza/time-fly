# Milestone 1 performance: scrub bench, load and memory

Measured by `--scrub-bench` (Task 10): after the first interactive frame and a 1 s settle, the
bench scrubs the full range in slider space, present to 540 Ma in 20 s and back in 20 s (both
ways through the 26-30 ka bridge), records every frame time and logs one block. Native runs
use `PresentMode::AutoNoVsync` and exit after the block; the web prints the same block to the
browser console as one message. The block names the bake (`bake: manifest v2, sha256 <first 16
hex digits of shasum -a 256 manifest.json>`) so runs on other machines can confirm the same
data.

**Pass rule (ruling R14).** When frames are display-paced (always in the browser; on native
when the median sits within 5% of the display's refresh interval although an uncapped present
mode was requested), a direction passes when at most 1% of its frames are missed (longer than
1.5x the target interval) and its median is at most 5% above the target interval. When an
uncapped present mode is really in effect, it passes when p95 <= the target interval. The block
prints the present mode, display refresh, which pacing was detected, the rule, the missed-frame
threshold and count on their own line, and which rule decided the result. The runs in the table
below predate the rule (they were judged by p95 alone); the re-run after it follows.

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
checked on Linux in Task 11, where `AutoNoVsync` gives uncapped frames). Under the R14 rule
(below) the same kind of run passes: 0.08-0.17% missed frames, median 16.6 ms. Headroom above 60 fps
cannot be read from a 60 Hz-paced run; resolution does not change the result (GPU is not the
limit at 2560x1440). First launches of a new binary hitch once or a few times (Metal's shader
cache and the file cache are cold); relaunches do not.

**Plate fragments write depth with early-z off** (Task 9): no measurable cost on the M4 at
2560x1440 within the 60 Hz pacing; the deep-time direction is not slower than the recent one.

### Re-run with the R14 rule (build 88fb7f8 + fix round 1)

```
present mode: AutoNoVsync requested; display refresh 60.0 Hz; median 16.60 ms -> display-paced
target: 60 fps (interval 16.67 ms)
pass rule (display-paced): missed frames (>25.0 ms) <= 1% and median <= 17.50 ms (interval +5%)
older   (present -> 540 Ma): frames 1200, median 16.58 ms, p95 19.11 ms, worst 27.62 ms, stalls (>33.3 ms = 2x interval) 0 -> PASS
  missed frames (>25.0 ms = 1.5x interval): 2 (0.17%)
younger (540 Ma -> present): frames 1201, median 16.65 ms, p95 18.41 ms, worst 26.58 ms, stalls (>33.3 ms = 2x interval) 0 -> PASS
  missed frames (>25.0 ms = 1.5x interval): 1 (0.08%)
bridge (26-30 ka, both directions): frames 22, median 16.16 ms, worst 26.04 ms, stalls 0
result: PASS (decided by the display-paced missed-frame rule)
```

(Same machine and bake, `bake: manifest v2, sha256 013f81f437f19736`, cold load 0.47 s.)

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

## Web build size (ruling R15)

`[profile.web-release]` (root `Cargo.toml`): inherits `release`, `debug = false`,
`strip = true`. Sizes of the wasm wasm-server-runner serves (after wasm-bindgen), measured
2026-11-22 from the same source:

| Build | Raw | brotli -q 11 | gzip -9 | Dev server `br` |
|---|---|---|---|---|
| `release` (line tables, `name` section) | 100.6 MB | n/a | n/a | 14.6 MB (gzip 17.3 MB) |
| `web-release`, opt-level 3 (adopted) | 65.8 MB | 7.7 MB | 13.8 MB | 11.2 MB |
| `web-release`, opt-level "s" (tried) | 45.6 MB | 6.0 MB | 9.7 MB | 8.3 MB |

Of the 100.6 MB, 30.8 MB was the wasm `name` section, which `strip = true` removes (browser
stack traces then show function indices instead of names; panic messages keep file and line).
Code is 59 MB at opt-level 3. opt-level "s" saves another 31% raw / 22% brotli, but its frame
cost could not be measured here (the browser bench needs the controller, and native runs on
this Mac are 60 Hz-paced), so the profile keeps opt-level 3. To try it in the browser bench:
`CARGO_PROFILE_WEB_RELEASE_OPT_LEVEL=s cargo run --profile web-release -p timefly --target
wasm32-unknown-unknown` from `crates/timefly`; adopt it if the R14 result and median hold.
There is no `wasm-opt` pass yet.

## Chrome WebGPU (to be run by the controller)

Not measured by the implementer (no browser). Steps, on this Mac with the same bake:

1. Build and serve with the **`web-release`** profile, from **`crates/timefly`**
   (wasm-server-runner serves `./assets` from the current directory; from the repo root every
   asset 404s and the page shows the failure overlay):
   `cd crates/timefly && PATH="/opt/homebrew/opt/rustup/bin:$PATH" cargo run --profile web-release -p timefly --target wasm32-unknown-unknown`
   (it prints the URL, `http://127.0.0.1:1334`). Checked by the implementer with curl: the page,
   `/?scrub-bench`, `assets/data/globe/manifest.json`, a frame and the shaders all return 200.
   App size: see "Web build size" below.
2. Chrome 158+, a fresh profile (`--user-data-dir=$(mktemp -d)`), `chrome://gpu` showing WebGPU
   hardware accelerated. Record the Chrome version.
3. Open DevTools (Network: "Disable cache"; Performance monitor), load the URL. The page must
   show "Loading Earth's history..." until the globe appears. The console's init log names the
   backend (`BrowserWebGpu`) and adapter.
4. Cold load x3: for each trial clear the cache (DevTools "Empty cache and hard reload" or a
   new profile) and load `http://127.0.0.1:1334/?scrub-bench`. The block's line
   `cold load (navigation to first interactive frame)` is `performance.now()` at the first
   interactive frame, i.e. seconds since navigation started (wasm download and compile
   included); the indented `app start (wasm running) to first interactive frame` line is the
   part after the wasm started. (The same values are logged at the first frame as
   `scrub bench: globe interactive`, so a trial need not wait for the whole scrub.) In the
   Network panel record transferred bytes for the app (`*.wasm`, `*.js`) and the data
   (`assets/data/globe/*`) separately, and the network conditions (localhost, no throttling).
5. Frame times: open `http://127.0.0.1:1334/?scrub-bench`. After ~42 s the console prints the
   `===== Time Fly scrub bench =====` block as one message (target 30 fps). The browser is
   always display-paced, so the result is decided by the display-paced rule (ruling R14):
   missed frames (> 50.0 ms, 1.5x the 33.3 ms interval) <= 1% in each direction and median
   <= 35.0 ms; the block prints raw median / p95 / worst too. Note console errors during the
   scrub.
6. Memory: Chrome Task Manager (Shift+Esc) with "GPU memory" and "Memory footprint" columns:
   record the tab (renderer) and GPU process at idle before loading, at the first frame and
   the peak during the scrub. The calculated texture residency is in the bench block
   (decoded textures and bridge targets for the canvas's physical size, which the block
   prints: the default 1280x720 canvas is 2560x1440 physical at devicePixelRatio 2).
7. Exercise the same visual and input checks as `docs/testing/mac-manual-checks.md` 6-10.

### Results: in-app browser pane (controller, 2026-11-22)

Run in the Claude desktop app's built-in browser pane (Chromium 156.0.8078.100, WebGPU via
`BrowserWebGpu`) on the Apple M4, with the pane visible (a hidden pane throttles rendering to
~1 Hz; an earlier run in a hidden pane was discarded). Build `3e2e7d2` (web-release), bake
`manifest v2, sha256 013f81f437f19736`, canvas 1280x720, served from `crates/timefly` on
localhost (no throttling, warm HTTP cache: this is not a cold-cache trial).

| | older (present → 540 Ma) | younger (540 Ma → present) |
|---|---|---|
| frames | 1195 | 1191 |
| median | 16.67 ms | 16.67 ms |
| p95 | 18.19 ms | 18.01 ms |
| worst | 98.01 ms | 197.15 ms |
| missed (> 50 ms) | 2 (0.17%) | 1 (0.08%) |
| stalls (> 66.7 ms) | 1 | 1 |

- Bridge (26–30 ka, both directions): 22 frames, median 16.68 ms, worst 19.88 ms, no stalls.
- Result: **PASS** (display-paced missed-frame rule, R14); the page holds 60 fps.
- Load: 0.83 s from navigation to first interactive frame (0.66 s after the wasm started),
  warm cache.
- Bridge render targets at 1280x720: 7.0 MiB persistent + 70.3 MiB while bridging
  (calculated). Process memory isn't available in the browser.
- Not yet done: three cold-cache trials, per-file transfer sizes and Chrome Task Manager memory
  in desktop Chrome (steps 2–4 and 6). The single ~100–200 ms hitch per direction is worth a
  look (likely first use of a pipeline or texture).
