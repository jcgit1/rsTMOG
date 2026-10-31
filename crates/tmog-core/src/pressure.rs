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
}
