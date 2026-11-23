#!/usr/bin/env bash
# Builds the install tree shared by the .deb and the Flatpak:
#
#   lib/time-fly/time-fly                    the game binary
#   lib/time-fly/assets/data/globe/...       baked globe data (bake.log and other files left out)
#   bin/time-fly                             wrapper: exec <dir>/time-fly "$@"
#   share/applications/io.github.adsouza.TimeFly.desktop
#   share/icons/hicolor/scalable/apps/io.github.adsouza.TimeFly.svg
#   share/metainfo/io.github.adsouza.TimeFly.metainfo.xml
#   share/doc/time-fly/{copyright,README}
#
# Bevy's file asset reader resolves `assets/` against the directory of the running executable
# (bevy_asset 0.20 `get_base_path`: BEVY_ASSET_ROOT, else CARGO_MANIFEST_DIR, else
# `current_exe().parent()`), so the binary and `assets/` live together and the wrapper execs the
# real path. The wrapper written here finds the binary relative to itself (`../lib/time-fly`),
# which is right for the Flatpak (/app/bin, /app/lib/time-fly); build-deb.sh writes its own for
# /usr/bin -> /opt/time-fly.
#
# Usage: stage.sh BINARY ASSETS_DIR OUT_DIR    (OUT_DIR must not exist or be empty)
# Requires bash, Python 3 and coreutils-style tools (checked on macOS too).
set -euo pipefail

APP_ID=io.github.adsouza.TimeFly
die() { echo "stage.sh: $*" >&2; exit 1; }

[ $# -eq 3 ] || die "usage: stage.sh BINARY ASSETS_DIR OUT_DIR"
binary=$1 assets=$2 out=$3
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
globe="$assets/data/globe"

[ -f "$binary" ] && [ -x "$binary" ] || die "binary not found or not executable: $binary"
[ -f "$globe/manifest.json" ] || die "no baked data: $globe/manifest.json (run timefly-bake)"
[ -f "$assets/credits.txt" ] || die "missing $assets/credits.txt"
if [ -e "$out" ] && [ -n "$(ls -A "$out")" ]; then die "output directory is not empty: $out"; fi

lib="$out/lib/time-fly"
mkdir -p "$lib/assets/data/globe/frames" "$out/bin" \
  "$out/share/applications" "$out/share/icons/hicolor/scalable/apps" \
  "$out/share/metainfo" "$out/share/doc/time-fly"

install -m 0755 "$binary" "$lib/time-fly"

# Runtime assets, by allowlist: what the game loads (`data.rs`: the manifest, the frames it
# lists, the plate footprints and rotations). Shaders and credits.txt are compiled into the
# binary; bake.log is a build log.
n_frames=$(python3 - "$globe" "$lib/assets/data/globe" <<'PYTHON'
import json
from pathlib import Path
import re
import shutil
import sys

source, dest = map(Path, sys.argv[1:])
manifest = (source / "manifest.json").read_bytes()
data = json.loads(manifest)
frames = {f["file"] for f in data["frames"]}
if not frames:
    sys.exit("manifest has no frames")
# Accept both numbered legacy assets and content-addressed bakes.
digest = r"(?:\.[0-9a-f]{64})?"


def copy(name, pattern):
    if not re.fullmatch(pattern, name):
        sys.exit(f"invalid manifest asset name {name!r}")
    shutil.copyfile(source / name, dest / name)
    (dest / name).chmod(0o644)


copy(data["footprints"], "plates" + digest + r"\.tfplates")
copy(data["rotations"], "rotations" + digest + r"\.tfrot")
for frame in frames:
    copy(frame, r"frames/f[0-9]+" + digest + r"\.tfgrid")
(dest / "manifest.json").write_bytes(manifest)
print(len(frames))
PYTHON
)

cat >"$out/bin/time-fly" <<'WRAPPER'
#!/bin/sh
# Runs Time Fly from its install directory, where Bevy finds assets/ beside the binary.
here=$(dirname "$(readlink -f "$0")")
exec "$here/../lib/time-fly/time-fly" "$@"
WRAPPER
chmod 0755 "$out/bin/time-fly"

install -m 0644 "$here/$APP_ID.desktop" "$out/share/applications/$APP_ID.desktop"
install -m 0644 "$here/$APP_ID.svg" "$out/share/icons/hicolor/scalable/apps/$APP_ID.svg"
install -m 0644 "$here/$APP_ID.metainfo.xml" "$out/share/metainfo/$APP_ID.metainfo.xml"

doc="$out/share/doc/time-fly"
{
  cat <<'HEAD'
Time Fly
Source: https://github.com/adsouza/time-fly

Copyright 2026 Antonio D'souza <275832+adsouza@users.noreply.github.com>
License: AGPL-3.0-or-later

The game is free software: you can redistribute it and/or modify it under the terms of the
GNU Affero General Public License as published by the Free Software Foundation, either
version 3 of the License, or (at your option) any later version (full text at the end of
this file).

The baked globe data in assets/data/globe is derived from the datasets credited below. Each
is redistributed in baked form only, under the terms stated with it; the original files are
not included. Provenance, checksums and licence notes: data/README.md in the source.

================================================================================
HEAD
  cat "$assets/credits.txt"
  printf '\n================================================================================\n'
  printf 'GNU AFFERO GENERAL PUBLIC LICENSE, version 3\n\n'
  cat "$root/LICENSE"
} >"$doc/copyright"
chmod 0644 "$doc/copyright"

cat >"$doc/README" <<'README'
Time Fly: Earth through deep time, from 540 million years ago to the present.

RUN
  Debian package:   time-fly            (or Time Fly in the application menu)
  Flatpak:          flatpak run io.github.adsouza.TimeFly
  Force Vulkan:     WGPU_BACKEND=vulkan time-fly
                    flatpak run --env=WGPU_BACKEND=vulkan io.github.adsouza.TimeFly
  Scrub benchmark:  time-fly --scrub-bench --target-fps 60 --window 2560x1440
  Crash probe:      time-fly --diagnostics-probe=panic   (then launch normally)

  Controls: drag to orbit, scroll to zoom, [ and ] to step older or younger, Esc for credits.

LOGS
  Each run writes a session directory with its log:
  Debian package:   ~/.local/state/time-fly/logs/   ($XDG_STATE_HOME/time-fly/logs if set)
  Flatpak:          ~/.var/app/io.github.adsouza.TimeFly/.local/state/time-fly/logs/
  After a crash, the next launch shows a notice with the log's location and a Show log button.

FILES
  Debian package:   /opt/time-fly (binary and assets), /usr/bin/time-fly (launcher)
  Flatpak:          /app/lib/time-fly inside the sandbox

Licences and data citations: copyright (this directory).
README
chmod 0644 "$doc/README"

echo "stage.sh: staged $(basename "$binary") with $n_frames frames into $out"
