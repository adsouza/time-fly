"""5 degree tiles of ETOPO elevation and Koppen classes: lattice, split, resample, manifest."""

import hashlib
import json

import numpy as np

from timefly_bake import cli, formats, tiles


def test_tile_bounds_cover_the_globe_once():
    lat_edges, lon_edges = set(), set()
    area = 0.0
    for row in range(36):
        for col in range(72):
            lat_n, lat_s, lon_w, lon_e = tiles.tile_bounds(row, col)
            assert lat_n - lat_s == 5 and lon_e - lon_w == 5
            area += (lat_n - lat_s) * (lon_e - lon_w)
            lat_edges.update([lat_n, lat_s])
            lon_edges.update([lon_w, lon_e])
    assert tiles.tile_bounds(0, 0) == (90, 85, -180, -175)
    assert tiles.tile_bounds(35, 71) == (-85, -90, 175, 180)
    assert area == 180 * 360  # no overlap, and (with the edge grids below) no gap
    assert lat_edges == set(range(-90, 91, 5)) and lon_edges == set(range(-180, 181, 5))
    # Consecutive tiles share exactly one edge.
    assert tiles.tile_bounds(3, 4)[1] == tiles.tile_bounds(4, 4)[0]
    assert tiles.tile_bounds(3, 4)[3] == tiles.tile_bounds(3, 5)[2]


def _ramp(per_deg=1):
    """Node lattice, south first: elevation encodes (lat, lon index), koppen cycles 1..30."""
    rows, cols = 180 * per_deg + 1, 360 * per_deg
    i, j = np.mgrid[0:rows, 0:cols]
    elev = (i * 3 + j).astype(np.float32) - 500.0
    kop = ((i + j) % 30 + 1).astype(np.uint8)
    return elev, kop


def test_split_shares_edge_samples():
    elev, kop = _ramp()
    got = {(r, c): t for r, c, t in tiles.split_tiles(elev, kop, per_deg=1)}
    assert len(got) == 36 * 72
    for (r, c), t in got.items():
        assert t.shape == (6, 6, 2) and t.dtype == np.float16
        east = got[(r, (c + 1) % 72)]  # (r, 71) -> (r, 0) crosses the antimeridian
        assert np.array_equal(t[:, -1, :], east[:, 0, :]), (r, c)
        if r < 35:
            south = got[(r + 1, c)]
            assert np.array_equal(t[-1, :, :], south[0, :, :]), (r, c)
    # Row 0 is the northernmost, so its first row is lat 90 N (node row 180).
    first = got[(0, 0)]
    assert first[0, 0, 0] == np.float16(180 * 3 + 0 - 500.0)
    assert first[-1, 0, 0] == np.float16(175 * 3 + 0 - 500.0)
    assert got[(0, 71)][0, -1, 0] == np.float16(180 * 3 + 0 - 500.0)  # lon 180 == lon -180


def test_mode_resample_picks_majority_and_ignores_zero_ties():
    codes = np.array([[5, 5, 3], [0, 0, 0], [0, 0, 0]], dtype=np.uint8)
    assert tiles.mode_resample(codes, 3).tolist() == [[5]]
    assert tiles.mode_resample(np.zeros((3, 3), np.uint8), 3).tolist() == [[0]]
    # Equal counts of land codes: the smaller code wins, deterministically.
    assert tiles.mode_resample(np.array([[7, 2], [2, 7]], np.uint8), 2).tolist() == [[2]]
    two_blocks = np.array([[1, 1, 9, 0], [1, 0, 9, 9]], np.uint8)
    assert tiles.mode_resample(two_blocks, 2).tolist() == [[1, 9]]


def test_mode_resample_nodes_centres_windows_on_lattice_nodes():
    # Four 30 arc-second-style cells wide, two rows: nodes sit between cells, wrapping in lon.
    codes = np.array([[1, 2, 3, 4], [1, 2, 3, 4]], dtype=np.uint8)
    nodes = tiles.mode_resample_nodes(codes, 2)
    assert nodes.shape == (2 // 2 + 1, 4 // 2)
    # Node column 0 sits between cell 3 (wrapped) and cell 0: tie of {4, 1} -> 1.
    # Node column 1 sits between cells 1 and 2: tie of {2, 3} -> 2.
    assert nodes.tolist() == [[1, 2], [1, 2]]


def test_cell_to_node_mean_averages_the_four_cells_with_wrap():
    cells = np.array([[0, 4, 8, 12], [2, 6, 10, 14]], dtype=np.float32)
    nodes = tiles.cell_to_node_mean(cells)
    assert nodes.shape == (3, 4)
    # Interior node (row 1) between cell columns 0 and 1: mean of 0, 4, 2, 6.
    assert nodes[1, 1] == 3.0
    # Antimeridian node (column 0): cells 3 and 0.
    assert nodes[1, 0] == np.mean([12, 0, 14, 2])
    # Pole rows only have the nearest cell row.
    assert nodes[0, 1] == 2.0 and nodes[2, 1] == 4.0


def test_elevation_is_exact_in_f16_range():
    rows, cols = 181, 360
    elev = np.linspace(-10800.0, 8800.0, rows * cols, dtype=np.float32).reshape(rows, cols)
    kop = np.zeros((rows, cols), np.uint8)
    worst = 0.0
    for r, c, t in tiles.split_tiles(elev, kop, per_deg=1):
        south = 180 - 5 * (r + 1)
        want = np.take(elev[south : south + 6][::-1], np.arange(5 * c, 5 * c + 6), axis=1, mode="wrap")
        worst = max(worst, float(np.abs(t[..., 0].astype(np.float64) - want).max()))
    assert worst <= 8.0


def test_manifest_lists_every_tile_with_hashes(tmp_path):
    elev, kop = _ramp()
    manifest = cli.write_tiles(elev, kop, tmp_path, {"etopo": "e" * 64, "koppen": "k" * 64}, per_deg=1)
    on_disk = json.loads((tmp_path / "manifest.json").read_text())
    assert on_disk == manifest
    assert manifest["version"] == 1 and manifest["tile_deg"] == 5 and manifest["samples"] == 6
    assert manifest["sources"] == {"etopo": "e" * 64, "koppen": "k" * 64}
    assert [(t["row"], t["col"]) for t in manifest["tiles"]] == [
        (r, c) for r in range(36) for c in range(72)
    ]
    for t in manifest["tiles"]:
        path = tmp_path / f"r{t['row']:02}c{t['col']:02}.tfgrid"
        blob = path.read_bytes()
        assert t["bytes"] == len(blob) and t["sha256"] == hashlib.sha256(blob).hexdigest()
    header, data = formats.read_tfgrid(tmp_path / "r00c00.tfgrid")
    assert header["kind"] == "latlon" and header["dtype"] == "f16" and header["channels"] == 2
    assert data.shape == (6, 6, 2)


def test_overview_shape_and_edges():
    elev, kop = _ramp(per_deg=5)
    ov = tiles.overview(elev, kop, per_deg=5, factor=5)
    # 1 degree nodes, both edges: 181 rows (90 N first) by 361 columns (-180 .. +180).
    assert ov.shape == (181, 361, 2) and ov.dtype == np.float16
    # The -180 column is the +180 column.
    assert np.array_equal(ov[:, 0, :], ov[:, -1, :])
    assert tiles.OVERVIEW_ROWS == 2161 and tiles.OVERVIEW_COLS == 4321
    assert tiles.OVERVIEW_FACTOR * 12 == tiles.PER_DEG


def test_overview_is_the_block_mean_and_mode():
    per_deg, factor = 5, 5
    elev, kop = _ramp(per_deg=per_deg)
    rng = np.random.default_rng(3)
    kop = rng.integers(0, 31, size=kop.shape).astype(np.uint8)
    ov = tiles.overview(elev, kop, per_deg=per_deg, factor=factor)
    rows, cols = elev.shape

    def block(a, i, j):
        r = np.clip(np.arange(i * factor - 2, i * factor + 3), 0, rows - 1)
        c = np.arange(j * factor - 2, j * factor + 3) % cols
        return a[np.ix_(r, c)]

    for i, j in [(0, 0), (1, 1), (90, 200), (180, 359), (37, 0), (180, 0), (5, 360)]:
        out_row = ov.shape[0] - 1 - i  # output is north first; node row i counts from the south
        want = np.float16(block(elev, i, j % 360).mean(dtype=np.float64))
        assert ov[out_row, j, 0] == want, (i, j)
        assert ov[out_row, j, 1] == np.float16(tiles.mode_resample(block(kop, i, j % 360), factor)[0, 0])
    # A ramp's block mean away from the poles is the ramp at the block's centre node.
    assert ov[ov.shape[0] - 1 - 90, 200, 0] == np.float16(90 * 5 * 3 + 200 * 5 - 500.0)


def test_manifest_lists_the_overview(tmp_path):
    elev, kop = _ramp()
    manifest = cli.write_tiles(elev, kop, tmp_path, {"etopo": "e" * 64, "koppen": "k" * 64}, per_deg=1)
    blob = (tmp_path / "overview.tfgrid").read_bytes()
    assert manifest["overview"] == {"bytes": len(blob), "sha256": hashlib.sha256(blob).hexdigest()}
    header, data = formats.read_tfgrid(tmp_path / "overview.tfgrid")
    assert header["kind"] == "latlon" and header["channels"] == 2
    assert data.shape == (180 // 5 + 1, 360 // 5 + 1, 2)
