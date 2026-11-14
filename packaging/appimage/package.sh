#!/bin/bash
# Turn a release binary into an AppImage.
# Usage: package.sh <rstmog-binary> <output-dir>
#
# Run this on Ubuntu 22.04 (the Docker image does). A binary linked on a
# newer distro carries a newer glibc requirement, and this script refuses it.
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: package.sh <rstmog-binary> <output-dir>" >&2
    exit 2
fi

BIN=$(readlink -f "$1")
OUT=$(readlink -f "$2")
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
[[ -x "$BIN" ]] || { echo "not an executable: $BIN" >&2; exit 1; }

VERSION=$("$BIN" --version | awk '{print $2}')
[[ -n "$VERSION" ]] || { echo "could not read version from $BIN" >&2; exit 1; }

# The Docker image already has these on PATH. A GitHub runner does not.
if ! command -v linuxdeploy >/dev/null 2>&1 \
    || ! command -v linuxdeploy-plugin-gtk >/dev/null 2>&1 \
    || ! command -v appimagetool >/dev/null 2>&1; then
    TOOLDIR=$(mktemp -d)
    curl -fsSL -o "$TOOLDIR/linuxdeploy" \
        https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage
    curl -fsSL -o "$TOOLDIR/linuxdeploy-plugin-gtk" \
        https://github.com/linuxdeploy/linuxdeploy-plugin-gtk/releases/download/continuous/linuxdeploy-plugin-gtk-x86_64.AppImage
    curl -fsSL -o "$TOOLDIR/appimagetool" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$TOOLDIR/linuxdeploy" "$TOOLDIR/linuxdeploy-plugin-gtk" "$TOOLDIR/appimagetool"
    export PATH="$TOOLDIR:$PATH"
fi

export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH=x86_64
export DEPLOY_GTK_VERSION=3

APPDIR=$(mktemp -d)
ICONDIR=$(mktemp -d)
cleanup() { rm -rf "$APPDIR" "$ICONDIR"; }
trap cleanup EXIT

# The desktop file's Icon= key is "rstmog".
cp "$ROOT/assets/icons/dark/icon-256.png" "$ICONDIR/rstmog.png"

# dlopen'd by libappindicator-sys, so linuxdeploy will not see it via ldd.
INDICATOR=$(ldconfig -p | awk '/libayatana-appindicator3\.so\.1/ { print $NF; exit }')
if [[ -z "$INDICATOR" ]]; then
    INDICATOR=$(ldconfig -p | awk '/libappindicator3\.so\.1/ { print $NF; exit }')
fi
[[ -n "$INDICATOR" ]] || { echo "libayatana-appindicator3.so.1 not found" >&2; exit 1; }

# Host GPU drivers. Bundling any of these makes Vulkan or NVML fail.
mapfile -t EXCLUDES < <(printf '%s\n' \
    'libvulkan.so*' \
    'libGL.so*' \
    'libGLX.so*' \
    'libGLdispatch.so*' \
    'libEGL.so*' \
    'libnvidia*.so*')
EXCLUDE_ARGS=()
for pattern in "${EXCLUDES[@]}"; do
    EXCLUDE_ARGS+=(--exclude-library "$pattern")
done

linuxdeploy --appdir "$APPDIR" \
    --executable "$BIN" \
    --desktop-file "$ROOT/packaging/rstmog.desktop" \
    --icon-file "$ICONDIR/rstmog.png" \
    --icon-filename rstmog \
    --library "$INDICATOR" \
    "${EXCLUDE_ARGS[@]}" \
    --plugin gtk

bundled=$(find "$APPDIR" -type f \
    \( -name 'libvulkan.so*' -o -name 'libnvidia*.so*' \
       -o -name 'libGL.so*' -o -name 'libEGL.so*' \
       -o -name 'libGLX.so*' -o -name 'libGLdispatch.so*' \) || true)
if [[ -n "$bundled" ]]; then
    echo "refusing to ship host GPU libraries inside the AppImage:" >&2
    echo "$bundled" >&2
    exit 1
fi

for needed in libgtk-3.so libayatana-appindicator3.so; do
    if ! find "$APPDIR" -name "${needed}*" | grep -q .; then
        echo "AppDir is missing $needed" >&2
        exit 1
    fi
done

max_glibc=$(objdump -T "$APPDIR/usr/bin/rstmog" \
    | grep -o 'GLIBC_[0-9.]*' \
    | sed 's/^GLIBC_//' \
    | sort -V \
    | tail -1)
echo "rstmog glibc requirement: $max_glibc"
winner=$(printf '%s\n%s\n' "$max_glibc" "2.35" | sort -V | tail -1)
if [[ "$winner" != "2.35" ]]; then
    echo "glibc $max_glibc is newer than Ubuntu 22.04 (2.35); build in packaging/appimage" >&2
    exit 1
fi

mkdir -p "$OUT"
DEST="$OUT/rstmog-${VERSION}-x86_64.AppImage"
appimagetool --no-appstream "$APPDIR" "$DEST"
chmod +x "$DEST"
(
    cd "$OUT"
    sha256sum "$(basename "$DEST")" > "$(basename "$DEST").sha256"
)
echo "wrote $DEST"
