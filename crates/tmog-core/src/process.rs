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
    /// resident set size in bytes (stat field 24, refreshed from statm
    /// field 2 when enriched; both are pages * page size)
    pub rss_bytes: u64,
    /// page faults: minor (stat field 10), major (field 12)
    pub minflt: u64,
    pub majflt: u64,
    /// from status VmSwap, KiB
    pub vm_swap_kib: u64,
    pub uid: Option<u32>,
    /// CPU the task was last seen running on (stat field 39,
    /// "processor"). NOTE: Linux does not track per-cpu time — this is
    /// the core the task happened to be on when /proc was read.
    pub last_cpu: u32,
    /// username resolved from /etc/passwd (None if unknown)
    pub user: Option<String>,
    /// argv, NUL-separated in procfs. Empty both before the slow tier
    /// has read it and for kernel threads; `kthread` tells those apart.
    pub cmdline: Vec<String>,
    /// `PF_KTHREAD` in stat field 9. Set on the cheap pass, so a row
    /// whose cmdline has not been read yet is not drawn as a kernel thread.
    pub kthread: bool,
}

/// `PF_KTHREAD` in the kernel task flags (stat field 9). A userspace
/// task does not have this bit set.
const PF_KTHREAD: u64 = 0x0020_0000;

impl Proc {
    /// Kernel thread, from the `PF_KTHREAD` bit in stat. An empty cmdline
    /// is not enough: the cheap pass has not read cmdline yet, and that
    /// used to draw every userspace process as `[comm]`.
    pub fn is_kernel_thread(&self) -> bool {
        self.kthread
    }

    /// Display name: argv[0] basename once cmdline is loaded, bare `comm`
    /// until then, `[comm]` for a kernel thread. Control characters and
    /// bidirectional overrides are removed; a process chooses both strings
    /// itself, and either one can reverse or break the rest of the row.
    pub fn display_name(&self) -> String {
        if let Some(exe) = self.cmdline.first()
            && !exe.is_empty()
        {
            let base = exe.rsplit('/').next().unwrap_or(exe);
            return visible_name(base);
        }
        let comm = visible_name(&self.comm);
        if self.kthread {
            format!("[{comm}]")
        } else {
            comm
        }
    }
}

/// Characters that change how the rest of a cell is drawn, rather than
/// being part of the name. Right-to-left *letters* stay: a process may
/// legitimately be named in Arabic or Hebrew. The marks below are the
/// overrides that reverse the Latin text around them.
fn spoofs_table_text(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// Drop spoofing characters. A name that was only those becomes `?`, so
/// the cell is not blank.
fn visible_name(s: &str) -> String {
    let cleaned: String = s.chars().filter(|c| !spoofs_table_text(*c)).collect();
    if cleaned.is_empty() {
        "?".into()
    } else {
        cleaned
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
        last_cpu: num(39) as u32,
        uid: None,
        user: None,
        cmdline: Vec::new(),
        kthread: num(9) & PF_KTHREAD != 0,
    })
}

/// Parse just the `processor` field (39) from a /proc/[pid]/stat text —
/// the CPU the task was last seen on. Cheaper than a full parse when the
/// caller only needs core attribution. Returns None on malformed input.
pub fn parse_last_cpu(text: &str) -> Option<u32> {
    let close = text.rfind(')')?;
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // field 39 -> rest[36]
    rest.get(36).and_then(|v| v.parse::<u32>().ok())
}

/// Parse just the `rss` field (24) from a /proc/[pid]/stat text: resident
/// pages, the same count statm reports. None on malformed input.
pub fn parse_stat_rss_pages(text: &str) -> Option<u64> {
    let close = text.rfind(')')?;
    let rest: Vec<&str> = text[close + 1..].split_whitespace().collect();
    // field 24 -> rest[21]; the kernel prints it signed
    rest.get(21)
        .and_then(|v| v.parse::<i64>().ok())
        .map(|v| v.max(0) as u64)
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

/// Split a Windows command line into argv following the MSVC rules the
/// shell uses: a run of backslashes before `"` halves up (2n → n literal,
/// 2n+1 → n literal + literal quote), quoted regions keep spaces.
/// Pure function — unit-tested without a Windows target.
pub fn parse_cmdline_windows(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    let mut bs = 0usize;
    for c in text.chars() {
        match c {
            '\\' => {
                bs += 1;
            }
            '"' => {
                cur.push_str(&"\\".repeat(bs / 2));
                if bs % 2 == 1 {
                    // odd run: literal quote, quote state unchanged
                    cur.push('"');
                    has_token = true;
                } else {
                    // even run (incl. 0): toggles quoting; a bare "" still
                    // starts/ends a token so `"" ` survives as an empty arg
                    in_quotes = !in_quotes;
                    has_token = true;
                }
                bs = 0;
            }
            ' ' | '\t' if !in_quotes => {
                bs = 0;
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            _ => {
                cur.push_str(&"\\".repeat(bs));
                bs = 0;
                cur.push(c);
                has_token = true;
            }
        }
    }
    cur.push_str(&"\\".repeat(bs));
    if has_token || !cur.is_empty() {
        out.push(cur);
    }
    out
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
    let mut p = collect_stat_only(root, pid, page_bytes)?;
    enrich_one(root, pid, page_bytes, passwd, &mut p);
    Ok(p)
}

/// Cheap tier: `/proc/[pid]/stat` (pid, comm, state, ppid, cpu times,
/// nice, threads, starttime, rss) plus the uid that owns `/proc/[pid]`.
/// swap/cmdline and the real uid from status wait for `enrich_one`
/// (slow tier, on demand).
///
/// The directory owner is the effective uid, except for a process that
/// made itself non-dumpable (gpg-agent, ssh-agent), whose directory is
/// owned by root until `enrich_one` reads the real uid from status.
pub fn collect_stat_only(root: &Path, pid: u32, page_bytes: u64) -> Result<Proc, String> {
    use crate::fs;
    let base = format!("proc/{pid}");
    let text = fs::read_under(root, &format!("{base}/stat")).map_err(|e| e.to_string())?;
    let mut p = parse_stat(&text)?;
    if let Some(pages) = parse_stat_rss_pages(&text) {
        p.rss_bytes = pages.saturating_mul(page_bytes);
    }
    p.uid = dir_owner_uid(&root.join(&base));
    Ok(p)
}

#[cfg(unix)]
fn dir_owner_uid(dir: &Path) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(dir).ok().map(|m| m.uid())
}

#[cfg(not(unix))]
fn dir_owner_uid(_dir: &Path) -> Option<u32> {
    None
}

/// Medium tier, for the every-row walk: `stat` plus two values every row
/// needs so the table can sort by them, at no extra file reads:
/// - resident memory from stat field 24 (`rss`, pages), and
/// - the owner uid from the `/proc/<pid>` directory itself (one `stat()`).
///
/// Before this, rss/user existed only for rows the GUI had enriched, so
/// sorting by memory or user ignored every process never scrolled to.
/// The slow tier (`enrich_one`) still refines visible rows: `statm` rss
/// and the real uid from `status` (a non-dumpable process's directory is
/// owned by root).
pub fn collect_row(
    root: &Path,
    pid: u32,
    page_bytes: u64,
    passwd: &HashMap<u32, String>,
) -> Result<Proc, String> {
    use std::os::unix::fs::MetadataExt;
    let base = format!("proc/{pid}");
    let text = crate::fs::read_under(root, &format!("{base}/stat")).map_err(|e| e.to_string())?;
    let mut p = parse_stat(&text)?;
    if let Some(pages) = parse_rss_pages(&text) {
        p.rss_bytes = pages.saturating_mul(page_bytes);
    }
    if let Ok(md) = std::fs::metadata(root.join(&base)) {
        p.uid = Some(md.uid());
        p.user = passwd.get(&md.uid()).cloned();
    }
    Ok(p)
}

/// Resident pages, stat field 24. None on malformed input.
pub fn parse_rss_pages(text: &str) -> Option<u64> {
    let close = text.rfind(')')?;
    // field 3 is rest[0], so field 24 is rest[21]
    text[close + 1..]
        .split_whitespace()
        .nth(21)
        .and_then(|v| v.parse().ok())
}

/// Slow tier: fill in status/statm/cmdline/user for an already-parsed
/// `Proc`. Called only for rows the GUI actually shows (or the followed
/// pid), which is why it is split from `collect_stat_only`.
pub fn enrich_one(
    root: &Path,
    pid: u32,
    page_bytes: u64,
    passwd: &HashMap<u32, String>,
    p: &mut Proc,
) {
    use crate::fs;
    let base = format!("proc/{pid}");
    if let Ok(t) = fs::read_under(root, &format!("{base}/status")) {
        apply_status(p, &t);
    }
    if let Ok(t) = fs::read_under(root, &format!("{base}/statm")) {
        apply_statm(p, &t, page_bytes);
    }
    let cmdline_path = root.join(format!("{base}/cmdline"));
    if let Ok(b) = std::fs::read(&cmdline_path) {
        p.cmdline = parse_cmdline(&b);
    }
    if let Some(uid) = p.uid {
        p.user = passwd.get(&uid).cloned();
    }
}

/// List numeric process dirs under `/proc` (unsorted; caller sorts).
pub fn list_pids(root: &Path) -> Vec<u32> {
    crate::fs::list_dir_sorted(root, "proc")
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

    /// Build a stat line where `processor` (field 39) == `cpu`.
    fn stat_with_cpu(pid: u32, cpu: u32) -> String {
        let mut f: Vec<String> = (3..=39).map(|i| (i as u64 + 100).to_string()).collect();
        // f[0] is field 3, so field 39 lives at index 36
        f[36] = cpu.to_string();
        format!("{pid} (my app (test)) {}", f.join(" "))
    }

    #[test]
    fn last_cpu_field_parsed() {
        let line = stat_with_cpu(42, 5);
        let p = parse_stat(&line).unwrap();
        assert_eq!(p.last_cpu, 5);
        assert_eq!(parse_last_cpu(&line), Some(5));
        let line = stat_with_cpu(7, 41);
        assert_eq!(parse_last_cpu(&line), Some(41));
        // a line truncated before field 39: parse_last_cpu -> None, and
        // parse_stat defaults last_cpu to 0 without panicking
        let short = "9 (z) S 1 9 9 0 -1 0 0 0 0 0 0 0 0 0 0 0 1 0 1 0";
        assert_eq!(parse_last_cpu(short), None);
        assert_eq!(parse_stat(short).unwrap().last_cpu, 0);
    }

    #[test]
    fn stat_rss_field_parsed() {
        // stat_with_cpu fills field N with N + 100
        assert_eq!(parse_stat_rss_pages(&stat_with_cpu(42, 5)), Some(124));
        assert_eq!(parse_stat_rss_pages("9 (z) S 1 9"), None);
    }

    #[test]
    fn stat_only_has_memory_and_owner() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/procroot");
        let line = std::fs::read_to_string(root.join("proc/4321/stat")).unwrap();
        let p = collect_stat_only(&root, 4321, 4096).unwrap();
        let pages = parse_stat_rss_pages(&line).unwrap();
        assert_eq!(p.rss_bytes, pages * 4096);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let owner = std::fs::metadata(root.join("proc/4321")).unwrap().uid();
            assert_eq!(p.uid, Some(owner));
        }
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
        // cmdline not read yet, and the flags are not PF_KTHREAD:
        // bare comm, not a bracketed kernel-thread name
        p.cmdline.clear();
        p.comm = "firefox".into();
        assert!(!p.is_kernel_thread());
        assert_eq!(p.display_name(), "firefox");
        p.kthread = true;
        p.comm = "kworker/0:1".into();
        assert_eq!(p.display_name(), "[kworker/0:1]");
    }

    #[test]
    fn display_name_strips_controls_and_bidi_overrides() {
        let mut p = parse_stat("1 (a) S 0 1 1 0 -1 4194304 1 0 0 0 0 0 0 0 20 0 1 0 5 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0").unwrap();
        // U+202E reverses the characters that follow it in the cell.
        p.cmdline = vec!["/usr/bin/evil\u{202e}ls".into()];
        assert_eq!(p.display_name(), "evills");
        p.cmdline = vec!["/bin/ok\nname\t\u{001b}[2J".into()];
        assert_eq!(p.display_name(), "okname[2J");
        p.cmdline = vec!["/usr/bin/\u{202e}\u{200f}".into()];
        assert_eq!(p.display_name(), "?");
        p.cmdline.clear();
        p.kthread = true;
        p.comm = "kthread\u{202e}".into();
        assert_eq!(p.display_name(), "[kthread]");
        // Letters with an inherent RTL direction are part of the name.
        p.cmdline = vec!["/usr/bin/محرر".into()];
        assert_eq!(p.display_name(), "محرر");
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
        // kernel-thread fixture: PF_KTHREAD in stat, empty cmdline
        let k = collect_one(&root, 2, 4096, &passwd).unwrap();
        assert!(k.is_kernel_thread());
        assert_eq!(k.display_name(), "[kthreadd]");
        // cheap pass has no cmdline, but still knows which rows are kernel threads
        let cheap = collect_stat_only(&root, 4321, 4096).unwrap();
        assert!(cheap.cmdline.is_empty());
        assert!(!cheap.is_kernel_thread());
        assert_eq!(cheap.display_name(), "my app (test)");
        let cheap_k = collect_stat_only(&root, 2, 4096).unwrap();
        assert!(cheap_k.is_kernel_thread());
        assert_eq!(cheap_k.display_name(), "[kthreadd]");
        // missing pid dir → error, no panic
        assert!(collect_one(&root, 99999, 4096, &passwd).is_err());
    }

    #[test]
    fn rss_from_stat_field_24() {
        // fields 3..=24 after comm; rss (field 24) = 777 pages
        let mut f: Vec<String> = (3..=39).map(|i| i.to_string()).collect();
        f[21] = "777".into();
        let line = format!("42 (a (b) c) {}", f.join(" "));
        assert_eq!(parse_rss_pages(&line), Some(777));
        assert_eq!(parse_rss_pages("1 (x) S 0"), None);
    }

    #[test]
    fn collect_row_fills_rss_and_owner_for_every_pid() {
        use std::os::unix::fs::MetadataExt;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/procroot");
        let owner = std::fs::metadata(root.join("proc/4321")).unwrap().uid();
        let mut passwd = HashMap::new();
        passwd.insert(owner, "fixture-owner".to_string());
        let p = collect_row(&root, 4321, 4096, &passwd).unwrap();
        let stat = std::fs::read_to_string(root.join("proc/4321/stat")).unwrap();
        assert_eq!(p.rss_bytes, parse_rss_pages(&stat).unwrap() * 4096);
        assert_eq!(p.uid, Some(owner));
        assert_eq!(p.user.as_deref(), Some("fixture-owner"));
        assert!(collect_row(&root, 99999, 4096, &passwd).is_err());
    }

    #[test]
    fn list_pids_numeric_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/procroot");
        let mut pids = list_pids(&root);
        pids.sort_unstable();
        assert_eq!(pids, vec![2, 4321]);
    }

    // ---- Windows command-line splitting (pure; runs on any host) ----

    #[test]
    fn win_cmdline_plain_args() {
        assert_eq!(
            parse_cmdline_windows("a.exe one two"),
            ["a.exe", "one", "two"]
        );
    }

    #[test]
    fn win_cmdline_quoted_path_with_spaces() {
        assert_eq!(
            parse_cmdline_windows("\"C:\\Program Files\\app.exe\" --path \"C:\\my dir\" x"),
            ["C:\\Program Files\\app.exe", "--path", "C:\\my dir", "x"]
        );
    }

    #[test]
    fn win_cmdline_escaped_quotes_in_argument() {
        // odd backslash run before a quote → literal quote, quoting unchanged
        assert_eq!(parse_cmdline_windows("\"say \\\"hi\\\"\""), ["say \"hi\""]);
    }

    #[test]
    fn win_cmdline_trailing_backslashes_before_closing_quote() {
        assert_eq!(
            parse_cmdline_windows("\"C:\\dir\\\\\" arg"),
            ["C:\\dir\\", "arg"]
        );
    }

    #[test]
    fn win_cmdline_empty_quoted_argument_survives() {
        assert_eq!(parse_cmdline_windows("a \"\" b"), ["a", "", "b"]);
    }

    #[test]
    fn win_cmdline_tabs_split_like_spaces() {
        assert_eq!(parse_cmdline_windows("a\tb  c"), ["a", "b", "c"]);
    }
}
