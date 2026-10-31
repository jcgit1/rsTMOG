//! Process identity from `/proc/[pid]/*`: the stat-line parser (with the
//! `comm`-contains-spaces/parens trap handled correctly), plus status,
//! statm and cmdline supplements.

use std::collections::HashMap;
use std::path::Path;

/// One process, fields merged from stat/status/cmdline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    /// command name from stat's parenthesised comm (may contain spaces
    /// and parens, e.g. `(tmux: server)`)
    pub comm: String,
    /// single-char state: R S D Z T I X ...
    pub state: char,
    pub ppid: u32,
    pub utime: u64,
    pub stime: u64,
    pub starttime: u64,
    pub nice: i32,
    pub threads: u32,
    /// resident set size in bytes (statm field 2 * page size)
    pub rss_bytes: u64,
    /// page faults: minor (stat field 10), major (field 12)
    pub minflt: u64,
    pub majflt: u64,
    /// from status VmSwap, KiB
    pub vm_swap_kib: u64,
    pub uid: Option<u32>,
    /// username resolved from /etc/passwd (None if unknown)
    pub user: Option<String>,
    /// argv, NUL-separated in procfs; empty for kernel threads
    pub cmdline: Vec<String>,
}

impl Proc {
    /// Kernel threads have empty cmdline and live at pid <= 2's children
    /// like kthreadd; the practical tell we use (like the C++ reference):
    /// empty cmdline → render as `[comm]`.
    pub fn is_kernel_thread(&self) -> bool {
        self.cmdline.is_empty()
    }

    /// Display name: argv[0] basename for userspace procs, `[comm]` for
    /// kernel threads.
    pub fn display_name(&self) -> String {
        if let Some(exe) = self.cmdline.first()
            && !exe.is_empty()
        {
            return exe.rsplit('/').next().unwrap_or(exe).to_string();
        }
        format!("[{}]", self.comm)
    }
}

/// Parse a `/proc/[pid]/stat` line. The comm field is the text between the
/// FIRST '(' and the LAST ')': it can contain spaces and nested parens
/// (`(tmux: server)`, `((sd-pam))`). Everything after the closing paren is
/// whitespace-separated, matching man proc's field numbering with comm
/// consumed as field 2.
pub fn parse_stat(text: &str) -> Result<Proc, String> {
    let open = text.find('(').ok_or("stat: no comm open paren")?;
    let close = text.rfind(')').ok_or("stat: no comm close paren")?;
    if close < open {
        return Err("stat: comm parens inverted".into());
    }
    let pid: u32 = text[..open]
        .trim_end()
        .parse()
        .map_err(|_| "stat: bad pid")?;
    let comm = text[open + 1..close].to_string();
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // rest[0] is field 3 (state); field N maps to rest[N-3]
    if rest.len() < 20 {
        return Err(format!("stat: only {} fields after comm", rest.len()));
    }
    let field = |n: usize| rest.get(n - 3).copied().unwrap_or("0");
    let num = |n: usize| -> u64 { field(n).parse().unwrap_or(0) };
    Ok(Proc {
        pid,
        comm,
        state: field(3).chars().next().ok_or("stat: empty state")?,
        ppid: num(4) as u32,
        utime: num(14),
        stime: num(15),
        starttime: num(22),
        nice: field(19).parse::<i64>().unwrap_or(0) as i32,
        threads: num(20) as u32,
        rss_bytes: 0,
        minflt: num(10),
        majflt: num(12),
        vm_swap_kib: 0,
        uid: None,
        user: None,
        cmdline: Vec::new(),
    })
}

/// Parse the subset of `/proc/[pid]/status` we keep: Uid, VmSwap,
/// Threads (overrides stat's if present). Unknown keys ignored.
pub fn apply_status(proc: &mut Proc, text: &str) {
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let v = v.trim();
        match k {
            "Uid" => proc.uid = v.split_whitespace().next().and_then(|x| x.parse().ok()),
            "VmSwap" => {
                proc.vm_swap_kib = v
                    .split_whitespace()
                    .next()
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(0)
            }
            "Threads" => {
                if let Ok(t) = v.parse::<u32>() {
                    proc.threads = t;
                }
            }
            _ => {}
        }
    }
}

/// Parse `/proc/[pid]/statm`: field 2 is resident pages. `page_bytes` from
/// the caller (sysconf) so tests are host-independent.
pub fn apply_statm(proc: &mut Proc, text: &str, page_bytes: u64) {
    if let Some(rss_pages) = text
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
    {
        proc.rss_bytes = rss_pages.saturating_mul(page_bytes);
    }
}

/// Parse NUL-separated `/proc/[pid]/cmdline`. Empty file (kernel thread)
/// → empty vec; trailing NUL tolerated.
pub fn parse_cmdline(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Minimal `/etc/passwd` uid→name map (numeric-only UIDs; shell field
/// ignored). Used to label the process table without libc NSS calls.
pub fn parse_passwd(text: &str) -> HashMap<u32, String> {
    let mut m = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 3
            && let Ok(uid) = f[2].parse::<u32>()
        {
            m.entry(uid).or_insert_with(|| f[0].to_string());
        }
    }
    m
}

/// Read every `/proc/[pid]/` file set for one pid under `root`.
/// `page_bytes` injected for the same reason as above.
pub fn collect_one(
    root: &Path,
    pid: u32,
    page_bytes: u64,
    passwd: &HashMap<u32, String>,
) -> Result<Proc, String> {
    use crate::core::fs;
    let base = format!("proc/{pid}");
    let mut p =
        parse_stat(&fs::read_under(root, &format!("{base}/stat")).map_err(|e| e.to_string())?)?;
    if let Ok(t) = fs::read_under(root, &format!("{base}/status")) {
        apply_status(&mut p, &t);
    }
    if let Ok(t) = fs::read_under(root, &format!("{base}/statm")) {
        apply_statm(&mut p, &t, page_bytes);
    }
    let cmdline_path = root.join(format!("{base}/cmdline"));
    if let Ok(b) = std::fs::read(&cmdline_path) {
        p.cmdline = parse_cmdline(&b);
    }
    if let Some(uid) = p.uid {
        p.user = passwd.get(&uid).cloned();
    }
    Ok(p)
}

/// List numeric process dirs under `/proc` (unsorted; caller sorts).
pub fn list_pids(root: &Path) -> Vec<u32> {
    crate::core::fs::list_dir_sorted(root, "proc")
        .unwrap_or_default()
        .iter()
        .filter_map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse::<u32>().ok())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_comm_with_spaces_and_parens() {
        // the classic trap: comm contains spaces AND parens
        let line = "4242 (tmux: server) S 1 4242 4242 0 -1 4194304 900 0 0 0 11 9 0 0 20 0 1 0 98765 1234 567 18446744073709551615 1 1 0 0 0 0 0 0 0 0 1 0 0 0 4 8 0 0 0 10 0 0 0";
        let p = parse_stat(line).unwrap();
        assert_eq!(p.pid, 4242);
        assert_eq!(p.comm, "tmux: server");
        assert_eq!(p.state, 'S');
        assert_eq!(p.ppid, 1);
        assert_eq!(p.utime, 11);
        assert_eq!(p.stime, 9);
        assert_eq!(p.nice, 0);
        assert_eq!(p.threads, 1);
        assert_eq!(p.starttime, 98765);
    }

    #[test]
    fn stat_comm_wrapped_in_own_parens() {
        let line = "7 ((sd-pam)) S 1 7 7 0 -1 1077936 100 0 0 0 0 0 0 0 20 0 1 0 12 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let p = parse_stat(line).unwrap();
        assert_eq!(p.comm, "(sd-pam)");
        assert_eq!(p.ppid, 1);
    }

    #[test]
    fn stat_negative_nice() {
        // field 19 (nice) of a -8 process
        let mut base = String::from(
            "1 (x) S 0 1 1 0 -1 4194304 1 0 0 0 0 0 0 0 12 -8 1 0 3507988 8925184 617 18446744073709551615 1 1 0 0 0 0 0 0 0 0 1 0 0 0 4 8 0 0 0 10 0 0 0",
        );
        base.push('\n');
        let p = parse_stat(base.trim_end()).unwrap();
        assert_eq!(p.nice, -8);
    }

    #[test]
    fn stat_rejects_garbage() {
        assert!(parse_stat("no parens here").is_err());
        assert!(parse_stat("xx (y) S").is_err());
        assert!(parse_stat("1 (y) S 0 1").is_err()); // too few fields
    }

    #[test]
    fn status_supplements() {
        let mut p = parse_stat("1 (init) S 0 1 1 0 -1 4194304 1 0 0 0 5 6 0 0 20 0 3 0 100 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0").unwrap();
        apply_status(
            &mut p,
            "Name:   init\nState:  S (sleeping)\nUid:\t0\t0\t0\t0\nVmSwap:     128 kB\nThreads:\t7\n",
        );
        assert_eq!(p.uid, Some(0));
        assert_eq!(p.vm_swap_kib, 128);
        assert_eq!(p.threads, 7); // status overrides stat
    }

    #[test]
    fn statm_rss_uses_page_size() {
        let mut p = parse_stat("1 (a) S 0 1 1 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0").unwrap();
        apply_statm(&mut p, "256 62 45 1 0 0 0", 4096);
        assert_eq!(p.rss_bytes, 62 * 4096);
    }

    #[test]
    fn cmdline_nul_split() {
        assert_eq!(
            parse_cmdline(b"/usr/bin/ls\0-la\0/home\0"),
            vec!["/usr/bin/ls", "-la", "/home"]
        );
        // empty cmdline = kernel thread
        assert!(parse_cmdline(b"").is_empty());
        // trailing NUL only
        assert!(parse_cmdline(b"\0").is_empty());
    }

    #[test]
    fn display_name_rules() {
        let mut p = parse_stat("1 (a) S 0 1 1 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0").unwrap();
        p.cmdline = vec!["/usr/lib/systemd/systemd-journald".into()];
        assert_eq!(p.display_name(), "systemd-journald");
        p.cmdline.clear();
        p.comm = "kworker/0:1".into();
        assert_eq!(p.display_name(), "[kworker/0:1]");
    }

    #[test]
    fn passwd_map_first_wins() {
        let m = parse_passwd(
            "root:x:0:0:root:/root:/bin/bash\njcain:x:1000:1000::/home/jcain:/bin/bash\ndup:x:0:\nnot-enough\n",
        );
        assert_eq!(m.get(&0).map(String::as_str), Some("root"));
        assert_eq!(m.get(&1000).map(String::as_str), Some("jcain"));
        assert_eq!(m.get(&9999), None);
    }

    #[test]
    fn collect_one_end_to_end_with_fake_proc() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/procroot");
        let mut passwd = HashMap::new();
        passwd.insert(1000, "jcain".to_string());
        let p = collect_one(&root, 4321, 4096, &passwd).unwrap();
        assert_eq!(p.pid, 4321);
        assert_eq!(p.comm, "my app (test)");
        assert_eq!(p.ppid, 1);
        assert_eq!(p.uid, Some(1000));
        assert_eq!(p.user.as_deref(), Some("jcain"));
        assert_eq!(p.rss_bytes, 100 * 4096);
        assert_eq!(p.cmdline, vec!["/opt/myapp", "--flag"]);
        assert!(!p.is_kernel_thread());
        // kernel-thread fixture: empty cmdline
        let k = collect_one(&root, 2, 4096, &passwd).unwrap();
        assert!(k.is_kernel_thread());
        assert_eq!(k.display_name(), "[kthreadd]");
        // missing pid dir → error, no panic
        assert!(collect_one(&root, 99999, 4096, &passwd).is_err());
    }

    #[test]
    fn list_pids_numeric_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/procroot");
        let mut pids = list_pids(&root);
        pids.sort_unstable();
        assert_eq!(pids, vec![2, 4321]);
    }
}
