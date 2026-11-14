#!/bin/bash
# Pinned AppImage build tools: exact releases plus SHA-256, verified before
# anything runs. Used by the Dockerfile and by package.sh on a bare runner.
#
# To upgrade: change a URL to a new tagged release (never "continuous",
# which can be replaced at any time), download it, and update its hash
# with `sha256sum`.
#
# Usage: bash tools.sh <dir>   -> <dir>/{linuxdeploy,linuxdeploy-plugin-gtk,
#                                         appimagetool,runtime-x86_64}
set -euo pipefail

TOOLS=(
    "linuxdeploy|https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage|c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d"
    # The GTK plugin no longer publishes an AppImage. This commit is the
    # script linuxdeploy still expects to find as linuxdeploy-plugin-gtk.
    "linuxdeploy-plugin-gtk|https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/3b67a1d1c1b0c8268f57f2bce40fe2d33d409cea/linuxdeploy-plugin-gtk.sh|b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171"
    "appimagetool|https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage|ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0"
    # appimagetool otherwise downloads the "continuous" runtime at pack time;
    # package.sh passes this file with --runtime-file instead.
    "runtime-x86_64|https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64|2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"
)

if [[ $# -ne 1 ]]; then
    echo "usage: tools.sh <dir>" >&2
    exit 2
fi
DIR=$1
mkdir -p "$DIR"

for entry in "${TOOLS[@]}"; do
    IFS='|' read -r name url sha <<<"$entry"
    curl -fsSL --proto '=https' -o "$DIR/$name.part" "$url"
    if ! echo "$sha  $DIR/$name.part" | sha256sum -c --quiet -; then
        echo "checksum mismatch for $name ($url)" >&2
        rm -f "$DIR/$name.part"
        exit 1
    fi
    mv "$DIR/$name.part" "$DIR/$name"
    chmod +x "$DIR/$name"
done
