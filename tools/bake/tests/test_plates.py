"""Plate footprints and rotation export against synthetic pygplates inputs."""

import math

import numpy as np
import pygplates
import pytest

from timefly_bake import cube, formats, geo, plates

N = 16

# Moving plate, age, pole lat, pole lon, angle (deg), fixed plate. PLATES4 columns.
ROT_LINES = [
    # 101: 30 deg about the north pole at 100 Ma.
    (101, 0.0, 0.0, 0.0, 0.0, 0),
    (101, 100.0, 90.0, 0.0, 30.0, 0),
    # 202: a genuine zero rotation throughout.
    (202, 0.0, 0.0, 0.0, 0.0, 0),
    (202, 600.0, 0.0, 0.0, 0.0, 0),
    # 303: the circuit ends at 20 Ma.
    (303, 0.0, 0.0, 0.0, 0.0, 0),
    (303, 20.0, 10.0, 20.0, 5.0, 0),
    # 404: relative to 000 until 10.2 Ma, then relative to 202 from 10.6 Ma: missing in between.
    (404, 0.0, 0.0, 0.0, 0.0, 0),
    (404, 10.2, 0.0, 90.0, 3.0, 0),
    (404, 10.6, 0.0, 90.0, 3.2, 202),
    (404, 30.0, 0.0, 90.0, 6.0, 202),
]


def write_rot(path, lines=ROT_LINES):
    text = "".join(
        f"{m:03d} {t:.1f} {plat:.2f} {plon:.2f} {ang:.2f} {f:03d} !! synthetic\n"
        for m, t, plat, plon, ang, f in lines
    )
    path.write_text(text)
    return path


def box(lat0, lat1, lon0, lon1, steps=8):
    """A lat-lon box as a polygon (edges densified so it stays box-shaped)."""
    pts = [(lat0, lon0 + (lon1 - lon0) * i / steps) for i in range(steps)]
    pts += [(lat0 + (lat1 - lat0) * i / steps, lon1) for i in range(steps)]
    pts += [(lat1, lon1 - (lon1 - lon0) * i / steps) for i in range(steps)]
    pts += [(lat1 - (lat1 - lat0) * i / steps, lon0) for i in range(steps)]
    return pygplates.PolygonOnSphere(pts)


def write_polygons(path, specs):
    """``specs`` are ``(plate_id, polygon, (begin_ma, end_ma))``."""
    features = [
        pygplates.Feature.create_reconstructable_feature(
            pygplates.FeatureType.gpml_unclassified_feature,
            poly,
            valid_time=valid,
            reconstruction_plate_id=pid,
        )
        for pid, poly, valid in specs
    ]
    pygplates.FeatureCollection(features).write(str(path))
    return path


def cell_of(fp, lat, lon):
    d = geo.geo_to_render(geo.latlon_to_geo(lat, lon))
    face, col, row = cube.dirs_to_texels(d, fp.cube_size)
    return fp.cell(int(face), int(row), int(col))


@pytest.fixture
def footprints(tmp_path):
    past = pygplates.GeoTimeInstant.create_distant_past()
    future = pygplates.GeoTimeInstant.create_distant_future()
    path = write_polygons(
        tmp_path / "polys.gpml",
        [
            (101, box(0, 40, -20, 40), (200.0, 0.0)),  # [0, 200]
            (101, box(0, 40, -20, 40), (400.0, 300.0)),  # disjoint: [300, 400]
            (101, box(20, 40, -20, 0), (250.0, 150.0)),  # overlaps [0, 200] -> [0, 250]
            (202, box(10, 50, 0, 60), (600.0, 100.0)),  # clipped to [100, 540]
            (303, box(-40, -10, 100, 140), (900.0, 600.0)),  # outside 0-540: dropped
            (505, box(-40, -10, -140, -100), (past, future)),  # [0, 540]
        ],
    )
    return plates.assign_plates(cube.texel_center_dirs(N), path)


def test_overlapping_polygons_keep_every_membership(footprints):
    fp = footprints
    assert fp.cube_size == N
    assert fp.offsets.shape == (6 * N * N + 1,)
    assert fp.offsets[0] == 0 and fp.offsets[-1] == len(fp.plate_ids)
    # Inside both 101 boxes and 202's box: different plates in one cell, and 101's disjoint
    # intervals kept apart.
    assert cell_of(fp, 15, 10) == [(101, 0.0, 200.0), (101, 300.0, 400.0), (202, 100.0, 540.0)]
    # Inside the third 101 box too: overlapping intervals of one plate are unioned.
    assert cell_of(fp, 30, -10) == [(101, 0.0, 250.0), (101, 300.0, 400.0)]
    assert cell_of(fp, 45, 50) == [(202, 100.0, 540.0)]
    assert cell_of(fp, -25, -120) == [(505, 0.0, 540.0)]
    assert cell_of(fp, -25, 120) == []  # entirely older than the model
    assert cell_of(fp, -60, 0) == []


def test_footprint_cells_are_in_stable_sorted_order(footprints):
    fp = footprints
    for c in range(6 * N * N):
        a, b = fp.offsets[c], fp.offsets[c + 1]
        keys = list(zip(fp.plate_ids[a:b], fp.younger_myr[a:b]))
        assert keys == sorted(keys)


def test_footprint_entries_map_source_ids_to_dense_indices(footprints, tmp_path):
    fp = footprints
    entries = plates.footprint_entries(fp, [101, 202, 505])
    assert len(entries) == len(fp.plate_ids)
    idx = {101: 0, 202: 1, 505: 2}
    assert [e[0] for e in entries] == [idx[int(p)] for p in fp.plate_ids]
    formats.write_tfplates(tmp_path / "p.tfplates", fp.cube_size, fp.offsets, entries)
    with pytest.raises(ValueError, match="202"):
        plates.footprint_entries(fp, [101, 505])


def test_assign_plates_matches_pygplates_containment(tmp_path):
    poly = box(-30, 30, 150, 200)  # straddles the antimeridian
    path = write_polygons(tmp_path / "p.gpml", [(7, poly, (100.0, 0.0))])
    dirs = cube.texel_center_dirs(8)
    fp = plates.assign_plates(dirs, path)
    geo_dirs = geo.render_to_geo(dirs)
    for f in range(6):
        for r in range(8):
            for c in range(8):
                inside = poly.is_point_in_polygon(tuple(geo_dirs[f, r, c]))
                assert fp.cell(f, r, c) == ([(7, 0.0, 100.0)] if inside else []), (f, r, c)


@pytest.fixture
def rot_path(tmp_path):
    return write_rot(tmp_path / "model.rot")


def test_genuine_zero_rotation_is_valid(rot_path):
    quats, valid = plates.export_rotations(rot_path, [202], [0.0, 50.0, 540.0])
    assert valid.tolist() == [[1], [1], [1]]
    assert np.allclose(quats[:, 0], [0, 0, 0, 1])


def test_missing_circuit_stays_missing(rot_path):
    times = [0.0, 10.0, 20.0, 21.0, 100.0]
    quats, valid = plates.export_rotations(rot_path, [303, 999], times)
    assert valid[:, 0].tolist() == [1, 1, 1, 0, 0]
    assert valid[:, 1].tolist() == [0, 0, 0, 0, 0]  # unknown plate: never an identity stand-in
    assert quats.shape == (5, 2, 4) and quats.dtype == np.float32


def test_north_pole_rotation_uses_the_render_permutation(rot_path):
    quats, valid = plates.export_rotations(rot_path, [101], [0.0, 50.0, 100.0])
    assert valid.all()
    for t, deg in [(1, 15.0), (2, 30.0)]:
        h = math.radians(deg) / 2
        # Geo axis +Z (north pole) becomes render +Y: geo (0, 0, s, c) -> render (0, s, 0, c).
        assert np.allclose(quats[t, 0], [0, math.sin(h), 0, math.cos(h)], atol=1e-6)
        moved = geo.quat_rotate(quats[t, 0], geo.geo_to_render(geo.latlon_to_geo(0.0, 0.0)))
        assert np.allclose(moved, geo.geo_to_render(geo.latlon_to_geo(0.0, deg)), atol=1e-6)


def test_quaternion_matches_pygplates_for_a_general_rotation(tmp_path):
    rot = write_rot(tmp_path / "g.rot", [(9, 0.0, 0, 0, 0, 0), (9, 50.0, 35.0, -60.0, 40.0, 0)])
    quats, _ = plates.export_rotations(rot, [9], [50.0])
    fr = pygplates.RotationModel(str(rot)).get_rotation(50.0, 9, anchor_plate_id=0)
    for lat, lon in [(12.0, 34.0), (-50.0, 170.0)]:
        want = geo.geo_to_render(np.array((fr * pygplates.PointOnSphere(lat, lon)).to_xyz()))
        got = geo.quat_rotate(quats[0, 0], geo.geo_to_render(geo.latlon_to_geo(lat, lon)))
        assert np.allclose(got, want, atol=1e-6)


def test_rotation_times_keep_boundaries_sources_and_missing_midpoints(rot_path):
    times = plates.rotation_times(rot_path, [101, 202, 303, 404], source_ages_myr=[66.5, 600.0])
    assert times[0] == 0.0 and times[-1] == 540.0
    assert np.all(np.diff(times) > 0)
    assert set(np.arange(541.0)) <= set(times)
    assert {66.5, 10.2, 10.6} <= set(times)  # source age and circuit boundaries
    assert 600.0 not in times  # outside the modelled range
    mid = [t for t in times if 10.2 < t < 10.6]
    assert len(mid) == 1  # the missing midpoint, so 10.2 and 10.6 are not slerped together
    quats, valid = plates.export_rotations(rot_path, [404], times)
    at = {t: bool(v) for t, v in zip(times, valid[:, 0])}
    assert at[10.0] and at[10.2] and at[10.6] and at[11.0]
    assert not at[mid[0]]


def test_export_rotations_requires_a_circuit_to_anchor_zero(tmp_path):
    rot = write_rot(tmp_path / "noanchor.rot", [(9, 0.0, 0, 0, 0, 5), (9, 50.0, 0, 0, 0, 5)])
    with pytest.raises(ValueError, match="anchor plate 0"):
        plates.export_rotations(rot, [9], [0.0])
