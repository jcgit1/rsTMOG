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

#[cfg(test)]
mod tests {
    use super::*;

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
}
