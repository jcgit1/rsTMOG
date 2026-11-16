//! Page widgets. v1: readable live readouts per page; graphs come in the
//! next phase. Everything renders from the immutable `Snapshot`.

use crate::sampler::Snapshot;
use eframe::egui;
use tmog_core::process_table::Row;

use crate::gui::graph::{GraphModel, Trace};

/// Build a single-trace model from a (tick, value) history slice.
/// `unit` labels the rate-mode y-axis (pass "" for percent graphs).
fn hist_model(
    name: &str,
    color: egui::Color32,
    hist: &[(u64, f64)],
    window: usize,
    percent: bool,
    unit: &str,
) -> GraphModel {
    let mut tr = Trace::new(name, color);
    tr.samples = hist.to_vec();
    let mut m = GraphModel::new(vec![tr], window, percent);
    m.unit = unit.to_string();
    if let Some(&(last_t, _)) = hist.last() {
        m.newest_tick = last_t;
    }
    let t0 = m.tick_window().0;
    let (mut mx, mut mn, mut cur) = (0.0f64, f64::MAX, None);
    for (t, v) in hist {
        if *t >= t0 && v.is_finite() {
            mx = mx.max(*v);
            mn = mn.min(*v);
            cur = Some(*v);
        }
    }
    if percent {
        m.scale.observe(mx);
    } else if cur.is_some() {
        // rate mode with a band: axis spans [min, max] padded 15% so a
        // 58-64 °C swing fills the frame instead of hugging the top.
        let pad = ((mx - mn) * 0.15).max(3.0);
        let lo = (mn - pad).max(0.0); // temperatures never go below 0
        m.band = Some((lo, (mx + pad).max(lo + 5.0)));
    }
    m
}

/// Build a dual-trace model from two parallel histories.
fn hist_model2(
    n1: &str,
    c1: egui::Color32,
    h1: &[(u64, f64)],
    n2: &str,
    c2: egui::Color32,
    h2: &[(u64, f64)],
    window: usize,
) -> GraphModel {
    let mut t1 = Trace::new(n1, c1);
    t1.samples = h1.to_vec();
    let mut t2 = Trace::new(n2, c2);
    t2.samples = h2.to_vec();
    let mut m = GraphModel::new(vec![t1, t2], window, false);
    if let Some(&(last_t, _)) = h1.last() {
        m.newest_tick = last_t;
    }
    let t0 = m.tick_window().0;
    let mx = h1
        .iter()
        .chain(h2.iter())
        .filter(|(t, _)| *t >= t0)
        .map(|(_, v)| *v)
        .fold(0.0f64, f64::max);
    m.scale.observe(mx);
    m
}

const WINDOW: usize = 90; // ~90s at 1 Hz

fn dash(o: Option<f64>, unit: &str) -> String {
    o.map(|v| format!("{v:.1}{unit}"))
        .unwrap_or_else(|| "-".into())
}

fn gib(kib: u64) -> String {
    format!("{:.1} GiB", kib as f64 / (1024.0 * 1024.0))
}

pub fn summary_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("Summary");
    egui::Grid::new("sum")
        .num_columns(2)
        .spacing([24.0, 6.0])
        .show(ui, |ui| {
            ui.label("CPU");
            ui.label(cpu_summary_line(s));
            ui.end_row();
            if let Some(g) = &s.gpu {
                ui.label("GPU");
                ui.label(gpu_summary_line(g));
                ui.end_row();
            }
            if let Some(line) = root_disk_readout(s) {
                ui.label("Disk");
                ui.label(line);
                ui.end_row();
            }
            if let Some(m) = s.mem_arc_adjusted.as_ref().or(s.mem.as_ref()) {
                ui.label("Memory");
                let arc = s
                    .arc
                    .map(|a| {
                        format!(
                            " (ARC {:.1} GiB)",
                            (a.size - a.c_min) as f64 / (1u64 << 30) as f64
                        )
                    })
                    .unwrap_or_default();
                ui.label(format!(
                    "{} / {} — {} used{arc}",
                    gib(m.used_kib()),
                    gib(m.mem_total),
                    dash(Some(m.used_percent()), "")
                ));
                ui.end_row();
            }
            if let Some(l) = s.load {
                ui.label("Load");
                ui.label(format!("{:.2}  {:.2}  {:.2}", l.one, l.five, l.fifteen));
                ui.end_row();
            }
        });
    // GPU histories ride the same scrolling-graph widget as the CPU;
    // only drawn when NVML is present (no GPU → no empty frame).
    let gpu_hist = s.hist_gpu.as_deref().unwrap_or(&[]);
    let gpu_temp = s.hist_gpu_temp.as_deref().unwrap_or(&[]);
    let gpu_temp_live = gpu_temp.iter().any(|(_, v)| v.is_finite());
    let show_graphs =
        !s.hist_cpu.is_empty() || !s.hist_temp.is_empty() || !gpu_hist.is_empty() || gpu_temp_live;
    if show_graphs {
        // Separator between the readout grid and the graph strip — the
        // pattern task managers use to split the stat section from the
        // history section (btop has a thin line, BSM Task Manager a
        // header band). Plus breathing room on both sides.
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        // One graph per line. A shared horizontal row painted the CPU
        // and GPU traces on top of each other.
        let celsius = |hist: &[(u64, f64)]| {
            hist.last()
                .filter(|(_, v)| v.is_finite())
                .map(|(_, v)| format!("  — {v:.0} °C"))
                .unwrap_or_default()
        };
        let mut cells: Vec<(String, GraphModel)> = Vec::new();
        if !s.hist_cpu.is_empty() {
            cells.push((
                "CPU %".into(),
                hist_model(
                    "cpu",
                    egui::Color32::from_rgb(80, 200, 120),
                    &s.hist_cpu,
                    60,
                    true,
                    "",
                ),
            ));
        }
        if !gpu_hist.is_empty() {
            cells.push((
                "GPU %".into(),
                hist_model(
                    "gpu",
                    egui::Color32::from_rgb(150, 120, 230),
                    gpu_hist,
                    60,
                    true,
                    "",
                ),
            ));
        }
        if !s.hist_temp.is_empty() {
            cells.push((
                format!("CPU temp °C{}", celsius(&s.hist_temp)),
                hist_model(
                    "temp",
                    egui::Color32::from_rgb(230, 140, 60),
                    &s.hist_temp,
                    60,
                    false,
                    " °C",
                ),
            ));
        }
        if gpu_temp_live {
            cells.push((
                format!("GPU temp °C{}", celsius(gpu_temp)),
                hist_model(
                    "gpu_temp",
                    egui::Color32::from_rgb(220, 90, 80),
                    gpu_temp,
                    60,
                    false,
                    " °C",
                ),
            ));
        }
        ui.push_id("graphs", |ui| {
            for (title, model) in cells {
                ui.label(title);
                ui.add_space(2.0);
                super::graph::graph_ui(ui, &model, 90.0);
                ui.add_space(8.0);
            }
        });
    }
}

fn cpu_summary_line(s: &Snapshot) -> String {
    let busy = format!("{} busy", dash(s.aggregate, "%"));
    match &s.cpu_temp {
        Some(t) => format!("{busy}, {:.0}°C ({})", t.value, t.label),
        None => busy,
    }
}

/// Root disk read/write rates and busy %. None when `/` is not a block device.
fn root_disk_readout(s: &Snapshot) -> Option<String> {
    // resolved by the sampler (no file I/O while painting)
    let name = s.root_disk.as_deref()?;
    Some(format_root_disk(name, s))
}

fn format_root_disk(name: &str, s: &Snapshot) -> String {
    let hist = s.hist_disk.iter().find(|e| e.0 == name);
    let read = hist.and_then(|(_, r, _)| r.last()).map(|(_, v)| *v);
    let write = hist.and_then(|(_, _, w)| w.last()).map(|(_, v)| *v);
    let busy = s
        .disk_util
        .iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, u)| *u);
    format!(
        "{name}  read {}  write {}  busy {}",
        rate_mb(read),
        rate_mb(write),
        match busy {
            Some(u) => format!("{u:.0}%"),
            None => "-".into(),
        }
    )
}

fn rate_mb(bytes_per_sec: Option<f64>) -> String {
    match bytes_per_sec {
        Some(b) if b.is_finite() => format!("{:.1} MB/s", b / 1_000_000.0),
        _ => "-".into(),
    }
}

fn gpu_summary_line(g: &tmog_core::gpu::GpuInfo) -> String {
    let mem = match (g.mem_used_mib, g.mem_total_mib) {
        (Some(u), Some(t)) => format!("{u}/{t} MiB"),
        _ => "-".into(),
    };
    let temp = g.temp_c.map(|c| format!(", {c:.0}°C")).unwrap_or_default();
    format!(
        "{} — {} busy, {}{}{}{}",
        g.name,
        dash(g.busy_percent, "%"),
        mem,
        temp,
        g.power_draw_w
            .map(|w| format!(", {w:.0} W"))
            .unwrap_or_default(),
        g.power_limit_w
            .map(|w| format!(" / {w:.0} W limit"))
            .unwrap_or_default(),
    )
}

pub fn cpu_ui(ui: &mut egui::Ui, s: &Snapshot, core_panel_mut: &mut Option<usize>) {
    let mut core_panel = *core_panel_mut;
    ui.heading(format!("CPU — {} busy", dash(s.aggregate, "%")));
    if !s.hist_cpu.is_empty() {
        let m = hist_model(
            "cpu",
            egui::Color32::from_rgb(80, 200, 120),
            &s.hist_cpu,
            WINDOW,
            true,
            "",
        );
        super::graph::graph_ui(ui, &m, 90.0);
        ui.add_space(8.0);
    }
    // topology + freq/governor header (collected by the sampler)
    if let Some(ci) = &s.cpuinfo {
        ui.label(
            egui::RichText::new(format!(
                "{} — {} threads, {} cores, {} socket(s)",
                ci.model, ci.logical, ci.cores, ci.sockets
            ))
            .weak(),
        );
        if let Some(f) = &s.cpufreq0 {
            let mhz = |k: Option<u64>| {
                k.map(|k| format!("{:.0} MHz", k as f64 / 1000.0))
                    .unwrap_or_else(|| "-".into())
            };
            ui.label(
                egui::RichText::new(format!(
                    "cpu0 {}  min {}  max {}  governor {}  driver {}",
                    mhz(f.cur_khz),
                    mhz(f.min_khz),
                    mhz(f.max_khz),
                    f.governor.as_deref().unwrap_or("-"),
                    f.driver.as_deref().unwrap_or("-"),
                ))
                .weak(),
            );
        }
    }
    ui.add_space(4.0);
    // gentle discoverability note for the clickable core grid: a single
    // subdued line right above it, hidden while a core panel is open
    // (no popup, no modal, no first-run state to persist)
    if core_panel.is_none() {
        ui.label(
            egui::RichText::new("tip: double-click any core bar to list tasks running on it")
                .weak()
                .italics()
                .size(11.0),
        );
    }
    // per-core grid: single-click = transient preview (highlight only),
    // double-click = commit the "tasks on this core" panel. Attribution
    // is approximate; see the panel's own disclaimer.
    let cols = (s.per_core.len() as f32).sqrt().ceil().max(1.0) as usize;
    let mut activated: Option<usize> = None; // double-click -> toggle panel
    let mut preview: Option<usize> = None; // single-click -> highlight
    let mut clear_sel = false;
    egui::Grid::new("cores")
        .num_columns(cols)
        .spacing([3.0, 3.0])
        .show(ui, |ui| {
            for (i, (name, pct)) in s.per_core.iter().enumerate() {
                let v = pct.unwrap_or(0.0) as f32 / 100.0;
                let bar = egui::ProgressBar::new(v)
                    .text(format!(
                        "{} {:.0}%",
                        name.replace("cpu", ""),
                        pct.map(|p| p.round()).unwrap_or(0.0)
                    ))
                    .desired_width(110.0);
                let resp = ui
                    .add(bar)
                    .on_hover_text("double-click: tasks running on this core");
                // make the bar respond to clicks (ProgressBar is hover-only)
                let resp = resp.interact(egui::Sense::click());
                if resp.double_clicked() {
                    activated = Some(i);
                } else if resp.clicked() {
                    preview = Some(i);
                }
                // highlight: the committed panel's core, else a preview click
                let hot = activated.or(preview).or(core_panel) == Some(i);
                if hot {
                    ui.painter().rect_stroke(
                        resp.rect,
                        2.0,
                        egui::Stroke::new(1.5_f32, ui.visuals().selection.stroke.color),
                        egui::epaint::StrokeKind::Outside,
                    );
                }
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
    if let Some(i) = activated {
        core_panel = if core_panel == Some(i) {
            None // toggle off
        } else {
            Some(i)
        };
    }
    // a single-click preview that isn't the open core shows a one-line
    // hint instead of jumping straight into the panel
    if let Some(i) = preview.filter(|p| *p != core_panel.unwrap_or(usize::MAX)) {
        let n = s.core_usage.get(&(i as u32)).map(|v| v.len()).unwrap_or(0);
        ui.label(
            egui::RichText::new(format!(
                "CPU {i}: {n} task(s) last seen here — double-click to open"
            ))
            .weak(),
        );
    }
    if core_panel.is_some() && ui.button("close core view").clicked() {
        clear_sel = true;
    }
    if clear_sel {
        core_panel = None;
    }
    if let Some(i) = core_panel {
        let core = i as u32;
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Tasks last seen on CPU {core}")).strong());
            if let Some(p) = s.per_core.get(i).and_then(|(_, p)| *p) {
                ui.label(format!("(core {:.0}% busy)", p));
            }
        });
        let pids = s.core_usage.get(&core).cloned().unwrap_or_default();
        if pids.is_empty() {
            ui.weak("nothing observed on this core since the last row pass");
        } else {
            let mut rows: Vec<&Row> = s
                .procs
                .iter()
                .filter(|r| pids.contains(&r.proc.pid))
                .collect();
            rows.sort_by(|a, b| {
                b.cpu
                    .unwrap_or(0.0)
                    .partial_cmp(&a.cpu.unwrap_or(0.0))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            egui::ScrollArea::vertical()
                .id_salt("core_panel")
                .max_height(160.0)
                .show(ui, |ui| {
                    for r in rows.iter().take(60) {
                        ui.horizontal(|ui| {
                            ui.label(format!("{:>7}", r.proc.pid));
                            ui.label(egui::RichText::new(r.proc.display_name()).monospace());
                            ui.label(
                                r.cpu
                                    .map(|c| format!("{c:.1}%"))
                                    .unwrap_or_else(|| "-".into()),
                            );
                            ui.label(user_or_uid(&r.proc));
                        });
                    }
                });
            ui.label(
                egui::RichText::new(
                    "approximate: Linux records where a task was LAST seen, \
                     not its time per core; busy tasks appear, migrating \
                     ones may shift",
                )
                .small()
                .weak(),
            );
        }
    }
    *core_panel_mut = core_panel;
}

fn user_or_uid(p: &tmog_core::process::Proc) -> String {
    p.user.clone().unwrap_or_else(|| match p.uid {
        Some(u) => format!("uid {u}"),
        None => String::new(),
    })
}

pub fn memory_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("Memory");
    let Some(m) = s.mem_arc_adjusted.as_ref().or(s.mem.as_ref()) else {
        ui.label("no data");
        return;
    };
    if !s.hist_mem.is_empty() {
        let gm = hist_model(
            "mem",
            egui::Color32::from_rgb(110, 150, 230),
            &s.hist_mem,
            WINDOW,
            true,
            "",
        );
        super::graph::graph_ui(ui, &gm, 110.0);
        ui.add_space(6.0);
    }
    let frac = (m.used_percent() / 100.0) as f32;
    ui.add(egui::ProgressBar::new(frac).text(format!(
        "{} / {} ({}%)",
        gib(m.used_kib()),
        gib(m.mem_total),
        m.used_percent().round()
    )));
    // stacked composition bar: in-use / zfs-arc / cached / buffers / free
    {
        let (cs_in, cs_arc, cs_cache, cs_buf, cs_free) =
            tmog_core::zfs::mem_composition(m, s.arc.as_ref());
        let segs = [cs_in, cs_arc, cs_cache, cs_buf, cs_free];
        let names = ["in use", "zfs arc", "cached", "buffers", "free"];
        let colors = [
            egui::Color32::from_rgb(200, 90, 90),
            egui::Color32::from_rgb(120, 200, 220),
            egui::Color32::from_rgb(110, 160, 230),
            egui::Color32::from_rgb(150, 130, 210),
            egui::Color32::from_rgb(90, 170, 110),
        ];
        let total = segs.iter().sum::<u64>().max(1) as f32;
        let (_r, resp) =
            ui.allocate_at_least(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
        let rect = resp.rect;
        let painter = ui.painter();
        let mut x = rect.left();
        for (i, (&val, (&name, &color))) in
            segs.iter().zip(names.iter().zip(colors.iter())).enumerate()
        {
            let w = val as f32 / total * rect.width();
            let r =
                egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, rect.height()));
            painter.rect_filled(r, 0.0, color);
            if w > 42.0 {
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    name,
                    egui::FontId::proportional(10.0),
                    egui::Color32::BLACK,
                );
            }
            let _ = i;
            x += w;
        }
        painter.rect_stroke(
            rect,
            0.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)),
            egui::StrokeKind::Inside,
        );
        ui.label(
            egui::RichText::new("composition: in use / zfs arc / cached / buffers / free")
                .small()
                .weak(),
        );
    }
    ui.add_space(4.0);
    egui::Grid::new("mem").num_columns(2).show(ui, |ui| {
        ui.label("available");
        ui.label(gib(m.available_kib()));
        ui.end_row();
        ui.label("cached");
        ui.label(gib(m.cached));
        ui.end_row();
        ui.label("buffers");
        ui.label(gib(m.buffers));
        ui.end_row();
        if let Some(a) = s.arc {
            let rec = (a.size - a.c_min) as f64 / (1u64 << 30) as f64;
            ui.label("zfs arc (reclaimable)");
            ui.label(format!(
                "{rec:.1} GiB of {}",
                a.size as f64 / (1u64 << 30) as f64
            ));
            ui.end_row();
        }
        if m.swap_total > 0 {
            ui.label("swap");
            ui.label(format!("{}% used", m.swap_used_percent().round()));
            ui.end_row();
        }
    });
    if let Some((some, full)) = &s.psi_memory {
        memory_stall_ui(ui, s, some, full.as_ref());
    }
}

/// Cumulative PSI `total` (microseconds) as a duration. Sub-second stalls
/// stay in milliseconds so a short reclaim spike is not rounded into the
/// same "0.00" the kernel's avg10 field prints.
fn fmt_stall(us: u64) -> String {
    if us < 1_000 {
        format!("{us} us")
    } else if us < 1_000_000 {
        format!("{:.1} ms", us as f64 / 1_000.0)
    } else if us < 60_000_000 {
        format!("{:.1}s", us as f64 / 1_000_000.0)
    } else {
        let secs = us / 1_000_000;
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

fn memory_stall_ui(
    ui: &mut egui::Ui,
    s: &Snapshot,
    some: &tmog_core::pressure::PsiAvg,
    full: Option<&tmog_core::pressure::PsiAvg>,
) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Memory stall").strong());
    ui.label(format!(
        "since boot   some {}   full {}",
        fmt_stall(some.total_us),
        full.map(|f| fmt_stall(f.total_us))
            .unwrap_or_else(|| "—".into())
    ));
    // Not percent-mode: a 0–100 axis flattens the 1–10% range where
    // memory stalls actually sit. Rate mode floors the axis near 1%.
    if !s.hist_psi_some.is_empty() {
        let mut model = hist_model2(
            "some",
            egui::Color32::from_rgb(230, 170, 60),
            &s.hist_psi_some,
            "full",
            egui::Color32::from_rgb(210, 80, 70),
            &s.hist_psi_full,
            WINDOW,
        );
        model.unit = "%".into();
        super::graph::graph_ui(ui, &model, 70.0);
        // The 0% tick is centered on the bottom edge and hangs below the
        // frame. Without this gap it paints through the next label.
        ui.add_space(14.0);
    }
    let sample = match s.psi_memory_rate {
        Some(r) => format!(
            "this sample   some {} ({:.2}%)   full {}",
            fmt_stall(r.some_us),
            r.some_percent,
            match (r.full_us, r.full_percent) {
                (Some(us), Some(pct)) => format!("{} ({pct:.2}%)", fmt_stall(us)),
                _ => "—".into(),
            }
        ),
        None => "this sample   some —   full —".into(),
    };
    ui.label(sample);
    let avg = |label: &str, pick: fn(&tmog_core::pressure::PsiAvg) -> f64| {
        format!(
            "{label} {:.2}% / {}",
            pick(some),
            full.map(|f| format!("{:.2}%", pick(f)))
                .unwrap_or_else(|| "—".into())
        )
    };
    ui.label(format!(
        "{}    {}    {}",
        avg("10s", |p| p.avg10),
        avg("1m", |p| p.avg60),
        avg("5m", |p| p.avg300),
    ));
    ui.label(
        egui::RichText::new(
            "time tasks spent stalled waiting on memory. some: at least one task. full: every runnable task. amber some, red full.",
        )
        .small()
        .weak(),
    );
}

pub fn disks_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("Disks");
    // Per-disk sections in plain vertical layout (NOT a Grid): a graph
    // inside a grid cell only gets the column's natural width, which is
    // why the graphs used to render short.
    for d in &s.disks {
        if !tmog_core::diskstats::is_physical_candidate(&d.name) {
            continue;
        }
        let id = s
            .disk_ids
            .get(&d.name)
            .cloned()
            .unwrap_or(tmog_core::diskstats::DiskIdentity {
                model: None,
                rotational: None,
            });
        let util = s
            .disk_util
            .iter()
            .find(|(n, _)| *n == d.name)
            .and_then(|(_, u)| *u);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&d.name).strong());
            ui.label(id.model.unwrap_or("-".into()));
            ui.label(format!(
                "read {} M  write {} M  {}  {}",
                d.read_bytes / 1_000_000,
                d.write_bytes / 1_000_000,
                match id.rotational {
                    Some(1) => "HDD",
                    Some(_) => "SSD",
                    None => "?",
                },
                match util {
                    Some(u) => format!("busy {u:.0}%"),
                    None => "busy -".into(),
                }
            ));
        });
        if let Some((_, rh, wh)) = s.hist_disk.iter().find(|e| e.0 == d.name) {
            let m = hist_model2(
                "read",
                egui::Color32::from_rgb(80, 200, 120),
                rh,
                "write",
                egui::Color32::from_rgb(230, 170, 60),
                wh,
                WINDOW,
            );
            super::graph::graph_ui(ui, &m, 70.0);
        }
        ui.add_space(10.0);
    }
}

pub fn network_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("Network");
    let tot_rx: u64 = s
        .nets
        .iter()
        .filter(|n| n.name != "lo")
        .map(|n| n.rx_bytes)
        .sum();
    let tot_tx: u64 = s
        .nets
        .iter()
        .filter(|n| n.name != "lo")
        .map(|n| n.tx_bytes)
        .sum();
    ui.label(
        egui::RichText::new(format!(
            "totals  rx {} MB  tx {} MB",
            tot_rx / 1_000_000,
            tot_tx / 1_000_000
        ))
        .weak(),
    );
    // per-interface sections in vertical layout (same fix as the Disks
    // page: graphs inside Grid cells don't stretch to the window width)
    for n in &s.nets {
        if n.name == "lo" {
            continue;
        }
        ui.horizontal(|ui| {
            // IP prefix: the address that actually belongs to this
            // interface — exact per-interface mapping from the kernel's
            // route/link tables (netlink) and if_inet6, no heuristics.
            // Virtual interfaces intentionally show no prefix.
            // identity + kind come from the sampler (netlink dumps used
            // to run here, per interface, on every frame)
            let Some((id, kind)) = s.net_ids.get(&n.name) else {
                ui.label(egui::RichText::new(&n.name).strong());
                ui.label(format!("rx {} MB", n.rx_bytes / 1_000_000));
                ui.label(format!("tx {} MB", n.tx_bytes / 1_000_000));
                return;
            };
            let kind = *kind;
            let my_ip = if kind == tmog_core::netdev::NetKind::Virtual {
                None
            } else if let Some(a) = id.ipv6.first() {
                Some(a.to_string())
            } else {
                id.ipv4.first().map(|a| a.to_string())
            };
            let title = match &my_ip {
                Some(a) => format!("{a}  {}", n.name),
                None => n.name.clone(),
            };
            ui.label(egui::RichText::new(title).strong());
            let speed = id.speed_mbps.map(|s| format!("{s}M")).unwrap_or_default();
            let mac = id.mac.clone().unwrap_or_default();
            ui.label(format!("{kind:?} {speed} {mac}"));
            ui.label(format!("rx {} MB", n.rx_bytes / 1_000_000));
            ui.label(format!("tx {} MB", n.tx_bytes / 1_000_000));
        });
        if let Some((_, rh, th)) = s.hist_net.iter().find(|e| e.0 == n.name) {
            let m = hist_model2(
                "rx",
                egui::Color32::from_rgb(80, 170, 230),
                rh,
                "tx",
                egui::Color32::from_rgb(230, 120, 120),
                th,
                WINDOW,
            );
            super::graph::graph_ui(ui, &m, 70.0);
        }
        ui.add_space(10.0);
    }
}

pub fn thermals_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("Thermals");
    if let Some(t) = &s.cpu_temp {
        ui.heading(format!("CPU: {:.0}°C  {} [{}]", t.value, t.label, t.chip));
    }
    if !s.hist_temp.is_empty() {
        let m = hist_model(
            "temp",
            egui::Color32::from_rgb(230, 140, 60),
            &s.hist_temp,
            WINDOW,
            false,
            " °C",
        );
        super::graph::graph_ui(ui, &m, 120.0);
        ui.add_space(6.0);
    }
    // the hwmon list grows taller than the window (k10temp + nvme +
    // acpitz groups); scroll the list instead of clipping sensors below
    // the fold — heading and trend graph stay pinned above it
    egui::ScrollArea::vertical()
        .id_salt("thermals")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut last_chip = "";
            egui::Grid::new("thermals")
                .num_columns(2)
                .spacing([18.0, 3.0])
                .show(ui, |ui| {
                    for r in &s.temps {
                        if r.chip != last_chip {
                            ui.separator();
                            ui.separator();
                            ui.end_row();
                            ui.label(egui::RichText::new(&r.chip).strong());
                            ui.label("");
                            ui.end_row();
                            last_chip = &r.chip;
                        }
                        match r.kind {
                            tmog_core::hwmon::SensorKind::Temp => {
                                // the headline sensor (what the footer/graph use)
                                // is bolded in the list so it stands out
                                let headline = s
                                    .cpu_temp
                                    .as_ref()
                                    .is_some_and(|h| h.chip == r.chip && h.label == r.label);
                                if headline {
                                    ui.label(egui::RichText::new(&r.label).strong());
                                    ui.label(
                                        egui::RichText::new(format!("{:.1} °C", r.value)).strong(),
                                    );
                                } else {
                                    ui.label(&r.label);
                                    ui.label(format!("{:.1} °C", r.value));
                                }
                            }
                            tmog_core::hwmon::SensorKind::Fan => {
                                ui.label(&r.label);
                                ui.label(format!("{:.0} RPM", r.value));
                            }
                            tmog_core::hwmon::SensorKind::Voltage => {
                                ui.label(&r.label);
                                ui.label(format!("{:.0} mV", r.value));
                            }
                            tmog_core::hwmon::SensorKind::Other => continue,
                        }
                        ui.end_row();
                    }
                });
        });
}

/// "booted Tue 2026-10-28 21:14" — civil date from unix seconds via
/// Howard Hinnant's days-from-civil algorithm (no chrono dependency).
/// `uptime_s` (when present) disambiguates the wall-vs-skew case: we
/// show the derived boot instant, not a live countdown.
fn format_boot_time(unix_secs: i64, _uptime_s: Option<f64>) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let wd = weekday_from_days(days);
    format!(
        "{} {y:04}-{m:02}-{d:02} {:02}:{:02}",
        DAYS[wd],
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// days → (year, month, day). Hinnant civil_from_days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// days → day-of-week index into DAYS (0 = Thursday, the unix epoch).
fn weekday_from_days(z: i64) -> usize {
    z.rem_euclid(7) as usize
}

const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];

pub fn system_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("System");
    if let Some(si) = &s.sysinfo {
        egui::Grid::new("sys")
            .num_columns(2)
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                for (k, v) in [
                    ("hostname", si.hostname.as_str()),
                    ("distro", si.distro.as_str()),
                    ("kernel", si.release.as_str()),
                    ("arch", si.machine.as_str()),
                ] {
                    ui.label(k);
                    ui.label(v);
                    ui.end_row();
                }
                if let Some(m) = s.mem.as_ref() {
                    ui.label("memory");
                    ui.label(gib(m.mem_total));
                    ui.end_row();
                }
                if let Some(bt) = si
                    .boot_time
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                {
                    ui.label("booted");
                    ui.label(format_boot_time(bt.as_secs() as i64, s.uptime_s));
                    ui.end_row();
                }
            });
    }
    // cpu model from cpuinfo live
    if let Some(ci) = &s.cpuinfo {
        ui.label(format!(
            "{} ({} logical, {} cores)",
            ci.model, ci.logical, ci.cores
        ));
    }
    // GPU identity sits directly under the CPU line. Live fields come from
    // the sampler's NVML snapshot (absent when there is no NVIDIA GPU).
    if let Some(g) = &s.gpu {
        ui.label(gpu_system_line(g));
    }
}

/// System-page GPU line: name, then the readings the sampler already has.
fn gpu_system_line(g: &tmog_core::gpu::GpuInfo) -> String {
    let mem = match (g.mem_used_mib, g.mem_total_mib) {
        (Some(u), Some(t)) => format!("{u}/{t} MiB"),
        (None, Some(t)) | (Some(t), None) => format!("{t} MiB"),
        (None, None) => "-".into(),
    };
    let mut extra = String::new();
    if let Some(t) = g.temp_c {
        extra.push_str(&format!(", {t:.0}°C"));
    }
    if let Some(mhz) = g.graphics_clock_mhz {
        extra.push_str(&format!(", {mhz} MHz"));
    }
    if let Some(w) = g.power_draw_w {
        extra.push_str(&format!(", {w:.0} W"));
    }
    format!(
        "{} — {} busy, {}{}",
        g.name,
        dash(g.busy_percent, "%"),
        mem,
        extra
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(59), (1970, 3, 1)); // 1970 not a leap year
        assert_eq!(civil_from_days(20_454), (2026, 1, 1));
        assert_eq!(civil_from_days(20_818), (2026, 12, 31));
        assert_eq!(civil_from_days(20_819), (2027, 1, 1));
        // leap-day boundaries
        assert_eq!(civil_from_days(20_147), (2025, 2, 28));
        assert_eq!(civil_from_days(20_148), (2025, 3, 1)); // 2025 not leap
        assert_eq!(civil_from_days(19_782), (2024, 2, 29)); // 2024 is
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        // 2026-10-31 (Saturday) == day 20757: wd idx = 20757 % 7 = 2 → DAYS[2]
        assert_eq!(weekday_from_days(20_757), 2);
        assert_eq!(DAYS[weekday_from_days(20_757)], "Sat");
    }

    #[test]
    fn weekday_matches_epoch_thursday() {
        assert_eq!(weekday_from_days(0), 0); // Thu
        assert_eq!(weekday_from_days(1), 1); // Fri
        assert_eq!(weekday_from_days(-1), 6); // Wed
        // 2025-10-31 was a Friday
        let s = format_boot_time(1_761_900_000, None);
        assert!(s.starts_with("Fri"), "{s}");
    }

    #[test]
    fn gpu_system_line_sits_name_then_readings() {
        let g = tmog_core::gpu::GpuInfo {
            index: 0,
            name: "NVIDIA GeForce RTX 5090 Laptop GPU".into(),
            busy_percent: Some(12.4),
            mem_used_mib: Some(512),
            mem_total_mib: Some(16384),
            graphics_clock_mhz: Some(2100),
            memory_clock_mhz: None,
            power_draw_w: Some(48.2),
            power_limit_w: None,
            temp_c: Some(62.0),
        };
        assert_eq!(
            gpu_system_line(&g),
            "NVIDIA GeForce RTX 5090 Laptop GPU — 12.4% busy, 512/16384 MiB, 62°C, 2100 MHz, 48 W"
        );
        let bare = tmog_core::gpu::GpuInfo {
            busy_percent: None,
            mem_used_mib: None,
            mem_total_mib: None,
            graphics_clock_mhz: None,
            power_draw_w: None,
            temp_c: None,
            ..g
        };
        assert_eq!(
            gpu_system_line(&bare),
            "NVIDIA GeForce RTX 5090 Laptop GPU — - busy, -"
        );
    }

    /// Summary graphs are one per line: CPU %, then GPU %, then the
    /// temperature graphs. A shared row painted the CPU and GPU traces
    /// on top of each other.
    #[test]
    fn summary_graphs_stack_cpu_above_gpu() {
        let series = |v: f64| (0u64..40).map(|t| (t, v)).collect::<Vec<_>>();
        let snap = Snapshot {
            hist_cpu: series(20.0),
            hist_gpu: Some(series(30.0)),
            hist_temp: series(55.0),
            hist_gpu_temp: Some(series(62.0)),
            load: Some(tmog_core::loadavg::LoadAvg {
                one: 1.0,
                five: 1.0,
                fifteen: 1.0,
                running: 1,
                total: 2,
            }),
            cpu_temp: Some(tmog_core::hwmon::Reading {
                chip: "k10temp".into(),
                label: "Tctl".into(),
                kind: tmog_core::hwmon::SensorKind::Temp,
                value: 60.0,
            }),
            gpu: Some(tmog_core::gpu::GpuInfo {
                index: 0,
                name: "NVIDIA GeForce RTX 5090 Laptop GPU".into(),
                busy_percent: Some(2.0),
                mem_used_mib: Some(248),
                mem_total_mib: Some(24463),
                graphics_clock_mhz: None,
                memory_clock_mhz: None,
                power_draw_w: Some(21.0),
                power_limit_w: None,
                temp_c: Some(44.0),
            }),
            ..Snapshot::default()
        };
        let ctx = egui::Context::default();
        let width = 800.0;
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 1100.0),
            )),
            ..Default::default()
        };
        let full_output = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                summary_ui(ui, &snap);
            });
        });
        let mut texts = Vec::new();
        let mut labels = Vec::new();
        fn collect_text(
            shape: &egui::epaint::Shape,
            texts: &mut Vec<String>,
            labels: &mut Vec<(f32, f32, String)>,
        ) {
            match shape {
                egui::epaint::Shape::Text(ts) => {
                    let text = ts.galley.text().to_owned();
                    labels.push((ts.pos.x, ts.pos.y, text.clone()));
                    texts.push(text);
                }
                egui::epaint::Shape::Vec(v) => {
                    for s in v {
                        collect_text(s, texts, labels);
                    }
                }
                _ => {}
            }
        }
        for clipped_shape in &full_output.shapes {
            collect_text(&clipped_shape.shape, &mut texts, &mut labels);
        }
        let clipped = ctx.tessellate(full_output.shapes, 1.0);
        let mut green = Vec::new();
        let mut purple = Vec::new();
        let mut orange = Vec::new();
        let mut red = Vec::new();
        for c in &clipped {
            let mesh = match &c.primitive {
                egui::epaint::Primitive::Mesh(m) => m,
                egui::epaint::Primitive::Callback(_) => continue,
            };
            for v in mesh.vertices.iter() {
                let (r, g, b) = (v.color.r(), v.color.g(), v.color.b());
                let p = (v.pos.x, v.pos.y);
                if r < 110 && g > 160 && b > 80 && b < 160 {
                    green.push(p);
                } else if r > 120 && r < 180 && g > 90 && g < 150 && b > 200 {
                    purple.push(p);
                } else if r > 200 && g > 110 && g < 170 && b < 90 {
                    orange.push(p);
                } else if r > 190 && g > 60 && g < 120 && b > 50 && b < 110 {
                    red.push(p);
                }
            }
        }
        let mean = |pts: &[(f32, f32)]| {
            let n = pts.len() as f32;
            let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), p| (x + p.0, y + p.1));
            (sx / n, sy / n)
        };
        assert!(!green.is_empty(), "CPU % trace missing");
        assert!(!purple.is_empty(), "GPU % trace missing");
        assert!(!orange.is_empty(), "CPU temp trace missing");
        assert!(!red.is_empty(), "GPU temp trace missing");
        let (gx, gy) = mean(&green);
        let (px, py) = mean(&purple);
        let (ox, oy) = mean(&orange);
        let (rx, ry) = mean(&red);
        assert!(
            (gx - px).abs() < 40.0,
            "CPU % and GPU % should share a column, got x {gx} vs {px}"
        );
        assert!(
            py > gy + 40.0,
            "GPU % should be the line under CPU %, y {py} vs {gy}"
        );
        assert!(
            (ox - rx).abs() < 40.0,
            "temperature graphs should share a column, got x {ox} vs {rx}"
        );
        assert!(
            oy > py + 40.0,
            "CPU temp should be below GPU %, y {oy} vs {py}"
        );
        assert!(
            ry > oy + 40.0,
            "GPU temp should be the line under CPU temp, y {ry} vs {oy}"
        );
        assert!(rx < width, "GPU temp clipped past the panel, x={rx}");
        let pos_of = |name: &str| -> (f32, f32) {
            labels
                .iter()
                .find(|(_, _, t)| t == name)
                .map(|(x, y, _)| (*x, *y))
                .unwrap_or_else(|| panic!("{name} label missing, got {labels:?}"))
        };
        let (cpu_x, cpu_y) = pos_of("CPU");
        let (gpu_x, gpu_y) = pos_of("GPU");
        let (_, load_y) = pos_of("Load");
        assert!(
            gpu_y > cpu_y + 8.0,
            "GPU readout should be the line under CPU, y {gpu_y} vs {cpu_y}"
        );
        assert!(
            load_y > gpu_y + 8.0,
            "Load should stay below the GPU line, y {load_y} vs {gpu_y}"
        );
        assert!(
            (gpu_x - cpu_x).abs() < 0.5,
            "GPU label should share the CPU column, x {gpu_x} vs {cpu_x}"
        );
        let val_x = |prefix: &str| {
            labels
                .iter()
                .find(|(_, _, t)| t.starts_with(prefix))
                .map(|(x, _, _)| *x)
                .unwrap_or_else(|| panic!("{prefix} value missing"))
        };
        let cpu_val = labels
            .iter()
            .find(|(_, _, t)| t.contains("60°C"))
            .map(|(x, _, _)| *x)
            .expect("CPU line should carry its temperature");
        let gpu_val = val_x("NVIDIA");
        assert!(
            (gpu_val - cpu_val).abs() < 0.5,
            "GPU value should share the value column, x {gpu_val} vs {cpu_val}"
        );
        let celsius: Vec<_> = texts.iter().filter(|t| t.contains("°C")).collect();
        assert!(
            celsius.iter().any(|t| t.contains("CPU temp")),
            "CPU temp title missing Celsius, texts={texts:?}"
        );
        assert!(
            celsius.iter().any(|t| t.contains("GPU temp")),
            "GPU temp title missing Celsius, texts={texts:?}"
        );
    }

    #[test]
    fn cpu_line_keeps_temperature_and_disk_line_is_root_io() {
        let snap = Snapshot {
            aggregate: Some(6.1),
            cpu_temp: Some(tmog_core::hwmon::Reading {
                chip: "k10temp".into(),
                label: "Tctl".into(),
                kind: tmog_core::hwmon::SensorKind::Temp,
                value: 78.0,
            }),
            hist_disk: vec![(
                "nvme1n1".into(),
                vec![(1, 1_500_000.0)],
                vec![(1, 200_000.0)],
            )],
            disk_util: vec![("nvme1n1".into(), Some(4.2))],
            ..Snapshot::default()
        };
        assert_eq!(cpu_summary_line(&snap), "6.1% busy, 78°C (Tctl)");
        assert_eq!(
            format_root_disk("nvme1n1", &snap),
            "nvme1n1  read 1.5 MB/s  write 0.2 MB/s  busy 4%"
        );
    }

    #[test]
    fn boot_time_renders_date_and_clock() {
        // 2026-01-01 00:00:00 UTC == 1_767_225_600
        let s = format_boot_time(1_767_225_600, Some(100.0));
        assert_eq!(s, "Thu 2026-01-01 00:00");
        // 2026-10-31 21:04 UTC == day 20757 → Sat
        let s = format_boot_time(20_757 * 86_400 + 21 * 3600 + 4 * 60, Some(0.0));
        assert_eq!(s, "Sat 2026-10-31 21:04");
    }

    #[test]
    fn memory_stall_shows_cumulative_and_sample_not_only_avg10() {
        use tmog_core::pressure::{PsiAvg, PsiRate};
        let snap = Snapshot {
            mem: Some(tmog_core::meminfo::Meminfo {
                mem_total: 1_000_000,
                mem_available: Some(400_000),
                ..Default::default()
            }),
            psi_memory: Some((
                PsiAvg {
                    avg10: 1.5,
                    avg60: 0.4,
                    avg300: 0.1,
                    total_us: 10_937_000,
                },
                Some(PsiAvg {
                    avg10: 1.25,
                    avg60: 0.3,
                    avg300: 0.05,
                    total_us: 10_897_000,
                }),
            )),
            psi_memory_rate: Some(PsiRate {
                some_percent: 12.5,
                full_percent: Some(4.0),
                some_us: 125_000,
                full_us: Some(40_000),
                elapsed_us: 1_000_000,
            }),
            hist_psi_some: (0..8).map(|t| (t, 12.5)).collect(),
            hist_psi_full: (0..8).map(|t| (t, 4.0)).collect(),
            ..Snapshot::default()
        };
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 800.0),
            )),
            ..Default::default()
        };
        let full_output = ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                memory_ui(ui, &snap);
            });
        });
        let mut texts = Vec::new();
        fn collect(shape: &egui::epaint::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(ts) => texts.push(ts.galley.text().to_owned()),
                egui::epaint::Shape::Vec(v) => {
                    for s in v {
                        collect(s, texts);
                    }
                }
                _ => {}
            }
        }
        for clipped in &full_output.shapes {
            collect(&clipped.shape, &mut texts);
        }
        let joined = texts.join("\n");
        assert!(joined.contains("since boot"), "{joined}");
        assert!(joined.contains("10.9s"), "{joined}");
        assert!(joined.contains("125.0 ms"), "{joined}");
        assert!(joined.contains("12.50%"), "{joined}");
        assert!(joined.contains("1.50%"), "{joined}");
        assert!(joined.contains("1.25%"), "{joined}");
        assert!(
            !joined.contains("avg10"),
            "kernel field name is not the readout\n{joined}"
        );
    }
}
