//! `/proc/meminfo` parse + used% math, with the `MemAvailable` fallback
//! for ancient kernels that don't export it. ZFS ARC awareness lives in
//! `core::zfs` and is applied on top of these raw values.

use crate::fs;
use std::path::Path;

/// Raw meminfo fields, all in KiB (the kernel's unit for this file).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Meminfo {
    pub mem_total: u64,
    pub mem_free: u64,
    pub mem_available: Option<u64>,
    pub buffers: u64,
    pub cached: u64,
    pub s_reclaimable: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

/// Parse `/proc/meminfo` text. Unknown fields are ignored; missing
/// required fields are an error (this file always has at least MemTotal).
pub fn parse(text: &str) -> Result<Meminfo, String> {
    let mut m = Meminfo::default();
    let mut got_total = false;
    for line in text.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        // value is "  12345 kB" — take the first token
        let Some(val_tok) = rest.split_whitespace().next() else {
            continue;
        };
        let Ok(v) = val_tok.parse::<u64>() else {
            continue;
        };
        match key {
            "MemTotal" => {
                m.mem_total = v;
                got_total = true;
            }
            "MemFree" => m.mem_free = v,
            "MemAvailable" => m.mem_available = Some(v),
            "Buffers" => m.buffers = v,
            "Cached" => m.cached = v,
            "SReclaimable" => m.s_reclaimable = v,
            "SwapTotal" => m.swap_total = v,
            "SwapFree" => m.swap_free = v,
            _ => {}
        }
    }
    if !got_total {
        return Err("meminfo: no MemTotal field".into());
    }
    Ok(m)
}

impl Meminfo {
    /// Available memory in KiB: `MemAvailable` when present, else the
    /// classic approximation free + buffers + cached (SReclaimable counts
    /// as cache on kernels with it — same era fallback the C++ reference
    /// used).
    pub fn available_kib(&self) -> u64 {
        self.mem_available.unwrap_or_else(|| {
            self.mem_free
                .saturating_add(self.buffers)
                .saturating_add(self.cached)
        })
    }

    /// Used memory in KiB (total minus available). Clamped at 0.
    pub fn used_kib(&self) -> u64 {
        self.mem_total.saturating_sub(self.available_kib())
    }

    /// Used percentage of total, 0.0..=100.0. Zero-total (fixture edge
    /// case) yields 0.0 rather than NaN.
    pub fn used_percent(&self) -> f64 {
        if self.mem_total == 0 {
            return 0.0;
        }
        (self.used_kib() as f64 * 100.0 / self.mem_total as f64).clamp(0.0, 100.0)
    }

    /// Swap used percentage; no swap configured (SwapTotal 0) yields 0.0.
    pub fn swap_used_percent(&self) -> f64 {
        if self.swap_total == 0 {
            return 0.0;
        }
        let used = self.swap_total.saturating_sub(self.swap_free);
        (used as f64 * 100.0 / self.swap_total as f64).clamp(0.0, 100.0)
    }
}

/// Read and parse meminfo under `root` (`/` in prod, fixture dir in tests).
pub fn collect(root: &Path) -> Result<Meminfo, String> {
    let text = fs::read_under(root, "proc/meminfo").map_err(|e| format!("read meminfo: {e}"))?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "\
MemTotal:       129813624 kB
MemFree:         4803160 kB
MemAvailable:   66321732 kB
Buffers:          293820 kB
Cached:          58932568 kB
SwapCached:            0 kB
Active:         41023456 kB
Inactive:       60123456 kB
SReclaimable:     321544 kB
SwapTotal:             0 kB
SwapFree:              0 kB
Dirty:               512 kB
";

    #[test]
    fn parses_all_fields_and_ignores_unknowns() {
        let m = parse(FULL).unwrap();
        assert_eq!(m.mem_total, 129_813_624);
        assert_eq!(m.mem_available, Some(66_321_732));
        assert_eq!(m.buffers, 293_820);
        assert_eq!(m.cached, 58_932_568);
        assert_eq!(m.s_reclaimable, 321_544);
        assert_eq!(m.swap_total, 0);
    }

    #[test]
    fn used_math_uses_mem_available() {
        let m = parse(FULL).unwrap();
        assert_eq!(m.used_kib(), 129_813_624 - 66_321_732);
        // ~48.9% used — sanity-check the percentage tracks that
        assert!((m.used_percent() - 48.91).abs() < 0.1);
    }

    #[test]
    fn mem_available_fallback() {
        let text = "\
MemTotal:       1000 kB
MemFree:        100 kB
Buffers:        200 kB
Cached:         300 kB
";
        let m = parse(text).unwrap();
        assert_eq!(m.mem_available, None);
        assert_eq!(m.available_kib(), 600);
        assert_eq!(m.used_kib(), 400);
        assert!((m.used_percent() - 40.0).abs() < 1e-9);
    }

    #[test]
    fn missing_memtotal_is_error() {
        assert!(parse("MemFree: 1 kB\n").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn zero_total_no_nan() {
        let text = "MemTotal: 0 kB\nMemFree: 0 kB\n";
        let m = parse(text).unwrap();
        assert_eq!(m.used_percent(), 0.0);
        assert_eq!(m.swap_used_percent(), 0.0);
    }

    #[test]
    fn swap_percent() {
        let text = "\
MemTotal: 1000 kB
MemFree:  500 kB
MemAvailable: 600 kB
SwapTotal: 2000 kB
SwapFree:  1500 kB
";
        let m = parse(text).unwrap();
        assert!((m.swap_used_percent() - 25.0).abs() < 1e-9);
    }
}
