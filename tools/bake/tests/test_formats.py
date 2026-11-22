"""Round-trips for the .tfgrid / .tfrot / .tfplates writers (cross-language fixtures live in Rust)."""

import numpy as np
import pytest

from timefly_bake import formats


def _cube():
    a = np.zeros((6, 2, 2, 2), dtype=np.float16)
    for f in range(6):
        for r in range(2):
            for c in range(2):
                for ch in range(2):
                    a[f, r, c, ch] = f * 100 + r * 10 + c + ch * 0.5
    return a


def test_tfgrid_cube_round_trip(tmp_path):
    p = tmp_path / "c.tfgrid"
    data = _cube()
    formats.write_tfgrid(p, "cube", data)
    header, back = formats.read_tfgrid(p)
    assert header["kind"] == "cube" and header["width"] == 2 and header["channels"] == 2
    assert header["dtype"] == "f16"
    assert back.dtype == np.float16 and np.array_equal(back, data)
    assert p.read_bytes()[:8] == b"TFGRID01"


def test_tfgrid_latlon_u16_round_trip(tmp_path):
    p = tmp_path / "l.tfgrid"
    data = np.arange(3 * 4 * 1, dtype=np.uint16).reshape(3, 4, 1)
    formats.write_tfgrid(p, "latlon", data)
    header, back = formats.read_tfgrid(p)
    assert (header["kind"], header["width"], header["height"]) == ("latlon", 4, 3)
    assert np.array_equal(back, data)


def test_tfgrid_rejects_bad_shapes(tmp_path):
    with pytest.raises(ValueError):
        formats.write_tfgrid(tmp_path / "x", "cube", np.zeros((6, 2, 3, 1), np.float32))
    with pytest.raises(ValueError):
        formats.write_tfgrid(tmp_path / "x", "latlon", np.zeros((2, 2, 1), np.float64))


def test_tfrot_layout(tmp_path):
    p = tmp_path / "r.tfrot"
    quats = np.zeros((2, 3, 4), np.float32)
    quats[:, :, 3] = 1.0
    quats[1, 2] = (0, 0.7071068, 0, 0.7071068)
    valid = np.ones((2, 3), np.uint8)
    valid[1, 1] = 0
    formats.write_tfrot(p, [101, 202, 303], [0.0, 1.0], quats, valid)
    raw = p.read_bytes()
    assert raw[:8] == b"TFROT002"
    assert int.from_bytes(raw[8:12], "little") == 3
    assert int.from_bytes(raw[12:16], "little") == 2


def test_tfplates_layout(tmp_path):
    p = tmp_path / "p.tfplates"
    offsets = np.zeros(25, np.uint32)
    offsets[2:] = 1
    offsets[8:] = 3
    entries = [(1, 0.0, 50.0), (0, 100.0, 200.0), (2, 0.0, 540.0)]
    formats.write_tfplates(p, 2, offsets, entries)
    raw = p.read_bytes()
    assert raw[:8] == b"TFPLAT01"
    assert int.from_bytes(raw[8:12], "little") == 2
    assert int.from_bytes(raw[12:16], "little") == 3


def test_tfplates_rejects_bad_offsets(tmp_path):
    with pytest.raises(ValueError):
        formats.write_tfplates(tmp_path / "x", 2, np.zeros(24, np.uint32), [])
