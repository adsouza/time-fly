//! The free-fly region camera (M2 design §6.6): `W` `A` `S` `D` move, a drag looks, `Space` /
//! `E` climb, `Ctrl` / `Q` descend and `Shift` boosts.
//!
//! The step is a pure function ([`fly_step`]), like M1's orbit camera. Its rules:
//!
//! - **Speed scales with height above ground** ([`fly_speed`]): one [`SPEED_PER_M_AGL`] of the
//!   height per second, so every height feels as quick to cross.
//! - **Clearance.** The camera never ends a step below `max(ground, 0) + CAMERA_CLEARANCE_M`
//!   (sea level is `y = 0`). A step is split into horizontal substeps of at most
//!   [`MAX_SUBSTEP_M`] and the floor is applied after each, so a long boosted step follows the
//!   terrain instead of tunnelling through a ridge.
//! - **A soft ceiling** at [`CAMERA_CEILING_M`]: climbing slows over the last
//!   [`CEILING_EASE_M`] and stops at the ceiling.
//! - **A soft edge:** past the inner field (`REGION_SIZE_M / 2` from the centre on either axis)
//!   the camera gains an inward velocity proportional to how far out it is, a stand-in for the
//!   parent spec's exit rule.
//!
//! The camera is the terrain scene's own `RegionCamera` entity (a root entity, so its
//! `Transform` is its world transform, which the patch planner reads). [`FlyState`] is a
//! resource: the step system ([`FlySystems::Step`]) writes it from the keyboard and mouse while
//! the player is flying, and [`FlySystems::Apply`] copies it to the camera every frame. A
//! scripted driver (Task 10's bench) writes [`FlyState`] between the two sets.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use timefly_core::terrain::blended_height_m;
use timefly_core::worldgen::REGION_SIZE_M;
use timefly_core::worldgen::generate::RegionGrid;

use crate::region::navigate::{NavPhase, Navigation, NavigationSystems};
use crate::region::{ActiveRegion, CAMERA_START_ALTITUDE_M};
use crate::terrain::{TerrainScene, TerrainSceneSystems};
use crate::ui::PointerOverUi;

/// The least height above `max(terrain, 0)` the camera ends any step at (design §6.6).
pub const CAMERA_CLEARANCE_M: f32 = 150.0;
/// The camera's ceiling above sea level (design §6.6).
pub const CAMERA_CEILING_M: f32 = 60_000.0;
/// Climbing slows over this last stretch below the ceiling.
pub const CEILING_EASE_M: f32 = 6_000.0;
/// The starting pitch: looking north and down this much (M2 design §6.1).
pub const START_PITCH_DEG: f32 = 20.0;
/// Speed per metre of height above ground, per second *(call)*: at the 8 km start the camera
/// crosses the 300 km inner field in about 40 s; at the clearance it skims at 120 m/s.
pub const SPEED_PER_M_AGL: f32 = 0.8;
/// The slowest cruise (m/s), near the ground.
pub const MIN_SPEED_MPS: f32 = 60.0;
/// The fastest cruise (m/s), high up.
pub const MAX_SPEED_MPS: f32 = 10_000.0;
/// `Shift` multiplies the speed by this.
pub const BOOST: f32 = 4.0;
/// Radians of turn per dragged pixel.
pub const LOOK_RADIANS_PER_PX: f32 = 0.003;
/// The pitch stops short of straight up or down, so the view's "up" stays defined.
pub const MAX_PITCH_DEG: f32 = 89.0;
/// A frame longer than this moves the camera only this far: a hitch never flings it.
pub const MAX_STEP_S: f32 = 0.5;
/// The longest horizontal move between two floor checks within a step.
pub const MAX_SUBSTEP_M: f32 = 250.0;
/// The soft edge's spring: inward speed (m/s) per metre past the inner field.
pub const EDGE_SPRING_PER_S: f32 = 1.0;

/// Half the inner field: the soft edge starts here on either axis.
pub const EDGE_M: f32 = (REGION_SIZE_M / 2.0) as f32;

/// Where the camera is and where it looks: local metres in the region's frame (`+x` east, `−z`
/// north, `+y` up from sea level), `yaw` about `+y` from north (positive turns west, as Bevy's
/// rotations go) and `pitch` about the camera's right axis (negative looks down).
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct FlyState {
    pub pos_m: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

impl Default for FlyState {
    fn default() -> Self {
        FlyState {
            pos_m: Vec3::new(0.0, CAMERA_START_ALTITUDE_M, 0.0),
            yaw: 0.0,
            pitch: -START_PITCH_DEG.to_radians(),
        }
    }
}

impl FlyState {
    /// The start of a region (M2 design §6.1): [`CAMERA_START_ALTITUDE_M`] above the higher of
    /// the ground and the sea at the centre, looking north and pitched down [`START_PITCH_DEG`].
    pub fn start(grid: &RegionGrid) -> FlyState {
        let ground = blended_height_m(grid, 0.0, 0.0).max(0.0);
        FlyState {
            pos_m: Vec3::new(0.0, ground + CAMERA_START_ALTITUDE_M, 0.0),
            ..default()
        }
    }

    /// The camera's orientation.
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }

    /// The camera's transform.
    pub fn transform(&self) -> Transform {
        Transform::from_translation(self.pos_m).with_rotation(self.rotation())
    }
}

/// One frame of flight input.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FlyInput {
    /// Movement in the camera's axes, each in [−1, 1]: `x` right (`D`), `y` up (`Space` / `E`)
    /// and `−z` forward (`W`), as Bevy's camera looks down `−z`.
    pub move_local: Vec3,
    /// Pointer movement while dragging, in logical pixels (y down).
    pub look_delta: Vec2,
    pub boost: bool,
    pub dt_s: f32,
}

/// Cruise speed (m/s) at `agl_m` above `max(ground, 0)`, before the boost.
pub fn fly_speed(agl_m: f32) -> f32 {
    (agl_m * SPEED_PER_M_AGL).clamp(MIN_SPEED_MPS, MAX_SPEED_MPS)
}

/// The inward velocity (m/s) along one axis at coordinate `at_m`: zero within [`EDGE_M`], then
/// [`EDGE_SPRING_PER_S`] per metre beyond it, pointing back toward the centre.
pub fn edge_nudge(at_m: f32) -> f32 {
    let excess = at_m.abs() - EDGE_M;
    if excess > 0.0 {
        -at_m.signum() * excess * EDGE_SPRING_PER_S
    } else {
        0.0
    }
}

/// The next camera state. `ground_m(x, z)` is the terrain height at local metres (sea level is
/// `y = 0`; below it, the sea is the floor). See the module docs for the rules.
pub fn fly_step(state: FlyState, input: FlyInput, ground_m: impl Fn(f32, f32) -> f32) -> FlyState {
    let dt = if input.dt_s.is_finite() {
        input.dt_s.clamp(0.0, MAX_STEP_S)
    } else {
        0.0
    };
    let max_pitch = MAX_PITCH_DEG.to_radians();
    let yaw = state.yaw - input.look_delta.x * LOOK_RADIANS_PER_PX;
    let pitch =
        (state.pitch - input.look_delta.y * LOOK_RADIANS_PER_PX).clamp(-max_pitch, max_pitch);
    let surface = |x: f32, z: f32| ground_m(x, z).max(0.0);
    let p = state.pos_m;

    // The direction asked for, in world axes: forward along the view, strafing and climbing
    // level. Diagonals are no faster than a single key.
    let m = input.move_local.clamp(Vec3::splat(-1.0), Vec3::ONE);
    let heading = Quat::from_rotation_y(yaw);
    let forward = heading * Quat::from_rotation_x(pitch) * Vec3::NEG_Z;
    let wish = heading * Vec3::X * m.x + Vec3::Y * m.y + forward * -m.z;
    let wish = if wish.length_squared() > 1.0 {
        wish.normalize()
    } else {
        wish
    };
    let boost = if input.boost { BOOST } else { 1.0 };
    let mut velocity = wish * fly_speed(p.y - surface(p.x, p.z)) * boost;
    if velocity.y > 0.0 {
        velocity.y *= ((CAMERA_CEILING_M - p.y) / CEILING_EASE_M).clamp(0.0, 1.0);
    }
    velocity.x += edge_nudge(p.x);
    velocity.z += edge_nudge(p.z);

    // Substeps, each ending on or above the floor and under the ceiling.
    let delta = velocity * dt;
    let substeps = (Vec2::new(delta.x, delta.z).length() / MAX_SUBSTEP_M)
        .ceil()
        .clamp(1.0, 1_024.0);
    let step = delta / substeps;
    let mut pos = p;
    for _ in 0..substeps as u32 {
        pos += step;
        let floor = surface(pos.x, pos.z) + CAMERA_CLEARANCE_M;
        pos.y = pos.y.min(CAMERA_CEILING_M).max(floor);
    }
    FlyState {
        pos_m: pos,
        yaw,
        pitch,
    }
}

/// The fly camera's systems: [`FlySystems::Step`] turns input into [`FlyState`] while flying;
/// [`FlySystems::Apply`] moves the region camera to it. A scripted driver writes [`FlyState`]
/// after `Step` and before `Apply`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlySystems {
    Step,
    Apply,
}

/// Whether the current left-button drag began off the UI (and so looks around).
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq, Eq)]
struct LookDrag(bool);

pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlyState>()
            .init_resource::<LookDrag>()
            .configure_sets(
                Update,
                (FlySystems::Step, FlySystems::Apply)
                    .chain()
                    .after(NavigationSystems),
            )
            .configure_sets(Update, FlySystems::Apply.after(TerrainSceneSystems))
            .add_systems(Update, step_fly.in_set(FlySystems::Step))
            .add_systems(Update, apply_fly.in_set(FlySystems::Apply));
    }
}

/// Movement from the held keys: `W` `A` `S` `D`, `Space` / `E` up, `Ctrl` / `Q` down.
pub fn keys_to_move(keys: &ButtonInput<KeyCode>) -> Vec3 {
    let axis = |plus: &[KeyCode], minus: &[KeyCode]| {
        f32::from(u8::from(keys.any_pressed(plus.iter().copied())))
            - f32::from(u8::from(keys.any_pressed(minus.iter().copied())))
    };
    Vec3::new(
        axis(&[KeyCode::KeyD], &[KeyCode::KeyA]),
        axis(
            &[KeyCode::Space, KeyCode::KeyE],
            &[KeyCode::ControlLeft, KeyCode::ControlRight, KeyCode::KeyQ],
        ),
        axis(&[KeyCode::KeyS], &[KeyCode::KeyW]),
    )
}

/// Gathers the keys and the look drag, steps [`FlyState`] over the region's ground.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn step_fly(
    nav: Option<Res<Navigation>>,
    region: Option<Res<ActiveRegion>>,
    time: Res<Time>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    motion: Option<Res<AccumulatedMouseMotion>>,
    over_ui: Option<Res<PointerOverUi>>,
    mut drag: ResMut<LookDrag>,
    mut fly: ResMut<FlyState>,
) {
    let flying = nav.is_some_and(|n| n.phase == NavPhase::Flying);
    let (true, Some(region)) = (flying, region) else {
        drag.0 = false;
        return;
    };
    if let Some(mouse) = &mouse {
        if mouse.just_pressed(MouseButton::Left) {
            drag.0 = !over_ui.is_some_and(|o| o.0);
        } else if !mouse.pressed(MouseButton::Left) {
            drag.0 = false;
        }
    }
    let input = FlyInput {
        move_local: keys.as_deref().map_or(Vec3::ZERO, keys_to_move),
        look_delta: match (&motion, drag.0) {
            (Some(m), true) => m.delta,
            _ => Vec2::ZERO,
        },
        boost: keys
            .as_ref()
            .is_some_and(|k| k.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight])),
        dt_s: time.delta_secs(),
    };
    let grid = &region.grid;
    let next = fly_step(*fly, input, |x, z| {
        blended_height_m(grid, f64::from(x), f64::from(z))
    });
    if next != *fly {
        *fly = next;
    }
}

/// Moves the terrain scene's region camera to [`FlyState`].
fn apply_fly(
    fly: Res<FlyState>,
    scene: Option<Res<TerrainScene>>,
    mut cameras: Query<&mut Transform>,
) {
    let Some(scene) = scene else { return };
    if let Ok(mut t) = cameras.get_mut(scene.camera) {
        let want = fly.transform();
        if *t != want {
            *t = want;
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn flat(h: f32) -> impl Fn(f32, f32) -> f32 {
        move |_, _| h
    }

    fn input(move_local: Vec3, boost: bool, dt_s: f32) -> FlyInput {
        FlyInput {
            move_local,
            look_delta: Vec2::ZERO,
            boost,
            dt_s,
        }
    }

    fn floor(ground: &impl Fn(f32, f32) -> f32, p: Vec3) -> f32 {
        ground(p.x, p.z).max(0.0) + CAMERA_CLEARANCE_M
    }

    /// Steep, rough terrain spanning −5,000 to +8,000 m: two crossed ridgelines, a short-wave
    /// ripple and a cliff, with the parameters from the strategy.
    fn terrain(a: f32, b: f32, k: f32, cliff: f32) -> impl Fn(f32, f32) -> f32 {
        move |x: f32, z: f32| {
            let ridges = (x * k).sin() * (z * k * 1.3 + a).cos();
            let ripple = (x * 0.01 + z * 0.013 + b).sin();
            let step = if x > cliff { 4_000.0 } else { 0.0 };
            (1_500.0 + 4_500.0 * ridges + 1_500.0 * ripple + step).clamp(-5_000.0, 8_000.0)
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(400))]

        /// After every step, the camera is at least the clearance above `max(ground, 0)` and at
        /// most at the ceiling, at boost speed and frame times up to 0.5 s, over steep terrain.
        #[test]
        fn clearance_holds_for_large_steps(
            a in -3.0f32..3.0,
            b in -3.0f32..3.0,
            k in 1e-5f32..2e-3,
            cliff in -200_000.0f32..200_000.0,
            x in -200_000.0f32..200_000.0,
            z in -200_000.0f32..200_000.0,
            above in 0.0f32..60_000.0,
            yaw in -4.0f32..4.0,
            pitch in -1.5f32..1.5,
            steps in prop::collection::vec(
                (
                    (-1.0f32..1.0, -1.0f32..1.0, -1.0f32..1.0),
                    (-300.0f32..300.0, -300.0f32..300.0),
                    0.0f32..=0.5,
                    any::<bool>(),
                ),
                1..24,
            ),
        ) {
            let ground = terrain(a, b, k, cliff);
            let start = Vec3::new(x, 0.0, z);
            let y = (floor(&ground, start) + above).min(CAMERA_CEILING_M);
            let mut state = FlyState { pos_m: Vec3::new(x, y, z), yaw, pitch };
            for ((mx, my, mz), (lx, ly), dt_s, boost) in steps {
                let step = FlyInput {
                    move_local: Vec3::new(mx, my, mz),
                    look_delta: Vec2::new(lx, ly),
                    // Most steps boost: the brief's worst case.
                    boost: boost || dt_s > 0.1,
                    dt_s,
                };
                state = fly_step(state, step, &ground);
                let p = state.pos_m;
                prop_assert!(p.is_finite(), "{state:?}");
                prop_assert!(p.y >= floor(&ground, p), "{p} under {}", floor(&ground, p));
                prop_assert!(p.y <= CAMERA_CEILING_M, "{p} over the ceiling");
                prop_assert!(state.pitch.abs() <= MAX_PITCH_DEG.to_radians() + 1e-6);
            }
        }
    }

    #[test]
    fn speed_scales_with_height_above_ground() {
        // Level flight north over flat land: the distance covered per second grows with the
        // height, within the cruise limits; the boost multiplies it.
        let forward = Vec3::new(0.0, 0.0, -1.0);
        let level = |y: f32, boost: bool| {
            let s = FlyState {
                pos_m: Vec3::new(0.0, y, 0.0),
                yaw: 0.0,
                pitch: 0.0,
            };
            let next = fly_step(s, input(forward, boost, 0.1), flat(500.0));
            assert!((next.pos_m.y - y).abs() < 1e-3, "level flight stays level");
            -(next.pos_m.z - s.pos_m.z) / 0.1
        };
        let low = level(500.0 + 400.0, false);
        let high = level(500.0 + 8_000.0, false);
        assert!((low - fly_speed(400.0)).abs() < 0.5, "{low}");
        assert!((high - 8_000.0 * SPEED_PER_M_AGL).abs() < 1.0, "{high}");
        assert!(high > 10.0 * low, "{low} vs {high}");
        assert!((level(500.0 + 8_000.0, true) - BOOST * high).abs() < 2.0);
        // Over the sea, height counts from sea level, not the sea floor.
        let sea = fly_step(
            FlyState {
                pos_m: Vec3::new(0.0, 1_000.0, 0.0),
                yaw: 0.0,
                pitch: 0.0,
            },
            input(forward, false, 0.1),
            flat(-4_000.0),
        );
        assert!((-sea.pos_m.z / 0.1 - fly_speed(1_000.0)).abs() < 0.5);
        // The limits.
        assert_eq!(fly_speed(0.0), MIN_SPEED_MPS);
        assert_eq!(fly_speed(1.0e6), MAX_SPEED_MPS);
    }

    #[test]
    fn soft_edge_nudges_back() {
        assert_eq!(edge_nudge(0.0), 0.0);
        assert_eq!(edge_nudge(EDGE_M), 0.0);
        assert!(edge_nudge(EDGE_M + 1_000.0) < 0.0);
        assert!(edge_nudge(-EDGE_M - 1_000.0) > 0.0);
        assert!((edge_nudge(EDGE_M + 1_000.0) + 1_000.0 * EDGE_SPRING_PER_S).abs() < 1e-3);
        // Beyond REGION_SIZE_M / 2, with no input, the camera drifts back toward the centre on
        // each axis it is out on; inside, it stays put.
        let ground = flat(100.0);
        let out = FlyState {
            pos_m: Vec3::new(EDGE_M + 10_000.0, 5_000.0, -EDGE_M - 4_000.0),
            yaw: 0.0,
            pitch: 0.0,
        };
        let next = fly_step(out, input(Vec3::ZERO, false, 0.1), &ground);
        assert!(next.pos_m.x < out.pos_m.x, "{next:?}");
        assert!(next.pos_m.z > out.pos_m.z, "{next:?}");
        assert_eq!(next.pos_m.y, out.pos_m.y);
        let inside = FlyState {
            pos_m: Vec3::new(EDGE_M - 1.0, 5_000.0, 0.0),
            ..out
        };
        assert_eq!(
            fly_step(inside, input(Vec3::ZERO, false, 0.1), &ground),
            inside
        );
        // Flying straight out at full boost, the nudge holds the camera within a bounded band
        // past the edge.
        let mut s = FlyState {
            pos_m: Vec3::new(0.0, 8_000.0, 0.0),
            yaw: -std::f32::consts::FRAC_PI_2, // facing east
            pitch: 0.0,
        };
        for _ in 0..2_000 {
            s = fly_step(s, input(Vec3::new(0.0, 0.0, -1.0), true, 0.05), &ground);
        }
        assert!(s.pos_m.x > EDGE_M, "{s:?}");
        let band = BOOST * fly_speed(8_000.0 - 100.0) / EDGE_SPRING_PER_S;
        assert!(s.pos_m.x < EDGE_M + band + 1.0, "{s:?}");
    }

    #[test]
    fn look_turns_and_stops_short_of_vertical() {
        let s = FlyState::default();
        let look = |dx: f32, dy: f32| {
            fly_step(
                s,
                FlyInput {
                    look_delta: Vec2::new(dx, dy),
                    dt_s: 0.016,
                    ..default()
                },
                flat(0.0),
            )
        };
        // Dragging right turns right (east): yaw decreases.
        assert!(look(10.0, 0.0).yaw < s.yaw);
        // Dragging down looks down further.
        assert!(look(0.0, 10.0).pitch < s.pitch);
        assert_eq!(look(0.0, 1.0e6).pitch, -MAX_PITCH_DEG.to_radians());
        assert_eq!(look(0.0, -1.0e6).pitch, MAX_PITCH_DEG.to_radians());
        assert_eq!(look(0.0, 0.0).pos_m, s.pos_m);
    }

    #[test]
    fn moves_follow_the_view_and_the_keys() {
        let s = FlyState {
            pos_m: Vec3::new(0.0, 5_000.0, 0.0),
            yaw: 0.0,
            pitch: -START_PITCH_DEG.to_radians(),
        };
        let step = |m: Vec3| fly_step(s, input(m, false, 0.1), flat(0.0)).pos_m - s.pos_m;
        // W flies along the view: north and down.
        let w = step(Vec3::new(0.0, 0.0, -1.0));
        assert!(w.z < 0.0 && w.y < 0.0 && w.x.abs() < 1e-3, "{w}");
        // D strafes east, level.
        let d = step(Vec3::X);
        assert!(d.x > 0.0 && d.y.abs() < 1e-3 && d.z.abs() < 1e-3, "{d}");
        // Space climbs straight up.
        let up = step(Vec3::Y);
        assert!(up.y > 0.0 && up.x.abs() < 1e-3 && up.z.abs() < 1e-3, "{up}");
        // Diagonal input is no faster than straight input.
        let diag = step(Vec3::new(1.0, 0.0, -1.0));
        assert!(diag.length() <= w.length() + 1e-2, "{diag} vs {w}");
        // A long hitch moves no farther than MAX_STEP_S.
        let hitch = fly_step(s, input(Vec3::X, false, 5.0), flat(0.0)).pos_m - s.pos_m;
        let capped = fly_step(s, input(Vec3::X, false, MAX_STEP_S), flat(0.0)).pos_m - s.pos_m;
        assert!((hitch - capped).length() < 1e-2);
    }

    #[test]
    fn climbing_slows_near_the_ceiling() {
        let near = |y: f32| FlyState {
            pos_m: Vec3::new(0.0, y, 0.0),
            yaw: 0.0,
            pitch: 0.0,
        };
        let climb = |y: f32| {
            fly_step(near(y), input(Vec3::Y, true, 0.1), flat(0.0))
                .pos_m
                .y
                - y
        };
        let mid = climb(CAMERA_CEILING_M - 2.0 * CEILING_EASE_M);
        let top = climb(CAMERA_CEILING_M - 0.2 * CEILING_EASE_M);
        assert!(top < 0.5 * mid, "{top} vs {mid}");
        assert_eq!(
            fly_step(near(CAMERA_CEILING_M), input(Vec3::Y, true, 0.5), flat(0.0))
                .pos_m
                .y,
            CAMERA_CEILING_M
        );
    }

    #[test]
    fn a_long_boosted_step_ends_above_a_wall() {
        // A 4 km wall within one boosted half-second step (2.4 km): the camera ends past the
        // wall and above it.
        let wall = |x: f32, _z: f32| if x > 2_000.0 { 4_000.0 } else { 0.0 };
        let s = FlyState {
            pos_m: Vec3::new(0.0, 1_500.0, 0.0),
            yaw: -std::f32::consts::FRAC_PI_2,
            pitch: 0.0,
        };
        let next = fly_step(s, input(Vec3::new(0.0, 0.0, -1.0), true, 0.5), wall);
        assert!(next.pos_m.x > 2_000.0, "{next:?}");
        assert!(next.pos_m.y >= 4_000.0 + CAMERA_CLEARANCE_M, "{next:?}");
    }

    #[test]
    fn keys_map_to_movement() {
        let mut keys = ButtonInput::<KeyCode>::default();
        assert_eq!(keys_to_move(&keys), Vec3::ZERO);
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyD);
        keys.press(KeyCode::KeyQ);
        assert_eq!(keys_to_move(&keys), Vec3::new(1.0, -1.0, -1.0));
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::Space);
        keys.release(KeyCode::KeyW);
        keys.press(KeyCode::KeyS);
        // Opposite keys cancel.
        assert_eq!(keys_to_move(&keys), Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn start_is_above_the_centre_looking_north_and_down() {
        let s = FlyState::default();
        let t = s.transform();
        let forward = t.forward().as_vec3();
        assert!(forward.z < 0.0 && forward.x.abs() < 1e-6, "{forward}");
        let down = (-forward.y).asin().to_degrees();
        assert!((down - START_PITCH_DEG).abs() < 1e-3, "{down}");
        // The same pose as Task 7's debug camera.
        let debug = Transform::from_xyz(0.0, CAMERA_START_ALTITUDE_M, 0.0)
            .with_rotation(Quat::from_rotation_x(-START_PITCH_DEG.to_radians()));
        assert!(t.rotation.abs_diff_eq(debug.rotation, 1e-6));
        assert_eq!(t.translation, debug.translation);
    }
}
