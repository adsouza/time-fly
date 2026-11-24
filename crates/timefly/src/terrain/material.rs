//! The terrain material and the GPU data it binds (design spec §4.2, §6.3).
//!
//! Every patch entity shares one grid mesh and one [`TerrainMaterial`]: an
//! `ExtendedMaterial<StandardMaterial, TerrainExtension>`, so the terrain goes through Bevy's
//! PBR lighting, shadows and fog. The extension swaps in the CDLOD vertex shaders
//! (`terrain.wesl` for the main pass, `terrain_prepass.wesl` for the depth, normal and shadow
//! passes; both call `terrain_cdlod.wesl`, so every pass displaces identically) and binds:
//!
//! | binding | resource |
//! |---|---|
//! | 100 | read-only storage buffer: [`TerrainParams`] then [`PatchInstance`]s, indexed by `MeshTag` |
//! | 101, 102 | inner and apron heights, `R32Float`, read with `textureLoad` |
//! | 103, 104 | inner and apron surface codes, `Rgba8Unorm` (`Field::surface`), `textureLoad` |
//! | 105 | the palette, `Rgba8UnormSrgb` ([`palette_texels`]: ground, canopy, rock per era and biome) |
//! | 106 | uniform [`TerrainLook`]: the painterly light (shadow hue, ramp, brush, snowline latitude) |
//!
//! None of the textures has a sampler: `R32Float` is not filterable in baseline WebGPU, and the
//! codes and palette are looked up texel by texel.
//!
//! The fragment (M2 design §6.4) blends the palette colours of the four surrounding surface
//! texels in colour space, breaks them up with `noise.wesl` at 400 m, 60 m and 9 m, adds rock on
//! slopes, snow from the ice byte and the snowline, and Barren-era microbial mats at the shore,
//! then lights through Bevy PBR and repaints the light: a soft three-band ramp and a cool,
//! era-tinted hue shift in shadow.
//!
//! The extension is not bindless, so the combined material is not either (Bevy only goes
//! bindless when the base and the extension both are). That one layout works on Metal, Vulkan
//! and WebGPU, which has no bindless support at all.

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bytemuck::{Pod, Zeroable};
use timefly_core::globe::EARTH_RADIUS_KM;
use timefly_core::terrain::{BLEND_APRON_TEXELS, LodSettings, Patch};
use timefly_core::worldgen::biome::{
    BiomeClass, VegetationEra, palette, rock_exposure, sand_sea_weight,
};
use timefly_core::worldgen::generate::{Field, RegionGrid};

/// `terrain.wesl`: the main-pass vertex and fragment shaders.
pub const TERRAIN_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e11");
/// `terrain_prepass.wesl`: the prepass and shadow vertex shader.
pub const TERRAIN_PREPASS_SHADER: Handle<Shader> =
    uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e12");
/// `terrain_cdlod.wesl`: the shared CDLOD vertex math (imported by the two above).
pub const TERRAIN_CDLOD_SHADER: Handle<Shader> =
    uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e13");
/// `noise.wesl`: the shared lattice noise (Task 2), imported as `package::noise`.
pub const NOISE_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e14");
/// `snowline.wesl`: the snowline (`snowline_m`), imported as `package::snowline`.
pub const SNOWLINE_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e15");

/// How strongly shadowed light takes the sky's cool, era-tinted hue (0 = not at all).
pub const SHADOW_HUE_STRENGTH: f32 = 0.30;
/// How far the diffuse light is pulled toward its three soft bands (0 = plain PBR).
pub const RAMP_AMOUNT: f32 = 0.4;
/// Brush-noise strength: the value jitter at the coarsest scale (finer scales are weaker).
pub const BRUSH_STRENGTH: f32 = 0.08;
/// How much steeper the terrain is lit than it is: the 293 m field's slopes read as soft
/// rolling ground at flight altitudes, and a painter models the forms more boldly. Shading
/// only; the geometry and the slope rules (rock, snow) use the true slope.
pub const RELIEF_EXAGGERATION: f32 = 1.8;

/// The painterly look's uniform (`TerrainLook` in `terrain.wesl`).
#[derive(ShaderType, Copy, Clone, Debug, PartialEq, Reflect)]
pub struct TerrainLook {
    /// `rgb`: the hue shadowed light shifts toward, scaled to luminance 1 (the sky's zenith, so
    /// shadows lit only by the sky take its colour); `a`: the shift's strength.
    pub shadow: Vec4,
    /// `x`: the illumination (exposure applied) of a surface facing the sun, which the ramp's
    /// bands are measured against; `y`: [`RAMP_AMOUNT`]; `z`: [`BRUSH_STRENGTH`]; `w`: the
    /// region centre's latitude in the era frame (degrees), for the snowline.
    pub light: Vec4,
    /// `x`: [`RELIEF_EXAGGERATION`]; `y`: the era's sea-level anomaly (m), which lowers the
    /// snowline in glacial eras (ruling R18, `snowline.wesl`); `zw`: reserved.
    pub relief: Vec4,
}

impl Default for TerrainLook {
    fn default() -> Self {
        terrain_look(Vec3::ONE, 1.0, 0.0, 0.0)
    }
}

/// The look for a sky zenith colour (linear), the full-sun illumination, the centre latitude and
/// the era's sea-level anomaly.
pub fn terrain_look(
    zenith: Vec3,
    full_sun: f32,
    centre_lat_deg: f32,
    sea_level_anomaly_m: f32,
) -> TerrainLook {
    let luminance = zenith.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(1e-6);
    TerrainLook {
        shadow: (zenith / luminance).extend(SHADOW_HUE_STRENGTH),
        light: Vec4::new(full_sun, RAMP_AMOUNT, BRUSH_STRENGTH, centre_lat_deg),
        relief: Vec4::new(RELIEF_EXAGGERATION, sea_level_anomaly_m, 0.0, 0.0),
    }
}

/// The terrain's material.
pub type TerrainMaterial = ExtendedMaterial<StandardMaterial, TerrainExtension>;

/// Bytes of [`TerrainParams`]: the patch array starts here.
pub const TERRAIN_PARAMS_BYTES: usize = 96;
/// Bytes of one [`PatchInstance`] (the array stride).
pub const PATCH_INSTANCE_BYTES: usize = 32;

/// One selected patch as the shader reads it (`PatchInstance` in `terrain_cdlod.wesl`).
/// Two 16-byte vectors, so the layout is the same under every WGSL alignment rule.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct PatchInstance {
    /// `[origin x cells, origin z cells, lod, 2^lod]`: the origin in finest-grid cells from the
    /// root's minimum corner ([`Patch::origin_cells`]).
    pub cells: [u32; 4],
    /// `[morph start m, morph end m, patch size m, reserved]`.
    pub morph: [f32; 4],
}

/// The storage buffer's header (`TerrainParams` in `terrain_cdlod.wesl`), written every frame.
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct TerrainParams {
    /// `[x, y, z, sea level]`: the main camera in local metres. Every pass morphs and drops
    /// vertices from this camera, never from its own view, so shadows match the main pass.
    pub camera: [f32; 4],
    /// `[finest quad m, root corner m, Earth radius m, quads per patch side]`.
    pub lattice: [f32; 4],
    /// `[size m, spacing m, resolution, blend start e0 m]`.
    pub inner: [f32; 4],
    /// `[size m, spacing m, resolution, blend end e1 m]`.
    pub apron: [f32; 4],
    /// Reserved for M4's voxel-window clip (the draft's clipping is removed for now): zero.
    pub voxel_window: [f32; 4],
    /// `[wireframe (0 or 1), reserved ×3]`.
    pub debug: [f32; 4],
}

impl TerrainParams {
    /// The header for a region grid and LOD settings, with the camera at the origin.
    pub fn new(grid: &RegionGrid, settings: &LodSettings) -> TerrainParams {
        let field = |f: &Field, edge: f64| {
            [
                f.size_m as f32,
                f.spacing_m() as f32,
                f.res as f32,
                edge as f32,
            ]
        };
        // The blend edges of `blended_height_m`.
        let e1 = grid.inner.size_m / 2.0;
        let e0 = e1 - BLEND_APRON_TEXELS * grid.apron.spacing_m();
        TerrainParams {
            camera: [0.0; 4],
            lattice: [
                settings.finest_quad_m() as f32,
                (-settings.extent_m() / 2.0) as f32,
                (EARTH_RADIUS_KM * 1000.0) as f32,
                settings.patch_quads as f32,
            ],
            inner: field(&grid.inner, e0),
            apron: field(&grid.apron, e1),
            voxel_window: [0.0; 4],
            debug: [0.0; 4],
        }
    }

    /// The same header with the main camera at `camera` (local metres).
    pub fn with_camera(self, camera: Vec3) -> TerrainParams {
        TerrainParams {
            camera: [camera.x, camera.y, camera.z, 0.0],
            ..self
        }
    }

    /// The same header with the wireframe view on or off.
    pub fn with_wireframe(self, on: bool) -> TerrainParams {
        TerrainParams {
            debug: [if on { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0],
            ..self
        }
    }
}

/// The selected patches as the shader reads them, in order (slot `i` is `MeshTag(i)`).
pub fn patch_instances(patches: &[Patch], settings: &LodSettings) -> Vec<PatchInstance> {
    patches
        .iter()
        .map(|p| {
            let [x, z] = p.origin_cells(settings);
            PatchInstance {
                cells: [x, z, p.lod, 1 << p.lod],
                morph: [p.morph_range_m[0], p.morph_range_m[1], p.size_m as f32, 0.0],
            }
        })
        .collect()
}

/// The whole storage buffer: the header, then `capacity` patch slots (the instances first, the
/// rest zero). Its size depends only on `capacity`, so rewriting it every frame reuses the GPU
/// buffer. Instances beyond `capacity` are dropped.
pub fn terrain_buffer_bytes(
    params: &TerrainParams,
    instances: &[PatchInstance],
    capacity: usize,
) -> Vec<u8> {
    let mut bytes = vec![0u8; TERRAIN_PARAMS_BYTES + capacity * PATCH_INSTANCE_BYTES];
    bytes[..TERRAIN_PARAMS_BYTES].copy_from_slice(bytemuck::bytes_of(params));
    let used = instances.len().min(capacity);
    let slots: &[u8] = bytemuck::cast_slice(&instances[..used]);
    bytes[TERRAIN_PARAMS_BYTES..TERRAIN_PARAMS_BYTES + slots.len()].copy_from_slice(slots);
    bytes
}

/// A field's heights as an `R32Float` texture: texel `(col, row)` is field column `col`, row
/// `row` (row 0 north). Render-world only: the CPU copy stays in the region grid.
pub fn height_image(field: &Field) -> Image {
    let bytes: Vec<u8> = bytemuck::cast_slice(&field.elevation_m).to_vec();
    Image::new(
        field_extent(field),
        TextureDimension::D2,
        bytes,
        TextureFormat::R32Float,
        RenderAssetUsages::RENDER_WORLD,
    )
}

/// A field's surface codes (`[biome, era, ice × 255, shore]`) as an `Rgba8Unorm` texture,
/// laid out like [`height_image`].
pub fn surface_image(field: &Field) -> Image {
    let bytes: Vec<u8> = field.surface.iter().flatten().copied().collect();
    Image::new(
        field_extent(field),
        TextureDimension::D2,
        bytes,
        TextureFormat::Rgba8Unorm,
        RenderAssetUsages::RENDER_WORLD,
    )
}

/// Every biome class, in discriminant order (`ALL_BIOMES[i] as usize == i`).
pub const ALL_BIOMES: [BiomeClass; 12] = [
    BiomeClass::TropicalRainforest,
    BiomeClass::MonsoonSavanna,
    BiomeClass::HotDesert,
    BiomeClass::ColdDesert,
    BiomeClass::Steppe,
    BiomeClass::Mediterranean,
    BiomeClass::TemperateBroadleaf,
    BiomeClass::TemperateRainforest,
    BiomeClass::Boreal,
    BiomeClass::Tundra,
    BiomeClass::IceCap,
    BiomeClass::Alpine,
];

/// Every vegetation era, in discriminant order (`ALL_ERAS[i] as usize == i`).
pub const ALL_ERAS: [VegetationEra; 6] = [
    VegetationEra::Barren,
    VegetationEra::EarlyPlants,
    VegetationEra::FirstForests,
    VegetationEra::ConiferAge,
    VegetationEra::FloweringPlants,
    VegetationEra::Modern,
];

/// The colours a palette entry holds per `(era, biome)`, in column order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaletteKind {
    Ground = 0,
    Canopy = 1,
    Rock = 2,
}

/// Palette texture size: three columns (ground, canopy, rock) per biome class, one row per
/// vegetation era. Texel `(biome × 3 + kind, era)`.
pub const PALETTE_SIZE: UVec2 = UVec2::new(36, 6);

/// Index into [`palette_texels`] of a colour (row-major, row = era).
pub fn palette_texel_index(era: VegetationEra, biome: BiomeClass, kind: PaletteKind) -> usize {
    era as usize * PALETTE_SIZE.x as usize + biome as usize * 3 + kind as usize
}

/// The share of the ground a biome's canopy covers where plants grow, which the shader breaks
/// into brushy patches. Where nothing grows the canopy colour equals the ground's, so the cover
/// does not matter there.
pub fn canopy_cover(biome: BiomeClass) -> f32 {
    match biome {
        BiomeClass::TropicalRainforest => 0.95,
        BiomeClass::MonsoonSavanna => 0.45,
        BiomeClass::HotDesert => 0.06,
        BiomeClass::ColdDesert => 0.12,
        BiomeClass::Steppe => 0.40,
        BiomeClass::Mediterranean => 0.50,
        BiomeClass::TemperateBroadleaf => 0.85,
        BiomeClass::TemperateRainforest => 0.95,
        BiomeClass::Boreal => 0.85,
        BiomeClass::Tundra => 0.50,
        BiomeClass::IceCap => 0.0,
        BiomeClass::Alpine => 0.35,
    }
}

/// The palette texture's texels ([`PALETTE_SIZE`], row-major, sRGB bytes): Task 3's
/// [`palette`] for every era and biome, ground, canopy and rock side by side. Alpha (linear) is
/// the ground's [`sand_sea_weight`], the canopy's [`canopy_cover`] and the rock's
/// [`rock_exposure`].
pub fn palette_texels() -> Vec<[u8; 4]> {
    let mut texels = vec![[0u8; 4]; (PALETTE_SIZE.x * PALETTE_SIZE.y) as usize];
    for era in ALL_ERAS {
        for biome in ALL_BIOMES {
            let p = palette(era, biome);
            let cover = (canopy_cover(biome) * 255.0).round() as u8;
            let sand = (sand_sea_weight(biome) * 255.0).round() as u8;
            let exposure = (rock_exposure(era, biome) * 255.0).round() as u8;
            for (kind, colour, alpha) in [
                (PaletteKind::Ground, p.ground, sand),
                (PaletteKind::Canopy, p.canopy, cover),
                (PaletteKind::Rock, p.rock, exposure),
            ] {
                let [r, g, b, _] =
                    Srgba::from(LinearRgba::rgb(colour[0], colour[1], colour[2])).to_u8_array();
                texels[palette_texel_index(era, biome, kind)] = [r, g, b, alpha];
            }
        }
    }
    texels
}

/// The palette texture (`Rgba8UnormSrgb`, so `textureLoad` returns linear colour).
pub fn palette_image() -> Image {
    let bytes: Vec<u8> = palette_texels().into_iter().flatten().collect();
    Image::new(
        Extent3d {
            width: PALETTE_SIZE.x,
            height: PALETTE_SIZE.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bytes,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

fn field_extent(field: &Field) -> Extent3d {
    Extent3d {
        width: field.res,
        height: field.res,
        depth_or_array_layers: 1,
    }
}

/// The CDLOD extension of the terrain's `StandardMaterial` (bindings in the module docs).
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct TerrainExtension {
    #[storage(100, read_only)]
    pub patches: Handle<ShaderBuffer>,
    #[texture(101, sample_type = "float", filterable = false)]
    pub inner_height: Handle<Image>,
    #[texture(102, sample_type = "float", filterable = false)]
    pub apron_height: Handle<Image>,
    #[texture(103, sample_type = "float", filterable = false)]
    pub inner_surface: Handle<Image>,
    #[texture(104, sample_type = "float", filterable = false)]
    pub apron_surface: Handle<Image>,
    #[texture(105, sample_type = "float", filterable = false)]
    pub palette: Handle<Image>,
    #[uniform(106)]
    pub look: TerrainLook,
}

impl MaterialExtension for TerrainExtension {
    fn vertex_shader() -> ShaderRef {
        TERRAIN_SHADER.into()
    }

    fn fragment_shader() -> ShaderRef {
        TERRAIN_SHADER.into()
    }

    fn prepass_vertex_shader() -> ShaderRef {
        TERRAIN_PREPASS_SHADER.into()
    }

    fn deferred_vertex_shader() -> ShaderRef {
        TERRAIN_PREPASS_SHADER.into()
    }
}

/// Adds the embedded terrain shaders to the shader assets, so the terrain renders from any
/// asset root. `terrain_cdlod.wesl` registers as the WESL module `package::terrain_cdlod`,
/// `noise.wesl` as `package::noise` (`import package::noise::gradient_noise3;`) and
/// `snowline.wesl` as `package::snowline`.
pub fn add_terrain_shaders(shaders: &mut Assets<Shader>) {
    let noise = Shader::from_wesl(
        include_str!("../../assets/shaders/noise.wesl"),
        "embedded://timefly/noise.wesl",
    );
    let snowline = Shader::from_wesl(
        include_str!("../../assets/shaders/snowline.wesl"),
        "embedded://timefly/snowline.wesl",
    );
    let cdlod = Shader::from_wesl(
        include_str!("../../assets/shaders/terrain_cdlod.wesl"),
        "embedded://timefly/terrain_cdlod.wesl",
    );
    let terrain = Shader::from_wesl(
        include_str!("../../assets/shaders/terrain.wesl"),
        "embedded://timefly/terrain.wesl",
    );
    let prepass = Shader::from_wesl(
        include_str!("../../assets/shaders/terrain_prepass.wesl"),
        "embedded://timefly/terrain_prepass.wesl",
    );
    let _ = shaders.insert(NOISE_SHADER.id(), noise);
    let _ = shaders.insert(SNOWLINE_SHADER.id(), snowline);
    let _ = shaders.insert(TERRAIN_CDLOD_SHADER.id(), cdlod);
    let _ = shaders.insert(TERRAIN_SHADER.id(), terrain);
    let _ = shaders.insert(TERRAIN_PREPASS_SHADER.id(), prepass);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::DVec3;
    use core::mem::{align_of, offset_of, size_of};
    use timefly_core::terrain::select_patches;
    use timefly_core::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};

    const CDLOD_WESL: &str = include_str!("../../assets/shaders/terrain_cdlod.wesl");

    /// `name: type` fields of a struct declared in `terrain_cdlod.wesl`, in order.
    fn wesl_fields(name: &str) -> Vec<(String, String)> {
        let start = CDLOD_WESL
            .find(&format!("struct {name} {{"))
            .unwrap_or_else(|| panic!("struct {name} in terrain_cdlod.wesl"));
        let body = &CDLOD_WESL[start..];
        let body = &body[body.find('{').unwrap() + 1..body.find('}').unwrap()];
        body.lines()
            .map(|l| l.split("//").next().unwrap().trim())
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (n, t) = l.trim_end_matches(',').split_once(':').unwrap();
                (n.trim().to_string(), t.trim().to_string())
            })
            .collect()
    }

    fn field(res: u32, size_m: f64) -> Field {
        let n = (res * res) as usize;
        Field {
            res,
            size_m,
            elevation_m: (0..n).map(|i| i as f32 * 0.5 - 100.0).collect(),
            surface: (0..n).map(|i| [i as u8, 1, 2, 3]).collect(),
        }
    }

    fn grid(inner_res: u32, apron_res: u32) -> RegionGrid {
        use timefly_core::deep_time::YearsBp;
        use timefly_core::geo::{geo_to_render, latlon_to_geo};
        use timefly_core::worldgen::generate::RegionSpec;
        let center = geo_to_render(latlon_to_geo(46.5, 10.0));
        RegionGrid {
            spec: RegionSpec {
                inner_res,
                apron_res,
                ..RegionSpec::new(center, YearsBp(0.0), 1)
            },
            inner: field(inner_res, REGION_SIZE_M),
            apron: field(apron_res, APRON_SIZE_M),
        }
    }

    #[test]
    fn patch_instances_pack_like_the_shader() {
        // The Rust layout: 16-byte vectors, header then a 32-byte stride.
        assert_eq!(size_of::<PatchInstance>(), PATCH_INSTANCE_BYTES);
        assert_eq!(offset_of!(PatchInstance, cells), 0);
        assert_eq!(offset_of!(PatchInstance, morph), 16);
        assert_eq!(size_of::<TerrainParams>(), TERRAIN_PARAMS_BYTES);
        assert!(align_of::<TerrainParams>() <= 16 && TERRAIN_PARAMS_BYTES.is_multiple_of(16));
        let header = [
            (offset_of!(TerrainParams, camera), "camera"),
            (offset_of!(TerrainParams, lattice), "lattice"),
            (offset_of!(TerrainParams, inner), "inner"),
            (offset_of!(TerrainParams, apron), "apron"),
            (offset_of!(TerrainParams, voxel_window), "voxel_window"),
            (offset_of!(TerrainParams, debug), "debug"),
        ];
        // The shader declares the same fields, in the same order, as 16-byte vectors.
        let wesl = wesl_fields("TerrainParams");
        assert_eq!(wesl.len(), header.len());
        for (i, ((offset, name), (wname, wtype))) in header.iter().zip(&wesl).enumerate() {
            assert_eq!((*offset, *name), (16 * i, wname.as_str()));
            assert_eq!(wtype, "vec4<f32>", "{wname}");
        }
        assert_eq!(
            wesl_fields("PatchInstance"),
            [
                ("cells".to_string(), "vec4<u32>".to_string()),
                ("morph".to_string(), "vec4<f32>".to_string()),
            ]
        );
        assert_eq!(
            wesl_fields("TerrainBuffer"),
            [
                ("params".to_string(), "TerrainParams".to_string()),
                ("patches".to_string(), "array<PatchInstance>".to_string()),
            ]
        );

        // The values: cells, LOD and 2^LOD; the morph range and size.
        let settings = LodSettings::region();
        let patches = select_patches(DVec3::new(-73_210.0, 1_500.0, 41_007.0), &settings);
        let instances = patch_instances(&patches, &settings);
        assert_eq!(instances.len(), patches.len());
        for (p, inst) in patches.iter().zip(&instances) {
            let [x, z] = p.origin_cells(&settings);
            assert_eq!(inst.cells, [x, z, p.lod, 1 << p.lod]);
            assert_eq!(
                inst.morph,
                [p.morph_range_m[0], p.morph_range_m[1], p.size_m as f32, 0.0]
            );
        }
        assert!(instances.iter().any(|i| i.cells[2] == 0));
        assert!(instances.iter().any(|i| i.cells[2] >= 5));

        // The bytes: header, the instances in slot order, zeroed spare slots, fixed size.
        let params = TerrainParams::new(&grid(9, 5), &settings).with_camera(Vec3::ONE);
        let capacity = instances.len() + 3;
        let bytes = terrain_buffer_bytes(&params, &instances, capacity);
        assert_eq!(
            bytes.len(),
            TERRAIN_PARAMS_BYTES + capacity * PATCH_INSTANCE_BYTES
        );
        assert_eq!(&bytes[..TERRAIN_PARAMS_BYTES], bytemuck::bytes_of(&params));
        let slots = &bytes[TERRAIN_PARAMS_BYTES..];
        for (i, inst) in instances.iter().enumerate() {
            let at = i * PATCH_INSTANCE_BYTES;
            assert_eq!(
                &slots[at..at + PATCH_INSTANCE_BYTES],
                bytemuck::bytes_of(inst)
            );
            let u = |o: usize| u32::from_le_bytes(slots[at + o..at + o + 4].try_into().unwrap());
            assert_eq!([u(0), u(4), u(8), u(12)], inst.cells);
            assert_eq!(f32::from_bits(u(16)), inst.morph[0]);
        }
        assert!(
            slots[instances.len() * PATCH_INSTANCE_BYTES..]
                .iter()
                .all(|&b| b == 0)
        );
        // More instances than slots: the extra ones are dropped, the size stays.
        let short = terrain_buffer_bytes(&params, &instances, 2);
        assert_eq!(short.len(), TERRAIN_PARAMS_BYTES + 2 * PATCH_INSTANCE_BYTES);
    }

    #[test]
    fn params_describe_the_grid() {
        let settings = LodSettings::region();
        let grid = grid(REGION_RES, APRON_RES);
        let p = TerrainParams::new(&grid, &settings);
        assert_eq!(p.camera, [0.0; 4]);
        assert_eq!(p.lattice, [9_375.0 / 64.0, -1_200_000.0, 6_371_000.0, 32.0]);
        assert_eq!(p.inner, [300_000.0, 292.96875, 1025.0, 131_250.0]);
        assert_eq!(p.apron, [2_400_000.0, 4_687.5, 513.0, 150_000.0]);
        assert_eq!(p.voxel_window, [0.0; 4]);
        assert_eq!(p.debug, [0.0; 4]);
        let moved = p
            .with_camera(Vec3::new(1.0, 2.0, -3.0))
            .with_wireframe(true);
        assert_eq!(moved.camera, [1.0, 2.0, -3.0, 0.0]);
        assert_eq!(moved.debug, [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(moved.with_wireframe(false).debug, [0.0; 4]);
        assert_eq!(moved.inner, p.inner);
    }

    /// The sRGB transfer function, written out independently of Bevy's.
    fn srgb_byte(linear: f32) -> f32 {
        let s = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        s * 255.0
    }

    #[test]
    fn palette_alphas_carry_sand_and_exposure() {
        let texels = palette_texels();
        let alpha = |era, biome, kind| texels[palette_texel_index(era, biome, kind)][3];
        use PaletteKind::*;
        let (m, d) = (VegetationEra::Modern, BiomeClass::HotDesert);
        assert_eq!((alpha(m, d, Ground), alpha(m, d, Rock)), (255, 255));
        let b = BiomeClass::Boreal;
        assert_eq!((alpha(m, b, Ground), alpha(m, b, Rock)), (0, 0));
        let t = BiomeClass::TemperateBroadleaf;
        assert_eq!(alpha(VegetationEra::Barren, t, Rock), 255);
        assert_eq!(alpha(m, d, Canopy), (canopy_cover(d) * 255.0).round() as u8);
        assert_eq!(alpha(m, b, Canopy), (canopy_cover(b) * 255.0).round() as u8);
    }

    #[test]
    fn palette_texture_matches_core() {
        for (i, b) in ALL_BIOMES.iter().enumerate() {
            assert_eq!(*b as usize, i);
        }
        for (i, e) in ALL_ERAS.iter().enumerate() {
            assert_eq!(*e as usize, i);
        }
        let texels = palette_texels();
        assert_eq!(texels.len(), (PALETTE_SIZE.x * PALETTE_SIZE.y) as usize);
        for era in ALL_ERAS {
            for biome in ALL_BIOMES {
                let p = palette(era, biome);
                for (kind, want) in [
                    (PaletteKind::Ground, p.ground),
                    (PaletteKind::Canopy, p.canopy),
                    (PaletteKind::Rock, p.rock),
                ] {
                    let t = texels[palette_texel_index(era, biome, kind)];
                    for c in 0..3 {
                        let exact = srgb_byte(want[c]);
                        assert!(
                            (f32::from(t[c]) - exact).abs() <= 0.5 + 1e-3,
                            "{era:?} {biome:?} {kind:?} channel {c}: {} vs {exact}",
                            t[c]
                        );
                    }
                    let alpha = match kind {
                        PaletteKind::Ground => (sand_sea_weight(biome) * 255.0).round() as u8,
                        PaletteKind::Canopy => (canopy_cover(biome) * 255.0).round() as u8,
                        PaletteKind::Rock => (rock_exposure(era, biome) * 255.0).round() as u8,
                    };
                    assert_eq!(t[3], alpha, "{era:?} {biome:?} {kind:?}");
                }
            }
        }
        // The layout: texel (biome × 3 + kind, era).
        assert_eq!(
            palette_texel_index(
                VegetationEra::FirstForests,
                BiomeClass::Boreal,
                PaletteKind::Rock
            ),
            2 * 36 + 8 * 3 + 2
        );
        for biome in ALL_BIOMES {
            assert!((0.0..=1.0).contains(&canopy_cover(biome)), "{biome:?}");
        }
        assert!(canopy_cover(BiomeClass::HotDesert) < canopy_cover(BiomeClass::TropicalRainforest));
        assert_eq!(canopy_cover(BiomeClass::IceCap), 0.0);
        let image = palette_image();
        assert_eq!(
            image.texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(image.size(), PALETTE_SIZE);
    }

    #[test]
    fn images_keep_the_field_layout() {
        let f = field(5, 1_000.0);
        let h = height_image(&f);
        assert_eq!(h.texture_descriptor.format, TextureFormat::R32Float);
        assert_eq!(h.size(), UVec2::new(5, 5));
        let data = h.data.as_ref().unwrap();
        // Texel (col 3, row 2) is elevation index 2 × 5 + 3.
        let at = (2 * 5 + 3) * 4;
        let texel = f32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        assert_eq!(texel, f.elevation_m[13]);
        let s = surface_image(&f);
        assert_eq!(s.texture_descriptor.format, TextureFormat::Rgba8Unorm);
        assert_eq!(&s.data.as_ref().unwrap()[13 * 4..14 * 4], &f.surface[13]);
        assert_eq!(h.asset_usage, RenderAssetUsages::RENDER_WORLD);
    }
}
