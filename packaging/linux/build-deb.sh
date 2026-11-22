#!/usr/bin/env bash
# Builds time-fly_<VERSION>_amd64.deb from the tree made by stage.sh:
#
#   STAGED/lib/time-fly/*   -> /opt/time-fly/       (binary and assets together)
#   (generated)             -> /usr/bin/time-fly    (wrapper: exec /opt/time-fly/time-fly "$@")
#   STAGED/share/*          -> /usr/share/
#
# Depends comes from dpkg-shlibdeps on the binary, plus libvulkan1 (wgpu loads Vulkan at run
# time). Recommends lists the libraries winit loads at run time.
#
# Usage: build-deb.sh STAGED_DIR VERSION OUT_DIR      (needs dpkg-deb and dpkg-shlibdeps)
set -euo pipefail

die() { echo "build-deb.sh: $*" >&2; exit 1; }
[ $# -eq 3 ] || die "usage: build-deb.sh STAGED_DIR VERSION OUT_DIR"
staged=$1 version=$2 out=$3
[ -x "$staged/lib/time-fly/time-fly" ] || die "no staged binary in $staged/lib/time-fly"
[[ "$version" =~ ^[0-9][A-Za-z0-9.+~-]*$ ]] || die "bad Debian version: $version"
command -v dpkg-deb >/dev/null || die "dpkg-deb not found"
command -v dpkg-shlibdeps >/dev/null || die "dpkg-shlibdeps not found (apt install dpkg-dev)"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
pkg="$work/root"
mkdir -p "$pkg/DEBIAN" "$pkg/opt/time-fly" "$pkg/usr/bin" "$pkg/usr/share"

cp -R "$staged/lib/time-fly/." "$pkg/opt/time-fly/"
cp -R "$staged/share/." "$pkg/usr/share/"
cat >"$pkg/usr/bin/time-fly" <<'WRAPPER'
#!/bin/sh
# Runs Time Fly from its install directory, where Bevy finds assets/ beside the binary.
exec /opt/time-fly/time-fly "$@"
WRAPPER

# Canonical modes: directories and executables 0755, everything else 0644.
find "$pkg" -type d -exec chmod 0755 {} +
find "$pkg" -type f -exec chmod 0644 {} +
chmod 0755 "$pkg/opt/time-fly/time-fly" "$pkg/usr/bin/time-fly"

# dpkg-shlibdeps wants a debian/control in the current directory.
mkdir -p "$work/src/debian"
printf 'Source: time-fly\n\nPackage: time-fly\nArchitecture: amd64\n' >"$work/src/debian/control"
shlibs=$(cd "$work/src" && dpkg-shlibdeps -O "$pkg/opt/time-fly/time-fly" 2>"$work/shlibdeps.err") \
  || { cat "$work/shlibdeps.err" >&2; die "dpkg-shlibdeps failed"; }
shlibs=${shlibs#shlibs:Depends=}
[ -n "$shlibs" ] || die "dpkg-shlibdeps found no dependencies"

installed_kb=$(du -sk "$pkg" | cut -f1)
cat >"$pkg/DEBIAN/control" <<EOF
Package: time-fly
Version: $version
Architecture: amd64
Maintainer: Antonio D'souza <275832+adsouza@users.noreply.github.com>
Installed-Size: $installed_kb
Depends: $shlibs, libvulkan1
Recommends: libwayland-client0 | libx11-6, libxkbcommon0, libxcursor1, libxrandr2, libxi6, mesa-vulkan-drivers | vulkan-icd
Section: games
Priority: optional
Homepage: https://github.com/adsouza/time-fly
Description: Earth through deep time
 A globe of the Earth from 540 million years ago to the present: drag the
 time slider to watch the continents drift and the ice sheets grow and melt.
 .
 Installs to /opt/time-fly with a time-fly launcher in /usr/bin. Data
 citations and licences: /usr/share/doc/time-fly/copyright.
EOF

mkdir -p "$out"
deb="$out/time-fly_${version}_amd64.deb"
# xz rather than the zstd default of Ubuntu's dpkg, so older Debian dpkg can install it too.
dpkg-deb --root-owner-group -Zxz --build "$pkg" "$deb"
echo "Depends: $shlibs, libvulkan1"
dpkg-deb --info "$deb"
dpkg-deb --contents "$deb" | awk '{print $6}' | grep -v '/frames/f0' || true
echo "build-deb.sh: wrote $deb"
