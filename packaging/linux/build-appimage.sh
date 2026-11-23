#!/usr/bin/env bash
# Build a portable x86_64 AppImage from the release binary.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

target="${TARGET:-x86_64-unknown-linux-gnu}"
version="${HUDDLE_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}"
binary="target/${target}/release/huddle"
appdir="dist/Huddle.AppDir"
out="dist/Huddle-${version}-x86_64.AppImage"
tools="dist/appimage-tools"

if [[ ! -x "$binary" ]]; then
  echo "missing binary: $binary" >&2
  exit 1
fi

rm -rf "$appdir"
mkdir -p \
  "$appdir/usr/bin" \
  "$appdir/usr/share/applications" \
  "$appdir/usr/share/icons/hicolor/scalable/apps" \
  "$tools"

install -m 755 "$binary" "$appdir/usr/bin/huddle"
install -m 644 packaging/linux/huddle.desktop "$appdir/huddle.desktop"
install -m 644 packaging/linux/huddle.desktop "$appdir/usr/share/applications/huddle.desktop"
install -m 644 assets/linux/huddle.svg "$appdir/huddle.svg"
install -m 644 assets/linux/huddle.svg "$appdir/usr/share/icons/hicolor/scalable/apps/huddle.svg"
printf '%s\n' '#!/bin/sh' 'HERE="$(dirname "$(readlink -f "$0")")"' 'exec "$HERE/usr/bin/huddle" "$@"' > "$appdir/AppRun"
chmod +x "$appdir/AppRun"
sed -i 's/^Exec=.*/Exec=huddle/' "$appdir/huddle.desktop"
sed -i 's/^Icon=.*/Icon=huddle/' "$appdir/huddle.desktop"

if [[ ! -x "$tools/appimagetool-x86_64.AppImage" ]]; then
  curl -fsSL -o "$tools/appimagetool-x86_64.AppImage" \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$tools/appimagetool-x86_64.AppImage"
fi

# Prefer extracted appimagetool when FUSE is unavailable on CI.
tool="$tools/appimagetool-x86_64.AppImage"
if ! "$tool" --version >/dev/null 2>&1; then
  mkdir -p "$tools/appimagetool-extract"
  (
    cd "$tools/appimagetool-extract"
    ../appimagetool-x86_64.AppImage --appimage-extract >/dev/null
  )
  tool="$tools/appimagetool-extract/squashfs-root/AppRun"
fi

rm -f "$out" dist/Huddle*.AppImage Huddle*.AppImage
ARCH=x86_64 "$tool" "$appdir" "$out"
test -f "$out"
chmod +x "$out"
echo "Built $out"
