//! Cube-map convention shared with the Python bake tool: the standard table used by WebGPU,
//! Vulkan and D3D. Row 0 is the top of a face. Directions are in the render frame.

use crate::geo::RenderDir;
use bevy_math::DVec3;

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CubeFace {
    PosX = 0,
    NegX,
    PosY,
    NegY,
    PosZ,
    NegZ,
}

impl CubeFace {
    pub const ALL: [CubeFace; 6] = [
        CubeFace::PosX,
        CubeFace::NegX,
        CubeFace::PosY,
        CubeFace::NegY,
        CubeFace::PosZ,
        CubeFace::NegZ,
    ];
}

/// The point on `face`'s plane at face coordinates `(sc, tc)` (each `-1..1` on the face).
fn face_point(face: CubeFace, sc: f64, tc: f64) -> DVec3 {
    match face {
        CubeFace::PosX => DVec3::new(1.0, -tc, -sc),
        CubeFace::NegX => DVec3::new(-1.0, -tc, sc),
        CubeFace::PosY => DVec3::new(sc, 1.0, tc),
        CubeFace::NegY => DVec3::new(sc, -1.0, -tc),
        CubeFace::PosZ => DVec3::new(sc, -tc, 1.0),
        CubeFace::NegZ => DVec3::new(-sc, -tc, -1.0),
    }
}

/// Unit direction through the centre of texel `(col, row)` on an `n x n` face.
pub fn texel_center_dir(face: CubeFace, col: u32, row: u32, n: u32) -> RenderDir {
    let sc = 2.0 * (f64::from(col) + 0.5) / f64::from(n) - 1.0;
    let tc = 2.0 * (f64::from(row) + 0.5) / f64::from(n) - 1.0;
    RenderDir(face_point(face, sc, tc).normalize())
}

/// The four texels a seamless bilinear sample at `dir` blends, as `((face, col, row), weight)`.
/// A tap past a face edge is the texel containing that tap's centre extended onto the
/// neighbouring face (the way GPUs filter cube maps seamlessly, up to corner handling).
pub fn bilinear_taps(dir: RenderDir, n: u32) -> [((CubeFace, u32, u32), f64); 4] {
    let (face, sc, tc) = face_coords(dir);
    let nf = f64::from(n);
    let u = (sc + 1.0) / 2.0 * nf - 0.5;
    let v = (tc + 1.0) / 2.0 * nf - 0.5;
    let (i0, j0) = (u.floor(), v.floor());
    let (fu, fv) = (u - i0, v - j0);
    let tap = |i: f64, j: f64| {
        if (0.0..nf).contains(&i) && (0.0..nf).contains(&j) {
            (face, i as u32, j as u32)
        } else {
            let point = face_point(face, 2.0 * (i + 0.5) / nf - 1.0, 2.0 * (j + 0.5) / nf - 1.0);
            dir_to_texel(RenderDir(point.normalize()), n)
        }
    };
    [
        (tap(i0, j0), (1.0 - fu) * (1.0 - fv)),
        (tap(i0 + 1.0, j0), fu * (1.0 - fv)),
        (tap(i0, j0 + 1.0), (1.0 - fu) * fv),
        (tap(i0 + 1.0, j0 + 1.0), fu * fv),
    ]
}

/// The face and face coordinates `(sc, tc)` of a direction, under the shared tie rule.
fn face_coords(dir: RenderDir) -> (CubeFace, f64, f64) {
    let DVec3 { x, y, z } = dir.0;
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    if ax >= ay && ax >= az {
        if x > 0.0 {
            (CubeFace::PosX, -z / ax, -y / ax)
        } else {
            (CubeFace::NegX, z / ax, -y / ax)
        }
    } else if ay >= az {
        if y > 0.0 {
            (CubeFace::PosY, x / ay, z / ay)
        } else {
            (CubeFace::NegY, x / ay, -z / ay)
        }
    } else if z > 0.0 {
        (CubeFace::PosZ, x / az, -y / az)
    } else {
        (CubeFace::NegZ, -x / az, -y / az)
    }
}

/// Face, column and row of the texel containing `dir` on an `n x n` face.
pub fn dir_to_texel(dir: RenderDir, n: u32) -> (CubeFace, u32, u32) {
    let (face, sc, tc) = face_coords(dir);
    let last = f64::from(n) - 1.0;
    let index = |c: f64| ((c + 1.0) / 2.0 * f64::from(n)).floor().clamp(0.0, last) as u32;
    (face, index(sc), index(tc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_texel_dirs() {
        let r = 1.0 / 1.5f64.sqrt();
        let d = texel_center_dir(CubeFace::PosZ, 0, 0, 2);
        assert!((d.0 - DVec3::new(-0.5 * r, 0.5 * r, r)).length() < 1e-12);
        let d = texel_center_dir(CubeFace::PosX, 1, 0, 2);
        assert!((d.0 - DVec3::new(r, 0.5 * r, -0.5 * r)).length() < 1e-12);
    }

    #[test]
    fn texel_round_trip_all_faces() {
        for f in CubeFace::ALL {
            for row in 0..8 {
                for col in 0..8 {
                    assert_eq!(
                        dir_to_texel(texel_center_dir(f, col, row, 8), 8),
                        (f, col, row)
                    );
                }
            }
        }
    }

    #[test]
    fn bilinear_taps_weigh_the_four_nearest_texels_across_faces() {
        // At a texel centre the texel takes all the weight.
        let d = texel_center_dir(CubeFace::PosZ, 2, 5, 8);
        let taps = bilinear_taps(d, 8);
        let total: f64 = taps.iter().map(|t| t.1).sum();
        assert!((total - 1.0).abs() < 1e-12);
        let main = taps.iter().find(|t| t.1 > 0.999).unwrap();
        assert_eq!(main.0, (CubeFace::PosZ, 2, 5));
        // Halfway between +Z's last column and the next face: half on each side.
        let edge = RenderDir(DVec3::new(1.0, -(2.0 * 3.5 / 8.0 - 1.0), 1.0).normalize());
        let taps = bilinear_taps(edge, 8);
        let faces: std::collections::HashSet<_> =
            taps.iter().filter(|t| t.1 > 0.1).map(|t| t.0.0).collect();
        assert!(faces.contains(&CubeFace::PosZ) && faces.contains(&CubeFace::PosX));
    }

    #[test]
    fn dir_to_texel_handles_axes_and_edges() {
        assert_eq!(dir_to_texel(RenderDir(DVec3::Y), 4).0, CubeFace::PosY);
        assert_eq!(dir_to_texel(RenderDir(-DVec3::Z), 4).0, CubeFace::NegZ);
        let (_, c, r) = dir_to_texel(RenderDir(DVec3::new(1.0, 1.0, 1.0).normalize()), 4); // corner tie
        assert!(c < 4 && r < 4);
    }
}
