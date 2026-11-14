//! Sampler: the single owner of live state. One tick = one pass over all
//! collectors; pages only ever read the resulting immutable `Snapshot`.
//! Runs on eframe's request_repaint timer (refresh_ms), never blocks the
//! paint path.

use std::path::PathBuf;
use std::time::Instant;
use tmog_core::cpu::{self, CpuJiffies};
use tmog_core::diskstats::DiskCounters;
use tmog_core::gpu::{GpuInfo, Nvml};
use tmog_core::loadavg::LoadAvg;
use tmog_core::meminfo::Meminfo;
use tmog_core::netdev::NetDevCounters;
use tmog_core::pressure::PsiAvg;
use tmog_core::process_table::Row;
use tmog_core::sysinfo::SysInfo;
use tmog_core::zfs::ArcStats;

/// Everything a page can read, computed once per tick.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub now: Option<Instant>,
    /// monotonic tick counter, one per `tick()` since start
    pub tick: u64,
    pub aggregate: Option<f64>,
    pub per_core: Vec<(String, Option<f64>)>,
    pub cpu_ticks_delta: u64,
    pub mem: Option<Meminfo>,
    pub mem_arc_adjusted: Option<Meminfo>,
    pub arc: Option<ArcStats>,
    pub load: Option<LoadAvg>,
    pub uptime_s: Option<f64>,
    pub procs: Vec<Row>,
    pub disks: Vec<DiskCounters>,
    /// live util% per disk (name, 0..=100 or None on the first sample)
    pub disk_util: Vec<(String, Option<f64>)>,
    pub nets: Vec<NetDevCounters>,
    pub temps: Vec<tmog_core::hwmon::Reading>,
    pub cpu_temp: Option<tmog_core::hwmon::Reading>,
    pub gpu: Option<GpuInfo>,
    pub psi_memory: Option<(PsiAvg, Option<PsiAvg>)>,
    pub psi_io: Option<(PsiAvg, Option<PsiAvg>)>,
    pub sysinfo: Option<SysInfo>,
    /// Slow-changing identity data, refreshed every IDENTITY_EVERY ticks
    /// (or when a new device appears) so pages never do I/O while painting.
    pub disk_ids: std::collections::HashMap<String, tmog_core::diskstats::DiskIdentity>,
    pub net_ids: std::collections::HashMap<
        String,
        (tmog_core::netdev::NetIdentity, tmog_core::netdev::NetKind),
    >,
    pub cpuinfo: Option<tmog_core::cpuinfo::CpuInfo>,
    /// cpufreq of the first present cpu (current MHz moves: every tick)
    pub cpufreq0: Option<tmog_core::cpuinfo::Cpufreq>,
    /// block device backing `/` (from /proc/mounts)
    pub root_disk: Option<String>,

    // ---- histories (sample_index == Snapshot.tick) -------------------
    /// aggregate cpu busy %
    pub hist_cpu: Vec<(u64, f64)>,
    /// cpu package temperature °C
    pub hist_temp: Vec<(u64, f64)>,
    /// GPU utilization % (None = no NVML/GPU present)
    pub hist_gpu: Option<Vec<(u64, f64)>>,
    /// GPU temperature °C (None = no NVML/GPU present). NVML reports Celsius.
    pub hist_gpu_temp: Option<Vec<(u64, f64)>>,
    /// memory used %
    pub hist_mem: Vec<(u64, f64)>,
    /// per-physical-disk (name, read B/s, write B/s) sampled rates
    pub hist_disk: RateHistory,
    /// per-interface (name, rx B/s, tx B/s)
    pub hist_net: RateHistory,

    /// Which pids were last seen running on each CPU. A running snapshot
    /// across ticks: each tick merges in the tasks whose `last_cpu` we
    /// just observed; old members whose pid vanished are pruned when the
    /// row pass runs. Approximate by nature (Linux records only where a
    /// task was *last seen*, not per-core time) — see process::last_cpu.
    pub core_usage: std::collections::HashMap<u32, Vec<u32>>,
}

const HIST_LEN: usize = 240; // ~4 min at 1 Hz

/// Per-device rate history: (device name, series A, series B) where the
/// series are (tick, bytes_per_second) pairs — read/rx and write/tx.
pub type RateHistory = Vec<(String, Vec<(u64, f64)>, Vec<(u64, f64)>)>;

/// Mutable sampling state between ticks (previous jiffies, passwd cache,
/// NVML handle, rate baselines).
pub struct Sampler {
    root: PathBuf,
    prev_cpu: Vec<CpuJiffies>,
    prev_ticks: u64,
    passwd: std::collections::HashMap<u32, String>,
    nvml: Option<Nvml>,
    last_tick: Option<Instant>,
    tick: u64,
    // rate baselines: name -> (last_bytes, last_us)
    prev_disk_read: std::collections::HashMap<String, (u64, u64)>,
    prev_disk_write: std::collections::HashMap<String, (u64, u64)>,
    /// busy_ms + monotonic time per disk at the last sample (util% window)
    prev_disk_busy: std::collections::HashMap<String, (u64, Instant)>,
    /// monotonic origin for rate timestamps (wall clock can step backwards)
    start: Instant,
    prev_net_rx: std::collections::HashMap<String, (u64, u64)>,
    prev_net_tx: std::collections::HashMap<String, (u64, u64)>,
    /// cumulative aggregate cpu ticks (USER_HZ) at the last row pass and
    /// now, so the per-process CPU% denominator spans exactly the ticks
    /// between ROW passes
    agg_ticks: u64,
    row_pass_ticks: u64,
    last_was_row: bool,
    last_hwmon: bool,
    /// pids the GUI asked to have fully enriched (visible slice + selected)
    detail_pids: Vec<u32>,
    /// rotating position in `detail_pids`, so a list longer than
    /// DETAIL_BATCH is covered over successive ticks
    detail_cursor: usize,
    /// a request arrived since the last tick (else the GUI left the page)
    detail_requested: bool,
    /// first present cpu, for the cpufreq readout (found at identity refresh)
    cpu0: Option<usize>,
    pub snapshot: Snapshot,
}

/// Refresh process rows every Nth tick (medium tier). A/B testing hook:
/// RSTMOG_ROW_EVERY=1 restores the always-full walk for comparisons.
pub fn row_every() -> u64 {
    match std::env::var("RSTMOG_ROW_EVERY").as_deref() {
        Ok("1") => 1,
        _ => 3,
    }
}

/// Max pids enriched per tick (slow tier batch cap).
const DETAIL_BATCH: usize = 25;

impl Sampler {
    pub fn new() -> Self {
        Self::with_root(PathBuf::from("/"))
    }

    /// Fixture-root constructor (tests / self-checks against trees).
    pub fn with_root(root: PathBuf) -> Self {
        let passwd = std::fs::read_to_string(root.join("etc/passwd"))
            .map(|s| tmog_core::process::parse_passwd(&s))
            .unwrap_or_default();
        Sampler {
            root,
            prev_cpu: Vec::new(),
            prev_ticks: 0,
            passwd,
            nvml: Nvml::new(),
            last_tick: None,
            tick: 0,
            prev_disk_read: Default::default(),
            prev_disk_write: Default::default(),
            prev_disk_busy: Default::default(),
            start: Instant::now(),
            prev_net_rx: Default::default(),
            prev_net_tx: Default::default(),
            agg_ticks: 0,
            row_pass_ticks: 0,
            last_was_row: false,
            last_hwmon: false,
            detail_pids: Vec::new(),
            detail_cursor: 0,
            detail_requested: false,
            cpu0: None,
            snapshot: Snapshot::default(),
        }
    }

    /// Bytes/sec between `now` and the stored baseline; updates baseline.
    /// First observation of a name seeds (rate 0). Backwards counters
    /// (device removed/restarted) re-seed to 0 rather than spike.
    fn rate(
        map: &mut std::collections::HashMap<String, (u64, u64)>,
        name: &str,
        bytes: u64,
        now_us: u64,
    ) -> f64 {
        match map.get(name) {
            None => {
                map.insert(name.to_string(), (bytes, now_us));
                0.0
            }
            Some(&(pb, pt)) => {
                let dt = now_us.saturating_sub(pt).max(1);
                let r = if bytes >= pb {
                    (bytes - pb) as f64 * 1_000_000.0 / dt as f64
                } else {
                    0.0
                };
                map.insert(name.to_string(), (bytes, now_us));
                r
            }
        }
    }

    /// Did the last completed tick refresh process rows? (bench/classify)
    pub fn last_tick_was_row(&self) -> bool {
        self.last_was_row
    }

    /// Ask the sampler to fully populate (rss/cmdline/user) these pids on
    /// the next tick — the GUI calls this with the visible table slice
    /// plus the selected pid (slow tier).
    pub fn request_detail(&mut self, pids: &[u32]) {
        // The cursor is deliberately NOT reset: the visible slice re-sorts
        // every row pass, and restarting at 0 each time starved the tail.
        self.detail_pids = pids.to_vec();
        self.detail_requested = true;
    }

    /// One collection pass. Cheap and panic-tolerant: any collector that
    /// fails leaves its previous snapshot value in place.
    pub fn tick(&mut self) {
        let now = Instant::now();
        // owned copy so `self` can be borrowed mutably below
        let root_buf = self.root.clone();
        let root = root_buf.as_path();

        // ---- cpu ----------------------------------------------------
        if let Ok(text) = tmog_core::fs::read_under(root, "proc/stat") {
            let rows: Vec<CpuJiffies> = text.lines().filter_map(cpu::parse_cpu_line).collect();
            if !self.prev_cpu.is_empty() {
                let mut per_core = Vec::new();
                let mut agg = None;
                for (p, n) in self.prev_cpu.iter().zip(rows.iter()) {
                    let pct = cpu::busy_percent(p, n);
                    if n.name == "cpu" {
                        agg = pct;
                    } else {
                        per_core.push((n.name.clone(), pct));
                    }
                }
                self.snapshot.aggregate = agg;
                self.snapshot.per_core = per_core;
                // aggregate ticks delta for per-process CPU%
                if let (Some(p), Some(n)) = (
                    self.prev_cpu.iter().find(|r| r.name == "cpu"),
                    rows.iter().find(|r| r.name == "cpu"),
                ) {
                    self.snapshot.cpu_ticks_delta = n.total().saturating_sub(p.total());
                }
                // cumulative aggregate ticks for the row-pass CPU% window
                if let Some(n) = rows.iter().find(|r| r.name == "cpu") {
                    self.agg_ticks = n.total();
                }
            }
            self.prev_ticks = rows
                .iter()
                .find(|r| r.name == "cpu")
                .map(|r| r.total())
                .unwrap_or(0);
            if self.row_pass_ticks == 0 {
                self.row_pass_ticks = self.agg_ticks;
            }
            self.prev_cpu = rows;
        }

        // ---- meminfo + zfs arc --------------------------------------
        self.snapshot.arc = tmog_core::zfs::collect(root);
        if let Ok(m) = tmog_core::meminfo::collect(root) {
            self.snapshot.mem_arc_adjusted = Some(tmog_core::zfs::arc_adjusted_meminfo(
                &m,
                self.snapshot.arc.as_ref(),
            ));
            self.snapshot.mem = Some(m);
        }

        // ---- load / uptime ------------------------------------------
        self.snapshot.load = tmog_core::loadavg::collect_loadavg(root).ok();
        self.snapshot.uptime_s = tmog_core::loadavg::collect_uptime(root).ok();

        // ---- processes (medium tier: every 3rd tick) -----------------
        // The full walk dominates tick cost — ~100 ms of the ~140 ms
        // worst case on this box (measured via the sampler). Rows
        // refresh every ROW_EVERY-th tick; between refreshes the table
        // keeps the previous rows. CPU% is computed against the LAST
        // actual sample and the ticks accumulated since then, so the
        // update reflects the whole interval, not one 100 ms slice.
        self.last_was_row =
            self.tick.wrapping_add(1).is_multiple_of(row_every()) || self.snapshot.procs.is_empty();
        if self.last_was_row
            && let Ok(pids) = read_pids(root)
        {
            {
                let mut rows: Vec<Row> = Vec::with_capacity(pids.len());
                for pid in pids {
                    // cheap tier: stat only; status/statm/cmdline are
                    // filled on demand for visible rows (slow tier below)
                    if let Ok(mut p) =
                        tmog_core::process::collect_row(root, pid, PAGE_BYTES, &self.passwd)
                    {
                        // carry enriched detail forward if the pid was
                        // known last pass (cheap: no syscalls)
                        if let Some(prev_full) =
                            self.snapshot.procs.iter().find(|r| r.proc.pid == pid)
                        {
                            if p.rss_bytes == 0 {
                                p.rss_bytes = prev_full.proc.rss_bytes;
                            }
                            if p.cmdline.is_empty() {
                                p.cmdline = prev_full.proc.cmdline.clone();
                            }
                            if p.user.is_none() {
                                p.user = prev_full.proc.user.clone();
                            }
                        }
                        let prev = self
                            .snapshot
                            .procs
                            .iter()
                            .find(|r| r.proc.pid == pid)
                            .map(|r| &r.proc);
                        // aggregate ticks accumulated SINCE the previous
                        // row pass (since_ticks covers all ticks, so use
                        // it directly — it equals the row window)
                        // saturating: totals can step back (containers with a
                        // virtualized /proc/stat); never underflow/panic
                        let total_delta = self.agg_ticks.saturating_sub(self.row_pass_ticks).max(1);
                        let cpu_pct = prev.and_then(|pv| {
                            tmog_core::process_table::cpu_percent(pv, &p, total_delta)
                        });
                        rows.push(Row {
                            proc: p,
                            cpu: cpu_pct,
                        });
                    }
                }
                self.snapshot.procs = rows;
                self.row_pass_ticks = self.agg_ticks;
                // rebuild core attribution from this pass's observations
                let mut usage: std::collections::HashMap<u32, Vec<u32>> =
                    std::collections::HashMap::new();
                for r in &self.snapshot.procs {
                    usage.entry(r.proc.last_cpu).or_default().push(r.proc.pid);
                }
                for v in usage.values_mut() {
                    v.sort_unstable();
                }
                self.snapshot.core_usage = usage;
            }
        }

        // ---- disks / nets -------------------------------------------
        if let Ok(d) = tmog_core::diskstats::collect(root) {
            // live util% per disk: busy_ms delta over real elapsed time
            // since the previous disk sample. First sample / restarted
            // counters → None (dash), never a fabricated spike.
            let mut utils = Vec::with_capacity(d.len());
            for c in &d {
                let u = self
                    .prev_disk_busy
                    .get(&c.name)
                    .and_then(|&(prev_busy, prev_at)| {
                        disk_util(prev_busy, prev_at, c.busy_ms, now)
                    });
                self.prev_disk_busy.insert(c.name.clone(), (c.busy_ms, now));
                utils.push((c.name.clone(), u));
            }
            self.snapshot.disk_util = utils;
            self.snapshot.disks = d;
        }
        if let Ok(n) = tmog_core::netdev::collect(root) {
            self.snapshot.nets = n;
        }

        // ---- hwmon (medium tier: every 3rd tick) ----------------------
        // Measured cost ~29 ms: it walks every hwmon chip and probes
        // every *_label/_input file. Temperatures move on a seconds
        // scale, so a 3-tick window loses nothing visible.
        self.last_hwmon =
            self.tick.wrapping_add(1).is_multiple_of(row_every()) || self.snapshot.temps.is_empty();
        if self.last_hwmon {
            let temps = tmog_core::hwmon::collect(root);
            if !temps.is_empty() {
                self.snapshot.cpu_temp = tmog_core::hwmon::select_cpu_temp(&temps).cloned();
                self.snapshot.temps = temps;
            }
        }

        // ---- gpu (light tier: every tick, but reuse one handle) ------
        if let Some(nvml) = &self.nvml {
            self.snapshot.gpu = nvml.sample(0);
        }

        // ---- psi ----------------------------------------------------
        self.snapshot.psi_memory = tmog_core::pressure::collect_pressure(root, "memory")
            .ok()
            .flatten();
        self.snapshot.psi_io = tmog_core::pressure::collect_pressure(root, "io")
            .ok()
            .flatten();

        // ---- sysinfo (cheap enough per tick; could cache) -----------
        self.snapshot.sysinfo = Some(tmog_core::sysinfo::collect(root));

        // ---- slow tier: enrich the pids the GUI cares about ---------
        // status/statm/cmdline only for the visible slice + selection.
        // Batched: at most DETAIL_BATCH per tick so a burst of requests
        // (fast scroll) never spikes a tick; a backlog is simply served
        // over the next few ticks.
        // A list longer than the batch is walked round-robin across ticks
        // (the GUI re-sends ~41 pids every frame; serving only the first 25
        // left the rest of the visible rows permanently blank).
        if !self.detail_pids.is_empty() {
            let n = self.detail_pids.len();
            let start = self.detail_cursor % n;
            let take = DETAIL_BATCH.min(n);
            for k in 0..take {
                let pid = self.detail_pids[(start + k) % n];
                if let Some(row) = self.snapshot.procs.iter_mut().find(|r| r.proc.pid == pid) {
                    tmog_core::process::enrich_one(
                        root,
                        pid,
                        PAGE_BYTES,
                        &self.passwd,
                        &mut row.proc,
                    );
                }
            }
            self.detail_cursor = (start + take) % n;
            if !self.detail_requested {
                self.detail_pids.clear(); // the GUI stopped asking
            }
            self.detail_requested = false;
        }

        // ---- identities (slow-changing; never read from the paint path)
        self.refresh_identities(root);

        // ---- histories ----------------------------------------------
        self.tick += 1;
        let t = self.tick;
        self.snapshot.tick = t;
        let now_us = now.duration_since(self.start).as_micros() as u64;

        let push = |hist: &mut Vec<(u64, f64)>, v: Option<f64>| {
            let v = v.unwrap_or(f64::NAN);
            hist.push((t, v));
            if hist.len() > HIST_LEN {
                let excess = hist.len() - HIST_LEN;
                hist.drain(0..excess);
            }
        };
        // graph widgets want a little scroll margin; the page picks the
        // visible window, HIST_LEN is just the retention cap.
        if let Some(a) = self.snapshot.aggregate {
            push(&mut self.snapshot.hist_cpu, Some(a));
        }
        if let Some(ref m) = self.snapshot.mem_arc_adjusted {
            push(&mut self.snapshot.hist_mem, Some(m.used_percent()));
        }
        if let Some(ref ct) = self.snapshot.cpu_temp {
            push(&mut self.snapshot.hist_temp, Some(ct.value));
        }
        // GPU busy % and temperature °C: push an explicit None gap when
        // NVML is present but a tick's read failed (get_or_insert so the
        // series starts lazily when a GPU exists). Temperature is Celsius
        // as NVML reports it.
        if self.nvml.is_some() {
            let gpu = self.snapshot.gpu.as_ref();
            let busy = gpu.and_then(|g| g.busy_percent);
            push(self.snapshot.hist_gpu.get_or_insert_with(Vec::new), busy);
            let temp = gpu.and_then(|g| g.temp_c);
            push(
                self.snapshot.hist_gpu_temp.get_or_insert_with(Vec::new),
                temp,
            );
        }

        // disk rates (physical candidates only, matching the page filter)
        let mut disk_rates: RateHistory = Vec::new();
        for d in &self.snapshot.disks {
            if !tmog_core::diskstats::is_physical_candidate(&d.name)
                || tmog_core::diskstats::is_partition(root, &d.name)
            {
                continue;
            }
            let rr = Self::rate(&mut self.prev_disk_read, &d.name, d.read_bytes, now_us);
            let wr = Self::rate(&mut self.prev_disk_write, &d.name, d.write_bytes, now_us);
            disk_rates.push((d.name.clone(), vec![(t, rr)], vec![(t, wr)]));
        }
        // merge this tick's samples into existing per-disk histories
        for (name, r, w) in disk_rates {
            let (rr, ww) = (&r[0], &w[0]);
            if let Some(entry) = self.snapshot.hist_disk.iter_mut().find(|e| e.0 == name) {
                entry.1.push(*rr);
                entry.2.push(*ww);
                if entry.1.len() > HIST_LEN {
                    let x = entry.1.len() - HIST_LEN;
                    entry.1.drain(0..x);
                    entry.2.drain(0..x);
                }
            } else {
                self.snapshot.hist_disk.push((name, r, w));
            }
        }

        // net rates (skip loopback)
        let mut net_rates: RateHistory = Vec::new();
        for n in &self.snapshot.nets {
            if n.name == "lo" {
                continue;
            }
            let rx = Self::rate(&mut self.prev_net_rx, &n.name, n.rx_bytes, now_us);
            let tx = Self::rate(&mut self.prev_net_tx, &n.name, n.tx_bytes, now_us);
            net_rates.push((n.name.clone(), vec![(t, rx)], vec![(t, tx)]));
        }
        for (name, r, w) in net_rates {
            let (rr, ww) = (&r[0], &w[0]);
            if let Some(entry) = self.snapshot.hist_net.iter_mut().find(|e| e.0 == name) {
                entry.1.push(*rr);
                entry.2.push(*ww);
                if entry.1.len() > HIST_LEN {
                    let x = entry.1.len() - HIST_LEN;
                    entry.1.drain(0..x);
                    entry.2.drain(0..x);
                }
            } else {
                self.snapshot.hist_net.push((name, r, w));
            }
        }

        self.snapshot.now = Some(now);
        self.last_tick = Some(now);
    }
}

impl Sampler {
    /// Disk/net identity, cpuinfo and the root disk change rarely: refresh
    /// every IDENTITY_EVERY ticks, or at once when a device without an
    /// entry shows up. cpufreq's current MHz moves, so it is read per tick.
    fn refresh_identities(&mut self, root: &std::path::Path) {
        use tmog_core::{cpuinfo, diskstats, netdev};
        let snap = &mut self.snapshot;
        let new_device = snap.disks.iter().any(|d| {
            diskstats::is_physical_candidate(&d.name) && !snap.disk_ids.contains_key(&d.name)
        }) || snap
            .nets
            .iter()
            .any(|n| n.name != "lo" && !snap.net_ids.contains_key(&n.name));
        if self.tick.is_multiple_of(IDENTITY_EVERY) || new_device {
            snap.disk_ids = snap
                .disks
                .iter()
                .filter(|d| diskstats::is_physical_candidate(&d.name))
                .map(|d| (d.name.clone(), diskstats::read_identity(root, &d.name)))
                .collect();
            let names: Vec<String> = snap
                .nets
                .iter()
                .filter(|n| n.name != "lo")
                .map(|n| n.name.clone())
                .collect();
            snap.net_ids = netdev::read_identities(root, &names)
                .into_iter()
                .map(|id| {
                    let kind = id.kind(root);
                    (id.name.clone(), (id, kind))
                })
                .collect();
            snap.cpuinfo = cpuinfo::collect(root).ok();
            self.cpu0 = cpuinfo::present_cpus(root).first().copied();
            snap.root_disk = tmog_core::fs::read_under(root, "proc/mounts")
                .ok()
                .and_then(|m| diskstats::root_disk_from_mounts(&m));
        }
        snap.cpufreq0 = self.cpu0.map(|c| cpuinfo::read_cpufreq(root, c));
    }
}

/// Disk busy % since the previous sample. `busy_ms` is in milliseconds,
/// so the window must be too (it used to be passed in microseconds, which
/// made every reading 1000x too low).
fn disk_util(prev_busy_ms: u64, prev_at: Instant, busy_ms: u64, now: Instant) -> Option<f64> {
    let elapsed_ms = now.checked_duration_since(prev_at)?.as_millis() as u64;
    tmog_core::diskstats::util_percent(prev_busy_ms, busy_ms, elapsed_ms)
}

/// Identity refresh cadence, in ticks.
const IDENTITY_EVERY: u64 = 10;

fn read_pids(root: &std::path::Path) -> Result<Vec<u32>, String> {
    Ok(tmog_core::process::list_pids(root))
}

/// sysconf(_SC_PAGESIZE) is 4096 everywhere Linux runs 64-bit today; a
/// wrong value would only scale RSS display.
const PAGE_BYTES: u64 = 4096;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_against_real_proc_yields_the_essentials() {
        let mut s = Sampler::new();
        s.tick();
        // first tick seeds; second tick gets deltas
        std::thread::sleep(std::time::Duration::from_millis(120));
        s.tick();
        assert!(s.snapshot.aggregate.is_some(), "aggregate cpu% missing");
        assert!(!s.snapshot.per_core.is_empty(), "per-core missing");
        assert!(s.snapshot.mem.is_some());
        assert!(s.snapshot.procs.len() > 10);
        assert!(!s.snapshot.disks.is_empty());
        assert!(!s.snapshot.nets.is_empty());
        assert!(s.snapshot.sysinfo.is_some());
        assert!(s.snapshot.cpu_ticks_delta > 0);
        assert!(
            s.snapshot.cpuinfo.is_some(),
            "identities refreshed by the sampler"
        );
    }

    #[test]
    fn disk_util_uses_milliseconds() {
        // 240 ms busy over a 1000 ms window is 24%, not 0.024%
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_millis(1000);
        let u = disk_util(1_000, t0, 1_240, t1).unwrap();
        assert!((u - 24.0).abs() < 0.01, "{u}");
        // a window that runs backwards yields None, never a panic
        assert_eq!(disk_util(0, t1, 10, t0), None);
    }

    #[test]
    fn detail_requests_longer_than_a_batch_are_all_served() {
        let mut s = Sampler::new();
        s.tick();
        // the GUI's shape: ~41 pids re-sent before every tick
        let pids: Vec<u32> = s
            .snapshot
            .procs
            .iter()
            .filter(|r| r.proc.pid > 2)
            .take(DETAIL_BATCH + 16)
            .map(|r| r.proc.pid)
            .collect();
        assert!(pids.len() > DETAIL_BATCH, "need more pids than one batch");
        // statm is a slow-tier-only read: use a sentinel to see enrichment
        for r in s.snapshot.procs.iter_mut() {
            r.proc.vm_swap_kib = u64::MAX;
        }
        for _ in 0..2 {
            s.request_detail(&pids);
            s.tick();
        }
        let missed: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|p| {
                s.snapshot
                    .procs
                    .iter()
                    .any(|r| r.proc.pid == *p && r.proc.vm_swap_kib == u64::MAX)
            })
            .collect();
        assert!(missed.is_empty(), "never enriched: {missed:?}");
    }

    #[test]
    fn every_row_has_rss_and_owner_without_enrichment() {
        let mut s = Sampler::new();
        s.tick();
        let me = std::process::id();
        let row = s
            .snapshot
            .procs
            .iter()
            .find(|r| r.proc.pid == me)
            .expect("self");
        assert!(row.proc.rss_bytes > 0, "rss from stat");
        assert!(row.proc.uid.is_some(), "owner from /proc/<pid>");
    }
}
