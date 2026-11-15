//! ZFS ARC stats (`/proc/spl/kstat/zfs/arcstats`) and ARC-aware memory
//! accounting — a REQUIRED behavior on this fleet, not a stretch goal.
//!
//! The kernel's `MemAvailable` ignores the ZFS ARC entirely, so a box with
//! a huge ARC looks almost out of memory (the evildojo incident: 89% "used"
//! when 62 GiB was reclaimable ARC). We fold reclaimable ARC
//! (`arcstats size − c_min`) into the cached/available view, matching
//! btop's `zfs_arc_cached`.

use crate::fs;
use crate::meminfo::Meminfo;
use std::collections::HashMap;
use std::path::Path;

/// Raw arcstats keys we use (all in bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArcStats {
    /// current ARC size, bytes
    pub size: u64,
    /// ARC min target (never evicted below), bytes
    pub c_min: u64,
    pub c_max: u64,
}

/// Parse the kstat table: "name type data" rows; we take size/c_min/c_max.
/// `c` is the current adaptive target, not the floor, so it must not be
/// read as `c_min`.
pub fn parse_arcstats(text: &str) -> Result<ArcStats, String> {
    let mut m: HashMap<&str, u64> = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() == 3
            && let Ok(v) = f[2].parse::<u64>()
        {
            m.insert(f[0], v);
        }
    }
    Ok(ArcStats {
        size: *m.get("size").ok_or("arcstats: no size")?,
        c_min: *m.get("c_min").ok_or("arcstats: no c_min")?,
        c_max: *m.get("c_max").ok_or("arcstats: no c_max")?,
    })
}

/// Reclaimable ARC bytes: everything above the min target, clamped ≥ 0.
pub fn reclaimable(a: &ArcStats) -> u64 {
    a.size.saturating_sub(a.c_min)
}

/// Read arcstats; None when this box has no ZFS (file absent).
pub fn collect(root: &Path) -> Option<ArcStats> {
    let text = fs::read_under(root, "proc/spl/kstat/zfs/arcstats").ok()?;
    parse_arcstats(&text).ok()
}

/// Adjust a raw Meminfo for the ARC. `arc` None → unchanged values.
/// Returns a NEW Meminfo whose `mem_available` grew by the reclaimable
/// ARC (clamped to mem_total); cached is untouched so the composition bar
/// can still show ARC as its own segment.
pub fn arc_adjusted_meminfo(raw: &Meminfo, arc: Option<&ArcStats>) -> Meminfo {
    let mut m = *raw;
    if let Some(a) = arc {
        let extra = reclaimable(a);
        m.mem_available = Some(
            raw.available_kib()
                .saturating_add(extra / 1024)
                .min(raw.mem_total),
        );
    }
    m
}

/// Segments of the stacked memory composition bar, KiB:
/// (in_use, zfs_arc, cached, buffers, free). ARC is carved out of the
/// raw in-use before it's reported, in this order: arc reclaimable, then
/// kernel cached, then buffers; whatever's left of (total-available) is
/// "in use". Segments are clamped so they always sum to mem_total.
pub fn mem_composition(raw: &Meminfo, arc: Option<&ArcStats>) -> (u64, u64, u64, u64, u64) {
    let total = raw.mem_total;
    let arc_kib = arc.map(reclaimable).unwrap_or(0) / 1024;
    // carve order: arc out of used, then cached/buffers as their own
    let used_raw = raw.used_kib(); // total - available(kernel view)
    let arc_seg = arc_kib.min(used_raw);
    let cached_seg = raw.cached.min(total.saturating_sub(arc_seg));
    let buffers_seg = raw.buffers.min(total.saturating_sub(arc_seg + cached_seg));
    let in_use = used_raw.saturating_sub(arc_seg);
    let accounted = in_use + arc_seg + cached_seg + buffers_seg;
    let free = total.saturating_sub(accounted);
    // rescale-on-drift guard: if rounding/clamping lost bytes, give them
    // to `free` so segments sum to total (free is the slack segment)
    let sum = in_use + arc_seg + cached_seg + buffers_seg + free;
    if sum == total {
        (in_use, arc_seg, cached_seg, buffers_seg, free)
    } else {
        // pathological (cached overlaps available): clamp cached to fit
        let slack = total.saturating_sub(in_use + arc_seg + buffers_seg);
        let cached_seg2 = cached_seg.min(slack);
        let free2 = total.saturating_sub(in_use + arc_seg + cached_seg2 + buffers_seg);
        (in_use, arc_seg, cached_seg2, buffers_seg, free2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    const ARCS: &str = "\
name                            type data
hits                            4    123456
misses                          4    789
size                            4    68719476736
c                               4    34359738368
c_min                           4    4294967296
c_max                           4    103079215104
";

    #[test]
    fn parses_required_keys() {
        let a = parse_arcstats(ARCS).unwrap();
        assert_eq!(a.size, 64 << 30);
        // c_min, not the 32 GiB adaptive target `c`
        assert_eq!(a.c_min, 4 << 30);
        assert_eq!(a.c_max, 96 << 30);
        // reclaimable = 60 GiB
        assert_eq!(reclaimable(&a), 60 << 30);
    }

    #[test]
    fn reclaimable_never_underflows() {
        let a = ArcStats {
            size: 1 << 30,
            c_min: 2 << 30,
            c_max: 8 << 30,
        };
        assert_eq!(reclaimable(&a), 0);
    }

    #[test]
    fn missing_keys_error() {
        assert!(parse_arcstats("size 4 1\n").is_err());
    }

    #[test]
    fn no_zfs_is_none() {
        assert_eq!(collect(std::path::Path::new("/nonexistent-root")), None);
    }

    #[test]
    fn arc_adjusted_available_matches_incident() {
        // reproduce the evildojo scenario: 125 GiB total, kernel says 14 GiB
        // available, but 60 GiB of ARC is reclaimable
        let raw = Meminfo {
            mem_total: 125 * 1024 * 1024, // KiB
            mem_free: 2 * 1024 * 1024,
            mem_available: Some(14 * 1024 * 1024),
            buffers: 300_000,
            cached: 8 * 1024 * 1024,
            s_reclaimable: 0,
            swap_total: 0,
            swap_free: 0,
        };
        let a = parse_arcstats(ARCS).unwrap();
        assert!(raw.used_percent() > 88.0); // kernel view: looks full
        let adj = arc_adjusted_meminfo(&raw, Some(&a));
        // available 14 + 60 = 74 GiB -> used ~40.8%
        assert!((adj.used_percent() - 40.8).abs() < 0.5);
        // clamp: available never exceeds total
        let huge = Meminfo {
            mem_total: 100,
            mem_available: Some(90),
            ..Default::default()
        };
        let clamped = arc_adjusted_meminfo(&huge, Some(&a));
        assert_eq!(clamped.mem_available, Some(100));
        // None arc -> unchanged
        assert_eq!(arc_adjusted_meminfo(&raw, None), raw);
    }

    #[test]
    fn composition_segments_sum_to_total() {
        let raw = Meminfo {
            mem_total: 100_000,
            mem_free: 20_000,
            mem_available: Some(40_000), // used 60_000
            buffers: 5_000,
            cached: 20_000,
            s_reclaimable: 0,
            swap_total: 0,
            swap_free: 0,
        };
        let arc = ArcStats {
            size: 20_000 * 1024,
            c_min: 4_000 * 1024,
            c_max: 50_000 * 1024,
        }; // 16 MiB reclaimable
        let (in_use, arc_s, cached, buffers, free) = mem_composition(&raw, Some(&arc));
        assert_eq!(in_use + arc_s + cached + buffers + free, raw.mem_total);
        assert_eq!(arc_s, 16_000); // KiB reclaimable carved out
        assert_eq!(in_use, 60_000 - 16_000);
        // no-zfs path also sums correctly
        let (i2, a2, c2, b2, f2) = mem_composition(&raw, None);
        assert_eq!(a2, 0);
        assert_eq!(i2 + a2 + c2 + b2 + f2, raw.mem_total);
    }

    #[test]
    fn fixture_arcstats_file_reads() {
        let a = collect(&root()).expect("fixture arcstats");
        assert_eq!(a.size, 1 << 30);
    }
}
