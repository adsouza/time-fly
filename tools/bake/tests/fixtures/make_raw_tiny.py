"""Regenerate ``raw_tiny/``: a miniature copy of the ``data/raw`` tree for the bake CLI.

Run from ``tools/bake``: ``uv run python tests/fixtures/make_raw_tiny.py``. The tree mirrors the
real layout (archives as downloaded) so the CLI's production path is the tested path:

* ``paleodem/tiny_paleodem_nc.zip``: PaleoDEMs for 0 Ma and 540 Ma on a 10 deg grid (with the
  duplicated 180 deg seam column the real files carry) plus a licence file the bake ignores.
* ``paleodem_ages.csv``: the explicit age mapping (present-day at the 0.030 Myr bridge, 540 Ma).
* ``plates/{Rotations,StaticPolygons}.zip``: two plates. Plate 101 turns 40 deg east about the
  pole by 540 Ma and plate 202 stays put, so at 540 Ma they overlap and leave a gap behind 101.
* ``ice6g/I6_C.VM5a_1deg.0.nc.gz``: one 0 ka slice in the ICE-6G_C layout (10 deg cells,
  0..360 longitudes, masks in percent) with ocean, land, grounded and floating ice.
* ``sealevel/spratt2016-noaa.txt``: a NOAA-template sea-level file (tab-separated, CRLF).

Output is deterministic apart from library version strings inside the netCDF/GPML files.
"""

from __future__ import annotations

import csv
import gzip
import io
import tempfile
import zipfile
from pathlib import Path

import netCDF4
import numpy as np
import pygplates

OUT = Path(__file__).resolve().parent / "raw_tiny"
_ZIP_TIME = (2020, 1, 1, 0, 0, 0)

# Present-day boxes of the two plates: (plate, lat0, lat1, lon0, lon1).
PLATE_BOXES = [(101, -30.0, 30.0, -60.0, -10.0), (202, -30.0, 30.0, 0.0, 50.0)]
ROTATIONS = [
    (101, 0.0, 90.0, 0.0, 0.0, 0),
    (101, 540.0, 90.0, 0.0, 40.0, 0),
    (202, 0.0, 0.0, 0.0, 0.0, 0),
    (202, 540.0, 0.0, 0.0, 0.0, 0),
]
# (age_calkaBP, SeaLev_longPC1) rows; shortPC1 is NaN in the older rows, as in the real file.
SEA_LEVEL = [(2, -1.8), (0, 0.5), (1, -1.5), (30, -80.0), (31, -82.5), (29, -78.0)]


def _zip(path: Path, members: dict[str, bytes]) -> None:
    with zipfile.ZipFile(path, "w") as zf:
        for name, data in members.items():
            info = zipfile.ZipInfo(name, date_time=_ZIP_TIME)
            info.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(info, data)


def _box_mask(lat, lon, lat0, lat1, lon0, lon1):
    lon = (np.asarray(lon) + 180.0) % 360.0 - 180.0
    return (lat[:, None] >= lat0) & (lat[:, None] <= lat1) & (lon[None, :] >= lon0) & (
        lon[None, :] <= lon1
    )


def _paleodem_nc(directory: Path, name: str, shift_deg: float) -> bytes:
    """Ocean at -4000 m, the plate boxes as land (101 shifted east by ``shift_deg``)."""
    lat = np.arange(-90.0, 90.1, 10.0)
    lon = np.arange(-180.0, 180.1, 10.0)  # 37 columns: 180 duplicates -180
    z = np.full((lat.size, lon.size), -4000.0, dtype=np.float32)
    for plate, la0, la1, lo0, lo1 in PLATE_BOXES:
        shift = shift_deg if plate == 101 else 0.0
        z[_box_mask(lat, lon, la0, la1, lo0 + shift, lo1 + shift)] = 800.0 if plate == 101 else 300.0
    z[:, -1] = z[:, 0]
    path = directory / name
    with netCDF4.Dataset(path, "w", format="NETCDF4") as ds:
        ds.createDimension("lon", lon.size)
        ds.createDimension("lat", lat.size)
        ds.createVariable("lon", "f8", ("lon",))[:] = lon
        ds.createVariable("lat", "f8", ("lat",))[:] = lat
        ds.createVariable("z", "f4", ("lat", "lon"), fill_value=np.float32(np.nan))[:] = z
    return path.read_bytes()


def _ice6g_nc(directory: Path) -> bytes:
    lat = np.arange(-85.0, 86.0, 10.0, dtype=np.float32)  # cell centres, 18 rows
    lon = np.arange(5.0, 360.0, 10.0, dtype=np.float32)  # 0..360 convention, 36 columns
    topo = np.full((lat.size, lon.size), -3500.0, dtype=np.float32)
    land = np.zeros_like(topo)
    ice = np.zeros_like(topo)
    for _, la0, la1, lo0, lo1 in PLATE_BOXES:
        m = _box_mask(lat, lon, la0, la1, lo0, lo1)
        topo[m], land[m] = 400.0, 100.0
    # Antarctica: grounded ice on land; a floating ice shelf at sea level over bathymetry.
    topo[lat < -70], land[lat < -70], ice[lat < -70] = 2500.0, 100.0, 100.0
    shelf = (lat[:, None] > -70) & (lat[:, None] < -60) & (lon[None, :] > 150) & (lon[None, :] < 220)
    topo[shelf], land[shelf], ice[shelf] = -600.0, 0.0, 100.0
    # A partly-land coastal cell.
    land[9, 0] = 40.0
    path = directory / "I6_C.VM5a_1deg.0.nc"
    with netCDF4.Dataset(path, "w", format="NETCDF3_CLASSIC") as ds:
        ds.Model = "ICE=ICE-6G_C, Viscosity=VM5a (synthetic test fixture)"
        ds.createDimension("lat", lat.size)
        ds.createDimension("lon", lon.size)
        for name, values, units in (("lat", lat, "degrees_north"), ("lon", lon, "degrees_east")):
            var = ds.createVariable(name, "f4", (name,), fill_value=np.float32(1e20))
            var.units = units
            var[:] = values
        for name, values, units, long_name in (
            ("sftlf", land, "%", "Land area fraction. Time=0 Ka"),
            ("sftgif", ice, "%", "Ice area fraction. Time=0 Ka"),
            ("Topo", topo, "m", "Topography at time=0 Ka. (Point-value altitude)"),
        ):
            var = ds.createVariable(name, "f4", ("lat", "lon"), fill_value=np.float32(1e20))
            var.units = units
            var.long_name = long_name
            var.time = "  0.00 KBP"
            var[:] = values
    return path.read_bytes()


def _rotation_text() -> str:
    return "".join(
        f"{m:03d} {t:.1f} {plat:.2f} {plon:.2f} {ang:.2f} {f:03d} !! tiny plate {m}\n"
        for m, t, plat, plon, ang, f in ROTATIONS
    )


def _polygons_gpml(directory: Path) -> bytes:
    past = pygplates.GeoTimeInstant.create_distant_past()
    future = pygplates.GeoTimeInstant.create_distant_future()
    features = []
    for plate, la0, la1, lo0, lo1 in PLATE_BOXES:
        steps = 8
        pts = [(la0, lo0 + (lo1 - lo0) * i / steps) for i in range(steps)]
        pts += [(la0 + (la1 - la0) * i / steps, lo1) for i in range(steps)]
        pts += [(la1, lo1 - (lo1 - lo0) * i / steps) for i in range(steps)]
        pts += [(la1 - (la1 - la0) * i / steps, lo0) for i in range(steps)]
        features.append(
            pygplates.Feature.create_reconstructable_feature(
                pygplates.FeatureType.gpml_unclassified_feature,
                pygplates.PolygonOnSphere(pts),
                valid_time=(past, future),
                reconstruction_plate_id=plate,
            )
        )
    path = directory / "tiny.gpml"
    pygplates.FeatureCollection(features).write(str(path))
    return path.read_bytes()


def _sea_level_text() -> str:
    lines = [
        "# Global Sea Level Reconstruction using Stacked Records from 0-800 ka (TINY TEST FIXTURE)",
        "#   Time_Unit: cal yr BP",
        "## age_calkaBP\tage,,,calendar kiloyear before present,,paleoceanography,,,N,",
        "## SeaLev_shortPC1\tsea level,delta 18O,,meter,,,,,N,",
        "## SeaLev_longPC1\tsea level,delta 18O,,meter,,,,,N,meters above modern sea level",
        "# Missing_Value: NaN",
        "age_calkaBP\tSeaLev_shortPC1\tSeaLev_shortPC1_err_sig\tSeaLev_longPC1\tSeaLev_longPC1_err_sig",
    ]
    for ka, long_pc1 in SEA_LEVEL:
        short = "NaN" if ka >= 29 else f"{long_pc1 + 0.25:g}"
        lines.append(f"{ka}\t{short}\t1.0\t{long_pc1:g}\t2.0")
    return "\r\n".join(lines) + "\r\n"


def main() -> None:
    for sub in ("paleodem", "plates", "ice6g", "sealevel"):
        (OUT / sub).mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        _zip(
            OUT / "paleodem" / "tiny_paleodem_nc.zip",
            {
                "tiny_v1/Map01_tiny_0Ma.nc": _paleodem_nc(tmp, "a.nc", 0.0),
                "tiny_v1/Map88_tiny_540Ma.nc": _paleodem_nc(tmp, "b.nc", 40.0),
                "tiny_v1/License.txt": b"Synthetic test data; no licence restrictions.\n",
            },
        )
        _zip(OUT / "plates" / "Rotations.zip", {"Rotations/tiny.rot": _rotation_text().encode()})
        _zip(OUT / "plates" / "StaticPolygons.zip", {"StaticPolygons/tiny.gpml": _polygons_gpml(tmp)})
        buf = io.BytesIO()
        with gzip.GzipFile(fileobj=buf, mode="wb", mtime=0, filename="") as gz:
            gz.write(_ice6g_nc(tmp))
        (OUT / "ice6g" / "I6_C.VM5a_1deg.0.nc.gz").write_bytes(buf.getvalue())
    (OUT / "sealevel" / "spratt2016-noaa.txt").write_bytes(_sea_level_text().encode())
    with (OUT / "paleodem_ages.csv").open("w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(
            ["filename", "map_number", "label", "timeline_age_myr", "source_rotation_age_myr", "notes"]
        )
        w.writerow(["Map01_tiny_0Ma.nc", "1", "Present-day (tiny)", "0.03", "0", "bridge frame"])
        w.writerow(["Map88_tiny_540Ma.nc", "88", "540 Ma (tiny)", "540", "540", "terminal frame"])


if __name__ == "__main__":
    main()
