"""Pure transforms for the region-world tiles: lattice, tile split, Koppen mode resample.

Everything here works on arrays held in memory; the loaders (``etopo``, ``koppen``) and the
writer (``cli.write_tiles``) do the I/O.

Lattice. The ETOPO 2022 60 arc-second product is cell-registered (cell centres half a step in
from the edges). Tiles are sampled on the *node* lattice instead, with both tile edges included,
so :func:`cell_to_node_mean` and :func:`mode_resample_nodes` move cell grids onto nodes. Node
arrays are ordered south to north, ``180 * per_deg + 1`` rows by ``360 * per_deg`` columns from
lon -180 (the 180 E column is the -180 column, so longitude wraps).

Tiles. Tile ``(row, col)`` covers ``TILE_DEG`` x ``TILE_DEG`` degrees; row 0 is the northernmost
(lat 85..90 N), column 0 starts at lon -180. Inside a tile row 0 is the northern edge. Channel 0
is elevation in metres and channel 1 the Koppen code, both float16.
"""

from __future__ import annotations

from collections.abc import Iterator

import numpy as np

from timefly_bake.log import traced

TILE_DEG = 5
TILE_ROWS = 180 // TILE_DEG
TILE_COLS = 360 // TILE_DEG
PER_DEG = 60  # 60 arc-second lattice
_CHUNK_ROWS = 512


@traced
def tile_bounds(row: int, col: int) -> tuple[int, int, int, int]:
    """``(lat_n, lat_s, lon_w, lon_e)`` in degrees of tile ``(row, col)``."""
    if not (0 <= row < TILE_ROWS and 0 <= col < TILE_COLS):
        raise ValueError(f"tile ({row}, {col}) is outside the {TILE_ROWS} x {TILE_COLS} grid")
    lat_n = 90 - TILE_DEG * row
    lon_w = -180 + TILE_DEG * col
    return lat_n, lat_n - TILE_DEG, lon_w, lon_w + TILE_DEG


@traced
def split_tiles(
    elev: np.ndarray, koppen: np.ndarray, per_deg: int = PER_DEG
) -> Iterator[tuple[int, int, np.ndarray]]:
    """Yield ``(row, col, tile)`` for all 36 x 72 tiles; ``tile`` is float16 ``(n, n, 2)`` with
    ``n = 5 * per_deg + 1``. Neighbouring tiles share their edge samples, and column 71 wraps to
    column 0 across the antimeridian. ``elev`` and ``koppen`` are node lattices (see module doc).
    """
    shape = (180 * per_deg + 1, 360 * per_deg)
    if elev.shape != shape or koppen.shape != shape:
        raise ValueError(f"expected node lattices of {shape}, got {elev.shape} and {koppen.shape}")
    step = TILE_DEG * per_deg
    for row in range(TILE_ROWS):
        south = (TILE_ROWS - 1 - row) * step
        e_band = elev[south : south + step + 1][::-1]
        k_band = koppen[south : south + step + 1][::-1]
        for col in range(TILE_COLS):
            cols = np.arange(col * step, col * step + step + 1) % shape[1]
            tile = np.empty((step + 1, step + 1, 2), dtype=np.float16)
            tile[..., 0] = e_band[:, cols]
            tile[..., 1] = k_band[:, cols]
            yield row, col, tile


@traced
def mode_resample(codes: np.ndarray, factor: int) -> np.ndarray:
    """Most frequent non-zero code in each ``factor`` x ``factor`` block of ``codes`` (0 means
    no data). A block of only zeros gives 0; equal counts go to the smaller code."""
    codes = np.asarray(codes)
    h, w = codes.shape
    if factor < 1 or h % factor or w % factor:
        raise ValueError(f"shape {codes.shape} is not a multiple of factor {factor}")
    classes = [int(c) for c in np.unique(codes) if c != 0]
    out = np.zeros((h // factor, w // factor), dtype=codes.dtype)
    if not classes:
        return out
    step = max(1, _CHUNK_ROWS // factor) * factor
    for r0 in range(0, h, step):
        block = codes[r0 : r0 + step].reshape(-1, factor, w // factor, factor)
        best = np.zeros(block.shape[:3:2], dtype=np.int32)
        pick = np.zeros(block.shape[:3:2], dtype=codes.dtype)
        for c in classes:
            count = (block == c).sum(axis=(1, 3), dtype=np.int32)
            better = count > best  # strict: ascending classes keep the smaller code on a tie
            best = np.where(better, count, best)
            pick = np.where(better, np.asarray(c, dtype=codes.dtype), pick)
        out[r0 // factor : (r0 + step) // factor] = pick
    return out


@traced
def mode_resample_nodes(codes: np.ndarray, factor: int) -> np.ndarray:
    """Mode-resample a cell grid onto the lattice nodes at every ``factor``-th cell boundary.

    ``codes`` is south to north, ``h`` x ``w`` cells with longitude wrapping; ``factor`` must be
    even. Node ``(i, j)`` takes the mode of the ``factor`` cells centred on it, so the result is
    ``(h / factor + 1)`` x ``(w / factor)`` and pole rows see only the nearest cells."""
    if factor % 2:
        raise ValueError(f"factor must be even to centre windows on nodes, got {factor}")
    half = factor // 2
    padded = np.pad(codes, ((half, half), (0, 0)), mode="edge")
    padded = np.roll(padded, half, axis=1)
    return mode_resample(padded, factor)


@traced
def cell_to_node_mean(cells: np.ndarray) -> np.ndarray:
    """Move a cell-registered grid (south to north, longitude wrapping) onto the node lattice
    by averaging the four cells around each node; pole rows average their two nearest cells."""
    h, w = cells.shape
    out = np.empty((h + 1, w), dtype=np.float32)
    for i0 in range(0, h + 1, _CHUNK_ROWS):
        idx = np.arange(i0, min(i0 + _CHUNK_ROWS, h + 1))
        band = 0.5 * (cells[np.clip(idx - 1, 0, h - 1)] + cells[np.clip(idx, 0, h - 1)])
        out[idx] = 0.5 * (band + np.roll(band, 1, axis=1))
    return out
