"""Coordinate frames shared with the Rust crate (``timefly_core::geo``).

Geo frame: +X at (0N, 0E), +Y at (0N, 90E), +Z at the north pole. Render frame is Y-up: longitude
0 faces +Z, east is +X, north is +Y. Geo to render is ``(x, y, z) -> (y, z, x)``. All functions
are vectorised; vectors live on the last axis.
"""

from __future__ import annotations

import numpy as np

from timefly_bake.log import traced


@traced
def latlon_to_geo(lat, lon):
    """Unit geo-frame vectors, shape ``(..., 3)``, for latitude and longitude in degrees."""
    lat, lon = np.radians(lat), np.radians(lon)
    return np.stack(
        np.broadcast_arrays(np.cos(lat) * np.cos(lon), np.cos(lat) * np.sin(lon), np.sin(lat)),
        axis=-1,
    )


@traced
def geo_to_latlon(v):
    """Latitude and longitude in degrees; longitude in (-180, 180], zero at the poles."""
    v = np.asarray(v, dtype=np.float64)
    lat = np.degrees(np.arcsin(np.clip(v[..., 2], -1.0, 1.0)))
    lon = np.degrees(np.arctan2(v[..., 1], v[..., 0]))
    lon = np.where(lon <= -180.0, 180.0, lon)
    lon = np.where(np.hypot(v[..., 0], v[..., 1]) < 1e-12, 0.0, lon)
    return lat, lon


@traced
def geo_to_render(v):
    v = np.asarray(v)
    return np.stack([v[..., 1], v[..., 2], v[..., 0]], axis=-1)


@traced
def render_to_geo(v):
    v = np.asarray(v)
    return np.stack([v[..., 2], v[..., 0], v[..., 1]], axis=-1)


@traced
def quat_rotate(q_xyzw, v):
    """Rotates ``v`` (``(..., 3)``) by the unit quaternion ``q_xyzw`` (``(..., 4)``, broadcast)."""
    q = np.asarray(q_xyzw, dtype=np.float64)
    v = np.asarray(v, dtype=np.float64)
    u, w = q[..., :3], q[..., 3:4]
    t = 2.0 * np.cross(u, v)
    return v + w * t + np.cross(u, t)
