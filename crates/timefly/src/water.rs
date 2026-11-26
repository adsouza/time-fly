//! The region's water (M2 design §6.5, spec §7.5; rivers and lakes design §4.4): the sea, the
//! lakes and the rivers, drawn from the fields' water textures (`water_image`), curved like the
//! terrain, out to the apron's edge.
//!
//! [`WaterMaterial`] extends `StandardMaterial`, so the water takes the region sun's light, the
//! terrain's shadows, the fog and the tonemapping exactly as the land does. Its extension binds
//! the terrain's own patch buffer (for the camera and the field layout) and the two water
//! textures, and `water.wesl` (importing `package::terrain_cdlod`, whose `blended_water` is the
//! WESL copy of `timefly_core::terrain::blended_water`):
//! - **Vertex:** a polar mesh centred on the camera ([`water_mesh`]), at the drawn surface (the
//!   water level, or the ground where that is higher: `blended_surface_m`), dropped by `d² / 2R`
//!   like the terrain, then pulled toward the camera along the view ray by [`WATER_DEPTH_PULL`]
//!   of the distance (plus [`WATER_PULL_M`]). That changes only depth, never the pixel, so the
//!   water wins depth ties against the terrain at its shore instead of z-fighting with it.
//! - **Fragment:** the water's thickness over the ground is `blended_water`'s at the fragment's
//!   surface point. Fragments over dry ground (thickness ≤ 0), and outside a river's true width
//!   (ribbon < 0), are discarded, so the shoreline is the fields' own zero contour, per pixel.
//!   Then the depth tint ([`water_tint`] on standing water, [`river_tint`] on rivers), foam
//!   where standing water is under 4 m thick (within [`FOAM_WIDTH_M`] of the edge on gentle
//!   coasts, [`foam_band_depth_m`]) broken up by `noise.wesl`, Bevy PBR lighting, and a Schlick
//!   Fresnel blend (f0 = 0.02) toward the sky's colour along the reflected ray.
//!
//! Rivers (Task 8): their ripples and foam scroll downstream along the flow, the flow-map
//! technique with two phases half a period apart ([`flow_phase_weights`], [`FLOW_PERIOD_S`]),
//! so the water moves without pulsing; in rapids (`rapid_from_speed` of the flow's speed) the
//! scroll is faster, the ripples rougher ([`RAPID_NORMAL_GAIN`]) and the water broken and white
//! ([`WHITE_WATER`]).
//!
//! Waves (art pass §5.4, `waves.wesl`):
//! - **Swell:** three directional components around the region's wind ([`water_waves`]). Below
//!   [`SWELL_FADE_AGL_M`] the vertex stage displaces the surface by them, each kept only where
//!   the mesh has [`SWELL_MIN_VERTICES`] per wavelength ([`swell_spacing_weight`]) and faded by
//!   the floor's depth ([`SWELL_FADE_DEPTH_M`]). The fragment stage tilts the normal by the same
//!   waves at every height, plus three long normal-only bands ([`BAND_WAVELENGTHS_M`]), so moving
//!   light shows on the sea from high up.
//! - **Swash:** in the shallows the land test becomes `depth + level ≤ 0`, where the level rises
//!   and falls by [`swash_amplitude_m`] over [`SWASH_PERIOD_S`] (zero on average, so the resting
//!   shoreline is unchanged), limited by [`swash_reach_factor`], with a foam line riding the
//!   edge. The vertex stage pulls the shallows toward the camera as if the sea were that much
//!   higher, so water laid over the beach still wins depth ties with the terrain.
//!
//! The camera never goes below the water (the fly's clearance over `blended_surface_m`), so there
//! is no underwater view. The water is opaque: the depth tint already stands for the floor
//! beneath it. On a region swap's first frame the scene binds no water: the sea is hidden and
//! its material binds [`NO_WATER`] until the new water textures exist.

use std::sync::Arc;

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::camera::visibility::{NoFrustumCulling, RenderLayers};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::pbr::{ExtendedMaterial, MaterialExtension, MaterialPlugin};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::render::storage::ShaderBuffer;
use bevy::shader::ShaderRef;
use timefly_core::worldgen::generate::RegionGrid;
use timefly_core::worldgen::wind::wind_axis;

use crate::sky::{region_look, sky_tint};
use crate::terrain::{TERRAIN_LAYER, TerrainScene, TerrainSceneSystems};

/// `water.wesl`: the sea's vertex and fragment shaders.
pub const WATER_SHADER: Handle<Shader> = uuid_handle!("6f2d0f0e-7a4b-4c2e-9d61-3b1f9a0c9e31");
/// What the water's material binds while the scene has no water textures (a swap's first
/// frame): a handle no image is ever stored at, so the material is never prepared (and the sea
/// is hidden besides). Binding the scene's empty handle instead would bind Bevy's default
/// image, a float texture where the shader declares `texture_2d<u32>`.
pub const NO_WATER: Handle<Image> = uuid_handle!("0b6b6f61-5a52-4f3e-9c1e-77c2a1d4e8b5");

/// The sea's fraction of the view distance by which it is pulled toward the camera (depth only).
pub const WATER_DEPTH_PULL: f32 = 6.0e-4;
/// The constant part of the pull (m).
pub const WATER_PULL_M: f32 = 1.0;
/// Foam lives where the sea floor is less than this deep (m), on coasts steep enough
/// ([`foam_band_depth_m`]); on gentler ones the band is kept within [`FOAM_WIDTH_M`].
pub const FOAM_DEPTH_M: f32 = 4.0;
/// The widest the shore's foam band gets on the ground (m): on a gentle shelf the
/// [`FOAM_DEPTH_M`] band would be kilometres wide, a white field that hides the swash's line.
pub const FOAM_WIDTH_M: f32 = 60.0;
/// The thinnest water (m) the foam band ever ends at, so a flat floor still has one.
pub const FOAM_MIN_DEPTH_M: f32 = 0.02;
/// The swash's foam line is the water thinner than this fraction of the swash amplitude.
pub const SWASH_LINE_FRACTION: f32 = 0.35;
/// ... and never thinner than this (m), where the amplitude is tiny.
pub const SWASH_LINE_MIN_M: f32 = 0.03;
/// Schlick's reflectance at normal incidence for water.
pub const WATER_F0: f32 = 0.02;
/// The water mesh's outer radius (m): past the apron's corner from its centre.
pub const WATER_RADIUS_M: f32 = 1_750_000.0;
/// Segments around the water mesh.
pub const WATER_SEGMENTS: u32 = 256;
/// The Earth's radius (m) the water's drop and horizon use, as the terrain's `lattice.z`.
const EARTH_RADIUS_M: f32 = (crate::globe::EARTH_RADIUS_KM * 1000.0) as f32;
/// The water reaches this fraction past the horizon (`water_radius_m`), so the mesh's rim and
/// its chord sag stay behind the curve.
pub const WATER_HORIZON_MARGIN: f32 = 0.05;

/// The water mesh's radius (m) seen from `camera_height_m` above the sea: [`WATER_RADIUS_M`],
/// or past the horizon where that is farther (M3 Task 7: above about 215 km the 1,750 km disc's
/// rim stood 0.35° over the true horizon as a 256-sided outline). The horizon is the tangent
/// distance `√(h (2R + h))` over the drop `d² / 2R`; the margin is [`WATER_HORIZON_MARGIN`].
/// Mirrored in `water.wesl`'s vertex stage, which stretches the mesh by this over
/// [`WATER_RADIUS_M`].
pub fn water_radius_m(camera_height_m: f32) -> f32 {
    let h = camera_height_m.max(0.0);
    let horizon = (h * (2.0 * EARTH_RADIUS_M + h)).sqrt();
    WATER_RADIUS_M.max(horizon * (1.0 + WATER_HORIZON_MARGIN))
}
/// The water mesh's ring spacing: this many metres near the camera, growing to
/// [`WATER_RING_GROWTH`] of the radius farther out.
pub const WATER_RING_M: f32 = 25.0;
pub const WATER_RING_GROWTH: f32 = 0.04;

/// The flow map's period (s): each of the two phases scrolls the ripples downstream for this
/// long, then restarts while the other shows. Divides [`crate::art::TIME_WRAP_S`].
pub const FLOW_PERIOD_S: f32 = 2.0;
/// The scale (m) of the ripples the flow carries.
pub const FLOW_NORMAL_SCALE_M: f32 = 6.0;
/// White water's foam strength in a full rapid.
pub const RAPID_FOAM: f32 = 1.0;
/// The scroll speeds up by `1 + RAPID_SCROLL × rapid`.
pub const RAPID_SCROLL: f32 = 1.0;
/// The ripples' slope grows by `1 + RAPID_NORMAL_GAIN × rapid`. A `water.wesl` constant.
pub const RAPID_NORMAL_GAIN: f32 = 3.0;
/// A fast mountain river's colour (sRGB): clear green-teal.
pub const RIVER_CLEAR: Color = Color::srgb(0.10, 0.32, 0.33);
/// A slow lowland river's colour (sRGB): silty brown.
pub const RIVER_SILTY: Color = Color::srgb(0.36, 0.30, 0.20);
/// Flow speeds (m/s): fully silty at or below the first, clear from the second.
pub const SILT_SPEED_MPS: [f32; 2] = [1.0, 3.0];
/// White water's colour (sRGB). A `water.wesl` constant (linear).
pub const WHITE_WATER: Color = Color::srgb(0.92, 0.94, 0.93);

/// The flow map's two phase weights at time `t_s`, shifted by `jitter` periods: triangles half
/// a period apart, each 0 where its phase restarts (its offset jumps back to none), so they
/// always sum to 1 and one of them always weighs at least 0.5. Mirrored in `water.wesl`, where
/// each phase `p` samples the ripples at `xz − flow × p × FLOW_PERIOD_S`.
pub fn flow_phase_weights(t_s: f32, jitter: f32) -> [f32; 2] {
    let p = (t_s / FLOW_PERIOD_S + jitter).rem_euclid(1.0);
    let q = (p + 0.5).rem_euclid(1.0);
    [1.0 - (2.0 * p - 1.0).abs(), 1.0 - (2.0 * q - 1.0).abs()]
}

/// How silty a river flowing at `speed_mps` is: 1 at [`SILT_SPEED_MPS`]`[0]` and slower, 0 at
/// `[1]` and faster (`1 − smoothstep`). Mirrored in `water.wesl`.
pub fn river_silt(speed_mps: f32) -> f32 {
    let [a, b] = SILT_SPEED_MPS;
    let t = ((speed_mps - a) / (b - a)).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// A river's own colour `depth_m` deep, flowing at `speed_mps`, before lighting: from
/// [`RIVER_CLEAR`] to [`RIVER_SILTY`] by [`river_silt`]; the clear part darkens with depth as
/// [`water_tint`] does (toward [`WATER_DEEP`] by `1 − e^(−d/scale)`), while silt keeps the
/// water opaque brown. Mirrored by `river_tint` in `water.wesl`.
pub fn river_tint(depth_m: f32, speed_mps: f32) -> Color {
    let silt = river_silt(speed_mps);
    let base = RIVER_CLEAR.to_linear().mix(&RIVER_SILTY.to_linear(), silt);
    let t = 1.0 - (-depth_m.max(0.0) / WATER_DEPTH_SCALE_M).exp();
    Color::LinearRgba(base.mix(&WATER_DEEP.to_linear(), t * (1.0 - silt)))
}

/// Water colour right at the shore (sRGB): a clear turquoise over sand.
pub const WATER_SHALLOW: Color = Color::srgb(0.27, 0.66, 0.64);
/// Water colour over the deep sea floor (sRGB): a dark navy.
pub const WATER_DEEP: Color = Color::srgb(0.035, 0.14, 0.26);
/// Water colour right at the shore as seen from altitude (sRGB): the shallow turquoise, calmed
/// to a sea teal so the shoreline does not glow as a bright rim from the air.
pub const WATER_SHALLOW_FAR: Color = Color::srgb(0.07, 0.27, 0.36);
/// Camera height above the sea (m) up to which the shallow tint is the clear-water one.
pub const WATER_CALM_FROM_M: f32 = 300.0;
/// Camera height above the sea (m) from which the shallow tint is fully the calmed one.
pub const WATER_CALM_TO_M: f32 = 3000.0;
/// The depth (m) over which the shallow colour gives way to the deep one, as `1 − e^(−d/scale)`.
pub const WATER_DEPTH_SCALE_M: f32 = 28.0;

/// The displaced swell's periods (s): three directional components. Each divides
/// [`crate::art::TIME_WRAP_S`], so the swell loops seamlessly at the clock's wrap.
pub const SWELL_PERIODS_S: [f32; 3] = [8.0, 9.0, 10.0];
/// The swell's wavelengths (m): deep-water dispersion `λ = g·T² / 2π` for
/// [`SWELL_PERIODS_S`], rounded to the metre.
pub const SWELL_WAVELENGTHS_M: [f32; 3] = [100.0, 126.0, 156.0];
/// Each swell component's direction relative to the region's wind (degrees, turning from `+x`
/// toward `+z`, so clockwise seen from above with north up): spread within ±25° so the sea
/// follows the wind without copying it.
pub const SWELL_SPREAD_DEG: [f32; 3] = [-25.0, 25.0, 5.0];
/// Each swell component's height amplitude (m): at most 1.5 m when all three crest together.
pub const SWELL_AMPLITUDE_M: f32 = 0.5;
/// Camera heights above the sea (m): full displacement below the first, none above the second.
pub const SWELL_FADE_AGL_M: [f32; 2] = [500.0, 1000.0];
/// Sea-floor depths (m): no displacement shallower than the first, full deeper than the second,
/// so the swell doesn't pile up on beaches.
pub const SWELL_FADE_DEPTH_M: [f32; 2] = [2.0, 10.0];
/// A displaced component keeps its full height while it has at least this many mesh vertices
/// per wavelength, and fades to none by three.
pub const SWELL_MIN_VERTICES: f32 = 4.0;
/// The slow normal-only bands' periods (s): long waves that only tilt the normal, so moving
/// light shows on the sea from high up. Each divides [`crate::art::TIME_WRAP_S`].
pub const BAND_PERIODS_S: [f32; 3] = [16.0, 24.0, 30.0];
/// The bands' wavelengths (m): deep-water dispersion for [`BAND_PERIODS_S`], rounded.
pub const BAND_WAVELENGTHS_M: [f32; 3] = [400.0, 900.0, 1400.0];
/// Each band's direction relative to the region's wind (degrees, as [`SWELL_SPREAD_DEG`]).
pub const BAND_SPREAD_DEG: [f32; 3] = [-30.0, 15.0, 40.0];
/// Each band's largest surface slope (the normal's tilt, as rise over run).
pub const BAND_SLOPE: f32 = 0.02;
/// The bands' strength varies over patches this large (m), so they don't tile the sea evenly.
pub const BAND_PATCH_M: f32 = 6000.0;
/// The swash's period (s): the water's edge runs up the beach and back once in this time.
/// Divides [`crate::art::TIME_WRAP_S`].
pub const SWASH_PERIOD_S: f32 = 8.0;
/// How far up and down the beach the swash's edge travels (m), whatever the slope, while the
/// beach is gentle enough for [`SWASH_MAX_AMPLITUDE_M`].
pub const SWASH_REACH_M: f32 = 25.0;
/// The swash's largest rise and fall of the water level (m).
pub const SWASH_MAX_AMPLITUDE_M: f32 = 0.5;
/// How far (m) the swash looks along the floor's slope for the sea (from land) or the land (from
/// the sea): it floods or drains a point only if the shore lies within this distance, so the
/// edge stays within about this far of the resting shoreline even where the floor flattens
/// (a gentle slope near the shore's 293 m texels, then a plateau barely above the sea, would
/// otherwise flood or drain hundreds of metres).
pub const SWASH_PROBE_M: f32 = 40.0;
/// The swash moves the water level only where the sea floor is shallower than this (m);
/// offshore, the swell owns the surface.
pub const SWASH_DEPTH_M: f32 = 3.0;
/// The swash's phase varies along the coast with noise at this scale (m), so the whole coast
/// doesn't pulse in lockstep.
pub const SWASH_PHASE_SCALE_M: f32 = 800.0;

/// The swash's vertical amplitude (m) on a sea floor of slope `floor_slope` (rise over run):
/// [`SWASH_REACH_M`] times the slope, capped at [`SWASH_MAX_AMPLITUDE_M`]. So the edge travels
/// the fixed reach on gentle beaches and less on steep ones, and a near-flat shore does not
/// flood hundreds of metres. Mirrored by `swash_amplitude_m` in `waves.wesl`.
pub fn swash_amplitude_m(floor_slope: f32) -> f32 {
    (SWASH_REACH_M * floor_slope).min(SWASH_MAX_AMPLITUDE_M)
}

/// How much of the swash level applies at a point `depth_m` below the resting sea (negative on
/// land), given the depth `probe_depth_m` found [`SWASH_PROBE_M`] along the floor's slope
/// toward the shore (downhill from land, uphill from the sea) and the local
/// [`swash_amplitude_m`]: all of it when the probe has crossed the shore by at least half the
/// amplitude, none when it has not crossed at all. Mirrored by `swash_reach_factor` in
/// `waves.wesl`.
pub fn swash_reach_factor(depth_m: f32, probe_depth_m: f32, amplitude_m: f32) -> f32 {
    // How far past the shore the probe landed (m of height): into the sea from land, onto land
    // from the sea.
    let crossed = if depth_m > 0.0 {
        -probe_depth_m
    } else {
        probe_depth_m
    };
    let t = (crossed / (0.5 * amplitude_m).max(1e-4)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The water thickness (m) at which the shore's foam band ends on a floor of slope `floor_slope`:
/// [`FOAM_DEPTH_M`] on coasts steep enough for that to be within [`FOAM_WIDTH_M`] of the edge
/// (today's look), less on gentler ones so the band stays about [`FOAM_WIDTH_M`] wide on the
/// ground, and never below [`FOAM_MIN_DEPTH_M`]. The band starts fading at 7.5 % of it (0.3 m
/// of 4 m). Mirrored in `water.wesl`'s fragment stage.
pub fn foam_band_depth_m(floor_slope: f32) -> f32 {
    (FOAM_WIDTH_M * floor_slope).clamp(FOAM_MIN_DEPTH_M, FOAM_DEPTH_M)
}

/// The swash foam line's width on the ground (m) on a floor of slope `floor_slope`: the water
/// thinner than [`SWASH_LINE_FRACTION`] of the amplitude (at least [`SWASH_LINE_MIN_M`]),
/// spread over the slope. About 9 m on a gentle beach; well under a metre on a steep one, where
/// [`octave_weight`] fades it out before it can alias. Mirrored in `water.wesl`.
pub fn swash_line_width_m(floor_slope: f32) -> f32 {
    (SWASH_LINE_FRACTION * swash_amplitude_m(floor_slope)).max(SWASH_LINE_MIN_M)
        / floor_slope.max(1e-4)
}

/// How much of a pattern `scale_m` across survives a pixel footprint of `footprint_m`: all of it
/// below a quarter of the scale, none from 0.6 of it, so nothing narrower than a couple of
/// pixels is drawn at strength. Mirrors `octave_weight` in `water.wesl`.
pub fn octave_weight(footprint_m: f32, scale_m: f32) -> f32 {
    let t = ((footprint_m / scale_m - 0.25) / (0.6 - 0.25)).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// The water's own colour over a sea floor `depth_m` below the surface, before lighting: the
/// shallow colour at 0 m, darkening monotonically toward the deep colour. Mirrored by
/// `water_tint` in `water.wesl` (from the same constants, through [`WaterLook`]).
pub fn water_tint(depth_m: f32) -> Color {
    let t = 1.0 - (-depth_m.max(0.0) / WATER_DEPTH_SCALE_M).exp();
    Color::LinearRgba(WATER_SHALLOW.to_linear().mix(&WATER_DEEP.to_linear(), t))
}

/// The sea's look uniform (`WaterLook` in `water.wesl`).
#[derive(ShaderType, Copy, Clone, Debug, PartialEq, Reflect)]
pub struct WaterLook {
    /// [`WATER_SHALLOW`], linear; `w`: [`WATER_DEPTH_SCALE_M`].
    pub shallow: Vec4,
    /// [`WATER_DEEP`], linear; `w`: [`FOAM_DEPTH_M`].
    pub deep: Vec4,
    /// The sky's zenith (linear) for the Fresnel reflection; `w`: [`WATER_F0`].
    pub sky_zenith: Vec4,
    /// The sky's horizon (linear); `w`: [`WATER_DEPTH_PULL`].
    pub sky_horizon: Vec4,
    /// `x`: [`WATER_PULL_M`]; `y`: [`WATER_CALM_FROM_M`]; `z`: [`WATER_CALM_TO_M`]; `w`: reserved.
    pub pull: Vec4,
    /// [`WATER_SHALLOW_FAR`], linear: the shallow colour from altitude (`w` reserved).
    pub shallow_far: Vec4,
    /// `x`: [`SWELL_AMPLITUDE_M`]; `y`, `z`: [`SWELL_FADE_AGL_M`]; `w`: [`SWELL_MIN_VERTICES`].
    pub swell: Vec4,
    /// `x`: [`SWASH_PERIOD_S`]; `y`: [`SWASH_PHASE_SCALE_M`]; `z`: [`SWASH_DEPTH_M`]; `w`:
    /// [`FOAM_WIDTH_M`]. The reach and the cap are `waves.wesl` constants (ruling R1).
    pub swash: Vec4,
    /// `xy`: the region-local wind direction (`wind_axis`), the way it blows; `zw`: unused.
    pub wind: Vec4,
    /// The water mesh's vertex spacing ([`water_vertex_spacing_m`]): `x`: [`WATER_RING_M`];
    /// `y`: [`WATER_RING_GROWTH`]; `z`: [`WATER_SEGMENTS`]; `w`: [`WATER_RADIUS_M`], which the
    /// vertex stage stretches to [`water_radius_m`] from high up.
    pub mesh: Vec4,
    /// `x`: [`BAND_SLOPE`]; `y`: [`BAND_PATCH_M`]; `z`, `w`: [`SWELL_FADE_DEPTH_M`], the depths
    /// over which every wave (displaced or normal-only) comes in from the shore.
    pub bands: Vec4,
    /// The rivers' flow map: `x`: [`FLOW_PERIOD_S`]; `y`: [`FLOW_NORMAL_SCALE_M`]; `z`:
    /// [`RAPID_FOAM`]; `w`: [`RAPID_SCROLL`].
    pub flow: Vec4,
    /// [`RIVER_CLEAR`], linear; `w`: [`SILT_SPEED_MPS`]`[0]`.
    pub river_clear: Vec4,
    /// [`RIVER_SILTY`], linear; `w`: [`SILT_SPEED_MPS`]`[1]`.
    pub river_silty: Vec4,
    /// The six directional waves ([`water_waves`]): the three displaced swell components, then
    /// the three normal-only bands.
    pub waves: [Vec4; 6],
}

/// The look for a region: the constants above and the region's sky.
pub fn water_look(grid: &RegionGrid) -> WaterLook {
    let (era, biome) = region_look(grid);
    let (zenith, horizon, _) = sky_tint(era, biome);
    let v = |c: Color| Vec3::from_slice(&c.to_linear().to_f32_array()[..3]);
    let wind = wind_axis(&grid.spec.frame()).0.as_vec2();
    WaterLook {
        shallow: v(WATER_SHALLOW).extend(WATER_DEPTH_SCALE_M),
        deep: v(WATER_DEEP).extend(FOAM_DEPTH_M),
        sky_zenith: v(zenith).extend(WATER_F0),
        sky_horizon: v(horizon).extend(WATER_DEPTH_PULL),
        pull: Vec4::new(WATER_PULL_M, WATER_CALM_FROM_M, WATER_CALM_TO_M, 0.0),
        shallow_far: v(WATER_SHALLOW_FAR).extend(0.0),
        swell: Vec4::new(
            SWELL_AMPLITUDE_M,
            SWELL_FADE_AGL_M[0],
            SWELL_FADE_AGL_M[1],
            SWELL_MIN_VERTICES,
        ),
        swash: Vec4::new(
            SWASH_PERIOD_S,
            SWASH_PHASE_SCALE_M,
            SWASH_DEPTH_M,
            FOAM_WIDTH_M,
        ),
        wind: wind.extend(0.0).extend(0.0),
        mesh: Vec4::new(
            WATER_RING_M,
            WATER_RING_GROWTH,
            WATER_SEGMENTS as f32,
            WATER_RADIUS_M,
        ),
        bands: Vec4::new(
            BAND_SLOPE,
            BAND_PATCH_M,
            SWELL_FADE_DEPTH_M[0],
            SWELL_FADE_DEPTH_M[1],
        ),
        flow: Vec4::new(FLOW_PERIOD_S, FLOW_NORMAL_SCALE_M, RAPID_FOAM, RAPID_SCROLL),
        river_clear: v(RIVER_CLEAR).extend(SILT_SPEED_MPS[0]),
        river_silty: v(RIVER_SILTY).extend(SILT_SPEED_MPS[1]),
        waves: water_waves(wind),
    }
}

/// The six directional waves for a region whose wind blows along `wind` (region-local xz, unit):
/// the swell components ([`SWELL_SPREAD_DEG`]) then the bands ([`BAND_SPREAD_DEG`]), each
/// `(dir.x, dir.z, wavelength m, period s)`. Mirrored by `wave_phase` in `waves.wesl`.
pub fn water_waves(wind: Vec2) -> [Vec4; 6] {
    let wave = |deg: f32, wavelength: f32, period: f32| {
        let dir = Vec2::from_angle(deg.to_radians()).rotate(wind);
        Vec4::new(dir.x, dir.y, wavelength, period)
    };
    core::array::from_fn(|i| {
        if i < 3 {
            wave(
                SWELL_SPREAD_DEG[i],
                SWELL_WAVELENGTHS_M[i],
                SWELL_PERIODS_S[i],
            )
        } else {
            let j = i - 3;
            wave(BAND_SPREAD_DEG[j], BAND_WAVELENGTHS_M[j], BAND_PERIODS_S[j])
        }
    })
}

/// The water mesh's vertex spacing (m) at radius `r_m`: the larger of the ring step and the
/// arc between segments. Mirrored in `water.wesl`'s vertex stage.
pub fn water_vertex_spacing_m(r_m: f32) -> f32 {
    WATER_RING_M
        .max(r_m * WATER_RING_GROWTH)
        .max(core::f32::consts::TAU * r_m / WATER_SEGMENTS as f32)
}

/// How much of a swell component of `wavelength_m` a mesh with `spacing_m` between vertices
/// carries: all of it with [`SWELL_MIN_VERTICES`] vertices per wavelength or more, none with
/// three or fewer. Mirrored by `swell_spacing_weight` in `waves.wesl`.
pub fn swell_spacing_weight(spacing_m: f32, wavelength_m: f32) -> f32 {
    let (full, none) = (wavelength_m / SWELL_MIN_VERTICES, wavelength_m / 3.0);
    let t = ((spacing_m - full) / (none - full)).clamp(0.0, 1.0);
    1.0 - t * t * (3.0 - 2.0 * t)
}

/// The ring radii of [`water_mesh`], from 0 out to [`WATER_RADIUS_M`].
pub fn water_rings() -> Vec<f32> {
    let mut rings = vec![0.0f32];
    let mut r = 0.0f32;
    while r < WATER_RADIUS_M {
        r = (r + WATER_RING_M.max(r * WATER_RING_GROWTH)).min(WATER_RADIUS_M);
        rings.push(r);
    }
    rings
}

/// The sea's mesh: a position-only polar grid in metres from the camera (the shader adds the
/// camera's xz and the curvature drop): a centre vertex, then [`water_rings`] of
/// [`WATER_SEGMENTS`] vertices, wound to face `+y`.
pub fn water_mesh() -> Mesh {
    let rings = water_rings();
    let seg = WATER_SEGMENTS;
    let mut positions: Vec<[f32; 3]> = vec![[0.0; 3]];
    for &r in &rings[1..] {
        for k in 0..seg {
            let a = k as f32 / seg as f32 * core::f32::consts::TAU;
            positions.push([r * a.cos(), 0.0, r * a.sin()]);
        }
    }
    let at = |ring: u32, k: u32| 1 + (ring - 1) * seg + k % seg;
    let mut indices = Vec::new();
    for k in 0..seg {
        indices.extend([0, at(1, k + 1), at(1, k)]);
    }
    for ring in 1..rings.len() as u32 - 1 {
        for k in 0..seg {
            let (a, b) = (at(ring, k), at(ring, k + 1));
            let (c, d) = (at(ring + 1, k), at(ring + 1, k + 1));
            indices.extend([a, b, d, a, d, c]);
        }
    }
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_indices(Indices::U32(indices))
}

/// The sea's material.
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterExtension>;

/// The water's extension of `StandardMaterial`: the terrain's patch buffer and the fields' water
/// textures (shared, never copied) and its look.
#[derive(Asset, AsBindGroup, Reflect, Debug, Clone)]
pub struct WaterExtension {
    #[storage(100, read_only)]
    pub patches: Handle<ShaderBuffer>,
    /// `TerrainScene::inner_water` (`Rgba32Uint`, read with `textureLoad`), or [`NO_WATER`].
    #[texture(101, sample_type = "u_int")]
    pub inner_water: Handle<Image>,
    #[texture(102, sample_type = "u_int")]
    pub apron_water: Handle<Image>,
    #[uniform(103)]
    pub look: WaterLook,
}

impl MaterialExtension for WaterExtension {
    fn vertex_shader() -> ShaderRef {
        WATER_SHADER.into()
    }
    fn fragment_shader() -> ShaderRef {
        WATER_SHADER.into()
    }
    // The sea neither casts shadows nor sits in a prepass (it pulls its depth toward the camera
    // in the main pass only).
    fn enable_prepass() -> bool {
        false
    }
    fn enable_shadows() -> bool {
        false
    }
}

/// The water's material over the terrain's patch buffer and the fields' water textures (shared
/// with the pipeline warm-up, `crate::terrain::warmup`, so its pipeline keys match).
pub fn water_material(
    patches: &Handle<ShaderBuffer>,
    inner_water: &Handle<Image>,
    apron_water: &Handle<Image>,
    look: WaterLook,
) -> WaterMaterial {
    WaterMaterial {
        base: StandardMaterial {
            base_color: Color::WHITE,
            perceptual_roughness: 0.12,
            // f0 = 0.16 × reflectance² = 0.02 (WATER_F0).
            reflectance: (WATER_F0 / 0.16).sqrt(),
            // The mesh follows the drawn surface (the ground beside the water), so far out, at a
            // grazing angle, a triangle stepping down to a river faces away from the camera:
            // culled, it cut distant water into dashes, one per mesh segment.
            cull_mode: None,
            ..default()
        },
        extension: WaterExtension {
            patches: patches.clone(),
            inner_water: inner_water.clone(),
            apron_water: apron_water.clone(),
            look,
        },
    }
}

/// The sea for the current region (present while its entity exists).
#[derive(Resource, Debug)]
pub struct RegionWater {
    /// The grid whose heights it reads.
    pub grid: Arc<RegionGrid>,
    pub entity: Entity,
    pub material: Handle<WaterMaterial>,
}

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        let water = Shader::from_wesl(
            include_str!("../assets/shaders/water.wesl"),
            "embedded://timefly/water.wesl",
        );
        let _ = app
            .world_mut()
            .resource_mut::<Assets<Shader>>()
            .insert(WATER_SHADER.id(), water);
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default());
        add_water_systems(app);
    }
}

/// The systems without the render plugin and shader, for headless tests.
pub(crate) fn add_water_systems(app: &mut App) {
    app.add_systems(Update, sync_water.after(TerrainSceneSystems));
}

/// The scene's water textures, when both exist: on a replace's swap frame the scene holds no
/// water (its handles are empty, `Handle::default()`, which Bevy's default image answers to).
fn scene_water(scene: &TerrainScene, images: &Assets<Image>) -> Option<[Handle<Image>; 2]> {
    let ready = |h: &Handle<Image>| *h != Handle::default() && images.contains(h);
    (ready(&scene.inner_water) && ready(&scene.apron_water))
        .then(|| [scene.inner_water.clone(), scene.apron_water.clone()])
}

/// Gathers the scene and the sea, decides (the sky's rule: build, rebind on a replaced grid,
/// remove), and applies it. Whenever the sea exists it binds the scene's water textures, or
/// [`NO_WATER`] and is hidden while the scene has none.
fn sync_water(
    mut commands: Commands,
    scene: Option<Res<TerrainScene>>,
    water: Option<ResMut<RegionWater>>,
    images: Res<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut visibility: Query<&mut Visibility>,
) {
    use crate::sky::{SkyAction, sky_action};
    let action = sky_action(
        scene.as_ref().map(|s| &s.grid),
        water.as_ref().map(|w| &w.grid),
    );
    let bound = scene.as_ref().and_then(|s| scene_water(s, &images));
    let [inner, apron] = bound.clone().unwrap_or([NO_WATER, NO_WATER]);
    let shown = if bound.is_some() {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    match (action, scene, water) {
        (SkyAction::Build, Some(scene), _) => {
            let material = materials.add(water_material(
                &scene.buffer,
                &inner,
                &apron,
                water_look(&scene.grid),
            ));
            let entity = commands
                .spawn((
                    Name::new("Region water"),
                    Mesh3d(meshes.add(water_mesh())),
                    MeshMaterial3d(material.clone()),
                    Transform::IDENTITY,
                    shown,
                    NoFrustumCulling,
                    RenderLayers::layer(TERRAIN_LAYER),
                ))
                .id();
            info!("region water built");
            commands.insert_resource(RegionWater {
                grid: scene.grid.clone(),
                entity,
                material,
            });
        }
        (SkyAction::Teardown, _, Some(water)) => {
            commands.entity(water.entity).despawn();
            commands.remove_resource::<RegionWater>();
            info!("region water removed");
        }
        (action @ (SkyAction::Retint | SkyAction::Keep), Some(scene), Some(mut water)) => {
            if action == SkyAction::Retint {
                if let Some(mut m) = materials.get_mut(&water.material) {
                    m.extension.look = water_look(&scene.grid);
                }
                water.grid = scene.grid.clone();
                info!("region water rebound to the replaced grid");
            }
            // A replace frees the old water on its swap frame and makes the new one the frame
            // after (`TerrainScene::steps_remaining`).
            let stale = materials.get(&water.material).is_some_and(|m| {
                m.extension.inner_water != inner || m.extension.apron_water != apron
            });
            if stale && let Some(mut m) = materials.get_mut(&water.material) {
                m.extension.inner_water = inner;
                m.extension.apron_water = apron;
                debug!(
                    bound = bound.is_some(),
                    "region water rebound to the scene's water"
                );
            }
            if let Ok(mut v) = visibility.get_mut(water.entity)
                && *v != shown
            {
                *v = shown;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::art::{assert_divides_wrap, wesl_const_f32, wesl_struct_fields};

    const WATER_WESL: &str = include_str!("../assets/shaders/water.wesl");
    const WAVES_WESL: &str = include_str!("../assets/shaders/waves.wesl");

    /// `λ = g·T² / 2π`, deep-water dispersion.
    fn deep_water_wavelength_m(period_s: f32) -> f32 {
        9.81 * period_s * period_s / core::f32::consts::TAU
    }

    #[test]
    fn wave_periods_divide_the_wrap() {
        for &t in SWELL_PERIODS_S.iter().chain(&BAND_PERIODS_S) {
            assert_divides_wrap(t);
        }
        assert_divides_wrap(SWASH_PERIOD_S);
        // The wavelengths follow deep-water dispersion (rounded, within 0.5 %), so each wave
        // travels at its natural speed.
        for (&t, &l) in SWELL_PERIODS_S
            .iter()
            .chain(&BAND_PERIODS_S)
            .zip(SWELL_WAVELENGTHS_M.iter().chain(&BAND_WAVELENGTHS_M))
        {
            let want = deep_water_wavelength_m(t);
            assert!((l - want).abs() < 0.005 * want, "{t} s: {l} m vs {want} m");
        }
        // The swell is 60–200 m and the bands 400–1,500 m (design §5.4, brief).
        assert!(
            SWELL_WAVELENGTHS_M
                .iter()
                .all(|l| (60.0..=200.0).contains(l))
        );
        assert!(
            BAND_WAVELENGTHS_M
                .iter()
                .all(|l| (400.0..=1500.0).contains(l))
        );
    }

    #[test]
    fn swash_amplitude_is_capped_by_slope() {
        let close = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(close(swash_amplitude_m(0.001), 0.025));
        assert!(close(swash_amplitude_m(0.001) / 0.001, SWASH_REACH_M));
        assert!(close(swash_amplitude_m(0.02), 0.5));
        assert!(close(swash_amplitude_m(0.5), 0.5));
        assert_eq!(swash_amplitude_m(0.0), 0.0);
        // The edge never travels farther than the reach, on any slope.
        for i in 1..=1000 {
            let slope = i as f32 * 1e-3;
            assert!(swash_amplitude_m(slope) / slope <= SWASH_REACH_M + 1e-4);
        }
    }

    /// The farthest a 1D shore profile's edge moves from the resting shoreline (m), modelled as
    /// the shader does it: elevation `h(x)` (land at `x > 0` near the shore), the slope as a
    /// central difference over one 293 m texel, the reach factor from a probe along the slope,
    /// and a point under water at high swash when `0 < h < amplitude · factor` (or dry at low
    /// swash when `-amplitude · factor < h < 0`).
    fn swash_extent_m(h: impl Fn(f32) -> f32) -> f32 {
        let texel = 293.0;
        let mut far = 0.0f32;
        for step in -4000..=4000 {
            let x = step as f32 * 0.25;
            let elevation = h(x);
            let gradient = (h(x + texel) - h(x - texel)) / (2.0 * texel);
            let amplitude = swash_amplitude_m(gradient.abs());
            // Toward the shore: downhill from land, uphill from the sea.
            let uphill = gradient.signum();
            let toward = if elevation > 0.0 { -uphill } else { uphill };
            let probe = -h(x + toward * SWASH_PROBE_M);
            let level = amplitude * swash_reach_factor(-elevation, probe, amplitude);
            if elevation.abs() < level {
                // The resting shoreline of these profiles is at x = 0.
                far = far.max(x.abs());
            }
        }
        far
    }

    #[test]
    fn swash_reach_is_bounded_where_the_floor_flattens() {
        // A plain beach of slope 0.001: the edge travels the reach.
        let beach = swash_extent_m(|x| 0.001 * x);
        assert!((beach - SWASH_REACH_M).abs() < 1.0, "beach: {beach} m");
        // A steep coast: the 0.5 m cap, so less.
        let cliff = swash_extent_m(|x| 0.1 * x);
        assert!(cliff <= 5.5, "cliff: {cliff} m");
        // A shore that rises 3 cm, then stays flat for 280 m before the land climbs (and the
        // sea floor mirrors it): the one-texel slope reaches the climb, so without the probe
        // the plateau would flood, and the shelf drain, for about 230 m.
        let shelf = |x: f32| {
            0.03 * (x / 20.0).clamp(-1.0, 1.0) + 0.01 * x.signum() * (x.abs() - 300.0).max(0.0)
        };
        let flat = swash_extent_m(shelf);
        assert!(flat <= SWASH_PROBE_M, "flattening shore: {flat} m");
        // At the shoreline itself the whole level applies from either side.
        assert_eq!(swash_reach_factor(-0.01, 1.0, 0.5), 1.0);
        assert_eq!(swash_reach_factor(0.01, -1.0, 0.5), 1.0);
        // No shore within the probe: none.
        assert_eq!(swash_reach_factor(-0.01, -0.01, 0.5), 0.0);
        assert_eq!(swash_reach_factor(0.01, 0.01, 0.5), 0.0);
        // A flat floor (no amplitude) never divides by zero.
        assert!(swash_reach_factor(0.01, -0.01, 0.0).is_finite());
    }

    #[test]
    fn foam_band_stays_narrow_on_flat_coasts() {
        // Steep coasts keep the M2 band: FOAM_DEPTH_M thick.
        for slope in [FOAM_DEPTH_M / FOAM_WIDTH_M, 0.1, 0.5, 2.0] {
            assert_eq!(foam_band_depth_m(slope), FOAM_DEPTH_M, "slope {slope}");
        }
        // Gentler ones: about FOAM_WIDTH_M wide on the ground, not kilometres.
        for slope in [0.0005f32, 0.001, 0.003, 0.01, 0.03, 0.06] {
            let width = foam_band_depth_m(slope) / slope;
            assert!(
                (width - FOAM_WIDTH_M).abs() < 1e-3 * FOAM_WIDTH_M,
                "slope {slope}: {width} m"
            );
        }
        // A flat floor still has a band (no zero-width smoothstep in the shader).
        assert_eq!(foam_band_depth_m(0.0), FOAM_MIN_DEPTH_M);
        // Monotonic in the slope, so there is no seam where it changes.
        let mut last = 0.0;
        for i in 0..=2000 {
            let d = foam_band_depth_m(i as f32 * 1e-4);
            assert!(d >= last);
            last = d;
        }
        assert_eq!(
            wesl_const_f32(WATER_WESL, "FOAM_MIN_DEPTH_M"),
            FOAM_MIN_DEPTH_M
        );
        // The shader carries the same widths, and the band ends at its depth.
        assert!(
            WATER_WESL
                .contains("clamp(look.swash.w * floor_slope_m, FOAM_MIN_DEPTH_M, look.deep.w)")
        );
    }

    #[test]
    fn swash_line_fades_before_it_aliases() {
        // About a third of the reach wide on a gentle beach...
        let gentle = swash_line_width_m(0.01);
        assert!(
            (gentle - SWASH_LINE_FRACTION * SWASH_REACH_M).abs() < 1e-3,
            "{gentle}"
        );
        // ...under 2 m on a steep coast, once the amplitude is capped.
        assert!(swash_line_width_m(0.1) < 2.0);
        assert!(swash_line_width_m(0.5) < 0.4);
        // A flat floor never divides by zero.
        assert!(swash_line_width_m(0.0).is_finite());
        // Full strength while the footprint is under a quarter of the width; gone well before
        // the pixel is as wide as the line, on any slope.
        for slope in [0.0005f32, 0.005, 0.02, 0.1, 0.5, 1.0] {
            let width = swash_line_width_m(slope);
            assert_eq!(octave_weight(0.2 * width, width), 1.0, "slope {slope}");
            assert_eq!(octave_weight(0.6 * width, width), 0.0, "slope {slope}");
            assert_eq!(octave_weight(3.0 * width, width), 0.0, "slope {slope}");
        }
        assert_eq!(
            wesl_const_f32(WATER_WESL, "SWASH_LINE_FRACTION"),
            SWASH_LINE_FRACTION
        );
        assert_eq!(
            wesl_const_f32(WATER_WESL, "SWASH_LINE_MIN_M"),
            SWASH_LINE_MIN_M
        );
        // The shader fades the line by this weight.
        assert!(WATER_WESL.contains("octave_weight(footprint, line_width)"));
        assert!(WATER_WESL.contains("fn octave_weight(footprint_m: f32, scale_m: f32) -> f32 {\n    return 1.0 - smoothstep(0.25, 0.6, footprint_m / scale_m);"));
    }

    #[test]
    fn waves_wesl_mirrors_the_swash_constants() {
        // Ruling R1: terrain.wesl imports `swash_amplitude_m` with no WaterLook, so the reach
        // and the cap live in waves.wesl and must match these.
        assert_eq!(wesl_const_f32(WAVES_WESL, "SWASH_REACH_M"), SWASH_REACH_M);
        assert_eq!(
            wesl_const_f32(WAVES_WESL, "SWASH_MAX_AMPLITUDE_M"),
            SWASH_MAX_AMPLITUDE_M
        );
        assert!(WAVES_WESL.contains("fn swash_amplitude_m(floor_slope: f32) -> f32"));
        assert!(WAVES_WESL.contains("min(SWASH_REACH_M * floor_slope, SWASH_MAX_AMPLITUDE_M)"));
        assert_eq!(wesl_const_f32(WAVES_WESL, "SWASH_PROBE_M"), SWASH_PROBE_M);
        assert!(WAVES_WESL.contains(
            "fn swash_reach_factor(depth_m: f32, probe_depth_m: f32, amplitude_m: f32) -> f32"
        ));
    }

    #[test]
    fn swell_is_resolved_where_displaced() {
        // Ruling R2: with 25 m rings, the shortest displaced wavelength has at least
        // SWELL_MIN_VERTICES vertices per wavelength out to 600 m from the camera.
        let shortest = SWELL_WAVELENGTHS_M.iter().copied().fold(f32::MAX, f32::min);
        assert_eq!(shortest, 100.0);
        let rings = water_rings();
        let mut checked = 0;
        for w in rings.windows(2) {
            let r = w[0];
            if r > 600.0 {
                break;
            }
            let spacing = (w[1] - w[0]).max(core::f32::consts::TAU * r / WATER_SEGMENTS as f32);
            assert!(spacing * 4.0 <= 100.0, "ring {r}: spacing {spacing}");
            assert_eq!(water_vertex_spacing_m(r), spacing, "ring {r}");
            // Full height for every component there.
            for &l in &SWELL_WAVELENGTHS_M {
                assert!(swell_spacing_weight(spacing, l) > 0.9999, "ring {r}, {l} m");
            }
            checked += 1;
        }
        assert!(checked >= 24, "{checked} rings within 600 m");
        // Farther out, a component is never drawn with fewer than three vertices a wavelength,
        // and the weight only falls with distance (no seam where it fades).
        for &l in &SWELL_WAVELENGTHS_M {
            let mut last = 1.0f32;
            for &r in &rings {
                let spacing = water_vertex_spacing_m(r);
                let weight = swell_spacing_weight(spacing, l);
                if weight > 0.0 {
                    assert!(spacing < l / 3.0, "{l} m at ring {r}");
                }
                assert!(weight <= last, "{l} m at ring {r}");
                last = weight;
            }
            assert_eq!(last, 0.0);
        }
    }

    #[test]
    fn swell_follows_the_wind() {
        let wind = Vec2::new(0.6, -0.8);
        let waves = water_waves(wind);
        for (i, w) in waves.iter().enumerate() {
            let dir = w.truncate().truncate();
            assert!((dir.length() - 1.0).abs() < 1e-6, "wave {i}");
            let deg = dir.angle_to(wind).abs().to_degrees();
            let limit = if i < 3 { 25.0 } else { 40.0 };
            assert!(deg <= limit + 1e-3, "wave {i}: {deg} deg off the wind");
        }
        for i in 0..3 {
            assert_eq!(waves[i].z, SWELL_WAVELENGTHS_M[i]);
            assert_eq!(waves[i].w, SWELL_PERIODS_S[i]);
            assert_eq!(waves[i + 3].z, BAND_WAVELENGTHS_M[i]);
            assert_eq!(waves[i + 3].w, BAND_PERIODS_S[i]);
        }
    }

    /// A 3 x 3 region in the Red Sea.
    fn look_grid() -> RegionGrid {
        use timefly_core::deep_time::YearsBp;
        use timefly_core::geo::{geo_to_render, latlon_to_geo};
        use timefly_core::worldgen::generate::{Field, RegionSpec};
        let field = |res: u32, size_m: f64| {
            Field::dry(
                res,
                size_m,
                vec![-10.0; (res * res) as usize],
                vec![[0; 4]; (res * res) as usize],
            )
        };
        RegionGrid {
            spec: RegionSpec {
                inner_res: 3,
                apron_res: 3,
                ..RegionSpec::new(geo_to_render(latlon_to_geo(20.0, 37.5)), YearsBp(0.0), 1)
            },
            inner: field(3, 300_000.0),
            apron: field(3, 2_400_000.0),
            boulders: Vec::new(),
        }
    }

    #[test]
    fn water_look_matches_its_wesl_mirror() {
        use timefly_core::worldgen::wind::wind_axis;
        let grid = look_grid();
        let look = water_look(&grid);
        let wind = wind_axis(&grid.spec.frame()).0.as_vec2();
        assert!((look.wind.truncate().truncate() - wind).length() < 1e-6);
        assert_eq!(look.waves, water_waves(wind));
        assert_eq!(look.swell, Vec4::new(0.5, 500.0, 1000.0, 4.0));
        assert_eq!(look.swash, Vec4::new(8.0, 800.0, 3.0, 60.0));
        assert_eq!(look.mesh, Vec4::new(25.0, 0.04, 256.0, 1_750_000.0));
        assert_eq!(look.bands, Vec4::new(0.02, 6000.0, 2.0, 10.0));
        // Task 8's rivers: the flow map, and the river tints with the silt's speeds.
        assert_eq!(look.flow, Vec4::new(2.0, 6.0, RAPID_FOAM, RAPID_SCROLL));
        let lin = |c: Color| Vec3::from_slice(&c.to_linear().to_f32_array()[..3]);
        assert_eq!(look.river_clear, lin(RIVER_CLEAR).extend(1.0));
        assert_eq!(look.river_silty, lin(RIVER_SILTY).extend(3.0));
        // Task 4's calm-from-altitude ramp.
        assert!(look.pull.y < look.pull.z);
        let names: Vec<String> = wesl_struct_fields(WATER_WESL, "WaterLook")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            [
                "shallow",
                "deep",
                "sky_zenith",
                "sky_horizon",
                "pull",
                "shallow_far",
                "swell",
                "swash",
                "wind",
                "mesh",
                "bands",
                "flow",
                "river_clear",
                "river_silty",
                "waves"
            ]
        );
    }

    #[test]
    fn flow_period_divides_the_wrap() {
        assert_divides_wrap(FLOW_PERIOD_S);
        // Half a period apart, the second phase loops at the wrap too.
        assert_divides_wrap(FLOW_PERIOD_S / 2.0);
    }

    #[test]
    fn the_two_flow_phases_never_fade_together() {
        let mut rng = 0x5EED_u64;
        let mut unit = || {
            rng = rng
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (rng >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut resets = [0, 0];
        for _ in 0..1000 {
            let t = unit() * crate::art::TIME_WRAP_S;
            let jitter = 2.0 * unit() - 1.0;
            let w = flow_phase_weights(t, jitter);
            assert!(
                (w[0] + w[1] - 1.0).abs() < 1e-5,
                "t {t}, jitter {jitter}: {w:?}"
            );
            assert!(
                w[0].max(w[1]) >= 0.5 - 1e-6,
                "t {t}, jitter {jitter}: {w:?}"
            );
            assert!(w.iter().all(|w| (0.0..=1.0).contains(w)), "{w:?}");
            for k in 0..2 {
                resets[k] += usize::from(w[k] < 0.02);
            }
        }
        // Each phase fades out (where its offset jumps back) in its turn.
        assert!(resets.iter().all(|&r| r > 5), "{resets:?}");
        // A phase's weight is 0 exactly where its offset restarts.
        assert_eq!(flow_phase_weights(0.0, 0.0), [0.0, 1.0]);
        assert_eq!(flow_phase_weights(FLOW_PERIOD_S / 2.0, 0.0), [1.0, 0.0]);
        // The shader mirrors it.
        assert!(WATER_WESL.contains(
            "fn flow_phase_weights(t_s: f32, jitter: f32) -> vec2<f32> {\n    let p = fract(t_s / look.flow.x + jitter);\n    let q = fract(p + 0.5);\n    return vec2<f32>(1.0 - abs(2.0 * p - 1.0), 1.0 - abs(2.0 * q - 1.0));"
        ));
    }

    #[test]
    fn rapids_decode_from_speed() {
        use timefly_core::hydrology::params::{RAPID_SPEED_BASE_MS, RAPID_SPEED_GAIN_MS};
        use timefly_core::worldgen::generate::rivers::rapid_from_speed;
        assert_eq!(
            wesl_const_f32(WATER_WESL, "RAPID_SPEED_BASE_MPS"),
            RAPID_SPEED_BASE_MS as f32
        );
        assert_eq!(
            wesl_const_f32(WATER_WESL, "RAPID_SPEED_GAIN_MPS"),
            RAPID_SPEED_GAIN_MS as f32
        );
        assert!(WATER_WESL.contains(
            "fn rapid_from_speed(speed_mps: f32) -> f32 {\n    return clamp((speed_mps - RAPID_SPEED_BASE_MPS) / RAPID_SPEED_GAIN_MPS, 0.0, 1.0);"
        ));
        // Lowland rivers carry no white water; the steepest rapids are all white.
        assert_eq!(rapid_from_speed(1.0), 0.0);
        assert_eq!(rapid_from_speed(6.0), 1.0);
        assert_eq!(
            wesl_const_f32(WATER_WESL, "RAPID_NORMAL_GAIN"),
            RAPID_NORMAL_GAIN
        );
        // The shader reads the rapid from the flow's speed.
        assert!(WATER_WESL.contains("let rapid = rapid_from_speed(length(w.flow));"));
    }

    #[test]
    fn river_tint_is_browner_when_slow() {
        let brown = |c: Color| {
            let c = c.to_linear();
            c.red - c.blue
        };
        assert_eq!(river_silt(0.3), 1.0);
        assert_eq!(river_silt(SILT_SPEED_MPS[0]), 1.0);
        assert_eq!(river_silt(SILT_SPEED_MPS[1]), 0.0);
        assert_eq!(river_silt(5.0), 0.0);
        for depth in [0.5, 2.0, 10.0, 30.0] {
            assert_eq!(
                river_tint(depth, 0.5).to_linear(),
                river_tint(depth, 0.0).to_linear()
            );
            let mut last = f32::MAX;
            for k in 0..=60 {
                let speed = k as f32 * 0.1;
                let b = brown(river_tint(depth, speed));
                assert!(b <= last + 1e-6, "{depth} m at {speed} m/s");
                last = b;
            }
            assert!(brown(river_tint(depth, 0.5)) > brown(river_tint(depth, 4.0)) + 0.05);
        }
        // Slow and shallow is the silty colour; fast and shallow the clear one.
        assert_eq!(river_tint(0.0, 0.5).to_linear(), RIVER_SILTY.to_linear());
        assert_eq!(river_tint(0.0, 4.0).to_linear(), RIVER_CLEAR.to_linear());
        // Clear water darkens with depth; silty water stays silty.
        assert!(river_tint(30.0, 4.0).luminance() < river_tint(1.0, 4.0).luminance());
        assert_eq!(river_tint(30.0, 0.5).to_linear(), RIVER_SILTY.to_linear());
        assert_eq!(
            wesl_const_f32(WATER_WESL, "RIBBON_NONE_M"),
            timefly_core::hydrology::params::RIBBON_NONE_M as f32
        );
    }

    /// `const NAME: vec3<f32> = vec3<f32>(r, g, b);` in a WESL source.
    fn wesl_const_vec3(src: &str, name: &str) -> Vec3 {
        let decl = format!("const {name}: vec3<f32> = vec3<f32>(");
        let start = src.find(&decl).unwrap_or_else(|| panic!("const {name}")) + decl.len();
        let end = start + src[start..].find(')').unwrap();
        let v: Vec<f32> = src[start..end]
            .split(',')
            .map(|c| c.trim().parse().unwrap())
            .collect();
        Vec3::from_slice(&v)
    }

    #[test]
    fn white_water_matches_its_wesl_mirror() {
        let want = Vec3::from_slice(&WHITE_WATER.to_linear().to_f32_array()[..3]);
        let got = wesl_const_vec3(WATER_WESL, "WHITE_WATER");
        assert!((got - want).abs().max_element() < 5e-4, "{got} vs {want}");
    }

    /// The mesh follows the ground beside the water, so it has slopes that face away from a
    /// distant camera: the water draws both faces (culled, far rivers broke into dashes, one per
    /// mesh segment), and its normal is its own, never flipped for the back face.
    #[test]
    fn the_water_draws_both_faces() {
        let m = water_material(
            &Handle::default(),
            &NO_WATER,
            &NO_WATER,
            water_look(&look_grid()),
        );
        assert_eq!(m.base.cull_mode, None);
        assert!(!m.base.double_sided);
    }

    /// The sea bound the terrain's two height textures; the water binds the two water textures
    /// in their place, and no height texture (Task 7: word 1 carries `level − h`).
    #[test]
    fn the_water_binds_no_more_textures_than_before() {
        use bevy::reflect::structs::Struct;
        let ext = WaterExtension {
            patches: Handle::default(),
            inner_water: Handle::default(),
            apron_water: Handle::default(),
            look: water_look(&look_grid()),
        };
        let images: Vec<&str> = (0..ext.field_len())
            .filter(|&i| {
                ext.field_at(i)
                    .and_then(|f| f.try_downcast_ref::<Handle<Image>>())
                    .is_some()
            })
            .map(|i| ext.name_at(i).unwrap())
            .collect();
        assert_eq!(images, ["inner_water", "apron_water"]);
        let textures: Vec<&str> = WATER_WESL
            .lines()
            .filter(|l| l.starts_with("@group(") && l.contains(": texture_"))
            .collect();
        assert_eq!(textures.len(), 2, "{textures:?}");
        assert!(
            textures.iter().all(|l| l.contains("texture_2d<u32>")),
            "{textures:?}"
        );
    }

    #[test]
    fn the_water_reaches_past_the_horizon_from_any_height() {
        use crate::fly_camera::CAMERA_CEILING_M;
        use crate::terrain::view::view_ranges;
        use timefly_core::flight::altitudes::{CHASE_DISTANCE_PER_AGL, CHASE_ELEVATION_DEG};
        let r = 6_371_000.0f32;
        let mut last = water_radius_m(0.0);
        assert_eq!(last, WATER_RADIUS_M);
        // From the ground to the chase eye over a fly at the cloud base (351 km), and beyond.
        for i in 0..=500 {
            let h = i as f32 * 1_000.0;
            let radius = water_radius_m(h);
            let horizon = (h * (2.0 * r + h)).sqrt();
            assert!(radius >= WATER_RADIUS_M, "{h} m: {radius}");
            assert!(
                radius >= horizon * (1.0 + WATER_HORIZON_MARGIN) * (1.0 - 1e-6),
                "{h} m: the rim at {radius} m is inside the horizon at {horizon} m"
            );
            // Grows with height, without a jump.
            assert!(radius >= last && radius - last < 0.01 * radius, "{h} m");
            last = radius;
        }
        // Below about 200 km the mesh keeps its radius (and its vertex spacing).
        assert_eq!(water_radius_m(200_000.0), WATER_RADIUS_M);
        // The horizon itself is never past the far plane, from any fly height up to the cloud
        // base (the chase eye sits up to CHASE_DISTANCE_PER_AGL × AGL behind and above).
        let rise = (CHASE_DISTANCE_PER_AGL * CHASE_ELEVATION_DEG.to_radians().sin()) as f32;
        for i in 0..=300 {
            let fly = (i as f32 * 1_000.0).min(CAMERA_CEILING_M);
            let eye = fly * (1.0 + rise);
            let tangent = (eye * (2.0 * r + eye)).sqrt();
            let far = view_ranges(fly).far_m;
            assert!(
                tangent < far,
                "fly {fly} m: horizon {tangent} m, far {far} m"
            );
        }
        // The shader mirrors it.
        assert_eq!(
            wesl_const_f32(WATER_WESL, "WATER_HORIZON_MARGIN"),
            WATER_HORIZON_MARGIN
        );
        assert!(WATER_WESL.contains(
            "let reach = max(look.mesh.w, sqrt(h * (2.0 * params.lattice.z + h)) * (1.0 + WATER_HORIZON_MARGIN));"
        ));
        assert!(WATER_WESL.contains("let rel = in.position.xz * (reach / look.mesh.w);"));
    }

    #[test]
    fn water_mesh_reaches_the_horizon_and_faces_up() {
        let rings = water_rings();
        assert_eq!(rings[0], 0.0);
        assert_eq!(*rings.last().unwrap(), WATER_RADIUS_M);
        // Beyond the apron's corner, so the sea covers every apron texel.
        assert!(WATER_RADIUS_M > 1_200_000.0 * 2f32.sqrt());
        for w in rings.windows(2) {
            let step = w[1] - w[0];
            assert!(step > 0.0);
            // f32 rounding of the running sum: an ulp or two at the outer radius.
            assert!(step <= WATER_RING_M.max(w[0] * WATER_RING_GROWTH) + 1e-6 * w[1] + 1e-3);
        }
        // The pull hides the mesh's chord sag: radially and around each ring.
        for &r in &rings {
            let step = WATER_RING_M.max(r * WATER_RING_GROWTH);
            let radial_sag = step * step / (8.0 * 6_371_000.0);
            let angle = core::f32::consts::PI / WATER_SEGMENTS as f32;
            let chord_in = r * (1.0 - angle.cos());
            let tangential_sag = r * chord_in / 6_371_000.0;
            let pull = WATER_PULL_M + WATER_DEPTH_PULL * r;
            assert!(radial_sag.max(tangential_sag) < pull, "ring {r}");
        }
        let mesh = water_mesh();
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(p)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("positions");
        };
        assert_eq!(p.len(), 1 + (rings.len() - 1) * WATER_SEGMENTS as usize);
        let Some(Indices::U32(idx)) = mesh.indices() else {
            panic!("indices");
        };
        for t in idx.chunks(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| Vec3::from(p[i as usize]));
            assert!((b - a).cross(c - a).y > 0.0, "a triangle faces down");
        }
    }

    #[test]
    fn water_depth_tint_is_monotonic() {
        let lum = |d: f32| water_tint(d).luminance();
        assert_eq!(water_tint(0.0).to_linear(), WATER_SHALLOW.to_linear());
        let mut last = lum(0.0);
        for step in 1..=400 {
            let d = step as f32 * 0.5;
            let l = lum(d);
            assert!(l < last, "{d} m: {l} not darker than {last}");
            last = l;
        }
        // Most of the way to the deep colour by 200 m, and never past it.
        let deep = WATER_DEEP.luminance();
        assert!(lum(200.0) - deep < 0.01 * (lum(0.0) - deep));
        assert!(lum(10_000.0) >= deep - 1e-6);
        // Negative depth (land above the plane) clamps to the shore colour.
        assert_eq!(water_tint(-5.0).to_linear(), WATER_SHALLOW.to_linear());
    }
}
