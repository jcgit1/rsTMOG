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

# The Docker image already has these on PATH. A GitHub runner does not:
# fetch the same pinned, checksum-verified releases (see tools.sh).
if ! command -v linuxdeploy >/dev/null 2>&1 \
    || ! command -v linuxdeploy-plugin-gtk >/dev/null 2>&1 \
    || ! command -v appimagetool >/dev/null 2>&1 \
    || [[ ! -f "${APPIMAGE_RUNTIME:-}" ]]; then
    TOOLDIR=$(mktemp -d)
    bash "$ROOT/packaging/appimage/tools.sh" "$TOOLDIR"
    export PATH="$TOOLDIR:$PATH"
    export APPIMAGE_RUNTIME="$TOOLDIR/runtime-x86_64"
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
# libxkbcommon stays on the host too: libgdk needs soname .0 and only
# symbols from 0.5.0, and the host copy understands the host Compose
# file. Ubuntu 22.04's libxkbcommon logs "unrecognized keysym" for
# names added later (dead_hamza and the rest).
mapfile -t EXCLUDES < <(printf '%s\n' \
    'libvulkan.so*' \
    'libGL.so*' \
    'libGLX.so*' \
    'libGLdispatch.so*' \
    'libEGL.so*' \
    'libnvidia*.so*' \
    'libxkbcommon.so*' \
    'libxkbcommon-x11.so*')
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

# The GTK plugin may still copy libxkbcommon. Drop it so libgdk's
# $ORIGIN runpath misses it and the loader uses the host library.
find "$APPDIR" -type f -name 'libxkbcommon*.so*' -delete
xkb_left=$(find "$APPDIR" -type f -name 'libxkbcommon*.so*' || true)
if [[ -n "$xkb_left" ]]; then
    echo "libxkbcommon is still inside the AppDir:" >&2
    echo "$xkb_left" >&2
    exit 1
fi

# Empty on purpose. GIO scans this directory and does not fall back to
# the host module path. A file keeps the directory in the squashfs.
mkdir -p "$APPDIR/usr/lib/gio/modules"
printf '%s\n' 'Empty so host gvfs is not loaded. See apprun-hooks/rstmog-host-compat.sh.' \
    > "$APPDIR/usr/lib/gio/modules/README"

cp "$ROOT/packaging/appimage/host-compat.sh" \
    "$APPDIR/apprun-hooks/rstmog-host-compat.sh"
if ! grep -q 'rstmog-host-compat.sh' "$APPDIR/AppRun"; then
    awk '
        /^exec / && !inserted {
            print "source \"$this_dir\"/apprun-hooks/rstmog-host-compat.sh"
            print ""
            inserted = 1
        }
        { print }
    ' "$APPDIR/AppRun" > "$APPDIR/AppRun.new"
    mv "$APPDIR/AppRun.new" "$APPDIR/AppRun"
fi

# mktemp creates the AppDir as 0700. The runtime refuses that, because the
# person who runs the AppImage is not the packager.
chmod -R a+rX "$APPDIR"
chmod a+rx "$APPDIR/AppRun"

mkdir -p "$OUT"
DEST="$OUT/rstmog-${VERSION}-x86_64.AppImage"
appimagetool --no-appstream --runtime-file "$APPIMAGE_RUNTIME" "$APPDIR" "$DEST"
chmod +x "$DEST"
(
    cd "$OUT"
    sha256sum "$(basename "$DEST")" > "$(basename "$DEST").sha256"
)
echo "wrote $DEST"
