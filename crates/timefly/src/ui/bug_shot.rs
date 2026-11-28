//! Bug shots (M3 Task 12): `F12` ([`SHOT_KEY`]) or the HUD's camera button saves the window as
//! a PNG named by where the fly is and how it looks, so the picture serves as a visual bug
//! report, with a JSON sidecar ([`ShotPose`]) of the full pose beside it; `--from-shot <json>`
//! starts the game at that pose ([`ShotRestore`]).
//!
//! - **The name** ([`shot_name`]): `timefly_<UTC yyyymmdd-hhmmss>_<lat>_<lon>_<alt>m_h<heading>
//!   _p<look pitch>_<era>y_<globe|region>.png`, e.g.
//!   `timefly_20261126-143005_22.870N_71.420E_8101m_h047_p15_0y_region.png`: the era frame's
//!   latitude and longitude to 3 decimals with their hemispheres, the altitude above sea level
//!   in whole metres, the true compass heading and the chase camera's look below the horizontal
//!   in whole degrees, and the era in whole years. Captures append a unique ID before `.png`
//!   so consecutive shots of the same place keep distinct pairs. [`parse_shot_name`] reads both forms.
//! - **The folder** ([`shot_folder`]): the pictures folder's `Time Fly`, or without one the
//!   data folder's `shots`. On the web both files are browser downloads of the same names.
//! - **Timing.** A Bevy screenshot captures a frame a few frames after its request, so the name
//!   and the sidecar are fixed at the key press; a toast names the saved file's full path once
//!   the picture is written (never before, and hidden while a shot is pending, so no toast is
//!   in a picture). A press while a shot is still pending is ignored.
//! - **The repro** ([`ShotRestore`]): over the globe it waits for the globe to turn interactive,
//!   then sets the era, the fly and the view's drag offset; in a region it asks for the region
//!   (its spec is a function of the centre and the snapshot) and, once it is entered, puts the
//!   fly where it was. Either way the camera is then held at the saved pose
//!   ([`HeldCamera`], to the bit: its translation and rotation are restored, not re-derived)
//!   until the player's first input, so the repro frame is the shot's frame while the chase's
//!   glide catches up underneath. A transition in progress (the cloud, a hop) is not restored:
//!   the repro starts in the world that was on screen.

use std::path::{Path, PathBuf};

use bevy::ecs::system::SystemParam;
use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::input::touch::Touches;
use bevy::math::{DQuat, DVec3};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::window::PrimaryWindow;
use serde::{Deserialize, Serialize};
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::globe::GlobeFlight;
use timefly_core::flight::position::{
    compact_latitude, compact_longitude, parse_coordinate, whole_degrees,
};
use timefly_core::geo::RenderDir;
use timefly_core::worldgen::generate::RegionSpec;

use super::position::{FlyPlace, Place, ShotWorld, update_fly_place};
use super::{BlocksCamera, PANEL_COLOR, TEXT_COLOR};
use crate::clock::{LoadClock, add_load_clock};
use crate::diagnostics::SessionInfo;
use crate::diagnostics::session::UtcTime;
use crate::fly_camera::{FlyChase, FlyState, FlySystems, apply_fly};
use crate::globe::{GlobeCamera, GlobeCameraSync, GlobeRenderStatus};
use crate::globe_flight::{
    GlobeChase, GlobeFlightState, GlobeFlightSystems, GlobeLook, apply_globe_flight,
};
use crate::region::navigate::{NavPhase, Navigation, NavigationSystems};
use crate::region::{ActiveRegion, RegionRequested, RegionState, RegionSystems, RequestPurpose};
use crate::terrain::TerrainScene;
use crate::time::GlobeTime;

/// The key that saves a bug shot.
pub const SHOT_KEY: KeyCode = KeyCode::F12;
/// The sidecar's format version.
pub const SIDECAR_VERSION: u32 = 1;
/// The pictures folder's subfolder for bug shots.
pub const PICTURES_SUBFOLDER: &str = "Time Fly";
/// The data folder's subfolder for bug shots, without a pictures folder.
pub const DATA_SUBFOLDER: &str = "shots";
/// Decimal places of the name's latitude and longitude.
pub const NAME_DECIMALS: usize = 3;
/// How long the toast shows (seconds).
const TOAST_S: f64 = 6.0;
/// The toast's distance from the window's bottom (logical pixels): above the slider.
const TOAST_BOTTOM_PX: f32 = 110.0;
/// A shot that has not resolved after this long no longer blocks the next (seconds).
const PENDING_GIVE_UP_S: f64 = 5.0;

/// What a shot's name says: the UTC second it was taken and the fly's place, as the name
/// rounds them.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ShotName {
    pub utc: UtcTime,
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub altitude_m: f64,
    pub heading_deg: f64,
    pub look_pitch_deg: f64,
    pub era_years: f64,
    pub world: ShotWorld,
}

impl ShotName {
    /// The name's parts for `place` at `utc`.
    pub fn of(place: &Place, utc: UtcTime) -> ShotName {
        ShotName {
            utc,
            lat_deg: place.lat_deg,
            lon_deg: place.lon_deg,
            altitude_m: place.altitude_m,
            heading_deg: place.heading_deg,
            look_pitch_deg: place.look_pitch_deg,
            era_years: place.era_years,
            world: place.world,
        }
    }
}

fn world_word(world: ShotWorld) -> &'static str {
    match world {
        ShotWorld::Globe => "globe",
        ShotWorld::Region => "region",
    }
}

/// The PNG's file name (see the module docs).
pub fn shot_name(name: &ShotName) -> String {
    let t = name.utc;
    format!(
        "timefly_{:04}{:02}{:02}-{:02}{:02}{:02}_{}_{}_{}m_h{:03}_p{}_{}y_{}.png",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        t.second,
        compact_latitude(name.lat_deg, NAME_DECIMALS),
        compact_longitude(name.lon_deg, NAME_DECIMALS),
        name.altitude_m.round() as i64,
        whole_degrees(name.heading_deg),
        name.look_pitch_deg.round() as i64,
        name.era_years.round().max(0.0) as u64,
        world_word(name.world),
    )
}

/// The sidecar's file name beside a shot's PNG.
pub fn sidecar_name(png: &str) -> String {
    format!("{}.json", png.strip_suffix(".png").unwrap_or(png))
}

/// Reads the rounded pose from a capture filename, including legacy names without a capture ID.
pub fn parse_shot_name(file: &str) -> Option<ShotName> {
    let stem = file.strip_prefix("timefly_")?.strip_suffix(".png")?;
    let stem = match stem.rsplit_once('+') {
        Some((base, id)) => {
            bevy::asset::uuid::Uuid::parse_str(id).ok()?;
            base
        }
        None => stem,
    };
    let parts: Vec<&str> = stem.split('_').collect();
    let [stamp, lat, lon, alt, heading, pitch, era, world] = parts[..] else {
        return None;
    };
    let (date, clock) = stamp.split_once('-')?;
    fn digits(s: &str) -> Option<&str> {
        s.bytes().all(|b| b.is_ascii_digit()).then_some(s)
    }
    let (date, clock) = (digits(date)?, digits(clock)?);
    if date.len() != 8 || clock.len() != 6 {
        return None;
    }
    let number = |s: &str| s.parse::<u32>().ok();
    let utc = UtcTime {
        year: date[..4].parse().ok()?,
        month: number(&date[4..6])?,
        day: number(&date[6..])?,
        hour: number(&clock[..2])?,
        minute: number(&clock[2..4])?,
        second: number(&clock[4..])?,
        millis: 0,
    };
    let world = match world {
        "globe" => ShotWorld::Globe,
        "region" => ShotWorld::Region,
        _ => return None,
    };
    Some(ShotName {
        utc,
        lat_deg: parse_coordinate(lat, ('N', 'S'))?,
        lon_deg: parse_coordinate(lon, ('E', 'W'))?,
        altitude_m: alt.strip_suffix('m')?.parse::<i64>().ok()? as f64,
        heading_deg: f64::from(heading.strip_prefix('h')?.parse::<u32>().ok()?),
        look_pitch_deg: pitch.strip_prefix('p')?.parse::<i64>().ok()? as f64,
        era_years: era.strip_suffix('y')?.parse::<u64>().ok()? as f64,
        world,
    })
}

/// The globe flight's state at the shot (in a region, as it was left).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GlobeShot {
    /// The fly's unit direction and unit tangent heading (render frame).
    pub dir: [f64; 3],
    pub heading: [f64; 3],
    pub altitude_km: f64,
    /// The view drag's offset (degrees, positive looks further down).
    pub look_offset_deg: f64,
}

/// The region on screen at the shot and the fly in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegionShot {
    /// The spec's centre direction (era frame, render axes) and its other fields.
    pub center: [f64; 3],
    pub snapshot_years: f64,
    pub seed: u32,
    pub inner_res: u32,
    pub apron_res: u32,
    pub sea_level_anomaly_m: f64,
    /// The fly: local metres (`+x` east, `−z` north, `+y` up from sea level), the heading's
    /// yaw (0 faces the frame's north, positive turns west) and the view's pitch (its look
    /// offset: negative looks down).
    pub fly_pos_m: [f64; 3],
    pub yaw_rad: f64,
    pub pitch_rad: f64,
}

impl RegionShot {
    /// The region's spec.
    pub fn spec(&self) -> RegionSpec {
        RegionSpec {
            center: RenderDir(DVec3::from_array(self.center)),
            snapshot: YearsBp(self.snapshot_years),
            seed: self.seed,
            inner_res: self.inner_res,
            apron_res: self.apron_res,
            sea_level_anomaly_m: self.sea_level_anomaly_m,
        }
    }

    /// The fly as it was.
    pub fn fly(&self) -> FlyState {
        let [x, y, z] = self.fly_pos_m;
        FlyState {
            pos_m: Vec3::new(x as f32, y as f32, z as f32),
            yaw: self.yaw_rad as f32,
            pitch: self.pitch_rad as f32,
        }
    }
}

/// The camera on screen, in its world's frame (globe: km from the centre, render frame;
/// region: local metres): the eye, a target one unit ahead along the view, the up vector and
/// the rotation itself (`[x, y, z, w]`), so a repro restores it exactly.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraShot {
    pub eye: [f64; 3],
    pub target: [f64; 3],
    pub up: [f64; 3],
    pub rotation: [f64; 4],
}

impl CameraShot {
    /// The camera's pose from its transform.
    pub fn of(transform: &Transform) -> CameraShot {
        let eye = transform.translation.as_dvec3();
        let rotation = transform.rotation.as_dquat();
        CameraShot {
            eye: eye.to_array(),
            target: (eye + rotation * DVec3::NEG_Z).to_array(),
            up: (rotation * DVec3::Y).to_array(),
            rotation: rotation.to_array(),
        }
    }

    /// The camera's transform, exactly as it was saved.
    pub fn transform(&self) -> Transform {
        let [x, y, z, w] = self.rotation;
        Transform {
            translation: DVec3::from_array(self.eye).as_vec3(),
            rotation: DQuat::from_xyzw(x, y, z, w).as_quat(),
            scale: Vec3::ONE,
        }
    }
}

/// The window at the shot: physical pixels and the scale factor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowShot {
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
}

impl WindowShot {
    /// The window's logical size (what `--window WxH` asks for: the OS scales it by the
    /// display's factor), or `None` for an unknown size. A 2560 × 1440 shot at 2× is 1280 × 720.
    pub fn logical_size(&self) -> Option<(u32, u32)> {
        let scale = f64::from(self.scale_factor);
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let logical = |px: u32| ((f64::from(px) / scale).round() as u32).max(1);
        (self.width > 0 && self.height > 0).then(|| (logical(self.width), logical(self.height)))
    }
}

/// A repro's window: the shot's logical size, unless `--window` already chose one.
pub fn repro_window(args: &mut crate::args::LaunchArgs, pose: &ShotPose) {
    if args.window.is_none() {
        args.window = pose.window.logical_size();
    }
}

/// The sidecar: everything needed to stand where the shot was taken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotPose {
    pub version: u32,
    /// The PNG's file name.
    pub file: String,
    /// When the shot was taken (RFC 3339, UTC).
    pub utc: String,
    pub world: ShotWorld,
    /// The place as the HUD read it (era-frame degrees, metres above sea level, true heading,
    /// the chase look below the horizontal) and the era shown.
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub altitude_m: f64,
    pub heading_deg: f64,
    pub look_pitch_deg: f64,
    pub era_years: f64,
    /// The fly's position in its world's frame (globe: km from the centre, render frame;
    /// region: local metres).
    pub fly_position: [f64; 3],
    pub globe: GlobeShot,
    pub region: Option<RegionShot>,
    pub camera: CameraShot,
    pub build_sha: String,
    /// The build's one-line summary (version, SHA, profile, target, features).
    pub build: String,
    pub session_id: String,
    pub window: WindowShot,
}

/// What [`shot_pose`] is built from.
pub struct ShotInputs<'a> {
    pub place: Place,
    pub globe: GlobeFlight,
    pub look_offset_deg: f64,
    /// The region on screen and its fly.
    pub region: Option<(&'a RegionSpec, FlyState)>,
    pub camera: &'a Transform,
    pub utc: UtcTime,
    pub build: &'a str,
    pub session_id: &'a str,
    pub window: WindowShot,
}

/// The sidecar for a shot taken now.
pub fn shot_pose(inputs: ShotInputs) -> ShotPose {
    let place = inputs.place;
    let fly_position = match (place.world, &inputs.region) {
        (ShotWorld::Region, Some((_, fly))) => fly.pos_m.as_dvec3(),
        _ => inputs.globe.position_km(),
    };
    let base = shot_name(&ShotName::of(&place, inputs.utc));
    let stem = base.strip_suffix(".png").unwrap();
    let id = bevy::asset::uuid::Uuid::new_v4();
    ShotPose {
        version: SIDECAR_VERSION,
        file: format!("{stem}+{id}.png"),
        utc: inputs.utc.rfc3339(),
        world: place.world,
        lat_deg: place.lat_deg,
        lon_deg: place.lon_deg,
        altitude_m: place.altitude_m,
        heading_deg: place.heading_deg,
        look_pitch_deg: place.look_pitch_deg,
        era_years: place.era_years,
        fly_position: fly_position.to_array(),
        globe: GlobeShot {
            dir: inputs.globe.dir.to_array(),
            heading: inputs.globe.heading.to_array(),
            altitude_km: inputs.globe.altitude_km,
            look_offset_deg: inputs.look_offset_deg,
        },
        region: inputs.region.map(|(spec, fly)| RegionShot {
            center: spec.center.0.to_array(),
            snapshot_years: spec.snapshot.0,
            seed: spec.seed,
            inner_res: spec.inner_res,
            apron_res: spec.apron_res,
            sea_level_anomaly_m: spec.sea_level_anomaly_m,
            fly_pos_m: fly.pos_m.as_dvec3().to_array(),
            yaw_rad: f64::from(fly.yaw),
            pitch_rad: f64::from(fly.pitch),
        }),
        camera: CameraShot::of(inputs.camera),
        build_sha: env!("TIMEFLY_GIT_SHA").to_string(),
        build: inputs.build.to_string(),
        session_id: inputs.session_id.to_string(),
        window: inputs.window,
    }
}

/// Where bug shots go: `pictures/Time Fly`, or without a pictures folder `data/shots`; `None`
/// with neither.
pub fn shot_folder(pictures: Option<PathBuf>, data: Option<PathBuf>) -> Option<PathBuf> {
    pictures
        .map(|p| p.join(PICTURES_SUBFOLDER))
        .or_else(|| data.map(|d| d.join(DATA_SUBFOLDER)))
}

/// This machine's bug-shot folder ([`shot_folder`] of the user's pictures folder and the
/// game's data folder); `None` on the web, where shots are downloads.
pub fn default_shot_folder() -> Option<PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let pictures =
            directories::UserDirs::new().and_then(|u| u.picture_dir().map(Path::to_path_buf));
        let data =
            directories::ProjectDirs::from("", "", "Time Fly").map(|p| p.data_dir().to_path_buf());
        shot_folder(pictures, data)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Reads a sidecar for `--from-shot`.
pub fn load_shot(path: &Path) -> Result<ShotPose, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read the shot: {e}"))?;
    let pose: ShotPose =
        serde_json::from_str(&text).map_err(|e| format!("not a bug-shot sidecar: {e}"))?;
    if pose.version != SIDECAR_VERSION {
        return Err(format!(
            "sidecar version {} (this build reads {SIDECAR_VERSION})",
            pose.version
        ));
    }
    if pose.world == ShotWorld::Region && pose.region.is_none() {
        return Err("a region shot without its region".to_string());
    }
    Ok(pose)
}

/// The bug-shot folder; `None` on the web (downloads) or where the machine has none.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct ShotFolder(pub Option<PathBuf>);

impl Default for ShotFolder {
    fn default() -> Self {
        ShotFolder(default_shot_folder())
    }
}

/// What a bug shot captures: the primary window, or this image (a tour's offscreen target).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct ShotTarget(pub Option<Handle<Image>>);

/// Asks for a bug shot this frame (the HUD's button; `F12` does the same).
#[derive(Message, Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ShotRequested;

/// The last bug shot taken: the sidecar, and where the PNG goes (for tours and tests).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct LastShot {
    pub pose: Option<ShotPose>,
    pub png: Option<PathBuf>,
    /// Whether the PNG has been written.
    pub saved: bool,
}

/// The toast's text and when it was shown (real seconds).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
struct ShotToast {
    text: String,
    shown_at: Option<f64>,
}

/// When the pending shot was requested (real seconds).
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
struct ShotPending(Option<f64>);

#[derive(Component)]
struct ToastText;

#[derive(Component)]
struct ShotButton;

pub(super) struct BugShotPlugin;

impl Plugin for BugShotPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShotFolder>()
            .init_resource::<ShotTarget>()
            .init_resource::<LastShot>()
            .init_resource::<ShotToast>()
            .init_resource::<ShotPending>()
            .add_message::<ShotRequested>()
            .add_systems(Startup, (spawn_button, spawn_toast))
            .add_systems(Update, take_shot.after(update_fly_place))
            .add_systems(PostUpdate, show_toast);
    }
}

/// The repro's plugin: [`ShotRestore`] and [`HeldCamera`] (added by `crate::app` with
/// `--from-shot`, or by a tour).
pub struct ShotRestorePlugin;

impl Plugin for ShotRestorePlugin {
    fn build(&self, app: &mut App) {
        add_load_clock(app);
        app.add_message::<RegionRequested>()
            .add_systems(
                PreUpdate,
                release_held_camera.after(bevy::input::InputSystems),
            )
            .add_systems(
                Update,
                (
                    restore_shot
                        .run_if(resource_exists::<ShotRestore>)
                        .after(RegionSystems)
                        .after(NavigationSystems)
                        .after(FlySystems::Step)
                        .before(FlySystems::Apply)
                        .after(GlobeFlightSystems::Step)
                        .before(GlobeFlightSystems::Apply),
                    hold_region_camera
                        .run_if(resource_exists::<HeldCamera>)
                        .in_set(FlySystems::Apply)
                        .after(apply_fly),
                    hold_globe_camera
                        .run_if(resource_exists::<HeldCamera>)
                        .in_set(GlobeFlightSystems::Apply)
                        .after(apply_globe_flight)
                        .before(GlobeCameraSync),
                ),
            );
    }
}

/// A small camera drawn from boxes (the UI font has no camera glyph): a body, a lens and the
/// viewfinder's bump. In the window's top-right corner (Task 11c fix round 2, the owner's
/// request), the frame-rate readout just left of it.
fn spawn_button(mut commands: Commands) {
    commands
        .spawn((
            Name::new("Bug shot button"),
            ShotButton,
            BlocksCamera,
            Node {
                position_type: PositionType::Absolute,
                top: px(super::GUTTER_PX),
                right: px(super::GUTTER_PX),
                width: px(super::CORNER_BUTTON_PX.x),
                height: px(super::CORNER_BUTTON_PX.y),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                border_radius: BorderRadius::all(px(4)),
                ..default()
            },
            BackgroundColor(PANEL_COLOR),
            // Above the scene's UI, as the readout beside it.
            GlobalZIndex(10),
        ))
        .with_children(|b| {
            b.spawn((
                Node {
                    width: px(16),
                    height: px(11),
                    margin: UiRect::top(px(2)),
                    border: UiRect::all(px(1.5)),
                    border_radius: BorderRadius::all(px(2)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BorderColor::all(TEXT_COLOR.with_alpha(0.85)),
                Pickable::IGNORE,
            ))
            .with_children(|body| {
                body.spawn((
                    Node {
                        width: px(5),
                        height: px(5),
                        border: UiRect::all(px(1.5)),
                        border_radius: BorderRadius::MAX,
                        ..default()
                    },
                    BorderColor::all(TEXT_COLOR.with_alpha(0.85)),
                    Pickable::IGNORE,
                ));
                body.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        top: px(-4),
                        left: px(3),
                        width: px(5),
                        height: px(3),
                        ..default()
                    },
                    BackgroundColor(TEXT_COLOR.with_alpha(0.85)),
                    Pickable::IGNORE,
                ));
            });
        })
        .observe(
            |_: On<PointerClick>, mut requests: MessageWriter<ShotRequested>| {
                requests.write(ShotRequested);
            },
        );
}

fn spawn_toast(mut commands: Commands) {
    commands.spawn((
        Name::new("Bug shot toast"),
        // Above the slider, clear of the HUD's column.
        Node {
            position_type: PositionType::Absolute,
            bottom: px(TOAST_BOTTOM_PX),
            left: px(0),
            right: px(0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        Visibility::Hidden,
        Pickable::IGNORE,
        children![(
            ToastText,
            Text::default(),
            TextFont::from_font_size(13.0),
            TextColor(TEXT_COLOR),
            Node {
                padding: UiRect::axes(px(10), px(5)),
                max_width: percent(70),
                ..default()
            },
            BackgroundColor(PANEL_COLOR),
            Pickable::IGNORE,
        )],
    ));
}

impl ShotToast {
    fn show(&mut self, text: String, now_s: f64) {
        self.text = text;
        self.shown_at = Some(now_s);
    }
}

fn show_toast(
    toast: Res<ShotToast>,
    pending: Res<ShotPending>,
    time: Res<Time<Real>>,
    mut text: Single<(&mut Text, &ChildOf), With<ToastText>>,
    mut roots: Query<&mut Visibility>,
) {
    let (text, parent) = &mut *text;
    let now = time.elapsed_secs_f64();
    // Never in a picture: hidden while a shot is pending.
    let shooting = pending.0.is_some_and(|at| now - at < PENDING_GIVE_UP_S);
    let shown = !shooting && toast.shown_at.is_some_and(|at| now - at < TOAST_S);
    if toast.is_changed() && text.0 != toast.text {
        text.0.clone_from(&toast.text);
    }
    if let Ok(mut visibility) = roots.get_mut(parent.parent()) {
        visibility.set_if_neq(if shown {
            Visibility::Visible
        } else {
            Visibility::Hidden
        });
    }
}

/// What a bug shot reads, besides the place.
#[derive(SystemParam)]
struct ShotSources<'w, 's> {
    place: Res<'w, FlyPlace>,
    flight: Res<'w, GlobeFlightState>,
    look: Res<'w, GlobeLook>,
    fly: Res<'w, FlyState>,
    scene: Option<Res<'w, TerrainScene>>,
    session: Option<Res<'w, SessionInfo>>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    globe_cameras: Query<'w, 's, &'static Transform, With<GlobeCamera>>,
    transforms: Query<'w, 's, &'static Transform>,
}

impl ShotSources<'_, '_> {
    /// The sidecar for a shot now, or `None` when no world shows the fly.
    fn pose(&self, utc: UtcTime) -> Option<ShotPose> {
        let place = self.place.0?;
        let camera = match place.world {
            ShotWorld::Region => self.transforms.get(self.scene.as_ref()?.camera).ok()?,
            ShotWorld::Globe => self.globe_cameras.single().ok()?,
        };
        let window = self.windows.single().map_or(
            WindowShot {
                width: 0,
                height: 0,
                scale_factor: 1.0,
            },
            |w| WindowShot {
                width: w.physical_width(),
                height: w.physical_height(),
                scale_factor: w.scale_factor(),
            },
        );
        let (session_id, build) = self.session.as_ref().map_or(("unknown", "unknown"), |s| {
            (s.id.as_str(), s.build.as_str())
        });
        Some(shot_pose(ShotInputs {
            place,
            globe: self.flight.0,
            look_offset_deg: self.look.offset_deg,
            region: self.scene.as_ref().map(|s| (&s.grid.spec, *self.fly)),
            camera,
            utc,
            build,
            session_id,
            window,
        }))
    }
}

/// `F12` or the button: fixes the name and writes the sidecar now, and asks for the picture,
/// which resolves a few frames later.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn take_shot(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mut requests: MessageReader<ShotRequested>,
    sources: ShotSources,
    folder: Res<ShotFolder>,
    target: Res<ShotTarget>,
    time: Res<Time<Real>>,
    mut pending: ResMut<ShotPending>,
    mut toast: ResMut<ShotToast>,
    mut last: ResMut<LastShot>,
    mut commands: Commands,
) {
    let asked = requests.read().count() > 0;
    if !(asked || keys.is_some_and(|k| k.just_pressed(SHOT_KEY))) {
        return;
    }
    let now = time.elapsed_secs_f64();
    if pending.0.is_some_and(|at| now - at < PENDING_GIVE_UP_S) {
        return;
    }
    let utc = UtcTime::from_unix_millis(crate::diagnostics::now_unix_millis());
    let Some(pose) = sources.pose(utc) else {
        toast.show("Nothing to shoot yet".to_string(), now);
        return;
    };
    let png = match &folder.0 {
        Some(dir) => dir.join(&pose.file),
        None => PathBuf::from(&pose.file),
    };
    let json = match serde_json::to_string_pretty(&pose) {
        Ok(json) => json,
        Err(e) => {
            error!(error = %e, "bug shot: the sidecar did not serialise");
            return;
        }
    };
    if let Err(e) = write_sidecar(&png, &json) {
        warn!(file = pose.file, error = %e, "bug shot: the sidecar was not saved");
        toast.show(format!("Bug shot not saved: {e}"), now);
        return;
    }
    info!(file = pose.file, world = ?pose.world, "bug shot requested");
    pending.0 = Some(now);
    *last = LastShot {
        pose: Some(pose),
        png: Some(png.clone()),
        saved: false,
    };
    let screenshot = match &target.0 {
        Some(image) => Screenshot::image(image.clone()),
        None => Screenshot::primary_window(),
    };
    let mut save = save_to_disk(png.clone());
    commands.spawn(screenshot).observe(
        move |captured: On<ScreenshotCaptured>,
              time: Res<Time<Real>>,
              mut pending: ResMut<ShotPending>,
              mut toast: ResMut<ShotToast>,
              mut last: ResMut<LastShot>| {
            save(captured);
            pending.0 = None;
            let now = time.elapsed_secs_f64();
            if cfg!(target_arch = "wasm32") || png.exists() {
                if last.png.as_ref() == Some(&png) {
                    last.saved = true;
                }
                let verb = if cfg!(target_arch = "wasm32") {
                    "downloaded"
                } else {
                    "saved"
                };
                toast.show(format!("Bug shot {verb}: {}", png.display()), now);
            } else {
                toast.show(format!("Bug shot not saved: {}", png.display()), now);
            }
        },
    );
}

/// Writes the sidecar beside `png` (native: into its folder, made if missing; web: a download).
fn write_sidecar(png: &Path, json: &str) -> Result<(), String> {
    let name = sidecar_name(&png.file_name().unwrap_or_default().to_string_lossy());
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::Write;
        let dir = png.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::File::create_new(dir.join(name))
            .and_then(|mut file| file.write_all(json.as_bytes()))
            .map_err(|e| e.to_string())
    }
    #[cfg(target_arch = "wasm32")]
    {
        web::download(&name, json).ok_or_else(|| "the browser refused the download".to_string())
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::JsCast;

    /// Offers `text` as a download named `filename`.
    pub(super) fn download(filename: &str, text: &str) -> Option<()> {
        let document = web_sys::window()?.document()?;
        let parts = js_sys::Array::of1(&text.into());
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("application/json");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options).ok()?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).ok()?;
        let anchor: web_sys::HtmlAnchorElement =
            document.create_element("a").ok()?.dyn_into().ok()?;
        anchor.set_href(&url);
        anchor.set_download(filename);
        anchor.click();
        let _ = web_sys::Url::revoke_object_url(&url);
        Some(())
    }
}

/// Where a `--from-shot` repro has got to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RestoreStage {
    /// Waiting for the globe to turn interactive.
    WaitGlobe,
    /// The region is asked for; waiting to enter it.
    Requested,
    /// The pose is restored and the camera held.
    Placed,
    /// The region failed to load or never came ([`RESTORE_GIVE_UP_S`]); logged once.
    GaveUp,
}

/// How long a repro waits, after asking for its region, to be flying in it (seconds of the
/// [`LoadClock`], so a hidden tab does not count). A web first entry takes about 10 s, each load
/// may take [`crate::region::LOAD_TIMEOUT_S`] before it fails, and the descent cloud follows.
pub const RESTORE_GIVE_UP_S: f64 = 120.0;

/// The largest angle (radians) between a shot's region centre and the generated region's that
/// still counts as the same region: about 6 mm on the Earth. `spec_for` normalises the saved
/// centre again, which moves about a third of unit vectors in their last bits.
pub const SAME_CENTRE_RAD: f64 = 1e-9;

/// Whether two region centres are the same direction up to rounding. `atan2(|a × b|, a · b)`
/// resolves angles near 0; glam's `angle_between` (an `acos`) jumps from 0 to about 1.5e-8.
pub fn same_centre(a: RenderDir, b: RenderDir) -> bool {
    let (a, b) = (a.0, b.0);
    a.cross(b).length().atan2(a.dot(b)) < SAME_CENTRE_RAD
}

/// Whether a generated region's spec is a shot's: the centre up to rounding
/// ([`same_centre`]), every other field exactly.
pub fn same_spec(generated: &RegionSpec, saved: &RegionSpec) -> bool {
    same_centre(generated.center, saved.center)
        && RegionSpec {
            center: saved.center,
            ..*generated
        } == *saved
}

/// A `--from-shot` repro: the sidecar and its progress.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct ShotRestore {
    pub pose: ShotPose,
    pub stage: RestoreStage,
    /// The [`LoadClock`] when the region was asked for.
    requested_at_s: f64,
    /// Whether the region has been generating since it was asked for (so a return to the globe
    /// is a failed load, not the request still waiting to be read).
    generating_seen: bool,
}

impl ShotRestore {
    pub fn new(pose: ShotPose) -> ShotRestore {
        ShotRestore {
            pose,
            stage: RestoreStage::WaitGlobe,
            requested_at_s: 0.0,
            generating_seen: false,
        }
    }

    /// Whether the pose is restored.
    pub fn placed(&self) -> bool {
        self.stage == RestoreStage::Placed
    }
}

/// The camera held at a shot's pose until the player's first input (keys other than
/// [`SHOT_KEY`], a mouse button or the scroll wheel).
#[derive(Resource, Copy, Clone, Debug, PartialEq)]
pub struct HeldCamera {
    pub world: ShotWorld,
    pub transform: Transform,
}

/// The worlds a repro writes.
#[derive(SystemParam)]
struct RestoreWorlds<'w> {
    time: ResMut<'w, GlobeTime>,
    flight: ResMut<'w, GlobeFlightState>,
    look: ResMut<'w, GlobeLook>,
    globe_chase: ResMut<'w, GlobeChase>,
    fly: ResMut<'w, FlyState>,
    fly_chase: ResMut<'w, FlyChase>,
}

/// Steps a repro: the globe pose once the globe is interactive; or the region asked for, and
/// the fly placed once it is entered. A region that fails to load, or is not flown in within
/// [`RESTORE_GIVE_UP_S`], ends the repro with a warning.
#[expect(clippy::too_many_arguments, reason = "a Bevy system's parameters")]
fn restore_shot(
    mut restore: ResMut<ShotRestore>,
    clock: Res<LoadClock>,
    status: Option<Res<GlobeRenderStatus>>,
    region: Option<Res<ActiveRegion>>,
    state: Option<Res<State<RegionState>>>,
    nav: Option<Res<Navigation>>,
    mut worlds: RestoreWorlds,
    mut requests: MessageWriter<RegionRequested>,
    mut commands: Commands,
) {
    let pose = &restore.pose;
    let held = HeldCamera {
        world: pose.world,
        transform: pose.camera.transform(),
    };
    match restore.stage {
        RestoreStage::WaitGlobe => {
            if !matches!(
                status.as_deref(),
                Some(GlobeRenderStatus::Interactive { .. })
            ) {
                return;
            }
            worlds.time.years = YearsBp(pose.era_years);
            match (pose.world, &pose.region) {
                (ShotWorld::Region, Some(region)) => {
                    let spec = region.spec();
                    info!(file = pose.file, "from shot: entering its region");
                    requests.write(RegionRequested {
                        era_dir: spec.center,
                        snapshot: spec.snapshot,
                        purpose: RequestPurpose::Enter,
                    });
                    restore.stage = RestoreStage::Requested;
                    restore.requested_at_s = clock.elapsed_s;
                    restore.generating_seen = false;
                }
                _ => {
                    let g = &pose.globe;
                    worlds.flight.0 = GlobeFlight {
                        dir: DVec3::from_array(g.dir),
                        heading: DVec3::from_array(g.heading),
                        altitude_km: g.altitude_km,
                    };
                    worlds.look.offset_deg = g.look_offset_deg;
                    worlds.globe_chase.reset();
                    commands.insert_resource(held);
                    info!(file = pose.file, "from shot: over the globe");
                    restore.stage = RestoreStage::Placed;
                }
            }
        }
        RestoreStage::Requested => {
            let Some(saved) = &pose.region else {
                return;
            };
            let want = saved.spec();
            // By angle: `spec_for` normalises the saved centre again (see `same_centre`).
            let entered = region.as_ref().is_some_and(|r| {
                same_centre(r.grid.spec.center, want.center)
                    && r.grid.spec.snapshot == want.snapshot
            });
            let now = state.map(|s| *s.get());
            let flying = now.is_none_or(|s| s == RegionState::InRegion)
                && nav.is_none_or(|n| n.phase == NavPhase::Flying);
            if !(entered && flying) {
                let generating_seen =
                    restore.generating_seen || now == Some(RegionState::Generating);
                // Generating, then the globe again: the load failed (`job::fail`).
                let failed = generating_seen && now == Some(RegionState::Globe);
                let waited_s = clock.elapsed_s - restore.requested_at_s;
                if failed || waited_s > RESTORE_GIVE_UP_S {
                    warn!(
                        file = pose.file,
                        saved = ?want,
                        generated = ?region.as_ref().map(|r| r.grid.spec),
                        waited_s,
                        "from shot: giving up; {}",
                        if failed {
                            "the region failed to load"
                        } else {
                            "the region never came"
                        }
                    );
                    restore.stage = RestoreStage::GaveUp;
                }
                restore.generating_seen = generating_seen;
                return;
            }
            if let Some(r) = &region
                && !same_spec(&r.grid.spec, &want)
            {
                warn!(
                    saved = ?want,
                    generated = ?r.grid.spec,
                    "from shot: this build generates the region with another spec"
                );
            }
            *worlds.fly = saved.fly();
            worlds.fly_chase.reset();
            commands.insert_resource(held);
            info!(file = pose.file, "from shot: in its region");
            restore.stage = RestoreStage::Placed;
        }
        RestoreStage::Placed | RestoreStage::GaveUp => {}
    }
}

fn hold_region_camera(
    held: Res<HeldCamera>,
    scene: Option<Res<TerrainScene>>,
    mut transforms: Query<&mut Transform>,
) {
    if held.world != ShotWorld::Region {
        return;
    }
    if let Some(scene) = scene
        && let Ok(mut camera) = transforms.get_mut(scene.camera)
    {
        camera.set_if_neq(held.transform);
    }
}

fn hold_globe_camera(held: Res<HeldCamera>, mut cameras: Query<&mut Transform, With<GlobeCamera>>) {
    if held.world != ShotWorld::Globe {
        return;
    }
    for mut camera in &mut cameras {
        camera.set_if_neq(held.transform);
    }
}

/// The player's first input lets the chase camera go.
fn release_held_camera(
    held: Option<Res<HeldCamera>>,
    keys: Option<Res<ButtonInput<KeyCode>>>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    touches: Option<Res<Touches>>,
    mut commands: Commands,
) {
    if held.is_none() {
        return;
    }
    let key = keys.is_some_and(|k| k.get_just_pressed().any(|&code| code != SHOT_KEY));
    let button = mouse.is_some_and(|m| m.get_just_pressed().next().is_some());
    let scrolled = scroll.is_some_and(|s| s.delta != Vec2::ZERO);
    let touched = touches.is_some_and(|t| t.iter_just_pressed().next().is_some());
    if key || button || scrolled || touched {
        info!("from shot: the camera is released");
        commands.remove_resource::<HeldCamera>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn circular_gap(a: f64, b: f64) -> f64 {
        let d = (a - b).rem_euclid(360.0);
        d.min(360.0 - d)
    }

    proptest! {
        #[test]
        fn shot_names_parse_back_to_the_pose(
            millis in 0u64..4_102_444_800_000,
            lat in prop_oneof![-90.0f64..=90.0, Just(-0.0002), Just(90.0), Just(-90.0)],
            lon in prop_oneof![-180.0f64..=180.0, Just(180.0), Just(-179.9999)],
            altitude in prop_oneof![-500.0f64..60_000.0, 1.0e6f64..4.0e7],
            heading in prop_oneof![0.0f64..360.0, Just(359.7), Just(0.0)],
            pitch in -90.0f64..=90.0,
            era in prop_oneof![0.0f64..=540.0e6, Just(0.0), Just(21_000.4)],
            region in any::<bool>(),
        ) {
            let world = if region { ShotWorld::Region } else { ShotWorld::Globe };
            let utc = UtcTime::from_unix_millis(millis);
            let name = ShotName {
                utc,
                lat_deg: lat,
                lon_deg: lon,
                altitude_m: altitude,
                heading_deg: heading,
                look_pitch_deg: pitch,
                era_years: era,
                world,
            };
            let file = shot_name(&name);
            prop_assert!(file.ends_with(".png") && !file.contains(' ') && !file.contains('/'));
            let back = parse_shot_name(&file).expect(&file);
            let half = 0.5 * 10f64.powi(-(NAME_DECIMALS as i32)) + 1e-9;
            prop_assert!((back.lat_deg - lat).abs() <= half, "{file}");
            prop_assert!((back.lon_deg - lon).abs() <= half, "{file}");
            prop_assert!((back.altitude_m - altitude).abs() <= 0.5, "{file}");
            prop_assert!(circular_gap(back.heading_deg, heading) <= 0.5 + 1e-9, "{file}");
            prop_assert!((back.look_pitch_deg - pitch).abs() <= 0.5, "{file}");
            prop_assert!((back.era_years - era).abs() <= 0.5, "{file}");
            prop_assert_eq!(back.world, world);
            prop_assert_eq!(back.utc, UtcTime { millis: 0, ..utc });
        }
    }

    #[test]
    fn a_shot_name_reads_as_the_brief_gives_it() {
        let name = ShotName {
            utc: UtcTime::from_unix_millis(1_795_703_405_000),
            lat_deg: 22.87,
            lon_deg: 71.42,
            altitude_m: 8_100.6,
            heading_deg: 47.2,
            look_pitch_deg: 15.0,
            era_years: 0.0,
            world: ShotWorld::Region,
        };
        assert_eq!(
            shot_name(&name),
            "timefly_20261126-143005_22.870N_71.420E_8101m_h047_p15_0y_region.png"
        );
        assert_eq!(
            sidecar_name(&shot_name(&name)),
            "timefly_20261126-143005_22.870N_71.420E_8101m_h047_p15_0y_region.json"
        );
        for bad in [
            "timefly_20261126-143005_22.870N_71.420E_8101m_h047_p15_0y_moon.png",
            "timefly_20261126-143005_22.870X_71.420E_8101m_h047_p15_0y_region.png",
            "timefly_2026112-143005_22.870N_71.420E_8101m_h047_p15_0y_region.png",
            "shot_20261126-143005_22.870N_71.420E_8101m_h047_p15_0y_region.png",
            "timefly_20261126-143005_22.870N_71.420E_8101m_h047_p15_region.png",
        ] {
            assert_eq!(parse_shot_name(bad), None, "{bad}");
        }
    }

    /// A region shot's sidecar with values that need every bit of an f64.
    fn region_pose() -> ShotPose {
        let spec = RegionSpec {
            sea_level_anomaly_m: -120.123_456_789_012_34,
            ..RegionSpec::new(
                RenderDir(DVec3::new(0.1, 0.7, -0.3).normalize()),
                YearsBp(21_000.333_333_333_332),
                crate::region::REGION_SEED,
            )
        };
        let fly = FlyState {
            pos_m: Vec3::new(12_345.678, 8_100.25, -98_765.43),
            yaw: 0.123_456_7,
            pitch: -0.333_333_3,
        };
        let place = super::super::position::region_place(&fly, &spec);
        let camera = Transform::from_xyz(12_000.1, 9_000.2, -97_000.3)
            .looking_at(Vec3::new(12_345.678, 8_100.25, -98_765.43), Vec3::Y);
        let mut globe = GlobeFlight::start(-33.912_345_678_9, 151.198_765_432_1);
        globe.altitude_km = 12_345.678_901_234;
        shot_pose(ShotInputs {
            place,
            globe,
            look_offset_deg: -std::f64::consts::PI,
            region: Some((&spec, fly)),
            camera: &camera,
            utc: UtcTime::from_unix_millis(1_795_703_405_123),
            build: "0.1.0 (abc1234, release, aarch64-apple-darwin; system-spans)",
            session_id: "0123abcd",
            window: WindowShot {
                width: 2560,
                height: 1440,
                scale_factor: 2.0,
            },
        })
    }

    #[test]
    fn shots_at_the_same_place_and_instant_get_distinct_file_pairs() {
        let first = region_pose();
        let second = region_pose();
        assert_ne!(first.file, second.file);
        assert_ne!(sidecar_name(&first.file), sidecar_name(&second.file));
        let name = parse_shot_name(&first.file).unwrap();
        assert_eq!(parse_shot_name(&second.file), Some(name));
    }

    #[test]
    fn the_sidecar_round_trips() {
        let pose = region_pose();
        let json = serde_json::to_string_pretty(&pose).unwrap();
        let back: ShotPose = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pose);
        // It holds what the brief lists, and the name it gives agrees with it.
        let region = back.region.as_ref().unwrap();
        assert_eq!(region.spec().sea_level_anomaly_m, -120.123_456_789_012_34);
        assert_eq!(
            region.fly().pos_m,
            Vec3::new(12_345.678, 8_100.25, -98_765.43)
        );
        assert_eq!(back.fly_position, region.fly_pos_m);
        assert_eq!(back.globe.look_offset_deg, -std::f64::consts::PI);
        assert_eq!(back.build_sha, env!("TIMEFLY_GIT_SHA"));
        assert_eq!((back.window.width, back.window.height), (2560, 1440));
        assert_eq!(back.session_id, "0123abcd");
        assert_eq!(back.utc, "2026-11-26T14:30:05.123Z");
        let name = parse_shot_name(&back.file).unwrap();
        assert_eq!(name.world, ShotWorld::Region);
        assert!((name.lat_deg - back.lat_deg).abs() <= 0.0005);
        assert!((name.era_years - 21_000.0).abs() < 1e-9);
        // The camera's transform comes back to the bit.
        let camera = Transform::from_xyz(12_000.1, 9_000.2, -97_000.3)
            .looking_at(Vec3::new(12_345.678, 8_100.25, -98_765.43), Vec3::Y);
        assert_eq!(back.camera.transform(), camera);
        // A file that is not a sidecar, or a newer one, is refused.
        let dir = std::env::temp_dir().join(format!("timefly-shot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pose.json");
        std::fs::write(&path, &json).unwrap();
        assert_eq!(load_shot(&path), Ok(pose.clone()));
        std::fs::write(&path, json.replace("\"version\": 1", "\"version\": 2")).unwrap();
        assert!(load_shot(&path).unwrap_err().contains("version 2"));
        std::fs::write(&path, "{}").unwrap();
        assert!(load_shot(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_repro_window_is_the_shots_logical_size() {
        // A Retina shot: 2560 × 1440 physical at 2×. `--window` is logical (bevy_winit makes a
        // `LogicalSize`), so the repro asks for 1280 × 720, not 2560 × 1440.
        let pose = region_pose();
        assert_eq!(
            (
                pose.window.width,
                pose.window.height,
                pose.window.scale_factor
            ),
            (2560, 1440, 2.0)
        );
        let mut args = crate::args::LaunchArgs::default();
        repro_window(&mut args, &pose);
        assert_eq!(args.window, Some((1280, 720)));
        // `--window` wins.
        let mut args = crate::args::LaunchArgs {
            window: Some((800, 600)),
            ..Default::default()
        };
        repro_window(&mut args, &pose);
        assert_eq!(args.window, Some((800, 600)));
        // Odd sizes and fractional factors round; an unknown size or factor is safe.
        let shot = |width, height, scale_factor| WindowShot {
            width,
            height,
            scale_factor,
        };
        assert_eq!(shot(1281, 721, 1.0).logical_size(), Some((1281, 721)));
        assert_eq!(shot(3000, 2000, 1.5).logical_size(), Some((2000, 1333)));
        assert_eq!(shot(0, 0, 1.0).logical_size(), None);
        assert_eq!(shot(1280, 720, 0.0).logical_size(), Some((1280, 720)));
        assert_eq!(shot(1280, 720, f32::NAN).logical_size(), Some((1280, 720)));
    }

    #[test]
    fn the_shot_folder_falls_back_without_a_pictures_dir() {
        let pictures = PathBuf::from("/home/someone/Pictures");
        let data = PathBuf::from("/home/someone/.local/share/timefly");
        assert_eq!(
            shot_folder(Some(pictures.clone()), Some(data.clone())),
            Some(pictures.join("Time Fly"))
        );
        assert_eq!(
            shot_folder(None, Some(data.clone())),
            Some(data.join("shots"))
        );
        assert_eq!(shot_folder(None, None), None);
        // This machine has one of the two (native): the default is one of them.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let folder = default_shot_folder().expect("a pictures or data folder");
            assert!(
                folder.ends_with("Time Fly") || folder.ends_with("shots"),
                "{folder:?}"
            );
        }
    }

    #[test]
    fn region_centres_match_up_to_rounding() {
        let c = DVec3::new(0.1, 0.7, -0.3).normalize();
        assert_ne!(c.normalize(), c);
        assert!(same_centre(RenderDir(c), RenderDir(c.normalize())));
        // 10 m on the Earth is another region.
        let moved = (c + c.any_orthonormal_vector() * (10.0 / 6_371_000.0)).normalize();
        assert!(!same_centre(RenderDir(c), RenderDir(moved)));
        // So is the antipode, and a spec differing in anything else.
        assert!(!same_centre(RenderDir(c), RenderDir(-c)));
        let saved = region_pose().region.unwrap().spec();
        let generated = RegionSpec {
            center: RenderDir(saved.center.0.normalize()),
            ..saved
        };
        assert!(same_spec(&generated, &saved));
        assert!(!same_spec(
            &RegionSpec {
                inner_res: saved.inner_res + 1,
                ..generated
            },
            &saved
        ));
    }

    mod apps {
        use std::sync::Arc;
        use std::time::Duration;

        use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
        use bevy::time::TimeUpdateStrategy;
        use timefly_core::flight::globe::{GlobeInput, globe_fly_step};
        use timefly_core::worldgen::generate::{Field, RegionGrid};
        use timefly_core::worldgen::{APRON_SIZE_M, REGION_SIZE_M};

        use super::*;
        use crate::diagnostics::DiagnosticsContext;
        use crate::globe_flight::GlobeFlightPlugin;
        use crate::ui::position::FlyPlace;

        const STEP: Duration = Duration::from_micros(16_667);

        /// The globe flight and the HUD's place (`hud`: the whole HUD with bug shots), with a
        /// globe camera, at 60 fps.
        fn globe_app(hud: bool) -> (App, Entity) {
            let mut app = App::new();
            app.add_plugins(MinimalPlugins)
                .insert_resource(TimeUpdateStrategy::ManualDuration(STEP))
                .init_resource::<ButtonInput<KeyCode>>()
                .init_resource::<ButtonInput<MouseButton>>()
                .init_resource::<AccumulatedMouseMotion>()
                .init_resource::<AccumulatedMouseScroll>()
                .init_resource::<DiagnosticsContext>()
                .init_resource::<GlobeTime>()
                .init_resource::<FlyState>()
                .init_resource::<FlyChase>()
                .add_plugins(GlobeFlightPlugin)
                .add_plugins(ShotRestorePlugin);
            if hud {
                app.add_plugins((
                    crate::ui::altitude::AltitudeOverlayPlugin,
                    crate::ui::position::PositionOverlayPlugin,
                    BugShotPlugin,
                ));
            } else {
                app.init_resource::<FlyPlace>().add_systems(
                    Update,
                    crate::ui::position::update_fly_place
                        .after(FlySystems::Apply)
                        .after(GlobeFlightSystems::Apply),
                );
            }
            let camera = app
                .world_mut()
                .spawn((
                    GlobeCamera,
                    Camera::default(),
                    Projection::Perspective(PerspectiveProjection {
                        aspect_ratio: 16.0 / 9.0,
                        ..default()
                    }),
                    Transform::default(),
                ))
                .id();
            (app, camera)
        }

        fn camera(app: &App, entity: Entity) -> Transform {
            *app.world().get::<Transform>(entity).unwrap()
        }

        /// The button takes the window's top-right corner (fix round 2, the owner's request),
        /// above the scene's input, and keeps its click observer.
        #[test]
        fn the_shot_button_sits_in_the_top_right_corner() {
            use crate::ui::{CORNER_BUTTON_PX, GUTTER_PX};
            let (mut app, _) = globe_app(true);
            app.update();
            let world = app.world_mut();
            let mut q = world.query_filtered::<(Entity, &Node, Has<BlocksCamera>, Option<&ChildOf>), With<ShotButton>>();
            let (button, node, blocks, parent) = q.single(world).unwrap();
            assert_eq!(node.position_type, PositionType::Absolute);
            assert_eq!((node.top, node.right), (px(GUTTER_PX), px(GUTTER_PX)));
            assert_eq!(
                (node.width, node.height),
                (px(CORNER_BUTTON_PX.x), px(CORNER_BUTTON_PX.y))
            );
            assert!(blocks, "the button keeps the pointer from the camera");
            assert!(parent.is_none(), "a root node, not in the HUD's column");
            // Its click observer (it writes ShotRequested) comes with it.
            assert!(
                app.world()
                    .get::<bevy::ecs::observer::ObservedBy>(button)
                    .is_some_and(|o| !o.get().is_empty()),
                "the click observer"
            );
        }

        #[test]
        fn f12_fixes_the_name_and_the_sidecar_at_the_press() {
            let dir = std::env::temp_dir().join(format!("timefly-f12-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let (mut app, _) = globe_app(true);
            app.insert_resource(ShotFolder(Some(dir.clone())));
            app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(66.0e6);
            app.update();
            app.update();
            let screenshots = |app: &mut App| {
                let world = app.world_mut();
                world.query::<&Screenshot>().iter(world).count()
            };
            assert_eq!(screenshots(&mut app), 0);
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(SHOT_KEY);
            app.update();
            let last = app.world().resource::<LastShot>().clone();
            let pose = last.pose.expect("a shot was taken");
            let png = last.png.unwrap();
            assert_eq!(png, dir.join(&pose.file));
            // The sidecar is on disk already, the name agrees with the HUD's place, and the
            // picture is asked for (it resolves frames later).
            let sidecar = dir.join(sidecar_name(&pose.file));
            assert_eq!(load_shot(&sidecar), Ok(pose.clone()));
            let place = app.world().resource::<FlyPlace>().0.unwrap();
            let name = parse_shot_name(&pose.file).unwrap();
            assert_eq!(name.world, ShotWorld::Globe);
            assert!((name.lat_deg - place.lat_deg).abs() <= 0.0005);
            assert_eq!(name.era_years, 66.0e6);
            assert_eq!(screenshots(&mut app), 1);
            // No toast until the picture is written: it would be in the picture.
            assert_eq!(app.world().resource::<ShotToast>().shown_at, None);
            // A second press while the first is pending is ignored; the button asks the same.
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .release(SHOT_KEY);
            app.update();
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(SHOT_KEY);
            app.world_mut().write_message(ShotRequested);
            app.update();
            assert_eq!(screenshots(&mut app), 1);
            std::fs::remove_dir_all(&dir).unwrap();
        }

        #[test]
        fn a_globe_shot_below_the_dive_threshold_waits_for_input() {
            use crate::region::{RegionPlugin, navigate::RegionNavigationPlugin};
            use bevy::{asset::AssetPlugin, state::app::StatesPlugin};

            let (mut app, _) = globe_app(false);
            let mut pose = region_pose();
            pose.world = ShotWorld::Globe;
            pose.region = None;
            pose.globe.altitude_km = timefly_core::flight::altitudes::DIVE_ALTITUDE_KM - 1.0;
            app.add_plugins((
                StatesPlugin,
                AssetPlugin::default(),
                RegionPlugin,
                crate::time::TimePlugin,
                RegionNavigationPlugin,
            ))
            .insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 })
            .insert_resource(ShotRestore::new(pose));
            for _ in 0..10 {
                app.update();
                assert!(app.world().resource::<ShotRestore>().placed());
                assert_eq!(app.world().resource::<Navigation>().phase, NavPhase::Globe);
            }
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(KeyCode::KeyW);
            app.update();
            assert!(matches!(
                app.world().resource::<Navigation>().phase,
                NavPhase::Diving { .. }
            ));
        }

        #[test]
        fn from_shot_restores_the_pose() {
            // A shot mid-turn: the chase camera's glide still swinging, so the camera is not
            // where the chase wants it; the repro must restore the camera, not re-derive it.
            let (mut app, entity) = globe_app(false);
            app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(250.0e6);
            let mut flight = GlobeFlight::start(-22.0, -18.0);
            flight.altitude_km = 3_000.0;
            app.world_mut().resource_mut::<GlobeFlightState>().0 = flight;
            app.world_mut().resource_mut::<GlobeLook>().offset_deg = 4.5;
            app.update();
            for _ in 0..5 {
                let mut f = app.world().resource::<GlobeFlightState>().0;
                f = globe_fly_step(
                    f,
                    GlobeInput {
                        forward: 1.0,
                        strafe: 0.0,
                        turn_rad: 0.05,
                        climb: 0.0,
                        boost: false,
                        dt_s: STEP.as_secs_f64(),
                    },
                );
                app.world_mut().resource_mut::<GlobeFlightState>().0 = f;
                app.update();
            }
            let place = app
                .world()
                .resource::<FlyPlace>()
                .0
                .expect("the globe shows the fly");
            let shot_camera = camera(&app, entity);
            let wanted = timefly_core::flight::chase::globe_chase_pose(
                &app.world().resource::<GlobeFlightState>().0,
                4.5,
            );
            assert!(
                (shot_camera.translation.as_dvec3() - wanted.eye).length() > 1.0,
                "the fixture must catch the glide mid-swing"
            );
            let pose = shot_pose(ShotInputs {
                place,
                globe: app.world().resource::<GlobeFlightState>().0,
                look_offset_deg: app.world().resource::<GlobeLook>().offset_deg,
                region: None,
                camera: &shot_camera,
                utc: UtcTime::from_unix_millis(0),
                build: "test",
                session_id: "test",
                window: WindowShot {
                    width: 1280,
                    height: 720,
                    scale_factor: 1.0,
                },
            });
            // Through the file, as `--from-shot` reads it.
            let pose: ShotPose =
                serde_json::from_str(&serde_json::to_string(&pose).unwrap()).unwrap();

            // A fresh game: nothing happens until the globe is interactive.
            let (mut app, entity) = globe_app(false);
            app.insert_resource(GlobeRenderStatus::PreparingGpu)
                .insert_resource(ShotRestore::new(pose.clone()));
            app.update();
            assert!(!app.world().resource::<ShotRestore>().placed());
            assert_eq!(app.world().resource::<GlobeTime>().years, YearsBp(0.0));
            *app.world_mut().resource_mut::<GlobeRenderStatus>() =
                GlobeRenderStatus::Interactive { first_frame: 1 };
            for _ in 0..30 {
                app.update();
                let restored = app.world().resource::<ShotRestore>().placed();
                assert!(restored);
                // Same era, same fly, same drag offset.
                assert_eq!(app.world().resource::<GlobeTime>().years, YearsBp(250.0e6));
                let f = app.world().resource::<GlobeFlightState>().0;
                assert_eq!(f.dir.to_array(), pose.globe.dir);
                assert_eq!(f.heading.to_array(), pose.globe.heading);
                assert_eq!(f.altitude_km, pose.globe.altitude_km);
                assert_eq!(app.world().resource::<GlobeLook>().offset_deg, 4.5);
                // The same camera, every frame while nothing is pressed.
                let c = CameraShot::of(&camera(&app, entity));
                let gap = |a: [f64; 3], b: [f64; 3]| {
                    (DVec3::from_array(a) - DVec3::from_array(b)).length()
                };
                assert!(gap(c.eye, pose.camera.eye) < 1e-6, "eye {:?}", c.eye);
                assert!(
                    gap(c.target, pose.camera.target) < 1e-6,
                    "target {:?}",
                    c.target
                );
                assert!(gap(c.up, pose.camera.up) < 1e-6);
                // And the HUD reads the same place.
                let p = app.world().resource::<FlyPlace>().0.unwrap();
                assert_eq!((p.lat_deg, p.lon_deg), (pose.lat_deg, pose.lon_deg));
                assert_eq!(p.heading_deg, pose.heading_deg);
                assert_eq!(p.look_pitch_deg, pose.look_pitch_deg);
            }
            // The first key lets the chase take over again (F12 does not).
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(SHOT_KEY);
            app.update();
            assert!(app.world().get_resource::<HeldCamera>().is_some());
            app.world_mut()
                .resource_mut::<ButtonInput<KeyCode>>()
                .press(KeyCode::KeyW);
            app.update();
            assert!(app.world().get_resource::<HeldCamera>().is_none());
        }

        /// The region pipeline's stand-in: an enter request becomes the region `spec_for` builds
        /// (it normalises the direction again), with the shot's other fields. In `Last`: test
        /// messages are read with a reader there.
        fn enter_requested(
            mut requests: MessageReader<RegionRequested>,
            restore: Res<ShotRestore>,
            mut commands: Commands,
        ) {
            let Some(saved) = restore.pose.region.as_ref() else {
                return;
            };
            for request in requests.read() {
                let spec = RegionSpec {
                    center: RenderDir(request.era_dir.0.normalize_or_zero()),
                    snapshot: request.snapshot,
                    ..saved.spec()
                };
                let field = |size_m| Field::dry(2, size_m, vec![0.0; 4], vec![[0; 4]; 4]);
                commands.insert_resource(ActiveRegion {
                    grid: Arc::new(RegionGrid {
                        spec,
                        inner: field(REGION_SIZE_M),
                        apron: field(APRON_SIZE_M),
                        boulders: Vec::new(),
                    }),
                    frame: spec.frame(),
                });
            }
        }

        #[test]
        fn from_shot_enters_its_region() {
            // A centre that `spec_for`'s second normalisation moves in the last bits (as about a
            // third of unit vectors do): the region it generates is still the shot's.
            let pose = region_pose();
            let saved = pose.region.clone().expect("a region shot");
            let c = DVec3::from_array(saved.center);
            assert_ne!(
                c.normalize(),
                c,
                "the fixture's centre must move when normalised again"
            );
            assert_eq!(pose.world, ShotWorld::Region);
            let (mut app, _) = globe_app(false);
            app.add_systems(Last, enter_requested)
                .insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 })
                .insert_resource(ShotRestore::new(pose.clone()));
            for _ in 0..10 {
                app.update();
            }
            let generated = app.world().resource::<ActiveRegion>().grid.spec.center.0;
            assert_ne!(generated, c, "the stand-in must generate the moved centre");
            assert!(
                app.world().resource::<ShotRestore>().placed(),
                "the repro never entered its region"
            );
            assert_eq!(*app.world().resource::<FlyState>(), saved.fly());
            assert_eq!(
                app.world().resource::<GlobeTime>().years,
                YearsBp(pose.era_years)
            );
            let held = *app.world().resource::<HeldCamera>();
            assert_eq!(held.world, ShotWorld::Region);
            assert_eq!(held.transform, pose.camera.transform());
        }

        #[test]
        fn from_shot_gives_up_on_a_region_that_does_not_come() {
            let stage = |app: &App| app.world().resource::<ShotRestore>().stage;
            let asked = |state: Option<RegionState>| {
                let (mut app, _) = globe_app(false);
                app.insert_resource(GlobeRenderStatus::Interactive { first_frame: 1 })
                    .insert_resource(ShotRestore::new(region_pose()));
                if let Some(state) = state {
                    app.insert_resource(State::new(state));
                }
                app.update();
                assert_eq!(stage(&app), RestoreStage::Requested);
                app
            };
            // On the globe before the request is read: still waiting.
            let mut app = asked(Some(RegionState::Globe));
            app.update();
            assert_eq!(stage(&app), RestoreStage::Requested);
            // Generating, then the globe again: the load failed.
            app.insert_resource(State::new(RegionState::Generating));
            app.update();
            assert_eq!(stage(&app), RestoreStage::Requested);
            app.insert_resource(State::new(RegionState::Globe));
            app.update();
            assert_eq!(stage(&app), RestoreStage::GaveUp);
            assert!(app.world().get_resource::<HeldCamera>().is_none());
            // No region at all: given up once the wait runs out, not before.
            let mut app = asked(None);
            let clock = app.world().resource::<LoadClock>().elapsed_s;
            app.world_mut().resource_mut::<LoadClock>().elapsed_s = clock + RESTORE_GIVE_UP_S - 1.0;
            app.update();
            assert_eq!(stage(&app), RestoreStage::Requested);
            app.world_mut().resource_mut::<LoadClock>().elapsed_s += 1.0;
            app.update();
            assert_eq!(stage(&app), RestoreStage::GaveUp);
        }
    }
}
