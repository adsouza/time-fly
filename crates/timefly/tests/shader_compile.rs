//! Every WESL shader under `assets/shaders` compiles and validates, on the CPU, through the
//! pipeline the app uses (Task 11c fix round 4: a parameter named `patch`, a WGSL reserved
//! word, failed every water pipeline at run time while the default `cargo test` passed, since
//! the GPU parity tests are `#[ignore]`d and the unit tests only match shader text).
//!
//! The path is Bevy's own: Bevy's shader libraries are loaded by its plugins in a headless app
//! (no GPU: the renderer has no backends), the app's shaders are added as the app adds them
//! (`Shader::from_wesl(source, "embedded://timefly/<file>")`), and Bevy's `ShaderCache` resolves
//! the imports and conditional compilation and composes each shader with `wesl`, exactly as its
//! pipeline cache does, handing the WGSL to a module loader that here parses and validates it
//! with naga instead of making a GPU module. The shader defs are the ones each material's
//! pipelines used in a real run (`fixtures/shader_defs.txt`); the shared libraries (`noise`,
//! `terrain_cdlod`, ...) are also compiled on their own, with no defs.

use std::borrow::Cow;
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::settings::WgpuSettings;
use bevy::shader::{
    Shader, ShaderCache, ShaderCacheError, ShaderCacheSource, ShaderDefVal, ValidateShader,
};
use bevy::window::ExitCondition;

/// Parses and validates composed WGSL with naga: the module loader the cache calls.
fn validate(
    _device: &(),
    source: ShaderCacheSource,
    _validate: &ValidateShader,
) -> Result<(), ShaderCacheError> {
    let ShaderCacheSource::Wgsl(wgsl) = source else {
        return Err(ShaderCacheError::ProcessShaderError("not WGSL".into()));
    };
    let module = naga::front::wgsl::parse_str(&wgsl)
        .map_err(|e| ShaderCacheError::ProcessShaderError(e.emit_to_string(&wgsl)))?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| ShaderCacheError::ProcessShaderError(e.emit_to_string(&wgsl)))?;
    Ok(())
}

/// Bevy's shaders, as its plugins load them, in a headless app without a GPU.
fn bevy_shaders() -> Vec<(AssetId<Shader>, Shader)> {
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .disable::<bevy::log::LogPlugin>()
            .set(RenderPlugin {
                render_creation: WgpuSettings {
                    backends: None,
                    ..default()
                }
                .into(),
                ..default()
            })
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                ..default()
            }),
    );
    app.finish();
    app.cleanup();
    // The embedded libraries load on the task pools: update until their count holds still.
    let (mut last, mut still) = (0, 0);
    for _ in 0..1_000 {
        app.update();
        let n = app.world().resource::<Assets<Shader>>().len();
        still = if n == last && n > 0 { still + 1 } else { 0 };
        last = n;
        if still >= 30 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let shaders = app.world().resource::<Assets<Shader>>();
    shaders.iter().map(|(id, s)| (id, s.clone())).collect()
}

/// One `ShaderDefVal` from its `Debug` form: `Bool("NAME", true)`, `Int("NAME", 4)` or
/// `UInt("NAME", 18)`.
fn parse_def(text: &str) -> ShaderDefVal {
    let (kind, rest) = text.split_once('(').expect("a def");
    let rest = rest.trim_end_matches(')');
    let (name, value) = rest.split_once(", ").expect("name, value");
    let name: Cow<'static, str> = name.trim_matches('"').to_string().into();
    match kind.trim() {
        "Bool" => ShaderDefVal::Bool(name, value == "true"),
        "Int" => ShaderDefVal::Int(name, value.parse().expect("an i32")),
        "UInt" => ShaderDefVal::UInt(name, value.parse().expect("a u32")),
        other => panic!("def kind {other}"),
    }
}

/// `fixtures/shader_defs.txt`: (shader file, its defs) per pipeline variant.
fn pipeline_defs() -> Vec<(String, Vec<ShaderDefVal>)> {
    include_str!("fixtures/shader_defs.txt")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let (file, defs) = l.split_once(' ').expect("file defs");
            let defs = defs.trim().trim_start_matches('[').trim_end_matches(']');
            // Split on the `), ` between defs (names and values hold no parentheses).
            let defs = if defs.is_empty() {
                Vec::new()
            } else {
                defs.split("), ")
                    .map(|d| parse_def(&format!("{})", d.trim_end_matches(')'))))
                    .collect()
            };
            (file.to_string(), defs)
        })
        .collect()
}

/// The app's shaders: every `.wesl` under `assets/shaders`, as (file name, source).
fn app_shader_files() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/shaders");
    let mut files: Vec<(String, String)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wesl"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            (name, std::fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort();
    files
}

/// Compiles each of `files` (name, source) with Bevy's shaders: its pipeline variants' defs,
/// and with none (the libraries, and every shader once). Returns each failure.
fn compile_all(
    bevy: &[(AssetId<Shader>, Shader)],
    files: &[(String, String)],
) -> (usize, Vec<String>) {
    let mut cache: ShaderCache<(), ()> = ShaderCache::new((), validate);
    for (id, shader) in bevy {
        cache.set_shader(*id, shader.clone());
    }
    let mut ids = Vec::new();
    for (name, source) in files {
        let id = AssetId::<Shader>::Uuid {
            uuid: bevy::asset::uuid::Uuid::from_u128(
                0x7469_6d65_666c_7900_0000_0000_0000_0000 + ids.len() as u128,
            ),
        };
        let shader = Shader::from_wesl(source.clone(), format!("embedded://timefly/{name}"));
        cache.set_shader(id, shader);
        ids.push((name.clone(), id));
    }
    let variants = pipeline_defs();
    let (mut compiled, mut failures) = (0, Vec::new());
    for (pipeline, (name, id)) in ids.iter().enumerate() {
        let mut runs: Vec<Vec<ShaderDefVal>> = variants
            .iter()
            .filter(|(f, _)| f == name)
            .map(|(_, d)| d.clone())
            .collect();
        let has_entry = files
            .iter()
            .any(|(f, s)| f == name && (s.contains("@vertex") || s.contains("@fragment")));
        if runs.is_empty() || !has_entry {
            runs.push(Vec::new());
        }
        for defs in runs {
            compiled += 1;
            if let Err(e) = cache.get(pipeline, *id, &defs) {
                failures.push(format!("{name} with {} defs: {e}", defs.len()));
            }
        }
    }
    (compiled, failures)
}

#[test]
fn every_wesl_shader_compiles_and_validates() {
    let bevy = bevy_shaders();
    assert!(
        bevy.len() > 50,
        "only {} of Bevy's shaders loaded",
        bevy.len()
    );
    let files = app_shader_files();
    assert!(files.len() >= 13, "{} shaders", files.len());
    // Every material shader with entry points has the defs of a real pipeline.
    let variants = pipeline_defs();
    for (name, source) in &files {
        if source.contains("@fragment") {
            assert!(
                variants.iter().any(|(f, _)| f == name),
                "{name} has no pipeline defs in fixtures/shader_defs.txt"
            );
        }
    }
    let (compiled, failures) = compile_all(&bevy, &files);
    println!(
        "{} of Bevy's shaders, {} of the app's, {compiled} compilations",
        bevy.len(),
        files.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    assert!(compiled >= 20, "{compiled} compilations");
}

/// The check is not vacuous: a parameter named with a WGSL reserved word (`patch`), a type
/// error and a call to a function that doesn't exist each fail it.
#[test]
fn a_broken_shader_fails_the_compile_check() {
    let bevy = bevy_shaders();
    let files = app_shader_files();
    let water = &files.iter().find(|(f, _)| f == "water.wesl").unwrap().1;
    let scatter = &files.iter().find(|(f, _)| f == "scatter.wesl").unwrap().1;
    let breaks: [(&str, &String, &str, &str); 3] = [
        (
            "water.wesl",
            water,
            "fn white_water_foam(rapid: f32, crest: f32, crests: f32,",
            "fn white_water_foam(rapid: f32, crest: f32, patch: f32,",
        ),
        (
            "scatter.wesl",
            scatter,
            "    let reach = max(wake, 1e-3);",
            "    let reach = max(wake, 1u);",
        ),
        (
            "water.wesl",
            water,
            "    let crests = crest_patch(xz);",
            "    let crests = crest_patchy(xz);",
        ),
    ];
    for (name, source, from, to) in breaks {
        assert_eq!(source.matches(from).count(), 1, "{from}");
        let broken: Vec<(String, String)> = files
            .iter()
            .map(|(f, s)| {
                if f == name {
                    (f.clone(), s.replace(from, to))
                } else {
                    (f.clone(), s.clone())
                }
            })
            .collect();
        let (_, failures) = compile_all(&bevy, &broken);
        println!(
            "{to}\n  -> {}",
            failures
                .first()
                .map_or("", |f| f.as_str())
                .lines()
                .take(4)
                .collect::<Vec<_>>()
                .join("\n     ")
        );
        assert!(
            failures.iter().any(|f| f.starts_with(name)),
            "{to}: no failure"
        );
    }
}
