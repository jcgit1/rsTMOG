#!/usr/bin/env bash
# Install rsTMOG icon + .desktop launcher for the current user.
# Usage: BINDIR=/path/to/target/release bash packaging/install-desktop.sh
set -eu
BIN="${BINDIR:-$HOME/repos/rsTMOG/target/release}/rstmog"
[ -x "$BIN" ] || { echo "no executable at $BIN (cargo build --release first)"; exit 1; }
XDG="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$XDG/icons/hicolor/256x256/apps" "$XDG/applications"
HERE="$(cd "$(dirname "$0")" && pwd)"
install -m 644 "$HERE/../assets/icons/dark/icon-256.png" \
  "$XDG/icons/hicolor/256x256/apps/rstmog.png"
sed "s|^Exec=rstmog|Exec=$BIN|" "$HERE/rstmog.desktop" > "$XDG/applications/rstmog.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$XDG/applications" || true
command -v gtk-update-icon-cache >/dev/null && \
  gtk-update-icon-cache -q "$XDG/icons/hicolor" || true
echo "installed: $XDG/applications/rstmog.desktop"
