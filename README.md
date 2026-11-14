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

## Download

Linux x86_64 AppImages are on the [release page](https://github.com/jcgit1/rsTMOG/releases). No Rust toolchain is required. Vulkan and the NVIDIA driver stay on your machine; the AppImage bundles GTK 3 for the tray.

```sh
chmod +x rstmog-*-x86_64.AppImage
./rstmog-*-x86_64.AppImage
```

Rebuild it with Docker (Ubuntu 22.04, so the binary still runs on older glibc):

```sh
bash packaging/appimage/build.sh
```

## Building

Rust stable (edition 2024; CI pins 1.99.1). On Linux the tray stack
(`tray-icon` → `libappindicator` → `gtk` 0.18 → `glib-sys`) probes for
system GLib with `pkg-config` while compiling. A missing `glib-2.0.pc`
stops the build here:

```
warning: glib-sys@0.18.1:
error: failed to run custom build command for glib-sys v0.18.1
```

The warning line is blank because `glib-sys` emits the `pkg-config` error
as one `cargo:warning` whose text starts with a blank line, and that first
line is all Cargo prints. A missing `pkg-config` binary and a missing
`glib-2.0.pc` both land on this same message. Install the packages below
and rebuild. The window still opens when the session has no tray; these
packages are what the compile needs. libayatana is the shared library the
tray dlopens at runtime.

Debian / Ubuntu (the same packages the AppImage build installs):

```sh
sudo apt-get install --no-install-recommends \
  build-essential pkg-config \
  libgtk-3-dev libayatana-appindicator3-dev \
  libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
  libxkbcommon-dev libwayland-dev
```

Arch / CachyOS:

```sh
sudo pacman -S --needed base-devel pkgconf gtk3 libayatana-appindicator \
  libx11 libxcursor libxi libxrandr libxkbcommon wayland
```

Fedora:

```sh
sudo dnf install gcc pkgconf-pkg-config gtk3-devel \
  libayatana-appindicator-gtk3 \
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
