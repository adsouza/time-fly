//! Headless globe capture for verification: renders the globe through the real shaders into an
//! image, without a window, and classifies what landed at a given world direction. Used by the
//! ignored GPU fixture test and the `globe_shot` example (native only).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::app::PluginsState;
use bevy::asset::{AssetMetaCheck, RenderAssetUsages};
use bevy::camera::RenderTarget;
use bevy::prelude::*;
use bevy::render::RenderPlugin;
use bevy::render::render_resource::{PollType, TextureFormat};
use bevy::render::renderer::RenderDevice;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use bevy::window::ExitCondition;
use bevy::winit::WinitPlugin;
use timefly_core::deep_time::YearsBp;
use timefly_core::geo::{geo_to_render, latlon_to_geo};

use super::{EARTH_RADIUS_KM, GlobeCamera, GlobeDebugView, GlobePlugin, GlobeRenderStatus};
use crate::data::GlobeDataPlugin;
use crate::time::{GlobeTime, TimePlugin};

/// Rendering, loading and capture together must finish within this.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Frames rendered after the globe turns interactive, before the capture.
const SETTLE_FRAMES: u32 = 4;

/// What to render.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    /// The asset root holding `data/globe/manifest.json`.
    pub asset_root: PathBuf,
    pub age_years: f64,
    pub size: (u32, u32),
    /// The camera looks at this point from above it.
    pub look_at_lat_lon: (f64, f64),
    pub distance_km: f64,
}

/// What a pixel shows.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Pixel {
    Land,
    Water,
    /// Space, or a direction off the visible hemisphere.
    Other,
}

/// A rendered globe image with the camera that took it.
pub struct GlobeCapture {
    pub image: Image,
    camera: Camera,
    camera_transform: GlobalTransform,
}

impl GlobeCapture {
    /// The sRGB pixel at the projection of the surface point in world direction `dir`.
    pub fn pixel_at(&self, dir: Vec3) -> Option<[u8; 4]> {
        let world = dir.normalize() * EARTH_RADIUS_KM as f32;
        let eye = self.camera_transform.translation();
        if (eye - world).dot(world) <= 0.0 {
            return None;
        }
        let p = self
            .camera
            .world_to_viewport(&self.camera_transform, world)
            .ok()?;
        let size = self.image.texture_descriptor.size;
        let (x, y) = (p.x.round() as u32, p.y.round() as u32);
        if x >= size.width || y >= size.height {
            return None;
        }
        let i = ((y * size.width + x) * 4) as usize;
        let data = self.image.data.as_ref()?;
        Some([data[i], data[i + 1], data[i + 2], data[i + 3]])
    }

    /// The direction the camera looks from (unit, world frame).
    pub fn view_dir(&self) -> Vec3 {
        self.camera_transform.translation().normalize()
    }

    /// Water is blue-dominant; land palette colours are not.
    pub fn classify(&self, dir: Vec3) -> Pixel {
        match self.pixel_at(dir) {
            None => Pixel::Other,
            Some([r, g, b, _]) => {
                let (r, g, b) = (i32::from(r), i32::from(g), i32::from(b));
                if r + g + b < 60 {
                    Pixel::Other
                } else if b > r + 10 && b + 10 >= g {
                    Pixel::Water
                } else {
                    Pixel::Land
                }
            }
        }
    }

    pub fn save_png(&self, path: &std::path::Path) -> Result<(), String> {
        self.image
            .clone()
            .try_into_dynamic()
            .map_err(|e| e.to_string())?
            .to_rgb8()
            .save(path)
            .map_err(|e| e.to_string())
    }
}

/// One headless globe app that can render several ages and views.
pub struct CaptureSession {
    app: App,
    target: Handle<Image>,
    interactive: bool,
}

impl CaptureSession {
    /// Builds the headless app for `asset_root`; `verification_view` switches the globe to its
    /// plate-index view (and turns MSAA off so pixels are never blends).
    pub fn new(
        asset_root: &std::path::Path,
        size: (u32, u32),
        verification_view: bool,
    ) -> Result<CaptureSession, String> {
        let mut app = App::new();
        app.add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: None,
                    exit_condition: ExitCondition::DontExit,
                    ..default()
                })
                .set(RenderPlugin {
                    synchronous_pipeline_compilation: true,
                    ..default()
                })
                .set(AssetPlugin {
                    file_path: asset_root.to_string_lossy().into_owned(),
                    meta_check: AssetMetaCheck::Never,
                    ..default()
                })
                .disable::<WinitPlugin>()
                .disable::<bevy::log::LogPlugin>(),
        )
        .add_plugins((
            crate::clusters::NoClusteringPlugin,
            TimePlugin,
            GlobeDataPlugin::default(),
            GlobePlugin,
        ))
        .insert_resource(GlobeDebugView(verification_view));
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();
        let (width, height) = size;
        let mut target =
            Image::new_target_texture(width, height, TextureFormat::Rgba8UnormSrgb, None);
        target.asset_usage = RenderAssetUsages::RENDER_WORLD;
        let target = app.world_mut().resource_mut::<Assets<Image>>().add(target);
        let mut session = CaptureSession {
            app,
            target,
            interactive: false,
        };
        // Spawn the camera, then aim it at the target.
        let deadline = Instant::now() + TIMEOUT;
        loop {
            session.step();
            let world = session.app.world_mut();
            let mut q = world.query_filtered::<Entity, With<GlobeCamera>>();
            if let Some(camera) = q.iter(world).next() {
                world
                    .entity_mut(camera)
                    .insert(RenderTarget::Image(session.target.clone().into()));
                if verification_view {
                    world.entity_mut(camera).insert(Msaa::Off);
                }
                break;
            }
            if Instant::now() > deadline {
                return Err("no globe camera".into());
            }
        }
        Ok(session)
    }

    fn step(&mut self) {
        self.app.update();
        if let Some(device) = self.app.world().get_resource::<RenderDevice>() {
            let _ = device.wgpu_device().poll(PollType::Wait {
                submission_index: None,
                timeout: None,
            });
        }
    }

    /// Renders the globe at `age_years`, looking at `(lat, lon)` from `distance_km`.
    pub fn shoot(
        &mut self,
        age_years: f64,
        look_at_lat_lon: (f64, f64),
        distance_km: f64,
    ) -> Result<GlobeCapture, String> {
        let (lat, lon) = look_at_lat_lon;
        let look = geo_to_render(latlon_to_geo(lat, lon)).0.as_vec3();
        let eye = look * distance_km as f32;
        let deadline = Instant::now() + TIMEOUT;
        let mut placed = false;
        let mut settle = 0u32;
        let captured: Arc<Mutex<Option<Image>>> = Arc::default();
        let mut requested = false;
        loop {
            if Instant::now() > deadline {
                return Err("timed out waiting for the globe".into());
            }
            let world = self.app.world_mut();
            if world.resource::<GlobeTime>().years != YearsBp(age_years) {
                world.resource_mut::<GlobeTime>().years = YearsBp(age_years);
            }
            if !placed {
                let mut q = world.query_filtered::<&mut Transform, With<GlobeCamera>>();
                for mut t in q.iter_mut(world) {
                    *t = Transform::from_translation(eye).looking_at(Vec3::ZERO, Vec3::Y);
                }
                // Light the face the camera looks at, from the upper left.
                let sun = (Quat::from_rotation_y(-0.6) * Quat::from_rotation_x(-0.35)) * look;
                let mut lights = world.query_filtered::<&mut Transform, With<DirectionalLight>>();
                for mut t in lights.iter_mut(world) {
                    *t = Transform::from_translation(sun).looking_at(Vec3::ZERO, Vec3::Y);
                }
                placed = lights.iter(world).next().is_some();
            }
            self.step();
            let world = self.app.world_mut();
            match world.resource::<GlobeRenderStatus>().clone() {
                GlobeRenderStatus::Failed(message) => return Err(message),
                GlobeRenderStatus::Interactive { .. } => self.interactive = true,
                _ => {}
            }
            if self.interactive && placed {
                settle += 1;
            }
            if !requested && settle >= SETTLE_FRAMES {
                let slot = captured.clone();
                world.spawn(Screenshot::image(self.target.clone())).observe(
                    move |shot: On<ScreenshotCaptured>| {
                        if let Ok(mut slot) = slot.lock() {
                            *slot = Some(shot.image.clone());
                        }
                    },
                );
                requested = true;
            }
            let image = captured.lock().ok().and_then(|mut s| s.take());
            if let Some(image) = image {
                let mut q =
                    world.query_filtered::<(&Camera, &GlobalTransform), With<GlobeCamera>>();
                let (camera, transform) = q.iter(world).next().ok_or("no globe camera")?;
                return Ok(GlobeCapture {
                    image,
                    camera: camera.clone(),
                    camera_transform: *transform,
                });
            }
        }
    }
}

/// Renders the globe headless at the requested age and returns the image.
pub fn capture_globe(request: &CaptureRequest) -> Result<GlobeCapture, String> {
    CaptureSession::new(&request.asset_root, request.size, false)?.shoot(
        request.age_years,
        request.look_at_lat_lon,
        request.distance_km,
    )
}

/// The id [`compare_with_reference`] uses for frame land on the ocean shell.
pub const GAP_LAND_ID: u32 = u32::MAX;

/// What the shader drew at a direction compared with what the CPU reference says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParityStats {
    /// Directions where both sides gave an answer away from any CPU boundary.
    pub compared: usize,
    /// Land/water agreement among those.
    pub mask_agree: usize,
    /// Exact agreement: ocean, or the same winning plate.
    pub plate_agree: usize,
    /// Skipped: the CPU answer changes within the tolerance.
    pub near_boundary: usize,
    /// Skipped: off the visible face, space, or an unreadable pixel.
    pub unreadable: usize,
    /// Mismatches beyond the tolerance: (direction, CPU, GPU).
    pub mismatches: Vec<(Vec3, Option<u32>, Option<u32>)>,
    /// Every compared direction.
    pub compared_dirs: Vec<Vec3>,
}

impl ParityStats {
    pub fn add(&mut self, other: &ParityStats) {
        self.compared += other.compared;
        self.mask_agree += other.mask_agree;
        self.plate_agree += other.plate_agree;
        self.near_boundary += other.near_boundary;
        self.unreadable += other.unreadable;
        self.mismatches.extend(other.mismatches.iter().copied());
        self.compared_dirs
            .extend(other.compared_dirs.iter().copied());
    }
}

/// Compares a verification-view capture with the CPU reference over `dirs`. `cpu(dir)` returns
/// the winning plate index, [`GAP_LAND_ID`] for gap land on the shell, or `None` for ocean. A direction counts only if the CPU answer is the
/// same at every point `tolerance_rad` around it (boundaries move with sampling and depth
/// interpolation) and the pixel decodes cleanly; `max_view_angle_rad` keeps to the face of the
/// globe the camera sees head-on.
pub fn compare_with_reference(
    capture: &GlobeCapture,
    dirs: impl IntoIterator<Item = Vec3>,
    cpu: impl FnMut(Vec3) -> Option<u32>,
    tolerance_rad: f32,
    max_view_angle_rad: f32,
) -> ParityStats {
    use super::material::{IdPixel, decode_plate_id};
    compare_with(
        capture,
        dirs,
        |c, d| match c.pixel_at(d).map(|[r, g, b, _]| decode_plate_id([r, g, b])) {
            Some(IdPixel::Ocean) => Some(None),
            Some(IdPixel::Plate(p)) => Some(Some(p)),
            Some(IdPixel::GapLand) => Some(Some(GAP_LAND_ID)),
            _ => None,
        },
        cpu,
        tolerance_rad,
        max_view_angle_rad,
    )
}

/// Like [`compare_with_reference`] for a normal (painterly) capture: land/water only, read with
/// [`GlobeCapture::classify`]. `cpu_land(dir)` is the CPU reference's land test.
pub fn compare_mask_with_reference(
    capture: &GlobeCapture,
    dirs: impl IntoIterator<Item = Vec3>,
    mut cpu_land: impl FnMut(Vec3) -> bool,
    tolerance_rad: f32,
    max_view_angle_rad: f32,
) -> ParityStats {
    compare_with(
        capture,
        dirs,
        |c, d| match c.classify(d) {
            Pixel::Land => Some(Some(0)),
            Pixel::Water => Some(None),
            Pixel::Other => None,
        },
        |d| cpu_land(d).then_some(0),
        tolerance_rad,
        max_view_angle_rad,
    )
}

fn compare_with(
    capture: &GlobeCapture,
    dirs: impl IntoIterator<Item = Vec3>,
    gpu: impl Fn(&GlobeCapture, Vec3) -> Option<Option<u32>>,
    mut cpu: impl FnMut(Vec3) -> Option<u32>,
    tolerance_rad: f32,
    max_view_angle_rad: f32,
) -> ParityStats {
    let view = capture.camera_transform.translation().normalize();
    let mut stats = ParityStats::default();
    for d in dirs {
        let d = d.normalize();
        if d.angle_between(view) > max_view_angle_rad {
            continue;
        }
        let Some(gpu) = gpu(capture, d) else {
            stats.unreadable += 1;
            continue;
        };
        let centre = cpu(d);
        let up = if d.y.abs() > 0.9 { Vec3::X } else { Vec3::Y };
        let t1 = up.cross(d).normalize();
        let t2 = d.cross(t1);
        let boundary = (0..8).any(|i| {
            let a = i as f32 * std::f32::consts::TAU / 8.0;
            let q = (d + (t1 * a.cos() + t2 * a.sin()) * tolerance_rad).normalize();
            cpu(q) != centre
        });
        if boundary {
            stats.near_boundary += 1;
            continue;
        }
        stats.compared += 1;
        stats.compared_dirs.push(d);
        if centre.is_some() == gpu.is_some() {
            stats.mask_agree += 1;
        }
        if centre == gpu {
            stats.plate_agree += 1;
        } else {
            stats.mismatches.push((d, centre, gpu));
        }
    }
    stats
}
