"""Golden values shared with the Rust crate (timefly_core::geo and ::cube)."""

import numpy as np

from timefly_bake import cube, geo


def test_latlon_frames():
    assert np.allclose(geo.geo_to_render(geo.latlon_to_geo(0.0, 0.0)), [0, 0, 1])  # lon 0 faces +Z
    assert np.allclose(geo.geo_to_render(geo.latlon_to_geo(0.0, 90.0)), [1, 0, 0])  # east is +X
    assert np.allclose(geo.geo_to_render(geo.latlon_to_geo(90.0, 0.0)), [0, 1, 0])  # north is +Y
    lat, lon = geo.geo_to_latlon(geo.latlon_to_geo(-33.9, 151.2))
    assert abs(lat + 33.9) < 1e-9 and abs(lon - 151.2) < 1e-9


def test_vectorised_and_round_trips():
    lat = np.array([[10.0, -45.0], [89.0, 0.0]])
    lon = np.array([[170.0, -170.0], [20.0, 180.0]])
    v = geo.latlon_to_geo(lat, lon)
    assert v.shape == (2, 2, 3)
    assert np.allclose(geo.render_to_geo(geo.geo_to_render(v)), v)
    lat2, lon2 = geo.geo_to_latlon(v)
    assert np.allclose(lat2, lat) and np.allclose(lon2, lon)


def test_longitude_canonical_at_poles_and_antimeridian():
    lat, lon = geo.geo_to_latlon(geo.latlon_to_geo(np.array([90.0, -90.0]), np.array([33.0, -5.0])))
    assert np.all(lon == 0.0) and np.allclose(lat, [90.0, -90.0])
    _, lon = geo.geo_to_latlon(np.array([-1.0, -0.0, 0.0]))
    assert lon == 180.0


def test_quat_rotate_matches_permutation():
    axis = np.array([0.3, -0.5, 0.8])
    axis /= np.linalg.norm(axis)
    half = 0.55
    q = np.array([*(axis * np.sin(half)), np.cos(half)])  # xyzw
    v = geo.latlon_to_geo(12.0, 34.0)
    # Rodrigues reference.
    angle = 2 * half
    ref = (
        v * np.cos(angle)
        + np.cross(axis, v) * np.sin(angle)
        + axis * np.dot(axis, v) * (1 - np.cos(angle))
    )
    assert np.allclose(geo.quat_rotate(q, v), ref)
    qr = np.array([q[1], q[2], q[0], q[3]])
    assert np.allclose(geo.quat_rotate(qr, geo.geo_to_render(v)), geo.geo_to_render(ref))


def test_golden_texel_dirs():
    d = cube.texel_center_dirs(2)
    assert d.shape == (6, 2, 2, 3)
    assert np.allclose(d[4, 0, 0], (-0.408248290, 0.408248290, 0.816496581))  # +Z
    assert np.allclose(d[0, 0, 1], (0.816496581, 0.408248290, -0.408248290))  # +X
    assert np.allclose(np.linalg.norm(d, axis=-1), 1.0)


def test_texel_round_trip_all_faces():
    n = 8
    face, col, row = cube.dirs_to_texels(cube.texel_center_dirs(n), n)
    f, r, c = np.meshgrid(range(6), range(n), range(n), indexing="ij")
    assert np.array_equal(face, f) and np.array_equal(col, c) and np.array_equal(row, r)


def test_dirs_to_texels_axes_and_corner():
    face, _, _ = cube.dirs_to_texels(np.array([[0.0, 1.0, 0.0], [0.0, 0.0, -1.0]]), 4)
    assert list(face) == [2, 5]  # +Y, -Z
    _, c, r = cube.dirs_to_texels(np.ones(3) / np.sqrt(3), 4)
    assert 0 <= c < 4 and 0 <= r < 4
