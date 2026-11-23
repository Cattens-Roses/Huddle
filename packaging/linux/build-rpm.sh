#!/usr/bin/env bash
# Build an RPM from the release binary (Fedora / RHEL / Alma / openSUSE).
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

target="${TARGET:-x86_64-unknown-linux-gnu}"
version="${HUDDLE_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}"
# RPM versions cannot contain '-' without a release suffix dance; map -gSHA → .gSHA
rpm_version="${version//-/.}"
arch="${RPM_ARCH:-x86_64}"
binary="target/${target}/release/huddle"
stage="dist/rpm-root"
spec="dist/huddle.spec"
topdir="dist/rpmbuild"

if [[ ! -x "$binary" ]]; then
  echo "missing binary: $binary" >&2
  exit 1
fi

rm -rf "$stage" "$topdir"
mkdir -p \
  "$stage/usr/bin" \
  "$stage/usr/share/applications" \
  "$stage/usr/share/icons/hicolor/scalable/apps" \
  "$topdir"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}

install -m 755 "$binary" "$stage/usr/bin/huddle"
install -m 644 packaging/linux/huddle.desktop "$stage/usr/share/applications/huddle.desktop"
install -m 644 assets/linux/huddle.svg "$stage/usr/share/icons/hicolor/scalable/apps/huddle.svg"

cat > "$spec" <<EOF
Name:           huddle
Version:        ${rpm_version}
Release:        1%{?dist}
Summary:        Matrix chat client
License:        GPL-3.0-or-later
URL:            https://github.com/Cattens-Roses/Huddle
BuildArch:      ${arch}
AutoReqProv:    no

Requires:       alsa-lib
Requires:       fontconfig
Requires:       freetype
Requires:       libX11
Requires:       libXcursor
Requires:       libXi
Requires:       libxkbcommon
Requires:       libwayland-client
Requires:       vulkan-loader
Requires:       gtk3
Requires:       webkit2gtk4.1

%description
A native Matrix desktop client built with Rust and GPUI.

%install
mkdir -p %{buildroot}
cp -a ${root}/${stage}/. %{buildroot}/

%files
/usr/bin/huddle
/usr/share/applications/huddle.desktop
/usr/share/icons/hicolor/scalable/apps/huddle.svg
EOF

rpmbuild \
  --define "_topdir ${root}/${topdir}" \
  --define "_rpmdir ${root}/dist" \
  --define "_build_id_links none" \
  -bb "$spec"

# Normalize output name next to other dist artifacts.
shopt -s nullglob
built=(dist/${arch}/huddle-*.rpm dist/huddle-*.rpm)
if (( ${#built[@]} == 0 )); then
  echo "rpmbuild produced no package" >&2
  find dist -name '*.rpm' -print || true
  exit 1
fi
mkdir -p dist
cp -f "${built[0]}" "dist/huddle-${rpm_version}-1.${arch}.rpm"
echo "Built dist/huddle-${rpm_version}-1.${arch}.rpm"
