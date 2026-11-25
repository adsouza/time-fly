# Milestone 3: The Fly and Zoom-and-Swap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a dragonfly that flies from orbit down through a cloud into a region, across regions via cloud-bank hops at 60 km, and back up to the globe, replacing M1's orbit camera and M2's double-click / `Backspace` stand-in.

**Architecture:** the flight, chase-camera, cloud-density and altitude rules are pure functions in a new `timefly_core::flight` module. The shell keeps M2's gather → decide → apply navigation (`region/navigate.rs`, `step_nav`), which grows new phases (dive, emerge, climb, hop) and drives one cloud-density value instead of the haze overlay's alpha. One dragonfly entity and one full-screen cloud entity live on their own render layers, seen by both the globe camera and the region camera. Only one of the two cameras is active at a time, as in M2. Neighbour regions generate into a pending slot without replacing `ActiveRegion` until the swap.

**Tech Stack:** Rust, Bevy 0.20.1, WESL shaders, `timefly_core` (pure, f64 geography), wasm32 WebGPU and native Metal and Vulkan.

**Spec:** `docs/superpowers/specs/2026-11-24-milestone-3-zoom-and-swap-design.md` (the "design"). Its parent spec is `docs/superpowers/specs/2026-11-21-time-fly-design.md`; M2 is `docs/superpowers/specs/2026-11-23-milestone-2-region-worlds-design.md`.

## Global Constraints

- Bevy 0.20.1 and WESL. Stay within the WebGPU ceiling: no mesh shaders, no `MeshletMesh`, no `std::thread` in core loops, no 64-bit atomics.
- f32 inside worlds. f64 on the CPU for geography and globe flight.
- Functional core, imperative shell. Every transition decision is a pure function, tested without a window.
- Domain events are Bevy messages registered with `add_domain_event`. States use `add_logged_state`.
- Only one world is drawn at a time. A swap happens only on a frame where cloud density is exactly `1.0`.
- The fly appears in every era. Never gate it by era (design §1.1).
- **Altitudes and distances**, all *(call)*, defined once in `timefly_core::flight::altitudes`:
  - `GLOBE_MAX_CAMERA_DISTANCE_KM = 3.0 * EARTH_RADIUS_KM` (owner: never zoom out past 3R)
  - `GLOBE_MAX_ALTITUDE_KM = 1.3 * EARTH_RADIUS_KM`, the fly's ceiling. It is tuned so the chase eye stays within `GLOBE_MAX_CAMERA_DISTANCE_KM`, and Task 4's test pins it.
  - `GLOBE_START_ALTITUDE_KM = GLOBE_MAX_ALTITUDE_KM`
  - `CLOUD_TOP_KM = 2_500.0`
  - `DIVE_ALTITUDE_KM = 1_200.0`
  - `GLOBE_EXAGGERATION_FULL_KM = 6_000.0`
  - `GLOBE_EXAGGERATION_AT_CLOUD = 2.0`
  - `REGION_CLOUD_BASE_M = 300_000.0`
  - `REGION_CLOUD_BAND_M = 60_000.0`
  - `REGION_EMERGE_TO_M = 230_000.0`
  - `HOP_ALTITUDE_M = 60_000.0`
  - `PRELOAD_MIN_M = 50_000.0`
  - `NEIGHBOUR_PAST_EDGE_M = 100_000.0`
  - `LIFT_LEAD_M = 20_000.0`
  - `EXPECTED_GEN_S_NATIVE = 3.0`
  - `EXPECTED_GEN_S_WEB = 8.0`
- **Speeds:**
  - Regions keep `fly_speed` and its 10 km/s cap (`MAX_SPEED_MPS`).
  - On the globe, speed is `0.8 × altitude` per second, with the ground track's angular speed capped at `GLOBE_MAX_TURN_DEG_PER_S = 30.0`.
- **Chase camera** *(call)*:
  - `CHASE_DISTANCE_PER_AGL = 0.5`
  - `CHASE_MIN_DISTANCE_M = 60.0`
  - `CHASE_ELEVATION_DEG = 20.0`
  - `CHASE_GLOBE_TOP_PITCH_DEG = 70.0` (the look-down pitch at `GLOBE_MAX_ALTITUDE_KM`)
  - `FLY_SCREEN_FRACTION = 0.05` (5% of the view's height)
- **Cloud timings** *(call)*:
  - `CLOUD_GATHER_S = 0.6`
  - `CLOUD_CLEAR_S = 1.2`
  - `EMERGE_S = 2.0`
- **Web budgets:**
  - `WEB_FLIGHT_GEN_BUDGET_MS = 3.0` while a preload runs during flight
  - `WEB_GEN_LARGE_BUDGET_MS` (12 ms, existing) while the cloud is opaque
- **Toolchain:** `PATH="/opt/homebrew/opt/rustup/bin:$PATH"` and `CARGO_TARGET_DIR=/Users/adsouza/Code/time-fly/target`, which is shared with other worktrees.
  - Before any build whose artefact you test, run `touch crates/timefly/src/lib.rs crates/timefly_core/src/lib.rs`.
  - Prove a web or windowed artefact is this branch's with `grep -a -c '<a string only this branch has>'`.
- **Repo rules:**
  - Stage explicit paths. Never `git add -A`.
  - Never push or merge.
  - Commits use the conventional style and end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Windowed runs:**
  - Use a fresh `TIMEFLY_LOG_DIR="$(mktemp -d)"` per run.
  - Never kill the windowed app with a timeout.
  - Measure first-run worst frames twice, because of the Metal shader cache.
- **Per-task gate:** `cargo test -q --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo check -p timefly --target wasm32-unknown-unknown`.

## Review Focus

1. **Crossing a pole on the globe:** heading and position stay continuous with no flip. Task 2, `crosses_the_pole_without_a_flip`.
2. **Turning back mid-dive or mid-hop:** the cloud clears, `ActiveRegion` survives a cancelled hop, and nothing is left stuck at density 1. Tasks 7 and 9, `climbing_out_of_the_cloud_cancels_the_dive` and `turning_back_cancels_the_preload_and_keeps_the_region`.
3. **A slider change during any transition** waits until it ends and then applies once. Task 7, `slider_changes_wait_for_the_transition`.
4. **A neighbour that fails or times out** (missing tiles, load timeout, invalid crust): the fly stays in its current region, the cloud clears, and it is turned back inside the edge. It is never stranded in white-out. Task 9, `a_failed_neighbour_returns_the_fly_inside_the_edge`.
5. **Portrait and resized windows:** the fly stays in view at `FLY_SCREEN_FRACTION` within tolerance when the globe camera widens its fov for portrait (`fit_globe_projection`). Task 4, `the_fly_stays_in_view_in_portrait`.

---

### Task 1: Flight altitudes, cloud density and the chase camera (core)

**Files:**
- Create: `crates/timefly_core/src/flight/mod.rs`, `flight/altitudes.rs`, `flight/cloud.rs`, `flight/chase.rs`
- Modify: `crates/timefly_core/src/lib.rs` (add `pub mod flight;`)
- Test: inline `#[cfg(test)]` modules in each file

**Interfaces:**
- Produces:
  - The constants in Global Constraints, all `pub const`, in `flight::altitudes`.
  - `flight::cloud::globe_density(altitude_km: f64) -> f32`
    - 0 at `CLOUD_TOP_KM` and above, 1 at `DIVE_ALTITUDE_KM` and below, smoothstep between.
  - `flight::cloud::region_density(height_m: f32) -> f32`
    - 0 at `REGION_CLOUD_BASE_M - REGION_CLOUD_BAND_M` and below, 1 at `REGION_CLOUD_BASE_M` and above, smoothstep between.
  - `flight::cloud::globe_exaggeration(altitude_km: f64) -> f64`
    - Returns `ELEVATION_EXAGGERATION` (20) at `GLOBE_EXAGGERATION_FULL_KM` and above, and `GLOBE_EXAGGERATION_AT_CLOUD` at `CLOUD_TOP_KM` and below.
    - Between those, smoothstep in `ln(altitude)`.
  - `flight::chase::ChasePose { eye: DVec3, target: DVec3, up: DVec3 }`
  - `flight::chase::chase_pose(fly: DVec3, forward: DVec3, up: DVec3, agl: f64, look_pitch_deg: f64) -> ChasePose`
    - All inputs are in one world's units (m in regions, km on the globe).
    - The eye sits `max(CHASE_MIN_DISTANCE_M in units, CHASE_DISTANCE_PER_AGL × agl)` behind the fly, raised by `CHASE_ELEVATION_DEG`.
    - It looks down through the fly by `look_pitch_deg`.
  - `flight::chase::lift_above_ground(pose: ChasePose, ground_at: impl Fn(DVec3) -> f64, clearance: f64) -> ChasePose`
    - When the eye would be below `ground + clearance`, the eye rises and the distance shortens, keeping the target.
  - `flight::chase::fly_scale(eye_to_fly: f64, vfov_rad: f64) -> f64`
    - The fly model's uniform scale that makes its 1-unit body span `FLY_SCREEN_FRACTION` of the view's height.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn globe_density_is_clear_above_the_cloud_top_and_opaque_at_the_dive() {
    assert_eq!(globe_density(CLOUD_TOP_KM + 1.0), 0.0);
    assert_eq!(globe_density(DIVE_ALTITUDE_KM), 1.0);
    let mid = globe_density((CLOUD_TOP_KM + DIVE_ALTITUDE_KM) / 2.0);
    assert!(mid > 0.0 && mid < 1.0);
}
#[test] fn region_density_is_opaque_at_the_cloud_base() {
    assert_eq!(region_density(REGION_CLOUD_BASE_M as f32), 1.0);
    assert_eq!(region_density((REGION_CLOUD_BASE_M - REGION_CLOUD_BAND_M) as f32), 0.0);
}
#[test] fn exaggeration_eases_from_twenty_to_the_cloud_value() {
    assert_eq!(globe_exaggeration(GLOBE_MAX_ALTITUDE_KM), ELEVATION_EXAGGERATION);
    assert_eq!(globe_exaggeration(CLOUD_TOP_KM), GLOBE_EXAGGERATION_AT_CLOUD);
    // monotonic over 100 log-spaced altitudes between the two
}
#[test] fn the_chase_eye_is_behind_and_above_the_fly() { /* forward = -Z: eye.z > fly.z, eye.y > fly.y; distance = 0.5 × agl */ }
#[test] fn the_fly_keeps_its_screen_size_across_heights() {
    // for agl in [150 m, 8 km, 60 km, 300 km]: the projected height of a body of fly_scale(..)
    // at the pose's eye distance is FLY_SCREEN_FRACTION ± 1% of the view
}
#[test] fn the_eye_rises_over_a_ridge_behind_the_fly() {
    // ground_at = 2 km ridge behind the fly, fly 500 m above flat ground ahead:
    // lifted eye >= ridge + clearance, and target unchanged
}
```

- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly_core flight::`. Expected: they fail to compile, because `flight` doesn't exist yet.
- [ ] **Step 3: Implement the module.** Write the constants, the three density and exaggeration functions, and `chase_pose`, `lift_above_ground` and `fly_scale` as specified. Use a shared `smoothstep(e0, e1, x)` helper.
- [ ] **Step 4: Run the tests to check they pass.** Run `cargo test -p timefly_core flight::`. Expected: all pass.
- [ ] **Step 5: Run the per-task gate, then commit.**

```bash
git add crates/timefly_core/src/lib.rs crates/timefly_core/src/flight
git commit -m "feat(core): flight altitudes, cloud density and chase camera"
```

### Task 2: Globe flight (core)

**Files:**
- Create: `crates/timefly_core/src/flight/globe.rs`
- Test: inline

**Interfaces:**
- Consumes: Task 1 constants; `EARTH_RADIUS_KM`.
- Produces:
  - `GlobeFlight { dir: DVec3, heading: DVec3, altitude_km: f64 }`
    - Render-frame unit position, unit tangent heading. `#[derive(Copy, Clone, Debug, PartialEq)]`.
  - `GlobeFlight::start(lat_deg: f64, lon_deg: f64) -> Self`, at `GLOBE_START_ALTITUDE_KM`, heading north.
  - `GlobeFlight::position_km(&self) -> DVec3`
  - `GlobeFlight::up(&self) -> DVec3`
  - `GlobeInput { forward: f64, strafe: f64, turn_rad: f64, climb: f64, boost: bool, dt_s: f64 }`
    - Axes in `-1..=1`; `turn_rad` already includes drag.
  - `globe_fly_step(state: GlobeFlight, input: GlobeInput) -> GlobeFlight`
    - Moves along the great circle given by the heading, then re-orthonormalises the heading.
    - Altitude speed is `0.8 × altitude` per second.
    - Ground speed is the same, capped so the angular rate is ≤ `GLOBE_MAX_TURN_DEG_PER_S`.
    - Boost ×4 as in M2.
    - Altitude is clamped to `[DIVE_ALTITUDE_KM * 0.5, GLOBE_MAX_ALTITUDE_KM]`. The lower half is the in-cloud range, which the shell eases (Task 7).
  - `heading_to_region_yaw(dir: DVec3, heading: DVec3, north: DVec3, east: DVec3) -> f32`
  - `region_yaw_to_heading(yaw: f32, north: DVec3, east: DVec3) -> DVec3`
    - These carry heading across the swap. Yaw follows `FlyState`'s convention: 0 faces −z (north), positive turns left. Check this against `fly_camera.rs`.

- [ ] **Step 1: Write the failing tests:**
  - `crosses_the_pole_without_a_flip`: start at 89° N heading north and step 200 times at `dt` 1/60. Latitude must pass 90° and descend. The heading's dot with the previous heading must stay > 0.99 every step.
  - `forward_moves_along_the_heading_and_keeps_altitude`
  - `the_ground_track_turns_at_most_thirty_degrees_a_second`: at `GLOBE_MAX_ALTITUDE_KM` with boost, the angle between `dir`s one second apart is ≤ 30° + 1e-6.
  - `altitude_speed_scales_with_altitude`
  - `heading_and_yaw_round_trip`: 32 random headings at random dirs, using a region frame basis built from `dir`. The error must be < 1e-6 rad.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly_core flight::globe`. Expected: they fail to compile.
- [ ] **Step 3: Implement** `GlobeFlight`, `globe_fly_step` and the yaw conversions.
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Run the gate and commit:** `feat(core): globe flight over the sphere`.

### Task 3: The dragonfly and the chase camera in regions

**Files:**
- Create: `crates/timefly/src/creature/mod.rs` (`CreaturePlugin`), `creature/mesh.rs`, `assets/shaders/dragonfly.wesl`
- Modify:
  - `fly_camera.rs`: `FlyState` becomes the **fly's** pose. `apply_fly` writes the chase pose to the `RegionCamera` and the fly's transform to the `Dragonfly`.
  - `ui/altitude.rs`: read the fly's altitude.
  - `ui/mod.rs`
  - `lib.rs`: add `CreaturePlugin` after `FlyCameraPlugin`.
- Test: `creature/mesh.rs` inline, `fly_camera.rs` tests, `ui/altitude.rs` tests, `tests/region_bench.rs`

**Interfaces:**
- Consumes: Task 1 `chase_pose`, `lift_above_ground`, `fly_scale`.
- Produces:
  - `creature::Dragonfly` (marker component) and `creature::FLY_LAYER: usize` (a new render layer, added to both the `RegionCamera` and `GlobeCamera` layers).
  - `creature::FlyPose { translation: Vec3, rotation: Quat, scale: f32, bank_rad: f32 }`, a resource written by whichever world is active.
  - `creature::dragonfly_mesh() -> Mesh`: a body of 1 unit length plus four wing quads with a `wing` vertex attribute (0 body, 1 wing) for the flap.
  - `ui::altitude::FlyAltitude(pub Option<Altitude>)`, a resource with `enum Altitude { Metres(f32), Kilometres(f64) }`. The overlay shows it instead of reading camera transforms.
  - `FlyState` keeps its fields and convention (`pos_m`, `yaw`, `pitch`). `pitch` now tilts the view, not the fly's flight path.

- [ ] **Step 1: Write the failing tests:**
  - `the_dragonfly_mesh_has_a_body_and_four_wings`: wing-vertex count > 0, four connected wing components, body length 1 ± 0.05.
  - `the_region_camera_chases_the_fly`: in a `MinimalPlugins` app with a fake grid (the `navigate/tests.rs` helpers), after `apply_fly`, the camera is behind and above `FlyState.pos_m` and looks at it.
  - `the_fly_never_hides_behind_a_ridge`: a grid with a ridge behind the fly. The camera stays above `ground + CAMERA_CLEARANCE_M`.
  - `the_overlay_shows_the_flys_altitude`: replaces the camera-based case in `ui/altitude.rs`.
  - The existing `fly_camera` tests stay green: clearance, ceiling, substeps, arrow keys.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib creature:: fly_camera:: ui::altitude`. Expected: they fail to compile or fail.
- [ ] **Step 3: Implement.**
  - **Mesh:** a slim tapered body of about 7 segments, with wings as quads with a vertex-colour iridescent blue-green.
  - **`dragonfly.wesl`:**
    - The wing flap rotates wing vertices about the body axis by `sin(globals.time * 2π * 25) * 0.6`, which reads as a blur.
    - Wings use `AlphaMode::Blend` at 0.45 alpha. The body is opaque.
    - Lit by the scene's directional light.
  - **Bank:** proportional to the yaw rate, smoothed with a 0.25 s time constant.
  - **Hover:** with no input, a 0.3 Hz idle bob of 0.02 × scale.
  - **Chase camera:** `apply_fly` uses `chase_pose(pos, forward, Y, agl, CHASE_ELEVATION_DEG + pitch)` and then `lift_above_ground`, smoothed critically damped with a 0.2 s time constant. The fly's scale is `fly_scale(...)` at `REGION_FOV_DEG`.
  - **The step itself:** `fly_step` keeps moving `FlyState`. Only the camera placement changes.
  - **Overlay:** the altitude overlay reads `FlyAltitude`. The region writes `Metres(FlyState.pos_m.y)`.
- [ ] **Step 4: Run the tests to check they pass,** plus `cargo test -p timefly --test region_bench`.
- [ ] **Step 5: Windowed check.**
  - Run `TIMEFLY_LOG_DIR="$(mktemp -d)" cargo run --release -p timefly -- --region=46.5,10.5,0`.
  - Screenshot the fly at 150 m, 8 km and 60 km. It should be the same size on screen and below the view's centre.
- [ ] **Step 6: Run the gate and commit:** `feat: the dragonfly and its chase camera in regions`.

### Task 4: Globe flight replaces the orbit camera

**Files:**
- Create: `crates/timefly/src/globe_flight.rs` (`GlobeFlightPlugin`, `GlobeFlightSystems`)
- Modify:
  - `camera.rs`: delete `orbit_step`, `OrbitState` and `OrbitCameraPlugin`. Keep `fit_globe_projection` and move it into `globe_flight.rs`.
  - `globe/material.rs`: `GlobeParams.shape.y` = `globe_exaggeration(altitude)` every frame.
  - `region/navigate.rs`: replace `OrbitState` uses with `GlobeFlight`. This is temporary: the double-click entry still works, and its descent eases `GlobeFlight.altitude_km` toward `DIVE_ALTITUDE_KM`.
  - `bench/region/mod.rs`: `pan_orbit` becomes `pan_globe(from, to, altitude_km, t_s) -> GlobeFlight`.
  - `examples/ui_tour.rs`
  - `lib.rs`
- Test: `globe_flight.rs` inline, `region/navigate/tests.rs`

**Interfaces:**
- Consumes: Task 2 `GlobeFlight` and `globe_fly_step`; Task 1 `chase_pose`, `fly_scale`, `globe_exaggeration`; Task 3 `FlyPose`, `FLY_LAYER`, `FlyAltitude`.
- Produces:
  - `GlobeFlight` as a Bevy `Resource`, through a newtype `GlobeFlightState(pub GlobeFlight)` in the shell.
  - `GlobeFlightSystems { Step, Apply }`. `Apply` writes the `GlobeCamera` transform before `GlobeCameraSync`, as `OrbitCameraSystems` did.
  - `globe_look_pitch_deg(altitude_km: f64) -> f64`: `CHASE_ELEVATION_DEG` at `DIVE_ALTITUDE_KM`, rising to `CHASE_GLOBE_TOP_PITCH_DEG` at `GLOBE_MAX_ALTITUDE_KM`, smoothstep in `ln`.
  - `DiagnosticsContext.camera_distance_km` and `.camera_direction` are still written.

- [ ] **Step 1: Write the failing tests:**
  - `the_globe_camera_chases_the_fly`: the eye is outside the fly's altitude, and the globe centre is in front of the camera at `GLOBE_MAX_ALTITUDE_KM`.
  - `the_camera_never_goes_past_three_radii`: for 64 headings and look pitches at `GLOBE_MAX_ALTITUDE_KM`, `|eye| <= GLOBE_MAX_CAMERA_DISTANCE_KM`. If it fails, lower `GLOBE_MAX_ALTITUDE_KM` and ledger the new value.
  - `the_exaggeration_uniform_follows_altitude`: `shape.y` equals `globe_exaggeration(alt)` after one update.
  - `the_fly_stays_in_view_in_portrait` (Review Focus 5): with a 600×1000 window, the fly's projected centre lies inside the view and its height is `FLY_SCREEN_FRACTION` ± 1% of the view.
  - `keys_fly_the_globe`: pressing `W` for 1 s moves `dir` along the heading.
  - Existing navigate tests keep passing with `GlobeFlight` in place of `OrbitState`.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib globe_flight:: region::navigate`.
- [ ] **Step 3: Implement.**
  - **Input:** gather it with M2's key mapping (`keys_to_input`) and drag.
  - **Chase camera:** the globe chase is in km, using `chase_pose` with `globe_look_pitch_deg`.
  - **Fly:** write `FlyPose` scaled by `fly_scale` at the globe camera's vfov. On the globe, `FlyAltitude` = `Kilometres(altitude_km)`, using true heights (no exaggeration).
  - **Rendering:** add `FLY_LAYER` to the `GlobeCamera`'s layers. The bridge cameras need nothing new, because the fly is drawn by the main globe camera over the composite.
- [ ] **Step 4: Run the tests to check they pass,** plus `cargo test -p timefly --test region_bench --test scrub_bench`.
- [ ] **Step 5: Windowed check at the dive altitude, at present and at 250 Ma.**
  - Fly down to 1,250 km. Screenshot.
  - Pass: no visible facets on the silhouette, and no ocean or atmosphere shell crossing land.
  - If a shell crosses, raise `SHELL_SUBDIVISIONS` (`globe/mod.rs`) until it doesn't, and record the value in the ledger.
- [ ] **Step 6: Run the gate and commit:** `feat: fly the dragonfly over the globe`.

### Task 5: The cloud effect

**Files:**
- Create: `crates/timefly/src/cloud.rs` (`CloudPlugin`, `CloudMaterial`, `CloudDensity`), `assets/shaders/cloud.wesl`
- Modify:
  - `region/navigate.rs`: `FadeOverlay` is removed. Its alpha (`phase.overlay_alpha()`) now writes `CloudDensity`.
  - `terrain/view.rs` and `globe/mod.rs`: add `CLOUD_LAYER` to both cameras.
  - `lib.rs`
- Test: `cloud.rs` inline, `region/navigate/tests.rs`

**Interfaces:**
- Consumes: `globe::mesh::fullscreen_triangle()`, `noise.wesl` `gradient_noise3`, `FlyPose`, the sun direction (`sky.rs` / `sync_sun`).
- Produces:
  - `CloudDensity(pub f32)`, a resource clamped to `0..=1`. The single input every transition writes, taking the max of its sources per frame.
  - `CloudSystems`: runs after `NavigationSystems` and before rendering extraction.
  - `CLOUD_LAYER: usize`.

- [ ] **Step 1: Write the failing tests:**
  - `the_cloud_is_hidden_when_clear`: density 0 means the entity is `Visibility::Hidden`.
  - `the_cloud_draws_just_beyond_the_fly`: the uniform's `fly_view_depth` equals the eye-to-fly distance × 1.2.
  - `the_snapshot_change_uses_the_cloud`: in `navigate/tests.rs`, during `ChangingSnapshot` `CloudDensity` peaks at 1.0 at the swap frame, and no `FadeOverlay` entity exists.
- [ ] **Step 2: Run them to check they fail.**
- [ ] **Step 3: Implement.**
  - **The pass:** the full-screen triangle with `CloudMaterial` (`AlphaMode::Blend`, depth test on, no depth write, `cull_mode = None`).
  - **Depth:** the vertex shader writes the clip depth for view depth `fly_view_depth`, so the fly, which is nearer, is drawn over the cloud and the world behind it is covered.
  - **Look:** the fragment shader draws 3–4 octaves of `gradient_noise3`. Offset them by the camera's accumulated motion (a uniform), at a different rate per octave for parallax. Light them with the painterly ramp from the sun direction and tint them by the era's sky colour.
  - **Opacity:** alpha = `density` remapped so 1.0 is fully opaque at every pixel. Assert this in the shader with `max(alpha, step(0.999, density))`.
  - **Tilt-shift** (design §6, optional): a light blur of the cloud's own top and bottom bands while density is between 0.05 and 0.95, inside the same pass. Drop it, with a ledger ruling, if the pass costs more than 1 ms at 2560×1440 natively.
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Windowed check.**
  - Change the snapshot in the Alps with `]`. Screenshot at density 0.5 and 1.0.
  - Pass: wisps are lit from the sun's side, the fly stays visible, and the view is fully covered at 1.0.
- [ ] **Step 6: Run the gate and commit:** `feat: the cloud transition effect`.

### Task 6: Regions seen from higher up

**Files:**
- Modify:
  - `fly_camera.rs`: `CAMERA_CEILING_M` becomes `REGION_CLOUD_BASE_M`, and the ceiling ease band becomes `REGION_CLOUD_BAND_M`.
  - `terrain/view.rs`: far plane, `SHADOW_DISTANCE_M` and the fog ranges scale with the fly's height.
  - `terrain/mod.rs`: CDLOD ranges if they assume ≤ 60 km.
  - `sky.rs` / `water.rs` if they assume ≤ 60 km.
  - `bench/region/path.rs`: add a high-altitude segment.
- Test: `terrain/tests.rs`, `bench/region/path.rs` tests

**Interfaces:**
- Produces:
  - `view_ranges(height_m: f32) -> ViewRanges { far_m: f32, shadow_m: f32, fog_start_m: f32, fog_end_m: f32 }` in `terrain/view.rs`. It is pure. It must give M2's values exactly at ≤ 60 km.
  - The curvature-drop audit: terrain, water and the clearance queries agree.

- [ ] **Step 1: Write the failing tests:**
  - `view_ranges_match_m2_below_sixty_km`
  - `the_apron_edge_is_never_visible`: for heights in `{60, 120, 240, 300}` km and pitches in `[-89°, +10°]`, cast the view's top-edge ray against the curved ground (`curvature_drop_m`). The hit, or the horizon, must lie beyond `fog_end_m` whenever it falls past the apron edge (`APRON_SIZE_M / 2`), so the edge is always fully fogged.
  - `clearance_matches_the_drawn_ground_under_the_fly`: `blended_height_m` minus `curvature_drop_m(0)` equals the drawn ground at the fly. The drop is measured from the camera, not the fly, so check the camera's offset from the fly too.
  - `the_bench_path_reaches_the_cloud_base_band`: the new segment reaches at least 250 km and stays below `REGION_CLOUD_BASE_M`.
- [ ] **Step 2: Run them to check they fail.**
- [ ] **Step 3: Implement** `view_ranges` and use it in the region view systems. Raise the ceiling.
- [ ] **Step 4: Run the tests to check they pass,** plus `cargo test -p timefly --test region_bench`.
- [ ] **Step 5: Windowed check.** Climb to 290 km over the Alps and Pangaea. Screenshot looking north and looking down. Pass: no apron edge and no shadow seam, and the region reads as curved.
- [ ] **Step 6: Run the native `--region-bench` twice, since the first run warms the shader cache.** Flight gates pass, including the new segment. Record the results in the ledger.
- [ ] **Step 7: Run the gate and commit:** `feat: regions seen from the cloud base`.

### Task 7: The dive and the climb

**Files:**
- Modify:
  - `region/navigate.rs`: split it into `region/navigate/phase.rs` (`NavPhase`, `NavInputs`, `NavAction`, `step_nav`) and `region/navigate/mod.rs` (the shell). The file is 1,113 lines.
  - `region/navigate/tests.rs`
  - `globe_flight.rs`
- Test: `region/navigate/tests.rs`

**Interfaces:**
- Consumes:
  - Tasks 1–2 densities and `GlobeFlight`.
  - Task 5 `CloudDensity`.
  - `RegionFrame::basis()` and `local_to_dir` / `dir_to_local`.
  - `carry_crust_position`, `nearest_snapshot`.
- Produces:
  - **Phases:** `NavPhase` gains:
    - `Diving { target: RenderDir, snapshot: YearsBp, started: bool }`
    - `Emerging { world: World, elapsed_s: f32 }`
    - `Climbing { elapsed_s: f32, centre: RenderDir }`
    - `Globe` is now "flying on the globe".
    - `Descending` and `Entering` are removed. `Leaving` is replaced by `Climbing`.
    - `enum World { Globe, Region }`.
  - **Inputs:**
    - `NavInputs.double_click` is removed.
    - Adds `globe_altitude_km: f64`, `globe_dir: RenderDir`, `globe_heading: DVec3`, `region_height_m: f32` and `climb_key: bool` (`Backspace`).
  - **Actions:** `NavAction` gains:
    - `PlaceInRegion { yaw: f32, height_m: f32 }`
    - `PlaceOnGlobe { dir: RenderDir, heading: DVec3, altitude_km: f64 }`
    - `EaseFly { vertical_mps: f32 }` (the auto-descent or auto-climb while emerging)
    - `CancelRequest`
  - **Density:** `NavPhase::cloud_density(&self, i: &NavInputs) -> f32` replaces `overlay_alpha()`. It is the max of the altitude bands (Task 1) and the phase's own fade.
  - **The queued time:** `Navigation.queued_time: Option<YearsBp>`.

- [ ] **Step 1: Write the failing tests:**
  - `descending_below_the_dive_altitude_commits_once`: `SnapTime(nearest)` and `Request` with `target = globe_dir`, exactly once.
  - `no_swap_before_the_cloud_is_opaque`
  - `waiting_holds_in_the_cloud`: while not entered, density is 1 and `EaseFly` slows to a drift (|v| ≤ 0.05 × the M2 speed at that height).
  - `entering_places_the_fly_below_the_cloud_base_with_its_heading`: `PlaceInRegion.height_m = REGION_CLOUD_BASE_M`, yaw matches `heading_to_region_yaw`, then `Emerging` eases down to `REGION_EMERGE_TO_M` over `EMERGE_S`.
  - `climbing_out_of_the_cloud_cancels_the_dive` (Review Focus 2): above `CLOUD_TOP_KM` before entry, the outcome is `CancelRequest`, the phase is `Globe`, and density returns to 0 within `CLOUD_CLEAR_S`.
  - `climbing_through_the_cloud_base_returns_to_the_globe`: the outcome is `PlaceOnGlobe` at `DIVE_ALTITUDE_KM` over the carried crust position, with the heading converted, then `Emerging{Globe}` climbs to `CLOUD_TOP_KM`.
  - `backspace_climbs_out`: the same as above, from any region height.
  - `slider_changes_wait_for_the_transition` (Review Focus 3): a `time_change` during `Diving`, `Emerging` or `Climbing` is queued and applied once on return to `Flying` or `Globe`.
  - `a_snapshot_with_no_crust_climbs_out`: a snapshot change whose carried crust doesn't exist goes to `Climbing`, then the globe (design §9, M2 §6.2).
  - All of M2's snapshot-change tests still pass, now with `cloud_density`.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --lib region::navigate`.
- [ ] **Step 3: Implement.**
  - Remove the double-click gather, `globe_hit` and `cursor_globe_dir`. Delete them if unused.
  - Apply `PlaceInRegion` / `PlaceOnGlobe` to `FlyState` / `GlobeFlightState`, and write `RegionLeft` at the globe swap, as now.
  - While `Diving`, the globe step's floor is `DIVE_ALTITUDE_KM * 0.5`, eased.
- [ ] **Step 4: Run the tests to check they pass,** plus `cargo test -p timefly --test region_loading --test region_bench`.
- [ ] **Step 5: Windowed check.**
  - Dive into the Himalayas at present and into Pangaea at 250 Ma. Climb out of each.
  - Pass: no visible cut, and the land just above and just below the cloud reads as the same place.
  - Screenshot both sides of each cloud.
- [ ] **Step 6: Run the gate and commit:** `feat: dive into and climb out of regions through the cloud`.

### Task 8: Preloading a neighbour region

**Files:**
- Modify:
  - `region/mod.rs`: `RegionRequested` gains `purpose: RequestPurpose`. Add `PendingRegion` and the messages below.
  - `region/job.rs`: a `Neighbour` result goes to `PendingRegion`. `RegionState` stays `InRegion`. A cancel drops only the job.
  - `region/launch.rs`: `purpose: Enter`.
  - `bench/region/mod.rs`
- Test: `tests/region_loading.rs`, `region/job.rs` inline

**Interfaces:**
- Produces:
  - `enum RequestPurpose { Enter, Snapshot, Neighbour }`. Existing senders use `Enter` or `Snapshot`.
  - `PendingRegion(pub Option<ActiveRegion>)`, a resource.
  - `RegionPreloadCancelled`, a message that drops a `Neighbour` job and leaves `ActiveRegion` and `RegionState` untouched.
  - `RegionSwapRequested`, a message that promotes `PendingRegion` to `ActiveRegion` and writes `RegionEntered { spec }`.
  - `RegionPreloadFailed { reason: String }`, a message for a failed or timed-out neighbour job, logged at `warn`.
  - `gen_budget` takes a third input `flight_preload: bool`. On budgeted (web) runs it returns `WEB_FLIGHT_GEN_BUDGET_MS` when `true` and `!large`.

- [ ] **Step 1: Write the failing tests:**
  - `a_neighbour_generates_without_replacing_the_active_region`: `RegionState` stays `InRegion`, `ActiveRegion` is unchanged until `RegionSwapRequested`, and `PendingRegion` is set when done.
  - `cancelling_a_preload_keeps_the_region`
  - `a_preload_that_times_out_reports_failure` (stalled-tile helper `region_app_with`): `RegionPreloadFailed` is written, and `ActiveRegion` and the state are unchanged.
  - `the_flight_budget_is_smaller_on_the_web`: `gen_budget(base, false, true) == 3 ms` for budgeted runs, and native is unaffected.
  - `a_hop_holds_two_grids_at_most`: after a swap, `PendingRegion` is `None` and the old grid's `Arc` strong count drops to the scene's references only.
- [ ] **Step 2: Run them to check they fail.** Run `cargo test -p timefly --test region_loading` and `cargo test -p timefly --lib region::job`.
- [ ] **Step 3: Implement.** Snapshot-change requests keep today's replace-in-place path. Only `Neighbour` uses the pending slot.
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Run the gate and commit:** `feat: preload a neighbouring region without leaving the current one`.

### Task 9: The region hop

**Files:**
- Modify: `region/navigate/phase.rs`, `region/navigate/mod.rs`, `fly_camera.rs` (remove `edge_nudge` for the inner field; the hop replaces it)
- Create: `crates/timefly_core/src/flight/hop.rs`
- Test: `flight/hop.rs` inline, `region/navigate/tests.rs`

**Interfaces:**
- Consumes: Task 8's messages and `PendingRegion`; Task 1 constants; `RegionFrame`.
- Produces:
  - `hop::preload_distance_m(speed_mps: f32, expected_gen_s: f32) -> f32`: `max(PRELOAD_MIN_M, speed × expected_gen_s)`.
  - `hop::should_preload(pos_m: Vec2, vel_mps: Vec2, speed_mps: f32, expected_gen_s: f32) -> bool`: Chebyshev distance to the inner edge (`EDGE_M` on either axis) ≤ the preload distance, and moving outward on that axis.
  - `hop::neighbour_centre(frame: &RegionFrame, pos_m: Vec2, heading: Vec2) -> RenderDir`: the point `NEIGHBOUR_PAST_EDGE_M` past the inner edge along the heading's dominant outward axis, then `local_to_dir`. No lattice snapping (ruling: any centre agrees on the shared lattice).
  - `hop::carry_pose(old: &RegionFrame, new: &RegionFrame, pos_m: Vec3, yaw: f32) -> (Vec3, f32)`: the position through `local_to_dir` → `dir_to_local`. Yaw comes from a point 1 km ahead carried the same way. Height above ground is kept by the caller.
  - **Phase:** `NavPhase::Hopping { centre: RenderDir, lift_from_agl_m: f32, swapped: bool, elapsed_s: f32 }`.
  - **Actions:** `NavAction::Preload(RenderDir)`, `CancelPreload`, `LiftTo { height_m: f32 }`, `SwapToNeighbour`, `ReturnToAgl { agl_m: f32 }`.

- [ ] **Step 1: Write the failing tests (core):**
  - `preload_starts_earlier_when_flying_faster`
  - `heading_inward_never_preloads`
  - `the_neighbour_lies_ahead_past_the_edge`: the fly at (140 km, 0) heading east gives a neighbour centre whose local x is 250 km ± 1 m.
  - `carrying_the_pose_round_trips`: old → new → old with an error < 1 mm and < 1e-5 rad.
- [ ] **Step 2: Write the failing tests (shell, `navigate/tests.rs`):**
  - `nearing_the_edge_preloads_once`
  - `turning_back_cancels_the_preload_and_keeps_the_region` (Review Focus 2)
  - `reaching_the_edge_lifts_to_sixty_km` (a fly already at 80 km is not lifted)
  - `the_hop_swaps_only_when_opaque_and_ready`
  - `after_the_hop_the_fly_returns_to_its_height_above_ground` (unless climb or descend is held)
  - `a_failed_neighbour_returns_the_fly_inside_the_edge` (Review Focus 4): on `RegionPreloadFailed`, the cloud clears, the yaw is turned 180°, and the fly is 10 km inside the edge.
  - `the_region_bench_path_never_triggers_a_hop`: all `BenchPath` samples stay outside `should_preload` at their speed, with `EXPECTED_GEN_S_NATIVE`.
- [ ] **Step 3: Run them to check they fail.**
- [ ] **Step 4: Implement** `flight/hop.rs` and the `Hopping` phase.
  - The swap applies `SwapToNeighbour` (writes `RegionSwapRequested`) and `carry_pose` on the same frame, behind density 1.
  - `expected_gen_s` is `EXPECTED_GEN_S_WEB` on wasm32 and `EXPECTED_GEN_S_NATIVE` otherwise.
  - The swap uploads the new terrain at the swap frame. Ruling: no pre-upload unless Task 10's swap-frame gate fails.
- [ ] **Step 5: Run the tests to check they pass,** plus `cargo test -p timefly --test region_loading --test region_bench`.
- [ ] **Step 6: Windowed check.** From the Alps, fly east across two hops, and fly across a coastline once. Pass: no visible cut, and the land on both sides of each cloud agrees.
- [ ] **Step 7: Run the gate and commit:** `feat: hop between neighbouring regions through a cloud bank`.

### Task 10: The journey bench

**Files:**
- Create: `crates/timefly/src/bench/journey/mod.rs` (`JourneyBenchPlugin`, `JourneyBenchConfig`), `crates/timefly/tests/journey_bench.rs`
- Modify:
  - `bench/mod.rs`: make `memory`, `header_lines_titled`, `mib` and `max_opt` `pub(crate)`.
  - `args.rs`: `--journey-bench`, and on the web `?journey-bench`.
  - `lib.rs`: add the plugin and `PresentMode::AutoNoVsync` for it.
  - `window_memory.rs`: treat it like `region_bench`.
- Test: `args.rs`, `tests/journey_bench.rs`

**Interfaces:**
- Produces:
  - `LaunchArgs.journey_bench: bool`.
  - `JourneyBenchConfig { cycles: u32 (3), region: BenchRegion (Alps), target_fps, exit_when_done }`.
  - **The script.** It drives inputs, not poses, so the real transitions run:
    1. Wait for the interactive globe.
    2. Fly the globe to above the region and descend to the dive.
    3. Fly east to the edge and hop. Repeat once.
    4. Climb out with `Backspace`.
    5. Repeat per cycle.
  - **Summary lines and gates:**
    - `hold: <transition> <ms>` for each transition. Native ≤ 3,000 ms is gated. Web ≤ 8,000 ms is reported only.
    - `flight: median/p95/worst`, outside the cloud. M2's region gates.
    - `swap worst frame`. Native ≤ 100 ms.
    - `hop memory peak`. Native ≤ in-region baseline + 40 MiB (raised to + 64 MiB after Task 11; see the rulings). Web: wasm linear memory flat across hops after the first (per route after the final review: flat across the hops of a route's repeat visits; a first visit may grow it).
    - `journey memory cycles`. Every cycle within 10% of cycle 1.
    - `result: PASS|FAIL`, with exit codes as in the region bench.

- [ ] **Step 1: Write the failing tests:**
  - `journey_bench_is_a_bare_flag`
  - `journey_bench_on_the_web_in_any_order`
  - `a_journey_run_reaches_the_summary`: headless, on the `tests/assets` tiny data, with `cycles: 1`. Expect the summary lines and a hold line for each of the 4 transitions.
- [ ] **Step 2: Run them to check they fail.**
- [ ] **Step 3: Implement**, reusing `frame_time_stats`, `passes`, `pacing`, `missed_frames` and the memory helpers.
- [ ] **Step 4: Run the tests to check they pass.**
- [ ] **Step 5: Run the gate and commit:** `feat: --journey-bench dives, hops and climbs out`.

### Task 11: Verification and documentation

**Files:**
- Create: `docs/testing/m3/performance.md`, `docs/testing/m3/manual-checks.md`, `docs/testing/m3/*.png`
- Modify: this plan's appendix (rulings), `docs/superpowers/specs/2026-11-21-time-fly-design.md` §11.2 and §14 (the owner-approved changes: sideways hops, the parent not visible past the edge, rivers between M3 and M4).

- [ ] **Step 1: Native runs** on the M4 at 2560×1440: `--region-bench` twice and `--journey-bench` twice. Record the adapter, backend, commit and every summary line.
- [ ] **Step 2: Chrome WebGPU in an Incognito window of the user's Chrome** (not the in-app pane, which throttles rAF): `?region-bench` and `?journey-bench` on a `web-release` build. Prove the build is this branch's by a string grep before running.
- [ ] **Step 3: Check the bench numbers against M2's** in `docs/testing/region-m2/performance.md`. Flight gates hold. Region generation, warm generation and memory cycles show no regression beyond noise.
- [ ] **Step 4: Write `manual-checks.md`** for the owner, from design §10:
  - the dive and the climb at a recent and a deep era
  - a hop across a coastline
  - flying over a pole
  - finding the fly at every height
  - a slider change mid-hop
  - motion comfort
- [ ] **Step 5: Update the parent spec §11.2 and §14** with the owner's decisions (design §1.1, items 6–9).
- [ ] **Step 6: Run the gate and commit:** `docs: M3 verification and spec updates`.

## Appendix: rulings

- **Globe clearance is structural.** The fly cannot be below `DIVE_ALTITUDE_KM` (1,200 km) on the globe outside a dive. Exaggerated relief is at most about 177 km (Everest at 20×). So design §5.4's clearance query, and §5.4's "the clearance query uses the same exaggeration", are satisfied without a CPU globe height query. Cost if wrong: a globe height query (`rendered_surface`) has to be added if a later milestone lets the fly lower over the globe.
- **No lattice snapping of neighbour centres.** Every centre agrees on the shared planet lattice (M2), so snapping buys nothing. Cost if wrong: none for heights; biome colours differ anyway and are hidden by the cloud.
- **The terrain is uploaded at the swap frame, not before.** It is hidden by the opaque cloud and gated by the swap-frame worst frame (Task 10). Cost if wrong: add a pre-built second terrain scene if the gate fails.
- **`FlyState` keeps its name**, now meaning the fly's pose. This avoids churn in the benches, tests and examples. Cost if wrong: a rename later.

### Rulings made while building (copied from the SDD ledger)

Each is *what*; *why*; *cost if wrong*. The disk-guard branch's rulings, also in the ledger, are
left out on purpose: they belong to that branch, not to this plan.

**Pre-flight (before Task 1)**
- **P1.** `chase_pose` takes a trailing `min_distance: f64` (regions pass `CHASE_MIN_DISTANCE_M`, the globe `CHASE_MIN_DISTANCE_M / 1000.0`). Why: one constant can't be in both metres and kilometres. Cost: a one-argument signature change.
- **P2.** Task 3 builds its own minimal test grid in `fly_camera.rs` tests (or lifts the navigate helpers into a `#[cfg(test)] pub(crate)` module). Why: the navigate helpers are private. Cost: duplicated test scaffolding.
- **P3.** The climb out triggers at `height >= REGION_CLOUD_BASE_M - 1_000.0`; Task 6's ceiling eases over the band but caps at the base, never short of base − 1 km. Why: a ceiling that eases to zero speed at the base never reaches it, so the trigger must sit below it. Cost: retune one threshold.
- **P4.** The climb trigger is live only in `Flying`, not `Emerging`. Why: the fly placed at the base on entry must emerge downward first, not climb straight back out. Cost: none expected.
- **P5.** If the M2 bench path comes within the preload distance of the edge, Task 9 pulls the path inward and records it. Why: the path is a test driver, not product, and it must never preload. Cost: the edge segments' numbers aren't strictly comparable with M2's (applied: `FIELD_LIMIT` 0.5 and `COAST_IN_FIELD` 0.45 × `EDGE_M`).

**Task 1**
- A pure rustfmt fix commit (`86f1a79`) was verified mechanically (no non-whitespace change) instead of by a reviewer. Why: a formatting-only diff has nothing to judge. Cost: none.

**Task 3**
- Drag keeps M2's sense (dragging down looks further down): look pitch = (`CHASE_ELEVATION_DEG` − 5°) − (pitch − start pitch), and `FlyState.pitch` is clamped to the range the look allows. Why: the brief's literal `CHASE_ELEVATION_DEG + pitch` reversed M2's drag and started level; this starts with the fly a little below centre (design §5.3). Cost: one formula.
- W/S/strafe move the fly level; Space/E and Ctrl/Q change height. Why: the design says drag tilts the view, not the flight path. Cost: players who liked diving with W use Ctrl/Q.
- A light rim (fresnel) brightening on the dragonfly. Why: anti-goal "the fly lost" over dark forest. Cost: a slightly glowier fly.

**Task 4**
- On the globe the eye's elevation rises with altitude too: elevation = `globe_look_pitch_deg(alt)` + 5°, through `chase_pose_with_elevation`. Why: the plan's fixed 20° eye with a 70° look put the fly about 50° above centre, off screen. Cost: retune two curves.
- Framing targets replace the 15°-at-dive end: at `DIVE_ALTITUDE_KM` land fills at least the lower two-thirds under the fly (look 50°); at `GLOBE_MAX_ALTITUDE_KM` the disc centre is within about 10% of view height of centre (look 85°). Why: the approach must show the place you dive into. Cost: retune two constants.
- The top target is accepted at 10.6%. Why: it is the geometric floor with the fly 5° below centre. Cost: none.
- Globe drag keeps the region's sense, as an offset on both curves, clamped so the fly stays in frame and the eye within `GLOBE_MAX_CAMERA_DISTANCE_KM`. Why: one control feel in both worlds. Cost: a feel tweak.
- On the globe the fly is lit by the globe's sun, never the region sun (which reaches the globe view via `FLY_LAYER`). Why: wrong shading otherwise. Cost: wrong fly shading on the globe.
- The scroll wheel climbs and descends on the globe at the keys' speed rule. Why: M1 users' zoom habit. Cost: remove one input.
- The region bench's globe pan runs at `GLOBE_MAX_ALTITUDE_KM`. Why: closest to M1's whole-disc pan. Cost: pan numbers still not strictly M2-comparable.

**Task 5**
- Keep the cloud's tilt-shift until a native measurement at 2560 × 1440; drop it (`band` in `cloud.wesl`, `tilt_shift` in `cloud.rs`) if the pass exceeds 1 ms. Why: the brief's gate couldn't be measured without a clean native run (xctrace filled the disk). Cost: up to about 1 ms of GPU in short transitions, then a small deletion. Outcome (Task 11): the pass measured about 1.8 ms (ioreg, offscreen at 2560 × 1440), so the tilt-shift was dropped; without it the pass still costs about 2.0–2.2 ms, since the cost is the noise itself. The owner accepted about 2 ms on 2026-11-25 and asked for the tilt-shift's softer edges back, so it is restored (about 1.8 ms with it). See `docs/testing/m3/performance.md`, "Cloud pass cost".
- Pale UI text over the full white-out goes to the owner's manual checks. Why: taste, not correctness. Cost: a small HUD tweak later.

**Task 6**
- No third region-bench run for run 2's single-frame stalls (Red Sea sweep 65 ms, Japan run 59 ms); Panama judged on run 1 (run 2's was occluded and uncapped). Why: one-offs the owner doesn't weigh; medians and p95 pass everywhere; Task 11 re-runs natively. Cost: a sweep regression from the pitch retune would surface later.

**Task 7**
- While Diving, horizontal globe motion gets the same drift damping as vertical (|v| ≤ 0.05 × M2 speed). Why: "waiting holds in the cloud" means holding position; a 2,400–6,400 km drift breaks "the same place above and below". Cost: no steering inside the cloud during a slow generation.
- Keep the cloud's `frag_depth` write (near-plane depth outside the fly's hole at every density above clear) despite losing early-Z for the pass. Why: coverage is the milestone's core promise, and the pass shades nearly every pixel anyway. Cost: the pass may exceed budget and need a cheaper coverage scheme (measured in Task 11).

**Task 8**
- The region job's concurrency rules: a `Neighbour` request is ignored while an enter or snapshot job runs, or with no `ActiveRegion`; a newer `Neighbour` request replaces an older one and drops a ready `PendingRegion`; an enter or snapshot request replaces any job (a neighbour's included) and drops `PendingRegion`; `RegionLeft` drops the neighbour's job and `PendingRegion`; `RegionPreloadCancelled` drops a ready `PendingRegion` as well as the job in flight. Why: one job at a time, a stale neighbour is never promoted, and no grid outlives its use. Cost if wrong: an ignored request gets no reply, so the hop bounds its own wait (Task 9's `HOLD_MAX_S`).
- A tile load that times out fails a neighbour (`RegionPreloadFailed`) instead of falling back to coarse data, as an enter or snapshot job does; missing tiles still fall back. Why: a slow preload that fell back would pop in at the swap with coarse terrain, so a slow preload is a failed one, not a degraded one. Cost if wrong: on a slow network the fly turns back at the edge instead of crossing onto coarse terrain.
- A flight preload's web budget is `min(base, WEB_FLIGHT_GEN_BUDGET_MS)` (3 ms). Why: the region on screen must stay smooth while the neighbour generates. Cost if wrong: slower web preloads.

**Task 9**
- The neighbour lies past the edge the fly's heading reaches **first** (`hop::exit_edge`), level with where it crosses, not past the heading's dominant outward axis as the plan's `neighbour_centre` said. Why: on a slanting heading the dominant axis can name an edge the fly does not reach first; the first edge puts the fly about 100 km inside the neighbour, level with its centre (the Task 9 review judged it better). Cost if wrong: one function.
- Fix round 3 was committed before clippy and the wasm check ran (fmt and tests green). Why: the disk was held by another process and the re-review needed only the diff; both ran clean in round 4. Cost: one extra small commit.
- `HOLD_MAX_S` = 40 s (2 × `LOAD_TIMEOUT_S`) on the clamped load clock caps time at full cover; past it the hold fails and turns the fly back. Why: a bounded exit from white-out even if a job never answers or a player zig-zags retargets. Cost: a neighbour needing more than 40 s turns the fly back once and is re-asked on the next approach.
- A tap of S in the hold ends the hop (`PlaceInside` at the edge, `CancelPreload`); the next push out starts a fresh hop. Why: the only option that makes invariants I1–I4 hold by construction. Cost: an accidental tap costs about 8–13 s; the owner may prefer keep-alive-while-outside.

**Task 10**
- The deferred native measurements (cloud pass cost and the tilt-shift, old segments against M2, the near-geometry cloud check on Metal and Chrome) run in Task 11. Why: Task 10 builds the bench and a headless test only; Task 11 already does the native and Chrome runs. Cost: none.

**Task 11**
- The journey bench's hop-memory FAIL (worst +48.9 MiB against the spec's +40 MiB) is recorded as open, pending the owner; neither the gate nor the bench changes. Why: the gate is the spec's, so changing it is the owner's call; every other gate passes and memory is flat across journeys. Cost: M3 can't be called done until the owner decides (options in `docs/testing/m3/performance.md`).
- The Chrome WebGPU step is split: the implementer builds `web-release`, proves it by string grep and writes the serve instructions; the controller runs the owner's Chrome (Incognito) and commits the numbers. Why: a subagent can't drive the owner's Chrome. Cost: none.
- Hop memory (after the FAIL above): measured with an off-by-default trace (`timefly::hop_memory=trace`), then the generator's lattice moved to 512 KiB chunks (`ec9293e`), and the native gate raised to the owner's +64 MiB (`HOP_MEMORY_LIMIT_MIB`). Why: macOS 27 keeps a freed block resident and reuses it only for a request of the same size, and the lattice's size changes with every region, so each hop added up to 22 MiB of new pages; in chunks it reuses the last region's, and a hop reads about +22 MiB. The first hop of a run still reads up to +37.5 MiB (2.5 MiB under +40) and the parts add up to about +44 MiB with no reuse, so +40 would flap; the owner allowed +64 ("+64 is fine if we need it"). Freeing the old textures a frame earlier was tried and dropped: Metal frees them when the frames in flight finish, so the footprint didn't change. Cost: a hop may now add up to 64 MiB natively before the bench fails; region generation is about 3–5% slower (the rows read nodes through a chunk index). Breakdown in `docs/testing/m3/performance.md`, "Hop memory".
- The journey bench flies a route set (owner, 2026-11-25: "geographic diversity", "definitely include a coastline", a cube corner, the Grand Canyon, then "Andes and Fuji"): the Andes north from Aconcagua, Pangaea interior at 250 Ma, the Grand Canyon north, Mt Fuji west from the Pacific over the Kantō coast onto Fuji and on to the cube corner at 35.26° N, 135° E, then the Andes again (`JOURNEY_ROUTES`, `JOURNEY_ORDER`; `--region` still flies one place three times). Hops are judged on every route; memory cycles compare a route only with itself (the Andes twice); flight frames are judged per route, each with its own pacing rule, as the region bench judges each region (R14); route checks (a coast by land share, the corner by distance) are gated and the canyon and Fuji readings reported. The scripted dive now steers in to within 0.25° while descending, so a hop 250 km on reaches what it is aimed at. Why: one route flew near-repeat hops, none at a corner (the lattice there is 31–35 MiB against 14–16 for the Andes) or over a coast; pooled frames from routes that render very differently (Pangaea's dunes near the interval at 2560 × 1440, Fuji's sea far under it) failed the uncapped p95 rule though no route does on its own rule. Cost: about 7 minutes natively (5 journeys), about 10 on the web; the Alps stay in the headless tests, the manual checks and the region tour; Hawaii, the Red Sea and Japan at 1 Ma were drafted, then replaced by Fuji before any run.
- Journey bench flight, fix round 1 (owner, 2026-11-25, "Accept it"): the pacing rule is decided once per run from every flight frame pooled, then each route is judged under it; Pangaea's flight is reported, not gated (`JourneyRoute::flight_gated`), while its holds, swaps, hop memory and route checks stay gated. Why: classified on its own frames, a GPU-bound route whose uncapped median sits near 16.67 ms (Pangaea's 16.56 ms in `n1`) passed by the lenient display-paced rule; and Pangaea's dunes fail the uncapped p95 at 2560 × 1440 whenever the display doesn't pace the run (18.40 ms in `o1`), a terrain cost, not the transitions'. Cost: Pangaea's flight can regress without failing the journey bench (its line still prints a verdict; the region bench still gates it).

**Final review fix wave**
- On the web, the hop-memory gate judges growth per route: every hop of a route's first visit may grow the linear memory, and no hop of a repeat visit may; the journey memory cycles judge, on the web, what a route's repeat visits added against the memory just before them. Why: wasm linear memory never shrinks, so a route whose regions are larger than any before it (Mt Fuji's lattices, 31–35 MiB at the cube corner, against the Andes' 14–16) raises it for good, and the old rule (growth only on the run's first hop) failed the route set by construction. The brief said a route's first hop; its second may grow too, since Fuji's corner hop needs a larger lattice than its first. Cost if wrong: a leak that only a first visit shows goes unflagged on the web (natively every hop is still gated at +64 MiB).
- The dive's horizontal damping holds only in the opaque cloud (`globe_density == 1`, at or below `DIVE_ALTITUDE_KM`); above it, a fly more than `DIVE_LEAVE_KM` (10 km) from its target calls the dive off, and a descent dives where it is. Why: the damping exists so the region below is the place above; freed above the dive altitude, a fly could otherwise fly far off and then swap into the old target. Cost if wrong: one threshold.
- A hop's end lasts at most `HOP_END_MAX_S` = `CLOUD_CLEAR_S` + 12 s. Why: over steep ground under boosted flight `ReturnToAgl` may never get within 1 m, which held the slider and confined the fly; the slowest flat return (60 km down to the fly's 150 m clearance) takes about 10.2 s. Cost if wrong: a return cut short leaves the fly above its old height.
- The fly pitches with climbs: `MAX_PITCH_DEG` = 20° at the climb keys' full speed (a climb fraction of 1, clamped), smoothed over `PITCH_SMOOTHING_S` = 0.25 s like the bank. Why: design §5.1. Cost if wrong: two constants.
- A wish under `WISH_DEADBAND_MPS` (1 m/s) on an axis is no push out of an edge. Why: flying along an edge the fly sits exactly on, float noise (about 1e-8) started a hop; the slowest real push (60 m/s, 1° out) is 1.05 m/s. Cost if wrong: one constant.
