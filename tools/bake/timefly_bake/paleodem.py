"""PaleoDEM elevation grids: loading, sampling, geographic cube bake and age metadata.

Each PaleoDEM (Scotese & Wright 2018) is a 1 deg lat-lon grid of signed elevation in metres, in
the geographic frame of its own plate reconstruction. A frame therefore carries two ages: its
**timeline age** (where it sits on the slider) and its **source rotation age** (the PALEOMAP
plate model age the grid was reconstructed for, Table 1 "Plate Tectonic Model Age"). The two can
differ; the renderer rotates plate-fixed samples with the source age. The mapping between files
and ages is a reviewed table (``data/paleodem_ages.csv``), never parsed from filenames: the
filename ages are plate model ages for 107 maps but stratigraphic ages for two.
"""

from __future__ import annotations

import csv
import logging
import math
from dataclasses import dataclass
from pathlib import Path

import netCDF4
import numpy as np

from timefly_bake import geo
from timefly_bake.log import traced
from timefly_bake.plates import MAX_AGE_MYR

_log = logging.getLogger(__name__)

AGE_COLUMNS = (
    "filename",
    "map_number",
    "label",
    "timeline_age_myr",
    "source_rotation_age_myr",
    "notes",
)
_LAT_NAMES = ("lat", "latitude")
_LON_NAMES = ("lon", "longitude")
# Longitudes closer than this (degrees) after wrapping are the same meridian.
_SEAM_TOL_DEG = 1e-6


@dataclass(frozen=True, eq=False)
class LatLonGrid:
    """A periodic lat-lon sample grid in canonical form.

    ``lat`` is strictly ascending in [-90, 90]; ``lon`` is strictly ascending within
    [-180, 180) with no duplicate meridian; ``values`` is float32 ``(len(lat), len(lon))``.
    Use :meth:`normalized` to build one from arbitrary axis order, 0..360 longitudes or a
    duplicated seam column.
    """

    lat: np.ndarray
    lon: np.ndarray
    values: np.ndarray

    def __post_init__(self):
        lat = np.asarray(self.lat, dtype=np.float64)
        lon = np.asarray(self.lon, dtype=np.float64)
        values = np.asarray(self.values, dtype=np.float32)
        if lat.ndim != 1 or lon.ndim != 1 or values.shape != (lat.size, lon.size):
            raise ValueError(
                f"grid shape {values.shape} does not match lat {lat.shape} x lon {lon.shape}"
            )
        if lat.size < 1 or lon.size < 1:
            raise ValueError("grid has no samples")
        if np.any(np.diff(lat) <= 0) or lat[0] < -90.0 or lat[-1] > 90.0:
            raise ValueError("latitudes must be strictly ascending within [-90, 90]")
        if np.any(np.diff(lon) <= 0) or lon[0] < -180.0 or lon[-1] >= 180.0:
            raise ValueError("longitudes must be strictly ascending within [-180, 180)")
        if not np.all(np.isfinite(values)):
            raise ValueError("grid holds non-finite samples")
        object.__setattr__(self, "lat", lat)
        object.__setattr__(self, "lon", lon)
        object.__setattr__(self, "values", values)

    @classmethod
    def normalized(cls, lat, lon, values) -> "LatLonGrid":
        """Canonicalize ``values[lat, lon]``: ascending latitude, longitude wrapped to
        [-180, 180) and sorted, duplicate meridians (a repeated seam column) merged after
        checking they agree."""
        lat = np.asarray(lat, dtype=np.float64)
        lon = np.asarray(lon, dtype=np.float64)
        values = np.asarray(values, dtype=np.float32)
        if values.shape != (lat.size, lon.size):
            raise ValueError(f"grid shape {values.shape} does not match lat x lon")
        lat_order = np.argsort(lat, kind="stable")
        lat, values = lat[lat_order], values[lat_order]
        # Source axes carry float noise (~2e-11 deg in the PaleoDEMs): snap the poles.
        lat = np.where(np.abs(np.abs(lat) - 90.0) < _SEAM_TOL_DEG, np.sign(lat) * 90.0, lat)

        wrapped = np.mod(lon + 180.0, 360.0) - 180.0
        # Anything within tolerance of the antimeridian is exactly -180 (so 180 + noise and
        # -180 - noise both land on the seam column).
        near_seam = (wrapped >= 180.0 - _SEAM_TOL_DEG) | (wrapped < -180.0 + _SEAM_TOL_DEG)
        wrapped = np.where(near_seam, -180.0, wrapped)
        order = np.argsort(wrapped, kind="stable")
        wrapped, values = wrapped[order], values[:, order]
        same = np.diff(wrapped) < _SEAM_TOL_DEG
        if same.any():
            keep = np.concatenate([[True], ~same])
            first_of = np.cumsum(keep) - 1  # index into the kept columns
            kept = values[:, keep]
            if not np.allclose(values, kept[:, first_of], rtol=0.0, atol=1e-3, equal_nan=True):
                raise ValueError("duplicate seam columns hold different values")
            wrapped, values = wrapped[keep], kept
        return cls(lat, wrapped, values)


@dataclass(frozen=True)
class PaleoDemAge:
    """One reviewed row of the age mapping table."""

    filename: str
    map_number: str
    label: str
    timeline_age_myr: float
    source_rotation_age_myr: float
    notes: str = ""


@dataclass(frozen=True)
class PaleoDemFrame:
    """A PaleoDEM file placed on the timeline."""

    timeline_age_myr: float
    path: Path
    source_rotation_age_myr: float
    label: str
    map_number: str


def _axis_name(ds, names) -> str:
    for name in names:
        if name in ds.variables and ds.variables[name].ndim == 1:
            return name
    raise ValueError(f"no coordinate variable named any of {names}")


@traced
def load_paleodem(path) -> LatLonGrid:
    """Read a PaleoDEM netCDF into a canonical :class:`LatLonGrid` (metres, float32).

    Finds the single 2-D variable over the lat/lon coordinates, whatever its dimension order,
    and rejects fill values or NaNs rather than baking holes.
    """
    path = Path(path)
    with netCDF4.Dataset(path) as ds:
        lat_name = _axis_name(ds, _LAT_NAMES)
        lon_name = _axis_name(ds, _LON_NAMES)
        lat_dim = ds.variables[lat_name].dimensions[0]
        lon_dim = ds.variables[lon_name].dimensions[0]
        fields = [
            v
            for v in ds.variables.values()
            if v.ndim == 2 and set(v.dimensions) == {lat_dim, lon_dim}
        ]
        if len(fields) != 1:
            raise ValueError(f"{path.name}: expected one 2-D lat/lon variable, found {len(fields)}")
        var = fields[0]
        field_name = var.name
        var.set_auto_mask(True)
        data = np.ma.filled(var[:].astype(np.float64), np.nan)
        if var.dimensions != (lat_dim, lon_dim):
            data = data.T
        lat = np.asarray(ds.variables[lat_name][:], dtype=np.float64)
        lon = np.asarray(ds.variables[lon_name][:], dtype=np.float64)
    bad = int(np.count_nonzero(~np.isfinite(data)))
    if bad:
        raise ValueError(f"{path.name}: {bad} missing or non-finite samples in '{field_name}'")
    grid = LatLonGrid.normalized(lat, lon, data.astype(np.float32))
    _log.debug(
        "%s: %d x %d, elevation %.0f..%.0f m",
        path.name,
        grid.lat.size,
        grid.lon.size,
        float(grid.values.min()),
        float(grid.values.max()),
    )
    return grid


@traced
def sample_bilinear(grid: LatLonGrid, lat, lon):
    """Bilinear samples at ``lat``/``lon`` (degrees, broadcast); longitude wraps across the
    seam and latitude clamps to the first and last rows."""
    lat = np.asarray(lat, dtype=np.float64)
    lon = np.asarray(lon, dtype=np.float64)
    lat, lon = np.broadcast_arrays(lat, lon)

    # Latitude: clamp, then locate the bracketing rows (a single row degenerates to constant).
    glat = grid.lat
    la = np.clip(lat, glat[0], glat[-1])
    if glat.size > 1:
        i0 = np.clip(np.searchsorted(glat, la, side="right") - 1, 0, glat.size - 2)
        ty = (la - glat[i0]) / (glat[i0 + 1] - glat[i0])
        i1 = i0 + 1
    else:
        i0 = i1 = np.zeros(la.shape, dtype=np.int64)
        ty = np.zeros(la.shape)

    # Longitude: periodic, with column 0 repeated one turn east of the last column.
    glon = grid.lon
    ext = np.append(glon, glon[0] + 360.0)
    lo = glon[0] + np.mod(lon - glon[0], 360.0)
    j0 = np.clip(np.searchsorted(ext, lo, side="right") - 1, 0, glon.size - 1)
    tx = (lo - ext[j0]) / (ext[j0 + 1] - ext[j0])
    j1 = (j0 + 1) % glon.size

    v = grid.values
    top = v[i0, j0] * (1.0 - tx) + v[i0, j1] * tx
    bottom = v[i1, j0] * (1.0 - tx) + v[i1, j1] * tx
    return top * (1.0 - ty) + bottom * ty


@traced
def bake_paleo_frame(dirs_render, grid: LatLonGrid):
    """Sample ``grid`` at render-frame directions ``(..., 3)`` (usually
    ``cube.texel_center_dirs(n)``), giving float16 ``(..., 2)``: R = signed elevation in
    metres, G zero (the bake CLI fills it with the frame's plate-coverage mask,
    ``plates.coverage_mask``). Every texel is filled from the geographic grid, whether or not
    any plate covers it."""
    lat, lon = geo.geo_to_latlon(geo.render_to_geo(np.asarray(dirs_render, dtype=np.float64)))
    elevation = sample_bilinear(grid, lat, lon)
    out = np.zeros(elevation.shape + (2,), dtype=np.float16)
    out[..., 0] = elevation
    return out


def _parse_age(value: str, column: str, row: int) -> float:
    try:
        age = float(value)
    except ValueError:
        raise ValueError(f"row {row}: {column} {value!r} is not a number") from None
    if not math.isfinite(age):
        raise ValueError(f"row {row}: {column} {value!r} is not finite")
    return age


@traced
def load_age_mapping(path) -> list[PaleoDemAge]:
    """Read the reviewed age mapping CSV (columns :data:`AGE_COLUMNS`) in file order."""
    path = Path(path)
    with path.open(newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        missing = [c for c in AGE_COLUMNS if c not in (reader.fieldnames or [])]
        if missing:
            raise ValueError(f"{path.name}: missing column(s) {missing}")
        rows = []
        for i, rec in enumerate(reader, start=2):
            rows.append(
                PaleoDemAge(
                    filename=rec["filename"].strip(),
                    map_number=rec["map_number"].strip(),
                    label=rec["label"].strip(),
                    timeline_age_myr=_parse_age(rec["timeline_age_myr"], "timeline_age_myr", i),
                    source_rotation_age_myr=_parse_age(
                        rec["source_rotation_age_myr"], "source_rotation_age_myr", i
                    ),
                    notes=(rec["notes"] or "").strip(),
                )
            )
    return rows


@traced
def list_paleodem_files(directory, age_mapping) -> list[PaleoDemFrame]:
    """Place every ``*.nc`` in ``directory`` on the timeline using ``age_mapping`` (an iterable
    of :class:`PaleoDemAge`), in strictly ascending timeline age.

    Fails, naming the file, when a file has no mapping row, a row names a missing file or is
    repeated, or two files share a timeline age. Also requires a present-day source
    (reconstruction age 0) as the youngest frame and the terminal frame at 540 Ma on both the
    timeline and the reconstruction.
    """
    directory = Path(directory)
    by_name: dict[str, PaleoDemAge] = {}
    for row in age_mapping:
        if row.filename in by_name:
            raise ValueError(f"{row.filename}: listed more than once in the age mapping")
        if row.timeline_age_myr < 0 or row.source_rotation_age_myr < 0:
            raise ValueError(f"{row.filename}: negative age in the age mapping")
        by_name[row.filename] = row

    present = {p.name: p for p in directory.iterdir() if p.is_file() and p.suffix == ".nc"}
    unmapped = sorted(set(present) - set(by_name))
    if unmapped:
        raise ValueError(f"PaleoDEM file(s) with no age mapping: {', '.join(unmapped)}")
    absent = sorted(set(by_name) - set(present))
    if absent:
        raise ValueError(f"age mapping names missing PaleoDEM file(s): {', '.join(absent)}")

    rows = sorted(by_name.values(), key=lambda r: (r.timeline_age_myr, r.filename))
    if not rows:
        raise ValueError(f"no PaleoDEM files in {directory}")
    for a, b in zip(rows, rows[1:]):
        if b.timeline_age_myr <= a.timeline_age_myr:
            raise ValueError(
                f"{b.filename}: timeline age {b.timeline_age_myr} Ma duplicates {a.filename}"
            )
        if b.source_rotation_age_myr < a.source_rotation_age_myr:
            raise ValueError(
                f"{b.filename}: reconstruction age {b.source_rotation_age_myr} Ma is younger "
                f"than {a.filename} ({a.source_rotation_age_myr} Ma) at an older timeline age"
            )
    if rows[0].source_rotation_age_myr != 0.0:
        raise ValueError(
            f"{rows[0].filename}: the youngest PaleoDEM must be the present-day source "
            f"(reconstruction age 0), not {rows[0].source_rotation_age_myr} Ma"
        )
    last = rows[-1]
    if last.timeline_age_myr != MAX_AGE_MYR or last.source_rotation_age_myr != MAX_AGE_MYR:
        raise ValueError(
            f"{last.filename}: the oldest PaleoDEM must sit at {MAX_AGE_MYR:g} Ma and be "
            f"reconstructed at {MAX_AGE_MYR:g} Ma (timeline {last.timeline_age_myr}, "
            f"reconstruction {last.source_rotation_age_myr})"
        )

    frames = [
        PaleoDemFrame(
            timeline_age_myr=r.timeline_age_myr,
            path=present[r.filename],
            source_rotation_age_myr=r.source_rotation_age_myr,
            label=r.label,
            map_number=r.map_number,
        )
        for r in rows
    ]
    differing = sum(f.timeline_age_myr != f.source_rotation_age_myr for f in frames)
    _log.info(
        "%d PaleoDEM frames, %g..%g Ma; %d with timeline age != reconstruction age",
        len(frames),
        frames[0].timeline_age_myr,
        frames[-1].timeline_age_myr,
        differing,
    )
    return frames
