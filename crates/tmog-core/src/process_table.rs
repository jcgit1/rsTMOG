//! Process table: per-process CPU% between two snapshots, tree flattening,
//! and the shared sort/filter model used by the table frontends.

use crate::process::Proc;

/// CPU seconds consumed so far (user+sys jiffies).
fn cpu_ticks(p: &Proc) -> u64 {
    p.utime + p.stime
}

/// Per-process busy% between two snapshots of the same pid. `total_ticks`
/// is the aggregate delta of all CPUs between the samples (from
/// `cpu::CpuJiffies`), so a fully-busy single core on a 32-core box is
/// 100/32 ≈ 3.1% here — the "one core = 100%" convention would instead
/// divide by per-core ticks; we keep the whole-system convention (matches
/// TMOG/btop). None when the process didn't accumulate ticks or no time
/// passed (zero-delta guard).
pub fn cpu_percent(prev: &Proc, now: &Proc, total_ticks_delta: u64) -> Option<f64> {
    if total_ticks_delta == 0 {
        return None;
    }
    let d = cpu_ticks(now).saturating_sub(cpu_ticks(prev));
    Some((d as f64 * 100.0 / total_ticks_delta as f64).clamp(0.0, 100.0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    Pid,
    Name,
    #[default]
    Cpu,
    Mem,
    User,
    Threads,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDir {
    Asc,
    #[default]
    Desc,
}

/// A row the frontend renders. `cpu` and `mem_bytes` come from the
/// snapshot math; rows the sampler couldn't compute keep None.
#[derive(Debug, Clone)]
pub struct Row {
    pub proc: Proc,
    pub cpu: Option<f64>,
}

impl Row {
    pub fn mem_bytes(&self) -> u64 {
        self.proc.rss_bytes
    }
}

/// Substring filter, case-insensitive, over name/cmdline/user/pid.
pub fn matches(row: &Row, filter: &str) -> bool {
    if filter.is_empty() {
        return true;
    }
    let f = filter.to_lowercase();
    if row.proc.pid.to_string().contains(&f) {
        return true;
    }
    if row.proc.display_name().to_lowercase().contains(&f) {
        return true;
    }
    if row
        .proc
        .user
        .as_deref()
        .is_some_and(|u| u.to_lowercase().contains(&f))
    {
        return true;
    }
    row.proc
        .cmdline
        .iter()
        .any(|a| a.to_lowercase().contains(&f))
}

/// Sort rows in place. Deterministic: equal keys fall back to ascending
/// pid in BOTH directions, so refreshes don't jitter.
pub fn sort_rows(rows: &mut [Row], key: SortKey, dir: SortDir) {
    rows.sort_by(|a, b| {
        use std::cmp::Ordering;
        let ord = match key {
            SortKey::Pid => a.proc.pid.cmp(&b.proc.pid),
            SortKey::Name => a
                .proc
                .display_name()
                .to_lowercase()
                .cmp(&b.proc.display_name().to_lowercase()),
            SortKey::Cpu => a
                .cpu
                .unwrap_or(0.0)
                .partial_cmp(&b.cpu.unwrap_or(0.0))
                .unwrap_or(Ordering::Equal),
            SortKey::Mem => a.mem_bytes().cmp(&b.mem_bytes()),
            SortKey::User => a
                .proc
                .user
                .as_deref()
                .unwrap_or("")
                .cmp(b.proc.user.as_deref().unwrap_or("")),
            SortKey::Threads => a.proc.threads.cmp(&b.proc.threads),
        };
        let ord = match dir {
            SortDir::Asc => ord,
            SortDir::Desc => ord.reverse(),
        };
        // tiebreak ALWAYS ascending pid, even in Desc — stable display
        ord.then(a.proc.pid.cmp(&b.proc.pid))
    });
}

/// A flattened tree row: depth (0 = root) + index into the sorted row vec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub row: usize,
}

/// Flatten rows into a depth-first tree using each proc's ppid.
/// - children of a node appear in the CURRENT sort order;
/// - a proc whose parent is absent from the set (exited parent, or
///   filtered out) floats to root;
/// - ppid cycles surface their members flat at root (no infinite loop);
/// - self-referencing ppid (pid == ppid) is treated as an orphan.
pub fn tree_rows(rows: &[Row]) -> Vec<TreeRow> {
    // pid -> row index (first occurrence wins if a pid somehow repeats)
    let mut index: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        index.entry(r.proc.pid).or_insert(i);
    }
    // parent index -> children (row indices), preserving `rows` order
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); rows.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let parent = r.proc.ppid;
        match index.get(&parent) {
            Some(&pi) if pi != i => children[pi].push(i),
            // orphan / missing parent / self-parent → root
            _ => roots.push(i),
        }
    }
    // detect cycles: walk up from each node with a step budget; anything
    // that exceeds the budget participates in a cycle → promote to root.
    // Marked by removing it from its parent's children list.
    let n = rows.len();
    for i in 0..n {
        let mut cur = i;
        let mut steps = 0;
        loop {
            let parent_of_cur = match rows[cur].proc.ppid {
                p if index.get(&p).is_some_and(|&pi| pi != cur) => index[&p],
                _ => break, // reaches a root: no cycle for this chain
            };
            steps += 1;
            cur = parent_of_cur;
            if steps > n {
                // i is in a cycle: detach from its parent, make root
                if let Some(&pi) = index.get(&rows[i].proc.ppid)
                    && children[pi].contains(&i)
                {
                    children[pi].retain(|&c| c != i);
                    if !roots.contains(&i) {
                        roots.push(i);
                    }
                }
                break;
            }
        }
    }
    // dedup roots: a node pushed twice (orphaned twice) appears once
    {
        let mut seen: Vec<usize> = Vec::with_capacity(roots.len());
        for r in roots {
            if !seen.contains(&r) {
                seen.push(r);
            }
        }
        roots = seen;
    }
    // depth-first emit, children in rows order (== current sort order)
    let mut out = Vec::with_capacity(rows.len());
    let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|&r| (r, 0usize)).collect();
    let mut visited = vec![false; rows.len()];
    while let Some((idx, depth)) = stack.pop() {
        if visited[idx] {
            continue;
        }
        visited[idx] = true;
        out.push(TreeRow { depth, row: idx });
        for &c in children[idx].iter().rev() {
            if !visited[c] {
                stack.push((c, depth + 1));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::parse_stat;

    fn proc(pid: u32, ppid: u32, comm: &str, utime: u64, rss_pages: u64) -> Proc {
        let line = format!(
            "{pid} ({comm}) S {ppid} {pid} {pid} 0 -1 4194304 0 0 0 0 {utime} 0 0 0 20 0 1 0 5 0 {rss_pages} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0"
        );
        let mut p = parse_stat(&line).unwrap();
        p.rss_bytes = rss_pages * 4096;
        p
    }

    fn row(pid: u32, ppid: u32, comm: &str, utime: u64, cpu: Option<f64>) -> Row {
        Row {
            proc: proc(pid, ppid, comm, utime, 10),
            cpu,
        }
    }

    #[test]
    fn cpu_percent_math() {
        let a = proc(1, 0, "x", 100, 1);
        let b = proc(1, 0, "x", 150, 1);
        // 50 ticks used of 1000 total system ticks = 5%
        assert_eq!(cpu_percent(&a, &b, 1000), Some(5.0));
        // zero-total guard, and no-progress gives 0.0 (legit idle)
        assert_eq!(cpu_percent(&a, &b, 0), None);
        assert_eq!(cpu_percent(&a, &a, 1000), Some(0.0));
    }

    #[test]
    fn sort_deterministic_tiebreak() {
        let mut rows = vec![
            row(30, 1, "b", 0, Some(5.0)),
            row(10, 1, "A", 0, Some(5.0)),
            row(20, 1, "b", 0, Some(5.0)),
        ];
        sort_rows(&mut rows, SortKey::Name, SortDir::Asc);
        let pids: Vec<u32> = rows.iter().map(|r| r.proc.pid).collect();
        assert_eq!(pids, vec![10, 20, 30]); // A first, ties by pid
        sort_rows(&mut rows, SortKey::Cpu, SortDir::Desc);
        let pids: Vec<u32> = rows.iter().map(|r| r.proc.pid).collect();
        assert_eq!(pids, vec![10, 20, 30]); // equal cpu desc: still pid-asc tiebreak
    }

    #[test]
    fn filter_substring_cases() {
        let mut r1 = row(100, 1, "Firefox", 0, None);
        r1.proc.user = Some("jcain".into());
        r1.proc.cmdline = vec!["/usr/lib/firefox/firefox".into()];
        let rows = [r1];
        assert!(matches(&rows[0], "fire")); // name, case-insensitive
        assert!(matches(&rows[0], "JCAIN")); // user
        assert!(matches(&rows[0], "lib/firefox")); // cmdline substring
        assert!(matches(&rows[0], "100")); // pid
        assert!(!matches(&rows[0], "zzz"));
        assert!(matches(&rows[0], "")); // empty filter = all
    }

    #[test]
    fn tree_basic_and_orphans() {
        // 1 -> 2 -> 4, plus orphan 9 (parent 8 absent) and orphan 5 (self-parent)
        let rows = vec![
            row(1, 0, "systemd", 0, None),
            row(2, 1, "sshd", 0, None),
            row(3, 1, "cron", 0, None),
            row(4, 2, "bash", 0, None),
            row(9, 8, "orphan", 0, None),
            row(5, 5, "selfp", 0, None),
        ];
        let t = tree_rows(&rows);
        let flat: Vec<(usize, u32)> = t
            .iter()
            .map(|tr| (tr.depth, rows[tr.row].proc.pid))
            .collect();
        // roots: 1, 9, 5 (9 and 5 float up); DFS keeps children under parent
        assert_eq!(flat, vec![(0, 1), (1, 2), (2, 4), (1, 3), (0, 9), (0, 5)]);
        assert_eq!(t.len(), 6);
    }

    #[test]
    fn tree_filtered_parents_float_children() {
        // simulate a filter that removed pid 2: its child 4 must surface at root
        let rows = vec![row(1, 0, "systemd", 0, None), row(4, 2, "bash", 0, None)];
        let t = tree_rows(&rows);
        let flat: Vec<(usize, u32)> = t
            .iter()
            .map(|tr| (tr.depth, rows[tr.row].proc.pid))
            .collect();
        assert_eq!(flat, vec![(0, 1), (0, 4)]);
    }

    #[test]
    fn tree_ppid_cycle_surfaces_flat() {
        // a -> b -> a (impossible in reality, must not hang or drop rows)
        let rows = vec![row(7, 8, "a", 0, None), row(8, 7, "b", 0, None)];
        let t = tree_rows(&rows);
        assert_eq!(t.len(), 2);
        for tr in &t {
            assert_eq!(tr.depth, 0); // both at root
        }
    }
}
