//! Static system identity: uname, hostname, os-release, boot time.

use crate::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SysInfo {
    pub hostname: String,
    /// sysname, e.g. "Linux"
    pub sysname: String,
    /// kernel release, e.g. "6.17.0-cachyos"
    pub release: String,
    /// machine, e.g. "x86_64"
    pub machine: String,
    /// PRETTY_NAME from /etc/os-release, fallback to "Linux"
    pub distro: String,
    /// boot instant in CLOCK_MONOTONIC seconds (uptime - boottime == boot)
    pub boot_time: Option<std::time::SystemTime>,
}

/// Parse `uname -a`-ish input from `/proc/sys/kernel/{ostype,osrelease}`
/// plus machine from `uname(2)` (we read /proc/cpuinfo-independent
/// machine via /sys? simplest: std::env consts are build-time, so use
/// a small uname call via libc-free approach: read from /proc).
pub fn collect(root: &Path) -> SysInfo {
    let hostname = fs::read_under(root, "proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let sysname = fs::read_under(root, "proc/sys/kernel/ostype")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "Linux".into());
    let release = fs::read_under(root, "proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    // machine: the portable runtime source is uname(2); without libc we
    // fall back to the compile-time arch of this binary, which is honest
    // for a single-arch native tool (documented deviation from `uname -m`).
    let machine = target_arch().to_string();
    let distro = fs::read_under(root, "etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into());
    SysInfo {
        hostname,
        sysname,
        release,
        machine,
        distro,
        boot_time: boot_time(root),
    }
}

fn target_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    }
}

/// boot_time = now_wall - uptime. Reads /proc/uptime under `root`;
/// in tests with a fixture uptime, returns now-fixture_uptime.
pub fn boot_time(root: &Path) -> Option<std::time::SystemTime> {
    let up = crate::loadavg::collect_uptime(root).ok()?;
    std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs_f64(up))
}

/// "up 3d 4h 12m" style rendering used by the status bar.
pub fn format_uptime(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    let (d, h, m, sec) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60, s % 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {sec}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysinfo_fixture() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let si = collect(&root);
        assert_eq!(si.hostname, "testbox");
        assert_eq!(si.sysname, "Linux");
        assert_eq!(si.release, "6.9.0-test");
        assert_eq!(si.distro, "TestOS 1.2.3");
        // uptime fixture 3600.5 -> boot about an hour ago
        let bt = si.boot_time.expect("boot time");
        let ago = std::time::SystemTime::now().duration_since(bt).unwrap();
        assert!((ago.as_secs_f64() - 3600.5).abs() < 5.0);
    }

    #[test]
    fn uptime_formatting() {
        assert_eq!(format_uptime(45.0), "0m 45s");
        assert_eq!(format_uptime(3725.0), "1h 2m");
        assert_eq!(format_uptime(90061.0), "1d 1h 1m");
        assert_eq!(format_uptime(-5.0), "0m 0s");
    }
}
