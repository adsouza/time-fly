"""ETOPO 2022 (60 arc-second, surface) loader: netCDF to an elevation grid on the node lattice.

The file ``ETOPO_2022_v1_60s_N90W180_surface.nc`` holds ``z`` (m, ``lat`` x ``lon``) on a
10800 x 21600 grid. Its axes are cell centres (first latitude -89.99167, i.e. half a step in
from the pole, and ``node_offset = 1``), so the loader averages onto the node lattice the tiles
use (:func:`timefly_bake.tiles.cell_to_node_mean`) and logs that it did. A node-registered file
(axes starting on the pole and the antimeridian) is accepted as is.
"""

from __future__ import annotations

import logging
from pathlib import Path

import netCDF4
import numpy as np

from timefly_bake import tiles
from timefly_bake.log import traced

_log = logging.getLogger(__name__)
_AXIS_TOL_DEG = 1e-6
_STEP_DEG = 1.0 / tiles.PER_DEG


@traced
def load(path) -> np.ndarray:
    """Elevation in metres as float32 on the node lattice, south to north, shape
    ``(180 * 60 + 1, 360 * 60)`` (the 180 E column is the -180 column)."""
    path = Path(path)
    with netCDF4.Dataset(path) as ds:
        for name in ("z", "lat", "lon"):
            if name not in ds.variables:
                raise ValueError(f"{path.name}: no '{name}' variable")
        lat = np.asarray(ds.variables["lat"][:], dtype=np.float64)
        lon = np.asarray(ds.variables["lon"][:], dtype=np.float64)
        var = ds.variables["z"]
        if var.dimensions != ("lat", "lon"):
            raise ValueError(f"{path.name}: 'z' has dimensions {var.dimensions}")
        var.set_auto_mask(True)
        z = np.ma.filled(var[:].astype(np.float32), np.nan)
    if not (np.all(np.diff(lat) > 0) and np.all(np.diff(lon) > 0)):
        raise ValueError(f"{path.name}: lat and lon must ascend")
    if z.shape != (lat.size, lon.size) or lat.size != 180 * tiles.PER_DEG:
        raise ValueError(f"{path.name}: grid {z.shape} is not 60 arc-second global")
    if lon.size != 360 * tiles.PER_DEG:
        raise ValueError(f"{path.name}: grid {z.shape} is not 60 arc-second global")
    bad = int(np.count_nonzero(~np.isfinite(z)))
    if bad:
        raise ValueError(f"{path.name}: {bad} missing or non-finite elevation samples")
    steps = np.concatenate([np.diff(lat), np.diff(lon)])
    if np.abs(steps - _STEP_DEG).max() > _AXIS_TOL_DEG:
        raise ValueError(f"{path.name}: axes are not spaced 1/60 degree")
    if abs(lat[0] + 90.0 - _STEP_DEG / 2) < _AXIS_TOL_DEG and abs(
        lon[0] + 180.0 - _STEP_DEG / 2
    ) < _AXIS_TOL_DEG:
        _log.info(
            "%s is cell-registered (lat %.5f..%.5f, lon %.5f..%.5f): averaging the four cells "
            "around each node onto the node lattice",
            path.name,
            lat[0],
            lat[-1],
            lon[0],
            lon[-1],
        )
        return tiles.cell_to_node_mean(z)
    raise ValueError(
        f"{path.name}: unsupported registration (lat starts {lat[0]!r}, lon starts {lon[0]!r}); "
        "expected cell centres half a step from -90 and -180"
    )
