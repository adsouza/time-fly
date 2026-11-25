# Time Fly Milestone 3: The Fly and Zoom-and-Swap — Design

Date: 2026-11-24. Parent design: [`2026-11-21-time-fly-design.md`](2026-11-21-time-fly-design.md)
(the "spec" below). Previous milestone:
[`2026-11-23-milestone-2-region-worlds-design.md`](2026-11-23-milestone-2-region-worlds-design.md)
("M2" below). This document narrows the spec's build-order step 3 ("Zoom-and-swap: globe ↔ region,
then region ↔ local") into one milestone. Where it is silent, the spec governs. Where it differs
from the spec, the owner decided the change (§1.1).

Choices the owner made are stated plainly. Choices the builder made are marked *(call)*, and the
builder may revise them with a reason recorded in the plan.

## 1. Intent

Replace M2's stand-in entry (double-click the globe, fade through haze, `Backspace` to leave)
with the real thing. The player is a **dragonfly**, seen from a chase camera, and it flies
everywhere: over the globe, down through a cloud into a region, across the region, through a
cloud bank into the next region, and back up to the globe. No menus, no loading screens, and no
cuts the player can see.

**Why now:** the spec's first requirement for nesting is that "moving between levels feels like
nesting: zoom-and-swap with continuous visuals" (spec §2). M2 proved the region worlds; M3 makes
getting into and between them feel like one journey. The creature arrives now because the
journey is the creature's.

**Success looks like:**
- From the opening globe, the player can steer the fly down to any spot on Earth at any era and
  arrive in a region without seeing a cut, a loading screen or a jump in the land.
- Flying out of a region's detailed centre carries the fly over a cloud bank into the next
  region, so the player can cross a continent without returning to the globe.
- Climbing out returns to the globe over the same place.
- Flight stays smooth (the M2 frame-time gates still pass), including during transitions.
- The fly is always easy to find on screen and never clips into the ground.

### 1.1 Owner decisions (2026-11-24)

1. **The creature is a dragonfly**, from the spec's "hummingbird or dragonfly" (§11.1). It is a
   time-travelling fly, so it appears in every era, including before dragonflies' ancestors
   (~320 Ma). Never gate it by era.
2. **The chase camera sits well behind and above the fly**, so the fly needs little modelled
   detail.
3. **The fly is everywhere, the globe included.** Steering the fly over the planet replaces the
   globe's drag-to-spin orbit camera.
4. **The globe camera may now get much closer** than M1's full-disc limit (2.74 Earth radii).
5. **Globe ↔ region transitions look like passing through a cloud.**
6. **Approach A** (§4): the swap happens inside the cloud. Below it, only the region is drawn.
   This drops the spec's rule that the parent stays visible past the child's edge.
7. **Region ↔ region, sideways.** As the fly nears the border, the neighbouring region is
   preloaded. At the border the fly is lifted to about 60 km into cloud cover that hides the
   swap. This replaces the spec's "sideways goes up through the parent and back down" (§11.2).
8. **Region ↔ local moves to M4**, since local worlds don't exist yet *(proposed by the builder,
   not objected to)*.
9. **The globe's relief flattens as the fly descends**, so the land above and below the cloud
   matches (§5.4).

### 1.2 Goals

1. **The dragonfly and its chase camera**, in regions and on the globe, replacing both M2's free
   camera (`fly_camera.rs`) and M1's orbit camera (`camera.rs`).
2. **Globe flight**: steer the fly over the planet, from orbit down to the dive altitude.
3. **The cloud**: one painterly cloud effect shared by every transition (§6).
4. **The dive and the climb** between the globe and a region (§7).
5. **The region hop** between neighbouring regions, with preloading (§8).
6. **Regions seen from higher up**: a ceiling raised from 60 km to the region's cloud base, with
   curvature and haze (§7.3).
7. **Benches and verification** for all of the above (§10).

### 1.3 Non-goals (this milestone)

- Region ↔ local transitions, local worlds and street worlds (M4 and later).
- Swimming, landing, perching, and flight below the M2 clearance (150 m above
  `max(terrain, 0)` in regions). The fly's near-ground life starts with local worlds.
- Weather clouds on the globe or over regions (the spec's "painterly atmosphere rim, clouds",
  §7.1). The transition cloud is an effect around the fly, not a weather system *(call)*.
- Gamepad controls (spec §11.1 lists them). Keyboard and mouse only for now; gamepad goes with
  the polish milestone *(call)*.
- Captions, settlements, ships, sound.
- Lakes and rivers. The owner put them in a milestone of their own between M3 and M4, at region
  scale, from the generator's hydrology (spec §7.5) (2026-11-24).
- Fixing the globe's resolution at low altitude (its cube map is 128 texels a face, about 80 km
  a texel). The cloud covers the altitudes where it would show (§7.1).

### 1.4 Anti-goals

- **A loading screen**, or a frozen view while something loads. Waiting happens inside the
  cloud, with the fly still moving gently.
- **A visible cut or jump**: land that changes shape, position or colour while the player can see
  it. Every swap happens behind an opaque cloud.
- **The fly lost or clipping**: off screen, hidden behind terrain, inside a mountain, or the
  camera inside the ground.
- **Motion sickness**: abrupt camera swings, roll the player didn't ask for, or sudden speed
  changes at a swap.
- **A region that disagrees with the globe it came from** (carried over from M2).
- **Regressing M2**: region frame times, generation times or memory gates getting worse.

## 2. Current state (what M3 replaces)

- **Globe** (M1): an orbit camera (`camera.rs`, `OrbitState`) between 2.74 and 6 Earth radii from
  the centre. Drag spins, scroll zooms. The globe is drawn by several bridge cameras on render
  layers (recent, deep, composite, atmosphere) that copy the orbit camera's transform.
- **Entering** (M2 §6.1, `region/navigate.rs`): double-click ray-casts to the globe, snaps the
  time to the nearest snapshot, sends `RegionRequested`, eases the orbit camera in to
  `NEAR_ORBIT_KM` (the closest zoom), and when the region is ready fades through a pale haze
  overlay (`FadeOverlay`, `ENTER_FADE_S` 0.8 s) to the region camera 8 km above the centre.
  Only one camera is ever active.
- **Region flight** (M2 §6.6, `fly_camera.rs`): a free camera. `W` `A` `S` `D` and the arrow keys
  move, drag looks, `Space`/`E` climb, `Ctrl`/`Q` descend, `Shift`+forward boosts. Speed is
  0.8 × height above ground per second (60 m/s to 10 km/s). Clearance 150 m. Soft ceiling at
  60 km. A soft edge nudges the camera back inside the 300 km inner field.
- **Leaving:** `Backspace` fades back to the globe centred on the region.
- **Snapshot changes in a region** regenerate around the camera's crust position and fade over
  about 1 s (M2 §6.2).
- **Regions** are generated on demand, centred anywhere, on one planet-wide sample lattice, so
  overlapping regions agree exactly on height (the zonal biome is the one window-dependent
  field). Generation: native ≤ 3 s target (measured 0.3–2.4 s first entry, ~0.45 s warm); web
  runs in 6 ms slices per frame (12 ms behind a fade), ≤ 8 s target.
- **Overlays:** the owner's altitude readout (`ui/altitude.rs`) shows the active camera's
  altitude above sea level; the FPS readout sits top right.
- **Launch options:** `--region=LAT,LON,AGE` starts in a region; `--region-bench` drives
  `RegionRequested` and `FlyState` directly; `--scrub-bench` drives the globe.

## 3. Constraints

All of the spec's constraints (§3) and M2's (§3) apply: Bevy 0.20.1 and WESL, the WebGPU
ceiling, f32 inside worlds and f64 on the CPU for geography, functional core and imperative
shell, AOP-style tracing, domain events as Bevy messages with loggers, and the crash flow. In
addition:

- **Only one world is drawn at a time** (as in M2). A swap happens on a frame where the cloud is
  fully opaque, so the frame cost never doubles.
- **No `std::thread` in core loops**; the web generates in time-boxed slices (M2).
- **Every transition is a pure decision function** in `timefly_core` (or a pure function in the
  shell), gather → decide → apply like M2's `step_nav`, so it can be tested without a window.

## 4. Approaches

How the globe ↔ region descent is built. The globe and region are separate scenes today and are
never drawn together.

| | Approach | Trade-off | |
|---|---|---|---|
| **A** | **Swap inside the cloud.** The globe is drawn down to the cloud; at the white-out the region replaces it; below the cloud only the region is drawn, bent to read as curved, with haze hiding its far edge. | Simplest and robust: the cloud hides the whole handover. Drops the spec's "parent visible past the child's edge". | **Chosen** by the owner |
| B | The region is drawn on top of the globe in one shared view below the cloud. | Keeps the spec's rule and a true planetary horizon. Two scenes with different units and precision must align in one camera, and the region's detailed coastline sits next to the globe's 80 km texels, putting M2's anti-goal on screen. | Rejected |
| C | Give the globe enough close-range detail to descend without a swap. | The spec rejected one continuous planet (spec §4, approach 3). | Rejected |

**Region ↔ region**, considered after the owner asked to fly between regions:

| | Approach | Trade-off | |
|---|---|---|---|
| **1** | **Preload the neighbour, then hop through a cloud bank at about 60 km.** | Short, local, and the cloud hides the one field that differs between overlapping regions (the zonal biome colour). | **Chosen** by the owner |
| 2 | Up through the globe and back down (the spec's rule). | A full dive per 300 km of travel. | Replaced |
| 3 | A cloudless swap relying on exact overlap. | Heights match exactly, but biome colours and the detail boundary would visibly shift. | Rejected |

## 5. The dragonfly and the chase camera

### 5.1 The fly

- **Look** *(call)*: a slim segmented body and four translucent wings in painterly, iridescent
  blue-green, lit by the scene's sun. Wings flicker (a vertex-shader or transform flap at a
  rate that reads as a blur, not individual beats), and the body banks into turns and pitches
  with climbs. Low-poly and generated in code (no external asset), since it is always seen from
  well behind and above.
- **Size on screen:** the fly stays about the same size on screen at every height *(call:
  about 4–6% of the view's height)*. Its true 5–10 cm size is meaningless above a few metres, so
  it is drawn at a scale that follows the chase distance. It is the same fly from orbit to the
  region floor.
- **Hover:** with no input, the fly hovers in place with a small idle drift and wing flicker,
  rather than stopping dead *(call)*.
- **Era:** the same fly in every era (§1.1).

### 5.2 Controls

The same keys as M2's free camera, now steering the fly, everywhere *(call)*:

- `W` `A` `S` `D` and the arrow keys fly (forward, back, turn or strafe as in M2, including the
  arrow-key rules from PR #10).
- Drag turns the fly and tilts the view.
- `Space`/`E` climb, `Ctrl`/`Q` descend, `Shift` with forward boosts.
- `Backspace` starts an automatic climb back to the globe (the reverse zoom), a shortcut kept from
  M2 *(call)*.
- The slider, `[` and `]` behave as now, with the transition rules in §9.

**Speed scales with height above ground** everywhere, as in M2 (one 0.8 × height per second),
so every height feels as quick to cross. On the globe "ground" is the globe's surface (sea
level or the globe's land height at that era).

- **Caps** *(call)*: in regions, M2's cap stays (`MAX_SPEED_MPS`, 10 km/s), so even near the
  cloud base the inner field takes about 30 s to cross and hops stay at least that far apart.
  On the globe the rule is kept, but the turn rate of the planet beneath the fly is capped
  *(about 30° a second)*. Uncapped, 0.8 × height at 6 Earth radii spins the globe about 230° a
  second.
- **Hops happen at every height** below the region's cloud base (§8). There is no special
  high-altitude rule *(call)*.

### 5.3 The chase camera

- **Placement:** well behind and above the fly, looking slightly down past it so the land ahead
  fills the frame. The distance behind scales with height above ground (and with altitude on
  the globe), so the fly's on-screen size holds steady *(call: tuned so the fly sits a little
  below the view's centre)*.
- **Smoothing:** the camera follows the fly's position and heading with critically damped
  smoothing, never rolls unless the fly banks, and never swings more than the fly turned
  *(call)*.
- **Clearance:** the camera, not just the fly, stays above the ground. When terrain rises behind
  the fly, the camera rises (and shortens its distance) rather than entering the hill. The fly
  must never be hidden behind terrain for more than a moment.
- **Overlays:** the owner's altitude readout shows the **fly's** altitude above sea level, on
  the globe and in regions *(call)*:
  - It is honest about the world it's in, so it jumps at a swap (for example about 1,200 km to
    about 300 km). It is not remapped, and it doesn't hide during the cloud.
  - On the globe, "above sea level" uses true land heights, not the drawn exaggerated ones.

### 5.4 Globe flight

- **State** (f64 on the CPU): the fly's position as a direction on the sphere plus altitude, and
  its heading as a tangent-plane frame, not latitude and longitude, so it flies over the poles
  with no special case *(call)*.
- **Range:** from the dive altitude (§7.1) up to where the chase camera is **3 Earth radii**
  from the centre (owner, 2026-11-24: never zoom out past 3R; M1 allowed 6R). At the top of that
  range the view looks like M1's starting orbit view with the fly in the foreground, always
  drawn at its fixed on-screen size. Steering then turns the globe beneath you, as the orbit camera's drag did.
- **Clearance:** the globe's land height at the current time, plus a margin, from the CPU
  height query (spec §11.1).
- **Time:** continuous on the globe, as in M1; scrubbing animates plate motion beneath the fly.
  The fly keeps its era direction (it does not ride a plate) *(call)*.
- **Relief flattens on the way down** (owner, 2026-11-24):
  - The globe draws relief at 20× true height (`ELEVATION_EXAGGERATION`). Region geometry is
    true height, with only its shading boosted.
  - The globe's exaggeration eases from 20× at orbit toward the region's look as the fly nears
    the cloud top, so the mountains above the cloud match the ones below it.
  - The curve and the end value are *(call: true height, or close to the region's 1.8× shading
    boost, by the cloud top)*.
  - The globe's clearance query uses the same exaggeration as the drawn surface at that
    altitude, so the fly never passes through a drawn mountain.
- **Rendering closer:** the bridge cameras follow the chase camera. The globe's meshes and shells
  must stay correct at the dive altitude: no visible facets on the silhouette and no shell
  (ocean, atmosphere) crossing the land at that range (the M1 chord-sag lesson). Near and far
  planes adapt to altitude.

## 6. The cloud

One effect, used by every transition: the dive and the climb (§7), the region hop (§8), and
snapshot changes inside a region (§9) *(call: the last replaces M2's haze fade, so every change
of world looks alike)*.

- **What the player sees:** wisps stream past and thicken around the fly into a bright,
  sunlit white-out, then thin and part to reveal the other world. The fly stays visible in front
  of the cloud throughout; it is flying through it, not hidden by it *(call)*.
- **How it is drawn** *(call)*: a full-screen pass over the world and under the fly and UI.
  Several layers of the shared WESL noise (M2's `noise.wesl`), scrolled with the camera's motion
  so they parallax like real wisps, lit from the era's sun direction with the painterly ramp,
  and tinted by the era's sky. Not a volumetric ray march. It costs nothing when idle and one
  pass while active.
- **One parameter drives it:** cloud density from 0 (clear) to 1 (opaque), animated by the
  transition. The swap happens only on frames at density 1.
- **Holding:** while waiting for a region, the cloud stays at density 1 with wisps still
  moving and the fly still flapping, and the fly's motion eases to a gentle drift
  (spec §11.2's "gentle slowdown"). "Descending..." appears after 1 s, as in M2.
- **Tilt-shift:** a light tilt-shift during transitions only (spec §9's call), if it fits the
  budget. *(Dropped in Task 11: the pass was over budget; see the plan's rulings.)*

## 7. The dive and the climb (globe ↔ region)

### 7.1 Down

1. **Flying down** over the globe, the cloud starts to gather around the fly below a cloud-top
   altitude, thickening as it descends *(call: about 2,500 km)*.
2. **At the dive altitude** *(call: about 1,200 km)* the dive commits: the dive point is the
   ground point under the fly at that moment; the time snaps to the nearest snapshot (spec
   §11.2, M2 §6.1); the crust position resolves at that snapshot; `RegionRequested` is sent. The
   cloud reaches density 1.
3. **Inside the cloud** the fly keeps descending at an eased rate; the view is white-out.
4. **When the region is ready**, the views swap behind the cloud. The fly appears below the
   region's cloud base (§7.3), over the dive point (the region's centre), with its heading
   carried from the globe into the region's north-up era frame.
5. **The cloud thins** as the fly descends out of it, and the region takes over.

If the fly climbs back above the cloud top before the swap, the dive cancels: the request is
dropped and the cloud clears over the globe *(call)*.

The swap compresses altitude (about 1,200 km on the globe side, the region's cloud base on the
other). The opaque cloud hides it; the acceptance test is that the land just above and just
below the cloud reads as the same place (§10).

### 7.2 Up

- Climbing through the region's cloud base (or `Backspace`) gathers the cloud. At density 1 the
  view swaps to the globe with the fly at the dive altitude over the fly's crust position at the
  current snapshot, heading carried into the globe's tangent frame. The cloud thins as the fly
  climbs out over the globe. The globe is always loaded, so this never waits.
- `RegionLeft` is written at the swap, as now.

### 7.3 Regions from higher up

- **Ceiling:** the region's top rises from 60 km to its **cloud base** *(call: about 300 km)*.
  Between 60 km and the cloud base the fly sees the region's 2,400 km surroundings (the apron)
  at about 4.7 km a sample, with the detailed 300 km centre beneath it.
- **Curvature:** M2 already drops terrain by the sphere's sag (`d² / 2R`). M3 checks that the
  drop is applied consistently to terrain, water and the clearance queries, and that it still
  holds when seen from the cloud base.
- **Haze:** atmospheric perspective scales with altitude so the apron's edge is never visible:
  it fades into haze and sky before the edge at every allowed altitude and pitch.
- **Render settings** that assumed ≤ 60 km (far plane, shadow cascades, fog ranges, CDLOD
  ranges) scale with altitude. The region benches gain a high-altitude segment (§10).

## 8. The region hop (region ↔ region)

1. **Preload:** when the fly is within a preload distance of the inner field's edge and heading
   outward, the neighbour starts generating *(call)*. The preload distance is the larger of:
   - 50 km;
   - the distance the fly covers at its current speed in the platform's expected generation
     time, so that faster flight starts the preload earlier.

   The neighbour starts
   generating. Its centre is ahead of the fly along its heading, snapped to the shared lattice,
   so the fly arrives well inside it with room to continue *(call: about 100 km past the
   current edge)*. One preload at a time. Turning away cancels it; a new heading retargets it
   *(call)*.
2. **The lift:** as the fly reaches the edge, it is lifted gently to about **60 km** (owner), and
   the cloud bank gathers around it. A fly already above 60 km is not lifted; the cloud forms
   at its height *(call)*. The player cannot skip the lift.
3. **The swap:** at density 1, if the neighbour is ready, swap. If not, hold in the cloud (§6).
4. **After:** the cloud thins. The fly eases back down to the height above ground it had before
   the lift, unless the player is steering vertically *(call)*.

- **Same era:** the neighbour is generated at the current snapshot.
- **Crust positions:** the neighbour's centre is resolved like a dive point (an era direction
  to a crust position at the snapshot). Era-fixed points stay explorable (spec §5.2).
- **Memory:** two regions' data are resident only from the preload to the swap. Estimated peak:
  about 10 MiB of grid data, plus about 10 MiB of GPU textures if uploaded before the swap
  (recommended, so the swap frame does not stall) *(call)*. Source tiles mostly overlap and
  stay within the existing 96-tile cache cap. On the web, wasm memory grows once by the extra
  grid and stays flat across later hops. *(Measured in M3, natively: the swap's upload adds
  about +22 MiB for two or three frames (staging, and the new textures beside the old), and the
  first hop of a run up to +37.5 MiB; the generator's lattice, 19.5–24.5 MiB, now reuses the last
  region's pages. See `docs/testing/m3/performance.md`, "Hop memory".)*
- **Web frame budget:** a preload generates while the player flies, so its slices must not break
  the flight gates *(call: a smaller per-frame budget during flight, the full 12 ms budget while
  the cloud is opaque)*. A longer hold in the cloud is acceptable; dropped frames are not.
  **Dropped frames are the blocker** *(call, following the owner's stated view that sustained
  flight frame rate matters most)*. The web's cloud-hold time is reported against its 8 s
  target but does not fail the bench on its own.

## 9. Time during transitions

- **On the globe:** the slider is continuous (M1).
- **Entering a region** snaps the time at the dive commit (§7.1).
- **In a region:** the slider snaps to snapshots; a change regenerates around the fly's crust
  position, keeping its height above ground, behind the cloud (§6) instead of M2's haze fade.
  If that crust doesn't exist in the new era, the fly climbs out to the globe (M2 §6.2).
- **During a transition** (cloud gathering, opaque or thinning), slider changes wait until the
  transition ends, then apply *(call)*.

## 10. Verification

The spec's §15 applies: native build and `cargo check --target wasm32-unknown-unknown`; render
natively on Metal and in Chrome WebGPU on the development Mac; the owner runs the CI-built Linux
packages on Vulkan. Record the adapter and backend.

**Automated tests** (pure functions in `timefly_core` where possible):
- the chase camera's placement keeps the fly's on-screen size within tolerance across heights,
  and the camera above the ground over a steep ridge
- globe flight crosses a pole without a jump in heading or position
- the dive decision: commit at the dive altitude, cancel above the cloud top, no swap before
  density 1, hold while not ready
- the climb carries the crust position and heading back to the globe within tolerance
- the hop: preload trigger and cancel, the neighbour's centre ahead of the fly and inside the
  lattice rules, lift and return height, no swap before density 1
- transitions queue slider changes
- the region seen from its cloud base never shows the apron's edge (a projection test over
  allowed pitches and altitudes)

**Benches** *(call)*:
- `--region-bench` keeps its eight regions and gates. Its scripted flight drives the fly
  instead of the free camera, and it gains a high-altitude segment near the cloud base.
- A new **journey bench** (`--journey-bench` / `?journey-bench`): from the globe, dive into one
  of the bench regions, fly to the edge and hop twice, then climb back out, several times.
  Gates *(call; the owner sets final numbers in the plan)*:
  - time spent holding in the cloud per transition: native ≤ 3 s (gated); web ≤ 8 s (reported
    only, §8)
  - flight frame times outside the cloud at the M2 gates, including while a preload generates
  - the worst frame during a swap (native ≤ 100 ms), reported separately in line with the
    owner's view on one-off hitches
  - peak memory during a hop: native ≤ +40 MiB over the in-region baseline; wasm linear memory
    flat across hops after the first. *(Raised to +64 MiB natively by the owner, 2026-11-25,
    on the measured breakdown in `docs/testing/m3/performance.md`, "Hop memory": the first hop
    of a run reads up to +37.5 MiB, and with no page reuse the parts add up to about +44 MiB.)*
  - memory returning to its first-cycle level across repeated journeys (as M2's cycles)
- `--region=LAT,LON,AGE` still starts in a region (with the fly at M2's start pose) for testing.

**Manual checks** (the owner, on the Mac): the dive and climb at a recent and a deep era; a hop
across a coastline; flying over a pole; finding the fly at every height; a slider change mid-hop;
and no motion discomfort.

## 11. Suggested order inside the milestone *(call)*

1. The fly and chase camera in regions (replace `fly_camera.rs`; benches still pass).
2. Globe flight (replace `camera.rs`; the globe closer).
3. The cloud effect, first on snapshot changes (replacing the haze fade).
4. The dive and the climb, with the higher region ceiling, curvature and haze.
5. The region hop with preloading.
6. The journey bench and the verification runs.

## 12. Left to the builder

The fly's modelling and animation, exact altitudes and distances (the cloud top, dive altitude,
region cloud base, preload distance), the cloud's look within §6, the camera smoothing, how the
region's curvature is applied, the web frame budget during preloads, and module layout (the
spec's `WorldTreePlugin` and `CreaturePlugin` are the suggested homes). Record changes to any
*(call)* in the plan with a reason.
