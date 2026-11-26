# Rivers reality check (Task 11)

The hidden `--hydro-check[=OUT_DIR]` runs the continental pass at 0 ka on the real bake and
compares it with a 1′ reference pass over two boxes, five lakes and four river outlets
(`crates/timefly/src/hydro_check.rs`, bars in `CheckSpec::standard`).

## How to run it

```sh
# The references (each well under 10 minutes; both files 0.6 MB):
cd tools/bake
uv run timefly-hydro-ref --box indus --etopo …/ETOPO_2022_v1_60s_N90W180_surface.nc \
  --climate …/crates/timefly/assets/data/climate \
  --out ../../crates/timefly/tests/fixtures/hydro_reference/indus.json
uv run timefly-hydro-ref --box amazon …   # same, amazon.json

# The check (headless, no window):
TIMEFLY_LOG_DIR="$(mktemp -d)" BEVY_ASSET_ROOT="$ROOT" target/debug/timefly --hydro-check=OUT
```

It exits 0 on PASS and 3 (`FAILED_RESULT_EXIT_CODE`) on FAIL. `$ROOT/assets/data` is the main
checkout's real data.

Reference timings (M4, Python): Indus 6.7 s (870 × 960 nodes), Amazon 25 s (1620 × 1920 nodes).
`indus.json` is 95,668 bytes and `amazon.json` 503,031 bytes.

## Result, 2026-11-26 (Task 11b: today's river courses burnt in)

Task 11b burns Natural Earth's river courses into the pass's routing wherever the terrain is
today's (`data/hydrology/rivers_today.json`, `timefly_core::hydrology::today_rivers`; see "Choices
made in Task 11b" below). The Python reference does not burn: the check now compares two
independent sources of today's rivers, a 1′ DEM and Natural Earth.

The figures below are from Task 11b's fix round 2:
- The river graph alone takes the courses' levels.
- The outlet points are cited (see "Outlet points" below), and the Amazon's allowance is 160 km.
- The regions relax a burnt course's nodes to the course itself (its anchors).
- The graph's beds slope across held stretches.

The binary was the `dev` profile (opt-level 1 for the workspace, 3 for dependencies), not
`target/release`: release was not built, to save disk. Pass time at 0 ka: 1.30 s (0.20 s
sampling), against 1.22 s before. The lattice is n = 900, with 118,792 river nodes (121,263
before) and 861 lakes (861).

**hydro check: FAIL.** The Indus box's share now passes, but 11 links are still missed:
- The Shyok and the Arghandab are not in Natural Earth 1:10m.
- The rest are mostly rivers outside its rank ≤ 7 courses, or short reaches.

Every other bar passes.

| Bar | Needs (brief, or † set in Task 11) | Before (Task 11) | After (Task 11b) | Verdict |
|---|---|---|---|---|
| Indus box, relaxed (judged) | ≥ 80% within 10 km, every link | 76.4%, 70/84 links, worst 134.6 km | 81.6%, 73/84 links, worst 77.5 km | **FAIL** (links only) |
| Indus box, coarse graph | (informative) | 75.0%, 69/84, worst 137.5 km | 80.5%, 72/84, worst 77.7 km | |
| Amazon flats (< 300 m), relaxed | ≥ 80% within 25 km | 91.7%, 456/482 links, worst 144.3 km | 91.6%, 458/482, worst 145.9 km | PASS |
| Amazon flats, coarse graph | (informative) | 91.1%, 454/482, worst 142.6 km | 91.3%, 455/482, worst 142.6 km | |
| Caspian Sea (42.0, 50.5) | within 5 m of the table (−28.0) | −28.0 m | −28.0 m | PASS |
| Dead Sea (31.5, 35.5) | within 5 m of the map's fine surface (−427.0) | −427.0 m | −427.0 m | PASS |
| Titicaca (−15.8, −69.4) | the same (3,808.5) | 3,808.5 m | 3,808.5 m | PASS |
| Great Salt Lake (41.1, −112.5) | the same (1,278.0) | 1,278.0 m | 1,278.0 m | PASS |
| Victoria (−1.0, 33.0) | the same (1,133.0) | 1,133.0 m | 1,133.0 m | PASS |
| Danube from Budapest | through the Iron Gate to the Black Sea; † within 15 km of the gate, ocean within 150 km of the delta | 0.5 km from the gate; ends at (45.16, 29.23), 37 km from the delta | 0.3 km from the cited gate; ends at (45.16, 29.23), 42 km from the cited mouth | PASS |
| Jhelum from Srinagar | out of the Kashmir valley at Baramulla; † within 15 km of it, then the sea within 150 km of the Indus mouth | 4.0 km from Baramulla; the sea 144 km from the Indus mouth | 1.1 km from the cited Baramulla; through one lake, then the Indus to the sea at (23.96, 67.49), 7 km from the cited mouth | PASS |
| Indus from Skardu | its sea; † ocean within 150 km of its mouth | ends at (24.16, 68.80), 144 km off | ends at (23.96, 67.49), 7 km from the cited mouth | PASS |
| Amazon from Iquitos | its sea; † ocean within 150 km of its mouth, 160 km for the Amazon (owner, 2026-11-26) | through one floodplain lake; ends at (−0.54, −50.76), 140 km from (−0.5, −49.5) | through no lake; ends at (−0.25, −51.07), 152 km from the cited North Channel mouth (177 km from the old point) | PASS |
| Arid basins | no lake ≥ 1,000 km² where the basin averages < 250 mm/yr, in either network († basin = lake plus upstream river nodes) | game 0 of 3 (750, 1,942 and 386 mm/yr); reference 0 of 2 | game 0 of 3 (750, 1,942 and 378 mm/yr); reference 0 of 2 | PASS |

`TODAY_RIVER_BURN_M` = 10 m was enough: the lower Indus now follows the reference by Sehwan, so
20 m was not tried.

SVGs (reference blue, game red, equirectangular, 100 km scale bar), from this run:

- [hydro-check-indus.svg](hydro-check-indus.svg)
- [hydro-check-amazon.svg](hydro-check-amazon.svg)

### Outlet points (owner, 2026-11-26: "relocate all mouth points based on cited sources")

Every start, via and mouth point of the outlet bars is now an English Wikipedia article's
coordinates, read 2026-11-26 (the article's own coordinates, or its infobox's mouth). The
tolerances are unchanged: start 30 km, via 15 km, mouth 150 km.

| Point | Old | New | Source |
|---|---|---|---|
| Danube start, Budapest | (47.50, 19.05) | (47.49250, 19.05139) | "Budapest" |
| Danube via, the Iron Gate | (44.67, 22.5) | (44.67111, 22.52972) | "Iron Gates" |
| Danube mouth | (45.2, 29.7) | (45.21750, 29.76139) | "Danube", infobox mouth (Danube Delta) |
| Jhelum start, Srinagar | (34.08, 74.80) | (34.09000, 74.79000) | "Srinagar" |
| Jhelum via, Baramulla | (34.2, 74.35) | (34.20869, 74.34350) | "Baramulla" |
| Jhelum and Indus mouth | (24.0, 67.4) | (23.99500, 67.43500) | "Indus River", infobox mouth (Indus River Delta) |
| Indus start, Skardu | (35.3, 75.6) | (35.29028, 75.64444) | "Skardu" |
| Amazon start, Iquitos | (−3.75, −73.25) | (−3.750, −73.250) | "Iquitos" (unchanged) |
| Amazon mouth | (−0.5, −49.5) | (0.70778, −50.08944) | "Amazon River", infobox mouth: the North Channel, north of Marajó past Macapá |

Margins on this run: the Danube passes 0.3 km from the gate (14.7 km margin) and ends 42 km
from its mouth (108 km); the Jhelum passes 1.1 km from Baramulla (13.9 km) and ends 7 km from
the Indus mouth (143 km), as does the Indus; the Amazon ends 152 km from its mouth (8 km under
its own 160 km allowance).

### The Amazon's outlet (owner, 2026-11-26: "Raise the Amazon's allowance to 160 km")

Natural Earth's Amazonas course ends at Gurupá (−1.58, −52.71), above the estuary; the delta's
channels are not among the rank ≤ 7 rivers. The burnt course carries the Amazon to Gurupá, and the
lattice routes the last 200 km or so: east, then north to the coast by Macapá at (−0.25, −51.07).
That is 152 km from the cited North Channel mouth (0.71, −50.09), 2 km over the 150 km allowed,
and 177 km from the old point (−0.5, −49.5); before the burn the lattice reached the coast at
(−0.54, −50.76), 140 km from the old point. Nothing was tuned for it. The owner raised the
Amazon's own allowance to 160 km; every other outlet keeps 150 km.

### What the Indus box misses

The check prints each unmatched reference link. After fix round 2 (11 links: the 10 of fix round 1,
all missed before the burn too, and the 2 km reach at Muzaffarabad):

| Reference link | Length | Within 10 km | Why |
|---|---|---|---|
| Shyok, (33.94, 78.44) to Skardu (35.23, 75.91) | 332 km | 20% | Not in Natural Earth 1:10m; the lattice takes another valley (unchanged). |
| Arghandab, (33.06, 68.16) to (31.46, 66.01) | 317 km | 0% | Not in Natural Earth 1:10m; a parallel valley up to 56 km off (unchanged). |
| Beas, Pong (31.96, 75.94) to Harike (31.18, 75.09) | 162 km | 18% | Not among the rank ≤ 7 courses. |
| Sindh (Madhya Pradesh), (25.77, 78.29) to (26.44, 79.22) | 151 km | 13% | Not among the rank ≤ 7 courses. |
| Thar's edge to the Rann, (23.59, 71.09) to (24.56, 71.04) | 140 km | 22% | Not among the rank ≤ 7 courses. |
| 5 more | 5–87 km | 0–40% | Ladakh's valleys, the Swat, two short reaches. |
| Jhelum at Muzaffarabad, (34.34, 73.46) to (34.33, 73.47) | 2 km | 33% | Matched before fix round 2 (and before the burn): anchoring the hairpin to Natural Earth's line moved the game's river up to 10.9 km from the reference's confluence reach. |

Now matched: the lower Indus (939 km, 18% before), the lower Jhelum from Mangla to the Chenab
(329 km, 34%), a 178 km link at (36.99, 72.38) to (37.49, 71.51) (7%) and a 2 km reach at
Muzaffarabad (that one missed again after fix round 2's anchors). The first run of the burn
missed the Jhelum from Muzaffarabad to Mangla and a reach of the Chambal, until course nodes kept
their hairpins (see "Choices made in Task 11b").

### Before Task 11b

Task 11's longest misses were the lower Indus, (29.11, 70.69) to the sea (939 km, 18%: about 90 km
east of Sehwan, straight down the Sindh plain), the Shyok (332 km, 20%), the lower Jhelum from
Mangla to the Chenab (329 km, 34%: another line across the Punjab doab) and the Arghandab (317 km,
0%), plus 10 more of 2–178 km.

Before Task 11's Upper Indus gorge, the box read 74.3% with 69/84 links. Then the Indus's
Tibetan headwaters (32.5, 80.8) flowed east through three Tibetan lakes to the Bay of Bengal
(15.98, 95.65).

## Choices made in Task 11

The brief gives the box, lake and arid bars' numbers. The choices below were made while building
the check, and the controller has ratified them (2026-11-26).

- **Outlet tolerances** (`hydro_check.rs`): the river starts at the largest within
  `OUTLET_START_KM` = 30 km of its point. It must pass within `OUTLET_VIA_KM` = 15 km of its
  gorge, about one and a half lattice spacings. It must end in the ocean within `OUTLET_SEA_KM` =
  150 km of its mouth. The brief only says "reach their real outlets".
  - Why 150 km: the misroutes this bar exists for end in the wrong sea, 1,000 km or more off. The
    Danube reached the Oder mouth, 1,469 km off; the Indus headwaters reached the Bay of Bengal,
    over 3,000 km off. Deltas and estuaries are wide: the Amazon's is about 300 km across.
  - Margins on Task 11's run, against the old points (Task 11b's, against the cited points, are
    under "Outlet points"):
    - Danube: 0.5 km from the gate (14.5 km margin), 37 km from the delta (113 km).
    - Jhelum: 4.0 km from Baramulla (11 km margin), 144 km from the Indus mouth (6 km).
    - Indus: 144 km from the mouth (6 km margin).
    - Amazon: 140 km from the mouth (10 km margin).
  - In Task 11's run the Indus (and the Jhelum, which follows it) ended 144 km east of (24.0,
    67.4), at (24.16, 68.80) by Kori Creek: the same Sindh-plain route error the Indus box
    failed on. Task 11b's burn fixed both.
- **The Upper Indus gorge agrees with the reference by construction.** Its course and levels are
  the 1′ reference's own Indus link from (32.49, 80.81) to Nimo, so the game's river there follows
  the reference's line. That link is 459 km of the box's 16,570 km of reference links, about 2.8%.
  The Indus box reads 74.3% without the gorge and 76.4% with it (69 → 70 of 84 links). The other
  two gorges lie outside both boxes.
- **Game rivers near the box, not clipped to it.** The controller's ruling said to compare the
  game's rivers clipped to the box. The check instead keeps every game segment with an end within
  the box plus 0.5° (44–56 km across these boxes' latitudes). Distances are measured only from reference points, which all lie
  inside the box, and the widest tolerance is 25 km. So a game segment outside the margin can never
  count. A segment inside the margin counts only where it truly lies within the tolerance of a
  reference point. Strict clipping would cut a segment that crosses the box edge, and would score a
  reference river along the edge as missed when the game's river runs a few km outside. The SVG
  shows only the box itself.
- **The arid bar's "basin mean precipitation" is a proxy.** It is the area-weighted mean over the
  lake's own nodes and the river nodes (discharge ≥ 20 m³/s) upstream of it. The bar uses the same
  definition in both networks. The game's result keeps no per-node receivers, so its full basin
  cannot be listed; the Python side uses the same set to match. Smaller streams' land is left out
  on both sides.
- **The reference fixture matches the committed table.** `indus.json` was re-baked from the
  committed `straits.json` on 2026-11-26 and came out byte-identical (95,668 bytes).

## Choices made in Task 11b

The owner chose to burn Natural Earth's courses (2026-11-26); the brief fixed the source, the rank
cut, the 1 km simplification, the 2′ orientation window and `TODAY_RIVER_BURN_M` = 10 m. The
choices below were made while building it.

- **The burn.** Wherever the era's terrain is today's (data lakes, and the node's present-day
  shift known: the gorges' gate), after the straits, each course's nodes (upstream first) take a
  level, the running minimum of the era's node elevations along it (after the straits' burn). The
  node routes at `level − 10 m`. The routing elevation of a node at or above sea level is never
  burnt below 0 m, so the burn never adds a node to the ocean; the river graph's bed there is the
  level, itself never below 0 m on such a node. Ice nodes are not burnt. A node on several courses
  takes the lowest level. Reference (PaleoDEM) eras burn nothing.
- **Routing only.** Only the river graph's beds take the courses' levels, so the regions carve no
  10 m trench. Routing, the flats, the climate and the lakes all read the burnt surface. (Fix
  round 1: the first version raised the burnt nodes back to their levels after routing, before
  the lakes read them. A basin could then hold nodes above its own spill, and a course ending
  inland on a 3 m ridge in a semi-arid past era made a lake at 50.4 m over a 45.9 m spill.
  `a_course_ending_inland_keeps_its_lakes_within_their_basins` pins it.)
- **Course nodes are taut.** As the straits, a course's nodes are the nearest to points a quarter
  spacing apart. A return to an earlier node cuts the loop, and an L-shaped corner is cut: when the
  new node is a diagonal neighbour of the node before the last, the last is dropped. Without that,
  steepest descent took the diagonal past a corner node and left the course (a test shows it). A
  V-shaped turn (two diagonal steps whose ends are side by side) keeps its apex: the first run cut
  every corner, which dropped the Jhelum's hairpin at Muzaffarabad. The valley there is lower than
  a 10 m burn of the course's other nodes, so the river left the course westward and the box
  missed the 136 km link to Mangla (and an 82 km reach of the Chambal). Keeping V apexes gave the
  table above (80.7% and 72/84 links before, 81.4% and 74/84 after).
- **Every era with today's terrain**, not 0 ka only (the brief's ruling): an ice-age river follows
  today's course where it may then have run elsewhere.
- **Time-boxed.** The table is read a course at a time, each course's nodes found in blocks, and
  the burnt nodes lowered in blocks (stage `today_rivers`; fix round 1 removed the `beds` stage:
  the graph build reads the beds). Nothing
  is cached between passes, so every pass does the same units. On the synthetic recent lattice
  (`time_the_today_rivers_stage`, dev build) the stage takes 45 steps of 8,192 units, the longest
  0.53 ms. On the real bake (`real_continental_pass_timing`, dev build; release was not built)
  the time-boxed runs give the same result as `run_parallel`, bit for bit, at 0 and 21 ka. Their
  longest steps are 1.39 ms (walk→rivers) and 1.64 ms (runoff→walk) at 0 ka, at 6 and 12 ms
  budgets, and 1.52 ms (walk) and 2.03 ms (sample) at 21 ka: none in `today_rivers`.
  The dev build runs the workspace at opt-level 1, so release steps are shorter (Task 10: 1.1–1.3
  ms).
- **Where the courses end.** A course that stops short of the sea or its trunk leaves a pit at its
  end: the water fills it and spills where the lattice says. The Amazon's course ends at Gurupá,
  above the estuary (see the Result).
- **How far the beds lie below the terrain** (fix round 1, measured, not changed). The check
  prints it (informative): over 30,799 burnt nodes at 0 ka the era's node elevation stands above
  the bed by at most 1,304 m, 373 m at the 99th percentile, and over 50 m at 3,693 nodes. The
  worst five are mountain canyons:

  | Course | Node at | Node terrain | Bed | ETOPO 60″ lowest within about 6′ |
  |---|---|---|---|---|
  | Columbia (Hells Canyon) | (45.28, −116.59) | 1,979 m | 676 m | 610 m |
  | Indus | (35.75, 74.89) | 3,718 m | 2,484 m | 1,967 m |
  | Indus | (35.55, 75.25) | 3,647 m | 2,484 m | 2,056 m |
  | Jinsha | (27.66, 103.44) | 2,357 m | 1,328 m | 601 m |
  | Indus | (32.58, 79.49) | 5,231 m | 4,206 m | 4,231 m |

  In four of the five the bed stands above the lowest 60″ cell near the node, and in the fifth
  it is 25 m below it. The depth is mostly the 10 km node's mean over canyon walls, not a trench
  below the real river. 22 nodes on 9 courses are pinned at 0 m after their course passed a node
  below sea level (the Danube's Sulina arm, the Lek and the Waal, the Lena, the Sacramento, the
  Suez Canal, the Sénégal, the Volga and one unnamed course).

  After fix round 2's sloped beds: at most 1,362 m, 439 m at the 99th percentile, and over 50 m
  at 4,446 nodes. The beds now fall across held stretches, below the held level.

  A `region_tour` render of the worst node (Hells Canyon, 45.28° N, 116.59° W, 0 ka, 6,000 m
  above the ground, looking north and 45° down;
  [hells-canyon-burn-down.jpg](hells-canyon-burn-down.jpg)) shows the following:
  - South of the camera the river runs as a narrow, straight, uniformly dark-brown channel along
    the canyon floor, with no water drawn in it.
  - North of the camera the river is blue and meanders.
  - To the north-east a river crosses a ridge in a narrow cut whose floor reads black: a pit or a
    very steep notch.

  No render without the burn was made to compare, and the rule was not changed: the owner
  decides.
- **Anchors and sloped beds** (fix round 2, after the Hells Canyon investigation).
  - Each burnt node records its anchor: the nearest point to its centre on the course's polyline
    segments that sampled it. It is held within `TODAY_RIVER_ANCHOR_MAX_SPACINGS` = 0.75 mean
    spacings, and the regions relax the node there instead of searching the coarse floor.
  - The anchors are pass data, stored with the river nodes (`EraHydrology::anchor`), so regions
    still agree. The gather's relaxation reach grows from C(1 + √2) to max(C√2, 0.75 spacings) +
    C.
  - Across a held stretch (a run of nodes above the running minimum), the river graph's bed now
    falls linearly by path length to the next lower level. Routing keeps the plain running
    minimum.
  - On the real bake at Hells Canyon:
    - The refined channel now follows the canyon instead of the chord over the plateau.
    - The ground stands 70–324 m above the water along the stretch, not 1,100–1,500 m.
    - The water runs at 1.09 m/s, not 0.30.
    - The ridge notch to the north-east remains: a capture of the eastern river into the course.
      It is cut 826–1,146 m deep and 375–875 m wide. It goes to the owner.
- **The reference does not burn** (the brief's ruling), so the check compares two independent
  sources. The fixtures under `crates/timefly/tests/fixtures/hydro_reference` are unchanged.

## Gorges added to `data/hydrology/straits.json`

The table now holds straits (a sill) and gorges (a river surface per point, interpolated between
points). Both the pass and the Python reference read it.

| Entry | Why | Source |
|---|---|---|
| Iron Gate (Golubac to below the Iron Gate I dam, 66 m to 41 m) | Without it, the Pannonian basin spilled through the Moravian Gate. The Danube reached the Baltic by the Oder at (53.79, 14.14), 1,469 km from its delta. | Wikipedia, "Iron Gates" (read 2026-11-26), for the gorges and the dam. Course and levels from ETOPO 2022 60″ (doi:10.25921/fd45-gt74). |
| Danube Bend (Esztergom to Budapest, 105 m to 101.5 m) | Found by this check: without it, the Danube left the Little Hungarian Plain past Lake Balaton and missed Budapest. | Karátson (2015), *Danube Bend: Miocene Half-Caldera and Pleistocene Gorge*, in Lóczy (ed.), *Landscapes and Landforms of Hungary*, Springer, doi:10.1007/978-3-319-08997-3_16. Levels from ETOPO 2022. |
| Upper Indus (32.49, 80.81) to Nimo (34.18, 77.33), 4,495 m to 3,210 m | Without it, the Indus's headwaters went to the Bay of Bengal. | Kumar & Srivastava (2017), *Quaternary Research* 87(3), 363–385, doi:10.1017/qua.2017.19. Course and levels from the 1′ reference's own Indus link. |
| Bosporus (−35 m), now with its sill cited | — | Gregg & Özsoy (2002), *J. Geophys. Res.* 107(C3), 3016, doi:10.1029/2000JC000485, ¶52: South Sill is mostly 30–40 m deep. |

The Danube now passes Budapest and the Iron Gate and reaches the Black Sea. Between them, it
still crosses the Great Hungarian Plain by Szeged rather than by Mohács and Novi Sad. That is the
lattice's routing on a flat plain, not a gorge, so it is not fixed here.

## Constants

No constants were tuned. The misses come from route choice, so the corridor, discharge threshold
and runoff table cannot change them. Task 11b adds `TODAY_RIVER_BURN_M` = 10 m (the brief's value; 20 m
was not needed) and `TODAY_RIVER_MAX_SCALERANK` = 7 in the bake. The hydro golden
fixture, the reference fixtures and the region golden hashes are unchanged: the full workspace
tests pass.
