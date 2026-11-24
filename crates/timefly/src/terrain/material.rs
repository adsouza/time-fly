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
//! | 106 | uniform [`TerrainLook`]: the painterly light (shadow hue, ramp, brush, snowline latitude, snow shading), the region frame and wind (planet position, dunes) and the region sun's direction (the dune tilt bound) |
//!
//! None of the textures has a sampler: `R32Float` is not filterable in baseline WebGPU, and the
//! codes and palette are looked up texel by texel.
//!
//! The fragment (M2 design §6.4) blends the palette colours of the four surrounding surface
//! texels in colour space, breaks them up with `noise.wesl` at 400 m, 60 m and 9 m, adds rock on
//! slopes, snow from the ice byte and the snowline, and Barren-era microbial mats at the shore,
//! then lights through Bevy PBR and repaints the light: a soft three-band ramp and a cool,
//! era-tinted hue shift in shadow (snow keeps more light and less of the hue, so shaded snow
//! stays pale blue).
//!
//! The extension is not bindless, so the combined material is not either (Bevy only goes
//! bindless when the base and the extension both are). That one layout works on Metal, Vulkan
//! and WebGPU, which has no bindless support at all.

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::math::DVec3;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderType, TextureDimension, TextureFormat,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bytemuck::{Pod, Zeroable};
use timefly_core::geo::RenderDir;
use timefly_core::globe::EARTH_RADIUS_KM;
use timefly_core::terrain::{BLEND_APRON_TEXELS, LodSettings, Patch};
use timefly_core::worldgen::biome::{
    BiomeClass, VegetationEra, palette, rock_exposure, sand_sea_weight,
};
use timefly_core::worldgen::frame::{EARTH_RADIUS_M, RegionFrame};
use timefly_core::worldgen::generate::{Field, RegionGrid};
use timefly_core::worldgen::wind::wind_axis;

use super::view::ViewRanges;

use super::view::REGION_SUN_DIRECTION;

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

/// `planet.wesl`: the planet-fixed position (`planet_position_m`), imported as `package::planet`.
pub const PLANET_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e16");
/// `waves.wesl`: the sea's swell and swash (`crate::water`), imported as `package::waves` by
/// `water.wesl`.
pub const WAVES_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e17");

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

/// The ramp's shade level for snow (others keep 0.16): lifted so snow in shadow is paler than
/// before (darkest 5 % over brightest 20 % of a snowfield: 0.57, against 0.45 with the plain
/// floor). The half-light (0.58) and full-light (1.0) bands do not move, so sunlit snow is
/// unchanged and relief stays readable.
pub const SNOW_SHADE_FLOOR: f32 = 0.30;
/// How much of the cool shadow hue shaded snow takes (1 = as strongly as everything else): most of
/// it, so the shade stays clearly blue, though paler than other shade.
pub const SNOW_HUE_FACTOR: f32 = 0.78;

/// The main dunes' crest spacing (m) in sand seas: transverse crests across the wind.
pub const DUNE_SPACING_M: f32 = 1000.0;
/// The small dunes' crest spacing (m), riding on the main dunes.
pub const DUNE_SMALL_SPACING_M: f32 = 200.0;
/// Seconds for the main crests to creep one spacing downwind: 1 km an hour, about 17 m a minute.
/// Divides [`crate::art::TIME_WRAP_S`], so the creep loops seamlessly at the clock's wrap.
pub const DUNE_PERIOD_S: f32 = 3600.0;
/// Seconds for the small crests to creep one small spacing: 200 m in 10 min, about 20 m a minute
/// against the main crests' 17, so 1.2 times as fast, as small dunes are in nature. Their pattern
/// cycles six times as often (one spacing per 600 s against 3,600 s). Divides
/// [`crate::art::TIME_WRAP_S`].
pub const DUNE_SMALL_PERIOD_S: f32 = 600.0;
/// The sand-sea mask's noise scale (m): sand seas are tens of km across, the rest of the desert
/// is stony plain.
pub const SAND_SEA_SCALE_M: f32 = 40_000.0;
/// The rock strata's mean band thickness (m); the shader mixes bands of 10-60 m around it.
pub const BAND_SCALE_M: f32 = 35.0;
/// The amplitude (m) of the km-scale noise that warps the strata, so layers dip and fold.
pub const BAND_WARP_M: f32 = 40.0;
/// The slope (rise over run, about 3.4 degrees) from which dry, bare ground (rock exposure
/// of at least 0.8) starts to show strata. The weight ramps up to full at [`BAND_FULL_SLOPE`], so steeper
/// ground reads stronger and there is no visible gate edge. The ETOPO source softens canyon walls
/// to slopes of 0.08-0.25, so a 15 degree gate would miss the Grand Canyon.
pub const BAND_MIN_SLOPE: f32 = 0.06;
/// The slope at which that ramp reaches full weight. Mirrored as `BAND_FULL_SLOPE` in
/// `terrain.wesl`.
pub const BAND_FULL_SLOPE: f32 = 0.2;
/// The true slopes (rise over run) over which the sand sea fades out: the desert there is stony
/// plain, so the dunes' tilt never rides on a hillside's. Mirrored as `DUNE_SLOPE` in
/// `terrain.wesl`.
pub const DUNE_SLOPE: [f32; 2] = [0.03, 0.07];
/// How far the dunes may turn a face away from the sun: this share of the way from facing straight
/// up to the terminator, counting the terrain's own shading tilt ([`bound_dune_tilt`]). At 0.25 a
/// slip face keeps about three quarters of level ground's sunlight. Mirrored as `DUNE_AWAY_LIMIT`
/// in `terrain.wesl`.
pub const DUNE_AWAY_LIMIT: f32 = 0.25;

/// The Rust mirror of `bound_dune_tilt` (`terrain.wesl`): the dunes' shading tilt `dune`
/// (region-local xz, minus the rise over run), bounded so that with the terrain's shading tilt
/// `base` it never turns a face more than [`DUNE_AWAY_LIMIT`] of the way to the terminator of the
/// sun at `to_sun` (unit, toward the light). Unnormalised, N·L is `to_sun.y + dot(tilt,
/// to_sun.xz)`. Only the dune tilt's component along `to_sun.xz` changes, and only where it turns
/// the face away: the bounded N·L is at least `min(base N·L, (1 - DUNE_AWAY_LIMIT) · to_sun.y)`.
pub fn bound_dune_tilt(base: Vec2, dune: Vec2, to_sun: Vec3) -> Vec2 {
    let across = Vec2::new(to_sun.x, to_sun.z);
    let len2 = across.dot(across);
    if to_sun.y <= 0.0 || len2 < 1e-6 {
        return dune;
    }
    let lowest = -DUNE_AWAY_LIMIT * to_sun.y;
    let along = dune.dot(across);
    let allowed = along.max(0.0_f32.min(lowest - base.dot(across)));
    dune + across * ((allowed - along) / len2)
}

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
    /// snowline in glacial eras (ruling R18, `snowline.wesl`); `z`: [`SNOW_SHADE_FLOOR`];
    /// `w`: [`SNOW_HUE_FACTOR`].
    pub relief: Vec4,
    /// `xyz`: the region centre's unit direction in the era frame; `w`: `EARTH_RADIUS_M`.
    /// With `frame_north` and `frame_east`, the input of `planet_position_m`.
    pub frame_center: Vec4,
    /// `xyz`: the unit tangent toward north at the centre (region-local `-z`); `w`: 0.
    pub frame_north: Vec4,
    /// `xyz`: the unit tangent toward east at the centre (region-local `+x`); `w`: 0.
    pub frame_east: Vec4,
    /// `xyz`: the prevailing wind's 3D axis `W` at the centre (`wind_axis`), tangent to the
    /// sphere there. A dune's crest phase is the along-wind distance from the region centre,
    /// `dot(p, W)` for the planet position `p`, which the shader takes from local metres. It
    /// depends on the centre, so crests shift between visits from different centres (a known
    /// limitation; the plan appendix). `w`: 1 when the dunes are drawn, 0 when `--no-dunes`
    /// turned them off; the shader tests `> 0.5` once, so every pixel takes the same branch.
    pub wind: Vec4,
    /// `x`: [`DUNE_SPACING_M`]; `y`: [`DUNE_SMALL_SPACING_M`]; `z`: [`DUNE_PERIOD_S`]; `w`:
    /// [`DUNE_SMALL_PERIOD_S`].
    pub dunes: Vec4,
    /// `x`: [`SAND_SEA_SCALE_M`]; `y`: [`BAND_SCALE_M`]; `z`: [`BAND_WARP_M`]; `w`:
    /// [`BAND_MIN_SLOPE`].
    pub patterns: Vec4,
    /// `xyz`: the unit direction toward the region's sun ([`REGION_SUN_DIRECTION`], the
    /// direction the sun entity's transform gives its light), for `bound_dune_tilt`; `w`: 0.
    /// Passed here rather than read from Bevy's light list, whose order is not guaranteed.
    pub sun: Vec4,
}

impl Default for TerrainLook {
    fn default() -> Self {
        terrain_look(
            Vec3::ONE,
            1.0,
            0.0,
            0.0,
            &RegionFrame {
                center: RenderDir(DVec3::Z),
            },
            true,
        )
    }
}

/// The look for a sky zenith colour (linear), the full-sun illumination, the centre latitude, the
/// era's sea-level anomaly, the region's frame (for the planet position and the wind) and
/// whether the dunes are drawn (`--no-dunes` turns them off).
pub fn terrain_look(
    zenith: Vec3,
    full_sun: f32,
    centre_lat_deg: f32,
    sea_level_anomaly_m: f32,
    frame: &RegionFrame,
    dunes: bool,
) -> TerrainLook {
    let basis = frame.basis();
    let (_, wind) = wind_axis(frame);
    let luminance = zenith.dot(Vec3::new(0.2126, 0.7152, 0.0722)).max(1e-6);
    TerrainLook {
        shadow: (zenith / luminance).extend(SHADOW_HUE_STRENGTH),
        light: Vec4::new(full_sun, RAMP_AMOUNT, BRUSH_STRENGTH, centre_lat_deg),
        relief: Vec4::new(
            RELIEF_EXAGGERATION,
            sea_level_anomaly_m,
            SNOW_SHADE_FLOOR,
            SNOW_HUE_FACTOR,
        ),
        frame_center: basis.center.as_vec3().extend(EARTH_RADIUS_M as f32),
        frame_north: basis.north.as_vec3().extend(0.0),
        frame_east: basis.east.as_vec3().extend(0.0),
        wind: wind.as_vec3().extend(if dunes { 1.0 } else { 0.0 }),
        dunes: Vec4::new(
            DUNE_SPACING_M,
            DUNE_SMALL_SPACING_M,
            DUNE_PERIOD_S,
            DUNE_SMALL_PERIOD_S,
        ),
        patterns: Vec4::new(SAND_SEA_SCALE_M, BAND_SCALE_M, BAND_WARP_M, BAND_MIN_SLOPE),
        sun: REGION_SUN_DIRECTION.normalize().extend(0.0),
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
    /// `[wireframe (0 or 1), fog start m, fog scale, reserved]`: the fog reads a fragment's
    /// distance `d` as `max(d − start, 1 m) × scale` ([`super::view::ViewRanges`]); `[0, 1]`, the
    /// identity, at M2's heights.
    pub shading: [f32; 4],
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
            shading: [0.0, 0.0, 1.0, 0.0],
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
        let [_, start, scale, reserved] = self.shading;
        TerrainParams {
            shading: [if on { 1.0 } else { 0.0 }, start, scale, reserved],
            ..self
        }
    }

    /// The same header with the fog's start and scale from `ranges`.
    pub fn with_fog(self, ranges: &ViewRanges) -> TerrainParams {
        let [wireframe, _, _, reserved] = self.shading;
        TerrainParams {
            shading: [wireframe, ranges.fog_start_m, ranges.fog_scale(), reserved],
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

/// Adds the shared noise module (`noise.wesl`), which the terrain, the sea and the cloud import.
/// Idempotent: each plugin that imports it calls this, so plugin order does not matter.
pub fn add_noise_shader(shaders: &mut Assets<Shader>) {
    let noise = Shader::from_wesl(
        include_str!("../../assets/shaders/noise.wesl"),
        "embedded://timefly/noise.wesl",
    );
    let _ = shaders.insert(NOISE_SHADER.id(), noise);
}

/// Adds the embedded terrain shaders to the shader assets, so the terrain renders from any
/// asset root. `terrain_cdlod.wesl` registers as the WESL module `package::terrain_cdlod`,
/// `noise.wesl` as `package::noise` (`import package::noise::gradient_noise3;`) and
/// `snowline.wesl` as `package::snowline`, `planet.wesl` as `package::planet` and `waves.wesl`
/// as `package::waves`.
pub fn add_terrain_shaders(shaders: &mut Assets<Shader>) {
    add_noise_shader(shaders);
    let planet = Shader::from_wesl(
        include_str!("../../assets/shaders/planet.wesl"),
        "embedded://timefly/planet.wesl",
    );
    let snowline = Shader::from_wesl(
        include_str!("../../assets/shaders/snowline.wesl"),
        "embedded://timefly/snowline.wesl",
    );
    let waves = Shader::from_wesl(
        include_str!("../../assets/shaders/waves.wesl"),
        "embedded://timefly/waves.wesl",
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
    let _ = shaders.insert(PLANET_SHADER.id(), planet);
    let _ = shaders.insert(SNOWLINE_SHADER.id(), snowline);
    let _ = shaders.insert(WAVES_SHADER.id(), waves);
    let _ = shaders.insert(TERRAIN_CDLOD_SHADER.id(), cdlod);
    let _ = shaders.insert(TERRAIN_SHADER.id(), terrain);
    let _ = shaders.insert(TERRAIN_PREPASS_SHADER.id(), prepass);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::art::wesl_struct_fields;
    use bevy::math::DVec3;
    use core::mem::{align_of, offset_of, size_of};
    use timefly_core::terrain::select_patches;
    use timefly_core::worldgen::{APRON_RES, APRON_SIZE_M, REGION_RES, REGION_SIZE_M};

    #[test]
    fn look_carries_snow_shade() {
        let look = terrain_look(
            Vec3::ONE,
            1.0,
            0.0,
            0.0,
            &RegionFrame {
                center: RenderDir(DVec3::Z),
            },
            true,
        );
        assert_eq!(look.relief.z, 0.30);
        assert_eq!(look.relief.w, 0.78);
    }

    #[test]
    fn look_carries_the_dune_switch() {
        assert_eq!(TerrainLook::default().wind.w, 1.0);
        let frame = RegionFrame {
            center: RenderDir(DVec3::Z),
        };
        assert_eq!(
            terrain_look(Vec3::ONE, 1.0, 0.0, 0.0, &frame, false).wind.w,
            0.0
        );
        assert_eq!(
            terrain_look(Vec3::ONE, 1.0, 0.0, 0.0, &frame, true).wind.w,
            1.0
        );
    }

    #[test]
    fn band_constants() {
        assert_eq!(BAND_SCALE_M, 35.0);
        assert_eq!(BAND_WARP_M, 40.0);
        assert_eq!(BAND_MIN_SLOPE, 0.06);
        assert_eq!(BAND_FULL_SLOPE, 0.2);
        assert!(TERRAIN_WESL.contains("const BAND_FULL_SLOPE: f32 = 0.2;"));
        let look = TerrainLook::default();
        assert_eq!(look.patterns.y, 35.0);
        assert_eq!(look.patterns.z, 40.0);
        assert_eq!(look.patterns.w, 0.06);
    }

    #[test]
    fn dune_constants() {
        assert_eq!(DUNE_SPACING_M, 1000.0);
        assert_eq!(DUNE_SMALL_SPACING_M, 200.0);
        assert_eq!(DUNE_PERIOD_S, 3600.0);
        assert_eq!(DUNE_SMALL_PERIOD_S, 600.0);
        assert_eq!(SAND_SEA_SCALE_M, 40_000.0);
        crate::art::assert_divides_wrap(DUNE_PERIOD_S);
        crate::art::assert_divides_wrap(DUNE_SMALL_PERIOD_S);
        let look = TerrainLook::default();
        assert_eq!(look.dunes, Vec4::new(1000.0, 200.0, 3600.0, 600.0));
        assert_eq!(look.patterns.x, 40_000.0);
        // The WESL mirror declares the same fields in the same order.
        let names: Vec<String> = wesl_struct_fields(TERRAIN_WESL, "TerrainLook")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            [
                "shadow",
                "light",
                "relief",
                "frame_center",
                "frame_north",
                "frame_east",
                "wind",
                "dunes",
                "patterns",
                "sun"
            ]
        );
    }

    #[test]
    fn look_carries_the_region_sun() {
        let look = TerrainLook::default();
        assert_eq!(look.sun, REGION_SUN_DIRECTION.normalize().extend(0.0));
        // The sun entity's light shines from where its transform puts it: toward the origin from
        // REGION_SUN_DIRECTION, so its direction to the light is that direction.
        let t = Transform::from_translation(REGION_SUN_DIRECTION.normalize())
            .looking_at(Vec3::ZERO, Vec3::Y);
        assert!((-t.forward().as_vec3() - look.sun.truncate()).length() < 1e-6);
    }

    #[test]
    fn dune_tilt_bound_mirrors_the_wesl() {
        // The shader bounds the dunes' tilt with the look's sun, not with Bevy's light list
        // (whose order a second directional light on the terrain layer could change).
        assert!(TERRAIN_WESL.contains(
            "let shading = base_shading + bound_dune_tilt(base_shading, painted.dune_gradient);"
        ));
        assert!(TERRAIN_WESL.contains("let to_sun = look.sun.xyz;"));
        assert!(!TERRAIN_WESL.contains("directional_lights"));
        assert_eq!(
            crate::art::wesl_const_f32(TERRAIN_WESL, "DUNE_AWAY_LIMIT"),
            DUNE_AWAY_LIMIT
        );
        assert_eq!(DUNE_AWAY_LIMIT, 0.25);
        // The sand sea fades out on slopes, by the same constants.
        assert!(TERRAIN_WESL.contains(&format!(
            "const DUNE_SLOPE: vec2<f32> = vec2<f32>({:?}, {:?});",
            DUNE_SLOPE[0], DUNE_SLOPE[1]
        )));
        // A zero dune tilt stays exactly zero, whatever the terrain does.
        let sun = REGION_SUN_DIRECTION.normalize();
        for base in [Vec2::ZERO, Vec2::new(3.0, -2.0), Vec2::new(-5.0, 4.0)] {
            assert_eq!(bound_dune_tilt(base, Vec2::ZERO, sun), Vec2::ZERO);
        }
    }

    /// N·L before normalisation for a shading tilt (`normal = (tilt.x, 1, tilt.y)`).
    fn unnormalised_n_dot_l(tilt: Vec2, to_sun: Vec3) -> f32 {
        to_sun.y + tilt.dot(Vec2::new(to_sun.x, to_sun.z))
    }

    proptest::proptest! {
        /// For any terrain tilt, dune tilt and a sun at the region sun's elevation (any bearing),
        /// the bounded tilt never turns a face further than `DUNE_AWAY_LIMIT` toward the
        /// terminator unless the terrain alone already does, leaves the component toward the sun
        /// and the component across the sun's bearing unchanged, and a zero dune tilt stays zero.
        #[test]
        fn bound_dune_tilt_keeps_faces_lit(
            bx in -4.0_f32..4.0,
            bz in -4.0_f32..4.0,
            dx in -1.5_f32..1.5,
            dz in -1.5_f32..1.5,
            bearing in 0.0_f32..core::f32::consts::TAU,
        ) {
            let elevation = REGION_SUN_DIRECTION.normalize().y;
            let flat = (1.0 - elevation * elevation).sqrt();
            let sun = Vec3::new(flat * bearing.cos(), elevation, flat * bearing.sin());
            let (base, dune) = (Vec2::new(bx, bz), Vec2::new(dx, dz));
            let bounded = bound_dune_tilt(base, dune, sun);
            let across = Vec2::new(sun.x, sun.z);
            let tol = 1e-4 * (1.0 + base.length() + dune.length());

            let lit = unnormalised_n_dot_l(base + bounded, sun);
            let floor = unnormalised_n_dot_l(base, sun).min((1.0 - DUNE_AWAY_LIMIT) * sun.y);
            proptest::prop_assert!(lit >= floor - tol, "N.L {lit} under {floor}");
            // Only the component along the sun's bearing ever changes.
            let side = across.perp();
            proptest::prop_assert!((bounded.dot(side) - dune.dot(side)).abs() <= tol);
            // A dune tilt toward the sun (the windward face) is kept as it is.
            if dune.dot(across) >= 0.0 {
                proptest::prop_assert_eq!(bounded, dune);
            }
            // The bound never adds tilt toward the sun beyond what the dunes had, and never
            // pushes past the dunes' own tilt away from it.
            proptest::prop_assert!(bounded.dot(across) >= dune.dot(across) - tol);
            proptest::prop_assert!(bounded.dot(across) <= dune.dot(across).max(0.0) + tol);
            proptest::prop_assert_eq!(bound_dune_tilt(base, Vec2::ZERO, sun), Vec2::ZERO);
        }
    }

    const CDLOD_WESL: &str = include_str!("../../assets/shaders/terrain_cdlod.wesl");
    const TERRAIN_WESL: &str = include_str!("../../assets/shaders/terrain.wesl");

    /// `name: type` fields of a struct declared in `terrain_cdlod.wesl`, in order.
    /// The block a `{` at `open` opens, up to its matching `}` (exclusive), as byte offsets.
    fn block_after(text: &str, open: usize) -> (usize, usize) {
        let mut depth = 0usize;
        for (i, c) in text[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return (open, open + i);
                    }
                }
                _ => {}
            }
        }
        panic!("unbalanced braces after byte {open}");
    }

    #[test]
    fn sloping_sand_seas_get_stony_plain_detail() {
        // At raw_sea = 1 and slope > 0.07, the effective weight is zero.
        // Both texture branches and the final blend must use that same weight.
        let (_, desert) = TERRAIN_WESL.split_once("fn desert_at(").unwrap();
        let (body, paint) = desert.split_once("return d;").unwrap();
        let (_, adjusted) = body
            .split_once("d.sea = raw_sea * (1.0 - smoothstep(DUNE_SLOPE.x, DUNE_SLOPE.y, slope));")
            .unwrap();
        assert!(adjusted.contains("if d.sea > 0.0 && main_w > 0.0"));
        let (_, stony) = adjusted.split_once("if d.sea < 1.0 {").unwrap();
        assert!(
            stony.contains("d.gravel = smoothstep") && stony.contains("d.mottle = planet_noise")
        );
        assert!(paint.contains("let sea = desert.sea;"));
        assert!(paint.contains("let plain = desert_ground * (1.0 - sea);"));
    }

    #[test]
    fn desert_skips_the_dunes_when_off() {
        // `--no-dunes` (wind.w = 0) must skip every piece of dune work, with one test of a
        // uniform the whole frame shares, while the sand-sea mask and the stony plain still run.
        let start = TERRAIN_WESL.find("fn desert_at(").unwrap();
        let (_, end) = block_after(
            TERRAIN_WESL,
            start + TERRAIN_WESL[start..].find('{').unwrap(),
        );
        let body = &TERRAIN_WESL[start..end];
        let gate = body
            .find("if look.wind.w > 0.5 {")
            .expect("desert_at gates the dunes on look.wind.w");
        let (open, close) = block_after(body, gate + body[gate..].find('{').unwrap());
        let inside = &body[open..close];
        for work in [
            "let sharp_w",
            "let main_w",
            "let small_w",
            "dune_warp(p)",
            "dune_small_warp(p)",
            "dune_profile(u)",
            "let sinc",
        ] {
            assert!(inside.contains(work), "{work:?} runs outside the dune gate");
            assert_eq!(
                body.matches(work).count(),
                inside.matches(work).count(),
                "{work:?}"
            );
        }
        assert!(
            body[..gate].contains("let mask"),
            "the sand-sea mask must run with dunes off"
        );
        assert!(
            body[close..].contains("if d.sea < 1.0 {"),
            "the stony plain must run with dunes off"
        );
    }

    fn wesl_fields(name: &str) -> Vec<(String, String)> {
        wesl_struct_fields(CDLOD_WESL, name)
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
            (offset_of!(TerrainParams, shading), "shading"),
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
        assert_eq!(p.shading, [0.0, 0.0, 1.0, 0.0]);
        let moved = p
            .with_camera(Vec3::new(1.0, 2.0, -3.0))
            .with_wireframe(true);
        assert_eq!(moved.camera, [1.0, 2.0, -3.0, 0.0]);
        assert_eq!(moved.shading, [1.0, 0.0, 1.0, 0.0]);
        assert_eq!(moved.with_wireframe(false).shading, [0.0, 0.0, 1.0, 0.0]);
        // The fog's start and scale ride alongside the wireframe switch.
        let high = super::super::view::view_ranges(300_000.0);
        let fogged = moved.with_fog(&high);
        assert_eq!(
            fogged.shading,
            [1.0, high.fog_start_m, high.fog_scale(), 0.0]
        );
        assert_eq!(
            fogged.with_wireframe(false).shading[1..3],
            fogged.shading[1..3]
        );
        assert_eq!(
            moved
                .with_fog(&super::super::view::view_ranges(0.0))
                .shading,
            moved.shading
        );
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
