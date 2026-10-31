# rsTMOG — build plan

Goal: Task Manager OG rewritten in Rust, Linux-only. Same behavioral target
as [evildojo-tmog](https://github.com/mikedesu/evildojo-tmog) (MIT), which
stays cloned at `~/repos/evildojo-tmog` as the read-only behavioral
reference. This is a rewrite, not a translation: idiomatic Rust, no
line-by-line porting.

Architecture carries over the one good idea from the C++ project, with Rust
mechanics:

- **`tmog-core`** — metrics library over `/proc`, `/sys`, NVML. Every
  collector is a pure parser (`&str` / fixture path in, typed struct out)
  so tests need no root and no live procfs. Errors are `thiserror` enums;
  callers use `anyhow` at the top.
- **`rstmog` (bin)** — egui/eframe frontend. A `Sampler` owns all state and
  ring-buffer histories on a background tick; pages only read snapshots.

Testing convention: `cargo test` green at every checkbox. Fixture text/trees
under `tests/fixtures/`. Live sanity pass behind `--self-test` (exit 0/1),
GUI construction check behind `--smoke` (headless: skip GPU, still run the
tick loop). `cargo fmt` + `cargo clippy --all-targets -- -D warnings` clean.

Crate layout: single crate for now (`src/core/`, `src/gui.rs`); split into a
cargo workspace (`crates/tmog-core` + `crates/rstmog`) at the end of Phase 2
once the core API settles — splitting earlier churns more than it helps.

## Known environment (verified 2026-10-31, llm / CachyOS)

- rustc/cargo 1.99.1 stable + rustfmt/clippy/rust-analyzer; cargo-audit/deny/watch;
  cargo-nextest install FAILED on this toolchain — use plain `cargo test`.
- egui/eframe 0.33 (wgpu), procfs, nvml-wrapper, serde/toml — all compile,
  window opens. Two gotchas are commented in Cargo.toml: eframe's `x11`/
  `wayland` features are not in its default set; wgpu's `vulkan`/`gles`
  backends must be enabled from our manifest (eframe pins wgpu with
  default-features=false).
- NVIDIA driver present (`libnvidia-ml.so`); ZFS ARC matters on this fleet —
  the ARC-aware memory accounting bug from evildojo (kernel MemAvailable
  ignores ARC) is a REQUIRED behavior, not a stretch goal.

## Phase 0 — Foundation (done)

- [x] Repo + remotes (Forgejo `origin`, GitHub `github`), README, .gitignore
- [x] Toolchain + Cargo.toml with GUI/window verified building and opening
- [x] First collector as pattern proof: `core/cpu.rs` (`/proc/stat` line
      parser + tests)

## Phase 1 — Core plumbing (code + unit tests)

- [x] `core/ring.rs` — history ring buffer + `RateTracker` with injected
      clock (tests use fake clock): capacity eviction, min/max/latest,
      re-baseline on backwards counters. Design note: a u64 wrap and a
      subsystem restart are indistinguishable from two samples — we
      re-baseline (Reset) rather than fabricate a wrap spike
- [x] `core/fs.rs` — read file / u64 / sorted dir-list helpers taking a
      root `Path` (fixture roots in tests, `/` in prod); missing optional
      dirs → empty list
- [x] `--self-test` and `--snapshot` CLI in main.rs (plain arg match),
      `--version`; broken-pipe-safe printing (piping to `head` exits 0)
- [x] CPU: delta → busy% between two `CpuJiffies` samples (zero-delta
      guard, backwards guard, iowait = not busy)

## Phase 2 — Metrics collectors (one checkbox = parser + fixtures + wiring
into `--self-test`; hardware-dependent ones warn, never fail)

- [x] `core/meminfo.rs` — `/proc/meminfo` parse; used%; `MemAvailable`
      fallback (free+buffers+cached)
- [x] `core/loadavg.rs`, `core/uptime.rs`
- [x] `core/cpuinfo.rs` — model, topology, cpufreq sysfs tree (fake root in
      tests), governor/driver
- [ ] `core/vmstat.rs` + `core/pressure.rs` (PSI some/full avg lines)
- [ ] `core/process.rs` — `/proc/[pid]/stat` (comm with spaces/parens!),
      status/statm/cmdline (NUL argv, empty cmdline → kernel thread),
      uid→user; fake proc-root end-to-end test
- [ ] `core/process_table.rs` — per-process CPU% between snapshots; tree
      (ppid chains, orphans float to root, cycle rows surface flat);
      sort/filter model with deterministic pid tiebreak
- [ ] `core/diskstats.rs` — `/proc/diskstats` + `/sys/class/block` (model,
      rotational, partition filter), sectors→bytes, rate + util clamp,
      wrap guard
- [ ] `core/netdev.rs` — `/proc/net/dev` rx/tx rates + wrap guard;
      `/sys/class/net/*` (speed, operstate, mtu, mac, kind)
- [ ] `core/hwmon.rs` — temps/fans/voltages, millidegree scaling, label
      fallback, faulted-sensor skip + `select_cpu_temp()` priority
      (coretemp package → AMD Tctl/Tdie → ARM cpu_thermal → hottest core →
      acpitz)
- [ ] `core/battery.rs` + `core/rapl.rs` — both ACPI unit variants; RAPL
      joule-delta with `max_energy_range_uj` wraparound (warn-only here:
      unprivileged desktop, no battery)
- [ ] `core/gpu.rs` — NVML via nvml-wrapper: busy%, VRAM, clocks, power
      draw/limit; absent driver → dash values, no crash, no build dep
- [ ] `core/sysinfo.rs` — uname, hostname, os-release, boot time
- [ ] `core/zfs.rs` — `/proc/spl/kstat/zfs/arcstats`; ARC-aware memory:
      reclaimable = size − c_min folded into cache/available (btop's
      view); `mem_composition()` for the stacked bar. REQUIRED
- [ ] `core/settings.rs` — TOML in `dirs::config_dir()/rstmog/config.toml`;
      round-trip tests; unknown keys preserved or tolerated
- [ ] Split workspace: `crates/tmog-core` (lib, zero GUI deps) +
      `crates/rstmog` (bin)

## Phase 3 — Actions (test against spawned child processes)

- [ ] `core/actions.rs`: signal primitives; reject pid ≤ 0 (kill(0)/
      kill(-1) are broadcast!) before anything reaches kill()
- [ ] end_task: SIGTERM → bounded wait → SIGKILL (async, unlike C++
      blocking version); force kill
- [ ] suspend/resume (SIGSTOP/SIGCONT), verified via state polling
- [ ] renice get/set — remember the evildojo lesson: lowering succeeds for
      unprivileged callers within RLIMIT_NICE + own-nice floor; tests
      predict, not assume EPERM

## Phase 4 — GUI (egui). Each page: renders from Sampler snapshots only

- [ ] App shell: sidebar nav, page stack, status footer, tick loop
      (`Sampler` on a thread or `wg.tick` timer; refresh 0.5/1/2 s)
- [ ] Graph widget: scrolling history (newest right), autoscale, quarter
      grid, dual-trace (rx/tx, read/write); mapping math unit-tested
- [ ] Processes page: sortable/filterable table, selection by pid survives
      refresh, tree toggle (`└─` indents), follow mode, context menu
      (end/force/suspend/priority ladder/copy cmdline) with errors shown
- [ ] CPU page: aggregate + per-core grid, topology header, freq/governor
- [ ] Memory page: in-use graph, stacked composition bar (ARC segment),
      PSI lines
- [ ] Disk page: per-disk dual-trace graphs, identity line, busy%
- [ ] Network page: per-iface rx/tx + totals, kind/speed/MAC
- [ ] Thermals page: CPU-package trend + hwmon list grouped by chip,
      headline sensor bolded
- [ ] System page: hostname/distro/kernel/CPU/mem/boot/uptime/load
- [ ] Themes: Dark default + phosphor Green/Amber/Blue + Mono + Light
      (egui Visuals generated from a Theme struct); persisted
- [ ] Settings dialog + persistence of theme/refresh/sort/tree/follow

## Phase 5 — Fit & finish

- [ ] `--smoke`: headless tick loop + all pages constructed (CI-able)
- [ ] Perf: tick cost budget (<2 ms sampling at 2 Hz on this box); no
      allocations in the paint path beyond egui's own
- [ ] cargo-audit/deny wired into a CI script; fmt+clippy gate
- [ ] README rewrite (build, screenshots, desktop entry); LICENSE (MIT)
- [ ] Packaging pass on this box: release build, `.desktop` entry

## Deliberate non-goals (v1)

Windows/macOS; Services/Users pages (no libsystemd/D-Bus); internet
benchmark; disk-space scanner; ncurses TUI (maybe later — core is
frontend-agnostic by design).
