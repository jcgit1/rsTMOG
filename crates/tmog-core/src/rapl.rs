//! `/sys/class/power_supply` (battery) and `/sys/class/powercap` (RAPL).
//! Both are warn-only collectors on hardware without them.

use crate::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    pub present: bool,
    /// 0..=100
    pub percent: Option<f64>,
    /// watts (µW source divided out)
    pub power_w: Option<f64>,
    pub status: Option<String>,
    pub cycle_count: Option<u64>,
    /// seconds, from energy/charge remaining over power/current
    pub time_remaining_s: Option<u64>,
}

/// Read one `/sys/class/power_supply/BAT*` dir. Handles both ACPI unit
/// variants: POWER_NOW(µW) directly, or VOLTAGE_NOW(µV)*CURRENT_NOW(µA);
/// same split for time-remaining via energy_now vs charge_now.
pub fn read_battery(root: &Path, name: &str) -> Battery {
    let rel = |f: &str| format!("sys/class/power_supply/{name}/{f}");
    let num = |f: &str| fs::read_u64_under(root, &rel(f)).ok();
    let txt = |f: &str| {
        fs::read_under(root, &rel(f))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let present = txt("present").is_some_and(|p| p == "1");
    let percent = num("capacity").map(|c| c.min(100) as f64);
    let power_w = num("power_now").map(|uw| uw as f64 / 1e6).or_else(|| {
        let v = num("voltage_now")?;
        let a = num("current_now")?;
        Some(v as f64 * a as f64 / 1e12)
    });
    let time_remaining_s = num("energy_now")
        .zip(num("power_now"))
        .and_then(|(e, p)| (e * 3600).checked_div(p))
        .or_else(|| {
            num("charge_now")
                .zip(num("current_now"))
                .and_then(|(c, a)| (c * 3600).checked_div(a))
        });
    Battery {
        present,
        percent,
        power_w,
        status: txt("status"),
        cycle_count: num("cycle_count"),
        time_remaining_s,
    }
}

/// First present battery device (BAT* sorted).
pub fn collect_battery(root: &Path) -> Option<Battery> {
    let dirs = fs::list_dir_sorted(root, "sys/class/power_supply").ok()?;
    for d in dirs {
        let name = d.file_name()?.to_str()?;
        if name.starts_with("BAT") {
            let b = read_battery(root, name);
            if b.present {
                return Some(b);
            }
        }
    }
    None
}

/// RAPL energy consumption between samples. `energy_uj` is cumulative
/// microjoules wrapping at `max_energy_range_uj`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Rapl {
    last_uj: Option<u64>,
    last_us: Option<u64>,
    max_uj: u64,
}

impl Rapl {
    pub fn new(max_energy_range_uj: u64) -> Self {
        Self {
            last_uj: None,
            last_us: None,
            max_uj: max_energy_range_uj,
        }
    }

    /// Feed energy_uj + wall clock µs; watts over the window, or None on
    /// seed / zero-elapsed / unknown (0) range / degenerate wrap.
    pub fn sample(&mut self, now_uj: u64, now_us: u64) -> Option<f64> {
        let result = match (self.last_uj, self.last_us) {
            (Some(prev_uj), Some(prev_us)) => {
                let elapsed = now_us.checked_sub(prev_us)?;
                if elapsed == 0 || self.max_uj == 0 {
                    None
                } else if now_uj >= prev_uj {
                    Some(now_uj - prev_uj)
                } else if prev_uj < self.max_uj {
                    // single wrap within the window
                    Some((self.max_uj - prev_uj) + now_uj)
                } else {
                    None
                }
                .map(|d_uj| d_uj as f64 / elapsed as f64) // µJ/µs = W
            }
            _ => None,
        };
        self.last_uj = Some(now_uj);
        self.last_us = Some(now_us);
        result
    }
}

/// Read the package RAPL domain's energy_uj + max range under `root`.
/// Returns (energy_uj, max_energy_range_uj); no RAPL → Err.
pub fn read_package_energy(root: &Path) -> Result<(u64, u64), String> {
    let dirs = fs::list_dir_sorted(root, "sys/class/powercap")
        .map_err(|e| format!("read powercap: {e}"))?;
    for d in dirs {
        let name = d
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        if !name.starts_with("intel-rapl") {
            continue;
        }
        let rel = format!("sys/class/powercap/{name}");
        // package domain: "package-0", or the intel-rapl:N root without "-"
        let is_package =
            name == "intel-rapl:0" || name.ends_with(":0") || name.contains("package-0");
        if !is_package {
            continue;
        }
        if let (Ok(e), Ok(m)) = (
            fs::read_u64_under(root, &format!("{rel}/energy_uj")),
            fs::read_u64_under(root, &format!("{rel}/max_energy_range_uj")),
        ) {
            return Ok((e, m));
        }
    }
    Err("no RAPL package domain readable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    #[test]
    fn battery_power_now_variant() {
        // fixture BAT0: power_now + energy_now (µW/µWh)
        let b = read_battery(&root(), "BAT0");
        assert!(b.present);
        assert_eq!(b.percent, Some(83.0));
        // power_now = 12_500_000 µW = 12.5 W
        assert!((b.power_w.unwrap() - 12.5).abs() < 1e-9);
        assert_eq!(b.status.as_deref(), Some("Discharging"));
        assert_eq!(b.cycle_count, Some(214));
        // energy 41666666 µWh @ 12.5 W -> 41666666*3600/12500000 = 11999.9 s
        // (integer floor in-kernel style); accept the floored second
        assert_eq!(b.time_remaining_s, Some(11999));
    }

    #[test]
    fn battery_voltage_current_variant() {
        // fixture BAT1: no power_now/energy_now; voltage 11.4 V * 2 A = 22.8 W
        let b = read_battery(&root(), "BAT1");
        assert!(b.present);
        assert_eq!(b.percent, Some(50.0));
        assert!((b.power_w.unwrap() - 22.8).abs() < 1e-6);
        // charge_now 2500000 µAh / current 2000000 µA -> 4500 s
        assert_eq!(b.time_remaining_s, Some(4500));
    }

    #[test]
    fn battery_absent_is_none() {
        // AC-only fixture ACAD is not a BAT*; collect_battery finds BAT0 though,
        // so test the device-level absence via a nonexistent name
        let b = read_battery(&root(), "NOPE");
        assert!(!b.present);
        assert_eq!(b.percent, None);
    }

    #[test]
    fn rapl_delta_and_wrap() {
        let mut r = Rapl::new(1_000_000_000); // 1e9 µJ range
        assert_eq!(r.sample(1000, 1_000_000), None); // seed
        // delta = 5_000_001-1000 = 4_999_001 µJ over 1 s = 4.999001 W
        assert_eq!(r.sample(5_000_001, 2_000_000), Some(4.999001));
        // wrap: prev 995_000_000 -> now 5_000_000, range 1e9 => 10 MJ over 1 s = 10 W
        let mut r2 = Rapl::new(1_000_000_000);
        r2.sample(995_000_000, 0);
        assert_eq!(r2.sample(5_000_000, 1_000_000), Some(10.0));
        // unknown range -> always None (refuse to guess)
        let mut r3 = Rapl::new(0);
        r3.sample(10, 0);
        assert_eq!(r3.sample(20, 1_000_000), None);
    }

    #[test]
    fn rapl_fixture_package_read() {
        let (e, m) = read_package_energy(&root()).expect("fixture RAPL");
        assert_eq!(e, 123_456_789);
        assert_eq!(m, 262_144_000_000);
    }
}
