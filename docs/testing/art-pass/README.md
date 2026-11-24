# Art pass: reference renders

The art pass (design: `docs/superpowers/specs/2026-11-23-art-pass-design.md`, plan and rulings:
`docs/superpowers/plans/2026-11-23-art-pass.md`) changes five looks in the region worlds:

- shaded snow;
- coastal foam and the shallow tint;
- sand dunes and stony plain in deserts;
- swell and swash on the sea;
- colour banding on exposed rock.

`before/` holds renders from `a682922` (the art pass's first commit, M2's look). The Grand Canyon
pair was re-shot from the same commit after the scene moved (ruling R11). `after/` holds renders
from the finished art pass.

All renders are 1280 × 720, from `tools/dev/region-shots.sh`, looking north from above the scene's
centre. The scene name gives the camera height:

- no suffix: 8 km;
- `-low`: 400 m, or 1.5 km for the Grand Canyon and the Rockies;
- `-high`: 40 km.

To re-render one scene:

```sh
tools/dev/region-shots.sh docs/testing/art-pass/after 1280x720 sahara-0ka-low
```

Stills can't show the motion. Each scene below says what to watch for when flying it in the game
(`--region=LAT,LON,AGE_YEARS`, then fly down with `Q`).

## What moves, and how fast

- **Dunes creep downwind.**
  - The main crests are 1 km apart and move one spacing per hour: about 17 m a minute.
  - The small crests are 200 m apart and move one spacing in 10 minutes: about 20 m a minute,
    a little faster than the main crests. They cross the main crests at about 20°, and their
    pattern cycles six times as often, so they are the motion you notice first.
  - Hover at 400 m over a sand sea for a minute: the crest lines slide, the ground doesn't.
- **Swash.** In the shallows the water level rises and falls every 8 s.
  - The water's edge slides 10–40 m up the beach and back.
  - Measured in the coastal scenes: a median of 30–36 m, and at most about 52 m peak to peak.
  - The phase changes slowly along the coast, so the shore doesn't pulse in lockstep.
  - A broken foam line rides the moving edge.
- **Swell.** Three long, low waves (100–156 m apart, 8–10 s, 0.5 m each) spread around the
  region's wind.
  - Below about 1 km, the water surface really rises and falls. It is full height from 500 m
    down, and within about 600 m of the point under the camera.
  - Above 1 km, only the lighting moves.
- **Moving light bands.** Three slow normal-only bands (400, 900 and 1,400 m apart; 16, 24 and
  30 s) slide across the sea at every height, so open water reads as alive from 8 km.
  - From 8 km they can look like a regular corduroy (a deferred minor in the plan appendix).

Every new period divides the game's one-hour time wrap, so nothing jumps when the wrap comes
round.

## Scenes

### Sahara, 0 ka (23° N, 10° E): `sahara-0ka`, `sahara-0ka-low`, `sahara-0ka-high`

- **Before:** one flat ochre wash at every height.
- **After:**
  - 8 km: a sand sea fills the foreground, with transverse crests that wander and fork. Stony
    plain, with darker gravel patches, lies beyond it.
  - 400 m: both crest octaves show. The windward faces are long and gentle, and the slip faces
    are short and dimmer.
  - 40 km: the stony plain's gravel mottling, with a faint corrugation where the erg is. No
    moiré.
- **Lit slip faces.** Here the wind is the north-east trade, and the fixed sun stands in the
  south-west. So the slip faces turn partly toward the sun and stay lit: dimmer than the
  windward faces, never dark. The owner chose to keep them lit ("Keep them lit"; plan
  appendix).
- **Watch for:** the crests creeping (above), and no shimmer on the crests as the camera climbs.
- **The sand-sea share:**
  - 30.2 % of desert ground over the sphere is sand sea (the mask's 70th-percentile threshold,
    measured with the Rust noise mirror);
  - 33.2 % of the Sahara inner region is sand sea;
  - the rest is stony plain.
  - The mask's seeds were picked so that this scene's centre sits on an erg with stony plain in
    view.

### Red Sea, 0 ka (bench stop, 20° N, 37.5° E): `bench-red-sea-0ka`, `red-sea-0ka-low`

- **Before:** a glowing turquoise halo around every island and along the coast, with a solid
  pale foam band.
- **After:**
  - The halo is gone. The shallows keep a faint clear-water tint near the shore, and the foam has
    faded out at this height.
  - The light bands move across the open water.
  - At 400 m (`-low`, over open sea), the swell's ripple pattern is much stronger.
- The coast in this bench scene is stony plain, not an erg, so the bench doesn't measure the dune
  path (ruling R8). The dune frame times are in `docs/testing/region-m2/performance.md`.
- **Watch for:**
  - at 400 m, the sea surface bobbing;
  - no turquoise rim reappearing as you climb.

### Red Sea coast, 0 ka (19.9105° N, 37.2116° E): `red-sea-coast-0ka-low` (`-shaded` and `-later`)

- After only. The two shots are 4 s apart, half a swash period, so the water's edge is at
  opposite ends of its travel.
- **After:** a desert beach (floor slope about 0.007). The foam line rides the edge, broken up
  by noise.
- Compare the two frames: the edge moves tens of metres, and so does the foam line.
- This scene replaced the earlier one at 19.9, 37.2, which was 1.7–5 km from the shore
  (ruling R10).

### Panama, 3 Ma (bench stop, 9° N, 79.5° W): `bench-panama-3ma`, `panama-3ma-low`

- **Before:** dark, flat open sea, with a faint teal patch in the lower right.
- **After:** light bands over the whole sea surface. At 400 m, a strong ripple pattern of swell
  and bands.
- **Watch for:** the surface rising and falling below 1 km.

### Panama coast, 3 Ma (9.0° N, 79.312° W): `panama-coast-3ma-low` (`-shaded` and `-later`)

- After only, 4 s apart.
- **After:** a forested coast. The foam line follows the edge. Swell bands show offshore.

### Beringia, 21 ka (bench stop, 62° N, 175° W): `beringia-21ka-low`

- **Before and after: no visible change.** This is forest and tundra, with no coast in frame.
  It is a control.

### Beringia coast, 21 ka (61.815° N, 176.516° W): `beringia-coast-21ka-low` (`-shaded` and `-later`)

- After only, 4 s apart.
- **After:** a near-flat shore (floor slope about 0.0007).
  - The foam band stays about 60 m wide on the ground. M2's depth band would have been
    kilometres of white here (ruling R9).
  - A 40 m probe toward the shore keeps the swash's reach bounded where the floor flattens
    (without it, the reach flooded up to 351 m).
- **Watch for:** the edge sliding tens of metres, never hundreds.

### Rockies ice margin, 21 ka (48.5° N, 113.5° W): `bench-rockies-21ka`, `rockies-21ka-low`

- **Before:** shaded snow is a darker, navy-tinged blue-grey.
- **After:** shaded snow is lighter and a paler blue. Lit snow is no brighter, and the relief
  reads as before.
- **Measured** on `bench-rockies-21ka`, darkest 5 % of the snowfield against its brightest 20 %:
  - shade:lit linear-luminance ratio 0.454 → 0.566;
  - B − R in sRGB on the darkest 5 %: 27 → 18.
- The owner approved this look: "Yes, the new lighter blue looks much better!"

### Himalayas, 0 ka (bench stop, 28° N, 86.9° E): `bench-himalayas-0ka`

- **Before:** vivid blue blotches in the deepest shaded snow, for example (13, 72, 171) at pixel
  (370, 415).
- **After:** the blotches are gone. Deep shade takes its hue from the surface, not from the blue
  sky ambient (ruling R7).
- **Measured:** pixels with B > 120 and B − R > 70 went from 147 to 0.
- The M2 docs shot `docs/testing/region-m2/bench-himalayas-0ka-shaded.png` shows the before.

### Grand Canyon, 0 ka (36.08° N, 112.25° W): `grand-canyon-0ka`, `grand-canyon-0ka-low`

- From the south rim at 8 km and 1.5 km, looking north across the gorge to the Kaibab forest.
- **Before:** flat plateau colour, with vivid blue blotches in the shaded gorge (the same
  deep-shade bug as the Himalayas).
- **After:**
  - Horizontal colour strata (warmer and cooler, lighter and darker) step down the canyon's
    slopes. They are faint, because the data softens the walls (below).
  - The blue blotches are gone.
  - There are no dunes on the plateau slopes: the sand sea fades out above a true slope of 0.03–0.07.
  - There are no dark contour lines across the foreground, because a slip face never turns past
    soft shade.
- **Watch for:**
  - the bands staying put as CDLOD patches morph (they may crawl as much as the snowline, no
    more);
  - no moiré as you climb.
- **What the data allows.** The relief in the generated field was measured with the ignored test
  `real_bake_region::grand_canyon_relief` (293 m texels from ETOPO 60″):
  - within 15 km of 36.1° N, 112.1° W: heights 976–2,532 m (1,557 m of relief);
  - land steeper than 0.27 rise over run (about 15°): 1.16 %;
  - land steeper than 0.6: 0 %;
  - within 150 km: heights 575–3,365 m, and 0.08 % of land is steeper than 0.27.

  The canyon walls reach the game as softened slopes of about 0.08–0.25, so the bands are keyed
  to a slope-scaled weight on dry, bare ground, not to a 15° gate (ruling R11).
- **Banded share of desert land** (band weight above 0.1, real bake, 0 ka):
  - Grand Canyon: 49.8 % within 15 km, 9.0 % within 150 km;
  - Sahara (23° N, 10° E): 0.00 % within 15 km, 0.13 % within 150 km;
  - flat plains (slope below 0.05) get exactly zero (asserted in the test).

### Barren era, 540 Ma (16° S, 0°): `era-540ma-barren`

- **Before:** a bright turquoise rim along the coast on the right.
- **After:** the rim is gone and the water is a calm grey-blue. The flat interior shows no bands:
  banding needs a slope, so plains stay plain.
- **Watch for:** strata on any slope you fly to in this era (there are no plants, so all ground
  counts as exposed).

## Not in these renders

- **Wet sand** was dropped (design §5.4): the band never agreed with the swash.
- **Owner checks still open:**
  - how the light bands look in motion from 8 km (a corduroy risk);
  - the dune and swash motion in general.
- **Frame cost:** the native region bench and the dune-path measurement are in
  `docs/testing/region-m2/performance.md` ("Art pass"). The owner's real-Chrome rerun is pending.
