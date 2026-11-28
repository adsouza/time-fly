//! Instanced scatter in region worlds (rivers and lakes design §4.4, "Rocks"): many copies of a
//! few small meshes, drawn as instanced batches. Boulders are the first kind
//! ([`crate::boulders`]); vegetation will be the next, so nothing here knows about rocks.
//!
//! - **The set.** An owner (a plugin per kind) spawns one entity with a [`ScatterSet`], the
//!   instances it wants drawn, and a [`ScatterPool`] holding the kind's meshes (one per shape)
//!   and its one material. To change what is drawn the owner inserts a new `ScatterSet`; an
//!   empty set draws nothing.
//! - **The pool.** Slot `i` draws instance `i`: a child entity of the set's, with the shape's
//!   mesh, the kind's material, the instance's transform and its [`MeshTag`] (the shader's
//!   per-instance data), and an abrupt [`VisibilityRange`] out to the pool's range. Every slot
//!   shares one mesh per shape and one material, so Bevy draws each shape as one instanced batch.
//!   The pool grows in chunks of [`SCATTER_POOL_CHUNK`] and never shrinks while the set has
//!   instances: spare slots are hidden, and a new set rewrites the same entities, so a region
//!   swap never holds two sets' worth of instance buffers. An empty set (leaving region worlds)
//!   despawns the slots, so their memory goes back to where it was.
//! - **Spread over frames.** A new set rewrites at most [`SCATTER_UPDATE_PER_FRAME`] slots a
//!   frame (16,384 boulders take 8 frames). Until it has settled the set's entity is hidden, so
//!   its children never show a mix of the old and new sets (the old in the new region's frame).
//!
//! `scatter.wesl` draws a [`ScatterMaterial`]: the kind's colour from the terrain palette
//! ([`ScatterLook::texel`]) with a per-instance hue and value jitter from the tag's seed,
//! darkened where the instance is wet (the tag's top byte: the share of its height under the
//! water, and a band above it), lit through Bevy's PBR path with the shade floored as the
//! terrain floors it ([`ScatterLook::light`]). Its vertex stage drops the
//! instance by the curvature `d² / 2R` as the terrain does, and pulls the part above the water
//! toward the camera by the water's own depth pull (`crate::water::WATER_DEPTH_PULL`), so the
//! water's pull never hides a rock top that stands above it.

use std::sync::Arc;

use bevy::asset::uuid_handle;
use bevy::camera::visibility::{RenderLayers, VisibilityRange};
use bevy::light::NotShadowCaster;
use bevy::mesh::MeshTag;
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bevy::transform::TransformSystems;

use crate::terrain::TERRAIN_LAYER;

/// `scatter.wesl`: the scatter's vertex and fragment shaders.
pub const SCATTER_SHADER: Handle<Shader> = uuid_handle!("3c0b5d8e-21f4-4a7e-b0c6-9e5d71a42f18");

/// The pool grows by this many slots at a time.
pub const SCATTER_POOL_CHUNK: usize = 1_024;
/// The most slots a pool rewrites in one frame.
pub const SCATTER_UPDATE_PER_FRAME: usize = 2_048;

/// One drawn copy: where it stands (region-local metres; the mesh's origin), its uniform scale,
/// its turn about `+y` (radians) and its [`MeshTag`] ([`ScatterInstance::tag_of`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ScatterInstance {
    pub translation: Vec3,
    pub scale: f32,
    pub yaw: f32,
    pub tag: u32,
}

impl ScatterInstance {
    /// The tag of an instance whose share of height under the water is `wet` (clamped to
    /// [0, 1]), with seed `seed`: `(wet × 255) << 24 | seed & 0xFF_FFFF`, the wetness rounded
    /// to the nearest 255th.
    pub fn tag_of(wet: f32, seed: u32) -> u32 {
        // `clamp` keeps a NaN, which would cast to 0 anyway; say so.
        let wet = if wet.is_nan() {
            0.0
        } else {
            wet.clamp(0.0, 1.0)
        };
        let byte = (wet * 255.0).round() as u32;
        byte << 24 | seed & 0xFF_FFFF
    }

    /// The tag's wetness (the top byte, over 255).
    pub fn wet(&self) -> f32 {
        (self.tag >> 24) as f32 / 255.0
    }

    /// The tag's seed (its low 24 bits): the shader's jitter, and the shape.
    pub fn seed(&self) -> u32 {
        self.tag & 0xFF_FFFF
    }

    /// Which of `shapes` meshes draws it: from the seed's high bits, so a given instance always
    /// has the same shape, whichever slot draws it.
    pub fn shape(&self, shapes: usize) -> usize {
        (self.seed() as usize >> 12) % shapes.max(1)
    }

    /// Its slot's transform.
    pub fn transform(&self) -> Transform {
        Transform {
            translation: self.translation,
            rotation: Quat::from_rotation_y(self.yaw),
            scale: Vec3::splat(self.scale),
        }
    }
}

/// The kinds of scatter.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Reflect)]
pub enum ScatterKind {
    Boulders,
    /// The white water around rocks at the surface (Task 11c, `crate::boulders`): flat rings at
    /// the water level with a wake downstream, alpha-masked.
    FoamCollar,
}

impl ScatterKind {
    /// The kind's shading mode in `scatter.wesl` (`ScatterLook::texel.z`).
    pub fn shader_mode(self) -> u32 {
        match self {
            ScatterKind::Boulders => SCATTER_MODE_SOLID,
            ScatterKind::FoamCollar => SCATTER_MODE_FOAM,
        }
    }
}

/// `scatter.wesl`'s mode for solid, palette-coloured instances (boulders).
pub const SCATTER_MODE_SOLID: u32 = 0;
/// `scatter.wesl`'s mode for foam collars.
pub const SCATTER_MODE_FOAM: u32 = 1;

/// The instances of one kind to draw, on the entity that holds the kind's [`ScatterPool`]. The
/// owner replaces it to change what is drawn.
#[derive(Component, Clone, Debug)]
pub struct ScatterSet {
    pub kind: ScatterKind,
    pub instances: Arc<[ScatterInstance]>,
}

impl ScatterSet {
    /// A set drawing nothing.
    pub fn empty(kind: ScatterKind) -> ScatterSet {
        ScatterSet {
            kind,
            instances: Arc::from([]),
        }
    }
}

/// A pooled slot entity, drawing instance `slot` of its set.
#[derive(Component, Copy, Clone, Debug, PartialEq, Eq)]
pub struct ScatterSlot {
    pub slot: u32,
}

/// One kind's slot entities, its meshes and material, and how far it has got toward its set.
#[derive(Component, Debug)]
#[require(Transform, Visibility)]
pub struct ScatterPool {
    meshes: Vec<Handle<Mesh>>,
    material: Handle<ScatterMaterial>,
    visible_m: f32,
    /// Slot entities, by slot.
    slots: Vec<Entity>,
    /// The instances the slots are being rewritten to.
    target: Arc<[ScatterInstance]>,
    /// The next slot to rewrite, and the end of the slots that need it.
    cursor: usize,
    end: usize,
    /// Slots `[0, shown)` may be visible.
    shown: usize,
}

impl ScatterPool {
    /// A pool drawing each instance with `meshes[instance.shape(meshes.len())]` and `material`,
    /// out to `visible_m` from the camera.
    pub fn new(
        meshes: Vec<Handle<Mesh>>,
        material: Handle<ScatterMaterial>,
        visible_m: f32,
    ) -> Self {
        assert!(!meshes.is_empty(), "a scatter pool needs a mesh");
        ScatterPool {
            meshes,
            material,
            visible_m,
            slots: Vec::new(),
            target: Arc::from([]),
            cursor: 0,
            end: 0,
            shown: 0,
        }
    }

    /// Its slot entities, by slot.
    pub fn slots(&self) -> &[Entity] {
        &self.slots
    }

    /// Its meshes, by shape.
    pub fn meshes(&self) -> &[Handle<Mesh>] {
        &self.meshes
    }

    pub fn material(&self) -> &Handle<ScatterMaterial> {
        &self.material
    }

    /// Whether every slot shows its set's instance (or is hidden past the set's end).
    pub fn settled(&self) -> bool {
        self.cursor >= self.end
    }

    /// Starts rewriting the slots to `target`: every slot that may be showing, or will.
    fn retarget(&mut self, target: Arc<[ScatterInstance]>) {
        self.end = self.shown.max(target.len());
        self.cursor = 0;
        self.target = target;
    }

    /// What slot `slot` should draw: its instance, or nothing (hidden).
    fn wanted(&self, slot: usize) -> Option<&ScatterInstance> {
        self.target.get(slot)
    }

    /// A slot entity's drawn parts for `instance` (a hidden placeholder for none).
    fn drawn(
        &self,
        instance: Option<&ScatterInstance>,
    ) -> (Transform, MeshTag, Mesh3d, Visibility) {
        match instance {
            Some(i) => (
                i.transform(),
                MeshTag::new(i.tag),
                Mesh3d(self.meshes[i.shape(self.meshes.len())].clone()),
                Visibility::Inherited,
            ),
            None => (
                Transform::IDENTITY,
                MeshTag::new(0),
                Mesh3d(self.meshes[0].clone()),
                Visibility::Hidden,
            ),
        }
    }

    /// Spawns one chunk of slots under `parent`, each drawing what it should if it lies in
    /// `writing` (this frame's slots), hidden otherwise.
    fn grow(&mut self, commands: &mut Commands, parent: Entity, writing: &std::ops::Range<usize>) {
        let start = self.slots.len();
        for slot in start..start + SCATTER_POOL_CHUNK {
            let instance = writing.contains(&slot).then(|| self.wanted(slot)).flatten();
            let entity = commands
                .spawn((
                    Name::new("Scatter slot"),
                    ScatterSlot { slot: slot as u32 },
                    self.drawn(instance),
                    MeshMaterial3d(self.material.clone()),
                    VisibilityRange::abrupt(0.0, self.visible_m),
                    NotShadowCaster,
                    RenderLayers::layer(TERRAIN_LAYER),
                    ChildOf(parent),
                ))
                .id();
            self.slots.push(entity);
        }
    }

    /// Despawns every slot.
    fn release(&mut self, commands: &mut Commands) {
        for e in self.slots.drain(..) {
            commands.entity(e).despawn();
        }
        (self.cursor, self.end, self.shown) = (0, 0, 0);
    }
}

/// The scatter's material.
pub type ScatterMaterial = ExtendedMaterial<StandardMaterial, ScatterExtension>;

/// The scatter's look uniform (`ScatterLook` in `scatter.wesl`).
#[derive(ShaderType, Copy, Clone, Debug, PartialEq, Reflect)]
pub struct ScatterLook {
    /// `xy`: the palette texel holding the kind's colour; `z`: the shading mode
    /// ([`ScatterKind::shader_mode`]); `w`: unused.
    pub texel: UVec4,
    /// Solid: `x`: the hue jitter (± radians about the grey axis); `y`: the value jitter (±
    /// share); `z`: how much a wet surface darkens (share); `w`: the wet band above the
    /// waterline (share of the instance's height). Foam: `rgb`: the foam's colour (linear);
    /// `w`: the wake's length (radii).
    pub colour: Vec4,
    /// `x`: the water's constant depth pull (m, `WATER_PULL_M`); `y`: its share of the view
    /// distance (`WATER_DEPTH_PULL`); `z`: the Earth's radius (m) for the curvature drop; `w`:
    /// foam only, the extra pull (m) that puts it in front of the water at its level.
    pub pull: Vec4,
    /// `x`: the least light (exposure applied) a surface shows, as the terrain's light ramp
    /// floors its shade, so faces turned from the sun read as soft shade, never black; `yzw`:
    /// unused.
    pub light: Vec4,
}

/// The scatter's extension of `StandardMaterial`: the terrain palette, the look, and the
/// region's terrain buffer and water textures (Task 11c fix round 5: a foam collar draws only
/// where the water does, read as the water's material reads it).
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct ScatterExtension {
    /// `palette_image()` (`Rgba8UnormSrgb`, read with `textureLoad`).
    #[texture(100, sample_type = "float", filterable = false)]
    pub palette: Handle<Image>,
    #[uniform(101)]
    pub look: ScatterLook,
    /// `TerrainScene::buffer` (its header's field layout).
    #[storage(102, read_only)]
    pub patches: Handle<ShaderBuffer>,
    /// `TerrainScene::inner_water` (`Rgba32Uint`, read with `textureLoad`).
    #[texture(103, sample_type = "u_int")]
    pub inner_water: Handle<Image>,
    #[texture(104, sample_type = "u_int")]
    pub apron_water: Handle<Image>,
}

/// The region water a scatter material binds ([`ScatterExtension`]'s last three).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScatterWater {
    pub patches: Handle<ShaderBuffer>,
    pub inner_water: Handle<Image>,
    pub apron_water: Handle<Image>,
}

impl ScatterWater {
    /// Stand-ins while no region's water is ready: dry 2 × 2 water textures (a `u_int` binding
    /// can't take the default image, which is a float one) and an empty terrain buffer (the
    /// default handle when there are no shader buffers: headless apps).
    pub fn placeholder(
        images: &mut Assets<Image>,
        buffers: Option<&mut Assets<ShaderBuffer>>,
    ) -> Self {
        use crate::terrain::material::{TerrainParams, terrain_buffer_bytes, water_image};
        use bevy::asset::RenderAssetUsages;
        use bevy::render::render_resource::BufferUsages;
        use timefly_core::worldgen::generate::Field;
        let dry = Field::dry(2, 1.0, vec![0.0; 4], vec![[0; 4]; 4]);
        let mut buffer = ShaderBuffer::new(
            terrain_buffer_bytes(&TerrainParams::default(), &[], 1),
            RenderAssetUsages::default(),
        );
        buffer.buffer_usage = BufferUsages::STORAGE | BufferUsages::COPY_DST;
        ScatterWater {
            patches: buffers.map(|b| b.add(buffer)).unwrap_or_default(),
            inner_water: images.add(water_image(&dry)),
            apron_water: images.add(water_image(&dry)),
        }
    }

    /// The region water of the terrain scene's buffer and textures.
    pub fn of(
        patches: &Handle<ShaderBuffer>,
        inner_water: &Handle<Image>,
        apron_water: &Handle<Image>,
    ) -> Self {
        ScatterWater {
            patches: patches.clone(),
            inner_water: inner_water.clone(),
            apron_water: apron_water.clone(),
        }
    }

    /// Binds this water to `extension`.
    pub fn bind(&self, extension: &mut ScatterExtension) {
        extension.patches = self.patches.clone();
        extension.inner_water = self.inner_water.clone();
        extension.apron_water = self.apron_water.clone();
    }
}

impl MaterialExtension for ScatterExtension {
    fn vertex_shader() -> ShaderRef {
        SCATTER_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        SCATTER_SHADER.into()
    }
    // Small and many: no shadows cast and no prepass (the region camera has none), so the
    // scatter draws in the main opaque pass only, where its vertex stage's camera is the view.
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
}

/// A scatter material over `palette` with `look`, binding `water` (shared with the pipeline
/// warm-up, `crate::terrain::warmup`, so its pipeline keys match).
pub fn scatter_material(
    palette: &Handle<Image>,
    look: ScatterLook,
    water: &ScatterWater,
) -> ScatterMaterial {
    ScatterMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.9,
            reflectance: 0.3,
            ..default()
        },
        extension: ScatterExtension {
            palette: palette.clone(),
            look,
            patches: water.patches.clone(),
            inner_water: water.inner_water.clone(),
            apron_water: water.apron_water.clone(),
        },
    }
}

pub struct ScatterPlugin;

impl Plugin for ScatterPlugin {
    fn build(&self, app: &mut App) {
        let shader = Shader::from_wesl(
            include_str!("../assets/shaders/scatter.wesl"),
            "embedded://timefly/scatter.wesl",
        );
        let _ = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(SCATTER_SHADER.id(), shader);
        app.add_plugins(MaterialPlugin::<ScatterMaterial>::default());
        add_scatter_systems(app);
    }
}

/// The systems without the render plugin and shader, for headless tests.
/// The rewrite runs before transform propagation, so a slot written (or spawned: the commands
/// apply at the sync point before it) in the frame its set shows is drawn at its new place, and
/// before visibility propagation, so the set's hidden state applies that frame too.
pub(crate) fn add_scatter_systems(app: &mut App) {
    app.add_systems(
        PostUpdate,
        update_scatter_pools
            .before(TransformSystems::Propagate)
            .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate),
    );
}

/// Each pool's step toward its set: retargets on a new set, rewrites up to
/// [`SCATTER_UPDATE_PER_FRAME`] slots (spawning chunks as they are reached), and hides the set's
/// entity until it has settled. An empty set despawns the slots at once.
fn update_scatter_pools(
    mut commands: Commands,
    mut pools: Query<
        (Entity, &ScatterSet, &mut ScatterPool, &mut Visibility),
        Without<ScatterSlot>,
    >,
    mut slots: Query<
        (&mut Transform, &mut MeshTag, &mut Mesh3d, &mut Visibility),
        With<ScatterSlot>,
    >,
) {
    for (entity, set, mut pool, mut visibility) in &mut pools {
        if !Arc::ptr_eq(&set.instances, &pool.target) {
            pool.retarget(set.instances.clone());
            debug!(kind = ?set.kind, instances = set.instances.len(), "scatter set replaced");
        }
        if pool.target.is_empty() && !pool.slots.is_empty() {
            debug!(kind = ?set.kind, slots = pool.slots.len(), "scatter pool released");
            pool.release(&mut commands);
        }
        let writing = pool.cursor..(pool.cursor + SCATTER_UPDATE_PER_FRAME).min(pool.end);
        // Slots past the target's end only need hiding, and those were shown, so they exist.
        let needed = writing.end.min(pool.target.len());
        let existing = pool.slots.len();
        while pool.slots.len() < needed {
            pool.grow(&mut commands, entity, &writing);
        }
        if pool.slots.len() > existing {
            debug!(kind = ?set.kind, from = existing, to = pool.slots.len(), "scatter pool grown");
        }
        for slot in writing.start..writing.end.min(existing) {
            let (t, tag, mesh, vis) = pool.drawn(pool.wanted(slot));
            if let Ok((mut ct, mut ctag, mut cmesh, mut cvis)) = slots.get_mut(pool.slots[slot]) {
                ct.set_if_neq(t);
                ctag.set_if_neq(tag);
                cmesh.set_if_neq(mesh);
                cvis.set_if_neq(vis);
            }
        }
        pool.shown = pool.shown.max(needed);
        pool.cursor = writing.end;
        if pool.settled() {
            pool.shown = pool.target.len();
        }
        visibility.set_if_neq(if pool.settled() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
}

#[cfg(test)]
mod tests {
    use bevy::asset::AssetPlugin;

    use super::*;

    fn instances(n: usize, salt: u32) -> Arc<[ScatterInstance]> {
        (0..n)
            .map(|i| {
                let i = i as u32;
                ScatterInstance {
                    translation: Vec3::new(i as f32, salt as f32, -(i as f32)),
                    scale: 1.0 + (i % 7) as f32 * 0.25,
                    yaw: (i % 13) as f32 * 0.4,
                    tag: ScatterInstance::tag_of(
                        (i % 5) as f32 / 4.0,
                        i.wrapping_mul(2_654_435_761) ^ salt,
                    ),
                }
            })
            .collect()
    }

    fn scatter_app() -> (App, Entity) {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Mesh>()
            .init_asset::<Image>()
            .init_asset::<ScatterMaterial>();
        add_scatter_systems(&mut app);
        let world = app.world_mut();
        let meshes: Vec<_> = (0..4)
            .map(|_| world.resource_mut::<Assets<Mesh>>().add(Cuboid::default()))
            .collect();
        let material = world
            .resource_mut::<Assets<ScatterMaterial>>()
            .add(scatter_material(
                &Handle::default(),
                test_look(),
                &ScatterWater::default(),
            ));
        let set = world
            .spawn((
                ScatterSet::empty(ScatterKind::Boulders),
                ScatterPool::new(meshes, material, 6_000.0),
            ))
            .id();
        (app, set)
    }

    fn test_look() -> ScatterLook {
        ScatterLook {
            texel: UVec4::ZERO,
            colour: Vec4::ZERO,
            pull: Vec4::ZERO,
            light: Vec4::ZERO,
        }
    }

    fn show(app: &mut App, set: Entity, instances: &Arc<[ScatterInstance]>) {
        app.world_mut().entity_mut(set).insert(ScatterSet {
            kind: ScatterKind::Boulders,
            instances: instances.clone(),
        });
    }

    /// Updates until the pool has settled (at most `frames`), returning the frames it took.
    fn settle(app: &mut App, set: Entity, frames: usize) -> usize {
        for frame in 1..=frames {
            app.update();
            if app.world().get::<ScatterPool>(set).unwrap().settled() {
                return frame;
            }
        }
        panic!("not settled within {frames} frames");
    }

    /// Every slot entity of the pool with its slot, transform, tag, mesh and visibility.
    fn slot_states(app: &mut App) -> Vec<(u32, Transform, u32, Handle<Mesh>, Visibility)> {
        let world = app.world_mut();
        let mut q = world.query::<(&ScatterSlot, &Transform, &MeshTag, &Mesh3d, &Visibility)>();
        let mut v: Vec<_> = q
            .iter(world)
            .map(|(s, t, tag, m, vis)| (s.slot, *t, tag.value, m.0.clone(), *vis))
            .collect();
        v.sort_by_key(|s| s.0);
        v
    }

    /// The slots draw exactly `want`, in order, with the rest hidden.
    fn assert_draws(app: &mut App, set: Entity, want: &Arc<[ScatterInstance]>) {
        let meshes = app
            .world()
            .get::<ScatterPool>(set)
            .unwrap()
            .meshes()
            .to_vec();
        for (slot, transform, tag, mesh, vis) in slot_states(app) {
            match want.get(slot as usize) {
                Some(i) => {
                    assert_eq!(vis, Visibility::Inherited, "slot {slot}");
                    assert_eq!(transform, i.transform(), "slot {slot}");
                    assert_eq!(tag, i.tag, "slot {slot}");
                    assert_eq!(mesh, meshes[i.shape(meshes.len())], "slot {slot}");
                }
                None => assert_eq!(vis, Visibility::Hidden, "slot {slot}"),
            }
        }
        assert_eq!(
            *app.world().get::<Visibility>(set).unwrap(),
            Visibility::Inherited,
            "a settled set shows"
        );
    }

    #[test]
    fn a_scatter_set_reuses_its_pool() {
        let (mut app, set) = scatter_app();
        let (a, b, c) = (
            instances(3_000, 1),
            instances(1_000, 2),
            instances(3_000, 3),
        );
        show(&mut app, set, &a);
        settle(&mut app, set, 8);
        let pool: Vec<Entity> = app
            .world()
            .get::<ScatterPool>(set)
            .unwrap()
            .slots()
            .to_vec();
        assert_eq!(
            pool.len(),
            3_072,
            "3,000 instances in whole chunks of 1,024"
        );
        assert_draws(&mut app, set, &a);
        // Fewer: the same entities, the rest hidden.
        show(&mut app, set, &b);
        settle(&mut app, set, 8);
        assert_eq!(
            app.world().get::<ScatterPool>(set).unwrap().slots(),
            &pool[..]
        );
        assert_draws(&mut app, set, &b);
        let hidden = slot_states(&mut app)
            .iter()
            .filter(|s| s.4 == Visibility::Hidden)
            .count();
        assert_eq!(hidden, 2_072);
        // More again: still the one pool.
        show(&mut app, set, &c);
        settle(&mut app, set, 8);
        assert_eq!(
            app.world().get::<ScatterPool>(set).unwrap().slots(),
            &pool[..]
        );
        assert_draws(&mut app, set, &c);
        let world = app.world_mut();
        assert_eq!(world.query::<&ScatterSlot>().iter(world).count(), 3_072);
        // Every slot is a child of the set's entity, on the region's layer, out to the range.
        for &e in &pool {
            let entity = app.world().entity(e);
            assert_eq!(entity.get::<ChildOf>().unwrap().parent(), set);
            assert_eq!(
                *entity.get::<RenderLayers>().unwrap(),
                RenderLayers::layer(TERRAIN_LAYER)
            );
            let range = entity.get::<VisibilityRange>().unwrap();
            assert!(range.is_abrupt(), "no dithered pipeline");
            assert!(range.is_visible_at_all(5_999.0) && range.is_culled(6_001.0));
            assert!(entity.contains::<NotShadowCaster>());
        }
        // An empty set releases the pool.
        show(&mut app, set, &Arc::from([]));
        app.update();
        app.update();
        assert!(
            app.world()
                .get::<ScatterPool>(set)
                .unwrap()
                .slots()
                .is_empty()
        );
        for e in pool {
            assert!(app.world().get_entity(e).is_err());
        }
    }

    #[test]
    fn scatter_entities_share_one_mesh_and_material_per_shape() {
        let (mut app, set) = scatter_app();
        let a = instances(2_500, 7);
        show(&mut app, set, &a);
        settle(&mut app, set, 8);
        let pool = app.world().get::<ScatterPool>(set).unwrap();
        let (meshes, material) = (pool.meshes().to_vec(), pool.material().clone());
        let world = app.world_mut();
        let mut q = world
            .query_filtered::<(&Mesh3d, &MeshMaterial3d<ScatterMaterial>), With<ScatterSlot>>();
        let mut used = std::collections::HashSet::new();
        for (mesh, mat) in q.iter(world) {
            assert_eq!(mat.0, material, "one material for the kind");
            assert!(meshes.contains(&mesh.0), "one of the kind's meshes");
            used.insert(mesh.0.id());
        }
        // Each shape is drawn, by the same mesh asset everywhere: four batches.
        assert_eq!(used.len(), 4);
        assert_eq!(world.resource::<Assets<Mesh>>().len(), 4);
        assert_eq!(world.resource::<Assets<ScatterMaterial>>().len(), 1);
    }

    /// Slots written each frame: the slot entities whose drawn state changed since the last
    /// frame (spawned ones included).
    #[derive(Resource, Default)]
    struct Writes(Vec<usize>);

    /// A slot whose drawn state changed.
    type Written = (
        With<ScatterSlot>,
        Or<(
            Changed<Transform>,
            Changed<MeshTag>,
            Changed<Mesh3d>,
            Changed<Visibility>,
        )>,
    );

    fn count_writes(mut writes: ResMut<Writes>, q: Query<(), Written>) {
        writes.0.push(q.iter().count());
    }

    #[test]
    fn updates_are_spread_over_frames() {
        let (mut app, set) = scatter_app();
        app.init_resource::<Writes>()
            .add_systems(Last, count_writes);
        let a = instances(16_384, 11);
        show(&mut app, set, &a);
        let frames = settle(&mut app, set, 8);
        assert_eq!(frames, 8);
        assert_draws(&mut app, set, &a);
        let writes = app.world_mut().resource_mut::<Writes>().0.split_off(0);
        assert!(
            writes.iter().all(|&n| n <= SCATTER_UPDATE_PER_FRAME),
            "{writes:?}"
        );
        assert_eq!(writes.iter().sum::<usize>(), 16_384);
        // While it settles, the set is hidden: never a mix of two sets.
        let b = instances(16_384, 12);
        show(&mut app, set, &b);
        for _ in 0..7 {
            app.update();
            assert_eq!(
                *app.world().get::<Visibility>(set).unwrap(),
                Visibility::Hidden
            );
        }
        app.update();
        assert_draws(&mut app, set, &b);
        let writes = app.world_mut().resource_mut::<Writes>().0.split_off(0);
        assert!(
            writes.iter().all(|&n| n <= SCATTER_UPDATE_PER_FRAME),
            "{writes:?}"
        );
    }

    /// Every slot drawn in the frame the set shows has its instance's world transform: the
    /// rewrite runs before transform propagation (and its spawns are applied before it), so no
    /// slot shows a stale `GlobalTransform` (the old set's place, or the origin for a new slot).
    #[test]
    fn a_shown_set_is_propagated_in_the_frame_it_shows() {
        let (mut app, set) = scatter_app();
        app.add_plugins(bevy::transform::TransformPlugin);
        let check = |app: &mut App, want: &Arc<[ScatterInstance]>| {
            let world = app.world_mut();
            let mut q = world.query::<(&ScatterSlot, &GlobalTransform, &Visibility)>();
            let mut shown = 0;
            for (slot, global, vis) in q.iter(world) {
                if *vis == Visibility::Hidden {
                    continue;
                }
                let want = GlobalTransform::from(want[slot.slot as usize].transform());
                assert!(
                    global.affine().abs_diff_eq(want.affine(), 1e-4),
                    "slot {}: {global:?}, want {want:?}",
                    slot.slot
                );
                shown += 1;
            }
            assert_eq!(shown, want.len());
        };
        // Fresh slots (the last chunk spawns in the frame the set shows), then a replace.
        for (n, salt) in [(3_000, 21), (2_500, 22)] {
            let want = instances(n, salt);
            show(&mut app, set, &want);
            settle(&mut app, set, 8);
            assert_eq!(
                *app.world().get::<Visibility>(set).unwrap(),
                Visibility::Inherited
            );
            check(&mut app, &want);
        }
    }

    #[test]
    fn wetness_round_trips_through_the_tag() {
        for k in 0..=1_000 {
            let wet = k as f32 / 1_000.0;
            for seed in [0u32, 1, 0x00AB_CDEF, 0xFFFF_FFFF, 0x1234_5678] {
                let i = ScatterInstance {
                    translation: Vec3::ZERO,
                    scale: 1.0,
                    yaw: 0.0,
                    tag: ScatterInstance::tag_of(wet, seed),
                };
                assert!(
                    (i.wet() - wet).abs() <= 0.5 / 255.0 + 1e-6,
                    "{wet}: {}",
                    i.wet()
                );
                assert_eq!(i.seed(), seed & 0xFF_FFFF);
                assert_eq!(i.tag >> 24, (wet * 255.0).round() as u32);
            }
        }
        // Outside [0, 1] it clamps; the ends are exact.
        let tag = |w: f32| ScatterInstance::tag_of(w, 5);
        assert_eq!(tag(-0.5), tag(0.0));
        assert_eq!(tag(1.5), tag(1.0));
        assert_eq!(tag(f32::NAN), tag(0.0));
        assert_eq!(tag(1.0), 0xFF00_0005);
        assert_eq!(tag(0.0), 5);
        // The shader reads it the same way.
        assert!(SCATTER_WESL.contains("let wet = f32(tag >> 24u) / 255.0;"));
        assert!(SCATTER_WESL.contains("let seed = tag & 0xFFFFFFu;"));
    }

    const SCATTER_WESL: &str = include_str!("../assets/shaders/scatter.wesl");

    #[test]
    fn scatter_look_matches_its_wesl_mirror() {
        use crate::art::wesl_struct_fields;
        let names: Vec<String> = wesl_struct_fields(SCATTER_WESL, "ScatterLook")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["texel", "colour", "pull", "light"]);
        // The shade floor applies after the lighting, to the jittered and wet-darkened colour.
        assert!(
            SCATTER_WESL
                .contains("color = vec4<f32>(max(color.rgb, albedo * look.light.x), color.a);")
        );
    }
}
