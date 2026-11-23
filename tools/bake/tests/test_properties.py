"""Property tests for the bake tool (Hypothesis).

Each test states an invariant from the module's docstring and checks it over generated inputs.
The cube and geo properties mirror ``crates/timefly_core/tests/properties.rs``: both sides must
agree on the conventions, so they are held to the same rules.
"""

from __future__ import annotations

import math
import os
import tempfile
from collections import deque
from pathlib import Path
from unittest import mock

import numpy as np
import pytest
from hypothesis import assume, given, settings
from hypothesis import strategies as st
from hypothesis.extra import numpy as hnp

from timefly_bake import ages, cube, formats, geo, log, paleodem, plates

# numpy's first call in a process can be slow; correctness, not speed, is under test here.
settings.register_profile("bake", deadline=None)
settings.load_profile("bake")

finite = st.floats(allow_nan=False, allow_infinity=False)
# A coordinate that is often exactly 0 or +-1, so face edges, corners and axis ties come up.
coord = st.one_of(
    st.floats(-1.0, 1.0, allow_nan=False),
    st.sampled_from([-1.0, 0.0, 1.0]),
)
direction = st.tuples(coord, coord, coord).map(np.array).filter(lambda v: np.linalg.norm(v) > 1e-6)


def unit_quat(axis, angle):
    axis = np.asarray(axis, dtype=np.float64) / np.linalg.norm(axis)
    return np.array([*(axis * math.sin(angle / 2)), math.cos(angle / 2)])


# --------------------------------------------------------------------------------------------
# geo
# --------------------------------------------------------------------------------------------


@given(st.floats(-89.9, 89.9), st.floats(-179.9, 180.0))
def test_latlon_round_trips(lat, lon):
    v = geo.latlon_to_geo(lat, lon)
    assert abs(np.linalg.norm(v) - 1.0) < 1e-12
    lat2, lon2 = geo.geo_to_latlon(v)
    assert abs(lat2 - lat) < 1e-9 and abs(lon2 - lon) < 1e-9


@given(direction)
def test_longitude_is_canonical(d):
    lat, lon = geo.geo_to_latlon(d / np.linalg.norm(d))
    assert -90.0 <= lat <= 90.0
    assert -180.0 < lon <= 180.0


@given(direction, direction, direction, st.floats(-math.pi, math.pi))
def test_frames_are_a_proper_rotation(a, b, axis, angle):
    assert np.array_equal(geo.render_to_geo(geo.geo_to_render(a)), a)
    cross = geo.geo_to_render(np.cross(a, b))
    assert np.allclose(cross, np.cross(geo.geo_to_render(a), geo.geo_to_render(b)), atol=1e-12)
    q = unit_quat(axis, angle)
    v = geo.quat_rotate(q, a)
    assert abs(np.linalg.norm(v) - np.linalg.norm(a)) < 1e-9
    # Rodrigues reference.
    k = np.asarray(axis) / np.linalg.norm(axis)
    ref = a * math.cos(angle) + np.cross(k, a) * math.sin(angle) + k * (k @ a) * (1 - math.cos(angle))
    assert np.allclose(v, ref, atol=1e-9)


# --------------------------------------------------------------------------------------------
# cube
# --------------------------------------------------------------------------------------------


@given(st.integers(1, 64))
def test_texel_centres_round_trip(n):
    dirs = cube.texel_center_dirs(n)
    assert np.allclose(np.linalg.norm(dirs, axis=-1), 1.0)
    face, col, row = cube.dirs_to_texels(dirs, n)
    f, r, c = np.meshgrid(np.arange(6), np.arange(n), np.arange(n), indexing="ij")
    assert np.array_equal(face, f) and np.array_equal(row, r) and np.array_equal(col, c)


@given(direction, st.integers(1, 300))
def test_every_direction_has_a_nearby_texel(d, n):
    u = d / np.linalg.norm(d)
    face, col, row = (int(x) for x in cube.dirs_to_texels(u, n))
    assert 0 <= col < n and 0 <= row < n
    assert abs(d[face // 2]) >= np.abs(d).max() - 1e-12  # the face the direction points at
    centre = cube.texel_center_dirs(n)[face, row, col]
    angle = math.acos(min(1.0, float(centre @ u)))
    # Half a texel diagonal on the face plane; radial projection never lengthens a chord.
    assert angle <= 2 * math.asin(min(1.0, math.sqrt(2) / n / 2)) + 1e-9


# --------------------------------------------------------------------------------------------
# ages
# --------------------------------------------------------------------------------------------


@given(st.floats(-1e6, 1e9))
def test_bp_ages_gain_76_years(years):
    assert ages.myr_from_1950_bp(years) == (years + 76.0) / 1e6
    out = ages.myr_from_1950_bp(np.array([years, years]))
    assert out.shape == (2,) and out[0] == (years + 76.0) / 1e6


# --------------------------------------------------------------------------------------------
# formats
# --------------------------------------------------------------------------------------------

grid_dtype = st.sampled_from([np.float16, np.uint16, np.float32])


@st.composite
def grids(draw):
    kind = draw(st.sampled_from(["cube", "latlon"]))
    dtype = draw(grid_dtype)
    c = draw(st.integers(1, 4))
    if kind == "cube":
        n = draw(st.integers(1, 5))
        shape = (6, n, n, c)
    else:
        shape = (draw(st.integers(1, 6)), draw(st.integers(1, 6)), c)
    data = draw(hnp.arrays(dtype, shape, elements=hnp.from_dtype(np.dtype(dtype), allow_nan=False)))
    return kind, data


@given(grids())
def test_tfgrid_round_trips(case):
    kind, data = case
    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / "g.tfgrid"
        formats.write_tfgrid(path, kind, data)
        header, back = formats.read_tfgrid(path)
    assert header["kind"] == kind and header["channels"] == data.shape[-1]
    assert back.dtype == data.dtype and back.shape == data.shape
    assert back.tobytes() == data.tobytes()


@given(st.integers(1, 3), st.data())
def test_tfplates_rejects_offsets_that_break_a_rule(n, data):
    cells = 6 * n * n
    entries = [(0, 0.0, 1.0), (1, 2.0, 3.0)]
    good = [0] * cells + [2]
    with tempfile.TemporaryDirectory() as d:
        formats.write_tfplates(Path(d) / "ok.tfplates", n, good, entries)
        which = data.draw(st.sampled_from(["length", "start", "end", "decreasing"]))
        bad = list(good)
        if which == "length":
            bad.append(2)
        elif which == "start":
            bad = [1] * cells + [2]
        elif which == "end":
            bad[-1] = 1
        else:
            bad[cells // 2] = 2
        with pytest.raises(ValueError):
            formats.write_tfplates(Path(d) / "bad.tfplates", n, bad, entries)


# --------------------------------------------------------------------------------------------
# plates: interval helpers and footprint dilation
# --------------------------------------------------------------------------------------------

age = st.floats(-100.0, 700.0)


@given(age, age)
def test_clipped_interval(begin, end):
    out = plates._clipped_interval(begin, end)
    younger, older = max(end, 0.0), min(begin, plates.MAX_AGE_MYR)
    if younger > older:
        assert out is None
    else:
        assert out == (younger, older)
        assert 0.0 <= out[0] <= out[1] <= plates.MAX_AGE_MYR


@given(st.lists(st.tuples(st.floats(0, 540), st.floats(0, 100)), max_size=8), st.floats(0, 640))
def test_union_covers_the_same_ages_with_disjoint_intervals(raw, t):
    intervals = [(a, a + span) for a, span in raw]
    out = plates._union(intervals)
    for (a0, b0), (a1, b1) in zip(out, out[1:]):
        assert b0 < a1  # sorted, and touching intervals were merged
    for a, b in out:
        assert a <= b
    inside = any(a <= t <= b for a, b in intervals)
    assert inside == any(a <= t <= b for a, b in out)
    for a, b in intervals:  # endpoints are kept exactly
        assert any(x <= a and b <= y for x, y in out)


def footprints_from(n, owners):
    """Footprints where each owned cell holds one plate with a cell-specific interval."""
    offsets = [0]
    ids, young, old = [], [], []
    for cell in range(6 * n * n):
        if cell in owners:
            ids.append(owners[cell])
            young.append(float(cell % 7))
            old.append(float(cell % 7) + 10.0)
        offsets.append(len(ids))
    return plates.PlateFootprints(
        cube_size=n,
        offsets=np.asarray(offsets, dtype=np.uint32),
        plate_ids=np.asarray(ids, dtype=np.uint32),
        younger_myr=np.asarray(young, dtype=np.float64),
        older_myr=np.asarray(old, dtype=np.float64),
    )


def entries(fp, cell):
    a, b = int(fp.offsets[cell]), int(fp.offsets[cell + 1])
    return [
        (int(p), float(y), float(o))
        for p, y, o in zip(fp.plate_ids[a:b], fp.younger_myr[a:b], fp.older_myr[a:b])
    ]


@st.composite
def dilation_cases(draw):
    n = draw(st.integers(1, 4))
    total = 6 * n * n
    owners = draw(
        st.dictionaries(st.integers(0, total - 1), st.sampled_from([101, 202, 303]), max_size=total)
    )
    return n, owners, draw(st.integers(0, 3))


@given(dilation_cases())
def test_dilation_fills_only_near_cells_from_the_nearest_plate(case):
    n, owners, k = case
    fp = footprints_from(n, owners)
    out = plates.dilate_footprints(fp, k)
    total = 6 * n * n
    neighbours = plates.cube_neighbours(n)

    def bfs(seeds):
        """Breadth-first distance (8-connected, across faces) from the seed cells."""
        dist = {c: 0 for c in seeds}
        queue = deque(seeds)
        while queue:
            c = queue.popleft()
            for m in map(int, neighbours[c]):
                if m not in dist:
                    dist[m] = dist[c] + 1
                    queue.append(m)
        return dist

    dist = bfs(list(owners))
    by_plate = {p: bfs([c for c in owners if owners[c] == p]) for p in set(owners.values())}
    for cell in range(total):
        got = entries(out, cell)
        if cell in owners:
            assert got == entries(fp, cell)  # owned cells never change
        elif cell in dist and dist[cell] <= k:
            assert len(got) == 1, (cell, got)
            plate = got[0][0]
            # The nearest plate wins, ties going to the lowest source ID ...
            nearest = [p for p, d in by_plate.items() if d.get(cell) == dist[cell]]
            assert plate == min(nearest), (cell, plate, nearest)
            # ... with the intervals of one of its own cells at that distance.
            sources = [c for c in owners if owners[c] == plate]
            assert got[0] in [entries(fp, c)[0] for c in sources]
        else:
            assert got == []  # out of reach
    assert np.all(np.diff(out.offsets.astype(np.int64)) >= 0)
    assert int(out.offsets[-1]) == len(out.plate_ids)


@given(dilation_cases())
def test_dilation_grows_monotonically(case):
    n, owners, k = case
    fp = footprints_from(n, owners)
    owned = lambda f: {c for c in range(6 * n * n) if f.offsets[c + 1] > f.offsets[c]}  # noqa: E731
    small, large = plates.dilate_footprints(fp, k), plates.dilate_footprints(fp, k + 1)
    assert owned(fp) <= owned(small) <= owned(large)


# --------------------------------------------------------------------------------------------
# paleodem: lat-lon grids and bilinear sampling
# --------------------------------------------------------------------------------------------


@st.composite
def latlon_grids(draw):
    rows = draw(st.integers(1, 6))
    cols = draw(st.integers(1, 8))
    lat = np.sort(draw(st.lists(st.floats(-90, 90), min_size=rows, max_size=rows, unique=True)))
    lon_step = 360.0 / cols
    lon = -180.0 + lon_step * np.arange(cols)
    values = draw(hnp.arrays(np.float32, (rows, cols), elements=st.floats(-1e4, 1e4, width=32)))
    return paleodem.LatLonGrid(lat, lon, values)


@given(latlon_grids(), st.floats(-120, 120), st.floats(-720, 720))
def test_bilinear_is_a_convex_periodic_blend(grid, lat, lon):
    v = float(paleodem.sample_bilinear(grid, lat, lon))
    lo, hi = float(grid.values.min()), float(grid.values.max())
    assert lo - 1e-3 <= v <= hi + 1e-3
    assert abs(float(paleodem.sample_bilinear(grid, lat, lon + 360.0)) - v) <= 1e-3 * (1 + abs(v))


@given(latlon_grids(), st.data())
def test_bilinear_is_exact_at_grid_nodes(grid, data):
    i = data.draw(st.integers(0, grid.lat.size - 1))
    j = data.draw(st.integers(0, grid.lon.size - 1))
    got = float(paleodem.sample_bilinear(grid, grid.lat[i], grid.lon[j]))
    assert abs(got - float(grid.values[i, j])) <= 1e-3


@given(latlon_grids(), st.integers(0, 7), st.booleans())
def test_normalized_ignores_column_order_and_longitude_convention(grid, shift, east):
    order = np.roll(np.arange(grid.lon.size), shift)
    lon = grid.lon[order]
    if east:
        lon = np.mod(lon, 360.0)  # 0..360 convention
    again = paleodem.LatLonGrid.normalized(grid.lat[::-1], lon, grid.values[::-1][:, order])
    # Latitudes within the seam tolerance of a pole are snapped to it (documented).
    assert np.allclose(again.lat, grid.lat, rtol=0.0, atol=paleodem._SEAM_TOL_DEG)
    assert np.allclose(again.lon, grid.lon, atol=1e-9)
    assert np.array_equal(again.values, grid.values)


# --------------------------------------------------------------------------------------------
# log: redaction
# --------------------------------------------------------------------------------------------

text = st.text(alphabet=st.characters(blacklist_categories=("Cs",)), max_size=40)


@given(text, text)
def test_redaction_removes_the_home_directory(before, after):
    home = "/home/prop-tester"
    assume(home not in before + after and "~" not in home)
    with mock.patch.dict(os.environ, {"HOME": home}):
        out = log.redact(f"{before}{home}/data/raw{after}")
        assert home not in out
        assert out == f"{before}~/data/raw{after}"
        assert log.redact(out) == out  # idempotent
