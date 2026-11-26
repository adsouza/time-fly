"""The 1-arc-minute reference pass for the reality check (rivers and lakes Task 11, design §7).

It runs the continental pass's rules (``timefly_core::hydrology``: ``route.rs``, ``lakes.rs``,
``today.rs``) on ETOPO 2022's 60 arc-second surface over a box, at 0 ka, and writes the box's
rivers as a JSON fixture the game's ``--hydro-check`` compares its own rivers against.

What is mirrored, rule for rule (the golden fixture ``hydro_golden.json`` pins it):

- Routing (``route.rs``): the ocean (below-zero components, the largest, the strait dilation),
  the priority flood keyed ``(filled, seq)``, the steepest strictly-lower receiver (else the
  parent), basins in pop order and the redirect to the exit.
- Discharge and lakes (``lakes.rs``): own runoff, then the walk upstream first, each basin
  resolved at its exit with the water balance (Open or Terminal by the groups of distinct
  elevations), and the BFS routing inside it.
- The present day (``today.rs``, Task 8b round 2): with the lake map, an open basin passes its
  water through, a closed one ends at its lowest node (a dry sink), and every mapped lake stands
  at its baked surface over its own mapped nodes, its outlet the footprint node with the largest
  discharge whose chain meets no other of its nodes; discharge is then recomputed over the
  redirected receivers.
- The climate (``climate.rs``): Beyer et al. 2020 at 0 ka, bilinear on the 1 degree grid, a tap
  with no value taking the zonal climate at a coast distance of 0, as the game's
  ``BeyerClimate`` does; Turc-Pike runoff and net lake evaporation from the climate table.
- Straits and gorges (``straits.rs``): each table entry's nodes, a quarter spacing apart along
  its polyline, are lowered to its level (a strait's sill, a gorge's river surface interpolated
  between its points).

What differs, by design: the graph is a latitude-longitude grid of ETOPO's own cells
(:class:`LatLonGraph`, haversine distances); the box's ocean starts from every below-zero
component that touches the box's edge, not only the largest (the box cuts the open sea into
pieces); and the box's edge nodes are extra flood seeds, so water may leave the box (an edge
node with no lower neighbour has no receiver). No node is ice. Table lakes (the lake level table)
are not mirrored: none lies in a reference box, and :func:`reference` refuses a box that holds
one. The data-flat and past-era rules of ``lakes.rs`` are not mirrored either: they never run at
the present-day snapshot.

The fixture (:func:`reference`)::

    {"box": [lat_n, lon_w, lat_s, lon_e],
     "links": [{"lonlat": [[lon, lat], ...], "area_km2": [...], "elevation_m": [...],
                "discharge_m3s": f}],
     "lakes": [{"lonlat": [lon, lat], "area_km2": f, "basin_precip_mm": f}]}

Links run between confluences of the river nodes (discharge at least
``RIVER_MIN_DISCHARGE_M3S``, outside lakes) whose drainage area is at least
:data:`LINK_MIN_AREA_KM2`, upstream first; a link that leaves that set ends at the node it drains
to. Each is simplified to :data:`SIMPLIFY_M` (Ramer-Douglas-Peucker), and ``discharge_m3s`` is
its last river node's. Lakes are those of at least ``LARGE_LAKE_KM2``, at their outlet, with the
mean precipitation over the lake and the river nodes upstream of it.
"""

from __future__ import annotations

import heapq
import json
import logging
import math
from collections import deque
from dataclasses import dataclass, field
from pathlib import Path

import netCDF4
import numpy as np

from timefly_bake import formats, lakemap
from timefly_bake.log import traced

_log = logging.getLogger(__name__)
_REPO = Path(__file__).resolve().parents[3]
HYDROLOGY_DIR = _REPO / "data" / "hydrology"

# timefly_core::hydrology::params, and the Earth the game uses (globe::EARTH_RADIUS_KM).
EARTH_RADIUS_M = 6_371_000.0
SECONDS_PER_YEAR = 365.25 * 86_400.0
OCEAN_STRAIT_M = 25_000.0
DEPRESSION_EPS_M = 0.01
DATA_LAKE_FLAT_TOL_M = 0.5
MIN_LAKE_DEPTH_M = DATA_LAKE_FLAT_TOL_M
RIVER_MIN_DISCHARGE_M3S = 20.0
LARGE_LAKE_KM2 = 1_000.0

LINK_MIN_AREA_KM2 = 10_000.0
SIMPLIFY_M = 1_000.0
PER_DEG = 60

# (column, row) steps in the order E, NE, N, NW, W, SW, S, SE; row 0 is north
# (lattice.rs NEIGHBOUR_STEPS).
NEIGHBOUR_STEPS = [(1, 0), (1, -1), (0, -1), (-1, -1), (-1, 0), (-1, 1), (0, 1), (1, 1)]
RECEIVER_NONE = 8
RECEIVER_EXIT = 9
NO_BASIN = -1

# (lat_n, lon_w, lat_s, lon_e), degrees.
BOXES = {"indus": (37.5, 66.0, 23.0, 82.0), "amazon": (6.0, -80.0, -21.0, -48.0)}


class _Grid:
    """A ``rows x cols`` grid, row 0 north, node ``row * cols + col``; neighbours in
    :data:`NEIGHBOUR_STEPS` order, ``None`` off the grid."""

    rows: int
    cols: int

    def __len__(self) -> int:
        return self.rows * self.cols

    def neighbours(self, i: int) -> list[int | None]:
        r, c = divmod(i, self.cols)
        out: list[int | None] = []
        for dc, dr in NEIGHBOUR_STEPS:
            rr, cc = r + dr, c + dc
            out.append(rr * self.cols + cc if 0 <= rr < self.rows and 0 <= cc < self.cols else None)
        return out

    def neighbour_offsets(self, i: int) -> list[tuple[int, int]]:
        """``(k, j)`` for each neighbour ``j`` of ``i`` that exists, in neighbour order."""
        r, c = divmod(i, self.cols)
        rows, cols = self.rows, self.cols
        out = []
        for k, (dc, dr) in enumerate(NEIGHBOUR_STEPS):
            rr, cc = r + dr, c + dc
            if 0 <= rr < rows and 0 <= cc < cols:
                out.append((k, rr * cols + cc))
        return out

    def edge(self, i: int) -> bool:
        r, c = divmod(i, self.cols)
        return r == 0 or c == 0 or r == self.rows - 1 or c == self.cols - 1


class GridGraph(_Grid):
    """``lattice.rs``'s ``GridGraph``: a flat grid at ``spacing_m`` (the golden fixture)."""

    def __init__(self, rows: int, cols: int, spacing_m: float):
        self.rows, self.cols, self.spacing_m = rows, cols, spacing_m

    def distance_k(self, i: int, k: int) -> float:
        dc, dr = NEIGHBOUR_STEPS[k]
        return self.spacing_m * math.sqrt(dr * dr + dc * dc)

    def area_m2(self, i: int) -> float:
        return self.spacing_m * self.spacing_m

    def mean_spacing_m(self) -> float:
        return self.spacing_m


class LatLonGraph(_Grid):
    """Cells of a latitude-longitude grid, ``per_deg`` to the degree, from ``(lat_n, lon_w)``:
    node ``(row, col)`` at the cell centre ``(lat_n - (row + 0.5) / per_deg, lon_w + (col +
    0.5) / per_deg)``. Distances are haversine between centres; a cell's area is
    ``R² Δλ (sin φ₁ - sin φ₂)``."""

    def __init__(self, lat_n: float, lon_w: float, rows: int, cols: int, per_deg: int):
        self.lat_n, self.lon_w = lat_n, lon_w
        self.rows, self.cols, self.per_deg = rows, cols, per_deg
        step = math.radians(1.0 / per_deg)
        lat_c = [math.radians(lat_n - (r + 0.5) / per_deg) for r in range(rows)]
        north = [math.radians(lat_n - r / per_deg) for r in range(rows + 1)]
        self._area = [
            EARTH_RADIUS_M**2 * step * (math.sin(north[r]) - math.sin(north[r + 1]))
            for r in range(rows)
        ]
        # Distance of step k from row r, by symmetry the same for every column.
        self._dist = []
        for r in range(rows):
            row = []
            for dc, dr in NEIGHBOUR_STEPS:
                lat2 = math.radians(lat_n - (r + dr + 0.5) / per_deg)
                row.append(_haversine(lat_c[r], 0.0, lat2, dc * step))
            self._dist.append(row)

    def lat(self, i: int) -> float:
        return self.lat_n - (i // self.cols + 0.5) / self.per_deg

    def lon(self, i: int) -> float:
        return self.lon_w + (i % self.cols + 0.5) / self.per_deg

    def distance_k(self, i: int, k: int) -> float:
        return self._dist[i // self.cols][k]

    def distance_m(self, i: int, j: int) -> float:
        return _haversine(
            math.radians(self.lat(i)),
            math.radians(self.lon(i)),
            math.radians(self.lat(j)),
            math.radians(self.lon(j)),
        )

    def area_m2(self, i: int) -> float:
        return self._area[i // self.cols]

    def mean_spacing_m(self) -> float:
        return math.sqrt(sum(self._area) / self.rows)

    def node_at(self, lat: float, lon: float) -> int | None:
        r = math.floor((self.lat_n - lat) * self.per_deg)
        c = math.floor((lon - self.lon_w) * self.per_deg)
        if 0 <= r < self.rows and 0 <= c < self.cols:
            return r * self.cols + c
        return None


def _haversine(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    h = math.sin((lat2 - lat1) / 2) ** 2 + math.cos(lat1) * math.cos(lat2) * math.sin(
        (lon2 - lon1) / 2
    ) ** 2
    return 2.0 * EARTH_RADIUS_M * math.asin(min(1.0, math.sqrt(h)))


def _back(k: int) -> int:
    """The neighbour index pointing back along step ``k`` (on a grid, the opposite step)."""
    return (k + 4) % 8


@traced
def ocean_mask(graph, elevation, extra_components: bool = False) -> list[bool]:
    """``route.rs``'s ocean: below-zero components (8-connected, numbered in scan order), the
    largest first (ties: the lowest id), then the strait dilation by ``ceil(OCEAN_STRAIT_M /
    spacing)`` steps through any node, adding every below-zero component it touches. With
    ``extra_components`` (a box), every component touching the graph's edge starts as ocean
    too."""
    n = len(graph)
    e = elevation
    component = [-1] * n
    starts: list[int] = []
    members: list[int] = []
    for i in range(n):
        if e[i] < 0.0 and component[i] < 0:
            c = len(starts)
            starts.append(len(members))
            component[i] = c
            members.append(i)
            queue = deque([i])
            while queue:
                v = queue.popleft()
                for u in graph.neighbours(v):
                    if u is not None and e[u] < 0.0 and component[u] < 0:
                        component[u] = c
                        members.append(u)
                        queue.append(u)
    starts.append(len(members))
    count = len(starts) - 1
    ocean = [False] * n
    if count == 0:
        return ocean
    size = [starts[c + 1] - starts[c] for c in range(count)]
    largest = 0
    for c in range(1, count):
        if size[c] > size[largest]:
            largest = c
    reach = math.ceil(OCEAN_STRAIT_M / graph.mean_spacing_m())
    steps = [math.inf] * n
    is_ocean = [False] * count
    queue = deque()

    def add(c: int) -> None:
        is_ocean[c] = True
        for v in members[starts[c] : starts[c + 1]]:
            ocean[v] = True
            steps[v] = 0
            queue.append(v)

    add(largest)
    if extra_components:
        for c in range(count):
            if not is_ocean[c] and any(graph.edge(v) for v in members[starts[c] : starts[c + 1]]):
                add(c)
    while queue:
        v = queue.popleft()
        d = steps[v]
        if d < reach:
            for u in graph.neighbours(v):
                if u is None or not d + 1 < steps[u]:
                    continue
                steps[u] = d + 1
                c = component[u]
                if c >= 0 and not is_ocean[c]:
                    add(c)
                else:
                    queue.append(u)
    return ocean


@dataclass
class Basin:
    nodes: list[int]
    spill_m: float
    exit: int
    rim: int


@dataclass
class Routed:
    elevation_m: list[float]
    ocean: list[bool]
    filled_m: list[float]
    receiver: list[int]
    order: list[int]
    basins: list[Basin]
    basin_of: list[int]


@traced
def route(graph, elevation, ocean, outlets=None) -> Routed:
    """``route.rs``'s flood, receivers and basins for a given ``ocean``. ``outlets`` (a list of
    flags, the box's edge) are land nodes seeded with the coast, in ascending id: water reaches
    them at their own level and leaves (no receiver unless a neighbour is lower)."""
    n = len(graph)
    e = [float(x) for x in elevation]
    filled = list(e)
    visited = [False] * n
    parent = [RECEIVER_NONE] * n
    heap: list[tuple[float, int, int]] = []
    seq = 0
    any_seed = any(ocean) or (outlets is not None and any(outlets))
    if any_seed:
        for i in range(n):
            if ocean[i]:
                if any(not ocean[u] for u in graph.neighbours(i) if u is not None):
                    heapq.heappush(heap, (filled[i], seq, i))
                    seq += 1
            elif outlets is not None and outlets[i]:
                visited[i] = True
                heapq.heappush(heap, (filled[i], seq, i))
                seq += 1
    elif n:
        lowest = min(range(n), key=lambda i: (e[i], i))
        visited[lowest] = True
        heapq.heappush(heap, (filled[lowest], seq, lowest))
        seq += 1
    order: list[int] = []
    while heap:
        level, _, c = heapq.heappop(heap)
        if not ocean[c]:
            order.append(c)
        for k, u in graph.neighbour_offsets(c):
            if ocean[u] or visited[u]:
                continue
            visited[u] = True
            filled[u] = e[u] if e[u] >= level else level
            parent[u] = _back(k)
            heapq.heappush(heap, (filled[u], seq, u))
            seq += 1
    receiver = [RECEIVER_NONE] * n
    for i in range(n):
        if ocean[i]:
            continue
        level = filled[i]
        best_k, best_s = -1, 0.0
        for k, j in graph.neighbour_offsets(i):
            other = filled[j]
            if other < level:
                s = (level - other) / graph.distance_k(i, k)
                if best_k < 0 or s > best_s:
                    best_k, best_s = k, s
        receiver[i] = best_k if best_k >= 0 else parent[i]
    basin_of = [NO_BASIN] * n
    basins: list[Basin] = []

    def depressed(i: int) -> bool:
        return filled[i] - e[i] > DEPRESSION_EPS_M

    for v in order:
        if basin_of[v] != NO_BASIN or not depressed(v):
            continue
        b = len(basins)
        basin_of[v] = b
        rim = graph.neighbours(v)[parent[v]]
        nodes = [v]
        queue = deque([v])
        while queue:
            x = queue.popleft()
            for u in graph.neighbours(x):
                if u is not None and basin_of[u] == NO_BASIN and depressed(u):
                    basin_of[u] = b
                    nodes.append(u)
                    queue.append(u)
        nodes.sort()
        basins.append(Basin(nodes, filled[v], v, rim))
    for b, basin in enumerate(basins):
        for v in basin.nodes:
            if v == basin.exit:
                continue
            to = graph.neighbours(v)[receiver[v]]
            if basin_of[to] != b:
                receiver[v] = RECEIVER_EXIT
    return Routed(e, list(ocean), filled, receiver, order, basins, basin_of)


def _receiver_of(graph, routed: Routed, i: int) -> int | None:
    k = routed.receiver[i]
    if k == RECEIVER_NONE:
        return None
    if k == RECEIVER_EXIT:
        return routed.basins[routed.basin_of[i]].exit
    return graph.neighbours(i)[k]


@traced
def receiver_of(graph, routed: Routed, i: int) -> int | None:
    """The node ``i`` drains to (``route.rs`` ``receiver_of``)."""
    return _receiver_of(graph, routed, i)


@dataclass
class Lake:
    level_m: float
    spill_m: float
    kind: str
    inflow_m3s: float
    evaporation_m3s: float
    outflow_m3s: float
    area_km2: float
    exit: int
    footprint: list[int] = field(default_factory=list)


@dataclass
class Accumulated:
    q: np.ndarray
    area_km2: np.ndarray
    lakes: list[Lake]
    lake_of: dict[int, int]
    evaporated_m3s: float


def _balance(e, cap, spill, q_in):
    """``lakes.rs`` ``balance``: ``(kind, level, evaporation, outflow, wet_through)``."""
    by_e = sorted(range(len(e)), key=lambda k: (e[k], k))
    groups: list[list[float]] = []
    total = 0.0
    for k in by_e:
        total += cap[k]
        if groups and groups[-1][0] == e[k]:
            groups[-1][1] = total
        else:
            groups.append([e[k], total])
    if total < q_in or not groups:
        return "Open", float(spill), total, q_in - total, spill
    g = next(i for i, (_, c) in enumerate(groups) if not c < q_in)
    below = groups[g - 1][1] if g > 0 else 0.0
    step = groups[g][1] - below
    frac = (q_in - below) / step if step > 0.0 else 0.0
    nxt = groups[g + 1][0] if g + 1 < len(groups) else spill
    e_g = groups[g][0]
    return "Terminal", e_g + frac * (nxt - e_g), q_in, 0.0, groups[g][0]


def _f32(x: float) -> float:
    return float(np.float32(x))


@traced
def resolve_lakes(graph, routed: Routed, own, capacity, mapped=None) -> Accumulated:
    """``lakes.rs``'s discharge with the lake hook, rewriting ``routed.receiver`` inside basins.
    ``own(i, area_m2)`` is a land node's runoff (m³/s), ``capacity(i, e, area_m2)`` its lake
    evaporation capacity (m³/s). Without ``mapped`` it is the plain water balance (data lakes
    off, the golden fixture); with ``mapped`` (``{node: (map id, surface)}``) it is the
    present-day mode: no lake but the map's (``today.rs``)."""
    n = len(graph)
    ocean = routed.ocean
    q = np.zeros(n, dtype=np.float32)
    area = np.zeros(n, dtype=np.float32)
    for i in range(n):
        if not ocean[i]:
            a = graph.area_m2(i)
            q[i] = own(i, a)
            area[i] = a * 1e-6
    present = mapped is not None
    own_q, own_area = (q.copy(), area.copy()) if present else (None, None)
    state = {"evaporated": 0.0}
    lakes: list[Lake] = []
    lake_of: dict[int, int] = {}
    receiver = routed.receiver
    for i in reversed(routed.order):
        b = routed.basin_of[i]
        if b == NO_BASIN:
            k = receiver[i]
            if k != RECEIVER_NONE:
                j = graph.neighbours(i)[k]
                if not ocean[j]:
                    q[j] += q[i]
                    area[j] += area[i]
        elif routed.basins[b].exit == i:
            _resolve(graph, routed, b, q, area, capacity, present, lakes, lake_of, state)
    if present:
        q, area = _today(graph, routed, own_q, own_area, q, capacity, mapped, lakes, lake_of, state)
    return Accumulated(q, area, lakes, lake_of, state["evaporated"])


def _resolve(graph, routed, b, q, area, capacity, present, lakes, lake_of, state) -> None:
    basin = routed.basins[b]
    nodes = basin.nodes
    m = len(nodes)
    local = {v: k for k, v in enumerate(nodes)}
    ev = routed.elevation_m
    e = [ev[v] for v in nodes]
    cap = [capacity(v, ev[v], graph.area_m2(v)) for v in nodes]
    q_in = 0.0
    for v in nodes:
        q_in += float(q[v])
    inside = [[None if j is None else local.get(j, -1) for j in graph.neighbours(v)] for v in nodes]
    exit_k = local[basin.exit]
    kmin = 0
    for k in range(1, m):
        if e[k] < e[kmin]:
            kmin = k
    e_min = e[kmin]
    spill = basin.spill_m
    kind, level, evaporation, outflow, wet_through = _balance(e, cap, spill, q_in)
    lake = None
    if present:
        if kind == "Open":
            spills, sources = True, [False] * m
        else:
            spills, sources = False, [False] * m
            sources[kmin] = True
        sink_evaporation = q_in
    elif kind == "Open":
        footprint = [True] * m
        if spill - e_min >= MIN_LAKE_DEPTH_M:
            lake = (Lake(_f32(level), spill, "Open", q_in, evaporation, outflow, 0.0, basin.exit), footprint)
        spills = True
        sources = list(footprint) if lake else [False] * m
        sink_evaporation = 0.0
    else:
        sinks = [x <= wet_through for x in e]
        playa = level - e_min < MIN_LAKE_DEPTH_M and q_in < RIVER_MIN_DISCHARGE_M3S
        if not playa and any(sinks):
            lake = (Lake(_f32(level), spill, "Terminal", q_in, q_in, 0.0, 0.0, basin.exit), sinks)
        spills, sources, sink_evaporation = False, sinks, q_in
    if spills:
        sources[exit_k] = True
    dist = [-1] * m
    queue = deque(k for k in range(m) if sources[k])
    for k in queue:
        dist[k] = 0
    while queue:
        k = queue.popleft()
        for j in inside[k]:
            if j is not None and j >= 0 and dist[j] < 0:
                dist[j] = dist[k] + 1
                queue.append(j)
    receiver = routed.receiver
    target = [-1] * m
    for k in range(m):
        if sources[k]:
            continue
        best = None
        for slot, j in enumerate(inside[k]):
            if j is None or j < 0:
                continue
            if dist[j] < dist[k] and (best is None or e[j] < e[best[1]]):
                best = (slot, j)
        receiver[nodes[k]] = best[0]
        target[k] = best[1]
    for k in sorted((k for k in range(m) if not sources[k]), key=lambda k: (-dist[k], k)):
        f, t = nodes[k], nodes[target[k]]
        q[t] += q[f]
        area[t] += area[f]
    if spills:
        ex = basin.exit
        for k in range(m):
            if sources[k] and k != exit_k:
                v = nodes[k]
                receiver[v] = RECEIVER_EXIT
                q[ex] += q[v]
                area[ex] += area[v]
        if lake is not None:
            state["evaporated"] += lake[0].evaporation_m3s
            out = np.float32(lake[0].outflow_m3s)
        else:
            out = q[ex]
        k = receiver[ex]
        down = None if k == RECEIVER_NONE else graph.neighbours(ex)[k]
        if down is not None and not routed.ocean[down]:
            q[down] += out
            area[down] += area[ex]
    else:
        state["evaporated"] += sink_evaporation
        for k in range(m):
            if sources[k]:
                receiver[nodes[k]] = RECEIVER_NONE
    if lake is not None:
        l, footprint = lake
        l.footprint = [nodes[k] for k in range(m) if footprint[k]]
        l.area_km2 = sum(graph.area_m2(v) * 1e-6 for v in l.footprint)
        for v in l.footprint:
            lake_of[v] = len(lakes)
        lakes.append(l)


def _today(graph, routed, q, area, walked_q, capacity, mapped, lakes, lake_of, state):
    """``today.rs``: the map's lakes over the walk's receivers, then discharge again."""
    n = len(graph)
    ocean = routed.ocean
    none = -1
    nxt = [none] * n
    for v in range(n):
        if not ocean[v]:
            j = _receiver_of(graph, routed, v)
            nxt[v] = none if j is None else j
    groups: dict[int, tuple[float, list[int]]] = {}
    for v in sorted(mapped):
        if ocean[v] or v in lake_of:
            continue
        lid, surface = mapped[v]
        s, nodes = groups.get(lid, (-math.inf, []))
        nodes.append(v)
        groups[lid] = (max(s, surface), nodes)
    units: dict[int, list] = {}
    for lid in sorted(groups):
        surface, footprint = groups[lid]
        inside = set(footprint)
        outlet = None
        for v in footprint:
            at, meets = nxt[v], False
            while at != none:
                if at in inside:
                    meets = True
                    break
                at = nxt[at]
            if not meets and (outlet is None or walked_q[v] > walked_q[outlet]):
                outlet = v
        cap = 0.0
        for v in footprint:
            if v != outlet:
                nxt[v] = outlet
            cap += capacity(v, routed.elevation_m[v], graph.area_m2(v))
        units[outlet] = [lid, surface, footprint, cap, 0.0, 0.0]
    indeg = [0] * n
    for v in range(n):
        if nxt[v] != none:
            indeg[nxt[v]] += 1
    queue = deque(v for v in range(n) if not ocean[v] and indeg[v] == 0)
    evaporated = 0.0
    while queue:
        v = queue.popleft()
        to = nxt[v]
        unit = units.get(v)
        if unit is not None:
            unit[4] = float(q[v])
            unit[5] = 0.0 if to == none else max(0.0, unit[4] - unit[3])
            evaporated += unit[4] - unit[5]
            passed = np.float32(unit[5])
        else:
            if to == none:
                evaporated += float(q[v])
            passed = q[v]
        if to == none:
            continue
        if not ocean[to]:
            q[to] += passed
            area[to] += area[v]
        indeg[to] -= 1
        if indeg[to] == 0 and not ocean[to]:
            queue.append(to)
    state["evaporated"] = evaporated
    for outlet, (lid, surface, footprint, _, q_in, out) in sorted(units.items(), key=lambda u: u[1][0]):
        lake_area = sum(graph.area_m2(v) * 1e-6 for v in footprint)
        lake = Lake(surface, surface, "Data", q_in, q_in - out, out, lake_area, outlet, footprint)
        for v in footprint:
            lake_of[v] = len(lakes)
        lakes.append(lake)
    return q, area


# The climate (climate.rs), vectorised over nodes.


def _piecewise(points, x: np.ndarray) -> np.ndarray:
    xs = np.array([p[0] for p in points], dtype=np.float64)
    ys = np.array([p[1] for p in points], dtype=np.float64)
    return np.interp(x.astype(np.float64), xs, ys)


def _zonal(table, lat: np.ndarray, elevation: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """``ZonalClimate`` at a coast distance of 0 and no sea-level anomaly: ``(precip, temp)``."""
    a = np.abs(lat)
    snowline = np.maximum(0.0, 5500.0 - 70.0 * np.maximum(a - 20.0, 0.0))
    names = np.select(
        [
            elevation > snowline,
            a < 10.0,
            a < 30.0,
            a < 45.0,
            a < 60.0,
            a < 70.0,
        ],
        [
            np.where(a > 60.0, "IceCap", "Alpine"),
            "TropicalRainforest",
            "MonsoonSavanna",
            "Mediterranean",
            "TemperateBroadleaf",
            "Boreal",
        ],
        "Tundra",
    )
    biomes = table["biome_climate"]
    p = np.array([biomes[str(b)][0] for b in names], dtype=np.float64)
    t = np.array([biomes[str(b)][1] for b in names], dtype=np.float64)
    return p, t


@traced
def node_climate(grid: np.ndarray, table: dict, lat, lon, elevation) -> tuple[np.ndarray, np.ndarray]:
    """``BeyerClimate`` at one age: ``(precip_mm, temp_c)`` as float32 per node, from the baked
    grid ``(180, 360, 2)`` (BIO1, BIO12; row 0 at 89.5 N, column 0 at 179.5 W)."""
    lat = np.asarray(lat, dtype=np.float64)
    lon = np.asarray(lon, dtype=np.float64)
    elevation = np.asarray(elevation, dtype=np.float64)
    y = np.clip(89.5 - lat, 0.0, 179.0)
    r0 = np.minimum(np.floor(y), 178.0)
    fy = y - r0
    x = np.mod(lon + 179.5, 360.0)
    x = np.where((x >= 0.0) & (x < 360.0), x, 0.0)
    c0 = np.floor(x)
    fx = x - c0
    r0 = r0.astype(np.int64)
    c0 = c0.astype(np.int64)
    c1 = (c0 + 1) % 360
    taps = [
        (r0, c0, (1.0 - fy) * (1.0 - fx)),
        (r0, c1, (1.0 - fy) * fx),
        (r0 + 1, c0, fy * (1.0 - fx)),
        (r0 + 1, c1, fy * fx),
    ]
    zp, zt = _zonal(table, lat, elevation)
    p = np.zeros(lat.shape)
    t = np.zeros(lat.shape)
    for r, c, w in taps:
        tc = grid[r, c, 0].astype(np.float32).astype(np.float64)
        pm = grid[r, c, 1].astype(np.float32).astype(np.float64)
        bad = np.isnan(tc) | np.isnan(pm)
        tc = np.where(bad, zt, tc)
        pm = np.where(bad, zp, pm)
        use = w != 0.0
        t += np.where(use, w * tc, 0.0)
        p += np.where(use, w * pm, 0.0)
    return np.maximum(p, 0.0).astype(np.float32), t.astype(np.float32)


@traced
def runoff_mm(table: dict, precip: np.ndarray, temp: np.ndarray) -> np.ndarray:
    """Turc-Pike runoff (float32 mm/yr): ``P - P / (1 + (P/PET)^n)^(1/n)``, ``P`` where PET is
    0, 0 without precipitation."""
    p = np.maximum(precip.astype(np.float64), 0.0)
    pet = _piecewise(table["pet_mm"], temp)
    n = float(np.float32(table["turc_pike_n"]))
    with np.errstate(divide="ignore", invalid="ignore"):
        actual = p / (1.0 + (p / pet) ** n) ** (1.0 / n)
    out = np.where(pet <= 0.0, p, np.maximum(p - actual, 0.0))
    return np.where(p == 0.0, 0.0, out).astype(np.float32)


@traced
def net_evaporation_mm(table: dict, precip: np.ndarray, temp: np.ndarray) -> np.ndarray:
    """A lake surface's net evaporation (float32 mm/yr): ``max(0, E(T) - P)``."""
    lake = _piecewise(table["lake_evaporation_mm"], temp)
    return np.maximum(lake - precip.astype(np.float64), 0.0).astype(np.float32)


# Inputs.


@traced
def read_window(etopo_path, box) -> np.ndarray:
    """ETOPO's 60 arc-second surface cells inside ``box``, as float32 rows from the north, read
    as one window (the file runs south to north on cell-centred axes)."""
    lat_n, lon_w, lat_s, lon_e = box
    with netCDF4.Dataset(Path(etopo_path)) as ds:
        lat = ds.variables["lat"]
        lon = ds.variables["lon"]
        lat0, lon0 = float(lat[0]), float(lon[0])
        i0 = round((lat_s - lat0) * PER_DEG + 0.5)
        i1 = round((lat_n - lat0) * PER_DEG + 0.5)
        j0 = round((lon_w - lon0) * PER_DEG + 0.5)
        j1 = round((lon_e - lon0) * PER_DEG + 0.5)
        if not (0 <= i0 < i1 <= lat.size and 0 <= j0 < j1 <= lon.size):
            raise ValueError(f"box {box} is not inside {Path(etopo_path).name}")
        first = float(lat[i0]), float(lon[j0])
        want = lat_s + 0.5 / PER_DEG, lon_w + 0.5 / PER_DEG
        if max(abs(first[0] - want[0]), abs(first[1] - want[1])) > 1e-6:
            raise ValueError(f"{Path(etopo_path).name}: axes are not 60 arc-second cell centres")
        var = ds.variables["z"]
        var.set_auto_mask(True)
        z = np.ma.filled(var[i0:i1, j0:j1].astype(np.float32), np.nan)
    if not np.isfinite(z).all():
        raise ValueError(f"box {box}: missing elevation samples")
    return np.ascontiguousarray(z[::-1])


def _climate_grid(climate_dir) -> np.ndarray:
    manifest = json.loads((Path(climate_dir) / "manifest.json").read_text())
    entry = next(f for f in manifest["files"] if float(f["age_ka"]) == 0.0)
    _, data = formats.read_tfgrid(Path(climate_dir) / entry["file"])
    return np.asarray(data, dtype=np.float32)


def _strait_levels(graph: LatLonGraph, entry) -> list[tuple[int, float]]:
    """``straits.rs`` ``strait_levels`` on the box: the nodes nearest to points a quarter
    spacing apart along each great-circle step of the polyline, each at the lowest level near
    it (a strait's sill, or a gorge's per-point levels interpolated along the step)."""
    points = entry["points"]
    gorge = entry.get("kind", "strait") == "gorge"
    step = 0.25 * graph.mean_spacing_m() / EARTH_RADIUS_M

    def unit(lat, lon):
        la, lo = math.radians(lat), math.radians(lon)
        return np.array([math.cos(la) * math.cos(lo), math.cos(la) * math.sin(lo), math.sin(la)])

    dirs = [unit(p[0], p[1]) for p in points]
    out: dict[int, float] = {}
    for w, (a, b) in enumerate(zip(dirs[:-1], dirs[1:])):
        ang = math.acos(max(-1.0, min(1.0, float(a @ b))))
        steps = max(1, math.ceil(ang / step))
        for k in range(steps + 1):
            t = k / steps
            d = a + (b - a) * t
            d /= np.linalg.norm(d)
            v = graph.node_at(math.degrees(math.asin(d[2])), math.degrees(math.atan2(d[1], d[0])))
            if v is None:
                continue
            if gorge:
                la, lb = float(np.float32(points[w][2])), float(np.float32(points[w + 1][2]))
                level = float(np.float32(la + t * (lb - la)))
            else:
                level = float(np.float32(entry["sill_m"]))
            out[v] = min(out.get(v, math.inf), level)
    return list(out.items())


# The fixture.


def _simplify(xy: np.ndarray, tol: float) -> list[int]:
    """Ramer-Douglas-Peucker: indices of the points kept (ends always)."""
    keep = {0, len(xy) - 1}
    stack = [(0, len(xy) - 1)]
    while stack:
        a, b = stack.pop()
        if b <= a + 1:
            continue
        seg = xy[b] - xy[a]
        length = math.hypot(seg[0], seg[1])
        rel = xy[a + 1 : b] - xy[a]
        if length == 0.0:
            d = np.hypot(rel[:, 0], rel[:, 1])
        else:
            d = np.abs(seg[0] * rel[:, 1] - seg[1] * rel[:, 0]) / length
        k = int(np.argmax(d))
        if d[k] > tol:
            mid = a + 1 + k
            keep.add(mid)
            stack.append((a, mid))
            stack.append((mid, b))
    return sorted(keep)


def _links(graph, routed, acc, min_area_km2) -> list[dict]:
    n = len(graph)
    q, area = acc.q, acc.area_km2
    ocean = routed.ocean
    river = [
        not ocean[v] and float(q[v]) >= RIVER_MIN_DISCHARGE_M3S and v not in acc.lake_of
        for v in range(n)
    ]
    big = [river[v] and float(area[v]) >= min_area_km2 for v in range(n)]
    down = [None] * n
    ups = [0] * n
    for v in range(n):
        if big[v]:
            d = _receiver_of(graph, routed, v)
            down[v] = d
            if d is not None and big[d]:
                ups[d] += 1
    links = []
    for v in range(n):
        if not big[v] or ups[v] == 1:
            continue
        # The link's own nodes, then the node it ends at: a confluence, or where it leaves the
        # set (the ocean, a lake, a smaller river node), when it drains anywhere.
        chain = [v]
        at = v
        while down[at] is not None and big[down[at]] and ups[down[at]] == 1:
            at = down[at]
            chain.append(at)
        last_river = at
        if down[at] is not None:
            chain.append(down[at])
        if len(chain) < 2:
            continue
        lat0 = math.radians(graph.lat(chain[0]))
        xy = np.array(
            [
                [
                    math.radians(graph.lon(c)) * math.cos(lat0) * EARTH_RADIUS_M,
                    math.radians(graph.lat(c)) * EARTH_RADIUS_M,
                ]
                for c in chain
            ]
        )
        kept = [chain[i] for i in _simplify(xy, SIMPLIFY_M)]
        links.append(
            {
                "lonlat": [[round(graph.lon(c), 5), round(graph.lat(c), 5)] for c in kept],
                "area_km2": [round(float(area[c if big[c] else last_river]), 1) for c in kept],
                "elevation_m": [round(routed.elevation_m[c], 1) for c in kept],
                "discharge_m3s": round(float(q[last_river]), 2),
            }
        )
    return links


def _lakes(graph, routed, acc, precip) -> list[dict]:
    n = len(graph)
    q = acc.q
    river = [
        not routed.ocean[v] and float(q[v]) >= RIVER_MIN_DISCHARGE_M3S and v not in acc.lake_of
        for v in range(n)
    ]
    upstream: dict[int, list[int]] = {}
    for v in range(n):
        if river[v]:
            d = _receiver_of(graph, routed, v)
            if d is not None:
                upstream.setdefault(d, []).append(v)
    out = []
    for lake in acc.lakes:
        if lake.area_km2 < LARGE_LAKE_KM2:
            continue
        seen = set(lake.footprint)
        stack = list(lake.footprint)
        while stack:
            for u in upstream.get(stack.pop(), []):
                if u not in seen and river[u]:
                    seen.add(u)
                    stack.append(u)
        weights = np.array([graph.area_m2(v) for v in seen])
        mean = float(np.sum(weights * precip[list(seen)]) / np.sum(weights))
        out.append(
            {
                "lonlat": [round(graph.lon(lake.exit), 5), round(graph.lat(lake.exit), 5)],
                "area_km2": round(lake.area_km2, 1),
                "basin_precip_mm": round(mean, 1),
            }
        )
    return out


@traced
def reference_from_grid(
    box,
    elevation: np.ndarray,
    climate: np.ndarray,
    table: dict,
    lake_ids=None,
    lake_levels=None,
    straits=(),
    min_area_km2: float = LINK_MIN_AREA_KM2,
) -> dict:
    """The fixture for ``box`` from its 60 arc-second ``elevation`` (rows from the north),
    the 0 ka climate grid, the climate table, the lake map (``lake_ids`` at 0.05 degrees with
    ``lake_levels``; none without) and the straits and gorges (``straits.json``'s entries)."""
    lat_n, lon_w, lat_s, lon_e = box
    rows, cols = elevation.shape
    if (rows, cols) != (round((lat_n - lat_s) * PER_DEG), round((lon_e - lon_w) * PER_DEG)):
        raise ValueError(f"elevation {elevation.shape} does not cover box {box}")
    g = LatLonGraph(lat_n, lon_w, rows, cols, PER_DEG)
    e = elevation.astype(np.float32).ravel().copy()
    e[e == 0.0] = 0.0  # -0.0 and 0.0 are one level here
    for entry in straits:
        for v, level in _strait_levels(g, entry):
            e[v] = min(e[v], np.float32(level))
    lat = np.repeat(lat_n - (np.arange(rows) + 0.5) / PER_DEG, cols)
    lon = np.tile(lon_w + (np.arange(cols) + 0.5) / PER_DEG, rows)
    elev = [float(x) for x in e]
    _log.info("box %s: %d x %d nodes", box, rows, cols)
    ocean = ocean_mask(g, elev, extra_components=True)
    outlets = [g.edge(i) and not ocean[i] for i in range(len(g))]
    routed = route(g, elev, ocean, outlets)
    _log.info("routed: %d basins", len(routed.basins))
    precip, temp = node_climate(climate, table, lat, lon, e)
    run = runoff_mm(table, precip, temp).astype(np.float64)
    net = net_evaporation_mm(table, precip, temp).astype(np.float64)

    def own(i, area_m2):
        return np.float32(run[i] * 1e-3 * area_m2 / SECONDS_PER_YEAR)

    def capacity(i, _e, area_m2):
        return float(net[i] * 1e-3 * area_m2 / SECONDS_PER_YEAR)

    mapped = {}
    if lake_ids is not None:
        height, width = lake_ids.shape
        step = 360.0 / width
        r = np.clip(np.floor((90.0 - lat) / step), 0, height - 1).astype(np.int64)
        c = (np.floor((lon + 180.0) / step).astype(np.int64)) % width
        ids = lake_ids[r, c]
        for v in np.flatnonzero(ids):
            lid = int(ids[v])
            mapped[int(v)] = (lid, float(lake_levels[lid - 1]))
    acc = resolve_lakes(g, routed, own, capacity, mapped=mapped)
    links = _links(g, routed, acc, min_area_km2)
    lakes = _lakes(g, routed, acc, precip.astype(np.float64))
    _log.info("%d links, %d large lakes, %d lakes in all", len(links), len(lakes), len(acc.lakes))
    return {"box": list(box), "links": links, "lakes": lakes}


@traced
def reference(
    box,
    etopo_path,
    climate_dir,
    table_path=None,
    hydrology_dir=None,
    min_area_km2: float = LINK_MIN_AREA_KM2,
) -> dict:
    """The fixture for ``box`` from the raw files: ETOPO's netCDF (only the box is read), the
    baked climate folder, the climate table, and the lake map, lake levels and straits in
    ``hydrology_dir`` (``data/hydrology``)."""
    hydrology_dir = Path(hydrology_dir or HYDROLOGY_DIR)
    table = json.loads(Path(table_path or hydrology_dir / "climate_table.json").read_text())
    lat_n, lon_w, lat_s, lon_e = box
    for level in json.loads((hydrology_dir / "lake_levels.json").read_text())["lakes"]:
        if lat_s <= level["lat"] <= lat_n and lon_w <= level["lon"] <= lon_e:
            raise ValueError(f"{level['name']} is a table lake inside {box}: not mirrored")
    ids, levels = lakemap.decode((hydrology_dir / "lake_map.bin").read_bytes())
    straits = json.loads((hydrology_dir / "straits.json").read_text())["straits"]
    return reference_from_grid(
        box,
        read_window(etopo_path, box),
        _climate_grid(climate_dir),
        table,
        ids,
        levels,
        straits,
        min_area_km2,
    )


@traced
def write_fixture(fixture: dict, out_path) -> int:
    """Writes ``fixture`` as compact JSON; returns its size in bytes."""
    text = json.dumps(fixture, separators=(",", ":"))
    Path(out_path).write_text(text)
    return len(text)


@traced
def main(argv=None) -> int:
    """``timefly-hydro-ref --box indus --etopo ETOPO...60s...nc --climate DIR --out FILE``."""
    import argparse

    from timefly_bake import log

    p = argparse.ArgumentParser(
        prog="timefly-hydro-ref", description="Bake a reality-check reference (rivers Task 11)."
    )
    p.add_argument("--box", choices=sorted(BOXES), required=True)
    p.add_argument("--etopo", type=Path, required=True, help="ETOPO 2022 60s surface netCDF")
    p.add_argument("--climate", type=Path, required=True, help="baked climate folder")
    p.add_argument("--out", type=Path, required=True, help="output fixture (JSON)")
    p.add_argument("--log-level", default="INFO", help="stderr log level")
    args = p.parse_args(argv)
    log.configure(None, args.log_level)
    try:
        fixture = reference(BOXES[args.box], args.etopo, args.climate)
        size = write_fixture(fixture, args.out)
    except Exception as exc:  # already logged with its traceback by @traced
        _log.error("reference pass failed: %s", exc)
        return 1
    _log.info("wrote %s: %d links, %d bytes", args.out, len(fixture["links"]), size)
    return 0


__all__ = [
    "BOXES",
    "GridGraph",
    "LatLonGraph",
    "main",
    "net_evaporation_mm",
    "node_climate",
    "ocean_mask",
    "read_window",
    "receiver_of",
    "reference",
    "reference_from_grid",
    "resolve_lakes",
    "route",
    "runoff_mm",
    "write_fixture",
]
