//! The water pass's constants (rivers and lakes plan, Global Constraints). Lengths are metres,
//! discharge m³/s, speeds m/s, and times seconds unless a name says otherwise.

// Lattice.

/// Cube-face size of the lattice for recent and bridge eras: a 10.2 km mean spacing.
pub const HYDRO_RECENT_N: u32 = 900;
/// Cube-face size of the lattice for deep eras: a 51 km mean spacing.
pub const HYDRO_DEEP_N: u32 = 180;
/// Eras younger than this (Myr) route in the present-day frame.
pub const PRESENT_FRAME_MAX_MYR: f64 = 0.2;

// Routing.

/// Below-sea-level water within this distance of the ocean counts as ocean (a strait).
pub const OCEAN_STRAIT_M: f64 = 25_000.0;
/// A node is in a depression when the filled surface stands more than this above it.
pub const DEPRESSION_EPS_M: f64 = 0.01;
/// A burnt course's corridor (Task 11d fix round 3) ends at its first interior node that the
/// ocean rule makes sea and that samples below this (m): open sea, not the channel and lagoon
/// floors (−0.2 to −7 m) ETOPO stores up estuaries. Natural Earth's lines sometimes run past the
/// coast; that node then serves as the course's end, and the sea beyond stays sea.
pub const OPEN_SEA_M: f32 = -10.0;
/// A node at or above this ice fraction carries no flow.
pub const ICE_NODE_FRACTION: f64 = 0.5;
/// Where the terrain is today's, the pass routes along today's major river courses
/// ([`today_rivers`](super::today_rivers), Task 11b) this far below the running minimum of the
/// era's node elevations along each course. Routing only: the river graph's bed there is the
/// running minimum itself.
pub const TODAY_RIVER_BURN_M: f64 = 10.0;
/// A burnt course's river node is relaxed by the regions to its anchor, the nearest point on
/// its course (Task 11b, fix round 2). The pass clamps each anchor to at most this many mean
/// lattice spacings from the node's centre, and the regions' gather and read reach allow for
/// it.
pub const TODAY_RIVER_ANCHOR_MAX_SPACINGS: f64 = 0.75;
/// A node off today's courses may not drain into a course node across a step whose 60″ terrain
/// climbs more than this above the higher of the two nodes' unburnt elevations
/// ([`ridged`](super::ridged), Task 11g). Chosen from the 0 ka census of the 4,645 joining links
/// (the 11g probe): at 100 m the rule stops 76 of the 81 links whose region chord cut more than
/// 500 m (Hells Canyon's Salmon among them, 726 m) and 241 of the 739 over 50 m, while it stops
/// 36 of the 3,906 joins that cut 50 m or less; re-routed, the joins over 500 m fall to 24, every
/// outlet bar still passes and no lake changes. At 300 m it stops only 48 of the 81; at 500 m, 16.
pub const JOIN_RIDGE_M: f64 = 100.0;

// Data lakes.

pub const DATA_LAKE_FLAT_TOL_M: f64 = 0.5;
pub const DATA_LAKE_MIN_NODES: u32 = 2;
pub const DATA_LAKE_MIN_BALANCE: f64 = 0.25;
/// A depression's data lake also needs `Q_in ≤ DATA_LAKE_MAX_BALANCE × capacity` of its flat:
/// far more water than the flat evaporates means the data's flat is a pit, not a lake surface,
/// and the water balance decides (fix round 1).
pub const DATA_LAKE_MAX_BALANCE: f64 = 10.0;
/// A flat outside every depression is a data lake (one that drains, like Victoria) when it has
/// at least this many connected nodes within [`DATA_LAKE_FLAT_TOL_M`] of its lowest.
pub const DATA_LAKE_FLAT_MIN_NODES: u32 = 20;
/// ... and when at most this many of its nodes have a strictly lower neighbour off the flat: a
/// lake drains through a narrow outlet, while a stair of F16 quantisation on a plateau or a
/// sloping delta has a long lower edge (fix round 2).
pub const DATA_LAKE_FLAT_MAX_OUTLETS: u32 = 3;
pub const DATA_LAKE_MAX_DEPTH_M: f64 = 50.0;
pub const DATA_LAKE_DEPTH_RAMP_NODES: f64 = 2.0;
/// A lake by the water balance must stand at least this deep over its basin's lowest node *(Task
/// 3's call)*. Shallower water over a 10 km node is noise in the data: an open basin passes its
/// inflow through, and a terminal one is a dry playa unless a river reaches it
/// ([`RIVER_MIN_DISCHARGE_M3S`]).
pub const MIN_LAKE_DEPTH_M: f64 = DATA_LAKE_FLAT_TOL_M;
/// Where the terrain is today's, an open lake whose level stands more than this above the
/// surface of the present-day mapped lake it overlaps drops to that surface (Task 8b): the
/// lattice dammed it above the real lake.
pub const OPEN_LAKE_MAP_TOL_M: f64 = 10.0;

// Rivers.

pub const RIVER_MIN_DISCHARGE_M3S: f64 = 20.0;
pub const APRON_RIVER_MIN_DISCHARGE_M3S: f64 = 200.0;
/// The oldest age (ka) the Beyer et al. 2020 climate covers.
pub const CLIMATE_MAX_KA: f64 = 120.0;
pub const LARGE_LAKE_KM2: f64 = 1_000.0;

// Hydraulic geometry: width `w = WIDTH_COEFF · Q^WIDTH_EXP`, depth `d = DEPTH_COEFF · Q^DEPTH_EXP`.

pub const WIDTH_COEFF: f64 = 5.0;
pub const WIDTH_EXP: f64 = 0.5;
pub const DEPTH_COEFF: f64 = 0.27;
pub const DEPTH_EXP: f64 = 0.39;
pub const MANNING_N: f64 = 0.035;
/// Hydraulic geometry reads discharge clamped to this *(Task 5's call)*: above the Amazon's
/// wet-season flow, so no real river is touched, and it bounds a channel's reach (its valley
/// and meanders), which the region's tile margin must cover ([`river_tile_margin_m`]).
///
/// [`river_tile_margin_m`]: super::river_tile_margin_m
pub const MAX_CHANNEL_DISCHARGE_M3S: f64 = 300_000.0;

// Speeds: lowland speeds lie in `LOWLAND_SPEED_MS`; rapids run at
// `RAPID_SPEED_BASE_MS + RAPID_SPEED_GAIN_MS · rapid`, where `rapid` is the smoothstep of the
// slope over `RAPID_SLOPE`.

pub const LOWLAND_SPEED_MS: [f64; 2] = [0.3, 3.0];
pub const RAPID_SPEED_BASE_MS: f64 = 3.0;
pub const RAPID_SPEED_GAIN_MS: f64 = 3.0;
pub const RAPID_SLOPE: [f64; 2] = [0.008, 0.03];

// Relaxation.

/// The corridor's half-width as a fraction of the lattice's mean spacing.
pub const CORRIDOR_FRACTION: f64 = 0.4;
pub const CORRIDOR_SAMPLES: u32 = 13;
/// The search grid around a node is `NODE_SEARCH_GRID x NODE_SEARCH_GRID`.
pub const NODE_SEARCH_GRID: u32 = 7;

// Sampling. The apron uses `max(SUBPOINT_M, apron texel / 2)`.

pub const SUBPOINT_M: f64 = 500.0;

// Incision.

pub const INCISION_WINDOW_M: f64 = 30_000.0;
/// The water level below the bank, as a fraction of the channel depth.
pub const LEVEL_BELOW_BED: f64 = 0.2;

// Meanders.

pub const MEANDER_WAVELENGTH_PER_WIDTH: f64 = 11.0;
pub const MEANDER_AMPLITUDE_PER_WIDTH: f64 = 1.5;
pub const MEANDER_FLAT_SLOPE: [f64; 2] = [5.0e-4, 5.0e-3];
pub const MEANDER_MIN_WAVELENGTH_M: f64 = 2_000.0;

// Valley.

pub const VALLEY_HALF_WIDTH_PER_WIDTH: f64 = 4.0;
pub const VALLEY_WALL_SLOPE: f64 = 0.3;
pub const BANK_FREEBOARD_M: f64 = 0.5;
pub const CHANNEL_MIN_TEXELS: f64 = 2.0;

// Encoding.

pub const NO_WATER_DROP_M: f64 = 64.0;
pub const RIBBON_NONE_M: f64 = 1_000.0;

// Boulders.

pub const BOULDERS_PER_SUBPOINT_MAX: u32 = 6;
pub const BOULDER_RADIUS_M: [f64; 2] = [0.4, 3.0];
pub const MAX_BOULDERS: u32 = 16_384;

// Shell.

pub const HYDRO_CACHE_ERAS: usize = 2;
pub const HYDRO_SETTLE_S: f64 = 0.5;
