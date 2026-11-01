//! Platform collection seam — the OS boundary for rsTMOG.
//!
//! The Linux implementation is the original one: collectors that read
//! `/proc` + `/sys` text (see `cpu`, `meminfo`, `process`, …) wrapped as a
//! `Collector` impl. The Windows implementation reads the same logical
//! quantities from Win32 APIs. Everything above this module — `Sampler`,
//! the ring buffers, all pages — is platform-neutral and identical.
//!
//! Design rules (keep them, they are what makes the port cheap):
//! - Raw OS values only. Rates/deltas stay in `Sampler` so the smoothing
//!   and rate history are shared and testable on one platform.
//! - Never panic on a missing counter: `None` means "not available here"
//!   and the UI renders a dash (same contract as the NVML handle).
//! - A platform must be able to return `None` for whole features (ZFS ARC
//!   on Windows, PDH disk counters when the service is stopped).

use std::path::PathBuf;

use crate::cpuinfo::CpuInfo;
use crate::diskstats::DiskCounters;
use crate::gpu::GpuInfo;
use crate::hwmon::Reading;
use crate::loadavg::LoadAvg;
use crate::meminfo::Meminfo;
use crate::netdev::NetDevCounters;
use crate::pressure::PsiAvg;
use crate::process::Proc;
use crate::sysinfo::SysInfo;
use crate::zfs::ArcStats;

/// One pass of raw counters. Field-for-field the Linux sampler's inputs;
/// a Windows impl fills the same slots from Win32 sources.
#[derive(Debug, Clone, Default)]
pub struct RawCounters {
    /// Aggregate + per-core CPU time. `(label, jiffies)`; Linux labels are
    /// "cpu" and "cpu0".."cpuN", Windows uses "_Total" and "0".."N".
    pub cpu: Vec<(String, crate::cpu::CpuJiffies)>,
    pub mem: Option<Meminfo>,
    /// /proc/meminfo + ARC folded in; Linux-only (None elsewhere).
    pub arc: Option<ArcStats>,
    pub load: Option<LoadAvg>,
    pub uptime_s: Option<f64>,
    /// Full task list: Linux walks /proc, Windows the toolhelp snapshot.
    pub procs: Vec<Proc>,
    pub disks: Vec<DiskCounters>,
    pub nets: Vec<NetDevCounters>,
    pub temps: Vec<Reading>,
    /// Memory (and IO where the OS has it) pressure stall info.
    pub psi_memory: Option<(PsiAvg, Option<PsiAvg>)>,
    pub psi_io: Option<(PsiAvg, Option<PsiAvg>)>,
    pub sysinfo: SysInfo,
    /// CPU identity (model/vendor/socket-core counts) for the CPU page header.
    pub cpu_info: Option<CpuInfo>,
}

/// A platform's collector set. Object-safe by design: `Sampler` holds a
/// `Box<dyn Collector>` so the GUI never learns which OS it is on.
pub trait Collector: Send {
    /// Cheap identity for the System page / title bar.
    fn name(&self) -> &'static str;

    /// Full raw pass. `now_us` is a monotonic microsecond timestamp owned
    /// by the caller (Sampler) so all rate baselines share one clock.
    fn sample(&mut self, now_us: u64) -> RawCounters;

    /// GPU handle, if the platform can provide one (dlopen'd NVML on both
    /// Linux and Windows — the nvml-wrapper crate is cross-platform).
    fn gpu(&self) -> Option<GpuInfo> {
        None
    }

    /// Expensive, on-demand detail (Linux: per-proc cwd/exe links, disk
    /// identity). Called on the slow tier, never per tick.
    fn detail(&mut self) {}

    /// Rows pass: the heavier task walk + username resolution. Linux runs
    /// it every 3rd tick; the flag exists so both platforms share the
    /// Sampler's tiering instead of deciding it themselves.
    fn wants_rows_pass(&self) -> bool {
        true
    }

    /// Linux-only collectors read a fixture `root` in tests; Windows
    /// ignores it (its fixtures are recorded API responses).
    fn set_root(&mut self, _root: PathBuf) {}
}

/// The collector for the platform we are running on.
pub fn collector() -> Box<dyn Collector> {
    #[cfg(target_os = "linux")]
    {
        let mut c = Box::new(crate::linux::LinuxCollector::new());
        // RSTMOG_ROOT lets tests/self-checks point the collectors at a
        // fixture tree (same contract the Sampler's with_root uses).
        if let Ok(root) = std::env::var("RSTMOG_ROOT") {
            c.set_root(PathBuf::from(root));
        }
        c
    }
    #[cfg(target_os = "windows")]
    {
        Box::new(crate::windows::WindowsCollector::new())
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        compile_error!("rsTMOG supports Linux and Windows");
    }
}
