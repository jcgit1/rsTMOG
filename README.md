# rsTMOG

Task Manager OG, rewritten in Rust. Linux-only by design.

A from-scratch Rust reimplementation of the TMOG concept (Dave Plummer's
Task Manager OG), inspired by the behavioral design of
[evildojo-tmog](https://github.com/mikedesu/evildojo-tmog) (MIT, Michael
Bell). No C++ code is ported line-by-line; the Rust is written idiomatically.

![Summary page](docs/capture-summary.png)

## What you get

- **Eight pages**: Summary (live CPU/temperature trend graphs + readouts),
  CPU (aggregate history graph, per-core bars, topology + freq/governor),
  Memory (used% graph, ARC-aware stacked composition bar, PSI), Disks
  (per-device read/write rate graphs + identity), Network (per-iface
  rx/tx graphs, totals, speed/MAC), Thermals (CPU trend + grouped hwmon
  sensors), Processes (sortable/filterable table, tree view, follow mode,
  right-click End/Force-kill/Suspend/priority/Copy-cmdline), System.
- **ZFS ARC-aware memory accounting** (the btop view): reclaimable ARC
  folds back into available so a big ARC doesn't fake a full machine.
- **NVIDIA GPU** via NVML (dlopen'd — no build-time driver dependency).
- **Six themes** (dark + green/amber/blue phosphor + mono + light), live
  switching, persisted — with theme-aware window and tray icons.
- **System tray**: Open / poll-rate (500/1000/2000 ms) / Quit.
- **Settings dialog**: theme, poll rate, process-table defaults; saved to
  TOML immediately.
- **Headless mode**: `rstmog --capture <page> out.png` renders any page
  through the real egui pipeline with a software rasterizer — no GPU or
  window server needed. `--smoke` runs it across all pages as a CI gate.

## Layout

```
crates/tmog-core/   metrics library: /proc + /sys + NVML collectors,
                    pure parsers, fixture-tested, zero GUI deps
src/                rstmog bin: egui frontend (gui/, graph widget,
                    processes table), sampler (+ background thread),
                    tray/icons, headless capture, CLI
assets/icons/       app icons (dark/light, 16-1024 px + SVG)
packaging/          .desktop entry + installer for KDE/GNOME menus
docs/               headless-rendered page screenshots
```

## Install

The Linux x86_64 build is [rstmog 0.2.0](https://github.com/jcgit1/rsTMOG/releases/tag/v0.2.0). No Rust toolchain is required. Vulkan, the NVIDIA driver, and libxkbcommon stay on your machine. The tray talks to the desktop over the session bus and does not bundle GTK. It needs glibc 2.35 and Linux 3.2 or newer (Ubuntu 22.04 and later).

```sh
cd /tmp
curl -fLO https://github.com/jcgit1/rsTMOG/releases/download/v0.2.0/rstmog-0.2.0-x86_64.AppImage
curl -fLO https://github.com/jcgit1/rsTMOG/releases/download/v0.2.0/rstmog-0.2.0-x86_64.AppImage.sha256
sha256sum -c rstmog-0.2.0-x86_64.AppImage.sha256

mkdir -p "$HOME/.local/bin" \
  "$HOME/.local/share/applications" \
  "$HOME/.local/share/icons/hicolor/256x256/apps"
install -m 755 rstmog-0.2.0-x86_64.AppImage "$HOME/.local/bin/rstmog"

tmp=$(mktemp -d)
( cd "$tmp" && "$HOME/.local/bin/rstmog" --appimage-extract usr/share/icons/hicolor/256x256/apps/rstmog.png )
install -m 644 "$tmp/squashfs-root/usr/share/icons/hicolor/256x256/apps/rstmog.png" \
  "$HOME/.local/share/icons/hicolor/256x256/apps/rstmog.png"
rm -rf "$tmp"

cat > "$HOME/.local/share/applications/rstmog.desktop" << EOF
[Desktop Entry]
Type=Application
Name=rsTMOG
GenericName=Task Manager
Comment=Task Manager OG, rewritten in Rust
Exec=$HOME/.local/bin/rstmog
Icon=rstmog
Terminal=false
Categories=System;Monitor;
Keywords=task;manager;process;monitor;cpu;memory;
EOF
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
```

Open a new terminal and run `rstmog`, or launch rsTMOG from the application menu. If the file reports that FUSE is missing, start it with `APPIMAGE_EXTRACT_AND_RUN=1 rstmog`.

To rebuild the AppImage (Docker, Ubuntu 22.04): `bash packaging/appimage/build.sh`.

## Building

Rust stable (edition 2024; CI pins 1.99.1). The window uses X11 or
Wayland through winit, and the tray is a StatusNotifier item over the
session bus, so the build does not need GTK or libayatana. Install the
packages below and rebuild. The window still opens when the session has
no StatusNotifier watcher; the tray is the only thing that needs one.

Debian / Ubuntu (the same packages the AppImage build installs):

```sh
sudo apt-get install --no-install-recommends \
  build-essential pkg-config \
  libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
  libxkbcommon-dev libwayland-dev
```

Arch / CachyOS:

```sh
sudo pacman -S --needed base-devel pkgconf \
  libx11 libxcursor libxi libxrandr libxkbcommon wayland
```

Fedora:

```sh
sudo dnf install gcc pkgconf-pkg-config \
  libX11-devel libXcursor-devel libXi-devel libXrandr-devel \
  libxkbcommon-devel wayland-devel
```

```sh
cargo build            # debug
cargo run              # GUI
cargo test --workspace # 100 tests, no root, no live-proc deps
bash scripts/ci.sh     # fmt + clippy + tests + self-test + smoke + deny
```

Desktop launcher:

```sh
cargo build --release
BINDIR=$PWD/target/release bash packaging/install-desktop.sh
```

## Releasing

Pushing a version tag builds the AppImage on GitHub's Ubuntu 22.04 runner
and publishes it as a release (`.github/workflows/release.yml`):

```sh
# 1. bump `version` in Cargo.toml (e.g. 0.2.1), merge to main
# 2. tag that commit and push the tag
git tag v0.2.1 && git push origin v0.2.1
```

The workflow refuses a tag that doesn't match `Cargo.toml`, runs the tests,
packages with the pinned, checksum-verified tools, and checks that the
AppImage starts before publishing it with its `.sha256`. Update the version
in the Install section above afterwards.

## CLI

```
rstmog                     GUI
rstmog --self-test         live sanity pass over every collector
rstmog --snapshot          one-shot terminal dashboard
rstmog --capture <page> <out.png>   headless page render (no GPU/X)
rstmog --smoke             render every page headless; non-zero on failure
rstmog --version
```

## Credits

TMOG (Task Manager OG) was created by David W. Plummer, known as Dave
Plummer. rsTMOG is an independent, from-scratch Rust reimplementation of his
original concept and includes none of his code or assets. For the original
TMOG on Linux, see [tmog.org](https://www.tmog.org/rtm/linux.html). Follow
Dave Plummer on [X](https://x.com/davepl1968).

## License

MIT — see [LICENSE](LICENSE). "Task Manager OG"/TMOG is Dave Plummer's
product; this project is not affiliated with or endorsed by him, and no
upstream code or assets are included.
