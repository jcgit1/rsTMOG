//! `/proc/loadavg` and `/proc/uptime`.

use crate::core::fs;
use std::path::Path;

/// One-minute / five / fifteen load averages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadAvg {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
    pub running: u32,
    pub total: u32,
}

/// Parse `/proc/loadavg` ("0.52 0.41 0.38 1/512 12345").
pub fn parse_loadavg(text: &str) -> Result<LoadAvg, String> {
    let mut it = text.split_whitespace();
    let mut f32s = || {
        it.next()
            .and_then(|v| v.parse::<f64>().ok())
            .ok_or_else(|| "loadavg: missing field".to_string())
    };
    let one = f32s()?;
    let five = f32s()?;
    let fifteen = f32s()?;
    // "running/total"
    let frac = it
        .next()
        .ok_or_else(|| "loadavg: missing running/total".to_string())?;
    let (r, t) = frac
        .split_once('/')
        .ok_or_else(|| "loadavg: bad running/total".to_string())?;
    Ok(LoadAvg {
        one,
        five,
        fifteen,
        running: r.parse().map_err(|_| "loadavg: bad running".to_string())?,
        total: t.parse().map_err(|_| "loadavg: bad total".to_string())?,
    })
}

/// System uptime in seconds (first field of `/proc/uptime`; fractional).
pub fn parse_uptime(text: &str) -> Result<f64, String> {
    text.split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .ok_or_else(|| "uptime: no value".into())
}

pub fn collect_loadavg(root: &Path) -> Result<LoadAvg, String> {
    let t = fs::read_under(root, "proc/loadavg").map_err(|e| format!("read loadavg: {e}"))?;
    parse_loadavg(&t)
}

pub fn collect_uptime(root: &Path) -> Result<f64, String> {
    let t = fs::read_under(root, "proc/uptime").map_err(|e| format!("read uptime: {e}"))?;
    parse_uptime(&t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loadavg_full_line() {
        let l = parse_loadavg("0.52 0.41 0.38 1/512 12345\n").unwrap();
        assert!((l.one - 0.52).abs() < 1e-9);
        assert!((l.fifteen - 0.38).abs() < 1e-9);
        assert_eq!(l.running, 1);
        assert_eq!(l.total, 512);
    }

    #[test]
    fn loadavg_rejects_garbage() {
        assert!(parse_loadavg("").is_err());
        assert!(parse_loadavg("a b c d/e f").is_err());
        assert!(parse_loadavg("0.1 0.2 0.3 nofrac 9").is_err());
    }

    #[test]
    fn uptime_seconds() {
        assert!((parse_uptime("123456.78 98765.43\n").unwrap() - 123456.78).abs() < 1e-9);
        assert!(parse_uptime("").is_err());
    }
}
