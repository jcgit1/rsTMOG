//! egui/eframe frontend. v1 shell: sidebar nav, page stack, tick loop,
//! status footer. The live data is in `Sampler`'s `Snapshot`.

pub mod graph;
pub mod pages;
pub mod proc_table;

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
    proc_state: proc_table::ProcTableState,
    /// text to place on the clipboard next update (Copy command line)
    clipboard_text: Option<String>,
    persisted: bool,
    applied_theme: Option<String>,
    /// system tray (None on sessions without tray support)
    tray: Option<crate::traythread::TrayHandle>,
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
            settings: settings.clone(),
            proc_state: proc_table::ProcTableState::from_settings(&settings),
            clipboard_text: None,
            persisted: false,
            applied_theme: None,
            tray: None,
            timer_started: false,
        }
    }

    /// Build the app with the system tray wired in (real GUI only).
    pub fn with_tray(
        _cc: &eframe::CreationContext<'_>,
        tray: Option<crate::traythread::TrayHandle>,
    ) -> Self {
        let mut app = App::new();
        app.tray = tray;
        if let Some(tray) = &app.tray {
            tray.set_refresh(app.settings.refresh_ms);
        }
        app
    }

    /// Drain tray menu events into app behavior.
    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        let Some(tray) = &self.tray else { return };
        while let Some(ev) = tray.try_event() {
            match ev {
                crate::icons::TrayEvent::OpenOrShow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                crate::icons::TrayEvent::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                crate::icons::TrayEvent::SetRefresh(ms) => {
                    self.sampler.set_refresh_ms(ms);
                    self.settings.refresh_ms = ms;
                    if let Err(e) = crate::core::settings::save(
                        &crate::core::settings::default_path(),
                        &self.settings,
                    ) {
                        eprintln!("settings save: {e}");
                    }
                    tray.set_refresh(ms);
                }
            }
        }
    }

    /// Start the sampler at a fast cadence for headless capture (fills
    /// history quickly). Not used by the real GUI.
    pub fn new_for_capture(refresh_ms: u32) -> Self {
        let sampler = SamplerHandle::start(refresh_ms);
        let snap = sampler.snapshot();
        let settings = Settings::default();
        App {
            sampler,
            snap,
            page: Page::Summary,
            proc_state: proc_table::ProcTableState::from_settings(&settings),
            settings,
            clipboard_text: None,
            persisted: false,
            applied_theme: None,
            tray: None,
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
        // tray menu events (Open / refresh-rate / Quit)
        self.handle_tray_events(ctx);
        // place any pending clipboard text (Copy command line)
        if let Some(text) = self.clipboard_text.take() {
            ctx.copy_text(text);
        }
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

        let snap = self.sampler.snapshot();
        self.snap = snap;
        let snap_arc = self.snap.clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_central(ui, &snap_arc, self.page);
        });

        // execute any action the processes context menu requested
        if let Some(action) = self.proc_state.pending_action.take() {
            self.run_proc_action(action);
        }

        // persist table prefs (sort/tree/follow) when the window closes
        if ctx.input(|i| i.viewport().close_requested()) && !self.persisted {
            self.proc_state.persist_into(&mut self.settings);
            if let Err(e) =
                crate::core::settings::save(&crate::core::settings::default_path(), &self.settings)
            {
                eprintln!("settings save: {e}");
            }
            self.persisted = true;
        }
    }
}

impl App {
    /// Execute a process action off the paint path. end_task is blocking
    /// (SIGTERM→wait→SIGKILL) — acceptable here because it's user-
    /// initiated and short-lived; heavy work would move to a thread.
    fn run_proc_action(&mut self, a: proc_table::ProcAction) {
        use crate::core::actions;
        use std::time::Duration;
        let res = match &a {
            proc_table::ProcAction::EndTask(pid) => {
                actions::end_task(*pid as i32, Duration::from_secs(3))
            }
            proc_table::ProcAction::ForceKill(pid) => actions::force_kill(*pid as i32),
            proc_table::ProcAction::Suspend(pid) => actions::suspend(*pid as i32),
            proc_table::ProcAction::Resume(pid) => actions::resume(*pid as i32),
            proc_table::ProcAction::SetNice(pid, nice) => actions::set_nice(*pid as i32, *nice),
            proc_table::ProcAction::CopyCmdline(cmd) => {
                self.clipboard_text = Some(cmd.clone());
                return;
            }
        };
        if !res.ok {
            // surface the failure; a toast in the footer would be nicer,
            // stderr is honest for now
            eprintln!("process action {a:?} failed: errno {}", res.errno);
        }
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
    let snap_arc = app.snap.clone();
    egui::CentralPanel::default().show(ctx, |ui| {
        app.render_central(ui, &snap_arc, page);
    });
}

impl App {
    /// Central page dispatch. The Processes page uses App-owned table
    /// state (sort/filter/tree/follow/selection + pending actions).
    pub fn render_central(
        &mut self,
        ui: &mut egui::Ui,
        snap: &crate::sampler::Snapshot,
        page: Page,
    ) {
        match page {
            Page::Summary => pages::summary_ui(ui, snap),
            Page::Cpu => pages::cpu_ui(ui, snap),
            Page::Memory => pages::memory_ui(ui, snap),
            Page::Disks => pages::disks_ui(ui, snap),
            Page::Network => pages::network_ui(ui, snap),
            Page::Thermals => pages::thermals_ui(ui, snap),
            Page::Processes => proc_table::processes_table_ui(ui, snap, &mut self.proc_state),
            Page::System => pages::system_ui(ui, snap),
        }
    }
}

pub fn run() -> anyhow::Result<()> {
    let app = App::new();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 700.0])
            // window titlebar + DE taskbar icon (theme-aware "T")
            .with_icon(crate::icons::window_icon(&app.settings.theme))
            // X11/Wayland desktop-file matching name (see packaging/)
            .with_app_id("rstmog")
            .with_title("rsTMOG — Task Manager OG"),
        ..Default::default()
    };
    eframe::run_native(
        "rsTMOG",
        opts,
        Box::new(|cc| {
            // System tray with Open / refresh-rate / Quit menu. Best-effort:
            // platforms without a tray (some Wayland setups) continue without.
            let tray = crate::traythread::spawn("dark", "rsTMOG", 1000);
            if tray.is_none() {
                eprintln!("tray: unavailable on this desktop session");
            }
            Ok(Box::new(App::with_tray(cc, tray)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
