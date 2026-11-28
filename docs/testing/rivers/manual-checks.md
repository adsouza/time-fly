# Rivers and lakes: manual checks on the Mac

These are the owner's checks from the rivers and lakes design §7, plus the questions the 11x
tasks raised that need a person at the screen. Everything else is covered by
`cargo test --workspace`, `region_gpu -- --ignored`, the reality check (`--hydro-check`,
`reality-check.md`) and the benches (`performance.md` in this folder).

Run from a checkout that has the baked data in `crates/timefly/assets/data` (the main checkout),
with rustup's toolchain:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build --release -p timefly
TIMEFLY_LOG_DIR=/tmp/timefly-rivers-checks target/release/timefly --region=32.5,74.5,0
```

`TIMEFLY_LOG_DIR` keeps these sessions out of your real logs. `--region=LAT,LON,AGE_YEARS` starts
in a region; without it the game starts on the globe.

**Controls** (as M3): `W` `A` `S` `D` or the arrows fly; drag turns the fly and tilts the view;
`Space`/`E` climb, `Option`/`Q` descend; `Shift` with forward boosts; `Backspace` climbs back out
to the globe; `[` and `]` step to the older and younger snapshot. The HUD shows the altitude,
the latitude and longitude, and a compass rose.

**Reporting a problem: press `F12`** (`fn`+`F12` on Apple keyboards, or the camera button in
the window's top-right corner, with the frame-rate readout just left of it). The game saves a bug shot, a PNG and a JSON sidecar with the place, the era and the
view, in `~/Pictures/Time Fly/`. Send the pair; `--from-shot <sidecar.json>` starts the game at
that pose, so the builder sees what you saw.

Tick a box when it passes. If one fails, note what you saw next to it, with a bug shot.

**Reference shots** (this folder; `examples/region_tour.rs` offscreen at 1280 × 720 unless
stated; listed with each check below). They were rendered at `9814212`, the Task 12 tree,
except `render-rapid-closeup.jpg`, re-rendered at `385bdfc` (check 9). Task 11c's rounds 2–6,
after `9814212`, changed the rocks and the white water, so the other shots that show rapids
predate that look.

## Design §7 checks

### 1. The Indus tributaries

1. `target/release/timefly --region=32.5,74.5,0` (with `TIMEFLY_LOG_DIR` as above).
2. Fly north into the hills, then low (a few hundred metres) along a mountain river, then south
   onto the Punjab plain.

- [ ] Mountain rivers run in gorges, with rapids, and join on the Punjab plain.
- [ ] Water moves downstream (watch the ripples and the foam).
- [ ] Rapids churn over rocks: clustered boulders break the surface, with foam on their upstream
      faces and trailing downstream (see check 9 for the white-water look itself).
- [ ] No rivers on ridges.

Shots: `render-indus-tributaries-down.jpg` (6 km up, 45° down: the plain's river, continuous);
`render-indus-gorge.jpg` (`34.2,74.3,0`, 1.5 km: the Kashmir valley, the Jhelum winding to
the right, no artefact lake since Task 8b).

### 2. The Amazon

1. `--region=-3.1,-60.0,0`. Climb to about 12 km and look around, then fly south of Manaus.

- [ ] Wide, slow and meandering, at true width, with no map lines.

Shots: `render-manaus.jpg` (3 km, looking north: forest, the water only at the horizon) and
`render-amazon-south-down.jpg` (`-3.6,-60.0,0`, 12 km, 45° down). The second shows the main
channel at the top and a tributary meandering toward it; note for this check that the tributary
is drawn **dark maroon-brown** (the slow, silty tint) and reads more like a line than water from
this height, and the main channel's edges are slightly stepped.

### 3. The Sahara at 0 ka

1. `--region=16.0,15.0,0`.

- [ ] Dry: dunes, no lakes, no rivers.

Shot: `render-sahara-0ka-down.jpg` (20 km): dunes only.

### 4. The Sahara at 6–9 ka

1. `--region=13.0,17.0,8000` (unsnapped, 8 ka). Climb to about 150 km to see the whole lake.
2. On the globe, set the slider to the 10 ka snapshot and dive at about 13° N 17° E.

- [ ] Lakes and rivers, Lake Mega-Chad among them.
- [ ] The dive at the 10 ka snapshot shows them too.

Shot: `render-sahara-8ka-down.jpg` (150 km, 45° down): a lake filling most of the view, with
rivers coming in from the east. Its south-eastern shore is speckled (a ragged, grainy fringe of
small dry and wet patches) rather than a clean line: judge whether that reads as marsh or as
noise.

### 5. No seams

1. `--region=-32.65,-70.01,0` (the journey bench's Andes start). Fly east at a few hundred
   metres along a river, over the region's edge and on through a second hop.
2. `--region=32.5,74.5,0` (the Indus tributaries). Fly east along a river through two hops.

- [ ] No seam at the region edges: rivers and lakes continue in the same place and width across
      each hop, and nothing pops at the swap.
- [ ] Rapids' rocks are the same on both sides of a hop.

### 6. The Caspian at its own level

1. `--region=42.0,50.5,0`; climb to about 150 km. Then `--region=46.2,48.2,0` (the Volga's
   lower course) at about 20 km.

- [ ] The Caspian stands at about −28 m (the HUD over open water reads your height plus −28).
- [ ] The Volga lowlands are not flooded.

Shots: `render-caspian.jpg` (20 km: HUD 19,972 m = 20,000 − 28) and
`render-caspian-high-down.jpg` (150 km). See check 12 for the Volga delta.

### 7. The Dead Sea, Titicaca, the Great Salt Lake and Victoria

Levels from the reality check (`--hydro-check`, this task's run): the Dead Sea −427.0 m,
Titicaca 3,808.5 m, the Great Salt Lake 1,278.0 m, Victoria 1,133.0 m, each the lake map's
fine surface, off by 0.0 m (5 m allowed).

1. `--region=31.5,35.5,0`, `--region=-15.8,-69.4,0`, `--region=41.1,-112.5,0` and
   `--region=-1,33,0`, each a few hundred metres to 2 km above the water.

- [ ] Each lake stands at its level (the HUD over the water reads your height above it plus the
      level) with a natural shore, and no water wall at a lattice line.

Shots: `render-dead-sea.jpg` (400 m: HUD −27 m = −427 + 400) and `render-titicaca.jpg`
(2 km: HUD 5,808 m = 3,808 + 2,000).

### 8. A slider change in a region

1. In any region above, press `[` once (or drag the slider) and wait.

- [ ] The cloud holds while the pass runs, then shows the new era's rivers. No visible cut.

## The 11x tasks: today's courses, the ridge rule, tributary joins, white water

These checks cover Tasks 11b–11g: today's major courses burnt in (11b, 11d), lakes at their
table levels (11f), no drainage across a ridge (11g), tributaries down their own valleys (11e)
and white water (11c).

### 9. White water: a rapids close-up

1. `--region=32.5735,75.8714,0`, then descend to about 150 m over the river ahead (north).
2. Fly slowly along it upstream and downstream.

- [ ] Rocks break the surface in clusters, with a foam collar on each, piling white on the
      upstream face and trailing downstream.
- [ ] Standing waves hold still while the foam runs through them.
- [ ] The amount of white is right (your "softer, whiter wakes" of 2026-11-27: white at the
      rocks, 10–28% of the water at the wide reference poses, crests sparse and irregular).
- [ ] From afar a rapid reads pale, not white (`FOAM_MEAN` 0.19): is that right, or should
      distant rapids read whiter? ______

Shot: `render-rapid-closeup.jpg`, re-rendered at `385bdfc` (Task 11c's round 6;
a crop of a 3840 × 2160 frame at 150 m). Every rock in view sits in the drawn water (since 11c's
round 2; at `9814212` some sat on the bank). The near breaker trails one broad, solid white wake
downstream, a fluffy triangle with a feathered edge: the round-5 look the owner shipped
("Ship, tune later"), with lacier wakes a follow-up. Two small clusters with short wakes sit
farther up. The background crest foam is sparse and irregular (the old ladder of white stripes
is gone), though the distant reach still shows fine, regular ripples across the flow. The river
lies on a smooth grassy slope with no visible banks or gorge at this place.

### 10. Hells Canyon: the Snake in its canyon

1. `--region=45.2,-116.7,0`; fly north along the river at a few km, then low.

- [ ] The Snake runs in its canyon, not over the ridge, and no tributary cuts a notch across a
      ridge into it (Task 11g's ridge rule: the Salmon no longer crosses the 1.7 km ridge at
      45.48° N 116.36° W but drains north-east; the real Salmon joins about 40 km north).

Shots: `render-hells-canyon.jpg` and `render-hells-canyon-down.jpg` (6 km). The river follows the
canyon floor between bare walls. Note for this check: the near reach is drawn very dark brown
(slow, silty water in a carved slot) and turns blue-grey farther north; a short side stub joins
from the east.

### 11. The Caspian and the Great Lakes, Baikal: single lakes at table levels

1. `--region=42.0,50.5,0` at about 150 km: one Caspian at −28 m.
2. `--region=53.5,108.0,0` at about 150 km: Baikal, one lake at 455.5 m.
3. `--region=44.5,-82.5,0` at about 150 km: Huron, Georgian Bay and the North Channel, with
   Michigan to the west, as one lake at 176 m; Erie at 173 m and Superior at 183.4 m beyond.

- [ ] Each is one lake at its table level: no lower "fragment" lake inside it at the bed level,
      no step in the water.

Shots: `render-caspian-high-down.jpg`, `render-baikal-down.jpg` (one lake; note blocky tan
patches along the west shore, a strip of texel-sized bare ground) and
`render-great-lakes-down.jpg` (one water body across Huron, Georgian Bay and the North Channel).

### 12. The Volga delta

1. `--region=46.2,48.2,0` at about 20 km, then follow the river south-east to the Caspian.

- [ ] The Volga reaches the Caspian (the reality check: it passes 3.8 km from Volgograd and ends
      in the Caspian 70 km from its cited mouth).
- [ ] Note how the delta reads: in `render-volga-delta-down.jpg` (20 km, 45° down) the lower
      Volga is **one dark channel, not a fan of distributaries**; it has a short straight stub
      on its east bank and a T-shaped blob near the frame's edge. `render-volga-south.jpg`
      (from over the Caspian at `45.3,48.6,0`, 30 km, looking north) shows the same: one
      channel winding into the Caspian, no delta fan; the coast to its east is pink-beige flats.
      The real delta is hundreds of distributaries over about 100 km. Acceptable / needs work:
      ______

### 13. Sri Lanka (you saw odd rivers here)

1. `--region=7.9,80.7,0`; climb to about 20 km, then fly over the island's centre and north.

- [ ] Rivers where you expect them (the Mahaweli runs north-east to Trincomalee), no gaps.
- [ ] Note what looked odd when you flew here, with an `F12` shot.

Shots: `render-sri-lanka.jpg` and `render-sri-lanka-down.jpg` (`7.9,80.7,0`, 20 km, looking
north over the dry zone): flat tan ground with one short river in the north-west and two small
lakes near the north-east coast; no river crosses the view's centre. `render-sri-lanka-island-down.jpg`
(`6.5,80.7,0`, 150 km) and its crop `render-sri-lanka-west-crop.jpg` show what may be what you
saw:
- the rivers are mostly **short, thin, dark red-brown lines** in the wet south-west, several
  running side by side to the west coast, some breaking into dashes from this height;
- the dry north and east hold almost none, and **no large river runs north-east across the
  island** where the Mahaweli does;
- the wet zone's green meets the dry zone's tan along a **blocky, stepped edge** (the biome
  raster's cells), which is the terrain's colour, not the rivers.

### 14. A Sichuan tributary join (the corridor search)

1. `--region=31.0,103.6,0` (the Min leaving the mountains at Dujiangyan); fly north up the
   valley at about 6 km, then low over the joins.

- [ ] Tributaries reach the Min down their own valleys, not across a ridge.

Shots: `render-sichuan-min.jpg` and `render-sichuan-min-down.jpg` (6 km), and a crop of a
3840 × 2160 frame, `render-sichuan-min-crop.jpg`. The Min leaves the mountains and crosses the
plain as one continuous line. Things to look at:
- where it leaves the mountains it passes the edge of a **near-black pit** beside a bare slope:
  a deep, steep-sided hollow whose floor is in shadow (it may be a carved slot or a lake basin
  seen from above; it is not water);
- the river's colour switches abruptly from dark maroon (slow, silty) to blue-grey at a bend,
  rather than over a reach;
- a **tiny orphan river fragment** (a short curved dash) lies on the slope south of the river,
  joined to nothing.

**No tributary join is visible in these frames**: they show the Min alone, so this check's box
needs a flight up the valley. Task 11e's own frames of a join are
`joins-sichuan-before.jpg` and `joins-sichuan-after.jpg` in this folder (the biggest-gain join,
29.38° N 101.10° E).

### 15. The big falls (a data-resolution limit, as ruled)

1. `--region=-17.925,25.857,0` (Victoria Falls) and `--region=-25.695,-54.437,0` (Iguazu).

- [ ] Understood: the 10 km lattice spreads the 108 m and 82 m drops below the rapid threshold,
      so neither shows white water; waterfalls are a non-goal.

Shots: `render-victoria-falls-down.jpg` (the Zambezi, a wide dark navy river on a flat
savanna, no white water) and `render-iguazu-falls-down.jpg` (the Iguazu meandering through
forest, maroon-brown and blue in turns, no white water).

### 16. Qattara stays dry

1. `--region=29.5,27.0,0`.

- [ ] Dry desert; no sea plane over the below-sea basin.

Shot: `render-qattara.jpg` (3 km): dry.

### 17. The Andes east of Titicaca

1. `--region=-15.6,-68.7,0` at about 4 km, 45° down.

- [ ] A clear river in its valley, white on steep reaches.

Shots: `render-andes-rapids-down.jpg` (4 km, 45° down) and a crop of a 3840 × 2160 frame of the
same pose, `render-andes-rapids-crop.jpg`. The main river (pale, white-flecked: rapids) runs
continuously up the valley. **Two things look wrong:**
- **Straight east–west steps in the terrain** across the valley: several sharp, perfectly
  straight edges with hard shadows, like terraces cut along grid rows, on both valley sides;
- **thin dark-blue water slivers running parallel to the river** about a valley-width away on
  each side, along the outer edges of the carved valley, with dark undersides. They look like
  water drawn along the carve's edge (or doubled rivers in a breached valley, deferred since
  Task 8b), not real streams.
Both show identically in a render from the `12ef525` build (before Task 11c), so white water
did not cause them; neither was investigated further in this task.
