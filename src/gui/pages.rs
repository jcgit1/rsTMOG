//! Page widgets. v1: readable live readouts per page; graphs come in the
//! next phase. Everything renders from the immutable `Snapshot`.

use crate::sampler::Snapshot;
use eframe::egui;

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
                ui.label(format!("{:.0}°C  ({})", t.value, t.label));
                ui.end_row();
            }
            if let Some(g) = &s.gpu {
                ui.label("GPU");
                let mem = match (g.mem_used_mib, g.mem_total_mib) {
                    (Some(u), Some(t)) => format!("{u}/{t} MiB"),
                    _ => "-".into(),
                };
                ui.label(format!(
                    "{} — {} busy, {}",
                    g.name,
                    dash(g.busy_percent, "%"),
                    mem
                ));
                ui.end_row();
            }
        });
    ui.add_space(10.0);
    if !s.hist_cpu.is_empty() || !s.hist_temp.is_empty() {
        ui.horizontal(|ui| {
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
                    super::graph::graph_ui(ui, &m, 90.0);
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
                    ui.label("\u{b0}C");
                    super::graph::graph_ui(ui, &m, 90.0);
                });
            }
        });
    }
}

pub fn cpu_ui(ui: &mut egui::Ui, s: &Snapshot) {
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
    let cols = (s.per_core.len() as f32).sqrt().ceil().max(1.0) as usize;
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
                ui.add(bar);
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
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
    egui::Grid::new("disks")
        .num_columns(3)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for d in &s.disks {
                if !tmog_core::diskstats::is_physical_candidate(&d.name) {
                    continue;
                }
                let id = tmog_core::diskstats::read_identity(std::path::Path::new("/"), &d.name);
                ui.label(d.name.clone());
                ui.label(id.model.unwrap_or("-".into()));
                ui.label(format!(
                    "read {} M  write {} M  busy {}",
                    d.read_bytes / 1_000_000,
                    d.write_bytes / 1_000_000,
                    match id.rotational {
                        Some(1) => "HDD".to_string(),
                        Some(_) => "SSD".to_string(),
                        None => "?".to_string(),
                    }
                ));
                ui.end_row();
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
                    super::graph::graph_ui(ui, &m, 64.0);
                    ui.end_row();
                }
            }
        });
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
    egui::Grid::new("net")
        .num_columns(4)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for n in &s.nets {
                if n.name == "lo" {
                    continue;
                }
                let id = tmog_core::netdev::read_identity(std::path::Path::new("/"), &n.name);
                ui.label(n.name.clone());
                let speed = id.speed_mbps.map(|s| format!("{s}M")).unwrap_or_default();
                let mac = id.mac.clone().unwrap_or_default();
                ui.label(format!(
                    "{:?} {speed} {mac}",
                    id.kind(std::path::Path::new("/"))
                ));
                ui.label(format!("rx {} MB", n.rx_bytes / 1_000_000));
                ui.label(format!("tx {} MB", n.tx_bytes / 1_000_000));
                ui.end_row();
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
                    super::graph::graph_ui(ui, &m, 64.0);
                    ui.end_row();
                }
            }
        });
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
                        ui.label(&r.label);
                        ui.label(format!("{:.1} °C", r.value));
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
