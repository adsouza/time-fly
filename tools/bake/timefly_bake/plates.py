"""Plate footprints and rotations from a GPlates plate model (pygplates).

Footprints record, for every cube-map cell, each plate whose present-day (plate-fixed) polygon
contains the cell centre, with the interval over which that polygon is valid. Rotations are
total rotations relative to anchor plate 0, sampled at fixed times, with missing circuits kept
as explicit invalid flags rather than replaced by identity.

Rotation quaternions follow ``timefly_core::geo``: a geo-frame quaternion ``(x, y, z, w)`` becomes
``(y, z, x, w)`` in the render frame, and they rotate a plate-fixed direction into the era frame.
"""

from __future__ import annotations

import logging
import math
from dataclasses import dataclass

import numpy as np
import pygplates

from timefly_bake import cube, geo
from timefly_bake.log import traced

MAX_AGE_MYR = 540.0
ANCHOR_PLATE_ID = 0

_log = logging.getLogger(__name__)
# Margin added to a polygon's bounding cap so boundary points are never pruned by rounding.
_CAP_MARGIN_RAD = 1e-6


@dataclass(frozen=True)
class PlateFootprints:
    """Cell memberships in compressed-row form, keyed by source plate ID.

    Cell ``c`` (``[face][row][col]`` order) owns entries ``offsets[c]:offsets[c + 1]``, sorted by
    plate ID and then by younger bound. Intervals are closed, in Ma, within ``0..=540``.
    """

    cube_size: int
    offsets: np.ndarray  # uint32, 6*n*n + 1
    plate_ids: np.ndarray  # uint32, one per entry
    younger_myr: np.ndarray  # float64
    older_myr: np.ndarray  # float64

    def cell(self, face: int, row: int, col: int) -> list[tuple[int, float, float]]:
        """The ``(plate_id, younger_myr, older_myr)`` memberships of one cell."""
        n = self.cube_size
        c = (face * n + row) * n + col
        a, b = int(self.offsets[c]), int(self.offsets[c + 1])
        return [
            (int(p), float(y), float(o))
            for p, y, o in zip(self.plate_ids[a:b], self.younger_myr[a:b], self.older_myr[a:b])
        ]


def _clipped_interval(begin, end) -> tuple[float, float] | None:
    """A feature's valid time ``(begin, end)`` clipped to the modelled range, or ``None``."""
    younger = max(float(end), 0.0)
    older = min(float(begin), MAX_AGE_MYR)
    return (younger, older) if younger <= older else None


def _bounding_cap(polygon) -> tuple[np.ndarray, float] | None:
    """Centre and cosine of a cap containing the polygon, or ``None`` if no small cap does.

    A cap narrower than a hemisphere is convex, so it contains every edge between vertices it
    contains. The antipode check guards against a polygon whose interior is the large side.
    """
    pts = np.array([p.to_xyz() for p in polygon.get_exterior_ring_points()], dtype=np.float64)
    centre = pts.mean(axis=0)
    norm = np.linalg.norm(centre)
    if norm < 1e-9:
        return None
    centre /= norm
    radius = float(np.arccos(np.clip(pts @ centre, -1.0, 1.0)).max()) + _CAP_MARGIN_RAD
    if radius >= math.pi / 2 - 1e-3:
        return None
    if polygon.is_point_in_polygon(tuple(-centre)):
        return None
    return centre, math.cos(radius)


def _union(intervals: list[tuple[float, float]]) -> list[tuple[float, float]]:
    """Union of closed intervals, ascending; touching intervals merge."""
    out: list[list[float]] = []
    for younger, older in sorted(intervals):
        if out and younger <= out[-1][1]:
            out[-1][1] = max(out[-1][1], older)
        else:
            out.append([younger, older])
    return [(a, b) for a, b in out]


@traced
def assign_plates(dirs_render, polygons_path) -> PlateFootprints:
    """Footprints of every polygon in ``polygons_path`` over the cube texel centres ``dirs_render``.

    ``dirs_render`` has shape ``(6, n, n, 3)`` (see ``cube.texel_center_dirs``). Each containing
    polygon contributes its source plate ID and valid interval to the cell. Intervals of one
    plate in one cell are unioned where they overlap and kept apart where they are disjoint;
    different plates in one cell are all kept.
    """
    dirs = np.asarray(dirs_render, dtype=np.float64)
    if dirs.ndim != 4 or dirs.shape[0] != 6 or dirs.shape[1] != dirs.shape[2] or dirs.shape[3] != 3:
        raise ValueError(f"dirs_render must have shape (6, n, n, 3), got {dirs.shape}")
    n = dirs.shape[1]
    flat = geo.render_to_geo(dirs).reshape(-1, 3)
    members: list[dict[int, list[tuple[float, float]]]] = [{} for _ in range(len(flat))]

    n_polygons = n_skipped = n_tests = 0
    for feature in pygplates.FeatureCollection(str(polygons_path)):
        interval = _clipped_interval(*feature.get_valid_time())
        plate_id = int(feature.get_reconstruction_plate_id())
        for polygon in feature.get_geometries():
            if not isinstance(polygon, pygplates.PolygonOnSphere):
                n_skipped += 1
                continue
            n_polygons += 1
            if interval is None:
                continue
            cap = _bounding_cap(polygon)
            if cap is None:
                candidates = np.arange(len(flat))
            else:
                centre, cos_radius = cap
                candidates = np.flatnonzero(flat @ centre >= cos_radius)
            n_tests += len(candidates)
            for i in candidates:
                if polygon.is_point_in_polygon(tuple(flat[i])):
                    members[i].setdefault(plate_id, []).append(interval)

    offsets = np.zeros(len(flat) + 1, dtype=np.uint32)
    ids: list[int] = []
    younger: list[float] = []
    older: list[float] = []
    for c, cell in enumerate(members):
        for plate_id in sorted(cell):
            for a, b in _union(cell[plate_id]):
                ids.append(plate_id)
                younger.append(a)
                older.append(b)
        offsets[c + 1] = len(ids)

    covered = int(np.count_nonzero(np.diff(offsets.astype(np.int64))))
    _log.info(
        "footprints: %d polygons (%d non-polygon geometries skipped), %d point tests, "
        "%d of %d cells covered, %d entries",
        n_polygons,
        n_skipped,
        n_tests,
        covered,
        len(flat),
        len(ids),
    )
    return PlateFootprints(
        cube_size=n,
        offsets=offsets,
        plate_ids=np.asarray(ids, dtype=np.uint32),
        younger_myr=np.asarray(younger, dtype=np.float64),
        older_myr=np.asarray(older, dtype=np.float64),
    )


@traced
def default_dilate_cells(cube_size: int) -> int:
    """The default footprint dilation: 2 cells at cube size 128, scaled to keep the same angular
    reach (0 for small test cubes)."""
    return int(round(2.0 * cube_size / 128.0))


@traced
def cube_neighbours(n: int) -> np.ndarray:
    """The 8 neighbours of every cell, shape ``(6*n*n, 8)``, in ``[face][row][col]`` order.

    A neighbour off the edge of a face is the cell containing the neighbour's texel centre
    extended past the edge onto the cube surface (``cube.dirs_to_texels``, the Task 3 tie rule),
    so neighbourhoods continue across faces.
    """
    s = 2.0 * (np.arange(n) + 0.5) / n - 1.0
    step = 2.0 / n
    sc, tc = np.meshgrid(s, s)  # [row, col]
    out = np.empty((6, n, n, 8), dtype=np.int64)
    offsets = [(dr, dc) for dr in (-1, 0, 1) for dc in (-1, 0, 1) if (dr, dc) != (0, 0)]
    for face in range(6):
        for k, (dr, dc) in enumerate(offsets):
            a, b = sc + dc * step, tc + dr * step
            one = np.ones_like(a)
            point = [
                (one, -b, -a),
                (-one, -b, a),
                (a, one, b),
                (a, -one, -b),
                (a, -b, one),
                (-a, -b, -one),
            ][face]
            f, col, row = cube.dirs_to_texels(np.stack(point, axis=-1), n)
            out[face, :, :, k] = (f * n + row) * n + col
    return out.reshape(6 * n * n, 8)


@traced
def dilate_footprints(footprints: PlateFootprints, cells: int) -> PlateFootprints:
    """Closes plate-boundary cracks: every unowned cell within ``cells`` steps (8-connected,
    across faces) of an owned cell joins the nearest plate by breadth-first distance, ties going
    to the lowest source plate ID (then the lowest source cell), and inherits that source cell's
    interval list for that plate. Owned cells never change; ``cells <= 0`` returns the input.
    """
    if cells <= 0:
        return footprints
    fp = footprints
    n = fp.cube_size
    total = 6 * n * n
    neighbours = cube_neighbours(n)
    owned = np.diff(fp.offsets.astype(np.int64)) > 0
    # cell -> (plate_id, source cell); seeds are the lowest plate of each owned cell.
    best: dict[int, tuple[int, int]] = {}
    for c in np.flatnonzero(owned):
        best[int(c)] = (int(fp.plate_ids[fp.offsets[c]]), int(c))
    claimed: dict[int, tuple[int, int]] = {}
    frontier = sorted(best)
    for _ in range(cells):
        offers: dict[int, tuple[int, int]] = {}
        for c in frontier:
            label = best[c] if c in best else claimed[c]
            for m in neighbours[c]:
                m = int(m)
                if owned[m] or m in claimed:
                    continue
                if m not in offers or label < offers[m]:
                    offers[m] = label
        claimed.update(offers)
        frontier = sorted(offers)
        if not frontier:
            break

    offsets = np.zeros(total + 1, dtype=np.uint32)
    ids: list[int] = []
    younger: list[float] = []
    older: list[float] = []
    for c in range(total):
        a, b = int(fp.offsets[c]), int(fp.offsets[c + 1])
        if c in claimed:
            plate, source = claimed[c]
            a, b = int(fp.offsets[source]), int(fp.offsets[source + 1])
            keep = [k for k in range(a, b) if int(fp.plate_ids[k]) == plate]
        else:
            keep = list(range(a, b))
        for k in keep:
            ids.append(int(fp.plate_ids[k]))
            younger.append(float(fp.younger_myr[k]))
            older.append(float(fp.older_myr[k]))
        offsets[c + 1] = len(ids)
    _log.info("footprint dilation: %d cells, %d cracks/margin cells filled", cells, len(claimed))
    return PlateFootprints(
        cube_size=n,
        offsets=offsets,
        plate_ids=np.asarray(ids, dtype=np.uint32),
        younger_myr=np.asarray(younger, dtype=np.float64),
        older_myr=np.asarray(older, dtype=np.float64),
    )


@traced
def footprint_entries(footprints: PlateFootprints, rotation_plate_ids):
    """``(plate_index, younger_myr, older_myr)`` triples for ``formats.write_tfplates``.

    ``plate_index`` is the position of the source ID in ``rotation_plate_ids`` (the rotation
    table's plate order). Entries keep ``footprints.offsets`` and are re-sorted within each cell
    by plate index, then younger bound. Raises ``ValueError`` naming any footprint plate missing
    from the rotation table.
    """
    index = {int(p): i for i, p in enumerate(rotation_plate_ids)}
    missing = sorted({int(p) for p in footprints.plate_ids} - index.keys())
    if missing:
        raise ValueError(f"footprint plates missing from the rotation table: {missing}")
    fp = footprints
    entries = []
    for c in range(len(fp.offsets) - 1):
        ks = range(int(fp.offsets[c]), int(fp.offsets[c + 1]))
        cell = [
            (index[int(fp.plate_ids[k])], float(fp.younger_myr[k]), float(fp.older_myr[k]))
            for k in ks
        ]
        entries.extend(sorted(cell))
    return entries


def _sequences(rotation_path) -> list[tuple[int, int, list[float]]]:
    """``(moving, fixed, enabled sample times)`` for every rotation sequence in the file."""
    out = []
    for feature in pygplates.FeatureCollection(str(rotation_path)):
        pole = feature.get_total_reconstruction_pole()
        if pole is None:
            continue
        fixed, moving, sampling = pole
        times = [float(s.get_time()) for s in sampling.get_enabled_time_samples()]
        if times:
            out.append((int(moving), int(fixed), times))
    return out


def _rotation_model(rotation_path):
    if not any(fixed == ANCHOR_PLATE_ID for _, fixed, _ in _sequences(rotation_path)):
        raise ValueError(
            f"{rotation_path}: no rotation sequence is fixed to anchor plate {ANCHOR_PLATE_ID}"
        )
    return pygplates.RotationModel(str(rotation_path), default_anchor_plate_id=ANCHOR_PLATE_ID)


def _rotation(model, plate_id: int, time: float):
    return model.get_rotation(
        float(time),
        int(plate_id),
        anchor_plate_id=ANCHOR_PLATE_ID,
        use_identity_for_missing_plate_ids=False,
    )


def _render_quat(rotation) -> tuple[float, float, float, float]:
    """Render-frame ``(x, y, z, w)`` of a pygplates rotation, with ``w >= 0``."""
    pole, angle = rotation.get_euler_pole_and_angle()
    gx, gy, gz = pole.to_xyz()
    s, w = math.sin(angle / 2), math.cos(angle / 2)
    q = (gy * s, gz * s, gx * s, w)
    return tuple(-v for v in q) if w < 0 else q


@traced
def rotation_times(rotation_path, plate_ids, source_ages_myr) -> np.ndarray:
    """Ascending sample times (Ma) for ``export_rotations``.

    The 1 Myr samples from 0 to 540 Ma, the source reconstruction ages and every rotation
    sequence's first and last sample within that range. Between two consecutive such times no
    sequence starts or ends, so each plate's validity is constant there; wherever a plate is
    valid at both ends but missing in between, the midpoint is added so the gap survives
    interpolation.
    """
    times = set(np.arange(0.0, MAX_AGE_MYR + 1.0).tolist())
    times |= {float(a) for a in source_ages_myr if 0.0 <= float(a) <= MAX_AGE_MYR}
    for _, _, seq_times in _sequences(rotation_path):
        for t in (min(seq_times), max(seq_times)):
            if 0.0 <= t <= MAX_AGE_MYR:
                times.add(t)
    ordered = sorted(times)

    model = _rotation_model(rotation_path)
    ids = [int(p) for p in plate_ids]

    def valid(t: float) -> np.ndarray:
        return np.array([_rotation(model, p, t) is not None for p in ids], dtype=bool)

    mids = []
    here = valid(ordered[0])
    for a, b in zip(ordered, ordered[1:]):
        there = valid(b)
        both = here & there
        if both.any():
            mid = (a + b) / 2
            if np.any(both & ~valid(mid)):
                mids.append(mid)
        here = there
    _log.info(
        "rotation times: %d samples (%d missing-span midpoints)", len(ordered) + len(mids), len(mids)
    )
    return np.asarray(sorted(ordered + mids), dtype=np.float64)


@traced
def export_rotations(rotation_path, plate_ids, times_myr):
    """Total rotations of ``plate_ids`` at ``times_myr`` relative to anchor plate 0.

    Returns ``(quats, valid)``: ``quats`` is float32 ``(n_times, n_plates, 4)`` render-frame
    ``x y z w`` and ``valid`` is uint8 ``(n_times, n_plates)``. A plate pygplates cannot reach
    from the anchor at a time is invalid there (its quaternion is identity and must not be used);
    a valid identity is a genuine zero rotation.
    """
    model = _rotation_model(rotation_path)
    ids = [int(p) for p in plate_ids]
    times = [float(t) for t in times_myr]
    quats = np.zeros((len(times), len(ids), 4), dtype=np.float32)
    quats[..., 3] = 1.0
    valid = np.zeros((len(times), len(ids)), dtype=np.uint8)
    for ti, t in enumerate(times):
        for pi, plate_id in enumerate(ids):
            rotation = _rotation(model, plate_id, t)
            if rotation is not None:
                quats[ti, pi] = _render_quat(rotation)
                valid[ti, pi] = 1
    _log.info(
        "rotations: %d plates x %d times, %d of %d samples valid",
        len(ids),
        len(times),
        int(valid.sum()),
        valid.size,
    )
    return quats, valid
