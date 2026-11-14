//! `/proc/diskstats` + `/sys/class/block`: per-device counters, identity,
//! and physical-disk filtering.

use crate::fs;
use std::path::Path;

/// One diskstats line. Counters are cumulative; bytes fields are sectors
/// × 512 (the kernel's fixed unit, regardless of physical sector size).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCounters {
    pub name: String,
    pub reads: u64,
    pub read_bytes: u64,
    pub writes: u64,
    pub write_bytes: u64,
    /// ms spent doing I/O (field 10, "busy time") — util% = delta/elapsed_ms
    pub busy_ms: u64,
}

/// Parse one `/proc/diskstats` line. Old kernels have 14 fields, modern
/// 18+; we take the first 10 and tolerate the rest. Field indices follow
/// the kernel docs (0-based): f[3]=reads completed, f[5]=sectors read,
/// f[7]=writes completed, f[9]=sectors written, f[12]=ms doing I/O.
/// Sectors are always 512 bytes regardless of physical sector size.
pub fn parse_diskstats_line(line: &str) -> Option<DiskCounters> {
    let f: Vec<&str> = line.split_whitespace().collect();
    if f.len() < 14 {
        return None;
    }
    let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    Some(DiskCounters {
        name: f[2].to_string(),
        reads: n(3),
        read_bytes: n(5) * 512,
        writes: n(7),
        write_bytes: n(9) * 512,
        busy_ms: n(12),
    })
}

/// Devices that are not physical disks and should not get a graph:
/// loop/ram/zram/zd (zvols)/dm mappings are excluded by the GUI layer.
pub fn is_physical_candidate(name: &str) -> bool {
    !name.starts_with("loop")
        && !name.starts_with("ram")
        && !name.starts_with("zram")
        && !name.starts_with("zd")
        && !name.starts_with("dm-")
}

/// Heuristic partition test on the name: sdXN (letter + digits), nvmeXnYpZ,
/// mmcblkXpY, vdaN. Used when sysfs `partition` link is unavailable.
pub fn looks_like_partition(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("mmcblk") {
        // mmcblk0p2; bare mmcblk0 is the disk
        return rest.contains('p');
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        // nvme0n1p1 → 'p' followed by digits, with a non-empty head;
        // nvme0n1 has no 'p' → false
        match rest.rfind('p') {
            Some(pidx) => {
                let tail = &rest[pidx + 1..];
                pidx > 0 && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit())
            }
            None => false,
        }
    } else if let Some(rest) = name
        .strip_prefix("sd")
        .or(name.strip_prefix("vd"))
        .or(name.strip_prefix("hd"))
    {
        // sda1 / vda2: at least one non-digit letter then all digits.
        // "da1" for sda1: split at first digit, head letters, tail digits.
        match rest.find(|c: char| c.is_ascii_digit()) {
            Some(i) if i > 0 => rest[i..].bytes().all(|b| b.is_ascii_digit()),
            _ => false,
        }
    } else {
        false
    }
}

/// sysfs identity for one block device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskIdentity {
    pub model: Option<String>,
    /// 0 = SSD/NVMe, 1 = rotational
    pub rotational: Option<u64>,
}

pub fn read_identity(sysfs_root: &Path, name: &str) -> DiskIdentity {
    let rel = |f: &str| format!("sys/class/block/{name}/{f}");
    // nvme devices keep model under device/model; SATA under device/model too
    let model = fs::read_under(sysfs_root, &rel("device/model"))
        .or_else(|_| fs::read_under(sysfs_root, &rel("device/../device/model")))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    DiskIdentity {
        model,
        rotational: fs::read_u64_under(sysfs_root, &rel("queue/rotational")).ok(),
    }
}

/// Whole disk that owns `name`. A partition (`nvme1n1p2`, `sda1`) maps to
/// its disk; a disk name is returned unchanged. Rates are sampled on the
/// disk, not the partition.
pub fn physical_disk_name(name: &str) -> String {
    if !looks_like_partition(name) {
        return name.to_string();
    }
    if name.starts_with("nvme") || name.starts_with("mmcblk") {
        if let Some(idx) = name.rfind('p') {
            let tail = &name[idx + 1..];
            if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) {
                return name[..idx].to_string();
            }
        }
    }
    let bare = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if bare.is_empty() {
        name.to_string()
    } else {
        bare.to_string()
    }
}

/// Block device that holds `/`, reduced to the physical disk we sample.
/// `text` is `/proc/mounts`. Non-block roots (overlay, tmpfs) yield None.
pub fn root_disk_from_mounts(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(dev) = parts.next() else { continue };
        let Some(mnt) = parts.next() else { continue };
        if mnt != "/" {
            continue;
        }
        // tmpfs, overlay, and btrfs subvolume ids are not disks we sample.
        if !dev.starts_with("/dev/") {
            return None;
        }
        let name = dev.rsplit('/').next().unwrap_or(dev);
        if name.is_empty() || !is_physical_candidate(name) {
            return None;
        }
        return Some(physical_disk_name(name));
    }
    None
}

/// True when the device has a `partition` attribute (it IS a partition).
pub fn is_partition(sysfs_root: &Path, name: &str) -> bool {
    fs::read_u64_under(sysfs_root, &format!("sys/class/block/{name}/partition")).unwrap_or(0) != 0
}

/// Read and parse all diskstats lines under `root`.
pub fn collect(root: &Path) -> Result<Vec<DiskCounters>, String> {
    let text =
        fs::read_under(root, "proc/diskstats").map_err(|e| format!("read diskstats: {e}"))?;
    Ok(text.lines().filter_map(parse_diskstats_line).collect())
}

/// util% between samples: busy_ms delta over wall-clock elapsed ms,
/// clamped 0..100 (a device can appear >100% momentarily with queued I/O).
pub fn util_percent(prev_busy_ms: u64, now_busy_ms: u64, elapsed_ms: u64) -> Option<f64> {
    if elapsed_ms == 0 {
        return None;
    }
    let d = now_busy_ms.saturating_sub(prev_busy_ms);
    Some((d as f64 * 100.0 / elapsed_ms as f64).clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NVME: &str = "259 0 nvme1n1 330806 40672 29025589 189005 299859 96179 10788918 492436 0 178017 684999 684 0 10468704 1629 2745 1929";
    const OLD_FMT: &str = "8 0 sda 5494 24 202288 10941 0 0 0 0 0 7194 10941";

    #[test]
    fn root_mount_maps_to_the_physical_disk() {
        assert_eq!(physical_disk_name("nvme1n1p2"), "nvme1n1");
        assert_eq!(physical_disk_name("nvme0n1"), "nvme0n1");
        assert_eq!(physical_disk_name("sda1"), "sda");
        assert_eq!(physical_disk_name("mmcblk0p2"), "mmcblk0");
        let mounts = "\
/dev/nvme1n1p1 /boot ext4 rw 0 0
/dev/nvme1n1p2 / btrfs rw,relatime 0 0
";
        assert_eq!(root_disk_from_mounts(mounts).as_deref(), Some("nvme1n1"));
        assert_eq!(root_disk_from_mounts("overlay / overlay rw 0 0"), None);
    }

    #[test]
    fn parses_modern_and_old_lines() {
        let d = parse_diskstats_line(NVME).unwrap();
        assert_eq!(d.name, "nvme1n1");
        assert_eq!(d.reads, 330806);
        assert_eq!(d.read_bytes, 29025589 * 512);
        assert_eq!(d.writes, 299859);
        assert_eq!(d.write_bytes, 10788918 * 512);
        assert_eq!(d.busy_ms, 178017);
        let o = parse_diskstats_line(OLD_FMT).unwrap();
        assert_eq!(o.name, "sda");
        assert_eq!(o.busy_ms, 7194);
    }

    #[test]
    fn rejects_junk() {
        assert!(parse_diskstats_line("").is_none());
        assert!(parse_diskstats_line("8 0 sda 1 2 3").is_none());
    }

    #[test]
    fn physical_filtering() {
        assert!(is_physical_candidate("nvme0n1"));
        assert!(is_physical_candidate("sda"));
        assert!(!is_physical_candidate("loop0"));
        assert!(!is_physical_candidate("zram0"));
        assert!(!is_physical_candidate("zd0"));
        assert!(!is_physical_candidate("dm-0"));
    }

    #[test]
    fn partition_name_heuristic() {
        assert!(looks_like_partition("sda1"));
        assert!(looks_like_partition("sdb12"));
        assert!(looks_like_partition("nvme0n1p1"));
        assert!(looks_like_partition("vda2"));
        assert!(looks_like_partition("mmcblk0p2"));
        assert!(!looks_like_partition("sda"));
        assert!(!looks_like_partition("nvme0n1"));
        assert!(!looks_like_partition("nvme1n1")); // no pXX
        assert!(!looks_like_partition("zram0"));
    }

    #[test]
    fn util_math() {
        assert_eq!(util_percent(100, 250, 1000), Some(15.0));
        assert_eq!(util_percent(100, 100, 1000), Some(0.0));
        assert_eq!(util_percent(0, 2000, 1000), Some(100.0)); // clamped
        assert_eq!(util_percent(0, 10, 0), None);
    }

    #[test]
    fn identity_and_partition_from_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let id = read_identity(&root, "nvme0n1");
        assert_eq!(id.model.as_deref(), Some("Test SSD 9900"));
        assert_eq!(id.rotational, Some(0));
        assert!(!is_partition(&root, "nvme0n1"));
        assert!(is_partition(&root, "nvme0n1p1"));
        // unknown device → all None
        let u = read_identity(&root, "nonexistent");
        assert_eq!(
            u,
            DiskIdentity {
                model: None,
                rotational: None
            }
        );
    }
}
