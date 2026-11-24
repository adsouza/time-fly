"""Copy the active tile bake into a Linux package or Pages build."""

import json
from pathlib import Path
import re
import shutil
import sys

source, dest = map(Path, sys.argv[1:])
manifest = (source / "manifest.json").read_bytes()
data = json.loads(manifest)
dest.mkdir(parents=True, exist_ok=True)
manifest_dest = dest / "manifest.json"
if data["version"] == 2:
    generation = data["generation"]
    if not re.fullmatch(r"[0-9a-f]{64}", generation):
        sys.exit("invalid tile generation")
    source, dest = source / generation, dest / generation
    dest.mkdir(exist_ok=True)
elif data["version"] != 1:
    sys.exit("unsupported tile manifest version")


def copy(name, size):
    if (source / name).stat().st_size != size:
        sys.exit(f"{name}: size differs from the tile manifest")
    shutil.copyfile(source / name, dest / name)
    (dest / name).chmod(0o644)


if not data.get("tiles"):
    sys.exit("tile manifest lists no tiles")
if not data.get("overview"):
    sys.exit("tile manifest lists no overview")
copy("overview.tfgrid", data["overview"]["bytes"])
for tile in data["tiles"]:
    row, col = int(tile["row"]), int(tile["col"])
    if not (0 <= row < 36 and 0 <= col < 72):
        sys.exit(f"tile outside the tile map: {row}, {col}")
    copy(f"r{row:02}c{col:02}.tfgrid", tile["bytes"])
manifest_dest.write_bytes(manifest)
print(len(data["tiles"]))
