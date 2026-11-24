//! The cloud (M3 design §6): one painterly full-screen cloud around the dragonfly that every
//! transition hides behind (the dive and the climb, region hops, snapshot changes).
//!
//! - **One input.** [`CloudDensity`], 0 (clear) to 1 (opaque). Every transition raises it from
//!   `Update` ([`CloudDensity::raise`]: the frame takes the largest of its sources); it is
//!   cleared at the start of each frame, so a frame with no source is clear. The views swap only
//!   on frames at exactly 1.
//! - **One pass** ([`CloudMaterial`], `cloud.wesl`): the clip-space full-screen triangle
//!   (`crate::globe::mesh::fullscreen_triangle`) on [`CLOUD_LAYER`], which both the region and
//!   the globe cameras see. Blended, depth-tested, never writing depth, at the clip depth of
//!   [`DEPTH_PAST_FLY`] × the eye-to-fly distance ([`fly_view_depth`]): the fly, nearer, draws
//!   over it and the world behind is covered. Hidden (no cost) while clear.
//! - **Sorting.** The entity sits at that depth along the view, so the fly's blended wings,
//!   nearer, sort after the cloud and blend over it.
//! - **Look.** [`CLOUD_OCTAVES`] octaves of the shared gradient noise (`noise.wesl`)
//!   on spheres around the eye at different distances, offset by the camera's accumulated
//!   motion ([`carry_motion`]) and a slow wind on real time, so the layers parallax and keep
//!   streaming while a transition holds. Lit from the active world's sun with the painterly ramp
//!   (lit where a wisp thins toward the sun, lavender-blue shade) and tinted by the era's sky
//!   ([`crate::sky::sky_tint`]). The colour is display colour in both worlds: the globe camera
//!   does not tone map and the region's tone-maps in each material, so the cloud does neither and
//!   looks the same in each.
//! - **Tilt-shift** ([`tilt_shift`], design §6): between densities 0.05 and 0.95 the top and
//!   bottom bands of the cloud lose their fine octaves and soften, in the same pass.
//! - **Warm pipelines.** It draws (at density 0, invisibly) while the globe prepares its GPU
//!   assets behind the loading screen, and the region warm-up draws one too
//!   (`crate::terrain::warmup`), so the first transition never waits for a pipeline in either
//!   camera.

use bevy::asset::uuid_handle;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::ecs::system::SystemParam;
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::chase::fly_scale;
use timefly_core::worldgen::biome::{BiomeClass, vegetation_era};

use crate::creature::FlyPose;
use crate::globe::{GlobeCamera, GlobeRenderStatus, GlobeSun, SUN_DIRECTION};
use crate::sky::{RegionSky, sky_tint};
use crate::terrain::view::{REGION_SUN_COLOR, REGION_SUN_DIRECTION, RegionCamera, TerrainSun};
use crate::time::GlobeTime;

/// The render layer the cloud draws on; the region and globe cameras both see it.
pub const CLOUD_LAYER: usize = 7;

/// `cloud.wesl`.
pub const CLOUD_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e51");

/// The cloud draws at this many times the eye-to-fly distance: just beyond the fly.
pub const DEPTH_PAST_FLY: f32 = 1.2;
/// Octaves of noise in the cloud (`OCTAVES` in `cloud.wesl`).
pub const CLOUD_OCTAVES: u32 = 4;
/// A pose further than this factor from the distance its drawn size implies (either way) is not
/// the active camera's: on a swap frame it can still be the other world's.
pub const POSE_MISMATCH: f32 = 3.0;
/// Without a usable pose the cloud draws this many near-plane distances out: it covers
/// everything, the fly included, rather than leaving near ground in front of it.
pub const FALLBACK_NEAR_PLANES: f32 = 2.0;
/// Camera travel in one frame beyond this many cloud depths is a jump (a teleport, a region
/// recentred under the camera), not motion: the wisps do not race past.
pub const MAX_STEP_DEPTHS: f32 = 2.0;
/// The wind the wisps drift on, in cloud depths per second of real time, so they keep moving
/// while a transition holds (and while virtual time is paused).
pub const CLOUD_WIND: Vec3 = Vec3::new(0.06, 0.015, 0.04);
/// The densities between which the tilt-shift runs, and how far it eases in and out.
pub const TILT_SHIFT_FROM: f32 = 0.05;
pub const TILT_SHIFT_TO: f32 = 0.95;
pub const TILT_SHIFT_EASE: f32 = 0.1;
/// On the globe the cloud takes the era's sky with no biome haze (temperate broadleaf has none).
pub const GLOBE_TINT_BIOME: BiomeClass = BiomeClass::TemperateBroadleaf;

/// The cloud's density this frame, from 0 (clear) to 1 (opaque). Transitions raise it from
/// `Update`; it is cleared in `First`.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
pub struct CloudDensity(pub f32);

impl CloudDensity {
    /// Raises the density to `density` (clamped to `0..=1`; a non-finite value is ignored): the
    /// frame takes the largest of its sources.
    pub fn raise(&mut self, density: f32) {
        if density.is_finite() {
            let want = density.clamp(0.0, 1.0);
            if want > self.0 {
                self.0 = want;
            }
        }
    }
}

/// The cloud's systems: in `PostUpdate`, after every transition (`Update`) and before the
/// transforms propagate and the frame is extracted for rendering.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CloudSystems;

/// `CloudParams` in `cloud.wesl`.
#[derive(ShaderType, Copy, Clone, Debug, Default, PartialEq, Reflect)]
pub struct CloudParams {
    /// x: density; y: the cloud's view depth (world units); z: the tilt-shift amount.
    pub state: Vec4,
    /// xyz: the camera's accumulated motion and the wind, in cloud depths.
    pub motion: Vec4,
    /// xyz: world direction toward the active world's sun.
    pub sun: Vec4,
    /// The sun's linear colour.
    pub sun_color: Vec4,
    /// The era's sky overhead and at the horizon, linear.
    pub zenith: Vec4,
    pub horizon: Vec4,
}

/// The cloud's material: one uniform.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct CloudMaterial {
    #[uniform(0)]
    pub params: CloudParams,
}

impl Material for CloudMaterial {
    fn vertex_shader() -> ShaderRef {
        CLOUD_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        CLOUD_SHADER.into()
    }
    fn alpha_mode(&self) -> AlphaMode {
        AlphaMode::Blend
    }
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        // A clip-space triangle: never culled; depth-tested (the fly stays in front), never
        // writing depth.
        descriptor.primitive.cull_mode = None;
        if let Some(depth) = descriptor.depth_stencil.as_mut() {
            depth.depth_write_enabled = Some(false);
        }
        descriptor.vertex.buffers = vec![
            layout
                .0
                .get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?,
        ];
        Ok(())
    }
}

/// Marks the cloud's entity.
#[derive(Component, Copy, Clone, Debug, Default)]
pub struct Cloud;

/// The cloud's entity and material.
#[derive(Resource, Clone, Debug)]
pub struct CloudScene {
    pub entity: Entity,
    pub material: Handle<CloudMaterial>,
}

/// The wisps' motion between frames: the camera they were last drawn for and where it was, and
/// the accumulated offset (cloud depths). Reset while the cloud is clear.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
struct CloudMotion {
    last: Option<(Entity, Vec3)>,
    offset: Vec3,
}

pub struct CloudPlugin;

impl Plugin for CloudPlugin {
    fn build(&self, app: &mut App) {
        let mut shaders = app.world_mut().resource_mut::<Assets<Shader>>();
        // The cloud imports the shared noise; whichever plugin builds first registers it.
        crate::terrain::material::add_noise_shader(&mut shaders);
        let cloud = Shader::from_wesl(
            include_str!("../assets/shaders/cloud.wesl"),
            "embedded://timefly/cloud.wesl",
        );
        let _ = shaders.insert(CLOUD_SHADER.id(), cloud);
        app.add_plugins(MaterialPlugin::<CloudMaterial>::default());
        add_cloud_systems(app);
    }
}

/// Adds [`CloudDensity`] and its clearing at the start of each frame, once (navigation adds it
/// too, so it runs headless without the cloud).
pub fn add_cloud_density(app: &mut App) {
    if !app.world().contains_resource::<CloudDensity>() {
        app.init_resource::<CloudDensity>()
            .add_systems(First, clear_cloud_density);
    }
}

/// The systems without the render plugin and shader, for headless tests.
pub(crate) fn add_cloud_systems(app: &mut App) {
    add_cloud_density(app);
    app.init_resource::<FlyPose>()
        .init_resource::<CloudMotion>()
        .configure_sets(PostUpdate, CloudSystems.before(TransformSystems::Propagate))
        .add_systems(Startup, spawn_cloud)
        .add_systems(PostUpdate, update_cloud.in_set(CloudSystems));
}

fn clear_cloud_density(mut density: ResMut<CloudDensity>) {
    if density.0 != 0.0 {
        density.0 = 0.0;
    }
}

fn spawn_cloud(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    let material = materials.add(CloudMaterial {
        params: CloudParams::default(),
    });
    let entity = commands
        .spawn((
            Name::new("Cloud"),
            Cloud,
            Mesh3d(meshes.add(crate::globe::mesh::fullscreen_triangle())),
            MeshMaterial3d(material.clone()),
            Transform::IDENTITY,
            Visibility::Hidden,
            NoFrustumCulling,
            RenderLayers::layer(CLOUD_LAYER),
        ))
        .id();
    commands.insert_resource(CloudScene { entity, material });
}

/// The cloud's view depth: [`DEPTH_PAST_FLY`] × the distance from `eye` to the fly, for a
/// camera with vertical field of view `vfov_rad` and near plane `near`. A pose whose distance
/// does not match its drawn size (within [`POSE_MISMATCH`]; `crate::creature` sizes the fly by
/// its distance) is not this camera's, and the cloud then sits [`FALLBACK_NEAR_PLANES`] near
/// planes out, covering everything.
pub fn fly_view_depth(eye: Vec3, fly: &FlyPose, vfov_rad: f32, near: f32) -> f32 {
    let fallback = FALLBACK_NEAR_PLANES * near;
    let distance = (fly.translation - eye).length();
    let per_unit = fly_scale(1.0, f64::from(vfov_rad)) as f32;
    let implied = fly.scale / per_unit;
    let consistent = distance.is_finite()
        && implied.is_finite()
        && implied > 0.0
        && distance > near
        && (implied / POSE_MISMATCH..=implied * POSE_MISMATCH).contains(&distance);
    if consistent {
        distance * DEPTH_PAST_FLY
    } else {
        fallback
    }
}

/// The tilt-shift amount at `density`: 1 between [`TILT_SHIFT_FROM`] and [`TILT_SHIFT_TO`],
/// easing in and out over [`TILT_SHIFT_EASE`], 0 outside.
pub fn tilt_shift(density: f32) -> f32 {
    if !(TILT_SHIFT_FROM..=TILT_SHIFT_TO).contains(&density) {
        return 0.0;
    }
    let ease = |x: f32| {
        let t = (x / TILT_SHIFT_EASE).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    ease(density - TILT_SHIFT_FROM).min(ease(TILT_SHIFT_TO - density))
}

/// The wisps' offset after the camera moved to `eye`: the travel since `prev`, in cloud depths
/// (`depth` world units each), added to `motion`. A different camera than last frame (a swap of
/// worlds), no previous frame, or a jump past [`MAX_STEP_DEPTHS`] carries the wisps on
/// unchanged.
pub fn carry_motion(
    prev: Option<(Entity, Vec3)>,
    camera: Entity,
    eye: Vec3,
    depth: f32,
    motion: Vec3,
) -> Vec3 {
    match prev {
        Some((last, at)) if last == camera && depth > 0.0 => {
            let step = (eye - at) / depth;
            if step.is_finite() && step.length() <= MAX_STEP_DEPTHS {
                motion + step
            } else {
                motion
            }
        }
        _ => motion,
    }
}

/// A world's camera: its entity, settings, transform, projection and whether it is the region's.
type WorldCamera = (
    Entity,
    &'static Camera,
    &'static Transform,
    &'static Projection,
    Has<RegionCamera>,
);

/// The region's and the globe's cameras.
type WorldCameraFilter = (Or<(With<RegionCamera>, With<GlobeCamera>)>, Without<Cloud>);

/// The world the cloud is drawn in: the active camera (the region's when both are), with what
/// it lights and tints the cloud by.
#[derive(SystemParam)]
struct CloudWorld<'w, 's> {
    cameras: Query<'w, 's, WorldCamera, WorldCameraFilter>,
    region_suns: Query<'w, 's, &'static GlobalTransform, With<TerrainSun>>,
    globe_suns: Query<'w, 's, &'static GlobalTransform, With<GlobeSun>>,
    sky: Option<Res<'w, RegionSky>>,
    time: Option<Res<'w, GlobeTime>>,
    status: Option<Res<'w, GlobeRenderStatus>>,
}

fn linear(c: Color) -> Vec4 {
    Vec4::from_array(c.to_linear().to_f32_array())
}

/// Places, shows or hides the cloud and writes its uniform from the density, the active camera,
/// the fly's pose, the sun and the era.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn update_cloud(
    density: Res<CloudDensity>,
    real: Res<Time<Real>>,
    pose: Res<FlyPose>,
    scene: Option<Res<CloudScene>>,
    mut motion: ResMut<CloudMotion>,
    world: CloudWorld,
    mut materials: ResMut<Assets<CloudMaterial>>,
    mut clouds: Query<(&mut Transform, &mut Visibility), With<Cloud>>,
) {
    let Some(scene) = scene else { return };
    let Ok((mut transform, mut visibility)) = clouds.get_mut(scene.entity) else {
        return;
    };
    let d = density.0;
    // Behind the loading screen the globe compiles its pipelines; the cloud's compiles with them.
    let warming = world
        .status
        .as_deref()
        .is_some_and(|s| *s == GlobeRenderStatus::PreparingGpu);
    let active = world
        .cameras
        .iter()
        .filter(|c| c.1.is_active)
        .max_by_key(|c| c.4);
    let Some((camera, _, eye, projection, in_region)) = active.filter(|_| d > 0.0 || warming)
    else {
        visibility.set_if_neq(Visibility::Hidden);
        if *motion != CloudMotion::default() {
            *motion = CloudMotion::default();
        }
        return;
    };
    let (fov, near) = match projection {
        Projection::Perspective(p) => (p.fov, p.near),
        _ => (PerspectiveProjection::default().fov, 0.1),
    };
    let depth = fly_view_depth(eye.translation, &pose, fov, near);
    if d > 0.0 {
        let offset = carry_motion(motion.last, camera, eye.translation, depth, motion.offset)
            + CLOUD_WIND * real.delta_secs();
        motion.offset = offset;
        motion.last = Some((camera, eye.translation));
    }
    let forward = eye.forward().as_vec3();
    // The triangle's bounds centre is (1, 1, 0): sort it at the cloud's depth along the view.
    let sorted_at = eye.translation + forward * depth - Vec3::new(1.0, 1.0, 0.0);
    if transform.translation != sorted_at {
        transform.translation = sorted_at;
    }
    visibility.set_if_neq(Visibility::Inherited);

    // A directional light shines along its forward axis; the sun lies the other way.
    let sun = if in_region {
        world
            .region_suns
            .iter()
            .next()
            .map_or(REGION_SUN_DIRECTION, |t| -t.forward().as_vec3())
    } else {
        world
            .globe_suns
            .iter()
            .next()
            .map_or(SUN_DIRECTION, |t| -t.forward().as_vec3())
    };
    let (era, biome) = match (&world.sky, in_region) {
        (Some(sky), true) => (sky.era, sky.biome),
        _ => (
            vegetation_era(world.time.as_ref().map_or(YearsBp(0.0), |t| t.years)),
            GLOBE_TINT_BIOME,
        ),
    };
    let (zenith, horizon, _) = sky_tint(era, biome);
    let params = CloudParams {
        state: Vec4::new(d, depth, tilt_shift(d), 0.0),
        motion: motion.offset.extend(0.0),
        sun: sun.normalize_or(Vec3::Y).extend(0.0),
        sun_color: linear(REGION_SUN_COLOR),
        zenith: linear(zenith),
        horizon: linear(horizon),
    };
    if materials
        .get(&scene.material)
        .is_some_and(|m| m.params != params)
        && let Some(mut m) = materials.get_mut(&scene.material)
    {
        m.params = params;
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;
    use timefly_core::flight::chase::fly_scale;

    use super::*;
    use crate::globe::GlobeCamera;
    use crate::terrain::view::{REGION_FOV_DEG, RegionCamera};

    /// The density a test asks for, raised each frame from `Update` like a transition would.
    #[derive(Resource, Default)]
    struct Want(Vec<f32>);

    fn raise_wanted(want: Res<Want>, mut density: ResMut<CloudDensity>) {
        for &d in &want.0 {
            density.raise(d);
        }
    }

    fn cloud_app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<CloudMaterial>()
            .init_resource::<Want>()
            .add_systems(Update, raise_wanted);
        add_cloud_systems(&mut app);
        app.update();
        app
    }

    fn want(app: &mut App, densities: &[f32]) {
        app.world_mut().resource_mut::<Want>().0 = densities.to_vec();
        app.update();
    }

    fn cloud_entity(app: &App) -> Entity {
        app.world().resource::<CloudScene>().entity
    }

    fn params(app: &App) -> CloudParams {
        let scene = app.world().resource::<CloudScene>();
        app.world()
            .resource::<Assets<CloudMaterial>>()
            .get(&scene.material)
            .unwrap()
            .params
    }

    fn camera(fov_rad: f32, near: f32, transform: Transform, active: bool) -> impl Bundle {
        (
            Camera {
                is_active: active,
                ..default()
            },
            Projection::Perspective(PerspectiveProjection {
                fov: fov_rad,
                near,
                ..default()
            }),
            transform,
        )
    }

    /// A pose the given eye would draw: `distance` ahead and a little below, scaled for `fov`.
    fn pose_ahead(eye: &Transform, distance: f32, fov_rad: f32) -> FlyPose {
        let dir = (eye.forward().as_vec3() - 0.1 * eye.up().as_vec3()).normalize();
        FlyPose {
            translation: eye.translation + dir * distance,
            scale: fly_scale(f64::from(distance), f64::from(fov_rad)) as f32,
            ..default()
        }
    }

    #[test]
    fn the_cloud_is_hidden_when_clear() {
        let mut app = cloud_app();
        let fov = REGION_FOV_DEG.to_radians();
        let eye = Transform::from_xyz(0.0, 2_000.0, 0.0);
        app.world_mut()
            .spawn((RegionCamera, camera(fov, 1.0, eye, true)));
        app.insert_resource(pose_ahead(&eye, 300.0, fov));
        let visibility = |app: &App| *app.world().get::<Visibility>(cloud_entity(app)).unwrap();
        want(&mut app, &[]);
        assert_eq!(visibility(&app), Visibility::Hidden, "density 0");
        want(&mut app, &[0.5]);
        assert_eq!(visibility(&app), Visibility::Inherited, "density 0.5");
        assert_eq!(params(&app).state.x, 0.5);
        want(&mut app, &[0.0]);
        assert_eq!(visibility(&app), Visibility::Hidden, "clear again");
        // On its own layer, which both cameras see; never culled (it is a clip-space triangle).
        let world = app.world();
        let e = cloud_entity(&app);
        assert_eq!(
            world.get::<bevy::camera::visibility::RenderLayers>(e),
            Some(&bevy::camera::visibility::RenderLayers::layer(CLOUD_LAYER))
        );
        assert!(
            world
                .get::<bevy::camera::visibility::NoFrustumCulling>(e)
                .is_some()
        );
    }

    #[test]
    fn density_takes_the_max_of_its_sources_and_clears_each_frame() {
        let mut app = cloud_app();
        want(&mut app, &[0.3, 0.8, 0.5]);
        assert_eq!(app.world().resource::<CloudDensity>().0, 0.8);
        want(&mut app, &[1.7]);
        assert_eq!(app.world().resource::<CloudDensity>().0, 1.0, "clamped");
        want(&mut app, &[-2.0, f32::NAN]);
        assert_eq!(app.world().resource::<CloudDensity>().0, 0.0);
        // No source this frame: clear.
        want(&mut app, &[0.6]);
        want(&mut app, &[]);
        assert_eq!(app.world().resource::<CloudDensity>().0, 0.0);
    }

    #[test]
    fn the_cloud_draws_just_beyond_the_fly() {
        let mut app = cloud_app();
        // The region: metres, looking north and down 20 degrees.
        let fov = REGION_FOV_DEG.to_radians();
        let eye = Transform::from_xyz(30.0, 2_400.0, 500.0)
            .with_rotation(Quat::from_rotation_x(-20f32.to_radians()));
        let region = app
            .world_mut()
            .spawn((RegionCamera, camera(fov, 1.0, eye, true)))
            .id();
        let pose = pose_ahead(&eye, 420.0, fov);
        app.insert_resource(pose);
        want(&mut app, &[0.5]);
        let distance = (pose.translation - eye.translation).length();
        let depth = params(&app).state.y;
        assert!(
            (depth - distance * 1.2).abs() < 1e-3 * distance,
            "{depth} vs {distance}"
        );
        // Sorted as a transparent at that depth along the view, so the fly's blended wings,
        // nearer, draw after (over) it. The triangle's bounds centre is (1, 1, 0).
        let t = *app.world().get::<Transform>(cloud_entity(&app)).unwrap();
        let centre = t.transform_point(Vec3::new(1.0, 1.0, 0.0));
        let ahead = (centre - eye.translation).dot(eye.forward().as_vec3());
        assert!((ahead - depth).abs() < 1e-3 * depth, "{ahead} vs {depth}");

        // The globe: kilometres, its own camera and pose, a wider field of view.
        app.world_mut().get_mut::<Camera>(region).unwrap().is_active = false;
        let globe_fov = 1.1;
        let globe_eye = Transform::from_xyz(0.0, 0.0, 9_000.0).looking_at(Vec3::ZERO, Vec3::Y);
        app.world_mut()
            .spawn((GlobeCamera, camera(globe_fov, 0.1, globe_eye, true)));
        let globe_pose = pose_ahead(&globe_eye, 700.0, globe_fov);
        app.insert_resource(globe_pose);
        want(&mut app, &[0.5]);
        let distance = (globe_pose.translation - globe_eye.translation).length();
        let depth = params(&app).state.y;
        assert!(
            (depth - distance * 1.2).abs() < 1e-3 * distance,
            "{depth} vs {distance}"
        );
    }

    #[test]
    fn a_pose_from_the_other_world_hides_the_fly_not_the_view() {
        let fov = REGION_FOV_DEG.to_radians();
        let eye = Vec3::new(0.0, 2_000.0, 0.0);
        let at = |translation: Vec3, scale: f32| FlyPose {
            translation,
            scale,
            ..default()
        };
        // A consistent pose: 1.2 x the eye-to-fly distance.
        let d = 350.0;
        let s = fly_scale(f64::from(d), f64::from(fov)) as f32;
        let depth = fly_view_depth(eye, &at(eye + Vec3::new(0.0, -30.0, -d), s), fov, 1.0);
        let distance = Vec3::new(0.0, -30.0, -d).length();
        assert!((depth - 1.2 * distance).abs() < 1e-3, "{depth}");
        // On a swap frame the pose can still be the other world's (kilometres around the
        // globe, read in metres): its distance does not match its drawn size. The cloud then
        // sits just past the near plane, covering everything, rather than far beyond the view's
        // nearest ground.
        let stale = at(Vec3::new(0.0, 0.0, 7_500.0), 40.0);
        assert_eq!(fly_view_depth(eye, &stale, fov, 1.0), 2.0);
        assert_eq!(
            fly_view_depth(eye, &at(eye, s), fov, 1.0),
            2.0,
            "no distance"
        );
        assert_eq!(
            fly_view_depth(eye, &at(Vec3::splat(f32::NAN), s), fov, 1.0),
            2.0
        );
    }

    #[test]
    fn the_wisps_follow_the_camera_but_not_a_switch() {
        let a = Entity::from_raw_u32(1).unwrap();
        let b = Entity::from_raw_u32(2).unwrap();
        // In fly depths: 50 m of travel against a 500 m cloud depth is a tenth.
        let m = carry_motion(
            Some((a, Vec3::ZERO)),
            a,
            Vec3::new(50.0, 0.0, 0.0),
            500.0,
            Vec3::ONE,
        );
        assert!(m.abs_diff_eq(Vec3::new(1.1, 1.0, 1.0), 1e-6), "{m}");
        // A switch of camera (or the first frame) carries the wisps on unchanged...
        let still = Vec3::new(0.3, 0.0, 0.0);
        assert_eq!(
            carry_motion(Some((a, Vec3::ZERO)), b, Vec3::splat(9e6), 2.0, still),
            still
        );
        assert_eq!(carry_motion(None, a, Vec3::ONE, 2.0, still), still);
        // ...as does a jump (a teleport, a region recentred under the camera).
        assert_eq!(
            carry_motion(
                Some((a, Vec3::ZERO)),
                a,
                Vec3::new(0.0, 0.0, 5e4),
                500.0,
                still
            ),
            still
        );
    }

    #[test]
    fn the_tilt_shift_runs_only_mid_transition() {
        assert_eq!(tilt_shift(0.0), 0.0);
        assert_eq!(tilt_shift(0.04), 0.0);
        assert_eq!(tilt_shift(0.5), 1.0);
        assert_eq!(tilt_shift(0.96), 0.0);
        assert_eq!(tilt_shift(1.0), 0.0);
        let mut last = 0.0;
        for i in 5..=50 {
            let t = tilt_shift(i as f32 / 100.0);
            assert!(t >= last && (0.0..=1.0).contains(&t));
            last = t;
        }
    }

    #[test]
    fn the_shader_covers_everything_at_full_density() {
        let wesl = include_str!("../assets/shaders/cloud.wesl");
        // Fully opaque at density 1, whatever the noise says.
        assert!(wesl.contains("max(alpha, step(0.999, density))"));
        // Placed at the fly's view depth, under it; several octaves of the shared noise.
        assert!(wesl.contains("vec4<f32>(0.0, 0.0, -cloud.state.y, 1.0)"));
        assert!(wesl.contains("import package::noise::gradient_noise3;"));
        assert!(wesl.contains(&format!("const OCTAVES: u32 = {CLOUD_OCTAVES}u;")));
        assert!((3..=4).contains(&CLOUD_OCTAVES), "design: 3-4 octaves");
        // Display colour in both worlds: never tone-mapped (the globe camera does not tone map,
        // the region's tone-maps in each shader), so the cloud looks the same in each.
        assert!(!wesl.contains("tone_mapping"));
    }
}
