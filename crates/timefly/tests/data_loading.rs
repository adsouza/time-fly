//! CPU-only asset tests for `GlobeDataPlugin` against the committed tiny bake
//! (`tests/assets/data/globe`, made by `timefly-bake --cube-size 8` from the bake tool's
//! `raw_tiny` fixture). No window, no renderer.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy::asset::AssetMetaCheck;
use bevy::prelude::*;
use bevy::render::render_resource::{TextureFormat, TextureViewDimension};
use bevy::state::app::StatesPlugin;
use timefly::data::{GlobeData, GlobeDataPlugin, GlobeLoadError, GlobeLoadState};
use timefly_core::deep_time::slider_to_years;
use timefly_core::geodynamics::PlateIndex;
use timefly_core::surface::{FrameSelection, FrameSource};

/// Asynchronous loading must settle within this.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

fn tiny_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets")
}

fn test_app(asset_root: &Path) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        AssetPlugin {
            file_path: asset_root.to_string_lossy().into_owned(),
            meta_check: AssetMetaCheck::Never,
            ..default()
        },
        ImagePlugin::default(),
        StatesPlugin,
        GlobeDataPlugin::default(),
    ));
    app
}

/// Updates until the load leaves `Loading`, or panics after [`LOAD_TIMEOUT`].
fn settle(app: &mut App) -> GlobeLoadState {
    let deadline = Instant::now() + LOAD_TIMEOUT;
    loop {
        app.update();
        let state = *app.world().resource::<State<GlobeLoadState>>().get();
        if state != GlobeLoadState::Loading {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "globe data still loading after {LOAD_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn load(asset_root: &Path) -> App {
    let mut app = test_app(asset_root);
    let state = settle(&mut app);
    if state != GlobeLoadState::Ready {
        let error = app
            .world()
            .get_resource::<GlobeLoadError>()
            .map(|e| e.0.clone());
        panic!("expected Ready, got {state:?}: {error:?}");
    }
    app
}

/// Asserts the load fails and returns the error message.
fn load_failure(asset_root: &Path) -> String {
    let mut app = test_app(asset_root);
    assert_eq!(settle(&mut app), GlobeLoadState::Failed);
    assert!(app.world().get_resource::<GlobeData>().is_none());
    app.world()
        .get_resource::<GlobeLoadError>()
        .expect("a failed load records its error")
        .0
        .clone()
}

/// A copy of the tiny bake in a fresh asset root, to break on purpose.
fn tiny_copy(test: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("data_loading-{test}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    copy_dir(&tiny_root(), &root);
    root
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn edit_manifest(root: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = root.join("data/globe/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut manifest);
    fs::write(path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
}

/// A valid `.tfplates` with one entry, for `plate_index`, in one cell.
fn tfplates_with_one_entry(cube_size: u32, plate_index: u32) -> Vec<u8> {
    let cells = 6 * (cube_size * cube_size) as usize;
    let mut raw = Vec::new();
    for i in 0..=cells {
        raw.extend_from_slice(&u32::from(i > 0).to_le_bytes());
    }
    raw.extend_from_slice(&plate_index.to_le_bytes());
    raw.extend_from_slice(&0.0f64.to_le_bytes());
    raw.extend_from_slice(&540.0f64.to_le_bytes());
    let z =
        ruzstd::encoding::compress_to_vec(&raw[..], ruzstd::encoding::CompressionLevel::Fastest);
    let mut out = b"TFPLAT01".to_vec();
    out.extend_from_slice(&cube_size.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(z.len() as u32).to_le_bytes());
    out.extend_from_slice(&z);
    out
}

#[test]
fn loads_tiny_bake_to_ready() {
    let app = load(&tiny_root());
    let data = app.world().resource::<GlobeData>();
    let ages = data.timeline.ages_myr();
    assert_eq!(ages, [0.0, 0.000076, 0.03, 540.0]);
    assert_eq!(data.timeline.bridge_myr(), (0.000076, 0.03));
    assert_eq!(data.images.len(), 3);
    assert_eq!(data.frame_image(0), data.frame_image(1));
    assert_eq!(data.cube_size, 8);
    assert!(!data.attribution.is_empty());
    // The revision names the manifest file's SHA-256, as `shasum -a 256` prints it.
    let manifest = fs::read(tiny_root().join("data/globe/manifest.json")).unwrap();
    assert_eq!(data.revision.manifest_version, 2);
    assert_eq!(
        data.revision.manifest_sha256,
        timefly_core::sha256::sha256_hex(&manifest)
    );

    let images = app.world().resource::<Assets<Image>>();
    for (frame, meta) in data.timeline.frames().iter().enumerate() {
        let image = images
            .get(data.frame_image(frame))
            .expect("frame image loaded");
        let d = &image.texture_descriptor;
        assert_eq!(
            (d.size.width, d.size.height, d.size.depth_or_array_layers),
            (8, 8, 6)
        );
        let expected = match meta.source {
            FrameSource::Ice6g => TextureFormat::Rgba16Float,
            FrameSource::PaleoDem { .. } => TextureFormat::Rg16Float,
        };
        assert_eq!(d.format, expected, "frame {frame}");
        assert_eq!(
            image.texture_view_descriptor.as_ref().unwrap().dimension,
            Some(TextureViewDimension::Cube)
        );
    }

    let crust = &data.crust;
    assert_eq!(crust.footprints().cube_size, data.cube_size);
    assert_eq!(crust.rotations().plate_ids.len(), 2);
    assert!(!crust.footprints().entries.is_empty());
    for plate in 0..2 {
        for age in [0.0, 540.0] {
            assert!(
                crust
                    .rotations()
                    .rotation_at(PlateIndex(plate), age)
                    .is_some()
            );
        }
    }
}

#[test]
fn failed_load_enters_failed_state() {
    let root = tiny_copy("missing");
    edit_manifest(&root, |m| {
        m["frames"][3]["file"] = "frames/missing.tfgrid".into()
    });
    let error = load_failure(&root);
    assert_eq!(error, "data/globe/frames/missing.tfgrid: file not found");
}

#[test]
fn missing_manifest_fails() {
    let root = tiny_copy("no-manifest");
    fs::remove_file(root.join("data/globe/manifest.json")).unwrap();
    let error = load_failure(&root);
    assert_eq!(error, "data/globe/manifest.json: file not found");
}

#[test]
fn missing_rotations_fail_without_the_file_path() {
    let root = tiny_copy("no-rotations");
    fs::remove_file(root.join("data/globe/rotations.tfrot")).unwrap();
    let error = load_failure(&root);
    assert!(
        error.ends_with("could not read data/globe/rotations.tfrot: file not found"),
        "{error}"
    );
    assert!(!error.contains(env!("CARGO_TARGET_TMPDIR")), "{error}");
}

#[test]
fn footprint_outside_rotation_table_fails() {
    let root = tiny_copy("footprint");
    fs::write(
        root.join("data/globe/plates.tfplates"),
        tfplates_with_one_entry(8, 7),
    )
    .unwrap();
    let error = load_failure(&root);
    assert!(
        error.contains("plate index 7 is not in the rotation table"),
        "{error}"
    );
}

#[test]
fn mismatched_image_format_fails() {
    // The ICE-6G_C frames point at the two-channel PaleoDEM image.
    let root = tiny_copy("format");
    edit_manifest(&root, |m| {
        for i in 0..2 {
            m["frames"][i]["file"] = "frames/f0001.tfgrid".into();
        }
    });
    let error = load_failure(&root);
    assert!(error.contains("does not match its source"), "{error}");
}

#[test]
fn mismatched_cube_size_fails() {
    let root = tiny_copy("cube-size");
    edit_manifest(&root, |m| m["cube_size"] = 16.into());
    let error = load_failure(&root);
    assert!(
        error.contains("cube size 8, but the manifest says 16"),
        "{error}"
    );
}

#[test]
fn scrubbing_reuses_preloaded_images() {
    let mut app = load(&tiny_root());
    let preloaded: HashSet<AssetId<Image>> = app
        .world()
        .resource::<GlobeData>()
        .images
        .iter()
        .map(Handle::id)
        .collect();
    let image_count = app.world().resource::<Assets<Image>>().len();
    let (mut recent, mut bridge, mut deep) = (0, 0, 0);

    for i in 0..50 {
        let age_myr = slider_to_years(f64::from(i) / 49.0).myr();
        let world = app.world();
        let data = world.resource::<GlobeData>();
        let selection = data.timeline.select_frames(age_myr);
        match selection {
            FrameSelection::Recent(_) => recent += 1,
            FrameSelection::Bridge { .. } => bridge += 1,
            FrameSelection::Deep(_) => deep += 1,
        }
        for frame in selection.frame_indices() {
            let handle = data.frame_image(frame);
            assert!(
                preloaded.contains(&handle.id()),
                "frame {frame} at {age_myr} Myr"
            );
            assert!(world.resource::<Assets<Image>>().contains(handle));
        }
        app.update();
    }

    assert!(
        recent > 0 && bridge > 0 && deep > 0,
        "{recent} {bridge} {deep}"
    );
    assert_eq!(app.world().resource::<Assets<Image>>().len(), image_count);
}
