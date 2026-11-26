//! The region sky (M2 design §6.4, spec §9): a gradient dome behind the terrain and Bevy
//! `DistanceFog` for atmospheric perspective, both tinted per vegetation era and biome.
//!
//! [`sky_tint`] is the art direction in one pure table: dusty ochre skies before land plants,
//! humid hazes in the coal-swamp and greenhouse eras, clean blue today, and per-biome hazes on
//! top (desert dust, rainforest humidity, crisp polar air). The region's biome is the commonest
//! land biome of its inner field ([`region_look`]).
//!
//! - **Dome.** A unit sphere drawn at infinite depth around the camera (`sky.wesl`): the horizon
//!   colour at the geometric horizon (which dips below level as the camera climbs), the zenith
//!   overhead, and a warm glow and disc toward the region's sun. It tonemaps like the terrain,
//!   so the horizon meets the fogged far terrain. Below the horizon it is the fully fogged
//!   ground's colour (the fog's, with its sun scattering): from high up (M3) the apron ends
//!   inside the horizon, and the ground beyond its edge reads as more fogged ground.
//! - **Fog.** `DistanceFog` on the region camera: the [`sky_tint`] fog colour, an
//!   exponential-squared falloff from [`fog_visibility_m`], and the sun's colour scattered toward
//!   the sun.
//! - **Ambient.** The camera's `AmbientLight` takes the zenith colour, so shadow lit only by the
//!   sky is cool and era-tinted.
//!
//! The dome follows the [`TerrainScene`]: spawned with it, retinted when a snapshot change
//! replaces the grid, despawned when it goes.

use std::sync::Arc;

use bevy::asset::RenderAssetUsages;
use bevy::asset::uuid_handle;
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::{MeshVertexBufferLayoutRef, PrimitiveTopology};
use bevy::pbr::{
    DistanceFog, FogFalloff, Material, MaterialPipeline, MaterialPipelineKey, MaterialPlugin,
};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::shader::ShaderRef;
use timefly_core::deep_time::YearsBp;
use timefly_core::worldgen::biome::{BiomeClass, VegetationEra, vegetation_era};
use timefly_core::worldgen::generate::RegionGrid;
use tracing::instrument;

use crate::terrain::material::ALL_BIOMES;
use crate::terrain::view::{
    REGION_AMBIENT, REGION_SUN_COLOR, REGION_SUN_DIRECTION, REGION_SUN_LUX,
};
use crate::terrain::{TERRAIN_LAYER, TerrainScene, TerrainSceneSystems};

/// `sky.wesl`: the gradient dome.
pub const SKY_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e21");

type Srgb = [u8; 3];

/// `(zenith, horizon)` per era, indexed by `VegetationEra as usize`.
const ERA_SKY: [(Srgb, Srgb); 6] = [
    ([118, 126, 150], [224, 184, 138]), // Barren: dusty grey-violet over an ochre haze
    ([100, 128, 168], [216, 194, 160]), // EarlyPlants
    ([88, 128, 172], [204, 204, 186]),  // FirstForests: humid coal-swamp haze
    ([84, 128, 180], [216, 202, 172]),  // ConiferAge: hot greenhouse haze
    ([72, 126, 192], [198, 210, 216]),  // FloweringPlants
    ([62, 122, 202], [184, 208, 232]),  // Modern: clean blue
];

/// A biome's horizon haze `(colour, amount)`, indexed by `BiomeClass as usize`.
const BIOME_HAZE: [(Srgb, f32); 12] = [
    ([212, 220, 214], 0.30), // TropicalRainforest: humid white
    ([226, 210, 176], 0.25), // MonsoonSavanna
    ([232, 202, 150], 0.45), // HotDesert: dust
    ([216, 202, 172], 0.30), // ColdDesert
    ([218, 204, 168], 0.25), // Steppe
    ([226, 212, 182], 0.20), // Mediterranean
    ([200, 210, 220], 0.00), // TemperateBroadleaf
    ([210, 218, 222], 0.25), // TemperateRainforest
    ([204, 220, 238], 0.20), // Boreal
    ([206, 222, 240], 0.30), // Tundra
    ([212, 228, 246], 0.35), // IceCap
    ([206, 222, 240], 0.25), // Alpine
];

/// How far the fog's colour leans from the horizon toward the zenith: distant land reads a
/// little bluer than the sky at the horizon line.
const FOG_ZENITH_SHARE: f32 = 0.15;
/// The fog's sun glow: the sun's colour times this, concentrated by [`FOG_SUN_EXPONENT`].
const FOG_SUN_GLOW: f32 = 0.35;
const FOG_SUN_EXPONENT: f32 = 10.0;
/// The dome's sun glow (`sun.w` in `SkyParams`).
const SKY_SUN_GLOW: f32 = 0.45;

/// The sky's colours for an era and biome, linear and before exposure-independent tonemapping:
/// `(zenith, horizon, fog)`. The dome blends zenith to horizon; the fog colour is the horizon
/// leaning slightly toward the zenith.
pub fn sky_tint(era: VegetationEra, biome: BiomeClass) -> (Color, Color, Color) {
    let (zenith, horizon) = ERA_SKY[era as usize];
    let (haze, amount) = BIOME_HAZE[biome as usize];
    let zenith = linear(zenith);
    let horizon = linear(horizon).mix(&linear(haze), amount);
    let fog = horizon.mix(&zenith, FOG_ZENITH_SHARE);
    (
        Color::LinearRgba(zenith),
        Color::LinearRgba(horizon),
        Color::LinearRgba(fog),
    )
}

/// The fog's visibility (m) for an era and biome: hazier in dusty, humid and barren air.
pub fn fog_visibility_m(era: VegetationEra, biome: BiomeClass) -> f32 {
    let biome_m = match biome {
        BiomeClass::HotDesert => 220_000.0,
        BiomeClass::TropicalRainforest | BiomeClass::TemperateRainforest => 250_000.0,
        BiomeClass::ColdDesert | BiomeClass::Steppe | BiomeClass::MonsoonSavanna => 300_000.0,
        BiomeClass::Mediterranean | BiomeClass::TemperateBroadleaf => 340_000.0,
        BiomeClass::Boreal | BiomeClass::Alpine => 400_000.0,
        BiomeClass::Tundra | BiomeClass::IceCap => 460_000.0,
    };
    let era_factor = match era {
        VegetationEra::Barren => 0.75,
        VegetationEra::EarlyPlants => 0.8,
        VegetationEra::FirstForests | VegetationEra::ConiferAge => 0.85,
        VegetationEra::FloweringPlants => 0.95,
        VegetationEra::Modern => 1.0,
    };
    biome_m * era_factor
}

fn linear(c: Srgb) -> LinearRgba {
    Color::srgb_u8(c[0], c[1], c[2]).to_linear()
}

/// The region's vegetation era (from its snapshot) and its look biome: the commonest biome over
/// the inner field's land (every fourth texel each way), or the centre texel's where the field
/// has no land.
pub fn region_look(grid: &RegionGrid) -> (VegetationEra, BiomeClass) {
    let era = vegetation_era(YearsBp(grid.spec.snapshot.0));
    let field = &grid.inner;
    let res = field.res as usize;
    let mut counts = [0u32; 12];
    for row in (0..res).step_by(4) {
        for col in (0..res).step_by(4) {
            let i = row * res + col;
            if field.elevation_m[i] > 0.0 {
                counts[usize::from(field.surface[i][0]).min(11)] += 1;
            }
        }
    }
    let commonest = (0..12).max_by_key(|&b| (counts[b], std::cmp::Reverse(b)));
    let biome = match commonest {
        Some(b) if counts[b] > 0 => b,
        _ => usize::from(field.surface[(res / 2) * res + res / 2][0]).min(11),
    };
    (era, ALL_BIOMES[biome])
}

/// The region centre's latitude (degrees) in the era frame (`CoarseField::paleolat`).
pub fn centre_lat_deg(grid: &RegionGrid) -> f32 {
    grid.spec
        .center
        .0
        .normalize()
        .y
        .clamp(-1.0, 1.0)
        .asin()
        .to_degrees() as f32
}

/// The dome's uniform (`SkyParams` in `sky.wesl`).
#[derive(ShaderType, Copy, Clone, Debug, PartialEq, Reflect)]
pub struct SkyParams {
    /// Linear colour overhead.
    pub zenith: Vec4,
    /// Linear colour at the horizon.
    pub horizon: Vec4,
    /// `xyz`: direction toward the sun; `w`: the glow's strength.
    pub sun: Vec4,
    /// The sun's linear colour.
    pub sun_color: Vec4,
    /// Below the horizon: `rgb` the fog's linear colour, `w` its sun glow ([`FOG_SUN_GLOW`]).
    pub fog: Vec4,
    /// Below the horizon: `rgb` the sun's linear colour times its illuminance (as Bevy's light
    /// uniform carries it), `w` the fog's sun exponent ([`FOG_SUN_EXPONENT`]).
    pub fog_sun: Vec4,
}

/// The dome's uniform for an era and biome.
pub fn sky_params(era: VegetationEra, biome: BiomeClass) -> SkyParams {
    let (zenith, horizon, fog) = sky_tint(era, biome);
    let v = |c: Color| Vec4::from_array(c.to_linear().to_f32_array());
    SkyParams {
        zenith: v(zenith),
        horizon: v(horizon),
        sun: REGION_SUN_DIRECTION.normalize().extend(SKY_SUN_GLOW),
        sun_color: v(REGION_SUN_COLOR),
        fog: v(fog).truncate().extend(FOG_SUN_GLOW),
        fog_sun: (v(REGION_SUN_COLOR).truncate() * REGION_SUN_LUX).extend(FOG_SUN_EXPONENT),
    }
}

/// The region camera's fog for an era and biome.
pub fn distance_fog(era: VegetationEra, biome: BiomeClass) -> DistanceFog {
    let (_, _, fog) = sky_tint(era, biome);
    DistanceFog {
        color: fog,
        directional_light_color: REGION_SUN_COLOR.with_alpha(FOG_SUN_GLOW),
        directional_light_exponent: FOG_SUN_EXPONENT,
        // Exponential-squared: the near ground stays crisp, the far apron melts into the sky.
        falloff: FogFalloff::from_visibility_squared(fog_visibility_m(era, biome)),
    }
}

/// The gradient sky dome's material.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct SkyMaterial {
    #[uniform(0)]
    pub params: SkyParams,
}

impl Material for SkyMaterial {
    fn vertex_shader() -> ShaderRef {
        SKY_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        SKY_SHADER.into()
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
        // Seen from inside, at infinite depth: never culled, never writes depth.
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

/// The dome's mesh: a position-only unit sphere (the shader puts it at infinity).
pub fn sky_mesh() -> Mesh {
    let sphere = Sphere::new(1.0).mesh().uv(48, 24);
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    );
    if let Some(positions) = sphere.attribute(Mesh::ATTRIBUTE_POSITION) {
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions.clone());
    }
    if let Some(indices) = sphere.indices() {
        mesh.insert_indices(indices.clone());
    }
    mesh
}

/// The sky dome for the current region (present while its entity exists).
#[derive(Resource, Debug)]
pub struct RegionSky {
    /// The grid it was tinted for.
    pub grid: Arc<RegionGrid>,
    pub dome: Entity,
    pub material: Handle<SkyMaterial>,
    pub era: VegetationEra,
    pub biome: BiomeClass,
}

pub struct SkyPlugin;

impl Plugin for SkyPlugin {
    fn build(&self, app: &mut App) {
        let sky = Shader::from_wesl(
            include_str!("../assets/shaders/sky.wesl"),
            "embedded://timefly/sky.wesl",
        );
        let _ = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(SKY_SHADER.id(), sky);
        app.add_plugins(MaterialPlugin::<SkyMaterial>::default());
        add_sky_systems(app);
    }
}

/// The systems without the render plugin and shader, for headless tests.
pub(crate) fn add_sky_systems(app: &mut App) {
    app.add_systems(Update, sync_sky.after(TerrainSceneSystems));
}

/// What the sky must do for the current terrain scene.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SkyAction {
    Keep,
    Build,
    Retint,
    Teardown,
}

/// The sky's action from the scene's grid and the grid the sky was tinted for.
pub fn sky_action(scene: Option<&Arc<RegionGrid>>, sky: Option<&Arc<RegionGrid>>) -> SkyAction {
    match (scene, sky) {
        (None, None) => SkyAction::Keep,
        (Some(_), None) => SkyAction::Build,
        (None, Some(_)) => SkyAction::Teardown,
        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => SkyAction::Keep,
        (Some(_), Some(_)) => SkyAction::Retint,
    }
}

/// Gathers the scene and the sky, decides with [`sky_action`], and builds, retints or removes
/// the dome, the fog and the ambient colour.
fn sync_sky(
    mut commands: Commands,
    scene: Option<Res<TerrainScene>>,
    sky: Option<ResMut<RegionSky>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<SkyMaterial>>,
) {
    let action = sky_action(
        scene.as_ref().map(|s| &s.grid),
        sky.as_ref().map(|s| &s.grid),
    );
    match (action, scene, sky) {
        (SkyAction::Build, Some(scene), _) => {
            let (era, biome) = region_look(&scene.grid);
            let material = materials.add(SkyMaterial {
                params: sky_params(era, biome),
            });
            let dome = commands
                .spawn((
                    Name::new("Region sky"),
                    Mesh3d(meshes.add(sky_mesh())),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    NoFrustumCulling,
                    RenderLayers::layer(TERRAIN_LAYER),
                ))
                .id();
            tint_camera(&mut commands, scene.camera, era, biome);
            info!(?era, ?biome, "region sky built");
            commands.insert_resource(RegionSky {
                grid: scene.grid.clone(),
                dome,
                material,
                era,
                biome,
            });
        }
        (SkyAction::Retint, Some(scene), Some(mut sky)) => {
            let (era, biome) = region_look(&scene.grid);
            if let Some(mut m) = materials.get_mut(&sky.material) {
                m.params = sky_params(era, biome);
            }
            tint_camera(&mut commands, scene.camera, era, biome);
            info!(?era, ?biome, "region sky retinted");
            sky.grid = scene.grid.clone();
            sky.era = era;
            sky.biome = biome;
        }
        (SkyAction::Teardown, _, Some(sky)) => {
            commands.entity(sky.dome).despawn();
            commands.remove_resource::<RegionSky>();
            info!("region sky removed");
        }
        _ => {}
    }
}

/// The fog and the sky-coloured ambient light on the region camera.
#[instrument(level = "debug", skip_all, fields(?camera, ?era, ?biome))]
fn tint_camera(commands: &mut Commands, camera: Entity, era: VegetationEra, biome: BiomeClass) {
    let (zenith, _, _) = sky_tint(era, biome);
    if let Ok(mut camera) = commands.get_entity(camera) {
        camera.insert((
            distance_fog(era, biome),
            AmbientLight {
                color: zenith,
                brightness: REGION_AMBIENT,
                ..default()
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::material::{ALL_BIOMES, ALL_ERAS};

    fn hsl(c: Color) -> Hsla {
        Hsla::from(c)
    }

    #[test]
    fn sky_tint_is_era_aware() {
        for biome in ALL_BIOMES {
            let (barren_zenith, barren_horizon, barren_fog) =
                sky_tint(VegetationEra::Barren, biome);
            let (modern_zenith, modern_horizon, _) = sky_tint(VegetationEra::Modern, biome);
            // Warmer: a Barren horizon is orange-ochre everywhere, and redder than today's
            // (a modern desert keeps a dusty horizon, but a less red one).
            let warm = hsl(barren_horizon).hue;
            assert!(
                (15.0..=60.0).contains(&warm),
                "{biome:?}: Barren horizon hue {warm}"
            );
            let red_over_blue = |c: Color| c.to_linear().red / c.to_linear().blue;
            assert!(
                red_over_blue(barren_horizon) > red_over_blue(modern_horizon),
                "{biome:?}"
            );
            // Dustier: a greyer zenith.
            assert!(
                hsl(barren_zenith).saturation < hsl(modern_zenith).saturation,
                "{biome:?}"
            );
            // The fog sits between the horizon and the zenith.
            let (f, h, z) = (
                barren_fog.to_linear(),
                barren_horizon.to_linear(),
                barren_zenith.to_linear(),
            );
            assert!(f.red <= h.red.max(z.red) && f.red >= h.red.min(z.red));
            assert!(f.blue <= h.blue.max(z.blue) && f.blue >= h.blue.min(z.blue));
        }
        // The pinned ordering on the horizon hue: Barren ochre, Modern blue (temperate air).
        let hue = |era| hsl(sky_tint(era, BiomeClass::TemperateBroadleaf).1).hue;
        assert!((15.0..=60.0).contains(&hue(VegetationEra::Barren)));
        assert!((180.0..=240.0).contains(&hue(VegetationEra::Modern)));
        // Every era's zenith is darker than its horizon, and bluer.
        for era in ALL_ERAS {
            for biome in ALL_BIOMES {
                let (z, h, _) = sky_tint(era, biome);
                assert!(z.luminance() < h.luminance(), "{era:?} {biome:?}");
                let z = z.to_linear();
                assert!(z.blue > z.red, "{era:?} {biome:?}");
            }
        }
    }

    #[test]
    fn region_look_takes_the_commonest_land_biome() {
        use timefly_core::geo::{geo_to_render, latlon_to_geo};
        use timefly_core::worldgen::generate::{Field, RegionSpec};
        let res = 9u32;
        let n = (res * res) as usize;
        // Sea on the west third (tundra codes there must not count), desert on most of the
        // land, a strip of steppe.
        let mut elevation_m = vec![300.0f32; n];
        let mut surface = vec![[BiomeClass::HotDesert as u8, 5, 0, 0]; n];
        for row in 0..res as usize {
            for col in 0..res as usize {
                let i = row * res as usize + col;
                if col < 4 {
                    elevation_m[i] = -50.0;
                    surface[i][0] = BiomeClass::Tundra as u8;
                } else if col == 8 {
                    surface[i][0] = BiomeClass::Steppe as u8;
                }
            }
        }
        let field = Field {
            res,
            size_m: 300_000.0,
            elevation_m,
            surface,
        };
        let centre = geo_to_render(latlon_to_geo(23.0, 10.0));
        let grid = RegionGrid {
            spec: RegionSpec {
                inner_res: res,
                apron_res: res,
                ..RegionSpec::new(centre, YearsBp(0.0), 1)
            },
            inner: field.clone(),
            apron: field.clone(),
        };
        assert_eq!(
            region_look(&grid),
            (VegetationEra::Modern, BiomeClass::HotDesert)
        );
        assert!((centre_lat_deg(&grid) - 23.0).abs() < 1e-4);
        // No land at all: the centre texel's biome. The era follows the snapshot.
        let mut sea = grid.clone();
        sea.inner.elevation_m.iter_mut().for_each(|h| *h = -100.0);
        sea.spec.snapshot = YearsBp(540.0e6);
        assert_eq!(
            region_look(&sea),
            (VegetationEra::Barren, BiomeClass::HotDesert)
        );
    }

    #[test]
    fn sky_actions_follow_the_scene() {
        use timefly_core::worldgen::generate::{Field, RegionSpec};
        let field = Field {
            res: 2,
            size_m: 1.0,
            elevation_m: vec![0.0; 4],
            surface: vec![[0; 4]; 4],
        };
        let grid = |seed| {
            Arc::new(RegionGrid {
                spec: RegionSpec::new(
                    timefly_core::geo::RenderDir(bevy::math::DVec3::Y),
                    YearsBp(0.0),
                    seed,
                ),
                inner: field.clone(),
                apron: field.clone(),
            })
        };
        let (a, b) = (grid(1), grid(1));
        assert_eq!(sky_action(None, None), SkyAction::Keep);
        assert_eq!(sky_action(Some(&a), None), SkyAction::Build);
        assert_eq!(sky_action(Some(&a), Some(&a)), SkyAction::Keep);
        // A replaced grid is retinted even when equal in value.
        assert_eq!(sky_action(Some(&b), Some(&a)), SkyAction::Retint);
        assert_eq!(sky_action(None, Some(&a)), SkyAction::Teardown);
    }

    #[test]
    fn fog_is_hazier_in_dust_than_in_polar_air() {
        for era in ALL_ERAS {
            for biome in ALL_BIOMES {
                let v = fog_visibility_m(era, biome);
                assert!(
                    (100_000.0..=600_000.0).contains(&v),
                    "{era:?} {biome:?}: {v}"
                );
            }
            assert!(
                fog_visibility_m(era, BiomeClass::HotDesert)
                    < fog_visibility_m(era, BiomeClass::Tundra)
            );
        }
        assert!(
            fog_visibility_m(VegetationEra::Barren, BiomeClass::TemperateBroadleaf)
                < fog_visibility_m(VegetationEra::Modern, BiomeClass::TemperateBroadleaf)
        );
    }
}
