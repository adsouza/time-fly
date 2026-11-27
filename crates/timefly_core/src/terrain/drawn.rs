//! Boulders against the water as it is drawn (Task 11c fix round 2). The water shader draws a
//! river where [`blended_water`] has thickness over the ground and a non-negative ribbon: the
//! region's own texels, bilinearly blended. Rocks are placed on the routing geometry (the
//! sub-segments' arcs), which the 293 m texels reconstruct only approximately where a channel
//! bends or kinks, so a rock at the true bank could stand on drawn grass. The generator keeps a
//! rock only where a spread of texel lattices all draw water over it
//! (`water::hold_in_drawn_water`); this module
//! holds what both it and the tests share: a collar's footprint, the flow in region axes and
//! the shader's wet test.

use bevy_math::{DVec2, DVec3};

use super::blended_water;
use crate::geo::RenderDir;
use crate::worldgen::frame::{EARTH_RADIUS_M, FrameBasis, RegionFrame};
use crate::worldgen::generate::RegionGrid;
use crate::worldgen::generate::water::Boulder;

/// A foam collar's footprint, in its boulder's radii (Task 11c fix round 4): a ring this far
/// out all round, which holds the pillow piled on its upstream face and the boil over a rock
/// just under...
pub const COLLAR_RING_RADII: f64 = 2.7;
/// ...and a V wake downstream, as long as the collar's wake (radii), whose arms reach this far
/// either side of the flow line at the rock...
pub const COLLAR_WAKE_HALF_RADII: f64 = 1.6;
/// ...widening by this much a radius downstream.
pub const COLLAR_WAKE_SPREAD: f64 = 0.5;
/// A collar's wake is `COLLAR_WAKE_BASE_RADII + COLLAR_WAKE_RADII_PER_MPS × speed` radii long
/// (`collar_wake_radii`): longer in faster water...
pub const COLLAR_WAKE_BASE_RADII: f64 = 2.0;
pub const COLLAR_WAKE_RADII_PER_MPS: f64 = 1.5;
/// ...up to this.
pub const COLLAR_WAKE_MAX_RADII: f64 = 12.0;
/// Where the drawn water doesn't hold a collar's full wake, these shares of it are tried, the
/// longest first; a collar whose shortest doesn't fit isn't drawn.
pub const COLLAR_WAKE_FITS: [f64; 4] = [1.0, 0.6, 0.35, 0.2];
/// A rock is kept only where the drawn ribbon holds it with this much water (m) to spare, and a
/// collar's footprint only where the drawn ribbon reaches this far past it: the slack for a
/// region's own lattice drawing the bank differently from every one checked (on the test's
/// fixtures, by up to 0.9 m at 0.5).
pub const DRAWN_ROCK_MARGIN_M: f64 = 1.5;
/// ...and a rock or a footprint point needs this many times as much water again as the checked
/// lattices' ribbons disagree there, since a region's own lattice can draw the bank farther in
/// than any of them where they disagree.
pub const DRAWN_SPREAD_FACTOR: f64 = 1.5;

/// The flow at `dir` (`[east, north]` in `dir`'s own frame, m/s) as a unit vector in the
/// region's local `(x, z)` axes of `basis`: carried through the frames exactly, not by a sign
/// flip (they turn apart away from the region's centre). `None` where there is no flow.
pub fn flow_local(basis: &FrameBasis, dir: DVec3, flow_mps: [f32; 2]) -> Option<DVec2> {
    let (north, east) = RegionFrame {
        center: RenderDir(dir),
    }
    .north_east();
    let v = east * f64::from(flow_mps[0]) + north * f64::from(flow_mps[1]);
    let speed = v.length();
    if speed.is_nan() || speed <= 1e-6 {
        return None;
    }
    // 10 m along the flow.
    let a = 10.0 / EARTH_RADIUS_M;
    let ahead = (dir * a.cos() + v / speed * a.sin()).normalize();
    let (x0, z0) = basis.dir_to_local(RenderDir(dir))?;
    let (x1, z1) = basis.dir_to_local(RenderDir(ahead))?;
    DVec2::new(x1 - x0, z1 - z0).try_normalize()
}

/// Whether the water is drawn at local `(x, z)`: it stands over the ground and the point lies
/// within a river's ribbon (standing water's ribbon is huge), as the water shader keeps it.
pub fn drawn_wet(grid: &RegionGrid, x_m: f64, z_m: f64) -> bool {
    let w = blended_water(grid, x_m, z_m);
    w.thickness_m > 0.0 && w.ribbon_m >= 0.0
}

/// A collar's full wake (radii) in water flowing at `speed_mps`: it grows with the speed.
pub fn collar_wake_radii(speed_mps: f32) -> f64 {
    (COLLAR_WAKE_BASE_RADII + COLLAR_WAKE_RADII_PER_MPS * f64::from(speed_mps).max(0.0))
        .min(COLLAR_WAKE_MAX_RADII)
}

/// The points a collar's footprint is checked at, in local metres, for a rock of radius
/// `radius_m` at `(x, z)` whose flow runs along `down`: its ring ([`COLLAR_RING_RADII`] out,
/// every 30°) and, given a flow and a wake, its V wake: `wake_radii` downstream in four steps,
/// at each three points across its width: its flow line and `±(COLLAR_WAKE_HALF_RADII +
/// COLLAR_WAKE_SPREAD × y)`.
pub fn collar_footprint(
    x_m: f64,
    z_m: f64,
    radius_m: f32,
    down: Option<DVec2>,
    wake_radii: f64,
) -> Vec<DVec2> {
    let r = f64::from(radius_m);
    let c = DVec2::new(x_m, z_m);
    let mut points: Vec<DVec2> = (0..12)
        .map(|k| {
            let a = std::f64::consts::TAU * f64::from(k) / 12.0;
            c + DVec2::new(a.cos(), a.sin()) * COLLAR_RING_RADII * r
        })
        .collect();
    if let Some(d) = down.filter(|_| wake_radii > 0.0) {
        let across = DVec2::new(-d.y, d.x);
        for k in 1..=4 {
            let y = wake_radii * f64::from(k) / 4.0;
            let half = COLLAR_WAKE_HALF_RADII + COLLAR_WAKE_SPREAD * y;
            for s in [-1.0, 0.0, 1.0] {
                points.push(c + (d * y + across * (s * half)) * r);
            }
        }
    }
    points
}

/// Whether the water at local `(x, z)` is drawn from the inner field's texels alone: outside
/// the band at the inner field's edge where it blends toward the apron's (whose rivers carry
/// only the largest, so a smaller river fades out across the band).
pub fn drawn_from_the_inner_field(grid: &RegionGrid, x_m: f64, z_m: f64) -> bool {
    super::blend_weight(grid, x_m, z_m) == 0.0
}

/// The region's boulders among the generator's `boulders` (Task 11c fix round 2): those whose
/// water the region draws from its inner field alone. The generator already kept only rocks any
/// region's texels draw water over (`water::hold_in_drawn_water`); this drops the ones in the
/// band at the inner field's edge, where the river itself fades out. The band is the region's
/// own, so two overlapping regions differ only inside one of their bands.
pub fn region_boulders(grid: &RegionGrid, boulders: &[Boulder]) -> Vec<Boulder> {
    let basis = grid.spec.frame().basis();
    boulders
        .iter()
        .filter(|b| {
            basis
                .dir_to_local(RenderDir(b.dir))
                .is_some_and(|(x, z)| drawn_from_the_inner_field(grid, x, z))
        })
        .copied()
        .collect()
}
