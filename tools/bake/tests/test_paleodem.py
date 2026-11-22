"""PaleoDEM loading, sampling, geographic cube bake and the timeline/source age mapping."""

from pathlib import Path

import netCDF4
import numpy as np
import pytest

from timefly_bake import cube, geo, paleodem

REPO = Path(__file__).resolve().parents[3]
AGE_TABLE = REPO / "data" / "paleodem_ages.csv"


def test_bilinear_wraps_longitude_and_clamps_latitude():
    lat = np.array([-60.0, 0.0, 60.0])
    lon = np.array([-135.0, -45.0, 45.0, 135.0])
    values = np.arange(12, dtype=np.float32).reshape(3, 4) * 10.0
    grid = paleodem.LatLonGrid(lat, lon, values)

    # lon 180 lies halfway between the 135 and -135 (= 225) columns across the seam.
    at_seam = paleodem.sample_bilinear(grid, np.array([0.0, -60.0]), np.array([180.0, -180.0]))
    np.testing.assert_allclose(at_seam, [(values[1, 0] + values[1, 3]) / 2, (values[0, 0] + values[0, 3]) / 2])

    # Beyond the last row the 60 deg row is used unchanged.
    north = paleodem.sample_bilinear(grid, np.full(4, 89.0), lon)
    np.testing.assert_allclose(north, values[2])
    south = paleodem.sample_bilinear(grid, np.full(4, -90.0), lon)
    np.testing.assert_allclose(south, values[0])

    # Exact nodes and an interior point, plus longitudes given outside [-180, 180).
    np.testing.assert_allclose(paleodem.sample_bilinear(grid, 0.0, 45.0 + 360.0), values[1, 2])
    mid = paleodem.sample_bilinear(grid, 30.0, 0.0)
    np.testing.assert_allclose(mid, values[1:3, 1:3].mean())


def test_duplicate_seam_column_is_dropped():
    lat = np.array([-45.0, 45.0])
    lon = np.array([-180.0, -90.0, 0.0, 90.0, 180.0])
    values = np.array([[1, 2, 3, 4, 1], [5, 6, 7, 8, 5]], dtype=np.float32)
    grid = paleodem.LatLonGrid.normalized(lat, lon, values)
    np.testing.assert_array_equal(grid.lon, [-180.0, -90.0, 0.0, 90.0])
    np.testing.assert_allclose(paleodem.sample_bilinear(grid, 45.0, 135.0), (8 + 5) / 2)

    bad = values.copy()
    bad[0, -1] = 99.0
    with pytest.raises(ValueError, match="seam"):
        paleodem.LatLonGrid.normalized(lat, lon, bad)


def test_float_noise_at_seam_and_poles_is_tolerated():
    # The real PaleoDEM axes carry ~2e-11 deg of noise, e.g. lon[0] = -180.00000000002.
    lat = np.array([-90.0 - 1e-11, 0.0, 90.0 + 1e-11])
    lon = np.array([-180.0 - 2e-11, -90.0, 0.0, 90.0, 180.0 + 2e-11])
    values = np.tile(np.array([1, 2, 3, 4, 1], dtype=np.float32), (3, 1))
    grid = paleodem.LatLonGrid.normalized(lat, lon, values)
    np.testing.assert_array_equal(grid.lon, [-180.0, -90.0, 0.0, 90.0])
    np.testing.assert_array_equal(grid.lat, [-90.0, 0.0, 90.0])
    np.testing.assert_array_equal(grid.values[1], [1, 2, 3, 4])


def write_nc(path, lat, lon, z, lon_first=False):
    with netCDF4.Dataset(path, "w") as ds:
        ds.createDimension("lon", len(lon))
        ds.createDimension("lat", len(lat))
        ds.createVariable("lon", "f8", ("lon",))[:] = lon
        ds.createVariable("lat", "f8", ("lat",))[:] = lat
        dims = ("lon", "lat") if lon_first else ("lat", "lon")
        var = ds.createVariable("z", "f4", dims, fill_value=np.float32(np.nan))
        var[:] = z.T if lon_first else z
    return path


def test_load_normalizes_axis_order_and_seam(tmp_path):
    lat = np.array([60.0, 0.0, -60.0])  # descending
    lon = np.array([0.0, 90.0, 180.0, 270.0, 360.0])  # 0..360 with a duplicate seam
    z = np.array([[1, 2, 3, 4, 1], [5, 6, 7, 8, 5], [9, 10, 11, 12, 9]], dtype=np.float32)
    path = write_nc(tmp_path / "grid.nc", lat, lon, z, lon_first=True)

    grid = paleodem.load_paleodem(path)
    np.testing.assert_array_equal(grid.lat, [-60.0, 0.0, 60.0])
    np.testing.assert_array_equal(grid.lon, [-180.0, -90.0, 0.0, 90.0])
    assert grid.values.dtype == np.float32
    # Row -60 came from the last source row; lon -180 is source lon 180, -90 is source 270.
    np.testing.assert_array_equal(grid.values[0], [11, 12, 9, 10])
    np.testing.assert_array_equal(grid.values[2], [3, 4, 1, 2])


def test_load_rejects_fill_values(tmp_path):
    lat = np.array([-45.0, 45.0])
    lon = np.array([-90.0, 0.0, 90.0, 180.0])
    z = np.ones((2, 4), dtype=np.float32)
    z[1, 2] = np.nan
    path = write_nc(tmp_path / "holes.nc", lat, lon, z)
    with pytest.raises(ValueError, match="holes.nc"):
        paleodem.load_paleodem(path)


def coastline_grid():
    """A 1 deg global grid: sea south of 5 N, land north of it, plus a longitude ripple."""
    lat = np.arange(-90.0, 90.0 + 0.5, 1.0)
    lon = np.arange(-180.0, 180.0, 1.0)
    la, lo = np.meshgrid(lat, lon, indexing="ij")
    values = (40.0 * (la - 5.0) + 150.0 * np.cos(np.radians(lo))).astype(np.float32)
    return paleodem.LatLonGrid(lat, lon, values)


def expected_coastline(dirs_render):
    lat, lon = geo.geo_to_latlon(geo.render_to_geo(dirs_render))
    return 40.0 * (lat - 5.0) + 150.0 * np.cos(np.radians(lon)), lon


def test_geographic_cube_preserves_signed_elevation():
    n = 16
    dirs = cube.texel_center_dirs(n)
    grid = coastline_grid()
    out = paleodem.bake_paleo_frame(dirs, grid)

    assert out.shape == (6, n, n, 2)
    assert out.dtype == np.float16
    assert np.all(np.isfinite(out))
    assert np.all(out[..., 1] == 0)

    want, lon = expected_coastline(dirs)
    elev = out[..., 0].astype(np.float64)
    # Bilinear on a 1 deg grid reproduces the field to well within float16 precision.
    np.testing.assert_allclose(elev, want, atol=4.0)
    assert (elev > 0).any() and (elev < 0).any()
    np.testing.assert_array_equal(np.sign(elev[np.abs(want) > 10]), np.sign(want[np.abs(want) > 10]))

    # Pretend the plate model only tracks the eastern hemisphere: the bake knows nothing of
    # plates, so the untracked cells keep their geographic elevation on both sides of sea level.
    untracked = lon < 0.0
    assert (elev[untracked] < 0).any() and (elev[untracked & (want > 10)] > 0).any()
    np.testing.assert_allclose(elev[untracked], want[untracked], atol=4.0)

    # Face +Z centre is (0 N, 0 E) in the render frame.
    centre = paleodem.bake_paleo_frame(np.array([[[[0.0, 0.0, 1.0]]]]), grid)
    np.testing.assert_allclose(float(centre[0, 0, 0, 0]), 40.0 * -5.0 + 150.0, atol=0.5)


def age(filename, timeline, source, label="label", map_number="1", notes=""):
    return paleodem.PaleoDemAge(filename, map_number, label, timeline, source, notes)


def touch_all(directory, names):
    for name in names:
        (directory / name).write_bytes(b"")


def test_source_age_and_timeline_age_are_distinct(tmp_path):
    names = ["Map01_x_0Ma.nc", "Map16_x_65Ma.nc", "Map88_x_540Ma.nc"]
    touch_all(tmp_path, names)
    mapping = [
        age("Map88_x_540Ma.nc", 540.0, 540.0, "Cambrian/Precambrian boundary (541 Ma)", "88"),
        age("Map01_x_0Ma.nc", 0.030, 0.0, "Present-day (Holocene, 0 Ma)", "1"),
        age("Map16_x_65Ma.nc", 66.0, 65.0, "KT Boundary (latest Maastrichtian, 66 Ma)", "16"),
    ]
    frames = paleodem.list_paleodem_files(tmp_path, mapping)

    assert [f.timeline_age_myr for f in frames] == [0.030, 66.0, 540.0]
    assert [f.source_rotation_age_myr for f in frames] == [0.0, 65.0, 540.0]
    assert [f.path for f in frames] == [tmp_path / n for n in names]
    assert frames[1].label == "KT Boundary (latest Maastrichtian, 66 Ma)"
    assert frames[1].map_number == "16"

    # A file nobody mapped fails with its name.
    (tmp_path / "Map17_x_70Ma.nc").write_bytes(b"")
    with pytest.raises(ValueError, match="Map17_x_70Ma.nc"):
        paleodem.list_paleodem_files(tmp_path, mapping)
    (tmp_path / "Map17_x_70Ma.nc").unlink()

    # Two files on one timeline age fail with the second file's name.
    (tmp_path / "Map15_x_60Ma.nc").write_bytes(b"")
    duplicate = mapping + [age("Map15_x_60Ma.nc", 66.0, 60.0)]
    with pytest.raises(ValueError, match="Map15_x_60Ma.nc"):
        paleodem.list_paleodem_files(tmp_path, duplicate)


def test_mapping_rows_must_resolve_and_span_the_timeline(tmp_path):
    touch_all(tmp_path, ["Map01_x_0Ma.nc", "Map88_x_540Ma.nc"])
    base = [age("Map01_x_0Ma.nc", 0.030, 0.0), age("Map88_x_540Ma.nc", 540.0, 540.0)]
    assert len(paleodem.list_paleodem_files(tmp_path, base)) == 2

    with pytest.raises(ValueError, match="Map99_x_9Ma.nc"):
        paleodem.list_paleodem_files(tmp_path, base + [age("Map99_x_9Ma.nc", 9.0, 9.0)])
    with pytest.raises(ValueError, match="Map01_x_0Ma.nc"):
        paleodem.list_paleodem_files(tmp_path, base + [age("Map01_x_0Ma.nc", 1.0, 0.0)])
    with pytest.raises(ValueError, match="present-day"):
        paleodem.list_paleodem_files(
            tmp_path, [age("Map01_x_0Ma.nc", 0.030, 5.0), base[1]]
        )
    with pytest.raises(ValueError, match="540"):
        paleodem.list_paleodem_files(
            tmp_path, [base[0], age("Map88_x_540Ma.nc", 541.0, 540.0)]
        )
    with pytest.raises(ValueError, match="540"):
        paleodem.list_paleodem_files(
            tmp_path, [base[0], age("Map88_x_540Ma.nc", 540.0, 535.0)]
        )


def test_age_mapping_csv_round_trip(tmp_path):
    path = tmp_path / "ages.csv"
    path.write_text(
        "filename,map_number,label,timeline_age_myr,source_rotation_age_myr,notes\n"
        'Map16_x_65Ma.nc,16,"KT Boundary (latest Maastrichtian, 66 Ma)",66,65,strat vs plate\n'
    )
    (row,) = paleodem.load_age_mapping(path)
    assert row == age("Map16_x_65Ma.nc", 66.0, 65.0, "KT Boundary (latest Maastrichtian, 66 Ma)", "16", "strat vs plate")

    path.write_text("filename,label\nx.nc,y\n")
    with pytest.raises(ValueError, match="column"):
        paleodem.load_age_mapping(path)


def test_committed_age_table_is_consistent():
    rows = paleodem.load_age_mapping(AGE_TABLE)
    timeline = [r.timeline_age_myr for r in rows]
    source = [r.source_rotation_age_myr for r in rows]
    assert len(rows) == 109
    assert len({r.filename for r in rows}) == len(rows)
    assert all(b > a for a, b in zip(timeline, timeline[1:]))
    assert all(b > a for a, b in zip(source, source[1:]))
    assert rows[0].source_rotation_age_myr == 0.0 and rows[0].timeline_age_myr == 0.030
    assert rows[-1].timeline_age_myr == 540.0 and rows[-1].source_rotation_age_myr == 540.0
    # Reconstruction ages are the Table 1 plate model ages: the 5 Myr series 0..540.
    assert source == [5.0 * i for i in range(109)]
    by_file = {r.filename: r for r in rows}
    kt = by_file["Map16_PALEOMAP_1deg_KT_Boundary_65Ma.nc"]
    assert (kt.timeline_age_myr, kt.source_rotation_age_myr) == (66.0, 65.0)
    givetian = by_file["Map67.5_PALEOMAP_1deg_Middle_Devonian_385.2Ma.nc"]
    assert (givetian.timeline_age_myr, givetian.source_rotation_age_myr) == (385.2, 385.0)
    assert all(r.label for r in rows)
