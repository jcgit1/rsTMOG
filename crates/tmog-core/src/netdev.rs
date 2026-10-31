//! Network interfaces: `/proc/net/dev` counters + `/sys/class/net/*`
//! identity (speed, operstate, mtu, mac, kind).

use crate::fs;
use crate::ring::{Rate, RateTracker};
use std::path::Path;

/// Cumulative rx/tx counters for one interface, from `/proc/net/dev`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetDevCounters {
    pub name: String,
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_drop: u64,
    pub rx_errs: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_drop: u64,
    pub tx_errs: u64,
}

/// Parse one `/proc/net/dev` body line ("eth0: 123 456 ..."). Header lines
/// and malformed rows return None.
pub fn parse_netdev_line(line: &str) -> Option<NetDevCounters> {
    let (name, rest) = line.split_once(':')?;
    let name = name.trim();
    if name.is_empty() || line.starts_with("  Inter") || line.contains("face") {
        return None;
    }
    let f: Vec<&str> = rest.split_whitespace().collect();
    if f.len() < 16 {
        return None;
    }
    let n = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    Some(NetDevCounters {
        name: name.to_string(),
        rx_bytes: n(0),
        rx_packets: n(1),
        rx_errs: n(2),
        rx_drop: n(3),
        tx_bytes: n(8),
        tx_packets: n(9),
        tx_errs: n(10),
        tx_drop: n(11),
    })
}

/// Interface identity from sysfs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetIdentity {
    pub name: String,
    /// Mbps link speed; absent/0 for wifi and virtual (None)
    pub speed_mbps: Option<u64>,
    pub operstate: Option<String>,
    pub mtu: Option<u64>,
    pub mac: Option<String>,
}

/// Coarse interface kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetKind {
    Loopback,
    Wifi,
    Wired,
    Virtual,
    Other,
}

pub fn read_identity(sys_root: &Path, name: &str) -> NetIdentity {
    let rel = |f: &str| format!("sys/class/net/{name}/{f}");
    let txt = |f: &str| {
        fs::read_under(sys_root, &rel(f))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let speed = fs::read_u64_under(sys_root, &rel("speed"))
        .ok()
        .filter(|&s| s > 0); // -1/0 on wifi/virtual -> None
    let type_num = fs::read_u64_under(sys_root, &rel("type")).unwrap_or(99);
    let kind = kind_of(name, type_num, txt("wireless").as_deref());
    let _ = kind; // identity keeps raw fields; kind() is a method below
    NetIdentity {
        name: name.to_string(),
        speed_mbps: speed,
        operstate: txt("operstate"),
        mtu: fs::read_u64_under(sys_root, &rel("mtu")).ok(),
        mac: txt("address").filter(|m| !m.is_empty() && m != "00:00:00:00:00:00"),
    }
}

/// Classify by name, arp type (1=ethernet, 776=loopback) and wireless dir.
/// Name-based virtual checks run BEFORE the type check: veth/docker/bridge
/// devices report type 1 (ethernet) but are not physical NICs.
pub fn kind_of(name: &str, type_num: u64, wireless: Option<&str>) -> NetKind {
    if name == "lo" || type_num == 776 {
        NetKind::Loopback
    } else if wireless.is_some() || name.starts_with("wl") {
        NetKind::Wifi
    } else if name.starts_with("veth")
        || name.starts_with("docker")
        || name.starts_with("br-")
        || name.starts_with("virbr")
        || name.starts_with("tap")
        || name.starts_with("tun")
    {
        NetKind::Virtual
    } else if type_num == 1 {
        NetKind::Wired
    } else {
        NetKind::Other
    }
}

impl NetIdentity {
    pub fn kind(&self, sys_root: &Path) -> NetKind {
        let type_num = fs::read_u64_under(sys_root, &format!("sys/class/net/{}/type", self.name))
            .unwrap_or(99);
        let wireless =
            fs::list_dir_sorted(sys_root, &format!("sys/class/net/{}/wireless", self.name))
                .ok()
                .filter(|v| !v.is_empty());
        kind_of(&self.name, type_num, wireless.as_deref().map(|_| "yes"))
    }
}

/// Read + parse all `/proc/net/dev` rows.
pub fn collect(root: &Path) -> Result<Vec<NetDevCounters>, String> {
    let text = fs::read_under(root, "proc/net/dev").map_err(|e| format!("read net/dev: {e}"))?;
    Ok(text.lines().filter_map(parse_netdev_line).collect())
}

/// Rates between ticks for one interface: two trackers (rx/tx), counter
/// wrap/restart re-baselines to Unknown via RateTracker.
#[derive(Debug, Default)]
pub struct NetRates {
    rx: RateTracker,
    tx: RateTracker,
}

impl NetRates {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed counters with a clock in microseconds; returns bytes/sec for
    /// rx and tx (Unknown on seed/reset/zero-elapsed).
    pub fn sample(&mut self, c: &NetDevCounters, now_us: u64) -> (Rate, Rate) {
        // A zero counter-delta maps to Rate::Unknown inside RateTracker
        // ("no rate yet"), but for network counters an idle interface is
        // genuinely 0 B/s, not unknown. Detect zero-progress by comparing
        // against the tracker's last value before sampling.
        let rx_idle = self.rx.last_value() == Some(c.rx_bytes);
        let tx_idle = self.tx.last_value() == Some(c.tx_bytes);
        let rx = self.rx.sample(c.rx_bytes, now_us);
        let tx = self.tx.sample(c.tx_bytes, now_us);
        let fix = |r: Rate, idle: bool| match r {
            Rate::Unknown if idle => Rate::PerSec(0.0),
            other => other,
        };
        (fix(rx, rx_idle), fix(tx, tx_idle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_body_rows_skips_headers() {
        let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 138751141 1602203    0    0    0     0          0         0 138751141 1602203    0    0    0     0       0          0
 wlan0: 17633635323 15524965    0 3844    0     0          0         0 823589096 3012557    0    5    0     0       0          0
";
        let rows: Vec<_> = text.lines().filter_map(parse_netdev_line).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "lo");
        assert_eq!(rows[1].name, "wlan0");
        assert_eq!(rows[1].rx_bytes, 17633635323);
        assert_eq!(rows[1].rx_drop, 3844);
        assert_eq!(rows[1].tx_bytes, 823589096);
        assert_eq!(rows[1].tx_drop, 5);
    }

    #[test]
    fn kind_classification() {
        assert_eq!(kind_of("lo", 776, None), NetKind::Loopback);
        assert_eq!(kind_of("wlan0", 1, None), NetKind::Wifi); // wl prefix wins
        assert_eq!(kind_of("eth0", 1, Some("yes")), NetKind::Wifi); // wireless dir wins over type
        assert_eq!(kind_of("enp6s0", 1, None), NetKind::Wired);
        assert_eq!(kind_of("veth12ab", 1, None), NetKind::Virtual);
        assert_eq!(kind_of("docker0", 1, None), NetKind::Virtual);
        assert_eq!(kind_of("weird0", 99, None), NetKind::Other);
    }

    #[test]
    fn identity_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let id = read_identity(&root, "eth-test");
        assert_eq!(id.speed_mbps, Some(1000));
        assert_eq!(id.operstate.as_deref(), Some("up"));
        assert_eq!(id.mtu, Some(1500));
        assert_eq!(id.mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert_eq!(id.kind(&root), NetKind::Wired);
        // wireless iface: speed 0/-1 -> None, kind Wifi via wireless dir
        let w = read_identity(&root, "wlan-test");
        assert_eq!(w.speed_mbps, None);
        assert_eq!(w.kind(&root), NetKind::Wifi);
        // unknown iface: all None, Other
        let u = read_identity(&root, "nope0");
        assert_eq!(u.speed_mbps, None);
        assert_eq!(u.kind(&root), NetKind::Other);
    }

    #[test]
    fn rates_between_ticks_with_wrap() {
        let base = NetDevCounters {
            name: "eth0".into(),
            rx_bytes: 1000,
            rx_packets: 0,
            rx_drop: 0,
            rx_errs: 0,
            tx_bytes: 2000,
            tx_packets: 0,
            tx_drop: 0,
            tx_errs: 0,
        };
        let mut tr = NetRates::new();
        assert_eq!(tr.sample(&base, 1_000_000), (Rate::Unknown, Rate::Unknown));
        let mut later = base.clone();
        later.rx_bytes = 3500; // 2500 B in 0.5 s = 5000 B/s
        later.tx_bytes = 2000; // no tx
        let (rx, tx) = tr.sample(&later, 1_500_000);
        match (rx, tx) {
            (Rate::PerSec(r), Rate::PerSec(t)) => {
                assert!((r - 5000.0).abs() < 1e-9);
                assert!((t - 0.0).abs() < 1e-9);
            }
            other => panic!("{other:?}"),
        }
        // counters reset backwards -> both Unknown this tick
        let mut reset = base.clone();
        reset.rx_bytes = 10;
        reset.tx_bytes = 5;
        let (rx, _) = tr.sample(&reset, 2_000_000);
        assert_eq!(rx, Rate::Unknown);
    }
}
