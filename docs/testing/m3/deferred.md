# Milestone 3: deferred minors

Every minor finding the task reviews deferred, one per line, for the final review to triage.
File and line references are as of the task that raised them and may have moved since. Items an
owner has to judge are also in `manual-checks.md`.

## Task 1 (flight altitudes, cloud density and the chase camera)
- The ridge test in `chase.rs` is weak: a step ridge, asserting only `eye.y >= 2050`.
- The fly's screen-size test is algebraic (the plan's wording); the windowed screenshots are the real check.
- Four copies of `smoothstep` across `timefly_core`.
- `chase_pose` returns NaN if `forward` is parallel to `up` (no guard).
- No const-asserts on the ordering of the altitude constants.

## Task 2 (globe flight)
- The climb uses the boosted, angular-capped speed (consistent with M2, undocumented).
- The pole test lacks an assertion that longitude moves by about 180°.
- The yaw-conversion sign isn't pinned independently (the round trip is only self-consistent).
- Strafe, backward and the boost factor are untested.
- 0.8 and 4.0 duplicate `fly_camera` constants.
- `GLOBE_MIN_ALTITUDE_KM` lives in `globe.rs`, not `altitudes.rs`.

## Task 3 (the dragonfly and the chase camera in regions)
- The dragonfly pipelines aren't in the region warm-up (a first-show compile hitch); carried through Task 4, where the globe shows the fly from startup.
- With the eye straight above the target on very steep ground, the camera heading falls back to the fly's yaw (arbitrary).
- The line of sight between the eye and the fly isn't checked against a crest.
- `fly_camera.rs` grew to about 1,327 lines: move the chase tests to `fly_camera/tests.rs`.
- `FlyState::transform` is now test-only.
- The `Chase.smooth` doc says "until a region shows" but it resets on the scene's existence.
- The region sun on `FLY_LAYER` enters the globe view's light list (Task 4 ruled which sun lights the fly).
- The bench pitches' meaning changed (retuned in Task 6).

## Task 4 (globe flight replaces the orbit camera)
- At 250 Ma a staircase of ocean between plate pieces is visible from 1,250 to 2,500 km (deep globe plate resolution): flagged to the owner (manual check 2).
- `DiagnosticsContext` is written every frame (guard with `!=`).
- `f64::clamp` can panic on NaN or inverted bounds (`chase.rs:139`, `globe_flight.rs:270`).
- No glide reset on the `CentreGlobe` leave teleport (Task 7 replaced that path).
- Scroll is linear per frame: a big flick drops the fly to the floor.
- The pose and altitude write block is duplicated between `apply_globe_flight` and `apply_fly`.
- A long doc line at `globe_flight.rs:18`, and a 65-line test scaffold in `globe/mod.rs`.
- `on_the_globe`'s `NavPhase` gate is untested.
- The fly may read dark at high latitude under the low globe sun (unconfirmed; manual check 4).

## Task 5 (the cloud effect)
- `update_cloud` uses `density.0` unclamped (`cloud.rs:351`; `pin_cloud` writes it directly).
- The warm-up `CloudParams` has `state.y = 0`, a divide by zero in the clip depth (`warmup.rs:295`, harmless).
- The cloud material is rewritten every frame while visible (`cloud.rs:419`).
- The "no overlay" test counts a name string (`navigate/tests.rs:1013`).
- The globe tint uses `REGION_SUN_COLOR` (`cloud.rs:415`).

## Task 6 (regions seen from higher up)
- The bench's high-pass climb peaks at about 39 km/s, 4 × `MAX_SPEED_MPS`, ignoring the ceiling ease (`path.rs:453`: document it or lengthen the segment).
- `view_ranges` doesn't clamp `up` to [0, 1] (`view.rs:107`).
- The apron test samples only 60, 120, 240 and 300 km: add 90, 180 and 270 (`tests.rs:602`).
- `super::super::view::view_ranges` paths in the `material.rs` tests (:976, :988).

## Task 7 (the dive and the climb)
- A launch or bench `--region` entry pops from clear to opaque with no gather (`phase.rs:420`; manual check 12).
- Moving the slider back to the held time doesn't unqueue the change (`phase.rs:651`).
- A slider move on the `Backspace` frame becomes the held time (`mod.rs:540`, undocumented).
- `tests.rs` is 2,304 lines: split out the headless worlds.
- The task's gate log lacked exit codes.
- The HUD altitude lags a frame on the swap (under the white-out).

## Task 8 (preloading a neighbour region)
- No tests for: a neighbour sent the same frame as an enter or snapshot request; a neighbour with no active region; a neighbour replacing a ready neighbour; a swap with nothing pending.

## Task 9 (the region hop)
- `velocity_at_edges` treats about 1e-8 of float noise as intent (`hop.rs:108`: add a ±1 m/s deadband).
- The landing gate accepts a carry exactly on the neighbour's side edge (`phase.rs:1110`: add a margin against back-to-back hops).
- The hop property test never presses `Backspace`, moves the slider or changes `region_state` (covered by named tests).
- The fallback notice can't show for a neighbour that fell back.
- The region bench's `flying()` counts a preloading hop.

## Task 10 (the journey bench)
- The flight summary lines are duplicated in the journey bench, the region bench and `bench/mod.rs`: extract a shared helper.
- `ChosenBench` and `chosen_bench` are `pub` but only `lib.rs` uses them (`pub(crate)`).
- The `hop lift` line compares the height above the ground ("from") with the height above sea level ("peak").
