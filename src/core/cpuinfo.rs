//! CPU model / topology / frequency: `/proc/cpuinfo` text plus the
//! `/sys/devices/system/cpu/**` trees, both rooted so tests use fixtures.

use crate::core::fs;
use std::path::Path;

/// Static-ish CPU identity and topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuInfo {
    /// e.g. "AMD Ryzen 9 9955HX3D 16-Core Processor"
    pub model: String,
    pub vendor: String,
    /// logical CPUs (entries in /proc/cpuinfo)
    pub logical: usize,
    /// physical cores (unique core ids among present CPUs)
    pub cores: usize,
    /// sockets (unique physical ids)
    pub sockets: usize,
}

/// Parse `/proc/cpuinfo`: model name (first occurrence), vendor, and counts
/// of logical CPUs / cores / sockets. Fields absent → conservative counts
/// (logical from processor entries; cores=logical, sockets=1 when no core
/// ids are present, e.g. ARM/virtual).
pub fn parse_cpuinfo(text: &str) -> CpuInfo {
    let mut info = CpuInfo {
        model: String::new(),
        vendor: String::new(),
        logical: 0,
        cores: 0,
        sockets: 0,
    };
    // (physical, core) pairs; `cur_phys` tracks the most recent
    // "physical id" because it appears before "core id" in each block.
    let mut core_ids: Vec<(i64, i64)> = Vec::new();
    let mut cur_phys: i64 = 0;
    let mut saw_core_id = false;
    for line in text.lines() {
        let Some((key, val)) = line.split_once(':') else {
            continue;
        };
        let val = val.trim();
        match key.trim() {
            "processor" => info.logical += 1,
            "model name" if info.model.is_empty() => info.model = val.to_string(),
            "vendor_id" if info.vendor.is_empty() => info.vendor = val.to_string(),
            "physical id" => cur_phys = val.parse().unwrap_or(0),
            "core id" => {
                if let Ok(c) = val.parse::<i64>() {
                    core_ids.push((cur_phys, c));
                    saw_core_id = true;
                }
            }
            _ => {}
        }
    }
    if !saw_core_id {
        // no topology exposed (some ARM/virtual): cores = logical
        info.cores = info.logical;
        info.sockets = if info.logical == 0 { 0 } else { 1 };
    } else {
        let mut uniq: Vec<(i64, i64)> = core_ids;
        uniq.sort_unstable();
        uniq.dedup();
        info.cores = uniq.len();
        let mut socks: Vec<i64> = uniq.iter().map(|(p, _)| *p).collect();
        socks.sort_unstable();
        socks.dedup();
        info.sockets = socks.len();
    }
    info
}

/// Per-CPU frequency + governor state read from sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cpufreq {
    pub cpu: usize,
    /// kHz, `scaling_cur_freq` (None if absent, e.g. amd_pstate old
    /// kernels — callers may fall back to /proc/cpuinfo "cpu MHz")
    pub cur_khz: Option<u64>,
    pub max_khz: Option<u64>,
    pub min_khz: Option<u64>,
    pub governor: Option<String>,
    pub driver: Option<String>,
}

/// Read cpufreq for one logical cpu `n` under `sys_root`. Any missing file
/// is None; a missing `cpufreq` dir at all yields all-None (the struct
/// still reports which cpu it was).
pub fn read_cpufreq(sys_root: &Path, cpu: usize) -> Cpufreq {
    let rel = |f: &str| format!("sys/devices/system/cpu/cpu{cpu}/cpufreq/{f}");
    let num = |f: &str| fs::read_u64_under(sys_root, &rel(f)).ok();
    let txt = |f: &str| {
        fs::read_under(sys_root, &rel(f))
            .ok()
            .map(|s| s.trim().to_string())
    };
    Cpufreq {
        cpu,
        cur_khz: num("scaling_cur_freq"),
        max_khz: num("scaling_max_freq"),
        min_khz: num("scaling_min_freq"),
        governor: txt("scaling_governor"),
        driver: txt("scaling_driver"),
    }
}

/// List present CPU numbers from `sys/devices/system/cpu/cpu*` dirs,
/// numerically sorted.
pub fn present_cpus(sys_root: &Path) -> Vec<usize> {
    let mut out = Vec::new();
    for p in fs::list_dir_sorted(sys_root, "sys/devices/system/cpu").unwrap_or_default() {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(rest) = name.strip_prefix("cpu")
            && let Ok(n) = rest.parse::<usize>()
        {
            out.push(n);
        }
    }
    out.sort_unstable();
    out
}

/// Read CPU identity from the real `/proc/cpuinfo` (or fixture root).
pub fn collect(root: &Path) -> Result<CpuInfo, String> {
    let text = fs::read_under(root, "proc/cpuinfo").map_err(|e| format!("read cpuinfo: {e}"))?;
    let info = parse_cpuinfo(&text);
    if info.logical == 0 {
        return Err("cpuinfo: no processor entries".into());
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    const CPUINFO: &str = "\
processor\t: 0
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
physical id\t: 0
core id\t\t: 0

processor\t: 1
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
physical id\t: 0
core id\t\t: 1

processor\t: 2
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz
physical id\t: 1
core id\t\t: 0

";

    #[test]
    fn cpuinfo_counts_topology() {
        let i = parse_cpuinfo(CPUINFO);
        assert_eq!(i.logical, 3);
        // (phys 0, core 0), (0,1), (1,0) — same core id on different
        // sockets must NOT merge.
        assert_eq!(i.cores, 3);
        assert_eq!(i.sockets, 2);
        assert_eq!(i.model, "Intel(R) Core(TM) i7-8700K CPU @ 3.70GHz");
        assert_eq!(i.vendor, "GenuineIntel");
    }

    #[test]
    fn cpuinfo_threads_share_core_id() {
        // SMT: two logical CPUs, same (physical, core) → 1 core.
        let text = "\
processor\t: 0
model name\t: X
physical id\t: 0
core id\t\t: 0

processor\t: 1
model name\t: X
physical id\t: 0
core id\t\t: 0

";
        let i = parse_cpuinfo(text);
        assert_eq!(i.logical, 2);
        assert_eq!(i.cores, 1);
        assert_eq!(i.sockets, 1);
    }

    #[test]
    fn cpuinfo_no_core_ids_falls_back() {
        let text = "processor\t: 0\nmodel name\t: ARMv8\nprocessor\t: 1\nmodel name\t: ARMv8\n";
        let i = parse_cpuinfo(text);
        assert_eq!(i.logical, 2);
        assert_eq!(i.cores, 2);
        assert_eq!(i.sockets, 1);
    }

    #[test]
    fn cpufreq_fixture_read() {
        let root = fixture_root();
        let c0 = read_cpufreq(&root, 0);
        assert_eq!(c0.cur_khz, Some(2_400_000));
        assert_eq!(c0.max_khz, Some(3_500_000));
        assert_eq!(c0.governor.as_deref(), Some("schedutil"));
        assert_eq!(c0.driver.as_deref(), Some("acpi-cpufreq"));
        // cpu1 has no scaling_driver file
        let c1 = read_cpufreq(&root, 1);
        assert_eq!(c1.governor.as_deref(), Some("powersave"));
        assert_eq!(c1.driver, None);
        // cpu2 has no governor file
        let c2 = read_cpufreq(&root, 2);
        assert_eq!(c2.governor, None);
        assert_eq!(c2.max_khz, Some(4_400_000));
        // cpu3 does not exist: all None, no panic
        let c3 = read_cpufreq(&root, 3);
        assert_eq!(c3.cur_khz, None);
        assert_eq!(c3.governor, None);
    }

    #[test]
    fn present_cpus_numeric_order() {
        let root = fixture_root();
        assert_eq!(present_cpus(&root), vec![0, 1, 2]);
        // nonexistent root → empty, no error
        assert_eq!(
            present_cpus(std::path::Path::new("/nonexistent-fixture-root")),
            Vec::<usize>::new()
        );
    }
}
