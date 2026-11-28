//! The fly's flight in a region and its chase camera (M2 design §6.6, M3 design §5).
//! [`FlyState`] is the **fly's** pose (M3): the keys steer it as they steered M2's free camera,
//! and the region camera follows it from behind and above.
//!
//! The controls: `W` `A` `S` `D` move the fly level along its heading, a drag turns it and
//! tilts the view (dragging down looks further down, as in M2; the pitch never tilts the
//! flight path), `Space` / `E` climb and `Q` or `Ctrl` (`Option` on a Mac,
//! [`descend_modifiers`]) descend. The arrow keys also fly: `↑` / `↓` forward and back,
//! `←` / `→` turn, and with `Shift` held `←` / `→` fly sideways instead. `Shift` with forward
//! (`↑` or `W`) boosts; `Shift` alone, or with any other direction, does not.
//!
//! The step is a pure function ([`fly_step`]), like M1's orbit camera. Its rules:
//!
//! - **Speed scales with height above the surface** ([`fly_speed`]): one [`SPEED_PER_M_AGL`] of
//!   the height per second, so every height feels as quick to cross.
//! - **Clearance.** The camera never ends a step below `surface + CAMERA_CLEARANCE_M`, the
//!   surface being the drawn one ([`blended_surface_m`]): the higher of the ground and the
//!   water (the sea at `y = 0`, a lake, a river). A dry basin below sea level is ground like any
//!   other. A step is split into horizontal substeps of at most
//!   [`MAX_SUBSTEP_M`] and the floor is applied after each, so a long boosted step follows the
//!   terrain instead of tunnelling through a ridge.
//! - **A soft ceiling** at [`CAMERA_CEILING_M`], the region's cloud base: climbing slows over
//!   the last [`CEILING_EASE_M`] (to no less than [`CEILING_MIN_EASE`] of its speed) and stops
//!   at the ceiling.
//! - **No edge here:** the inner field's edge ([`EDGE_M`]) is crossed by a hop to the
//!   neighbouring region (M3 Task 9, `crate::region::navigate`), which replaced M2's soft edge.
//!   Navigation also decides when the keys fly at all ([`crate::region::navigate::NavPhase::fly_controls`]): during a
//!   hop's lift the height is the lift's, and while it holds in the cloud the fly drifts.
//!
//! (The clearance and ceiling hold the fly; the chase camera keeps its own clearance.)
//!
//! **The chase camera** is the terrain scene's own `RegionCamera` entity (a root entity, so its
//! `Transform` is its world transform, which the patch planner reads). [`FlyState`] is a
//! resource: the step system ([`FlySystems::Step`]) writes it from the keyboard and mouse while
//! the player is flying, and [`FlySystems::Apply`] places the camera every frame:
//! [`region_chase_pose`] puts the eye behind the fly's heading and above it (Task 1's
//! `chase_pose`, looking [`chase_look_pitch_deg`] below the horizontal, so the view pitch tilts
//! the view), lifted clear of the ground. The smoothing works in the fly's frame: the eye's and
//! the target's offsets from the fly glide with [`critically_damped`] smoothing
//! ([`CHASE_SMOOTHING_S`]) and are added to the fly's current position, so the camera follows
//! the fly's motion rigidly and only turns and view changes glide. The result is lifted again,
//! so the camera never enters a hill. The same set writes the dragonfly's `FlyPose` (scaled by `fly_scale` to
//! hold its size on screen, banked and bobbing) and the overlay's `FlyAltitude`. A scripted
//! driver (Task 10's bench) writes [`FlyState`] between the two sets.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::math::{DQuat, DVec3};
use bevy::prelude::*;
use timefly_core::terrain::blended_surface_m;
use timefly_core::worldgen::generate::RegionGrid;

use timefly_core::flight::altitudes::{
    CHASE_DISTANCE_PER_AGL, CHASE_ELEVATION_DEG, CHASE_MIN_DISTANCE_M, REGION_CLOUD_BAND_M,
    REGION_CLOUD_BASE_M,
};
pub use timefly_core::flight::chase::CHASE_START_LOOK_DEG;
use timefly_core::flight::chase::{ChasePose, chase_pose, fly_scale, lift_above_ground};

use crate::creature::{FlyPose, HOVER_EASE_S, bank_step, hover_offset, pitch_step};
use crate::region::navigate::{Navigation, NavigationSystems};
use crate::region::{ActiveRegion, CAMERA_START_ALTITUDE_M};
use crate::terrain::view::REGION_FOV_DEG;
use crate::terrain::{RegionViewSystems, TerrainScene, TerrainSceneSystems};
use crate::ui::PointerOverUi;
use crate::ui::altitude::{Altitude, FlyAltitude};

/// The least height above the drawn surface ([`blended_surface_m`]: the ground or the water over
/// it) the camera ends any step at (design §6.6).
pub const CAMERA_CLEARANCE_M: f32 = 150.0;
/// The fly's ceiling above sea level: the region's cloud base (M3 design §7.3; M2's was 60 km).
pub const CAMERA_CEILING_M: f32 = REGION_CLOUD_BASE_M as f32;
/// Climbing slows over this last stretch below the ceiling: the cloud base's band.
pub const CEILING_EASE_M: f32 = REGION_CLOUD_BAND_M as f32;
/// The slowest the climb gets under the ceiling, as a fraction of the climb speed *(call)*: the
/// ease never stalls short of the base, so the climb out (`REGION_CLOUD_BASE_M − 1 km`, ruling
/// P3) is always reached, and the fly stops at the base itself.
pub const CEILING_MIN_EASE: f32 = 0.1;
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
/// How fast `←` / `→` turn the camera.
pub const TURN_DEG_PER_S: f32 = 60.0;
/// The view pitch's range (degrees): exactly the pitches whose chase look stays within
/// [`CHASE_LOOK_MIN_DEG`]..=[`CHASE_LOOK_MAX_DEG`], so a drag never has a dead zone. With a 60°
/// view: −50° (looking 45° down) to 0° (looking 5° up).
pub const MIN_PITCH_DEG: f32 = pitch_for_look_deg(CHASE_LOOK_MAX_DEG);
/// See [`MIN_PITCH_DEG`].
pub const MAX_PITCH_DEG: f32 = pitch_for_look_deg(CHASE_LOOK_MIN_DEG);
/// A frame longer than this moves the camera only this far: a hitch never flings it.
pub const MAX_STEP_S: f32 = 0.5;
/// The longest horizontal move between two floor checks within a step.
pub const MAX_SUBSTEP_M: f32 = 250.0;
/// The chase camera's smoothing time constant (s).
pub const CHASE_SMOOTHING_S: f64 = 0.2;
/// The chase camera's look-down pitch stays within this margin of the view's edge, so the fly
/// (seen [`CHASE_ELEVATION_DEG`] below the horizontal) never leaves the frame.
pub const CHASE_LOOK_MARGIN_DEG: f64 = 5.0;
/// The shallowest chase look (degrees below the horizontal; negative looks up): the fly stays
/// [`CHASE_LOOK_MARGIN_DEG`] inside the bottom of the frame.
pub const CHASE_LOOK_MIN_DEG: f64 =
    CHASE_ELEVATION_DEG - (REGION_FOV_DEG as f64 / 2.0 - CHASE_LOOK_MARGIN_DEG);
/// The steepest chase look: the fly stays [`CHASE_LOOK_MARGIN_DEG`] inside the top of the frame.
pub const CHASE_LOOK_MAX_DEG: f64 =
    CHASE_ELEVATION_DEG + (REGION_FOV_DEG as f64 / 2.0 - CHASE_LOOK_MARGIN_DEG);

/// The view pitch (degrees) whose chase look is `look_deg` (degrees below the horizontal): the
/// inverse of [`chase_look_pitch_deg`].
pub const fn pitch_for_look_deg(look_deg: f64) -> f32 {
    (-(START_PITCH_DEG as f64) + CHASE_START_LOOK_DEG - look_deg) as f32
}

/// When the wanted eye offset from the fly jumps by more than this many chase distances (a
/// region entered, a scripted height), the camera takes it at once instead of gliding.
pub const CHASE_SNAP_DISTANCES: f64 = 4.0;

/// How far below the horizontal the chase camera looks (degrees) for the view `pitch`
/// (radians, negative looks down): [`CHASE_START_LOOK_DEG`] at the start pitch, and one degree
/// further down per degree the pitch drops, so dragging down looks further down, as in M2
/// (controller ruling, fix round 1). The start pose looks 15° down with the fly 5° below the
/// view's centre. [`fly_step`] keeps the pitch within [`MIN_PITCH_DEG`]..=[`MAX_PITCH_DEG`],
/// which maps exactly onto [`CHASE_LOOK_MIN_DEG`]..=[`CHASE_LOOK_MAX_DEG`]; the clamp here only
/// guards scripted poses.
pub fn chase_look_pitch_deg(pitch: f32) -> f64 {
    let start = -f64::from(START_PITCH_DEG);
    (CHASE_START_LOOK_DEG - (f64::from(pitch).to_degrees() - start))
        .clamp(CHASE_LOOK_MIN_DEG, CHASE_LOOK_MAX_DEG)
}

/// One critically damped step of `dt_s` toward a fixed `target`, with time constant `tau_s`
/// (`x'' = −2x'/τ − (x − target)/τ²`, integrated exactly): from rest it never overshoots, and
/// after one time constant `2/e` of the gap remains. Returns the new position and velocity.
pub fn critically_damped(
    x: DVec3,
    v: DVec3,
    target: DVec3,
    dt_s: f64,
    tau_s: f64,
) -> (DVec3, DVec3) {
    let stepping = dt_s > 0.0 && tau_s > 0.0;
    if !stepping {
        return (x, v);
    }
    let omega = 1.0 / tau_s;
    let gap = x - target;
    let decay = (-omega * dt_s).exp();
    let temp = (v + gap * omega) * dt_s;
    (target + (gap + temp) * decay, (v - temp * omega) * decay)
}

/// The chase pose the camera wants for `fly`: [`chase_pose`] behind and above it at
/// [`chase_look_pitch_deg`], then lifted to [`CAMERA_CLEARANCE_M`] over `ground_m(x, z)` (the
/// drawn surface, [`blended_surface_m`]: the higher of the terrain and the water).
pub fn region_chase_pose(fly: &FlyState, ground_m: impl Fn(f64, f64) -> f64) -> ChasePose {
    let p = fly.pos_m.as_dvec3();
    let forward = (Quat::from_rotation_y(fly.yaw) * Vec3::NEG_Z).as_dvec3();
    let agl = p.y - ground_m(p.x, p.z);
    let pose = chase_pose(
        p,
        forward,
        DVec3::Y,
        agl,
        chase_look_pitch_deg(fly.pitch),
        CHASE_MIN_DISTANCE_M,
    );
    lift_above_ground(pose, |e| ground_m(e.x, e.z), f64::from(CAMERA_CLEARANCE_M))
}

/// Half the inner field: a hop crosses the edge here, on either axis.
pub use timefly_core::flight::hop::EDGE_M;

/// Where the fly is and how the view looks: local metres in the region's frame (`+x` east, `−z`
/// north, `+y` up from sea level), `yaw` (the fly's heading) about `+y` from north (positive
/// turns west, as Bevy's rotations go) and `pitch`, the view's tilt (negative looks down; the
/// chase camera reads it through [`chase_look_pitch_deg`]). [`FlyState::transform`] is the M2
/// free-camera pose at that position, kept for scripted drivers and tests.
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
    /// The start of a region (M2 design §6.1): [`CAMERA_START_ALTITUDE_M`] above the drawn
    /// surface at the centre (the higher of the ground and the water), looking north and pitched
    /// down [`START_PITCH_DEG`].
    pub fn start(grid: &RegionGrid) -> FlyState {
        let ground = blended_surface_m(grid, 0.0, 0.0);
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
    /// Turning in [−1, 1], `+1` to the right (`→`), at [`TURN_DEG_PER_S`].
    pub turn: f32,
    pub boost: bool,
    pub dt_s: f32,
    /// Keep the fly inside the inner field ([`timefly_core::flight::hop::confine`]), before the
    /// substeps, so their floors follow only the path it takes.
    pub confined: bool,
}

/// Cruise speed (m/s) at `agl_m` above the drawn surface, before the boost.
pub fn fly_speed(agl_m: f32) -> f32 {
    (agl_m * SPEED_PER_M_AGL).clamp(MIN_SPEED_MPS, MAX_SPEED_MPS)
}

/// The next camera state. `surface_m(x, z)` is the drawn surface's height at local metres, the
/// floor: the region's [`blended_surface_m`] (the ground, or the water standing over it; sea
/// level is `y = 0`). See the module docs for the rules.
pub fn fly_step(state: FlyState, input: FlyInput, surface_m: impl Fn(f32, f32) -> f32) -> FlyState {
    let dt = if input.dt_s.is_finite() {
        input.dt_s.clamp(0.0, MAX_STEP_S)
    } else {
        0.0
    };
    let turn = input.turn.clamp(-1.0, 1.0) * TURN_DEG_PER_S.to_radians() * dt;
    let yaw = state.yaw - input.look_delta.x * LOOK_RADIANS_PER_PX - turn;
    let pitch = (state.pitch - input.look_delta.y * LOOK_RADIANS_PER_PX)
        .clamp(MIN_PITCH_DEG.to_radians(), MAX_PITCH_DEG.to_radians());
    let p = state.pos_m;

    // The direction asked for, in world axes: forward along the heading and strafing, both
    // level (the pitch tilts only the chase view, M3 design §5.2); climbing straight up or down.
    // Diagonals are no faster than a single key.
    let m = input.move_local.clamp(Vec3::splat(-1.0), Vec3::ONE);
    let heading = Quat::from_rotation_y(yaw);
    let forward = heading * Vec3::NEG_Z;
    let wish = heading * Vec3::X * m.x + Vec3::Y * m.y + forward * -m.z;
    let wish = if wish.length_squared() > 1.0 {
        wish.normalize()
    } else {
        wish
    };
    let boost = if input.boost { BOOST } else { 1.0 };
    let mut velocity = wish * fly_speed(p.y - surface_m(p.x, p.z)) * boost;
    if velocity.y > 0.0 {
        velocity.y *= ((CAMERA_CEILING_M - p.y) / CEILING_EASE_M).clamp(CEILING_MIN_EASE, 1.0);
    }

    // Substeps, each ending on or above the floor and under the ceiling.
    let mut delta = velocity * dt;
    if input.confined {
        let from = Vec2::new(p.x, p.z);
        let to = timefly_core::flight::hop::confine(from, from + Vec2::new(delta.x, delta.z));
        delta.x = to.x - from.x;
        delta.z = to.y - from.y;
    }
    let substeps = (Vec2::new(delta.x, delta.z).length() / MAX_SUBSTEP_M)
        .ceil()
        .clamp(1.0, 1_024.0);
    let step = delta / substeps;
    let mut pos = p;
    for _ in 0..substeps as u32 {
        pos += step;
        let floor = surface_m(pos.x, pos.z) + CAMERA_CLEARANCE_M;
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
            .init_resource::<FlyChase>()
            .init_resource::<FlyPose>()
            .init_resource::<FlyAltitude>()
            .configure_sets(
                Update,
                (FlySystems::Step, FlySystems::Apply)
                    .chain()
                    .after(NavigationSystems),
            )
            .configure_sets(Update, FlySystems::Apply.after(TerrainSceneSystems))
            // The cameras switch first, so a swap frame's pose is the active world's.
            .configure_sets(Update, FlySystems::Apply.after(RegionViewSystems))
            .add_systems(Update, step_fly.in_set(FlySystems::Step))
            .add_systems(Update, apply_fly.in_set(FlySystems::Apply));
    }
}

/// What the held keys ask for this frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct KeyInput {
    /// See [`FlyInput::move_local`].
    pub move_local: Vec3,
    /// See [`FlyInput::turn`].
    pub turn: f32,
    pub boost: bool,
}

/// Whether `navigator.platform` names a Mac (`"MacIntel"` in every current Mac browser).
pub fn is_mac_platform(platform: &str) -> bool {
    platform.starts_with("Mac")
}

/// Whether the game runs on a Mac: natively by the target, on the web by `navigator.platform`
/// (read once). A Mac browser has the native app's clash (Mission Control is the system's, so a
/// page cannot take the keys back), and elsewhere a browser's `Alt` with the arrows is Back and
/// Forward, so the web follows the machine, not the build.
pub fn platform_is_mac() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        static MAC: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *MAC.get_or_init(|| {
            web_sys::window()
                .and_then(|w| w.navigator().platform().ok())
                .is_some_and(|p| is_mac_platform(&p))
        })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        cfg!(target_os = "macos")
    }
}

/// The modifier keys that descend (with `Q`, everywhere): `Option` on a Mac, where `Ctrl+↑`
/// opens Mission Control and `Ctrl+←` / `→` switch Spaces; `Ctrl` elsewhere. Not `Cmd`: `⌘Q`
/// quits, and `Q` is the other descend key.
pub fn descend_modifiers(mac: bool) -> [KeyCode; 2] {
    if mac {
        [KeyCode::AltLeft, KeyCode::AltRight]
    } else {
        [KeyCode::ControlLeft, KeyCode::ControlRight]
    }
}

/// The held keys as flight input:
/// - `W` / `↑` forward, `S` / `↓` back, `A` / `D` sideways;
/// - `Space` / `E` up, `Q` or the descend modifier ([`descend_modifiers`]: `Option` on a Mac,
///   `Ctrl` elsewhere) down;
/// - `←` / `→` turn, or fly sideways while `Shift` is held;
/// - `Shift` with forward boosts. It boosts nothing else, since with the arrows it means
///   "sideways".
pub fn keys_to_input(keys: &ButtonInput<KeyCode>) -> KeyInput {
    let held = |codes: &[KeyCode]| keys.any_pressed(codes.iter().copied());
    let axis = |plus: bool, minus: bool| f32::from(u8::from(plus)) - f32::from(u8::from(minus));
    let shift = held(&[KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    let left = held(&[KeyCode::ArrowLeft]);
    let right = held(&[KeyCode::ArrowRight]);
    let (strafe_left, strafe_right) = if shift { (left, right) } else { (false, false) };
    let move_local = Vec3::new(
        axis(
            held(&[KeyCode::KeyD]) || strafe_right,
            held(&[KeyCode::KeyA]) || strafe_left,
        ),
        axis(
            held(&[KeyCode::Space, KeyCode::KeyE]),
            held(&descend_modifiers(platform_is_mac())) || held(&[KeyCode::KeyQ]),
        ),
        axis(
            held(&[KeyCode::KeyS, KeyCode::ArrowDown]),
            held(&[KeyCode::KeyW, KeyCode::ArrowUp]),
        ),
    );
    KeyInput {
        move_local,
        turn: if shift { 0.0 } else { axis(right, left) },
        boost: shift && move_local.z < 0.0,
    }
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
    // Flying, or around a hop's lift, whose height is the lift's (and which slows the fly to a
    // drift while it holds in the cloud).
    let controls = nav.and_then(|n| n.phase.fly_controls());
    let (Some(controls), Some(region)) = (controls, region) else {
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
    let held = keys.as_deref().map(keys_to_input).unwrap_or_default();
    let m = held.move_local;
    let input = FlyInput {
        move_local: Vec3::new(
            m.x * controls.horizontal,
            if controls.vertical { m.y } else { 0.0 },
            m.z * controls.horizontal,
        ),
        look_delta: match (&motion, drag.0) {
            (Some(m), true) => m.delta,
            _ => Vec2::ZERO,
        },
        turn: held.turn,
        boost: held.boost,
        dt_s: time.delta_secs(),
        confined: controls.confined,
    };
    let grid = &region.grid;
    let ground = |x: f32, z: f32| blended_surface_m(grid, f64::from(x), f64::from(z));
    // Inside the inner field (`input.confined`): past its edge only under a hop's opaque cloud.
    let next = fly_step(*fly, input, ground);
    if next != *fly {
        *fly = next;
    }
}

/// The chase camera's glide, shared by the region and the globe: the eye's and the target's
/// **offsets from the fly** glide with [`critically_damped`] smoothing ([`CHASE_SMOOTHING_S`])
/// and are added to where the fly is now. Translation is followed rigidly (a world-space
/// follower lags 2τ·v behind steady flight, which at 0.8–3.2 × AGL per second is as long as the
/// chase distance itself and runs the fly off the top of the frame); only turns, view changes
/// and height changes glide.
#[derive(Clone, Debug, Default)]
pub(crate) struct ChaseGlide {
    /// The smoothed eye and target offsets and their velocities; `None` until a world shows
    /// (and after it goes), so the next one starts in place.
    smooth: Option<[DVec3; 4]>,
}

impl ChaseGlide {
    pub(crate) fn reset(&mut self) {
        self.smooth = None;
    }

    /// The eye and target this frame for the fly at `fly` wanting `want`. `carry` turns the held
    /// offsets first: the rotation of the fly's frame since last frame that is travel, not a
    /// turn (the globe's up turning beneath a fly crossing it; identity in a flat region). A
    /// wanted eye more than [`CHASE_SNAP_DISTANCES`] × `chase_distance` from the held one is
    /// taken at once.
    pub(crate) fn follow(
        &mut self,
        fly: DVec3,
        want: &ChasePose,
        chase_distance: f64,
        carry: DQuat,
        dt_s: f64,
    ) -> (DVec3, DVec3) {
        let (want_eye, want_target) = (want.eye - fly, want.target - fly);
        let [eye_off, eye_v, target_off, target_v] = match self.smooth {
            Some(held) => {
                let [eye, eye_v, target, target_v] = held.map(|v| carry * v);
                if (eye - want_eye).length() <= CHASE_SNAP_DISTANCES * chase_distance {
                    let (eye, eye_v) =
                        critically_damped(eye, eye_v, want_eye, dt_s, CHASE_SMOOTHING_S);
                    let (target, target_v) =
                        critically_damped(target, target_v, want_target, dt_s, CHASE_SMOOTHING_S);
                    [eye, eye_v, target, target_v]
                } else {
                    [want_eye, DVec3::ZERO, want_target, DVec3::ZERO]
                }
            }
            None => [want_eye, DVec3::ZERO, want_target, DVec3::ZERO],
        };
        self.smooth = Some([eye_off, eye_v, target_off, target_v]);
        (fly + eye_off, fly + target_off)
    }
}

/// The fly's drawn-only motion, shared by the region and the globe: the bank into turns
/// ([`bank_step`]), the pitch with climbs ([`pitch_step`]) and the idle bob ([`hover_offset`]),
/// which fades in while the fly is left alone. None of them moves the flight state.
#[derive(Clone, Debug, Default)]
pub(crate) struct FlyFlourish {
    bank_rad: f32,
    pitch_rad: f32,
    /// How much of the idle bob shows, 0 to 1.
    hover: f32,
}

impl FlyFlourish {
    /// Steps the bank toward `yaw_rate` (rad/s, positive to the left), the pitch toward
    /// `climb` (the vertical speed over the climb keys' speed, [`pitch_step`]) and the bob toward
    /// `idle`; returns the bank, the pitch and the bob's height (along up) for a fly drawn at
    /// `scale`.
    pub(crate) fn step(
        &mut self,
        turn: FlyTurn,
        dt_s: f32,
        t_s: f32,
        scale: f32,
    ) -> (f32, f32, f32) {
        self.bank_rad = bank_step(self.bank_rad, turn.yaw_rate, dt_s);
        self.pitch_rad = pitch_step(self.pitch_rad, turn.climb, dt_s);
        let ease = 1.0 - (-dt_s / HOVER_EASE_S).exp();
        self.hover += (f32::from(u8::from(turn.idle)) - self.hover) * ease;
        (
            self.bank_rad,
            self.pitch_rad,
            self.hover * hover_offset(t_s, scale),
        )
    }
}

/// How the fly moved over the last frame, for [`FlyFlourish::step`].
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct FlyTurn {
    /// The heading's turn rate (rad/s, positive to the left).
    pub(crate) yaw_rate: f32,
    /// The vertical speed over the speed the climb keys fly at (1 climbing at full speed).
    pub(crate) climb: f32,
    /// Whether the fly did not move at all.
    pub(crate) idle: bool,
}

/// The chase camera's smoothed pose and the fly's visual state between frames.
#[derive(Resource, Clone, Debug, Default)]
pub(crate) struct FlyChase {
    glide: ChaseGlide,
    /// The fly's state last frame, for the yaw rate and whether it is idle.
    last: Option<FlyState>,
    flourish: FlyFlourish,
}

impl FlyChase {
    /// Forgets the glide, the last state and the flourish: the next frame starts at the wanted
    /// pose (a placement into the region: the dive's swap, the launch region).
    pub(crate) fn reset(&mut self) {
        *self = FlyChase::default();
    }
}

/// Places the region camera behind and above [`FlyState`] (smoothed, and always clear of the
/// ground) and writes the dragonfly's [`FlyPose`] and the overlay's [`FlyAltitude`].
pub(crate) fn apply_fly(
    fly: Res<FlyState>,
    scene: Option<Res<TerrainScene>>,
    time: Res<Time>,
    mut chase: ResMut<FlyChase>,
    mut pose: ResMut<FlyPose>,
    mut altitude: ResMut<FlyAltitude>,
    mut cameras: Query<(&mut Transform, &Camera)>,
) {
    let region = scene
        .as_ref()
        .and_then(|s| cameras.get_mut(s.camera).ok().map(|c| (s, c)));
    let Some((scene, (mut camera, camera_settings))) = region else {
        chase.glide.reset();
        chase.last = None;
        if matches!(altitude.0, Some(Altitude::Metres(_))) {
            altitude.0 = None;
        }
        return;
    };
    let grid = &scene.grid;
    let ground = |x: f64, z: f64| f64::from(blended_surface_m(grid, x, z));
    let want = region_chase_pose(&fly, ground);
    let dt = time.delta_secs_f64();

    // Smooth in the fly's frame (`ChaseGlide`); the result is lifted again, so the glide never
    // dips into the ground.
    let fly_m = fly.pos_m.as_dvec3();
    let chase_distance =
        CHASE_MIN_DISTANCE_M.max(CHASE_DISTANCE_PER_AGL * (fly_m.y - ground(fly_m.x, fly_m.z)));
    let (eye, target) = chase
        .glide
        .follow(fly_m, &want, chase_distance, DQuat::IDENTITY, dt);
    let shown = lift_above_ground(
        ChasePose {
            eye,
            target,
            up: DVec3::Y,
        },
        |e| ground(e.x, e.z),
        f64::from(CAMERA_CLEARANCE_M),
    );
    let want_camera = Transform::from_translation(shown.eye.as_vec3())
        .looking_at(shown.target.as_vec3(), Vec3::Y);
    if *camera != want_camera {
        *camera = want_camera;
    }

    // The fly: its size on screen held by the camera's distance; banked by its turn rate,
    // pitched by its climb and bobbing while left alone (all drawn only, never moving
    // `FlyState`).
    let dt_s = time.delta_secs();
    let turn = match chase.last {
        Some(last) if dt_s > 0.0 => {
            // Against the speed the climb keys flew at from last frame's height.
            let full = fly_speed(
                last.pos_m.y - ground(f64::from(last.pos_m.x), f64::from(last.pos_m.z)) as f32,
            );
            FlyTurn {
                yaw_rate: (fly.yaw - last.yaw) / dt_s,
                climb: (fly.pos_m.y - last.pos_m.y) / dt_s / full,
                idle: last == *fly,
            }
        }
        Some(last) => FlyTurn {
            idle: last == *fly,
            ..default()
        },
        None => FlyTurn {
            idle: true,
            ..default()
        },
    };
    chase.last = Some(*fly);
    let scale = fly_scale(
        (shown.eye - fly_m).length(),
        f64::from(REGION_FOV_DEG.to_radians()),
    ) as f32;
    let (bank_rad, pitch_rad, bob) = chase.flourish.step(turn, dt_s, time.elapsed_secs(), scale);
    let next = FlyPose {
        translation: fly.pos_m + Vec3::Y * bob,
        rotation: Quat::from_rotation_y(fly.yaw),
        scale,
        bank_rad,
        pitch_rad,
    };
    // Only the world on screen writes the pose: a region held while the globe draws (a
    // preload, a swap) leaves it to the globe.
    if camera_settings.is_active && *pose != next {
        *pose = next;
    }

    // The overlay shows the fly's altitude while the region is on screen.
    let reading = if camera_settings.is_active {
        Some(Altitude::Metres(fly.pos_m.y))
    } else if matches!(altitude.0, Some(Altitude::Metres(_))) {
        None
    } else {
        altitude.0
    };
    if altitude.0 != reading {
        altitude.0 = reading;
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
            turn: 0.0,
            boost,
            dt_s,
            confined: false,
        }
    }

    fn held(codes: &[KeyCode]) -> KeyInput {
        let mut keys = ButtonInput::<KeyCode>::default();
        for &code in codes {
            keys.press(code);
        }
        keys_to_input(&keys)
    }

    fn floor(surface: &impl Fn(f32, f32) -> f32, p: Vec3) -> f32 {
        surface(p.x, p.z) + CAMERA_CLEARANCE_M
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

        /// After every step, the camera is at least the clearance above the surface and at
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
                    turn: 0.0,
                    // Most steps boost: the brief's worst case.
                    boost: boost || dt_s > 0.1,
                    dt_s,
                    confined: false,
                };
                state = fly_step(state, step, &ground);
                let p = state.pos_m;
                prop_assert!(p.is_finite(), "{state:?}");
                prop_assert!(p.y >= floor(&ground, p), "{p} under {}", floor(&ground, p));
                prop_assert!(p.y <= CAMERA_CEILING_M, "{p} over the ceiling");
                let deg = state.pitch.to_degrees();
                prop_assert!((MIN_PITCH_DEG - 1e-3..=MAX_PITCH_DEG + 1e-3).contains(&deg));
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
        // Height counts from the surface, wherever it is: over a dry basin below sea level, from
        // its floor.
        let basin = fly_step(
            FlyState {
                pos_m: Vec3::new(0.0, 1_000.0, 0.0),
                yaw: 0.0,
                pitch: 0.0,
            },
            input(forward, false, 0.1),
            flat(-300.0),
        );
        assert!((-basin.pos_m.z / 0.1 - fly_speed(1_300.0)).abs() < 0.5);
        // The limits.
        assert_eq!(fly_speed(0.0), MIN_SPEED_MPS);
        assert_eq!(fly_speed(1.0e6), MAX_SPEED_MPS);
    }

    #[test]
    fn a_confined_step_stops_at_the_edge_and_climbs_nothing_beyond_it() {
        // Flat inside; a 20 km wall just past the east edge, which a confined fly never reaches.
        let ground = |x: f32, _z: f32| if x > EDGE_M + 100.0 { 20_000.0 } else { 100.0 };
        let s = FlyState {
            pos_m: Vec3::new(EDGE_M - 100.0, 5_000.0, 0.0),
            yaw: -std::f32::consts::FRAC_PI_2, // facing east
            pitch: 0.0,
        };
        let step = FlyInput {
            confined: true,
            ..input(Vec3::new(0.0, 0.0, -1.0), true, 0.1)
        };
        let next = fly_step(s, step, ground);
        assert_eq!(next.pos_m.x, EDGE_M, "{next:?}");
        assert_eq!(
            next.pos_m.y, 5_000.0,
            "lifted over ground it never reached: {next:?}"
        );
        // Unconfined, the same step flies out over the wall.
        let free = fly_step(s, input(Vec3::new(0.0, 0.0, -1.0), true, 0.1), ground);
        assert!(
            free.pos_m.x > EDGE_M + 1_000.0 && free.pos_m.y > 20_000.0,
            "{free:?}"
        );
    }

    #[test]
    fn fly_step_has_no_edge() {
        // The step itself neither pushes back nor stops at the field's edge (M3 Task 9): the
        // shell keeps the fly inside it (`FlyControls::confined`) except under a hop's cloud.
        let ground = flat(100.0);
        let out = FlyState {
            pos_m: Vec3::new(EDGE_M + 10_000.0, 5_000.0, -EDGE_M - 4_000.0),
            yaw: 0.0,
            pitch: 0.0,
        };
        assert_eq!(fly_step(out, input(Vec3::ZERO, false, 0.1), &ground), out);
        let mut s = FlyState {
            pos_m: Vec3::new(EDGE_M - 1_000.0, 8_000.0, 0.0),
            yaw: -std::f32::consts::FRAC_PI_2, // facing east
            pitch: 0.0,
        };
        for _ in 0..20 {
            let next = fly_step(s, input(Vec3::new(0.0, 0.0, -1.0), false, 0.1), &ground);
            assert!(
                (next.pos_m.x - s.pos_m.x - fly_speed(8_000.0 - 100.0) * 0.1).abs() < 1.0,
                "{next:?}"
            );
            s = next;
        }
        assert!(s.pos_m.x > EDGE_M + 10_000.0, "{s:?}");
    }

    #[test]
    fn look_turns_and_stays_within_the_chase_range() {
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
        assert_eq!(look(0.0, 1.0e6).pitch, MIN_PITCH_DEG.to_radians());
        assert_eq!(look(0.0, -1.0e6).pitch, MAX_PITCH_DEG.to_radians());
        assert_eq!(look(0.0, 0.0).pos_m, s.pos_m);
    }

    #[test]
    fn moves_follow_the_heading_and_the_keys() {
        // M3: the drag tilts the view, not the flight path (design §5.2, brief Task 3), so `W`
        // flies level along the heading even while the view looks down. (M2 flew along the
        // pitched view.)
        let s = FlyState {
            pos_m: Vec3::new(0.0, 5_000.0, 0.0),
            yaw: 0.0,
            pitch: -START_PITCH_DEG.to_radians(),
        };
        let step = |m: Vec3| fly_step(s, input(m, false, 0.1), flat(0.0)).pos_m - s.pos_m;
        // W flies north, level.
        let w = step(Vec3::new(0.0, 0.0, -1.0));
        assert!(w.z < 0.0 && w.y.abs() < 1e-3 && w.x.abs() < 1e-3, "{w}");
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
    fn forward_flight_is_level_whatever_the_view_pitch() {
        let level = FlyState {
            pos_m: Vec3::new(0.0, 5_000.0, 0.0),
            yaw: 0.7,
            pitch: 0.0,
        };
        let reference = fly_step(
            level,
            input(Vec3::new(0.0, 0.0, -1.0), false, 0.1),
            flat(0.0),
        );
        for deg in [MIN_PITCH_DEG, -35.0, -START_PITCH_DEG, -5.0, MAX_PITCH_DEG] {
            let s = FlyState {
                pitch: deg.to_radians(),
                ..level
            };
            for m in [
                Vec3::new(0.0, 0.0, -1.0),
                Vec3::new(0.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, -1.0),
            ] {
                let moved = fly_step(s, input(m, true, 0.1), flat(0.0)).pos_m - s.pos_m;
                assert!(moved.y.abs() < 1e-3, "pitch {deg}: {m} moved {moved}");
                assert!(Vec2::new(moved.x, moved.z).length() > 1.0, "{deg}: {moved}");
            }
            // The same ground covered as with a level view.
            let w = fly_step(s, input(Vec3::new(0.0, 0.0, -1.0), false, 0.1), flat(0.0));
            assert!((w.pos_m - reference.pos_m).length() < 1e-2, "{deg}: {w:?}");
        }
    }

    #[test]
    fn dragging_down_looks_further_down() {
        let s = FlyState::default();
        let dragged = |dy: f32| {
            fly_step(
                s,
                FlyInput {
                    look_delta: Vec2::new(0.0, dy),
                    dt_s: 0.016,
                    ..default()
                },
                flat(0.0),
            )
        };
        let start = chase_look_pitch_deg(s.pitch);
        assert!(chase_look_pitch_deg(dragged(20.0).pitch) > start);
        assert!(chase_look_pitch_deg(dragged(-20.0).pitch) < start);
        // Monotonic over the whole range, and the range ends exactly at the look limits: no
        // dead zone where the pitch moves but the view does not.
        let mut last = f64::MIN;
        for k in 0..=100 {
            let deg = MAX_PITCH_DEG + (MIN_PITCH_DEG - MAX_PITCH_DEG) * k as f32 / 100.0;
            let look = chase_look_pitch_deg(deg.to_radians());
            assert!(look > last, "{deg}: {look} after {last}");
            last = look;
        }
        assert!(
            (chase_look_pitch_deg(MIN_PITCH_DEG.to_radians()) - CHASE_LOOK_MAX_DEG).abs() < 1e-4
        );
        assert!(
            (chase_look_pitch_deg(MAX_PITCH_DEG.to_radians()) - CHASE_LOOK_MIN_DEG).abs() < 1e-4
        );
        assert_eq!((MIN_PITCH_DEG, MAX_PITCH_DEG), (-50.0, 0.0));
    }

    #[test]
    fn at_the_start_pitch_the_fly_sits_a_little_below_centre() {
        let s = FlyState::start(&RegionGrid {
            spec: timefly_core::worldgen::generate::RegionSpec::new(
                timefly_core::geo::geo_to_render(timefly_core::geo::latlon_to_geo(46.5, 10.0)),
                timefly_core::deep_time::YearsBp(0.0),
                1,
            ),
            inner: flat_field(timefly_core::worldgen::REGION_SIZE_M),
            apron: flat_field(timefly_core::worldgen::APRON_SIZE_M),
            boulders: Vec::new(),
        });
        let look = chase_look_pitch_deg(s.pitch);
        assert!((look - CHASE_START_LOOK_DEG).abs() < 1e-4, "{look}");
        let pose = region_chase_pose(&s, |_, _| 0.0);
        // The target sits above the fly on its vertical, so the fly shows below the centre:
        // by the elevation less the look, 5°.
        assert!(pose.target.y > f64::from(s.pos_m.y), "{pose:?}");
        let to_fly = (s.pos_m.as_dvec3() - pose.eye).normalize();
        let to_target = (pose.target - pose.eye).normalize();
        let below = to_fly.angle_between(to_target).to_degrees();
        assert!((below - 5.0).abs() < 1e-3, "{below}");
    }

    fn flat_field(size_m: f64) -> timefly_core::worldgen::generate::Field {
        timefly_core::worldgen::generate::Field::dry(3, size_m, vec![0.0; 9], vec![[0, 0, 0, 0]; 9])
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

    /// Ruling P3: the climb out triggers at `REGION_CLOUD_BASE_M − 1 km`, so the eased ceiling
    /// must reach that height (and hold at the base), not creep toward it forever.
    #[test]
    fn the_eased_ceiling_reaches_the_climb_trigger() {
        use timefly_core::flight::altitudes::{REGION_CLOUD_BAND_M, REGION_CLOUD_BASE_M};
        assert_eq!(CAMERA_CEILING_M, REGION_CLOUD_BASE_M as f32);
        assert_eq!(CEILING_EASE_M, REGION_CLOUD_BAND_M as f32);
        let trigger = (REGION_CLOUD_BASE_M - 1_000.0) as f32;
        let mut s = FlyState {
            pos_m: Vec3::new(0.0, CAMERA_CEILING_M - CEILING_EASE_M, 0.0),
            yaw: 0.0,
            pitch: 0.0,
        };
        // Holding climb, unboosted, at 60 fps: the trigger within 30 s.
        let mut reached = None;
        for frame in 0..60 * 30 {
            s = fly_step(s, input(Vec3::Y, false, 1.0 / 60.0), flat(0.0));
            assert!(s.pos_m.y <= CAMERA_CEILING_M, "{}", s.pos_m);
            if s.pos_m.y >= trigger && reached.is_none() {
                reached = Some(frame);
            }
        }
        assert!(reached.is_some(), "stuck at {} m", s.pos_m.y);
        assert_eq!(s.pos_m.y, CAMERA_CEILING_M, "held at the base");
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
        assert_eq!(keys_to_input(&keys), KeyInput::default());
        keys.press(KeyCode::KeyW);
        keys.press(KeyCode::KeyD);
        keys.press(KeyCode::KeyQ);
        assert_eq!(keys_to_input(&keys).move_local, Vec3::new(1.0, -1.0, -1.0));
        keys.press(KeyCode::KeyA);
        keys.press(KeyCode::Space);
        keys.release(KeyCode::KeyW);
        keys.press(KeyCode::KeyS);
        // Opposite keys cancel.
        assert_eq!(keys_to_input(&keys).move_local, Vec3::new(0.0, 0.0, 1.0));
    }

    #[test]
    fn descend_is_option_on_macos_and_ctrl_elsewhere() {
        // On a Mac `Ctrl+↑` opens Mission Control (and `Ctrl+←/→` switch Spaces), so descend is
        // `Option` there; `Ctrl` elsewhere. Not `Cmd`: `⌘Q` quits, and `Q` descends too.
        let option = [KeyCode::AltLeft, KeyCode::AltRight];
        let ctrl = [KeyCode::ControlLeft, KeyCode::ControlRight];
        assert_eq!(descend_modifiers(true), option);
        assert_eq!(descend_modifiers(false), ctrl);
        // The web asks `navigator.platform`.
        for (platform, mac) in [
            ("MacIntel", true),
            ("MacPPC", true),
            ("Win32", false),
            ("Linux x86_64", false),
            ("iPhone", false),
            ("", false),
        ] {
            assert_eq!(is_mac_platform(platform), mac, "{platform:?}");
        }
        // Natively, the target decides.
        let mac = cfg!(target_os = "macos");
        assert_eq!(platform_is_mac(), mac);
        let (descends, inert) = if mac { (option, ctrl) } else { (ctrl, option) };
        for key in descends.into_iter().chain([KeyCode::KeyQ]) {
            assert_eq!(
                held(&[key]).move_local,
                Vec3::new(0.0, -1.0, 0.0),
                "{key:?}"
            );
        }
        for key in inert {
            assert_eq!(held(&[key]).move_local, Vec3::ZERO, "{key:?}");
        }
        // Descending while flying forward with the arrow: the case Mission Control took.
        let modifier = descends[0];
        assert_eq!(
            held(&[modifier, KeyCode::ArrowUp]).move_local,
            Vec3::new(0.0, -1.0, -1.0)
        );
    }

    #[test]
    fn arrows_move_forward_and_back_and_turn() {
        let forward = held(&[KeyCode::ArrowUp]);
        assert_eq!(forward.move_local, Vec3::new(0.0, 0.0, -1.0));
        assert_eq!((forward.turn, forward.boost), (0.0, false));
        assert_eq!(
            held(&[KeyCode::ArrowDown]).move_local,
            Vec3::new(0.0, 0.0, 1.0)
        );
        let left = held(&[KeyCode::ArrowLeft]);
        assert_eq!((left.move_local, left.turn), (Vec3::ZERO, -1.0));
        let right = held(&[KeyCode::ArrowRight]);
        assert_eq!((right.move_local, right.turn), (Vec3::ZERO, 1.0));
        // Arrows and letters add up and clamp like any two keys for one direction.
        assert_eq!(
            held(&[KeyCode::ArrowUp, KeyCode::KeyW]).move_local,
            Vec3::new(0.0, 0.0, -1.0)
        );
    }

    #[test]
    fn shift_turns_left_and_right_arrows_into_strafing() {
        for shift in [KeyCode::ShiftLeft, KeyCode::ShiftRight] {
            let left = held(&[shift, KeyCode::ArrowLeft]);
            assert_eq!(
                (left.move_local, left.turn),
                (Vec3::new(-1.0, 0.0, 0.0), 0.0)
            );
            let right = held(&[shift, KeyCode::ArrowRight]);
            assert_eq!(
                (right.move_local, right.turn),
                (Vec3::new(1.0, 0.0, 0.0), 0.0)
            );
        }
    }

    #[test]
    fn boost_needs_shift_with_forward() {
        assert!(held(&[KeyCode::ShiftLeft, KeyCode::ArrowUp]).boost);
        assert!(held(&[KeyCode::ShiftRight, KeyCode::KeyW]).boost);
        for keys in [
            &[KeyCode::ShiftLeft][..],
            &[KeyCode::ArrowUp],
            &[KeyCode::ShiftLeft, KeyCode::ArrowDown],
            &[KeyCode::ShiftLeft, KeyCode::ArrowRight],
            &[KeyCode::ShiftLeft, KeyCode::KeyA],
            &[KeyCode::ShiftLeft, KeyCode::Space],
            // Forward and back cancel, so there is nothing to boost.
            &[KeyCode::ShiftLeft, KeyCode::ArrowUp, KeyCode::KeyS],
        ] {
            assert!(!held(keys).boost, "{keys:?}");
        }
    }

    #[test]
    fn turning_turns_like_a_drag() {
        let s = FlyState::default();
        let turned = |turn: f32| {
            fly_step(
                s,
                FlyInput {
                    turn,
                    dt_s: 0.5,
                    ..input(Vec3::ZERO, false, 0.5)
                },
                flat(0.0),
            )
        };
        let right = turned(1.0);
        let expected = TURN_DEG_PER_S.to_radians() * 0.5;
        assert!((s.yaw - right.yaw - expected).abs() < 1e-6, "{right:?}");
        assert!((turned(-1.0).yaw - s.yaw - expected).abs() < 1e-6);
        // A drag to the right turns the same way as the right arrow.
        let dragged = fly_step(
            s,
            FlyInput {
                look_delta: Vec2::new(100.0, 0.0),
                ..input(Vec3::ZERO, false, 0.5)
            },
            flat(0.0),
        );
        assert!(dragged.yaw < s.yaw && right.yaw < s.yaw);
        assert_eq!(right.pos_m, s.pos_m, "turning alone does not move");
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

    #[test]
    fn critically_damped_smoothing_settles_without_overshoot() {
        let target = DVec3::new(100.0, -40.0, 7.0);
        let (mut x, mut v) = (DVec3::ZERO, DVec3::ZERO);
        let dt = 1.0 / 120.0;
        let mut t = 0.0;
        while t < CHASE_SMOOTHING_S - 1e-9 {
            (x, v) = critically_damped(x, v, target, dt, CHASE_SMOOTHING_S);
            t += dt;
            // Never past the target on any axis.
            for i in 0..3 {
                assert!(
                    x[i].abs() <= target[i].abs() + 1e-9,
                    "{x} overshot {target}"
                );
            }
        }
        // After one time constant, (1 + 1) e^(−1) of the gap remains.
        let left = (target - x).length() / target.length();
        assert!((left - 2.0 / std::f64::consts::E).abs() < 1e-6, "{left}");
        // Exact for a fixed target: one long step equals many short ones.
        let (one, _) = critically_damped(DVec3::ZERO, DVec3::ZERO, target, t, CHASE_SMOOTHING_S);
        assert!((one - x).length() < 1e-9, "{one} vs {x}");
        for _ in 0..600 {
            (x, v) = critically_damped(x, v, target, dt, CHASE_SMOOTHING_S);
        }
        assert!((x - target).length() < 1e-6 && v.length() < 1e-6);
        // A zero step changes nothing.
        assert_eq!(critically_damped(x, v, DVec3::ONE, 0.0, 0.2), (x, v));
    }

    /// The chase camera in a headless app: the real terrain scene (its `RegionCamera`) over a
    /// fake grid, the fly camera's systems and a fixed 60 Hz step.
    mod chase {
        use std::sync::Arc;

        use bevy::asset::AssetPlugin;
        use bevy::render::storage::ShaderBuffer;
        use bevy::time::TimeUpdateStrategy;
        use timefly_core::deep_time::YearsBp;
        use timefly_core::flight::altitudes::{
            CHASE_DISTANCE_PER_AGL, CHASE_ELEVATION_DEG, CHASE_MIN_DISTANCE_M,
        };
        use timefly_core::flight::chase::fly_scale;
        use timefly_core::geo::{geo_to_render, latlon_to_geo};
        use timefly_core::worldgen::frame::RegionFrame;
        use timefly_core::worldgen::generate::{Field, RegionSpec};
        use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

        use super::super::*;
        use crate::creature::FlyPose;
        use crate::terrain::add_terrain_systems;
        use crate::terrain::material::TerrainMaterial;
        use crate::terrain::view::{REGION_FOV_DEG, RegionCamera};
        use crate::ui::altitude::{Altitude, FlyAltitude};
        use timefly_core::terrain::{blended_height_m, blended_water};

        const STEP: std::time::Duration = std::time::Duration::from_micros(16_667);

        fn field(res: u32, size_m: f64, height: impl Fn(f64, f64) -> f32) -> Field {
            let spacing = size_m / f64::from(res - 1);
            let mut elevation_m = Vec::new();
            for row in 0..res {
                for col in 0..res {
                    let x = -size_m / 2.0 + f64::from(col) * spacing;
                    let z = -size_m / 2.0 + f64::from(row) * spacing;
                    elevation_m.push(height(x, z));
                }
            }
            Field::dry(
                res,
                size_m,
                elevation_m,
                vec![[0, 0, 0, 0]; (res * res) as usize],
            )
        }

        /// A grid with heights `height(x, z)`. The apron is fine enough (9.4 km) that the blend
        /// toward it starts 112 km out, beyond anywhere these tests fly.
        fn grid(height: impl Fn(f64, f64) -> f32 + Copy) -> Arc<RegionGrid> {
            let center = geo_to_render(latlon_to_geo(46.5, 10.0));
            Arc::new(RegionGrid {
                spec: RegionSpec {
                    inner_res: 257,
                    apron_res: 257,
                    ..RegionSpec::new(center, YearsBp(0.0), 1)
                },
                inner: field(257, REGION_SIZE_M, height),
                apron: field(257, APRON_SIZE_M, height),
                boulders: Vec::new(),
            })
        }

        fn app(grid: &Arc<RegionGrid>, fly: FlyState) -> App {
            let mut app = App::new();
            app.add_plugins((MinimalPlugins, AssetPlugin::default()))
                .init_asset::<Image>()
                .init_asset::<Mesh>()
                .init_asset::<ShaderBuffer>()
                .init_asset::<TerrainMaterial>()
                .insert_resource(TimeUpdateStrategy::ManualDuration(STEP));
            add_terrain_systems(&mut app);
            app.add_plugins(FlyCameraPlugin)
                .insert_resource(ActiveRegion {
                    grid: grid.clone(),
                    frame: RegionFrame {
                        center: grid.spec.center,
                    },
                })
                .insert_resource(fly);
            app
        }

        fn camera(app: &mut App) -> Transform {
            let world = app.world_mut();
            *world
                .query_filtered::<&Transform, With<RegionCamera>>()
                .single(world)
                .expect("one region camera")
        }

        fn ground(grid: &RegionGrid, p: Vec3) -> f32 {
            blended_surface_m(grid, f64::from(p.x), f64::from(p.z))
        }

        /// The angle (degrees) by which `p` sits below the camera's view centre, and its
        /// sideways offset (as a sine) from the view's vertical plane.
        fn seen(cam: &Transform, p: Vec3) -> (f32, f32) {
            let to = (p - cam.translation).normalize();
            let forward = cam.forward().as_vec3();
            let below = -(to.dot(cam.up().as_vec3()))
                .atan2(to.dot(forward))
                .to_degrees();
            (below, to.dot(cam.right().as_vec3()))
        }

        #[test]
        fn the_region_camera_chases_the_fly() {
            let grid = grid(|_, _| 500.0);
            let agl = 4_000.0;
            let fly = FlyState {
                pos_m: Vec3::new(1_000.0, 500.0 + agl, -2_000.0),
                yaw: 0.3,
                pitch: -START_PITCH_DEG.to_radians(),
            };
            let mut app = app(&grid, fly);
            for _ in 0..10 {
                app.update();
            }
            let cam = camera(&mut app);
            let heading = Quat::from_rotation_y(fly.yaw) * Vec3::NEG_Z;
            let to_fly = fly.pos_m - cam.translation;
            // Behind (along the heading), above, and at the chase distance.
            assert!(to_fly.dot(heading) > 0.0, "{cam:?} is not behind {fly:?}");
            assert!(cam.translation.y > fly.pos_m.y, "{cam:?} is not above");
            let d = to_fly.length();
            let want = CHASE_DISTANCE_PER_AGL as f32 * agl;
            assert!((d - want).abs() < 1e-3 * want, "distance {d} vs {want}");
            let rise = (-to_fly.y).atan2(to_fly.dot(heading)).to_degrees();
            assert!((rise - CHASE_ELEVATION_DEG as f32).abs() < 0.05, "{rise}");
            // No roll, and the fly is in view: on the view's vertical centre line, below the
            // centre by the elevation less the look-down pitch.
            assert!(cam.right().y.abs() < 1e-5, "the camera rolled: {cam:?}");
            let (below, side) = seen(&cam, fly.pos_m);
            let look = chase_look_pitch_deg(fly.pitch) as f32;
            assert!(side.abs() < 1e-4, "{side}");
            assert!(
                (below - (CHASE_ELEVATION_DEG as f32 - look)).abs() < 0.05,
                "{below}"
            );
            assert!(below.abs() < REGION_FOV_DEG / 2.0, "out of view: {below}");
            // At a 25° drag down from level (look = elevation) the camera looks straight at
            // the fly.
            app.world_mut().resource_mut::<FlyState>().pitch = (-25f32).to_radians();
            for _ in 0..180 {
                app.update();
            }
            let (below, side) = seen(&camera(&mut app), fly.pos_m);
            assert!(below.abs() < 0.05 && side.abs() < 1e-4, "{below} {side}");
            // The fly is drawn where it is, at the scale that holds its size on screen.
            let cam = camera(&mut app);
            let pose = *app.world().resource::<FlyPose>();
            let scale = fly_scale(
                f64::from((fly.pos_m - cam.translation).length()),
                f64::from(REGION_FOV_DEG.to_radians()),
            ) as f32;
            assert!(
                (pose.scale - scale).abs() < 1e-3 * scale,
                "{pose:?} vs {scale}"
            );
            let bob = crate::creature::HOVER_AMPLITUDE * scale;
            assert!(
                (pose.translation - fly.pos_m).length() <= bob + 1e-2,
                "{pose:?}"
            );
            let heading_of = |q: Quat| q * Vec3::NEG_Z;
            assert!(
                heading_of(pose.rotation).abs_diff_eq(heading, 1e-4),
                "{pose:?}"
            );
            // The overlay reads the fly's altitude.
            assert_eq!(
                app.world().resource::<FlyAltitude>().0,
                Some(Altitude::Metres(fly.pos_m.y))
            );
        }

        /// The bearing (radians, as `yaw`) of the camera from the fly: `rot_y(yaw) · +z` is behind.
        fn bearing(cam: &Transform, fly: Vec3) -> f32 {
            let o = cam.translation - fly;
            o.x.atan2(o.z)
        }

        #[test]
        fn the_chase_camera_follows_moves_rigidly_and_turns_smoothly() {
            let grid = grid(|_, _| 0.0);
            let fly = FlyState {
                pos_m: Vec3::new(0.0, 2_000.0, 0.0),
                yaw: 0.0,
                pitch: -START_PITCH_DEG.to_radians(),
            };
            let mut app = app(&grid, fly);
            for _ in 0..10 {
                app.update();
            }
            let offset = camera(&mut app).translation - fly.pos_m;
            // A move is followed in the same frame: the camera keeps its place behind the fly.
            app.world_mut().resource_mut::<FlyState>().pos_m.z -= 100.0;
            app.update();
            let moved = *app.world().resource::<FlyState>();
            let now = camera(&mut app).translation - moved.pos_m;
            assert!((now - offset).length() < 1e-2, "{now} vs {offset}");
            // A turn glides: the camera swings round behind the fly over a few frames, never
            // past the new heading.
            app.world_mut().resource_mut::<FlyState>().yaw = 0.5;
            app.update();
            let first = bearing(&camera(&mut app), moved.pos_m);
            assert!(first > 0.0 && first < 0.25, "{first}");
            let mut last = first;
            for _ in 0..150 {
                app.update();
                let b = bearing(&camera(&mut app), moved.pos_m);
                assert!(b >= last - 1e-5 && b <= 0.5 + 1e-4, "{b} after {last}");
                last = b;
            }
            assert!((last - 0.5).abs() < 1e-3, "{last}");
            // A jump of many chase distances (entering a region, a script) is followed at once.
            app.world_mut().resource_mut::<FlyState>().pos_m.x += 50_000.0;
            app.update();
            let fly = *app.world().resource::<FlyState>();
            let cam = camera(&mut app);
            let (below, side) = seen(&cam, fly.pos_m);
            assert!(
                (below - 5.0).abs() < 0.05 && side.abs() < 1e-3,
                "{below} {side}"
            );
        }

        #[test]
        fn the_fly_stays_in_frame_in_sustained_flight() {
            // Steady level flight north for 2 s, plain and boosted, at the steepest, start and
            // shallowest view pitches: the fly never leaves the frame, and at the start pitch it
            // holds its place a little below the centre.
            let grid = grid(|_, _| 0.0);
            let agl = 2_000.0;
            let dt = STEP.as_secs_f32();
            for pitch_deg in [MIN_PITCH_DEG, -START_PITCH_DEG, MAX_PITCH_DEG] {
                for boost in [false, true] {
                    let mut state = FlyState {
                        pos_m: Vec3::new(0.0, agl, 60_000.0),
                        yaw: 0.0,
                        pitch: pitch_deg.to_radians(),
                    };
                    let mut app = app(&grid, state);
                    for _ in 0..5 {
                        app.update();
                    }
                    let speed = fly_speed(agl) * if boost { BOOST } else { 1.0 };
                    let at_rest =
                        CHASE_ELEVATION_DEG as f32 - chase_look_pitch_deg(state.pitch) as f32;
                    for frame in 0..120 {
                        state.pos_m.z -= speed * dt;
                        app.world_mut().insert_resource(state);
                        app.update();
                        let (below, side) = seen(&camera(&mut app), state.pos_m);
                        let what = format!("pitch {pitch_deg}, boost {boost}, frame {frame}");
                        assert!(
                            below.abs() <= REGION_FOV_DEG / 2.0 - 4.0,
                            "{what}: {below}° from the centre"
                        );
                        assert!(side.abs() < 1e-3, "{what}: {side}");
                        // The view holds the fly where it sits at rest.
                        assert!(
                            (below - at_rest).abs() < 0.1,
                            "{what}: {below} vs {at_rest}"
                        );
                        if pitch_deg == -START_PITCH_DEG {
                            assert!(below > 4.0, "{what}: {below}");
                        }
                    }
                }
            }
        }

        #[test]
        fn the_fly_never_hides_behind_a_ridge() {
            // Flat ground north of z = 2 km, then a 76° slope climbing to 40 km behind the fly.
            let slope = |_: f64, z: f64| ((z - 2_000.0) * 4.0).clamp(0.0, 40_000.0) as f32;
            let grid = grid(slope);
            let fly = FlyState {
                pos_m: Vec3::new(0.0, 20_000.0, 0.0),
                yaw: 0.0,
                pitch: -START_PITCH_DEG.to_radians(),
            };
            let mut app = app(&grid, fly);
            // Unlifted, the eye would be inside the slope.
            let unlifted = CHASE_DISTANCE_PER_AGL as f32 * 20_000.0;
            let elev = (CHASE_ELEVATION_DEG as f32).to_radians();
            let behind = Vec3::new(0.0, 20_000.0 + unlifted * elev.sin(), unlifted * elev.cos());
            assert!(
                behind.y < ground(&grid, behind),
                "the test needs a buried eye"
            );
            // Hover, then fly south toward the slope and turn about, checking every frame.
            let mut state = fly;
            for frame in 0..240 {
                if frame >= 30 {
                    state.pos_m.z += 15.0;
                    state.yaw += 0.01;
                }
                app.world_mut().insert_resource(state);
                app.update();
                let cam = camera(&mut app).translation;
                let floor = ground(&grid, cam) + CAMERA_CLEARANCE_M;
                assert!(cam.y >= floor - 1e-2, "frame {frame}: {cam} under {floor}");
                // Still a chase camera: back from the fly and above it.
                let back = cam - state.pos_m;
                assert!(
                    back.length() >= CHASE_MIN_DISTANCE_M as f32 && back.y > 0.0,
                    "frame {frame}: {cam} is not chasing {:?}",
                    state.pos_m
                );
                // Nothing between the camera and the fly.
                for k in 1..64 {
                    let p = cam.lerp(state.pos_m, k as f32 / 64.0);
                    assert!(
                        p.y > ground(&grid, p),
                        "frame {frame}: the slope hides the fly"
                    );
                }
            }
        }

        /// [`grid`] with water standing at `level(x, z)` where it is `Some` (a lake or a river
        /// surface), dry elsewhere.
        fn wet_grid(
            height: impl Fn(f64, f64) -> f32 + Copy,
            level: impl Fn(f64, f64) -> Option<f32> + Copy,
        ) -> Arc<RegionGrid> {
            let mut grid = (*grid(height)).clone();
            for f in [&mut grid.inner, &mut grid.apron] {
                for row in 0..f.res {
                    for col in 0..f.res {
                        let (x, z) = f.local_of(row, col);
                        if let Some(w) = level(x, z) {
                            f.water_level_m[(row * f.res + col) as usize] = w;
                        }
                    }
                }
            }
            Arc::new(grid)
        }

        /// The fly in flight (the navigation's `Flying`), with `keys` held for every frame.
        fn flying(app: &mut App, keys: &[KeyCode]) {
            let mut held = ButtonInput::<KeyCode>::default();
            for &k in keys {
                held.press(k);
            }
            app.insert_resource(held).insert_resource(Navigation {
                phase: crate::region::navigate::NavPhase::Flying,
                ..default()
            });
        }

        /// Descends with the keys held (boosted forward, so it also crosses the water) until the
        /// height stops changing; returns every frame's fly and camera positions. `Q` descends on
        /// every platform (the descend modifier is `Option` on a Mac, `Ctrl` elsewhere).
        fn descend(app: &mut App) -> Vec<(Vec3, Vec3)> {
            flying(app, &[KeyCode::KeyQ, KeyCode::KeyW, KeyCode::ShiftLeft]);
            let mut frames = Vec::new();
            for _ in 0..3_000 {
                app.update();
                let p = app.world().resource::<FlyState>().pos_m;
                frames.push((p, camera(app).translation));
                let n = frames.len();
                if n > 60 && frames[n - 60].0.y == p.y {
                    break;
                }
            }
            frames
        }

        /// Review Focus 4: over Titicaca (a 3,812 m lake on a 3,700 m bed) the fly keeps its
        /// clearance over the water, not over the lake bed.
        #[test]
        fn the_fly_stays_above_a_lake_surface() {
            const LAKE_M: f32 = 3_812.0;
            let grid = wet_grid(|_, _| 3_700.0, |_, _| Some(LAKE_M));
            let floor = LAKE_M + CAMERA_CLEARANCE_M;
            // The hazard: the water really stands over the ground, higher than the clearance
            // over the ground alone would keep the fly.
            for (x, z) in [(0.0, 0.0), (5_000.0, -60_000.0), (-149_000.0, 149_000.0)] {
                let h = blended_height_m(&grid, x, z);
                assert_eq!(h, 3_700.0);
                assert_eq!(blended_surface_m(&grid, x, z), LAKE_M);
                assert!(h + CAMERA_CLEARANCE_M < floor - 100.0);
            }
            let start = FlyState {
                pos_m: Vec3::new(0.0, LAKE_M + 2_000.0, 0.0),
                yaw: 0.0,
                pitch: -START_PITCH_DEG.to_radians(),
            };
            let mut app = app(&grid, start);
            let frames = descend(&mut app);
            for (i, (fly, eye)) in frames.iter().enumerate() {
                assert!(
                    fly.y >= floor,
                    "frame {i}: the fly at {fly} is under {floor}"
                );
                assert!(
                    eye.y >= floor - 1e-2,
                    "frame {i}: the camera at {eye} is under {floor}"
                );
            }
            let (last, _) = *frames.last().unwrap();
            assert_eq!(last.y, floor, "the fly settles at the lake's clearance");
            assert!(last.z < -1_000.0, "the fly crossed the lake: {last}");
        }

        /// Review Focus 4: a dry basin below sea level (no sea in it) is ground the fly may
        /// descend into, down to its clearance over the basin's floor.
        #[test]
        fn the_fly_descends_into_a_dry_basin_below_sea_level() {
            const BASIN_M: f32 = -300.0;
            let grid = grid(|_, _| BASIN_M);
            // The hazard: the basin is below the sea, and no water stands in it.
            assert_eq!(blended_height_m(&grid, 0.0, 0.0), BASIN_M);
            assert_eq!(blended_surface_m(&grid, 0.0, 0.0), BASIN_M);
            assert!(blended_water(&grid, 0.0, 0.0).thickness_m < 0.0);
            let start = FlyState::start(&grid);
            assert_eq!(start.pos_m.y, BASIN_M + CAMERA_START_ALTITUDE_M);
            let mut app = app(&grid, start);
            let frames = descend(&mut app);
            let floor = BASIN_M + CAMERA_CLEARANCE_M;
            for (i, (fly, eye)) in frames.iter().enumerate() {
                assert!(
                    fly.y >= floor,
                    "frame {i}: the fly at {fly} is under {floor}"
                );
                assert!(eye.y >= floor - 1e-2, "frame {i}: the camera at {eye}");
            }
            let (last, _) = *frames.last().unwrap();
            assert_eq!(
                last.y, floor,
                "the fly settles at the basin's clearance, below the sea"
            );
            assert!(last.y < 0.0);
        }

        /// Spec §5.1: the body "banks into turns and pitches with climbs" (the final review).
        #[test]
        fn the_fly_pitches_with_climbs() {
            let grid = grid(|_, _| 0.0);
            let fly = FlyState::start(&grid);
            let mut app = app(&grid, fly);
            app.update();
            let dt = STEP.as_secs_f32();
            let pitch_after = |app: &mut App, sign: f32| {
                for _ in 0..90 {
                    let mut f = app.world_mut().resource_mut::<FlyState>();
                    // Straight up or down at the climb keys' full speed.
                    f.pos_m.y += sign * fly_speed(f.pos_m.y) * dt;
                    app.update();
                }
                app.world().resource::<FlyPose>().pitch_rad
            };
            let full = crate::creature::MAX_PITCH_DEG.to_radians();
            let up = pitch_after(&mut app, 1.0);
            assert!((up - full).abs() < 0.02, "{up} vs {full}");
            // The nose rises: the drawn forward points up.
            let pose = *app.world().resource::<FlyPose>();
            assert!(
                (pose.transform().rotation * Vec3::NEG_Z).y > 0.3,
                "{pose:?}"
            );
            let down = pitch_after(&mut app, -1.0);
            assert!((down + full).abs() < 0.02, "{down} vs {full}");
            // Level flight settles level.
            let level = pitch_after(&mut app, 0.0);
            assert!(level.abs() < 0.01, "{level}");
        }

        #[test]
        fn the_fly_shows_only_with_the_region() {
            // The region claims the altitude while it is on screen and gives it up after.
            let grid = grid(|_, _| 0.0);
            let fly = FlyState::start(&grid);
            let mut app = app(&grid, fly);
            for _ in 0..3 {
                app.update();
            }
            let shown = Some(Altitude::Metres(fly.pos_m.y));
            assert_eq!(app.world().resource::<FlyAltitude>().0, shown);
            // A region held off screen (the globe drawing, a preload, a swap) writes neither
            // the fly's pose nor its altitude: the active world owns them.
            app.world_mut()
                .resource_mut::<crate::terrain::TerrainSettings>()
                .visible = false;
            app.update();
            let other = FlyPose {
                translation: Vec3::new(1.0, 2.0, 3.0),
                ..default()
            };
            app.insert_resource(other);
            app.world_mut().resource_mut::<FlyState>().pos_m.z -= 500.0;
            for _ in 0..3 {
                app.update();
            }
            assert_eq!(*app.world().resource::<FlyPose>(), other);
            assert_eq!(app.world().resource::<FlyAltitude>().0, None);
            // Back on screen, the region writes them again.
            app.world_mut()
                .resource_mut::<crate::terrain::TerrainSettings>()
                .visible = true;
            for _ in 0..3 {
                app.update();
            }
            assert_ne!(*app.world().resource::<FlyPose>(), other);
            assert!(app.world().resource::<FlyAltitude>().0.is_some());
            app.world_mut().remove_resource::<ActiveRegion>();
            for _ in 0..3 {
                app.update();
            }
            assert_eq!(app.world().resource::<FlyAltitude>().0, None);
            // A value another world wrote (the globe's, Task 4) is left alone.
            let globe = Some(Altitude::Kilometres(2_000.0));
            app.world_mut().resource_mut::<FlyAltitude>().0 = globe;
            app.update();
            assert_eq!(app.world().resource::<FlyAltitude>().0, globe);
        }
    }
}
