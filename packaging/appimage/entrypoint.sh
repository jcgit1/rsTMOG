#!/bin/bash
# Run inside the Ubuntu 22.04 image. The repo is mounted at /src.
set -euo pipefail
if [[ ! -f /src/Cargo.toml ]]; then
    echo "mount the rsTMOG repo at /src (see packaging/appimage/build.sh)" >&2
    exit 1
fi
exec bash /src/packaging/appimage/inside.sh
