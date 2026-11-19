//! rsTMOG — Task Manager OG, rewritten in Rust (bin crate).
//!
//! The metrics engine lives in the `tmog-core` workspace crate (procfs/
//! sysfs/NVML, zero GUI deps); this crate is the egui frontend, sampler,
//! tray/icons and CLI entry points.

mod bg;
mod gui;
mod headless;
pub mod icons;
mod sampler;
mod single;
mod traythread;

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
fn cpu_rows(text: &str) -> Vec<tmog_core::cpu::CpuJiffies> {
    text.lines()
        .filter_map(tmog_core::cpu::parse_cpu_line)
        .collect()
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
            let text = tmog_core::fs::read_to_string(Path::new("/proc/stat"))
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
            let m = tmog_core::meminfo::collect(Path::new("/"))?;
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
            let l = tmog_core::loadavg::collect_loadavg(Path::new("/"))?;
            let up = tmog_core::loadavg::collect_uptime(Path::new("/"))?;
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

    check(
        "cpuinfo + cpufreq",
        (|| {
            let i = tmog_core::cpuinfo::collect(Path::new("/"))?;
            let cpus = tmog_core::cpuinfo::present_cpus(Path::new("/"));
            if cpus.is_empty() {
                return Err("no cpus found under /sys/devices/system/cpu".into());
            }
            let c0 = tmog_core::cpuinfo::read_cpufreq(Path::new("/"), cpus[0]);
            detail(&format!(
                "{} ({} logical, {} cores, {} socket(s)); cpu0 {} governor={:?} driver={:?}",
                i.model,
                i.logical,
                i.cores,
                i.sockets,
                c0.cur_khz
                    .map(|k| format!("{:.0} MHz", k as f64 / 1000.0))
                    .unwrap_or("-".into()),
                c0.governor.as_deref().unwrap_or("-"),
                c0.driver.as_deref().unwrap_or("-"),
            ));
            Ok(())
        })(),
    );

    // PSI: warn-only when the kernel does not expose it.
    for res in ["cpu", "memory", "io"] {
        match tmog_core::pressure::collect_pressure(Path::new("/"), res) {
            Ok(Some((s, _))) => {
                check(&format!("psi {res}"), Ok(()));
                detail(&format!("some avg10={:.2} avg60={:.2}", s.avg10, s.avg60));
            }
            Ok(None) => line(&format!("warn  psi {res}: not exposed by kernel")),
            Err(e) => check(&format!("psi {res}"), Err(e)),
        }
    }

    check(
        "vmstat parses",
        (|| {
            let t = tmog_core::fs::read_under(Path::new("/"), "proc/vmstat")
                .map_err(|e| format!("read vmstat: {e}"))?;
            let m = tmog_core::pressure::parse_vmstat(&t);
            if m.is_empty() {
                return Err("vmstat parsed empty".into());
            }
            detail(&format!(
                "{} counters (pgfault={:?})",
                m.len(),
                m.get("pgfault")
            ));
            Ok(())
        })(),
    );

    check(
        "process enumeration",
        (|| {
            let passwd = std::fs::read_to_string("/etc/passwd")
                .map(|s| tmog_core::process::parse_passwd(&s))
                .unwrap_or_default();
            let pids = tmog_core::process::list_pids(Path::new("/"));
            if pids.is_empty() {
                return Err("no pids under /proc".into());
            }
            // collect every readable process; unreadable ones (exited,
            // permission) are tolerated like the C++ reference does.
            let mut ok = 0usize;
            let mut me: Option<tmog_core::process::Proc> = None;
            for pid in &pids {
                if let Ok(p) = tmog_core::process::collect_one(Path::new("/"), *pid, 4096, &passwd)
                {
                    ok += 1;
                    if std::process::id() == *pid {
                        me = Some(p);
                    }
                }
            }
            if ok < 10 {
                return Err(format!("only {ok} processes collected"));
            }
            let m = me.ok_or("self not found in process list")?;
            detail(&format!(
                "{} processes; self = {:?} (uid {:?} user {:?}, rss {} KiB, nice {})",
                ok,
                m.display_name(),
                m.uid,
                m.user,
                m.rss_bytes / 1024,
                m.nice,
            ));
            Ok(())
        })(),
    );

    check(
        "diskstats parses",
        (|| {
            let d = tmog_core::diskstats::collect(Path::new("/"))?;
            let phys: Vec<_> = d
                .iter()
                .filter(|c| tmog_core::diskstats::is_physical_candidate(&c.name))
                .filter(|c| !tmog_core::diskstats::is_partition(Path::new("/"), &c.name))
                .collect();
            if phys.is_empty() {
                return Err("no physical disks found".into());
            }
            let id = tmog_core::diskstats::read_identity(Path::new("/"), &phys[0].name);
            detail(&format!(
                "{} devices, {} physical candidates (first: {} {:?} rot={:?})",
                d.len(),
                phys.len(),
                phys[0].name,
                id.model.as_deref().unwrap_or("-"),
                id.rotational
            ));
            Ok(())
        })(),
    );

    check(
        "netdev + identity",
        (|| {
            let rows = tmog_core::netdev::collect(Path::new("/"))?;
            if rows.is_empty() {
                return Err("no net/dev rows".into());
            }
            let mut desc = Vec::new();
            for r in rows.iter().take(4) {
                let id = tmog_core::netdev::read_identity(Path::new("/"), &r.name);
                let kind = id.kind(Path::new("/"));
                desc.push(format!(
                    "{}:{}{}",
                    r.name,
                    match kind {
                        tmog_core::netdev::NetKind::Loopback => "lo",
                        tmog_core::netdev::NetKind::Wifi => "wifi",
                        tmog_core::netdev::NetKind::Wired => "wired",
                        tmog_core::netdev::NetKind::Virtual => "virt",
                        tmog_core::netdev::NetKind::Other => "other",
                    },
                    id.speed_mbps
                        .map(|s| format!(" {s}Mbps"))
                        .unwrap_or_default()
                ));
            }
            detail(&format!("{} ifaces: {}", rows.len(), desc.join(", ")));
            Ok(())
        })(),
    );

    // Hardware-dependent collectors: warn, never fail (per PLAN).
    {
        match tmog_core::hwmon::collect(Path::new("/")) {
            readings if readings.is_empty() => line("warn  hwmon: no sensors readable"),
            readings => {
                let headline = tmog_core::hwmon::select_cpu_temp(&readings);
                check("hwmon + cpu headline", Ok(()));
                detail(&format!(
                    "{} sensors; headline = {}",
                    readings.len(),
                    headline
                        .map(|r| format!("{} {} {:.0}C", r.chip, r.label, r.value))
                        .unwrap_or_else(|| "none (no cpu-family sensor)".into())
                ));
            }
        }
        match tmog_core::rapl::read_package_energy(Path::new("/")) {
            Ok((e, m)) => detail(&format!("RAPL package energy_uj={e} max={m}")),
            Err(_) => line("warn  rapl: no package domain (unprivileged or unsupported)"),
        }
        match tmog_core::rapl::collect_battery(Path::new("/")) {
            Some(b) => detail(&format!(
                "battery {}% {:?}",
                b.percent.unwrap_or(0.0),
                b.status.as_deref().unwrap_or("?")
            )),
            None => line("warn  battery: none present"),
        }
        match tmog_core::zfs::collect(Path::new("/")) {
            Some(a) => detail(&format!(
                "zfs arc {:.1} GiB (reclaimable {:.1} GiB)",
                a.size as f64 / (1u64 << 30) as f64,
                tmog_core::zfs::reclaimable(&a) as f64 / (1u64 << 30) as f64
            )),
            None => line("warn  zfs: no arcstats (module not loaded on this box)"),
        }
        let si = tmog_core::sysinfo::collect(Path::new("/"));
        check("sysinfo", Ok(()));
        detail(&format!(
            "{} / {} {} / {}",
            si.hostname, si.distro, si.release, si.machine
        ));
    }

    match tmog_core::gpu::Nvml::new() {
        Some(n) => {
            let gpus = n.sample_all();
            check("gpu (NVML)", Ok(()));
            for g in gpus {
                let fmt = |o: Option<f64>, unit: &str| {
                    o.map(|v| format!("{v:.0}{unit}"))
                        .unwrap_or_else(|| "-".into())
                };
                detail(&format!(
                    "GPU{} {}: busy {} mem {}MiB clk {}MHz power {}/{}W temp {}",
                    g.index,
                    g.name,
                    fmt(g.busy_percent, "%"),
                    g.mem_used_mib.map(|m| m.to_string()).unwrap_or("-".into()),
                    fmt(g.graphics_clock_mhz.map(|c| c as f64), ""),
                    fmt(g.power_draw_w, ""),
                    fmt(g.power_limit_w, ""),
                    fmt(g.temp_c, "C"),
                ));
            }
        }
        None => line("warn  gpu: NVML unavailable (no driver or headless)"),
    }

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

    let text1 = match tmog_core::fs::read_to_string(Path::new("/proc/stat")) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read /proc/stat: {e}");
            return 1;
        }
    };
    sleep(Duration::from_millis(200));
    let text2 = match tmog_core::fs::read_to_string(Path::new("/proc/stat")) {
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
        match tmog_core::cpu::busy_percent(p, n) {
            Some(pct) => line(&format!("{:<6} {:5.1}%", n.name, pct)),
            None => line(&format!("{:<6}  (no data)", n.name)),
        }
    }
    0
}

/// Create a fresh, owner-only directory under the temp dir. `create_dir`
/// (not `create_dir_all`) fails if the path already exists, so a directory
/// or symlink planted by another user is never written into.
fn private_temp_dir(prefix: &str) -> std::io::Result<std::path::PathBuf> {
    let base = std::env::temp_dir();
    let mut last_err = None;
    for attempt in 0..16u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = base.join(format!("{prefix}-{}-{nanos}-{attempt}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no free temp dir name")))
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") => print_version(),
        Some("--self-test") => std::process::exit(self_test()),
        Some("--capture") => {
            let page = match args.next().as_deref() {
                None | Some("summary") => gui::Page::Summary,
                Some("cpu") => gui::Page::Cpu,
                Some("memory") => gui::Page::Memory,
                Some("disks") => gui::Page::Disks,
                Some("network") => gui::Page::Network,
                Some("thermals") => gui::Page::Thermals,
                Some("processes") => gui::Page::Processes,
                Some("system") => gui::Page::System,
                Some(other) => {
                    eprintln!("unknown page: {other}");
                    std::process::exit(2);
                }
            };
            let out = args
                .next()
                .map(std::path::PathBuf::from)
                // current directory, not a fixed /tmp name another user
                // could pre-create as a symlink
                .unwrap_or_else(|| std::path::PathBuf::from("rstmog-capture.png"));
            let mut app = gui::App::new_for_capture(50);
            // The sampler runs on its own thread; wait until enough
            // history samples have accumulated for the graphs to be
            // meaningful (~60 ticks), then capture the published snapshot.
            let want = 60u64;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                if app.sampler().snapshot().tick >= want {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "sampler stalled before capture"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            app.pull_snapshot();
            headless::capture(&mut app, page, &out)?;
            println!("captured {page:?} -> {}", out.display());
        }
        Some("--smoke") => {
            // Headless CI gate: build the app, accumulate some history,
            // then run EVERY page through the real egui pass + software
            // rasterizer to a throwaway image. Any panic or render error
            // exits non-zero.
            let mut app = gui::App::new_for_capture(50);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if app.sampler().snapshot().tick >= 10 {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    eprintln!("smoke: sampler stalled");
                    std::process::exit(1);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            app.pull_snapshot();
            let pages = [
                ("summary", gui::Page::Summary),
                ("cpu", gui::Page::Cpu),
                ("memory", gui::Page::Memory),
                ("disks", gui::Page::Disks),
                ("network", gui::Page::Network),
                ("thermals", gui::Page::Thermals),
                ("processes", gui::Page::Processes),
                ("system", gui::Page::System),
            ];
            let outdir = private_temp_dir("rstmog-smoke")?;
            for (name, page) in pages {
                let out = outdir.join(format!("{name}.png"));
                headless::capture(&mut app, page, &out)
                    .map_err(|e| anyhow::anyhow!("smoke page {name}: {e}"))?;
                println!("smoke {name}: ok");
            }
            let _ = std::fs::remove_dir_all(&outdir);
            println!("smoke PASSED ({} pages)", pages.len());
        }
        Some("--bench-tick") => {
            let n: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(20);
            let mut s = sampler::Sampler::new();
            s.tick();
            let mut worst = 0f64;
            let mut tot = 0f64;
            let mut row_tot = 0f64;
            let mut light_tot = 0f64;
            let mut row_n = 0u64;
            for _ in 0..n {
                let t0 = std::time::Instant::now();
                s.tick();
                let e = t0.elapsed().as_secs_f64() * 1000.0;
                // classify AFTER the tick: last_tick_was_row() reflects
                // the tick that just ran
                let is_row = s.last_tick_was_row();
                worst = worst.max(e);
                tot += e;
                if is_row {
                    row_tot += e;
                    row_n += 1;
                } else {
                    light_tot += e;
                }
            }
            println!(
                "ticks={n} avg={:.2}ms worst={:.2}ms | row-ticks n={row_n} avg={:.2}ms | light-ticks avg={:.2}ms | procs={}",
                tot / n as f64,
                worst,
                if row_n > 0 {
                    row_tot / row_n as f64
                } else {
                    0.0
                },
                if n - row_n > 0 {
                    light_tot / (n - row_n) as f64
                } else {
                    0.0
                },
                s.snapshot.procs.len()
            );
        }
        Some("--bench-phases") => {
            // per-collector cost breakdown on the live system
            let root = std::path::Path::new("/");
            macro_rules! time_it {
                ($name:expr, $body:expr) => {{
                    let mut best = f64::MAX;
                    for _ in 0..5 {
                        let t = std::time::Instant::now();
                        let _ = $body;
                        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    println!("{:<14} {:.2} ms", $name, best);
                }};
            }
            time_it!("list_pids", tmog_core::process::list_pids(root).len());
            time_it!("stat-only-walk", {
                let ps = tmog_core::process::list_pids(root);
                let mut n = 0;
                for pid in ps {
                    if tmog_core::process::collect_stat_only(root, pid, 4096).is_ok() {
                        n += 1;
                    }
                }
                n
            });
            time_it!("full-walk", {
                use std::collections::HashMap;
                let passwd: HashMap<u32, String> = tmog_core::process::parse_passwd(
                    &std::fs::read_to_string("/etc/passwd").unwrap_or_default(),
                );
                let ps = tmog_core::process::list_pids(root);
                let mut n = 0;
                for pid in ps {
                    if tmog_core::process::collect_one(root, pid, 4096, &passwd).is_ok() {
                        n += 1;
                    }
                }
                n
            });
            time_it!("hwmon", tmog_core::hwmon::collect(root).len());
            time_it!("gpu", {
                if let Some(n) = tmog_core::gpu::Nvml::new() {
                    n.sample(0);
                }
            });
            time_it!("psi x2", {
                let _ = tmog_core::pressure::collect_pressure(root, "memory");
                let _ = tmog_core::pressure::collect_pressure(root, "io");
            });
            time_it!("sysinfo", tmog_core::sysinfo::collect(root));
            time_it!("disks+identity", {
                let d = tmog_core::diskstats::collect(root).unwrap_or_default();
                for c in &d {
                    let _ = tmog_core::diskstats::read_identity(root, &c.name);
                    let _ = tmog_core::diskstats::is_partition(root, &c.name);
                }
                d.len()
            });
        }
        Some("--cores-probe") => {
            // verify core_usage attribution: 3 row passes, then print
            // per-core pid counts + a sample from the busiest core
            let mut s = sampler::Sampler::new();
            for _ in 0..6 {
                s.tick();
            }
            let snap = &s.snapshot;
            let mut tot = 0usize;
            for c in 0..snap.per_core.len() {
                tot += snap
                    .core_usage
                    .get(&(c as u32))
                    .map(|v| v.len())
                    .unwrap_or(0);
            }
            let busy = snap
                .core_usage
                .iter()
                .max_by_key(|(_, v)| v.len())
                .map(|(c, v)| (*c, v.len()))
                .unwrap_or((0, 0));
            println!(
                "cores={} attributed_pid_slots={tot} busiest_core={} ({} pids)",
                snap.per_core.len(),
                busy.0,
                busy.1
            );
            if let Some(v) = snap.core_usage.get(&busy.0) {
                let mut entries: Vec<(f64, bool, String)> = v
                    .iter()
                    .filter_map(|pid| {
                        snap.procs.iter().find(|r| r.proc.pid == *pid).map(|r| {
                            (
                                r.cpu.unwrap_or(0.0),
                                r.proc.is_kernel_thread(),
                                r.proc.display_name(),
                            )
                        })
                    })
                    // userspace before kthreads; sorted by cpu below
                    .filter(|(_, k, _)| !k)
                    .collect();
                entries.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                let mut names: Vec<String> =
                    entries.into_iter().take(8).map(|(_, _, n)| n).collect();
                if names.is_empty() {
                    names = v
                        .iter()
                        .take(8)
                        .filter_map(|pid| {
                            snap.procs
                                .iter()
                                .find(|r| r.proc.pid == *pid)
                                .map(|r| r.proc.display_name())
                        })
                        .collect();
                }
                println!("  sample: {}", names.join(", "));
            }
        }
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
