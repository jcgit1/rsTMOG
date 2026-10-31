//! egui/eframe frontend. v1 shell: sidebar nav, page stack, tick loop,
//! status footer. The live data is in `Sampler`'s `Snapshot`.

pub mod graph;
pub mod pages;

use crate::sampler::Sampler;
use eframe::egui;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Page {
    Summary,
    Cpu,
    Memory,
    Disks,
    Network,
    Thermals,
    Processes,
    System,
}

pub struct App {
    pub sampler: Sampler,
    page: Page,
    refresh_ms: u32,
    timer_started: bool,
}

impl App {
    pub fn new() -> Self {
        let mut sampler = Sampler::new();
        sampler.tick(); // seed tick
        // user-configurable cadence (Phase 4 settings dialog; TOML today)
        let refresh_ms = match crate::core::settings::load(&crate::core::settings::default_path()) {
            Ok(s) => s.refresh_ms,
            Err(e) => {
                eprintln!("settings: {e} (using defaults)");
                crate::core::settings::Settings::default().refresh_ms
            }
        };
        App {
            sampler,
            page: Page::Summary,
            refresh_ms,
            timer_started: false,
        }
    }

    /// Repaint driver: one thread that requests a repaint every
    /// refresh_ms until the window closes. Started on the first update
    /// (when a `Context` exists).
    fn schedule(&mut self, ctx: &egui::Context) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        let ms = self.refresh_ms;
        let rt = ctx.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(ms as u64));
                rt.request_repaint();
                if rt.input(|i| i.viewport().close_requested()) {
                    break;
                }
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.schedule(ctx);
        // sample once per update, driven by repaint timer
        self.sampler.tick();
        let snap = &self.sampler.snapshot;

        egui::SidePanel::left("nav")
            .default_width(130.0)
            .show(ctx, |ui| {
                ui.heading("rsTMOG");
                ui.separator();
                for (label, p) in [
                    ("Summary", Page::Summary),
                    ("CPU", Page::Cpu),
                    ("Memory", Page::Memory),
                    ("Disks", Page::Disks),
                    ("Network", Page::Network),
                    ("Thermals", Page::Thermals),
                    ("Processes", Page::Processes),
                    ("System", Page::System),
                ] {
                    if ui.selectable_label(self.page == p, label).clicked() {
                        self.page = p;
                    }
                }
            });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(up) = snap.uptime_s {
                    ui.label(format!("up {}", crate::core::sysinfo::format_uptime(up)));
                }
                ui.separator();
                if let Some(l) = snap.load {
                    ui.label(format!("load {:.2} {:.2} {:.2}", l.one, l.five, l.fifteen));
                }
                ui.separator();
                ui.label(format!("{} procs", snap.procs.len()));
                if let Some(g) = &snap.gpu {
                    ui.separator();
                    ui.label(format!("GPU {}%", g.busy_percent.unwrap_or(0.0).round()));
                }
                if let Some(t) = &snap.cpu_temp {
                    ui.separator();
                    ui.label(format!("{:.0}°C {}", t.value, t.label));
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let snap = &self.sampler.snapshot;
            render_central(ui, snap, self.page);
        });
    }
}

/// Render one page's central panel into `ctx` (used by the window and by
/// the headless capture). Ticks the sampler once first.
pub fn render_page(ctx: &egui::Context, app: &mut App, page: Page) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let snap = &app.sampler.snapshot;
        render_central(ui, snap, page);
    });
}

pub fn render_central(ui: &mut egui::Ui, snap: &crate::sampler::Snapshot, page: Page) {
    match page {
        Page::Summary => pages::summary_ui(ui, snap),
        Page::Cpu => pages::cpu_ui(ui, snap),
        Page::Memory => pages::memory_ui(ui, snap),
        Page::Disks => pages::disks_ui(ui, snap),
        Page::Network => pages::network_ui(ui, snap),
        Page::Thermals => pages::thermals_ui(ui, snap),
        Page::Processes => pages::processes_ui(ui, snap),
        Page::System => pages::system_ui(ui, snap),
    }
}

pub fn run() -> anyhow::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("rsTMOG", opts, Box::new(|_cc| Ok(Box::new(App::new()))))
        .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
