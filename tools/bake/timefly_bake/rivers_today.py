"""Today's major river courses (rivers Task 11b): Natural Earth 1:10m "Rivers + lake centerlines"
v5.1.2 centrelines, upstream first and simplified, which the continental pass burns into its
routing wherever the terrain is today's (``timefly_core::hydrology::today_rivers``).

- **Select:** features whose ``featurecla`` is ``River`` with ``scalerank`` at most
  :data:`TODAY_RIVER_MAX_SCALERANK`; each part of a MultiLineString is its own polyline. A
  ``Lake Centerline`` of the same ranks is kept when today's lake map does not cover it (Task 11d
  fix round 1, the owner: "Burn them"): the lake map drops reservoirs, so the river through a
  reservoir (Novosibirsk, Krasnoyarsk, the Volga's chain) had neither a lake nor a course.
  Covered means at least :data:`LAKE_CENTRELINE_MAX_COVER` of the part's length lies in mapped
  lake cells (:func:`lake_cover`); those lakes are the lake map's (Task 8b). Artificial channels are
  left out too (Task 11d): Natural Earth files canals as ``River``, so a feature whose name holds
  a word of :data:`ARTIFICIAL_NAME_WORDS` is dropped (the Suez, Panama, Ismailiya and Gharraf
  canals and the Ferenc Csatorna, the Danube-Tisa-Danube Canal; the Hindiyah Channel, a natural
  branch of the Euphrates, stays).
- **Hand-added channels** (Task 11d fix round 1, the owner: "Hand-add delta channels"):
  ``data/hydrology/rivers_extra.json`` holds a delta's main channel where Natural Earth's course
  stops above it, each entry cited (:func:`read_extras`). They are appended after Natural
  Earth's courses as given: already upstream first and simplified.
- **Orient lake centrelines by their connections** (Task 11d fix round 2): a reservoir's ends
  are flat water, so the oriented courses it joins decide (:func:`orient_lake_parts`); ETOPO
  only where neither end connects, or the ends disagree.
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
#: Name words of an artificial channel, matched whole and ignoring case: "canal", and the
#: Hungarian "csatorna" (canal) of the Ferenc Csatorna.
ARTIFICIAL_NAME_WORDS = ("canal", "csatorna")
#: A lake centreline part with at least this share of its length in mapped lake cells is covered
#: by today's lake map and left out.
LAKE_CENTRELINE_MAX_COVER = 0.5
#: The step along a part at which :func:`lake_cover` samples the map (m).
LAKE_COVER_STEP_M = 1000.0
LAKE_MAP_PATH = Path(__file__).resolve().parents[3] / "data" / "hydrology" / "lake_map.bin"
EXTRA_PATH = Path(__file__).resolve().parents[3] / "data" / "hydrology" / "rivers_extra.json"
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
    "Today's major river courses (rivers Task 11b): Natural Earth rivers of scalerank <= 7, and lake "
    "centrelines of those ranks that today's lake map does not cover (reservoirs; canals left out), then the "
    "cited hand-added delta channels of data/hydrology/rivers_extra.json, one entry per line part, upstream first (the end with the higher "
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
    #: A ``Lake Centerline`` part (oriented by :func:`orient_lake_parts`).
    lake: bool = False


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
def is_artificial(name: str) -> bool:
    """Whether a feature's name marks an artificial channel (:data:`ARTIFICIAL_NAME_WORDS`)."""
    return any(word in ARTIFICIAL_NAME_WORDS for word in name.lower().split())


@traced
def lake_cover(points: np.ndarray, ids: np.ndarray) -> float:
    """The share of the (lat, lon) polyline's length whose sample points (the midpoints of
    :data:`LAKE_COVER_STEP_M` steps along each segment) lie in a lake map cell (``ids`` from
    :func:`timefly_bake.lakemap.decode`, 0 for no lake). 0 for a polyline of no length."""
    from timefly_bake.lakemap import cell_of

    height, width = ids.shape
    xyz = _unit(points)
    inside = total = 0.0
    for k in range(len(points) - 1):
        length = float(_angle(xyz[k : k + 1], xyz[k + 1])[0]) * EARTH_RADIUS_M
        steps = max(1, math.ceil(length / LAKE_COVER_STEP_M))
        for i in range(steps):
            t = (i + 0.5) / steps
            lat, lon = points[k] + t * (points[k + 1] - points[k])
            row, col = cell_of(float(lat), float(lon), width, height)
            if ids[row, col] != 0:
                inside += length / steps
            total += length / steps
    return inside / total if total > 0.0 else 0.0


@traced
def select(features, cover=None) -> list[Polyline]:
    """Each part of every ``River`` feature with ``scalerank <= TODAY_RIVER_MAX_SCALERANK`` that
    is not an artificial channel (:func:`is_artificial`), as a polyline of (lat, lon), in file
    order; with ``cover(name, points)`` (the share of a part's length in mapped lakes), also each
    such ``Lake Centerline`` part whose cover is under :data:`LAKE_CENTRELINE_MAX_COVER`."""
    out = []
    for f in features:
        p = f["properties"]
        cla = p.get("featurecla")
        lake = cla == "Lake Centerline"
        if (cla != "River" and not (lake and cover is not None)) or (
            _rank(p) > TODAY_RIVER_MAX_SCALERANK
        ):
            continue
        name = (p.get("name") or p.get("name_en") or "").strip()
        if is_artificial(name):
            continue
        g = f["geometry"]
        parts = g["coordinates"] if g["type"] == "MultiLineString" else [g["coordinates"]]
        for part in parts:
            lonlat = np.asarray(part, dtype=np.float64)[:, :2]
            points = lonlat[:, ::-1].copy()
            if lake and cover(name, points) >= LAKE_CENTRELINE_MAX_COVER:
                continue
            out.append(Polyline(name, _rank(p), points, lake))
    return out


def _key(point) -> tuple[float, float]:
    return (round(float(point[0]), 6), round(float(point[1]), 6))


@traced
def orient_lake_parts(lakes, rivers, fallback) -> tuple[list[Polyline], dict]:
    """The lake centreline parts ``lakes`` upstream first, by their connections to the oriented
    ``rivers``: an end at a course's first point (its upstream end) is the part's downstream end,
    an end at a course's last point its upstream end. Repeated, so a chain of parts resolves from
    the courses it joins. A part whose ends disagree, or which touches nothing, takes
    ``fallback(part)`` (ETOPO). Returns the parts in their order and the counts by rule:
    ``{"topology", "etopo", "ambiguous"}``."""
    firsts = {_key(p.points[0]) for p in rivers}
    lasts = {_key(p.points[-1]) for p in rivers}
    done: dict[int, Polyline] = {}
    ambiguous: set[int] = set()
    changed = True
    while changed:
        changed = False
        for k, part in enumerate(lakes):
            if k in done or k in ambiguous:
                continue
            a, b = _key(part.points[0]), _key(part.points[-1])
            keep = (a in lasts) or (b in firsts)
            flip = (a in firsts) or (b in lasts)
            if keep and flip:
                ambiguous.add(k)
                continue
            if not (keep or flip):
                continue
            out = part
            if flip:
                out = Polyline(part.name, part.scalerank, part.points[::-1].copy(), True)
            done[k] = out
            firsts.add(_key(out.points[0]))
            lasts.add(_key(out.points[-1]))
            changed = True
    counts = {"topology": len(done), "etopo": 0, "ambiguous": len(ambiguous)}
    result = []
    for k, part in enumerate(lakes):
        if k in done:
            result.append(done[k])
        else:
            counts["etopo"] += k not in ambiguous
            result.append(fallback(part))
    return result, counts


@traced
def read_extras(text: str) -> list[Polyline]:
    """The hand-added channels' polylines from ``rivers_extra.json``'s text. Each entry needs a
    ``name``, a ``scalerank`` in 0..7, a cited ``source``, a ``mouth`` and at least two
    ``points`` [lat, lon]; anything else is a ``ValueError``."""
    out = []
    for entry in json.loads(text)["rivers"]:
        name = entry.get("name", "?")
        if not str(entry.get("source", "")).strip():
            raise ValueError(f"{name}: no source")
        points = np.asarray(entry.get("points", []), dtype=np.float64)
        rank = int(entry.get("scalerank", -1))
        if points.ndim != 2 or points.shape[0] < 2 or points.shape[1] != 2:
            raise ValueError(f"{name}: needs at least two [lat, lon] points")
        if not 0 <= rank <= TODAY_RIVER_MAX_SCALERANK or len(entry.get("mouth", [])) != 2:
            raise ValueError(f"{name}: needs a scalerank in 0..7 and a mouth")
        out.append(Polyline(name, rank, points))
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
def bake(
    geojson_path, etopo_path, out_path, lake_map_path=LAKE_MAP_PATH, extra_path=EXTRA_PATH
) -> dict:
    """Bakes the table to ``out_path`` and returns a summary; refuses a table over
    :data:`MAX_TABLE_BYTES`."""
    features = json.loads(Path(geojson_path).read_text(encoding="utf-8"))["features"]
    from timefly_bake import lakemap

    ids, _ = lakemap.decode(Path(lake_map_path).read_bytes())
    lake_parts: dict[str, list] = {"added": [], "covered": []}

    def cover(name, points):
        share = lake_cover(points, ids)
        key = "covered" if share >= LAKE_CENTRELINE_MAX_COVER else "added"
        lake_parts[key].append(
            [name, round(share, 3), [round(float(x), 2) for x in points[0]],
             [round(float(x), 2) for x in points[-1]]]
        )
        return share

    selected = select(features, cover=cover)
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
        rivers = [orient(p, grid) for p in selected if not p.lake]
        lakes = [p for p in selected if p.lake]
        lakes_oriented, orient_counts = orient_lake_parts(
            lakes, rivers, lambda p: orient(p, grid)
        )
        by_kind = {False: iter(rivers), True: iter(lakes_oriented)}
        oriented = [next(by_kind[p.lake]) for p in selected]
    reversed_count = sum(
        1 for a, b in zip(selected, oriented) if a.points[0].tolist() != b.points[0].tolist()
    )
    simple = [simplify(p, SIMPLIFY_M) for p in oriented]
    extras = read_extras(Path(extra_path).read_text(encoding="utf-8"))
    simple += extras
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
        "extras": [p.name for p in extras],
        "lake_orientation": orient_counts,
        "lake_centrelines_added": lake_parts["added"],
        "lake_centrelines_covered": lake_parts["covered"],
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
    "ARTIFICIAL_NAME_WORDS",
    "CellGrid",
    "Polyline",
    "bake",
    "is_artificial",
    "lake_cover",
    "main",
    "orient",
    "orient_lake_parts",
    "read_extras",
    "select",
    "simplify",
    "table_text",
]
