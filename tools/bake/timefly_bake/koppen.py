"""Koppen-Geiger loader: the 1991-2020 map from Beck et al. (2023) onto the node lattice.

``koppen_geiger_tif.zip`` holds one GeoTIFF per period and resolution; the one used is
``1991_2020/koppen_geiger_0p00833333.tif`` (uint8, 21600 x 43200, 30 arc-second cells, north
up, origin at 90 N / 180 W). Codes 1-30 are the Beck classes and 0 is sea / no data. The
GeoTIFF is read straight from the zip (nothing is unpacked or executed) and mode-resampled onto
the 60 arc-second node lattice with :func:`timefly_bake.tiles.mode_resample_nodes`.
"""

from __future__ import annotations

import io
import logging
import zipfile
from pathlib import Path

import numpy as np
import tifffile

from timefly_bake import tiles
from timefly_bake.log import traced

MEMBER = "1991_2020/koppen_geiger_0p00833333.tif"
CELLS_PER_NODE_STEP = 2  # 30 arc-second cells per 60 arc-second step
MAX_CODE = 30
_MAX_MEMBER_BYTES = 256 * 1024 * 1024
_log = logging.getLogger(__name__)


@traced
def load(zip_path, member: str = MEMBER) -> np.ndarray:
    """Koppen codes (uint8, 0 = sea / no data) on the node lattice, south to north, shape
    ``(180 * 60 + 1, 360 * 60)``."""
    zip_path = Path(zip_path)
    with zipfile.ZipFile(zip_path) as zf:
        info = zf.getinfo(member)
        if info.file_size > _MAX_MEMBER_BYTES:
            raise ValueError(f"{zip_path.name}: {member} is {info.file_size} bytes")
        blob = zf.read(info)
    with tifffile.TiffFile(io.BytesIO(blob)) as tif:
        page = tif.pages[0]
        scale = page.tags["ModelPixelScaleTag"].value
        tie = page.tags["ModelTiepointTag"].value
        codes = page.asarray()
    cell = 1.0 / (tiles.PER_DEG * CELLS_PER_NODE_STEP)
    expect = (180 * tiles.PER_DEG * CELLS_PER_NODE_STEP, 360 * tiles.PER_DEG * CELLS_PER_NODE_STEP)
    if codes.shape != expect or codes.dtype != np.uint8:
        raise ValueError(f"{member}: {codes.dtype} {codes.shape}, expected uint8 {expect}")
    if abs(scale[0] - cell) > 1e-9 or abs(scale[1] - cell) > 1e-9:
        raise ValueError(f"{member}: pixel scale {tuple(scale[:2])}, expected {cell}")
    if tuple(tie[:2]) != (0.0, 0.0) or abs(tie[3] + 180.0) > 1e-9 or abs(tie[4] - 90.0) > 1e-9:
        raise ValueError(f"{member}: tie point {tuple(tie)}, expected pixel (0, 0) at 180 W, 90 N")
    top = int(codes.max())
    if top > MAX_CODE:
        raise ValueError(f"{member}: class code {top} is above {MAX_CODE}")
    _log.info(
        "%s: %s cells at 30 arc-seconds, %.1f%% sea / no data; mode-resampling to nodes",
        member,
        codes.shape,
        100.0 * float(np.count_nonzero(codes == 0)) / codes.size,
    )
    return tiles.mode_resample_nodes(codes[::-1], CELLS_PER_NODE_STEP)
