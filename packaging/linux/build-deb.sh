#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

target="${TARGET:-x86_64-unknown-linux-gnu}"
version="${HUDDLE_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}"
arch="${DEB_ARCH:-amd64}"
stage="dist/deb-root"
package="dist/huddle_${version}_${arch}.deb"

rm -rf "$stage"
mkdir -p \
  "$stage/DEBIAN" \
  "$stage/usr/bin" \
  "$stage/usr/share/applications" \
  "$stage/usr/share/icons/hicolor/scalable/apps"

install -m 755 "target/${target}/release/huddle" "$stage/usr/bin/huddle"
install -m 644 packaging/linux/huddle.desktop "$stage/usr/share/applications/huddle.desktop"
install -m 644 assets/linux/huddle.svg "$stage/usr/share/icons/hicolor/scalable/apps/huddle.svg"

cat > "$stage/DEBIAN/control" <<EOF
Package: huddle
Version: ${version}
Section: net
Priority: optional
Architecture: ${arch}
Maintainer: Huddle <hdmain@users.noreply.github.com>
Depends: libasound2, libc6, libgcc-s1, libstdc++6, libfontconfig1, libfreetype6, libx11-6, libxcb1, libxcursor1, libxi6, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libwayland-cursor0, libwayland-egl1, libvulkan1, libgtk-3-0, libwebkit2gtk-4.1-0, libjavascriptcoregtk-4.1-0, libsoup-3.0-0
Description: Matrix chat client
 A desktop Matrix client.
EOF

mkdir -p dist
dpkg-deb --build --root-owner-group "$stage" "$package"
echo "Built ${package}"
