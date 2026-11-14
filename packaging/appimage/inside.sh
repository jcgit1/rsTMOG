#!/bin/bash
# Compile on Ubuntu 22.04, then pack. glibc symbols cannot exceed this image.
set -euo pipefail
cd /src
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/rstmog-target}"
if [[ -f Cargo.lock ]]; then
    cargo build --release --locked
else
    cargo build --release
fi
exec bash /src/packaging/appimage/package.sh \
    "$CARGO_TARGET_DIR/release/rstmog" \
    /dist
