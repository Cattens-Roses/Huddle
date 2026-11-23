#!/usr/bin/env bash
# Build an Arch Linux package (.pkg.tar.zst) from the release binary.
# Intended to run inside an Arch container (or any host with makepkg).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

target="${TARGET:-x86_64-unknown-linux-gnu}"
version="${HUDDLE_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}"
pkgver="${version//-/.}"
binary="target/${target}/release/huddle"
work="dist/arch-build"

if [[ ! -x "$binary" ]]; then
  echo "missing binary: $binary" >&2
  exit 1
fi

if ! command -v makepkg >/dev/null 2>&1; then
  echo "makepkg is required (run inside Arch Linux)" >&2
  exit 1
fi

rm -rf "$work"
mkdir -p "$work" dist
cp -f packaging/arch/PKGBUILD "$work/PKGBUILD"
install -m 755 "$binary" "$work/huddle"
install -m 644 packaging/linux/huddle.desktop "$work/huddle.desktop"
install -m 644 assets/linux/huddle.svg "$work/huddle.svg"
sed -i "s/^pkgver=.*/pkgver=${pkgver}/" "$work/PKGBUILD"

(
  cd "$work"
  export HUDDLE_PKGVER="$pkgver"
  makepkg -sf --noconfirm --nodeps
)

shopt -s nullglob
built=("$work"/huddle-*.pkg.tar.zst)
if (( ${#built[@]} == 0 )); then
  echo "makepkg produced no package" >&2
  ls -la "$work" || true
  exit 1
fi
cp -f "${built[0]}" "dist/huddle-${pkgver}-1-x86_64.pkg.tar.zst"
echo "Built dist/huddle-${pkgver}-1-x86_64.pkg.tar.zst"
