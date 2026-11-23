# Time Fly Milestone 2: Region Worlds — Design

Date: 2026-11-23. Parent design: [`2026-11-21-time-fly-design.md`](2026-11-21-time-fly-design.md)
(the "spec" below). This document narrows the spec's build-order step 2 ("Region worlds: world
generator, CDLOD fixes, painterly terrain, water, era vegetation palette") into one milestone.
Where it is silent, the spec governs.

**Decided by the owner:** present-day elevation at about 2 km (ETOPO 2022, 60 arc-seconds) for
the recent eras (option B). The owner handed every other choice to the builder ("go with your
picks"). Those choices are marked *(call)*, and the builder may revise them with a reason
recorded in the plan.

## 1. Intent

Let the player leave the globe and fly over a real ~300 km piece of Earth at any snapshot. It
should look painterly and plausible, and agree with the globe. Recent eras show real present-day
landforms with the ice and sea level of their time. Deep eras show the coarse
paleo-geography, amplified by generated relief. This is the first ground-level world, and it
proves the world generator, the CDLOD terrain and the painterly material that every later level
reuses.

**Why now:** region worlds are the first step below the globe in the world tree (spec §5.1).
Zoom-and-swap (M3) and local worlds (M4) build on the generator and terrain proved here.

**Success looks like:**
- Pick a spot on the globe at any era and arrive in a region within a few seconds.
- The region matches the globe's coastline, ice and sea level for that era.
- Flying is smooth (60 fps native).
- The terrain reads as painted, not as a heightmap.
- The vegetation colours change across eras: no green land before about 470 Ma, no grass
  before 20 Ma.

## 2. Goals, non-goals, anti-goals

**Goals**
1. **A world generator** in the functional core. It is deterministic in (region centre, era,
   seed), uses real data for the large scale, adds noise for detail, and preserves the
   coastline.
2. **Present-day elevation (ETOPO 2022 60″) and climate classes (Köppen-Geiger 1991–2020)**,
   baked to tiles. They are bundled on desktop and streamed from the GitHub Pages site on the web.
3. **CDLOD terrain** from the owner's draft `reference/draft/terrain_cdlod.wesl`, with the
   spec's three fixes (§7.2). It is crack-free and lit by Bevy's lights, shadows and fog.
4. **Painterly terrain shading**:
   - biome and era palette
   - rock on slopes, snow and ice
   - brush-noise breakup
   - ramped light with hue-shifted shadows
   - atmospheric perspective
5. **A sea-level water plane**: Fresnel, depth tint and shoreline foam.
6. **An era vegetation palette** (spec §6.3) as terrain colour. There are no plant meshes yet.
7. **A stand-in for zoom-and-swap** *(call)*:
   - double-click the globe to enter that spot's region
   - fly it with a free camera
   - press `Backspace` to return to the globe
   - in a region, the slider snaps to snapshots and changing snapshot regenerates with a
     crossfade (spec §6.2)
8. **Shared noise** in Rust and WESL, with golden-value tests (spec §8.3).
9. **A region benchmark** (`--region-bench` / `?region-bench`), plus the spec §15 verification
   that applies.

**Non-goals (this milestone)**
- The real zoom-and-swap transition (M3), local worlds, voxels, the creature and avian3d.
- Lakes and rivers (sea level only, as spec §7.5 allows to start), plant meshes and cards,
  human land use (HYDE), settlements, ships, clouds over regions, underwater views, and sound.
- PALEOMAP deep-time climate belts. Deep eras use the zonal rule in §5.4 for now, with a data
  note on the credits screen.

**Anti-goals**
- A region that disagrees with the globe it came from: a different coastline, land where the
  globe shows sea, or missing ice sheets.
- Era-wrong vegetation (spec §2): green land before about 470 Ma, grassland before 20 Ma, or a
  green Sahara today.
- Cracks, popping or swimming terrain while flying.
- A blocking loading screen. Entering shows the globe zooming in and fading over (§6.1).

## 3. Constraints

All of the spec's constraints (§3) apply: Bevy 0.20.1 and WESL, the WebGPU ceiling, f32 inside
worlds, f64 on the CPU for geography, functional core and imperative shell, AOP-style tracing,
and the crash flow (§19, §20). In addition:

- **Region size:** 300 km square *(call)*, laid out in the era's frame, north up, 1 world unit =
  1 m. Coordinates stay within ±150 km, so f32 precision stays near 1 cm.
- **Data:**
  - ETOPO 2022 v1 60″ surface elevation (NOAA NCEI, public domain; credit and citation given).
    One netCDF, 478 MB.
  - Köppen-Geiger maps (Beck et al. 2023, CC BY 4.0, Figshare `koppen_geiger_tif.zip`,
    131 MB), the 1991–2020 map.
  - Both are pinned by SHA-256 in `data/sources.json`. Downloads need the owner's go-ahead, as
    in M1.
- **Web budget:** the tile set must fit the Pages site alongside the globe (1 GB site limit,
  100 MB per file).

## 4. Approaches

### 4.1 Where terrain heights are computed

| | Approach | Trade-off |
|---|---|---|
| **A** | **CPU heightmap per region, in the functional core** (task pool on native, chunked per frame on the web), uploaded as a texture that CDLOD samples. | Deterministic and unit-testable. CPU height queries are exact, because they read the same map. It reuses the globe's CPU reference sampling for deep eras. Costs some seconds of CPU per region, and the web needs chunking. **Chosen.** |
| B | GPU compute generation. | Fast, but deep-era lookups (plate resolve, footprints, rotations) would have to be ported to WESL. It's harder to test, and CPU queries would need a full twin anyway. |
| C | No heightmap: evaluate noise per vertex in the vertex shader. | No load step, but the CPU twin is expensive, the data lookups still need a texture, and LOD consistency is harder. |

With A, the WESL noise copy is used for material breakup (brush noise and colour variation).
This keeps the spec's twin-noise rule meaningful without making GPU heights authoritative.

### 4.2 How CDLOD reaches Bevy's lighting

| | Approach | Trade-off |
|---|---|---|
| **A** | **One entity per selected patch**, sharing one grid mesh and one `ExtendedMaterial<StandardMaterial, TerrainExtension>`. Each patch's `MeshTag` indexes a read-only storage buffer of patch parameters (the route in Bevy 0.20's `automatic_instancing` and `storage_buffer` examples). | Full PBR lighting, shadows and fog with automatic instancing. Needs matching prepass and shadow vertex code. Patch entities are recycled each frame. **Chosen.** |
| B | A custom render pipeline, as the draft assumed. | Full control, but lighting, shadow and fog integration has to be re-implemented and would drift from the globe and later levels. |
| C | Rebuild one CPU mesh per frame. | Simple, but CPU-heavy, and morphing on the CPU pops. |

### 4.3 How the player gets in, before zoom-and-swap

| | Approach | Trade-off |
|---|---|---|
| **A** | **Double-click the globe to enter, `Backspace` to leave, with a free-fly camera.** | Cheap, and exercises everything M3 needs (entry location, snapshot snap, generation while transitioning). Throwaway input only. **Chosen.** |
| B | Build zoom-and-swap now. | Pulls M3 forward into an unproven terrain stack. |
| C | A debug menu of preset regions. | Hides location bugs, since only the presets ever get tested. |

## 5. World generator (functional core)

New bounded context **`worldgen`** in `timefly_core` (spec §19), with ubiquitous-language types
`RegionSpec`, `RegionGrid`, `TerrainSample`, `BiomeClass`, `VegetationEra`.

### 5.1 Region frame

A region is a `RegionSpec`:
- the centre's era-frame direction
- the snapshot age
- the seed
- the size
- the grid resolutions

Local `(x, z)` metres map to an era-frame direction through an **azimuthal equidistant
projection** around the centre, north up *(call)*. Distances from the centre are exact, and the
distortion at the region's edge is well under 1%. The inverse is exact too, so world positions
convert to geography and back.

### 5.2 Coarse elevation, by era class

Each sample's era-frame direction is classified by the snapshot. "Elevation" always means
**effective elevation relative to that era's sea level** (the M1 rule: `elev − anomaly`).

- **Recent (ICE-6G frames, ≤ 26 ka):** ETOPO present-day elevation, plus the ICE-6G change
  `Topo(era) − Topo(0)`, interpolated bilinearly from the 1° frames. The change carries the
  ice-sheet surface, isostasy and the sea-level shift. The ICE-6G ice fraction (`sftgif`) gives
  the ice mask. Lookup is geographic, as for the recent globe (spec §5.2).
- **Bridge (after the last ICE-6G frame, before the first deep PaleoDEM):** ETOPO minus
  `Timeline::sea_level_anomaly_m(age)`. This is the same curve the globe's bridge uses.
- **Deep (PaleoDEM frames):** the globe's CPU reference, `rendered_surface` (land, gap land or
  ocean, with elevation), sampled at the era direction with the snapshot's frame selection. A
  region therefore agrees with the globe by construction. Ice: none from data. A zonal snow
  rule applies (§5.4), with a data note that deep glaciations are not shown.

**Agreeing with the globe (test):** at the region's grid points, the coarse field's land/sea
sign must match `rendered_surface` (deep) or the globe's recent sampling (recent) wherever the
globe is unambiguous. "Unambiguous" means all four bracketing source samples agree.

### 5.3 Detail and the coastline-preserving blend

```
h = coarse + D
D = amplitude · fBm(lattice)
```

- **Noise input:** `lattice` is the canonical crust-space position: the plate-fixed direction
  for tracked crust, the era-fixed key otherwise (spec §5.2). It is split into an integer cell
  index (hashed into a per-cell seed) and a local f64 fraction.
- **Amplitude:** set by the terrain class *(call)*.
  - Relief scales with coarse elevation and the coarse gradient: mountains get ridged octaves,
    plains get low-amplitude octaves.
  - The source resolution decides which octaves are new: below about 2 km in recent eras,
    below about 110 km in deep eras.
- **Coastline rule (test):** `D` may change the sign of `h` only where the source data is
  ambiguous there, that is, where the four bracketing source samples straddle sea level.
  - Where all four agree, the sample clamps to stay on their side: land stays at least +1 m,
    and sea at most −1 m.
  - This gives fractal coastlines inside the data's own uncertainty band, and never land where
    the data says open ocean.
- **Determinism (test):** the same `RegionSpec` gives the same bytes.
- **Overlap (test):** two regions whose windows overlap give the same height at the same
  geographic point, within the bilinear-resampling tolerance.

### 5.4 Biome, ice and vegetation era

- **Recent and bridge eras:** biome is the present-day Köppen-Geiger class, collapsed to about
  12 painterly biome classes *(call)*:
  - tropical rainforest, monsoon/savanna, hot desert, cold desert, steppe
  - mediterranean, temperate broadleaf, temperate rainforest, boreal
  - tundra, ice cap, alpine
  - For the ice-age snapshots this is a known approximation (present-day climate under
    past ice), recorded as a data note.
- **Deep eras:** a zonal rule on **paleolatitude** (the sample's latitude in the era frame),
  elevation and continentality (distance to coast) *(call)*. It gives tropical humid, arid
  subtropical, temperate humid, cool and polar bands. Polar plus high elevation means snow.
- **Vegetation era:** from the snapshot age, using the spec §6.3 table. A palette lookup
  `(VegetationEra, BiomeClass) → colours` maps each pair to a ground colour, a canopy colour
  and a rock tint.
  - Before 470 Ma: bare rock and sediment colours, with a green-grey microbial tint at
    shorelines.
  - Before 20 Ma: no grassland colours. Steppe and savanna fall back to shrubland and open
    woodland.
- **Ice and snow:** the ICE-6G mask (recent), Köppen ice-cap and tundra plus a snowline from
  elevation and latitude (all eras), and slope-limited.

### 5.5 Output

`RegionGrid`:
- **Inner field:** a 1025² grid over 300 km (about 293 m spacing).
- **Outer apron:** a 513² grid over 2,400 km (about 4.7 km spacing, near the globe's own
  resolution). It continues the terrain to the horizon at the parent's resolution, from the
  same generator (spec §7.2).
- **Per sample:** effective elevation (f32), plus an RGBA8 surface code (biome class, vegetation
  era, ice fraction, shore flag).

CPU height queries (camera clearance now, the creature later) bilinearly sample the same grids.

## 6. Shell: regions, terrain, water, camera

### 6.1 Entering and leaving (the stand-in)

1. A double-click on the globe ray-casts to the era direction under the cursor.
2. The slider snaps to the nearest snapshot (spec §11.2), then the region's location resolves
   at that snapshot.
3. Generation starts. Meanwhile, the globe camera eases toward the spot.
4. When the grid is ready, the view crossfades (under a second) to the region. The camera
   starts about 8 km above the centre, looking north and slightly down.
5. On the web, generation runs in row chunks within a per-frame time budget *(call: about
   6 ms)*. A soft "descending…" caption shows only if it takes more than a second.
6. `Backspace` crossfades back to the globe, centred on the region.

### 6.2 Snapshot changes inside a region

- The slider snaps to snapshots (spec §6.2). A change regenerates around the camera's **crust
  position**, keeping the camera's height above ground, then crossfades over about 1 s.
- If that crust position isn't valid in the new era (era-fixed terrain or newly invalid crust),
  return to the globe (spec §5.2).

### 6.3 CDLOD terrain

**Patch selection** is a pure core function. Given the camera position, the LOD ranges and the
grid extents, it returns the selected patches (origin, size, LOD, morph range). It covers the
inner field, then apron rings to the horizon.

**Rust reference and tests.** A Rust copy of the vertex morph has two property tests:
- vertices on edges shared by neighbouring patches coincide at every camera position
  (no cracks or T-junctions)
- morph factors are continuous across ring boundaries

**Shader.** The shader is ported from the draft with the spec's fixes:
1. **Snapping:** morphing vertices snap by whole-number grid index (`grid_uv * N`).
2. **Lighting:** lit through Bevy's PBR path (the §4.2 route). The prepass and shadow passes use
   the same displacement.
3. **Clip radius:** the voxel-window clipping is removed for now. The uniform keeps its slot,
   for M4.

**Rendering details**
- **Curvature:** a radial drop of `d² / 2R` is applied in the vertex shader, so the apron meets
  a true horizon. The region itself is flat.
- **Normals:** finite differences of the height textures.
- **Patch grid:** 33×33 vertices *(call)*. Patch entities are pooled.

### 6.4 Painterly terrain material

`ExtendedMaterial<StandardMaterial, TerrainExtension>`:

**Fragment colour**
- **Base colour:** the palette lookup for the sample's biome and vegetation era. Bilinear in the
  surface code, with class blending done in colour space.
- **Rock:** above a slope threshold.
- **Snow and ice:** from the mask and the snowline.
- **Brush-noise breakup:** WESL noise at two or three world-space scales, plus a few percent of
  per-pixel hue and value jitter.

**Lighting** stays Bevy PBR. The extension then applies a soft diffuse ramp and shifts the hue of
shadowed areas toward a cool, era-tinted shade (spec §9).

**Sky:** a gradient sky dome and Bevy `DistanceFog` tinted per era and biome give atmospheric
perspective *(call)*. Bevy's procedural `Atmosphere` may replace them if it fits the web frame
budget.

### 6.5 Water

A sea-level plane at y = 0, curved like the terrain, out to the horizon. Its material:
- **Fresnel:** a painterly blend toward a sky-tinted colour.
- **Depth tint and foam:** from water depth, read by sampling the region height textures at the
  fragment's position (no depth prepass needed). Foam sits in a narrow band where the depth is
  near 0.

The camera never goes below `max(terrain, 0) + clearance`, so underwater stays out of scope.

### 6.6 Free-fly camera

The camera step is a pure function, as in M1's orbit camera.

**Controls**
- `W` `A` `S` `D` move
- drag to look
- `Space` / `E` climb
- `Ctrl` / `Q` descend
- `Shift` boosts

**Movement**
- Speed scales with height above ground *(call)*.
- Clearance: at least 150 m above `max(terrain, 0)`, from the CPU height query.
- A soft ceiling at about 60 km.
- A soft edge: past the inner field the camera is nudged back, a stand-in for the spec's exit
  rule.

## 7. Data pipeline (bake tool)

**New `timefly-bake` steps:**
- `etopo`: ETOPO netCDF → elevation tiles.
- `koppen`: Köppen GeoTIFF → climate-class tiles, mode-resampled onto the ETOPO 60″ grid.

**Tiling** *(call)*:
- 5° × 5° tiles of 300 × 300 samples, plus a one-sample overlap (301²) so bilinear lookups never
  cross a tile.
- Each tile is a 2-channel `.tfgrid` `LatLon` grid: channel 0 is F16 elevation in metres,
  channel 1 is the Köppen class code (exact in F16).
- No format change. All-ocean tiles compress to almost nothing.

**Manifest:** a tile manifest lists every tile with its byte size and SHA-256.

**Size:** the expected total compressed size is roughly 150–250 MB. The builder measures it and
records it in `performance.md`.

**Where tiles live**
- **Desktop and the Linux packages:** bundled under `assets/data/tiles/`.
- **Web:** the Pages workflow publishes the same tiles, and the client fetches only the tiles a
  region needs (at most 4 for the inner field).

**Provenance:** `data/sources.json` pins both downloads (URL, size, SHA-256). The credits screen
gains the NOAA ETOPO 2022 citation and the Beck et al. 2023 CC BY 4.0 attribution, plus two data
notes: deep-era biomes are zonal approximations, and ice-age biomes use the present-day climate.

## 8. Architecture, diagnostics, verification

**Architecture and diagnostics**
- Core contexts are `worldgen` (generator) and `terrain` (CDLOD selection and morph reference);
  the shell plugins are `RegionPlugin`, `TerrainCdlodPlugin`, `WaterPlugin` and `FlyCameraPlugin`
  (spec §16 naming).
- Domain events: `RegionRequested`, `RegionGenerated` (with timing and grid stats),
  `RegionEntered`, `RegionLeft`, `RegionSnapshotChanged`.
- Generation, tile I/O and every failure are traced (spec §20).

**Verification (spec §15, as it applies to M2)**
- The native build and `cargo check --target wasm32-unknown-unknown` both pass. Rendering is
  checked on Metal, in Chrome WebGPU, and on Vulkan via CI packages.
- **Generator tests:** determinism, overlap agreement, the coastline rule (property tests),
  agreement with the globe, and vegetation era by age.
- **Noise tests:**
  - Rust noise matches a committed golden table in CI.
  - WESL noise matches the same table on a GPU. That test is local-only and ignored in CI, like
    M1's GPU test.
- **CDLOD tests:** crack-free and morph-continuity property tests on the Rust reference. A GPU
  golden check of the shader morph runs where a GPU exists.
- **Benchmark:** `--region-bench` flies a scripted path (low pass, climb, coastline, apron) in
  three fixed regions:
  - present-day Himalayas (Everest area)
  - 21 ka Scandinavia (ice)
  - 250 Ma Pangaea interior

  Owner decision 2026-11-23: the Himalayas replace the Alps, to stress relief, f16 height
  precision, and sky and fog at altitude (plan ruling R24).
- **What the benchmark reports:**
  - frame-time stats under the R14 rule
  - region generation time (native and web)
  - tile bytes fetched
  - peak memory
- **Targets** (spec §15): 60 fps at 1440p on the Linux machine; 30+ in Chrome on an M1-class
  laptop (provisional on the M4). Generation should take at most 3 s native and 8 s on the web,
  with a warm tile cache *(call)*.
- Screenshots of the three regions at each vegetation era go in `docs/testing/region-m2/`.

## 9. Left to the builder

- **Noise:** the noise family (gradient or simplex) and the hash.
- **Amplitude:** the exact amplitude and terrain-class formulas.
- **Palettes:**
  - the Köppen-to-biome collapse
  - the palette colours, chosen from reference paintings and photos
- **CDLOD:** LOD range constants.
- **Generation:** chunk sizes for web generation.
- **Look:**
  - whether Bevy `Atmosphere` replaces the gradient sky
  - shadow cascade settings

  Each must hold the 60 fps target.
- **Plan:** task order within the plan. Suggested: data bake, then generator, then CDLOD, then
  material and water, then the stand-in UX, then the benchmark and packaging.
