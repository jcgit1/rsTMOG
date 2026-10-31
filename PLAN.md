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
- [x] Split workspace: `crates/tmog-core` (lib, zero GUI deps — nvml/
      serde/toml/dirs only) + `rstmog` (bin: egui frontend, sampler, tray,
      CLI). Fixture tree moved with the tests to crates/tmog-core/tests/.
      100 tests total (92 core + 8 bin), all green

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

- [x] `core/actions.rs` (first entry above; the duplicate original
      checkbox is superseded): signal primitives; reject pid ≤ 0 (kill(0)/
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
- [x] end_task: SIGTERM → bounded wait → SIGKILL — DONE but *blocking*
      (spec said async; async remains an open follow-up). Force kill done.
- [x] suspend/resume (SIGSTOP/SIGCONT), verified via state polling
- [x] renice get/set — evildojo lesson honored: no EPERM assumption, the
      kernel verdict is reported via errno (lowering succeeds for
      unprivileged callers within RLIMIT_NICE + own-nice floor)

## Phase 4.9 — Icons (user-supplied package, 2026-10-31)

Icon package (glossy ice-blue serif "T", light + dark variants, 16–1024 px
PNGs + SVGs) lives in `assets/icons/{light,dark}/`.

- [x] Window/taskbar icon: theme-aware (`ViewportBuilder::with_icon`,
      64 px); dark/light picked from the persisted theme, falling back to
      the OS `dark-light` preference
- [x] System tray (`tray-icon` + GTK on a dedicated thread — Linux needs
      gtk::init + a GTK main loop on the owning thread; muda items are Rc/
      !Send so the handle stays on the tray thread): icon + tooltip +
      menu Open / Refresh rate 500|1000|2000 ms (checked = current) /
      Quit. Events bridge to the eframe loop over channels; Open shows+
      focuses, Quit closes (persisting settings), rate changes flow
      through sampler.set_refresh_ms + settings save + tray tick sync.
      Honest degradation: no StatusNotifier/GTK → app runs with no tray
      (spawn returns None within 2 s).
- [x] Taskbar identity: `with_app_id("rstmog")` + full title
      "rsTMOG — Task Manager OG" so KDE matches the .desktop entry
- [x] packaging/rstmog.desktop (Name/GenericName/Categories/Keywords +
      Icon=rstmog) and packaging/install-desktop.sh (installs the 256 px
      icon to hicolor + the .desktop with the absolute Exec path;
      verified installed for this user). Upstream README note honored:
      desktop icon does NOT auto-switch with the DE theme — we ship the
      dark variant
- Note: tray-icon built with default-features=false (libxdo off — lib not
  on this box, accelerators unused). gtk/glib pinned to the 0.18 series
  matching muda 0.16.

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
- [x] Settings dialog (⚙ button in the sidebar): theme picker
      (dark/green/amber/blue/mono/light, applied live), poll-rate radio
      ladder 500/1000/2000 (reprograms the sampler thread + tray ticks +
      repaint cadence), Processes defaults (tree/follow, synced to the
      live table state); changes save to TOML immediately with an inline
      error row on failure. Also closes the gap for tray-driven rate
      persistence
- [x] Medium tier (rows + hwmon every 3rd tick): CPU% computed against
      the last real sample over the exact tick window (agg_ticks minus
      the row-pass baseline), so updates reflect the full 3-tick span.
      Measured (release, llm, ~740 procs): light tick 0.25 ms vs full
      37 ms; average tick 37 ms → ~12 ms. RSTMOG_ROW_EVERY=1 restores
      the old always-full behavior for A/B.
      Debugging note: the first measurements looked wrong (light ticks
      "35 ms", rows "0.25 ms") because (a) the real costs were hwmon
      ~29 ms + process walk ~8 ms (gpu's 16 ms in --bench-phases is
      that bench re-initializing NVML per call; the sampler holds one handle), NOT the process walk (my earlier
      assumption was wrong), and (b) the bench classified ticks BEFORE
      ticking. Both fixed; --bench-phases added for per-collector cost.
- [x] Slow tier: process::collect_one split into collect_stat_only (fast)
      + enrich_one (status/statm/cmdline/user); the Processes page sends
      the visible slice (first 40 of the current sort) + selected pid to
      the sampler each frame over a channel; enrichment runs in batches
      of 25/tick (fast-scroll backlog drains over a few ticks). Rows
      carry rss/cmdline/user forward between passes so columns never
      blank out.

## Phase 4 — GUI (egui). Each page: renders from Sampler snapshots only

- [x] App shell: sidebar nav (8 pages), page stack, status footer
      (uptime/load/procs/GPU%/temp), background tick loop at the user's
      refresh cadence (Phase 4.5 `SamplerHandle` thread)
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
- [x] CPU page: aggregate graph + per-core grid, topology header (model/
      threads/cores/sockets), freq/governor/driver line
- [x] Memory page: in-use graph, stacked composition bar (in-use / zfs
      arc / cached / buffers / free via core mem_composition), readout
      grid, PSI line
- [x] Disk page: per-disk dual-trace graphs + identity line (model,
      rotational kind); cumulative counters. Live busy% needs a util
      sampler tick — small follow-up, noted
- [x] Network page: per-iface rx/tx graphs + identity (kind, speed, MAC)
      and page totals row
- [x] Thermals page: CPU-package headline + hwmon list grouped by chip,
      temps/fans/voltages (landed in the v1 shell; the package *trend
      graph* rides the hist_temp series already in the Summary page —
      moving a copy onto Thermals is a quick follow-up)
      headline sensor bolded (partially: headline shown; bold-in-list TODO)
- [x] System page: hostname/distro/kernel/arch/CPU/mem (landed in the
      v1 shell; boot-time line is the remaining nit)
- [x] Themes: dark default + phosphor green/amber/blue + mono + light
      (egui Visuals via theme_visuals()); live switch in the settings
      dialog; persisted to TOML. Live window icon follows the theme too
- [x] Settings dialog + persistence of theme/refresh/tree/follow (sort persists on window close)

## Phase 5 — Fit & finish

- [x] `--smoke`: headless run of every page through the real egui pass +
      software rasterizer (see below), exits non-zero on failure
- [~] Perf: measured (release, llm, ~735 procs): tick avg ~44 ms,
      worst ~143 ms. The <2 ms budget was unrealistic for a full walk —
      the background-thread design already keeps it off the paint path;
      real fix is the medium/slow polling tiers (open). No allocations
      added to the paint path beyond egui's own (snapshots are Arc
      clones)
- [x] CI gate: `scripts/ci.sh` (fmt --check, clippy -D warnings, tests,
      self-test, --smoke of all 8 pages, cargo-deny licenses/sources
      with a curated deny.toml, cargo-audit as a warning — 7 open
      advisories today are all transitive (zvariant/glib etc., flagged
      unmaintained/unsound, none in a path we rely on for security);
      none are deny-level yet. Ran end-to-end: CI PASSED
- [x] README rewritten (feature list, workspace layout, build/CI/CLI,
      desktop entry, headless capture); LICENSE (MIT, with the TMOG
      non-affiliation + evildojo attribution note)
- [x] Packaging pass on this box: release build (30 s, self-test + smoke
      green), `.desktop` + hicolor icon installed for this user with
      Exec pointing at target/release/rstmog

## Deliberate non-goals (v1)

Windows/macOS; Services/Users pages (no libsystemd/D-Bus); internet
benchmark; disk-space scanner; ncurses TUI (maybe later — core is
frontend-agnostic by design).
