//! The player's creature: a low-detail dragonfly (M3 design §5.1), drawn on its own render
//! layer ([`FLY_LAYER`]) that both the region and the globe cameras see.
//!
//! - **Mesh** ([`mesh`]): a slim tapered body (opaque) and four translucent wings, generated in
//!   code. The two parts are separate entities under the [`Dragonfly`] root so the body draws
//!   in the opaque pass and the wings blend at [`WING_ALPHA`].
//! - **Flap** (`dragonfly.wesl`): the vertex stage rotates the wings about the body axis by
//!   `sin(t × 2π × FLAP_HZ) × FLAP_AMPLITUDE_RAD`.
//! - **Pose** ([`FlyPose`]): whichever world is active writes it (the region's
//!   `crate::fly_camera::apply_fly` today); [`follow_fly_pose`] copies it to the root's
//!   transform, adding the bank. The bank ([`bank_step`]) and the idle bob ([`hover_offset`])
//!   are visual only: they never move `FlyState`.
//! - **Shown** only while the region camera is active, until the globe flies the fly too
//!   (Task 4).

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
use crate::terrain::view::RegionCamera;
pub use mesh::{ATTRIBUTE_WING, dragonfly_body_mesh, dragonfly_mesh, dragonfly_wings_mesh};

/// The render layer the dragonfly draws on; the region and globe cameras both see it.
pub const FLY_LAYER: usize = 6;

/// `dragonfly.wesl`: the wing flap.
pub const DRAGONFLY_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e41");

/// The wing beat's frequency (Hz): fast enough to alias into a blur at any frame rate.
pub const FLAP_HZ: f32 = 25.0;
/// The wing beat's amplitude about the body axis (radians).
pub const FLAP_AMPLITUDE_RAD: f32 = 0.6;
/// The wings' opacity.
pub const WING_ALPHA: f32 = 0.45;
/// Bank (radians) per radian a second of turn *(call)*: the full arrow-key turn (60°/s) banks
/// about 30°.
pub const BANK_PER_YAW_RATE: f32 = 0.5;
/// The bank's smoothing time constant (s).
pub const BANK_SMOOTHING_S: f32 = 0.25;
/// The steepest bank *(call)*.
pub const MAX_BANK_DEG: f32 = 40.0;
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
/// body length, from `fly_scale`) and its bank about the body axis. The drawn rotation is
/// `rotation × Rz(bank_rad)`.
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct FlyPose {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: f32,
    pub bank_rad: f32,
}

impl Default for FlyPose {
    fn default() -> Self {
        FlyPose {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: 1.0,
            bank_rad: 0.0,
        }
    }
}

impl FlyPose {
    /// The dragonfly root's transform: the pose with the bank applied.
    pub fn transform(&self) -> Transform {
        Transform {
            translation: self.translation,
            rotation: self.rotation * Quat::from_rotation_z(self.bank_rad),
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

/// The idle bob's height at `t_s` for a fly drawn at `scale`: [`HOVER_AMPLITUDE`] × `scale` at
/// [`HOVER_HZ`].
pub fn hover_offset(t_s: f32, scale: f32) -> f32 {
    HOVER_AMPLITUDE * scale * (core::f32::consts::TAU * HOVER_HZ * t_s).sin()
}

/// The dragonfly's material: `StandardMaterial` lighting with the flapping vertex stage.
pub type DragonflyMaterial = ExtendedMaterial<StandardMaterial, DragonflyExtension>;

/// The flap's parameters, as `dragonfly.wesl`'s `DragonflyLook`.
#[derive(Clone, Copy, Debug, ShaderType, Reflect)]
pub struct DragonflyLook {
    /// x: frequency (Hz); y: amplitude (radians).
    pub flap: Vec4,
}

impl Default for DragonflyLook {
    fn default() -> Self {
        DragonflyLook {
            flap: Vec4::new(FLAP_HZ, FLAP_AMPLITUDE_RAD, 0.0, 0.0),
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
    app.init_resource::<FlyPose>()
        .add_systems(Update, follow_fly_pose.after(FlySystems::Apply));
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

/// Places the dragonfly at [`FlyPose`] and shows it only while a region is on screen.
fn follow_fly_pose(
    pose: Res<FlyPose>,
    cameras: Query<&Camera, With<RegionCamera>>,
    mut flies: Query<(&mut Transform, &mut Visibility), With<Dragonfly>>,
) {
    let shown = cameras.iter().any(|c| c.is_active);
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
    fn the_dragonfly_follows_the_pose_only_in_a_region() {
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
        };
        app.insert_resource(pose);
        app.update();
        let t = *app.world().get::<Transform>(fly).unwrap();
        assert_eq!(t.translation, pose.translation);
        assert_eq!(t.scale, Vec3::splat(120.0));
        // Banked about its own body axis: the heading is kept, the right wing lifts.
        assert!((t.rotation * Vec3::NEG_Z).abs_diff_eq(pose.rotation * Vec3::NEG_Z, 1e-6));
        assert!((t.rotation * Vec3::X).y > 0.2, "{t:?}");
        // Hidden off the region, shown with it.
        let visibility = |app: &App| *app.world().get::<Visibility>(fly).unwrap();
        assert_eq!(visibility(&app), Visibility::Hidden);
        app.world_mut().get_mut::<Camera>(camera).unwrap().is_active = true;
        app.update();
        assert_eq!(visibility(&app), Visibility::Inherited);
    }

    #[test]
    fn the_flap_and_wing_constants_reach_the_shader() {
        let look = DragonflyLook::default();
        assert_eq!(look.flap.x, 25.0);
        assert_eq!(look.flap.y, 0.6);
        assert_eq!(wing_material().base.alpha_mode, AlphaMode::Blend);
        assert_eq!(wing_material().base.base_color.alpha(), 0.45);
        assert_eq!(body_material().base.alpha_mode, AlphaMode::Opaque);
        let wesl = include_str!("../../assets/shaders/dragonfly.wesl");
        assert!(wesl.contains("sin(globals.time * TAU * look.flap.x) * look.flap.y"));
        assert!(wesl.contains("@location(8) wing: f32"));
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
