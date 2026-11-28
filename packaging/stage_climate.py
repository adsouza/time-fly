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
from pathlib import Path
import re
import shutil
import sys

# len(timefly_bake.beyer.climate_ages_ka()); tools/bake/tests/test_stage_climate.py ties them.
SHIPPED_GRIDS = 35

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
if data.get("version") != 1:
    die(f"unsupported climate manifest version {data.get('version')!r}")
files = data.get("files") or []
if len(files) != args.expect_grids:
    die(f"climate manifest lists {len(files)} grids, expected {args.expect_grids}")
if len(data.get("ages_ka") or []) != len(files):
    die("climate manifest ages and files differ in length")

for entry in files:
    name, size, sha = entry["file"], entry["bytes"], entry["sha256"]
    if not re.fullmatch(r"bio_[0-9.]+ka\.[0-9a-f]{64}\.tfgrid", name):
        die(f"invalid climate grid name {name!r}")
    try:
        blob = (source / name).read_bytes()
    except OSError as exc:
        die(f"{name}: {exc}")
    if len(blob) != size:
        die(f"{name}: {len(blob)} bytes, the manifest says {size}")
    if hashlib.sha256(blob).hexdigest() != sha:
        die(f"{name}: sha256 differs from the climate manifest")

dest.mkdir(parents=True, exist_ok=True)
for entry in files:
    shutil.copyfile(source / entry["file"], dest / entry["file"])
    (dest / entry["file"]).chmod(0o644)
(dest / "manifest.json").write_bytes(manifest)
(dest / "manifest.json").chmod(0o644)
print(len(files))
