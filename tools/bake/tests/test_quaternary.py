"""ICE-6G_C frames and the Spratt & Lisiecki (2016) sea-level curve."""

import gzip

import netCDF4
import numpy as np
import pytest

from timefly_bake import ages, cube, geo, ice6g, sealevel
from timefly_bake.paleodem import LatLonGrid

N = 32

# Cell-centred 2 deg grid in the ICE-6G_C layout (0..360 longitudes).
LAT = np.arange(-89.0, 90.0, 2.0)
LON = np.arange(1.0, 360.0, 2.0)

# name: (lat0, lat1, lon0, lon1) in degrees, lon in -180..180.
REGIONS = {
    "shelf": (10.0, 30.0, 20.0, 40.0),
    "floating": (-80.0, -64.0, 140.0, 178.0),
    "grounded": (60.0, 80.0, -100.0, -60.0),
    "zero_ocean": (-30.0, -10.0, 100.0, 120.0),
    "open_ocean": (-10.0, 10.0, -150.0, -130.0),
}


def region_mask(name):
    lat0, lat1, lon0, lon1 = REGIONS[name]
    lon = (LON + 180.0) % 360.0 - 180.0
    return (LAT[:, None] >= lat0) & (LAT[:, None] <= lat1) & (lon >= lon0) & (lon <= lon1)


def grids(values):
    """``values``: region -> (topo m, land 0-1, ice 0-1); elsewhere open ocean at -3000 m."""
    topo = np.full((LAT.size, LON.size), -3000.0)
    land = np.zeros_like(topo)
    ice = np.zeros_like(topo)
    for name, (t, la, ic) in values.items():
        m = region_mask(name)
        topo[m], land[m], ice[m] = t, la, ic
    return tuple(LatLonGrid.normalized(LAT, LON, v) for v in (topo, land, ice))


LGM = {
    "shelf": (20.0, 1.0, 0.0),  # exposed shelf
    "floating": (-500.0, 0.0, 1.0),  # ice shelf floating over bathymetry
    "grounded": (2500.0, 1.0, 1.0),  # ice sheet on land (thickness already in Topo)
    "zero_ocean": (0.0, 0.0, 0.0),  # open ocean that happens to sit at 0 m
}
PRESENT = {
    "shelf": (-100.0, 0.0, 0.0),  # flooded shelf
    "floating": (-500.0, 0.0, 1.0),
    "grounded": (300.0, 1.0, 0.0),  # deglaciated land
    "zero_ocean": (0.0, 0.0, 0.0),
}


def texel(frame, name):
    lat0, lat1, lon0, lon1 = REGIONS[name]
    d = geo.geo_to_render(geo.latlon_to_geo((lat0 + lat1) / 2, (lon0 + lon1) / 2))
    face, col, row = cube.dirs_to_texels(d, N)
    return frame[int(face), int(row), int(col)].astype(np.float32)


def test_ice_frame_preserves_ocean_shelf_and_floating_ice():
    frame = ice6g.bake_ice_frame(cube.texel_center_dirs(N), *grids(LGM))
    assert frame.shape == (6, N, N, 4) and frame.dtype == np.float16
    # Open water: signed depth, no land, no ice.
    np.testing.assert_array_equal(texel(frame, "open_ocean"), [-3000.0, 0.0, 0.0, 0.0])
    # Exposed shelf: positive topography and full land coverage.
    np.testing.assert_array_equal(texel(frame, "shelf"), [20.0, 1.0, 0.0, 0.0])
    # Floating ice: ice coverage over otherwise open ocean; the surface stays bathymetry.
    np.testing.assert_array_equal(texel(frame, "floating"), [-500.0, 0.0, 1.0, 0.0])
    # Grounded ice on land.
    np.testing.assert_array_equal(texel(frame, "grounded"), [2500.0, 1.0, 1.0, 0.0])
    # Zero elevation alone does not make land.
    np.testing.assert_array_equal(texel(frame, "zero_ocean"), [0.0, 0.0, 0.0, 0.0])
    # Masks stay within 0..1 even across region edges; alpha is reserved.
    assert frame[..., 1:3].min() >= 0.0 and frame[..., 1:3].max() <= 1.0
    assert not frame[..., 3].any()


def test_ice_frame_masks_blend_linearly_and_independently():
    dirs = cube.texel_center_dirs(N)
    lgm = ice6g.bake_ice_frame(dirs, *grids(LGM)).astype(np.float32)
    now = ice6g.bake_ice_frame(dirs, *grids(PRESENT)).astype(np.float32)
    mid = 0.5 * lgm + 0.5 * now
    # The shelf floods: elevation and land coverage fall together, ice is unaffected.
    np.testing.assert_allclose(texel(mid, "shelf"), [-40.0, 0.5, 0.0, 0.0])
    # The ice sheet melts: land stays land, the ice overlay fades on its own channel.
    np.testing.assert_allclose(texel(mid, "grounded"), [1400.0, 1.0, 0.5, 0.0])
    for t in (0.0, 0.25, 0.75, 1.0):
        blend = (1 - t) * lgm + t * now
        assert blend[..., 1:3].min() >= 0.0 and blend[..., 1:3].max() <= 1.0


def write_ice_nc(path, ka=0, mask_units="%", drop=None, scale=100.0):
    topo = np.full((LAT.size, LON.size), -3000.0, dtype=np.float32)
    land = np.zeros_like(topo)
    ice = np.zeros_like(topo)
    m = region_mask("grounded")
    topo[m], land[m], ice[m] = 2500.0, scale, scale
    with netCDF4.Dataset(path, "w", format="NETCDF3_CLASSIC") as ds:
        ds.createDimension("lat", LAT.size)
        ds.createDimension("lon", LON.size)
        ds.createVariable("lat", "f4", ("lat",))[:] = LAT
        ds.createVariable("lon", "f4", ("lon",))[:] = LON
        for name, values, units in (
            ("Topo", topo, "m"),
            ("sftlf", land, mask_units),
            ("sftgif", ice, mask_units),
        ):
            if name == drop:
                continue
            var = ds.createVariable(name, "f4", ("lat", "lon"), fill_value=np.float32(1e20))
            var.units = units
            var.time = f"{ka:6.2f} KBP"
            var[:] = values
    return path


def test_load_ice6g_normalizes_percent_masks(tmp_path):
    path = write_ice_nc(tmp_path / "I6_C.VM5a_1deg.0.nc")
    topo, land, ice = ice6g.load_ice6g(path)
    for grid in (topo, land, ice):
        assert grid.lon[0] == -179.0 and grid.lon[-1] == 179.0  # wrapped from 0..360
        assert grid.lat[0] == -89.0
    assert topo.values.max() == 2500.0 and topo.values.min() == -3000.0
    assert land.values.max() == 1.0 and ice.values.max() == 1.0
    assert land.values.min() == 0.0

    # The same file gzipped, as downloaded, loads identically.
    gz = tmp_path / "I6_C.VM5a_1deg.0.nc.gz"
    gz.write_bytes(gzip.compress(path.read_bytes()))
    topo2, land2, ice2 = ice6g.load_ice6g(gz)
    np.testing.assert_array_equal(topo2.values, topo.values)
    np.testing.assert_array_equal(ice2.values, ice.values)


def test_load_ice6g_rejects_bad_inputs(tmp_path):
    with pytest.raises(ValueError, match="sftgif"):
        ice6g.load_ice6g(write_ice_nc(tmp_path / "a.nc", drop="sftgif"))
    with pytest.raises(ValueError, match="units"):
        ice6g.load_ice6g(write_ice_nc(tmp_path / "b.nc", mask_units="furlongs"))
    with pytest.raises(ValueError, match="0..1"):
        ice6g.load_ice6g(write_ice_nc(tmp_path / "c.nc", mask_units="1", scale=100.0))
    with pytest.raises(ValueError, match="21"):
        ice6g.load_ice6g(write_ice_nc(tmp_path / "d.nc", ka=20), expect_ka=21)
    # A fraction-valued mask is accepted as-is.
    _, land, _ = ice6g.load_ice6g(write_ice_nc(tmp_path / "e.nc", mask_units="1", scale=1.0))
    assert land.values.max() == 1.0


def test_list_ice6g_slices_is_contiguous_from_present(tmp_path):
    for k in (2, 0, 1):
        (tmp_path / f"I6_C.VM5a_1deg.{k}.nc.gz").write_bytes(b"")
    (tmp_path / "README.txt").write_bytes(b"")
    slices = ice6g.list_ice6g_slices(tmp_path)
    assert [k for k, _ in slices] == [0, 1, 2]
    assert slices[1][1] == tmp_path / "I6_C.VM5a_1deg.1.nc.gz"
    (tmp_path / "I6_C.VM5a_1deg.1.nc.gz").unlink()
    with pytest.raises(ValueError, match="1"):
        ice6g.list_ice6g_slices(tmp_path)


def test_ice6g_ages_add_the_game_offset():
    assert ages.BP_OFFSET_YEARS == 76
    assert ages.myr_from_1950_bp(0.0) == 0.000076
    assert ages.myr_from_1950_bp(26_000.0) == 0.026076
    np.testing.assert_array_equal(ages.myr_from_1950_bp(np.array([0.0, 21_000.0])), [7.6e-5, 0.021076])


SEA_TEXT = "\r\n".join(
    [
        "# Global Sea Level Reconstruction using Stacked Records from 0-800 ka",
        "#   Time_Unit: cal yr BP",
        "## age_calkaBP\tage,,,calendar kiloyear before present,,,,,N,",
        "# Missing_Value: NaN",
        "age_calkaBP\tSeaLev_shortPC1\tSeaLev_shortPC1_err_sig\tSeaLev_longPC1\tSeaLev_longPC1_err_sig",
        "2\t4.01\t4.83\t5.96\t4.69",
        "0\t8.49\t5.23\t8.96\t5.72",
        "798\tNaN\tNaN\t-92.37\t10.08",
        "1\t7.63\t4.87\t7.72\t5.13",
    ]
)


def test_sealevel_parses_and_sorts(tmp_path):
    path = tmp_path / "spratt2016-noaa.txt"
    path.write_bytes((SEA_TEXT + "\r\n").encode())
    age_myr, anomaly_m = sealevel.load_spratt2016(path)
    raw_ka = np.array([0, 1, 2, 798])
    assert np.all(np.diff(age_myr) > 0)
    # Exactly 76 years added to the raw BP dates.
    np.testing.assert_allclose(age_myr * 1e6 - raw_ka * 1000.0, 76.0, rtol=0, atol=1e-6)
    assert age_myr[0] == 0.000076 and age_myr[-1] == 0.798076
    # Long-PC1 anomalies (metres relative to present), not the short stack.
    np.testing.assert_array_equal(anomaly_m, [8.96, 7.72, 5.96, -92.37])


def test_sealevel_rejects_bad_files(tmp_path):
    path = tmp_path / "s.txt"
    path.write_text(SEA_TEXT.replace("SeaLev_longPC1\t", "Other\t"))
    with pytest.raises(ValueError, match="SeaLev_longPC1"):
        sealevel.load_spratt2016(path)
    path.write_text(SEA_TEXT.replace("-92.37", "NaN"))
    with pytest.raises(ValueError, match="798"):
        sealevel.load_spratt2016(path)
    path.write_text(SEA_TEXT.replace("798\t", "1\t"))
    with pytest.raises(ValueError, match="duplicate"):
        sealevel.load_spratt2016(path)
