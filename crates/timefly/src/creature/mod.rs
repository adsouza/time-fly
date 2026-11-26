//! The player's creature: a low-detail dragonfly (M3 design §5.1), drawn on its own render
//! layer ([`FLY_LAYER`]) that both the region and the globe cameras see.
//!
//! - **Mesh** ([`mesh`]): a slim tapered body (opaque) and four translucent wings, generated in
//!   code. The two parts are separate entities under the [`Dragonfly`] root so the body draws
//!   in the opaque pass and the wings blend at [`WING_ALPHA`].
//! - **Flap and rim** (`dragonfly.wesl`): the vertex stage rotates the wings about the body
//!   axis by `sin(2π × phase) × FLAP_AMPLITUDE_RAD`, the phase stepping
//!   [`FLAP_CYCLES_PER_FRAME`] each frame ([`flap_phase`]); the fragment stage lights the fly
//!   with Bevy's PBR and adds a subtle rim light ([`RIM_COLOR`], [`RIM_STRENGTH`],
//!   [`RIM_POWER`]) so it reads over dark ground.
//! - **Pose** ([`FlyPose`]): whichever world is active writes it (the region's
//!   `crate::fly_camera::apply_fly`, the globe's `crate::globe_flight`); [`follow_fly_pose`]
//!   copies it to the root's transform, adding the pitch and the bank. The bank into turns
//!   ([`bank_step`]), the pitch with climbs ([`pitch_step`]) and the idle bob ([`hover_offset`])
//!   are visual only: they never move the flight state.
//! - **Shown** while the region camera is active, or while the globe camera is and the globe
//!   is interactive (not over the loading screen).
//! - **Lit** by exactly one sun in each world: the region's sun shares the region camera's
//!   terrain layer, so only the region's view lists it, and the globe's sun (layer 0) is in
//!   only the globe's view. Bevy keeps a view's directional lights whose layers meet the
//!   view's, and lights every mesh in that view with them.

pub mod mesh;

use bevy::asset::uuid_handle;
use bevy::camera::visibility::RenderLayers;
use bevy::light::NotShadowCaster;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{
    ExtendedMaterial, MaterialExtension, MaterialExtensionKey, MaterialExtensionPipeline,
    MaterialPlugin,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;

use crate::fly_camera::FlySystems;
use crate::globe::{GlobeCamera, GlobeRenderStatus};
use crate::globe_flight::GlobeFlightSystems;
use crate::terrain::view::RegionCamera;
pub use mesh::{ATTRIBUTE_WING, dragonfly_body_mesh, dragonfly_mesh, dragonfly_wings_mesh};

/// The render layer the dragonfly draws on; the region and globe cameras both see it.
pub const FLY_LAYER: usize = 6;

/// `dragonfly.wesl`: the wing flap.
pub const DRAGONFLY_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e41");

/// How far the wing beat steps each frame (cycles) *(call)*: locked to frames, so it never aliases.
/// A beat in time (25 Hz in M3 Task 4) folds around the frame rate: about 1 Hz at 26 fps, 5 Hz
/// at 30, and held near one place at 50. A step just under half a cycle sweeps the stroke from
/// one side to the other every frame, a fast flutter at any frame rate (12 beats a second at
/// 30 fps, 24 at 60).
pub const FLAP_CYCLES_PER_FRAME: f32 = 0.4;
/// The frame count wraps every this many frames in the flap (`dragonfly.wesl`), a whole number
/// of cycles at [`FLAP_CYCLES_PER_FRAME`], so the stroke runs on across the wrap and `f32` keeps
/// the phase exact.
pub const FLAP_WRAP_FRAMES: u32 = 1_000;
/// The wing beat's amplitude about the body axis (radians).
pub const FLAP_AMPLITUDE_RAD: f32 = 0.6;
/// The wings' opacity.
pub const WING_ALPHA: f32 = 0.45;
/// The rim light's colour (linear): a pale blue-green, so the fly's outline reads over dark
/// forest *(call)*.
pub const RIM_COLOR: Vec3 = Vec3::new(0.55, 0.9, 1.0);
/// The rim light's strength where the surface is edge-on to the view, in the lit colour's units
/// (sunlit white is about 2.2 there) *(call: subtle)*.
pub const RIM_STRENGTH: f32 = 0.35;
/// How tightly the rim hugs the outline: the brightening goes as `(1 − N·V)^power`.
pub const RIM_POWER: f32 = 3.0;
/// Bank (radians) per radian a second of turn *(call)*: the full arrow-key turn (60°/s) banks
/// about 30°.
pub const BANK_PER_YAW_RATE: f32 = 0.5;
/// The bank's smoothing time constant (s).
pub const BANK_SMOOTHING_S: f32 = 0.25;
/// The steepest bank *(call)*.
pub const MAX_BANK_DEG: f32 = 40.0;
/// The steepest pitch, nose up climbing or down diving at the climb keys' full speed *(call)*:
/// a climb fraction of 1 (the vertical speed over the speed the climb keys fly at) pitches
/// this far; a boost or a lift's faster climb is clamped to it.
pub const MAX_PITCH_DEG: f32 = 20.0;
/// The pitch's smoothing time constant (s), the bank's.
pub const PITCH_SMOOTHING_S: f32 = BANK_SMOOTHING_S;
/// The idle bob's frequency (Hz).
pub const HOVER_HZ: f32 = 0.3;
/// The idle bob's amplitude, as a fraction of the fly's drawn scale.
pub const HOVER_AMPLITUDE: f32 = 0.02;
/// How quickly the idle bob fades in once the fly is left alone, and out once it moves (s).
pub const HOVER_EASE_S: f32 = 0.5;

/// Marks the dragonfly's root entity (its body and wings are children).
#[derive(Component, Copy, Clone, Debug, Default)]
pub struct Dragonfly;

/// Where the dragonfly is drawn, written by whichever world is active: its position (the idle
/// bob included), its heading frame (`−z` forward, `+y` up, no bank), its uniform scale (the
/// body length, from `fly_scale`), its pitch (nose up positive) about its right axis and its
/// bank about the body axis. The drawn rotation is `rotation × Rx(pitch_rad) × Rz(bank_rad)`.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct FlyPose {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: f32,
    pub bank_rad: f32,
    pub pitch_rad: f32,
}

impl Default for FlyPose {
    fn default() -> Self {
        FlyPose {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: 1.0,
            bank_rad: 0.0,
            pitch_rad: 0.0,
        }
    }
}

impl FlyPose {
    /// The dragonfly root's transform: the pose with the pitch and the bank applied.
    pub fn transform(&self) -> Transform {
        Transform {
            translation: self.translation,
            rotation: self.rotation
                * Quat::from_rotation_x(self.pitch_rad)
                * Quat::from_rotation_z(self.bank_rad),
            scale: Vec3::splat(self.scale),
        }
    }
}

/// The next bank (radians) after `dt_s` seconds turning at `yaw_rate` (rad/s, positive to the
/// left): toward [`BANK_PER_YAW_RATE`] × the rate, within [`MAX_BANK_DEG`], smoothed with
/// [`BANK_SMOOTHING_S`]. A positive bank lifts the right wing (a lean into a left turn).
pub fn bank_step(bank_rad: f32, yaw_rate: f32, dt_s: f32) -> f32 {
    let limit = MAX_BANK_DEG.to_radians();
    let want = if yaw_rate.is_finite() {
        (BANK_PER_YAW_RATE * yaw_rate).clamp(-limit, limit)
    } else {
        0.0
    };
    let dt = if dt_s.is_finite() { dt_s.max(0.0) } else { 0.0 };
    bank_rad + (want - bank_rad) * (1.0 - (-dt / BANK_SMOOTHING_S).exp())
}

/// The next pitch (radians, nose up positive) after `dt_s` seconds climbing at `climb` (the
/// vertical speed over the speed the climb keys fly at: 1 climbing at full speed, −1 descending):
/// toward `climb` × [`MAX_PITCH_DEG`], clamped to it, smoothed with [`PITCH_SMOOTHING_S`], as
/// [`bank_step`] is.
pub fn pitch_step(pitch_rad: f32, climb: f32, dt_s: f32) -> f32 {
    let limit = MAX_PITCH_DEG.to_radians();
    let want = if climb.is_finite() {
        climb.clamp(-1.0, 1.0) * limit
    } else {
        0.0
    };
    let dt = if dt_s.is_finite() { dt_s.max(0.0) } else { 0.0 };
    pitch_rad + (want - pitch_rad) * (1.0 - (-dt / PITCH_SMOOTHING_S).exp())
}

/// The wing beat's phase (cycles, `0..1`) on frame `frame_count` (Bevy's `FrameCount`, the
/// shader's `globals.frame_count`): [`FLAP_CYCLES_PER_FRAME`] a frame, as `dragonfly.wesl`
/// reckons it.
pub fn flap_phase(frame_count: u32) -> f32 {
    ((frame_count % FLAP_WRAP_FRAMES) as f32 * FLAP_CYCLES_PER_FRAME).fract()
}

/// The idle bob's height at `t_s` for a fly drawn at `scale`: [`HOVER_AMPLITUDE`] × `scale` at
/// [`HOVER_HZ`].
pub fn hover_offset(t_s: f32, scale: f32) -> f32 {
    HOVER_AMPLITUDE * scale * (core::f32::consts::TAU * HOVER_HZ * t_s).sin()
}

/// The dragonfly's material: `StandardMaterial` lighting with the flapping vertex stage.
pub type DragonflyMaterial = ExtendedMaterial<StandardMaterial, DragonflyExtension>;

/// The flap and rim parameters, as `dragonfly.wesl`'s `DragonflyLook`.
#[derive(Clone, Copy, Debug, ShaderType, Reflect)]
pub struct DragonflyLook {
    /// x: the beat's step (cycles per frame); y: amplitude (radians).
    pub flap: Vec4,
    /// rgb: the rim colour; w: its strength.
    pub rim: Vec4,
    /// x: the rim's power.
    pub rim_shape: Vec4,
}

impl Default for DragonflyLook {
    fn default() -> Self {
        DragonflyLook {
            flap: Vec4::new(FLAP_CYCLES_PER_FRAME, FLAP_AMPLITUDE_RAD, 0.0, 0.0),
            rim: RIM_COLOR.extend(RIM_STRENGTH),
            rim_shape: Vec4::new(RIM_POWER, 0.0, 0.0, 0.0),
        }
    }
}

#[derive(Asset, AsBindGroup, Reflect, Debug, Clone, Default)]
pub struct DragonflyExtension {
    #[uniform(100)]
    pub look: DragonflyLook,
}

impl MaterialExtension for DragonflyExtension {
    fn vertex_shader() -> ShaderRef {
        DRAGONFLY_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        DRAGONFLY_SHADER.into()
    }
    // The fly is drawn at a scale that follows the chase distance (kilometres high up), so its
    // shadow would mislead; it sits in no prepass either (the vertex stage needs the wing
    // attribute, which Bevy's prepass shader does not read).
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _pipeline: &MaterialExtensionPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialExtensionKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_NORMAL.at_shader_location(1),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(5),
            ATTRIBUTE_WING.at_shader_location(8),
        ])?];
        Ok(())
    }
}

/// The body's material: opaque, a little glossy, colour from the vertices.
pub fn body_material() -> DragonflyMaterial {
    DragonflyMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.35,
            metallic: 0.3,
            // Seen from up to tens of kilometres away, fog would wash the fly out.
            fog_enabled: false,
            ..default()
        },
        extension: DragonflyExtension::default(),
    }
}

/// The wings' material: translucent at [`WING_ALPHA`] and seen from both sides.
pub fn wing_material() -> DragonflyMaterial {
    DragonflyMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE.with_alpha(WING_ALPHA),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.2,
            double_sided: true,
            cull_mode: None,
            fog_enabled: false,
            ..default()
        },
        extension: DragonflyExtension::default(),
    }
}

pub struct CreaturePlugin;

impl Plugin for CreaturePlugin {
    fn build(&self, app: &mut App) {
        let shader = Shader::from_wesl(
            include_str!("../../assets/shaders/dragonfly.wesl"),
            "embedded://timefly/dragonfly.wesl",
        );
        let _ = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(DRAGONFLY_SHADER.id(), shader);
        app.add_plugins(MaterialPlugin::<DragonflyMaterial>::default())
            .add_systems(Startup, spawn_dragonfly);
        add_creature_systems(app);
    }
}

/// The systems without the render plugin and shader, for headless tests.
pub(crate) fn add_creature_systems(app: &mut App) {
    app.init_resource::<FlyPose>().add_systems(
        Update,
        follow_fly_pose
            .after(FlySystems::Apply)
            .after(GlobeFlightSystems::Apply),
    );
}

/// Spawns the dragonfly, hidden: a root with the opaque body and the translucent wings.
fn spawn_dragonfly(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<DragonflyMaterial>>,
) {
    let layer = RenderLayers::layer(FLY_LAYER);
    let body = (
        Name::new("Dragonfly body"),
        Mesh3d(meshes.add(dragonfly_body_mesh())),
        MeshMaterial3d(materials.add(body_material())),
        layer.clone(),
        NotShadowCaster,
    );
    let wings = (
        Name::new("Dragonfly wings"),
        Mesh3d(meshes.add(dragonfly_wings_mesh())),
        MeshMaterial3d(materials.add(wing_material())),
        layer.clone(),
        NotShadowCaster,
    );
    commands.spawn((
        Name::new("Dragonfly"),
        Dragonfly,
        Transform::default(),
        Visibility::Hidden,
        layer,
        children![body, wings],
    ));
}

/// Places the dragonfly at [`FlyPose`] and shows it while a world that flies it is on screen:
/// a region, or the globe once it is interactive.
fn follow_fly_pose(
    pose: Res<FlyPose>,
    regions: Query<&Camera, With<RegionCamera>>,
    globes: Query<&Camera, With<GlobeCamera>>,
    status: Option<Res<GlobeRenderStatus>>,
    mut flies: Query<(&mut Transform, &mut Visibility), With<Dragonfly>>,
) {
    let globe_ready = status.is_none_or(|s| matches!(*s, GlobeRenderStatus::Interactive { .. }));
    let shown =
        regions.iter().any(|c| c.is_active) || (globe_ready && globes.iter().any(|c| c.is_active));
    for (mut transform, mut visibility) in &mut flies {
        let want = pose.transform();
        if *transform != want {
            *transform = want;
        }
        visibility.set_if_neq(if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bank_leans_into_turns_and_settles() {
        // Turning left (positive yaw rate) banks left: a positive roll about −z's opposite,
        // `Quat::from_rotation_z`, lifts the right wing.
        let mut bank = 0.0;
        for _ in 0..240 {
            bank = bank_step(bank, TURN_RATE, 1.0 / 60.0);
        }
        let steady = BANK_PER_YAW_RATE * TURN_RATE;
        assert!((bank - steady).abs() < 1e-3, "{bank} vs {steady}");
        assert!(bank_step(0.0, -TURN_RATE, 1.0 / 60.0) < 0.0);
        // Smoothed with a 0.25 s time constant: after one time constant, 1 − 1/e of the way.
        let one_tau = bank_step(0.0, TURN_RATE, BANK_SMOOTHING_S);
        assert!(
            (one_tau / steady - (1.0 - (-1.0f32).exp())).abs() < 1e-3,
            "{one_tau}"
        );
        // A frantic drag never rolls the fly past the limit.
        let wild = bank_step(0.0, 1.0e4, 10.0);
        assert!(wild <= MAX_BANK_DEG.to_radians() + 1e-6, "{wild}");
        // No turn: the bank decays back to level.
        let mut b = steady;
        for _ in 0..240 {
            b = bank_step(b, 0.0, 1.0 / 60.0);
        }
        assert!(b.abs() < 1e-3, "{b}");
    }

    const TURN_RATE: f32 = 1.0;

    #[test]
    fn the_pitch_follows_climbs_and_settles() {
        // Climbing at the climb keys' full speed (a climb fraction of 1) pitches the nose up to
        // the limit; diving, down to it.
        let limit = MAX_PITCH_DEG.to_radians();
        let settle = |climb: f32| {
            let mut p = 0.0;
            for _ in 0..240 {
                p = pitch_step(p, climb, 1.0 / 60.0);
            }
            p
        };
        assert!((settle(1.0) - limit).abs() < 1e-3);
        assert!((settle(-1.0) + limit).abs() < 1e-3);
        assert!((settle(0.5) - 0.5 * limit).abs() < 1e-3);
        // Boosted (4x) or a placement's jump: never past the limit.
        assert!((settle(4.0) - limit).abs() < 1e-3);
        assert!(pitch_step(0.0, 1.0e6, 10.0) <= limit + 1e-6);
        assert!(pitch_step(0.1, f32::NAN, 1.0 / 60.0).is_finite());
        // Smoothed like the bank: one time constant is 1 − 1/e of the way.
        let one_tau = pitch_step(0.0, 1.0, PITCH_SMOOTHING_S);
        assert!((one_tau / limit - (1.0 - (-1.0f32).exp())).abs() < 1e-3);
        // Level flight decays back to level.
        let mut p = limit;
        for _ in 0..240 {
            p = pitch_step(p, 0.0, 1.0 / 60.0);
        }
        assert!(p.abs() < 1e-3, "{p}");
        // Drawn: a positive pitch lifts the nose (−z) about the fly's own right axis, before
        // the bank about the body axis.
        let pose = FlyPose {
            rotation: Quat::from_rotation_y(0.7),
            pitch_rad: 0.3,
            bank_rad: 0.2,
            ..default()
        };
        let nose = pose.transform().rotation * Vec3::NEG_Z;
        assert!((nose.y - 0.3f32.sin()).abs() < 1e-5, "{nose}");
        let flat = Quat::from_rotation_y(0.7) * Vec3::NEG_Z;
        assert!(
            (Vec2::new(nose.x, nose.z).normalize() - Vec2::new(flat.x, flat.z)).length() < 1e-5
        );
    }

    #[test]
    fn the_dragonfly_follows_the_pose_in_either_world() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        add_creature_systems(&mut app);
        let fly = app
            .world_mut()
            .spawn((Dragonfly, Transform::default(), Visibility::Hidden))
            .id();
        let camera = app
            .world_mut()
            .spawn((
                RegionCamera,
                Camera {
                    is_active: false,
                    ..default()
                },
            ))
            .id();
        let pose = FlyPose {
            translation: Vec3::new(10.0, 2_000.0, -30.0),
            rotation: Quat::from_rotation_y(0.4),
            scale: 120.0,
            bank_rad: 0.3,
            pitch_rad: 0.0,
        };
        app.insert_resource(pose);
        app.update();
        let t = *app.world().get::<Transform>(fly).unwrap();
        assert_eq!(t.translation, pose.translation);
        assert_eq!(t.scale, Vec3::splat(120.0));
        // Banked about its own body axis: the heading is kept, the right wing lifts.
        assert!((t.rotation * Vec3::NEG_Z).abs_diff_eq(pose.rotation * Vec3::NEG_Z, 1e-6));
        assert!((t.rotation * Vec3::X).y > 0.2, "{t:?}");
        // Hidden with neither world on screen, shown with the region.
        let visibility = |app: &App| *app.world().get::<Visibility>(fly).unwrap();
        assert_eq!(visibility(&app), Visibility::Hidden);
        app.world_mut().get_mut::<Camera>(camera).unwrap().is_active = true;
        app.update();
        assert_eq!(visibility(&app), Visibility::Inherited);
        // Shown with the globe once it is interactive; not over its loading screen.
        app.world_mut().get_mut::<Camera>(camera).unwrap().is_active = false;
        app.world_mut().spawn((GlobeCamera, Camera::default()));
        app.insert_resource(GlobeRenderStatus::PreparingGpu);
        app.update();
        assert_eq!(visibility(&app), Visibility::Hidden);
        app.insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 });
        app.update();
        assert_eq!(visibility(&app), Visibility::Inherited);
    }

    #[test]
    fn the_rim_light_brightens_the_outline() {
        // A fragment stage of our own adds the rim to Bevy's lit colour, on body and wings.
        assert!(
            matches!(DragonflyExtension::fragment_shader(), ShaderRef::Handle(h) if h == DRAGONFLY_SHADER)
        );
        let look = DragonflyLook::default();
        assert_eq!(look.rim, Vec4::new(0.55, 0.9, 1.0, 0.35));
        assert_eq!(look.rim_shape.x, 3.0);
        let wesl = include_str!("../../assets/shaders/dragonfly.wesl");
        assert!(wesl.contains("@fragment"));
        assert!(wesl.contains("pow(1.0 - facing, look.rim_shape.x) * look.rim.w"));
        assert!(wesl.contains("apply_pbr_lighting(pbr_input)"));
    }

    #[test]
    fn the_flap_and_wing_constants_reach_the_shader() {
        let look = DragonflyLook::default();
        assert_eq!(look.flap.x, 0.4);
        assert_eq!(look.flap.y, 0.6);
        assert_eq!(wing_material().base.alpha_mode, AlphaMode::Blend);
        assert_eq!(wing_material().base.base_color.alpha(), 0.45);
        assert_eq!(body_material().base.alpha_mode, AlphaMode::Opaque);
        let wesl = include_str!("../../assets/shaders/dragonfly.wesl");
        // The phase is `flap_phase`'s: the frame count, wrapped, times the step.
        assert!(wesl.contains(&format!(
            "fract(f32(globals.frame_count % {FLAP_WRAP_FRAMES}u) * look.flap.x)"
        )));
        assert!(wesl.contains("sin(TAU * flap_phase()) * look.flap.y"));
        // Never on time, which aliases (`the_flap_reads_fast_at_every_frame_rate`).
        assert!(!wesl.contains("globals.time"), "the flap reads the clock");
        assert!(wesl.contains("@location(8) wing: f32"));
        // The wrap falls on a whole number of cycles, so the stroke runs on across it.
        let cycles = f64::from(FLAP_WRAP_FRAMES) * f64::from(FLAP_CYCLES_PER_FRAME);
        assert!((cycles - cycles.round()).abs() < 1e-4, "{cycles}");
        let step = flap_phase(0) + 1.0 - flap_phase(FLAP_WRAP_FRAMES - 1);
        assert!((step - FLAP_CYCLES_PER_FRAME).abs() < 1e-4, "{step}");
        assert_eq!(flap_phase(FLAP_WRAP_FRAMES), flap_phase(0));
    }

    /// The beat as seen on screen at `fps`, for a phase rule `phase_at(frame, seconds)`: the
    /// apparent beat (Hz: the median frame-to-frame step, wrapped to the nearest half cycle, ×
    /// `fps`) and the narrowest sweep any five consecutive frames show (the peak-to-peak flap,
    /// in amplitudes, 0 to 2).
    fn seen_beat(phase_at: impl Fn(u32, f32) -> f32, fps: f32) -> (f32, f32) {
        let frames: Vec<f32> = (0..400u32).map(|k| phase_at(k, k as f32 / fps)).collect();
        let mut steps: Vec<f32> = frames
            .windows(2)
            .map(|w| {
                let d = w[1] - w[0];
                (d - d.round()).abs()
            })
            .collect();
        steps.sort_by(f32::total_cmp);
        let apparent_hz = steps[steps.len() / 2] * fps;
        let flaps: Vec<f32> = frames
            .iter()
            .map(|p| (core::f32::consts::TAU * p).sin())
            .collect();
        let sweep = flaps
            .windows(5)
            .map(|w| {
                let max = w.iter().copied().fold(f32::MIN, f32::max);
                let min = w.iter().copied().fold(f32::MAX, f32::min);
                max - min
            })
            .fold(f32::MAX, f32::min);
        (apparent_hz, sweep)
    }

    #[test]
    fn the_flap_reads_fast_at_every_frame_rate() {
        // A real dragonfly's beat, which the M3 Task 4 shader drew in time.
        const REAL_BEAT_HZ: f32 = 25.0;
        let in_time = |_: u32, t: f32| (t * REAL_BEAT_HZ).fract();
        // The measure sees the owner's report in the beat drawn in time: a slow flutter near
        // 26 fps, and the stroke held still at 50.
        let (slow, _) = seen_beat(in_time, 26.0);
        assert!((slow - 1.0).abs() < 0.05, "{slow}");
        let (_, held) = seen_beat(in_time, 50.0);
        assert!(held < 0.01, "{held}");
        // The shader's beat: at every frame rate from 20 to 144 (and the video rates), at least
        // three quarters as fast as the real beat or the fastest the frame rate can show (half
        // the frame rate), whichever is slower, and every five frames sweep most of the stroke.
        let rates = (20..=144)
            .map(|f| f as f32)
            .chain([23.976, 29.97, 47.952, 59.94, 119.88]);
        for fps in rates {
            let (hz, sweep) = seen_beat(|frame, _| flap_phase(frame), fps);
            let fastest = REAL_BEAT_HZ.min(fps / 2.0);
            assert!(
                hz >= 0.75 * fastest,
                "{fps} fps: the beat reads at {hz} Hz, want at least {}",
                0.75 * fastest
            );
            assert!(
                sweep >= 1.2,
                "{fps} fps: five frames sweep only {sweep} amplitudes"
            );
        }
    }

    /// The wings' triangles in the model frame: which side each is on (`±1`, the shader's
    /// `side`) and its plane's unit normal, from the mesh's positions.
    fn wing_faces() -> Vec<(f32, Vec3)> {
        let mesh = dragonfly_wings_mesh();
        let p = mesh
            .attribute(Mesh::ATTRIBUTE_POSITION)
            .and_then(|a| a.as_float3())
            .expect("positions")
            .to_vec();
        let indices: Vec<usize> = mesh.indices().expect("indices").iter().collect();
        indices
            .chunks(3)
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(p[i]));
                (a.x.signum(), (b - a).cross(c - a).normalize())
            })
            .collect()
    }

    #[test]
    fn the_wings_never_lie_edge_on_to_the_chase_view() {
        use bevy::math::DVec3;
        use timefly_core::flight::altitudes::CHASE_MIN_DISTANCE_M;
        use timefly_core::flight::chase::{CHASE_START_LOOK_DEG, chase_pose};
        // The fly heading −z from the origin; the region's chase eye behind it, raised
        // CHASE_ELEVATION_DEG above its horizon (the look pitch moves only the target).
        let chase = chase_pose(
            DVec3::ZERO,
            DVec3::NEG_Z,
            DVec3::Y,
            1_000.0,
            CHASE_START_LOOK_DEG,
            CHASE_MIN_DISTANCE_M,
        );
        let behind = chase.eye.normalize().as_vec3();
        // At least this angle between a wing's plane and the view.
        const MIN_VIEW_DEG: f32 = 10.0;
        // The chase eye glides, so in turns it swings a little to either side of behind.
        const SWING_DEG: i32 = 10;
        const STROKE_SAMPLES: usize = 40;
        let least = MIN_VIEW_DEG.to_radians().sin();
        let faces = wing_faces();
        assert_eq!(faces.len(), 8, "four quads");
        let degrees = |limit: f32, step: usize| {
            let limit = limit.round() as i32;
            (-limit..=limit)
                .step_by(step)
                .map(|d| (d as f32).to_radians())
        };
        // Every pitch (a full-speed descent is the steepest, nose down MAX_PITCH_DEG), bank and
        // swing of the eye, at every point of the stroke.
        for pitch_rad in degrees(MAX_PITCH_DEG, 2) {
            for bank_rad in degrees(MAX_BANK_DEG, 4) {
                for swing in degrees(SWING_DEG as f32, 5) {
                    let eye = Quat::from_rotation_y(swing) * behind;
                    let drawn = FlyPose {
                        pitch_rad,
                        bank_rad,
                        ..default()
                    }
                    .transform()
                    .rotation;
                    let mut mean = vec![0.0; faces.len()];
                    for k in 0..STROKE_SAMPLES {
                        // `dragonfly.wesl`: each wing turns about the body axis by the flap,
                        // mirrored between the sides.
                        let flap = (core::f32::consts::TAU * k as f32 / STROKE_SAMPLES as f32)
                            .sin()
                            * FLAP_AMPLITUDE_RAD;
                        let seen: Vec<f32> = faces
                            .iter()
                            .map(|&(side, n)| {
                                (drawn * Quat::from_rotation_z(side * flap) * n)
                                    .dot(eye)
                                    .abs()
                            })
                            .collect();
                        let best = seen.iter().copied().fold(0.0, f32::max);
                        assert!(
                            best >= least,
                            "every wing edge-on: pitch {:.0}°, bank {:.0}°, swing {:.0}°, \
                             flap {flap:.2}: best {:.1}°",
                            pitch_rad.to_degrees(),
                            bank_rad.to_degrees(),
                            swing.to_degrees(),
                            best.asin().to_degrees()
                        );
                        for (m, s) in mean.iter_mut().zip(&seen) {
                            *m += s / STROKE_SAMPLES as f32;
                        }
                    }
                    // No wing stays edge-on through its stroke either.
                    for (i, m) in mean.iter().enumerate() {
                        assert!(
                            *m >= least,
                            "wing face {i} edge-on through the stroke: pitch {:.0}°, bank \
                             {:.0}°, swing {:.0}°: {:.1}°",
                            pitch_rad.to_degrees(),
                            bank_rad.to_degrees(),
                            swing.to_degrees(),
                            m.asin().to_degrees()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_hover_bobs_at_a_third_of_a_hertz() {
        let scale = 500.0;
        let amp = HOVER_AMPLITUDE * scale;
        let samples: Vec<f32> = (0..400)
            .map(|k| hover_offset(k as f32 / 100.0, scale))
            .collect();
        let max = samples.iter().copied().fold(f32::MIN, f32::max);
        let min = samples.iter().copied().fold(f32::MAX, f32::min);
        assert!((max - amp).abs() < 0.01 * amp, "{max} vs {amp}");
        assert!((min + amp).abs() < 0.01 * amp, "{min} vs {amp}");
        // Periodic at HOVER_HZ.
        let period = 1.0 / HOVER_HZ;
        for t in [0.0, 0.7, 1.9] {
            let a = hover_offset(t, scale);
            let b = hover_offset(t + period, scale);
            assert!((a - b).abs() < 1e-3 * amp, "{a} vs {b}");
        }
    }
}
