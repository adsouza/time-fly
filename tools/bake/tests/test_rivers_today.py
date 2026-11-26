import json
import math
from pathlib import Path

import numpy as np

from timefly_bake import rivers_today
from timefly_bake.rivers_today import CellGrid, Polyline

SHIPPED = Path(__file__).resolve().parents[3] / "data" / "hydrology" / "rivers_today.json"


def _feature(featurecla, scalerank, parts, name="r"):
    """A GeoJSON MultiLineString feature; ``parts`` are lists of (lon, lat)."""
    return {
        "type": "Feature",
        "properties": {"featurecla": featurecla, "scalerank": float(scalerank), "name": name},
        "geometry": {"type": "MultiLineString", "coordinates": [list(map(list, p)) for p in parts]},
    }


def test_select_keeps_ranked_rivers_only():
    features = [
        _feature("River", 3, [[(10, 1), (11, 2)]], "kept"),
        _feature("Lake Centerline", 3, [[(10, 1), (11, 2)]], "lake"),
        _feature("River", 8, [[(10, 1), (11, 2)]], "minor"),
        _feature("River", 7, [[(20, 5), (21, 6), (22, 7)], [(30, -5), (31, -6)]], "two"),
    ]
    got = rivers_today.select(features)
    assert [(p.name, p.scalerank) for p in got] == [("kept", 3), ("two", 7), ("two", 7)]
    # One polyline per part, as (lat, lon).
    assert got[1].points.tolist() == [[5, 20], [6, 21], [7, 22]]
    assert got[2].points.tolist() == [[-5, 30], [-6, 31]]
    assert rivers_today.TODAY_RIVER_MAX_SCALERANK == 7


def test_select_drops_canals():
    # Natural Earth files canals as River: the Suez Canal (rank 6), the Ismailiya and Gharraf
    # canals, and the Ferenc Csatorna (Hungarian for the Franz Canal, the Danube-Tisa-Danube
    # Canal). The Hindiyah Channel is a natural branch of the Euphrates and stays.
    features = [
        _feature("River", 6, [[(32.3, 31.2), (32.5, 30.0)]], "Suez Canal"),
        _feature("River", 6, [[(31.2, 30.1), (32.2, 30.6)]], "Ismailiya Canal"),
        _feature("River", 4, [[(45.8, 32.5), (46.2, 31.0)]], "Gharraf Canal"),
        _feature("River", 7, [[(19.0, 45.8), (20.0, 45.6)]], "Ferenc Csatorna"),
        _feature("River", 7, [[(44.3, 32.7), (44.6, 32.0)]], "Hindiyah Channel"),
        _feature("River", 1, [[(19.0, 47.5), (22.5, 44.7)]], "Danube"),
    ]
    got = rivers_today.select(features)
    assert [p.name for p in got] == ["Hindiyah Channel", "Danube"]
    # The committed table holds none of them.
    if SHIPPED.exists():
        names = {r["name"] for r in json.loads(SHIPPED.read_bytes())["rivers"]}
        assert not {n for n in names if "canal" in n.lower() or "csatorna" in n.lower()}


def test_select_keeps_lake_centrelines_no_mapped_lake_covers():
    # Owner, 2026-11-26 (Task 11d fix round 1): burn the reservoirs. A lake centreline is kept
    # unless a mapped lake covers it; without a cover test none is kept (as before).
    features = [
        _feature("Lake Centerline", 4, [[(82.0, 54.0), (83.0, 54.8)]], "Ob"),
        _feature("Lake Centerline", 1, [[(32.2, 1.7), (31.8, -0.9)]], "Victoria Nile"),
        _feature("Lake Centerline", 8, [[(10, 1), (11, 2)]], "minor"),
        _feature("Lake Centerline", 6, [[(32.3, 31.2), (32.5, 30.0)]], "Suez Canal"),
        _feature("River", 3, [[(10, 1), (11, 2)]], "kept"),
    ]
    assert [p.name for p in rivers_today.select(features)] == ["kept"]
    cover = {"Ob": 0.0, "Victoria Nile": 0.9, "minor": 0.0, "Suez Canal": 0.0}
    got = rivers_today.select(features, cover=lambda name, points: cover[name])
    assert [p.name for p in got] == ["Ob", "kept"]
    assert got[0].points.tolist() == [[54.0, 82.0], [54.8, 83.0]]


def test_lake_cover_is_the_share_of_length_in_mapped_lakes():
    # A 0.05-degree map with one lake over 10..10.5 N, 20..20.5 E.
    ids = np.zeros((3600, 7200), dtype=np.uint16)
    ids[1590:1600, 4000:4010] = 7
    inside = np.array([[10.1, 20.0], [10.1, 20.5]])
    half = np.array([[10.1, 20.0], [10.1, 21.0]])
    outside = np.array([[12.0, 20.0], [12.0, 21.0]])
    assert rivers_today.lake_cover(inside, ids) == 1.0
    assert abs(rivers_today.lake_cover(half, ids) - 0.5) < 0.05
    assert rivers_today.lake_cover(outside, ids) == 0.0
    assert rivers_today.LAKE_CENTRELINE_MAX_COVER == 0.5


EXTRA = SHIPPED.parent / "rivers_extra.json"


def _km(a, b):
    la1, lo1, la2, lo2 = map(math.radians, (*a, *b))
    c = math.sin(la1) * math.sin(la2) + math.cos(la1) * math.cos(la2) * math.cos(lo1 - lo2)
    return 6371.0 * math.acos(min(1.0, c))


def test_the_hand_added_channels_are_cited_and_reach_their_mouths():
    # Owner, 2026-11-26 (Task 11d fix round 1): "Hand-add delta channels".
    text = EXTRA.read_text(encoding="utf-8")
    extras = rivers_today.read_extras(text)
    names = [p.name for p in extras]
    assert names == ["Yangtze estuary", "Hau (Bassac)", "Tien", "Rio Grande (Orinoco)"]
    table = json.loads(text)["rivers"]
    for entry, line in zip(table, extras):
        assert "read 2026-11-26" in entry["source"], entry["name"]
        assert _km(line.points[-1], entry["mouth"]) < 2.0, entry["name"]
        assert 0 <= line.scalerank <= 7
    # Three continue a Natural Earth course from its last point.
    shipped = json.loads(SHIPPED.read_bytes())["rivers"]
    ends = {tuple(r["points"][-1]) for r in shipped}
    for line in extras[:3]:
        assert tuple(line.points[0]) in ends, line.name
    # The committed table carries each, unchanged.
    for line in extras:
        assert any(
            r["name"] == line.name and r["points"] == line.points.tolist() for r in shipped
        ), line.name


def test_read_extras_refuses_an_uncited_entry():
    bad = {"rivers": [{"name": "x", "scalerank": 1, "mouth": [0, 0], "points": [[0, 0], [1, 1]]}]}
    try:
        rivers_today.read_extras(json.dumps(bad))
    except ValueError as e:
        assert "source" in str(e)
    else:
        raise AssertionError("an entry without a source was read")


def test_lake_parts_are_oriented_by_their_connections_first():
    # Task 11d fix round 2: a reservoir's ends are flat water, so ETOPO cannot tell its
    # direction; the oriented river courses it joins can. (Fix round 3: one rule set for every
    # part, rivers given here as already decided.)
    line = lambda name, *pts: Polyline(name, 3, np.array(pts, float))  # noqa: E731
    rivers = [
        line("above", (0, 0), (1, 0)),
        line("below", (5, 0), (6, 0)),
        line("beyond", (7, 0), (8, 0)),
    ]
    lakes = [
        # Its first end touches the river above's last point: it already runs downstream.
        line("first", (1, 0), (2, 0)),
        # Raw from the river below's first point back up: reversed.
        line("chained", (5, 0), (3, 0)),
        # Touches only the chain's other parts once they are oriented: (2, 0) is "first"'s end.
        line("between", (3, 0), (2, 0)),
        # Both ends at a course's first point, so each says the other way: ambiguous, ETOPO's.
        line("both", (0, 0), (7, 0)),
        # Touches nothing: ETOPO's.
        line("alone", (9, 9), (8, 8)),
    ]
    # ETOPO's heights of each part's (first, last) end: both "both" and "alone" reverse.
    heights = {"first": (0, 9), "chained": (9, 0), "between": (9, 0), "both": (0, 1), "alone": (0, 1)}
    got, counts = rivers_today.orient_by_connections(
        lakes, lambda p: heights[p.name], fixed=rivers
    )
    ends = {p.name: (tuple(p.points[0]), tuple(p.points[-1])) for p in got}
    assert ends["first"] == ((1, 0), (2, 0))
    assert ends["chained"] == ((3, 0), (5, 0))
    assert ends["between"] == ((2, 0), (3, 0))
    assert ends["both"] == ((7, 0), (0, 0))
    assert ends["alone"] == ((8, 8), (9, 9))
    assert counts == {"topology": 3, "etopo": 1, "ambiguous": 1}
    assert [p.name for p in got] == [p.name for p in lakes]


def test_ends_within_the_simplification_reach_are_one_junction():
    # Task 11d fix round 3: Guiping, where the Quan (carrying the Hongshui's clear fall) and the Yu
    # meet and the Xun leaves. Natural Earth ends the Xun 350 m from the point the other two
    # share. Read as two junctions, the Quan and the Yu looked like a chain, and the Hongshui's
    # 340 m carried the Yu backwards, up past Nanning.
    line = lambda name, *pts: Polyline(name, 3, np.array(pts, float))  # noqa: E731
    parts = [
        line("hongshui", (0, 0), (1, 0)),
        line("quan", (1, 0), (2, 0)),
        line("yu", (2, -1), (2, 0)),
        line("xun", (3, 0), (2.003, 0)),  # drawn from its mouth, ending 330 m from the others
    ]
    heights = {"hongshui": (400, 60), "quan": (60, 34), "yu": (64, 34), "xun": (22, 34)}
    got, _ = rivers_today.orient_by_connections(parts, lambda p: heights[p.name])
    ends = {p.name: (tuple(p.points[0]), tuple(p.points[-1])) for p in got}
    assert ends["yu"] == ((2, -1), (2, 0))
    assert ends["xun"] == ((2.003, 0), (3, 0))


def test_an_inferred_role_never_overrules_a_clear_fall():
    # Task 11d fix round 3: the Danube's delta. Natural Earth ends the Danube 12 km short of the
    # fork where the Sfantu Gheorghe and Sulina arms start and the Chilia arm ends, drawn from
    # the sea. With two outflows and no inflow known, the Chilia looks like the fork's inflow;
    # that role is only inferred, so its own 2.5 m fall to the sea decides.
    line = lambda name, *pts: Polyline(name, 3, np.array(pts, float))  # noqa: E731
    parts = [
        line("danube", (0, 0), (0.9, 0)),
        line("gheorghe", (1, 0), (2, -1)),
        line("sulina", (1.0002, 0.0002), (2, 1)),
        line("chilia", (2, 0.5), (1, 0)),
    ]
    heights = {
        "danube": (26, 1.2),
        "gheorghe": (0.83, -8.69),
        "sulina": (0.83, -9.31),
        "chilia": (-1.62, 0.83),
    }
    got, _ = rivers_today.orient_by_connections(parts, lambda p: heights[p.name])
    ends = {p.name: (tuple(p.points[0]), tuple(p.points[-1])) for p in got}
    assert ends["chilia"] == ((1, 0), (2, 0.5))


def test_a_fork_votes_only_once_all_its_parts_are_known():
    # Task 11d fix round 3: the Mekong at Phnom Penh. The stem reaches the fork through a short
    # link part; the Bassac arm's clear fall is decided first. Had the fork voted then, with the
    # link still unknown, it would have seen an outflow and no inflow and turned the Tien arm
    # into a tributary flowing in, overruling the Tien's own weaker fall.
    line = lambda name, *pts: Polyline(name, 3, np.array(pts, float))  # noqa: E731
    parts = [
        line("stem", (0, 0), (0.9, 0)),
        line("link", (0.95, 0), (1, 0)),
        line("bassac", (1, 0), (2, 1)),
        line("tien", (1, 0), (2, -1)),
    ]
    heights = {"stem": (90, 2), "link": (2, 2), "bassac": (2, -4), "tien": (2, -0.3)}
    got, _ = rivers_today.orient_by_connections(parts, lambda p: heights[p.name])
    ends = {p.name: (tuple(p.points[0]), tuple(p.points[-1])) for p in got}
    assert ends["tien"] == ((1, 0), (2, -1))
    assert ends["bassac"] == ((1, 0), (2, 1))


def test_river_parts_are_oriented_by_their_connections():
    # Task 11d fix round 3: Natural Earth river parts by how they join, ETOPO only where nothing
    # decides. A main stem in two parts, the second drawn backwards across flat ground (ETOPO
    # would keep it reversed), and a tributary drawn from its mouth, which lies on the second
    # part's interior vertex.
    line = lambda name, *pts: Polyline(name, 3, np.array(pts, float))  # noqa: E731
    parts = [
        line("upper", (0, 0), (1, 0)),
        line("lower", (2, 0), (1.5, 0), (1, 0)),
        line("tributary", (1.5, 0), (1.5, 1)),
    ]
    # The tributary's own ends are nearly flat and read the wrong way.
    heights = {"upper": (100, 50), "lower": (0.1, 0.2), "tributary": (40.5, 40.0)}
    got, counts = rivers_today.orient_by_connections(parts, lambda p: heights[p.name])
    ends = {p.name: (tuple(p.points[0]), tuple(p.points[-1])) for p in got}
    # The tributary's mouth is on the lower part's interior: it flows there (the mouth's vote,
    # 1 m sure, beats its own 0.5 m of contrary fall).
    assert ends["tributary"] == ((1.5, 1), (1.5, 0))
    # ETOPO's clearest part first (the upper, 50 m of fall), then the lower part continues it
    # at (1, 0), where every other part is decided and ends: it flows on from there.
    assert ends["upper"] == ((0, 0), (1, 0))
    assert ends["lower"] == ((1, 0), (2, 0))
    assert counts == {"topology": 2, "etopo": 1, "ambiguous": 0}


def test_a_clear_fall_is_never_overruled_by_a_weaker_join():
    # Task 11d fix round 3's first cut decided by joins alone, and a misread fork turned whole
    # chains uphill. The surest evidence decides: a part falling 300 m keeps ETOPO's direction,
    # though a join from a flat neighbour says the opposite.
    line = lambda name, *pts: Polyline(name, 3, np.array(pts, float))  # noqa: E731
    parts = [
        line("steep", (0, 0), (1, 0)),
        # A flat part (0.2 m) whose start, by ETOPO, is the steep part's end...
        line("flat", (1, 0), (2, 0)),
        # ... and a part drawn from the steep part's interior: a distributary, not a mouth.
        line("arm", (0.5, 0), (0.5, 1)),
    ]
    steep = Polyline("steep", 3, np.array([(0, 0), (0.5, 0), (1, 0)], float))
    parts[0] = steep
    heights = {"steep": (400, 100), "flat": (5.2, 5.0), "arm": (300.0, 299.9)}
    got, counts = rivers_today.orient_by_connections(parts, lambda p: heights[p.name])
    ends = {p.name: (tuple(p.points[0]), tuple(p.points[-1])) for p in got}
    assert ends["steep"] == ((0, 0), (1, 0))
    assert ends["flat"] == ((1, 0), (2, 0))
    # The arm's own fall is 0.1 m, less sure than its "mouth" vote: the vote decides it.
    assert ends["arm"] == ((0.5, 1), (0.5, 0))
    assert counts["etopo"] == 1


def test_an_end_reads_a_mapped_lake_as_its_surface():
    # Task 11d fix round 3: the Angara's start reads Baikal's bed (about -1,000 m in ETOPO), so
    # ETOPO said it flowed into Baikal. An end's lowest nearby cell reads a mapped lake's cells as
    # the lake's surface (the level table's, for the deep lakes whose bed ETOPO holds).
    rows, cols = 120, 120
    z = np.full((rows, cols), 500.0)
    z[60:, :] = -1000.0  # the lake's bed, north of 11 N (rows from the south)
    grid = CellGrid(z, lat_s=10.0, lon_w=20.0, per_deg=60)
    ids = np.zeros((3600, 7200), dtype=np.uint16)
    ids[1560:1580, 4000:4040] = 3  # 11..12 N, 20..22 E on the 0.05-degree map (rows from the north)
    surfaces = {3: 455.5}
    # Just south of the shore: its window holds lake cells, read as 455.5 m, not -1000 m.
    assert rivers_today.end_height(grid, 10.99, 21.0, ids, surfaces) == 455.5
    assert grid.lowest_near(10.99, 21.0) == -1000.0
    # Away from the lake: ETOPO's lowest cell.
    assert rivers_today.end_height(grid, 10.5, 21.0, ids, surfaces) == 500.0


def test_orient_puts_the_higher_end_first():
    # A 1' grid over 10..12 N, 20..22 E (rows from the south), rising to the north and east.
    rows, cols = 120, 120
    lat = 10.0 + (np.arange(rows) + 0.5) / 60.0
    lon = 20.0 + (np.arange(cols) + 0.5) / 60.0
    z = (100.0 * (lat[:, None] - 10.0) + 10.0 * (lon[None, :] - 20.0)).astype(np.float32)
    grid = CellGrid(z, lat_s=10.0, lon_w=20.0, per_deg=60)
    # The lowest cell within 2' of (11, 21) is 2 cells south and west of the point's cell.
    i, j = 60, 60
    assert grid.lowest_near(11.0 + 0.1 / 60, 21.0 + 0.1 / 60) == z[i - 2, j - 2]
    down = Polyline("d", 3, np.array([[10.5, 20.5], [11.0, 21.0], [11.5, 21.5]]))
    up = rivers_today.orient(down, grid)
    assert up.points.tolist() == down.points[::-1].tolist()
    assert rivers_today.orient(up, grid).points.tolist() == up.points.tolist()
    # A tie keeps Natural Earth's order.
    flat = CellGrid(np.zeros((rows, cols), np.float32), lat_s=10.0, lon_w=20.0, per_deg=60)
    assert rivers_today.orient(down, flat).points.tolist() == down.points.tolist()


def _unit(q):
    la, lo = math.radians(q[0]), math.radians(q[1])
    return np.array([math.cos(la) * math.cos(lo), math.cos(la) * math.sin(lo), math.sin(la)])


def _distance_to_polyline_m(p, line):
    """The distance (m) from ``p`` to ``line`` (both lat, lon), in the gnomonic projection
    centred on ``p``: great circles are straight lines there and a point's distance from the
    centre is the tangent of its angle, so the nearest point of a projected segment is exact.
    Independent of the module's cross-product code."""
    c = _unit(p)
    east = np.cross([0.0, 0.0, 1.0], c)
    east /= np.linalg.norm(east)
    north = np.cross(c, east)

    def plane(q):
        u = _unit(q)
        u = u / (u @ c)
        return np.array([u @ east, u @ north])

    best = math.inf
    for a, b in zip(line[:-1], line[1:]):
        pa, pb = plane(a), plane(b)
        seg = pb - pa
        t = 0.0 if not seg.any() else float(np.clip(-pa @ seg / (seg @ seg), 0.0, 1.0))
        best = min(best, float(np.linalg.norm(pa + t * seg)))
    return math.atan(best) * rivers_today.EARTH_RADIUS_M


def test_simplify_stays_within_a_kilometre():
    rng = np.random.default_rng(7)
    # A meandering river 2,000 km long at 30-50 N, a vertex every 2 km or so.
    n = 1000
    t = np.linspace(0.0, 1.0, n)
    lat = 30.0 + 20.0 * t + 0.05 * np.sin(40 * t) + rng.normal(0, 0.004, n)
    lon = 60.0 + 3.0 * np.sin(6 * t) + rng.normal(0, 0.004, n)
    line = Polyline("m", 3, np.stack([lat, lon], axis=1))
    simple = rivers_today.simplify(line, 1000.0)
    kept = simple.points
    assert 10 < len(kept) < n / 2, len(kept)
    assert kept[0].tolist() == line.points[0].tolist()
    assert kept[-1].tolist() == line.points[-1].tolist()
    for p in line.points:
        assert _distance_to_polyline_m(p, kept) <= 1000.0 + 1.0
    # A wider tolerance keeps fewer points; a closed loop keeps its shape.
    assert len(rivers_today.simplify(line, 5000.0).points) < len(kept)
    loop = Polyline("o", 3, np.array([[0, 0], [0, 0.1], [0.1, 0.1], [0.1, 0], [0, 0]], float))
    assert len(rivers_today.simplify(loop, 1000.0).points) == 5


def test_table_schema_and_size():
    lines = [
        Polyline("A", 3, np.array([[1.00004, 2.0], [1.0001, 2.00004], [1.5, 2.5]])),
        Polyline("", 7, np.array([[-0.00001, 3.0], [0.0, 3.0004]])),
    ]
    text = rivers_today.table_text(lines)
    table = json.loads(text)
    assert set(table) == {"comment", "source", "rivers"}
    assert "Natural Earth" in table["source"] and "5.1.2" in table["source"]
    assert table["rivers"][0] == {
        "name": "A",
        "scalerank": 3,
        # Rounded to 0.001 degrees; a repeat after rounding is dropped.
        "points": [[1.0, 2.0], [1.5, 2.5]],
    }
    # A polyline that rounds to one point is left out; -0.0 is written as 0.0.
    assert len(table["rivers"]) == 1
    assert "-0.0" not in rivers_today.table_text(
        [Polyline("z", 3, np.array([[-0.0001, 1.0], [-0.0001, 2.0]]))]
    )
    # The committed table: the schema, upstream first, at most 1 MB.
    if SHIPPED.exists():
        raw = SHIPPED.read_bytes()
        assert len(raw) <= rivers_today.MAX_TABLE_BYTES
        shipped = json.loads(raw)
        assert set(shipped) == {"comment", "source", "rivers"}
        assert len(shipped["rivers"]) >= 1000
        for r in shipped["rivers"]:
            assert set(r) == {"name", "scalerank", "points"}
            assert isinstance(r["name"], str) and 0 <= r["scalerank"] <= 7
            assert len(r["points"]) >= 2
            assert all(-90 <= la <= 90 and -180 <= lo <= 180 for la, lo in r["points"])
