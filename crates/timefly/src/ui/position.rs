//! The fly's place under the altitude (M3 Task 12): the latitude and longitude
//! (`22.87° N  71.42° E`, refreshed with the altitude, twice a second) and below it a small
//! heading-up compass rose: its top is where the fly faces, the N, E, S and W marks turn to show
//! the true directions, and its label gives the heading (`NE 47°`). Both show on the globe and
//! in regions, whichever world shows the fly ([`FlyAltitude`]).
//!
//! [`FlyPlace`] is the fly's place in one struct, also what a bug shot is named by
//! (`super::bug_shot`). It is the **era frame's** latitude and longitude: where the globe draws
//! the place at the era shown, the frame `--region LAT,LON,AGE_YEARS` and `--from-shot` take, so
//! a reading can be typed back in to return there. Today's coordinates of the ground below need
//! the plate model and do not exist for much of a deep era's crust (ocean floor subducted
//! since, era-fixed points), so the readout does not show them. Up to
//! [`GEOGRAPHIC_FRAME_MAX_YEARS`] the era frame *is* today's geographic frame and the two agree;
//! past it the readout is labelled ` (then)`, the place as it lay then.
//!
//! Nothing here allocates per frame: the texts are rebuilt into reused buffers and written only
//! when they change, and the rose's marks move only when the whole-degree heading changes.
//!
//! The UI font (Bevy's built-in Fira Mono subset) is ASCII only, so a `°` would draw as a
//! missing-glyph box: [`DegreeText`] splits a text at its degree signs and draws each as a
//! small ring between the pieces.

use std::time::Duration;

use bevy::prelude::*;
use timefly_core::deep_time::YearsBp;
use timefly_core::flight::chase::{globe_look_offset_range, globe_look_pitch_deg};
use timefly_core::flight::globe::GlobeFlight;
use timefly_core::flight::position::{
    compass_bearing_deg, region_bearing_deg, rose_angle_deg, rose_offset, whole_degrees,
    write_heading_label, write_lat_lon,
};
use timefly_core::geo::{RenderDir, geo_to_latlon, render_to_geo};
use timefly_core::worldgen::frame::RegionFrame;
use timefly_core::worldgen::generate::RegionSpec;

use super::TEXT_COLOR;
use super::altitude::{Altitude, FlyAltitude, HudColumn, spawn_altitude};
use crate::fly_camera::{FlyState, FlySystems, chase_look_pitch_deg};
use crate::globe_flight::{GlobeFlightState, GlobeFlightSystems, GlobeLook};
use crate::region::navigate::GEOGRAPHIC_FRAME_MAX_YEARS;
use crate::terrain::TerrainScene;
use crate::time::GlobeTime;

/// How often the latitude and longitude refresh, with the altitude.
const REFRESH: Duration = Duration::from_millis(500);
/// Decimal places of the readout (the owner's `22.87° N`).
pub const READOUT_DECIMALS: usize = 2;
/// The label after a readout in an era whose frame is not today's.
pub const THEN_LABEL: &str = " (then)";
/// The rose's box (logical pixels): a ring with the four marks just inside it.
const ROSE_PX: f32 = 46.0;
/// The marks' distance from the rose's centre.
const MARK_RADIUS_PX: f64 = 15.0;
/// Each mark's box, centred on its place.
const MARK_PX: f32 = 12.0;
/// North stands out from the other marks.
const NORTH_COLOR: Color = Color::srgb(1.0, 0.42, 0.34);

/// Which world shows the fly.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShotWorld {
    Globe,
    Region,
}

/// The fly's place, as the HUD shows it and a bug shot is named.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Place {
    pub world: ShotWorld,
    /// The era frame's latitude and longitude (degrees).
    pub lat_deg: f64,
    pub lon_deg: f64,
    /// Metres above sea level.
    pub altitude_m: f64,
    /// True compass bearing of the fly's heading (degrees clockwise from north).
    pub heading_deg: f64,
    /// How far below the horizontal the chase camera looks (degrees).
    pub look_pitch_deg: f64,
    /// The era shown: the region's snapshot in a region, the globe's time on the globe.
    pub era_years: f64,
}

impl Place {
    /// Whether the era's frame differs from today's, so the readout carries [`THEN_LABEL`].
    pub fn in_a_past_frame(&self) -> bool {
        self.era_years > GEOGRAPHIC_FRAME_MAX_YEARS
    }
}

/// The fly's place this frame, or `None` when no world shows the fly. Written after the
/// worlds place their cameras.
#[derive(Resource, Copy, Clone, Debug, Default, PartialEq)]
pub struct FlyPlace(pub Option<Place>);

/// The fly's place over the globe.
pub fn globe_place(flight: &GlobeFlight, look_offset_deg: f64, era: YearsBp) -> Place {
    let (lat_deg, lon_deg) = geo_to_latlon(render_to_geo(RenderDir(flight.dir)));
    let (lo, hi) = globe_look_offset_range(flight.altitude_km);
    Place {
        world: ShotWorld::Globe,
        lat_deg,
        lon_deg,
        altitude_m: flight.altitude_km * 1000.0,
        heading_deg: compass_bearing_deg(flight.dir, flight.heading),
        look_pitch_deg: globe_look_pitch_deg(flight.altitude_km) + look_offset_deg.clamp(lo, hi),
        era_years: era.0,
    }
}

/// The fly's place in the region `spec` describes.
pub fn region_place(fly: &FlyState, spec: &RegionSpec) -> Place {
    let frame = RegionFrame {
        center: spec.center,
    }
    .basis();
    let (x, z) = (f64::from(fly.pos_m.x), f64::from(fly.pos_m.z));
    let (lat_deg, lon_deg) = geo_to_latlon(render_to_geo(frame.local_to_dir(x, z)));
    Place {
        world: ShotWorld::Region,
        lat_deg,
        lon_deg,
        altitude_m: f64::from(fly.pos_m.y),
        heading_deg: region_bearing_deg(&frame, x, z, f64::from(fly.yaw)),
        look_pitch_deg: chase_look_pitch_deg(fly.pitch),
        era_years: spec.snapshot.0,
    }
}

/// Appends the readout for `place` to `out`: `22.87° N  71.42° E`, labelled ` (then)` in a
/// past era's frame.
pub fn write_readout(out: &mut String, place: &Place) {
    write_lat_lon(out, place.lat_deg, place.lon_deg, READOUT_DECIMALS);
    if place.in_a_past_frame() {
        out.push_str(THEN_LABEL);
    }
}

#[derive(Component)]
struct PositionText;

/// A row showing a text with degree signs: its text pieces, in order, with a small ring drawn
/// between each two (the UI font has no `°`).
#[derive(Component)]
pub struct DegreeText(Vec<Entity>);

impl DegreeText {
    /// Shows `text`, split at its `°`s; extra pieces are emptied. Writes only pieces that
    /// changed (no allocation once the pieces have grown to their widest).
    fn set(&self, text: &str, pieces: &mut Query<&mut Text>) {
        let mut parts = text.split('°');
        for &entity in &self.0 {
            let part = parts.next().unwrap_or("");
            if let Ok(mut piece) = pieces.get_mut(entity)
                && piece.0 != part
            {
                piece.0.clear();
                piece.0.push_str(part);
            }
        }
    }

    /// The text shown, with its degree signs.
    pub fn text(&self, world: &World) -> String {
        let pieces: Vec<&str> = self
            .0
            .iter()
            .filter_map(|&e| world.get::<Text>(e).map(|t| t.0.as_str()))
            .collect();
        pieces.join("°")
    }
}

/// Spawns a [`DegreeText`] row of `degrees` degree signs in `font_size` text of `color`.
fn spawn_degree_text(
    commands: &mut Commands,
    degrees: usize,
    font_size: f32,
    color: Color,
) -> (Entity, DegreeText) {
    let ring = font_size * 0.32;
    let mut pieces = Vec::with_capacity(degrees + 1);
    let row = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexStart,
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|row| {
            for i in 0..=degrees {
                pieces.push(
                    row.spawn((
                        Text::default(),
                        TextFont::from_font_size(font_size),
                        TextColor(color),
                        TextLayout::no_wrap(),
                        Pickable::IGNORE,
                    ))
                    .id(),
                );
                if i < degrees {
                    row.spawn((
                        Node {
                            width: px(ring),
                            height: px(ring),
                            margin: UiRect {
                                left: px(1),
                                right: px(1),
                                top: px(font_size * 0.2),
                                ..default()
                            },
                            border: UiRect::all(px(1)),
                            border_radius: BorderRadius::MAX,
                            ..default()
                        },
                        BorderColor::all(color),
                        Pickable::IGNORE,
                    ));
                }
            }
        })
        .id();
    (row, DegreeText(pieces))
}

/// The rose's row: the rose and its label.
#[derive(Component)]
struct RoseRow;

/// One of the rose's direction marks, at its true bearing (degrees).
#[derive(Component)]
struct RoseMark(f64);

#[derive(Component)]
struct HeadingText;

pub(super) struct PositionOverlayPlugin;

impl Plugin for PositionOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FlyPlace>()
            .add_systems(Startup, spawn_position.after(spawn_altitude))
            .add_systems(
                Update,
                update_fly_place
                    .after(FlySystems::Apply)
                    .after(GlobeFlightSystems::Apply),
            )
            .add_systems(PostUpdate, (update_readout, update_rose));
    }
}

pub(super) fn spawn_position(mut commands: Commands, column: Single<Entity, With<HudColumn>>) {
    let mark = |letter: &'static str, bearing: f64| {
        let color = if bearing == 0.0 {
            NORTH_COLOR
        } else {
            TEXT_COLOR.with_alpha(0.8)
        };
        (
            RoseMark(bearing),
            Node {
                position_type: PositionType::Absolute,
                width: px(MARK_PX),
                height: px(MARK_PX),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            Pickable::IGNORE,
            children![(
                Text::new(letter),
                TextFont::from_font_size(10.0),
                TextColor(color),
                TextLayout::no_wrap(),
                Pickable::IGNORE,
            )],
        )
    };
    let (readout, pieces) = spawn_degree_text(&mut commands, 2, 14.0, TEXT_COLOR.with_alpha(0.75));
    commands.entity(readout).insert((
        Name::new("Position readout"),
        PositionText,
        pieces,
        Visibility::Hidden,
    ));
    let (label, pieces) = spawn_degree_text(&mut commands, 1, 13.0, TEXT_COLOR.with_alpha(0.75));
    commands.entity(label).insert((HeadingText, pieces));
    let rose = commands
        .spawn((
            Name::new("Compass rose"),
            RoseRow,
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(6),
                ..default()
            },
            Visibility::Hidden,
            Pickable::IGNORE,
        ))
        .with_children(|row| {
            row.spawn((
                Node {
                    width: px(ROSE_PX),
                    height: px(ROSE_PX),
                    border: UiRect::all(px(1)),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BorderColor::all(TEXT_COLOR.with_alpha(0.35)),
                BackgroundColor(super::PANEL_COLOR.with_alpha(0.35)),
                Pickable::IGNORE,
            ))
            .with_children(|rose| {
                // The fly's heading: a tick at the top, fixed.
                rose.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(ROSE_PX / 2.0 - 2.0),
                        top: px(-1),
                        width: px(2),
                        height: px(6),
                        ..default()
                    },
                    BackgroundColor(TEXT_COLOR),
                    Pickable::IGNORE,
                ));
                rose.spawn(mark("N", 0.0));
                rose.spawn(mark("E", 90.0));
                rose.spawn(mark("S", 180.0));
                rose.spawn(mark("W", 270.0));
            });
        })
        .add_child(label)
        .id();
    commands.entity(*column).add_children(&[readout, rose]);
}

/// Writes [`FlyPlace`] from whichever world shows the fly.
pub(crate) fn update_fly_place(
    altitude: Res<FlyAltitude>,
    fly: Res<FlyState>,
    scene: Option<Res<TerrainScene>>,
    flight: Res<GlobeFlightState>,
    look: Res<GlobeLook>,
    time: Option<Res<GlobeTime>>,
    mut place: ResMut<FlyPlace>,
) {
    let next = match altitude.0 {
        Some(Altitude::Metres(_)) => scene.map(|s| region_place(&fly, &s.grid.spec)),
        Some(Altitude::Kilometres(_)) => Some(globe_place(
            &flight.0,
            look.offset_deg,
            time.map_or(YearsBp(0.0), |t| t.years),
        )),
        None => None,
    };
    if place.0 != next {
        place.0 = next;
    }
}

/// Refreshes the readout twice a second, rebuilding its text in a reused buffer.
fn update_readout(
    place: Res<FlyPlace>,
    time: Res<Time<Real>>,
    mut next_refresh: Local<Duration>,
    mut scratch: Local<String>,
    mut readout: Single<(&DegreeText, &mut Visibility), With<PositionText>>,
    mut pieces: Query<&mut Text>,
) {
    let now = time.elapsed();
    if now < *next_refresh {
        return;
    }
    *next_refresh = now + REFRESH;
    let (text, visibility) = &mut *readout;
    let Some(place) = place.0 else {
        visibility.set_if_neq(Visibility::Hidden);
        return;
    };
    scratch.clear();
    write_readout(&mut scratch, &place);
    text.set(&scratch, &mut pieces);
    visibility.set_if_neq(Visibility::Visible);
}

/// Turns the rose's marks and rewrites its label when the whole-degree heading changes.
fn update_rose(
    place: Res<FlyPlace>,
    mut shown: Local<Option<u32>>,
    mut scratch: Local<String>,
    mut row: Single<&mut Visibility, With<RoseRow>>,
    mut marks: Query<(&RoseMark, &mut Node)>,
    label: Single<&DegreeText, With<HeadingText>>,
    mut pieces: Query<&mut Text>,
) {
    let Some(place) = place.0 else {
        row.set_if_neq(Visibility::Hidden);
        *shown = None;
        return;
    };
    row.set_if_neq(Visibility::Visible);
    let degrees = whole_degrees(place.heading_deg);
    if *shown == Some(degrees) {
        return;
    }
    *shown = Some(degrees);
    let heading = f64::from(degrees);
    let centre = f64::from(ROSE_PX) / 2.0 - 1.0;
    let half = f64::from(MARK_PX) / 2.0;
    for (mark, mut node) in &mut marks {
        let (dx, dy) = rose_offset(rose_angle_deg(mark.0, heading), MARK_RADIUS_PX);
        let (left, top) = (
            px((centre + dx - half) as f32),
            px((centre + dy - half) as f32),
        );
        if node.left != left || node.top != top {
            node.left = left;
            node.top = top;
        }
    }
    scratch.clear();
    write_heading_label(&mut scratch, heading);
    label.set(&scratch, &mut pieces);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::time::TimeUpdateStrategy;
    use timefly_core::geo::{geo_to_render, latlon_to_geo};

    fn hud_app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins((
                super::super::altitude::AltitudeOverlayPlugin,
                PositionOverlayPlugin,
            ))
            .init_resource::<FlyState>()
            .init_resource::<GlobeFlightState>()
            .init_resource::<GlobeLook>()
            .init_resource::<GlobeTime>()
            .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
                250,
            )));
        app.update();
        app
    }

    fn readout(app: &mut App) -> (String, Visibility) {
        let world = app.world_mut();
        let (text, visibility) = world
            .query_filtered::<(&DegreeText, &Visibility), With<PositionText>>()
            .single(world)
            .unwrap();
        (text.text(world), *visibility)
    }

    fn heading(app: &mut App) -> String {
        let world = app.world_mut();
        let text = world
            .query_filtered::<&DegreeText, With<HeadingText>>()
            .single(world)
            .unwrap();
        text.text(world)
    }

    /// The north mark's centre relative to the rose's centre (x right, y down).
    fn north_mark(app: &mut App) -> (f32, f32) {
        let world = app.world_mut();
        let (_, node) = world
            .query::<(&RoseMark, &Node)>()
            .iter(world)
            .find(|(m, _)| m.0 == 0.0)
            .unwrap();
        let (Val::Px(left), Val::Px(top)) = (node.left, node.top) else {
            panic!("marks are placed in pixels");
        };
        let c = ROSE_PX / 2.0 - 1.0 - MARK_PX / 2.0;
        (left - c, top - c)
    }

    #[test]
    fn the_hud_shows_the_place_and_turns_the_rose() {
        let mut app = hud_app();
        assert_eq!(readout(&mut app).1, Visibility::Hidden);
        // Over the globe at 22.87° N 71.42° E, heading north-east at the present.
        let mut flight = GlobeFlight::start(22.8712, 71.4234);
        flight.altitude_km = 5_000.0;
        let east = flight.heading.cross(flight.dir).normalize();
        flight.heading = (flight.heading * 47f64.to_radians().cos()
            + east * 47f64.to_radians().sin())
        .normalize();
        let bearing = compass_bearing_deg(flight.dir, flight.heading);
        assert!((bearing - 47.0).abs() < 1e-6, "{bearing}");
        app.world_mut().resource_mut::<GlobeFlightState>().0 = flight;
        app.world_mut().resource_mut::<FlyAltitude>().0 = Some(Altitude::Kilometres(5_000.0));
        app.update();
        app.update();
        assert_eq!(
            readout(&mut app),
            ("22.87° N  71.42° E".to_string(), Visibility::Visible)
        );
        assert_eq!(heading(&mut app), "NE 47°");
        // Heading-up: north is 47° anticlockwise of the top.
        let (x, y) = north_mark(&mut app);
        let (sx, sy) = rose_offset(-47.0, MARK_RADIUS_PX);
        assert!(
            (x - sx as f32).abs() < 1e-3 && (y - sy as f32).abs() < 1e-3,
            "({x}, {y})"
        );
        // A deep era: the same place in that era's frame, labelled.
        app.world_mut().resource_mut::<GlobeTime>().years = YearsBp(250.0e6);
        app.update();
        app.update();
        assert_eq!(readout(&mut app).0, "22.87° N  71.42° E (then)");
        // Neither world shows the fly: hidden.
        app.world_mut().resource_mut::<FlyAltitude>().0 = None;
        app.update();
        app.update();
        assert_eq!(readout(&mut app).1, Visibility::Hidden);
    }

    #[test]
    fn a_region_place_reads_the_frame_at_the_fly() {
        let spec = RegionSpec::new(
            geo_to_render(latlon_to_geo(-22.0, -18.0)),
            YearsBp(250e6),
            1,
        );
        let fly = FlyState {
            pos_m: Vec3::new(0.0, 8_100.0, 0.0),
            yaw: -std::f32::consts::FRAC_PI_2,
            pitch: -20f32.to_radians(),
        };
        let place = region_place(&fly, &spec);
        assert!((place.lat_deg + 22.0).abs() < 1e-9 && (place.lon_deg + 18.0).abs() < 1e-9);
        // (An f32 quarter turn is 4e-8 rad off: 2.5e-6°.)
        assert!(
            (place.heading_deg - 90.0).abs() < 1e-4,
            "yaw −90° faces east"
        );
        assert_eq!(place.altitude_m, 8_100.0);
        assert_eq!(place.era_years, 250e6);
        assert!(place.in_a_past_frame());
        assert!((place.look_pitch_deg - chase_look_pitch_deg(fly.pitch)).abs() < 1e-12);
    }
}
