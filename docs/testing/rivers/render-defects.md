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
| C | Phoenix: river breaks at a hook beside a dark pit; Sichuan: near-black pit | A 2–3 texel slot, 400 m deep, carved under a flat lattice-held level; hidden by the coarse mesh from afar | No (rivers milestone) | Diagnosed; options for the owner |
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

## C. The hook and the pit: a narrow slot under a held level

**Shot:** Phoenix (the Verde north of Bartlett Lake). Probably also the Sichuan pit (§14), not
checked.

**Diagnosis.**

- The river's course is a polyline with sharp corners; the "hook" is one of them (row 330 in
  `defects/phoenix-texels-projected.jpg`; `defects/phoenix-slot-6km.jpg` shows the corners).
- Downstream of it, the level is held flat at 941 m for 14 km (rows 320–368), then drops 110 m in
  2 km (rows 368–380, where the bed is carved to 753 m, 170 m under the water). The held reach is
  carved as a slot 2–3 texels (600–900 m) wide and up to 400 m deep into a 1,300–1,550 m plateau
  (the real Verde is at about 750–800 m there).
- The water is present the whole way: offsets change sign cleanly between neighbouring texels
  and the half-width is a steady 11.3 m. From 6 km it draws as a continuous river
  (`defects/phoenix-slot-6km.jpg`).
- From 70 km (the shot), the coarse CDLOD mesh bridges the slot and the water, pulled toward the
  camera by only 1 m + 6e-4 · d (about 43 m), is hidden under it. The terrain shader's per-pixel
  normals still paint the slot's 400 m west wall in shadow: the black "pit".

**Before the milestone:** no; the carve and the held levels are the rivers milestone's.

**Options (owner's call):**
1. Shape deep cuts so the mesh can resolve them: widen the valley with depth (a wall slope that
   keeps a cut at least N texels wide per 100 m of depth), so a 400 m cut is a broad V rather
   than a slot.
2. Hide less: test the water against the fine height rather than the morphing mesh, or pull it
   further toward the camera where the mesh is coarse (risks water over rocks, cf. the depth-pull
   note in `water.wesl`).
3. Fix the level: held reaches follow the lattice's spill, not the real valley; a reach relaxed
   onto the fine DEM's valley floor (as burnt courses are) would need no slot at all.

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
