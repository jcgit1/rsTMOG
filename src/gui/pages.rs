//! Page widgets. v1: readable live readouts per page; graphs come in the
//! next phase. Everything renders from the immutable `Snapshot`.

use crate::sampler::Snapshot;
use eframe::egui;
use tmog_core::process_table::Row;

use crate::gui::graph::{GraphModel, Trace};

/// Build a single-trace model from a (tick, value) history slice.
fn hist_model(
    name: &str,
    color: egui::Color32,
    hist: &[(u64, f64)],
    window: usize,
    percent: bool,
) -> GraphModel {
    let mut tr = Trace::new(name, color);
    tr.samples = hist.to_vec();
    let mut m = GraphModel::new(vec![tr], window, percent);
    if let Some(&(last_t, _)) = hist.last() {
        m.newest_tick = last_t;
    }
    let t0 = m.tick_window().0;
    let mx = hist
        .iter()
        .filter(|(t, _)| *t >= t0)
        .map(|(_, v)| *v)
        .filter(|v| v.is_finite())
        .fold(0.0f64, f64::max);
    m.scale.observe(mx);
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
            ui.label(format!("{} busy", dash(s.aggregate, "%")));
            ui.end_row();
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
            if let Some(t) = &s.cpu_temp {
                ui.label("Temp");
                // CPU package temp, plus the GPU temp on the same line:
                // separated with a divider so both are readable but
                // clearly distinct readings.
                let gpu_part = s
                    .gpu
                    .as_ref()
                    .and_then(|g| g.temp_c)
                    .map(|gt| format!("   |   GPU {:.0}°C", gt))
                    .unwrap_or_default();
                ui.label(format!("{:.0}°C  ({}){}", t.value, t.label, gpu_part));
                ui.end_row();
            }
            if let Some(g) = &s.gpu {
                ui.label("GPU");
                let mem = match (g.mem_used_mib, g.mem_total_mib) {
                    (Some(u), Some(t)) => format!("{u}/{t} MiB"),
                    _ => "-".into(),
                };
                ui.label(format!(
                    "{} — {} busy, {}{}{}",
                    g.name,
                    dash(g.busy_percent, "%"),
                    mem,
                    g.power_draw_w
                        .map(|w| format!(", {:.0} W", w))
                        .unwrap_or_default(),
                    g.power_limit_w
                        .map(|w| format!(" / {:.0} W limit", w))
                        .unwrap_or_default(),
                ));
                ui.end_row();
            }
        });
    // GPU % history rides the same scrolling-graph widget as the CPU;
    // only drawn when NVML is present (no GPU → no empty frame).
    let gpu_hist = s.hist_gpu.as_deref().unwrap_or(&[]);
    let show_graphs = !s.hist_cpu.is_empty() || !s.hist_temp.is_empty() || !gpu_hist.is_empty();
    if show_graphs {
        ui.horizontal(|ui| {
            // fixed column width: `available_width` inside a horizontal
            // row reports the whole remaining width, so a graph asked to
            // fill it consumed the entire row and clipped every later
            // column (this is why the GPU graph never showed). 350 px per
            // column fits three side by side in the default window and
            // scrolls horizontally past that.
            let col = 330.0_f32;
            ui.push_id("graphs", |ui| {
                ui.spacing_mut().item_spacing.x = 20.0;
                if !s.hist_cpu.is_empty() {
                    let m = hist_model(
                        "cpu",
                        egui::Color32::from_rgb(80, 200, 120),
                        &s.hist_cpu,
                        60,
                        true,
                    );
                    ui.vertical(|ui| {
                        ui.label("CPU %");
                        super::graph::graph_ui_w(ui, &m, 90.0, col);
                    });
                }
                if !gpu_hist.is_empty() {
                    let m = hist_model(
                        "gpu",
                        egui::Color32::from_rgb(150, 120, 230),
                        gpu_hist,
                        60,
                        true,
                    );
                    ui.vertical(|ui| {
                        ui.label("GPU %");
                        super::graph::graph_ui_w(ui, &m, 90.0, col);
                    });
                }
                if !s.hist_temp.is_empty() {
                    let m = hist_model(
                        "temp",
                        egui::Color32::from_rgb(230, 140, 60),
                        &s.hist_temp,
                        60,
                        false,
                    );
                    ui.vertical(|ui| {
                        ui.label("CPU temp °C");
                        super::graph::graph_ui_w(ui, &m, 90.0, col);
                    });
                }
            });
        });
    }
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
        );
        super::graph::graph_ui(ui, &m, 90.0);
        ui.add_space(8.0);
    }
    // topology + freq/governor header (live from sysfs; cheap files)
    let root = std::path::Path::new("/");
    if let Ok(ci) = tmog_core::cpuinfo::collect(root) {
        ui.label(
            egui::RichText::new(format!(
                "{} — {} threads, {} cores, {} socket(s)",
                ci.model, ci.logical, ci.cores, ci.sockets
            ))
            .weak(),
        );
        if let Some(c0) = tmog_core::cpuinfo::present_cpus(root).first().copied() {
            let f = tmog_core::cpuinfo::read_cpufreq(root, c0);
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
        ui.label(format!(
            "PSI some avg10={:.2}  full avg10={:.2}",
            some.avg10,
            full.map(|f| f.avg10).unwrap_or(0.0)
        ));
    }
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
        let id = tmog_core::diskstats::read_identity(std::path::Path::new("/"), &d.name);
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
        let id = tmog_core::netdev::read_identity(std::path::Path::new("/"), &n.name);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&n.name).strong());
            let speed = id.speed_mbps.map(|s| format!("{s}M")).unwrap_or_default();
            let mac = id.mac.clone().unwrap_or_default();
            ui.label(format!(
                "{:?} {speed} {mac}",
                id.kind(std::path::Path::new("/"))
            ));
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
        );
        super::graph::graph_ui(ui, &m, 120.0);
        ui.add_space(6.0);
    }
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
                            ui.label(egui::RichText::new(format!("{:.1} °C", r.value)).strong());
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
    if let Ok(ci) = tmog_core::cpuinfo::collect(std::path::Path::new("/")) {
        ui.label(format!(
            "{} ({} logical, {} cores)",
            ci.model, ci.logical, ci.cores
        ));
    }
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
    fn boot_time_renders_date_and_clock() {
        // 2026-01-01 00:00:00 UTC == 1_767_225_600
        let s = format_boot_time(1_767_225_600, Some(100.0));
        assert_eq!(s, "Thu 2026-01-01 00:00");
        // 2026-10-31 21:04 UTC == day 20757 → Sat
        let s = format_boot_time(20_757 * 86_400 + 21 * 3600 + 4 * 60, Some(0.0));
        assert_eq!(s, "Sat 2026-10-31 21:04");
    }
}
