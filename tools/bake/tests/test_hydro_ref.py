import json
import math
import re
from pathlib import Path

import netCDF4
import numpy as np
import pytest

from timefly_bake import formats, hydro_ref

REPO = Path(__file__).resolve().parents[3]
GOLDEN = REPO / "crates" / "timefly_core" / "tests" / "fixtures" / "hydro_golden.json"
LATTICE_RS = REPO / "crates" / "timefly_core" / "src" / "hydrology" / "lattice.rs"
TABLE = json.loads((REPO / "data" / "hydrology" / "climate_table.json").read_text())


def _golden():
    fx = json.loads(GOLDEN.read_text())
    g = hydro_ref.GridGraph(fx["rows"], fx["cols"], fx["spacing_m"])
    elevation = [float(np.float32(x)) for row in fx["elevation_m"] for x in row]
    return fx, g, elevation


def _cell(g, rc):
    return None if rc is None else rc[0] * g.cols + rc[1]


def test_golden_fixture():
    fx, g, elevation = _golden()
    flat = lambda key: [x for row in fx[key] for x in row]  # noqa: E731
    ocean = hydro_ref.ocean_mask(g, elevation)
    assert ocean == flat("ocean")
    r = hydro_ref.route(g, elevation, ocean)
    assert r.filled_m == flat("filled_m")
    assert r.order == [_cell(g, c) for c in fx["order"]]
    assert [hydro_ref.receiver_of(g, r, i) for i in range(len(g))] == [
        _cell(g, c) for c in flat("receiver")
    ]
    assert [(b.nodes, b.spill_m, b.exit, b.rim) for b in r.basins] == [
        ([_cell(g, c) for c in b["nodes"]], b["spill_m"], _cell(g, b["exit"]), _cell(g, b["rim"]))
        for b in fx["basins"]
    ]
    for case, c in fx["q"].items():
        r = hydro_ref.route(g, elevation, ocean)
        cap = c["capacity_m3s"]
        acc = hydro_ref.resolve_lakes(g, r, lambda i, a: np.float32(1.0), lambda i, e, a: cap)
        want = [x for row in c["q"] for x in row]
        got = [None if ocean[i] else float(acc.q[i]) for i in range(len(g))]
        assert got == want, case
        assert len(acc.lakes) == 1, case
        lake, expect = acc.lakes[0], c["lake"]
        assert lake.kind == expect["kind"], case
        assert abs(lake.level_m - expect["level_m"]) < 1e-6, case
        for key in ("inflow_m3s", "evaporation_m3s", "outflow_m3s"):
            assert getattr(lake, key) == expect[key], (case, key)
        assert (lake.spill_m, lake.exit, lake.area_km2) == (3.0, _cell(g, [1, 2]), 4.0)
        assert lake.footprint == r.basins[0].nodes
        if lake.kind == "Terminal":
            assert all(hydro_ref.receiver_of(g, r, v) is None for v in r.basins[0].nodes)


def test_latlon_neighbour_order_matches_rust():
    text = LATTICE_RS.read_text()
    block = re.search(r"pub const NEIGHBOUR_STEPS: \[\(i32, i32\); 8\] = \[(.*?)\];", text, re.S)
    steps = [tuple(int(v) for v in m) for m in re.findall(r"\((-?\d+),\s*(-?\d+)\)", block.group(1))]
    assert steps == hydro_ref.NEIGHBOUR_STEPS
    g = hydro_ref.LatLonGraph(31.0, 70.0, 60, 60, 60)
    i = 30 * 60 + 30
    for (dc, dr), j in zip(steps, g.neighbours(i)):
        # Row 0 is north: a step of -1 row goes north.
        assert (g.lat(j) - g.lat(i)) * 60 == pytest.approx(-dr)
        assert (g.lon(j) - g.lon(i)) * 60 == pytest.approx(dc)
    assert g.neighbours(0) == [1, None, None, None, None, None, 60, 61]
    # Haversine distances: a minute of latitude is R * pi / 10800 everywhere.
    n_dist = g.distance_k(i, 2)
    assert n_dist == pytest.approx(hydro_ref.EARTH_RADIUS_M * math.pi / 10800, rel=1e-9)
    assert g.distance_k(i, 0) == pytest.approx(n_dist * math.cos(math.radians(g.lat(i))), rel=1e-6)
    assert g.distance_m(i, i + 61) == pytest.approx(g.distance_k(i, 7), rel=1e-9)
    # Areas sum to the box's: R² Δλ (sin φn - sin φs).
    box = hydro_ref.EARTH_RADIUS_M**2 * math.radians(1.0) * (
        math.sin(math.radians(31.0)) - math.sin(math.radians(30.0))
    )
    assert sum(g.area_m2(k) for k in range(len(g))) == pytest.approx(box, rel=1e-12)
    assert g.node_at(30.5, 70.5) == 30 * 60 + 30


def _two_valleys(rows=60, cols=60):
    """A 1 degree box falling south: two valleys from the north-west and north-east corners
    meet at the centre and run south to the edge."""
    y = 1.0 - (np.arange(rows) + 0.5) / rows
    x = (np.arange(cols) + 0.5) / cols
    X, Y = np.meshgrid(x, y)

    def to_segment(ax, ay, bx, by):
        dx, dy = bx - ax, by - ay
        t = np.clip(((X - ax) * dx + (Y - ay) * dy) / (dx * dx + dy * dy), 0.0, 1.0)
        return np.hypot(X - ax - t * dx, Y - ay - t * dy)

    # The confluence and the trunk on node centres, so no two columns tie along the trunk.
    m = 30.5 / cols
    d = np.minimum.reduce(
        [
            to_segment(0.15, 1.0, m, m),
            to_segment(0.85, 1.0, m, m),
            to_segment(m, m, m, 0.0),
        ]
    )
    return (50.0 + 400.0 * Y + 2000.0 * d).astype(np.float32)


def _wet_climate():
    grid = np.full((180, 360, 2), np.nan, dtype=np.float16)
    grid[..., 0] = 10.0
    grid[..., 1] = 3000.0
    return grid


def test_reference_on_a_synthetic_box():
    box = (1.0, 10.0, 0.0, 11.0)
    fixture = hydro_ref.reference_from_grid(
        box, _two_valleys(), _wet_climate().astype(np.float32), TABLE, min_area_km2=800.0
    )
    links = fixture["links"]
    assert len(links) == 3, [l["lonlat"][0] for l in links]
    ends = sorted(tuple(l["lonlat"][-1]) for l in links)
    starts = {tuple(l["lonlat"][0]) for l in links}
    # Two tributaries end at one confluence, where the trunk starts; it leaves by the south edge.
    confluence = ends[1]
    assert ends.count(confluence) == 2
    assert confluence in starts
    assert confluence == (10.50833, 0.50833)
    trunk = next(l for l in links if tuple(l["lonlat"][0]) == confluence)
    assert trunk["lonlat"][-1][1] < 0.02
    tributaries = [l for l in links if l is not trunk]
    assert {round(l["lonlat"][0][0] - 10.5) for l in tributaries} == {0}
    assert sorted(l["lonlat"][0][0] < 10.5 for l in tributaries) == [False, True]
    # Discharge and area grow downstream, and the trunk carries both tributaries.
    assert trunk["discharge_m3s"] > sum(l["discharge_m3s"] for l in tributaries)
    for l in links:
        assert l["area_km2"] == sorted(l["area_km2"])
        assert min(l["area_km2"]) >= 800.0
    assert fixture["lakes"] == []


def _synthetic_etopo(path, box):
    lat_n, lon_w, lat_s, lon_e = box
    with netCDF4.Dataset(path, "w") as ds:
        ds.createDimension("lat", round((lat_n - lat_s) * 60) + 120)
        ds.createDimension("lon", round((lon_e - lon_w) * 60) + 120)
        lat = ds.createVariable("lat", "f8", ("lat",))
        lon = ds.createVariable("lon", "f8", ("lon",))
        lat[:] = lat_s - 1.0 + (np.arange(lat.size) + 0.5) / 60
        lon[:] = lon_w - 1.0 + (np.arange(lon.size) + 0.5) / 60
        z = ds.createVariable("z", "f4", ("lat", "lon"))
        full = np.full((lat.size, lon.size), 9000.0, dtype=np.float32)
        full[60:-60, 60:-60] = _two_valleys()[::-1]
        z[:] = full


def test_reference_json_schema(tmp_path):
    box = (1.0, 10.0, 0.0, 11.0)
    etopo = tmp_path / "etopo.nc"
    _synthetic_etopo(etopo, box)
    window = hydro_ref.read_window(etopo, box)
    np.testing.assert_array_equal(window, _two_valleys())
    climate = tmp_path / "climate"
    climate.mkdir()
    formats.write_tfgrid(climate / "bio_0ka.x.tfgrid", "latlon", _wet_climate())
    (climate / "manifest.json").write_text(
        json.dumps({"files": [{"age_ka": 0.0, "file": "bio_0ka.x.tfgrid"}]})
    )
    fixture = hydro_ref.reference(box, etopo, climate, min_area_km2=800.0)
    out = tmp_path / "ref.json"
    assert hydro_ref.write_fixture(fixture, out) == out.stat().st_size
    data = json.loads(out.read_text())
    assert set(data) == {"box", "links", "lakes"}
    assert data["box"] == list(box)
    for link in data["links"]:
        assert set(link) == {"lonlat", "area_km2", "elevation_m", "discharge_m3s"}
        n = len(link["lonlat"])
        assert n >= 2 and len(link["area_km2"]) == n and len(link["elevation_m"]) == n
        for lon, lat in link["lonlat"]:
            assert 10.0 <= lon <= 11.0 and 0.0 <= lat <= 1.0
        assert min(link["area_km2"]) >= 800.0
        assert link["discharge_m3s"] >= hydro_ref.RIVER_MIN_DISCHARGE_M3S
        assert all(math.isfinite(v) for v in link["area_km2"] + link["elevation_m"])
    assert len(data["links"]) == 3
    assert data["lakes"] == []
    with pytest.raises(ValueError, match="not inside"):
        hydro_ref.read_window(etopo, (5.0, 10.0, 4.0, 11.0))


def _bowl():
    """Ocean in column 0, a 300 m ridge in column 1, and a bowl beyond it falling to 100 m at
    (3, 5): a closed basin spilling at 300 m over the ridge."""
    g = hydro_ref.GridGraph(7, 9, 1000.0)
    e = []
    for r in range(7):
        for c in range(9):
            e.append(-10.0 if c == 0 else 300.0 if c == 1 else 100.0 + 20.0 * ((r - 3) ** 2 + (c - 5) ** 2))
    return g, e


def test_present_mode_takes_its_lakes_from_the_map():
    g, e = _bowl()
    ocean = hydro_ref.ocean_mask(g, e)
    sink = 3 * 9 + 5
    one = lambda i, a: np.float32(1.0)  # noqa: E731
    arid = lambda i, e, a: 10.0  # noqa: E731
    # The plain balance: an arid closed basin holds a terminal lake.
    r = hydro_ref.route(g, e, ocean)
    assert len(r.basins) == 1 and r.basins[0].spill_m == 300.0
    plain = hydro_ref.resolve_lakes(g, r, one, arid)
    assert [l.kind for l in plain.lakes] == ["Terminal"]
    # Present mode without a mapped lake: none, and the basin ends at its lowest node.
    r = hydro_ref.route(g, e, ocean)
    dry = hydro_ref.resolve_lakes(g, r, one, arid, mapped={})
    assert dry.lakes == []
    assert hydro_ref.receiver_of(g, r, sink) is None
    inflow = float(dry.q[sink])
    assert inflow == len(r.basins[0].nodes) + 0.0 or inflow >= len(r.basins[0].nodes)
    # With the map: its lake at its highest surface over its own nodes, the sink its outlet.
    mapped = {sink - 1: (1, 150.0), sink: (1, 150.0), sink + 1: (1, 140.0)}
    r = hydro_ref.route(g, e, ocean)
    wet = hydro_ref.resolve_lakes(g, r, one, arid, mapped=mapped)
    (lake,) = wet.lakes
    assert (lake.kind, lake.level_m, lake.exit) == ("Data", 150.0, sink)
    assert lake.footprint == [sink - 1, sink, sink + 1]
    assert lake.inflow_m3s == inflow and lake.outflow_m3s == 0.0
    assert lake.evaporation_m3s == inflow
    assert lake.area_km2 == pytest.approx(3.0)


def _deep_bowl():
    """lakes.rs's deep_bowl: a 9 x 11 grid at 10 km, ocean in column 0, a 50 m rim at (4, 1), a
    cone 5 + 4 r m about (4, 6) inside rows 1..7 and columns 2..9, 90 m around."""
    g = hydro_ref.GridGraph(9, 11, 10_000.0)
    e = []
    for r in range(9):
        for c in range(11):
            if c == 0:
                e.append(-10.0)
            elif (r, c) == (4, 1):
                e.append(50.0)
            elif 1 <= r <= 7 and 2 <= c <= 9:
                e.append(float(np.float32(5.0 + 4.0 * np.float32(math.hypot(r - 4.0, c - 6.0)))))
            else:
                e.append(90.0)
    return g, e


def test_an_endorheic_lake_seeds_routing_and_is_terminal():
    # Task 11d fix round 2, mirroring lakes.rs's
    # an_endorheic_table_lake_is_terminal_and_its_land_drains_by_descent.
    g, e = _deep_bowl()
    ocean = hydro_ref.ocean_mask(g, e)
    centre, slope, rim = 4 * 11 + 6, 4 * 11 + 3, 4 * 11 + 1
    own = lambda i, a: np.float32(5.0)  # noqa: E731
    humid = lambda i, x, a: 0.1  # noqa: E731
    # Without the flag the slope lies in the bowl's basin, filled to its 50 m spill.
    r = hydro_ref.route(g, e, ocean)
    assert r.basin_of[slope] != hydro_ref.NO_BASIN
    # Flagged: the nodes below 15 m seed the flood at 15 m and are one basin, the slope drains
    # by descent, and the lake is terminal with its surplus reported.
    r = hydro_ref.route(g, e, ocean, sinks=[(centre, 15.0)])
    b = r.basin_of[centre]
    assert r.basins[b].spill_m == 15.0 and r.basins[b].rim == r.basins[b].exit
    assert r.basin_of[slope] == hydro_ref.NO_BASIN
    down = hydro_ref.receiver_of(g, r, slope)
    assert e[down] < e[slope]
    acc = hydro_ref.resolve_lakes(g, r, own, humid, endorheic={b: 15.0})
    (lake,) = acc.lakes
    assert (lake.kind, lake.level_m, lake.outflow_m3s) == ("Terminal", 15.0, 0.0)
    assert lake.evaporation_m3s == lake.inflow_m3s
    # The Rust pass's numbers: 405 in, 21 wet nodes at 0.1 each.
    assert lake.inflow_m3s == pytest.approx(405.0)
    assert lake.surplus_m3s == pytest.approx(405.0 - 2.1)
    assert float(acc.q[rim]) == 5.0


def test_a_bed_fragment_joins_its_table_lake():
    # Task 11f, mirroring lakes.rs's a_bed_fragment_joins_an_endorheic_lake_which_stays_terminal:
    # a mapped lake baked at the bed (3 m) beside the endorheic lake's footprint joins it.
    g, e = _deep_bowl()
    ocean = hydro_ref.ocean_mask(g, e)
    centre, slope = 4 * 11 + 6, 4 * 11 + 3
    own = lambda i, a: np.float32(5.0)  # noqa: E731
    humid = lambda i, x, a: 0.1  # noqa: E731

    def run(mapped):
        r = hydro_ref.route(g, e, ocean, sinks=[(centre, 15.0)])
        b = r.basin_of[centre]
        return r, hydro_ref.resolve_lakes(g, r, own, humid, mapped=mapped, endorheic={b: 15.0})

    _, plain = run({})
    (before,) = plain.lakes
    r, acc = run({slope: (1, 3.0)})
    # The hazard: the slope is off the lake's basin and footprint, its surface far below.
    assert r.basin_of[slope] == hydro_ref.NO_BASIN and slope not in before.footprint
    (lake,) = acc.lakes
    assert (lake.kind, lake.level_m, lake.outflow_m3s) == ("Terminal", 15.0, 0.0)
    assert lake.exit == before.exit
    assert lake.footprint == sorted(before.footprint + [slope])
    assert acc.lake_of[slope] == 0
    # The same water, one node more to evaporate it: 405 in, 22 wet nodes at 0.1 each.
    assert lake.inflow_m3s == pytest.approx(before.inflow_m3s) == pytest.approx(405.0)
    assert lake.evaporation_m3s == lake.inflow_m3s
    assert lake.surplus_m3s == pytest.approx(405.0 - 2.2)
    assert lake.area_km2 == pytest.approx(before.area_km2 + 100.0)
    # A mapped lake above the level stays its own.
    _, apart = run({slope: (1, 30.0)})
    assert [l.kind for l in apart.lakes] == ["Terminal", "Data"]


def test_a_gorge_lets_a_river_through_a_ridge():
    # A 1 degree box falling east to its edge, with a 500 m ridge across it at 10.5 E: without
    # the gorge, the west drains west; with it, through the gorge to the east.
    rows = cols = 60
    x = (np.arange(cols) + 0.5) / cols
    land = np.tile(300.0 - 200.0 * x, (rows, 1))
    land[:, 29:31] = 500.0
    land[0, :] = land[-1, :] = 1000.0  # walls north and south
    box = (1.0, 10.0, 0.0, 11.0)
    gorge = {
        "name": "g",
        "kind": "gorge",
        "points": [[0.5083, 10.45, 190.0], [0.5083, 10.55, 185.0]],
        "sill_m": None,
    }
    g = hydro_ref.LatLonGraph(*box[:2], rows, cols, 60)
    levels = dict(hydro_ref._strait_levels(g, gorge))
    assert set(levels) >= {29 * cols + 29, 29 * cols + 30}
    assert all(185.0 <= v <= 190.0 for v in levels.values())

    def east_share(straits):
        fx = hydro_ref.reference_from_grid(
            box, land.astype(np.float32), _wet_climate().astype(np.float32), TABLE,
            straits=straits, min_area_km2=500.0,
        )
        ends = [l["lonlat"][-1][0] for l in fx["links"]]
        return sum(lon > 10.9 for lon in ends), len(ends)

    east, _ = east_share([])
    assert east == 0
    east, total = east_share([gorge])
    assert east >= 1
