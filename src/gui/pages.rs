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
    ui.label(format!(
        "{} cores ({} logical)",
        s.sysinfo.as_ref().map(|_| s.per_core.len()).unwrap_or(0),
        s.per_core.len()
    ));
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
                if !crate::core::diskstats::is_physical_candidate(&d.name) {
                    continue;
                }
                let id = crate::core::diskstats::read_identity(std::path::Path::new("/"), &d.name);
                ui.label(d.name.clone());
                ui.label(id.model.unwrap_or("-".into()));
                ui.label(format!(
                    "read {} M  write {} M",
                    d.read_bytes / 1_000_000,
                    d.write_bytes / 1_000_000
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
    egui::Grid::new("net")
        .num_columns(4)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for n in &s.nets {
                if n.name == "lo" {
                    continue;
                }
                let id = crate::core::netdev::read_identity(std::path::Path::new("/"), &n.name);
                ui.label(n.name.clone());
                ui.label(format!("{:?}", id.kind(std::path::Path::new("/"))));
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
                    crate::core::hwmon::SensorKind::Temp => {
                        ui.label(&r.label);
                        ui.label(format!("{:.1} °C", r.value));
                    }
                    crate::core::hwmon::SensorKind::Fan => {
                        ui.label(&r.label);
                        ui.label(format!("{:.0} RPM", r.value));
                    }
                    crate::core::hwmon::SensorKind::Voltage => {
                        ui.label(&r.label);
                        ui.label(format!("{:.0} mV", r.value));
                    }
                    crate::core::hwmon::SensorKind::Other => continue,
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
    if let Ok(ci) = crate::core::cpuinfo::collect(std::path::Path::new("/")) {
        ui.label(format!(
            "{} ({} logical, {} cores)",
            ci.model, ci.logical, ci.cores
        ));
    }
}
