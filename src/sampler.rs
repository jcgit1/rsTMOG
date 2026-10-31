//! Sampler: the single owner of live state. One tick = one pass over all
//! collectors; pages only ever read the resulting immutable `Snapshot`.
//! Runs on eframe's request_repaint timer (refresh_ms), never blocks the
//! paint path.

use crate::core::cpu::{self, CpuJiffies};
use crate::core::diskstats::DiskCounters;
use crate::core::gpu::{GpuInfo, Nvml};
use crate::core::loadavg::LoadAvg;
use crate::core::meminfo::Meminfo;
use crate::core::netdev::NetDevCounters;
use crate::core::pressure::PsiAvg;
use crate::core::process_table::Row;
use crate::core::sysinfo::SysInfo;
use crate::core::zfs::ArcStats;
use std::path::PathBuf;
use std::time::Instant;

/// Everything a page can read, computed once per tick.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub now: Option<Instant>,
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
    pub nets: Vec<NetDevCounters>,
    pub temps: Vec<crate::core::hwmon::Reading>,
    pub cpu_temp: Option<crate::core::hwmon::Reading>,
    pub gpu: Option<GpuInfo>,
    pub psi_memory: Option<(PsiAvg, Option<PsiAvg>)>,
    pub psi_io: Option<(PsiAvg, Option<PsiAvg>)>,
    pub sysinfo: Option<SysInfo>,
}

/// Mutable sampling state between ticks (previous jiffies, passwd cache,
/// NVML handle).
pub struct Sampler {
    root: PathBuf,
    prev_cpu: Vec<CpuJiffies>,
    prev_ticks: u64,
    passwd: std::collections::HashMap<u32, String>,
    nvml: Option<Nvml>,
    last_tick: Option<Instant>,
    pub snapshot: Snapshot,
}

impl Sampler {
    pub fn new() -> Self {
        Self::with_root(PathBuf::from("/"))
    }

    /// Fixture-root constructor (tests / self-checks against trees).
    pub fn with_root(root: PathBuf) -> Self {
        let passwd = std::fs::read_to_string(root.join("etc/passwd"))
            .map(|s| crate::core::process::parse_passwd(&s))
            .unwrap_or_default();
        Sampler {
            root,
            prev_cpu: Vec::new(),
            prev_ticks: 0,
            passwd,
            nvml: Nvml::new(),
            last_tick: None,
            snapshot: Snapshot::default(),
        }
    }

    /// One collection pass. Cheap and panic-tolerant: any collector that
    /// fails leaves its previous snapshot value in place.
    pub fn tick(&mut self) {
        let now = Instant::now();
        let root = self.root.as_path();

        // ---- cpu ----------------------------------------------------
        if let Ok(text) = crate::core::fs::read_under(root, "proc/stat") {
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
            }
            self.prev_ticks = rows
                .iter()
                .find(|r| r.name == "cpu")
                .map(|r| r.total())
                .unwrap_or(0);
            self.prev_cpu = rows;
        }

        // ---- meminfo + zfs arc --------------------------------------
        self.snapshot.arc = crate::core::zfs::collect(root);
        if let Ok(m) = crate::core::meminfo::collect(root) {
            self.snapshot.mem_arc_adjusted = Some(crate::core::zfs::arc_adjusted_meminfo(
                &m,
                self.snapshot.arc.as_ref(),
            ));
            self.snapshot.mem = Some(m);
        }

        // ---- load / uptime ------------------------------------------
        self.snapshot.load = crate::core::loadavg::collect_loadavg(root).ok();
        self.snapshot.uptime_s = crate::core::loadavg::collect_uptime(root).ok();

        // ---- processes ----------------------------------------------
        if let Ok(pids) = read_pids(root) {
            let mut rows: Vec<Row> = Vec::with_capacity(pids.len());
            let total_delta = self.snapshot.cpu_ticks_delta;
            for pid in pids {
                if let Ok(p) =
                    crate::core::process::collect_one(root, pid, PAGE_BYTES, &self.passwd)
                {
                    // CPU% needs a previous sample of the same pid; the
                    // GUI keeps a pid map across ticks. v1 keeps the last
                    // snapshot's rows for the delta.
                    let prev = self
                        .snapshot
                        .procs
                        .iter()
                        .find(|r| r.proc.pid == pid)
                        .map(|r| &r.proc);
                    let cpu_pct = prev.and_then(|pv| {
                        crate::core::process_table::cpu_percent(pv, &p, total_delta)
                    });
                    rows.push(Row {
                        proc: p,
                        cpu: cpu_pct,
                    });
                }
            }
            self.snapshot.procs = rows;
        }

        // ---- disks / nets -------------------------------------------
        if let Ok(d) = crate::core::diskstats::collect(root) {
            self.snapshot.disks = d;
        }
        if let Ok(n) = crate::core::netdev::collect(root) {
            self.snapshot.nets = n;
        }

        // ---- hwmon --------------------------------------------------
        let temps = crate::core::hwmon::collect(root);
        if !temps.is_empty() {
            self.snapshot.cpu_temp = crate::core::hwmon::select_cpu_temp(&temps).cloned();
            self.snapshot.temps = temps;
        }

        // ---- gpu ----------------------------------------------------
        if let Some(nvml) = &self.nvml {
            self.snapshot.gpu = nvml.sample(0);
        }

        // ---- psi ----------------------------------------------------
        self.snapshot.psi_memory = crate::core::pressure::collect_pressure(root, "memory")
            .ok()
            .flatten();
        self.snapshot.psi_io = crate::core::pressure::collect_pressure(root, "io")
            .ok()
            .flatten();

        // ---- sysinfo (cheap enough per tick; could cache) -----------
        self.snapshot.sysinfo = Some(crate::core::sysinfo::collect(root));

        self.snapshot.now = Some(now);
        self.last_tick = Some(now);
    }
}

fn read_pids(root: &std::path::Path) -> Result<Vec<u32>, String> {
    Ok(crate::core::process::list_pids(root))
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
