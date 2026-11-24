# Art pass: snow, foam, dunes, waves and rock banding — design

Date: 2026-11-23. Status: draft for the owner's approval.
Builds on: the M2 region-worlds design (`2026-11-23-milestone-2-region-worlds-design.md`) and the
M2 plan's known limitations (`plans/2026-11-23-milestone-2-region-worlds.md`, "Known
limitations").

## 1. Intent

M2's regions are correct but look flat in places the owner noticed while watching the bench:

- shaded snow goes navy-grey;
- every coast wears a glowing turquoise halo from altitude;
- deserts are one flat ochre wash.

The art direction is painterly naturalism (main spec §9). This pass fixes those three looks.
It also adds the life the owner asked for (owner's words, 2026-11-23):

- "I want to see sand dunes. It'd be amazing if they moved a bit too."
- "It'd be even better if the waves lapped the shore & bobbed farther out."
- "Can we also add colour banding on exposed rock where appropriate?"

## 2. Goals, non-goals, anti-goals

**Goals**

1. **Shaded snow reads as snow:**
   - shaded snow is about half as bright as sunlit snow and pale blue, not navy;
   - rock and forest shading are unchanged.
2. **Coasts lose the halo from altitude:**
   - foam shows where its texture can be resolved, and fades to nothing (not to a 40 % grey
     band) where it can't;
   - the shallow-water tint stays, but no longer glows.
3. **Sand dunes** in sand seas within deserts:
   - crests lit on the windward side and shaded on the lee;
   - lined up across a wind that follows latitude;
   - creeping slowly downwind.
4. **Waves:**
   - long, low swell that visibly rises and falls offshore;
   - at the shore, the water's edge sliding up the beach and back every few seconds, with a
     foam line riding it.
5. **Rock banding:** horizontal colour strata on steep exposed rock, where the land is dry or
   bare.

**Non-goals**

- **No real dune shapes:**
  - no silhouettes, and no dune shadows on other terrain;
  - the CPU height field, camera clearance and collision do not change;
  - the dunes are paint and lighting only (approach C, §4).
- **No breaking waves or spray.** No water simulation, and the water never covers land beyond
  the swash.
- **No blowing sand, sand haze or dust storms.**
- **No real geology:** the banding is not a lithology map; there is no rock-type data.
- **No new data in the bake.** Everything comes from data the shaders already have: the biome,
  era, shore flag, height, slope and latitude.

**Anti-goals** (would make this a failure even if it works)

- **M2's anti-goals still hold:** no cracks, popping or swimming terrain, and the region still
  agrees with the globe.
  - Dune and wave motion is deliberate and slow, so it is not "swimming".
  - Banding and dunes must not crawl as CDLOD patches morph more than the snowline already
    does.
- **Shimmer or aliasing from altitude.** Every new pattern fades out as it shrinks below a
  couple of pixels, as the existing brush octaves do (`octave_weight`).
- **Era-wrong looks:**
  - no dunes where the climate isn't desert;
  - no banding under forest;
  - no green on any of it.
- **Breaking the region bench.** The art pass must not cost the web its frame budget: the bench
  gates of the M2 plan (Task 10, rulings R11–R30) still pass.
- **A coastline that moves:**
  - the swash moves the water's edge by tens of metres, never more;
  - at rest the shoreline is still the height field's zero contour.

## 3. Constraints

- **Unchanged from M2:** Bevy 0.20.1, WESL and the WebGPU ceiling.
- **The terrain heights are about 293 m apart** (a 300 km region on a 1025-point grid). The camera
  flies at least 150 m above the ground (`CAMERA_CLEARANCE_M`) and at most 60 km up. Every new
  look has to work across that whole range.
- **The water mesh is a polar grid around the camera:** rings 25 m apart near the camera, growing
  to 4 % of the radius farther out (`WATER_RING_M`, `WATER_RING_GROWTH`). Displacement must fade
  where the rings are too far apart to carry it.
- **What the shaders already know:**
  - The terrain shader knows the biome class and era per texel (surface bytes 0–1), so
    `HotDesert` and `ColdDesert` are known.
  - It also knows the shore flag, the true height, the slope and the pixel's latitude
    (`look.light.w`, as the snowline uses).
  - The water shader knows the sea-floor depth (from the terrain's height textures) and
    `globals.time`.
- **Determinism:** patterns are functions of position, time and data only, so every visit and
  every machine looks the same.
- **Large patterns are keyed to a global frame, not region-local metres.** This covers the
  sand-sea mask, the dune phase and the band warp. The same place then looks the same whichever
  centre a region was entered from, and M3's region streaming shows no seams.
  - This follows the generator's global lattice (overlapping regions match exactly).
  - The terrain material gets a per-region offset into that frame. The builder handles f32
    precision, for example by wrapping the offset per pattern period.
  - The existing small brush jitter can stay region-local.
  - My call (2026-11-23): the sand seas decide where the dunes are, so they belong to the
    place, not to the visit.
  - **As shipped:** the sand-sea mask, the dune warp and the band warp are planet-fixed, but the
    dunes' crest phase is the along-wind distance from the region centre (no per-region offset
    yet). So the crests shift between visits entered from different centres, and would seam under
    M3 streaming. The per-region phase offset is deferred to M3 (plan appendix, "Dune crests
    depend on the region centre").
- **Reference images live in `docs/testing/art-pass/`.**

## 4. Approaches

Three ways were considered for the dunes and waves (2026-11-23):

- **A. All painted.** Dunes and waves are lighting and colour tricks only.
  - Cheapest and lowest-risk.
  - But the sea would never visibly rise and fall.
- **B. All real shapes.** Dunes push up out of the terrain mesh, waves out of the water mesh.
  - Gives real silhouettes and shadows.
  - But dunes 20–150 m tall against a 150 m camera clearance mean the camera clearance, the CPU
    height copy, the shadow pass and the GPU-parity tests all have to learn about dunes.
  - Much more work, and the riskiest.
- **C. Hybrid. Chosen by the owner.**
  - **Dunes are painted:** a crest pattern with its own normals, so the sun lights it as if it
    had shape.
  - **Waves are real, on the water mesh only:** the vertex stage displaces the swell, and the
    water level near the shore rises and falls for the swash.
  - **Why it wins:** nothing collides with the water, so the camera and collision code are
    untouched, while the motion the owner asked for shows where it matters.

Snow, foam and banding are shading changes under any approach.

## 5. The design

### 5.1 Shaded snow

`paint_light` (`terrain.wesl`) applies one ramp floor (0.16) and one shadow hue shift
(`SHADOW_HUE_STRENGTH` 0.30) to every surface.

- **The change:** both become snow-aware.
  - Where the pixel is snow, the floor rises to about 0.45 and the hue shift halves.
  - Partial snow blends between the two.
- **Target:** shaded snow is about half the brightness of sunlit snow, with a pale blue cast.
- **Unchanged:** rock, forest and sand shading.
- **Builder's choice:**
  - how the snow weight reaches `paint_light` (plumbed from `paint` or derived);
  - the exact floor and hue values, within the target.
- **As shipped:** floor 0.30 and hue factor 0.78 (rulings R5–R6), and deep shade takes its hue
  from the albedo below l = 0.15 on every surface (R7). See the plan appendix.

### 5.2 Foam and the shallow tint

- **Foam fades out where its texture would alias.** Today foam keeps a 40 % floor when it fades
  (`mix(0.4, 1.0, w25)`), which turns into a solid pale band from altitude. The new foam:
  - fades to 0 as its pattern shrinks below a couple of pixels;
  - remains a broken, textured line near the camera.
- **The shallow tint stops glowing from altitude.**
  - Today the tint goes from `WATER_SHALLOW` (sRGB 0.27, 0.66, 0.64) to deep over 1 − e^(−d/scale).
    Over a texel 293 m wide, that turquoise shows as a bright rim.
  - The new tint keeps a clear-water look over shallow floors seen up close, but is less
    saturated and less bright where the camera is high.
  - Builder's choice: tune the colour, the scale, or a distance or footprint term.
  - The goal is judged by eye in the Red Sea and Panama shots.

### 5.3 Sand dunes

**Where:** sand seas, patches inside `HotDesert` and `ColdDesert` texels.

- A desert weight comes from the surface texel blend, like the other flags, so desert edges
  stay soft.
- A large-scale noise mask (tens of km) picks sand seas. Elsewhere in the desert is stony plain
  (see "Stony plain" below).
- **Coverage:** the sand-sea share is the builder's choice, aiming at about a quarter to a third
  of desert ground. That's real-world-like: most of the Sahara is not sand sea.
- **No dunes on:**
  - slopes: as shipped, the sand sea fades out between true slopes of 0.03 and 0.07
    (`DUNE_SLOPE`), and sloping desert is stony plain (plan appendix);
  - rock slopes (the slope test already paints rock);
  - snow;
  - beaches below the beach line;
  - water.

**Look:**

- **Shape:** transverse crests across the wind.
  - Crest spacing is about 0.5–2 km for the main dunes, with smaller dunes about 100–300 m apart
    on top. Both are warped by noise so crests wander and fork.
  - Real ergs look like this from altitude.
- **Lighting:** each crest has its own normal, so the sun's lighting (and the painterly ramp)
  makes:
  - a gentle, lit windward face;
  - a steep, shaded lee face (slip face).
  - The normal perturbs the terrain normal before lighting.
  - **As shipped:** the lee faces stay lit. Under the fixed south-west sun, slip faces in the
    trade-wind belts turn partly toward it, so they are only dimmer than the windward faces. The
    owner's ruling was "Keep them lit". A tilt cap (`bound_dune_tilt`, `DUNE_AWAY_LIMIT` 0.25)
    keeps any face from turning more than a quarter of the way to the terminator, so a slip face
    is at most soft shade, never a dark line. See the plan appendix
    (`plans/2026-11-23-art-pass.md`, "Rulings and deviations").
- **Colour:** the desert's own palette ground colour, slightly lighter on crests and darker in
  troughs (interdunes).
- **Altitude:**
  - from high up, the small octave fades out, then the main crests;
  - from the 60 km ceiling, a sand sea reads as a faint corrugated texture;
  - at 150 m, both octaves show.

**Wind:** the crest direction comes from the pixel's latitude (the era latitude, as for the
snowline):

- **Trade winds below 30°:** north-easterly in the north, south-easterly in the south.
- **Westerlies from 30° to 60°.**
- **Polar easterlies beyond 60°.**
- Blended smoothly across the boundaries.
- This is a pure function of latitude, written in Rust (`timefly_core`) with a unit test.
  - It is evaluated once per region at the centre (a region spans ±1.35° of latitude) and
    passed to the shaders, so it needs no WESL copy.
  - The dune phase uses that wind as one fixed 3D axis per region, because a coordinate along
    each pixel's own tangent axes is identically zero on a sphere (plan, Task 3). That phase is
    measured from the region centre, so it depends on the centre (see Determinism above).

**Motion:** the pattern creeps downwind, slowly.

- **Target:** a crest moves about its own height's worth in a minute: noticeable while hovering,
  never racing.
- The smaller dunes move faster than the main ones, as in nature.
- **Builder's choice:** the exact speeds.
- Motion uses wrapped time, so precision never degrades in a long session.

**Stony plain** (the rest of the desert):

- darker gravel patches and a faint mottling at two scales, from the existing brush noise;
- no new data;
- so a desert without dunes still isn't a flat wash.

### 5.4 Waves

**Swell (offshore):**

**The owner's decision (2026-11-23): real rise and fall shows when flying low.** From high up,
the swell reads as moving light on the water at every height: glints and ripple bands sliding
across the sea.

- **Why:** from 8 km a 3 m wave is under one pixel at 1440p, so no mesh can show it bobbing.
  Below about 1 km a real 0.5–1.5 m swell can be seen.
- **The mesh:** the water mesh gets finer around the camera when flying low, so the 60–200 m
  swell has at least 4 vertices per wavelength within the low-altitude view.
  - Above about 1 km it stays as it is.
  - The builder chooses how: rings that scale with the camera height, or a denser inner
    disc.
  - Either way, the builder re-checks the water mesh's existing chord-sag and depth-pull
    tests (`WATER_PULL_M`).

- **Displacement:** the water vertex stage displaces height by a small sum of directional waves.
  - Amplitude: about 0.5–1.5 m.
  - Wavelengths: about 60–200 m.
  - The wave directions are related to the dune wind (same latitude rule) but not identical.
- **Normals:** the fragment stage computes matching normals analytically from the same
  function, so light and Fresnel ripple with the swell.
  - The existing faint ripple stays on top.
- **Amplitude fades:**
  - with depth near the shore, so swell doesn't pile up on beaches;
  - with the local ring spacing, so a wave is never sampled by fewer than about 4 vertices per
    wavelength.
  - Beyond that, the normals alone carry the look.
- **Builder's choice:** the exact number of waves, Gerstner versus plain sine, and the speeds,
  within those targets.

**Swash (at the shore):**

- **The water level oscillates** with a period of about 6–10 s. **The horizontal reach is
  what's fixed:** the edge travels about 10–40 m up and down the beach.
  - The vertical amplitude is that reach times the local floor slope, capped at 0.5 m.
  - M2 coasts are interpolated over 293 m texels, so a fixed ±0.5 m would move the edge only a
    few metres on steep coasts but hundreds of metres on near-flat shores (21 ka Beringia).
  - My call (2026-11-23): a visible, bounded reach is what "lapping" means.
  - The phase varies along the coast with slow noise, so the whole coast doesn't pulse in
    lockstep.
  - The fragment stage's land test becomes "discard where the floor is above the local water
    level" instead of "above 0".
  - So the edge slides up the beach and back over the gently sloped bilinear height field: tens
    of metres.
  - The level offset applies only in the shallows (depth less than a few metres).
  - Offshore, the swell owns the surface.
- **The foam line follows the moving edge:**
  - a band where the water is thinnest over the floor;
  - broken up with noise;
  - fading out from altitude like the rest of the foam (5.2).
- **Wet sand:** a narrow, darker, shinier band above the highest swash line on beaches.
  - It's a terrain-shader change keyed on height near 0 and the shore flag.
  - It is optional. If it can't be made to agree with the swash, it is dropped and noted.
  - **Dropped (Task 8).** A terrain-shader band keyed on the swash's highest reach (same
    amplitude, shore probe and reach factor as the water) rendered on the Red Sea and Panama
    coasts as a thin dark strip with a hard straight edge that did not follow the water's edge,
    with undarkened sand between it and the foam. That is a gap, which this section forbids, so
    the code was reverted. Beaches keep their existing sand tint.
- **At rest:** with the swash's mean at 0, the shoreline is still the height field's zero
  contour (anti-goal "a coastline that moves").

### 5.5 Rock banding

**Where:**

- on rock pixels (the existing slope test, `rock`);
- in dry or bare land, also on moderate slopes, from about 15°, where the ground shows bare
  through the palette:
  - The source elevation is ETOPO 60″ (about 1.85 km), so canyon walls reach the game as
    softened slopes, often under the 31° rock threshold.
  - Banding only on cliffs would miss the Grand Canyon, the owner's named example
    (2026-11-23: "banding will look great in the Grand Canyon").
- weighted by how exposed the land is;
- the weight is the builder's mapping, but must give the following.

| Strong                                  | Faint                     | None                       |
| --------------------------------------- | ------------------------- | -------------------------- |
| `HotDesert`, `ColdDesert`, `Steppe`     | `Mediterranean`, `Tundra` | under snow                 |
| any texel in the Barren era (no plants) | `Alpine` rock             | forest biomes' canopy cover |

**Look:**

- **Strata:** horizontal bands of the palette rock colour, varied in value and slightly in hue.
  - Varied: warmer and cooler, lighter and darker.
  - Band thickness is about 10–60 m, mixed, so they don't look striped.
- **Position:** keyed to world height, with a slow noise tilt and warp of tens of metres over
  kilometres, so the layers dip and fold gently instead of being perfectly level.
- **Altitude:** the bands fade out where they would alias, like every other pattern.
- **Stability:** height-keyed bands use the true (blended) height like the snowline does. So
  they crawl only as much as the snowline when CDLOD morphs (a known M2 limitation), no more.

### 5.6 Performance

- **The new terrain work runs only where its weight is above zero.**
  - Dunes: desert pixels.
  - Banding: rock pixels.
  - Branch on the weight so other biomes pay nothing.
- **The water vertex stage adds two height-texture loads per vertex**, for the depth that damps
  the swell.
- **Gate:**
  - the native and Chrome region benches still pass every M2 gate;
  - the headless bench test still passes.
  - The bench's Red Sea stop (ruling R30: 0 ka, Köppen hot desert, a present-day coast at sea
    level 0) measures the dune, foam and swash cost. No new bench region is needed.
- **If the web budget is threatened, cut in this order:**
  1. the small dune octave;
  2. wet sand;
  3. swell displacement (keeping the normals).

## 6. How it's judged

- **Reference renders** go in `docs/testing/art-pass/`, before and after, with
  `tools/dev/region-shots.sh` extended as needed:
  - the Red Sea at 0 ka (coast, foam, dunes, banding in the Red Sea Hills);
  - the Sahara at 0 ka;
  - the Rockies at 21 ka (shaded snow);
  - Panama at 3 Ma (coast);
  - the 540 Ma Barren-era region (banding without plants);
  - the Grand Canyon at 0 ka (36.1, −112.1), the banding showcase. The builder measures and
    records how deep and steep the canyon is in the generated field, so the owner knows what
    the data allows.
  - at least one shot from low (about 300–500 m up) as well as the standard 8 km.
- **Motion is judged by the owner:** stills can't show it. The builder records what to watch
  for in each scene.
- **Tests:**
  - the wind-direction function gets unit tests and a GPU parity check like `snowline`;
  - the existing suites, the wasm check and the region bench pass.

## 7. What's left to the builder

- Exact colours, amplitudes, speeds, band thicknesses and noise scales, within the targets
  above.
- How the snow weight and the desert weight reach the lighting code.
- Whether swell uses Gerstner or sine waves.
- How the shallow tint is calmed from altitude (§5.2).
- The task order. Snow and foam are the smallest and land first.
