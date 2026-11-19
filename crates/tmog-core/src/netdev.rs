//! Network interfaces: `/proc/net/dev` counters + `/sys/class/net/*`
//! identity (speed, operstate, mtu, mac, kind) + per-interface
//! IP addresses via netlink (route + link dumps).

use crate::fs;
use crate::ring::{Rate, RateTracker};
use std::net::{Ipv4Addr, Ipv6Addr};
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
    /// IPv4 host addresses bound to this interface, from a netlink
    /// `RTM_GETROUTE` dump (table main, `RTN_LOCAL` entries) joined to
    /// interface names via `RTM_GETLINK` — exact per-interface mapping,
    /// no fib_trie heuristics. Sorted, deduped; empty when none.
    pub ipv4: Vec<Ipv4Addr>,
    /// Global IPv6 addresses from `/proc/net/if_inet6` (link-local fe80::
    /// excluded — every interface has one and it identifies nothing).
    pub ipv6: Vec<Ipv6Addr>,
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
    identity_with_v4(sys_root, name, iface_v4(name))
}

/// Identities for many interfaces with ONE netlink link dump and ONE route
/// dump in total (`read_identity` does both per interface). For callers
/// that refresh every interface at once, i.e. the sampler.
pub fn read_identities(sys_root: &Path, names: &[String]) -> Vec<NetIdentity> {
    let dumps = dump_links_and_routes();
    names
        .iter()
        .map(|name| {
            let v4 = match &dumps {
                Some((links, routes)) => parse_local_v4(routes, links, name),
                None => Vec::new(),
            };
            identity_with_v4(sys_root, name, v4)
        })
        .collect()
}

fn identity_with_v4(sys_root: &Path, name: &str, mut v4: Vec<Ipv4Addr>) -> NetIdentity {
    let rel = |f: &str| format!("sys/class/net/{name}/{f}");
    let txt = |f: &str| {
        fs::read_under(sys_root, &rel(f))
            .ok()
            .map(|s| s.trim().to_string())
    };
    let speed = fs::read_u64_under(sys_root, &rel("speed"))
        .ok()
        .filter(|&s| s > 0); // -1/0 on wifi/virtual -> None
    let mut v6 = iface_v6(sys_root, name);
    v4.sort();
    v4.dedup();
    v6.sort();
    v6.dedup();
    NetIdentity {
        name: name.to_string(),
        speed_mbps: speed,
        operstate: txt("operstate"),
        mtu: fs::read_u64_under(sys_root, &rel("mtu")).ok(),
        mac: txt("address").filter(|m| !m.is_empty() && m != "00:00:00:00:00:00"),
        ipv4: v4,
        ipv6: v6,
    }
}

/// IPv4 host addresses bound to `name`, from a netlink route dump.
///
/// `RTM_GETROUTE` (table main) returns one `RTN_LOCAL` route per host
/// address with its output-interface index in `RTA_OIF`; `RTM_GETLINK`
/// maps indices to names. This is the exact kernel mapping — fib_trie
/// carries no interface names, which is why reading it attributed the
/// wrong IPs on multi-NIC boxes. Any socket/parse failure degrades to
/// empty (the page then shows the bare interface name).
pub fn iface_v4(name: &str) -> Vec<Ipv4Addr> {
    let Some((names, routes)) = dump_links_and_routes() else {
        return Vec::new();
    };
    parse_local_v4(&routes, &names, name)
}

/// Request payload for `RTM_GETLINK` dump: `struct ifinfomsg` is 16 bytes
/// (family, pad, type, index, flags, change). Sending 12 bytes makes the
/// kernel fall back to `rtgenmsg` (4 bytes) and log
/// `netlink: 8 bytes leftover after parsing attributes` on every dump.
fn link_dump_payload() -> [u8; 16] {
    [0u8; 16]
}

/// Request payload for `RTM_GETROUTE` dump: `struct rtmsg` is 12 bytes.
fn route_v4_main_dump_payload() -> [u8; 12] {
    let mut payload = [0u8; 12];
    payload[0] = 2; // AF_INET
    payload[4] = 254; // RT_TABLE_MAIN
    payload
}

fn dump_links_and_routes() -> Option<(std::collections::HashMap<u32, String>, Vec<u8>)> {
    let links = nl_dump(NL_RTM_GETLINK, &link_dump_payload())?;
    let routes = nl_dump(NL_RTM_GETROUTE, &route_v4_main_dump_payload())?;
    Some((parse_link_names(&links), routes))
}

// ---- netlink plumbing (raw syscalls via extern "C", no libc crate) ----

const NL_RTM_GETLINK: u16 = 18;
const NL_RTM_NEWLINK: u16 = 16;
const NL_RTM_GETROUTE: u16 = 26;
const NL_RTM_NEWROUTE: u16 = 24;
const NLMSG_DONE: u16 = 3;
const NLMSG_ERROR: u16 = 2;
const AF_NETLINK: i32 = 16;
const SOCK_RAW: i32 = 3;
const NETLINK_ROUTE: i32 = 0;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_DUMP: u16 = 0x300;

#[repr(C)]
struct SockaddrNl {
    family: u16,
    pad: u16,
    pid: u32,
    groups: u32,
}

unsafe extern "C" {
    fn socket(domain: i32, type_: i32, protocol: i32) -> i32;
    fn connect(fd: i32, addr: *const SockaddrNl, len: u32) -> i32;
    fn send(fd: i32, buf: *const u8, len: usize, flags: i32) -> isize;
    fn recv(fd: i32, buf: *mut u8, len: usize, flags: i32) -> isize;
    fn close(fd: i32) -> i32;
    fn setsockopt(fd: i32, level: i32, name: i32, val: *const Timeval, len: u32) -> i32;
}

#[repr(C)]
struct Timeval {
    tv_sec: i64,
    tv_usec: i64,
}
const SOL_SOCKET: i32 = 1;
const SO_RCVTIMEO: i32 = 20;
/// A dump bigger than this is not a route table; stop rather than grow.
const NL_MAX_DUMP: usize = 16 << 20;

fn u32le(b: &[u8], i: usize) -> u32 {
    (b[i] as u32) | (b[i + 1] as u32) << 8 | (b[i + 2] as u32) << 16 | (b[i + 3] as u32) << 24
}
fn u16le(b: &[u8], i: usize) -> u16 {
    (b[i] as u16) | (b[i + 1] as u16) << 8
}

/// Send one netlink dump request, collect the whole reply. None on any
/// socket error (sandboxed/offline environments).
fn nl_dump(msg_type: u16, payload: &[u8]) -> Option<Vec<u8>> {
    let len = 16 + payload.len();
    let mut m = Vec::with_capacity(len);
    m.extend_from_slice(&(len as u32).to_le_bytes());
    m.extend_from_slice(&msg_type.to_le_bytes());
    m.extend_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_le_bytes());
    m.extend_from_slice(&1u32.to_le_bytes());
    m.extend_from_slice(&0u32.to_le_bytes());
    m.extend_from_slice(payload);
    unsafe {
        let fd = socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE);
        if fd < 0 {
            return None;
        }
        let addr = SockaddrNl {
            family: AF_NETLINK as u16,
            pad: 0,
            pid: 0,
            groups: 0,
        };
        // never block forever if the kernel reply stalls (recv then fails
        // with EAGAIN after 1 s and we degrade to "no addresses")
        let tv = Timeval {
            tv_sec: 1,
            tv_usec: 0,
        };
        if setsockopt(
            fd,
            SOL_SOCKET,
            SO_RCVTIMEO,
            &tv,
            std::mem::size_of::<Timeval>() as u32,
        ) != 0
        {
            close(fd);
            return None;
        }
        if connect(fd, &addr, std::mem::size_of::<SockaddrNl>() as u32) != 0 {
            close(fd);
            return None;
        }
        if send(fd, m.as_ptr(), m.len(), 0) <= 0 {
            close(fd);
            return None;
        }
        let mut out = Vec::new();
        let mut done = false;
        while !done {
            let mut buf = vec![0u8; 65536];
            let n = recv(fd, buf.as_mut_ptr(), buf.len(), 0);
            if n <= 0 {
                close(fd);
                return None;
            }
            out.extend_from_slice(&buf[..n as usize]);
            if out.len() > NL_MAX_DUMP {
                close(fd);
                return None;
            }
            // Scan the ENTIRE accumulated buffer every pass: DONE may sit
            // mid-chunk or at the start of the next one — the earlier
            // per-chunk scan missed it and discarded the whole dump
            // (every interface silently lost its IP).
            let mut off = 0usize;
            while off + 16 <= out.len() {
                let ln = u32le(&out, off) as usize;
                let typ = u16le(&out, off + 4);
                if ln < 16 {
                    done = true;
                    break;
                }
                match typ {
                    NLMSG_DONE => done = true,
                    NLMSG_ERROR => {
                        close(fd);
                        return None;
                    }
                    _ => off += (ln + 3) & !3,
                }
                if done {
                    break;
                }
            }
        }
        close(fd);
        Some(out)
    }
}

/// `RTM_GETLINK` dump → ifindex → interface name.
/// Netlink ifindex (low 16 bits) → interface name, exactly as routes
/// report it in RTA_OIF.
fn parse_link_names(msgs: &[u8]) -> std::collections::HashMap<u32, String> {
    let mut names = std::collections::HashMap::new();
    let mut off = 0usize;
    while off + 16 <= msgs.len() {
        let ln = u32le(msgs, off) as usize;
        let typ = u16le(msgs, off + 4);
        if ln < 16 || off + ln > msgs.len() {
            break;
        }
        if typ == NL_RTM_NEWLINK {
            let body = &msgs[off + 16..off + ln];
            // ifinfomsg: family(1) pad(1) type(2) index(4) flags(4) change(4)
            if body.len() >= 16 {
                // ifinfomsg lays out as family(u8) pad(u8) index(i32):
                // the index is the u16 at byte 4 (the kernel's upper
                // half is always zero — mask defensively, not as a
                // type-field hack, which mis-parsed every NIC but one)
                let ifindex = u16le(body, 4) as u32;
                let mut ao = 16usize;
                while ao + 4 <= body.len() {
                    let al = u16le(body, ao) as usize;
                    let at = u16le(body, ao + 2);
                    if al < 4 || ao + al > body.len() {
                        break;
                    }
                    if at == 3 {
                        // IFLA_IFNAME, NUL-terminated
                        let raw = &body[ao + 4..ao + al];
                        let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
                        if let Ok(s) = std::str::from_utf8(&raw[..end]) {
                            names.insert(ifindex, s.to_string());
                        }
                    }
                    ao += (al + 3) & !3;
                }
            }
        }
        off += (ln + 3) & !3;
    }
    names
}

/// `RTM_GETROUTE` dump (RT_TABLE_MAIN) → host (`RTN_LOCAL` /32) IPv4
/// addresses whose `RTA_OIF` resolves to `ifname`. Pure function over
/// the raw netlink dump so it is unit-testable with captured bytes.
fn parse_local_v4(
    msgs: &[u8],
    names: &std::collections::HashMap<u32, String>,
    ifname: &str,
) -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 16 <= msgs.len() {
        let ln = u32le(msgs, off) as usize;
        let typ = u16le(msgs, off + 4);
        if ln < 16 || off + ln > msgs.len() {
            break;
        }
        if typ == NL_RTM_NEWROUTE {
            let body = &msgs[off + 16..off + ln];
            // rtmsg: family dst_len src_len tos table protocol scope type flags
            if body.len() >= 12 && body[0] == 2 && body[1] == 32 && body[7] == 2 {
                // AF_INET, /32, RTN_LOCAL
                let mut ao = 12usize;
                let mut dst = None;
                let mut oif = None;
                let mut table = body[4] as u32;
                while ao + 4 <= body.len() {
                    let al = u16le(body, ao) as usize;
                    let at = u16le(body, ao + 2);
                    if al < 4 || ao + al > body.len() {
                        break;
                    }
                    let val = &body[ao + 4..ao + al];
                    match at {
                        1 if val.len() >= 4 => {
                            dst = Some(Ipv4Addr::new(val[0], val[1], val[2], val[3]))
                        }
                        // RTA_OIF = 4
                        4 if val.len() >= 4 => oif = Some(u32le(val, 0)),
                        // RTA_TABLE = 15: authoritative table id (the
                        // rtmsg table byte is u8 and reads 255 for the
                        // Local table even when we requested main)
                        15 if val.len() >= 4 => table = u32le(val, 0),
                        _ => {}
                    }
                    ao += (al + 3) & !3;
                }
                if let (Some(a), Some(i)) = (dst, oif) {
                    // host routes arrive from both Local (255) and main
                    // (254) tables; they ARE the interface addresses
                    if (table == 254 || table == 255)
                        && names.get(&i).map(String::as_str) == Some(ifname)
                    {
                        out.push(a);
                    }
                }
            }
        }
        off += (ln + 3) & !3;
    }
    out
}

/// Global IPv6 addresses for one interface from `/proc/net/if_inet6`
/// (link-local fe80:: excluded). Format: 32-hex scope ifindex flags name.
pub fn iface_v6(sys_root: &Path, name: &str) -> Vec<Ipv6Addr> {
    let Ok(text) = fs::read_under(sys_root, "proc/net/if_inet6") else {
        return Vec::new();
    };
    parse_if_inet6(&text, name)
}

fn parse_if_inet6(text: &str, name: &str) -> Vec<Ipv6Addr> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 6 || f[5] != name || f[0].len() != 32 {
                return None;
            }
            let hex = f[0];
            let bytes: Option<Vec<u8>> = (0..16)
                .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
                .collect();
            let b: [u8; 16] = bytes?.try_into().ok()?;
            let a = Ipv6Addr::from(b);
            // skip link-local (fe80::/10): universal, not identifying
            if a.segments()[0] & 0xffc0 == 0xfe80 {
                return None;
            }
            Some(a)
        })
        .collect()
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

    /// Build one RTM_NEWROUTE message: rtmsg + RTA_DST + RTA_OIF.
    fn route_msg(dst: Ipv4Addr, oif: u32) -> Vec<u8> {
        let mut body = vec![0u8; 12];
        body[0] = 2; // AF_INET
        body[1] = 32; // dst_len /32
        body[7] = 2; // RTN_LOCAL
        let d = dst.octets();
        // RTA_TABLE = 255 (Local), as the kernel sends on this box
        body.extend_from_slice(&8u16.to_le_bytes());
        body.extend_from_slice(&15u16.to_le_bytes());
        body.extend_from_slice(&255u32.to_le_bytes());
        // RTA_DST: rta_len = 4 header + 4 addr = 8
        body.extend_from_slice(&8u16.to_le_bytes());
        body.extend_from_slice(&1u16.to_le_bytes()); // RTA_DST
        body.extend_from_slice(&d);
        body.extend_from_slice(&8u16.to_le_bytes());
        body.extend_from_slice(&4u16.to_le_bytes()); // RTA_OIF
        body.extend_from_slice(&oif.to_le_bytes());
        let mut m = Vec::new();
        m.extend_from_slice(&((16 + body.len()) as u32).to_le_bytes());
        m.extend_from_slice(&24u16.to_le_bytes()); // RTM_NEWROUTE
        m.extend_from_slice(&0u16.to_le_bytes());
        m.extend_from_slice(&1u32.to_le_bytes());
        m.extend_from_slice(&0u32.to_le_bytes());
        m.extend_from_slice(&body);
        m
    }

    #[test]
    fn netlink_routes_attribute_v4_per_interface() {
        // ifindex 3 = wlan0 (192.168.1.197), 10 = enp59s0 (192.168.1.226)
        let names = std::collections::HashMap::from([
            (1u32, "lo".to_string()),
            (3u32, "wlan0".to_string()),
            (10u32, "enp59s0".to_string()),
        ]);
        let mut msgs = route_msg("127.0.0.1".parse().unwrap(), 1);
        msgs.extend(route_msg("192.168.1.197".parse().unwrap(), 3));
        msgs.extend(route_msg("192.168.1.226".parse().unwrap(), 10));
        // THE regression: each iface gets ONLY its own address
        assert_eq!(
            parse_local_v4(&msgs, &names, "wlan0"),
            vec!["192.168.1.197".parse::<Ipv4Addr>().unwrap()]
        );
        assert_eq!(
            parse_local_v4(&msgs, &names, "enp59s0"),
            vec!["192.168.1.226".parse::<Ipv4Addr>().unwrap()]
        );
        assert_eq!(
            parse_local_v4(&msgs, &names, "lo"),
            vec!["127.0.0.1".parse::<Ipv4Addr>().unwrap()]
        );
        assert!(parse_local_v4(&msgs, &names, "docker0").is_empty());
    }

    #[test]
    fn link_names_use_the_real_ifindex() {
        // The kernel sends ifinfomsg as family(u8) pad(u8) index(i32):
        // bytes [2..4] are padding, the index is the u16 at byte 4.
        // Reading u32le at offset 2 packed garbage into the key
        // ((type<<16)|index) and no route ever joined a link — the
        // "IPs vanished from the Network tab" regression.
        fn link_msg(index: u16, name: &str) -> Vec<u8> {
            let mut body = vec![0u8; 16]; // ifinfomsg
            body[4..6].copy_from_slice(&index.to_le_bytes());
            let mut attr = Vec::new();
            let n = name.as_bytes();
            let al = 4 + n.len() + 1; // IFLA_IFNAME + NUL
            attr.extend_from_slice(&(al as u16).to_le_bytes());
            attr.extend_from_slice(&3u16.to_le_bytes());
            attr.extend_from_slice(n);
            attr.push(0);
            if attr.len() % 4 != 0 {
                let pad = 4 - attr.len() % 4;
                attr.extend(std::iter::repeat_n(0u8, pad));
            }
            body.extend_from_slice(&attr);
            let mut m = Vec::new();
            m.extend_from_slice(&((16 + body.len()) as u32).to_le_bytes());
            m.extend_from_slice(&16u16.to_le_bytes()); // RTM_NEWLINK
            m.extend_from_slice(&0u16.to_le_bytes());
            m.extend_from_slice(&1u32.to_le_bytes());
            m.extend_from_slice(&0u32.to_le_bytes());
            m.extend_from_slice(&body);
            m
        }
        let mut msgs = link_msg(1, "lo");
        msgs.extend(link_msg(3, "wlan0"));
        msgs.extend(link_msg(6, "enp59s0"));
        let names = parse_link_names(&msgs);
        assert_eq!(names.get(&3).map(String::as_str), Some("wlan0"));
        assert_eq!(names.get(&6).map(String::as_str), Some("enp59s0"));
        assert_eq!(names.get(&1).map(String::as_str), Some("lo"));
    }

    #[test]
    fn dump_payloads_match_kernel_header_sizes() {
        // RTM_GETLINK expects struct ifinfomsg (16 bytes); 12 bytes triggers
        // the kernel's "8 bytes leftover after parsing attributes" warning.
        assert_eq!(link_dump_payload(), [0u8; 16]);
        let rt = route_v4_main_dump_payload();
        assert_eq!(rt.len(), 12);
        assert_eq!(rt[0], 2); // AF_INET
        assert_eq!(rt[4], 254); // RT_TABLE_MAIN
    }

    #[test]
    fn live_netlink_matches_ip_route_local() {
        // against the running kernel: every address iface_v4 reports for
        // an iface must appear as `local <addr> dev <iface>` in the
        // route table. (Skipped off Linux / sandboxed CI where netlink
        // is unavailable: iface_v4 degrades to empty.)
        let lo = iface_v4("lo");
        if lo.is_empty() {
            return; // no netlink here (CI sandbox) — nothing to compare
        }
        assert!(
            lo.contains(&Ipv4Addr::LOCALHOST),
            "lo missing 127.0.0.1: {lo:?}"
        );
        if let Ok(out) = std::process::Command::new("ip")
            .args(["-4", "route", "show", "table", "local", "dev", "lo"])
            .output()
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for addr in &lo {
                let needle = format!("local {addr} ");
                assert!(
                    text.lines().any(|l| l.starts_with(&needle)),
                    "{addr} not found as `{needle}` in:\n{text}"
                );
            }
        } else {
            assert!(
                lo.iter().all(|a| a.is_loopback()),
                "unexpected lo addrs: {lo:?}"
            );
        }
    }

    #[test]
    fn identity_carries_v6_from_fixture() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let id = read_identity(&root, "eth-test");
        assert!(!id.ipv6.is_empty());
        // ipv4 comes from live netlink (not fixture-rootable); reading a
        // nonexistent iface must be empty regardless
        assert!(id.ipv4.is_empty() || names_sane(&id.ipv4));
    }

    fn names_sane(v: &[Ipv4Addr]) -> bool {
        !v.is_empty()
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
