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
- [x] `core/vmstat.rs` + `core/pressure.rs` (PSI some/full avg lines)
      — one module (`pressure.rs`): vmstat map + PSI parse/collect,
      PSI absent → warn-only in self-test
- [x] `core/process.rs` — `/proc/[pid]/stat` (comm with spaces/parens!),
      status/statm/cmdline (NUL argv, empty cmdline → kernel thread),
      uid→user; fake proc-root end-to-end test. Live check: 778 procs
      collected, matches `ps -e` exactly
- [x] `core/process_table.rs` — per-process CPU% between snapshots; tree
      (ppid chains, orphans float to root, cycle rows surface flat);
      sort/filter model with deterministic pid tiebreak
- [x] `core/diskstats.rs` — `/proc/diskstats` + `/sys/class/block` (model,
      rotational, partition filter), sectors→bytes, rate + util clamp,
      wrap guard (Counter in ring.rs handles wrap/re-baseline)
- [x] `core/netdev.rs` — `/proc/net/dev` rx/tx rates + wrap guard
      (RateTracker); `/sys/class/net/*` speed/operstate/mtu/mac +
      kind classification (lo/wifi/virtual-before-type/wired/other)
- [x] `core/hwmon.rs` — temps/fans/voltages, millidegree scaling, label
      fallback, faulted-sensor skip + `select_cpu_temp()` priority
      (coretemp package → AMD Tctl/Tdie → ARM cpu_thermal → hottest core →
      acpitz; NEVER falls back to hottest — tested). Live: k10temp Tctl
- [x] `core/battery.rs` + `core/rapl.rs` — both ACPI unit variants
      (power_now/energy_now vs voltage×current/charge); RAPL joule-delta
      with `max_energy_range_uj` wraparound + unknown-range refusal.
      One module: `rapl.rs`. Live: battery 100% Full; RAPL unprivileged → warn
- [x] `core/gpu.rs` — NVML via nvml-wrapper (dlopen at runtime, zero
      build-time NVIDIA deps): name, busy%, VRAM used/total, graphics/
      memory clocks, power draw, temp; every field Option (unsupported
      per-GPU fields degrade to dashes, absent driver -> Nvml::new() ==
      None, never panics). Live: RTX 5090 Laptop GPU read fine.
      (power limit via nvml power-limit APIs: deferred, needs
      legacy-functions feature decision)
- [x] `core/sysinfo.rs` — uname-ish (proc/sys/kernel files), os-release,
      boot time (now − uptime), uptime formatting
- [x] `core/zfs.rs` — `/proc/spl/kstat/zfs/arcstats`; ARC-aware memory:
      reclaimable = size − c_min folded into available (btop's view),
      clamped; `mem_composition()` for the stacked bar, segments always
      sum to total. REQUIRED. Fixture-driven: this box has no ZFS module,
      the evildojo 89%-used incident is reproduced and fixed in tests
- [x] `core/settings.rs` — TOML in `dirs::config_dir()/rstmog/config.toml`;
      round-trip tests; unknown keys preserved (merge into existing table,
      atomic temp+rename write); corrupt file = Err (GUI falls back with a
      warning, never silently discards); serde(default) partial files
- [ ] Split workspace: `crates/tmog-core` (lib, zero GUI deps) +
      `crates/rstmog` (bin)

## Phase 2.5 — Sampler + v1 GUI shell (landed ahead of full Phase 4)

- [x] `sampler.rs` — owns all live state; one `tick()` pass per refresh;
      `Snapshot` is the immutable read model pages consume (cpu agg/per
      core, mem raw + ARC-adjusted, procs w/ CPU% deltas, disks, nets,
      temps + CPU headline, GPU, PSI, sysinfo). Unreadable collectors keep
      their previous value (tick never fails)
- [x] `gui.rs` — egui shell: sidebar nav (8 pages), page stack, status
      footer (uptime/load/procs/GPU%/temp), repaint-driver thread on
      refresh_ms. v1 pages in `gui/pages.rs` are live readouts (graphs
      next); processes page shows top-30 by CPU
- [x] `headless.rs` — `rstmog --capture [page] [out.png]`: runs the real
      page code through `egui::Context::run` + `tessellate`, then
      rasterizes meshes in software implementing the egui_glow shader
      contract (frag = vertex×texel, premultiplied, composited over
      opaque). Zero GPU/window needed — review + CI screenshots work
      headless. PNG writer is dependency-free (stored deflate blocks)

## Phase 3 — Actions (test against spawned child processes)

- [ ] `core/actions.rs`: signal primitives; reject pid ≤ 0 (kill(0)/
      kill(-1) are broadcast!) before anything reaches kill()
- [x] `core/actions.rs` — signal primitives + `end_task` (SIGTERM→grace→
      SIGKILL) / force_kill / suspend / resume / get+set_nice, all via a
      tiny unsafe extern block (no libc crate). Every entry point rejects
      pid ≤ 0 with EINVAL *before* the syscall (kill(0)=group, kill(-1)=
      broadcast). Tests spawn real children: compliant child dies on
      SIGTERM; a trap-ignoring child escalates to SIGKILL; suspend reaches
      state T then resumes; renice raise works. The evildojo lesson is
      honored: set_nice reports whatever the kernel says, no EPERM
      assumption.
- [ ] end_task: SIGTERM → bounded wait → SIGKILL (async, unlike C++
      blocking version); force kill
- [ ] suspend/resume (SIGSTOP/SIGCONT), verified via state polling
- [ ] renice get/set — remember the evildojo lesson: lowering succeeds for
      unprivileged callers within RLIMIT_NICE + own-nice floor; tests
      predict, not assume EPERM

## Phase 4.5 — Polling cost (user-flagged 2026-10-31)

Measured on llm (debug build): full `tick()` = ~62 ms avg / 156 ms worst
across ~700 pids — the process walk (4 files × pid) dominates. Decisions:

- [x] **Fast tier now:** `refresh_ms` (default 1000; 500/1000/2000 ladder)
      is read from settings at startup; the repaint driver and sample
      cadence follow it.
- [x] Background sampler thread (`bg.rs`): `SamplerHandle` owns the
      `Sampler` on a named thread and publishes an `Arc<Snapshot>` per
      cycle; the UI clones the Arc under a short read lock, so a ~60 ms
      (worst 156 ms) process walk can never hitch the paint path. Seeds
      one synchronous tick before the thread starts (no blank first
      frame); adaptive sleep handles tick-overrun cadence gracefully;
      `Drop`/`shutdown` sets the stop flag and joins (idempotent —
      verified no thread/process leaks on the capture path). Repaint
      driver tracks the shared `AtomicU32` refresh handle so a runtime
      cadence change takes effect within ~100 ms. `set_refresh_ms`
      clamps to 250..5000 (settings-dialog hook).
- [ ] Settings dialog lets the user change the poll rate at runtime
      (Phase 4 dialog item; persisted via core/settings — the runtime
      plumbing `set_refresh_ms` is now in place)
- [ ] Medium tier: process ROWS every 3rd tick; CPU% deltas computed
      against the last real sample so they stay honest between.
- [ ] Slow tier: status/statm/cmdline only for visible rows +
      selected/followed pid (GUI tells the Sampler which pids it shows).

## Phase 4 — GUI (egui). Each page: renders from Sampler snapshots only

- [ ] App shell: sidebar nav, page stack, status footer, tick loop
      (`Sampler` on a thread or `wg.tick` timer; refresh 0.5/1/2 s)
- [x] Graph widget: scrolling history (newest pinned right), autoscale
      (peak-hold follower: rises fast, falls slow), quarter grid +
      percent-mode 0/25/50/75/100 labels; `window_to_ui` mapping is a
      pure fn unit-tested without egui. Traces draw as a fill-under
      polygon + solid quad segments (NOT a stroked Shape::line — stroked
      paths tessellate to feather-AA quads that the software capture
      rasterizer clips out; solid quads render identically in the GPU
      path and headless capture). Histories live in the Sampler
      (cpu/mem/temp per-sample; per-disk and per-net read/write byte-rate
      series, ~4-min retention). Wired into Summary (cpu + temp), CPU
      (aggregate), Memory (used%), Disks (per-disk r/w), Network (rx/tx)
- [x] Processes page: sortable/filterable table (header-click, live filter
      over name/cmdline/user/pid), selection by pid survives refresh,
      tree toggle (└ indents via core tree_rows), follow mode
      (scroll_to_me center on the selected row), context menu
      (end/force/suspend-resume by state/priority ladder High..Idle/copy
      cmdline via egui clipboard), zombie rows red / stopped amber.
      Pending actions execute via core::actions in App::run_proc_action;
      failures go to stderr (a toast is a Phase 5 nicety). Sort/tree/
      follow persist to settings on window close. `end_task` is currently
      blocking on the UI thread (documented; short-lived + user-initiated,
      async follow-up noted)
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
