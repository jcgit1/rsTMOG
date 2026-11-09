//! Interactive processes table: sortable (header-click), live filter,
//! tree toggle, selection that survives refreshes (by pid), follow mode,
//! and a right-click context menu. State lives in `ProcTableState`, which
//! `App` owns across frames; the rows come from the immutable snapshot.

use crate::sampler::Snapshot;
use eframe::egui;
use tmog_core::process_table::{self, Row, SortDir, SortKey, TreeRow};

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
    pub fn from_settings(s: &tmog_core::settings::Settings) -> Self {
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

    pub fn persist_into(&self, s: &mut tmog_core::settings::Settings) {
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

/// Cell padding inside the table grid (left inset for every cell so text
/// never touches the column borders we draw).
const CELL_PAD: egui::Vec2 = egui::vec2(6.0, 2.0);

/// Monospace column widths (in characters) shared by the header and body so
/// they line up. The name column gets the slack; pid/cpu/mem are bounded,
/// and user is wide enough for any real name.
const PID_W: usize = 7;
const NAME_W: usize = 38;
const CPU_W: usize = 5;
const MEM_W: usize = 9;
const USER_W: usize = 12;

/// Header text with the sort arrow appended when this column is active.
fn header_label(label: &str, key: SortKey, st: &ProcTableState) -> String {
    let arrow = if st.sort == key {
        match st.dir {
            SortDir::Asc => " ▲",
            SortDir::Desc => " ▼",
        }
    } else {
        ""
    };
    format!("{label}{arrow}")
}

/// Pad/truncate to exactly `n` characters (left-aligned).
fn pad(s: &str, n: usize) -> String {
    let t = truncate(s, n);
    format!("{t:<n$}")
}

/// Pad/truncate to exactly `n` characters (right-aligned, for numbers).
fn pad_left(s: &str, n: usize) -> String {
    let t = truncate(s, n);
    format!("{t:>n$}")
}

/// Convenience: pad a `String` by value.
fn pads(s: String, n: usize) -> String {
    pad(&s, n)
}

/// Convenience: pad a `String` by value, right-aligned.
fn pads_left(s: String, n: usize) -> String {
    pad_left(&s, n)
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

    // ---- table: header + body in ONE grid so columns share widths,
    // with row/column borders drawn on the grid's background layer ----
    let mut click: Option<SortKey> = None;
    let mut menu_pid: Option<u32> = None;
    // row clicked this frame (applied after the grid so the highlight and
    // the re-sorted rows appear on the same frame)
    let mut clicked_pid: Option<u32> = None;

    // Border colour/stroke: theme widget stroke, bumped to visible width.
    let border_stroke = {
        let s = ui.visuals().widgets.noninteractive.bg_stroke;
        egui::Stroke::new(1.0_f32, s.color)
    };
    let sel_bg = ui.visuals().selection.bg_fill;

    let mut rows_rects: Vec<egui::Rect> = Vec::new();
    let mut sel_row: Option<usize> = None;

    egui::ScrollArea::vertical()
        .id_salt("proc_body")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("proc_grid")
                .num_columns(5)
                .spacing([0.0, 0.0])
                .striped(true)
                .show(ui, |ui| {
                    // ---- header cells (fixed-width monospace, clickable) ----
                    let cells: [(String, SortKey); 5] = [
                        (
                            pads(header_label("pid", SortKey::Pid, st), PID_W),
                            SortKey::Pid,
                        ),
                        (
                            pads(header_label("name", SortKey::Name, st), NAME_W),
                            SortKey::Name,
                        ),
                        (
                            pads(header_label("cpu%", SortKey::Cpu, st), CPU_W),
                            SortKey::Cpu,
                        ),
                        (
                            pads(header_label("mem", SortKey::Mem, st), MEM_W),
                            SortKey::Mem,
                        ),
                        (
                            pads(header_label("user", SortKey::User, st), USER_W),
                            SortKey::User,
                        ),
                    ];
                    for (txt, key) in cells {
                        ui.horizontal(|ui| {
                            ui.add_space(CELL_PAD.x);
                            let resp = ui.add(
                                egui::Label::new(egui::RichText::new(txt).monospace().strong())
                                    .sense(egui::Sense::click()),
                            );
                            if resp.clicked() {
                                click = Some(key);
                            }
                        });
                    }
                    rows_rects.push(ui.min_rect());
                    ui.end_row();

                    // ---- body rows ----
                    for tr in &display {
                        let r = &rows[tr.row];
                        let pid = r.proc.pid;
                        let name = r.proc.display_name();
                        let indent = if tr.depth > 0 {
                            format!("{}└ ", "  ".repeat(tr.depth.min(16)))
                        } else {
                            String::new()
                        };
                        let cpu = r
                            .cpu
                            .map(|c| format!("{c:.1}"))
                            .unwrap_or_else(|| "-".into());
                        let user = r.proc.user.clone().unwrap_or_default();

                        ui.horizontal(|ui| {
                            ui.add_space(CELL_PAD.x);
                            ui.label(
                                egui::RichText::new(pads_left(pid.to_string(), PID_W))
                                    .monospace()
                                    .color(row_color(r.proc.state)),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(CELL_PAD.x);
                            ui.label(
                                egui::RichText::new(pads(format!("{indent}{name}"), NAME_W))
                                    .monospace()
                                    .color(row_color(r.proc.state)),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(CELL_PAD.x);
                            ui.label(
                                egui::RichText::new(pads_left(cpu, CPU_W))
                                    .monospace()
                                    .color(row_color(r.proc.state)),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.add_space(CELL_PAD.x);
                            ui.label(
                                egui::RichText::new(pads_left(mem_h(r.proc.rss_bytes), MEM_W))
                                    .monospace()
                                    .color(row_color(r.proc.state)),
                            );
                        });
                        let resp = ui.horizontal(|ui| {
                            ui.add_space(CELL_PAD.x);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(pads(user, USER_W))
                                        .monospace()
                                        .color(row_color(r.proc.state)),
                                )
                                .sense(egui::Sense::click()),
                            )
                        });
                        let resp = resp.inner;
                        let row_rect = ui.min_rect();
                        if st.selected == Some(pid) {
                            sel_row = Some(rows_rects.len());
                        }
                        rows_rects.push(row_rect);

                        if resp.clicked() {
                            clicked_pid = Some(pid);
                        } else if resp.secondary_clicked() {
                            clicked_pid = Some(pid);
                            menu_pid = Some(pid);
                        }
                        // follow mode: keep the selected row centered as the
                        // list re-sorts under us (egui scrolls it into view)
                        if st.follow && st.selected == Some(pid) {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                        ui.end_row();
                    }
                });

            // Row/column borders + selection fill, painted on the scroll
            // content's background layer so they sit under the text and
            // scroll with the rows.
            let painter = ui.painter();
            if let Some(i) = sel_row {
                painter.rect_filled(rows_rects[i], 0.0, sel_bg);
            }
            for rect in &rows_rects {
                painter.rect_stroke(*rect, 0.0, border_stroke, egui::StrokeKind::Inside);
            }
        });

    if let Some(pid) = clicked_pid {
        st.selected = Some(pid);
    }
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
