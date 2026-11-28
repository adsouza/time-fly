//! Ridged steps (rivers Task 11g): the steps from a node off today's burnt courses into a
//! course node that cross a ridge. Routing forbids them ([`Routing::with_blocked`]), so a
//! tributary drains into a burnt course only where its land falls that way.
//!
//! **Why.** The burn makes a course the lowest ground for a lattice spacing around, so steepest
//! descent pulls neighbouring nodes into it, even across a ridge the 10 km nodes cannot see.
//! At Hells Canyon the Salmon's node (45.480, −116.362) drained west into the burnt Snake across
//! an unbroken ridge (the 60″ crest on the step stands 726 m above both nodes), and the regions
//! carved a gash 1,167 m deep through it. The real Salmon runs north for 40 km first.
//!
//! **The rule.** A step from a lattice node `u` that is on no course to a course node `c` is
//! *ridged* when the 60″ terrain along the great circle between their centres, sampled every
//! [`RIDGE_SAMPLE_M`] (both ends included), climbs more than [`JOIN_RIDGE_M`] above the higher
//! of the two nodes' unburnt elevations (the overview at the node centres, as the pass samples
//! the present day). A course node is any node of any course of the shipped table
//! ([`course_nodes`]), whether or not a given era burns it.
//!
//! **The data.** The pass reads no 60″ terrain, so the steps are baked:
//! `timefly --bake-ridged-steps` (native, from the region tiles: ETOPO 2022 60″) writes
//! `data/hydrology/ridged_steps.bin`, compiled in here. It is stamped with the course table's
//! hash ([`table_hash`]), the lattice size and the threshold; a test fails when the stamp does
//! not match the shipped table, so a course change forces a re-bake. Format, little-endian:
//! `"TFRIDGE1"`, `n: u32`, `ridge_m: f32`, `table_hash: u64`, `count: u32`, then `count`
//! pairs `(feeder: u32, course node: u32)`, sorted and distinct.
//!
//! [`Routing::with_blocked`]: super::Routing::with_blocked
//! [`JOIN_RIDGE_M`]: super::params::JOIN_RIDGE_M

use std::sync::OnceLock;

use bevy_math::DVec3;

use super::today_rivers::{TODAY_RIVERS_JSON, TodayRiver, today_river_nodes};
use super::{CubeLattice, HydroGraph};
use crate::worldgen::frame::EARTH_RADIUS_M;

/// The shipped steps' bytes.
pub const RIDGED_STEPS: &[u8] = include_bytes!("../../../../data/hydrology/ridged_steps.bin");

const MAGIC: &[u8; 8] = b"TFRIDGE1";
const HEADER_BYTES: usize = 28;
/// The ridge test samples a step's 60″ terrain this often (m): a quarter of a minute of arc at
/// the equator, so the narrowest ridges the tiles hold are seen.
pub const RIDGE_SAMPLE_M: f64 = 250.0;

/// A set of ridged steps for one lattice and course table.
#[derive(Clone, Debug, PartialEq)]
pub struct RidgedSteps {
    /// The lattice's face size.
    pub n: u32,
    /// The threshold they were baked with (m).
    pub ridge_m: f32,
    /// [`table_hash`] of the course table they were baked from.
    pub table_hash: u64,
    /// `(feeder, course node)`, sorted and distinct.
    pub steps: Vec<(u32, u32)>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().expect("four bytes"))
}

impl RidgedSteps {
    /// Decodes a file, checking its structure.
    pub fn parse(bytes: &[u8]) -> Result<RidgedSteps, String> {
        if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
            return Err("not a ridged-steps file (bad magic or header)".into());
        }
        let n = u32_at(bytes, 8);
        let ridge_m = f32::from_bits(u32_at(bytes, 12));
        let table_hash = u64::from_le_bytes(bytes[16..24].try_into().expect("eight bytes"));
        let count = u32_at(bytes, 24) as usize;
        if bytes.len() != HEADER_BYTES + 8 * count {
            return Err(format!(
                "{count} steps need {} bytes, the file has {}",
                HEADER_BYTES + 8 * count,
                bytes.len()
            ));
        }
        let steps: Vec<(u32, u32)> = (0..count)
            .map(|k| {
                let at = HEADER_BYTES + 8 * k;
                (u32_at(bytes, at), u32_at(bytes, at + 4))
            })
            .collect();
        if !steps.windows(2).all(|w| w[0] < w[1]) {
            return Err("the steps are not sorted and distinct".into());
        }
        Ok(RidgedSteps {
            n,
            ridge_m,
            table_hash,
            steps,
        })
    }

    /// The file's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_BYTES + 8 * self.steps.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&self.n.to_le_bytes());
        out.extend_from_slice(&self.ridge_m.to_bits().to_le_bytes());
        out.extend_from_slice(&self.table_hash.to_le_bytes());
        out.extend_from_slice(&(self.steps.len() as u32).to_le_bytes());
        for &(u, c) in &self.steps {
            out.extend_from_slice(&u.to_le_bytes());
            out.extend_from_slice(&c.to_le_bytes());
        }
        out
    }
}

/// The shipped steps, decoded once. Its tests guarantee it decodes and matches the shipped
/// course table.
pub fn shipped_ridged_steps() -> &'static RidgedSteps {
    static STEPS: OnceLock<RidgedSteps> = OnceLock::new();
    STEPS.get_or_init(|| RidgedSteps::parse(RIDGED_STEPS).expect("the shipped ridged steps decode"))
}

/// The stamp of a course table: FNV-1a (64-bit) over its JSON's bytes.
pub fn table_hash(json: &str) -> u64 {
    json.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The shipped course table's stamp.
pub fn shipped_table_hash() -> u64 {
    table_hash(TODAY_RIVERS_JSON)
}

/// Every node of every course ([`today_river_nodes`]), sorted and distinct.
pub fn course_nodes(lattice: &CubeLattice, rivers: &[TodayRiver]) -> Vec<u32> {
    let mut out: Vec<u32> = rivers
        .iter()
        .flat_map(|r| today_river_nodes(lattice, r))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// How far the terrain along the great circle from `a` to `b` (unit vectors) climbs above
/// `ends_m`: its highest sample ([`RIDGE_SAMPLE_M`] apart, both ends included) minus `ends_m`.
pub fn step_crest_above(a: DVec3, b: DVec3, ends_m: f64, fine_at: &impl Fn(DVec3) -> f64) -> f64 {
    let len = a.angle_between(b) * EARTH_RADIUS_M;
    let k = (len / RIDGE_SAMPLE_M).ceil().max(1.0) as u32;
    let top = (0..=k)
        .map(|i| {
            let t = f64::from(i) / f64::from(k);
            fine_at((a * (1.0 - t) + b * t).normalize())
        })
        .fold(f64::NEG_INFINITY, f64::max);
    top - ends_m
}

/// The ridged steps into `course` (sorted course nodes): every `(u, c)` with `c` a course node
/// and `u` a lattice neighbour of it on no course whose step's terrain (`fine_at`) climbs more
/// than `ridge_m` above the higher of `unburnt(u)` and `unburnt(c)`. Sorted and distinct.
pub fn ridged_steps(
    lattice: &CubeLattice,
    course: &[u32],
    unburnt: &impl Fn(u32) -> f64,
    fine_at: &impl Fn(DVec3) -> f64,
    ridge_m: f64,
) -> Vec<(u32, u32)> {
    debug_assert!(course.windows(2).all(|w| w[0] < w[1]));
    let mut out = Vec::new();
    for &c in course {
        for u in lattice.neighbours(c).into_iter().flatten() {
            if course.binary_search(&u).is_ok() {
                continue;
            }
            let ends = unburnt(u).max(unburnt(c));
            if step_crest_above(lattice.dir(u).0, lattice.dir(c).0, ends, fine_at) > ridge_m {
                out.push((u, c));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hydrology::params::{HYDRO_RECENT_N, JOIN_RIDGE_M};
    use crate::hydrology::today_rivers::shipped_today_rivers;

    /// The stamp test (Task 11g): the shipped steps were baked from the shipped course table, at
    /// the recent lattice and today's threshold, and are small. A course change (or a new
    /// threshold) fails here until `timefly --bake-ridged-steps` is run again.
    #[test]
    fn the_shipped_ridged_steps_match_the_course_table() {
        assert!(
            RIDGED_STEPS.len() <= 256 * 1024,
            "{} bytes",
            RIDGED_STEPS.len()
        );
        let steps = shipped_ridged_steps();
        assert_eq!(
            steps.table_hash,
            shipped_table_hash(),
            "the course table changed: run `timefly --bake-ridged-steps` again"
        );
        assert_eq!(steps.n, HYDRO_RECENT_N);
        assert_eq!(f64::from(steps.ridge_m), JOIN_RIDGE_M);
        // Not vacuous, and each step is a lattice step from off the courses into one.
        assert!(steps.steps.len() > 1000, "{}", steps.steps.len());
        let lattice = CubeLattice { n: steps.n };
        let course = course_nodes(&lattice, shipped_today_rivers());
        for &(u, c) in &steps.steps {
            assert!(lattice.neighbours(c).contains(&Some(u)), "{u} -> {c}");
            assert!(course.binary_search(&c).is_ok(), "{c} is on no course");
            assert!(course.binary_search(&u).is_err(), "{u} is on a course");
        }
    }

    #[test]
    fn ridged_steps_round_trip_and_bad_files_are_errors() {
        let s = RidgedSteps {
            n: 900,
            ridge_m: 100.0,
            table_hash: 0x0123_4567_89ab_cdef,
            steps: vec![(1, 2), (1, 3), (7, 2)],
        };
        let bytes = s.to_bytes();
        assert_eq!(bytes.len(), 28 + 24);
        assert_eq!(RidgedSteps::parse(&bytes), Ok(s));
        assert!(RidgedSteps::parse(&bytes[..bytes.len() - 1]).is_err());
        assert!(RidgedSteps::parse(b"TFRIDGE0").is_err());
        let unsorted = RidgedSteps {
            steps: vec![(7, 2), (1, 2)],
            ..RidgedSteps::parse(&bytes).unwrap()
        };
        assert!(RidgedSteps::parse(&unsorted.to_bytes()).is_err());
        // The stamp moves with any byte of the table.
        assert_ne!(
            table_hash("{\"rivers\": []}"),
            table_hash("{\"rivers\": [] }")
        );
    }
}
