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
