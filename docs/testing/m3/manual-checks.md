# Milestone 3 (the fly and zoom-and-swap): manual checks on the Mac

These are the owner's checks from design §10, plus the questions the build raised that need a
person at the screen: taste, comfort and what reads as "the same place". Everything else is
covered by `cargo test --workspace`, `region_gpu -- --ignored` and the two benches (see
`performance.md` in this folder).

Run from a checkout that has the baked data in `crates/timefly/assets/data` (the main checkout),
with rustup's toolchain:

```sh
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cargo build --release -p timefly
TIMEFLY_LOG_DIR=/tmp/timefly-m3-checks target/release/timefly
```

`TIMEFLY_LOG_DIR` keeps these sessions out of your real logs. Add `--region=LAT,LON,AGE_YEARS`
to start in a region instead of on the globe (the fly at M2's start pose).

**Controls:** `W` `A` `S` `D` or the arrows fly; drag turns the fly and tilts the view;
`Space`/`E` climb, `Ctrl`/`Q` descend; `Shift` with forward boosts; on the globe the scroll
wheel climbs and descends; `Backspace` climbs back out to the globe; `[` and `]` step to the
older and younger snapshot; `F12` (or the small camera button under the compass) saves a bug
shot, a PNG named by the place and view with a JSON sidecar beside it, in `Pictures/Time Fly`
(see check 16); `--from-shot <sidecar.json>` starts the game at a shot's pose.

**The HUD** (top left): the altitude, under it the latitude and longitude (`22.87° N  71.42° E`,
in the era's frame, the frame `--region` takes; marked `(then)` past 26 ka, where that frame is
no longer today's), and under that a heading-up compass rose: its top tick is where the fly
faces, the N (red), E, S and W marks turn to show true directions, and the label gives the
heading (`NE 47°`).

Tick a box when it passes. If one fails, note what you saw next to it (a screenshot helps).

**Reference shots** (this folder; `examples/region_tour.rs`, offscreen at 1280 × 720, Task 11's
tree):
- `alps-cloud-half.png`: the cloud held at density 0.5 over the Alps at 21 ka, 8 km up
  (`cloud` mode). Near ground is veiled, not sharp in front of the cloud.
- `alps-cloud-full.png`: the same at density 1.0, the white-out (note the pale HUD text, check 8).
- `himalayas-globe-above.png`: the fly 300 km above the cloud top over the Himalayas (`dive`
  mode).
- `himalayas-region-below.png`: the region below the cloud base after that dive, at 230 km (for
  check 9).

## Design §10 checks

### 1. The dive and the climb, recent era

1. Launch on the globe at the present. Steer toward the Alps (about 46.5° N, 10° E) and descend
   (`Ctrl`) until the cloud gathers and you pass through it.
2. Below the cloud, fly a little. Then climb (`Space`) up through the cloud base until you come
   out over the globe.
3. Repeat once with `Backspace` instead of climbing by hand.

- [ ] The cloud gathers around the fly, turns fully white, and the region appears below it.
      No frame shows a cut: land never changes shape, place or colour while you can see it.
- [ ] The fly is in view and the right way up throughout, including inside the white-out.
- [ ] While the cloud gathers and clears, its top and bottom bands are softer than its middle
      (the tilt-shift, restored at your request on 2026-11-25).
- [ ] Climbing out lands over the same spot on the globe, heading the same way.
- [ ] `Backspace` climbs out the same way, with no jump.

### 2. The dive and the climb, deep era

1. On the globe, step back to 250 Ma (`[` until the label reads 250 million years, or the
   slider). Steer to the Pangaea interior (about 22° S, 18° W in the era's frame) and dive.
2. Fly a little, then climb back out.

- [ ] As check 1: no visible cut, the fly always in view, a matching spot on the way back up.
- [ ] Between about 1,250 and 2,500 km, the land now fills the frame: note whether the
      staircase of ocean between plate pieces is acceptable (Task 4 raised it; the cloud only
      hides it near the dive). Acceptable / needs work: ______

### 3. A hop across a coastline

1. Launch with `--region=39.5,-11,0` (off Portugal; the region's east edge lies on the coast).
2. Fly east (`W`) at a low height and keep going over the edge. The fly lifts to about 60 km
   into the cloud bank, the swap happens inside it, and the fly comes back down.
3. Look back west at the coast you crossed.

- [ ] The lift is gentle, the cloud is opaque at the swap, and the fly eases back to its old
      height above the ground.
- [ ] The coastline continues across the hop: same shape and place on both sides.
- [ ] The speed doesn't jump at the swap.

### 4. Flying over a pole

1. On the globe at a medium height, steer due north and keep going over the North Pole, then
   do the same over the South Pole.

- [ ] No jump in heading or position at the pole; the view doesn't spin.
- [ ] The fly is lit well enough to read at high latitude (Task 4 noted it may look dark under
      the low globe sun; unconfirmed).

### 5. Finding the fly at every height

Look for the fly, without hunting for it, at each of these:

- [ ] the globe's top altitude (just after launch);
- [ ] just above the cloud top (2,500 km) and at the dive altitude (1,200 km);
- [ ] in a region just below the cloud base (about 290 km) and at 230 km where you emerge;
- [ ] in a region near the ground (300 m to a few km), over dark forest and over snow.

### 6. A slider change mid-hop

1. In a region, fly toward an edge. While the cloud bank is gathering for the hop, press `[`
   once (or drag the slider).

- [ ] The change waits for the hop (design §9): the neighbour appears at the old era, and once
      the hop's cloud has cleared, the era change runs behind its own cloud. No visible cut.
- [ ] The label and the land agree once the last cloud clears.

### 7. Motion comfort

Fly a full journey: dive, two hops, climb out.

- [ ] No abrupt camera swings, no roll you didn't ask for, no sudden speed change at a swap.
- [ ] Nothing in it made you queasy. If something did, note where: ______

### 16. Bug shots and the compass (Task 12, the owner's request of 2026-11-26)

Reference frames (this folder, `examples/region_tour.rs` `bugshot` and `globebug`, offscreen at
1280 × 720): `hud-region.jpg` (the Alps at the present, 8 km up, facing NE 47°) and
`hud-globe.jpg` (over the Himalayas at 5,000 km, facing NE 47°).

1. Launch on the globe. Fly a little, turning: watch the rose. Then dive into a region and fly
   there too.
2. Press `F12` once on the globe and once in a region, and click the camera button once.
3. Quit with `Cmd+Q`. Relaunch with `--from-shot` and one of the region shot's `.json` files:
   `target/release/timefly --from-shot "$HOME/Pictures/Time Fly/timefly_….json"`.

- [ ] The readout sits right under the altitude and the rose under it, small and out of the way.
- [ ] The rose's top is always where the fly faces; turning right swings N to the left.
- [ ] Each shot shows a toast naming its full path, and the PNG and the `.json` are both there.
      The PNG is the frame you saw (HUD included), not a blank one.
- [ ] The relaunch opens a window of the shot's size at the same place, era and view: the frame
      matches the PNG (the wings, water and clouds move with time). The camera stays put until
      you press a key or click.

## Questions the build raised

### 8. Pale UI text over the full white-out (Task 5)

1. Dive, and watch the altitude readout and the slider's end labels while the cloud is fully
   white.

- [ ] The text is readable enough, or it doesn't matter for the second or two it lasts.
      If not, the fix is a small HUD tweak (a shadow or a darker plate).

### 9. Land just above and just below the cloud: the same place? (Task 7)

The globe's relief flattens as you descend so that the land above the cloud matches the region
below. The region is framed from 230 km when you emerge, and the globe from the dive altitude.

1. Dive slowly into the Himalayas (28° N, 86.9° E, present) or the Alps. Note the coast,
   mountain or river valley under the fly as the cloud closes.
2. As the region appears below the cloud base, look for the same features.

- [ ] It reads as the same place: you can recognise what you dived into.
- [ ] If not, what differs (framing, scale, colour): ______

### 10. Pangaea from 290 km is mostly haze at the start pitch (Task 6)

1. Launch with `--region=-22,-18,250000000`. Climb (`Space`) to just under the cloud base
   (about 290 km) without dragging, so the view stays at the start pitch.
2. Then drag down to look at the ground.

- [ ] Acceptable: looking north at the start pitch is mostly ochre haze over a curved limb,
      and dragging down shows the dune sea clearly. Or: the view should tilt down with height
      (a look-down bias). Choose: acceptable / add the bias.

### 11. The shadow-cascade seam between 60 and 120 km (Task 6)

1. In a mountainous region (the Himalayas or the Alps), climb steadily from 50 km to 150 km
   while looking ahead and down.

- [ ] No visible line or band where the mountain shadows stop (around 60–120 km the seam fades
      out under about 41% fog at 80 km).

### 12. A `--region` launch goes from a clear globe straight to an opaque cloud (Task 7)

1. Launch with `--region=46.5,10,0` and watch the first seconds. Also watch the region bench's
   entries (`--region-bench`).

- [ ] Acceptable: the clear globe pops to an opaque cloud with no gather, then the region
      emerges. Or: it should gather like a dive. Choose: acceptable / gather.

### 13. A tap of S in the hop's hold costs a re-lift and a regeneration (Task 9)

1. In a region, fly east over the edge. While the fly is held in the opaque cloud waiting for
   the swap, tap `S` once.
2. Push east again.

- [ ] Expected now: the tap ends the hop. The fly is placed just inside the edge, facing in,
      the cloud clears, and pushing out again starts a fresh hop: a new lift and a new
      generation (about 5 s plus 3 s natively, 8 s on the web).
- [ ] Acceptable, or should the hop stay alive while the fly is still outside? Choose:
      acceptable / keep alive.

### 14. Turning straight back out after a failed hop holds the fly at the edge (Task 9)

1. Do check 13's tap, so the hop fails and the fly faces inward at the edge.
2. Turn straight round (`A` or `D`, or drag) and push east (`W`) at once.

- [ ] Expected now: the fly is held at the edge for several seconds (about 6 s) until the next
      hop's cloud gathers, then the hop runs.
- [ ] Acceptable, or does the hold feel like an invisible wall? Choose: acceptable / needs work.

### 15. The journey bench's routes (Task 10; answered by the owner, 2026-11-25)

Design §10 said the journey bench dives into "one of the bench regions"; Task 10 flew only the
Alps. The owner asked for geographic diversity, a cube corner, a coastline, the Grand Canyon, the
Andes in place of the Alps and Mt Fuji. A run now flies the Andes north from Aconcagua, Pangaea
at 250 Ma, the Grand Canyon north, Mt Fuji west over the Kantō coast to the cube corner at
35.26° N, 135° E, then the Andes again (`JOURNEY_ROUTES`, `JOURNEY_ORDER`). Any single place can
still be flown with `--journey-bench --region=LAT,LON,AGE_YEARS` (east, three times).

- [x] Answered: the route set above.
