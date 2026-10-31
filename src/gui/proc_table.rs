//! Interactive processes table: sortable (header-click), live filter,
//! tree toggle, selection that survives refreshes (by pid), follow mode,
//! and a right-click context menu. State lives in `ProcTableState`, which
//! `App` owns across frames; the rows come from the immutable snapshot.

use crate::core::process_table::{self, Row, SortDir, SortKey, TreeRow};
use crate::sampler::Snapshot;
use eframe::egui;

/// What the context menu can do to a row (the GUI never calls the kernel
/// directly — it emits intents that `App` executes via core::actions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcAction {
    EndTask(u32),
    ForceKill(u32),
    Suspend(u32),
    Resume(u32),
    SetNice(u32, i32),
    CopyCmdline(String),
}

#[derive(Default)]
pub struct ProcTableState {
    pub sort: SortKey,
    pub dir: SortDir,
    pub filter: String,
    pub tree: bool,
    pub follow: bool,
    /// selected pid (survives refresh; None = nothing selected)
    pub selected: Option<u32>,
    /// last action requested by the context menu, drained by App
    pub pending_action: Option<ProcAction>,
}

impl ProcTableState {
    /// Load persisted defaults (sort/tree/follow from settings).
    pub fn from_settings(s: &crate::core::settings::Settings) -> Self {
        let sort = match s.proc_sort.as_str() {
            "pid" => SortKey::Pid,
            "name" => SortKey::Name,
            "mem" => SortKey::Mem,
            "user" => SortKey::User,
            "threads" => SortKey::Threads,
            _ => SortKey::Cpu,
        };
        ProcTableState {
            sort,
            dir: if s.proc_sort_desc {
                SortDir::Desc
            } else {
                SortDir::Asc
            },
            tree: s.proc_tree,
            follow: s.proc_follow,
            ..Default::default()
        }
    }

    pub fn persist_into(&self, s: &mut crate::core::settings::Settings) {
        s.proc_sort = match self.sort {
            SortKey::Pid => "pid",
            SortKey::Name => "name",
            SortKey::Cpu => "cpu",
            SortKey::Mem => "mem",
            SortKey::User => "user",
            SortKey::Threads => "threads",
        }
        .to_string();
        s.proc_sort_desc = self.dir == SortDir::Desc;
        s.proc_tree = self.tree;
        s.proc_follow = self.follow;
    }
}

/// Sortable column header; returns Some(new_sort) if clicked to switch.
fn header(ui: &mut egui::Ui, label: &str, key: SortKey, st: &ProcTableState) -> Option<SortKey> {
    let arrow = if st.sort == key {
        match st.dir {
            SortDir::Asc => " ▲",
            SortDir::Desc => " ▼",
        }
    } else {
        ""
    };
    let txt = format!("{label}{arrow}");
    if ui
        .add(egui::Label::new(egui::RichText::new(txt).strong()).sense(egui::Sense::click()))
        .clicked()
    {
        return Some(key);
    }
    None
}

pub fn processes_table_ui(ui: &mut egui::Ui, s: &Snapshot, st: &mut ProcTableState) {
    ui.heading("Processes");

    // ---- toolbar ----
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text("filter name/cmdline/user/pid")
                .desired_width(220.0),
        );
        if ui.checkbox(&mut st.tree, "tree").changed() && st.tree {
            // tree + cpu sort is confusing (parents float); keep it allowed
            // but it's the reference behavior too
        }
        ui.checkbox(&mut st.follow, "follow");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(format!("{} procs", s.procs.len()));
        });
    });

    // ---- build the visible row set ----
    // filter
    let mut rows: Vec<Row> = s
        .procs
        .iter()
        .filter(|r| process_table::matches(r, &st.filter))
        .cloned()
        .collect();
    // sort
    process_table::sort_rows(&mut rows, st.sort, st.dir);

    // flatten to tree order if tree mode (children follow sort order,
    // filtered-out parents float children to root)
    let display: Vec<TreeRow> = if st.tree {
        process_table::tree_rows(&rows)
    } else {
        rows.iter()
            .enumerate()
            .map(|(i, _)| TreeRow { depth: 0, row: i })
            .collect()
    };

    // ---- header row ----
    let mut click: Option<SortKey> = None;
    egui::Grid::new("proc_header")
        .num_columns(5)
        .spacing([14.0, 2.0])
        .show(ui, |ui| {
            if let Some(k) = header(ui, "pid", SortKey::Pid, st) {
                click = Some(k);
            }
            if let Some(k) = header(ui, "name", SortKey::Name, st) {
                click = Some(k);
            }
            if let Some(k) = header(ui, "cpu%", SortKey::Cpu, st) {
                click = Some(k);
            }
            if let Some(k) = header(ui, "mem", SortKey::Mem, st) {
                click = Some(k);
            }
            if let Some(k) = header(ui, "user", SortKey::User, st) {
                click = Some(k);
            }
            ui.label("");
            ui.end_row();
        });
    if let Some(k) = click {
        if st.sort == k {
            st.dir = match st.dir {
                SortDir::Asc => SortDir::Desc,
                SortDir::Desc => SortDir::Asc,
            };
        } else {
            st.sort = k;
        }
    }

    ui.separator();

    // ---- scrollable body with selection + context menu ----
    // Rows rebuild each frame; selection is by pid so it survives refresh.
    let mut menu_pid: Option<u32> = None;

    egui::ScrollArea::vertical()
        .id_salt("proc_body")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for tr in &display {
                let r = &rows[tr.row];
                let pid = r.proc.pid;
                let is_sel = st.selected == Some(pid);
                let name = r.proc.display_name();
                let indent = if tr.depth > 0 {
                    format!("{}└ ", "  ".repeat(tr.depth.min(16)))
                } else {
                    String::new()
                };

                // One interactive label per row carries both selection
                // (left click) and the context-menu trigger (right click).
                let row_label = format!(
                    "{:>7}  {:<38} {:>5}  {:>9}  {}",
                    pid,
                    truncate(&format!("{indent}{name}"), 38),
                    r.cpu
                        .map(|c| format!("{c:.1}"))
                        .unwrap_or_else(|| "-".into()),
                    mem_h(r.proc.rss_bytes),
                    r.proc.user.clone().unwrap_or_default(),
                );
                let resp = ui.add(
                    egui::Label::new(
                        egui::RichText::new(row_label)
                            .monospace()
                            .color(row_color(r.proc.state)),
                    )
                    .sense(egui::Sense::click()),
                );
                if resp.clicked() {
                    st.selected = Some(pid);
                } else if resp.secondary_clicked() {
                    st.selected = Some(pid);
                    menu_pid = Some(pid);
                }
                // follow mode: keep the selected row centered as the list
                // re-sorts under us (egui scrolls the widget into view)
                if st.follow && is_sel {
                    resp.scroll_to_me(Some(egui::Align::Center));
                }
            }
        });

    // context menu for the requested row
    if let Some(pid) = menu_pid {
        let r = s.procs.iter().find(|r| r.proc.pid == pid);
        let state = r.map(|r| r.proc.state).unwrap_or('?');
        let cmdline = r.map(|r| r.proc.cmdline.join(" ")).unwrap_or_default();
        egui::Window::new("proc_menu")
            .id(egui::Id::new(("procmenu", pid)))
            .collapsible(false)
            .title_bar(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                if ui.button("End task").clicked() {
                    st.pending_action = Some(ProcAction::EndTask(pid));
                    ui.close();
                }
                if ui.button("Force kill").clicked() {
                    st.pending_action = Some(ProcAction::ForceKill(pid));
                    ui.close();
                }
                ui.separator();
                if state == 'T' {
                    if ui.button("Resume").clicked() {
                        st.pending_action = Some(ProcAction::Resume(pid));
                        ui.close();
                    }
                } else if ui.button("Suspend").clicked() {
                    st.pending_action = Some(ProcAction::Suspend(pid));
                    ui.close();
                }
                ui.separator();
                ui.label("priority:");
                for (label, nice) in [("High", -10), ("Normal", 0), ("Low", 10), ("Idle", 19)] {
                    if ui.button(label).clicked() {
                        st.pending_action = Some(ProcAction::SetNice(pid, nice));
                        ui.close();
                    }
                }
                ui.separator();
                let can_copy = !cmdline.is_empty();
                if ui
                    .add_enabled(can_copy, egui::Button::new("Copy command line"))
                    .clicked()
                {
                    st.pending_action = Some(ProcAction::CopyCmdline(cmdline.clone()));
                    ui.close();
                }
            });
    }
}

fn mem_h(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= 10 * GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{} KiB", bytes / 1024)
    }
}

/// Truncate to `n` chars without splitting a UTF-8 boundary.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let cut: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Row text color by process state: zombies red, stopped amber, else
/// inherited (returns None so the caller uses the theme default).
fn row_color(state: char) -> egui::Color32 {
    match state {
        'Z' | 'X' => egui::Color32::from_rgb(220, 90, 90),
        'T' => egui::Color32::from_rgb(230, 176, 90),
        _ => egui::Color32::from_rgb(200, 200, 200),
    }
}
