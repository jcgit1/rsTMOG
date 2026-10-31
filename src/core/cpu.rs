//! CPU metrics: aggregate + per-core jiffies from /proc/stat.
//!
//! Start with parse-only functions taking `&str` so tests can feed fixture
//! text; live collection will wrap these with `procfs`.

use serde::Serialize;

/// One CPU row: `cpu`, `cpu0`, ... jiffies in USER_HZ.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CpuJiffies {
    pub name: String,
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuJiffies {
    pub fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    pub fn busy(&self) -> u64 {
        self.total() - self.idle - self.iowait
    }
}

/// Parse a single `/proc/stat` cpu line ("cpu  1 2 3 ...").
/// Missing trailing fields default to 0 (older kernels).
pub fn parse_cpu_line(line: &str) -> Option<CpuJiffies> {
    let mut it = line.split_whitespace();
    let name = it.next()?;
    if !name.starts_with("cpu") {
        return None;
    }
    let mut next = || it.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    Some(CpuJiffies {
        name: name.to_string(),
        user: next(),
        nice: next(),
        system: next(),
        idle: next(),
        iowait: next(),
        irq: next(),
        softirq: next(),
        steal: next(),
    })
}

/// Busy percentage (0.0..=100.0) between two samples of the same CPU row.
/// Returns `None` when no time passed (zero delta) or time went backwards,
/// so callers keep the previous value instead of painting a spike.
pub fn busy_percent(prev: &CpuJiffies, now: &CpuJiffies) -> Option<f64> {
    let total_prev = prev.total();
    let total_now = now.total();
    if total_now <= total_prev {
        return None;
    }
    let busy_prev = prev.busy();
    let busy_now = now.busy();
    // busy counters are monotonic per-field, so busy_now >= busy_prev
    // whenever total advanced; clamp for pathological fixture inputs.
    let busy_delta = busy_now.saturating_sub(busy_prev);
    let pct = busy_delta as f64 * 100.0 / (total_now - total_prev) as f64;
    Some(pct.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str, busy: u64, idle: u64) -> CpuJiffies {
        CpuJiffies {
            name: name.into(),
            user: busy,
            idle,
            ..Default::default()
        }
    }

    #[test]
    fn parses_full_line() {
        let c = parse_cpu_line("cpu0 100 20 30 400 50 5 6 7").unwrap();
        assert_eq!(c.name, "cpu0");
        assert_eq!(c.user, 100);
        assert_eq!(c.steal, 7);
        assert_eq!(c.total(), 618);
        assert_eq!(c.busy(), 618 - 400 - 50);
    }

    #[test]
    fn short_line_zero_fills() {
        let c = parse_cpu_line("cpu  1 2 3 4").unwrap();
        assert_eq!(c.name, "cpu");
        assert_eq!(c.steal, 0);
    }

    #[test]
    fn rejects_non_cpu_lines() {
        assert!(parse_cpu_line("intr 12345").is_none());
        assert!(parse_cpu_line("").is_none());
    }

    #[test]
    fn busy_percent_half_busy() {
        let prev = sample("cpu", 100, 100);
        let now = sample("cpu", 150, 150); // +50 busy, +100 total
        assert_eq!(busy_percent(&prev, &now), Some(50.0));
    }

    #[test]
    fn busy_percent_zero_delta_guard() {
        let prev = sample("cpu", 100, 100);
        assert_eq!(busy_percent(&prev, &prev), None); // no time passed
        let backwards = sample("cpu", 50, 50); // counter went backwards
        assert_eq!(busy_percent(&prev, &backwards), None);
    }

    #[test]
    fn busy_percent_all_idle_and_all_busy() {
        let prev = sample("cpu", 0, 0);
        assert_eq!(busy_percent(&prev, &sample("cpu", 0, 100)), Some(0.0));
        assert_eq!(busy_percent(&prev, &sample("cpu", 100, 0)), Some(100.0));
    }

    #[test]
    fn iowait_counts_as_not_busy() {
        // iowait is excluded from busy (matches C++ reference semantics)
        let prev = parse_cpu_line("cpu 0 0 0 0 0").unwrap();
        let now = parse_cpu_line("cpu 50 0 0 0 50").unwrap(); // 50 user, 50 iowait
        assert_eq!(busy_percent(&prev, &now), Some(50.0));
    }
}
