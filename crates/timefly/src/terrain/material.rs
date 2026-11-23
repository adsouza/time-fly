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
//! | 105 | the palette, `Rgba8UnormSrgb` (a placeholder until Task 8 fills it) |
//!
//! None of the textures has a sampler: `R32Float` is not filterable in baseline WebGPU, and the
//! codes and palette are looked up texel by texel.
//!
//! The extension is not bindless, so the combined material is not either (Bevy only goes
//! bindless when the base and the extension both are). That one layout works on Metal, Vulkan
//! and WebGPU, which has no bindless support at all.

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, TextureDimension, TextureFormat};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use bytemuck::{Pod, Zeroable};
use timefly_core::globe::EARTH_RADIUS_KM;
use timefly_core::terrain::{BLEND_APRON_TEXELS, LodSettings, Patch};
use timefly_core::worldgen::generate::{Field, RegionGrid};

/// `terrain.wesl`: the main-pass vertex and fragment shaders.
pub const TERRAIN_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e11");
/// `terrain_prepass.wesl`: the prepass and shadow vertex shader.
pub const TERRAIN_PREPASS_SHADER: Handle<Shader> =
    uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e12");
/// `terrain_cdlod.wesl`: the shared CDLOD vertex math (imported by the two above).
pub const TERRAIN_CDLOD_SHADER: Handle<Shader> =
    uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e13");

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

/// Palette texture size: one column per biome class, one row per vegetation era.
pub const PALETTE_SIZE: UVec2 = UVec2::new(12, 6);

/// A neutral placeholder palette ([`PALETTE_SIZE`]); Task 8 replaces it with the real colours.
pub fn placeholder_palette() -> Image {
    let texels = (PALETTE_SIZE.x * PALETTE_SIZE.y) as usize;
    Image::new(
        Extent3d {
            width: PALETTE_SIZE.x,
            height: PALETTE_SIZE.y,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        [128u8, 140, 110, 255].repeat(texels),
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
/// asset root. `terrain_cdlod.wesl` registers as the WESL module `timefly::terrain_cdlod`.
pub fn add_terrain_shaders(shaders: &mut Assets<Shader>) {
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
