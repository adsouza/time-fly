# Rivers and Lakes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** rivers and lakes in every region world and every era: a continental water pass per era, refined by each region into carved valleys and true-width water that flows downstream, with white water and boulders in rapids, and lakes at their own levels.

**Architecture:** a new pure module, `timefly_core::hydrology`, routes water over a global cube lattice per era (priority-flood, steepest descent, discharge = upstream area × climate runoff, a water balance per lake) and returns an `EraHydrology`: a river graph, lakes and per-node ocean flags. The shell's `hydro` module runs the pass once an era settles on the globe (an async task natively, time slices on the web), caches it, and makes region jobs wait for it, so a dive holds in the cloud until it is ready. The region generator refines the era's rivers on its own coarse terrain (`worldgen::generate::rivers`), carves channel and valley, and writes a water level and a river texel per texel, all pure functions of direction plus the pass. The water material draws them, and a reusable instanced scatter draws the boulders. A bake step reads past climate from Beyer et al. 2020 by HTTP range requests. An offline reference pass at 1′ over the Indus and the Amazon checks the game's rivers.

**Tech Stack:** Rust, Bevy 0.20.1, WESL, `timefly_core` (pure, f64 geography), wasm32 WebGPU and native Metal and Vulkan. Python 3.12 bake tool run with uv: numpy, netCDF4, and h5py (new).

**Spec:** `docs/superpowers/specs/2026-11-25-rivers-and-lakes-design.md` (the "design"). Its parent is `docs/superpowers/specs/2026-11-21-time-fly-design.md` (§5, §7.5, §8). The M3 plan, `docs/superpowers/plans/2026-11-24-milestone-3-zoom-and-swap.md`, holds the gates this plan keeps, and its rulings still apply.

## Global Constraints

- Bevy 0.20.1 and WESL. Stay within the WebGPU ceiling: no mesh shaders, no `MeshletMesh`, no `std::thread` in core loops, no 64-bit atomics. The water material binds no more textures than M3's (Task 8 replaces its two height textures with two water textures).
- f32 inside worlds, f64 on the CPU for geography. Lattice directions, river geometry and path distances are f64. Texel outputs are f32, or f16 bits where stated.
- Functional core, imperative shell. The pass and the refinement are pure functions in `timefly_core`, tested without a window. The shell loads, schedules, caches and uploads.
- Domain events are Bevy messages registered with `add_domain_event`. States use `add_logged_state`.
- **Determinism (design §4.3).** Every refined quantity is a function of the era-frame direction, the era's `EraHydrology` and the field's source level: carved height, water level, river texel and boulders. Inner fields agree with inner fields bit for bit. An apron agrees with an apron.
- **Region grid:** `REGION_RES` = 1025 inner texels over `REGION_SIZE_M` = 300,000 m, and `APRON_RES` = 513 over `APRON_SIZE_M` = 2,400,000 m.
- **Web budgets:**
  - `WEB_FLIGHT_GEN_BUDGET_MS` = 3.0, unchanged.
  - `WEB_GEN_BUDGET_MS` = 6.0 and `WEB_GEN_LARGE_BUDGET_MS` = 12.0, unchanged.
  - New: `WEB_HYDRO_BUDGET_MS` = 6.0 for the pass on the globe. The pass takes `WEB_GEN_LARGE_BUDGET_MS` while `LargeGenBudget` is true, under an opaque cloud.
- **Generation times:** `EXPECTED_GEN_S_WEB` = 8.0 and `EXPECTED_GEN_S_NATIVE` = 3.0 bound a region's own generation. The wait for the era's pass is excluded and reported separately.
- **Pass targets:** native ≤ 1 s and web ≤ 5 s per era. Both are reported, not gated (design §4.1).
- **M3 gates, unchanged:**
  - Native hop memory: peak ≤ the in-region baseline + `HOP_MEMORY_LIMIT_MIB` (64).
  - The region bench's gates: R14 flight frame rate, the first-region worst frame ≤ 250 ms on the web, warm generation ≤ 3,000 ms native and 8,000 ms web, first-entry generation ≤ 8,000 ms on the web, memory cycles within 10%.
  - The journey bench's gates: holds ≤ 3,000 ms natively, the swap worst frame ≤ 100 ms, journey memory cycles within 10%, and M3's web per-route growth rule for hop memory.
- **Data:**
  - Downloads need the owner's go-ahead. Task 1's network bake is a controller step.
  - The only new shipped data is the climate grids, ≤ 5 MB in all.
  - Reference-pass outputs are never shipped. They are committed test fixtures, ≤ 1 MB in all.
- **Hydrology constants**, all `pub const` in `timefly_core::hydrology::params` *(my call)*:
  - Lattice: `HYDRO_RECENT_N = 900` (mean spacing 10.2 km), `HYDRO_DEEP_N = 180` (51 km), `PRESENT_FRAME_MAX_MYR = 0.2`.
  - Routing: `OCEAN_STRAIT_M = 25_000.0`, `DEPRESSION_EPS_M = 0.01`, `ICE_NODE_FRACTION = 0.5`.
  - Data lakes: `DATA_LAKE_FLAT_TOL_M = 0.5`, `DATA_LAKE_MIN_NODES = 2`, `DATA_LAKE_MIN_BALANCE = 0.25`, `DATA_LAKE_MAX_DEPTH_M = 50.0`, `DATA_LAKE_DEPTH_RAMP_NODES = 2.0`.
  - Rivers: `RIVER_MIN_DISCHARGE_M3S = 20.0`, `APRON_RIVER_MIN_DISCHARGE_M3S = 200.0`, `CLIMATE_MAX_KA = 120.0`, `LARGE_LAKE_KM2 = 1_000.0`.
  - Hydraulic geometry: width `w = 5.0 · Q^0.5`, depth `d = 0.27 · Q^0.39` (m, with Q in m³/s), `MANNING_N = 0.035`.
  - Speeds: lowland `[0.3, 3.0]` m/s, rapids `3.0 + 3.0 · rapid`. `RAPID_SLOPE = [0.008, 0.03]` (smoothstep gives `rapid`).
  - Relaxation: `CORRIDOR_FRACTION = 0.4` of the lattice's mean spacing, `CORRIDOR_SAMPLES = 13` across the line, `NODE_SEARCH_GRID = 7` (7 × 7 around a node).
  - Sampling: `SUBPOINT_M = 500.0` (inner). The apron uses `max(SUBPOINT_M, apron texel / 2)`.
  - Incision: `INCISION_WINDOW_M = 30_000.0`, `LEVEL_BELOW_BED = 0.2` (× depth).
  - Meanders: `MEANDER_WAVELENGTH_PER_WIDTH = 11.0`, `MEANDER_AMPLITUDE_PER_WIDTH = 1.5`, `MEANDER_FLAT_SLOPE = [5.0e-4, 5.0e-3]`, `MEANDER_MIN_WAVELENGTH_M = 2_000.0`.
  - Valley: `VALLEY_HALF_WIDTH_PER_WIDTH = 4.0`, `VALLEY_WALL_SLOPE = 0.3`, `BANK_FREEBOARD_M = 0.5`, `CHANNEL_MIN_TEXELS = 2.0`.
  - Encoding: `NO_WATER_DROP_M = 64.0`, `RIBBON_NONE_M = 1_000.0`.
  - Boulders: `BOULDERS_PER_SUBPOINT_MAX = 6`, `BOULDER_RADIUS_M = [0.4, 3.0]`, `MAX_BOULDERS = 16_384`.
  - Shell: `HYDRO_CACHE_ERAS = 2`, `HYDRO_SETTLE_S = 0.5`.
- **Toolchain:** `PATH="/opt/homebrew/opt/rustup/bin:$PATH"` and `CARGO_TARGET_DIR=/Users/adsouza/Code/time-fly/target`, which is shared with other worktrees.
  - Before any build whose artefact you test, run `touch crates/timefly/src/lib.rs crates/timefly_core/src/lib.rs`.
  - Prove a web or windowed artefact is this branch's with `grep -a -c '<a string only this branch has>'`.
  - The bake tool runs as `cd tools/bake && uv run …`.
- **Assets for runs:** the globe, tile and climate data live only in the main checkout, `/Users/adsouza/Code/time-fly/crates/timefly/assets/data`. Benches and tours use `BEVY_ASSET_ROOT="$ROOT"`, where `$ROOT/assets/` symlinks `data` to that folder and `shaders` and `credits.txt` to this worktree's.
- **Repo rules:**
  - Stage explicit paths. Never `git add -A`.
  - Never push or merge.
  - Never touch `.envrc`.
  - Commits use the conventional style, author `Antonio D'souza <275832+adsouza@users.noreply.github.com>`, and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Windowed runs:**
  - Use a fresh `TIMEFLY_LOG_DIR="$(mktemp -d)"` per run.
  - Never kill the windowed app with a timeout.
  - Measure first-run worst frames twice, because of the Metal shader cache.
  - Never use xctrace. Read GPU cost with ioreg, as M3 did.
- **Per-task gate:** `cargo test -q --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo check -p timefly --target wasm32-unknown-unknown`. Tasks that touch the bake tool also run `cd tools/bake && uv run pytest -q`.

## Review Focus

1. **Climate data missing or corrupt** (an older bake, a failed fetch on the web): the pass falls back to the zonal climate with one `warn`, the region still gets rivers, and the dive holds no longer than the pass takes. Task 4, `missing_climate_falls_back_to_the_zonal_rule`.
2. **The slider dragged through many eras, or `[` and `]` mashed:** one pass runs, for the settled era. A prefetch never delays a region's own pass. The cache never holds more than `HYDRO_CACHE_ERAS`. Task 4, `dragging_the_slider_runs_one_pass_for_the_settled_era` and `a_region_request_preempts_a_prefetch`.
3. **Cube-face seams and corners:** routing crosses face edges, and two regions that overlap at a cube corner agree bit for bit, rivers included. Task 2, `routing_crosses_cube_face_seams`. Task 6, `rivers_agree_bit_for_bit_at_a_cube_corner`.
4. **Water above and land below sea level:** the fly keeps its clearance over a lake's surface (Titicaca at 3,812 m), and may descend into a dry basin below sea level. Task 7, `the_fly_stays_above_a_lake_surface` and `the_fly_descends_into_a_dry_basin_below_sea_level`.
5. **Narrow straits and basins near the coast:** at the 10 km lattice the Black Sea and the Baltic stay sea, while the Caspian and the Qattara Depression do not. Task 2, `a_sea_behind_a_narrow_strait_stays_ocean` and `a_basin_beyond_the_strait_distance_is_not_ocean`.

---

### Task 1: Past climate from Beyer et al. 2020 (bake tool)

**Files:**
- Create:
  - `tools/bake/timefly_bake/beyer.py`
  - `tools/bake/tests/test_beyer.py`
  - `tools/bake/scripts/write_climate_fixture.py`
  - `crates/timefly/tests/assets/data/climate/` (the tiny fixture the script writes: ages 0, 10, 21 and 26 ka; wet tropics, a dry band at 15–30°)
- Modify:
  - `tools/bake/pyproject.toml` (add `h5py`) and `tools/bake/uv.lock`
  - `tools/bake/timefly_bake/cli.py` (a `timefly-bake climate` subcommand)
  - `data/sources.json` (a new top-level `range_read` list)
  - `data/README.md` (a table row and a "Past climate" section)
  - `crates/timefly/assets/credits.txt`
  - `crates/timefly/src/ui/credits.rs` (one more needle in its test)

**Interfaces:**
- Produces, as the shipped layout (`assets/data/climate/`):
  - `manifest.json`: `{"version": 1, "grid": {"deg": 1.0, "rows": 180, "cols": 360, "channels": ["bio1_c", "bio12_mm"]}, "ages_ka": […], "files": [{"age_ka": 8.0, "file": "bio_8ka.<sha256>.tfgrid", "bytes": n, "sha256": "…"}], "source": {"figshare_file": 22659026, "variables_sha256": {"BIO1": "…", "BIO12": "…"}}, "attribution": "…"}`.
  - One `.tfgrid` per age: `LatLon`, F16, shape (180, 360, 2). Row 0 is centred on 89.5° N and column 0 on 179.5° W. It is NaN where Beyer has no land value. Channel 0 is BIO1 (°C), channel 1 is BIO12 (mm/yr).
  - Baked ages, in ka: every Beyer age up to 26 (0–21 in steps of 1, then 22, 24, 26), then 30, 40, …, 120. That is 35 grids.
- Produces, in Python:
  - `RangeFile(url, size, block=1 << 20, cap_bytes=MAX_FETCH_BYTES, opener=None)`, an `io.RawIOBase`:
    - Each block is a fresh `GET url` with a `Range` header, following one redirect, which must go to https or to loopback in tests. It never reuses a signed URL.
    - It checks the `Content-Range` total against `size`.
    - It counts `fetched` and `requests`, keeps an LRU cache of at most 64 blocks in memory, and writes nothing to disk.
  - `MAX_FETCH_BYTES = 300_000_000`. Passing it raises `FetchCapError`.
  - `figshare_file_meta(article, file_id, opener=None) -> dict`: `size` and `computed_md5` from `https://api.figshare.com/v2/articles/{article}/files`.
  - `read_bio(h5, ages_ka) -> (ages_ka, bio1, bio12)` at 0.5°, north first:
    - It finds `time`, `latitude`/`lat` and `longitude`/`lon` case-insensitively, and fails by naming what it found.
    - `age_ka = −time / 1000`.
  - `regrid_to_1deg(a) -> ndarray`: nanmean of 2 × 2 blocks; NaN only when all four are NaN.
  - `bake_climate(entry, out, opener=None, record_pins=False) -> dict`: writes the grids under content-hash names, then `manifest.json` last, and returns the manifest.
- Produces, in `data/sources.json`: `"range_read": [{"name": "beyer2020", "url": "https://ndownloader.figshare.com/files/22659026", "figshare_article": 12293345, "figshare_file": 22659026, "file": "LateQuaternary_Environment.nc", "bytes": 1333236636, "md5": "d23abb7ce8671433ea36a2cae814ebab", "variables": {"BIO1": {"sha256": "…"}, "BIO12": {"sha256": "…"}}, "licence": "CC BY 4.0 (Beyer, Krapp & Manica 2020, doi:10.1038/s41597-020-0552-1)"}]`.
  - `load_sources` reads only `files`, so `timefly-fetch` never downloads this entry.
  - The variable pins are written by the controller's first run (Step 5).

- [ ] **Step 1: Write the failing tests** in `tools/bake/tests/test_beyer.py`. They run against a local HTTP server that serves a synthetic h5py file with Beyer's layout: four variables of shape (72, 300, 720), chunks (24, 100, 240), gzip. The ndownloader path redirects (302) to `/signed?token=N`, and each token is good for one request.
  - `test_range_reader_follows_a_fresh_redirect_per_block`: the tokens issued equal `requests`, and none is reused.
  - `test_range_reader_fetches_only_what_h5py_reads`: reading BIO1 and BIO12 fetches less than 60% of the file.
  - `test_reader_refuses_past_the_fetch_cap`: with `cap_bytes=1 << 20`, it raises `FetchCapError`.
  - `test_reader_checks_the_total_size`: a `Content-Range` total ≠ `size` raises, naming both.
  - `test_regrid_takes_the_nanmean_of_each_block`: [[1, NaN], [3, 5]] gives 3.0, and all-NaN gives NaN.
  - `test_bake_writes_one_grid_per_selected_age`:
    - 35 files from the synthetic 72 ages, each (180, 360, 2) F16.
    - Row 0's latitude is 89.5.
    - The manifest's sizes and SHA-256 hashes match the files.
  - `test_extracted_arrays_are_pinned`: a wrong pin fails, naming `BIO12`. `record_pins=True` returns the hashes.
  - `test_md5_and_size_come_from_the_figshare_api`: a served API JSON with the wrong md5 fails.
  - `test_repo_sources_pin_beyer`: the `range_read` entry carries the exact ids, the size 1,333,236,636 B and the md5 `d23abb7ce8671433ea36a2cae814ebab`, and `load_sources` ignores it.
- [ ] **Step 2: Run them to check they fail.** Run `cd tools/bake && uv run pytest -q tests/test_beyer.py`. Expected: they fail on `ModuleNotFoundError: timefly_bake.beyer`.
- [ ] **Step 3: Implement** `beyer.py` and the `climate` subcommand (`--sources`, `--out`, `--record-pins`). Then:
  - Add `h5py` to `pyproject.toml` with `uv add h5py`.
  - Write the fixture with `uv run python scripts/write_climate_fixture.py`. It goes through `bake_climate` on a synthetic file, with no network.
  - Add the citation to `credits.txt` and `data/README.md`: Beyer, R.M., Krapp, M. & Manica, A. (2020). An empirical evaluation of bioclimatic and vegetation reconstructions for the last 120,000 years. *Scientific Data* 7, 236. figshare doi:10.6084/m9.figshare.12293345. CC BY 4.0. Changes: BIO1 and BIO12 regridded to 1°.
- [ ] **Step 4: Run the tests to check they pass.** Run `uv run pytest -q` and `cargo test -p timefly --lib ui::credits`. Expected: all pass.
- [ ] **Step 5 (controller, with the owner's go-ahead): the network bake.**
  - Run `cd tools/bake && uv run timefly-bake climate --sources ../../data/sources.json --out /Users/adsouza/Code/time-fly/crates/timefly/assets/data/climate --record-pins`.
  - Expected: the log's fetch total is ≤ 60 MB, there are 35 grids, the folder holds ≤ 5 MB, and the two pins are printed.
  - Paste the pins into `sources.json` and re-run without `--record-pins`. Expected: it verifies and changes no file.
  - Confirm Beyer's coordinate names, units (°C and mm/yr) and latitude order against the log, and ledger any difference.
- [ ] **Step 6: Run the gate and commit** `feat(bake): past climate from Beyer et al. 2020 by range requests`. Stage `tools/bake/timefly_bake/beyer.py tools/bake/timefly_bake/cli.py tools/bake/tests/test_beyer.py tools/bake/scripts/write_climate_fixture.py tools/bake/pyproject.toml tools/bake/uv.lock data/sources.json data/README.md crates/timefly/assets/credits.txt crates/timefly/src/ui/credits.rs crates/timefly/tests/assets/data/climate`.

### Task 2: The continental lattice and routing (core)

**Files:**
- Create:
  - `crates/timefly_core/src/hydrology/mod.rs`
  - `hydrology/params.rs` (the constants in Global Constraints)
  - `hydrology/lattice.rs`
  - `hydrology/route.rs`
  - `crates/timefly_core/tests/fixtures/hydro_golden.json`
- Modify: `crates/timefly_core/src/lib.rs` (add `pub mod hydrology;`)
- Test: inline modules in each file

**Interfaces:**
- Produces:
  - `pub trait HydroGraph: Sync { fn len(&self) -> u32; fn neighbours(&self, i: u32) -> [Option<u32>; 8]; fn distance_m(&self, i: u32, j: u32) -> f64; fn area_m2(&self, i: u32) -> f64; }`
    - Neighbour order: E, NE, N, NW, W, SW, S, SE. The Python reference (Task 11) uses the same order.
  - `CubeLattice { pub n: u32 }`:
    - `node(face, col, row) = face·n² + row·n + col` (faces in `CubeFace::ALL` order), and `face_col_row(id)`.
    - `dir(id) -> RenderDir` is `texel_center_dir`.
    - `mean_spacing_m()` is `√(4πR² / 6n²)`.
    - `taps(dir) -> [(u32, f64); 4]` follows `bilinear_taps`, and `nearest(dir) -> u32` follows `dir_to_texel`.
    - `area_m2` is the texel's exact solid angle × R².
    - A neighbour is a ±1 step in face coordinates. Off the face, it is the stepped texel centre's direction through `dir_to_texel`. Duplicates at cube corners become `None`.
  - `GridGraph { rows: u32, cols: u32, spacing_m: f64 }`: row 0 is north, and `id = row·cols + col`. It is used for the golden fixture and the property tests.
  - `Routing`, a resumable router built by `Routing::new(elevation_m: Vec<f32>)`:
    - `step(&mut self, g: &impl HydroGraph, budget: u32) -> bool`: one unit is a node visit or a heap pop.
    - `finish(self) -> Routed`.
  - `Routed { ocean: Vec<bool>, filled_m: Vec<f32>, receiver: Vec<u8> (neighbour index, 8 for ocean), order: Vec<u32> (non-ocean nodes in pop order), basins: Vec<Basin> }`
  - `Basin { nodes: Vec<u32> (sorted), spill_m: f32, exit: u32, rim: u32 }`
  - `pub fn receiver_of(g: &impl HydroGraph, routed: &Routed, i: u32) -> Option<u32>`

The rules, which the tests and the golden fixture pin:

```text
ocean: nodes with e < 0, by 8-connected component. Start from the largest component (ties: lowest
  node id). Repeat until nothing changes: dilate the ocean by ceil(OCEAN_STRAIT_M / spacing) graph
  steps through any node, and add every below-zero component the dilation touches. Dilated land
  stays land.
flood: push every ocean node with a non-ocean neighbour, in ascending id, keyed (e, seq). Pop the
  minimum (filled, seq); for each unvisited non-ocean neighbour n, in neighbour order:
  filled[n] = max(e[n], filled[c]), parent[n] = c, push (filled[n], seq++). seq is a running u64.
  f32 keys compare by total order. With no ocean at all, seed the lowest node (ties: lowest id).
receiver[i]: the neighbour with filled strictly lower and the largest (filled[i] − filled[j]) /
  distance (ties: first in neighbour order); with none strictly lower, parent[i].
basins: 8-connected components of nodes with filled > e + DEPRESSION_EPS_M. exit = the basin node
  popped first; rim = its parent. Every other basin node whose receiver leaves the basin is
  redirected to exit. So pop order is a topological order (receiver before node).
```

- [ ] **Step 1: Write the fixture and the failing tests.**
  - `hydro_golden.json` is a 4 × 5 `GridGraph` at 1,000 m. Column 0 is ocean at −10 m. The fixture holds the input, `order`, `receiver` as (row, col), and `q` for unit runoff on every land node:

    | row | col 0 | col 1 | col 2 | col 3 | col 4 |
    |---|---|---|---|---|---|
    | 0 | −10 | 5 | 5 | 9 | 9 |
    | 1 | −10 | 3 | 1 | 2 | 9 |
    | 2 | −10 | 4 | 2 | 2 | 9 |
    | 3 | −10 | 6 | 6 | 9 | 9 |

  - Expected `order`: (1,1), (1,2), (2,2), (1,3), (2,3), (2,1), (0,1), (0,2), (3,1), (3,2), (0,3), (3,3), (1,4), (0,4), (2,4), (3,4).
  - Expected receivers:
    - (0,1)→(0,0), (0,2)→(1,2), (0,3)→(1,3), (0,4)→(1,3)
    - (1,1)→(1,0), (1,2)→(1,1), (1,3)→(1,2), (1,4)→(1,3)
    - (2,1)→(2,0), (2,2)→(1,2) (redirected), (2,3)→(1,2), (2,4)→(2,3)
    - (3,1)→(3,0), (3,2)→(2,2), (3,3)→(2,3), (3,4)→(2,3)
  - Expected basin: {(1,2), (1,3), (2,2), (2,3)}, spill 3 m, exit (1,2), rim (1,1).
  - Expected `q` with no evaporation (case A): (1,1) 13, (1,2) 12, (1,3) 4, (2,3) 4, (2,2) 2, and 1 elsewhere. Task 3 checks `q` and adds the lake cases B and C; routing does not accumulate.
  - Tests, inline:
    - `golden_grid_matches_the_fixture`: order, receivers, filled and basin exactly.
    - `cube_lattice_neighbours_are_symmetric_and_cross_seams` (n = 8): j ∈ N(i) ⇔ i ∈ N(j); 8 distinct neighbours, or 7 at the 24 corner texels; distances within [0.5, 1.6] × the mean spacing.
    - `texel_areas_sum_to_the_sphere`: relative error < 1e-9.
    - `no_node_drains_uphill_on_the_filled_surface` (proptest, random 12 × 12 DEMs): `filled[receiver] ≤ filled[i]`, every node reaches the ocean in fewer than `len` steps, and the run is the same for budgets 1, 7 and `u32::MAX`.
    - `routing_crosses_cube_face_seams` (Review Focus 3): on n = 16, a ramp falling from the centre of +X across its edge into an ocean on +Z. The path from +X's centre crosses the seam and ends in the ocean.
    - `a_sea_behind_a_narrow_strait_stays_ocean` (Review Focus 5): on a `GridGraph` at 10 km, a −50 m basin behind a 1-node land strip is ocean.
    - `a_basin_beyond_the_strait_distance_is_not_ocean`: the same basin 4 nodes inland is not ocean, and it is filled to its spill.
    - `flats_drain_in_flood_order`: a plateau with two outlets gives the same receivers on every run, and each drains to the outlet the flood reached first.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly_core hydrology::`. Expected: they fail to compile, because `hydrology` doesn't exist yet.
- [ ] **Step 3: Implement** `lattice.rs` and `route.rs` as specified. Use a `BinaryHeap` of `Reverse((ordered f32 bits, seq, node))`. Ocean dilation is a BFS bounded per step. Receivers and parents are stored as neighbour indices (`u8`).
- [ ] **Step 4: Run the tests to check they pass.** Run `cargo test -p timefly_core hydrology::`. Expected: all pass.
- [ ] **Step 5: Run the gate and commit** `feat(core): continental lattice and priority-flood routing`. Stage `crates/timefly_core/src/lib.rs crates/timefly_core/src/hydrology crates/timefly_core/tests/fixtures/hydro_golden.json`.

### Task 3: Climate, lakes and the river graph (core)

**Files:**
- Create:
  - `crates/timefly_core/src/hydrology/climate.rs`, `lakes.rs`, `graph.rs`, `pass.rs`
  - `data/hydrology/climate_table.json` (the owner's tuning table; Task 4's shell and Task 11's Python read it)
- Modify:
  - `hydrology/mod.rs`
  - `worldgen/coarse/deep.rs`: `pub fn reference_sample(&self, era_dir: RenderDir) -> CoarseSample`, which is `sample_with(.., SourceLevel::Overview)`
  - `crates/timefly_core/tests/fixtures/hydro_golden.json` (lake cases B and C)
- Test: inline

**Interfaces:**
- Consumes: Task 2's `CubeLattice`, `Routing`, `Routed`, `Basin` and `receiver_of`. Task 1's grid layout, read as `formats::Grid`. Also `snapshot_field`, `zonal_biome`, `snapshot_sea_level_anomaly_m` and `TileSet::sample_overview`.
- Produces, in `climate.rs`:
  - `Climate { precip_mm: f32, temp_c: f32 }`.
  - `ClimateTable { pet_mm: Vec<(f32, f32)>, lake_evaporation_mm: Vec<(f32, f32)>, turc_pike_n: f32, ice_melt_runoff_mm: f32, biome_climate: [Climate; 12] }`. The breakpoints are (°C, mm/yr), linear between and clamped at the ends.
    - `runoff_mm(&self, c) -> f32`: `P − P / (1 + (P/PET)^n)^(1/n)`, or `P` when PET = 0.
    - `net_evaporation_mm(&self, c) -> f32`: `max(0, E(T) − P)`.
  - `pub trait ClimateSource: Sync { fn climate(&self, dir: RenderDir, elevation_m: f32, coast_km: f32) -> Climate; fn needs_coast_km(&self) -> bool { false } }`.
  - `BeyerClimate<'a>::new(grids: [&'a Grid; 2], older_weight: f64, zonal: ZonalClimate<'a>)`: bilinear on the 1° grid, linear in age. A NaN tap falls back to `zonal`.
  - `ZonalClimate<'a> { table: &'a ClimateTable, sea_level_anomaly_m: f64 }`: `zonal_biome` mapped through `biome_climate`. Its `needs_coast_km` is true.
  - `climate_bracket(ages_ka: &[f64], age: YearsBp) -> Option<(usize, usize, f64)>`: `None` past `CLIMATE_MAX_KA`.
- Produces, in `pass.rs`:
  - `NodeSample { elevation_m: f32, ice: f32 }` and `pub trait NodeSource: Sync { fn node(&self, dir: RenderDir) -> NodeSample; fn data_lakes(&self) -> bool; }`.
  - `SourceMode { Recent, Bridge, PresentFrame, Reference }` and `LatticeSizes { recent_n: u32, deep_n: u32 }` (`Default` = the constants).
  - `hydro_lattice_for(timeline: &Timeline, age_myr: f64, sizes: LatticeSizes) -> (CubeLattice, SourceMode)`:
    - `Recent` for `FrameSelection::Recent` and `Bridge` for `Bridge`. Both take `recent_n`.
    - `PresentFrame` for `Deep` with `age ≤ PRESENT_FRAME_MAX_MYR`, also `recent_n`.
    - `Reference` otherwise, with `deep_n`.
  - `SnapshotSource::new(field: &'a SnapshotField<'a>, tiles: &'a TileSet, timeline: &Timeline, age_myr: f64, mode: SourceMode)`. Its `node` per mode:
    - `Recent`: `sample_overview`.
    - `Bridge`: `(1−w)·recent.sample_overview + w·(overview − anomaly(age))`, with ice `(1−w)·ice`.
    - `PresentFrame`: `overview − anomaly(age)`, ice 0.
    - `Reference`: `DeepField::reference_sample`.
    - `data_lakes()` is true for every mode but `Reference`.
  - `river_tile_margin_m(lattice: &CubeLattice) -> f64`: `(1.5 + CORRIDOR_FRACTION)·spacing + INCISION_WINDOW_M`.
  - `HydroPrep`:
    - `new(lattice: CubeLattice, age: YearsBp)`. Data-lake detection follows `source.data_lakes()`.
    - `step<S: NodeSource, C: ClimateSource>(&mut self, source: &S, climate: &C, table: &ClimateTable, max_units: u32) -> bool`.
    - `sample_range<S>(lattice, source, range: Range<u32>) -> NodeBlock` (static and pure, for parallel sampling) and `add_samples(&mut self, block: NodeBlock)`.
    - `units_done() -> u64`, `working_bytes() -> usize`, and `finish(self) -> EraHydrology` (debug-asserts that `step` returned true).
    - The phases are: sample, route, coast distance (only if `needs_coast_km`), accumulate with the lake hook, terminal-basin routing, graph.
- Produces, in `lakes.rs`:
  - `LakeKind { Open, Terminal, Data }`.
  - `Lake { level_m: f32, spill_m: f32, kind: LakeKind, inflow_m3s: f64, evaporation_m3s: f64, outflow_m3s: f64, area_km2: f64, exit: u32 }`.
- Produces, in `graph.rs`:
  - `Down { River(u32), Ocean(u32), Lake(u16, u32), Ice(u32) }`.
  - `RiverNode { node: u32, down: Down, area_km2: f32, discharge_m3s: f32, bed_m: f32 }`.
  - `RiverGraph` with `nodes()`, `index_of(lattice_node) -> Option<u32>`, `upstream(i) -> &[u32]` (CSR), `down_bed_m(i) -> f32`, and `near(&self, lattice: &CubeLattice, dir: RenderDir, radius_m: f64, out: &mut Vec<u32>)`.
    - `near` tests buckets of 32 × 32 texels by angular distance, so it is seam-safe.
    - `down_bed_m` is `min(0, bed)` at the ocean and `min(L, bed)` at a lake.
- Produces, in `mod.rs`: `EraHydrology { age: YearsBp, lattice: CubeLattice, lakes: Vec<Lake>, rivers: RiverGraph, … }` with:
  - `is_ocean(node)` and `ocean_tap(dir) -> bool` (any of the four taps).
  - `lake_tap(dir) -> Option<LakeTap { lake: u16, level_m: f32, kind: LakeKind, shore_steps: f32 }>`: the highest-level lake among the taps, with the taps' shore distance in nodes interpolated bilinearly.
  - `bytes() -> usize`.

The rules for lakes, routing inside basins and the graph:

```text
own runoff per node: runoff_mm(climate) (ice ≥ ICE_NODE_FRACTION: ice_melt_runoff_mm) × area / yr.
At each basin's exit, before passing on: Q_in = q[exit]; capacity c_i = net_evaporation × area for
  basin nodes that are not ice. Group the basin nodes by distinct elevation, ascending; C_g is the
  cumulative capacity through group g.
  - C_last < Q_in: Open, level = spill, evaporation = C_last, outflow = Q_in − C_last.
  - else: Terminal, outflow 0, evaporation = Q_in, level = e_g + (Q_in − C_{g−1}) / (C_g − C_{g−1})
    × (e_{g+1} − e_g), for the first g with C_g ≥ Q_in, where e_{last+1} = spill.
  - Data (only if data_lakes): at least DATA_LAKE_MIN_NODES connected basin nodes lie within
    DATA_LAKE_FLAT_TOL_M of the basin minimum, and Q_in ≥ DATA_LAKE_MIN_BALANCE × capacity of those
    nodes. Then level = the median of those nodes; outflow = max(0, Q_in − capacity below the level)
    if level ≥ spill − DATA_LAKE_FLAT_TOL_M, else 0.
  Footprint = basin nodes with e < level, minus ice; shore_steps by BFS inside the footprint.
In a terminal basin, a dry node (e ≥ level) routes to the neighbour with a smaller BFS distance to
  the footprint, choosing the lowest e (ties: neighbour order). Discharge inside the basin is
  re-accumulated in descending BFS distance.
River nodes: not ocean, not ice, not in a footprint, discharge ≥ RIVER_MIN_DISCHARGE_M3S.
  bed = min(e, the beds of its upstream river nodes), computed upstream first, so beds never rise
  downstream.
```

- [ ] **Step 1: Write the failing tests:**
  - `golden_lakes_match_the_fixture`: case A's `q` exactly. Case B (capacity 2 per basin node) is Open with outflow 4 and q(1,1) = 5. Case C (capacity 5) is Terminal with level 2 + 7/15 m, outflow 0 and q(1,1) = 1.
  - `lakes_conserve_the_water_balance` (proptest, random basins and climates):
    - `inflow = evaporation + outflow` within 1e-6 relative.
    - `outflow > 0` ⇔ `level == spill`.
    - The level is monotonic in the inflow.
  - `the_shipped_table_turns_climates_into_runoff`. It reads `data/hydrology/climate_table.json` with `serde_json` (a dev-dependency):
    - The Sahara (P 30, T 28) gives < 1 mm.
    - The Amazon (2,300, 26) gives 1,000–1,200 mm.
    - The upper Indus (1,000, 5) gives 450–700 mm.
    - Runoff is monotonic in P.
  - `an_arid_basin_gets_no_large_lake`: a 400 km endorheic bowl under the `HotDesert` row gives a lake area < `LARGE_LAKE_KM2`.
  - `a_humid_basin_spills_to_its_outlet`: under `TropicalRainforest` it is `Open`.
  - `a_data_lake_keeps_its_flat_level`: a Caspian-like flat at −28 m in a bowl that spills at +30 m becomes `Data` at −28 ± 0.01. It is not filled to the spill.
  - `a_dry_salt_flat_is_not_a_data_lake`: the same flat under `HotDesert` gives no lake.
  - `rivers_end_at_the_sea_a_lake_or_the_ice`: every `down` chain ends in `Ocean`, `Lake` or `Ice`.
  - `discharge_never_shrinks_between_lakes` and `river_beds_never_rise_downstream`.
  - `rivers_inside_a_terminal_basin_flow_to_its_lake`.
  - `no_rivers_or_lakes_on_ice`.
  - `desert_channels_are_not_rivers`: zero runoff over 50,000 km² gives no river nodes.
  - `the_pass_is_the_same_for_any_step_size`: step(1), step(`u32::MAX`), and parallel `sample_range` blocks give equal `EraHydrology`.
  - `hydro_lattice_follows_the_era` (on the testkit timeline): 0 ka and 21 ka are `Recent`/900, 28 ka is `Bridge`/900, 50 ka is `PresentFrame`/900, and 1 Ma and 100 Ma are `Reference`/180.
  - `snapshot_source_matches_the_overview_and_ice`: `Recent` equals `sample_overview` exactly, and `Bridge` and `PresentFrame` match their formulas within 1e-9.
  - `climate_interpolates_between_baked_ages`: 8.25 ka between 8 and 9 ka blends 0.75 / 0.25, and 130 ka is `None`.
  - `a_deep_era_pass_runs_on_a_paleodem_field`: the rift fixture at 100 Ma with `deep_n` = 24 is finite, and every river ends.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly_core hydrology::`. Expected: they fail to compile.
- [ ] **Step 3: Implement** as specified. Write the table:
  - `pet_mm`: [[-20,0],[-10,120],[0,300],[10,650],[20,1100],[30,1600]].
  - `lake_evaporation_mm`: [[-20,50],[-10,200],[0,450],[10,900],[20,1500],[30,2200]].
  - `turc_pike_n` 2.0 and `ice_melt_runoff_mm` 300.
  - Biome rows (P, T):
    - TropicalRainforest (2500, 26), MonsoonSavanna (1200, 25), HotDesert (60, 26), ColdDesert (150, 8)
    - Steppe (350, 10), Mediterranean (600, 16), TemperateBroadleaf (900, 11), TemperateRainforest (2000, 10)
    - Boreal (550, −2), Tundra (300, −10), IceCap (200, −25), Alpine (900, −2)
  - Accumulations are f32 per node. Peak `working_bytes` must stay ≤ 160 MiB at n = 900.
- [ ] **Step 4: Run the tests to check they pass.** Run `cargo test -p timefly_core hydrology::`. Expected: all pass.
- [ ] **Step 5: Run the gate and commit** `feat(core): climate runoff, lake balance and the river graph`. Stage `crates/timefly_core/src/hydrology crates/timefly_core/src/worldgen/coarse/deep.rs crates/timefly_core/tests/fixtures/hydro_golden.json data/hydrology/climate_table.json`.

### Task 4: Running the pass in the shell

**Files:**
- Create: `crates/timefly/src/hydro/mod.rs` (`HydroPlugin`, `HydroCache`, `HydroSettings`, the messages), `hydro/job.rs` (inputs and the run), `hydro/climate.rs` (`ClimateManifest` and its loader)
- Modify:
  - `region/frames.rs`: lift the frame loading out of `region/job.rs` as `FrameLoads::start(...)` and `FrameLoads::poll(...) -> Option<Result<Vec<Option<Arc<Grid>>>, String>>`, used by both jobs.
  - `region/job.rs`: the tile index starts once `GlobeData` exists, not on the first request. `advance_tiles` waits for the pass.
  - `region/generate.rs`: `SharedBudget`, `Slice` and `YieldOnce` become `pub(crate)`. `GenInputs` gains `hydro`.
  - `region/mod.rs`: `RegionGenerated` gains `hydro_wait_ms: u32` and `hydrology: bool`.
  - `lib.rs`: add `HydroPlugin` after `RegionPlugin`.
- Test: `hydro/*.rs` inline, `crates/timefly/tests/region_loading.rs`

**Interfaces:**
- Consumes: Task 3's `HydroPrep`, `SnapshotSource`, `BeyerClimate`, `ZonalClimate`, `ClimateTable`, `climate_bracket`, `hydro_lattice_for` and `EraHydrology`. Also `snapshot_field`, the `TileCache` overview, `FrameCache`, `LoadClock` and `LargeGenBudget`.
- Produces:
  - `HydroCache` (`Resource`): `status(age: YearsBp) -> HydroStatus { Ready(Arc<EraHydrology>), Running, Failed, Absent }` and `insert`. It is an LRU of at most `HYDRO_CACHE_ERAS`, keyed by `age.0.to_bits()`.
  - `HydroSettings { enabled: bool, sizes: LatticeSizes }` (`Resource`, default on). Tests lower the sizes to 16 and 8. Disabled, a region never waits and generates with `hydro: None`.
  - Messages, each `add_domain_event`:
    - `HydroRequested { age: YearsBp, purpose: HydroPurpose { Prefetch, Region } }`
    - `HydroPassFinished { age, wall_ms: u32, cpu_ms: u32, slices: u32, nodes: u32, river_nodes: u32, lakes: u32, platform: &'static str }`
    - `HydroPassFailed { age, reason: String }`
  - `climate_table() -> ClimateTable`: it parses `include_str!("../../../../data/hydrology/climate_table.json")` with serde.
  - `GenInputs.hydro: Option<Arc<EraHydrology>>`. Task 7 passes it to the generator; here it only travels with the inputs.
  - `WEB_HYDRO_BUDGET_MS: f64 = 6.0`.

The rules:
- **Prefetch.** On the globe, once `nearest_snapshot(GlobeTime)` has not changed for `HYDRO_SETTLE_S` on the load clock, the plugin sends `Prefetch` for it, unless it is cached or running.
- **One job at a time.** A `Region` request replaces a `Prefetch` job. A `Prefetch` never replaces a `Region` job.
- **Region jobs.** A region job reaching `advance_tiles` with `Absent` sends `Region` once and waits. With `Failed`, it proceeds with `hydro: None` and a `warn`.
- **Climate.** A missing or failed climate manifest or grid means zonal for every age, with one `warn` per session.
- **Native.** The pass runs on an `AsyncComputeTaskPool` task. Sampling runs in parallel `sample_range` blocks of 4,096 nodes through the pool's `scope`, then `step(u32::MAX)`.
- **Web.** A `HydroRun`, like `BudgetedRun`, at `WEB_HYDRO_BUDGET_MS`, or `WEB_GEN_LARGE_BUDGET_MS` while `LargeGenBudget` is true.

- [ ] **Step 1: Write the failing tests:**
  - `the_cache_keeps_two_eras` (inline): inserting three ages evicts the least recently used.
  - In `tests/region_loading.rs`, on the tiny assets with `HydroSettings` sizes (16, 8):
    - `the_tile_index_loads_with_the_globe`: the manifest and overview load starts with no region request.
    - `the_pass_runs_once_the_globe_data_is_ready`: `HydroPassFinished` for 0 ka, and no region job.
    - `a_dive_waits_for_its_eras_pass`: entering at 21 ka gives `HydroPassFinished { age: 21 ka }` before `RegionGenerated { hydrology: true, hydro_wait_ms > 0 }`.
    - `missing_climate_falls_back_to_the_zonal_rule` (Review Focus 1): with the climate folder removed from a copied asset root, there is one `warn`, the pass finishes, and `RegionGenerated { hydrology: true }`.
    - `dragging_the_slider_runs_one_pass_for_the_settled_era` (Review Focus 2): step `GlobeTime` through 10 snapshots, one per frame, then hold. Exactly one `HydroPassFinished`, for the held era, and `HydroCache` length ≤ 2.
    - `a_region_request_preempts_a_prefetch` (Review Focus 2): the region's age finishes first.
    - `a_disabled_pass_never_holds_a_region`.
  - `web_slices_respect_the_budget` (inline, native): a `HydroRun` at 2 ms. Every slice is ≤ 2 ms plus one unit's measured maximum, and the result equals the task run's.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib hydro::` and `cargo test -p timefly --test region_loading`. Expected: they fail to compile.
- [ ] **Step 3: Implement** as specified. Log `continental pass done` at `info` with the age, the wall and CPU milliseconds, the slices, the node, river-node and lake counts, and `working_bytes`.
- [ ] **Step 4: Run the tests to check they pass,** including the existing `region_loading` and `region_bench` tests. Expected: all pass.
- [ ] **Step 5: Run the gate and commit** `feat: run the continental pass per era and hold regions for it`. Stage `crates/timefly/src/hydro crates/timefly/src/region crates/timefly/src/lib.rs crates/timefly/tests/region_loading.rs`.

### Task 5: Refining the rivers on a region's terrain (core)

**Files:**
- Create: `crates/timefly_core/src/worldgen/generate/rivers.rs`
- Modify:
  - `worldgen/coarse/mod.rs`: a new trait method, `CoarseField::tiles_needed_with_margin(&self, frame: &RegionFrame, margin_m: f64) -> Vec<(u8, u8)>`, implemented by every field. The existing `tiles_needed(frame)` becomes `tiles_needed_with_margin(frame, TILE_MARGIN_M)`, so its callers don't change.
  - `worldgen/tiles.rs`: `needed_for_margin(frame, margin_m)` and `needed_for_rotated_margin(frame, q, margin_m)`, which the existing two call with `TILE_MARGIN_M`.
  - `worldgen/coarse/modern.rs`: `modern_tiles_needed` takes the margin.
  - `worldgen/generate/detail.rs`: `noise_input` becomes `pub(super)`.
  - `worldgen/testkit.rs`: move `FnField` there from `generate/tests.rs`.
- Test: `rivers.rs` inline, and `generate/tests.rs` (`tiles_needed_covers_every_river_read`)

**Interfaces:**
- Consumes: Task 3's `EraHydrology`, `RiverGraph`, `CubeLattice` and `river_tile_margin_m`.
- Produces:
  - `hydraulic_geometry(q_m3s: f32) -> (f32, f32)`: (width, depth), by the constants.
  - `river_speed_mps(depth_m: f32, slope: f32) -> (f32, f32)`: (speed, rapid). With `rapid = smoothstep(RAPID_SLOPE, slope)`, speed is the Manning speed `depth^(2/3)·√slope / MANNING_N` clamped to [0.3, 3.0] when `rapid == 0`, and `3.0 + 3.0·rapid` otherwise.
  - `rapid_from_speed(speed_mps: f32) -> f32`: `clamp((speed − 3) / 3, 0, 1)`. Task 8's shader mirrors it.
  - `SubPoint { dir: DVec3, coarse_m: f64, bed_m: f32, level_m: f32, half_width_m: f32, depth_m: f32, speed_mps: f32, rapid: f32, tangent: DVec3, river: u32, k: u32 }`.
  - `RiverBuilder<'a>::new(hydro: &'a EraHydrology, frame: &RegionFrame, half_size_m: f64, subpoint_m: f64, min_q: f32)`:
    - `step(&mut self, floor_at: &impl Fn(RenderDir) -> f64, key_at: &impl Fn(RenderDir) -> (DVec3, u32), budget: u32) -> bool`. One unit is one sub-point.
    - `finish(self) -> RegionRivers`.
    - `floor_at` is the field lattice's coarse elevation, at the field's level. `key_at` is `Detail::noise_input` of the point's lattice key.
  - `RegionRivers` with `subpoints()`, and `texel(&self, dir: RenderDir, h_m: f64, texel_m: f64) -> RiverTexel { h_m: f64, hit: Option<RiverHit> }`.
  - `RiverHit { level_m: f32, ribbon_m: f32, flow_east_mps: f32, flow_north_mps: f32, fade: f32 }`. Flow is in the texel's own east and north (`RegionFrame { center: dir }.north_east()`).

The refinement, which the tests pin:

```text
gather: river nodes whose segment (node → down) comes within half_size + V_max + 2·subpoint, then
  their upstream within INCISION_WINDOW_M + one spacing of coarse path distance (great-circle
  node-to-node lengths, global). Rivers below min_q are skipped.
relax: each node → the lowest interior local minimum of floor_at on a 7 × 7 grid over ±C in its
  own north/east (C = CORRIDOR_FRACTION × mean spacing): a grid point off the border, strictly
  lower than both neighbours along its row or both along its column (ties: smaller offset, then
  grid index); with none, the node stays put. Segment sub-points sit at t = k/K (K = ceil(length
  / subpoint)), k = 0..K−1, plus k = K on a terminal segment. Each takes the lowest interior
  local minimum of its 13 samples across the chord between the relaxed nodes (not at either
  edge, strictly lower than both neighbours; ties: smaller |offset|, then the negative side), or
  offset 0 when there is none. Next to an end whose node did not move (relaxed == lattice
  direction, or an unrelaxed ocean/lake/ice end) the raw offsets taper linearly to 0 over 3
  sub-points: sub-point k scaled by k/4 ((K − k)/4 at the far end; the smaller weight where both
  apply), so a valley beside a pinned node is entered gradually (Task 6 fix round 2). Then
  smoothed [1,2,1]/4 with the ends fixed. (Task 6 fix round 1: on
  a tilted plain the lowest sample is at the edge, and taking it made hairpins at every node.
  Nodes use row-or-column minima because a valley floor falls downstream, so it is never lower
  than all eight neighbours.)
meander: λ = 11 w. A = 1.5 w × (1 − smoothstep(MEANDER_FLAT_SLOPE, pass slope)), and 0 if λ < 2 km.
  Offset = A × fbm(key_at(p), λ, 2 octaves) along the normal. A, λ and the normal interpolate in t
  from node values; a node's normal bisects its outgoing segment and its main incoming one (the
  largest discharge, ties lowest node id).
bed, along coarse path distance, with B = the pass bed lerped in t (non-increasing):
  τ(s') = max(floor_at(s'), B(s' ⊕ L))    L = INCISION_WINDOW_M; ⊕ walks the global graph downstream
  bed(s) = min(B(s), min{ τ(s') : s' upstream of s within L, s itself included })
  level(s) = bed(s) − LEVEL_BELOW_BED × depth(Q).
  This is non-increasing downstream: a term leaves the window only once B(s) has fallen below it.
slope = (bed(s ⊖ 500 m) − bed(s ⊕ 500 m)) / 1000, then speed and rapid by river_speed_mps.
texel: the candidates are sub-segments within their valley half-width V = max(4w, w_eff/2 +
  3·texel), where w_eff = max(w, CHANNEL_MIN_TEXELS × texel). d is the great-circle distance in f64.
  cut_i(d): channel d ≤ w_eff/2: level − depth·(1 − (2d/w_eff)²); else level + BANK_FREEBOARD_M +
    VALLEY_WALL_SLOPE·(d − w_eff/2), blended to h over the outer half of V.
  h' = min(h, min_i cut_i). The hit is the candidate with the largest ribbon w/2 − d (ties: lower
  river, then lower k). Outside its channel, h' = max(h', hit.level + BANK_FREEBOARD_M) wherever
  its fade is 1. ribbon_m = w/2 − d, flow = speed × tangent.
```

- [ ] **Step 1: Write the failing tests.** Use synthetic `EraHydrology`s from `HydroPrep` with small n over `FnField` terrains: a V valley, a gorge and a flat plain.
  - `refined_rivers_are_the_same_from_any_region`: builders for two frames 100 km apart give the same sub-points (bits of `dir`, `bed_m`, `level_m`, `half_width_m`) for every shared segment.
  - `the_level_never_rises_downstream` (proptest over random valley fields): non-increasing along every path, across confluences included.
  - `the_relaxed_river_lies_in_the_valley_floor`: with a V valley 3 km off the coarse line, the centreline is within 300 m of the axis.
  - `incision_follows_a_gorge_below_the_pass_bed`: with the floor 400 m under B along 10 km at a 2% slope, the bed is within 1 m of the floor in the gorge.
  - `incision_never_cuts_below_the_window_rule`: for all s, `bed(s) ≥ min(B(s), floor(s), B(s ⊕ L))`.
  - `meanders_grow_on_flat_ground_and_vanish_in_steep_valleys`, and `small_rivers_do_not_meander` (w = 50 m).
  - `meanders_meet_at_shared_nodes`: the end of an incoming segment and the start of the outgoing one are the same bits.
  - `width_depth_and_speed_follow_hydraulic_geometry`:
    - Q = 200,000 gives w = 2,236 m and d ≈ 31.6 m.
    - Q = 100 at slope 0.02 gives `rapid > 0` and speed in [3, 6].
    - Q = 5,000 at 1e-4 gives speed in [0.3, 3].
  - `a_texel_on_the_centreline_is_below_the_level`, and `banks_hold_the_river`: every texel with `fade == 1` outside the channel has `h' ≥ level + BANK_FREEBOARD_M − 1e-3`.
  - `tiles_needed_covers_every_river_read`: every direction `floor_at` is called with lies in a tile of `field.tiles_needed_with_margin(&frame, river_tile_margin_m(..))` (the M2 pattern of `deep_tiles_needed_cover_every_inner_read`).
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly_core worldgen::generate`. Expected: they fail to compile.
- [ ] **Step 3: Implement** `rivers.rs` and the margin change.
- [ ] **Step 4: Run the tests to check they pass.** Run `cargo test -p timefly_core`. Expected: all pass, the M2 and M3 generator tests included.
- [ ] **Step 5: Run the gate and commit** `feat(core): refine the era's rivers on a region's terrain`. Stage `crates/timefly_core/src/worldgen`.

### Task 6: Carved terrain, water fields and boulders in the generator (core)

**Files:**
- Create: `crates/timefly_core/src/worldgen/generate/water.rs`
- Modify:
  - `worldgen/generate/mod.rs`:
    - `Field` and `FieldRows` gain `water_level_m: Vec<f32>` and `river: Vec<[u16; 4]>` (f16 bits of `[ribbon_m, flow_east_mps, flow_north_mps, 0]`).
    - `Field::dry(res, size_m, elevation_m, surface)` and `RegionGrid.boulders: Vec<Boulder>`.
    - `RegionPrep::new(spec, field, hydro: Option<&'a EraHydrology>)` and `RegionGenerator::new(spec, field, hydro)`.
    - A `Rivers(i)` phase after `Fill(i)`, stepped in sub-point units.
    - `sample_texel_in(kind, dir) -> Texel { h: f32, surface: [u8; 4], water_level_m: f32, river: [u16; 4] }`. `sample_point` is kept.
    - `bytes()` counts the new fields.
  - `crates/timefly_core/src/terrain/mod.rs`:
    - `WaterSample { level_m: f32, thickness_m: f32, ribbon_m: f32, flow: [f32; 2] }`.
    - `blended_water(grid, x_m, z_m) -> WaterSample`, which blends exactly as `blended_height_m`.
    - `blended_surface_m(grid, x_m, z_m) -> f32`: `max(height, level)`.
  - Every `Field { .. }` literal in both crates' tests becomes `Field::dry(..)`. Every `RegionPrep::new` and `RegionGenerator::new` call gains `None`.
- Test: `generate/tests.rs`, `water.rs` inline, `terrain/tests.rs`

**Interfaces:**
- Consumes: Task 5's `RegionRivers::texel`, and Task 3's `ocean_tap` and `lake_tap`.
- Produces:
  - The fields above.
  - `Boulder { dir: DVec3, base_m: f32, radius_m: f32, seed: u32, wet: f32 }`: inner field only.
    - Per sub-point with `rapid > 0`: `floor(rapid × BOULDERS_PER_SUBPOINT_MAX + h0)` boulders, where `h0` is the hash fraction of `(river node, k)`.
    - Each boulder's offsets and radius come from `hash3` of `(river node, k, j)`. Its base is the channel bed there.
    - `MAX_BOULDERS` caps the set, with a `warn`.

The per-texel rule (`water.rs`):

```text
h0 = coastline_clamp(coarse + D, corners)                       (unchanged)
lake = hydro.lake_tap(dir)
h1 = if lake is Data: min(h0, L − 1 − DATA_LAKE_MAX_DEPTH_M × min(1, shore_steps / RAMP)) else h0
rt = rivers[field].texel(dir, h1, texel_m);  h = rt.h_m   (written to elevation_m; surface uses h)
candidates (level, ribbon, flow):
  sea   if any corner < 0 and (no pass, or hydro.ocean_tap(dir), or an ocean neighbour of a tap
        within OCEAN_STRAIT_M of dir: Task 6's call and fix round 2):  (0, RIBBON_NONE_M, 0)
  lake  if lake:                                                    (L, RIBBON_NONE_M, 0)
  river if rt.hit with fade > 0:                                    (hit.level, hit.ribbon, hit.flow)
water = the highest level (ties: lake, sea, river); none: (h − NO_WATER_DROP_M, RIBBON_NONE_M, 0)
```

- [ ] **Step 1: Record the M3 baseline.** On the parent commit, before any change, hash the bytes of `elevation_m` and `surface` for the fixtures of `overlap_agreement` and `prep_in_steps_matches_new`. Put the hex in a constant of the new test.
- [ ] **Step 2: Write the failing tests:**
  - `without_a_pass_the_terrain_is_m3s`: with `hydro: None` the hashes are unchanged. The water level is 0 exactly where any corner is below sea level.
  - `rivers_agree_bit_for_bit_where_regions_overlap`: two regions 100 km apart, inner and apron, `sample_texel_in` at 400 probes. Every field is equal by bits.
  - `rivers_agree_bit_for_bit_at_a_cube_corner` (Review Focus 3): the same with the centres either side of the corner at 35.26° N, 45° E.
  - `grid_texels_equal_sample_texel_at_their_directions`, extending `overlap_agreement`'s last block.
  - `water_never_flows_uphill_along_a_refined_centreline`: the texel water level sampled along each centreline is non-increasing downstream, within 1e-3 m for the bilinear read.
  - `a_sub_texel_river_is_drawn_by_its_ribbon`: at Q = 20 m³/s (w ≈ 22 m) the ribbon is > 0 only within 11 m of the centre, and `blended_water.thickness_m > 0` under it.
  - `an_inland_basin_below_sea_level_is_not_sea`: a −28 m bowl 400 km from any ocean gets the lake's level, or no water. It never gets 0.
  - `the_sea_band_covers_every_shoreline`: at texels with mixed corners, `sign(level − h) == sign(−h)`.
  - `boulders_lie_in_rapids_and_agree_across_regions`: every boulder is within a rapid's channel, and the overlap's sets are equal.
  - `prep_in_steps_matches_new`, extended with a pass.
  - `blended_water_mirrors_blended_height` (terrain tests).
- [ ] **Step 3: Run them to check they fail.** Run `cargo test -p timefly_core`. Expected: the new tests fail to compile.
- [ ] **Step 4: Implement** as specified.
- [ ] **Step 5: Run the tests to check they pass.** Run `cargo test -q --workspace`. Expected: all pass, `without_a_pass_the_terrain_is_m3s` included.
- [ ] **Step 6: Run the gate and commit** `feat(core): carve rivers and write water fields per texel`. Stage `crates/timefly_core/src crates/timefly/src crates/timefly/tests crates/timefly/examples` (the `Field::dry` call sites).

### Task 7: Regions with rivers in the shell

**Files:**
- Modify:
  - `region/generate.rs`: `generate_parallel` and `run_budgeted` pass `inputs.hydro.as_deref()`.
  - `region/generate.rs` and `region/job.rs`: `tiles_needed(spec, timeline, crust, frames, margin_m)` calls `tiles_needed_with_margin`. The job passes `max(TILE_MARGIN_M, river_tile_margin_m(&hydro_lattice_for(..).0))`, or `TILE_MARGIN_M` when the pass is disabled.
  - `terrain/material.rs`: `water_image(field: &Field) -> Image`.
  - `terrain/mod.rs`: `FieldImages` and `TerrainScene` gain `inner_water` and `apron_water`, which `upload_fields` and `replace_fields` fill.
  - `fly_camera.rs`, `bench/region/*.rs`, `bench/journey/*.rs`, `region/navigate/*.rs` and `examples/region_tour.rs`: every `blended_height_m(..).max(0.0)` becomes `blended_surface_m(..)`.
- Test: `terrain/tests.rs`, `fly_camera.rs` tests, `tests/region_loading.rs`

**Interfaces:**
- Consumes: Task 6's fields, `blended_surface_m` and `blended_water`, and Task 4's `GenInputs.hydro`.
- Produces: `water_image`, an `Rgba32Uint` laid out like `height_image`, with texel = `[level bits, (level − h) bits, ribbon f32 bits, pack2x16float(flow_east, −flow_north)]`. Region z points south, so z flow is −north. `RENDER_WORLD` usage.

- [ ] **Step 1: Write the failing tests:**
  - `the_water_texture_holds_level_thickness_ribbon_and_flow`: the bits decode to `W`, `W − h`, the ribbon and `(flow_x, flow_z)`.
  - `a_replaced_region_rebinds_the_water_textures`.
  - `the_fly_stays_above_a_lake_surface` (Review Focus 4): over a grid with a 3,812 m lake on a 3,700 m bed, the fly never ends a step below 3,812 + `CAMERA_CLEARANCE_M`.
  - `the_fly_descends_into_a_dry_basin_below_sea_level` (Review Focus 4): over a dry −300 m basin, the fly settles at −300 + `CAMERA_CLEARANCE_M`.
  - `a_region_with_rivers_generates_the_same_on_the_web_budget` (`region_loading`): `Budgeted` and `Parallel` give equal grids.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib terrain:: fly_camera::` and `cargo test -p timefly --test region_loading`.
- [ ] **Step 3: Implement.** The CPU grid keeps the level and river arrays, which clearance and the upload need. The GPU copy is built at upload and dropped after it.
- [ ] **Step 4: Run the tests to check they pass,** plus `cargo test -p timefly --test region_bench --test journey_bench`. Expected: all pass.
- [ ] **Step 5: Run the native region bench twice** (`--region-bench`, `$ROOT` as in Global Constraints) and note the Himalayas' warm generation and the memory cycles. Expected: the gates still pass.
- [ ] **Step 6: Run the gate and commit** `feat: regions generate and upload their rivers`.

### Task 8: Drawing rivers and lakes

**Files:**
- Modify:
  - `crates/timefly/src/water.rs`:
    - `WaterExtension` replaces `inner_height` and `apron_height` with `#[texture(101, sample_type = "u_int", filterable = false)] inner_water` and `#[texture(102, ..)] apron_water`. Check the attribute's spelling against Bevy 0.20's `AsBindGroup`.
    - `water_material(patches, inner_water, apron_water, look)`.
    - `WaterLook` gains `flow: Vec4` (period s, normal scale m, white-water foam, rapid scroll factor), `river_clear` and `river_silty`.
  - `assets/shaders/terrain_cdlod.wesl`: `blended_water(inner: texture_2d<u32>, apron: texture_2d<u32>, params, xz) -> WaterSample`, which mirrors the Rust.
  - `assets/shaders/water.wesl`:
    - The vertex stage puts the surface at `level` (plus the swell where `ribbon ≥ RIBBON_NONE_M`, as today).
    - The fragment stage takes `thickness` in place of `−blended_height`, and the floor gradient from `−∇thickness`.
    - It discards where `thickness ≤ 0`, or where `ribbon < 0` on a river (M2's shoreline rule).
    - It adds flow-map scrolling and white water.
  - `terrain/warmup.rs`: the warm-up's water uses the new constructor over `warmup_grid()` (`Field::dry` fields; a pipeline's key doesn't depend on the data).
- Test: `water.rs` inline, `terrain/warmup.rs`, `crates/timefly/tests/region_gpu.rs`

**Interfaces:**
- Consumes: Task 7's `TerrainScene.inner_water` and `apron_water`, and Task 5's `rapid_from_speed`.
- Produces, with each constant mirrored in WESL and checked by the existing `wesl_const_f32` pattern:
  - `FLOW_PERIOD_S = 2.0`, which divides `TIME_WRAP_S`.
  - `FLOW_NORMAL_SCALE_M = 6.0`.
  - `RIVER_CLEAR = srgb(0.10, 0.32, 0.33)` and `RIVER_SILTY = srgb(0.36, 0.30, 0.20)`.
  - `SILT_SPEED_MPS = [1.0, 3.0]`: `silt = 1 − smoothstep`, so slow is brown.
  - `WHITE_WATER = srgb(0.92, 0.94, 0.93)`.
  - `flow_phase_weights(t_s: f32, jitter: f32) -> [f32; 2]`: the two phases, half a period apart, with a triangle weight.

- [ ] **Step 1: Write the failing tests:**
  - `flow_period_divides_the_wrap`.
  - `the_two_flow_phases_never_fade_together`: over 1,000 times and jitters, the weights sum to 1 and their maximum is ≥ 0.5.
  - `rapids_decode_from_speed`: the WESL `rapid_from_speed` text mirrors the Rust.
  - `river_tint_is_browner_when_slow`.
  - `water_look_matches_its_wesl_mirror`, extended.
  - `the_water_binds_no_more_textures_than_before`: two textures in the extension.
  - `wesl_water_matches_rust` (`region_gpu`, `#[ignore]`, GPU): `blended_water` in WESL equals `timefly_core::terrain::blended_water` within 2 ulps of the level, at 4,096 points over a random field with a river.
  - In `terrain/warmup.rs`: `the_warm_up_draws_the_new_water`, where the warm-up's water material holds both water textures.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib water:: terrain::warmup`.
- [ ] **Step 3: Implement.**
  - **Flow:** the normals and foam noise are sampled at `xz − flow × phase × FLOW_PERIOD_S` for each phase. The jitter is `brush(xz, 400 m)`.
  - **Rapids:** `rapid = rapid_from_speed(|flow|)`. Scroll ×(1 + rapid), normal roughness ×(1 + 3 rapid), and foam `max(foam, rapid × broken noise)`, bright and broken.
  - **Tint:** on rivers (`ribbon < RIBBON_NONE_M`) it is `mix(RIVER_CLEAR, RIVER_SILTY, silt)` by depth. Lakes and the sea keep M2's tint.
- [ ] **Step 4: Run the tests to check they pass,** then `cargo test -p timefly --test region_gpu -- --ignored`. Expected: all pass.
- [ ] **Step 5: Offscreen renders,** each with a fresh `TIMEFLY_LOG_DIR`, via `cargo run --release -p timefly --example region_tour -- <scratch> 1280x720 <place> <prefix> shaded <AGL_M>`:
  - The Amazon at Manaus: `-3.1,-60.0,0`, AGL 3,000.
  - An Indus tributary gorge: `34.2,74.3,0`, AGL 1,500.
  - Titicaca: `-15.8,-69.4,0`, AGL 2,000.
  - The Caspian (design §6.4): `42.0,50.5,0`, AGL 20,000.
  - Pass: water at true width; rapids white over rocks once Task 9 lands; Titicaca and the Caspian at their own levels with fractal shores; no water band at a rectangle's edge.
- [ ] **Step 6: Run the gate and commit** `feat: rivers and lakes drawn from the water fields`.

### Task 8b: Lakes checked against today's map

Owner ruling (2026-11-25): "Check against today": a basin that would fill but holds no lake in
today's data gets its river cut through the rim. Today's data is a present-day lake map. The
renders behind the ruling (rivers Task 8) showed four defects:
- false lakes dammed by the 10 km lattice (the Kashmir and Indus valleys filled to about 2,140 m);
- the Amazon at Manaus as a standing lake (a river flat read as a data lake);
- Titicaca missing;
- water walls where a lake ends on a lattice-cell line with ground below its level beyond.

**Files:**
- Create: `tools/bake/timefly_bake/lakemap.py` (+ test in `tools/bake/tests/`), `data/hydrology/lake_map.bin`
- Modify: `data/sources.json`, `crates/timefly_core/src/hydrology/{pass.rs,lakes.rs,levels.rs or a new lakemap.rs,params.rs}`, `crates/timefly_core/src/worldgen/generate/water.rs`, `crates/timefly/src/hydro/mod.rs` (the NodeSource impls)
- Test: `crates/timefly_core/src/hydrology/` tests, `crates/timefly_core/src/worldgen/generate/tests.rs`, `tools/bake/tests/`

**Interfaces:**
- Consumes: Task 3's lake rules and `NodeSource`, Task 6's `lake_tap` use in `water.rs`.
- Produces:
  - `NodeSource::mapped_lake(&self, dir: RenderDir) -> bool`. It defaults to false, and is true only where `data_lakes()` holds and the shipped map has a lake at `dir`.
  - `LAKE_MAP: &[u8]` embedded with `include_bytes!` from `data/hydrology/lake_map.bin`.
  - `OPEN_LAKE_MAP_TOL_M = 10.0` in `params.rs`.
- Task 11's Python mirror must follow the same rules (note in Task 11's brief).

**The map.** Natural Earth 1:10m lakes (`ne_10m_lakes.zip`, about 2.35 MB, public domain), pinned
in `data/sources.json`. Keep features whose `featurecla` is a lake (`Lake`, `Alkaline Lake`) and
drop `Reservoir`: dams aren't in the terrain the game samples, and the game spans the past. The
bake tool rasterises the polygons onto an equirectangular grid at 3′ (7,200 × 3,600, cell centres)
and writes a compact format of the implementer's choosing (for example row run-lengths). Limits:
- the committed file is at most 512 KB;
- decoding needs no new core dependency;
- the lookup is O(1) or O(log row).

**Rules** (only where `data_lakes()` holds, i.e. the terrain is today's; `Reference` eras are unchanged):

```text
mapped surface of a basin = the baked surface level of the mapped lake its nodes overlap
  (the highest, if several; none: unmapped). The bake takes each polygon's level as the median
  of the finest ETOPO cells inside it (the 10 km overview reads the Dead Sea at about −276 m
  against about −430 m).
Table lakes (lake_levels.json): unchanged; they take precedence.
Data flats and depression data lakes: a candidate becomes a lake only if it holds at least
  DATA_LAKE_MIN_NODES mapped nodes. Otherwise no data lake; the water balance decides.
Water-balance lakes:
  - Terminal: unchanged (arid closed basins fill by balance in any era: Mega-Chad, Bonneville).
  - Open, unmapped: breached. No lake; the basin passes its inflow through the spill as rivers
    (the routing already runs through the spill; the footprint is empty, so its nodes become
    river or dry nodes by the normal river rule).
  - Open, mapped, level > mapped surface + OPEN_LAKE_MAP_TOL_M: a Data lake at the mapped
    surface, with footprint = basin nodes below it and outflow = Q_in − evaporation over that
    footprint (the real outlet is a gorge below the lattice).
  - Open, mapped, level within the tolerance: unchanged.
```

**Shore scallops** (core `blended_water`, the WESL mirror, `wesl_water_matches_rust`): standing
water interpolates its level over its wet taps only (a flat L), so the shoreline is the terrain's
own h = L contour, not the zero contour of a level dropping across dry texels (h − 64). Rivers keep
their current blend. Test: `a_lake_shore_follows_the_ground_contour` (core: along a sloping
shore the wet/dry boundary lies within 0.05 texel of where h = L), plus the parity test.

**Water walls** (`water.rs`): a lake's water must not end on a lattice-cell line while the
ground beyond is below its level. The implementer picks the method. For example: let the lake
reach one lattice step past the footprint, decide the shore by the texel's own ground against
L, and raise the ground to just above L where the reach ends while the ground is still below L.
The test below pins the result.

- [ ] **Step 1: Bake the map.**
  - Python test `test_lake_map_round_trips`: decode(encode(raster)) equals the raster. The mask is set at Titicaca (−15.8, −69.4), Victoria (−1.0, 33.0) and the Great Salt Lake (41.1, −112.5), and clear at Manaus (−3.1, −60.0) and Skardu (35.3, 75.6).
  - The baked surface levels: the Dead Sea within 5 m of the fine ETOPO surface there (about −430 m, not the overview's −276), Titicaca within 5 m of 3,812 m.
- Run the bake, commit the file, and pin the source. Report the download's size.
- [ ] **Step 2: Write the failing core tests:**
  - `a_dammed_humid_valley_is_cut_through`: a sloping valley closed by a one-node rim 200 m high, humid, unmapped. Expect no lake, a river chain through the spill, and discharge below the rim equal to the inflow.
  - `an_open_lake_above_its_mapped_surface_drops_to_it`: the same valley with mapped floor nodes whose median sits 50 m below the spill. Expect a Data lake at that median ± 0.01, footprint below it only, outflow = Q_in − evaporation.
  - `a_mapped_open_lake_keeps_its_spill`: a mapped surface within the tolerance of the spill leaves the level unchanged.
  - `an_arid_closed_basin_still_fills_by_balance`: an unmapped Terminal basin keeps its balance lake.
  - `an_unmapped_flat_is_not_a_lake` and `a_mapped_flat_is_a_lake`: a 20+ node elongated river flat.
  - `reference_eras_ignore_the_map`: hydro_golden cases A–C and the no-pass golden hashes are unchanged.
  - `lake_water_never_stands_beside_lower_dry_ground` (generator; proptest over lake levels and footprint edges, including an edge that cuts ground below L): no lake texel at level L has a 4-neighbour that is dry with ground below L − 0.5 m.
  - Run them and watch each fail for the expected reason.
- [ ] **Step 3: Implement** the map lookup, the rules and the wall fix.
- [ ] **Step 4: Check the real data** with the present-day pass (probe binary or `#[ignore]` test on the real overview, as Task 11 will):
  - Titicaca, Victoria, the Dead Sea and the Great Salt Lake within 5 m of the overview there; the Caspian within 5 m of the table.
  - No lake at Manaus (−3.1, −60.0) or Skardu (35.3, 75.6).
  - No lake above 1,600 m on the Kashmir valley floor (34.1, 74.8).
  - Report lake counts and total area before and after.
- [ ] **Step 5: Re-render**, before and after:
  - indus-shaded, a Kashmir view, manaus-shaded, titicaca-shaded and andes-down;
  - a low oblique Andes lake shore;
  - a low oblique Dead Sea shore (31.5, 35.5): an inland lake at about −430 m between steep rift walls. Neither the sea plane nor a wall may show.
- Big-river rapids check (real data, `#[ignore]` probe): reaches above the falls carry the rapid flag; no reach within 50 km of Manaus does.
  - Victoria Falls: the rapid flag within 5 km upstream of (−17.925, 25.857).
  - Iguazu Falls: the rapid flag within 5 km upstream of (−25.695, −54.437).
- [ ] **Step 6: Run the gate and commit** `feat: lakes checked against today's lake map`.

**Amended by controller rulings (2026-11-25, after round 1 on real data):**
- At the present-day snapshot (age 0) the map decides every lake. Each mapped lake stands at its baked surface (table lakes at the table level), with footprint = its own mapped nodes, so several lakes can share a basin and no lattice node need lie below the level. An unmapped closed basin holds no lake, and its rivers end at its sink. Past eras keep the water balance with the rules above.
- In past eras, a lake capped at its mapped surface takes its mapped nodes as its footprint (never "basin nodes below S"), choosing the mapped lake that holds the most basin nodes. Its outlet is among those nodes.
- The Black Sea is sea at 0 m (a strait table, `data/hydrology/straits.json`), not part of a Caspian lake.
- Both falls are dropped from the Step 4 rapids check (the 10 km lattice can't hold their drops). They stay as Task 12 renders.
- The wet-taps-only blend applies to standing water (lake and sea taps) only. Rivers keep their blend.

### Task 9: Boulders, the first instanced scatter

**Files:**
- Create:
  - `crates/timefly/src/scatter.rs`: `ScatterPlugin`, `ScatterSet`, `ScatterInstance`, `ScatterMaterial`, `ScatterPool`.
  - `crates/timefly/src/boulders.rs`: `BoulderPlugin` and `boulder_meshes()`.
  - `assets/shaders/scatter.wesl`.
- Modify:
  - `lib.rs`.
  - `terrain/warmup.rs`: a stand-in boulder on the warm-up layer.
- Test: inline, `terrain/warmup.rs`

**Interfaces:**
- Consumes: `ActiveRegion.grid.boulders` and `frame.dir_to_local`, plus the terrain palette for colour (`palette_image`).
- Produces:
  - `ScatterInstance { translation: Vec3, scale: f32, yaw: f32, tag: u32 }`, with tag = `(wet × 255) << 24 | seed & 0xFF_FFFF`.
  - `ScatterSet { kind: ScatterKind, instances: Arc<[ScatterInstance]> }`, a component per kind that the owner replaces.
  - `ScatterPool`: pooled entities sharing one mesh per shape and one material per kind, so Bevy batches them into instanced draws. Each carries `MeshTag(tag)` and `VisibilityRange` up to `BOULDER_VISIBLE_M = 6_000.0`. The pool grows in chunks of 1,024 and updates at most `SCATTER_UPDATE_PER_FRAME = 2_048` instances per frame.
  - `boulder_meshes() -> [Mesh; 4]`: jittered icospheres, one subdivision (80 triangles), flat normals, positions only plus normals.
  - `scatter.wesl`: per-instance hue jitter from the seed, and wet-darkening by the tag's top byte.

- [ ] **Step 1: Write the failing tests:**
  - `a_scatter_set_reuses_its_pool`: 3,000 instances, then 1,000, then 3,000 give one pool of 3,072, and the hidden slots are `Visibility::Hidden`.
  - `scatter_entities_share_one_mesh_and_material_per_shape`, the precondition for batching.
  - `updates_are_spread_over_frames`: 16,384 instances settle within 8 frames, never over 2,048 per frame.
  - `boulders_follow_the_active_region`: a replace swaps the set, and a teardown empties it.
  - `the_boulder_meshes_are_low_poly_and_closed`: ≤ 80 triangles each, and every edge is shared by two triangles.
  - `wetness_round_trips_through_the_tag`.
  - In `terrain/warmup.rs`: `the_warm_up_draws_the_boulders`.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib scatter:: boulders:: terrain::warmup`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Re-render the Indus gorge from Task 8, Step 5.** Pass: rocks in the white water, wet-dark at the waterline, never floating or buried by more than half.
- [ ] **Step 6: Run the gate and commit** `feat: instanced boulders in rapids`.

### Task 10: Benches

**Files:**
- Modify:
  - `bench/region/mod.rs`:
    - `BENCH_REGIONS` gains, after the Himalayas, `BenchRegion { name: "Indus tributaries", arg: RegionArg { lat_deg: 32.5, lon_deg: 74.5, years: 0.0 }, pan_from_previous: false }`.
    - The per-region generation figure becomes `millis − hydro_wait_ms`, with the wait on its own line.
    - It reads `HydroPassFinished` and adds summary lines.
  - `bench/journey/mod.rs`: the same pass lines.
  - `bench/mod.rs`: `pass_line(..)` is shared.
  - Create `crates/timefly/src/diagnostics/gpu_time.rs`: GPU frame time from pass-level timestamps *(owner, 2026-11-25)*.
- Test: `bench/region` tests, `tests/region_bench.rs`, `tests/journey_bench.rs`, `gpu_time.rs` inline, `tests/region_gpu.rs` (ignored GPU test)

**Interfaces:**
- Consumes: Task 4's `HydroPassFinished` and `RegionGenerated.hydro_wait_ms`.
- Produces:
  - Summary lines `continental pass (<age>): <wall_ms> ms wall, <cpu_ms> ms CPU, <slices> slices on <native|web> (target <= 1000|5000 ms, reported)`.
  - `generation_ms(g: &RegionGenerated) -> u32`, which the warm-generation and first-entry gates now use.
  - **GPU frame time** *(owner, 2026-11-25)*.
    - **Why Bevy's plugin can't do it.** `RenderDiagnosticsPlugin` writes timestamps inside encoders, which needs `TIMESTAMP_QUERY_INSIDE_ENCODERS`. Apple-silicon Metal and WebGPU don't offer that, so the plugin records CPU time only there.
    - **What this task adds instead.** `GpuTimePlugin` adds two empty render-graph nodes: `GpuTimeBegin`, first in the graph, and `GpuTimeEnd`, last. Each opens a render pass with no attachments' work, or a trivial clear of a 1 × 1 target, whose `timestamp_writes` write the frame's begin and end timestamps into a 2-entry `QuerySet`.
    - **Readback.** The plugin resolves the set into a buffer and maps it asynchronously. It reads the result a few frames later, ring-buffered over 4 frames, and never stalls the frame.
    - **When it runs.** Only when the device has `Features::TIMESTAMP_QUERY`; it requests that feature only if the adapter offers it. Otherwise it is a no-op, and the summaries say `gpu time: unavailable`.
    - **The resource.** `GpuFrameTime { last_ms: Option<f32>, period_ns: f32 }` (`Resource`, main world).
    - **Convention.** The frame's GPU time is `(end − begin) × period`, because WebGPU timestamps are in ns. A frame whose end precedes its begin, or whose value is not finite, is dropped and counted.
    - **Benches.** Both benches report median, p95 and worst GPU frame time per segment or route next to the CPU frame times, reported only and not gated. For example: `gpu frame time (Indus tributaries): median 4.12 / p95 5.80 / worst 9.31 ms over 1,820 frames (timestamp-query)`.
    - **Chrome.** Chrome exposes `timestamp-query` but rounds it to 100 µs unless the "WebGPU Developer Features" flag is on. Note in the docs which applies to a run.
    - **Measurements to make.** With it, measure the cloud's and the river water's GPU cost by differencing scenes (`region_tour`: cloud off, at 0.5 and at 1.0; water on the Indus with and without rivers). Record them in `docs/testing/rivers/performance.md`, replacing M3's ioreg estimate of the cloud with a measured one.

- [ ] **Step 1: Write the failing tests:**
  - `bench_regions_include_the_indus_tributaries`.
  - `generation_excludes_the_pass_wait`.
  - `the_region_bench_reports_the_continental_pass` (`region_bench`, headless on the tiny assets): the summary has a `continental pass (0 ka)` line.
  - `a_journey_run_reports_the_continental_pass` (`journey_bench`).
  - The existing `the_region_bench_path_never_triggers_a_hop` covers the new region.
  - `gpu_time` units:
    - `frame_ms_from_timestamps` converts with the period and drops reversed or non-finite pairs.
    - The readback ring never hands out a slot still being mapped.
    - Without `TIMESTAMP_QUERY`, the plugin adds no nodes and reports `unavailable`.
  - `the_gpu_frame_time_is_measured_on_this_gpu` (`region_gpu`, ignored): on a headless device with the feature, a frame that draws a known number of full-screen passes reads a positive GPU time, which grows with the number of passes.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --test region_bench --test journey_bench`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests to check they pass.**
**Carried from earlier tasks (controller rulings, 2026-11-25):** record each of these in `docs/testing/rivers/performance.md`, with its pacing (median frame time):
- **Native hop memory, display-paced, two runs.** Judge run 2 against `HOP_MEMORY_LIMIT_MIB` (64). Task 7 measured +60.7 MiB with a 3.3 MiB margin. Task 8's fix round ran unpaced only (+48.7). Include a hop from a boulder-free region into a rapids region: Task 9's boulder pool only grows, by about 14 MiB from empty to full (estimated). If the gate fails, trim the pool on a replace or stage it like the water.
- **Web costs, from the owner's Chrome run:**
  - boulder placement on the swap frame (0.52 ms native for 2,058; up to 16,384);
  - the continental pass's wall time (0.89 → 1.40 s native dev after Task 8b);
  - the largest present-day overlay unit (`Today::place`) against `WEB_HYDRO_BUDGET_MS`;
  - the 2.7 ms apron prep step (Task 6) against the 6 ms slice.
  Time-slice whatever overruns.
- **The water's GPU cost** on WebGPU over dry land and over rivers (Task 8 measured native only). Check that distant thin rivers stay visible (Task 8 minor).
- **Tile-count growth** from the river read margin (Task 5), and the region CPU footprint (+15 MiB per region, Task 6).

- [ ] **Step 5: Run the gate and commit** `feat: benches report the continental pass and fly the Indus tributaries`.

### Task 11: The reality check

**Files:**
- Create:
  - `tools/bake/timefly_bake/hydro_ref.py` (`timefly-hydro-ref`) and `tools/bake/tests/test_hydro_ref.py`.
  - `crates/timefly/tests/fixtures/hydro_reference/indus.json` and `amazon.json`.
  - `crates/timefly_core/src/hydrology/compare.rs`.
  - `crates/timefly/src/hydro_check.rs`.
  - `crates/timefly/tests/hydro_check.rs`.
- Modify:
  - `tools/bake/pyproject.toml` (the script entry).
  - `args.rs` (`--hydro-check[=OUT_DIR]`, hidden, native).
  - `main.rs` and `lib.rs`.

**Interfaces:**
- Consumes: the golden fixture and the climate table (Tasks 2–3). The real 0 ka climate grid (Task 1, Step 5, the controller's fetch; until then only the tests run). The raw ETOPO 1′ at `/Users/adsouza/Code/time-fly/data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc`.
- Produces, in Python:
  - `LatLonGraph(lat_n, lon_w, rows, cols, per_deg)`, in the same neighbour order, with haversine distances and `R²·Δλ·(sin φ₁ − sin φ₂)` areas.
  - `route(graph, elevation, ocean)` and `resolve_lakes(...)`, mirroring the Rust rules line for line. Box-edge nodes are extra seeds, so water may leave the box.
  - `reference(box, etopo_path, climate_dir, table_path) -> dict`.
  - `BOXES = {"indus": (37.5, 66.0, 23.0, 82.0), "amazon": (6.0, -80.0, -21.0, -48.0)}` as (lat_n, lon_w, lat_s, lon_e).
- Produces, as the fixture JSON: `{"box": […], "links": [{"lonlat": [[lon, lat], …], "area_km2": [...], "elevation_m": [...], "discharge_m3s": f}]}`.
  - Links run between confluences, with area ≥ 10,000 km² and discharge ≥ `RIVER_MIN_DISCHARGE_M3S`.
  - They are simplified to 1 km (Ramer–Douglas–Peucker), and both files total ≤ 1 MB.
- Produces, in `compare.rs`:
  - `Agreement { share_within: f64, matched_links: u32, links: u32, worst_km: f64 }`.
  - `centreline_agreement(game: &[Vec<DVec3>], reference: &[RefLink], tolerance_km: f64, filter: impl Fn(&RefPoint) -> bool) -> Agreement`. It resamples the reference every 1 km and takes the great-circle distance to the game's polylines. A link matches when ≥ 50% of its length is within tolerance.
- Produces `--hydro-check[=OUT_DIR]`, which loads the bake headless and runs the pass at 0 ka (timed). Its bars:
  - **Indus:** game rivers with area ≥ 10,000 km² within 10 km for ≥ 80% of the reference's length, and every link matched.
  - **Amazon flats** (reference elevation < 300 m): within 25 km for ≥ 80%.
  - **Lakes:** at the Caspian (42.0, 50.5), the Dead Sea (31.5, 35.5), Titicaca (−15.8, −69.4), the Great Salt Lake (41.1, −112.5) and Victoria (−1.0, 33.0), `lake_tap(..).level_m` is within 5 m of the overview's elevation there.
    Task 8b amends this: mapped lakes compare against their baked fine-data surface level, not the overview (the overview reads the Dead Sea at about −276 m), and the Python mirror follows Task 8b's map rules (breached unmapped Open lakes, map-gated data flats, the mapped-surface cap).
    Task 8b's rulings, which the mirror must also follow:
    - At age 0 the map decides every lake (the present-mode overlay, `hydrology/today.rs`). Each mapped lake stands at its baked surface, or the table level for table lakes, over its own mapped nodes. Several lakes may share a basin. An unmapped closed basin holds no lake, and its rivers end in `Down::Sink`.
    - In past eras a capped lake's footprint is its mapped nodes only, with its outlet picked among them.
    - Straits (`data/hydrology/straits.json`, the Bosporus) join the Black Sea to the ocean.
  - **Major river outlets** (controller ruling, 2026-11-25): at the 10 km lattice a narrow gorge can close, sending a river out of its basin by the wrong pass. Task 8b found the Danube leaving the Pannonian basin through the Moravian Gate, not the Iron Gate. Check that these rivers reach their real outlets: the Danube through the Iron Gate (44.67, 22.5) to the Black Sea; the Jhelum out of the Kashmir valley at Baramulla (34.2, 74.35); the Indus and the Amazon to their seas. Fix any misroute with a gorge entry in the strait table, generalised to passes, with a cited source, and a test per entry.
  - **Arid basins:** no lake ≥ `LARGE_LAKE_KM2` where the basin's mean precipitation < 250 mm/yr, in either network inside the boxes.
  - It writes `hydro-check-{indus,amazon}.svg` (reference blue, game red, equirectangular, a 100 km scale bar) and prints the numbers. It exits 0 on PASS and `FAILED_RESULT_EXIT_CODE` on FAIL.

- [ ] **Step 1: Write the failing tests:**
  - `test_golden_fixture` (Python): `route` and `resolve_lakes` reproduce `hydro_golden.json` cases A–C exactly.
  - `test_latlon_neighbour_order_matches_rust`.
  - `test_reference_on_a_synthetic_box`: a 1° box with two valleys gives two links and their confluence.
  - `test_reference_json_schema`.
  - In `compare.rs`: `agreement_counts_length_within_tolerance`, `a_link_matches_with_half_its_length`, and `flats_take_their_own_filter`.
  - `hydro_check_runs_on_the_tiny_bake` (`tests/hydro_check.rs`, with a tiny reference fixture in `tests/assets`): the summary lines and exit code 0.
- [ ] **Step 2: Run them to check they fail.** Run `uv run pytest -q tests/test_hydro_ref.py`, `cargo test -p timefly_core hydrology::compare` and `cargo test -p timefly --test hydro_check`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Bake the fixtures (needs Task 1, Step 5).**
  - Run `cd tools/bake && uv run timefly-hydro-ref --box indus --etopo /Users/adsouza/Code/time-fly/data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc --climate /Users/adsouza/Code/time-fly/crates/timefly/assets/data/climate --out ../../crates/timefly/tests/fixtures/hydro_reference/indus.json`, then the same with `--box amazon`.
  - Expected: each finishes in under 10 minutes, and the two files total ≤ 1 MB.
- [ ] **Step 6: Run the check.**
  - Run `TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT="$ROOT" target/release/timefly --hydro-check=<scratch>`.
  - Expected: the numbers and the SVGs. Record them in `docs/testing/rivers/reality-check.md`.
  - If a bar fails, the constants are the builder's to tune (corridor, discharge threshold, runoff table). Ledger each change. A bar that still fails goes to the owner, unchanged.
- [ ] **Step 7: Run the gate and commit** `feat: the reality check against a 1′ reference pass`. Stage the Python, both fixtures, `compare.rs`, `hydro_check.rs` and its test, `args.rs`, `main.rs`, `lib.rs` and `docs/testing/rivers/reality-check.md` with its SVGs.

### Task 11b: Today's major river courses, burnt into today's terrain

Owner, 2026-11-26: "Burn real courses". The reality check's Indus box fails (76.4% within 10 km against ≥ 80%) mostly because the lattice routes the lower Indus about 90 km east of Sehwan across the flat Sindh plain, where the river really runs on its own alluvial ridge. Straits and gorges were not meant for plains. This task burns Natural Earth's river centrelines into the pass's routing wherever the terrain is today's.

**Files:**
- Create:
  - `tools/bake/timefly_bake/rivers_today.py` (`timefly-bake rivers-today`) and `tools/bake/tests/test_rivers_today.py`.
  - `data/hydrology/rivers_today.json` (the baked table, committed, compiled in).
  - `crates/timefly_core/src/hydrology/today_rivers.rs`.
- Modify:
  - `tools/bake/timefly_bake/cli.py` (the subcommand).
  - `data/sources.json` (pin the raw file) and `data/README.md` (provenance, licence).
  - `hydrology/params.rs`, `hydrology/mod.rs` and `hydrology/pass.rs` (the burn stage).
  - `docs/testing/rivers/reality-check.md` (the re-run, with the burn disclosed).

**Interfaces:**
- Consumes:
  - The raw file, already fetched by the controller: `/Users/adsouza/Code/time-fly/data/raw/naturalearth/ne_10m_rivers_lake_centerlines.geojson`, Natural Earth 1:10m "Rivers + lake centerlines" v5.1.2, from `https://raw.githubusercontent.com/nvkelso/natural-earth-vector/v5.1.2/geojson/ne_10m_rivers_lake_centerlines.geojson`, 7,307,743 bytes, sha256 `bb854a900ecbd3b408df46d5e16e3e0f974ba55993f9d8b5c26e855273c0905a`, public domain. It holds 1,455 features (1,202 `River`, 253 `Lake Centerline`) with `scalerank` 0–10.
  - ETOPO 2022 60″ (`/Users/adsouza/Code/time-fly/data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc`), to orient each line.
  - Task 8b/11's strait and gorge burn (`straits.rs`, `HydroPrep::burn_straits`), and Task 10's resumable stages and the `continental pass done` log's `longest_step_stage`.
- Produces:
  - The bake: `select(features) -> list[polyline]` keeps `featurecla == "River"` with `scalerank <= TODAY_RIVER_MAX_SCALERANK` (7), each part of a MultiLineString its own polyline; `orient(polyline, etopo)` puts it upstream first, by the lowest ETOPO cell within 2′ of each end (ties keep Natural Earth's order); `simplify(polyline, 1 km)` (Ramer–Douglas–Peucker in metres). Coordinates are rounded to 0.001°.
  - The table: `{ "comment", "source", "rivers": [{ "name", "scalerank", "points": [[lat, lon], …] }] }`, upstream first, ≤ 1 MB (the controller's estimate: about 1,275 polylines, 52,800 points, 0.87 MB).
  - Core: `TodayRiver { name, points }`, `parse_today_rivers(&str) -> Result<Vec<TodayRiver>, String>`, `shipped_today_rivers()`. `today_river_nodes(lattice, river) -> Vec<u32>`: the nearest nodes to points a quarter of the mean spacing apart, consecutive neighbours, no repeats (as `strait_levels` does, without its O(n²) search).
  - `params.rs`: `TODAY_RIVER_BURN_M = 10.0`.
  - The burn, in every era with today's terrain (`data_lakes`, the same gate as the gorges), after the straits: along each river's nodes, upstream first, the level is the running minimum of the era's own node elevations, and the node's routing elevation becomes `level − TODAY_RIVER_BURN_M`.
    - Routing only: the river graph's bed elevation along a burnt course is the level, not `level − TODAY_RIVER_BURN_M`, so the regions carve no 10 m trench.
    - Ice nodes are not burnt. The burn never changes which nodes are ocean.
    - Reference (PaleoDEM) eras burn nothing.

**Rulings carried in (controller, 2026-11-26):**
- Rivers only, rank ≤ 7: rank ≤ 7 includes the Jhelum, the reality check's third-longest miss; lake centrelines are left out because today's lakes are the map's (Task 8b). Neither the Shyok nor the Arghandab is in Natural Earth 1:10m, so the box's "every link matched" bar will still fail on them; report it.
- The Python reference does not burn: it stays a 1′ DEM reference, independent of Natural Earth. The check then compares two independent sources of today's rivers.
- The new committed table is shipped data beyond the Global Constraints' climate grids; ≤ 1 MB.
- Every era with today's terrain, not 0 ka only: today's course is a better guess for the last 26 kyr than the lattice's route across a plain. Cost if wrong: an ice-age river follows today's course where it then ran elsewhere.

- [ ] **Step 1: Write the failing tests:**
  - Python: `test_select_keeps_ranked_rivers_only` (a lake centreline and a rank-8 river are dropped, a MultiLineString gives one polyline per part), `test_orient_puts_the_higher_end_first` (a synthetic grid), `test_simplify_stays_within_a_kilometre` (every dropped vertex within 1 km of the kept line), `test_table_schema_and_size`.
  - Core, `today_rivers.rs`: `the_shipped_table_parses` (≥ 1,000 rivers; the Indus present; ≤ 1 MB), `river_nodes_run_node_to_node`, `the_lower_indus_runs_upstream_first` (the Indus's first point lies north of its last).
  - Core, `pass.rs`:
    - `a_burnt_course_routes_a_plain_river_along_its_line`: a synthetic plain where the unburnt route leaves the line (the line on a 3 m ridge); with the table, every line node drains to the next.
    - `a_burnt_course_never_rises_downstream`.
    - `the_burn_steers_routing_only`: the river graph's bed along the line is the running-minimum level.
    - `a_burnt_coastal_plain_stays_land`: a line ending at a coast 2 m above the sea changes no ocean flag.
    - `ice_nodes_and_reference_eras_are_not_burnt`.
    - `the_time_boxed_pass_matches_run_parallel_with_todays_rivers`: bit for bit, as Task 10's equality test, with the shipped table.
- [ ] **Step 2: Run them to check they fail.** `cd tools/bake && uv run pytest -q tests/test_rivers_today.py`; `cargo test -p timefly_core hydrology::`.
- [ ] **Step 3: Implement.** No time-boxed step may grow past Task 10's bound: parsing the table and finding its nodes are done in resumable blocks (or before the pass, off the frame), and the node lists may be cached per lattice `n` for later passes.
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Bake the table:** `cd tools/bake && uv run timefly-bake rivers-today --geojson /Users/adsouza/Code/time-fly/data/raw/naturalearth/ne_10m_rivers_lake_centerlines.geojson --etopo /Users/adsouza/Code/time-fly/data/raw/etopo/ETOPO_2022_v1_60s_N90W180_surface.nc --out ../../data/hydrology/rivers_today.json`. Expected: ≤ 1 MB, and the counts printed.
- [ ] **Step 6: Re-run the reality check** as Task 11, Step 6 (the `dev` profile is acceptable, as Task 11 recorded). Expected: the lower Indus follows Sehwan, and the Indus box's share passes 80%; the Shyok and the Arghandab still unmatched. Record every bar before and after in `reality-check.md`, the pass time at 0 ka and the longest step (≤ 2 ms native), with the burn disclosed under "Choices made". If the lower Indus still leaves its line, try `TODAY_RIVER_BURN_M` = 20 and record both. Any bar that gets worse goes to the owner, unchanged.
- [ ] **Step 7: Run the gate and commit** `feat: today's major river courses burnt into today's terrain`. If a region golden hash changes, the commit message says which region and why (a burnt course crosses it).

### Task 11d: The world's major river systems

Owner, 2026-11-26: "We should do sanity checks on other major river systems of the world (e.g. Ganges, Yangtze, Yellow, Nile, Mississippi, Congo, Mekong, Hudson, Yenisei, Niger, Orinoco, Brahmaputra, Parana, Ob, Pearl, etc.) too." The Indus and Amazon boxes test centrelines against a 1′ reference; with Task 11b's courses burnt in, a centreline test against Natural Earth would be circular. What can still go wrong is the network: a river leaving its basin by the wrong pass (the Danube through the Moravian Gate), ending in the wrong sea, stopping in a pit, or carrying the wrong water. This task extends the outlet check to the world's major rivers and adds a discharge sanity table.

**Files:**
- Modify: `crates/timefly/src/hydro_check.rs` (and its tests), `docs/testing/rivers/reality-check.md`, `tools/bake/timefly_bake/rivers_today.py` and `data/hydrology/rivers_today.json` (canals out), and `data/hydrology/straits.json` only for a gorge a misroute needs.

**Interfaces:**
- Consumes: Task 11's outlet bar (start within `OUTLET_START_KM`, each via within `OUTLET_VIA_KM`, the ocean within `OUTLET_SEA_KM` of the mouth) with Task 11b's cited points, and the pass's per-node discharge.
- Produces:
  - Outlet entries, each point with a cited source (English Wikipedia coordinates are acceptable, read date given; a paper or gazetteer better), for at least: the Ganges (via Varanasi, Farakka), the Brahmaputra (from the Yarlung Tsangpo above the Great Bend, via Pasighat and Guwahati), the Yangtze (via the Three Gorges at Yichang, Wuhan), the Yellow River (via Lanzhou, Zhengzhou), the Pearl (via Wuzhou), the Mekong (via Phnom Penh), the Nile (from Jinja, via Khartoum and Aswan), the Congo (via Kisangani, Kinshasa), the Niger (via Timbuktu, through the inner delta, and Lokoja), the Mississippi (via St. Louis, New Orleans), the Hudson (via Albany), the Yenisei (via Krasnoyarsk), the Ob (via Novosibirsk), the Orinoco (via Ciudad Bolívar), the Paraná (via Rosario), plus the Lena, the Volga (to the Caspian: its "sea" is the Caspian lake) and the Mackenzie. A river whose real end is a lake or an inland delta ends there.
  - A discharge table (informative, not a bar): at one cited gauge per river, the game's discharge against the published long-term mean (cited), with the ratio. Ratios outside [1/3, 3] are flagged in the doc for the owner.
  - Canals out of the burn: Natural Earth files the Suez Canal (and possibly others) as `River`. The bake drops features that are artificial channels (name containing "Canal", plus any others found, listed in the doc).

- [ ] **Step 1: Write the failing tests:** `the_major_rivers_have_cited_outlets` (every entry has a source string and every point lies on land within its tolerance of the burnt course or the lattice river), `the_discharge_table_flags_ratios_outside_three` (unit), and in Python `test_select_drops_canals`.
- [ ] **Step 2: Run them to check they fail.**
- [ ] **Step 3: Implement,** re-bake `rivers_today.json` without canals.
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Run the check** (dev profile acceptable). Record every river's outlet margins and discharge ratio in `reality-check.md`. For each failing outlet, find the cause (a closed gorge, a burnt course's gap, a pit) and fix a narrow pass with a cited gorge entry, as Task 11 did; anything else goes to the owner unchanged with its numbers and an SVG or render of the misroute.
- [ ] **Step 6: Run the gate and commit** `feat: the reality check follows the world's major rivers to their mouths`.

### Task 11f: Lakes baked at their bed

Owner, 2026-11-26: "Table entries now". Task 11d's lake-map audit found the lake map baked at the bed for Baikal (−328 m against 455.5 m), Superior (24.1 against 183), Michigan (96.4 against 176), Huron (124.7 against 176) and Ontario (−4.4 against 74), with Erie probably too (155 against 173). Their footprints also hold bed-level fragments that are separate data lakes (384 at Baikal, 375 at Superior, 376, 377, 15) and take river flow (384 took 3,597 m³/s). Baikal, Superior and Ontario already stand at their table levels; Michigan, Huron and Erie would be drawn 20–80 m low. The proper fix (re-baking the lake map from water surfaces) is a follow-up after this milestone.

**Files:** `data/hydrology/lake_levels.json` (the level table), `crates/timefly_core/src/hydrology/{levels.rs,today.rs,lakemap.rs}` as needed, `tools/bake/timefly_bake/hydro_ref.py` (the mirror reads the same table), `docs/testing/rivers/reality-check.md`.

**Interfaces and decisions:**
- **Table entries.** Add Michigan, Huron and Erie to the level table, each with a cited surface (e.g. NOAA GLERL or the US Army Corps of Engineers long-term mean, or Wikipedia's infobox with its cited source) and its point. Check the existing Baikal, Superior and Ontario entries and cite them if they lack sources. Michigan and Huron are hydraulically one lake (the Straits of Mackinac); decide one entry or two, and justify.
- **Fragments.** Every lake-map lake whose footprint lies within a table lake's basin, or adjoins the table lake's footprint, at a baked surface below the table level takes the table lake's level and joins it, so no bed-level fragment is a separate data lake with its own flow. Decide the joining rule concretely and test it.
- **The audit line and the lake bar.** After the fix, the audit line no longer flags the bed-baked lakes (it may still list the small mountain lakes); add Michigan/Huron and Erie to the reality check's lake bar (within 5 m of the table).
- **No re-bake of the shipped lake map.**

- [ ] **Step 1: Write the failing tests:** `the_great_lakes_stand_at_their_table_levels` (Michigan, Huron, Erie within 5 m), `a_bed_fragment_joins_its_table_lake` (a fixture with a table lake and a bed-level fragment: one lake, at the table level, no separate flow), and the Python mirror's equivalent.
- [ ] **Step 2: Run them to check they fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run them to check they pass.**
- [ ] **Step 5: Re-run the check.** Record the lake bar, the audit line, every outlet (unchanged or better) and the lake-dump diff in reality-check.md.
- [ ] **Step 6: Run the gate and commit** `fix: the Great Lakes and Baikal stand at their table levels, fragments included`.

### Task 11g: No tributary drains into a burnt course across a ridge

Owner, 2026-11-26: "Both, pass rule first". Task 11e's probes showed the Hells Canyon gash comes from the pass: the burnt Snake trench is the lowest ground within a lattice spacing, so the Salmon's node (45.480, −116.362) drains west into the burnt node (45.509, −116.536) at 119 m³/s across an unbroken ridge (lowest crest within ±1 spacing 1,492 m against water at 621–731 m); the real Salmon joins the Snake about 40 km north. Of 4,645 joining links at 0 ka, 81 cut more than 500 m. No region-side search can fix those; this task stops the pass making them. Task 11e's corridor search follows for the shallower joins.

**Files:** `crates/timefly_core/src/hydrology/{route.rs,today_rivers.rs,params.rs}` (and the pass's stage plumbing), `docs/testing/rivers/reality-check.md`.

**Interfaces and decisions:**
- **The rule.** A node that is not on a burnt course may not take a burnt course node as its receiver (nor be reached from it in the flood) when the terrain along the step between them climbs more than `JOIN_RIDGE_M` above the higher of the two nodes' unburnt elevations. Such a step is treated as absent for that pair; the node drains the way its own land falls.
- **What "the terrain along the step" is.** The pass reads the 5′ overview, not the fine tiles. Step 1 first measures whether the overview resolves the ridges in question (the Hells Canyon pair and a sample of the 81 deep joins): sample the overview along each step at overview-texel spacing and report the crest against the end heights. If the overview does not resolve them, stop and report (NEEDS_CONTEXT) with the numbers.
- **Determinism.** The ridged steps are a function of the lattice, the overview and the shipped course table: compute them once per lattice with the course nodes (cached as the course node lists are), in resumable blocks, and keep the time-boxed pass equal to `run_parallel` bit for bit.
- **`JOIN_RIDGE_M`** in params.rs, chosen from the measurements (justify: low enough to catch the 81, high enough not to cut joins through gentle saddles).

- [ ] **Step 1: Measure** (no rule yet): the overview crest along the Hells Canyon step and the 81 deepest joins; report.
- [ ] **Step 2: Write the failing tests:** `a_tributary_never_drains_into_a_course_across_a_ridge` (fixture: a ridge between a plateau node and a burnt trench; the node drains its own way), `a_tributary_through_a_saddle_still_joins` (a low saddle below the threshold keeps the join), the time-boxed equality on a fixture with ridged steps, and a real-table property: no joining link's step crest exceeds `JOIN_RIDGE_M` above its ends.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Re-run the reality check** (every bar before and after, the lake dump) and the joining-link census (count over 50 m and over 500 m before and after); render Hells Canyon (`region_tour <out> 1920x1080 45.28,-116.59,0 hells high 6000`): pass when the ridge gash is gone and the Salmon reaches the Snake its own way.
- [ ] **Step 6: Run the gate and commit** `fix: no tributary drains into a burnt course across a ridge`.

### Task 11e: Tributaries find their canyons

Owner, 2026-11-26: "Tributaries find canyons". With today's courses burnt in (Task 11b), a burnt canyon is the lowest ground for a lattice spacing around, so plateau tributaries drain into it, which is right in kind. But the region refines the joining link as a node-to-node chord, and where a fine ridge stands between the tributary's node and the canyon, the incision carves a gash through it: in Hells Canyon, 826–1,146 m deep and 375–875 m wide. This task routes such a link down the fine terrain instead.

**Files:**
- Modify: `crates/timefly_core/src/worldgen/generate/rivers.rs` (and a new focused module beside it if the search grows past about 300 lines), `hydrology/params.rs`, `docs/testing/rivers/reality-check.md` or a new `docs/testing/rivers/canyons.md` with the renders.

**Interfaces:**
- Consumes: Task 5's refinement (sub-points, `relax_river_node`, the incision bed), Task 11b's anchors (`EraHydrology` anchors, `relax_reach_m`, `river_tile_margin_m`).
- Produces:
  - A joining link (a link whose downstream node is burnt and whose upstream node is not) is refined along the least-cost path through the field's source level, from the upstream node's relaxed point to the downstream node's anchored point.
    - The cost per step is the step's length × (1 + `JOIN_PATH_CLIMB_WEIGHT` × the height the step stands above the straight line between the two ends' levels, clamped at 0).
    - The search runs over a lattice that is a function of the link alone (its two end directions, in f64), not of the region: points every `SUBPOINT_M` along and across the chord, out to `JOIN_PATH_HALF_WIDTH_FRACTION` × the lattice's mean spacing on each side.
    - So every region that sees the link finds the same path, bit for bit.
  - The path replaces the chord's sub-points, and the incision bed follows it (still never rising downstream).
  - Links that are not joining links are unchanged: the region golden hashes for regions without burnt courses stay the same.
  - New constants in `params.rs`: `JOIN_PATH_CLIMB_WEIGHT` and `JOIN_PATH_HALF_WIDTH_FRACTION`, values chosen and justified by the tests and the Hells Canyon render. The tile margin grows if the corridor needs it, and is counted.
  - Generation cost: region generation stays within `EXPECTED_GEN_S_NATIVE` and `EXPECTED_GEN_S_WEB` (measure the Hells Canyon and Indus tributaries regions before and after), and no time-boxed slice grows past the web budget.

- [ ] **Step 1: Write the failing tests:**
  - `a_tributary_joins_down_its_side_valley`: a synthetic plateau with a burnt canyon, a tributary node behind a 1,500 m ridge, and a side valley around the ridge's end. The refined path stays in the side valley (max height above the water < 50 m) and ends at the canyon.
  - `a_joining_path_never_climbs_more_than_the_chord`: on any field, the path's cost ≤ the chord's.
  - `joining_paths_agree_bit_for_bit_across_regions`: two regions whose edges cut the corridor differently, same path where they overlap. Prove the hazard: the path leaves the chord by more than a texel inside both.
  - `links_that_do_not_join_a_course_are_unchanged`: an unburnt network refines exactly as before.
- [ ] **Step 2: Run them to check they fail.**
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Re-render Hells Canyon** (`region_tour <out> 1920x1080 45.28,-116.59,0 hells high 6000`) and the Indus tributaries region (32.5, 74.5). Pass: no ridge gash; tributaries reach the Snake through side valleys. Record the generation times and commit the renders as JPG.
- [ ] **Step 6: Run the gate and commit** `feat: tributaries join a burnt course down their own valleys`.

### Task 11c: White water that sloshes over rocks

Owner, 2026-11-26: "add a white-water look pass". The design's intent (the owner's words): "In places where there are rapids it should look like the water is sloshing over rocks." Today the rapids read as a flat cream stripe: isotropic broken foam scrolled downstream, and at most 6 boulders per 500 m sub-segment, mostly under the surface.

**Files:**
- Modify:
  - `crates/timefly_core/src/hydrology/params.rs` and `worldgen/generate/water.rs` (denser, clustered boulders; foam sites).
  - `crates/timefly/src/boulders.rs`, `scatter.rs`, `water.rs` (foam collars as a second scatter kind).
  - `assets/shaders/water.wesl` and `assets/shaders/scatter.wesl`.
  - `docs/testing/rivers/performance.md` (the GPU cost) and `docs/testing/rivers/*.png` (before and after).

**Interfaces:**
- Consumes: Task 6's `boulder_sites`/`boulder` (sub-segments with `rapid`, `half_width_m`, the hash cells), Task 8's water texture (level, thickness, ribbon, flow; speed ≥ 3 m/s marks rapids), Task 9's `ScatterPool`/`ScatterKind`, and Task 10's GPU pass timer (`GpuFrameTime`, per-pass readings).
- Produces:
  - **Rocks:** rapids hold clusters, not a sprinkle. `BOULDERS_PER_SUBPOINT_MAX` = 24; each sub-segment's boulders fall in clusters of up to 6 around hashed centres, members within 3 cluster radii, the largest at the centre; at `rapid` = 1 at least a third of a cluster's members break the surface (top above the level). `MAX_BOULDERS` may rise to 32,768 if Task 12's hop memory stays under the gate; ledger the number. Boulders stay wholly in the channel and remain a function of direction plus the pass (bit for bit across overlaps).
  - **Foam collars:** `ScatterKind::FoamCollar`, one per boulder whose top lies within 0.5 m below to above the water level: a flat ring at the water level, stretched downstream into a wake about 4 radii long along the local flow, its edge broken by noise and churning with time, alpha-masked (no sorting). It applies the water surface's own depth pull, so the water never hides it.
  - **The water's white water** (`water.wesl`, rivers only, `river_w` gated, lakes and the sea unchanged):
    - Standing waves in rapids: a normal-only pattern that stays put while the foam moves through it, crests across the flow at the hydraulic wavelength λ = 2π·v²/g (v the texel's speed), its amplitude growing with `rapid`.
    - Foam streaked along the flow (noise stretched about 4:1 along it), gathering on the standing waves' crests, with clumps that break and re-form; still fading to its mean where unresolved, so rapids read white from afar.
  - **Cost:** the water and scatter passes together add at most 1.0 ms of GPU time at 2560×1440 on the M4 in the Indus tributaries region (Task 10's GPU timer, interleaved medians against the parent commit). Over budget, cut the collars' reach first, then the standing waves' octaves; ledger each.

- [ ] **Step 1: Write the failing tests:**
  - Core: `rapids_hold_clustered_boulders` (counts at `rapid` 1 and 0.5; every member within 3 cluster radii of its centre), `a_third_of_a_full_rapids_rocks_break_the_surface`, `clustered_boulders_stay_in_the_channel`, `foam_sites_mark_rocks_at_the_surface` (each site carries its level and flow direction), and the existing cube-corner agreement extended to foam sites.
  - Shell: `collars_follow_the_active_region` (replace and teardown), `collars_share_one_mesh_and_material`, `collar_sets_reuse_their_pool`.
  - Shader: the WESL constants mirrored from Rust stay equal (the existing mirror test pattern), and the water and scatter modules still compile for WebGPU (`cargo check -p timefly --target wasm32-unknown-unknown` plus the existing shader-load test).
- [ ] **Step 2: Run them to check they fail.** `cargo test -p timefly_core worldgen::generate`; `cargo test -p timefly --lib boulders:: scatter::`.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Render before and after** (the parent commit's binary copied out first) with `region_tour` at the Indus gorge close-up of Task 8, Step 5, 3840×2160 cropped, at about 150 m above the water, plus one Amazon and one lake frame to show they are unchanged. Pass: rocks break the surface in clusters, water piles white on their upstream faces and trails downstream, standing waves hold still while the foam runs through them; the lake and the Amazon frames match the parent's.
- [ ] **Step 6: Measure the GPU cost** as above and record it in `performance.md` with the commands.
- [ ] **Step 7: Run the gate and commit** `feat: white water that sloshes over rocks`.

### Task 12: Verification and documentation

**Files:**
- Create:
  - `docs/testing/rivers/performance.md`.
  - `docs/testing/rivers/manual-checks.md`.
  - `docs/testing/rivers/*.png`: Task 8's renders, at present on Metal and in Chrome.
- Modify:
  - This plan's appendix (the rulings made while building, copied from the ledger).
  - The parent spec's §7.5 and §14 (rivers and lakes done; the climate source).

- [ ] **Step 1: Native runs** on the M4 at 2560×1440: `--region-bench` twice and `--journey-bench` twice, each with a fresh `TIMEFLY_LOG_DIR` and `$ROOT`. Record:
  - The adapter, the backend, the commit and every summary line.
  - Each era's continental pass (target ≤ 1,000 ms).
  - The hop memory against +64 MiB, and the Indus region's flight.
  - Also run `--scrub-bench` once natively: the overview now decodes on the globe, and the scrub's frame gates must still pass.
- [ ] **Step 2: Chrome WebGPU,** split as in M3's ruling. The implementer builds `web-release`, proves it by a string grep and writes the serve instructions. The controller runs `?region-bench` and `?journey-bench` in the owner's Chrome (Incognito), plus the renders of Task 8, Step 5, and commits the numbers.
  - Record each era's pass, wall and CPU, against the 5,000 ms target.
  - If the pass misses 5 s, try these in order and ledger each: sample the ICE-6G terms on a coarser sub-lattice; raise `WEB_HYDRO_BUDGET_MS` to 8 if the globe's frame gate still passes; lower `HYDRO_RECENT_N` to 768.
- [ ] **Step 3: Compare with M3** (`docs/testing/m3/performance.md`). Flight gates hold. Region generation (pass wait excluded), warm generation and the memory cycles show no regression beyond noise. Hop memory stays under the gate.
  - If the hop memory fails, try these in order and ledger each: raise `APRON_RIVER_MIN_DISCHARGE_M3S`; drop the CPU river array after upload. Otherwise the gate is the owner's call.
- [ ] **Step 4: Write `manual-checks.md`** for the owner (design §7), in M3's format, with the commands. Each check is a box to tick:
  - **The Indus tributaries** (`--region=32.5,74.5,0`): mountain rivers in gorges with rapids, joining on the Punjab plain. Water moves downstream. Rapids churn over rocks. No rivers on ridges.
  - **The Amazon** (`--region=-3.1,-60.0,0`): wide, slow and meandering, at true width, with no map lines.
  - **The Sahara at 0 ka** (`--region=16.0,15.0,0`): dry.
  - **The Sahara at 6–9 ka** (`--region=13.0,17.0,8000`, unsnapped; also a dive at the 10 ka snapshot): lakes and rivers, Lake Mega-Chad among them.
  - **No seams:** at region edges and across two hops over a river, from the Andes route and from the Indus going east.
  - **The Caspian at its own level** (`--region=42.0,50.5,0`): about −28 m. The Volga lowlands are not flooded.
  - **The Dead Sea, Titicaca, the Great Salt Lake and Victoria**, with their levels from the reality check.
  - **A slider change in a region:** the cloud holds while the pass runs, then shows the new era's rivers.
- [ ] **Step 5: Write `performance.md`:**
  - The pass per era, native and web, against the targets.
  - Region generation against M3's.
  - Hop memory and its parts.
  - `EraHydrology::bytes()` and `working_bytes()` for 0 ka and 250 Ma.
  - The climate bake's fetch total and size.
- [ ] **Step 6: Update the parent spec,** run the gate and commit `docs: rivers and lakes verification and spec updates`.

## Appendix: rulings

Each ruling is *what*; *why*; *cost if wrong*.

- **The lattice by era.**
  - Recent (0–26 ka), bridge (26–30 ka) and deep ages up to 0.2 Ma use `HYDRO_RECENT_N` = 900 (mean 10.2 km), read from the 5′ overview with no plate lookups.
  - Older deep ages use `HYDRO_DEEP_N` = 180 (51 km) through the plate-resolving reference.
  - Why: the design's "about 10 km where present-day data drives the terrain", within the web's 5 s. Plates move under 10 km in 0.2 Myr. The `DeepField` support lattice would cost ~1.3 million reference calls at 10 km.
  - Cost if wrong: from 0.2 to 2.2 Ma (the 1 Ma snapshot), ETOPO-detailed terrain gets a 50 km network whose rivers may miss real valleys. The fix is a present-frame pass carried with the plates.
- **Bridge and present-frame sampling.**
  - Bridge: `(1−w)·recent + w·(overview − anomaly)`. Present-frame: `overview − anomaly`.
  - Why: no plate lookups (design §4.1), and identity rotation holds to under 2 km at 30 ka.
  - Cost if wrong: coasts off by the bridge frame's residual, at 10 km.
- **Ocean connectivity** uses straits of up to `OCEAN_STRAIT_M` (25 km) by dilation.
  - Why: at 10 km the Bosporus and the Danish straits close, and the Black Sea and the Baltic would become lakes. The Caspian (over 400 km away), the Dead Sea (75 km) and Qattara (55 km) stay inland.
  - Cost if wrong: a basin within 25 km of the sea floods as sea.
- **Ties and flats.**
  - Priority-flood keyed `(filled, seq)` with seeds in id order. Steepest descent on the filled surface, ties broken by neighbour order. Flood parents on flats.
  - Why: reproducible and FIFO across flats (design §6.2).
  - Cost if wrong: none for determinism; drainage on flats may look straight.
- **One level per depression.**
  - Nested sub-basins share the depression's level, found by grouping its nodes by elevation.
  - Why: priority-flood merges them at the outer spill. Arid basins still get small lakes.
  - Cost if wrong: two pools at different levels in one basin read as one.
- **Data lakes.**
  - A flat bottom of at least `DATA_LAKE_MIN_NODES` nodes, plus a water-balance veto (`DATA_LAKE_MIN_BALANCE` = 0.25). Detection is off for PaleoDEM sources.
  - Why: the design's data levels. The veto keeps salt pans (Uyuni) dry.
  - Cost if wrong: a wet salt pan, or a missed data lake. The reality check's five lakes catch the latter.
- **Rivers by discharge, not by area.**
  - `RIVER_MIN_DISCHARGE_M3S` = 20.
  - Why: the anti-goal of deserts full of rivers.
  - Cost if wrong: thin humid streams are missing. The reality check uses area ≥ 10,000 km² and discharge ≥ 20 on both sides.
- **Ice.**
  - Ice nodes route meltwater (`ice_melt_runoff_mm`) but hold no river and no lake. A river may end at an ice margin (`Down::Ice`).
  - Why: "rivers start at the ice margin" (design §4.1).
  - Cost if wrong: rivers that run under an ice sheet in reality stop at its edge.
- **The incision bed** is `bed(s) = min(B(s), min over the upstream window of max(floor, B(s' ⊕ L)))`.
  - Why: the pass's overview bed sits hundreds of metres above 1′ gorges. A plain windowed minimum is not monotonic; this one is, and it is a function of global data plus the field.
  - Cost if wrong: lowland rivers cut at most `B(s) − B(s ⊕ L)` below the pass, so a detail dip deeper than that is filled by the bank rule.
- **The river tile margin** is `(1.5 + CORRIDOR_FRACTION)·spacing + INCISION_WINDOW_M` (about 50 km at 10 km).
  - Why: every floor read must come from the same data in every region that sees the river.
  - Cost if wrong: a few more tiles per region.
- **One packed water texture per field** (`Rgba32Uint`: level, thickness, ribbon, packed flow), replacing the water's height textures. Rapids are encoded as a speed ≥ 3 m/s.
  - Why: the WebGPU per-stage texture limit, and the design's speeds (1–3 m/s lowland, 3–6 m/s rapids). The design's "two or three more field textures" becomes two.
  - Cost if wrong: one more channel if a later look needs the slope itself.
- **Flow is stored in each texel's own east and north,** and the upload uses it as region x and −z.
  - Why: it is frame-independent, so overlaps agree.
  - Cost if wrong: directions are off by the meridian convergence, under 2° below 60° of latitude.
- **The climate grids** are BIO1 and BIO12 at 1°, F16, for every Beyer age up to 26 ka and every 10 kyr from 30 to 120 ka (35 grids). Ages between are linear.
  - Why: a few MB, and runoff needs no more than 1°.
  - Cost if wrong: a re-bake at another selection.
- **Beyer's time 0** is the game's present, 76 years off.
  - Why: that is below Beyer's 1 kyr step.
  - Cost if wrong: none visible.
- **The whole-file md5** is checked against figshare's API metadata, and the size against `Content-Range`. The extracted arrays are pinned by SHA-256.
  - Why: the bake never holds the file.
  - Cost if wrong: an altered file could pass with the same md5 metadata, but the array pins still catch it.
- **The climate table** is committed data (`data/hydrology/climate_table.json`), compiled into the game and read by the Python reference.
  - Why: the owner can tune it (design §4.1), and there is one source for both languages.
  - Cost if wrong: a rebuild per tuning.
- **Pass scheduling.** A prefetch fires once the nearest snapshot has settled for 0.5 s on the globe. A region's request preempts a prefetch. The cache holds 2 eras.
  - Why: a slider drag must not queue passes, and memory stays bounded.
  - Cost if wrong: a dive just after a drag waits for one pass.
- **The tile index loads with the globe** rather than on the first region request.
  - Why: the recent pass needs the overview on the globe.
  - Cost if wrong: the overview's decode moves from the first region to the globe. The scrub bench is re-run in Task 12.
- **No journey route is added.**
  - Why: the Andes and Fuji routes already cross rivers and coasts, and the manual checks cover river hops.
  - Cost if wrong: a seam on a river hop only shows in manual checks.

### Spec points this plan reads narrowly

- "Discharge only grows downstream" holds between lakes. An open lake's outflow is its inflow minus evaporation.
- "Rivers end at the sea or at a lake" also allows an ice margin.
- No standard snapshot lies between 6 and 9 ka, so the Sahara check uses the unsnapped `--region=…,8000`, plus a dive at the 10 ka snapshot.
- The reality check's river set uses the game's discharge threshold on both sides. Otherwise arid channels of 10,000 km² (the Thar's) would demand game rivers the anti-goal forbids.

### Rulings made while building (copied from the SDD ledger)

- **A hop's swap holds the cloud one frame longer (owner, 2026-11-25: "One more frame in cloud").** *(Extended by the fourth stage below: `REPLACE_EXTRA_HELD_FRAMES` is now 2.)*
  - What: a replaced region uploads in three frames. On the swap frame the old water textures are removed and nothing binds water. The next frame makes the new water, once the render world has freed the old. The third frame replaces the heights and surface codes. Navigation holds the cloud at density exactly 1 for the swap frame and `REPLACE_EXTRA_HELD_FRAMES` (1) more, for both hops and snapshot changes. The active grid also frees its river texels after the upload.
  - Why: with rivers, M3's native hop-memory gate (+64 MiB) failed at +81 MiB. The old and new field data, their staging and both water texture sets peaked together.
  - Cost if wrong: one more white frame (about 17 ms) per hop and per snapshot change. Task 8's water material must draw nothing while `TerrainScene::inner_water` is not loaded.
- **A fourth stage: the heights wait a frame for the water's staging to drain (owner, 2026-11-25; the gate is not touched).**
  - What: between the new water and the new heights and surface codes there is now a frame with no upload. `REPLACE_EXTRA_HELD_FRAMES` becomes 2, so a hop or a snapshot change shows 3 opaque frames from the swap on.
  - Why: with three stages the worst hop still measured +64.2 MiB. The water's upload staging was still allocated when the heights' staging and textures arrived.
  - Cost if wrong: two white frames (about 33 ms) per hop and per snapshot change instead of M3's none.
- **Task 8: the flow is drawn in each texel's own frame, not rotated (controller's ruling 1).**
  - What: the water texture's flow is `(east, −north)` in the texel's own frame, and the shader reads it as region `(+x, +z)`. `water_image`'s doc now says so.
  - Why: the two frames differ by about `(d/R)·tan(lat)`: 1.4–3.7° across the inner region. A rotation per fragment buys nothing visible there.
  - Cost if wrong: in the far apron at high latitude (up to about 30° at 70°N) the ripples drift visibly across the channel. Seen only from very high up, where the ripples have faded out.
- **Task 8: the crossing guard (controller's ruling 2), by the offset's rate of change rather than the example rule.**
  - What: core's `blended_water` had no guard; it gains one, mirrored in WESL. A field whose four taps hold offsets of both signs that differ by more than `CROSSING_REACH_TEXELS` (1.5) texels gives its interpolated ribbons (offset 0) instead of `hw − |o|`. Test: `converging_rivers_leave_no_ribbon_between_them`.
  - Why: one river's signed offset changes no faster than the distance between taps (√2 texels across a cell), so a larger jump means two rivers. The example rule (signs differ and magnitudes exceed the half-width) would also fire across every sub-texel river's own centreline, where the taps sit up to a texel from a few-metre half-width, and erase those rivers.
  - Cost if wrong: where one river's taps disagree by more than 1.5 texels (the outer wedge of a bend sharper than about 90°, near a vertex), the field draws the plain ribbon blend there. That loses a sub-texel river for under a cell; a wide river is unaffected.
- **Task 8: the water mesh sits at the drawn surface, `max(level, ground)`, not at the level** (since fix round 1, `max(level, ground − 4 m)`; see below).
  - What: the vertex stage puts each vertex at `level − min(thickness, 0)`, which is `blended_surface_m`. The brief says "at `level`".
  - Why: a dry texel's level is `NO_WATER_DROP_M` (64 m) under its ground. Where the polar mesh is coarser than a channel (400 m spacing at 10 km), a plane from a river vertex to a dry vertex would sink under the river's banks and hide the river. At the ground, it never does.
  - Cost if wrong: between vertices near a shore the drawn plane stands up to a bank's height above the true level. The water is then drawn slightly high there, which is a parallax shift of the shoreline at grazing angles. Over land the mesh is discarded per fragment after one `blended_water` read: about one cheap full-screen pass, where the old sea plane under mountains was depth-culled.
- **Task 8: rivers keep at least about a pixel's width.**
  - What: the fragment discards where `ribbon + footprint/2 < 0` rather than `ribbon < 0`.
  - Why: from afar a 20 m river is under a pixel wide, and a hard true-width edge breaks it into crawling dashes. The rule changes nothing up close (1–2 m footprints) and never draws water outside the carved channel, which the thickness test still bounds.
  - Cost if wrong: distant thin rivers read up to a pixel wider than true.
- **Task 8: "by depth" for the river tint.**
  - What: `river_tint = mix(mix(RIVER_CLEAR, RIVER_SILTY, silt), WATER_DEEP, (1 − e^(−d/28 m))·(1 − silt))`. Clear water darkens with depth as M2's tint does; silty water stays brown.
  - Why: the brief gives the endpoints and "by depth", not the form. Turbid water does not go navy with depth.
  - Cost if wrong: deep, fast rivers (the Indus gorge) read darker teal than the owner wants. It is tunable from the constants.
- **Task 8: the swell fades by the camera's height above the water's own level.**
  - What: `SWELL_FADE_AGL_M` is measured from the vertex's water level, not from `y = 0`.
  - Why: with lakes at their own levels (Titicaca at 3,810 m), "above the sea" no longer means above the water. The sea is unchanged.
  - Cost if wrong: none for the sea. Lakes show the swell from as low as the sea does.
- **Task 8: the swap frame drops the water's entity and material (review fix round 1).**
  - What: while `TerrainScene::inner_water` or `apron_water` is missing (the swap frame), the water entity is despawned and its material removed with every handle to it. The frame after, a new material is made over the new water. The mesh is kept. Test: `the_sea_draws_nothing_while_its_water_is_missing`.
  - Why: the first version rebound the material to an image handle no image is stored at, and hid the entity. In Bevy 0.20.1 a material whose bind group can't be built (`RetryNextUpdate`) keeps its old binding until a new one succeeds. The old binding holds the old water's texture views, so old and new water coexisted. Only an unused material asset is unloaded (`unload_material`), which happens the frame after its handles drop: the same frame the old water images are freed. `Handle::default()` must still never be bound to a `texture_2d<u32>` slot (it is Bevy's float default image), so no material exists without water.
  - Cost if wrong: one material and entity rebuilt per replace, which is negligible.
- **Task 8: the GPU parity bound.**
  - What: `wesl_water_matches_rust` checks the level within 2 ulps of its taps on fields whose sizes make f32 bilinear weights exact (256 m and 2,048 m spacings). On the region's own sizes it allows the weights' few-ulp f32 error times each quantity's local spread. The brief's flat "2 ulps at 4,096 points" holds only where the weights are exact.
  - Why: WGSL computes the texel coordinate in f32, and the reference computes it in f64. On a 293 m spacing the weights differ by about 1e-4, which on rough random fields is hundreds of ulps of the level (measured 1,444).
  - Cost if wrong: a WESL slip smaller than the weight-error term on real sizes would pass the second case. The dyadic case still bounds it to 2 ulps.
- **Task 8: the river look ramps by flow speed across the cell where a river meets standing water.**
  - What: the brief's classification stands: rivers are `ribbon < RIBBON_NONE_M`, standing water the rest, and the vertex swell uses it as is. The fragment weights the river look (tint, ripples, white water, against the lake's waves, swash and shore foam) by `smoothstep(0.05, 0.3, |flow|)`.
  - Why: the ribbon reaches 1,000 m only at the edge of the last mixed cell. A hard switch on it drew brown wedges with straight, texel-aligned edges at river mouths (seen south of Manaus). Rivers run at 0.3 m/s or more, standing water has no flow, and the flow falls to 0 exactly where the ribbon reaches 1,000 m, so the ramp is continuous.
  - Cost if wrong: a river slower than 0.3 m/s would show part of the lake look. The lowland speed floor is 0.3 m/s, so only fading ends are affected.
- **Task 8: the water draws after the terrain, and its dry vertices sink under the ground (review fix round 1).**
  - What:
    - The water's material is `AlphaMode::Mask(0.0)`, so it is in the alpha-mask phase, which Bevy draws after the opaque terrain. No fragment's alpha is under 0, so the mask never discards.
    - Back faces are culled (the default), and `double_sided` stays false.
    - The vertex sits at `max(level, ground − DRY_SINK_M)` (4 m). Its depth pull is weighted by `smoothstep(−4, 0, thickness)`, so it is none on dry ground.
    - Tests: `the_water_draws_after_the_terrain`, `dry_vertices_sink_under_the_ground`.
  - Why: with the mesh at the ground, pulled toward the camera, every land pixel ran `blended_water` before discarding. That measured +0.8–1.8 ms per frame at 4K over the Alps (old 20.33–21.37 ms, HEAD 22.06–22.18 ms medians, interleaved). There is no depth prepass and opaque bins are ordered by pipeline id, so nothing guaranteed the terrain's depth was there first. The mask phase guarantees it, and the sunk, unpulled mesh is then rejected by the depth test before it shades: 20.26–20.30 ms. The sink can't hide water: where the ground is less than 4 m above the level the vertex stays at the level, and a higher bank's vertex sits above it. The first version's `cull_mode: None` was meant to fix distant dashes, but debug builds later showed those were occlusion; turning culling off never changed them.
  - Cost if wrong: where the terrain's coarse far LOD stands more than 4 m above the water mesh's interpolation, water fragments over land still run and discard (no worse than before). A river narrower than the terrain's far LOD can hide under it, where the old mesh at the ground, pulled, won. The far renders show rivers as continuous lines.
- **Task 8 renders (controller's rulings 3 and 4).** Step 5 adds Qattara (`29.5,27.0,0`, AGL 3,000), which must show dry ground. Rapids are judged by white water where `rapid_from_speed` is high, until Task 9's boulders. The brief's Manaus and Indus poses showed no river (see the report), so supplementary shots were added: south of Manaus, and an Andes river with rapids east of Titicaca.

#### Rulings from the rest of the ledger (copied in Task 12)

The rulings above were copied as they were made. These are the ledger's other `Ruling:` lines, in
ledger order, grouped by task. A ruling later superseded says so; the one that replaced it follows.
Disk-guard bookkeeping is left out, as in M3.

**Planning and Task 1**

- **Task 11's bars run on the relaxed centrelines.**
  - What: the bars use the relaxed (valley-searched, pre-meander) centrelines from Task 5's relaxation over the boxes, not only the continental graph; the coarse graph's numbers are reported alongside.
  - Why: spec §7 bars "the game's centreline", and the relaxation is what puts rivers in real valleys. Meanders are excluded because they are deterministic noise of at most 1.5 widths.
  - Cost if wrong: a little more work in Task 11 (a `floor_at` over the boxes from the inner-level field).
- **`MAX_FETCH_BYTES` = 100,000,000, not 300 MB.**
  - Why: the owner pre-approved downloads under 100 MB (2026-11-25); the measured need is about 40 MB.
  - Cost if wrong: a fetch that legitimately needs more fails loudly and comes back to the owner.
- **Task 1's real network fetch (about 40 MB) runs inside Task 1, by its implementer,** not as a separate controller step.
  - Why: the owner pre-approved downloads under 100 MB; the implementer reports the bytes fetched.
  - Cost if wrong: none beyond the 40 MB.
- **"Downloads need the owner's go-ahead" reads as "over 100 MB, or anything left large on disk"** (owner, 2026-11-25).
- **Cargo builds and benches hold while the owner runs the Chrome benches.**
  - Why: CPU-heavy builds would skew the web timings.
  - Cost if wrong: an idle hour on rivers.

**Task 2**

- **The interface grows:** `RECEIVER_EXIT` = 9 for basin nodes redirected to a distant exit, `Routed::basin_of` to resolve it, and `HydroGraph::mean_spacing_m()` for the strait reach.
  - Why: the brief's "redirect to exit" can't be a neighbour index. Carried into Tasks 3, 4 and 11.
  - Cost if wrong: an interface rename later.
- **Cube-corner duplicate neighbours resolve cardinal-first** (E, N, W, S, then the diagonals; later duplicates dropped). Task 11's Python mirrors it.
  - Cost if wrong: a reference/game mismatch at 24 corner texels only.
- **The neighbour-distance test normalises by step length (0.62–1.37),** not the brief's [0.5, 1.6] × mean spacing.
  - Why: that bound is impossible for diagonals (1.93 at a face centre); a plan defect.
  - Cost if wrong: none.

**Task 3**

- **The data-lake rule gets an upper bound:** Q_in ≤ `DATA_LAKE_MAX_BALANCE` × capacity(F), with `DATA_LAKE_MAX_BALANCE` = 10.
  - Why: without it any flat-bottomed pit outranks the water balance (a 0.3 m dip ended rivers; a steppe bowl became a 79 m/yr evaporator). The Caspian sits near 3, pits near 1,000.
  - Cost if wrong: a real data lake with a large inflow ratio falls back to the water balance.
- **Data lakes also come from large flats, not only depressions.** A connected set of at least `DATA_LAKE_FLAT_MIN_NODES` (20) non-ice land nodes within `DATA_LAKE_FLAT_TOL_M` of one level, passing the same balance veto, becomes an Open data lake at that level, its water passing through to its outlet.
  - Why: ETOPO's surface holds draining lakes (Victoria, the Great Lakes) as flats at the outlet level; spec §4.2 says a lake the data holds keeps its level, and §7's reality check names Victoria.
  - Cost if wrong: a flat that isn't a lake (a playa that passes the veto) reads as water.
- **`MIN_LAKE_DEPTH_M` = 0.5 is accepted:** shallower open basins pass through; shallower terminal basins are dry playas unless Q_in ≥ 20 m³/s. Task 11 mirrors it with the terminal-sink and footprint rules.
- **A flat counts as a data lake only if its downhill boundary is small:** at most `DATA_LAKE_FLAT_MAX_OUTLETS` (3) boundary nodes have a strictly lower outside neighbour.
  - Why: F16 quantisation stairs on plateaus and sloping deltas must not become lakes; real lake surfaces drain through one or two outlets.
  - Cost if wrong: a real lake with many low points on its rim (a delta lake) is missed, and the balance decides.
- **Table lake levels stay at the present level in recent eras** (`NodeSource` exposes no elevation change); accepted for this milestone.
  - Why: at 21 ka the Great Lakes are under ice and Baikal barely moved; the Caspian's past levels differ, but no data for them is here.
  - Cost if wrong: a Caspian at today's level in ice-age snapshots.
- **A table lake is Open whenever Q_in exceeds evaporation over its footprint,** passing Q_in − evaporation out through the basin exit, whatever its level against the spill; Terminal otherwise (the Caspian).
  - Why: the water surplus decides drainage physically, and the table's datum can't be trusted to the spill within a 10 km lattice.
  - Cost if wrong: a table lake that really is endorheic but humid would drain (none of the seven are). *(Narrowed in Task 11d: a cited `endorheic` flag makes the Caspian terminal; see below.)*

**Task 5**

- **`river_tile_margin_m` is about 116 km at n = 900** (was 49.5 km), and `MAX_CHANNEL_DISCHARGE_M3S` = 300,000 bounds the reach (the Amazon's mean is about 209,000).
  - Why: the old formula missed the gather reach and far incision segments; the tile test proves reads past it.
  - Cost if wrong: more tile loads per region (6 against 2 in the test) and more fallbacks near data gaps. Task 10 measured generation.

**Task 6**

- **The relaxation moves a line only to an interior local minimum of its corridor samples** (lower than both neighbours, strictly inside the corridor), and a node only to an interior local minimum of its 7 × 7 grid; with none, the offset is 0. The uphill bound off bends is restored. *(A Task 5 defect found in Task 6.)*
  - Why: on tilted plains the corridor minimum sits at its edge, so segments left and re-entered their pinned nodes sideways (1,064 of 12,437 sub-segments over 1 km at n = 900, legs up to 6.6 km) and the bilinear level rose up to 38 mm. Real valleys (gorges) are interior minima.
  - Cost if wrong: a river on a sloping plain beside a shallow real valley stays on the coarse line.
- **The node rule as built is accepted:** off the border and strictly lower than both neighbours along its row or its column, instead of "lower than all 8".
  - Why: a valley floor falls downstream, so the all-8 rule pins every node (Task 5's valley test failed).
  - Cost if wrong: a node on a saddle-like spot moves along one axis.
- **Raw cross-chord offsets taper to 0 over 3 sub-points next to an end whose node did not move** (reviewer's minor 3, folded into fix round 2).
  - Why: a valley pulling sub-points beside a pinned node should bend in gradually, not make a sideways leg; the remaining 336 legs over 1 km were a visible zigzag risk.
  - Cost if wrong: a river near a pinned node sits slightly off a side valley's floor.
- **No further relaxation rule in Task 6** (a "nearest interior minimum to the previous offset" continuity rule is deferred).
  - Why: the remaining legs come from a synthetic 40 m ripple fixture; real terrain is judged by Task 11's maps and the owner's flights before adding sequential rules.
  - Cost if wrong: visible short zigzags on rippled plains, fixable later in `rivers.rs` alone.

**Task 7 (hop memory)**

- **Native hop memory is measured (journey bench, release, twice) right after Task 7's review, before Task 8.**
  - Why: the computed swap growth put the +64 MiB gate at risk, and a texture-layout or upload change is cheaper before the shader binds it.
  - Cost if wrong: one extra bench run.
- **Mitigate before asking the owner to move the gate:** (1) drop the active grid's CPU `river` array (and any field only the GPU needs) after upload; (2) stagger the swap's uploads so old and new CPU image data and staging don't peak in one frame, with the cloud at full cover (extending the hop's hold by a few frames if needed); re-measure twice. If still over +64, bring the owner the numbers and options.
  - Cost if wrong: a few frames' longer hold and one more bench cycle.
- **The re-measure must be display-paced like every earlier run;** find why pacing dropped before judging.
  - Why: the owner's gate compares like with like.
  - Cost if wrong: a pass or fail that reflects pacing rather than memory. *(The uncapped runs traced to a bench window hidden behind others on an unlocked session.)*

**Task 8 and Task 8b**

- **"Today's data" means a present-day lake map** (Natural Earth 10m lakes, public domain, baked to a small committed mask pinned in `data/sources.json`), not ETOPO flats alone. Applied where terrain is today's (recent eras): an Open water-balance lake that overlaps no mapped lake is breached; Terminal (arid) lakes stay by water balance, so past lakes in closed basins (Mega-Chad, Bonneville) survive; a data flat becomes a lake only where the map holds one; table lakes are unchanged; deep-time eras skip the check. (Owner, 2026-11-25: "Check against today".)
  - Why: the flats are what made the Amazon at Manaus a lake and missed Titicaca, so a map fixes the artefact lakes, Manaus and Titicaca with one source.
  - Cost if wrong: a real Open lake missing from the map is drained (none of the reality-check lakes), and palaeolakes that overflowed in basins with no lake today are lost in past eras.
- **The data fix is its own task, Task 8b, before Task 9,** so Task 10's benches measure the final hydrology. It also fixes the water walls at lattice-cell lines (no wet texel at level L beside a dry texel whose ground is below L − tolerance).
  - Cost if wrong: one more task cycle.
- **Overlap with the map is not enough:** a mapped Open lake whose level exceeds its mapped surface (the median elevation of its mapped nodes) by more than `OPEN_LAKE_MAP_TOL_M` = 10 m becomes a data lake at the mapped surface; reservoirs are dropped from the map.
  - Why: Natural Earth may map Wular Lake inside the Kashmir valley, which would keep the 2,140 m artefact.
  - Cost if wrong: a past Open palaeolake that overflowed above today's surface (possibly Mega-Chad at 6–9 ka) drops to today's level.
- **The Dead Sea gets a low oblique shore render in Task 8b; Victoria Falls (−17.925, 25.857) and Iguazu Falls (−25.695, −54.437) become a big-river rapids check in Task 8b and renders in Task 12.**
  - Why: the owner asked about them as tests. They show as churning white water on a steep reach, not a falling curtain: waterfalls are a spec non-goal, and the drop sits inside a gorge narrower than the terrain data.
  - Cost if wrong: two renders. *(The rapids check was dropped later in Task 8b; see below.)*
- **The uncapped hop-memory PASS of Task 8's fix round is accepted;** the paced confirmation comes from Task 10's benches and the final-merge re-bench.
  - Why: uncapped runs read higher hop growth than paced (Task 7: +42–50 against +22–36 MiB), so +48.7 uncapped is consistent with the paced gate.
  - Cost if wrong: a paced overrun found in Task 10, after Tasks 8b and 9 had built on the swap code.
- **The shore scallops go to Task 8b with the walls:** standing water interpolates its level over wet taps only (core `blended_water`, the WESL mirror and the parity test), so the shoreline is the terrain's own h = L contour.
  - Cost if wrong: a sampler change late in the milestone, re-checked by the parity test.
- **A mapped lake's surface comes from the fine data, not the lattice:** Task 8b bakes a per-lake surface level from the finest ETOPO data inside each mapped polygon (the median), ships it with the map, and uses it as the mapped surface; Task 11's lake check compares against it.
  - Why: the Dead Sea read about −276 m (the 10 km overview mixes the rift walls) against about −430 m.
  - Cost if wrong: a baked level from a different ETOPO epoch than the region terrain (the Dead Sea falls about 1 m a year), off by metres.
- **At the present-day snapshot (age 0) the map decides every lake.** Each mapped lake stands at its baked surface (the table level for table lakes), its footprint its own mapped nodes only, so several lakes can share a basin (Titicaca beside Uyuni), a lake needs no lattice node below its level (the Dead Sea), and the balance veto doesn't apply (the Great Salt Lake). An unmapped basin holds no lake and its rivers end at its sink. Past eras keep the water balance with Task 8b's rules, and a mapped lake capped at its surface takes its mapped nodes as its footprint (never "basin nodes below S", which flooded the Hungarian plain).
  - Why: "check against today" is exact for today; the balance's calibration error (the Great Salt Lake +40 m) and the 10 km lattice can't reproduce today's lakes, and the past has no map.
  - Cost if wrong: lakes can jump at the first past snapshot (the Great Salt Lake up about 40 m, an Altiplano lake appears).
- **The Black Sea is sea at 0 m, not part of a Caspian lake,** fixed in Task 8b with a real-data check (no lake and the ocean flag at 43.0, 34.0; the Caspian stays at −28 m within its own nodes).
  - Cost if wrong: one more rule in the pass's ocean flags.
- **Both falls are dropped from Task 8b's rapids check;** "no rapids near Manaus" stays. The falls stay as Task 12 renders and go to the owner as a data-resolution limit.
  - Why: the 10 km lattice spreads Iguazu's 82 m and Victoria's 108 m drops below the rapid threshold and puts the Iguazu 5 km off.
  - Cost if wrong: weak white water at big falls ships unflagged by a test.
- **Parallel rivers in breached valleys** (Task 3's BFS routing in pass-through basins) are deferred to Task 11's maps unless they show on the Indus or Amazon flights.
  - Cost if wrong: visible doubled rivers in some Andes valleys.
- **Rivers keep their blend:** the wet-taps-only blend is restricted to standing water, with a river-bank test (supersedes a deferred minor that every wet tap, rivers included, was treated as standing water).
  - Why: the brief says rivers keep their blend.
  - Cost if wrong: none beyond the round.
- **The Pannonian basin spilling through the Moravian Gate is fixed in Task 11, not Task 8b.** Task 11 checks the outlets of a short list of major rivers against reality (the Danube through the Iron Gate to the Black Sea; the Jhelum out of Kashmir at Baramulla; the Indus and the Amazon to their seas) and fixes misroutes with gorge entries in the strait table (generalised to passes).
  - Why: the reality check is where routing is judged, and the same artefact may sit on the owner's Indus flights.
  - Cost if wrong: a famous river misrouted until Task 11.

**Task 9**

- **Hop memory with the grow-only boulder pool, the swap-frame placement cost on wasm at 16,384, and a WebGPU run all go to Task 10:** its hop bench includes a hop from a boulder-free region into a rapids region, and it times placement on wasm. The LOD-0-within-6-km assumption is checked in Task 9's fix round.
  - Cost if wrong: a pool trim or time-slicing added in Task 10.

**Task 10**

- **Task 10 starts before the owner answers the white-water question;** its bench plumbing and GPU timing don't depend on the look, and if a white-water pass follows, only the water GPU numbers are re-measured.
  - Cost if wrong: one re-run of the water measurements.
- **Run 1's +71.0 MiB first hop does not fail the task:** the brief judges run 2 (+31.4), and the owner allowed +64 headroom; the paced hop bench re-runs on the final merge.
  - Cost if wrong: a first-launch hop over the gate goes unaddressed until the merge re-run.
- *(Superseded by the next ruling.)* The continental pass's 28–31 ms native longest step can't fit `WEB_HYDRO_BUDGET_MS` = 6.0 on the web; it is a budget defect from Task 4's slicing; a Task 10b would name the step and split it to at most 2 ms natively, with a bench test.
- **Windowed GPU lines from occluded windows are not scene costs;** the offscreen numbers are the record.
  - Cost if wrong: none (reported only).
- **Naming and splitting the pass's longest step is Task 10's own carried "time-slice whatever overruns",** in its fix loop, not a new task. Target: the time-boxed run's longest step at most 2 ms natively (about 3× the wasm margin under 6 ms), pinned by a test.
  - Why: the controller's measurement found an indivisible step of about 23 ms even with adaptive units, so the web overrun was real.
  - Cost if wrong: a larger fix round on a bench task.

**Task 11**

- **The failing Indus bar goes to the owner unchanged** (brief Step 6); the task review proceeds on the code meanwhile.
  - Cost if wrong: none (the bar's outcome doesn't change the code under review).
- **The outlet tolerances are ratified** (start 30 km, via 15 km, sea 150 km).
  - Why: the misroutes this bar exists to catch end in the wrong sea, 1,000+ km off (the Baltic, the Bay of Bengal), so 150 km separates them; the Amazon's estuary is about 300 km wide; the Indus ending 144 km east by Kori Creek is the same Sindh-plain route error the box bar already fails on, and goes to the owner with it. The fix is disclosure: `reality-check.md` says which thresholds and inputs the task chose.
  - Cost if wrong: an outlet bar a touch loose.

**Task 11b (today's courses burnt in)**

- **Task 11b burns Natural Earth `River` features of scalerank ≤ 7 only** (no lake centrelines).
  - Why: rank ≤ 7 takes in the Jhelum, a check miss; today's lakes are the map's.
  - Cost if wrong: minor rivers (ranks 8–10) keep lattice routes; a river through an unmapped lake has a gap. *(Task 11d later burns the lake centrelines no mapped lake covers.)*
- **The Python 1′ reference does not burn Natural Earth courses;** it stays a DEM reference independent of Natural Earth, so the check compares two independent sources.
  - Cost if wrong: none for honesty; the box shares are not comparable with an NE-burnt reference.
- **The Natural Earth table (≤ 1 MB, committed, compiled in) is new shipped data** beyond the Global Constraints' climate grids; the owner chose the burn.
  - Cost if wrong: about +0.3 MB gzipped in the web build.
- **The burn runs in every era with today's terrain** (`data_lakes`), skipping ice nodes, routing-only (the river bed is the running-minimum level).
  - Why: today's course beats the lattice's on plains for the last 26 kyr; a 10 m trench in every burnt river would show.
  - Cost if wrong: ice-age rivers on fans follow today's course.
- *(Superseded by the next ruling.)* The Amazon outlet's mouth point moves from (−0.5, −49.5) to the estuary's North Channel at a cited coordinate; `OUTLET_SEA_KM` stays 150 (owner, 2026-11-26: "Move the mouth point").
- **Every outlet target in `CheckSpec::standard` gets a cited source** (paper, gazetteer, atlas) beside it in `hydro_check.rs` and in `reality-check.md`; the Amazon's goes to its North Channel mouth; a point with no findable source keeps its value and says "unsourced"; `OUTLET_SEA_KM` and `OUTLET_VIA_KM` stay, and the re-run reports each margin (owner, 2026-11-26: "Let's relocate all mouth points based on cited sources where we have them").
  - Cost if wrong: a gazetteer point may sit off the main channel, so a bar can fail on the point, not the river; that goes to the owner.
- **The plan-mandated running-minimum bed stays for routing and for the bed, but is measured:** max and p99 of (e − bed) over burnt nodes at 0 ka on the real bake, with the worst five nodes; over 50 m, the worst is rendered and brought to the owner before the rule changes.
  - Why: the spec's aim is no trench; a measured number decides.
  - Cost if wrong: one more fix round.
- **The Amazon outlet gets its own sea tolerance of 160 km** (`OUTLET_SEA_KM` stays 150 for the others), documented beside the entry (owner, 2026-11-26: "Raise the Amazon's allowance to 160 km").
  - Cost if wrong: none beyond the Amazon bar's 10 km slack.
- **The burn stays (it fixes a real misroute), and the region side is fixed:** (1) the pass records, per burnt node, its anchor (the nearest point on its course polyline), and region refinement relaxes burnt nodes from that anchor, not the coarse-floor search; (2) held running-minimum stretches get beds interpolated by path length (never rising, no flat holds); (3) the Hells Canyon ridge capture is re-judged by re-render after (1) and (2).
  - Why: tributaries really do join the Snake, so the capture is not wrong in kind, only in where it crosses.
  - Cost if wrong: a round of region work; regions with burnt courses change their golden hashes (determinism holds: anchors are pass data).

**Task 11d (the world's major rivers)**

- **Task 11d is outlet plus discharge, not centreline agreement.**
  - Why: the NE burn makes a centreline test against NE circular, and a 1′ reference per river is out of budget.
  - Cost if wrong: a river that reaches its mouth by a wrong mid-course route between via points passes.
- *(Superseded by the `endorheic` flag below.)* A lake whose level comes from the map or the level table and stands below its basin's spill is terminal, outflow 0 whatever its balance. Implemented test-first, then reverted: it cut Baikal, Superior, Ontario and 313 mapped lakes, because the lattice spill stands above narrow real outlets.
- **A cited `endorheic: true` flag per level-table lake** (the Caspian) makes that lake terminal (outflow 0, surplus reported); where its table level applies, its nodes at or below the level seed the priority flood like the sea at that level, so its basin's land drains by descent to it instead of filling to the basin's spill. The Python mirror follows.
  - Why: the flag is observed fact, not a lattice inference; filling a terminal basin to its spill flattened the Caspian lowland and erased the Volga's course.
  - Cost if wrong: an unflagged endorheic lake can still spill (the Aral, Balkhash: the check reports any it finds).
- **Burnt courses keep their order:** on a stretch clamped at 0 m each course node still drains to the next (a small descent toward the course's end), and a course node other than the course's last never drains into an ocean neighbour.
  - Why: the burn exists to say where the river runs; the clamp kept the land but lost the direction (the Mississippi at LaPlace, the Yangtze via the Hudi).
  - Cost if wrong: a course with a real second mouth (a distributary to the sea) loses it unless it is its own course.
- **Lake-centreline parts are oriented by their connections to oriented river courses first, ETOPO second;** parts still ambiguous are reported.
  - Why: course order now steers routing.
  - Cost if wrong: a reversed reservoir drives a river uphill.
- **The next Task 11d round (after the sea-start diagnosis) takes in** the parent-fallback hole with the Mississippi and the sea-start fix, the beside-the-ocean junction hole with a fixture, and river-part orientation by connection; the other minors stay deferred.
  - Why: they decide the remaining outlet failures and course order's correctness.
- **The surest-first orientation rule is accepted over the literal "connections first".**
  - Why: the literal rule ran 25 parts more than 50 m uphill on real data; the ruling's aim was the correct direction.
  - Cost if wrong: flat-delta parts still follow ETOPO noise (3 known).
- **The Nile's mouth moves to its Damietta branch mouth (Ras El Bar), cited;** the allowance stays 150 km.
  - Why: as the owner's Orinoco decision, cite where the river meets the sea, not the delta head.
  - Cost if wrong: none; the bar then tests the branch the burn carries.
- **Interior course nodes drain to their course successor,** proved by a property test over the real table (ties at shared nodes by a deterministic rule), with flood and basin consistency kept (filled levels never rise along receivers; no lake or basin made of course nodes). The 0 m clamp may be dropped for interior corridor nodes, since they can no longer be ocean.
  - Why: ruling B's intent; the ε chain lost to marsh at sea level.
  - Cost if wrong: a course with a real side outlet loses it.
- **A course's level never reads a lake bed:** a course node within one lattice spacing of a mapped lake's footprint (or a node whose 5′ sample mixes a lake bed, as at Baikal) reads its height as max(sample, that lake's surface); every course pinned at 0 m after a below-sea node is reported before and after.
  - Why: the Angara at Irkutsk must follow its own valley.
  - Cost if wrong: a real river leaving a lake below its surface (none expected).
- **Round 5 (the last):** (1) trace the Aral's lost inflow and fix or explain; (2) audit the lake map with an informative check line listing every lake-map lake whose baked surface stands more than 20 m below its shoreline ring, Baikal-type errors named, with no re-bake of shipped data in that round; (3) reorient or drop the Ganges delta's misoriented parts 1099 and 1101 and the ring 1100, with evidence.
  - Cost if wrong: deep-lake surfaces stay wrong until the bake fix. *(The owner then chose cited table entries, Task 11f, and a re-bake of the lake map after the milestone.)*

**Task 11g (no drainage into a burnt course across a ridge)**

- **Before shipping new data, an option needing none is measured:** a non-course node may take a burnt course node as its receiver only if that node's unburnt elevation stands no higher than its own (plus a small tolerance). Options 5 and 1 are compared on all 4,645 joining steps with a probe; 5 is chosen if comparable, else 1.
  - Why: no new shipped data, cheap, deterministic.
  - Cost if wrong: one probe round. *(Option 5 failed the probe.)*
- **Task 11g implements option 1 with `JOIN_RIDGE_M` = 100 m:** a committed, cited data file of blocked (feeder, burnt node) steps at the recent lattice (about 16,000 pairs, at most 256 KB), produced deterministically from ETOPO 60″ by a hidden tool command (`--bake-ridged-steps`), stamped with the course table's hash and the lattice n; a test fails if the stamp doesn't match the shipped course table; the pass forbids those steps in the flood; the time-boxed pass equals `run_parallel`; the full reality check and lake dump re-run.
  - Why: 76 of 81 deep joins stopped at 0.9% false blocks with every outlet intact; 300 m left 53 deep.
  - Cost if wrong: 36 shallow joins rerouted their own way; a stale file if courses change (the stamp test catches it).

**Task 11c (white water)**

- **Boulder thinning is a function of direction only:** a cluster is kept iff a hash of (river, k, c) is below a global keep fraction (`BOULDER_KEEP_FRACTION`, a `pub const`), chosen so no bench region exceeds 75% of `MAX_BOULDERS`; the cap stays as a guard that logs a `warn!` when it fires (it should never on the bench regions); a test checks that a thinned region and its overlapping neighbour agree; raising `MAX_BOULDERS` to 32,768 waits on Task 12's hop-memory run.
  - Why: determinism is a binding constraint, and a region-dependent prefix breaks it.
  - Cost if wrong: fewer rocks everywhere by the keep fraction. *(Fix round 1 chose 0.45: the Himalayas hold 68% of the cap. `MAX_BOULDERS` stays 16,384.)*

**Reviews**

- **Review packages are generated with `GIT_CONFIG_GLOBAL=/dev/null`** from Task 11c's re-review on.
  - Why: `~/.gitconfig` sets `diff.external=difft`, so every earlier package was a difftastic display, not a unified diff, and earlier reviewers may have missed context; the final whole-branch review gets a clean package.
