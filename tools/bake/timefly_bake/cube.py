"""Cube-map convention shared with the Rust crate (``timefly_core::cube``).

The standard table used by WebGPU, Vulkan and D3D; row 0 is the top of a face; directions are in
the render frame. Faces are ordered +X, -X, +Y, -Y, +Z, -Z (0..5).
"""

from __future__ import annotations

import numpy as np

from timefly_bake.log import traced


@traced
def texel_center_dirs(n: int):
    """Unit directions of every texel centre, shape ``(6, n, n, 3)``, indexed ``[face, row, col]``."""
    s = 2.0 * (np.arange(n) + 0.5) / n - 1.0
    sc, tc = np.meshgrid(s, s)  # [row, col]
    one = np.ones_like(sc)
    faces = [
        (one, -tc, -sc),
        (-one, -tc, sc),
        (sc, one, tc),
        (sc, -one, -tc),
        (sc, -tc, one),
        (-sc, -tc, -one),
    ]
    d = np.stack([np.stack(f, axis=-1) for f in faces])
    return d / np.linalg.norm(d, axis=-1, keepdims=True)


@traced
def dirs_to_texels(dirs, n: int):
    """Face, column and row index arrays for directions ``(..., 3)``; index as ``arr[face, row, col]``."""
    d = np.asarray(dirs, dtype=np.float64)
    x, y, z = d[..., 0], d[..., 1], d[..., 2]
    ax, ay, az = np.abs(x), np.abs(y), np.abs(z)
    is_x = (ax >= ay) & (ax >= az)
    is_y = ~is_x & (ay >= az)
    face = np.where(
        is_x,
        np.where(x > 0, 0, 1),
        np.where(is_y, np.where(y > 0, 2, 3), np.where(z > 0, 4, 5)),
    )
    ma = np.where(is_x, ax, np.where(is_y, ay, az))
    sc = np.select(
        [face == 0, face == 1, face == 2, face == 3, face == 4],
        [-z / ma, z / ma, x / ma, x / ma, x / ma],
        default=-x / ma,
    )
    tc = np.select(
        [face == 0, face == 1, face == 2, face == 3, face == 4],
        [-y / ma, -y / ma, z / ma, -z / ma, -y / ma],
        default=-y / ma,
    )
    col = np.clip(np.floor((sc + 1.0) / 2.0 * n), 0, n - 1).astype(np.int64)
    row = np.clip(np.floor((tc + 1.0) / 2.0 * n), 0, n - 1).astype(np.int64)
    return face.astype(np.int64), col, row
