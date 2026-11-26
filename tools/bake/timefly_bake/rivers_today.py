"""Today's major river courses (rivers Task 11b): Natural Earth 1:10m "Rivers + lake centerlines"
v5.1.2 centrelines, upstream first and simplified, which the continental pass burns into its
routing wherever the terrain is today's (``timefly_core::hydrology::today_rivers``).

- **Select:** features whose ``featurecla`` is ``River`` with ``scalerank`` at most
  :data:`TODAY_RIVER_MAX_SCALERANK`; each part of a MultiLineString is its own polyline. Lake
  centrelines are left out: today's lakes are the lake map's (Task 8b).
- **Orient:** upstream first. Each end's height is the lowest ETOPO 2022 60″ surface cell whose
  centre lies within :data:`ORIENT_CELLS` cells (2′) of the end's cell, in both rows and columns;
  the higher end goes first. Ties keep Natural Earth's order.
- **Simplify:** Ramer–Douglas–Peucker with great-circle distances, :data:`SIMPLIFY_M` (1 km), so
  every dropped vertex lies within 1 km of the kept line.
- **Write:** coordinates rounded to 0.001° (about 110 m), repeats after rounding dropped, one
  river per line; ``{ "comment", "source", "rivers": [{ "name", "scalerank", "points": [[lat,
  lon], ...] }] }``, at most :data:`MAX_TABLE_BYTES`.
"""

from __future__ import annotations

import json
import logging
import math
from dataclasses import dataclass
from pathlib import Path

import netCDF4
import numpy as np

from timefly_bake.log import traced

TODAY_RIVER_MAX_SCALERANK = 7
SIMPLIFY_M = 1000.0
ORIENT_CELLS = 2
ROUND_DIGITS = 3
MAX_TABLE_BYTES = 1_000_000
EARTH_RADIUS_M = 6_371_000.0
SOURCE = (
    "Natural Earth 1:10m physical vectors, Rivers + lake centerlines, version 5.1.2 "
    "(ne_10m_rivers_lake_centerlines.geojson from github.com/nvkelso/natural-earth-vector, tag "
    "v5.1.2; public domain, naturalearthdata.com). Oriented by ETOPO 2022 60 arc-second surface, "
    "NOAA NCEI, doi:10.25921/fd45-gt74."
)
COMMENT = (
    "Today's major river courses (rivers Task 11b): Natural Earth rivers of scalerank <= 7 (lake "
    "centrelines left out), one entry per line part, upstream first (the end with the higher "
    "lowest ETOPO 60-second cell within 2 arc-minutes goes first), simplified to 1 km, points "
    "[lat, lon] rounded to 0.001 degrees. Where the terrain is today's, the continental pass burns "
    "each course into its routing: the running minimum of the era's node elevations along it, "
    "less TODAY_RIVER_BURN_M. Written by `timefly-bake rivers-today` "
    "(tools/bake/timefly_bake/rivers_today.py); read by timefly_core::hydrology::today_rivers."
)

_log = logging.getLogger(__name__)


@dataclass(frozen=True)
class Polyline:
    name: str
    scalerank: int
    #: ``(n, 2)`` float64 (lat, lon) degrees.
    points: np.ndarray


class CellGrid:
    """Cell-centred elevation cells: ``z[row, col]`` with rows from the south, the cell
    ``(row, col)`` covering latitudes ``lat_s + row / per_deg`` up one cell and longitudes
    ``lon_w + col / per_deg`` east one cell. ``z`` may be a numpy array or a netCDF variable (read
    a window at a time)."""

    def __init__(self, z, lat_s: float, lon_w: float, per_deg: int):
        self.z = z
        self.lat_s = lat_s
        self.lon_w = lon_w
        self.per_deg = per_deg
        self.rows, self.cols = z.shape

    def lowest_near(self, lat: float, lon: float, cells: int = ORIENT_CELLS) -> float:
        """The lowest cell within ``cells`` rows and columns of the cell holding the point
        (clamped at the grid's edges)."""
        i = min(self.rows - 1, max(0, math.floor((lat - self.lat_s) * self.per_deg)))
        j = min(self.cols - 1, max(0, math.floor((lon - self.lon_w) * self.per_deg)))
        window = self.z[
            max(0, i - cells) : min(self.rows, i + cells + 1),
            max(0, j - cells) : min(self.cols, j + cells + 1),
        ]
        return float(np.nanmin(np.ma.filled(np.asarray(window, dtype=np.float64), np.nan)))


def _rank(properties) -> int:
    return int(round(float(properties["scalerank"])))


@traced
def select(features) -> list[Polyline]:
    """Each part of every ``River`` feature with ``scalerank <= TODAY_RIVER_MAX_SCALERANK``, as
    a polyline of (lat, lon), in file order."""
    out = []
    for f in features:
        p = f["properties"]
        if p.get("featurecla") != "River" or _rank(p) > TODAY_RIVER_MAX_SCALERANK:
            continue
        g = f["geometry"]
        parts = g["coordinates"] if g["type"] == "MultiLineString" else [g["coordinates"]]
        name = (p.get("name") or p.get("name_en") or "").strip()
        for part in parts:
            lonlat = np.asarray(part, dtype=np.float64)[:, :2]
            out.append(Polyline(name, _rank(p), lonlat[:, ::-1].copy()))
    return out


@traced
def orient(polyline: Polyline, etopo: CellGrid) -> Polyline:
    """``polyline`` upstream first: reversed when its last point's lowest nearby cell stands
    higher than its first point's."""
    first = etopo.lowest_near(*polyline.points[0])
    last = etopo.lowest_near(*polyline.points[-1])
    if last > first:
        return Polyline(polyline.name, polyline.scalerank, polyline.points[::-1].copy())
    return polyline


def _unit(points: np.ndarray) -> np.ndarray:
    la, lo = np.radians(points[:, 0]), np.radians(points[:, 1])
    return np.stack([np.cos(la) * np.cos(lo), np.cos(la) * np.sin(lo), np.sin(la)], axis=1)


def _angle(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """Angles (rad) between unit vectors, row by row (``b`` may be one vector)."""
    cross = np.linalg.norm(np.cross(a, b), axis=-1)
    return np.arctan2(cross, np.sum(a * b, axis=-1))


def _segment_distance(p: np.ndarray, a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """Great-circle distances (rad) from unit vectors ``p`` to the minor arc ``a``-``b``."""
    ends = np.minimum(_angle(p, a), _angle(p, b))
    n = np.cross(a, b)
    norm = np.linalg.norm(n)
    if norm < 1e-15:
        return ends
    n = n / norm
    s = p @ n
    foot = p - s[:, None] * n
    inside = (np.cross(a, foot) @ n >= 0.0) & (np.cross(foot, b) @ n >= 0.0)
    cross_track = np.abs(np.arcsin(np.clip(s, -1.0, 1.0)))
    return np.where(inside, cross_track, ends)


@traced
def simplify(polyline: Polyline, tol_m: float) -> Polyline:
    """Ramer–Douglas–Peucker on the sphere: every dropped vertex lies within ``tol_m`` of the
    kept line. The ends are always kept."""
    pts = polyline.points
    if len(pts) <= 2:
        return polyline
    xyz = _unit(pts)
    tol = tol_m / EARTH_RADIUS_M
    keep = {0, len(pts) - 1}
    stack = [(0, len(pts) - 1)]
    while stack:
        a, b = stack.pop()
        if b <= a + 1:
            continue
        d = _segment_distance(xyz[a + 1 : b], xyz[a], xyz[b])
        k = int(np.argmax(d))
        if d[k] > tol:
            mid = a + 1 + k
            keep.add(mid)
            stack.append((a, mid))
            stack.append((mid, b))
    return Polyline(polyline.name, polyline.scalerank, pts[sorted(keep)].copy())


def _rounded(points: np.ndarray) -> list[list[float]]:
    """Points rounded to :data:`ROUND_DIGITS`, ``-0.0`` as ``0.0``, repeats dropped."""
    out: list[list[float]] = []
    for la, lo in points:
        p = [round(float(la), ROUND_DIGITS) + 0.0, round(float(lo), ROUND_DIGITS) + 0.0]
        if not out or out[-1] != p:
            out.append(p)
    return out


@traced
def table_text(polylines) -> str:
    """The table's JSON, one river per line. Polylines that round to one point are left out."""
    rows = []
    for line in polylines:
        points = _rounded(line.points)
        if len(points) < 2:
            continue
        river = {"name": line.name, "scalerank": int(line.scalerank), "points": points}
        rows.append(json.dumps(river, separators=(",", ":"), ensure_ascii=False))
    head = (
        "{\n"
        f'"comment": {json.dumps(COMMENT, ensure_ascii=False)},\n'
        f'"source": {json.dumps(SOURCE, ensure_ascii=False)},\n'
        '"rivers": [\n'
    )
    text = head + ",\n".join(rows) + "\n]\n}\n"
    json.loads(text)
    return text


@traced
def bake(geojson_path, etopo_path, out_path) -> dict:
    """Bakes the table to ``out_path`` and returns a summary; refuses a table over
    :data:`MAX_TABLE_BYTES`."""
    features = json.loads(Path(geojson_path).read_text(encoding="utf-8"))["features"]
    selected = select(features)
    raw_points = sum(len(p.points) for p in selected)
    with netCDF4.Dataset(Path(etopo_path)) as ds:
        z = ds.variables["z"]
        lat = ds.variables["lat"]
        lon = ds.variables["lon"]
        if z.shape != (180 * 60, 360 * 60) or abs(float(lat[0]) + 90.0 - 0.5 / 60) > 1e-6:
            raise ValueError(f"{Path(etopo_path).name}: not the 60 arc-second cell grid")
        if abs(float(lon[0]) + 180.0 - 0.5 / 60) > 1e-6:
            raise ValueError(f"{Path(etopo_path).name}: longitudes do not start at -180")
        grid = CellGrid(z, lat_s=-90.0, lon_w=-180.0, per_deg=60)
        oriented = [orient(p, grid) for p in selected]
    reversed_count = sum(
        1 for a, b in zip(selected, oriented) if a.points[0].tolist() != b.points[0].tolist()
    )
    simple = [simplify(p, SIMPLIFY_M) for p in oriented]
    text = table_text(simple)
    size = len(text.encode("utf-8"))
    if size > MAX_TABLE_BYTES:
        raise ValueError(f"the table is {size} bytes, over {MAX_TABLE_BYTES}")
    Path(out_path).write_text(text, encoding="utf-8")
    table = json.loads(text)
    summary = {
        "features": len(features),
        "polylines": len(selected),
        "reversed": reversed_count,
        "rivers": len(table["rivers"]),
        "points_in": raw_points,
        "points_out": sum(len(r["points"]) for r in table["rivers"]),
        "bytes": size,
    }
    _log.info("rivers today: %s", summary)
    return summary


@traced
def main(argv=None) -> int:
    """``timefly-bake rivers-today --geojson ne_10m_rivers_lake_centerlines.geojson --etopo
    ETOPO...60s...nc --out FILE``."""
    import argparse

    from timefly_bake import log

    p = argparse.ArgumentParser(
        prog="timefly-bake rivers-today",
        description="Bake today's major river courses (rivers Task 11b).",
    )
    p.add_argument("--geojson", type=Path, required=True, help="Natural Earth rivers GeoJSON")
    p.add_argument("--etopo", type=Path, required=True, help="ETOPO 2022 60s surface netCDF")
    p.add_argument("--out", type=Path, required=True, help="output table (rivers_today.json)")
    p.add_argument("--log-level", default="INFO", help="stderr log level")
    args = p.parse_args(argv)
    log.configure(None, args.log_level)
    try:
        summary = bake(args.geojson, args.etopo, args.out)
    except Exception as exc:  # already logged with its traceback by @traced
        _log.error("rivers-today bake failed: %s", exc)
        return 1
    print(json.dumps(summary))
    return 0


__all__ = [
    "CellGrid",
    "Polyline",
    "bake",
    "main",
    "orient",
    "select",
    "simplify",
    "table_text",
]
