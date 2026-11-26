import io
import struct
import zipfile
from pathlib import Path

import numpy as np
import pytest

from timefly_bake import lakemap

SHIPPED = Path(__file__).resolve().parents[3] / "data" / "hydrology" / "lake_map.bin"

# ETOPO 2022 60 arc-second surface cells under these points (read once from
# ETOPO_2022_v1_60s_N90W180_surface.nc, which CI does not hold): the fine data's lake surface.
DEAD_SEA_FINE_M = -427.0
TITICACA_M = 3812.0


def _shapefile_zip(features) -> bytes:
    """A polygon shapefile in a zip: ``features`` is ``[(featurecla, name, rings)]``."""
    records = []
    for _, _, rings in features:
        points = np.concatenate(rings)
        parts = np.cumsum([0] + [len(r) for r in rings[:-1]]).astype("<i4")
        body = struct.pack("<i4d2i", 5, *points.min(0), *points.max(0), len(rings), len(points))
        body += parts.tobytes() + points.astype("<f8").tobytes()
        records.append(body)
    shp = b"".join(
        struct.pack(">2i", k + 1, len(b) // 2) + b for k, b in enumerate(records)
    )
    header = struct.pack(">i20xi", 9994, (100 + len(shp)) // 2)
    header += struct.pack("<2i4d32x", 1000, 5, -180, -90, 180, 90)
    fields = [("featurecla", 16), ("name", 12)]
    dbf = struct.pack("<B3xI2H20x", 3, len(features), 32 + 32 * len(fields) + 1, 1 + 28)
    for name, width in fields:
        dbf += name.encode().ljust(11, b"\0") + b"C" + b"\0" * 4 + bytes([width]) + b"\0" * 15
    dbf += b"\x0d"
    for cla, name, _ in features:
        dbf += b" " + cla.encode().ljust(16) + name.encode().ljust(12)
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as zf:
        zf.writestr(lakemap.SHP_MEMBER, header + shp)
        zf.writestr(lakemap.DBF_MEMBER, dbf + b"\x1a")
    return out.getvalue()


def _square(lon0, lat0, size):
    return np.array(
        [[lon0, lat0], [lon0, lat0 + size], [lon0 + size, lat0 + size], [lon0 + size, lat0],
         [lon0, lat0]],
        dtype=np.float64,
    )


def test_reader_keeps_lakes_and_rasterises_holes(tmp_path):
    zip_path = tmp_path / "lakes.zip"
    zip_path.write_bytes(
        _shapefile_zip(
            [
                ("Lake", "ring", [_square(10.0, 10.0, 1.0), _square(10.25, 10.25, 0.5)]),
                ("Reservoir", "dam", [_square(20.0, 20.0, 1.0)]),
                ("Alkaline Lake", "salt", [_square(-5.0, -5.0, 0.3)]),
            ]
        )
    )
    feats = lakemap.read_shapefile(zip_path)
    assert [a["name"] for a, _ in feats] == ["ring", "dam", "salt"]
    kept = lakemap.lake_features(zip_path)
    assert [a["name"] for a, _ in kept] == ["ring", "salt"]
    ids = lakemap.rasterise(kept)
    # The ring: 20 x 20 cells less its 10 x 10 hole; the salt lake 6 x 6.
    assert np.count_nonzero(ids == 1) == 400 - 100
    assert np.count_nonzero(ids == 2) == 36
    assert ids[lakemap.cell_of(10.1, 10.1)] == 1
    assert ids[lakemap.cell_of(10.5, 10.5)] == 0
    assert ids[lakemap.cell_of(20.5, 20.5)] == 0


def test_lake_map_round_trips():
    rng = np.random.default_rng(7)
    ids = np.zeros((40, 80), dtype=np.uint16)
    for _ in range(30):
        r, c = rng.integers(0, 40), rng.integers(0, 80)
        ids[r : r + rng.integers(1, 5), c : c + rng.integers(1, 9)] = rng.integers(1, 6)
    ids[:, -1] = 3  # a run ending on the last column
    levels = np.array([1.5, -427.0, 3808.5, 0.0, 12.25], dtype=np.float32)
    got_ids, got_levels = lakemap.decode(lakemap.encode(ids, levels))
    assert np.array_equal(got_ids, ids)
    assert np.array_equal(got_levels, levels)
    with pytest.raises(ValueError):
        lakemap.decode(b"XXXX" + lakemap.encode(ids, levels)[4:])

    # The shipped map: it decodes and re-encodes to the same bytes, within the size limit.
    blob = SHIPPED.read_bytes()
    assert len(blob) <= 512 * 1024
    ids, levels = lakemap.decode(blob)
    assert ids.shape == (lakemap.HEIGHT, lakemap.WIDTH)
    assert lakemap.encode(ids, levels) == blob

    def level_at(lat, lon):
        lake = ids[lakemap.cell_of(lat, lon)]
        return float(levels[lake - 1]) if lake else None

    for lat, lon in [(-15.8, -69.4), (-1.0, 33.0), (41.1, -112.5)]:
        assert level_at(lat, lon) is not None, (lat, lon)
    # Manaus (the Amazon's flats) and Skardu (the Indus valley) hold no lake.
    for lat, lon in [(-3.1, -60.0), (35.3, 75.6)]:
        assert level_at(lat, lon) is None, (lat, lon)
    # The fine surface, not the 10 km overview's -276 m for the Dead Sea.
    assert abs(level_at(31.5, 35.5) - DEAD_SEA_FINE_M) <= 5.0
    assert abs(level_at(-15.8, -69.4) - TITICACA_M) <= 5.0
