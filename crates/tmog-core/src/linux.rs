//! Linux `Collector` impl: a thin adapter over the original /proc + /sys
//! collectors. All logic lives in the platform modules (cpu, meminfo,
//! process, …); this only sequences a pass and fills `RawCounters`, so
//! `Sampler` can consume either platform through one trait object.

use std::path::PathBuf;

use crate::platform::{Collector, RawCounters};

pub struct LinuxCollector {
    root: PathBuf,
}

impl LinuxCollector {
    pub fn new() -> Self {
        Self {
            root: PathBuf::from("/"),
        }
    }
}

impl Default for LinuxCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector for LinuxCollector {
    fn name(&self) -> &'static str {
        "linux"
    }

    fn sample(&mut self, _now_us: u64) -> RawCounters {
        let root = self.root.as_path();
        let cpu: Vec<(String, crate::cpu::CpuJiffies)> = crate::fs::read_under(root, "proc/stat")
            .ok()
            .map(|text| {
                text.lines()
                    .filter_map(crate::cpu::parse_cpu_line)
                    .map(|j| (j.name.clone(), j))
                    .collect()
            })
            .unwrap_or_default();

        RawCounters {
            cpu,
            mem: crate::meminfo::collect(root).ok(),
            arc: crate::zfs::collect(root),
            load: crate::loadavg::collect_loadavg(root).ok(),
            uptime_s: crate::loadavg::collect_uptime(root).ok(),
            procs: crate::process::list_pids(root)
                .into_iter()
                .filter_map(|pid| crate::process::collect_stat_only(root, pid).ok())
                .collect(),
            disks: crate::diskstats::collect(root).unwrap_or_default(),
            nets: crate::netdev::collect(root).unwrap_or_default(),
            temps: crate::hwmon::collect(root),
            psi_memory: crate::pressure::collect_pressure(root, "memory")
                .ok()
                .flatten(),
            psi_io: crate::pressure::collect_pressure(root, "io").ok().flatten(),
            sysinfo: crate::sysinfo::collect(root),
            cpu_info: crate::cpuinfo::collect(root).ok(),
        }
    }

    fn set_root(&mut self, root: PathBuf) {
        self.root = root;
    }
}
