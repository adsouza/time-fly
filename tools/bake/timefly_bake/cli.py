"""``timefly-bake``: bake the globe asset set from the raw downloads.

Reads the raw tree as downloaded (``data/raw``; archives are parsed in place, nothing is run)::

    paleodem/*.zip          PaleoDEM netCDFs (one archive)
    plates/*.zip            one .rot rotation file and one .gpml polygon file
    ice6g/I6_C.VM5a_1deg.<k>.nc[.gz]   ICE-6G_C slices, k = 0, 1, ... ka
    sealevel/spratt2016-noaa.txt

plus the reviewed PaleoDEM age table (``--paleodem-ages``; by default ``<raw>/paleodem_ages.csv``
or else ``<raw>/../paleodem_ages.csv``), and writes to ``--out``::

    manifest.json           version 2: frames, footprints, rotations, sea level, attribution
    frames/fNNNN.<sha256>.tfgrid   geographic cube images in order of first use
    plates.<sha256>.tfplates       plate footprints
    rotations.<sha256>.tfrot       plate rotations and validity
    bake.log                DEBUG log of the run

Timeline: the youngest ICE-6G_C image at age 0, then one frame per slice at ``(k ka + 76 yr)``,
then the PaleoDEMs from the present-day bridge frame to 540 Ma.
"""

from __future__ import annotations

import argparse
import errno
import hashlib
import json
import logging
import math
import re
import shutil
import sys
import tempfile
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath

import numpy as np

from timefly_bake import ages, beyer, cube, etopo, formats, ice6g, koppen, log, paleodem, plates
from timefly_bake import sealevel, tiles
from timefly_bake.log import traced

MANIFEST_VERSION = 2
MANIFEST_NAME = "manifest.json"
FRAMES_DIR = "frames"
FOOTPRINTS_NAME = "plates.tfplates"
ROTATIONS_NAME = "rotations.tfrot"
SEA_LEVEL_NAME = "spratt2016-noaa.txt"
AGES_NAME = "paleodem_ages.csv"
ATTRIBUTION = [
    "Paleotopography: Scotese, C.R. & Wright, N. (2018). PALEOMAP Paleodigital Elevation Models "
    "(PaleoDEMS) for the Phanerozoic. PALEOMAP Project. doi:10.5281/zenodo.5460860 (CC BY 4.0).",
    "Plate model: PALEOMAP Global Plate Model rotations and continental polygons, Scotese, C.R. & "
    "Wright, N. (2018). doi:10.5281/zenodo.5460860 (CC BY 4.0).",
    "Ice sheets and recent topography: ICE-6G_C (VM5a), W.R. Peltier, University of Toronto. "
    "Argus, D.F., Peltier, W.R., Drummond, R. & Moore, A.W. (2014), Geophys. J. Int. 198, 537-563, "
    "doi:10.1093/gji/ggu140; Peltier, W.R., Argus, D.F. & Drummond, R. (2015), J. Geophys. Res. "
    "Solid Earth 120, 450-487, doi:10.1002/2014JB011176.",
    "Sea level: Spratt, R.M. & Lisiecki, L.E. (2016). A Late Pleistocene sea level stack. Climate "
    "of the Past 12, 1079-1092, doi:10.5194/cp-12-1079-2016; data from NOAA NCEI, "
    "doi:10.25921/rd66-5820.",
]

_log = logging.getLogger(__name__)
_MAX_MEMBER_BYTES = 512 * 1024 * 1024
_STEP_TOL = 1e-6


@dataclass(frozen=True)
class _Image:
    source: str  # "ice6g" or "paleodem"
    path: Path
    label: str
    ka: int | None = None
    source_age_myr: float | None = None


@dataclass(frozen=True)
class _Frame:
    age_myr: float
    image: int
    source: str
    source_rotation_age_myr: float | None = None


@traced
def select_paleodem_frames(frames, step_myr: float | None):
    """The PaleoDEM frames to bake: all of them when ``step_myr`` is ``None``, else those whose
    source reconstruction age is a multiple of ``step_myr``, always keeping the youngest
    (present-day bridge) and oldest (540 Ma) frames."""
    frames = list(frames)
    if step_myr is None:
        return frames
    if not (step_myr > 0 and math.isfinite(step_myr)):
        raise ValueError(f"PaleoDEM step must be a positive number of Myr, got {step_myr}")
    kept = []
    for i, f in enumerate(frames):
        ratio = f.source_rotation_age_myr / step_myr
        if i == 0 or i == len(frames) - 1 or abs(ratio - round(ratio)) < _STEP_TOL:
            kept.append(f)
    return kept


def _parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="timefly-bake", description=__doc__.split("\n\n")[0])
    p.add_argument("--raw", type=Path, required=True, help="raw download tree (data/raw)")
    p.add_argument("--out", type=Path, required=True, help="output directory")
    p.add_argument("--cube-size", type=int, default=128, help="cube face size in texels")
    p.add_argument("--log-level", default="INFO", help="stderr log level (bake.log is DEBUG)")
    p.add_argument(
        "--paleodem-ages",
        type=Path,
        help=f"PaleoDEM age table (default: <raw>/{AGES_NAME}, else <raw>/../{AGES_NAME})",
    )
    p.add_argument(
        "--footprint-dilate-cells",
        type=int,
        help="grow plate footprints into unowned cells within this many cells, closing "
        "boundary cracks (default: 2 at cube size 128, scaled with the cube size; 0 disables)",
    )
    p.add_argument(
        "--paleodem-step-myr",
        type=float,
        help="bake only PaleoDEMs whose reconstruction age is a multiple of this (plus the "
        "present-day bridge and 540 Ma frames); default: all",
    )
    return p


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _extract(archive: Path, suffixes: tuple[str, ...], dest: Path) -> list[Path]:
    """Copy the members of ``archive`` ending in ``suffixes`` into ``dest`` under their base
    names (never their archive paths), refusing duplicates and oversized members."""
    dest.mkdir(parents=True, exist_ok=True)
    out = []
    with zipfile.ZipFile(archive) as zf:
        for info in zf.infolist():
            name = PurePosixPath(info.filename.replace("\\", "/")).name
            if info.is_dir() or not name.endswith(suffixes) or name.startswith("."):
                continue
            if info.file_size > _MAX_MEMBER_BYTES:
                raise ValueError(f"{archive.name}: member {name} is {info.file_size} bytes")
            target = dest / name
            if target.exists():
                raise ValueError(f"{archive.name}: more than one member named {name}")
            with zf.open(info) as src, target.open("wb") as dst:
                shutil.copyfileobj(src, dst)
            out.append(target)
    return out


def _one(paths: list[Path], what: str) -> Path:
    if len(paths) != 1:
        raise ValueError(f"expected exactly one {what}, found {[p.name for p in paths]}")
    return paths[0]


def _ages_path(raw: Path, given: Path | None) -> Path:
    if given is not None:
        return given
    for candidate in (raw / AGES_NAME, raw.parent / AGES_NAME):
        if candidate.is_file():
            return candidate
    raise FileNotFoundError(f"no {AGES_NAME} in {raw} or its parent; pass --paleodem-ages")


def _check_inputs(raw: Path, ages_path: Path) -> list[str]:
    problems = []
    for sub in ("paleodem", "plates", "ice6g", "sealevel"):
        if not (raw / sub).is_dir():
            problems.append(f"missing directory {raw / sub}")
    if not (raw / "sealevel" / SEA_LEVEL_NAME).is_file():
        problems.append(f"missing {raw / 'sealevel' / SEA_LEVEL_NAME}")
    if not ages_path.is_file():
        problems.append(f"missing {ages_path}")
    return problems


def _write_images(out: Path, images: list[_Image], dirs, coverage=None) -> list[str]:
    """Writes every image; ``coverage(age_myr)`` gives a PaleoDEM's plate-coverage mask
    (channel G) at its source reconstruction age."""
    frames_dir = out / FRAMES_DIR
    frames_dir.mkdir(parents=True, exist_ok=True)
    names = []
    total = 0
    for i, image in enumerate(images):
        rel = f"{FRAMES_DIR}/f{i:04d}.tfgrid"
        if image.source == "ice6g":
            data = ice6g.bake_ice_frame(dirs, *ice6g.load_ice6g(image.path, expect_ka=image.ka))
        else:
            data = paleodem.bake_paleo_frame(dirs, paleodem.load_paleodem(image.path))
            if coverage is not None:
                data[..., 1] = coverage(image.source_age_myr)
        formats.write_tfgrid(out / rel, "cube", data)
        size = (out / rel).stat().st_size
        total += size
        names.append(rel)
        _log.info(
            "image %d/%d: %s %s -> %s (%.0f KB)",
            i + 1,
            len(images),
            image.source,
            image.label,
            rel,
            size / 1024,
        )
    _log.info("images: %d files, %.2f MB", len(images), total / 1e6)
    return names


def _write_plates(
    out: Path, dirs, rot_path: Path, gpml_path: Path, source_ages, dilate_cells: int
):
    """Writes footprints and rotations; returns a ``coverage(age_myr)`` mask function."""
    fp = plates.dilate_footprints(plates.assign_plates(dirs, gpml_path), dilate_cells)
    plate_ids = sorted({int(p) for p in fp.plate_ids})
    times = plates.rotation_times(rot_path, plate_ids, source_ages)
    if times[0] != 0.0 or times[-1] != plates.MAX_AGE_MYR:
        raise ValueError(f"rotation times span {times[0]}..{times[-1]}, not 0..540")
    missing = sorted(set(float(a) for a in source_ages) - set(times.tolist()))
    if missing:
        raise ValueError(f"rotation table lacks source reconstruction ages {missing}")
    quats, valid = plates.export_rotations(rot_path, plate_ids, times)
    formats.write_tfrot(out / ROTATIONS_NAME, plate_ids, times, quats, valid)
    entries = plates.footprint_entries(fp, plate_ids)
    formats.write_tfplates(out / FOOTPRINTS_NAME, fp.cube_size, fp.offsets, entries)

    per_cell = np.diff(fp.offsets.astype(np.int64))
    _log.info(
        "footprints: %d of %d cells covered, %d with more than one entry, %d entries, %d plates",
        int(np.count_nonzero(per_cell)),
        per_cell.size,
        int(np.count_nonzero(per_cell > 1)),
        len(entries),
        len(plate_ids),
    )
    ok = valid.astype(bool)
    ever = ok.any(axis=0)
    never = [p for p, e in zip(plate_ids, ever) if not e]
    at_sources = ok[np.isin(times, np.asarray(source_ages, dtype=np.float64))]
    gaps = int(np.count_nonzero(~at_sources[:, ever])) if ever.any() else 0
    _log.info(
        "rotations: %d plates x %d times (0..%g Ma), %.2f%% of samples valid; never valid: %s; "
        "%d invalid plate samples at %d source ages among plates valid elsewhere",
        len(plate_ids),
        len(times),
        times[-1],
        100.0 * float(ok.mean()) if ok.size else 100.0,
        never or "none",
        gaps,
        len(at_sources),
    )

    def coverage(age_myr):
        return plates.coverage_mask(dirs, fp, plate_ids, times, quats, valid, age_myr)

    return coverage


def _manifest_json(manifest: dict) -> str:
    """Indented JSON with each ``[age, value]`` pair of the sea-level curve on one line."""
    text = json.dumps(manifest, indent=2)
    number = r"-?[0-9][0-9.eE+-]*"
    return re.sub(rf"\[\s*({number}),\s*({number})\s*\]", r"[\1, \2]", text) + "\n"


@traced
def bake(
    raw,
    out,
    cube_size: int = 128,
    ages_path=None,
    paleodem_step_myr=None,
    footprint_dilate_cells=None,
) -> dict:
    """Bake everything from ``raw`` into ``out`` and return the manifest written there.

    Immutable payload names let readers finish loading an older manifest while a new
    generation is published. Only replacement of manifest.json changes the active bake.
    """
    raw, out = Path(raw), Path(out)
    if cube_size < 1:
        raise ValueError(f"cube size must be positive, got {cube_size}")
    ages_path = _ages_path(raw, None if ages_path is None else Path(ages_path))
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".bake-", dir=out) as tmp:
        stage = Path(tmp)
        manifest = _bake_staged(
            raw, stage, cube_size, ages_path, paleodem_step_myr, footprint_dilate_cells
        )
        payloads = {frame["file"] for frame in manifest["frames"]}
        payloads.update([manifest["footprints"], manifest["rotations"]])
        names = {}
        for rel in sorted(payloads):
            src = stage / rel
            name = Path(rel).with_suffix(f".{_sha256(src)}{src.suffix}").as_posix()
            dest = out / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            src.replace(dest)
            names[rel] = name
        for frame in manifest["frames"]:
            frame["file"] = names[frame["file"]]
        for key in ("footprints", "rotations"):
            manifest[key] = names[manifest[key]]
        staged_manifest = stage / MANIFEST_NAME
        staged_manifest.write_text(_manifest_json(manifest), encoding="utf-8")
        staged_manifest.replace(out / MANIFEST_NAME)
        return manifest


def _bake_staged(raw, out, cube_size, ages_path, paleodem_step_myr, footprint_dilate_cells):
    started = time.perf_counter()
    paleo_zip = _one(sorted((raw / "paleodem").glob("*.zip")), "PaleoDEM archive")
    plate_zips = sorted((raw / "plates").glob("*.zip"))
    sea_path = raw / "sealevel" / SEA_LEVEL_NAME
    slices = ice6g.list_ice6g_slices(raw / "ice6g")
    for p in [paleo_zip, *plate_zips, sea_path, ages_path] + [p for _, p in slices]:
        _log.debug("input %s sha256 %s", p.name, _sha256(p))

    sea_age, sea_m = sealevel.load_spratt2016(sea_path)

    with tempfile.TemporaryDirectory(prefix="timefly-bake-") as tmp:
        tmp = Path(tmp)
        _extract(paleo_zip, (".nc",), tmp / "paleodem")
        plate_files = []
        for z in plate_zips:
            plate_files += _extract(z, (".rot", ".gpml"), tmp / "plates")
        rot_path = _one([p for p in plate_files if p.suffix == ".rot"], "rotation file (.rot)")
        gpml_path = _one([p for p in plate_files if p.suffix == ".gpml"], "polygon file (.gpml)")

        all_paleo = paleodem.list_paleodem_files(
            tmp / "paleodem", paleodem.load_age_mapping(ages_path)
        )
        paleo = select_paleodem_frames(all_paleo, paleodem_step_myr)

        images: list[_Image] = []
        frames: list[_Frame] = []
        for k, path in slices:
            images.append(_Image("ice6g", path, f"{k} ka", ka=k))
        frames.append(_Frame(0.0, 0, "ice6g"))
        for i, (k, _) in enumerate(slices):
            frames.append(_Frame(ages.myr_from_1950_bp(k * 1000.0), i, "ice6g"))
        for f in paleo:
            images.append(
                _Image(
                    "paleodem",
                    f.path,
                    f"{f.path.name} (timeline {f.timeline_age_myr:g} Ma, "
                    f"reconstruction {f.source_rotation_age_myr:g} Ma)",
                    source_age_myr=f.source_rotation_age_myr,
                )
            )
            frames.append(
                _Frame(f.timeline_age_myr, len(images) - 1, "paleodem", f.source_rotation_age_myr)
            )
        frame_ages = [f.age_myr for f in frames]
        if any(b <= a for a, b in zip(frame_ages, frame_ages[1:])):
            raise ValueError(f"frame ages are not strictly ascending: {frame_ages}")
        bridge = paleo[0].timeline_age_myr
        if sea_age[-1] < bridge:
            raise ValueError(
                f"sea-level curve ends at {sea_age[-1]:g} Myr, before the bridge at {bridge:g} Myr"
            )
        _log.info(
            "timeline: %d frames (%d ICE-6G_C slices 0-%d ka, %d of %d PaleoDEMs), "
            "recent frames end at %g Myr, bridge at %g Myr",
            len(frames),
            len(slices),
            slices[-1][0],
            len(paleo),
            len(all_paleo),
            frames[len(slices)].age_myr,
            bridge,
        )

        dirs = cube.texel_center_dirs(cube_size)
        dilate = (
            plates.default_dilate_cells(cube_size)
            if footprint_dilate_cells is None
            else footprint_dilate_cells
        )
        coverage = _write_plates(
            out,
            dirs,
            rot_path,
            gpml_path,
            [f.source_rotation_age_myr for f in all_paleo],
            dilate,
        )
        files = _write_images(out, images, dirs, coverage)

    manifest = {
        "version": MANIFEST_VERSION,
        "cube_size": cube_size,
        "frames": [
            {"age_myr": f.age_myr, "file": files[f.image], "source": f.source}
            | (
                {}
                if f.source_rotation_age_myr is None
                else {"source_rotation_age_myr": f.source_rotation_age_myr}
            )
            for f in frames
        ],
        "footprints": FOOTPRINTS_NAME,
        "rotations": ROTATIONS_NAME,
        "sea_level_curve": [[float(a), float(s)] for a, s in zip(sea_age, sea_m)],
        "attribution": ATTRIBUTION,
    }
    total = sum((out / name).stat().st_size for name in [*files, FOOTPRINTS_NAME, ROTATIONS_NAME])
    _log.info(
        "bake done in %.1f s: %d frames, %d images, %.2f MB of staged assets",
        time.perf_counter() - started,
        len(frames),
        len(images),
        total / 1e6,
    )
    return manifest


TILES_MANIFEST_VERSION = 2
ETOPO_RAW = "etopo/ETOPO_2022_v1_60s_N90W180_surface.nc"
KOPPEN_RAW = "koppen/koppen_geiger_tif.zip"
OVERVIEW_NAME = "overview.tfgrid"


@traced
def write_tiles(elev, koppen_codes, out, sources: dict, per_deg: int = tiles.PER_DEG) -> dict:
    """Publish a complete tile generation, with manifest replacement as the commit point."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".tile-bake-", dir=out) as tmp:
        stage = Path(tmp)
        payloads = stage / "payloads"
        payloads.mkdir()
        manifest = _write_tile_payloads(elev, koppen_codes, payloads, sources, per_deg)
        generation = hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest()
        manifest["generation"] = generation
        destination = out / generation
        # Concurrent identical bakes can publish the same immutable directory.
        try:
            payloads.rename(destination)
        except OSError as error:
            if error.errno not in (errno.EEXIST, errno.ENOTEMPTY) or not destination.is_dir():
                raise
        staged = stage / MANIFEST_NAME
        staged.write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
        staged.replace(out / MANIFEST_NAME)
        return manifest


def _write_tile_payloads(elev, koppen_codes, out, sources, per_deg):
    entries = []
    for row, col, tile in tiles.split_tiles(elev, koppen_codes, per_deg):
        path = out / f"r{row:02}c{col:02}.tfgrid"
        formats.write_tfgrid(path, "latlon", tile)
        blob = path.read_bytes()
        entries.append(
            {"row": row, "col": col, "bytes": len(blob), "sha256": hashlib.sha256(blob).hexdigest()}
        )
    path = out / OVERVIEW_NAME
    formats.write_tfgrid(path, "latlon", tiles.overview(elev, koppen_codes, per_deg))
    blob = path.read_bytes()
    manifest = {
        "version": TILES_MANIFEST_VERSION,
        "tile_deg": tiles.TILE_DEG,
        "samples": tiles.TILE_DEG * per_deg + 1,
        "sources": dict(sources),
        "tiles": entries,
        "overview": {"bytes": len(blob), "sha256": hashlib.sha256(blob).hexdigest()},
    }
    return manifest


@traced
def bake_tiles(raw, out) -> dict:
    """Bake the ETOPO 2022 and Koppen-Geiger tiles from ``raw`` into ``out``; returns the manifest."""
    raw, out = Path(raw), Path(out)
    etopo_path, koppen_path = raw / ETOPO_RAW, raw / KOPPEN_RAW
    for p in (etopo_path, koppen_path):
        if not p.is_file():
            raise FileNotFoundError(f"missing {p}")
    started = time.perf_counter()
    sources = {"etopo": _sha256(etopo_path), "koppen": _sha256(koppen_path)}
    for name, digest in sources.items():
        _log.debug("input %s sha256 %s", name, digest)
    elev = etopo.load(etopo_path)
    kop = koppen.load(koppen_path)
    manifest = write_tiles(elev, kop, out, sources)
    sizes = [t["bytes"] for t in manifest["tiles"]]
    _log.info(
        "tiles done in %.1f s: %d tiles, %.2f MB, largest %d bytes; overview %d bytes",
        time.perf_counter() - started,
        len(sizes),
        sum(sizes) / 1e6,
        max(sizes),
        manifest["overview"]["bytes"],
    )
    return manifest


def _tiles_main(argv) -> int:
    p = argparse.ArgumentParser(
        prog="timefly-bake tiles", description="Bake 5 degree ETOPO 2022 + Koppen-Geiger tiles."
    )
    p.add_argument("--raw", type=Path, required=True, help="raw download tree (data/raw)")
    p.add_argument("--out", type=Path, required=True, help="output directory")
    p.add_argument("--log-level", default="INFO", help="stderr log level (bake.log is DEBUG)")
    args = p.parse_args(argv)
    log.configure(args.out, args.log_level)
    try:
        try:
            bake_tiles(args.raw, args.out)
        except Exception as exc:  # already logged with its traceback by @traced
            _log.error("tile bake failed: %s", exc)
            return 1
        return 0
    finally:
        log.configure(None, args.log_level)


BEYER_ENTRY = "beyer2020"


def _climate_main(argv) -> int:
    p = argparse.ArgumentParser(
        prog="timefly-bake climate",
        description="Bake past climate (Beyer et al. 2020 BIO1 and BIO12) by HTTP range requests.",
    )
    p.add_argument("--sources", type=Path, required=True, help="data/sources.json")
    p.add_argument("--out", type=Path, required=True, help="output directory")
    p.add_argument(
        "--record-pins",
        action="store_true",
        help="print the extracted arrays' SHA-256 pins as JSON instead of checking them",
    )
    p.add_argument("--log-level", default="INFO", help="stderr log level (bake.log is DEBUG)")
    args = p.parse_args(argv)
    log.configure(args.out, args.log_level)
    try:
        try:
            doc = json.loads(args.sources.read_text(encoding="utf-8"))
            entry = next(e for e in doc.get("range_read", []) if e.get("name") == BEYER_ENTRY)
        except (OSError, ValueError, StopIteration):
            _log.error("%s has no range_read entry named %s", args.sources, BEYER_ENTRY)
            return 1
        try:
            manifest = beyer.bake_climate(entry, args.out, record_pins=args.record_pins)
        except Exception as exc:  # already logged with its traceback by @traced
            _log.error("climate bake failed: %s", exc)
            return 1
        if args.record_pins:
            print(json.dumps(manifest["source"]["variables_sha256"], indent=1))
        return 0
    finally:
        log.configure(None, args.log_level)


@traced
def main(argv=None) -> int:
    """Entry point of ``timefly-bake``; returns the process exit code. ``timefly-bake tiles ...``
    bakes the region-world tiles, ``timefly-bake climate ...`` the past-climate grids; anything
    else bakes the globe set."""
    argv = sys.argv[1:] if argv is None else list(argv)
    if argv and argv[0] == "tiles":
        return _tiles_main(argv[1:])
    if argv and argv[0] == "climate":
        return _climate_main(argv[1:])
    parser = _parser()
    args = parser.parse_args(argv)
    log.configure(args.out, args.log_level)
    try:
        try:
            ages_path = _ages_path(args.raw, args.paleodem_ages)
        except FileNotFoundError as exc:
            parser.error(str(exc))
        problems = _check_inputs(args.raw, ages_path)
        if problems:
            parser.error("; ".join(problems))
        try:
            bake(
                args.raw,
                args.out,
                args.cube_size,
                ages_path,
                args.paleodem_step_myr,
                args.footprint_dilate_cells,
            )
        except Exception as exc:  # already logged with its traceback by @traced
            _log.error("bake failed: %s", exc)
            return 1
        return 0
    finally:
        log.configure(None, args.log_level)
