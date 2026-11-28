"""Copy the climate bake (`timefly-bake climate`) into a Linux package or Pages build.

Usage: stage_climate.py SOURCE DEST [--expect-grids N]

Copies the manifest and every grid it lists, each checked against the manifest's size and
SHA-256; bake.log and anything else in SOURCE is left out. Fails, copying nothing, when the
manifest is missing or unreadable, a listed grid is missing or differs from its entry, or the
manifest lists other than N grids (default: the 35 ages `timefly-bake climate` writes). A
package without the grids would still run, on the zonal climate (`ClimateIndex::fail`), so
this is the check that every shipped build carries them. Prints the number of grids staged.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import shutil
import struct
import sys

# len(timefly_bake.beyer.climate_ages_ka()); tools/bake/tests/test_stage_climate.py ties them.
SHIPPED_AGES = [*range(23), 24, 26, *range(30, 121, 10)]
SHIPPED_GRIDS = len(SHIPPED_AGES)
GRID_HEADER = struct.Struct("<8sBBBBIII")

p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
p.add_argument("source", type=Path)
p.add_argument("dest", type=Path)
p.add_argument("--expect-grids", type=int, default=SHIPPED_GRIDS)
args = p.parse_args()
source, dest = args.source, args.dest


def die(message):
    sys.exit(f"stage_climate.py: {message}")


try:
    manifest = (source / "manifest.json").read_bytes()
    data = json.loads(manifest)
except (OSError, ValueError) as exc:
    die(f"no usable climate manifest in {source} (run timefly-bake climate): {exc}")
if type(data.get("version")) is not int or data["version"] != 1:
    die(f"unsupported climate manifest version {data.get('version')!r}")
files = data.get("files") or []
if args.expect_grids < 1 or len(files) != args.expect_grids:
    die(f"climate manifest lists {len(files)} grids, expected {args.expect_grids}")
if len(data.get("ages_ka") or []) != len(files):
    die("climate manifest ages and files differ in length")

ages = data["ages_ka"]
if any(type(a) not in (int, float) or not math.isfinite(a) or a < 0 for a in ages):
    die("invalid climate ages")
if any(a >= b for a, b in zip(ages, ages[1:])):
    die("climate ages must be strictly increasing")
if args.expect_grids == SHIPPED_GRIDS and ages != SHIPPED_AGES:
    die("climate manifest differs from the shipped ages")
grid = data.get("grid", {})
if (
    grid != {"deg": 1, "rows": 180, "cols": 360, "channels": ["bio1_c", "bio12_mm"]}
    or type(grid["rows"]) is not int
    or type(grid["cols"]) is not int
):
    die("unsupported climate grid shape or channels")

for age, entry in zip(ages, files):
    name, size, sha = entry["file"], entry["bytes"], entry["sha256"]
    if type(entry.get("age_ka")) not in (int, float) or entry["age_ka"] != age:
        die(f"climate grid does not match age {age}")
    if name != f"bio_{age:g}ka.{sha}.tfgrid":
        die(f"climate grid does not match age {age} or hash")
    if not isinstance(sha, str) or len(sha) != 64 or any(c not in "0123456789abcdef" for c in sha):
        die(f"invalid climate grid name {name!r}")
    try:
        blob = (source / name).read_bytes()
    except OSError as exc:
        die(f"{name}: {exc}")
    if type(size) is not int or len(blob) != size:
        die(f"{name}: {len(blob)} bytes, the manifest says {size}")
    if hashlib.sha256(blob).hexdigest() != sha:
        die(f"{name}: sha256 differs from the climate manifest")

    if len(blob) <= GRID_HEADER.size or GRID_HEADER.unpack_from(blob) != (
        b"TFGRID01",
        1,
        0,
        2,
        0,
        360,
        180,
        len(blob) - GRID_HEADER.size,
    ):
        die(f"{name}: invalid climate grid header")

dest.mkdir(parents=True, exist_ok=True)
for entry in files:
    shutil.copyfile(source / entry["file"], dest / entry["file"])
    (dest / entry["file"]).chmod(0o644)
(dest / "manifest.json").write_bytes(manifest)
(dest / "manifest.json").chmod(0o644)
print(len(files))
