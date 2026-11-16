//! `/proc/vmstat` (counter subset) and `/proc/pressure/*` (PSI).

use crate::fs;
use std::path::Path;

/// Parse `key value` lines of `/proc/vmstat` into a map; non-integer
/// values are ignored.
pub fn parse_vmstat(text: &str) -> std::collections::HashMap<String, u64> {
    let mut out = std::collections::HashMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(' ')
            && let Ok(n) = v.trim().parse::<u64>()
        {
            out.insert(k.to_string(), n);
        }
    }
    out
}

/// PSI averages for one "some" or "full" line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsiAvg {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
    pub total_us: u64,
}

/// Stall accumulated between two reads of one PSI file.
///
/// `avg10`/`avg60`/`avg300` are the kernel's decaying windows. They print
/// `0.00` as soon as the stall falls out of that window, which is most of
/// the time on a machine that is not stalled *right now*. `total` is the
/// cumulative stall in microseconds and keeps moving. The live percent is
/// that counter's delta over wall time, clamped to 100.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsiRate {
    pub some_percent: f64,
    pub full_percent: Option<f64>,
    pub some_us: u64,
    pub full_us: Option<u64>,
    pub elapsed_us: u64,
}

/// Share of `elapsed_us` that was stalled, from a PSI `total` counter.
/// `None` when the clock did not advance or the counter stepped backwards
/// (a restart): the caller re-baselines instead of drawing a spike.
pub fn stall_percent(prev_total_us: u64, total_us: u64, elapsed_us: u64) -> Option<f64> {
    if elapsed_us == 0 {
        return None;
    }
    let delta = total_us.checked_sub(prev_total_us)?;
    Some((delta as f64 / elapsed_us as f64 * 100.0).min(100.0))
}

/// Live some/full stall between two samples. `full` is absent on CPU PSI.
/// A backwards `full` counter drops the whole sample so both series
/// re-baseline together.
pub fn psi_rate(
    prev_some_us: u64,
    some_us: u64,
    prev_full_us: Option<u64>,
    full_us: Option<u64>,
    elapsed_us: u64,
) -> Option<PsiRate> {
    let some_percent = stall_percent(prev_some_us, some_us, elapsed_us)?;
    let some_delta = some_us - prev_some_us;
    let (full_percent, full_delta) = match (prev_full_us, full_us) {
        (Some(prev), Some(now)) => {
            let pct = stall_percent(prev, now, elapsed_us)?;
            (Some(pct), Some(now - prev))
        }
        _ => (None, None),
    };
    Some(PsiRate {
        some_percent,
        full_percent,
        some_us: some_delta,
        full_us: full_delta,
        elapsed_us,
    })
}

/// Parse one PSI pressure file. Format:
///   some avg10=0.16 avg60=0.25 avg300=0.19 total=000123456
///   full avg10=0.00 avg60=0.00 avg300=0.00 total=000000000
/// cpu pressure has only "some". Missing `full` → None.
pub fn parse_pressure(text: &str) -> Result<(PsiAvg, Option<PsiAvg>), String> {
    let mut some = None;
    let mut full = None;
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let Some(kind) = it.next() else { continue };
        let mut avg = PsiAvg {
            avg10: 0.0,
            avg60: 0.0,
            avg300: 0.0,
            total_us: 0,
        };
        let mut got_any = false;
        for tok in it {
            let Some((k, v)) = tok.split_once('=') else {
                continue;
            };
            match k {
                "avg10" => avg.avg10 = v.parse().unwrap_or(0.0),
                "avg60" => avg.avg60 = v.parse().unwrap_or(0.0),
                "avg300" => avg.avg300 = v.parse().unwrap_or(0.0),
                "total" => avg.total_us = v.parse().unwrap_or(0),
                _ => continue,
            }
            got_any = true;
        }
        if !got_any {
            continue;
        }
        match kind {
            "some" => some = Some(avg),
            "full" => full = Some(avg),
            _ => {}
        }
    }
    some.ok_or_else(|| "pressure: no 'some' line".into())
        .map(|s| (s, full))
}

/// Collect PSI for one resource ("cpu", "memory", "io") under `root`.
/// Absent file (kernel without PSI) → Ok(None).
pub fn collect_pressure(
    root: &Path,
    resource: &str,
) -> Result<Option<(PsiAvg, Option<PsiAvg>)>, String> {
    let path = root.join(format!("proc/pressure/{resource}"));
    match fs::read_to_string(&path) {
        Ok(t) => parse_pressure(&t).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read pressure/{resource}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmstat_subset() {
        let text = "nr_free_pages 1204567\npgfault 88274651\nbad line here\npswpin 0\n";
        let m = parse_vmstat(text);
        assert_eq!(m.get("nr_free_pages"), Some(&1204567));
        assert_eq!(m.get("pgfault"), Some(&88274651));
        assert_eq!(m.get("pswpin"), Some(&0));
        assert!(!m.contains_key("bad"));
    }

    #[test]
    fn psi_some_and_full() {
        let text = "\
some avg10=0.16 avg60=0.25 avg300=0.19 total=00000123456
full avg10=0.00 avg60=0.05 avg300=0.01 total=00000000789
";
        let (s, f) = parse_pressure(text).unwrap();
        assert!((s.avg10 - 0.16).abs() < 1e-9);
        assert!((s.avg300 - 0.19).abs() < 1e-9);
        assert_eq!(s.total_us, 123456);
        let f = f.expect("full present");
        assert!((f.avg60 - 0.05).abs() < 1e-9);
    }

    #[test]
    fn psi_cpu_some_only() {
        let (s, f) = parse_pressure("some avg10=1.00 avg60=2.00 avg300=3.00 total=42\n").unwrap();
        assert!((s.avg60 - 2.0).abs() < 1e-9);
        assert!(f.is_none());
    }

    #[test]
    fn psi_garbage_errors() {
        assert!(parse_pressure("").is_err());
        assert!(parse_pressure("full avg10=1.0 avg60=1.0 avg300=1.0 total=5\n").is_err());
    }

    #[test]
    fn stall_percent_is_the_counter_delta() {
        // 0.5s stalled in a 1s window
        assert_eq!(stall_percent(1_000, 501_000, 1_000_000), Some(50.0));
        // idle interval
        assert_eq!(stall_percent(10, 10, 1_000_000), Some(0.0));
        // below the kernel's 0.01% print resolution: 50µs in 1s
        let tiny = stall_percent(0, 50, 1_000_000).unwrap();
        assert!((tiny - 0.005).abs() < 1e-9);
        assert!(stall_percent(10, 20, 0).is_none());
        assert!(stall_percent(20, 10, 1_000).is_none());
        // a clock glitch must not draw a spike past 100%
        assert_eq!(stall_percent(0, 5_000, 1_000), Some(100.0));
    }

    #[test]
    fn psi_rate_tracks_some_and_full() {
        let r = psi_rate(1_000, 11_000, Some(500), Some(2_500), 1_000_000).unwrap();
        assert!((r.some_percent - 1.0).abs() < 1e-9);
        assert!((r.full_percent.unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(r.some_us, 10_000);
        assert_eq!(r.full_us, Some(2_000));
        // CPU pressure has no full line
        let cpu = psi_rate(0, 0, None, None, 1_000_000).unwrap();
        assert!(cpu.full_percent.is_none());
        assert!(psi_rate(0, 1, Some(5), Some(4), 1_000).is_none());
    }
}
