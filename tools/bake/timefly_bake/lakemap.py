"""The present-day lake map (rivers Task 8b): Natural Earth 1:10m lakes rasterised onto a 3 arc-minute
equirectangular grid, with each lake's surface level from ETOPO 2022.

``ne_10m_lakes.zip`` holds a polygon shapefile (``ne_10m_lakes.shp`` and ``.dbf``, lon/lat
degrees). Only features whose ``featurecla`` is a lake (:data:`LAKE_CLASSES`) are kept;
reservoirs are dropped (dams are not in the terrain the game samples, and the game spans the
past). The shapefile is read straight from the zip by the small reader here (polygons and dBase
attributes only; nothing is unpacked or executed).

Rasterising: a cell is a lake's when its centre lies inside the lake's rings (even-odd over all
the feature's rings, so islands are holes). Cell ``(row, col)`` has its centre at latitude
``90 - (row + 0.5) * 0.05`` and longitude ``-180 + (col + 0.5) * 0.05``; row 0 is the north.

Levels: each kept lake's surface level is the median of the ETOPO 2022 60 arc-second surface
cells whose centres lie inside it (the finest ETOPO the bake reads), or, for a lake too small to
hold a cell centre, the cell under its first vertex.

The file (little-endian), which ``timefly_core::hydrology::lakemap`` decodes::

    b"TFLM", u32 version (1), u32 width, u32 height, u32 lakes
    f32 level[lakes]            (lake id k + 1 has level[k])
    u32 row_start[height + 1]   (row r's runs are runs[row_start[r] .. row_start[r + 1]])
    runs: (u16 col_start, u16 col_end exclusive, u16 lake id), ascending col_start per row
"""

from __future__ import annotations

import logging
import struct
import zipfile
from pathlib import Path

import netCDF4
import numpy as np

from timefly_bake.log import traced

MAGIC = b"TFLM"
VERSION = 1
CELL_DEG = 0.05
WIDTH = 7200
HEIGHT = 3600
LAKE_CLASSES = frozenset({"Lake", "Alkaline Lake"})
SHP_MEMBER = "ne_10m_lakes.shp"
DBF_MEMBER = "ne_10m_lakes.dbf"
ETOPO_CELL_DEG = 1.0 / 60.0
_MAX_MEMBER_BYTES = 64 * 1024 * 1024
_HEADER = struct.Struct("<4s4I")
_RUN = np.dtype([("start", "<u2"), ("end", "<u2"), ("lake", "<u2")])
_log = logging.getLogger(__name__)


@traced
def read_shapefile(zip_path) -> list[tuple[dict, list[np.ndarray]]]:
    """Every polygon feature in the zip's shapefile: its dBase attributes (text fields stripped,
    numbers as text) and its rings as ``(n, 2)`` float64 arrays of (lon, lat)."""
    zip_path = Path(zip_path)
    with zipfile.ZipFile(zip_path) as zf:
        blobs = {}
        for member in (SHP_MEMBER, DBF_MEMBER):
            info = zf.getinfo(member)
            if info.file_size > _MAX_MEMBER_BYTES:
                raise ValueError(f"{zip_path.name}: {member} is {info.file_size} bytes")
            blobs[member] = zf.read(info)
    shapes = _read_shp(blobs[SHP_MEMBER])
    records = _read_dbf(blobs[DBF_MEMBER])
    if len(shapes) != len(records):
        raise ValueError(f"{len(shapes)} shapes but {len(records)} attribute records")
    return list(zip(records, shapes))


def _read_shp(blob: bytes) -> list[list[np.ndarray]]:
    code, length_words = struct.unpack(">i20xi", blob[:28])
    if code != 9994 or 2 * length_words != len(blob):
        raise ValueError("not a shapefile (bad header)")
    shape_type = struct.unpack("<i", blob[32:36])[0]
    if shape_type != 5:
        raise ValueError(f"shape type {shape_type}, expected 5 (polygon)")
    shapes = []
    at = 100
    while at < len(blob):
        _, words = struct.unpack(">2i", blob[at : at + 8])
        body = blob[at + 8 : at + 8 + 2 * words]
        at += 8 + 2 * words
        kind = struct.unpack("<i", body[:4])[0]
        if kind == 0:
            shapes.append([])
            continue
        if kind != 5:
            raise ValueError(f"record of shape type {kind} in a polygon file")
        n_parts, n_points = struct.unpack("<2i", body[36:44])
        parts = np.frombuffer(body, "<i4", n_parts, 44).tolist() + [n_points]
        points = np.frombuffer(body, "<f8", 2 * n_points, 44 + 4 * n_parts).reshape(-1, 2)
        shapes.append([points[parts[k] : parts[k + 1]].copy() for k in range(n_parts)])
    return shapes


def _read_dbf(blob: bytes) -> list[dict]:
    n_records, header_len, record_len = struct.unpack("<4xI2H", blob[:12])
    fields = []
    at = 32
    while blob[at] != 0x0D:
        name = blob[at : at + 11].split(b"\0")[0].decode("ascii")
        width = blob[at + 16]
        fields.append((name, width))
        at += 32
    out = []
    for r in range(n_records):
        start = header_len + r * record_len
        row = blob[start : start + record_len]
        if row[:1] == b"*":
            continue
        pos = 1
        rec = {}
        for name, width in fields:
            rec[name] = row[pos : pos + width].decode("utf-8", "replace").strip()
            pos += width
        out.append(rec)
    return out


@traced
def lake_features(zip_path) -> list[tuple[dict, list[np.ndarray]]]:
    """The shapefile's lakes (``featurecla`` in :data:`LAKE_CLASSES`), in file order."""
    features = read_shapefile(zip_path)
    kept = [(a, rings) for a, rings in features if a.get("featurecla") in LAKE_CLASSES and rings]
    classes: dict[str, int] = {}
    for a, _ in features:
        classes[a.get("featurecla", "")] = classes.get(a.get("featurecla", ""), 0) + 1
    _log.info("%d features by class %s; kept %d lakes", len(features), classes, len(kept))
    return kept


def _crossings(rings: list[np.ndarray], y: float) -> np.ndarray:
    """The x of every ring edge crossing the horizontal line at y (half-open in y), sorted."""
    xs = []
    for ring in rings:
        x0, y0 = ring[:, 0], ring[:, 1]
        x1, y1 = np.roll(x0, -1), np.roll(y0, -1)
        hit = (y0 <= y) != (y1 <= y)
        if hit.any():
            t = (y - y0[hit]) / (y1[hit] - y0[hit])
            xs.append(x0[hit] + t * (x1[hit] - x0[hit]))
    return np.sort(np.concatenate(xs)) if xs else np.empty(0)


def _cells_inside(
    rings: list[np.ndarray], lat0: float, lon0: float, step: float, rows: int, cols: int
) -> list[tuple[int, int, int]]:
    """Runs ``(row, col_start, col_end)`` of the cells of a north-up grid (row 0's centre at
    ``lat0 - step / 2``, col 0's at ``lon0 + step / 2``) whose centres lie inside ``rings``."""
    lo = min(float(r[:, 1].min()) for r in rings)
    hi = max(float(r[:, 1].max()) for r in rings)
    r_first = max(0, int(np.floor((lat0 - hi) / step - 0.5)))
    r_last = min(rows - 1, int(np.ceil((lat0 - lo) / step - 0.5)))
    runs = []
    for row in range(r_first, r_last + 1):
        y = lat0 - (row + 0.5) * step
        xs = _crossings(rings, y)
        for a, b in zip(xs[0::2], xs[1::2]):
            # Centres x_c = lon0 + (c + 0.5) step with a <= x_c < b.
            c0 = max(0, int(np.ceil((a - lon0) / step - 0.5)))
            c1 = min(cols, int(np.ceil((b - lon0) / step - 0.5)))
            if c1 > c0:
                runs.append((row, c0, c1))
    return runs


@traced
def rasterise(features, width: int = WIDTH, height: int = HEIGHT) -> np.ndarray:
    """Lake ids (uint16, 0 = no lake; feature k is id k + 1) on the map grid, north row first.
    A later feature overwrites an earlier one where both hold a cell."""
    step = 360.0 / width
    if abs(180.0 / height - step) > 1e-12:
        raise ValueError(f"{width} x {height} is not an equirectangular grid")
    ids = np.zeros((height, width), dtype=np.uint16)
    for k, (_, rings) in enumerate(features):
        for row, c0, c1 in _cells_inside(rings, 90.0, -180.0, step, height, width):
            ids[row, c0:c1] = k + 1
    return ids


@traced
def encode(ids: np.ndarray, levels: np.ndarray) -> bytes:
    """The file's bytes for lake ids ``ids`` (uint16, ``(height, width)``) and ``levels``
    (one per id 1..len(levels))."""
    height, width = ids.shape
    if width > 65535 or int(ids.max(initial=0)) > len(levels):
        raise ValueError("grid too wide or an id without a level")
    runs = []
    row_start = [0]
    for row in ids:
        change = np.flatnonzero(np.diff(row.astype(np.int32), prepend=-1, append=-1))
        for a, b in zip(change[:-1], change[1:]):
            if row[a]:
                runs.append((a, b, row[a]))
        row_start.append(len(runs))
    run_arr = np.array(runs, dtype=_RUN) if runs else np.empty(0, dtype=_RUN)
    return b"".join(
        [
            _HEADER.pack(MAGIC, VERSION, width, height, len(levels)),
            np.asarray(levels, dtype="<f4").tobytes(),
            np.asarray(row_start, dtype="<u4").tobytes(),
            run_arr.tobytes(),
        ]
    )


@traced
def decode(blob: bytes) -> tuple[np.ndarray, np.ndarray]:
    """``(ids, levels)`` from the file's bytes (the inverse of :func:`encode`)."""
    magic, version, width, height, lakes = _HEADER.unpack_from(blob)
    if magic != MAGIC or version != VERSION:
        raise ValueError(f"not a lake map (magic {magic!r}, version {version})")
    at = _HEADER.size
    levels = np.frombuffer(blob, "<f4", lakes, at).astype(np.float32)
    at += 4 * lakes
    row_start = np.frombuffer(blob, "<u4", height + 1, at)
    at += 4 * (height + 1)
    runs = np.frombuffer(blob, _RUN, int(row_start[-1]), at)
    if at + runs.nbytes != len(blob):
        raise ValueError(f"{len(blob) - at - runs.nbytes} bytes after the runs")
    ids = np.zeros((height, width), dtype=np.uint16)
    for row in range(height):
        for start, end, lake in runs[row_start[row] : row_start[row + 1]]:
            ids[row, start:end] = lake
    return ids, levels


@traced
def cell_of(lat: float, lon: float, width: int = WIDTH, height: int = HEIGHT) -> tuple[int, int]:
    """The map cell holding (lat, lon)."""
    step = 360.0 / width
    row = min(height - 1, max(0, int(np.floor((90.0 - lat) / step))))
    col = int(np.floor((lon + 180.0) / step)) % width
    return row, col


@traced
def surface_levels(features, etopo_path) -> np.ndarray:
    """Each feature's level (float32, m): the median of the ETOPO 2022 60 arc-second surface
    cells whose centres lie inside it, read window by window from the netCDF."""
    out = np.zeros(len(features), dtype=np.float32)
    rows, cols = 180 * 60, 360 * 60
    with netCDF4.Dataset(Path(etopo_path)) as ds:
        z = ds.variables["z"]
        lat = ds.variables["lat"]
        if z.shape != (rows, cols) or abs(float(lat[0]) + 90.0 - ETOPO_CELL_DEG / 2) > 1e-6:
            raise ValueError(f"{Path(etopo_path).name}: not the 60 arc-second cell grid")
        for k, (attrs, rings) in enumerate(features):
            runs = _cells_inside(rings, 90.0, -180.0, ETOPO_CELL_DEG, rows, cols)
            if runs:
                values = []
                for row, c0, c1 in runs:
                    # The file runs south to north.
                    values.append(np.asarray(z[rows - 1 - row, c0:c1], dtype=np.float64))
                out[k] = float(np.median(np.concatenate(values)))
            else:
                lon, la = rings[0][0]
                row = min(rows - 1, int((90.0 - la) / ETOPO_CELL_DEG))
                col = min(cols - 1, int((lon + 180.0) / ETOPO_CELL_DEG))
                out[k] = float(z[rows - 1 - row, col])
    return out


@traced
def bake_lake_map(zip_path, etopo_path, out_path) -> dict:
    """Bakes the lake map to ``out_path`` and returns a summary. Lakes that hold no map cell
    are left out, and the rest are numbered in file order."""
    features = lake_features(zip_path)
    ids = rasterise(features)
    present = np.unique(ids[ids > 0])
    kept = [features[i - 1] for i in present]
    renumber = np.zeros(len(features) + 1, dtype=np.uint16)
    renumber[present] = np.arange(1, len(present) + 1, dtype=np.uint16)
    ids = renumber[ids]
    levels = surface_levels(kept, etopo_path)
    blob = encode(ids, levels)
    Path(out_path).write_bytes(blob)
    summary = {
        "lakes": len(kept),
        "cells": int(np.count_nonzero(ids)),
        "bytes": len(blob),
    }
    _log.info("lake map: %s", summary)
    return summary


@traced
def main(argv=None) -> int:
    """``timefly-bake lakemap --zip ne_10m_lakes.zip --etopo ETOPO...60s...nc --out FILE``."""
    import argparse

    from timefly_bake import log

    p = argparse.ArgumentParser(
        prog="timefly-bake lakemap", description="Bake the present-day lake map (Task 8b)."
    )
    p.add_argument("--zip", type=Path, required=True, help="ne_10m_lakes.zip")
    p.add_argument("--etopo", type=Path, required=True, help="ETOPO 2022 60s surface netCDF")
    p.add_argument("--out", type=Path, required=True, help="output file (lake_map.bin)")
    p.add_argument("--log-level", default="INFO", help="stderr log level")
    args = p.parse_args(argv)
    log.configure(None, args.log_level)
    try:
        bake_lake_map(args.zip, args.etopo, args.out)
    except Exception as exc:  # already logged with its traceback by @traced
        _log.error("lake map bake failed: %s", exc)
        return 1
    return 0


__all__ = [
    "bake_lake_map",
    "cell_of",
    "decode",
    "encode",
    "lake_features",
    "rasterise",
    "read_shapefile",
    "surface_levels",
]
