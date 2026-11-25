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
relax: each node → the lowest floor_at on a 7 × 7 grid over ±C in its own north/east (C =
  CORRIDOR_FRACTION × mean spacing; ties: smaller offset, then grid index). Segment sub-points sit
  at t = k/K (K = ceil(length / subpoint)), k = 0..K−1, plus k = K on a terminal segment. Each is
  the lowest of 13 offsets across the chord between the relaxed nodes (ties: smaller |offset|,
  then the negative side), smoothed [1,2,1]/4 with the ends fixed.
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
  sea   if any corner < 0 and (no pass, or hydro.ocean_tap(dir)):  (0, RIBBON_NONE_M, 0)
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

*(Filled in as tasks land.)*
