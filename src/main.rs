//! rsTMOG — Task Manager OG, rewritten in Rust.
//!
//! `core` is the metrics engine (procfs/sysfs/NVML); `gui` is the egui
//! frontend. Kept as modules of one crate for now; split into a workspace
//! (core lib + gui bin) once the core API settles.

pub mod core;

mod gui;

use std::path::Path;

/// Write a line to stdout; ignores BrokenPipe so piping into `head` exits
/// cleanly instead of panicking in the print macro.
fn line(text: &str) {
    use std::io::Write;
    let out = std::io::stdout();
    let mut w = out.lock();
    let _ = writeln!(w, "{text}");
}

/// Indented detail line under a self-test check.
fn detail(text: &str) {
    line(&format!("      {text}"));
}

/// Parse every cpu row of a `/proc/stat` text. Returns aggregate ("cpu")
/// first then per-core rows, in file order.
fn cpu_rows(text: &str) -> Vec<core::cpu::CpuJiffies> {
    text.lines().filter_map(core::cpu::parse_cpu_line).collect()
}

fn print_version() {
    line(&format!("rstmog {}", env!("CARGO_PKG_VERSION")));
}

/// Live sanity pass over every registered collector; exit 0/1 (mirrors the
/// original TMOG and evildojo-tmog flag). Hardware-dependent collectors
/// will warn instead of failing as they land.
fn self_test() -> i32 {
    let mut failures = 0usize;
    let mut check = |name: &str, result: Result<(), String>| match result {
        Ok(()) => line(&format!("ok    {name}")),
        Err(e) => {
            line(&format!("FAIL  {name}: {e}"));
            failures += 1;
        }
    };

    check(
        "cpu: /proc/stat parses",
        (|| {
            let text = core::fs::read_to_string(Path::new("/proc/stat"))
                .map_err(|e| format!("read /proc/stat: {e}"))?;
            let rows = cpu_rows(&text);
            if rows.is_empty() {
                return Err("no cpu lines parsed".into());
            }
            if rows[0].name != "cpu" {
                return Err(format!(
                    "first row is {}, expected aggregate 'cpu'",
                    rows[0].name
                ));
            }
            Ok(())
        })(),
    );

    check(
        "meminfo parses",
        (|| {
            let m = core::meminfo::collect(Path::new("/"))?;
            if m.mem_total == 0 {
                return Err("MemTotal parsed as 0".into());
            }
            let pct = m.used_percent();
            if !(0.0..=100.0).contains(&pct) {
                return Err(format!("used% out of range: {pct}"));
            }
            detail(&format!(
                "total {} GiB, used {:.1}% ({})",
                m.mem_total / 1024 / 1024,
                pct,
                if m.mem_available.is_some() {
                    "MemAvailable"
                } else {
                    "fallback avail"
                }
            ));
            Ok(())
        })(),
    );

    check(
        "loadavg+uptime parse",
        (|| {
            let l = core::loadavg::collect_loadavg(Path::new("/"))?;
            let up = core::loadavg::collect_uptime(Path::new("/"))?;
            if up <= 0.0 {
                return Err(format!("uptime {up} <= 0"));
            }
            detail(&format!(
                "load {:.2} {:.2} {:.2}, up {:.1} h",
                l.one,
                l.five,
                l.fifteen,
                up / 3600.0
            ));
            Ok(())
        })(),
    );

    if failures == 0 {
        line("self-test PASSED");
        0
    } else {
        line(&format!("self-test FAILED ({failures})"));
        1
    }
}

/// One-shot terminal snapshot: two CPU samples ~200 ms apart, then print
/// aggregate and per-core busy%.
fn snapshot() -> i32 {
    use std::thread::sleep;
    use std::time::Duration;

    let text1 = match core::fs::read_to_string(Path::new("/proc/stat")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read /proc/stat: {e}");
            return 1;
        }
    };
    sleep(Duration::from_millis(200));
    let text2 = match core::fs::read_to_string(Path::new("/proc/stat")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read /proc/stat: {e}");
            return 1;
        }
    };
    let prev = cpu_rows(&text1);
    let now = cpu_rows(&text2);
    line("rstmog snapshot (200ms window)");
    for (p, n) in prev.iter().zip(now.iter()) {
        match core::cpu::busy_percent(p, n) {
            Some(pct) => line(&format!("{:<6} {:5.1}%", n.name, pct)),
            None => line(&format!("{:<6}  (no data)", n.name)),
        }
    }
    0
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") => print_version(),
        Some("--self-test") => std::process::exit(self_test()),
        Some("--snapshot") => std::process::exit(snapshot()),
        Some("--") | None => gui::run()?,
        Some(other) => {
            eprintln!("unknown flag: {other}");
            eprintln!("usage: rstmog [--version | --self-test | --snapshot]");
            std::process::exit(2);
        }
    }
    Ok(())
}
