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

use super::{EARTH_RADIUS_KM, GlobeCamera, GlobePlugin, GlobeRenderStatus};
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

/// Renders the globe headless at the requested age and returns the image.
pub fn capture_globe(request: &CaptureRequest) -> Result<GlobeCapture, String> {
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
                file_path: request.asset_root.to_string_lossy().into_owned(),
                meta_check: AssetMetaCheck::Never,
                ..default()
            })
            .disable::<WinitPlugin>()
            .disable::<bevy::log::LogPlugin>(),
    )
    .add_plugins((TimePlugin, GlobeDataPlugin::default(), GlobePlugin));
    while app.plugins_state() == PluginsState::Adding {
        bevy::tasks::tick_global_task_pools_on_main_thread();
    }
    app.finish();
    app.cleanup();

    let (width, height) = request.size;
    let mut target = Image::new_target_texture(width, height, TextureFormat::Rgba8UnormSrgb, None);
    target.asset_usage = RenderAssetUsages::RENDER_WORLD;
    let target = app.world_mut().resource_mut::<Assets<Image>>().add(target);
    app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(request.age_years);

    let (lat, lon) = request.look_at_lat_lon;
    let look = geo_to_render(latlon_to_geo(lat, lon)).0.as_vec3();
    let eye = look * request.distance_km as f32;
    let deadline = Instant::now() + TIMEOUT;
    let mut placed = false;
    let mut interactive_at: Option<u32> = None;
    let captured: Arc<Mutex<Option<Image>>> = Arc::default();
    let mut requested = false;
    let mut frame = 0u32;
    loop {
        if Instant::now() > deadline {
            return Err("timed out waiting for the globe".into());
        }
        app.update();
        frame += 1;
        if let Some(device) = app.world().get_resource::<RenderDevice>() {
            let _ = device.wgpu_device().poll(PollType::Wait {
                submission_index: None,
                timeout: None,
            });
        }
        let world = app.world_mut();
        if !placed {
            let mut q = world.query_filtered::<Entity, With<GlobeCamera>>();
            if let Some(camera) = q.iter(world).next() {
                world.entity_mut(camera).insert((
                    RenderTarget::Image(target.clone().into()),
                    Transform::from_translation(eye).looking_at(Vec3::ZERO, Vec3::Y),
                ));
                // Light the face the camera looks at, from the upper left.
                let sun = (Quat::from_rotation_y(-0.6) * Quat::from_rotation_x(-0.35)) * look;
                let mut lights = world.query_filtered::<&mut Transform, With<DirectionalLight>>();
                for mut t in lights.iter_mut(world) {
                    *t = Transform::from_translation(sun).looking_at(Vec3::ZERO, Vec3::Y);
                }
                placed = lights.iter(world).next().is_some();
            }
        }
        match world.resource::<GlobeRenderStatus>().clone() {
            GlobeRenderStatus::Failed(message) => return Err(message),
            GlobeRenderStatus::Interactive { .. } if interactive_at.is_none() => {
                interactive_at = Some(frame);
            }
            _ => {}
        }
        if !requested && interactive_at.is_some_and(|f| frame >= f + SETTLE_FRAMES) && placed {
            let slot = captured.clone();
            world.spawn(Screenshot::image(target.clone())).observe(
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
            let mut q = world.query_filtered::<(&Camera, &GlobalTransform), With<GlobeCamera>>();
            let (camera, transform) = q.iter(world).next().ok_or("no globe camera")?;
            return Ok(GlobeCapture {
                image,
                camera: camera.clone(),
                camera_transform: *transform,
            });
        }
    }
}
