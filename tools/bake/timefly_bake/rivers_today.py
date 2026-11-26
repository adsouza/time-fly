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
- **Orient every part, the most certain first** (Task 11d fix rounds 2 and 3;
  :func:`orient_by_connections`): ETOPO's end heights and how the parts join both vote, each as
  sure as its evidence, and the surest decides; a decided part's junctions then vote for the
  parts they join. A reservoir's or a delta arm's ends are flat water, so a clearer neighbour
  decides it.
- **Orient** (ETOPO's vote): upstream first. Each end's height is the lowest ETOPO 2022 60″ surface cell whose
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
from collections.abc import Callable
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


@traced
def end_height(grid: CellGrid, lat: float, lon: float, ids, surfaces) -> float:
    """A part end's height for orienting it: :meth:`CellGrid.lowest_near`, but a cell inside a
    mapped lake (``ids``, the 0.05-degree lake map) reads as the lake's surface
    (``surfaces[id]``): ETOPO holds the bed of the deep lakes (Baikal's is about -1,000 m), and an
    outlet's end would read it (Task 11d fix round 3)."""
    from timefly_bake.lakemap import cell_of

    height, width = ids.shape
    cells = ORIENT_CELLS
    i = min(grid.rows - 1, max(0, math.floor((lat - grid.lat_s) * grid.per_deg)))
    j = min(grid.cols - 1, max(0, math.floor((lon - grid.lon_w) * grid.per_deg)))
    rows = range(max(0, i - cells), min(grid.rows, i + cells + 1))
    cols = range(max(0, j - cells), min(grid.cols, j + cells + 1))
    window = np.ma.filled(
        np.asarray(grid.z[rows.start : rows.stop, cols.start : cols.stop], dtype=np.float64),
        np.nan,
    )
    lowest = math.inf
    for a, r in enumerate(rows):
        for b, c in enumerate(cols):
            la = grid.lat_s + (r + 0.5) / grid.per_deg
            lo = grid.lon_w + (c + 0.5) / grid.per_deg
            lake = int(ids[cell_of(la, lo, width, height)])
            value = surfaces.get(lake, window[a, b]) if lake else window[a, b]
            if not math.isnan(value):
                lowest = min(lowest, float(value))
    return lowest


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


def _junctions(ends) -> Callable[[np.ndarray], tuple[float, float]]:
    """A point's junction among ``ends``: ends within :data:`SIMPLIFY_M` of each other, chained,
    are one junction, named by its least key. The table keeps no detail finer than its
    simplification, and Natural Earth leaves streams unjoined by less (at Guiping the Xun ends
    350 m from the point the Quan and the Yu share)."""
    keys = sorted({_key(e) for e in ends})
    root = {k: k for k in keys}

    def find(k):
        while root[k] != k:
            root[k] = root[root[k]]
            k = root[k]
        return k

    band = math.degrees(SIMPLIFY_M / EARTH_RADIUS_M)
    bands: dict[int, list[tuple[float, float]]] = {}
    for k in keys:
        bands.setdefault(math.floor(k[0] / band), []).append(k)
    xyz = {k: _unit(np.array([k]))[0] for k in keys}
    for b, members in bands.items():
        near = members + bands.get(b + 1, [])
        for x, a in enumerate(members):
            for c in near[x + 1 :]:
                if float(_angle(xyz[a][None, :], xyz[c])[0]) * EARTH_RADIUS_M <= SIMPLIFY_M:
                    ra, rc = find(a), find(c)
                    if ra != rc:
                        root[max(ra, rc)] = min(ra, rc)
    return lambda point: find(_key(point))


def _key(point) -> tuple[float, float]:
    """A point as the table stores it (:data:`ROUND_DIGITS`): ends the table cannot tell apart
    are one junction (Natural Earth's Sulina arm starts 30 m from the Chilia's end and the Sfantu
    Gheorghe's start; the table writes all three as one point)."""
    return (round(float(point[0]), ROUND_DIGITS) + 0.0, round(float(point[1]), ROUND_DIGITS) + 0.0)


#: How sure a tributary-mouth vote is (an end on another part's interior vertex), as a fall in
#: metres: it decides a part flatter than this, and a clearer fall overrules it (a distributary
#: leaves another part's interior too).
MOUTH_VOTE_M = 1.0


@traced
def orient_by_connections(parts, heights, fixed=(), rules=None) -> tuple[list[Polyline], dict]:
    """``parts`` upstream first, the most certain first (Task 11d fix rounds 2 and 3).

    ``fixed`` are parts already oriented (certain); ``heights(part)`` gives ETOPO's ``(first,
    last)`` end heights of a raw part. Each undecided part has candidates, each a direction and
    how sure it is:

    - **ETOPO**: reversed when the last end stands higher; sure by the ends' height difference.
    - **A tributary's mouth**: an end on another part's interior vertex is its downstream end;
      sure as a fall of :data:`MOUTH_VOTE_M`.
    - **A junction**, once every other part meeting there is decided: an end where one starts and
      none ends is downstream (it flows into that part); an end where they all end is upstream
      (it is their outflow); a fork or a confluence with a known inflow gives no vote. Where two
      parts meet (a chain), as sure as the last decision there; where three or more meet, the
      part's role is inferred, as sure as :data:`MOUTH_VOTE_M` at most.

    The surest candidate of any part is taken first, and its junctions offer candidates to the
    parts they join. Candidates of one part equally sure in opposite directions are "ambiguous":
    ETOPO decides. Returns the parts in their order and the counts by rule (``{"topology",
    "etopo", "ambiguous"}``); ``rules`` (a list) gets each part's rule."""
    import heapq

    n = len(parts)
    every = list(parts) + list(fixed)
    junc = _junctions([e for p in every for e in (p.points[0], p.points[-1])])
    # A part shorter than the junction reach (both ends one junction) joins nothing.
    short = [junc(p.points[0]) == junc(p.points[-1]) for p in every]
    ends_at: dict[tuple[float, float], list[int]] = {}
    interior_at: dict[tuple[float, float], set[int]] = {}
    for i, p in enumerate(every):
        if not short[i]:
            for end in (p.points[0], p.points[-1]):
                ends_at.setdefault(junc(end), []).append(i)
        for pt in p.points[1:-1]:
            interior_at.setdefault(_key(pt), set()).add(i)
    state: list[bool | None] = [None] * n + [True] * len(fixed)
    how: list[str] = [""] * n
    h = [heights(p) for p in parts]
    etopo = [not h[i][1] > h[i][0] for i in range(n)]
    heap: list[tuple[float, int, int, bool, str]] = []
    seq = 0
    pending: dict[int, list[tuple[float, bool]]] = {}

    def offer(i, sure, keep, kind):
        nonlocal seq
        pending.setdefault(i, []).append((sure, keep))
        heapq.heappush(heap, (-sure, seq, i, keep, kind))
        seq += 1

    def first(i):
        return junc(every[i].points[0] if state[i] else every[i].points[-1])

    def last(i):
        return junc(every[i].points[-1] if state[i] else every[i].points[0])

    def junction(j, at):
        """The direction a decided junction at ``at`` gives undecided part ``j``, if any, and
        whether it is only inferred (a junction of three or more parts)."""
        others = [k for k in ends_at.get(at, ()) if k != j]
        if not others or any(state[k] is None for k in others):
            # A junction votes once every other part meeting there is known: an outflow seen
            # before an undecided inflow would make a fork's arm (the Mekong's Tien at Phnom
            # Penh) look like a tributary.
            return None
        starts = any(first(k) == at for k in others)
        inflow = any(last(k) == at for k in others)
        if starts and not inflow:
            # The outflow is known and no inflow: this part flows in. With an inflow too, a third
            # part may be a tributary or a distributary (a fork): no vote.
            down = True
        elif not starts:
            down = False
        else:
            return None
        raw_first = junc(parts[j].points[0]) == at
        # Downstream at its raw first end, or upstream at its raw last end: reverse it. Two
        # parts meeting are a chain and the direction carries over; at three or more, the part's
        # role (a fork's missing inflow, a confluence's missing outflow) is only inferred, and
        # Natural Earth often leaves a stem unjoined (the Danube 12 km short of the fork of the
        # Chilia, Sulina and Sfantu Gheorghe arms).
        return not (down == raw_first), len(others) > 1

    def decided(i, sure):
        for at in {first(i), last(i)}:
            for j in ends_at.get(at, ()):
                if j < n and state[j] is None:
                    vote = junction(j, at)
                    if vote is not None:
                        keep, inferred = vote
                        offer(j, min(sure, MOUTH_VOTE_M) if inferred else sure, keep, "topology")

    for i in range(n):
        offer(i, abs(h[i][0] - h[i][1]), etopo[i], "etopo")
        a, b = _key(parts[i].points[0]), _key(parts[i].points[-1])
        at_a = any(j != i for j in interior_at.get(a, ()))
        at_b = any(j != i for j in interior_at.get(b, ()))
        if at_a != at_b:
            offer(i, MOUTH_VOTE_M, at_b, "topology")
    for k in range(n, len(every)):
        state[k] = True
        decided(k, float("inf"))
    while heap:
        sure, _, i, keep, kind = heapq.heappop(heap)
        if state[i] is not None:
            continue
        rivals = [c for c in pending.get(i, ()) if c[0] == -sure and c[1] != keep]
        if rivals:
            keep, kind = etopo[i], "ambiguous"
        state[i] = keep
        how[i] = kind
        decided(i, -sure)
    out = []
    for i, p in enumerate(parts):
        out.append(p if state[i] else Polyline(p.name, p.scalerank, p.points[::-1].copy(), p.lake))
    counts = {k: how.count(k) for k in ("topology", "etopo", "ambiguous")}
    if rules is not None:
        rules.extend(how)
    return out, counts


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

    ids, map_levels = lakemap.decode(Path(lake_map_path).read_bytes())
    surfaces = {k + 1: float(x) for k, x in enumerate(map_levels)}
    # The level table's lakes: ETOPO holds their beds, so the map's median is no surface.
    table = json.loads((Path(lake_map_path).parent / "lake_levels.json").read_text())["lakes"]
    for lake in table:
        from timefly_bake.lakemap import cell_of

        lake_id = int(ids[cell_of(lake["lat"], lake["lon"], ids.shape[1], ids.shape[0])])
        if lake_id:
            surfaces[lake_id] = float(lake["level_m"])
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
    index = {id(p): k for k, p in enumerate(selected)}
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
        ends = [
            (
                end_height(grid, *p.points[0], ids, surfaces),
                end_height(grid, *p.points[-1], ids, surfaces),
            )
            for p in selected
        ]
        rules: list[str] = []
        # The hand-added channels are cited upstream first: certain, and they decide the arms
        # that feed them.
        extras = read_extras(Path(extra_path).read_text(encoding="utf-8"))
        oriented, orient_counts = orient_by_connections(
            selected, lambda p: ends[index[id(p)]], fixed=extras, rules=rules
        )
    reversed_count = sum(
        1 for a, b in zip(selected, oriented) if a.points[0].tolist() != b.points[0].tolist()
    )
    simple = [simplify(p, SIMPLIFY_M) for p in oriented]
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
        "orientation": orient_counts,
        "orientation_rivers": {
            k: sum(1 for p, r in zip(selected, rules) if r == k and not p.lake) for k in orient_counts
        },
        "orientation_lakes": {
            k: sum(1 for p, r in zip(selected, rules) if r == k and p.lake) for k in orient_counts
        },
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
    "end_height",
    "Polyline",
    "bake",
    "is_artificial",
    "lake_cover",
    "main",
    "orient",
    "MOUTH_VOTE_M",
    "orient_by_connections",
    "read_extras",
    "select",
    "simplify",
    "table_text",
]
