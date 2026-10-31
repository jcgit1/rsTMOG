//! egui/eframe frontend. v1 shell: sidebar nav, page stack, tick loop,
//! status footer. The live data is in `Sampler`'s `Snapshot`.

pub mod graph;
pub mod pages;

use crate::bg::SamplerHandle;
use crate::core::settings::Settings;
use eframe::egui;
use std::sync::Arc;

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
    sampler: SamplerHandle,
    /// latest snapshot for the current frame; the UI thread never blocks
    /// on a tick (the sampler thread publishes a fresh Arc each cycle)
    snap: Arc<crate::sampler::Snapshot>,
    page: Page,
    settings: Settings,
    applied_theme: Option<String>,
    timer_started: bool,
}

impl App {
    pub fn new() -> Self {
        // user-configurable cadence (Phase 4 settings dialog; TOML today)
        let settings = match crate::core::settings::load(&crate::core::settings::default_path()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("settings: {e} (using defaults)");
                Settings::default()
            }
        };
        let sampler = SamplerHandle::start(settings.refresh_ms);
        let snap = sampler.snapshot();
        App {
            sampler,
            snap,
            page: Page::Summary,
            settings,
            applied_theme: None,
            timer_started: false,
        }
    }

    /// Start the sampler at a fast cadence for headless capture (fills
    /// history quickly). Not used by the real GUI.
    pub fn new_for_capture(refresh_ms: u32) -> Self {
        let sampler = SamplerHandle::start(refresh_ms);
        let snap = sampler.snapshot();
        App {
            sampler,
            snap,
            page: Page::Summary,
            settings: Settings::default(),
            applied_theme: None,
            timer_started: false,
        }
    }

    /// The background sampler handle (capture waits on its tick counter).
    pub fn sampler(&self) -> &SamplerHandle {
        &self.sampler
    }

    /// Publish the latest snapshot into the frame slot before a capture.
    pub fn pull_snapshot(&mut self) {
        self.snap = self.sampler.snapshot();
    }

    /// Active theme name (for the headless renderer).
    pub fn theme(&self) -> &str {
        &self.settings.theme
    }

    /// Repaint driver: repaint at the sampler's cadence so the UI reflects
    /// each published snapshot. The repaint period tracks the poll rate via
    /// the shared refresh_ms handle (settings dialog can change it live).
    fn schedule(&mut self, ctx: &egui::Context) {
        if self.timer_started {
            return;
        }
        self.timer_started = true;
        let rt = ctx.clone();
        let refresh = self.sampler.refresh_handle();
        std::thread::spawn(move || {
            loop {
                let ms = refresh.load(std::sync::atomic::Ordering::Relaxed) as u64;
                // sleep in small steps so a cadence change takes effect quickly
                let step = std::time::Duration::from_millis(ms.min(100));
                let mut waited = 0u64;
                while waited < ms {
                    std::thread::sleep(step);
                    waited += step.as_millis() as u64;
                }
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
        // apply the persisted theme (TMOG's phosphor palettes are a
        // signature feature); only re-set when the name actually changes
        if self.applied_theme.as_deref() != Some(self.settings.theme.as_str()) {
            ctx.set_visuals(theme_visuals(&self.settings.theme));
            self.applied_theme = Some(self.settings.theme.clone());
        }
        self.schedule(ctx);
        // grab the latest snapshot published by the sampler thread
        self.snap = self.sampler.snapshot();
        let snap = &*self.snap;

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
            let snap = &*self.snap;
            render_central(ui, snap, self.page);
        });
    }
}

/// Map a theme name to egui Visuals. Dark is egui's default; the phosphor
/// palettes tint the window bg + selection to evoke a CRT (TMOG signature).
pub fn theme_visuals(name: &str) -> egui::Visuals {
    use egui::Color32;
    let mut v = match name {
        "light" => egui::Visuals::light(),
        "mono" => {
            let mut v = egui::Visuals::dark();
            let g = Color32::from_gray(200);
            v.widgets.inactive.fg_stroke.color = g;
            v.widgets.hovered.weak_bg_fill = g.linear_multiply(0.25);
            v.widgets.active.weak_bg_fill = g.linear_multiply(0.4);
            v.selection.stroke.color = g;
            v.extreme_bg_color = Color32::BLACK;
            v
        }
        "green" | "amber" | "blue" => {
            let mut v = egui::Visuals::dark();
            let (accent, panel) = match name {
                "green" => (Color32::from_rgb(60, 255, 120), Color32::from_rgb(6, 16, 8)),
                "amber" => (Color32::from_rgb(255, 176, 60), Color32::from_rgb(20, 13, 4)),
                _ /* blue */ => (Color32::from_rgb(90, 180, 255), Color32::from_rgb(4, 10, 20)),
            };
            v.panel_fill = panel;
            v.window_fill = panel;
            v.extreme_bg_color = Color32::from_rgb(2, 6, 3);
            v.widgets.inactive.fg_stroke.color = accent;
            v.widgets.noninteractive.fg_stroke.color = accent;
            v.widgets.hovered.weak_bg_fill = accent.linear_multiply(0.25);
            v.widgets.active.weak_bg_fill = accent.linear_multiply(0.4);
            v.selection.stroke.color = accent;
            v.hyperlink_color = accent;
            v
        }
        _ => egui::Visuals::dark(),
    };
    // keep panel/window fills consistent with dark_mode flag
    v.dark_mode = name != "light";
    v
}

/// Render one page's central panel into `ctx` (used by the window and by
/// the headless capture).
pub fn render_page(ctx: &egui::Context, app: &mut App, page: Page) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let snap = &*app.snap;
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
