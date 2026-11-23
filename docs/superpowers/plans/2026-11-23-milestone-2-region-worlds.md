# Milestone 2 — Region Worlds Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** From the globe, double-click any spot at any era and fly over a painterly 300 km region
that matches the globe's coastline, ice and sea level. Recent eras use real present-day landforms
(ETOPO 2022, 60″); deep eras use the paleo-geography amplified by generated relief.

**Architecture:**
- **Offline:** the Python bake tool turns ETOPO 2022 and Köppen-Geiger into 5° tiles.
- **World generator:** a new `worldgen` bounded context in `timefly_core`. A pure, deterministic
  function `(region spec, coarse data) → RegionGrid` (heights plus surface codes), with noise
  keyed to crust-space positions and a coastline-preserving clamp.
- **Terrain core:** a new `terrain` context with CDLOD patch selection and a Rust reference of the
  vertex morph.
- **Shell:**
  - **Generation:** loads tiles and frames and runs generation (task pool on native, a time-boxed
    chunk per frame on the web).
  - **Terrain:** CDLOD patches as entities that share one mesh and one
    `ExtendedMaterial<StandardMaterial, TerrainExtension>`, indexed by `MeshTag` into a storage
    buffer.
  - **Look:** painterly shading, a sea-level water plane, a gradient sky with fog.
  - **Stand-in for zoom-and-swap:** double-click to enter, a free-fly camera, `Backspace` to
    leave.

**Tech Stack:**
- Everything from M1: Bevy 0.20.1, WESL, `timefly_core`, the Python bake tool via `uv`.
- New Python dependency: `tifffile` (reads the Köppen GeoTIFF).
- New dataset files: ETOPO 2022 v1 60″ surface netCDF and Köppen-Geiger 1991–2020 GeoTIFF
  (`koppen_geiger_tif.zip`).

**Spec:** `docs/superpowers/specs/2026-11-23-milestone-2-region-worlds-design.md` (the "M2 design").
Its parent is `docs/superpowers/specs/2026-11-21-time-fly-design.md` (the "spec"). Read both. M1
conventions continue: `docs/superpowers/plans/2026-11-21-milestone-1-globe-and-time.md`, including
its appendix "Rulings and deviations".

## Global Constraints

- **Everything in the M1 plan's Global Constraints still applies.** That includes Bevy 0.20 feature
  sets, WESL only, the WebGPU ceiling, the geographic and render-frame convention, ages in Myr
  (f64) at boundaries, the sea-level sign (`effective = elev − anomaly`), the architecture and
  observability rules, build-disk hygiene, commit trailers, downloads needing the owner's OK, and
  never pushing without the owner's OK.
- **Units inside a region:** 1 world unit = 1 m, in the era's frame, north up. `+x` is east, `−z`
  is north, `+y` is up. The globe stays at 1 unit = 1 km.
- **Region constants:**
  - `REGION_SIZE_M = 300_000.0`, `REGION_RES = 1025` (inner field)
  - `APRON_SIZE_M = 2_400_000.0`, `APRON_RES = 513` (outer apron)
  - `PATCH_QUADS = 32` (33×33 vertices per patch)
  - `MORPH_START = 0.70`, `MORPH_END = 0.98`
- **Camera constants:**
  - `CAMERA_CLEARANCE_M = 150.0`, `CAMERA_CEILING_M = 60_000.0`, `CAMERA_START_ALTITUDE_M = 8_000.0`
  - `ENTER_FADE_S = 0.8`, `SNAPSHOT_FADE_S = 1.0`
  - `WEB_GEN_BUDGET_MS = 6.0`
- **Heights are effective elevation:** a region's heights are relative to that era's sea level, so
  sea level is always `y = 0`.
- **Tiles:**
  - Each tile covers 5° × 5° with 301 × 301 samples on the 60″ lattice, both edges included.
  - Row 0 is the northernmost (lat 85…90°N); column 0 starts at lon −180°. That gives 36 rows ×
    72 columns.
  - File name `assets/data/tiles/r{row:02}c{col:02}.tfgrid`, a 2-channel F16 `LatLon` grid:
    elevation in metres, then the Köppen code (0 = ocean/no data, 1–30 = Beck classes).
  - `assets/data/tiles/manifest.json` lists every tile.
- **Raw data** goes in `data/raw/etopo/` and `data/raw/koppen/`. Baked tiles go in
  `crates/timefly/assets/data/tiles/` (git-ignored, reproducible).
- `cargo check --target wasm32-unknown-unknown -p timefly` passes at the end of every task that
  touches `crates/`.
- The Rust toolchain is 1.100.0 (`rust-toolchain.toml`), with rustup's proxies first on `PATH`.
- **Never stage files you did not change.** At plan start the working tree held the owner's
  uncommitted edits to `crates/timefly/src/diagnostics/{mod,probe}.rs` and
  `docs/testing/mac-manual-checks.md`. Stage paths explicitly; never use `git add -A`.

## Review Focus

1. **Regions at awkward places:** the antimeridian (lon ±180), within 300 km of a pole, a region
   centred in open ocean, and a region straddling a plate boundary or an era-fixed gap. Generation
   must finish without NaN, panic or seams. Tests: Task 4 `regions_at_awkward_places_are_finite`;
   Task 1 `tiles_wrap_the_antimeridian`.
2. **Missing or corrupt tiles on the web** (404, a truncated file, a SHA mismatch):
   - Recent and bridge eras fall back to the globe-resolution data.
   - A `warn` is logged and "terrain detail unavailable here" shows.
   - Never a panic or a stuck "descending…".
   - Test: Task 5 `missing_tile_falls_back_with_warning`.
3. **Changing snapshot inside a region** to an era where the crust under the camera doesn't
   exist: return to the globe, never a blank or stale region. Test: Task 9
   `snapshot_change_to_missing_crust_returns_to_globe`.
4. **Flying fast and low:** no frame where the camera dips below terrain or the sea, at any boost
   speed or frame time. Test: Task 9 `clearance_holds_for_large_steps`.
5. **Long sessions entering many regions:** memory must return to baseline when a region is left
   (grids, textures, patch entities, tile cache bounded). Tests: Task 5 `tile_cache_is_bounded`;
   Task 10 measures peak memory over ten enter/leave cycles.

---

## File Structure

```
tools/bake/timefly_bake/
  tiles.py          pure: lattice, tile split, Köppen mode resample, manifest entries
  etopo.py          loader: ETOPO netCDF -> float32 grid (I/O at the edge)
  koppen.py         loader: GeoTIFF from the zip -> uint8 grid
  cli.py            + `tiles` subcommand
tools/bake/tests/test_tiles.py
data/sources.json   + ETOPO + Köppen entries (url, size, sha256)
crates/timefly_core/src/
  worldgen/mod.rs        re-exports; RegionSpec, RegionGrid, Field
  worldgen/noise.rs      hash, gradient noise, fBm, lattice split (mirrored in noise.wesl)
  worldgen/frame.rs      RegionFrame: azimuthal equidistant local <-> era direction
  worldgen/biome.rs      BiomeClass, Köppen collapse, zonal rule, VegetationEra, palettes
  worldgen/tiles.rs      TileSet: decoded tiles + bilinear lookup (pure, bytes in)
  worldgen/coarse.rs     CoarseField trait + Recent / Bridge / Deep fields
  worldgen/generate.rs   RegionGenerator: chunked rows, detail, coastline clamp
  terrain/mod.rs         LodSettings, Patch
  terrain/cdlod.rs       select_patches, morph_vertex, blended_height_m
crates/timefly_core/tests/fixtures/noise_golden.json
crates/timefly/src/
  region/mod.rs          RegionPlugin, RegionState, domain events, entry/leave/snapshot flow
  region/tiles.rs        TileManifest + tile loading (bundled or fetched), bounded cache
  region/frames.rs       CpuGrid asset + loader (frames as CPU grids on demand)
  region/generate.rs     generation runner (task pool / web time budget)
  terrain/mod.rs         TerrainCdlodPlugin: patch pool, storage buffer, height textures
  terrain/material.rs    TerrainExtension (ExtendedMaterial), palette texture
  water.rs               WaterPlugin + WaterMaterial
  sky.rs                 gradient sky dome + DistanceFog per era/biome
  fly_camera.rs          FlyCameraPlugin; pure fly_step
  bench.rs               + region bench
  args.rs                + --region, --region-bench
crates/timefly/assets/shaders/
  noise.wesl  terrain.wesl  terrain_prepass.wesl  water.wesl  sky.wesl
crates/timefly/tests/region_gpu.rs   noise + morph GPU golden checks (#[ignore] in CI)
docs/testing/region-m2/              screenshots, performance.md
```

---

### Task 1: Tiles from ETOPO 2022 and Köppen-Geiger (bake tool)

**Files:**
- Create: `tools/bake/timefly_bake/{tiles,etopo,koppen}.py`, `tools/bake/tests/test_tiles.py`
- Modify: `tools/bake/timefly_bake/cli.py`, `tools/bake/pyproject.toml` (+`tifffile`),
  `data/sources.json`, `data/README.md`, `crates/timefly/assets/credits.txt`, `.gitignore`
  (`crates/timefly/assets/data/tiles/`)

**Interfaces:**
- Produces: the tile files and `manifest.json` as specified in Global Constraints:
  ```json
  {"version":1,"tile_deg":5,"samples":301,
   "sources":{"etopo":"<sha256>","koppen":"<sha256>"},
   "tiles":[{"row":0,"col":0,"bytes":1234,"sha256":"…"}]}
  ```
  All 2,592 tiles are listed, so a client never guesses whether a tile exists.
- Produces (Python):
  - `tiles.tile_bounds(row, col) -> (lat_n, lat_s, lon_w, lon_e)`
  - `tiles.split_tiles(elev, koppen) -> Iterator[(row, col, ndarray(301,301,2) float16)]`
  - `tiles.mode_resample(codes, factor) -> ndarray`

- [ ] **Step 1: Get the owner's OK for the downloads, then pin them.**
  - The files:
    - `ETOPO_2022_v1_60s_N90W180_surface.nc` (NOAA NCEI THREDDS, 478,290,125 bytes)
    - `koppen_geiger_tif.zip` (Figshare article 21789074, 130.6 MB, md5
      `7fc2f5a15d4f5fe0ce59c9a9b502aa09`)
  - Add both to `data/sources.json`: URL, size, SHA-256 (computed after download).
  - Run `timefly-fetch`.
  - Inspect the zip and record which member is the 1991–2020 map and its resolution.
- [ ] **Step 2: Write failing tests in `test_tiles.py`.**
  - `test_tile_bounds_cover_the_globe_once`: the 36 × 72 bounds tile [−90, 90] × [−180, 180]
    with no gaps or overlaps (shared edges only).
  - `test_split_shares_edge_samples`: on a synthetic 1° lattice ramp, tile (r, c)'s last column
    equals tile (r, c+1)'s first column, including (r, 71) → (r, 0) across the antimeridian.
    This is `tiles_wrap_the_antimeridian`.
  - `test_mode_resample_picks_majority_and_ignores_zero_ties`: a 3×3 block with codes
    `[5,5,3,0,0,0,0,0,0]` resamples to 5. Land wins over 0 when any land is present.
  - `test_elevation_is_exact_in_f16_range`: −10,800 m … 8,800 m survive the F16 store to within
    8 m.
  - `test_manifest_lists_every_tile_with_hashes`.
- [ ] **Step 3: Implement.**
  - `etopo.load(path)`: reads `z` with netCDF4 and checks the registration from the
    `lat`/`lon` axes. If it is cell-centred, shift onto the node lattice by bilinear averaging
    and record that in the bake log.
  - `koppen.load(zip_path)`: reads the 1991–2020 GeoTIFF with `tifffile`, mode-resamples it
    onto the 60″ lattice, and treats the sea code as 0.
  - `cli tiles --raw data/raw --out crates/timefly/assets/data/tiles`.
  - Every public function carries `@traced`.
- [ ] **Step 4: Run the tests.** `cd tools/bake && uv run --locked pytest -q`. Expected: PASS.
- [ ] **Step 5: Run the real bake.**
  - Run `uv run --locked timefly-bake tiles …`.
  - Record the total bytes, the largest tile and the wall time in
    `docs/testing/region-m2/performance.md`.
  - Expected: well under the 1 GB Pages limit. If over 300 MB, stop and report.
- [ ] **Step 6: Record provenance.**
  - `data/README.md` entries.
  - `credits.txt`:
    - "ETOPO 2022 15 Arc-Second Global Relief Model, NOAA National Centers for Environmental
      Information (2022), doi:10.25921/fd45-gt74" (60″ product).
    - "Beck, H.E. et al. (2023) High-resolution (1 km) Köppen-Geiger maps for 1901–2099 based
      on constrained CMIP6 projections. Scientific Data 10, 724. CC BY 4.0."
    - The two data notes from M2 design §7.
- [ ] **Step 7: Commit.**

### Task 2: Shared noise in Rust and WESL (golden values)

**Files:**
- Create: `crates/timefly_core/src/worldgen/{mod,noise}.rs`,
  `crates/timefly_core/tests/fixtures/noise_golden.json`,
  `crates/timefly/assets/shaders/noise.wesl`, `crates/timefly/tests/region_gpu.rs`
- Modify: `crates/timefly_core/src/lib.rs`

**Interfaces:**
- Produces (core), each mirrored one-to-one in `noise.wesl`:
  - `pub fn hash3(cell: [i32; 3], seed: u32) -> u32`
  - `pub fn gradient_noise3(cell: [i32; 3], frac: [f32; 3], seed: u32) -> f32`, range [−1, 1]
- Produces (core):
  - `pub fn lattice_split(p_m: DVec3, cell_m: f64) -> ([i32; 3], [f32; 3])`: floor in f64, then
    the fraction as f32.
  - `pub fn fbm(p_m: DVec3, base_cell_m: f64, octaves: u32, lacunarity: f64, gain: f32, seed: u32) -> f32`
  - `pub fn ridged(p_m: DVec3, base_cell_m: f64, octaves: u32, seed: u32) -> f32`
  - fBm and ridged split per octave in f64. Only the per-cell evaluation is mirrored on the GPU.

- [ ] **Step 1: Write the failing tests (core, `noise.rs`).**
  - `gradient_noise_is_bounded_and_continuous`: a proptest over cells and fractions: |n| ≤ 1,
    and neighbouring cells agree on a shared face within 1e-6.
  - `hash_is_deterministic_and_seed_sensitive`.
  - `lattice_split_round_trips_at_planet_scale`: for `p` up to 6.4e6 m and cells down to 10 m,
    `cell * cell_m + frac * cell_m` reconstructs `p` within 1e-3 m.
  - `matches_golden_table`: 256 `(cell, frac, seed) → value` rows in `noise_golden.json`,
    tolerance 1e-6.
- [ ] **Step 2: Implement.**
  - Hash: PCG3D-style integer mixing on `u32` with wrapping ops only. Use no ops WGSL lacks (no
    64-bit, no `i32` overflow UB): cast to `u32` before mixing.
  - Gradient: quintic-faded trilinear blend of hashed unit gradients from a 12-vector table.
  - Generate the golden file from the Rust implementation once, with a `#[test] #[ignore]`
    `write_noise_golden` helper, and commit it.
- [ ] **Step 3: Write the GPU golden test** `region_gpu.rs::wesl_noise_matches_golden`
  (`#[ignore]`). It runs a compute shader importing `noise.wesl` over the golden inputs through a
  headless wgpu device (the M1 GPU-test pattern) and compares within 1e-5. Run it locally:
  `cargo test -p timefly --test region_gpu -- --ignored`. Expected: PASS on Metal.
- [ ] **Step 4: Run the core tests**, then check wasm. Commit.

### Task 3: Region frame, biomes, vegetation eras, palettes (core)

**Files:**
- Create: `crates/timefly_core/src/worldgen/{frame,biome}.rs`

**Interfaces:**
- Produces:
  - `pub struct RegionFrame { pub center: RenderDir }`
    - `fn local_to_dir(&self, x_m: f64, z_m: f64) -> RenderDir`
    - `fn dir_to_local(&self, d: RenderDir) -> Option<(f64, f64)>`: `None` beyond the
      antipodal hemisphere.
    - `fn north_east(&self) -> (DVec3, DVec3)`
  - `#[repr(u8)] pub enum BiomeClass { TropicalRainforest, MonsoonSavanna, HotDesert, ColdDesert, Steppe, Mediterranean, TemperateBroadleaf, TemperateRainforest, Boreal, Tundra, IceCap, Alpine }`
  - `pub fn biome_from_koppen(code: u8) -> Option<BiomeClass>`: 0 maps to `None`.
  - `pub fn zonal_biome(paleolat_deg: f64, elevation_m: f64, coast_distance_km: f64) -> BiomeClass`
  - `pub fn snowline_m(lat_deg: f64) -> f64`
  - `#[repr(u8)] pub enum VegetationEra { Barren, EarlyPlants, FirstForests, ConiferAge, FloweringPlants, Modern }`
  - `pub fn vegetation_era(age: YearsBp) -> VegetationEra`
  - `pub fn effective_biome(era: VegetationEra, biome: BiomeClass) -> BiomeClass`
  - `pub struct SurfacePalette { pub ground: [f32; 3], pub canopy: [f32; 3], pub rock: [f32; 3] }`
    (linear RGB)
  - `pub fn palette(era: VegetationEra, biome: BiomeClass) -> SurfacePalette`

- [ ] **Step 1: Write the failing tests.**
  - `frame_round_trips`: a proptest over centres (including both poles and lon ±180) and offsets
    up to `APRON_SIZE_M / 2`. Round trip within 1e-6 m. Distance from the centre is exact within
    1e-9 relative. `−z` at the centre points to the era's north (at a pole: toward lon 0).
  - `koppen_collapse_table`: pins every code:
    - Af → TropicalRainforest
    - Am, Aw → MonsoonSavanna
    - BWh → HotDesert; BWk → ColdDesert
    - BSh, BSk → Steppe
    - Csa, Csb, Csc → Mediterranean
    - Cwa, Cwb, Cwc, Cfa, Dsa, Dsb, Dwa, Dwb, Dfa, Dfb → TemperateBroadleaf
    - Cfb, Cfc → TemperateRainforest
    - Dsc, Dsd, Dwc, Dwd, Dfc, Dfd → Boreal
    - ET → Tundra; EF → IceCap; 0 → None
  - `vegetation_era_boundaries`: each era covers `[young, old)` in Ma:
    - Modern: < 20
    - FloweringPlants: 20 ≤ age < 130
    - ConiferAge: 130 ≤ age < 300
    - FirstForests: 300 ≤ age < 385
    - EarlyPlants: 385 ≤ age < 470
    - Barren: ≥ 470
    - So the snapshots 540 and 400 Ma map to Barren and EarlyPlants, 300 to FirstForests, 250 to
      ConiferAge, and 20 to FloweringPlants.
  - `no_grass_before_20_ma`: `effective_biome` maps Steppe → Mediterranean (shrubland) and
    MonsoonSavanna → TemperateBroadleaf (open woodland) for every era older than Modern.
  - `no_green_before_470_ma`: for Barren, every palette colour has `g ≤ max(r, b) + 0.02`.
  - `zonal_rule_bands`:
    - |lat| < 10: TropicalRainforest within 1,500 km of the coast, else MonsoonSavanna.
    - 10–30: HotDesert beyond 500 km, else MonsoonSavanna.
    - 30–45: Mediterranean within 500 km, else Steppe.
    - 45–60: TemperateBroadleaf within 1,000 km, else Steppe.
    - 60–70: Boreal.
    - > 70: Tundra.
    - Above `snowline_m`: IceCap if |lat| > 60, else Alpine.
  - `snowline`: 5,500 m for |lat| ≤ 20, falling 70 m per degree beyond, floored at 0.
- [ ] **Step 2: Implement.**
  - Palette colours are the builder's choice, from reference paintings and photos (spec §9).
    Keep the full table in `biome.rs` as data.
- [ ] **Step 3: Run the tests, check wasm, commit.**

### Task 4: Coarse fields and the region generator (core)

**Files:**
- Create: `crates/timefly_core/src/worldgen/{tiles,coarse,generate}.rs`

**Interfaces:**
- Consumes:
  - Task 2's `fbm` and `ridged`.
  - Task 3's frame, biome, era and palette.
  - M1 `CrustModel::resolve`, `Timeline::{select_frames, sea_level_anomaly_m}`,
    `globe::{DeepSampling, PlateRotations, plate_rotations, rendered_surface, RenderedSurface}`,
    `formats::Grid`, `cube::bilinear_taps`.
- Produces:
  - **Tiles:**
    - `pub struct TileSet { tiles: HashMap<(u8,u8), Grid> }`
    - `fn insert(&mut self, row: u8, col: u8, grid: Grid)`
    - `fn needed_for(frame: &RegionFrame, size_m: f64) -> Vec<(u8,u8)>`
    - `fn sample(&self, lat_deg: f64, lon_deg: f64) -> Option<(f32, u8)>`: bilinear elevation and
      nearest Köppen code.
  - **Coarse samples:**
    - `pub struct CoarseSample { pub elevation_m: f64, pub ice: f32, pub koppen: Option<u8>, pub lattice: LatticeKey }`
    - `pub enum LatticeKey { Geographic(DVec3), Tracked { plate_id: PlateId, plate_fixed: DVec3 }, EraFixed(DVec3) }`
  - **Coarse fields:** `pub trait CoarseField: Sync` with:
    - `fn sample(&self, era_dir: RenderDir) -> CoarseSample`
    - `fn source_spacing_m(&self) -> f64`
    - `fn paleolat(&self, era_dir: RenderDir) -> f64`
  - `pub struct RecentField<'a> { tiles: &'a TileSet, ice_era: &'a Grid, ice_present: &'a Grid, fallback: bool }`
    - `ice_era` and `ice_present` are ICE-6G cube frames with channels R = Topo, G = land,
      B = ice.
    - Elevation is `etopo + (Topo_era − Topo_present)`. Ice comes from `ice_era.B`. Spacing is
      1,852 m, or the globe cube's spacing when a tile is missing.
  - `pub struct BridgeField<'a> { tiles: &'a TileSet, anomaly_m: f64, fallback_frame: &'a Grid }`
    - Elevation is `etopo − anomaly_m`.
  - `pub struct DeepField<'a> { crust: &'a CrustModel, sampling: DeepSampling, rotations: Vec<PlateRotations>, frames: [&'a Grid; 2] }`
    - Elevation comes from `rendered_surface`; spacing is 111 km.
  - **Generator:**
    - `pub struct RegionSpec { pub center: RenderDir, pub snapshot: YearsBp, pub seed: u32, pub inner_res: u32, pub apron_res: u32 }`
      (sizes from Global Constraints)
    - `pub struct Field { pub res: u32, pub size_m: f64, pub elevation_m: Vec<f32>, pub surface: Vec<[u8; 4]> }`
      - `surface` holds `[biome, vegetation era, ice × 255, shore flag]`.
      - `fn height_at(&self, x_m: f64, z_m: f64) -> Option<f32>`: bilinear.
    - `pub struct RegionGrid { pub spec: RegionSpec, pub inner: Field, pub apron: Field }`
    - `pub struct RegionGenerator<'a, F: CoarseField>`
      - `fn new(spec, field: &'a F) -> Self`
      - `fn sample_point(&self, era_dir: RenderDir) -> (f32, [u8; 4])`
      - `fn rows_remaining(&self) -> u32`
      - `fn generate_rows(&mut self, max_rows: u32)`
      - `fn finish(self) -> RegionGrid`
    - `pub fn coastline_clamp(h_m: f64, corners_m: [f64; 4]) -> f64`

- [ ] **Step 1: Write the failing tests.** Use synthetic grids and a synthetic two-plate
  `CrustModel`, as M1's `globe.rs` tests do.
  - `coastline_clamp_rule`:
    - All corners > 0: the result is ≥ +1.
    - All corners < 0: the result is ≤ −1.
    - Mixed corners: unchanged.
    - Also a proptest over random corners and heights.
  - `detail_never_lifts_open_ocean`: over a synthetic all-ocean field, every generated height is
    ≤ −1.
  - `determinism`: generating the same spec twice gives identical `Field` bytes.
  - `overlap_agreement`: `sample_point` at a fixed era direction is identical for two specs with
    centres 100 km apart. Grid texels equal `sample_point(local_to_dir(texel))`.
  - `chunking_is_invisible`: generating in chunks of 1, 7 and 64 rows gives identical output.
  - `deep_matches_globe`: on the synthetic two-plate model, the coarse land/sea sign equals
    `rendered_surface(...).is_land()` wherever all four corners agree.
  - `recent_adds_ice_delta`:
    - A tile at 100 m with `Topo_era − Topo_present = +2,000` and ice 1.0 gives
      elevation ≈ 2,100 and the ice code 255.
    - A −100 m shelf with the delta +120 gives +20.
    - The second case is the M1 sea-level sign, through ICE-6G.
  - `bridge_uses_the_sea_level_curve`: `etopo − anomaly`.
  - `regions_at_awkward_places_are_finite`: centres at lon 179.9 and −179.9, 89.5°N, 89.5°S,
    open ocean, and a plate boundary all give all-finite output.
  - `vegetation_follows_age`: the surface era byte equals `vegetation_era(spec.snapshot)`.
- [ ] **Step 2: Implement.**
  - **Coarse lattice:** evaluate the coarse field on a lattice `max(source_spacing / 4, texel)`
    and upsample bilinearly. Detail and the lattice key are per texel; the plate comes from the
    nearest coarse node. Record why in a comment: the deep source is about 110 km, and resolving
    plates at every 293 m texel is wasted work.
  - **Coastline clamp:** corners are the coarse field at the four offsets (±spacing/2, ±spacing/2).
  - **Detail amplitude:** grows with coarse elevation and relief.
    - ridged octaves above about 1,500 m coarse elevation, or where the gradient is above 2%
    - fBm elsewhere
    - octaves only below the source spacing
    - The exact formula is the builder's choice, under a test that mountains' detail RMS exceeds
      plains'.
  - **Apron:** samples the same field at its own spacing. Its octaves stop at the apron spacing.
  - **Distance to coast** (zonal rule): a two-pass chamfer distance transform over the apron's
    coarse land mask. The inner field reuses it, upsampled.
- [ ] **Step 3: Benchmark.** Add a `#[test] #[ignore]` `generation_timing` that prints the time to
  generate a full synthetic deep region single-threaded. Record it in `performance.md`.
- [ ] **Step 4: Run the tests, check wasm, commit.**

### Task 5: Region data and generation in the shell

**Files:**
- Create: `crates/timefly/src/region/{mod,tiles,frames,generate}.rs`
- Modify: `crates/timefly/src/lib.rs`, `crates/timefly/src/data.rs` (expose the frame paths)

**Interfaces:**
- Consumes: Task 4's generator and fields; M1 `GlobeData`, `TfgridLoader`, diagnostics
  `add_domain_event` and `add_logged_state`.
- Produces:
  - `#[derive(States)] pub enum RegionState { #[default] Globe, Generating, InRegion }`
    (through `add_logged_state`)
  - `#[derive(Message)] pub struct RegionRequested { pub era_dir: RenderDir, pub snapshot: YearsBp }`
  - `#[derive(Message)] pub struct RegionGenerated { pub spec: RegionSpec, pub millis: u32, pub tiles_fetched: u32, pub fallback: bool }`
  - `RegionEntered`, `RegionLeft`, `RegionSnapshotChanged` messages (domain events)
  - `#[derive(Resource)] pub struct ActiveRegion { pub grid: Arc<RegionGrid>, pub frame: RegionFrame }`
  - `pub struct CpuGrid(pub Grid)`: an asset, loaded with `CpuGridLoader` for `.tfgrid` under the
    label `#cpu`. It keeps CPU bytes and never becomes an `Image`.
  - `pub struct TileCache`: an LRU of decoded tiles, capped at 96 tiles.

- [ ] **Step 1: Write the failing tests** (headless App, M1's `data_loading.rs` pattern).
  - `generates_a_region_from_the_tiny_bake`:
    - Request a region on the tiny fixture bake (extended with four fixture tiles in
      `tests/assets/data/tiles/`).
    - Expect `Generating → InRegion` within 20 s.
    - `RegionGenerated` reports `fallback == false`.
  - `missing_tile_falls_back_with_warning`: delete one fixture tile. Expect `InRegion` with
    `fallback == true`, and a `warn` naming the redacted tile path.
  - `corrupt_tile_falls_back_with_warning`: same, for a truncated file and a SHA mismatch.
  - `tile_cache_is_bounded`: entering 30 synthetic regions keeps the cache at ≤ 96 tiles.
  - `leaving_frees_the_region`: after `RegionLeft`, `ActiveRegion` is removed and the CPU grids'
    strong handles drop.
- [ ] **Step 2: Implement.**
  - **Tile loading:** goes through the asset server with the same relative path everywhere
    (`data/tiles/rRRcCC.tfgrid`). The asset server reads bundled files on desktop and fetches
    from the Pages site on the web. Verify each tile's SHA-256 against the manifest.
  - **Native generation:** splits rows across `AsyncComputeTaskPool` tasks over disjoint row
    ranges, each with its own `RegionGenerator` view.
  - **Web generation:** calls `generate_rows` in a loop until `WEB_GEN_BUDGET_MS` passes each
    frame.
  - **Timing:** `RegionGenerated.millis` is measured from the request to the grid being ready.
- [ ] **Step 3: Run the tests, check wasm, commit.**

### Task 6: CDLOD selection and morph reference (core)

**Files:**
- Create: `crates/timefly_core/src/terrain/{mod,cdlod}.rs`

**Interfaces:**
- Produces:
  - `pub struct LodSettings { pub patch_quads: u32, pub levels: u32, pub finest_patch_m: f64, pub base_range_m: f64, pub morph_start: f32, pub morph_end: f32 }`
    with `LodSettings::region()`.
    - Defaults: 32 quads; 10 levels; finest patch 4,688 m (146.5 m quads); morph 0.70 / 0.98.
    - `base_range_m` is the builder's choice, under the tests below.
  - `pub struct Patch { pub origin_m: [f64; 2], pub size_m: f64, pub lod: u32, pub morph_range_m: [f32; 2] }`
  - `pub fn select_patches(camera_m: DVec3, settings: &LodSettings) -> Vec<Patch>`: covers
    ±`APRON_SIZE_M / 2`.
  - `pub fn morph_vertex(grid_index: [u32; 2], patch: &Patch, camera_m: [f32; 3], settings: &LodSettings) -> [f32; 2]`:
    snaps by whole-number grid index, the spec §7.2 fix.
  - `pub fn blended_height_m(grid: &RegionGrid, x_m: f64, z_m: f64) -> f32`
    - The inner field inside `REGION_SIZE_M / 2 − 4 × apron spacing`.
    - The apron outside `REGION_SIZE_M / 2`.
    - Smoothstep between the two.
    - Mirrored exactly in `terrain.wesl`.
  - `pub fn curvature_drop_m(horizontal_distance_m: f64) -> f64 = d² / (2 × 6_371_000)`

- [ ] **Step 1: Write the failing tests.**
  - `shared_edges_match_after_morph`: a proptest over camera positions within the region and
    altitudes from 150 m to 60 km. For every pair of selected patches sharing an edge, the
    morphed positions of their shared-edge vertices coincide exactly (f32 equality). There are
    no T-junctions: neighbouring LODs differ by at most 1.
  - `morph_is_continuous_across_rings`: the morph factor reaches 1 at each ring's outer range.
  - `selection_covers_the_apron_without_overlap`: patch footprints tile the square with no gaps
    or overlaps.
  - `patch_count_is_bounded`: at most 600 patches at any allowed camera position.
  - `blend_is_continuous`: `blended_height_m` is continuous across the inner/apron transition
    (sampled at 1 m steps).
- [ ] **Step 2: Implement.** Port the draft's selection and morph from
  `reference/draft/terrain_cdlod.wesl` into Rust, applying the index-snap fix.
- [ ] **Step 3: Run the tests, check wasm, commit.**

### Task 7: CDLOD terrain rendering

**Files:**
- Create: `crates/timefly/src/terrain/{mod,material}.rs`,
  `crates/timefly/assets/shaders/{terrain,terrain_prepass}.wesl`
- Modify: `crates/timefly/tests/region_gpu.rs`

**Interfaces:**
- Consumes: Task 5's `ActiveRegion`; Task 6's `select_patches`, `LodSettings`, `Patch` and
  `blended_height_m`.
- Produces:
  - `TerrainCdlodPlugin`.
  - `pub struct TerrainExtension`: `MaterialExtension`, with the patch storage buffer at
    binding 100, height textures (inner and apron, `R32Float`) at 101–102, the surface texture
    (`Rgba8Unorm`) at 103–104, and a palette texture at 105.
  - `pub fn patch_instances(patches: &[Patch], settings: &LodSettings) -> Vec<PatchInstance>`
    (pure).
  - `#[derive(Component)] pub struct TerrainPatch`.

- [ ] **Step 1: Write the failing tests.**
  - `patch_instances_pack_like_the_shader`: a pure test of the 16-byte-aligned layout.
  - `patch_pool_reuses_entities`: in a headless App, moving the camera keeps the entity count
    at or below the maximum and never despawns per frame.
- [ ] **Step 2: Implement.**
  - **Vertex shader** (the draft, with the spec's fixes):
    - snap by `grid_uv * N` index
    - Bevy view and mesh bindings, with `get_tag(instance_index)` indexing the patch buffer
    - displacement from `blended_height_m`, plus `curvature_drop_m`
    - normals by central differences over one texel
    - voxel clipping removed (keep the uniform slot)
  - **Prepass vertex shader:** applies the same displacement, so shadows and depth match.
  - **Shadows:** the sun is a `DirectionalLight` with `CascadeShadowConfig`, sized for
    60 km.
- [ ] **Step 3: GPU check.** `region_gpu.rs::wesl_morph_matches_rust` (`#[ignore]`): for 512
  random vertices, the shader's morphed `xz` equals `morph_vertex` within 1e-3 m, and the height
  equals `blended_height_m` within 0.5 m. Run it locally. Expected: PASS.
- [ ] **Step 4: Check by eye.**
  - Run `cargo run -p timefly -- --region=46.5,10,0`.
  - Screenshot to `docs/testing/region-m2/cdlod-wire.png` using a debug wireframe toggle (`F2`).
  - No cracks at ring transitions while flying.
- [ ] **Step 5: Run the tests, check wasm, commit.**

### Task 8: Painterly terrain, sky, fog and water

**Files:**
- Create: `crates/timefly/src/{water,sky}.rs`, `crates/timefly/assets/shaders/{water,sky}.wesl`
- Modify: `crates/timefly/src/terrain/material.rs`, `crates/timefly/assets/shaders/terrain.wesl`

**Interfaces:**
- Consumes: Task 3's `palette`; Task 2's `noise.wesl`; Task 6's `blended_height_m` (WESL copy).
- Produces:
  - `WaterPlugin` and `WaterMaterial`, a `Material` with the same height textures.
  - `SkyPlugin`, with `pub fn sky_tint(era: VegetationEra, biome: BiomeClass) -> (Color, Color, Color)`
    (pure).
  - `pub fn palette_texels() -> Vec<[u8; 4]>` (pure).

- [ ] **Step 1: Write the failing tests.**
  - `palette_texture_matches_core`: `palette_texels` reproduces `palette()` for every era and
    biome, after sRGB conversion.
  - `sky_tint_is_era_aware`: Barren skies are warmer and dustier than Modern. Pin one ordering
    on the horizon hue.
  - `water_depth_tint_is_monotonic`: the pure `water_tint(depth_m)` darkens with depth from 0
    to 200 m.
- [ ] **Step 2: Implement the terrain fragment** (M2 design §6.4):
  - the palette lookup from the surface code
  - rock above slope 0.6
  - snow from the ice byte and `snowline_m`
  - brush noise: `noise.wesl` at 400 m, 60 m and 9 m world scales
  - Bevy PBR lighting
  - a ramped diffuse (3 soft bands) and a hue-shifted shadow
- [ ] **Step 3: Implement the sky and water.**
  - **Sky:** a gradient dome plus `DistanceFog` from `sky_tint`.
  - **Water:**
    - a curved plane at y = 0
    - Schlick Fresnel with f0 = 0.02
    - the depth tint from `blended_height_m`
    - foam where 0 < depth < 4 m, broken up with noise
- [ ] **Step 4: Check by eye.** Screenshots go in `docs/testing/region-m2/`:
  - The three bench regions (Alps 0 ka, Scandinavia 21 ka, Pangaea 250 Ma).
  - One deep region at a snapshot in each vegetation era: 540, 400, 300, 250, 100 and 10 Ma.

  Check them against the anti-goals in M2 design §2.
- [ ] **Step 5: Run the tests, check wasm, commit.**

### Task 9: Entering and leaving, free-fly camera, snapshots in a region

**Files:**
- Create: `crates/timefly/src/fly_camera.rs`
- Modify: `crates/timefly/src/region/mod.rs`, `crates/timefly/src/args.rs`,
  `crates/timefly/src/camera.rs` (pause the orbit while `InRegion`),
  `crates/timefly/src/ui/slider.rs` (snapping while `InRegion`), `crates/timefly/src/globe/mod.rs`
  (hide while `InRegion`)

**Interfaces:**
- Consumes: Task 5's states and events; Task 6's `blended_height_m`; M1 `OrbitState`,
  `GlobeTime`, `CrustModel::{resolve, forward}`, `STANDARD_SNAPSHOTS`.
- Produces:
  - **Clicks and the globe:**
    - `pub fn is_double_click(prev: Option<(f64, Vec2)>, now_s: f64, pos: Vec2) -> bool`:
      within 0.35 s and 6 px.
    - `pub fn globe_hit(ray: Ray3d) -> Option<RenderDir>`: the ray against a sphere of radius
      `EARTH_RADIUS_KM`.
  - **Snapshots:**
    - `pub fn nearest_snapshot(age: YearsBp) -> YearsBp`: nearest in slider position.
    - `pub fn carry_crust_position(crust: &CrustModel, era_dir: RenderDir, from: YearsBp, to: YearsBp) -> Option<RenderDir>`:
      `None` for era-fixed or newly invalid crust.
  - **Camera:**
    - `pub struct FlyState { pub pos_m: Vec3, pub yaw: f32, pub pitch: f32 }`
    - `pub struct FlyInput { pub move_local: Vec3, pub look_delta: Vec2, pub boost: bool, pub dt_s: f32 }`
    - `pub fn fly_step(state: FlyState, input: FlyInput, ground_m: impl Fn(f32, f32) -> f32) -> FlyState`
  - **Launch arguments:** `--region=LAT,LON,AGE_YEARS` (native) and `?region=LAT,LON,AGE_YEARS`
    (web) start directly in a region.

- [ ] **Step 1: Write the failing tests.**
  - `double_click_window`: 0.34 s and 5 px is a double click; 0.36 s or 7 px is not.
  - `globe_hit_returns_the_front_surface`.
  - `nearest_snapshot_by_slider_position`: 30 ka → 21 ka, not 50 ka. Pin both neighbours by
    their slider distance.
  - `clearance_holds_for_large_steps`: a proptest over `dt` up to 0.5 s, boost, and terrain from
    −5,000 to +8,000 m. After every step, `pos.y ≥ max(ground, 0) + CAMERA_CLEARANCE_M` and
    `pos.y ≤ CAMERA_CEILING_M`. The speed scales with height above ground.
  - `soft_edge_nudges_back`: beyond `REGION_SIZE_M / 2`, the horizontal velocity gains an inward
    component.
  - `snapshot_change_keeps_crust_position`: on the synthetic two-plate model, the carried
    direction follows the plate's rotation.
  - `snapshot_change_to_missing_crust_returns_to_globe`: a headless App in `InRegion`. Changing
    to an era where `carry_crust_position` returns `None` ends in `RegionState::Globe` with
    `RegionLeft`.
  - `region_arg_parses`: native and web forms; malformed values are ignored with a `warn`
    (M1's `args.rs` rules).
- [ ] **Step 2: Implement the flow** (M2 design §6.1–6.2):
  - **Entering:** a double-click snaps the slider, then sends `RegionRequested`. While generating,
    the orbit camera eases toward the spot. When generation finishes, crossfade over
    `ENTER_FADE_S` to the fly camera at `CAMERA_START_ALTITUDE_M`, looking north, pitched
    down 20°.
  - **Leaving:** `Backspace` crossfades back to the globe, centred on the region.
  - **Slider in a region:** a drag is held until release, then snaps. A snapshot change
    regenerates around the carried crust position and keeps the height above ground, with a
    `SNAPSHOT_FADE_S` crossfade.
  - **"Descending…" caption:** shown only after 1 s of generating.
  - **Controls:** `W` `A` `S` `D`, drag to look, `Space`/`E` up, `Ctrl`/`Q` down, `Shift` boost.
- [ ] **Step 3: Check by hand.** Add a section to `docs/testing/mac-manual-checks.md`:
  - enter and leave
  - the snapshot change keeps the place
  - Backspace
  - leaving at the edge
  - the caption only on slow generation
- [ ] **Step 4: Run the tests, check wasm, commit.**

### Task 10: Region benchmark, packaging, web, acceptance

**Files:**
- Modify: `crates/timefly/src/bench.rs`, `crates/timefly/src/args.rs`,
  `.github/workflows/linux-package.yml`, `.github/workflows/pages.yml`,
  `packaging/linux/stage.sh`, `docs/testing/region-m2/performance.md`,
  `docs/testing/linux-vulkan.md`, `docs/superpowers/plans/2026-11-23-milestone-2-region-worlds.md`
  (appendix)

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `--region-bench` (native) and `?region-bench` (web).
  - **Scripted path:** in each of three regions (Alps 46.5,10 at 0 ka; Scandinavia 63,15 at
    21 ka; Pangaea interior at 250 Ma, centre chosen and pinned in the bench), 60 s of:
    1. a low pass at 300 m above ground
    2. a climb to 30 km
    3. a run along the coast
    4. a sweep toward the apron
  - **Output:** the M1 result block per region: frame stats under the R14 rule, generation
    millis, tiles fetched, peak memory (native: resident set; web: `performance.memory` where
    available), and the bake revision.
  - It exits non-zero on FAIL (exit 3, as M1).

- [ ] **Step 1: Write the failing tests.**
  - `region_bench_script_is_deterministic`: same inputs, same camera path.
  - `region_bench_reports_all_regions`: a headless run on the tiny bake prints three result
    blocks.
  - `bench_cli_flags`.
- [ ] **Step 2: Implement the bench, then run it.**
  - Native M4 at 2560×1440.
  - Chrome WebGPU in the in-app browser pane (visible; check `tabs_context`).
  - Ten enter/leave cycles while measuring memory: it returns within 10% of the first cycle's
    baseline.
  - Record all of it in `performance.md`.
  - Targets: 60 fps native under R14; Chrome 30+ provisional; generation ≤ 3 s native and
    ≤ 8 s web with a warm cache.
- [ ] **Step 3: Update CI and packaging.**
  - **`linux-package.yml`:** the build job fetches the new sources and bakes tiles. Cache
    `data/raw`, keyed on `data/sources.json`, and the tiles, keyed on the bake inputs.
    `--check-data` also loads three tiles.
  - **`stage.sh`:** ships `assets/data/tiles/`.
  - **`pages.yml`:** add the same raw-data and tile caches (today it fetches and bakes on every
    deploy), and publish the tiles.
  - Ask the owner before pushing and before each workflow run.
- [ ] **Step 4: Update the docs.**
  - `linux-vulkan.md`: add a region checklist (the three bench regions, enter and leave, a
    snapshot change) for the owner's Linux run.
  - The plan's appendix records every ruling and owner decision made during M2, so they survive
    the git-ignored ledger. This is M1's lesson.
- [ ] **Step 5: Run the full suites,** `cargo fmt --check`, clippy, and the wasm check. Commit.

---

## Known limitations (accepted for Milestone 2)

- **Deep-era biomes depend on the apron (R9).** Coast distance comes from each region's own
  apron, so overlapping deep-time regions can differ in biome colour near distance thresholds.
  Heights stay exact. The later fix is a global coast-distance precompute.
- **Thin cracks between plate footprints (R17).** Within 50 km of plate land, a region shows the
  source frame's land where the globe draws a one-pixel −200 m water line. The globe can adopt
  the same fill later.
- **Glacial ice in mountains (R18).** ICE-6G_C's 1° grid has no Alpine ice at 21 ka, so a lowered
  snowline stands in for mountain glaciers: summits and ridges are iced, but not the
  valley-filling ice network of the Last Glacial Maximum. The credits say so. The anomaly is the
  one a region's heights carry: the timeline's for recent and bridge snapshots, and for the deep
  brackets that read the modern-terrain bridge frame (50 ka to about 4 Ma) that frame's anomaly
  by its weight in the blend (final fix wave), so the Alps keep a lowered snowline past 21 ka
  and it fades toward the 4.47 Ma PaleoDEM; 0 from the first PaleoDEM bracket on.
- **The overview decodes on the web's main thread (R11).** The 12.4 MB overview is checked
  (SHA-256) and decoded (37 MB) in one piece on the first region of a session. Ruling R11 gates
  it: no frame of the first region's load may exceed 250 ms in Chrome WebGPU on the M4
  (`--region-bench` reports it; see `docs/testing/region-m2/performance.md`). If it fails, the
  overview is split into latitude bands in the bake or decoded in chunks.
- **ETOPO detail steps at the 0.5 weight (R5).** A deep snapshot reads the ETOPO tiles only while
  the ETOPO-backed bridge endpoint weighs at least 0.5; below that it reads the 5′ overview,
  though ETOPO still carries up to 49% of the height. Detail changes in one step at that age.
- **Deep-era biomes are zonal approximations.** They come from palaeolatitude, elevation and
  coast distance, not from a climate model. The credits carry the data note.
- **Ice-age biomes use the present-day climate.** Recent snapshots (21 ka and so on) read the
  1991–2020 Köppen-Geiger classes; only ice, sea level and the snowline change with the age. The
  credits carry the data note.
- **The two-level deep path near plate edges (Task 4, flagged for the final review).**
  ETOPO-dominant deep snapshots (50 ka to about 2 Ma) run the globe's reference only on a
  27.75 km support lattice. Support points match it to 1e-6 m on land and plate interiors to
  0.12 m, but within one support cell of a plate edge heights err by up to 109 m. Land and sea
  are checked against the globe's reference (|reference| > 150 m) at 50 ka, 100 ka, 1 Ma and
  2 Ma: 0 misses in 51,689–57,238 synthetic samples per age, and, since the final fix wave,
  0 on the real bake (four regions per age, 8,340–12,949 samples each; it had 8 misses at 1 and
  2 Ma, where a one-plate cell's bilinear PaleoDEM residual put a sample a few metres above sea
  level over plate water the globe draws as the −200 m crack fill). A one-plate sample within
  150 m of sea level now takes its plate's own terms (about 7% more generation time). Heights
  between nodes still carry the bilinear error away from the coast. `deep_matches_globe` runs
  only at 100 Ma.
- **The plate ranker (R19, Task 9).** A snapshot change carries the camera with the plate the
  globe draws, ranked from the old snapshot's cached frames. For the bridge endpoint it reads
  the cube frame (what the globe draws), not ETOPO. If the "from" frames were evicted from the
  4-slot frame cache, the overlap falls back to the lowest plate ID (logged at `debug`).
- **Failed loads are not retried (Task 5).** If the tile manifest or the overview fails to load,
  it is not fetched again for the session: regions fall back to the globe frames (with a `warn`
  at the failure, none per region).
- **Loads time out (final fix wave).** A load that neither arrives nor fails within 20 s of
  running frames (the load clock adds each frame's real delta clamped to 0.25 s, so a paused or
  throttled page does not time out its loads; Chrome-bench fix round) (the manifest's size at
  4 Mbit/s when longer: about 25 s for the overview) is abandoned with a
  `warn`: a tile counts as missing (the region falls back), the manifest or the overview as
  failed for the session, and a frame fails the request back to the globe. On a connection
  slower than about 4 Mbit/s the overview can time out though it would have arrived; the aprons
  then use the globe frames for the session.
- **The fallback notice (Review Focus 2, final fix wave).** A region with a missing or corrupt
  tile shows "terrain detail unavailable here" near the top of the view for 3 s after it
  appears; the job logs a `warn` per tile. A region without the overview (aprons on the globe
  frames) or without the manifest shows no notice when every inner tile it reads is present:
  the notice follows `RegionGenerated.fallback`, which concerns the inner field.
- **Offshore islands (Task 4).** The coastline's corner rule takes corners from the upsampled
  coarse lattice, so small offshore islands can appear within about spacing/4 of deep-era coasts.
- **The haze holds through a snapshot change (Task 9).** The pale fade overlay stays at full
  cover for the whole generation of the new region (0.6 s natively, longer on the web; the web
  takes 12 ms slices while it covers, against 6 ms otherwise), and "Descending..." can show on a
  slow change. `Backspace` or a missing-crust re-carry during a
  change restarts the leave fade, so the stale region can flash for about 0.4 s; `Backspace`
  during the entering fade is ignored.
- **`--region` / `?region=` waits for the interactive globe (final fix wave),** as the bench
  does: the launch region is requested once `GlobeRenderStatus::Interactive` is reached, never
  under the loading screen.
- **Region pipelines compile at startup (R11, final fix wave).** While the globe's pipelines
  compile, a stand-in region (one patch, the sea and the sky, the region camera's components
  and sun, a 1 × 1 `Rgba8UnormSrgb` target, its own render layer) draws the main-pass
  pipelines; the globe's "interactive" waits for them, so a cold load grows by their compile
  time (natively about 1.4 s on a cold Metal shader cache, under 0.1 s on a warm one). Once the
  globe is interactive the stand-in's sun casts shadows until the shadow pipelines have compiled
  (about 0.1 s natively; on the web one compile on the idle, just-loaded globe), then it is
  removed with every asset it made. Native first-region worst frame: 1,151 ms → 27 ms with the
  region shaders cold, 85 ms → 25 ms warm. Costs: about 36 MiB more `phys_footprint` for the
  session on the M4 (744–757 MiB on the globe after leaves, against 705–720; the cause of the
  remainder was not found), and the target format assumes an 8-bit sRGB window (true of Metal,
  Vulkan and WebGPU surfaces here); on another surface format the warm-up would compile unused
  pipelines and the first region would compile its own as before.
- **Bench caveats (R20, R21).** The native M4 bench ran at 1920 × 951 (the 1080p display clamps
  the requested window); the 2560 × 1440 desktop measurement is part of the owner's Linux run
  (`docs/testing/linux-vulkan.md`, section 7). In the final fix wave the M4 also ran at
  2560 × 1440 on a 4K display (uncapped, PASS on all four regions; performance.md). The Chrome
  WebGPU bench, with ruling R11's first-region gate, is the controller's run and was pending
  when this appendix was written.
- **Chrome/Tint paths unverified until that run.** Code paths Metal never compiles: the
  prepass's `UNCLIPPED_DEPTH_ORTHO_EMULATION` branch (Task 7), the water shader's PBR call after
  a non-uniform discard (Task 8) and a dynamic const-array index in the noise (Task 2). Per-pixel
  terrain normals cost 32 texture reads per fragment (a web performance watch).
- **Generation is mostly sequential.** The region prep (70–80% of native generation) runs on one
  thread; only the rows are parallel. Native regions take under 1 s, except the ETOPO-dominant
  deep coasts (the Aegean at 1 Ma: about 2.2 s), so this is headroom natively. A cancelled
  native job runs to completion in the background before it is dropped, so rapid `]` presses
  stack native generations (no cancel flag; deferred).
- **The web's generation duty cycle (final fix wave, Chrome-bench fix round).** On the web a
  region generates in time-boxed slices on the main thread: 6 ms per frame (wall time about
  2.8 × CPU time at 60 Hz) while a region is flown or shows through a fade, and 12 ms when no
  region is on screen (about 1.4 ×): a first entry (on the globe or descending) or a snapshot
  change held under the fade overlay. In the browser pane the Aegean's first entry took 613 and
  628 slices at 6 ms (about 10.5 s at 60 Hz, over the 8 s target); natively the time-boxed path
  needs 54% as many slices at 12 ms (523 → 282), so it projects to about 340 slices, 5.7 s. A
  12 ms slice plus the globe's own frame must stay under 16.7 ms, or the page drops to 30 Hz and
  the gain halves; the gated real-Chrome run decides.
- **Entering a region is abrupt.** The globe camera stops at about 955 km (the minimum orbit)
  and the view fades through haze over 0.8 s to the fly camera 8 km above the ground. The smooth
  descent is M3's zoom-and-swap (owner decision, 2026-11-23).
- **Paint follows the morphing geometry.** Beach, mat and snow thresholds use the interpolated
  CDLOD height, so snowlines and beach edges crawl slightly as patches morph. Foam is a solid pale
  band from high up (an art-pass item).
- **The geographic-frame limit is a constant.** `GEOGRAPHIC_FRAME_MAX_YEARS` (26,076 years)
  matches the bake's ICE-6G_C coverage but is not read from the timeline.
- **The packages grow by about 257 MB** for the region tiles (2,592 tiles of 244.0 MB, the 12.4 MB
  overview and the manifest). The tiles are already zstd-compressed, so the `.deb`'s xz gains
  almost nothing on them.
- **Browser numbers are provisional.** As in M1, Chrome is measured on an Apple M4, faster than
  the spec's M1-class reference.

---

## Appendix: Rulings and deviations

The execution ledger (`.superpowers/sdd/2026-11-23-milestone-2-region-worlds/progress.md`) is
git-ignored, so the rulings and owner decisions are copied here verbatim. Each ruling gives what
was decided, why, and the cost if it proves wrong.

**Owner decisions**

- 2026-11-23: present-day elevation option B (ETOPO 2022 60″). "Go with your picks for the rest" (all other choices are controller calls, marked *(call)* in the design).
- 2026-11-23: downloads approved: ETOPO_2022_v1_60s_N90W180_surface.nc (478,290,125 B) and koppen_geiger_tif.zip (130.6 MB, md5 7fc2f5a15d4f5fe0ce59c9a9b502aa09). The controller downloads; implementers never download.
- Execution: subagent-driven. This is the controller's pick under "go with your picks"; the owner was told the recommendation.

**Controller rulings**

- Ruling R1: the region constants (`REGION_SIZE_M`, `APRON_SIZE_M`, `REGION_RES`, `APRON_RES`) live in `timefly_core::worldgen` (`worldgen/mod.rs`), added by T3. `PATCH_QUADS`, `MORPH_START` and `MORPH_END` live in `timefly_core::terrain` (T6). Camera and fade constants live in the shell (`fly_camera.rs` and `region/mod.rs`, T9). — No task named a home, and the core needs the sizes first. — Cost if wrong: a move.
- Ruling R2: T4's `RegionGenerator` also exposes a pure `fn rows(&self, field: FieldKind, range: Range<u32>) -> FieldRows` (`FieldKind::{Inner, Apron}`; `FieldRows { elevation_m: Vec<f32>, surface: Vec<[u8; 4]> }`), plus `fn assemble(&self, inner: Vec<FieldRows>, apron: Vec<FieldRows>) -> RegionGrid`. `new` precomputes the coarse lattice and coast distance. `generate_rows` and `finish` remain as the sequential convenience, built on `rows`. — T5 needs parallel row ranges on native and time-boxed chunks on the web from one pure API. — Cost if wrong: an API reshuffle in T4 and T5.
- Ruling R3: T5 writes its fixture tiles (`crates/timefly/tests/assets/data/tiles/`, four tiles plus a manifest) with a small, committed generator, either a test helper or a bake-tool call on synthetic input, whichever is simpler. — The plan didn't say who produces them. — Cost if wrong: none.
- Ruling R4: T1's implementer does not download. The controller already did, and the dispatch carries the file paths and SHA-256s for `data/sources.json`. — Downloads need the owner's OK (M1 ruling R2). — Cost if wrong: none.
- Ruling R5: there is no separate "bridge era" field. Real timeline: ICE-6G ≤ 26.076 ka; bridge frame (modern terrain, source rotation 0) at 30 ka; next PaleoDEM at 4.47 Ma, so the 50 ka/100 ka/1 Ma snapshots are `FrameSelection::Deep` brackets (bridge frame ↔ 4.47 Ma), and ages between 26.076 and 30 ka are `FrameSelection::Bridge`. DeepField samples each endpoint through a per-endpoint sampler: the modern-terrain bridge frame reads ETOPO tiles at the rotated direction (geographic, present-day), any other endpoint reads its cube frame. The M1 rule elev − anomaly still applies per endpoint (R11). `FrameSelection::Bridge` (26–30 ka): RecentField's last ICE-6G result crossfaded into that DeepField by `deep_weight`. source_spacing_m = 1,852 m when the ETOPO-backed endpoint's weight ≥ 0.5, else 111 km. BridgeField from the plan is dropped. — This matches M1's frame selection, so regions agree with the globe, and recent-ish deep snapshots get ETOPO detail. — Cost if wrong: one field reshaped.
- Ruling R6: RecentField takes the ICE-6G bracket (two frames + older_weight) and the present-day frame: Topo_era = lerp(Topo_young, Topo_old, w); delta = Topo_era − Topo_present, where present is timeline frame 0. Ice = lerp of the B channels. — Most recent snapshots fall between ICE-6G frames (e.g. 21 ka = 0.021 Ma vs frames at 0.020076/0.021076). — Cost if wrong: none (single-frame is the w=0 case).
- Ruling R7: the apron never reads tiles. A global 5′ overview grid (`assets/data/tiles/overview.tfgrid`, 2-channel F16 LatLon node lattice 2161 × 4321, lat 90→−90 and lon −180→+180 at 1/12°, elevation = mean of the 60″ nodes in each 5×5 block, Köppen = mode) is baked by `timefly-bake tiles` and listed in the manifest. TileSet gains `overview: Option<Arc<Grid>>`. Samples read a tile if one is loaded, else the overview, else the cube frames. `needed_for` covers the inner field only (+ margin). — This keeps every apron texel a pure function of its direction (overlap stays exact), bounds tile counts to the inner field (≤ ~80 even at the poles), improves the missing-tile fallback (9 km instead of ~78 km), and is one ~10–15 MB fetch on the web. — Cost if wrong: a coarser horizon than per-tile data.
- Ruling R8: construction becomes incremental: `RegionPrep::new(spec, &field)` (cheap), `prep.step(max_nodes) -> bool` (true when done) and `prep.finish() -> RegionGenerator`. `RegionGenerator::new` stays as `prep.step(u32::MAX)` then finish. — Task 5's web path must stay within its 6 ms budget per frame; a 0.1–0.6 s (longer on wasm) block on entry would stall the transition. — Cost if wrong: API churn in T5.
- Ruling R9: parked: deep-era biomes depend on each region's apron through coast distance, so overlapping regions may differ in biome colour near distance thresholds. Terrain heights stay exact. Accepted for M2 as a known limitation (plan appendix); a global coast-distance precompute is the later fix. — Spec §5.2 agreement is about the world surface; a small biome-colour shift between visits is not a coastline or ice disagreement. — Cost if wrong: a visible colour shift when re-entering an adjacent deep region.
- Ruling R10: split now: `worldgen/coarse/{mod,recent,deep}.rs` and `worldgen/generate/{mod,lattice,detail}.rs` (tests stay in generate/tests.rs). — Over the ~600-line guide, and T5–T7 will read them. — Cost if wrong: churn.
- Ruling R11: Important 2 split. Now in T5: bounded LRU cache of decoded CPU frames by image slot (≤ 4 slots); count prep.finish/assemble (and the probe) inside the slice timing so longest_ms tells the truth. Gate in T10: in Chrome WebGPU on the M4, no single frame during the first region's load may exceed 250 ms; if it does, split the overview into latitude bands in the bake (or decode it in chunks) and re-measure. — wasm has no worker pool, and the 12.4 MB overview is R7's own cost; the threshold gives T10 a pass/fail. — Cost if wrong: a visible hitch on the first web region.
- Ruling R12: odd grid indices morph to the even index below (i − 1), standard CDLOD. The brief's "midpoint of its even neighbours" is the vertex's own xz position, so it was a wording error. T7 must mirror i − 1. — Cost if wrong: none (equivalent crack-free scheme).
- Ruling R13: T7's GPU morph check tolerance on xz is max(1e-3 m, 1 f32 ulp at |coordinate|) (one ulp ≈ 0.06 m at 1.2 Mm), to allow fused multiply-adds. Height tolerance stays 0.5 m. — Cost if wrong: a looser parity check on far apron vertices only.
- Ruling R14: finest patch 4,687.5 m (APRON_SIZE_M/512), not the brief's rounded 4,688 m, so ten levels tile ±1.2 Mm exactly. — Cost if wrong: none.
- Ruling R15 (preflight miss): T7's eye check needs `--region` (owned by T9) and a camera in the region. T7 adds the `--region=LAT,LON,AGE_YEARS` / `?region=` launch arg (parse + startup RegionRequested once globe data is Ready) and a fixed debug camera at CAMERA_START_ALTITUDE_M (8,000 m) above the centre, looking north and pitched down 20°; T9 swaps in the fly camera and keeps the arg (its region_arg_parses test then extends T7's). Screenshots come from an offscreen `examples/region_tour.rs` (M1 ui_tour pattern: render to an image, save, exit by itself), never a windowed app killed by timeout. — T7 must be verifiable by eye before T9. — Cost if wrong: small churn in T9.
- Ruling R16: T7's xz tolerance is max(1 mm, 2 ulps of (x − patch root)), replacing R13's "1 ulp at |coordinate|". The GPU fuses `cells × quad + root` into one rounding of an intermediate up to 2,400 km, so R13 can't pass on Metal; neighbours evaluate the identical expression, so GPU crack-freedom is unaffected (measured worst 1.375 such ulps; 198/199 differences match a fused model). — Cost if wrong: a looser CPU/GPU parity bound only.
- Ruling R17: regions deliberately differ from the globe in thin era-fixed cracks (≤ 50 km from plate land): they show the source frame's land, where the globe draws a 1-pixel −200 m water line. The globe's line is itself an artefact of the crack between footprints; the spec's agreement rule concerns real coastlines. The globe can adopt the same fill later (follow-up). — Cost if wrong: a hairline mismatch with the globe at plate seams.
- Ruling R18: glacial snowline depression. snowline_m gains the era's sea-level anomaly: snowline lowered by 1,000 m × (anomaly / −120 m), clamped to [0, 1,200] m of lowering, in core and the shader mirror. A credits data note says mountain glaciers below ICE-6G's 1° grid are approximated by a lowered snowline. Follow-up to T8 after the T9 review. — The Alps at the LGM were glaciated (spec anti-goal: ice must match era); a cheap physically motivated proxy (LGM ELA depression ≈ 1 km) beats bare peaks. — Cost if wrong: snow slightly too low or too high in glacial eras.
- Ruling R19: carry_crust_position gains a `surface_for_candidate` closure (a deviation from the plan's signature), which the shell builds with rendered_surface's ranking from the `from` snapshot's CPU frames (FrameCache) and DeepSampling. Inside the geographic frame (recent) the carry stays the identity. — Spec §5.2's overlap rule must be the same everywhere; the plan's signature was the defect. — Cost if wrong: one more frame lookup per snapshot change.
- Ruling R20: T10 split as in M1 (R3): the implementer builds the region bench, runs it natively on the M4, makes the CI/Pages/packaging/docs/plan-appendix edits and commits locally. The controller runs the Chrome WebGPU bench in the in-app browser pane (including R11's 250 ms first-region frame gate) and, only with the owner's OK, pushes and dispatches CI. — Subagents lack the browser pane, and pushes need the owner. — Cost if wrong: none.
- Ruling R21: the native 2560×1440 measurement moves to the owner's Linux run (spec §15 measures the desktop target on the Linux machine). The M4 reports at its display's size (1920×951 now), labelled as such. — No code can fix a 1080p display. — Cost if wrong: M4 GPU headroom at 1440p unknown until the Linux run.
- Ruling R22: add a fourth bench region over the Aegean, era-frame 38.0, 24.5 at 1 Ma: coastlines throughout the inner field (close-range water, foam and shore shading for the T8 Chrome/Tint watch) and T4's slowest real generation case (≈ 2.9 s single-thread). The coast segment uses it. — The plan's three regions never bench a shoreline up close. — Cost if wrong: about 60 s more bench time.
- Ruling R23: Chrome numbers measured in the Claude desktop app's browser pane are indicative only — the pane can pause rAF for tens of seconds with visibilityState "visible"; the gated web measurement is the owner's real-Chrome run. — Cost if wrong: one more manual Chrome run.
- Ruling R24: Bench region 1 is the Himalayas (28.0, 86.9 at 0 ka), not the Alps (owner decision). It keeps the R11 first-region and memory-cycle roles; it adds the highest relief, f16's 8 m height band, and the sky and fog about 13–17 km up. The close present-day coast at sea level 0 is no longer benched (the Aegean benches close coasts at 1 Ma). Earlier Alps numbers stay in performance.md as history. — Cost if wrong: one region swap back. (Measured: the bake's Everest node is 7,640 m, a 60″ cell mean, so the 8 m f16 band is not reached; the generated inner field tops out at 7,576 m.)

**Other deviations from the plan text**

- Task 5: `GlobeData.timeline` and `.crust` became `Arc`s; the CPU frame loader is chosen by asset
  type rather than a `#cpu` label; `RegionLeft` and `RegionSnapshotChanged` got their own
  semantics (a request back at the shown snapshot ends in `RegionEntered`).
- Task 9: `carry_crust_position` takes a `surface_for_candidate` closure (R19), so a carry follows
  the plate the globe draws.
- Task 10: `RegionGenerated` gained `bytes_fetched` (the tiles' file bytes) for the bench's
  report. `--check-data` also loads the tiles beside the globe folder: the manifest, the overview
  and the three largest tiles, size- and SHA-256-checked. `--region-bench` takes precedence over
  `--scrub-bench` and `--region`. The bench's enter/leave memory cycles run inside the bench
  (after the regions) rather than as a separate tool, so the web run measures them too. The
  bench has a fourth region, the Aegean at 1 Ma (R22).
