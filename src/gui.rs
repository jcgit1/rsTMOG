//! egui/eframe frontend. v1 shell: sidebar nav, page stack, tick loop,
//! status footer. The live data is in `Sampler`'s `Snapshot`.

pub mod graph;
pub mod pages;
pub mod proc_table;

use crate::bg::SamplerHandle;
use eframe::egui;
use std::sync::Arc;
use tmog_core::settings::Settings;

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
    /// settings dialog visibility (gear button in the sidebar footer)
    show_settings: bool,
    /// selected logical core on the CPU page (None = no core panel)
    cpu_core_panel: Option<usize>,
    /// last settings-save failure; shown until a later save succeeds
    settings_error: Option<String>,
    timer_started: bool,
    /// async process actions: EndTask runs on a worker thread; results
    /// come back over this channel and become footer toasts
    action_tx: crossbeam_channel::Sender<(proc_table::ProcAction, bool, i32)>,
    action_rx: crossbeam_channel::Receiver<(proc_table::ProcAction, bool, i32)>,
    /// footer toast: (message, when it expires)
    toast: Option<(String, std::time::Instant)>,
}

impl App {
    pub fn new() -> Self {
        // user-configurable cadence (Phase 4 settings dialog; TOML today)
        let settings = match tmog_core::settings::load(&tmog_core::settings::default_path()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("settings: {e} (using defaults)");
                Settings::default()
            }
        };
        let sampler = SamplerHandle::start(settings.refresh_ms);
        let snap = sampler.snapshot();
        let (action_tx, action_rx) = crossbeam_channel::unbounded();
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
            show_settings: false,
            cpu_core_panel: None,
            settings_error: None,
            timer_started: false,
            action_tx,
            action_rx,
            toast: None,
        }
    }

    /// Wire the system tray into the app (real GUI only).
    pub fn with_tray(mut self, tray: Option<crate::traythread::TrayHandle>) -> Self {
        self.tray = tray;
        if let Some(tray) = &self.tray {
            tray.set_refresh(self.settings.refresh_ms);
        }
        self
    }

    /// Drain tray menu events into app behavior.
    fn handle_tray_events(&mut self, ctx: &egui::Context) {
        // second-instance ping: surface the window exactly like tray
        // "Open" (Visible+Focus is the app-internal raise path; Wayland
        // forbids raising from outside)
        if crate::single::surface_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
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
                    if let Err(e) = tmog_core::settings::save(
                        &tmog_core::settings::default_path(),
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
        let (action_tx, action_rx) = crossbeam_channel::unbounded();
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
            show_settings: false,
            cpu_core_panel: None,
            settings_error: None,
            timer_started: false,
            action_tx,
            action_rx,
            toast: None,
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
                // floor guards against a 0 ms busy loop
                let ms = (refresh.load(std::sync::atomic::Ordering::Relaxed) as u64).max(50);
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

/// Pids to enrich this frame. `selected` is first (refreshed every tick).
/// `on_screen` follows, in the order the table drew them. `widen_to`, when
/// non-empty, is the rest of the process list: a cmdline filter or a name
/// sort cannot see rows that have not been read yet, so those rotate
/// through the sampler's batch after the rows on screen.
fn detail_interest(selected: Option<u32>, on_screen: &[u32], widen_to: &[u32]) -> Vec<u32> {
    let mut pids = Vec::with_capacity(on_screen.len() + widen_to.len() + 1);
    if let Some(sel) = selected {
        pids.push(sel);
    }
    for &pid in on_screen {
        if Some(pid) != selected {
            pids.push(pid);
        }
    }
    if !widen_to.is_empty() {
        let have: std::collections::HashSet<u32> = pids.iter().copied().collect();
        for &pid in widen_to {
            if !have.contains(&pid) {
                pids.push(pid);
            }
        }
    }
    pids
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
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    ui.separator();
                    if ui.button("⚙  Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                });
            });

        // settings dialog (before central so clicks land on top)
        if self.show_settings {
            let mut persist_now = false;
            let mut open = self.show_settings;
            egui::Window::new("Settings")
                .open(&mut open)
                .id(egui::Id::new("rstmog-settings"))
                .resizable(false)
                .collapsible(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.separator();
                    ui.label(egui::RichText::new("Theme").strong());
                    let mut theme = self.settings.theme.clone();
                    egui::ComboBox::from_id_salt("theme_combo")
                        .selected_text(theme.clone())
                        .show_ui(ui, |ui| {
                            for t in ["dark", "green", "amber", "blue", "mono", "light"] {
                                ui.selectable_value(&mut theme, t.to_string(), t);
                            }
                        });
                    if theme != self.settings.theme {
                        self.settings.theme = theme;
                        // re-apply immediately (update() re-sets visuals when
                        // the applied theme differs)
                        persist_now = true;
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Poll rate").strong());
                    let mut rate = self.settings.refresh_ms;
                    for ms in [500u32, 1000, 2000] {
                        if ui.radio_value(&mut rate, ms, format!("{ms} ms")).clicked() {
                            self.settings.refresh_ms = rate;
                            self.sampler.set_refresh_ms(rate);
                            if let Some(tray) = &self.tray {
                                tray.set_refresh(rate);
                            }
                            persist_now = true;
                        }
                    }
                    ui.separator();
                    ui.label(egui::RichText::new("Processes").strong());
                    let mut tree = self.settings.proc_tree;
                    if ui
                        .checkbox(&mut tree, "Tree view by default")
                        .on_hover_text("Initial state of the Processes page tree toggle")
                        .changed()
                    {
                        self.settings.proc_tree = tree;
                        self.proc_state.tree = tree;
                        persist_now = true;
                    }
                    let mut follow = self.settings.proc_follow;
                    if ui
                        .checkbox(&mut follow, "Follow selection by default")
                        .changed()
                    {
                        self.settings.proc_follow = follow;
                        self.proc_state.follow = follow;
                        persist_now = true;
                    }
                    ui.separator();
                    if ui.button("Save now").clicked() {
                        persist_now = true;
                    }
                    if persist_now {
                        match tmog_core::settings::save(
                            &tmog_core::settings::default_path(),
                            &self.settings,
                        ) {
                            Ok(()) => self.settings_error = None,
                            Err(e) => self.settings_error = Some(e.to_string()),
                        }
                    }
                    if let Some(err) = &self.settings_error {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 120, 120),
                            format!("save failed: {err}"),
                        );
                    }
                });
            self.show_settings = open;
        }

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(up) = snap.uptime_s {
                    ui.label(format!("up {}", tmog_core::sysinfo::format_uptime(up)));
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
                // action-result toast (end task / kill / priority), fades
                // out after a few seconds
                let toast_alive = self
                    .toast
                    .as_ref()
                    .is_some_and(|(_, until)| until > &std::time::Instant::now());
                if !toast_alive {
                    self.toast = None;
                } else if let Some((msg, _)) = &self.toast {
                    ui.separator();
                    ui.colored_label(
                        if msg.contains("failed") {
                            egui::Color32::from_rgb(235, 90, 90)
                        } else {
                            egui::Color32::from_rgb(120, 220, 140)
                        },
                        msg,
                    );
                }
                // build version pinned to the right edge (what the friend
                // asks about when a bug report comes in)
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("v{}", env!("CARGO_PKG_VERSION")));
                });
            });
        });

        let snap = self.sampler.snapshot();
        self.snap = snap;
        let snap_arc = self.snap.clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_central(ui, &snap_arc, self.page);
        });

        // slow tier: enrich what the table actually drew (selection first).
        // A cmdline filter or a name sort also walks the rest, because
        // those rows cannot match until cmdline has been read.
        if self.page == Page::Processes {
            let widen: Vec<u32> = if !self.proc_state.filter.is_empty()
                || self.proc_state.sort == tmog_core::process_table::SortKey::Name
            {
                snap_arc.procs.iter().map(|r| r.proc.pid).collect()
            } else {
                Vec::new()
            };
            let pids =
                detail_interest(self.proc_state.selected, &self.proc_state.on_screen, &widen);
            self.sampler.request_detail(&pids);
        }
        // CPU page core panel: enrich the rows it shows (top 60 by CPU),
        // not the pid-sorted core list. The sampler rotates past 25.
        if self.page == Page::Cpu
            && let Some(core) = self.cpu_core_panel
            && let Some(pids) = snap_arc.core_usage.get(&(core as u32))
        {
            let cpu_of = |pid: u32| {
                snap_arc
                    .procs
                    .iter()
                    .find(|r| r.proc.pid == pid)
                    .and_then(|r| r.cpu)
                    .unwrap_or(0.0)
            };
            let mut ranked = pids.clone();
            ranked.sort_by(|a, b| {
                cpu_of(*b)
                    .partial_cmp(&cpu_of(*a))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            ranked.truncate(60);
            self.sampler.request_detail(&ranked);
        }

        // async end-task (and anything else) reporting back: drain into toasts
        self.drain_action_results();

        // execute any action the processes context menu requested
        if let Some(action) = self.proc_state.pending_action.take() {
            self.run_proc_action(action);
        }

        // persist table prefs (sort/tree/follow) when the window closes
        if ctx.input(|i| i.viewport().close_requested()) && !self.persisted {
            self.proc_state.persist_into(&mut self.settings);
            if let Err(e) =
                tmog_core::settings::save(&tmog_core::settings::default_path(), &self.settings)
            {
                eprintln!("settings save: {e}");
            }
            self.persisted = true;
        }
    }
}

impl App {
    /// Execute a process action without touching the paint path. end_task
    /// is blocking (SIGTERM→wait→SIGKILL), so it runs on a short-lived
    /// worker thread; every action reports back over a channel and the
    /// result lands as a footer toast on a later frame. A thread is
    /// cheaper than a tick-loop state machine here: actions are rare and
    /// user-initiated, and the thread must never outlive the action.
    fn run_proc_action(&mut self, a: proc_table::ProcAction) {
        use std::time::Duration;
        use tmog_core::actions;
        // fast, non-blocking actions can complete inline
        // fast, non-blocking actions complete inline. Every kernel-facing
        // call goes through the *_ref API, which refuses a recycled pid.
        let inline = match &a {
            proc_table::ProcAction::EndTask(_) => None,
            proc_table::ProcAction::ForceKill(t) => Some(actions::force_kill_ref(*t)),
            proc_table::ProcAction::Suspend(t) => Some(actions::suspend_ref(*t)),
            proc_table::ProcAction::Resume(t) => Some(actions::resume_ref(*t)),
            proc_table::ProcAction::SetNice(t, nice) => Some(actions::set_nice_ref(*t, *nice)),
            proc_table::ProcAction::CopyCmdline(cmd) => {
                self.clipboard_text = Some(cmd.clone());
                return;
            }
        };
        if let Some(res) = inline {
            self.report_action(&a, res.ok, res.errno);
            return;
        }
        // EndTask: SIGTERM→grace→SIGKILL off the UI thread.
        if let proc_table::ProcAction::EndTask(t) = a {
            let tx = self.action_tx.clone();
            let _ = std::thread::Builder::new()
                .name("rstmog-endtask".into())
                .spawn(move || {
                    let res = actions::end_task_ref(t, Duration::from_secs(3));
                    // a closed receiver just means the app quit mid-kill
                    let _ = tx.send((proc_table::ProcAction::EndTask(t), res.ok, res.errno));
                });
            self.toast = Some((format!("ending task {}…", t.pid), Self::toast_deadline()));
        }
    }

    /// Record a toast + wake factor for surfacing action results.
    fn toast_deadline() -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_secs(4)
    }

    /// Drain async action results into the toast slot.
    fn drain_action_results(&mut self) {
        while let Ok((a, ok, errno)) = self.action_rx.try_recv() {
            self.report_action(&a, ok, errno);
        }
    }

    /// Turn an action outcome into a footer toast (success or errno).
    fn report_action(&mut self, a: &proc_table::ProcAction, ok: bool, errno: i32) {
        let what = match a {
            proc_table::ProcAction::EndTask(t) => format!("end task {}", t.pid),
            proc_table::ProcAction::ForceKill(t) => format!("force kill {}", t.pid),
            proc_table::ProcAction::Suspend(t) => format!("suspend {}", t.pid),
            proc_table::ProcAction::Resume(t) => format!("resume {}", t.pid),
            proc_table::ProcAction::SetNice(t, n) => format!("set priority {}→{n}", t.pid),
            proc_table::ProcAction::CopyCmdline(_) => return,
        };
        let msg = if ok {
            format!("{what} ok")
        } else if errno == 3 {
            // ESRCH: gone, or the pid now belongs to a different process
            format!("{what} failed: process has exited")
        } else {
            format!("{what} failed: errno {errno}")
        };
        self.toast = Some((msg, Self::toast_deadline()));
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
            Page::Cpu => pages::cpu_ui(ui, snap, &mut self.cpu_core_panel),
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
    // Single instance: one window per session. A second launch focuses the
    // running one (SIGUSR1 → its repaint loop shows/focuses the window)
    // and exits — no new process, no extra sampler thread.
    let lock_path = crate::single::lock_path();
    let _lock = match crate::single::acquire(&lock_path) {
        crate::single::Acquisition::Acquired(l) => l,
        crate::single::Acquisition::Running { holder_pid } => {
            match holder_pid {
                Some(pid) if crate::single::request_surface(pid) => {
                    // the running instance surfaces its window; we are done
                    return Ok(());
                }
                Some(pid) => {
                    eprintln!("rstmog: already running (pid {pid}) but could not focus it");
                    return Ok(());
                }
                None => {
                    // lock contended but the holder is gone (died between
                    // flock and our read) — refuse rather than open a
                    // second window on a race
                    eprintln!("rstmog: another instance holds the session lock; not opening");
                    return Ok(());
                }
            }
        }
    };
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
        // The window runs the same App (and its one sampler thread) that
        // supplied the icon theme above.
        Box::new(move |_cc| {
            // Second-instance focus pings land here (see single.rs).
            crate::single::install_surface_handler();
            // System tray with Open / refresh-rate / Quit menu. Best-effort:
            // platforms without a tray (some Wayland setups) continue without.
            let tray = crate::traythread::spawn("dark", "rsTMOG", 1000);
            if tray.is_none() {
                eprintln!("tray: unavailable on this desktop session");
            }
            Ok(Box::new(app.with_tray(tray)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

#[cfg(test)]
mod menu_tests {
    use super::*;

    type Texts = Vec<(String, egui::Rect)>;

    /// Run one frame of the Processes page; return every text run painted.
    fn frame(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) -> Texts {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1100.0, 700.0),
            )),
            events,
            ..Default::default()
        };
        let out = ctx.run(raw, |ctx| render_page(ctx, app, Page::Processes));
        fn walk(s: &egui::epaint::Shape, out: &mut Texts) {
            match s {
                egui::epaint::Shape::Text(t) => {
                    out.push((t.galley.text().to_string(), t.visual_bounding_rect()))
                }
                egui::epaint::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        for c in &out.shapes {
            walk(&c.shape, &mut texts);
        }
        texts
    }

    /// Move, press, release over three frames (egui hit-tests against the
    /// previous frame's layout). Returns the release frame's text.
    fn click(ctx: &egui::Context, app: &mut App, pos: egui::Pos2, b: egui::PointerButton) -> Texts {
        let m = egui::Modifiers::default();
        frame(ctx, app, vec![egui::Event::PointerMoved(pos)]);
        let press = |pressed| egui::Event::PointerButton {
            pos,
            button: b,
            pressed,
            modifiers: m,
        };
        frame(ctx, app, vec![press(true)]);
        frame(ctx, app, vec![press(false)])
    }

    fn find<'a>(texts: &'a Texts, label: &str) -> Option<&'a egui::Rect> {
        texts.iter().find(|(t, _)| t == label).map(|(_, r)| r)
    }

    /// App on the Processes page plus the screen rect of one row's pid
    /// cell (pid 1 sorts stably; any body row works).
    fn setup() -> (egui::Context, App, u32, egui::Rect) {
        let mut app = App::new_for_capture(50);
        app.pull_snapshot();
        let ctx = egui::Context::default();
        let texts = frame(&ctx, &mut app, vec![]);
        let (t, r) = texts
            .iter()
            .find(|(t, r)| r.min.y > 60.0 && t.trim().parse::<u32>().is_ok())
            .expect("a body row pid cell");
        (ctx, app, t.trim().parse().unwrap(), *r)
    }

    #[test]
    fn any_cell_selects_the_row() {
        // previously only the right-most (user) column took clicks
        let (ctx, mut app, pid, cell) = setup();
        click(&ctx, &mut app, cell.center(), egui::PointerButton::Primary);
        assert_eq!(app.proc_state.selected, Some(pid));
    }

    #[test]
    fn context_menu_stays_open_and_acts_on_that_process() {
        let (ctx, mut app, pid, cell) = setup();
        click(
            &ctx,
            &mut app,
            cell.center(),
            egui::PointerButton::Secondary,
        );
        // previously the menu existed only on the click frame and was
        // never visible; it must persist until something is chosen
        let mut texts = frame(&ctx, &mut app, vec![]);
        for _ in 0..3 {
            texts = frame(&ctx, &mut app, vec![]);
        }
        let end = *find(&texts, "End task").expect("menu still open");
        click(&ctx, &mut app, end.center(), egui::PointerButton::Primary);
        let want = tmog_core::actions::ProcRef {
            pid,
            starttime: app
                .snap
                .procs
                .iter()
                .find(|r| r.proc.pid == pid)
                .unwrap()
                .proc
                .starttime,
        };
        assert_eq!(
            app.proc_state.pending_action,
            Some(proc_table::ProcAction::EndTask(want))
        );
    }

    #[test]
    fn force_kill_needs_a_second_click() {
        let (ctx, mut app, _pid, cell) = setup();
        click(
            &ctx,
            &mut app,
            cell.center(),
            egui::PointerButton::Secondary,
        );
        let texts = frame(&ctx, &mut app, vec![]);
        let fk = *find(&texts, "Force kill").expect("menu open");
        let texts = click(&ctx, &mut app, fk.center(), egui::PointerButton::Primary);
        assert_eq!(
            app.proc_state.pending_action, None,
            "one click must not kill"
        );
        let texts = if find(&texts, "Click again to force kill").is_some() {
            texts
        } else {
            frame(&ctx, &mut app, vec![])
        };
        let confirm = *find(&texts, "Click again to force kill").expect("confirm label");
        click(
            &ctx,
            &mut app,
            confirm.center(),
            egui::PointerButton::Primary,
        );
        assert!(matches!(
            app.proc_state.pending_action,
            Some(proc_table::ProcAction::ForceKill(_))
        ));
    }

    #[test]
    fn on_screen_is_the_rows_the_table_drew() {
        let (_ctx, app, _pid, _cell) = setup();
        assert!(
            !app.proc_state.on_screen.is_empty(),
            "the viewport should report the rows it drew"
        );
        let all: std::collections::HashSet<u32> =
            app.snap.procs.iter().map(|r| r.proc.pid).collect();
        assert!(app.proc_state.on_screen.iter().all(|p| all.contains(p)));
    }

    #[test]
    fn detail_interest_puts_selection_and_screen_ahead_of_the_rest() {
        let pids = detail_interest(Some(7), &[7, 2, 3], &[1, 2, 3, 9]);
        assert_eq!(pids, vec![7, 2, 3, 1, 9]);
        let screen_only = detail_interest(None, &[4, 5], &[]);
        assert_eq!(screen_only, vec![4, 5]);
    }
}
