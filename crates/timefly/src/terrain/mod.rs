//! CDLOD terrain in the shell (design spec §4.2, §6.3): the region's grid drawn as pooled patch
//! entities with Bevy's PBR lighting and shadows.
//!
//! - **Scene.** When an [`ActiveRegion`] appears, the plugin uploads its fields as six textures
//!   (heights, surface codes and water, inner and apron), builds one grid mesh, one
//!   [`TerrainMaterial`] and one patch buffer, and spawns the [`RegionCamera`] and the
//!   [`TerrainSun`], then frees the grid's river texels, which only the GPU reads. A replaced
//!   region (a snapshot change, a hop) swaps the textures in place over four frames: the old
//!   water released on the swap frame, the new water on the next, nothing on the third while
//!   its staging drains, the heights and surface codes on the fourth
//!   ([`TerrainScene::steps_remaining`]), so the upload's memory never peaks with both water
//!   sets or both uploads' staging; navigation holds the cloud opaque meanwhile
//!   ([`REPLACE_EXTRA_HELD_FRAMES`]).
//!   A removed one despawns everything and drops every handle, so the GPU memory goes back to
//!   where it was.
//! - **Patches.** Every frame, [`select_patches`] runs for the region camera. Patch `i` goes into
//!   buffer slot `i`, drawn by the pool entity whose [`MeshTag`] is `i`. The pool only grows, in
//!   chunks of [`PATCH_POOL_CHUNK`] up to [`MAX_TERRAIN_PATCHES`]; spare entities are hidden,
//!   never despawned, while the region lasts.
//! - **View.** While the scene exists and [`TerrainSettings::visible`] is on, the region camera
//!   replaces the globe camera (see [`view`]).
//!
//! `F2` toggles the wireframe view ([`TerrainSettings::wireframe`]).

pub mod material;
pub mod view;
pub mod warmup;

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::camera::primitives::Aabb;
use bevy::camera::visibility::{NoAutoAabb, RenderLayers, VisibilitySystems};
use bevy::math::DVec3;
use bevy::mesh::{Indices, MeshTag, PrimitiveTopology};
use bevy::pbr::MaterialPlugin;
use bevy::prelude::*;
use bevy::render::render_resource::BufferUsages;
use bevy::render::storage::ShaderBuffer;
use timefly_core::globe::EARTH_RADIUS_KM;
use timefly_core::terrain::{LodSettings, PATCH_QUADS, Patch, select_patches};
use timefly_core::worldgen::generate::RegionGrid;
use tracing::instrument;

use crate::region::ActiveRegion;
use material::{
    PatchInstance, TerrainExtension, TerrainLook, TerrainMaterial, TerrainParams,
    add_terrain_shaders, height_image, palette_image, patch_instances, surface_image,
    terrain_buffer_bytes, terrain_look, water_image,
};
use view::{RegionCamera, apply_view_ranges, fly_view_ranges, spawn_region_view, sync_region_view};

/// The render layer of the terrain, the region camera and the region's sun (the globe uses
/// layers 0–3).
pub const TERRAIN_LAYER: usize = 4;
/// The most patches drawn at once: the patch buffer's slots and the pool's ceiling. Above
/// [`select_patches`]' worst case for any camera over the region at the camera's clearance
/// (`pool_capacity_covers_every_camera`).
pub const MAX_TERRAIN_PATCHES: usize = 640;
/// The pool grows by this many entities at a time.
pub const PATCH_POOL_CHUNK: usize = 64;
/// The wireframe toggle.
pub const WIREFRAME_KEY: KeyCode = KeyCode::F2;

const EARTH_RADIUS_M: f32 = (EARTH_RADIUS_KM * 1000.0) as f32;

/// Systems that build, replace and tear down the [`TerrainScene`]. The sky and the water order
/// themselves after it.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TerrainSceneSystems;

/// The system that switches between the globe and region cameras (`TerrainSettings::visible`).
/// Whatever writes the fly's pose for the active camera (`crate::fly_camera::FlySystems::Apply`,
/// `crate::globe_flight::GlobeFlightSystems::Apply`) runs after it, so on a swap frame the pose
/// is the newly active world's.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RegionViewSystems;

/// Terrain options set at launch.
#[derive(Resource, Copy, Clone, Debug, PartialEq, Eq)]
pub struct TerrainOptions {
    /// Draw the sand seas' dunes. `--no-dunes` (web `?no-dunes`) turns them off for slower
    /// machines: the shader then skips all dune work, and sand seas draw as flat, still sand.
    pub dunes: bool,
}

impl Default for TerrainOptions {
    fn default() -> Self {
        TerrainOptions { dunes: true }
    }
}

/// The painterly look for a region: the sky's zenith as the shadow hue, the region sun's
/// illumination, the centre's latitude for the snowline, the snow-shading constants, the
/// region's frame (planet position) and prevailing wind for the dunes, and whether the dunes are
/// drawn at all.
pub fn look_for(grid: &RegionGrid, options: TerrainOptions) -> TerrainLook {
    let (era, biome) = crate::sky::region_look(grid);
    let (zenith, _, _) = crate::sky::sky_tint(era, biome);
    let z = zenith.to_linear();
    terrain_look(
        Vec3::new(z.red, z.green, z.blue),
        view::full_sun_illumination(),
        crate::sky::centre_lat_deg(grid),
        grid.spec.sea_level_anomaly_m as f32,
        &grid.spec.frame(),
        options.dunes,
    )
}

/// What the terrain shows. Tasks 8 and 9 hide the terrain (and give the view back to the globe
/// camera) with `visible = false`.
#[derive(Resource, Copy, Clone, Debug, PartialEq, Eq)]
pub struct TerrainSettings {
    pub visible: bool,
    pub wireframe: bool,
    /// While set, a replaced [`ActiveRegion`] waits: the scene keeps drawing the grid it has.
    /// Region navigation holds a snapshot change's new region until the fade fully covers the
    /// view, then releases it in the frame it moves the camera, so terrain and camera change
    /// together.
    pub hold_swap: bool,
}

impl Default for TerrainSettings {
    fn default() -> Self {
        TerrainSettings {
            visible: true,
            wireframe: false,
            hold_swap: false,
        }
    }
}

/// A pooled patch entity; it draws buffer slot `slot` (its [`MeshTag`]).
#[derive(Component, Copy, Clone, Debug, PartialEq, Eq)]
pub struct TerrainPatch {
    pub slot: u32,
}

/// The terrain's GPU resources and entities for the current region. Present exactly while the
/// terrain has a region to draw. Read-only for other plugins (the water binds its textures).
#[derive(Resource, Debug)]
pub struct TerrainScene {
    /// The grid the textures hold (compared by pointer to notice a replaced region).
    pub grid: Arc<RegionGrid>,
    pub settings: LodSettings,
    pub material: Handle<TerrainMaterial>,
    pub buffer: Handle<ShaderBuffer>,
    pub mesh: Handle<Mesh>,
    pub inner_height: Handle<Image>,
    pub apron_height: Handle<Image>,
    pub inner_surface: Handle<Image>,
    pub apron_surface: Handle<Image>,
    /// The fields' water ([`material::water_image`]), for the water's material (Task 8).
    pub inner_water: Handle<Image>,
    pub apron_water: Handle<Image>,
    pub palette: Handle<Image>,
    /// The header without the per-frame camera and debug values.
    pub params: TerrainParams,
    /// `[lowest, highest]` height in either field (m), for the patch bounds.
    pub height_range_m: [f32; 2],
    /// Pool entities by slot.
    pub pool: Vec<Entity>,
    pub camera: Entity,
    pub sun: Entity,
    /// Whether a frame needing more than [`MAX_TERRAIN_PATCHES`] was already reported.
    overflow_reported: bool,
    /// A replace's uploads still to make ([`TerrainScene::steps_remaining`]).
    staged: Staged,
}

/// Frames a replace draws incomplete or mixed fields after its swap frame, for which navigation
/// keeps the cloud at density exactly 1 (`NavInputs::fields_settling`): the owner's rulings of
/// 2026-11-25, two more frames in cloud than M3. The swap frame itself binds no water (the old
/// water is freed in the render world the frame after its removal); the next draws the new
/// water over the old heights, and so does the one after, which uploads nothing while the
/// water's staging drains; the fourth is complete.
pub const REPLACE_EXTRA_HELD_FRAMES: u32 = 2;

/// A replace's uploads still to make ([`begin_replace`], [`advance_replace`]).
enum Staged {
    Done,
    /// The new grid's water, built on the swap frame (when the grid's rivers could be freed),
    /// added to the assets the frame after.
    Water(Box<[Image; 2]>),
    /// A frame with no upload, while the water's staging drains (the GPU still holds it for the
    /// frames in flight).
    Drain,
    /// The new grid's heights and surface codes.
    Heights,
}

impl std::fmt::Debug for Staged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Staged::Done => "Done",
            Staged::Water(_) => "Water",
            Staged::Drain => "Drain",
            Staged::Heights => "Heights",
        })
    }
}

impl TerrainScene {
    /// The frames a replace still has to step through: 3 after its swap frame (the water, a
    /// frame for its staging to drain, then the heights and surface codes), 2 and 1 after the
    /// next two, 0 once the fields are the grid's.
    pub fn steps_remaining(&self) -> u32 {
        match self.staged {
            Staged::Done => 0,
            Staged::Heights => 1,
            Staged::Drain => 2,
            Staged::Water(_) => 3,
        }
    }

    /// Whether the scene's fields are incomplete or mixed (a replace under way).
    pub fn fields_pending(&self) -> bool {
        self.steps_remaining() > 0
    }

    /// Whether this frame's step (the scene runs after navigation) still leaves the fields
    /// incomplete, so navigation must draw the frame at cloud density 1.
    pub fn holds_cloud(&self) -> bool {
        self.steps_remaining() > 1
    }
}

/// One frame's patches: what goes into the buffer and onto the pool.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchPlan {
    /// Buffer slots in order; at most [`MAX_TERRAIN_PATCHES`].
    pub instances: Vec<PatchInstance>,
    /// The world bounds of each instance's displaced patch.
    pub bounds: Vec<Aabb>,
    /// The pool size after this frame.
    pub pool_len: usize,
    /// Selected patches that did not fit (zero unless the selection exceeds the ceiling).
    pub dropped: usize,
}

/// The pool size for `needed` patches when it holds `current`: never smaller than now, grown to
/// a whole number of [`PATCH_POOL_CHUNK`]s, capped at [`MAX_TERRAIN_PATCHES`].
pub fn pool_len(current: usize, needed: usize) -> usize {
    let chunked = needed.div_ceil(PATCH_POOL_CHUNK) * PATCH_POOL_CHUNK;
    current.max(chunked).min(MAX_TERRAIN_PATCHES)
}

/// World bounds of a patch's displaced surface: its footprint, from the lowest height minus the
/// curvature drop at its corner farthest from the camera up to the highest height. Morphing
/// keeps vertices inside the footprint, and the drop only lowers them. A metre of slack (and a
/// thousandth of the drop) covers the shader's f32 rounding.
pub fn patch_bounds(patch: &Patch, height_range_m: [f32; 2], camera: Vec3) -> Aabb {
    let min_xz = Vec2::new(patch.origin_m[0] as f32, patch.origin_m[1] as f32);
    let max_xz = min_xz + Vec2::splat(patch.size_m as f32);
    let cam = Vec2::new(camera.x, camera.z);
    let far = (min_xz - cam).abs().max((max_xz - cam).abs());
    let drop = far.length_squared() / (2.0 * EARTH_RADIUS_M);
    Aabb::from_min_max(
        Vec3::new(min_xz.x, height_range_m[0] - drop * 1.001 - 1.0, min_xz.y),
        Vec3::new(max_xz.x, height_range_m[1] + 1.0, max_xz.y),
    )
}

/// Selection, packing and pool sizing for one frame, for a camera at local metres `camera`.
pub fn plan_patches(
    camera: Vec3,
    settings: &LodSettings,
    height_range_m: [f32; 2],
    current_pool: usize,
) -> PatchPlan {
    let mut patches = select_patches(camera.as_dvec3(), settings);
    let dropped = patches.len().saturating_sub(MAX_TERRAIN_PATCHES);
    if dropped > 0 {
        // Keep the nearest: the farthest patches matter least.
        let distance = |p: &Patch| {
            let centre = DVec3::new(
                p.origin_m[0] + p.size_m / 2.0,
                0.0,
                p.origin_m[1] + p.size_m / 2.0,
            );
            centre.distance_squared(camera.as_dvec3())
        };
        patches.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
        patches.truncate(MAX_TERRAIN_PATCHES);
    }
    PatchPlan {
        instances: patch_instances(&patches, settings),
        bounds: patches
            .iter()
            .map(|p| patch_bounds(p, height_range_m, camera))
            .collect(),
        pool_len: pool_len(current_pool, patches.len()),
        dropped,
    }
}

/// The shared patch mesh: `(quads + 1)²` vertices at `(i, 0, j) / quads`, each quad split on its
/// `(i, j)–(i + 1, j + 1)` diagonal and wound to face `+y`. That diagonal is the one CDLOD's
/// collapse-to-the-lower-index morph needs: a fully morphed 2 × 2 block becomes exactly its
/// parent quad, split the same way.
pub fn grid_mesh(quads: u32) -> Mesh {
    let n = quads + 1;
    let positions: Vec<[f32; 3]> = (0..n)
        .flat_map(|j| (0..n).map(move |i| [i as f32 / quads as f32, 0.0, j as f32 / quads as f32]))
        .collect();
    let at = |i: u32, j: u32| j * n + i;
    let indices: Vec<u32> = (0..quads)
        .flat_map(|j| (0..quads).map(move |i| (i, j)))
        .flat_map(|(i, j)| {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            [a, c, d, a, d, b]
        })
        .collect();
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

/// `[lowest, highest]` height over both fields of a grid.
pub fn height_range_m(grid: &RegionGrid) -> [f32; 2] {
    grid.inner
        .elevation_m
        .iter()
        .chain(&grid.apron.elevation_m)
        .fold([f32::INFINITY, f32::NEG_INFINITY], |[lo, hi], &h| {
            [lo.min(h), hi.max(h)]
        })
}

pub struct TerrainCdlodPlugin;

impl Plugin for TerrainCdlodPlugin {
    fn build(&self, app: &mut App) {
        add_terrain_shaders(&mut app.world_mut().resource_mut::<Assets<Shader>>());
        app.add_plugins(MaterialPlugin::<TerrainMaterial>::default());
        add_terrain_systems(app);
        debug!(
            max_patches = MAX_TERRAIN_PATCHES,
            layer = TERRAIN_LAYER,
            "terrain plugin ready"
        );
    }
}

/// Everything but the render plugin and the shaders, so the pool runs in headless tests.
pub(crate) fn add_terrain_systems(app: &mut App) {
    app.init_resource::<TerrainSettings>()
        .add_systems(
            Update,
            (
                sync_terrain_scene.in_set(TerrainSceneSystems),
                toggle_wireframe,
                sync_region_view.in_set(RegionViewSystems),
            )
                .chain(),
        )
        .add_systems(
            PostUpdate,
            (
                update_patches.before(VisibilitySystems::VisibilityPropagate),
                apply_view_ranges.before(bevy::camera::CameraUpdateSystems),
            ),
        );
}

fn toggle_wireframe(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut settings: ResMut<TerrainSettings>,
) {
    if keys.is_some_and(|k| k.just_pressed(WIREFRAME_KEY)) {
        settings.wireframe = !settings.wireframe;
        info!(wireframe = settings.wireframe, "terrain wireframe toggled");
    }
}

/// What the scene must do for the current region.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum SceneAction {
    Keep,
    Build,
    Replace,
    Teardown,
}

fn scene_action(region: Option<&Arc<RegionGrid>>, scene: Option<&Arc<RegionGrid>>) -> SceneAction {
    match (region, scene) {
        (None, None) => SceneAction::Keep,
        (Some(_), None) => SceneAction::Build,
        (None, Some(_)) => SceneAction::Teardown,
        (Some(r), Some(s)) if Arc::ptr_eq(r, s) => SceneAction::Keep,
        (Some(_), Some(_)) => SceneAction::Replace,
    }
}

/// The region's textures, by field.
struct FieldImages {
    inner_height: Handle<Image>,
    apron_height: Handle<Image>,
    inner_surface: Handle<Image>,
    apron_surface: Handle<Image>,
    inner_water: Handle<Image>,
    apron_water: Handle<Image>,
}

/// The bytes of a region's field textures: what one upload copies to the GPU (each texture once
/// more through the queue's staging), and the GPU textures it allocates.
pub fn field_texture_bytes(grid: &RegionGrid) -> usize {
    let texels = |f: &timefly_core::worldgen::generate::Field| (f.res as usize).pow(2);
    let per_texel = 4 + 4 + material::WATER_TEXEL_BYTES; // height, surface, water
    (texels(&grid.inner) + texels(&grid.apron)) * per_texel
}

#[instrument(level = "debug", skip_all, fields(inner = grid.inner.res, apron = grid.apron.res))]
fn upload_fields(grid: &RegionGrid, images: &mut Assets<Image>) -> FieldImages {
    let [inner_water, apron_water] = upload_water(grid, images);
    let [inner_height, apron_height, inner_surface, apron_surface] = upload_heights(grid, images);
    debug!(
        mib = format!("{:.1}", field_texture_bytes(grid) as f64 / 1_048_576.0),
        "terrain field textures uploaded"
    );
    FieldImages {
        inner_height,
        apron_height,
        inner_surface,
        apron_surface,
        inner_water,
        apron_water,
    }
}

/// The water textures, inner and apron (16 B a texel).
fn upload_water(grid: &RegionGrid, images: &mut Assets<Image>) -> [Handle<Image>; 2] {
    [&grid.inner, &grid.apron].map(|f| images.add(water_image(f)))
}

/// The height and surface textures: inner and apron heights, then inner and apron surfaces
/// (8 B a texel).
fn upload_heights(grid: &RegionGrid, images: &mut Assets<Image>) -> [Handle<Image>; 4] {
    [
        images.add(height_image(&grid.inner)),
        images.add(height_image(&grid.apron)),
        images.add(surface_image(&grid.inner)),
        images.add(surface_image(&grid.apron)),
    ]
}

/// Frees the grid's river texels once its water textures hold them (8 B a texel, 10 MiB at full
/// size): no CPU code reads them (the fly's floor reads the levels). Only while the region
/// holds the only reference, which it does from the region job until the scene adopts the grid;
/// otherwise they stay.
fn free_rivers(grid: &mut Arc<RegionGrid>) {
    match Arc::get_mut(grid) {
        Some(g) => {
            let before = g.bytes();
            g.inner.free_rivers();
            g.apron.free_rivers();
            debug!(
                freed = before - g.bytes(),
                "region river texels freed after the upload"
            );
        }
        None => debug!("the region grid is shared; its river texels stay"),
    }
}

/// Gathers the region, decides with [`scene_action`], and builds, replaces or tears down the
/// terrain scene.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn sync_terrain_scene(
    mut commands: Commands,
    region: Option<ResMut<ActiveRegion>>,
    terrain: Res<TerrainSettings>,
    mut scene: Option<ResMut<TerrainScene>>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut materials: ResMut<Assets<TerrainMaterial>>,
    options: Option<Res<TerrainOptions>>,
) {
    let options = options.map_or_else(TerrainOptions::default, |o| *o);
    // A staged replace's next step comes first, whatever this frame brings.
    if let Some(scene) = scene.as_deref_mut() {
        advance_replace(scene, &mut images, &mut materials, options);
    }
    let action = match scene_action(
        region.as_ref().map(|r| &r.grid),
        scene.as_ref().map(|s| &s.grid),
    ) {
        SceneAction::Replace if terrain.hold_swap => SceneAction::Keep,
        action => action,
    };
    match (action, region, scene) {
        (SceneAction::Build, Some(mut region), _) => {
            // Freeing the rivers changes no value anything watches the region for.
            let scene = build_scene(
                &mut commands,
                &mut region.bypass_change_detection().grid,
                &mut images,
                &mut meshes,
                &mut buffers,
                &mut materials,
                options,
            );
            commands.insert_resource(scene);
        }
        (SceneAction::Replace, Some(mut region), Some(mut scene)) => {
            // The camera stays put: region navigation places it over the new region.
            begin_replace(
                &mut scene,
                &mut region.bypass_change_detection().grid,
                &mut images,
            );
        }
        (SceneAction::Teardown, _, Some(scene)) => {
            info!(patches = scene.pool.len(), "terrain scene removed");
            for &e in scene.pool.iter().chain([&scene.camera, &scene.sun]) {
                commands.entity(e).despawn();
            }
            commands.remove_resource::<TerrainScene>();
        }
        _ => {}
    }
}

#[instrument(level = "debug", skip_all, fields(snapshot_years = grid.spec.snapshot.0))]
fn build_scene(
    commands: &mut Commands,
    grid: &mut Arc<RegionGrid>,
    images: &mut Assets<Image>,
    meshes: &mut Assets<Mesh>,
    buffers: &mut Assets<ShaderBuffer>,
    materials: &mut Assets<TerrainMaterial>,
    options: TerrainOptions,
) -> TerrainScene {
    let settings = LodSettings::region();
    let params = TerrainParams::new(grid, &settings);
    let fields = upload_fields(grid, images);
    free_rivers(grid);
    let palette = images.add(palette_image());
    let buffer = buffers.add(patch_buffer(&params, &[]));
    let material = materials.add(terrain_material(
        &fields,
        &palette,
        &buffer,
        look_for(grid, options),
    ));
    let (camera, sun) = spawn_region_view(commands, grid);
    let height_range_m = height_range_m(grid);
    info!(
        lowest_m = height_range_m[0],
        highest_m = height_range_m[1],
        "terrain scene built"
    );
    TerrainScene {
        grid: grid.clone(),
        settings,
        material,
        buffer,
        mesh: meshes.add(grid_mesh(PATCH_QUADS)),
        inner_height: fields.inner_height,
        apron_height: fields.apron_height,
        inner_surface: fields.inner_surface,
        apron_surface: fields.apron_surface,
        inner_water: fields.inner_water,
        apron_water: fields.apron_water,
        palette,
        params,
        height_range_m,
        pool: Vec::new(),
        camera,
        sun,
        overflow_reported: false,
        staged: Staged::Done,
    }
}

/// The patch buffer: the header and up to [`MAX_TERRAIN_PATCHES`] instances.
fn patch_buffer(params: &TerrainParams, instances: &[PatchInstance]) -> ShaderBuffer {
    let mut buffer = ShaderBuffer::new(
        terrain_buffer_bytes(params, instances, MAX_TERRAIN_PATCHES),
        RenderAssetUsages::default(),
    );
    buffer.buffer_usage = BufferUsages::STORAGE | BufferUsages::COPY_DST;
    buffer.label = "terrain patches".into();
    buffer
}

/// The terrain's material over a region's textures (shared with the pipeline warm-up, so its
/// pipeline keys match).
fn terrain_material(
    fields: &FieldImages,
    palette: &Handle<Image>,
    buffer: &Handle<ShaderBuffer>,
    look: TerrainLook,
) -> TerrainMaterial {
    TerrainMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.95,
            reflectance: 0.2,
            ..default()
        },
        extension: TerrainExtension {
            patches: buffer.clone(),
            inner_height: fields.inner_height.clone(),
            apron_height: fields.apron_height.clone(),
            inner_surface: fields.inner_surface.clone(),
            apron_surface: fields.apron_surface.clone(),
            palette: palette.clone(),
            look,
        },
    }
}

/// A replaced region at the same LOD settings, its uploads spread over four frames so the old
/// and new image data, their staging and the old and new GPU textures never all peak together
/// (the hop's memory, M3's gate). This frame, the swap's (at cloud density 1): the old water's
/// images are removed, the scene takes the new grid, and the new water is built while the
/// region alone holds the grid, so its rivers can be freed; nothing binds water this frame. The
/// next three ([`advance_replace`]): the new water, once the render world has freed the old; a
/// frame without uploads while the water's staging drains; then the heights and surface codes,
/// with the patches' header, the bounds and the look. The terrain draws the old heights until
/// then; the same material and pool throughout.
fn begin_replace(scene: &mut TerrainScene, grid: &mut Arc<RegionGrid>, images: &mut Assets<Image>) {
    release(images, &mut scene.inner_water);
    release(images, &mut scene.apron_water);
    let water = Box::new([water_image(&grid.inner), water_image(&grid.apron)]);
    free_rivers(grid);
    scene.grid = grid.clone();
    scene.staged = Staged::Water(water);
    debug!("terrain water released; the new water follows next frame, then the heights");
}

/// The next step of a staged replace, if one is under way: the water, a frame to drain its
/// staging, then the heights.
fn advance_replace(
    scene: &mut TerrainScene,
    images: &mut Assets<Image>,
    materials: &mut Assets<TerrainMaterial>,
    options: TerrainOptions,
) {
    match std::mem::replace(&mut scene.staged, Staged::Done) {
        Staged::Done => {}
        Staged::Water(water) => {
            let [inner, apron] = *water;
            scene.inner_water = images.add(inner);
            scene.apron_water = images.add(apron);
            scene.staged = Staged::Drain;
            debug!("terrain water replaced; the heights follow in two frames");
        }
        Staged::Drain => scene.staged = Staged::Heights,
        Staged::Heights => finish_replace(scene, images, materials, options),
    }
}

/// Takes `handle` and removes its image now, rather than once every handle has gone (a frame
/// later): the render world then frees its texture in the frame the replacement is made. The
/// materials that also hold it are rebound in the same frame.
fn release(images: &mut Assets<Image>, handle: &mut Handle<Image>) {
    let old = std::mem::take(handle);
    images.remove(&old);
}

/// The last frame of a replace ([`begin_replace`]): the scene grid's heights and surface
/// codes, the old ones released first.
fn finish_replace(
    scene: &mut TerrainScene,
    images: &mut Assets<Image>,
    materials: &mut Assets<TerrainMaterial>,
    options: TerrainOptions,
) {
    let grid = scene.grid.clone();
    for h in [
        &mut scene.inner_height,
        &mut scene.apron_height,
        &mut scene.inner_surface,
        &mut scene.apron_surface,
    ] {
        release(images, h);
    }
    let [inner_height, apron_height, inner_surface, apron_surface] = upload_heights(&grid, images);
    if let Some(mut m) = materials.get_mut(&scene.material) {
        m.extension.inner_height = inner_height.clone();
        m.extension.apron_height = apron_height.clone();
        m.extension.inner_surface = inner_surface.clone();
        m.extension.apron_surface = apron_surface.clone();
        m.extension.look = look_for(&grid, options);
    }
    scene.inner_height = inner_height;
    scene.apron_height = apron_height;
    scene.inner_surface = inner_surface;
    scene.apron_surface = apron_surface;
    scene.params = TerrainParams::new(&grid, &scene.settings);
    scene.height_range_m = height_range_m(&grid);
    info!("terrain fields replaced");
}

/// Gathers the region camera, plans the frame's patches, then writes the buffer and updates the
/// pool: grows it if needed, shows the used slots with their bounds and hides the rest.
fn update_patches(
    mut commands: Commands,
    scene: Option<ResMut<TerrainScene>>,
    settings: Res<TerrainSettings>,
    fly: Option<Res<crate::fly_camera::FlyState>>,
    cameras: Query<&Transform, With<RegionCamera>>,
    mut buffers: ResMut<Assets<ShaderBuffer>>,
    mut patches: Query<(&TerrainPatch, &mut Visibility, &mut Aabb)>,
) {
    let Some(mut scene) = scene else {
        return;
    };
    let Ok(camera) = cameras.get(scene.camera) else {
        return;
    };
    let camera = camera.translation;
    let plan = plan_patches(
        camera,
        &scene.settings,
        scene.height_range_m,
        scene.pool.len(),
    );
    if plan.dropped > 0 && !scene.overflow_reported {
        scene.overflow_reported = true;
        warn!(
            dropped = plan.dropped,
            max = MAX_TERRAIN_PATCHES,
            "more terrain patches selected than the pool holds; the farthest are not drawn"
        );
    }

    let params = scene
        .params
        .with_camera(camera)
        .with_wireframe(settings.wireframe)
        .with_fog(&fly_view_ranges(fly.as_deref()));
    if let Some(mut buffer) = buffers.get_mut(&scene.buffer)
        && let Some(data) = buffer.cast_slice_mut::<u8>()
    {
        data.copy_from_slice(&terrain_buffer_bytes(
            &params,
            &plan.instances,
            MAX_TERRAIN_PATCHES,
        ));
    }

    let shown = |slot: usize| {
        if settings.visible && slot < plan.instances.len() {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        }
    };
    let bounds = |slot: usize| plan.bounds.get(slot).copied().unwrap_or_default();
    for (patch, mut visibility, mut aabb) in &mut patches {
        let slot = patch.slot as usize;
        visibility.set_if_neq(shown(slot));
        aabb.set_if_neq(bounds(slot));
    }
    let grown = scene.pool.len()..plan.pool_len;
    if !grown.is_empty() {
        debug!(
            from = grown.start,
            to = grown.end,
            "terrain patch pool grown"
        );
    }
    for slot in grown {
        let entity = commands
            .spawn((
                Name::new("Terrain patch"),
                TerrainPatch { slot: slot as u32 },
                Mesh3d(scene.mesh.clone()),
                MeshMaterial3d(scene.material.clone()),
                MeshTag::new(slot as u32),
                Transform::IDENTITY,
                shown(slot),
                NoAutoAabb,
                bounds(slot),
                RenderLayers::layer(TERRAIN_LAYER),
            ))
            .id();
        scene.pool.push(entity);
    }
}

#[cfg(test)]
mod tests;
