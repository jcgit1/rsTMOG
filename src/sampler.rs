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

    // ---- histories (sample_index == Snapshot.tick) -------------------
    /// aggregate cpu busy %
    pub hist_cpu: Vec<(u64, f64)>,
    /// cpu package temperature °C
    pub hist_temp: Vec<(u64, f64)>,
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
    /// busy_ms + sampler-start time (µs) per disk at the last sample
    /// (util% window)
    prev_disk_busy: std::collections::HashMap<String, (u64, u64)>,
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
            prev_net_rx: Default::default(),
            prev_net_tx: Default::default(),
            agg_ticks: 0,
            row_pass_ticks: 0,
            last_was_row: false,
            last_hwmon: false,
            detail_pids: Vec::new(),
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
        self.detail_pids = pids.to_vec();
    }

    /// One collection pass. Cheap and panic-tolerant: any collector that
    /// fails leaves its previous snapshot value in place.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let root = self.root.as_path();

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
                    if let Ok(mut p) = tmog_core::process::collect_stat_only(root, pid) {
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
                        let total_delta = (self.agg_ticks - self.row_pass_ticks).max(1);
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
            let now_us = std::time::UNIX_EPOCH
                .elapsed()
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0);
            let mut utils = Vec::with_capacity(d.len());
            for c in &d {
                let u = match self.prev_disk_busy.get(&c.name) {
                    Some((prev_busy, prev_us)) => {
                        tmog_core::diskstats::util_percent(*prev_busy, c.busy_ms, now_us - prev_us)
                    }
                    None => None,
                };
                self.prev_disk_busy
                    .insert(c.name.clone(), (c.busy_ms, now_us));
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
        if !self.detail_pids.is_empty() {
            let want = std::mem::take(&mut self.detail_pids);
            for pid in want.iter().take(DETAIL_BATCH) {
                if let Some(row) = self.snapshot.procs.iter_mut().find(|r| r.proc.pid == *pid) {
                    tmog_core::process::enrich_one(
                        root,
                        *pid,
                        PAGE_BYTES,
                        &self.passwd,
                        &mut row.proc,
                    );
                }
            }
            // anything beyond the batch goes back for next tick
            if want.len() > DETAIL_BATCH {
                self.detail_pids = want[DETAIL_BATCH..].to_vec();
            }
        }

        // ---- histories ----------------------------------------------
        self.tick += 1;
        let t = self.tick;
        self.snapshot.tick = t;
        let now_us = std::time::UNIX_EPOCH
            .elapsed()
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);

        let push = |hist: &mut Vec<(u64, f64)>, v: f64| {
            hist.push((t, v));
            if hist.len() > HIST_LEN {
                let excess = hist.len() - HIST_LEN;
                hist.drain(0..excess);
            }
        };
        // graph widgets want a little scroll margin; the page picks the
        // visible window, HIST_LEN is just the retention cap.
        if let Some(a) = self.snapshot.aggregate {
            push(&mut self.snapshot.hist_cpu, a);
        }
        if let Some(ref m) = self.snapshot.mem_arc_adjusted {
            push(&mut self.snapshot.hist_mem, m.used_percent());
        }
        if let Some(ref ct) = self.snapshot.cpu_temp {
            push(&mut self.snapshot.hist_temp, ct.value);
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
    }
}
