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

/// Unit direction through the centre of texel `(col, row)` on an `n x n` face.
pub fn texel_center_dir(face: CubeFace, col: u32, row: u32, n: u32) -> RenderDir {
    let sc = 2.0 * (f64::from(col) + 0.5) / f64::from(n) - 1.0;
    let tc = 2.0 * (f64::from(row) + 0.5) / f64::from(n) - 1.0;
    let d = match face {
        CubeFace::PosX => DVec3::new(1.0, -tc, -sc),
        CubeFace::NegX => DVec3::new(-1.0, -tc, sc),
        CubeFace::PosY => DVec3::new(sc, 1.0, tc),
        CubeFace::NegY => DVec3::new(sc, -1.0, -tc),
        CubeFace::PosZ => DVec3::new(sc, -tc, 1.0),
        CubeFace::NegZ => DVec3::new(-sc, -tc, -1.0),
    };
    RenderDir(d.normalize())
}

/// Face, column and row of the texel containing `dir` on an `n x n` face.
pub fn dir_to_texel(dir: RenderDir, n: u32) -> (CubeFace, u32, u32) {
    let DVec3 { x, y, z } = dir.0;
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    let (face, sc, tc) = if ax >= ay && ax >= az {
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
    };
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
    fn dir_to_texel_handles_axes_and_edges() {
        assert_eq!(dir_to_texel(RenderDir(DVec3::Y), 4).0, CubeFace::PosY);
        assert_eq!(dir_to_texel(RenderDir(-DVec3::Z), 4).0, CubeFace::NegZ);
        let (_, c, r) = dir_to_texel(RenderDir(DVec3::new(1.0, 1.0, 1.0).normalize()), 4); // corner tie
        assert!(c < 4 && r < 4);
    }
}
