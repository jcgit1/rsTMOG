#!/usr/bin/env bash
# Build the x86_64 AppImage inside Ubuntu 22.04.
# Output: packaging/appimage/dist/rstmog-<version>-x86_64.AppImage
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
if ! docker info >/dev/null 2>&1; then
    echo "docker is not usable from this account. The release workflow builds on GitHub's Ubuntu 22.04 runner instead." >&2
    exit 1
fi
mkdir -p packaging/appimage/dist
docker build -t rstmog-appimage-builder -f packaging/appimage/Dockerfile packaging/appimage
docker run --rm \
    -v "$ROOT":/src \
    -v "$ROOT/packaging/appimage/dist":/dist \
    -v rstmog-cargo-registry:/opt/cargo/registry \
    -v rstmog-cargo-git:/opt/cargo/git \
    rstmog-appimage-builder
docker run --rm --entrypoint chown \
    -v "$ROOT/packaging/appimage/dist":/dist \
    rstmog-appimage-builder -R "$(id -u):$(id -g)" /dist
