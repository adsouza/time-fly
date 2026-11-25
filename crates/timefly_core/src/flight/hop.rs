//! The region hop (M3 design §8): when the neighbouring region preloads, where it is centred, and
//! how the fly's pose carries into it (pure logic).
//!
//! Positions are the region frame's local metres: `+x` east, `−z` north, and on a [`Vec2`]
//! `(x, z)`. The inner field's edge is [`EDGE_M`] from the centre on either axis.

use bevy_math::{DVec3, Vec2};

use super::altitudes::{NEIGHBOUR_PAST_EDGE_M, PRELOAD_MIN_M};
use crate::geo::RenderDir;
use crate::worldgen::REGION_SIZE_M;
use crate::worldgen::frame::RegionFrame;

/// Half the inner field: the edge a hop crosses, on either axis.
pub const EDGE_M: f32 = (REGION_SIZE_M / 2.0) as f32;

/// How far from the edge the neighbour starts generating: [`PRELOAD_MIN_M`], or farther when the
/// fly would cover more than that at `speed_mps` in the platform's `expected_gen_s`.
pub fn preload_distance_m(speed_mps: f32, expected_gen_s: f32) -> f32 {
    (PRELOAD_MIN_M as f32).max(speed_mps * expected_gen_s)
}

/// Whether the neighbour should be generating: on some axis the fly at `pos_m` is within
/// [`preload_distance_m`] of the edge (or past it) and moving out of it (`vel_mps`).
pub fn should_preload(pos_m: Vec2, vel_mps: Vec2, speed_mps: f32, expected_gen_s: f32) -> bool {
    let reach = preload_distance_m(speed_mps, expected_gen_s);
    (0..2).any(|a| {
        let (p, v) = (pos_m[a], vel_mps[a]);
        EDGE_M - p.abs() <= reach && p * v > 0.0
    })
}

/// Which edge the fly at `pos_m` reaches first flying along `heading`: the axis (0 for `x`, 1
/// for `z`), the side (±1) and how far along the heading it lies (0 once past it). With no
/// outward heading, the nearest edge.
pub fn exit_edge(pos_m: Vec2, heading: Vec2) -> (usize, f32, f32) {
    let mut best: Option<(usize, f32, f32)> = None;
    for a in 0..2 {
        let h = heading[a];
        if h == 0.0 {
            continue;
        }
        let side = h.signum();
        let t = ((side * EDGE_M - pos_m[a]) / h).max(0.0);
        if best.is_none_or(|(_, _, b)| t < b) {
            best = Some((a, side, t));
        }
    }
    best.unwrap_or_else(|| {
        let a = usize::from(pos_m.y.abs() > pos_m.x.abs());
        let side = if pos_m[a] < 0.0 { -1.0 } else { 1.0 };
        (a, side, 0.0)
    })
}

/// The neighbour's centre for the fly at `pos_m` flying along `heading` (local metres): past
/// the edge it reaches first ([`exit_edge`]), [`NEIGHBOUR_PAST_EDGE_M`] beyond it along that
/// axis, level with where the fly crosses it, so the fly arrives that far inside the neighbour
/// with room to go on. Not snapped to a lattice: every centre agrees on the shared one.
pub fn neighbour_centre(frame: &RegionFrame, pos_m: Vec2, heading: Vec2) -> RenderDir {
    let heading = heading.normalize_or_zero();
    let (axis, side, t) = exit_edge(pos_m, heading);
    let crossing = pos_m.as_dvec2() + heading.as_dvec2() * f64::from(t);
    let mut centre = crossing;
    centre[axis] = f64::from(side) * (f64::from(EDGE_M) + NEIGHBOUR_PAST_EDGE_M);
    frame.local_to_dir(centre.x, centre.y)
}

/// The fly at `pos_m` (`x`, height, `z`) heading `yaw` (about `+y` from north, positive west)
/// in `old`, carried into `new`: the same place on the sphere, and the yaw toward the point
/// 1 km ahead carried the same way. The height is passed through; the caller keeps the height
/// above the ground.
pub fn carry_pose(old: &RegionFrame, new: &RegionFrame, pos_m: DVec3, yaw: f32) -> (DVec3, f32) {
    const AHEAD_M: f64 = 1_000.0;
    let (old, new) = (old.basis(), new.basis());
    let carry = |x: f64, z: f64| {
        new.dir_to_local(old.local_to_dir(x, z))
            .expect("a neighbour is never antipodal")
    };
    let (x, z) = carry(pos_m.x, pos_m.z);
    // Bevy's yaw about +y turns −z (north) toward −x (west).
    let (s, c) = f64::from(yaw).sin_cos();
    let (ax, az) = carry(pos_m.x - s * AHEAD_M, pos_m.z - c * AHEAD_M);
    let yaw = (-(ax - x)).atan2(-(az - z)) as f32;
    (DVec3::new(x, pos_m.y, z), yaw)
}

/// Where the fly moving from `prev_m` toward `next_m` may be, kept inside the inner field: on
/// each axis no farther out than [`EDGE_M`], or than it already was (a hop's hold drifts past
/// the edge under the opaque cloud, and turning back from there is never held), so it slides
/// along an edge it meets and always gets back in.
pub fn confine(prev_m: Vec2, next_m: Vec2) -> Vec2 {
    let limit = prev_m.abs().max(Vec2::splat(EDGE_M));
    next_m.clamp(-limit, limit)
}

/// The fly's velocity for [`should_preload`]: as `measured_mps` (its motion over the last
/// frame), except on an axis where it is at or past the edge (held there by [`confine`]) and the
/// player's `wish_mps` points out of it, where the wish's component counts. Held at an edge
/// while pushing out at an angle, it slides along it, so its motion alone shows no way out.
pub fn velocity_at_edges(pos_m: Vec2, measured_mps: Vec2, wish_mps: Vec2) -> Vec2 {
    /// Within this of the edge counts as held at it (the clamp puts it exactly there).
    const AT_EDGE_M: f32 = 1.0;
    let mut v = measured_mps;
    for a in 0..2 {
        if pos_m[a].abs() >= EDGE_M - AT_EDGE_M && pos_m[a] * wish_mps[a] > 0.0 {
            v[a] = wish_mps[a];
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight::altitudes::{EXPECTED_GEN_S_NATIVE, EXPECTED_GEN_S_WEB};
    use crate::geo::{geo_to_render, latlon_to_geo};

    const GEN_S: f32 = EXPECTED_GEN_S_NATIVE as f32;

    fn alps() -> RegionFrame {
        RegionFrame {
            center: geo_to_render(latlon_to_geo(46.5, 10.0)),
        }
    }

    /// Unit vectors along each outward direction: east, west, south (+z), north (−z).
    const OUT: [Vec2; 4] = [Vec2::X, Vec2::NEG_X, Vec2::Y, Vec2::NEG_Y];

    #[test]
    fn preload_starts_earlier_when_flying_faster() {
        // Slow flight preloads at the floor; fast flight a generation's travel out.
        assert_eq!(preload_distance_m(0.0, GEN_S), PRELOAD_MIN_M as f32);
        assert_eq!(preload_distance_m(1_000.0, GEN_S), PRELOAD_MIN_M as f32);
        assert_eq!(preload_distance_m(30_000.0, GEN_S), 90_000.0);
        assert!(
            preload_distance_m(10_000.0, EXPECTED_GEN_S_WEB as f32)
                > preload_distance_m(10_000.0, GEN_S)
        );
        // 70 km from the east edge, flying east: too far at 1 km/s, inside the reach at 30 km/s.
        let p = Vec2::new(EDGE_M - 70_000.0, 0.0);
        assert!(!should_preload(p, Vec2::X * 1_000.0, 1_000.0, GEN_S));
        assert!(should_preload(p, Vec2::X * 30_000.0, 30_000.0, GEN_S));
        // Within the floor, any outward speed preloads, on each of the four edges.
        for out in OUT {
            let p = out * (EDGE_M - 40_000.0);
            assert!(should_preload(p, out * 200.0, 200.0, GEN_S), "{out}");
            // A diagonal flight preloads for the edge it nears.
            let q = p + out.perp() * 30_000.0;
            let v = (out + out.perp()).normalize() * 500.0;
            assert!(should_preload(q, v, 500.0, GEN_S), "{out}");
        }
    }

    #[test]
    fn heading_inward_never_preloads() {
        for out in OUT {
            for reach in [1.0, 10_000.0, 49_000.0] {
                let p = out * (EDGE_M - reach);
                for speed in [100.0, 10_000.0, 100_000.0] {
                    // Straight back in, and back in at an angle.
                    assert!(
                        !should_preload(p, -out * speed, speed, GEN_S),
                        "{out} {reach}"
                    );
                    let v = (-out + 0.5 * out.perp()).normalize() * speed;
                    assert!(!should_preload(p, v, speed, GEN_S), "{out} {reach}");
                    // Along the edge, with the perpendicular edge out of reach.
                    let v = out.perp() * speed;
                    assert!(
                        !should_preload(p, v, speed.min(10_000.0), GEN_S),
                        "{out} {reach}"
                    );
                }
                // Hovering.
                assert!(!should_preload(p, Vec2::ZERO, 0.0, GEN_S));
            }
        }
        // At the centre nothing preloads, whatever the heading.
        for out in OUT {
            assert!(!should_preload(Vec2::ZERO, out * 10_000.0, 10_000.0, GEN_S));
        }
    }

    #[test]
    fn the_neighbour_lies_ahead_past_the_edge() {
        let frame = alps();
        let local = |d: RenderDir| frame.dir_to_local(d).unwrap();
        // The fly at (140 km, 0) heading east: the neighbour 100 km past the east edge.
        let (x, z) = local(neighbour_centre(&frame, Vec2::new(140_000.0, 0.0), Vec2::X));
        assert!((x - 250_000.0).abs() < 1.0, "{x}");
        assert!(z.abs() < 1.0, "{z}");
        // Each edge, heading out of it: 100 km past it, level with the fly.
        for out in OUT {
            let p = out * 140_000.0 + out.perp() * 20_000.0;
            let (x, z) = local(neighbour_centre(&frame, p, out));
            let want = out * 250_000.0 + out.perp() * 20_000.0;
            assert!(
                (x - f64::from(want.x)).abs() < 1.0 && (z - f64::from(want.y)).abs() < 1.0,
                "{out}: ({x}, {z})"
            );
        }
        // Heading diagonally, the neighbour lies past the edge the fly reaches first, level with
        // where it crosses: from (100 km, 120 km) heading south-east (+x, +z), the south edge
        // (z = 150 km) comes first, at x = 130 km.
        let (x, z) = local(neighbour_centre(
            &frame,
            Vec2::new(100_000.0, 120_000.0),
            Vec2::new(1.0, 1.0).normalize(),
        ));
        assert!((z - 250_000.0).abs() < 1.0, "{z}");
        assert!((x - 130_000.0).abs() < 1.0, "{x}");
    }

    #[test]
    fn carrying_the_pose_round_trips() {
        let old = alps();
        for (p, yaw) in [
            (DVec3::new(140_000.0, 3_000.0, 20_000.0), 0.7f32),
            (DVec3::new(-151_000.0, 60_000.0, -149_000.0), -2.9),
            (DVec3::new(12_345.6, 800.0, 149_999.0), 3.1),
        ] {
            let heading = Vec2::new(-yaw.sin(), -yaw.cos());
            let new = RegionFrame {
                center: neighbour_centre(&old, xz(p), heading),
            };
            // A neighbour, not the region itself (or the carry would be the identity).
            assert!(old.center.0.angle_between(new.center.0) * 6.371e6 > 200_000.0);
            let (q, yaw_new) = carry_pose(&old, &new, p, yaw);
            // The same place on the sphere, at the same height.
            let a = old.local_to_dir(p.x, p.z).0;
            let b = new.local_to_dir(q.x, q.z).0;
            assert!(a.angle_between(b) * 6.371e6 < 1e-3, "{p} → {q}");
            assert_eq!(q.y, p.y);
            // And back.
            let (r, yaw_back) = carry_pose(&new, &old, q, yaw_new);
            assert!(r.distance(p) < 1e-3, "{p} → {q} → {r}");
            let turn = (yaw_back - yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
                - std::f32::consts::PI;
            assert!(turn.abs() < 1e-5, "{yaw} → {yaw_new} → {yaw_back}");
        }
        // Into the neighbour ahead, the fly keeps its heading (to within the meridians'
        // convergence between the centres) and lands 100 km from the new centre, on its west.
        let p = DVec3::new(EDGE_M as f64, 5_000.0, 0.0);
        let new = RegionFrame {
            center: neighbour_centre(&old, Vec2::new(EDGE_M, 0.0), Vec2::X),
        };
        let (q, yaw) = carry_pose(&old, &new, p, -std::f32::consts::FRAC_PI_2);
        assert!((q.x.hypot(q.z) - NEIGHBOUR_PAST_EDGE_M).abs() < 1.0, "{q}");
        assert!(
            q.x < -0.99 * NEIGHBOUR_PAST_EDGE_M && q.z.abs() < 6_000.0,
            "{q}"
        );
        assert!((yaw + std::f32::consts::FRAC_PI_2).abs() < 0.1, "{yaw}");
    }

    #[test]
    fn confining_keeps_the_fly_in_the_field_and_lets_it_back() {
        let e = EDGE_M;
        // Inside, nothing changes.
        let p = Vec2::new(100_000.0, -20_000.0);
        assert_eq!(
            confine(p, p + Vec2::new(30_000.0, 5_000.0)),
            Vec2::new(130_000.0, -15_000.0)
        );
        // Out over an edge on either side and axis: held at it, sliding along it.
        assert_eq!(
            confine(p, Vec2::new(e + 5_000.0, -10_000.0)),
            Vec2::new(e, -10_000.0)
        );
        assert_eq!(confine(p, Vec2::new(-e - 1.0, -e - 7.0)), Vec2::new(-e, -e));
        // Already out (a hold drifted past the edge): never farther out, always back in.
        let out = Vec2::new(e + 3_000.0, 0.0);
        assert_eq!(confine(out, out + Vec2::new(500.0, 0.0)), out);
        assert_eq!(
            confine(out, out - Vec2::new(500.0, 0.0)),
            out - Vec2::new(500.0, 0.0)
        );
    }

    #[test]
    fn at_an_edge_the_wish_out_of_it_is_the_velocity_on_its_axis() {
        let e = EDGE_M;
        // Pinned at the east edge, pushing out 20° off straight, sliding along it: the push out
        // counts on x, the slide on z, so the edge it is held at is the one it flies out of.
        let (s, c) = 20f32.to_radians().sin_cos();
        let wish = Vec2::new(c, s) * 10_000.0;
        let slide = Vec2::new(0.0, wish.y);
        let v = velocity_at_edges(Vec2::new(e, 3_000.0), slide, wish);
        assert_eq!(v, Vec2::new(wish.x, wish.y));
        assert!(should_preload(Vec2::new(e, 3_000.0), v, v.length(), 3.0));
        // At a corner, held on both axes.
        let wish = Vec2::new(1.0, -1.0) * 5_000.0;
        assert_eq!(velocity_at_edges(Vec2::new(e, -e), Vec2::ZERO, wish), wish);
        // Inside the field, or wishing back in, or along the edge: the motion as measured.
        let m = Vec2::new(700.0, -20.0);
        assert_eq!(velocity_at_edges(Vec2::new(e - 5_000.0, 0.0), m, wish), m);
        assert_eq!(
            velocity_at_edges(Vec2::new(e, 0.0), m, Vec2::new(-3_000.0, 0.0)),
            m
        );
        assert_eq!(
            velocity_at_edges(Vec2::new(-e, 0.0), Vec2::ZERO, Vec2::X * 100.0),
            Vec2::ZERO
        );
    }

    /// The horizontal `(x, z)` of a local position.
    fn xz(p: DVec3) -> Vec2 {
        Vec2::new(p.x as f32, p.z as f32)
    }
}
