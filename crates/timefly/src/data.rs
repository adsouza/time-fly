//! Globe data: loads manifest v2, the cube frames, plate footprints and rotations; builds the
//! validated [`Timeline`] and [`CrustModel`]; and maps timeline image slots to image handles.
//!
//! The manifest loader does the CPU work off the main thread: it parses the manifest, reads the
//! footprint and rotation files, builds both aggregates through their validating constructors and
//! starts one image load per unique frame file. `poll_globe_load` waits for the images, checks
//! their formats against the frame sources and enters [`GlobeLoadState::Ready`] or
//! [`GlobeLoadState::Failed`].

use std::fmt;

use bevy::asset::io::{AssetReaderError, Reader};
use bevy::asset::{
    AssetLoadError, AssetLoader, AssetPath, LoadContext, LoadState, ReadAssetBytesError,
    RecursiveDependencyLoadState, RenderAssetUsages,
};
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDimension, TextureFormat, TextureViewDescriptor, TextureViewDimension,
};
use serde::Deserialize;
use timefly_core::formats::{
    DType, FormatError, Grid, GridKind, read_tfgrid, read_tfplates, read_tfrot,
};
use timefly_core::geodynamics::{CrustModel, CrustModelError};
use timefly_core::surface::{FrameMeta, FrameSource, Timeline, TimelineError};
use tracing::instrument;

use crate::diagnostics::DiagnosticsContext;
use crate::diagnostics::aspects::DiagnosticsAppExt;

/// The production manifest, relative to the asset root.
pub const MANIFEST_PATH: &str = "data/globe/manifest.json";
pub const MANIFEST_VERSION: u32 = 2;

/// Whether the globe's CPU data is loaded. `Ready` lets the render plugins build the globe.
#[derive(States, Copy, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub enum GlobeLoadState {
    #[default]
    Loading,
    Ready,
    Failed,
}

/// The loaded globe data, inserted on entering [`GlobeLoadState::Ready`].
#[derive(Resource)]
pub struct GlobeData {
    pub cube_size: u32,
    pub timeline: Timeline,
    /// One handle per unique frame file, indexed by [`FrameMeta::image_slot`].
    pub images: Vec<Handle<Image>>,
    pub crust: CrustModel,
    pub attribution: Vec<String>,
}

impl GlobeData {
    /// The image of timeline frame `frame`.
    pub fn frame_image(&self, frame: usize) -> &Handle<Image> {
        &self.images[self.timeline.frames()[frame].image_slot as usize]
    }
}

/// Why the globe data failed to load, for the "data failed to load" message.
#[derive(Resource, Clone, Debug)]
pub struct GlobeLoadError(pub String);

/// Everything that can go wrong loading the globe data.
#[derive(Debug)]
pub enum GlobeDataError {
    Io(String),
    Manifest(String),
    Version {
        got: u32,
    },
    MissingSourceRotationAge {
        frame: usize,
    },
    Read {
        path: String,
        error: String,
    },
    Format {
        path: String,
        error: FormatError,
    },
    Crust(CrustModelError),
    Timeline(TimelineError),
    CubeSize {
        what: String,
        expected: u32,
        got: u32,
    },
    Grid(String),
    ImageFormat {
        frame: usize,
        expected: TextureFormat,
        got: TextureFormat,
    },
    ImageMissing {
        slot: usize,
    },
}

impl fmt::Display for GlobeDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GlobeDataError::Io(e) => write!(f, "could not read: {e}"),
            GlobeDataError::Manifest(e) => write!(f, "invalid manifest: {e}"),
            GlobeDataError::Version { got } => write!(
                f,
                "manifest version {got} is not supported (expected {MANIFEST_VERSION})"
            ),
            GlobeDataError::MissingSourceRotationAge { frame } => {
                write!(f, "paleodem frame {frame} has no source_rotation_age_myr")
            }
            GlobeDataError::Read { path, error } => write!(f, "could not read {path}: {error}"),
            GlobeDataError::Format { path, error } => write!(f, "{path}: {error}"),
            GlobeDataError::Crust(e) => write!(f, "{e}"),
            GlobeDataError::Timeline(e) => write!(f, "invalid timeline: {e}"),
            GlobeDataError::CubeSize {
                what,
                expected,
                got,
            } => write!(
                f,
                "{what} has cube size {got}, but the manifest says {expected}"
            ),
            GlobeDataError::Grid(why) => write!(f, "unsupported frame grid: {why}"),
            GlobeDataError::ImageFormat {
                frame,
                expected,
                got,
            } => write!(
                f,
                "frame {frame}: image format {got:?} does not match its source (expected {expected:?})"
            ),
            GlobeDataError::ImageMissing { slot } => {
                write!(f, "image slot {slot} is not in the image assets")
            }
        }
    }
}

impl std::error::Error for GlobeDataError {}

#[derive(Deserialize)]
struct ManifestFile {
    version: u32,
    cube_size: u32,
    frames: Vec<ManifestFrame>,
    footprints: String,
    rotations: String,
    sea_level_curve: Vec<(f64, f64)>,
    #[serde(default)]
    attribution: Vec<String>,
}

#[derive(Deserialize)]
struct ManifestFrame {
    age_myr: f64,
    file: String,
    source: SourceName,
    #[serde(default)]
    source_rotation_age_myr: Option<f64>,
}

#[derive(Deserialize, Copy, Clone, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum SourceName {
    Ice6g,
    Paleodem,
}

/// Frame metadata with image slots assigned by first occurrence of each file, and the unique
/// files in slot order. Repeated files share one slot.
fn frames_and_files(
    frames: &[ManifestFrame],
) -> Result<(Vec<FrameMeta>, Vec<String>), GlobeDataError> {
    let mut files: Vec<String> = Vec::new();
    let mut metas = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        let slot = match files.iter().position(|f| *f == frame.file) {
            Some(slot) => slot,
            None => {
                files.push(frame.file.clone());
                files.len() - 1
            }
        };
        let source = match frame.source {
            SourceName::Ice6g => FrameSource::Ice6g,
            SourceName::Paleodem => FrameSource::PaleoDem {
                source_rotation_age_myr: frame
                    .source_rotation_age_myr
                    .ok_or(GlobeDataError::MissingSourceRotationAge { frame: index })?,
            },
        };
        metas.push(FrameMeta {
            age_myr: frame.age_myr,
            source,
            image_slot: slot as u32,
        });
    }
    Ok((metas, files))
}

/// The manifest, with its aggregates validated and its frame images loading.
#[derive(Asset, TypePath)]
pub struct GlobeManifest {
    pub cube_size: u32,
    pub timeline: Timeline,
    pub crust: CrustModel,
    #[dependency]
    pub images: Vec<Handle<Image>>,
    pub attribution: Vec<String>,
}

#[derive(Default, TypePath)]
pub struct GlobeManifestLoader;

impl AssetLoader for GlobeManifestLoader {
    type Asset = GlobeManifest;
    type Settings = ();
    type Error = GlobeDataError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<GlobeManifest, GlobeDataError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| GlobeDataError::Io(e.to_string()))?;
        load_manifest(&bytes, load_context).await
    }

    fn extensions(&self) -> &[&str] {
        &["json"]
    }
}

/// A read error without the absolute file path some variants carry (spec §20: no personal
/// data in logs); the caller names the asset path.
fn describe_read_error(e: &ReadAssetBytesError) -> String {
    match e {
        ReadAssetBytesError::AssetReaderError(e) => describe_reader_error(e),
        ReadAssetBytesError::Io { source, .. } => source.to_string(),
        other => other.to_string(),
    }
}

fn describe_reader_error(e: &AssetReaderError) -> String {
    match e {
        AssetReaderError::NotFound(_) => "file not found".to_string(),
        other => other.to_string(),
    }
}

/// A load error for the message and log, naming the asset path instead of the file path.
pub fn describe_load_error(path: &str, e: &AssetLoadError) -> String {
    match e {
        AssetLoadError::AssetReaderError(e) => format!("{path}: {}", describe_reader_error(e)),
        other => other.to_string(),
    }
}

async fn read_sibling(
    load_context: &mut LoadContext<'_>,
    file: &str,
) -> Result<(String, Vec<u8>), GlobeDataError> {
    let path = load_context.path().resolve_embed(&AssetPath::parse(file));
    let bytes = load_context
        .read_asset_bytes(path.clone())
        .await
        .map_err(|e| GlobeDataError::Read {
            path: path.to_string(),
            error: describe_read_error(&e),
        })?;
    Ok((path.to_string(), bytes))
}

#[instrument(level = "debug", skip_all, fields(path = %load_context.path(), len = bytes.len()), err)]
async fn load_manifest(
    bytes: &[u8],
    load_context: &mut LoadContext<'_>,
) -> Result<GlobeManifest, GlobeDataError> {
    let manifest: ManifestFile =
        serde_json::from_slice(bytes).map_err(|e| GlobeDataError::Manifest(e.to_string()))?;
    if manifest.version != MANIFEST_VERSION {
        return Err(GlobeDataError::Version {
            got: manifest.version,
        });
    }
    let (frames, files) = frames_and_files(&manifest.frames)?;

    let (path, bytes) = read_sibling(load_context, &manifest.footprints).await?;
    let footprints = read_tfplates(&bytes).map_err(|error| GlobeDataError::Format {
        path: path.clone(),
        error,
    })?;
    if footprints.cube_size != manifest.cube_size {
        return Err(GlobeDataError::CubeSize {
            what: path,
            expected: manifest.cube_size,
            got: footprints.cube_size,
        });
    }
    let (path, bytes) = read_sibling(load_context, &manifest.rotations).await?;
    let rotations = read_tfrot(&bytes).map_err(|error| GlobeDataError::Format { path, error })?;
    let crust = CrustModel::new(rotations, footprints).map_err(GlobeDataError::Crust)?;

    let timeline =
        Timeline::new(frames, manifest.sea_level_curve).map_err(GlobeDataError::Timeline)?;
    timeline
        .check_rotation_coverage(&crust.rotations().times_myr)
        .map_err(GlobeDataError::Timeline)?;

    // Started only once the CPU data validates; repeated files already share a slot.
    let images = files
        .iter()
        .map(|file| {
            let path = load_context.path().resolve_embed(&AssetPath::parse(file));
            load_context.load::<Image>(path)
        })
        .collect();
    Ok(GlobeManifest {
        cube_size: manifest.cube_size,
        timeline,
        crust,
        images,
        attribution: manifest.attribution,
    })
}

/// Loads a cube `.tfgrid` as a cube-map [`Image`].
#[derive(Default, TypePath)]
pub struct TfgridLoader;

impl AssetLoader for TfgridLoader {
    type Asset = Image;
    type Settings = ();
    type Error = GlobeDataError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        load_context: &mut LoadContext<'_>,
    ) -> Result<Image, GlobeDataError> {
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| GlobeDataError::Io(e.to_string()))?;
        decode_tfgrid(&bytes, load_context.path())
    }

    fn extensions(&self) -> &[&str] {
        &["tfgrid"]
    }
}

#[instrument(level = "debug", skip_all, fields(path = %path, len = bytes.len()), err)]
fn decode_tfgrid(bytes: &[u8], path: &AssetPath<'_>) -> Result<Image, GlobeDataError> {
    let grid = read_tfgrid(bytes).map_err(|error| GlobeDataError::Format {
        path: path.to_string(),
        error,
    })?;
    grid_to_image(grid)
}

/// The texture format of a frame grid: two f16 channels (PaleoDEM) → `Rg16Float`, four
/// (ICE-6G_C) → `Rgba16Float`. Both are filterable WebGPU baseline formats.
pub fn grid_texture_format(grid: &Grid) -> Result<TextureFormat, GlobeDataError> {
    if grid.kind != GridKind::Cube {
        return Err(GlobeDataError::Grid("frames must be cube grids".into()));
    }
    if grid.dtype != DType::F16 {
        return Err(GlobeDataError::Grid(format!(
            "frames must be f16, got {:?}",
            grid.dtype
        )));
    }
    match grid.channels {
        2 => Ok(TextureFormat::Rg16Float),
        4 => Ok(TextureFormat::Rgba16Float),
        n => Err(GlobeDataError::Grid(format!(
            "frames must have 2 or 4 channels, got {n}"
        ))),
    }
}

/// A cube grid as a six-layer cube-map image. The `[face][row][col][channel]` layout is wgpu's
/// layer-major order, with faces in cube-layer order (+X, −X, +Y, −Y, +Z, −Z). Only the render
/// world keeps the image, so Bevy drops the CPU bytes once they are uploaded.
pub fn grid_to_image(grid: Grid) -> Result<Image, GlobeDataError> {
    let format = grid_texture_format(&grid)?;
    let mut image = Image::new(
        Extent3d {
            width: grid.width,
            height: grid.height,
            depth_or_array_layers: 6,
        },
        TextureDimension::D2,
        grid.data,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_view_descriptor = Some(TextureViewDescriptor {
        dimension: Some(TextureViewDimension::Cube),
        ..default()
    });
    image.sampler = ImageSampler::linear();
    Ok(image)
}

/// What [`check_frame_images`] needs to know about a loaded image.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ImageInfo {
    pub format: TextureFormat,
    pub size: u32,
    pub layers: u32,
}

impl ImageInfo {
    fn of(image: &Image) -> ImageInfo {
        let size = image.texture_descriptor.size;
        ImageInfo {
            format: image.texture_descriptor.format,
            size: if size.width == size.height {
                size.width
            } else {
                0
            },
            layers: size.depth_or_array_layers,
        }
    }
}

/// Checks every frame's image against its source format and the manifest's cube size.
/// `images[slot]` is `None` when the slot's image is not in the image assets.
pub fn check_frame_images(
    timeline: &Timeline,
    cube_size: u32,
    images: &[Option<ImageInfo>],
) -> Result<(), GlobeDataError> {
    for (slot, info) in images.iter().enumerate() {
        let info = info.ok_or(GlobeDataError::ImageMissing { slot })?;
        if info.size != cube_size || info.layers != 6 {
            return Err(GlobeDataError::CubeSize {
                what: format!("image slot {slot}"),
                expected: cube_size,
                got: info.size,
            });
        }
    }
    for (frame, meta) in timeline.frames().iter().enumerate() {
        let expected = match meta.source {
            FrameSource::Ice6g => TextureFormat::Rgba16Float,
            FrameSource::PaleoDem { .. } => TextureFormat::Rg16Float,
        };
        let slot = meta.image_slot as usize;
        let got = images
            .get(slot)
            .copied()
            .flatten()
            .ok_or(GlobeDataError::ImageMissing { slot })?
            .format;
        if got != expected {
            return Err(GlobeDataError::ImageFormat {
                frame,
                expected,
                got,
            });
        }
    }
    Ok(())
}

/// Where the progress of a load stands, from the manifest's load states.
#[derive(Clone, Debug)]
pub enum LoadProgress {
    Waiting,
    ManifestFailed(std::sync::Arc<AssetLoadError>),
    /// A frame image failed; which one is looked up from the images' own states.
    ImageFailed,
    Loaded,
}

/// Decides the load progress from the manifest's own and recursive dependency load states.
pub fn load_progress(own: &LoadState, deps: &RecursiveDependencyLoadState) -> LoadProgress {
    match (own, deps) {
        (LoadState::Failed(e), _) => LoadProgress::ManifestFailed(e.clone()),
        (_, RecursiveDependencyLoadState::Failed(_)) => LoadProgress::ImageFailed,
        (LoadState::Loaded, RecursiveDependencyLoadState::Loaded) => LoadProgress::Loaded,
        _ => LoadProgress::Waiting,
    }
}

/// The manifest being loaded.
#[derive(Resource)]
struct GlobeLoad {
    manifest: Handle<GlobeManifest>,
}

#[derive(Resource)]
struct GlobeManifestPath(String);

/// Loads the globe data and drives [`GlobeLoadState`].
pub struct GlobeDataPlugin {
    /// The manifest, relative to the asset root.
    pub manifest_path: String,
}

impl Default for GlobeDataPlugin {
    fn default() -> Self {
        GlobeDataPlugin {
            manifest_path: MANIFEST_PATH.to_string(),
        }
    }
}

impl Plugin for GlobeDataPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<GlobeManifest>()
            .init_asset_loader::<GlobeManifestLoader>()
            .init_asset_loader::<TfgridLoader>()
            .add_logged_state::<GlobeLoadState>()
            .insert_resource(GlobeManifestPath(self.manifest_path.clone()))
            .add_systems(Startup, start_globe_load)
            .add_systems(
                Update,
                poll_globe_load.run_if(in_state(GlobeLoadState::Loading)),
            );
    }
}

fn start_globe_load(
    mut commands: Commands,
    server: Res<AssetServer>,
    path: Res<GlobeManifestPath>,
    context: Option<ResMut<DiagnosticsContext>>,
) {
    commands.insert_resource(GlobeLoad {
        manifest: server.load(path.0.clone()),
    });
    if let Some(mut context) = context {
        context.load_state = Some("Loading".into());
    }
}

/// Gathers the manifest's load states, decides with [`load_progress`] and
/// [`check_frame_images`], and applies the result.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn poll_globe_load(
    mut commands: Commands,
    load: Option<Res<GlobeLoad>>,
    path: Res<GlobeManifestPath>,
    server: Res<AssetServer>,
    mut manifests: ResMut<Assets<GlobeManifest>>,
    images: Res<Assets<Image>>,
    mut next: ResMut<NextState<GlobeLoadState>>,
    context: Option<ResMut<DiagnosticsContext>>,
) {
    let Some(load) = load else { return };
    let Some((own, _, deps)) = server.get_load_states(&load.manifest) else {
        return;
    };
    let outcome = match load_progress(&own, &deps) {
        LoadProgress::Waiting => return,
        LoadProgress::ManifestFailed(e) => Err(describe_load_error(&path.0, &e)),
        LoadProgress::ImageFailed => Err(manifests
            .get(&load.manifest)
            .and_then(|manifest| {
                manifest
                    .images
                    .iter()
                    .find_map(|h| match server.get_load_state(h) {
                        Some(LoadState::Failed(e)) => {
                            let image_path = server.get_path(h).map(|p| p.to_string());
                            Some(describe_load_error(
                                image_path.as_deref().unwrap_or("?"),
                                &e,
                            ))
                        }
                        _ => None,
                    })
            })
            .unwrap_or_else(|| "a frame image failed to load".to_string())),
        LoadProgress::Loaded => match manifests.get(&load.manifest) {
            None => Err("the manifest asset is missing after loading".to_string()),
            Some(manifest) => {
                let infos: Vec<Option<ImageInfo>> = manifest
                    .images
                    .iter()
                    .map(|h| images.get(h).map(ImageInfo::of))
                    .collect();
                check_frame_images(&manifest.timeline, manifest.cube_size, &infos)
                    .map_err(|e| e.to_string())
            }
        },
    };

    let state = match outcome {
        Ok(()) => {
            let manifest = manifests
                .remove(&load.manifest)
                .expect("checked present above");
            info!(
                frames = manifest.timeline.frames().len(),
                images = manifest.images.len(),
                plates = manifest.crust.rotations().plate_ids.len(),
                cube_size = manifest.cube_size,
                "globe data loaded"
            );
            commands.insert_resource(GlobeData {
                cube_size: manifest.cube_size,
                timeline: manifest.timeline,
                images: manifest.images,
                crust: manifest.crust,
                attribution: manifest.attribution,
            });
            GlobeLoadState::Ready
        }
        Err(error) => {
            // `BevyError` (loader errors) ends its message with a newline.
            let error = error.trim_end().to_string();
            error!(%error, "globe data failed to load");
            commands.insert_resource(GlobeLoadError(error));
            GlobeLoadState::Failed
        }
    };
    commands.remove_resource::<GlobeLoad>();
    next.set(state);
    if let Some(mut context) = context {
        context.load_state = Some(format!("{state:?}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(age_myr: f64, file: &str, source: SourceName, rot: Option<f64>) -> ManifestFrame {
        ManifestFrame {
            age_myr,
            file: file.into(),
            source,
            source_rotation_age_myr: rot,
        }
    }

    #[test]
    fn repeated_files_share_one_image_slot() {
        let frames = [
            frame(0.0, "frames/f0000.tfgrid", SourceName::Ice6g, None),
            frame(0.000076, "frames/f0000.tfgrid", SourceName::Ice6g, None),
            frame(0.03, "frames/f0001.tfgrid", SourceName::Paleodem, Some(0.0)),
            frame(
                540.0,
                "frames/f0002.tfgrid",
                SourceName::Paleodem,
                Some(540.0),
            ),
        ];
        let (metas, files) = frames_and_files(&frames).unwrap();
        assert_eq!(
            files,
            [
                "frames/f0000.tfgrid",
                "frames/f0001.tfgrid",
                "frames/f0002.tfgrid"
            ]
        );
        let slots: Vec<u32> = metas.iter().map(|m| m.image_slot).collect();
        assert_eq!(slots, [0, 0, 1, 2]);
        assert_eq!(
            metas[3].source,
            FrameSource::PaleoDem {
                source_rotation_age_myr: 540.0
            }
        );
    }

    #[test]
    fn paleodem_frames_need_a_source_rotation_age() {
        let frames = [frame(0.03, "f.tfgrid", SourceName::Paleodem, None)];
        assert!(matches!(
            frames_and_files(&frames),
            Err(GlobeDataError::MissingSourceRotationAge { frame: 0 })
        ));
    }

    #[test]
    fn manifest_source_names_parse() {
        let f: ManifestFrame = serde_json::from_str(
            r#"{"age_myr": 0.03, "file": "a.tfgrid", "source": "paleodem", "source_rotation_age_myr": 0.0}"#,
        )
        .unwrap();
        assert_eq!(f.source, SourceName::Paleodem);
        let f: ManifestFrame =
            serde_json::from_str(r#"{"age_myr": 0.0, "file": "a.tfgrid", "source": "ice6g"}"#)
                .unwrap();
        assert_eq!(f.source, SourceName::Ice6g);
    }

    fn grid(kind: GridKind, dtype: DType, channels: u8, n: u32) -> Grid {
        let faces = if kind == GridKind::Cube { 6 } else { 1 };
        let width = if kind == GridKind::Cube { n } else { 2 * n };
        let bytes = faces * (n * width) as usize * usize::from(channels) * 2;
        Grid {
            kind,
            dtype,
            channels,
            width,
            height: n,
            data: vec![0; bytes],
        }
    }

    #[test]
    fn grids_map_to_cube_images() {
        for (channels, format) in [
            (2, TextureFormat::Rg16Float),
            (4, TextureFormat::Rgba16Float),
        ] {
            let image = grid_to_image(grid(GridKind::Cube, DType::F16, channels, 8)).unwrap();
            let d = &image.texture_descriptor;
            assert_eq!(d.format, format);
            assert_eq!(d.dimension, TextureDimension::D2);
            assert_eq!(
                d.size,
                Extent3d {
                    width: 8,
                    height: 8,
                    depth_or_array_layers: 6
                }
            );
            assert_eq!(
                image.texture_view_descriptor.as_ref().unwrap().dimension,
                Some(TextureViewDimension::Cube)
            );
            assert_eq!(image.asset_usage, RenderAssetUsages::RENDER_WORLD);
            assert_eq!(
                image.data.as_ref().unwrap().len(),
                6 * 8 * 8 * usize::from(channels) * 2
            );
        }
    }

    #[test]
    fn unsupported_grids_are_rejected() {
        for g in [
            grid(GridKind::LatLon, DType::F16, 2, 4),
            grid(GridKind::Cube, DType::U16, 2, 4),
            grid(GridKind::Cube, DType::F16, 3, 4),
        ] {
            assert!(matches!(grid_to_image(g), Err(GlobeDataError::Grid(_))));
        }
    }

    fn tiny_timeline() -> Timeline {
        let ice = |age_myr| FrameMeta {
            age_myr,
            source: FrameSource::Ice6g,
            image_slot: 0,
        };
        let paleo = |age_myr, source_rotation_age_myr, image_slot| FrameMeta {
            age_myr,
            source: FrameSource::PaleoDem {
                source_rotation_age_myr,
            },
            image_slot,
        };
        Timeline::new(
            vec![
                ice(0.0),
                ice(0.000076),
                paleo(0.03, 0.0, 1),
                paleo(540.0, 540.0, 2),
            ],
            vec![(0.000076, 0.5)],
        )
        .unwrap()
    }

    #[test]
    fn load_progress_and_errors_name_the_asset_not_the_file() {
        use std::path::PathBuf;
        use std::sync::Arc;
        let not_found = Arc::new(AssetLoadError::AssetReaderError(
            AssetReaderError::NotFound(PathBuf::from(
                "/home/someone/game/assets/data/globe/manifest.json",
            )),
        ));
        let loaded = RecursiveDependencyLoadState::Loaded;
        assert!(matches!(
            load_progress(&LoadState::Loading, &RecursiveDependencyLoadState::Loading),
            LoadProgress::Waiting
        ));
        assert!(matches!(
            load_progress(&LoadState::Loaded, &loaded),
            LoadProgress::Loaded
        ));
        assert!(matches!(
            load_progress(&LoadState::Failed(not_found.clone()), &loaded),
            LoadProgress::ManifestFailed(_)
        ));
        assert!(matches!(
            load_progress(
                &LoadState::Loaded,
                &RecursiveDependencyLoadState::Failed(not_found.clone())
            ),
            LoadProgress::ImageFailed
        ));
        assert_eq!(
            describe_load_error("data/globe/manifest.json", &not_found),
            "data/globe/manifest.json: file not found"
        );
    }

    #[test]
    fn frame_images_must_match_source_and_cube_size() {
        let t = tiny_timeline();
        let info = |format, size| {
            Some(ImageInfo {
                format,
                size,
                layers: 6,
            })
        };
        let good = [
            info(TextureFormat::Rgba16Float, 8),
            info(TextureFormat::Rg16Float, 8),
            info(TextureFormat::Rg16Float, 8),
        ];
        assert!(check_frame_images(&t, 8, &good).is_ok());

        let mut swapped = good;
        swapped[0] = info(TextureFormat::Rg16Float, 8);
        assert!(matches!(
            check_frame_images(&t, 8, &swapped),
            Err(GlobeDataError::ImageFormat { frame: 0, .. })
        ));
        assert!(matches!(
            check_frame_images(&t, 16, &good),
            Err(GlobeDataError::CubeSize { .. })
        ));
        let mut missing = good;
        missing[2] = None;
        assert!(matches!(
            check_frame_images(&t, 8, &missing),
            Err(GlobeDataError::ImageMissing { slot: 2 })
        ));
    }
}
