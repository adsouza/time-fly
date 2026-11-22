//! Globe materials and the GPU data they bind.
//!
//! All globe surfaces share `globe.wesl`, specialised by a shader def per material: the plate
//! pieces (`PLATE_PIECES`), the ocean shell (`OCEAN_SHELL`), the fixed recent globe
//! (`RECENT_GLOBE`) and the bridge crossfade (`COMPOSITE`). The atmosphere has its own shader.
//!
//! Validity is decided on the CPU, in f64, whenever the selected age changes: each plate gets
//! flags for its current and source rotations, and each footprint interval list gets flags for
//! the current age and both source ages ([`list_flags`], using the same
//! [`interval_accepts`] rule as `PlateFootprints::contains`). The GPU only tests bits, so CPU and
//! GPU acceptance cannot diverge at f32-rounded interval bounds such as 305.01 Ma. Rotations are
//! interpolated on the CPU too (shortest-path slerp) and normalised before upload; the shader
//! never interpolates quaternions.

use bevy::asset::uuid_handle;
use bevy::math::{Quat, Vec3, Vec4};
use bevy::mesh::MeshVertexBufferLayoutRef;
use bevy::pbr::{Material, MaterialPipeline, MaterialPipelineKey};
use bevy::prelude::*;
use bevy::render::render_resource::{
    AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError,
};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::{ShaderDefVal, ShaderRef};
use bytemuck::{Pod, Zeroable};
use timefly_core::geodynamics::{CrustModel, PlateIndex, interval_accepts};
use timefly_core::globe::{
    DeepSampling, ELEVATION_EXAGGERATION, endpoint_available, piece_visible, plate_rotations,
    source_id_ranks, tie_bias_km,
};

pub use timefly_core::globe::EARTH_RADIUS_KM;

use super::mesh::{ATTRIBUTE_PIECE, IntervalList};

/// `globe.wesl`, embedded so the globe renders from any asset root.
pub const GLOBE_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e01");
/// `atmosphere.wesl`.
pub const ATMOSPHERE_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e02");

/// Plate flag: the plate has a rotation at the current age.
pub const PLATE_CURRENT_VALID: u32 = 1;
/// Plate flag: the plate has a rotation at the younger endpoint's source age.
pub const PLATE_SOURCE0_VALID: u32 = 2;
/// Plate flag: the plate has a rotation at the older endpoint's source age.
pub const PLATE_SOURCE1_VALID: u32 = 4;
/// Interval-list flag: the list accepts the current age.
pub const LIST_VALID_NOW: u32 = 1;
/// Interval-list flag: the list accepts the younger endpoint's source age.
pub const LIST_VALID_SOURCE0: u32 = 2;
/// Interval-list flag: the list accepts the older endpoint's source age.
pub const LIST_VALID_SOURCE1: u32 = 4;

/// Below sea level the ocean shell sits this far under the land pieces (km).
pub const OCEAN_SHELL_DEPTH_KM: f32 = 2.0;
/// Ocean-tint depth used where a source grid has land in a deep-time gap (m).
pub const OCEAN_FALLBACK_M: f32 = -2500.0;
/// The atmosphere shell's radius relative to the Earth's: above the highest exaggerated peak.
pub const ATMOSPHERE_RADIUS_FACTOR: f32 = 1.045;

/// One plate as the shader reads it (`Plate` in `globe.wesl`): 64 bytes.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Pod, Zeroable)]
pub struct PlateGpu {
    /// Current-age rotation `[x, y, z, w]`, normalised (identity when invalid).
    pub current: [f32; 4],
    /// Source-frame rotations of the younger and older endpoints.
    pub sources: [[f32; 4]; 2],
    pub flags: u32,
    /// Radial tie bias by source-ID rank (km).
    pub tie_bias_km: f32,
    pub _pad: [u32; 2],
}

/// Every plate's GPU record for a deep sampling, indexed by plate index.
pub fn plate_gpu_data(crust: &CrustModel, sampling: &DeepSampling) -> Vec<PlateGpu> {
    let rotations = crust.rotations();
    let ranks = source_id_ranks(rotations);
    let count = ranks.len() as u32;
    (0..count)
        .map(|plate| {
            let r = plate_rotations(rotations, PlateIndex(plate), sampling);
            let bits = [
                (r.current, PLATE_CURRENT_VALID),
                (r.sources[0], PLATE_SOURCE0_VALID),
                (r.sources[1], PLATE_SOURCE1_VALID),
            ];
            let flags = bits
                .iter()
                .filter(|(q, _)| q.is_some())
                .fold(0, |acc, (_, bit)| acc | bit);
            let array = |q: Option<Quat>| q.unwrap_or(Quat::IDENTITY).normalize().to_array();
            PlateGpu {
                current: array(r.current),
                sources: r.sources.map(array),
                flags,
                tie_bias_km: tie_bias_km(ranks[plate as usize], count) as f32,
                _pad: [0; 2],
            }
        })
        .collect()
}

/// Validity flags per interval list at `[current, younger source, older source]` ages.
pub fn list_flags(lists: &[IntervalList], ages_myr: [f64; 3]) -> Vec<u32> {
    let bits = [LIST_VALID_NOW, LIST_VALID_SOURCE0, LIST_VALID_SOURCE1];
    lists
        .iter()
        .map(|list| {
            (0..3)
                .filter(|&i| {
                    list.intervals
                        .iter()
                        .any(|&(y, o)| interval_accepts(y, o, ages_myr[i]))
                })
                .fold(0, |acc, i| acc | bits[i])
        })
        .collect()
}

/// The shader's piece visibility test on the uploaded flags.
pub fn gpu_piece_visible(plate_flags: u32, list_flags: u32) -> bool {
    piece_visible(
        plate_flags & PLATE_CURRENT_VALID != 0,
        list_flags & LIST_VALID_NOW != 0,
    )
}

/// The shader's endpoint test on the uploaded flags; `endpoint` is 0 (younger) or 1 (older).
pub fn gpu_endpoint_available(plate_flags: u32, list_flags: u32, endpoint: usize) -> bool {
    let (plate_bit, list_bit) = [
        (PLATE_SOURCE0_VALID, LIST_VALID_SOURCE0),
        (PLATE_SOURCE1_VALID, LIST_VALID_SOURCE1),
    ][endpoint];
    endpoint_available(plate_flags & plate_bit != 0, list_flags & list_bit != 0)
}

/// Where the vertex stage puts a plate-fixed direction: rotated by the current rotation.
pub fn piece_world_dir(plate: &PlateGpu, plate_fixed_dir: Vec3) -> Vec3 {
    Quat::from_array(plate.current) * plate_fixed_dir
}

/// Uniform shared by the globe surfaces (`GlobeParams` in `globe.wesl`).
#[derive(ShaderType, Copy, Clone, Debug, PartialEq)]
pub struct GlobeParams {
    /// World direction toward the sun (`w` unused).
    pub sun_dir: Vec4,
    /// Deep: `(younger anomaly m, older anomaly m, older weight, cube size)`. Recent: `(0, 0,
    /// older weight, cube size)`. Composite: `(0, 0, deep weight, 0)`.
    pub endpoint: Vec4,
    /// `(radius km, exaggeration, ocean fallback m, ocean shell depth km)`.
    pub shape: Vec4,
}

impl GlobeParams {
    fn new(endpoint: Vec4, sun: Vec3) -> GlobeParams {
        GlobeParams {
            sun_dir: sun.normalize_or_zero().extend(0.0),
            endpoint,
            shape: Vec4::new(
                EARTH_RADIUS_KM as f32,
                ELEVATION_EXAGGERATION as f32,
                OCEAN_FALLBACK_M,
                OCEAN_SHELL_DEPTH_KM,
            ),
        }
    }

    /// Parameters of the plate pieces and the ocean shell for a deep sampling.
    pub fn deep(sampling: &DeepSampling, cube_size: u32, sun: Vec3) -> GlobeParams {
        let [a, b] = sampling.anomalies_m();
        GlobeParams::new(
            Vec4::new(
                a as f32,
                b as f32,
                sampling.older_weight as f32,
                cube_size as f32,
            ),
            sun,
        )
    }

    /// Parameters of the fixed recent globe for an ICE-6G_C bracket.
    pub fn recent(older_weight: f64, cube_size: u32, sun: Vec3) -> GlobeParams {
        GlobeParams::new(
            Vec4::new(0.0, 0.0, older_weight as f32, cube_size as f32),
            sun,
        )
    }

    /// Parameters of the bridge crossfade (and the atmosphere, which reads only the sun).
    pub fn composite(deep_weight: f64, sun: Vec3) -> GlobeParams {
        GlobeParams::new(Vec4::new(0.0, 0.0, deep_weight as f32, 0.0), sun)
    }

    /// Parameters of the atmosphere shell: the sun, and the shell radius factor in `endpoint.w`.
    pub fn atmosphere(sun: Vec3) -> GlobeParams {
        GlobeParams::new(Vec4::new(0.0, 0.0, 0.0, ATMOSPHERE_RADIUS_FACTOR), sun)
    }

    pub fn with_sun(mut self, sun: Vec3) -> GlobeParams {
        self.sun_dir = sun.normalize_or_zero().extend(0.0);
        self
    }
}

fn push_def(descriptor: &mut RenderPipelineDescriptor, def: &'static str) {
    descriptor
        .vertex
        .shader_defs
        .push(ShaderDefVal::Bool(def.into(), true));
    if let Some(fragment) = descriptor.fragment.as_mut() {
        fragment
            .shader_defs
            .push(ShaderDefVal::Bool(def.into(), true));
    }
}

fn position_only_layout(
    descriptor: &mut RenderPipelineDescriptor,
    layout: &MeshVertexBufferLayoutRef,
) -> Result<(), SpecializedMeshPipelineError> {
    descriptor.vertex.buffers = vec![
        layout
            .0
            .get_layout(&[Mesh::ATTRIBUTE_POSITION.at_shader_location(0)])?,
    ];
    Ok(())
}

/// The rigid plate pieces of the deep-time globe.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct PlateMaterial {
    #[uniform(0)]
    pub params: GlobeParams,
    /// The younger endpoint's PaleoDEM.
    #[texture(1, dimension = "cube")]
    #[sampler(2)]
    pub younger: Handle<Image>,
    /// The older endpoint's PaleoDEM.
    #[texture(3, dimension = "cube")]
    #[sampler(4)]
    pub older: Handle<Image>,
    /// `array<Plate>`, one [`PlateGpu`] per plate index.
    #[storage(5, read_only)]
    pub plates: Handle<ShaderBuffer>,
    /// One flag word per interval list.
    #[storage(6, read_only)]
    pub list_flags: Handle<ShaderBuffer>,
}

impl Material for PlateMaterial {
    fn vertex_shader() -> ShaderRef {
        GLOBE_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        GLOBE_SHADER.into()
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
        push_def(descriptor, "PLATE_PIECES");
        descriptor.vertex.buffers = vec![layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            ATTRIBUTE_PIECE.at_shader_location(1),
        ])?];
        Ok(())
    }
}

/// The continuous ocean shell under the deep-time pieces, tinted by geographic bathymetry.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct OceanMaterial {
    #[uniform(0)]
    pub params: GlobeParams,
    #[texture(1, dimension = "cube")]
    #[sampler(2)]
    pub younger: Handle<Image>,
    #[texture(3, dimension = "cube")]
    #[sampler(4)]
    pub older: Handle<Image>,
}

impl Material for OceanMaterial {
    fn vertex_shader() -> ShaderRef {
        GLOBE_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        GLOBE_SHADER.into()
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
        push_def(descriptor, "OCEAN_SHELL");
        position_only_layout(descriptor, layout)
    }
}

/// The fixed geographic globe of the recent (ICE-6G_C) path.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct RecentGlobeMaterial {
    #[uniform(0)]
    pub params: GlobeParams,
    #[texture(1, dimension = "cube")]
    #[sampler(2)]
    pub younger: Handle<Image>,
    #[texture(3, dimension = "cube")]
    #[sampler(4)]
    pub older: Handle<Image>,
}

impl Material for RecentGlobeMaterial {
    fn vertex_shader() -> ShaderRef {
        GLOBE_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        GLOBE_SHADER.into()
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
        push_def(descriptor, "RECENT_GLOBE");
        position_only_layout(descriptor, layout)
    }
}

/// The bridge crossfade: the recent and deep globe views, mixed in linear colour.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct CompositeMaterial {
    #[uniform(0)]
    pub params: GlobeParams,
    #[texture(1)]
    pub recent_view: Handle<Image>,
    #[texture(3)]
    pub deep_view: Handle<Image>,
}

impl Material for CompositeMaterial {
    fn vertex_shader() -> ShaderRef {
        GLOBE_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        GLOBE_SHADER.into()
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
        push_def(descriptor, "COMPOSITE");
        descriptor.primitive.cull_mode = None;
        position_only_layout(descriptor, layout)
    }
}

/// The painterly atmosphere rim, drawn once over whichever globe is showing.
#[derive(Asset, TypePath, AsBindGroup, Clone, Debug)]
pub struct AtmosphereMaterial {
    #[uniform(0)]
    pub params: GlobeParams,
}

impl Material for AtmosphereMaterial {
    fn vertex_shader() -> ShaderRef {
        ATMOSPHERE_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        ATMOSPHERE_SHADER.into()
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
        position_only_layout(descriptor, layout)
    }
}

/// Adds the embedded globe shaders to the shader assets.
pub fn add_globe_shaders(shaders: &mut Assets<Shader>) {
    let globe = Shader::from_wesl(
        include_str!("../../assets/shaders/globe.wesl"),
        "embedded://timefly/globe.wesl",
    );
    let atmosphere = Shader::from_wesl(
        include_str!("../../assets/shaders/atmosphere.wesl"),
        "embedded://timefly/atmosphere.wesl",
    );
    let _ = shaders.insert(GLOBE_SHADER.id(), globe);
    let _ = shaders.insert(ATMOSPHERE_SHADER.id(), atmosphere);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::math::{Quat, Vec3};
    use timefly_core::cube::{CubeFace, texel_center_dir};
    use timefly_core::geodynamics::{
        CrustModel, FootprintEntry, PlateFootprints, PlateId, PlateIndex, RotationTable,
    };
    use timefly_core::globe::{DeepEndpoint, DeepSampling};

    use crate::globe::mesh::build_plate_pieces;

    fn about_y(deg: f32) -> Quat {
        Quat::from_rotation_y(deg.to_radians())
    }

    fn canonical(q: Quat) -> [f32; 4] {
        (if q.w < 0.0 { -q } else { q }).to_array()
    }

    /// Plate 0 (id 300): 170 deg about +Y at 0 Ma, 190 deg at 10 Ma (stored w >= 0), slightly
    /// off unit length. Plate 1 (id 100): identity, missing circuit at 10 Ma.
    fn crust(footprint: Vec<FootprintEntry>) -> CrustModel {
        let q0 = Quat::from_array(canonical(about_y(170.0))) * 1.0008;
        let q1 = Quat::from_array(canonical(about_y(190.0))) * 0.9993;
        let rotations = RotationTable {
            plate_ids: vec![PlateId(300), PlateId(100)],
            times_myr: vec![0.0, 10.0],
            quats: vec![
                q0.to_array(),
                Quat::IDENTITY.to_array(),
                q1.to_array(),
                Quat::IDENTITY.to_array(),
            ],
            valid: vec![true, true, true, false],
        };
        // One cube-1 cell per face; the +X cell holds the given entries.
        let mut offsets = vec![0u32];
        let mut entries = Vec::new();
        for face in CubeFace::ALL {
            if face == CubeFace::PosX {
                entries.extend(footprint.iter().copied());
            }
            offsets.push(entries.len() as u32);
        }
        let footprints = PlateFootprints {
            cube_size: 1,
            offsets,
            entries,
        };
        CrustModel::new(rotations, footprints).unwrap()
    }

    fn entry(plate: u32, younger_myr: f64, older_myr: f64) -> FootprintEntry {
        FootprintEntry {
            plate_index: PlateIndex(plate),
            younger_myr,
            older_myr,
        }
    }

    fn sampling(age_myr: f64, sources: [f64; 2]) -> DeepSampling {
        let e = |s| DeepEndpoint {
            frame: 0,
            source_rotation_age_myr: s,
            sea_level_anomaly_m: 0.0,
        };
        DeepSampling {
            age_myr,
            endpoints: [e(sources[0]), e(sources[1])],
            older_weight: 0.5,
        }
    }

    #[test]
    fn plate_buffer_uploads_normalised_shortest_path_rotations() {
        let c = crust(vec![entry(0, 0.0, 540.0)]);
        let data = plate_gpu_data(&c, &sampling(5.0, [0.0, 10.0]));
        let p0 = data[0];
        for q in [p0.current, p0.sources[0], p0.sources[1]] {
            assert!((Quat::from_array(q).length() - 1.0).abs() < 1e-6, "{q:?}");
        }
        // The GPU gets the CPU's shortest-path midpoint: 180 deg, +Z to -Z.
        let mid = Quat::from_array(p0.current);
        // Inputs up to 1e-3 off unit length skew the slerp slightly.
        assert!(
            (mid * Vec3::Z - Vec3::NEG_Z).length() < 1e-3,
            "{mid:?} {:?}",
            mid * Vec3::Z
        );
        assert_eq!(
            p0.flags,
            PLATE_CURRENT_VALID | PLATE_SOURCE0_VALID | PLATE_SOURCE1_VALID
        );
        // Plate 1 has no circuit at 10 Ma: current (5 Ma, between a valid and a missing sample)
        // and the 10 Ma source are invalid, the 0 Ma source is valid.
        assert_eq!(data[1].flags, PLATE_SOURCE0_VALID);
    }

    #[test]
    fn plate_buffer_carries_the_tie_bias_by_source_id() {
        let c = crust(vec![]);
        let data = plate_gpu_data(&c, &sampling(0.0, [0.0, 0.0]));
        // Plate 1 (id 100) ranks first and sits higher.
        assert!(data[1].tie_bias_km > data[0].tie_bias_km);
        assert_eq!(data[0].tie_bias_km, 0.0);
    }

    #[test]
    fn list_flags_match_footprint_acceptance_at_f64_boundaries() {
        // 305.01 has no exact f32; an f32 comparison on the GPU could flip at the boundary.
        let boundary = 305.01;
        let below = f64::from(boundary as f32).min(boundary) - 1e-9;
        let c = crust(vec![
            entry(0, 0.0, boundary),
            entry(0, 400.0, 540.0),
            entry(1, boundary, 540.0),
        ]);
        let pieces = build_plate_pieces(c.footprints(), &[1, 0], 1);
        let x = timefly_core::geo::RenderDir(texel_center_dir(CubeFace::PosX, 0, 0, 1).0);
        for ages in [
            [boundary, boundary + 1e-9, below],
            [boundary - 1e-12, 350.0, 400.0],
            [0.0, 540.0, 540.0 + 1e-9],
        ] {
            let flags = list_flags(&pieces.lists, ages);
            for (list, &f) in pieces.lists.iter().zip(&flags) {
                for (bit, &age) in [LIST_VALID_NOW, LIST_VALID_SOURCE0, LIST_VALID_SOURCE1]
                    .iter()
                    .zip(&ages)
                {
                    assert_eq!(
                        f & bit != 0,
                        c.footprints().contains(list.plate, x, age),
                        "plate {:?} at {age}",
                        list.plate
                    );
                }
            }
        }
    }

    #[test]
    fn piece_visibility_and_endpoints_mirror_the_core_rules() {
        use timefly_core::globe::{endpoint_available, piece_visible};
        for plate_flags in 0..8u32 {
            for list in 0..8u32 {
                assert_eq!(
                    gpu_piece_visible(plate_flags, list),
                    piece_visible(
                        plate_flags & PLATE_CURRENT_VALID != 0,
                        list & LIST_VALID_NOW != 0
                    )
                );
                for e in 0..2 {
                    let (pb, lb) = [
                        (PLATE_SOURCE0_VALID, LIST_VALID_SOURCE0),
                        (PLATE_SOURCE1_VALID, LIST_VALID_SOURCE1),
                    ][e];
                    assert_eq!(
                        gpu_endpoint_available(plate_flags, list, e),
                        endpoint_available(plate_flags & pb != 0, list & lb != 0)
                    );
                }
            }
        }
    }

    #[test]
    fn deep_params_pack_endpoints_and_sea_level() {
        let mut s = sampling(0.02, [0.0, 5.0]);
        s.endpoints[0].sea_level_anomaly_m = -112.4;
        s.older_weight = 0.25;
        let p = GlobeParams::deep(&s, 128, Vec3::Y);
        assert_eq!(p.endpoint, Vec4::new(-112.4, 0.0, 0.25, 128.0));
        assert_eq!(p.sun_dir.truncate(), Vec3::Y);
        assert_eq!(p.shape.x, EARTH_RADIUS_KM as f32);
    }
}
