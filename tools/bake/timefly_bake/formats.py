"""Writers for the globe binary formats; the Rust readers live in ``timefly_core::formats``.

All integers are little-endian and each payload is a single zstd frame. Layouts:

* ``.tfgrid``: ``TFGRID01``, kind u8, dtype u8, channels u8, reserved u8, width u32, height u32,
  zlen u32, then zstd(array ``[face][row][col][channel]``, or ``[row][col][channel]`` for lat-lon).
* ``.tfrot``: ``TFROT002``, n_plates u32, n_times u32, zlen u32, then zstd(plate_ids u32,
  times_myr f64, quats f32 ``[time][plate][xyzw]``, valid u8 ``[time][plate]``).
* ``.tfplates``: ``TFPLAT01``, cube_size u32, n_entries u32, zlen u32, then zstd(offsets u32
  ``[6*n*n+1]``, entries of plate_index u32, younger_myr f64, older_myr f64, packed).
"""

from __future__ import annotations

import struct
from pathlib import Path

import numpy as np
import zstandard

from timefly_bake.log import traced

_KINDS = {"cube": 0, "latlon": 1}
_DTYPES = {np.dtype("<f2"): (0, "f16"), np.dtype("<u2"): (1, "u16"), np.dtype("<f4"): (2, "f32")}
_ENTRY = np.dtype([("plate_index", "<u4"), ("younger", "<f8"), ("older", "<f8")])
_ZSTD_LEVEL = 19


def _compress(payload: bytes) -> bytes:
    return zstandard.ZstdCompressor(level=_ZSTD_LEVEL).compress(payload)


def _decompress(blob: bytes, size: int) -> bytes:
    return zstandard.ZstdDecompressor().decompress(blob, max_output_size=size)


@traced
def write_tfgrid(path, kind: str, data: np.ndarray) -> None:
    """Write ``data`` (``(6, n, n, c)`` for ``cube``, ``(h, w, c)`` for ``latlon``) to ``path``."""
    if kind not in _KINDS:
        raise ValueError(f"unknown grid kind {kind!r}")
    dt = np.dtype(data.dtype).newbyteorder("<")
    if dt not in _DTYPES:
        raise ValueError(f"unsupported dtype {data.dtype}; use float16, uint16 or float32")
    if kind == "cube":
        if data.ndim != 4 or data.shape[0] != 6 or data.shape[1] != data.shape[2]:
            raise ValueError(f"cube data must have shape (6, n, n, c), got {data.shape}")
        height, width, channels = data.shape[1], data.shape[2], data.shape[3]
    else:
        if data.ndim != 3:
            raise ValueError(f"lat-lon data must have shape (h, w, c), got {data.shape}")
        height, width, channels = data.shape
    if not 1 <= channels <= 255 or width < 1 or height < 1:
        raise ValueError(f"bad grid dimensions {data.shape}")
    blob = _compress(np.ascontiguousarray(data, dtype=dt).tobytes())
    header = b"TFGRID01" + struct.pack(
        "<BBBBIII", _KINDS[kind], _DTYPES[dt][0], channels, 0, width, height, len(blob)
    )
    Path(path).write_bytes(header + blob)


@traced
def read_tfgrid(path):
    """Read a ``.tfgrid`` file back as ``(header dict, ndarray)``; used by tests and tooling."""
    raw = Path(path).read_bytes()
    if raw[:8] != b"TFGRID01":
        raise ValueError("bad .tfgrid magic")
    kind, dtype, channels, _res, width, height, zlen = struct.unpack("<BBBBIII", raw[8:24])
    kind_name = {v: k for k, v in _KINDS.items()}[kind]
    np_dtype, dname = next((d, n) for d, (i, n) in _DTYPES.items() if i == dtype)
    shape = (6, height, width, channels) if kind_name == "cube" else (height, width, channels)
    size = int(np.prod(shape)) * np_dtype.itemsize
    blob = raw[24 : 24 + zlen]
    if len(blob) != zlen:
        raise ValueError("truncated .tfgrid")
    data = np.frombuffer(_decompress(blob, size), dtype=np_dtype).reshape(shape)
    header = {
        "kind": kind_name,
        "dtype": dname,
        "channels": channels,
        "width": width,
        "height": height,
    }
    return header, data


@traced
def write_tfrot(path, plate_ids, times_myr, quats_render_xyzw, valid) -> None:
    """Write rotations: ``quats`` is ``[time][plate][4]`` (render frame x y z w), ``valid`` is
    ``[time][plate]`` (0 = missing circuit)."""
    ids = np.asarray(plate_ids, dtype="<u4")
    times = np.asarray(times_myr, dtype="<f8")
    quats = np.asarray(quats_render_xyzw, dtype="<f4")
    ok = np.asarray(valid, dtype=np.uint8)
    n_plates, n_times = len(ids), len(times)
    if quats.shape != (n_times, n_plates, 4) or ok.shape != (n_times, n_plates):
        raise ValueError(
            f"shape mismatch: quats {quats.shape}, valid {ok.shape}, "
            f"expected ({n_times}, {n_plates}, 4) and ({n_times}, {n_plates})"
        )
    if n_times == 0 or times[0] != 0.0 or np.any(np.diff(times) <= 0):
        raise ValueError("times_myr must be strictly ascending and start at 0")
    if ok.max(initial=0) > 1:
        raise ValueError("valid must contain only 0 or 1")
    blob = _compress(ids.tobytes() + times.tobytes() + quats.tobytes() + ok.tobytes())
    header = b"TFROT002" + struct.pack("<III", n_plates, n_times, len(blob))
    Path(path).write_bytes(header + blob)


@traced
def write_tfplates(path, cube_size: int, offsets, entries) -> None:
    """Write plate footprints. ``offsets`` has ``6*n*n+1`` non-decreasing values from 0 to
    ``len(entries)``; ``entries`` is ``(plate_index, younger_myr, older_myr)`` triples."""
    off = np.asarray(offsets, dtype="<u4")
    ent = np.zeros(len(entries), dtype=_ENTRY)
    for i, (idx, younger, older) in enumerate(entries):
        ent[i] = (idx, younger, older)
    cells = 6 * cube_size * cube_size
    if off.shape != (cells + 1,):
        raise ValueError(f"offsets must have {cells + 1} values, got {off.shape}")
    if off[0] != 0 or off[-1] != len(ent) or np.any(np.diff(off.astype(np.int64)) < 0):
        raise ValueError("offsets must start at 0, end at len(entries) and never decrease")
    blob = _compress(off.tobytes() + ent.tobytes())
    header = b"TFPLAT01" + struct.pack("<III", cube_size, len(ent), len(blob))
    Path(path).write_bytes(header + blob)
