//! `/sys/class/hwmon`: temperature / fan / voltage sensors, with the
//! CPU-package headline selection the C++ project learned the hard way
//! (headline = the CPU package, NOT the hottest sensor).

use crate::core::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensorKind {
    Temp,
    Fan,
    Voltage,
    Other,
}

/// One sensor reading from an hwmon chip directory.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub chip: String,  // hwmon chip name, e.g. "coretemp_0"
    pub label: String, // sensor label, e.g. "Package id 0"
    pub kind: SensorKind,
    /// temp: degrees C; fan: RPM; voltage: mV
    pub value: f64,
}

/// Parse an hwmon input value with milli-scaling ("45800" -> 45.8 C for
/// temps; "1231" RPM for fans is NOT milliscalled — fans/in/voltage inputs
/// are direct except temp which is millidegrees).
fn parse_tempmilli(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok().map(|v| v / 1000.0)
}

fn parse_plain(text: &str) -> Option<f64> {
    text.trim().parse::<f64>().ok()
}

/// Collect all readings for one hwmon directory (e.g. root-relative
/// "sys/class/hwmon/hwmon0"). Sensors whose label ends in fault semantics
/// are skipped when `label_faulted` (we simply skip unreadable inputs).
pub fn read_chip(root: &Path, rel: &str) -> Vec<Reading> {
    let mut out = Vec::new();
    let chip = fs::read_under(root, &format!("{rel}/name"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| rel.rsplit('/').next().unwrap_or(rel).to_string());
    let entries = match fs::list_dir_sorted(root, rel) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for path in entries {
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // temp1_input / fan2_input / in3_input ... (skip _label/_max/_crit
        // companion files by requiring "_input" suffix)
        if !fname.ends_with("_input") {
            continue;
        }
        let stem = &fname[..fname.len() - "_input".len()];
        let text = match fs::read_under(root, &format!("{rel}/{fname}")) {
            Ok(t) => t,
            Err(_) => continue, // faulted/permission: skip, like the C++ ref
        };
        // sensor index for label lookup: temp1 -> temp1_label
        let label = fs::read_under(root, &format!("{rel}/{stem}_label"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| stem.to_string());
        let (kind, value) = if let Some(v) = stem.strip_prefix("temp") {
            let _ = v;
            (SensorKind::Temp, parse_tempmilli(&text))
        } else if stem.starts_with("fan") {
            (SensorKind::Fan, parse_plain(&text))
        } else if stem.starts_with("in") {
            (SensorKind::Voltage, parse_plain(&text))
        } else {
            (SensorKind::Other, None)
        };
        if let Some(val) = value {
            // fans report 0 when nothing is plugged in — skip to reduce noise
            if kind == SensorKind::Fan && val == 0.0 {
                continue;
            }
            out.push(Reading {
                chip: chip.clone(),
                label,
                kind,
                value: val,
            });
        }
    }
    out
}

/// All hwmon chips under `root`.
pub fn collect(root: &Path) -> Vec<Reading> {
    let mut all = Vec::new();
    for chip_dir in fs::list_dir_sorted(root, "sys/class/hwmon").unwrap_or_default() {
        let rel = chip_dir.strip_prefix(root).unwrap_or(&chip_dir);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        all.extend(read_chip(root, &rel_str));
    }
    all
}

/// Pick the headline CPU temperature. Priority: coretemp "Package id 0";
/// AMD k10temp Tctl/Tdie; ARM cpu_thermal; hottest coretemp core; acpitz.
/// None when no CPU-family sensor exists — deliberately never falling back
/// to the hottest sensor (the evildojo lesson: an NVMe ran 30° over the
/// package and made the headline disagree with btop).
pub fn select_cpu_temp(readings: &[Reading]) -> Option<&Reading> {
    let find = |chip: &str, label: &str| {
        readings
            .iter()
            .find(|r| r.kind == SensorKind::Temp && r.chip == chip && r.label == label)
    };
    if let Some(r) = find("coretemp", "Package id 0") {
        return Some(r);
    }
    // k10temp labels Tctl/Tdie as one label "Tctl" or "Tdie"
    for lbl in ["Tctl", "Tdie", "Tctl/Tdie"] {
        if let Some(r) = find("k10temp", lbl) {
            return Some(r);
        }
    }
    if let Some(r) = readings
        .iter()
        .find(|r| r.kind == SensorKind::Temp && r.chip == "cpu_thermal")
    {
        return Some(r);
    }
    let cores: Vec<&Reading> = readings
        .iter()
        .filter(|r| {
            r.kind == SensorKind::Temp && r.chip == "coretemp" && r.label.starts_with("Core")
        })
        .collect();
    if let Some(m) = cores
        .iter()
        .max_by(|a, b| a.value.partial_cmp(&b.value).unwrap())
    {
        return Some(m);
    }
    readings
        .iter()
        .find(|r| r.kind == SensorKind::Temp && r.chip == "acpitz")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    #[test]
    fn chip_read_scaling_and_skips() {
        let chips = read_chip(&root(), "sys/class/hwmon/hwmon0");
        // fixture: coretemp Package id 0 (45800 -> 45.8C), Core 0 (43000),
        // fan1_input=1200 RPM, fan2_input=0 (skipped), in1_input=1231 mV
        assert!(
            chips
                .iter()
                .any(|r| r.label == "Package id 0" && (r.value - 45.8).abs() < 1e-9)
        );
        assert!(
            chips
                .iter()
                .any(|r| r.label == "Core 0" && (r.value - 43.0).abs() < 1e-9)
        );
        let fans: Vec<_> = chips.iter().filter(|r| r.kind == SensorKind::Fan).collect();
        assert_eq!(fans.len(), 1); // zero-RPM fan2 skipped
        assert!((fans[0].value - 1200.0).abs() < 1e-9);
        assert!(
            chips
                .iter()
                .any(|r| r.kind == SensorKind::Voltage && (r.value - 1231.0).abs() < 1e-9)
        );
    }

    #[test]
    fn collects_multiple_chips() {
        let all = collect(&root());
        assert!(all.iter().any(|r| r.chip == "coretemp"));
        assert!(
            all.iter()
                .any(|r| r.chip == "nvme" && (r.value - 57.2).abs() < 1e-9)
        );
    }

    #[test]
    fn headline_prefers_package_not_hottest() {
        let all = collect(&root());
        let hot = select_cpu_temp(&all).expect("cpu temp");
        // nvme sensor is hotter (57.2) but package (45.8) must win
        assert_eq!(hot.label, "Package id 0");
        assert!((hot.value - 45.8).abs() < 1e-9);
    }

    #[test]
    fn headline_amd_then_core_then_acpitz() {
        let mk = |chip: &str, label: &str, v: f64| Reading {
            chip: chip.into(),
            label: label.into(),
            kind: SensorKind::Temp,
            value: v,
        };
        let amd = vec![mk("nvme", "Composite", 60.0), mk("k10temp", "Tctl", 51.0)];
        assert_eq!(select_cpu_temp(&amd).unwrap().chip, "k10temp");
        let cores = vec![
            mk("nvme", "Composite", 60.0),
            mk("coretemp", "Core 1", 40.0),
            mk("coretemp", "Core 5", 44.0),
        ];
        assert_eq!(select_cpu_temp(&cores).unwrap().label, "Core 5");
        let acpi = vec![mk("acpitz", "acpitz", 39.0)];
        assert_eq!(select_cpu_temp(&acpi).unwrap().chip, "acpitz");
        // nothing cpu-ish -> None (no fallback to hottest!)
        assert!(select_cpu_temp(&[mk("nvme", "Composite", 70.0)]).is_none());
    }
}
