# Rivers and lakes: design

Status: approved by the owner (2026-11-25). Parent spec: `docs/superpowers/specs/2026-11-21-time-fly-design.md`
§7.5 and §14 (the milestone between M3 and local worlds, owner 2026-11-24). Builds on M3
(`m3-zoom-and-swap`).

## 1. Intent

From the fly, rivers and lakes in region worlds read as **real, moving water**. Where a river runs
steep it becomes **white water sloshing over rocks** (owner, 2026-11-25). The look and the motion
matter more than exact placement. Later milestones build on this: local worlds refine the
rivers, and settlements and ships (M5) need river towns and navigable water.

Success, as the owner described it, means:
- Flying over the Indus tributaries, the fly sees mountain rivers in gorges with rapids,
  joining into big rivers on the Punjab plain.
- Flying over the Amazon basin, it sees a wide, slow, meandering river system.
  *(Floodplain lakes, oxbows and channels that split and rejoin are not required this
  milestone; my call, since the owner asked only for flowing water and rapids.)*

### Goals
- Rivers and lakes in every region, in every era, generated from each era's terrain.
- Water that visibly flows downstream, at a speed that suits the river's slope and size.
- Rapids where the bed is steep: broken white water, spray and rocks in the channel.
- Lakes as flat water above sea level, including inland basins below sea level (the Caspian, the
  Dead Sea), which today's sea plane floods wrongly (§6.4).
- Rivers that agree across region edges, like the terrain does.

### Non-goals
- Real river datasets in the game. HydroSHEDS or similar could be added later if boats need
  exact placement (owner chose generated rivers "for now").
- Rivers on the globe.
- Floods, seasons, frozen rivers, deltas in detail, waterfalls as a separate feature (a very
  steep reach draws as steep white water), sound, swimming.
- Local-world (close-up) river detail. That is M4.

### Anti-goals
- **Rivers running uphill or along ridges.** A river must lie in a valley the terrain explains.
- **Deserts full of rivers, or giant lakes in arid basins.** Rainfall-blind routing would give
  the Sahara and Tibet rivers and seas they don't have, which breaks the parent spec's
  confidently-wrong-facts anti-goal.
- **Visible seams.** A river must not jump or change width at a region edge, or across a swap.
- **Map-like rendering.** Rivers are drawn at true width (owner, 2026-11-25). A river too narrow
  to resolve appears only as the fly gets close. No outlines and no minimum-width lines.

## 2. Constraints

- Bevy 0.20.1, WESL, the WebGPU ceiling. No `std::thread` in core loops; web work is time-sliced
  within the existing budgets: `WEB_FLIGHT_GEN_BUDGET_MS` = 3 ms while a preload runs in flight,
  and `EXPECTED_GEN_S_WEB` = 8 s for a region.
- Functional core, imperative shell. Hydrology is pure functions in `timefly_core`, tested
  without a window.
- Region terrain is a **pure function of direction** (M2's global lattice), so overlapping
  regions agree exactly. Rivers must keep that property (§4.3).
- Region grid: 1025² inner texels over 300 km (≈ 293 m), plus a 513² apron. Present-day source
  data is ETOPO at 1′ (≈ 1.85 km), with a 5′ global overview. Deep time uses PaleoDEMs at about
  1° (≈ 110 km).
- The native hop memory gate is +64 MiB, and the region bench's and journey bench's gates stay.
  Rivers must fit inside them, or the owner moves them.
- No new downloads without the owner's go-ahead. The owner approved one source for past climate
  (§4.1), on two conditions: it fits in a few hundred MB, and nothing large is kept on disk.

## 3. Approaches considered

1. **A continental pass computed in the game** (chosen by the owner, 2026-11-25). When the era
   changes, the game routes water over the whole planet on a coarse global lattice. Each region
   then refines rivers on its own grid from that pass. No new data, and every era runs the same
   code. It costs a one-off at each era change, hidden by the era load.
2. **A continental pass baked ahead of time.** The same, but precomputed per snapshot by the bake
   tool, and possibly at the full 1′ for the present. It means more shipped data and re-bakes.
   **Kept as a reality check only** (owner, 2026-11-25): an offline reference the tests compare
   the game's rivers against (§7). It is never shipped.
3. **Each region on its own over a wider window.** Rejected: big catchments (the Amazon, the
   Indus) are thousands of km across, and neighbouring windows would disagree at shared edges.

## 4. The design

### 4.1 The continental pass (per era)

- **Input.** The era's snapshot field (`snapshot_field`: recent, crossfade or deep), sampled on
  a global cube-map lattice, plus the era's sea level and ice (ICE-6G ice fraction), plus a
  **runoff weight** and an **evaporation rate** per node from the era's own climate. The table
  that turns climate into these is data, so the owner can tune it.
  - **Up to 120 ka: Beyer et al. 2020**, "Late Quaternary climate, bioclimate and vegetation".
    It gives annual precipitation (BIO12) and annual mean temperature (BIO1) at 0.5°, every
    1–2 kyr, from 120 ka to the present. Licence CC BY 4.0 (figshare article 12293345, file
    `LateQuaternary_Environment.nc`, 1,333,236,636 B, md5 `d23abb7ce8671433ea36a2cae814ebab`).
    The bake reads only BIO1 and BIO12 by HTTP range requests: about 40 MB stored for all 72
    ages, tested 2026-11-25, with nothing large written to disk. It bakes one small global
    grid per snapshot age into the shipped data, a few MB in all. It is pinned in
    `data/sources.json` by figshare id, size and md5, plus the SHA-256 of the extracted arrays,
    since the bake never holds the whole file. Credits gain the citation. The present uses
    Beyer too, so the climate doesn't jump between 0 and 1 ka. Köppen still drives the
    region's biome look.
  - **Beyond 120 ka:** the deep biome zonal rule (`worldgen/biome.rs`), as today.
  - **Effect:** the Sahara is green with lakes (Mega-Chad) at the African Humid Period
    snapshots, and dry today.
- **Steps:**
  1. Fill depressions (priority-flood).
  2. Route each node to its lowest neighbour, with a deterministic tie rule on flats so routing is
     reproducible.
  3. Accumulate discharge as upstream area × runoff weight.
  4. Resolve lakes with a simple **water balance**: a basin fills only until evaporation over its
     surface (by climate) matches its inflow. Arid basins hold small lakes or salt flats, or stay
     dry. Humid basins spill to their outlet.
- **Output.** A river graph: segments between lattice nodes, each carrying discharge. A lake list
  with each lake's level, outlet and footprint on the lattice. Plus a per-node "ocean-connected"
  flag for sea water.
- **Rules.** No flow over ice-sheet nodes; rivers start at the ice margin. Below sea level counts
  as ocean only where connected to the ocean.
- **Resolution follows the source** *(my call)*: about 10 km per node where present-day data
  drives the terrain (recent and bridge eras, read from the cheap 5′ overview, with no plate
  lookups), and about 50 km in deep eras, whose source is ~110 km anyway. That keeps the
  plate-resolving `DeepField` samples to a few hundred thousand.
- **When it runs** *(my call, for the owner to correct)*: in the background as soon as an era
  is chosen on the globe, time-sliced on the web, and cached per era. A dive that arrives
  first holds in the cloud until the pass is done. Targets: native ≤ 1 s, web ≤ 5 s per era
  (reported, as the holds are), and a region's own generation stays within
  `EXPECTED_GEN_S_WEB`.

### 4.2 Region refinement

- **The river's path follows the real valleys where the data has them** *(my call)*. A coarse
  segment's line is first relaxed onto the valley floor of the field's own coarse terrain, by a
  bounded local search across the line (a corridor a few km wide). It is then given
  deterministic **meander noise**, keyed by global position (crust space, like the detail noise
  `D`). The meander amplitude and wavelength scale with discharge, and flatter ground meanders
  more. The search is local, so the result stays a function of position and the field's
  source. The inner field (tiles) and the apron (overview) can therefore place a river a few km
  apart, the same difference terrain already accepts under ruling R7, at hazed distance. Then
  the terrain is **carved to fit the river**, not the river to the terrain: a valley
  and channel profile is cut along the centreline, with width and depth from discharge (standard
  hydraulic-geometry power laws; the builder picks the constants).
- **Width.** True width from discharge: tens of metres for mountain tributaries, kilometres for
  the lower Amazon. A river narrower than a texel is stored as a distance-to-centreline field, so
  the shader draws it at its true width below the grid's resolution.
- **The water surface.** A water-level field replaces "sea level everywhere":
  - sea level where the water is ocean-connected;
  - each lake's level inside its footprint;
  - a sloped river surface along each channel.

  Alongside it, a flow field (direction and speed). Speed follows slope and discharge: about
  1–3 m/s in lowland rivers and 3–6 m/s in rapids.
- **Rapids.** Reaches whose bed slope exceeds a threshold (builder's call, tuned on the Indus
  tributaries) are rapids, and the region marks them. Rocks are placed in rapid reaches as
  instanced boulders, deterministic per position and sized to the channel.
- **Lake levels in present-day data** *(my call)*. ETOPO 2022's surface model already holds
  lake surfaces as flats (the Caspian at about −28 m). Where the data has a lake's surface,
  that is its level, and the water balance only decides lakes the data doesn't hold (past and
  deep eras, or new basins). In recent eras, a data lake keeps its present level plus the
  ICE-6G change. This avoids, for example, a Caspian filled to its spill level over the Volga
  lowlands.
- **Lakes.** The surface is flat at the lake's level. The shoreline is where the refined terrain
  meets that level within the lake's footprint, so it is fractal like the coast. Inflows and
  outflows join the lake at the right level.

### 4.3 Agreement across regions

The terrain agrees across regions because each texel is a function of direction. Rivers keep
that by making every refined quantity a function of direction plus the era's continental pass:
- the centreline, its meander, the carve, width, depth, water level, flow and rapid flag;
- rock placement.

None of them depends on which region is computing it. (Inner fields agree with inner fields
exactly; an apron may differ from a neighbour's inner field by the source level, as its terrain
does, §4.2.) So neighbouring regions, the apron and a
hop's swap all show the same river in the same place. (This is the M2 "global lattice makes
overlap exact" lesson applied to water.)

### 4.4 Drawing the water

- **Generalise the existing water.** The region water material reads the water-level field
  instead of a constant sea level, and keeps M2's shoreline rule: pull the water along the view
  ray for depth only, and discard by true height.
- **Flow.** Normals and foam scroll along the flow field, the standard flow-map technique with two
  phases blended, so the water visibly moves downstream and never pulses.
- **White water.** In rapids: broken, bright, foamy water; spray around the rocks; faster scroll;
  rougher normals. Ordinary rivers show gentle ripples and the sky's reflection.
- **Colour and depth tint.** Follow M2's painterly water: silty rivers browner, mountain water
  clearer. The builder tunes from the test flights.
- **Rocks.** Low-poly boulders, instanced, coloured with the terrain's palette, wet-dark near the
  water. They are the first instanced scatter in region worlds. Keep the instancing reusable,
  since vegetation will use it.

### 4.5 Time and transitions

- Each era has its own rivers. The slider's existing behaviour applies: a change during a
  transition waits; in a region, the region regenerates for the new era.
- Inside the cloud nothing new is needed. Rivers are part of the region's terrain and water, so
  they swap with them.

### 4.6 What it costs

- Region generation gains routing, carving and the new fields. The swap uploads two or three more
  field textures (water level, flow, and the river-distance and rapid flags).
- Budget the hop's memory against the +64 MiB native gate, and report the native and web region
  generation times against M3's.
- The continental pass: report its time per era, native and web. Its memory is the lattice plus
  the river graph.

## 5. Decided by the owner

- Generated rivers everywhere for now (approach B / 1): 2026-11-25.
- A reality check of the computed rivers against an offline baked pass (approach 2): 2026-11-25.
- True width from altitude: no minimum width and no map lines (2026-11-25).
- Test flights: the Indus tributaries and the Amazon (2026-11-25).
- Lakes are in this milestone (the owner named it "rivers and lakes", 2026-11-24).
- Past climate from Beyer et al. 2020 for eras up to 120 ka, read in parts so it stays within a
  few hundred MB (2026-11-25).

## 6. Left to the builder

### 6.1 Numbers and constants
- The continental lattice resolution (within §4.1's range).
- The runoff and evaporation tables.
- The hydraulic-geometry constants.
- The rapid threshold.
- The meander constants.
- Flow speeds.

### 6.2 How to compute and draw
- The priority-flood variant and the tie rule on flats.
- How the river graph is stored and indexed for region lookup.
- The water-surface shader's details.
- The boulder mesh and its density.

### 6.3 How to build and test it
- Time-slicing and caching of the continental pass.
- Bench additions: a river region in `--region-bench` (e.g. the Indus tributaries), and whether
  the journey bench needs a river route.

### 6.4 Inland basins below sea level
ETOPO shows the Caspian and Dead Sea surfaces as land below sea level, so today's sea plane
floods them at sea level. With the ocean-connected rule, they become lakes at their own levels.
The builder confirms this on the present-day Caspian and records it.

## 7. Verification

- **Reality check (the owner's approach 2).**
  - **Bake.** A bake tool step routes water over the present-day ETOPO 1′ data offline: the same
    algorithm and runoff weights at full resolution, kept out of the shipped data, as test
    fixtures or a dev-only artefact.
  - **Compare.** Test the game's computed rivers for the present day against that reference over
    the Indus tributaries (Jhelum, Chenab, Ravi, Beas, Sutlej, then the Indus itself) and the
    Amazon basin.
  - **Proposed pass bar (the builder's call, for the owner to correct).** For rivers with upstream
    area over 10,000 km², the game's centreline lies within 10 km of the reference's for at
    least 80% of its length in the Indus test area, and within 25 km in the Amazon's flats.
    Every reference river of that size has a matching river in the game. Report the numbers and
    show both networks over the same map.
- **Past climate, manual check.** At a snapshot between 9 and 6 ka, the Sahara shows lakes and
  rivers (Lake Mega-Chad among them). At 0 ka it is dry.
- **Lakes in the reality check.** Present-day levels of the Caspian, Dead Sea, Titicaca, Great
  Salt Lake and Lake Victoria are within 5 m of the data's surface, and no large lake appears
  in the reference's arid basins.
- **Pure tests.**
  - Water never flows uphill along a refined centreline.
  - Discharge only grows downstream.
  - Lakes conserve the water balance.
  - Rivers end at the sea or at a lake.
  - Two overlapping regions give bit-identical river fields where they overlap.
  - An arid basin gets no large lake.
- **GPU and visual checks.**
  - Renders of a lowland river, a rapid and a lake, natively on Metal and in Chrome WebGPU (the
    owner runs Chrome).
  - The owner's manual flights over the Indus tributaries and the Amazon, with a checklist: water
    moves downstream; rapids churn over rocks; no seams at region edges or across hops; no
    rivers on ridges; the Sahara stays dry.
- **Benches.** The M3 gates still pass, with the new numbers reported.
