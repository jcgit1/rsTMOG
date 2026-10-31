//! Page widgets. v1: readable live readouts per page; graphs come in the
//! next phase. Everything renders from the immutable `Snapshot`.

use crate::sampler::Snapshot;
use eframe::egui;

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
    ui.weak("graphs landing next phase");
}

pub fn cpu_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading(format!("CPU — {} busy", dash(s.aggregate, "%")));
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

pub fn processes_ui(ui: &mut egui::Ui, s: &Snapshot) {
    ui.heading("Processes");
    ui.weak("full sortable table landing next phase; v1 shows the top 30 by CPU");
    let mut rows: Vec<_> = s.procs.iter().collect();
    rows.sort_by(|a, b| {
        b.cpu
            .unwrap_or(0.0)
            .partial_cmp(&a.cpu.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    egui::Grid::new("procs")
        .num_columns(4)
        .spacing([16.0, 2.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("pid").strong());
            ui.label(egui::RichText::new("name").strong());
            ui.label(egui::RichText::new("cpu%").strong());
            ui.label(egui::RichText::new("rss").strong());
            ui.end_row();
            for r in rows.iter().take(30) {
                ui.label(r.proc.pid.to_string());
                ui.label(egui::RichText::new(r.proc.display_name()).monospace());
                ui.label(r.cpu.map(|c| format!("{c:.1}")).unwrap_or("-".into()));
                ui.label(format!("{} MiB", r.proc.rss_bytes / (1024 * 1024)));
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
