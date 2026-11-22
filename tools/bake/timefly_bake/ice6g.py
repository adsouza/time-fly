"""ICE-6G_C (VM5a) recent-globe frames: relief, land and ice coverage for 0-26 ka.

Source: the author's 1 deg files ``I6_C.VM5a_1deg.<k>.nc[.gz]`` (k = ka BP, 0..26), NETCDF3
classic, 180 x 360 cell centres (lat -89.5..89.5, lon 0.5..359.5). Variables used:

* ``Topo`` (m): signed topography and bathymetry relative to that slice's sea level; grounded
  ice thickness is already part of it.
* ``sftlf`` (%): land area fraction.
* ``sftgif`` (%): ice area fraction, grounded and floating; floating ice sits over water where
  ``sftlf`` is 0.

Each slice's ``time`` attribute reads e.g. ``' 21.00 KBP'``; the files name no BP reference year,
so 1950 CE is assumed and ages gain the 76-year game offset (``timefly_bake.ages``).
"""

from __future__ import annotations

import gzip
import logging
import re
from pathlib import Path

import netCDF4
import numpy as np

from timefly_bake import geo
from timefly_bake.log import traced
from timefly_bake.paleodem import LatLonGrid, sample_bilinear

TOPO, LAND, ICE = "Topo", "sftlf", "sftgif"
SLICE_PATTERN = re.compile(r"^I6_C\.VM5a_1deg\.(\d+)\.nc(\.gz)?$")

_log = logging.getLogger(__name__)
# A 1 deg slice is 1.6 MB; refuse anything absurdly larger when inflating a .gz.
_MAX_INFLATED_BYTES = 256 * 1024 * 1024
_MASK_SCALES = {"%": 0.01, "percent": 0.01, "1": 1.0, "fraction": 1.0}
_MASK_TOL = 1e-4
_KA_PATTERN = re.compile(r"^\s*([0-9]+(?:\.[0-9]*)?)\s*KBP\s*$", re.IGNORECASE)


def _open(path: Path):
    if path.suffix != ".gz":
        return netCDF4.Dataset(path)
    with gzip.open(path, "rb") as f:
        raw = f.read(_MAX_INFLATED_BYTES + 1)
    if len(raw) > _MAX_INFLATED_BYTES:
        raise ValueError(f"{path.name}: inflates past {_MAX_INFLATED_BYTES} bytes")
    return netCDF4.Dataset(path.stem, memory=raw)


def _field(ds, name: str, path: Path, lat_dim: str, lon_dim: str) -> np.ndarray:
    if name not in ds.variables:
        raise ValueError(f"{path.name}: no '{name}' variable")
    var = ds.variables[name]
    if set(var.dimensions) != {lat_dim, lon_dim}:
        raise ValueError(f"{path.name}: '{name}' has dimensions {var.dimensions}")
    var.set_auto_mask(True)
    data = np.ma.filled(var[:].astype(np.float64), np.nan)
    if var.dimensions != (lat_dim, lon_dim):
        data = data.T
    bad = int(np.count_nonzero(~np.isfinite(data)))
    if bad:
        raise ValueError(f"{path.name}: {bad} missing or non-finite samples in '{name}'")
    return data


def _slice_ka(var) -> float | None:
    text = getattr(var, "time", None)
    if text is None:
        return None
    m = _KA_PATTERN.match(str(text))
    return float(m.group(1)) if m else None


@traced
def load_ice6g(path, expect_ka: int | None = None):
    """``(topo, land, ice)`` :class:`LatLonGrid` s of one slice: metres, and coverage in 0..1.

    Masks in percent are divided by 100; fraction-valued masks (units ``1``) are kept. Any other
    unit, values outside 0..1 after scaling, or a missing variable fail. With ``expect_ka``, every
    variable's ``time`` attribute must name that slice.
    """
    path = Path(path)
    with _open(path) as ds:
        lat_dim = ds.variables["lat"].dimensions[0]
        lon_dim = ds.variables["lon"].dimensions[0]
        lat = np.asarray(ds.variables["lat"][:], dtype=np.float64)
        lon = np.asarray(ds.variables["lon"][:], dtype=np.float64)
        out = []
        for name in (TOPO, LAND, ICE):
            data = _field(ds, name, path, lat_dim, lon_dim)
            units = str(getattr(ds.variables[name], "units", "")).strip()
            if name == TOPO:
                if units != "m":
                    raise ValueError(f"{path.name}: '{name}' units {units!r}, expected 'm'")
            else:
                if units not in _MASK_SCALES:
                    raise ValueError(f"{path.name}: '{name}' units {units!r} are not % or 1")
                data = data * _MASK_SCALES[units]
                if data.min() < -_MASK_TOL or data.max() > 1.0 + _MASK_TOL:
                    raise ValueError(
                        f"{path.name}: '{name}' spans {data.min():g}..{data.max():g}, not 0..1"
                    )
                data = np.clip(data, 0.0, 1.0)
            if expect_ka is not None:
                ka = _slice_ka(ds.variables[name])
                if ka is None or ka != float(expect_ka):
                    raise ValueError(
                        f"{path.name}: '{name}' is dated {ka} ka, expected {expect_ka} ka"
                    )
            out.append(LatLonGrid.normalized(lat, lon, data.astype(np.float32)))
    topo, land, ice = out
    _log.debug(
        "%s: %d x %d, topo %.0f..%.0f m, land %.1f%%, ice %.1f%% of cells (unweighted)",
        path.name,
        topo.lat.size,
        topo.lon.size,
        float(topo.values.min()),
        float(topo.values.max()),
        100.0 * float(land.values.mean()),
        100.0 * float(ice.values.mean()),
    )
    return topo, land, ice


@traced
def list_ice6g_slices(directory) -> list[tuple[int, Path]]:
    """``(ka, path)`` for every ``I6_C.VM5a_1deg.<k>.nc[.gz]`` in ``directory``, ascending.

    The slices must run 0, 1, ... without gaps; a slice present both gzipped and not fails.
    """
    directory = Path(directory)
    found: dict[int, Path] = {}
    for p in sorted(directory.iterdir()):
        m = SLICE_PATTERN.match(p.name)
        if not m or not p.is_file():
            continue
        k = int(m.group(1))
        if k in found:
            raise ValueError(f"ICE-6G_C slice {k} ka appears twice: {found[k].name}, {p.name}")
        found[k] = p
    if not found:
        raise ValueError(f"no ICE-6G_C slices in {directory}")
    missing = sorted(set(range(max(found) + 1)) - set(found))
    if missing:
        raise ValueError(f"ICE-6G_C slices missing for ka {missing} in {directory}")
    return sorted(found.items())


@traced
def bake_ice_frame(dirs_render, topo: LatLonGrid, land: LatLonGrid, ice: LatLonGrid):
    """Sample one slice at render-frame directions ``(..., 3)``, giving float16 ``(..., 4)``:
    R = signed ``Topo`` (m), G = land coverage, B = ice coverage (both 0..1), A reserved at 0.

    Channels are sampled independently and bilinearly, so the renderer may blend frames
    linearly; zero elevation alone never implies land.
    """
    lat, lon = geo.geo_to_latlon(geo.render_to_geo(np.asarray(dirs_render, dtype=np.float64)))
    elevation = sample_bilinear(topo, lat, lon)
    out = np.zeros(elevation.shape + (4,), dtype=np.float16)
    out[..., 0] = elevation
    out[..., 1] = np.clip(sample_bilinear(land, lat, lon), 0.0, 1.0)
    out[..., 2] = np.clip(sample_bilinear(ice, lat, lon), 0.0, 1.0)
    return out
