# River and globe render defects: the owner's bug shots of 2026-11-27

Branch `river-render-defects`, cut from `rivers-and-lakes` at `009d2d0` (PR #16). This note
diagnoses the three bug shots in `~/Pictures/Time Fly/` and the related issues from Task 12's
renders (`manual-checks.md`), groups them by cause and says which fixes are ready to ship.

**How the evidence was made.** `region_tour --from-shot <sidecar>` replays each shot offscreen at
2560 × 1440; all three replays show the same view and the same defects as the owner's PNGs. For the region
shots a scratch build (not committed) dumped the region's fields (height, water level, the river
texel's ribbon and offset, the surface bytes) at the shot, and the texels were projected through
the shot's camera onto the frame (`defects/phoenix-texels-projected.jpg`), so every feature in a
frame is tied to texel values. Images are in `defects/`.

## Summary

| # | Issue | Cause | Before the rivers milestone? | Status |
|---|-------|-------|------------------------------|--------|
| A | Globe 250 Ma: dashed blue strokes, a line ending in a blob, slivers | Gaps between rigid plates show the ocean shell | Yes (globe code identical to `main`) | Diagnosed; fix proposed (skirts) |
| B | Phoenix: dotted dark lines beside the river; Andes: dark-blue slivers beside the river | Dry `RIVER_NONE` taps leak a +1,000 m ribbon into a perched river's footprint edge | No (rivers milestone) | **Fixed**, tests RED → GREEN, renders before/after |
| C | Phoenix: river breaks at a hook beside a dark pit; Sichuan: near-black pit | The course runs 3–6 km off its valley, past the corridor's reach; the bed forces a 400 m slot through the plateau, which the coarse mesh hides from afar | No (rivers milestone) | **Fixed**: deep-cut segments rerouted by least-cost path; tests, reality check, renders |
| D | Kanto: tan bands along rivers and coast | The beach rule paints all land under ~9 m, not the coast | Yes (M2 rule, unchanged) | Diagnosed; fix proposed |
| E | Kanto: small blocky lakes in the bands | Sea-level water in the Tone lowlands decided per hydro tap / lake-map cell | Mixed (DEM-sign rule) | Diagnosed; known follow-up |
| F | Phoenix: teal blocky patches at the horizon | Mogollon Rim forest canopy colour on 1.85 km Köppen cells | Yes | Confirmed not water |
| G | Slow rivers read maroon from altitude | `RIVER_SILTY` is a fixed brown, sRGB (0.36, 0.30, 0.20) | No (design choice) | Owner's call |
| — | Sri Lanka, Volga delta, Baikal patches, Mega-Chad shore, Andes terraces | Not investigated in this pass | — | Open |

## A. Globe at 250 Ma: the ocean shell through plate gaps

**Shot:** `timefly_20261127-162757_…_250000000y_globe` (2,500 km up, 61° pitch).

**Diagnosis.** The plate pieces are rigid cube-cell quads, one plate per quad, each rotated by its
own plate's rotation; vertices are duplicated along plate boundaries (M1 design). Where two
plates' rotations disagree, their shared boundary pulls apart. Through the slot the camera sees the
ocean shell, 2 km under sea level (`OCEAN_SHELL_DEPTH_KM`), painted ocean because the frame's
coverage mask says a plate covers that point.

Evidence (`defects/globe-252-plate-debug.jpg`, the verification view, top; normal view, bottom):
the blue shell shows only on the **risers** of the stair-stepped boundary between two plates (red
and dark red), one stroke per cell step, and never on the treads: the two plates have separated
across the risers. Each stroke reads as radial because the land is lifted 20× (a 500 m plain
stands 10 km up), so the slot is deep and seen obliquely.

It is not the documented "slivers 1–2 Myr from a frame" (M1 plan): 250 Ma is 2 Myr from the
252.0 Ma frame, but the strokes are just as strong **at exactly 252.0 Ma**, and gone at 244.6 Ma
(`defects/globe-250-252-244.6.jpg`: 250, 252, 244.6 Ma, top to bottom, same pose).

**Before the milestone:** yes. `globe.wesl`, `planet.wesl`, `crates/timefly/src/globe/` and
`timefly_core::{globe, geodynamics}` are byte-identical to `main`, with the same data.

**Fix (proposed, not made):** skirts. Each piece's boundary edges (an edge used by one quad of
its plate) get a quad hanging down to the shell, coloured from the edge's own sample, so a slot
seen at an angle shows the plate's side, not the shell. Alternatives: paint covered shell texels
with the frame's land where its elevation is positive (it would ghost land behind plates that
drifted away from their frame position between frames), or close the gaps in the bake by
overlapping neighbouring plates (the footprint dilation, R12, only fills unowned cells). The
verification view also shows depth-tie speckle where two plates overlap with equal elevation; it
is invisible in the normal view (same raster, same colour).

## B. Water strips beside perched rivers (fixed)

**Shots:** Phoenix (`…_33.833N_111.864W_38724m_…`) dotted dark lines along the valley edges;
the Andes east of Titicaca (`manual-checks.md` §17) thin dark-blue slivers a valley-width from the
river.

**Diagnosis.** Two ingredients:

1. *Perched levels.* The river's level comes from the 10 km lattice graph and here stands up to
   96 m above the fine DEM's valley floor. Every texel inside the river's valley half-width (its
   footprint, about 3 km wide on the Verde at 34.1° N) is a river texel holding that level, so its
   water stands over its ground. Inside the footprint the ribbon (negative away from the
   centreline) correctly hides it. Example, row 400: level 800 m over ground 714–750 m.
2. *The sentinel leak (the code bug).* Every non-river texel stores `RIVER_NONE` = ribbon
   +1,000 m (meaning "over standing water"), dry texels included. Readers rebuild a sub-texel
   river as `hw − |o|` with `hw = ribbon + |offset|` interpolated. Between a footprint texel
   (hw 11 m, offset −1,000 m) and a dry neighbour (hw 1,000, offset 0) the rebuilt ribbon turns
   positive about a quarter of the way across, and while the interpolated level is still above
   the ground, water is drawn: a strip along the footprint's edge, parallel to the river. The
   crossing guard's fallback (interpolating raw ribbons) had the same leak.

**Fix (commit on this branch).** In a cell that holds a river tap, a dry tap that is no river
(`level − h ≤ DRY_BELOW_M`) stands outside the water: ribbon −1,000 m, offset 0, no flow
(`timefly_core::terrain::river_taps`, mirrored in `terrain_cdlod.wesl`'s `tap_river`). Wet
non-river taps (lake, sea) keep +1,000 m, so river mouths and lake shores are unchanged; cells
with no river tap are untouched.

**Tests.** `a_perched_river_draws_no_water_at_its_footprints_edge` (a footprint 5.2 texels wide
holding its level 80 m over the ground; it failed with water drawn 1,397 m from the river, ribbon
11 m, thickness 0.04 m) and `the_crossing_guard_keeps_dry_taps_dry` (failed with ribbon 323 m
over a dry tap). Both assert their fixture creates the hazard. All 396 `timefly_core` tests pass;
`shader_compile` validates the shader.

**Renders.** `defects/phoenix-before-after.jpg` (the bug shot's pose, before and after: the
dotted lines are gone, the river is unchanged) and `defects/andes-before-after.jpg` (§17's pose,
4 km, 45° down; before is the committed `render-andes-rapids-down.jpg`: both slivers are gone; the
river is less white because that render predates Task 11c's later rounds; the straight terraces
remain, a separate cause, not investigated).

**Not run:** the `timefly` crate's unit tests and clippy. Disk was at the target guard's floor
(5.0–5.2 GB free; a `timefly` test binary costs about 900 MB) and the guard was already deleting
build dirs.

## C. The hook and the pit: a course off its valley (fixed)

**Shot:** Phoenix (the Verde north of Bartlett Lake). Probably also the Sichuan pit (§14), not
checked.

**Diagnosis.**

- The "hook" is a corner of the course's polyline: a lattice node, where one straight link meets
  the next (row 330 in `defects/phoenix-texels-projected.jpg`).
- Downstream of it the course crosses a plateau at 1,000–1,550 m while the fine DEM's own Verde
  valley, its floor at 790–890 m, runs 3–6 km east of it (rows 332–372, read from the
  uncarved texels beside the carve). The relaxation that moves a course onto its valley floor
  reaches only about ±3 km: node relaxation ±2.7 km, sub-point offsets ±3.3 km (`C` = 0.4 of
  the 11 km lattice spacing, interior minima only). The valley lies just past it.
- A river's bed is `min(B, τ)`: the coarse pass bed `B` (here 941–942 m, flat for 14 km) or
  lower where the fine floor allows, never higher. So the carve cut a slot 600–900 m wide and up
  to 400 m deep through the plateau to reach `B`.
- From 70 km the terrain mesh (LOD 3, 1.17 km quads) bridges the slot. The water is drawn the
  whole way: a render with the distant terrain dropped shows the river continuous through the
  slot (`defects/phoenix-mesh-experiment.jpg`, third panel; a 10× finer water mesh alone changes
  nothing). The water's depth pull, about 43 m there, cannot reach it under the bridge, and the
  per-pixel shading paints the slot's west wall black: the "pit".

**Before the milestone:** no; the carve and the river courses are the rivers milestone's.

**Fix (commit on this branch, the owner's choice of 2026-11-28).** A segment whose corridor
course stands more than `REROUTE_CUT_M` = 150 m over its pass bed at some interior sub-point is
refined along the least-cost path instead: the joining search of Task 11e (one lattice spacing
either side of the chord), with each step's climb measured from the pass bed lerped along the
segment rather than from the straight line between its ends' floors (with the ends' line, a
path between two plateau nodes has no reason to leave the chord). The path is smoothed four
times instead of once, since on gentle ground the cheapest 8-neighbour path is a staircase. It
replaces the corridor only where its own deepest cut is smaller. Segments with an end on one of
today's burnt courses keep their anchors; joining segments are unchanged. `relaxed_centreline`
(the reality check's) applies the same rule, so every region and the check agree.

**Tests.** `a_river_follows_its_valley_where_the_corridor_misses_it`: a valley that meets the
lattice at every node and swings 6 km off between them, cut into a plateau 400 m over the bed;
every refined sub-point stays within 50 m of the floor (RED: 75 m and more with the reroute off),
the result is identical at a budget of one unit a step, a second region 67 km away refines the
rerouted segment bit for bit the same, and the reality check's centreline crosses no plateau.
`links_that_do_not_join_a_course_are_unchanged` is re-pinned: both its fixtures hold corridors
that cut far deeper than 150 m (2 km in the tree), and the test now asserts each holds a rerouted
segment. `placed_boulders_and_collars_lie_in_the_drawn_water` judges its 200 m river against
`MAX_BOULDERS`, which that river already hit before the change (16,040 of 47,866 kept, the
check passing by 0.5%); the rerouted course is longer (53,088 generated, 16,115 kept). All 397
`timefly_core` tests pass and clippy is clean.

**The reality check.** Same 270 summary lines and verdicts as `9814212`: everything passes
except the Indus box, which failed identically before. Two of the Amazon box's missed links in
the Peruvian Andes moved slightly (worst 62.5 → 61.0 km, 27.7 → 28.4 km); the box passes.

**Renders.** `defects/phoenix-reroute-shot.jpg` (the bug shot's pose; left before, right after):
the river runs on past the hook instead of vanishing into a black gash; a few short breaks
remain where the real valley's own shadowed wall hides the water from 70 km.
`defects/phoenix-reroute-6km.jpg` (6 km, 45° down; top before, bottom after): the course leaves
the straight link and curves down the real valley; the slot, its two black trenches and the
smeared walls across the plateau are gone. The hook's corner stays: the upstream segment's
corridor does not cut deep enough to be rerouted.

**Sichuan (§14).** `defects/sichuan-reroute.jpg` (the §14 pose, 6 km, 45° down; top the
committed render, bottom after): the near-black pit beside the Min and the orphan dash on the
slope are gone. The course near the mountain front now swings west round a ridge before turning
east, with a shadowed slope beside the bend; whether that bend follows the real Min needs the
owner's eye (the texels there were not dumped).

**Not measured:** region generation time in release (`time_a_river_dense_region`). A rerouted
segment costs one path search like a joining segment (at most 4,096 lattice points); the debug
replays generated the Phoenix region in 372 ms of preparation.

## D. Kanto's tan bands: the beach rule is elevational

**Shot:** `…_35.461N_141.125E_19959m_…` (Kanto from the Pacific).

**Diagnosis.** The generator sets the shore flag where the carved height is within ±15 m of sea
level (`SHORE_BAND_M`, `worldgen::generate::surface`), and `terrain.wesl` paints sand where the
flag is set and the height is under about 1–9 m. Nothing in the rule asks how far the coast is,
although its own comment says sand "along shores, just above the sea". Kanto's plains are low:
13% of the inner field's land is under 9 m, in broad bands along the lower Tone and the Kujukuri
coast (`defects/kanto-low-land-map.jpg`: pale = under 9 m, tan = 9–15 m), which is exactly where
the frame is tan (`defects/kanto-bands-crop.jpg`). River carving adds little: 534 of 5,469 river
texels are under 15 m.

**Before the milestone:** yes (the rule and `terrain.wesl` are unchanged since `main`).

**Fix (proposed, not made):** gate the flag on coast distance. `surface()` already reads
`coast_km_at`, but that comes from the 4.7 km apron mask, so a beach could still be up to ~5 km
wide on a low plain; a beach a few hundred metres wide needs a fine (inner-field) coast distance,
a second pass over the field.

Side note: the manifest's sea-level curve reads +8.96 m at 76 years and +7.72 m at 1 ka (0 at
5 ka, −5.38 m at 7 ka). It does not move today's heights (the recent field is ETOPO plus the
ICE-6G change, exactly ETOPO at 0 ka), only the snowline and the zonal-biome fallback, but a sea
9 m above today's at the present looks wrong.

## E. Kanto's blocky lakes

Inland water in the bands stands at level 0 (no lake has another level): the lower Tone and the
lake there (449 texels, its bed down to −51 m). Beside it lie 1,341 dry texels at or below 0 m
(median −4 m, down to −48 m): ETOPO's sub-zero lowlands and lake floor that the sea and lake
candidates do not reach. Whether a cell floods is decided per hydro tap or lake-map cell, so the
wet/dry edge follows cell lines: the blocky outlines. This is the DEM-sign estuary problem and the
"lake map re-baked from water surfaces" follow-up already listed in the spec.

## F. Phoenix's teal patches

Projecting the frame's teal pixels onto the field: no water under any of them, and 87% sit on
biome class TemperateBroadleaf at 2,100–2,300 m (elsewhere Steppe, Mediterranean, ColdDesert).
The Mogollon Rim's forest, drawn as canopy colour on the 1.85 km Köppen cells.

## G. Maroon slow rivers

`river_tint` mixes `RIVER_CLEAR` (sRGB 0.10, 0.32, 0.33) toward `RIVER_SILTY` (0.36, 0.30, 0.20)
as the speed falls below `SILT_SPEED_MPS`; from altitude a narrow silty river reads as a dark
red-brown line. A lighter, greyer silt, or a blend toward the sky's reflection with distance,
would read as water. An art decision.

## Not investigated in this pass

Sri Lanka's short, broken rivers and the missing Mahaweli (a hydrology question: discharge
threshold or the lattice route), the Volga's single channel (the model draws one course per
river; a delta needs distributaries), Baikal's west-shore tan patches, Mega-Chad's speckled shore,
and the Andes valley's straight east–west terraces.
