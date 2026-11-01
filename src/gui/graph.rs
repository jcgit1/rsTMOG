//! Scrolling history graph widget for egui.
//!
//! Design: history is stored per-series as `Vec<(f64, f64)>` of
//! `(sample_index, value)` pairs — sample_index is a monotonically
//! increasing counter, so the newest sample is always pinned right and a
//! window of the last N samples scrolls left. Autoscale uses a slow
//! follower (peak-hold with decay) so traces don't thrash the y-axis;
//! a quarter grid plus 0/50/100% (percent mode) or 0/peak (rate mode).
//! Pure mapping math (`window_to_ui`) is unit-tested without egui.

use eframe::egui;

/// One trace: a name, a color, and its samples (sample_index, value).
#[derive(Debug, Clone)]
pub struct Trace {
    /// legend label; rendered with the theme pass
    #[allow(dead_code)]
    pub name: String,
    pub color: egui::Color32,
    pub samples: Vec<(u64, f64)>,
}

impl Trace {
    pub fn new(name: impl Into<String>, color: egui::Color32) -> Self {
        Trace {
            name: name.into(),
            color,
            samples: Vec::new(),
        }
    }

    /// Append a sample at `tick`, trimming to `max_len` (keeps newest).
    pub fn push(&mut self, tick: u64, value: f64, max_len: usize) {
        self.samples.push((tick, value));
        if self.samples.len() > max_len {
            let excess = self.samples.len() - max_len;
            self.samples.drain(0..excess);
        }
    }
}

/// Autoscale follower: tracks a decaying peak so the y-range is stable.
#[derive(Debug, Clone, Copy)]
pub struct ScaleFollower {
    peak: f64,
    /// per-tick decay toward the observed max (0.0..1.0; smaller = stiffer)
    decay: f64,
    floor: f64,
}

impl ScaleFollower {
    pub fn new(floor: f64, decay: f64) -> Self {
        ScaleFollower {
            peak: floor,
            decay: decay.clamp(0.0, 1.0),
            floor: floor.max(1e-9),
        }
    }

    /// Feed the max value of the current window; returns the y-max to use.
    pub fn observe(&mut self, window_max: f64) -> f64 {
        let target = window_max.max(self.floor);
        // rise fast, fall slow: never shrink the axis faster than decay
        self.peak = if target > self.peak {
            target
        } else {
            self.peak * (1.0 - self.decay) + target * self.decay
        };
        self.peak.max(self.floor)
    }

    pub fn current(&self) -> f64 {
        self.peak.max(self.floor)
    }
}

/// Map a (tick, value) point into a ui rectangle given the visible tick
/// window and y-max. Pure function so it is testable without a GUI.
///
/// - `ticks`: (oldest_visible_tick, newest_visible_tick)
/// - `y_max`: top of the graph in value units (> 0)
/// - returns ui-space Pos2; values above y_max clamp to the top edge.
pub fn window_to_ui(
    rect: egui::Rect,
    tick: u64,
    value: f64,
    ticks: (u64, u64),
    y_max: f64,
) -> egui::Pos2 {
    let (t0, t1) = ticks;
    let span = (t1.saturating_sub(t0)).max(1) as f64;
    let fx = ((tick.saturating_sub(t0)) as f64 / span).clamp(0.0, 1.0) as f32;
    let fy = (value / y_max.max(1e-9)).clamp(0.0, 1.0) as f32;
    egui::pos2(
        rect.left() + fx * rect.width(),
        rect.bottom() - fy * rect.height(),
    )
}

/// A graph with one or more traces sharing a tick window + autoscale.
#[derive(Debug, Clone)]
pub struct GraphModel {
    pub traces: Vec<Trace>,
    pub newest_tick: u64,
    /// number of samples visible (window width in ticks)
    pub window: usize,
    pub scale: ScaleFollower,
    /// percent mode draws 0/25/50/75/100 gridlines; rate mode 0..peak
    pub percent_mode: bool,
}

/// Live-app ticks flow through `Sampler`; the GUI builds models from
/// stored history and paints them directly, so `advance`/`push` are the
/// headless + test-driven path.
#[allow(dead_code)]
impl GraphModel {
    pub fn new(traces: Vec<Trace>, window: usize, percent_mode: bool) -> Self {
        let floor = if percent_mode { 100.0 } else { 1.0 };
        GraphModel {
            traces,
            newest_tick: 0,
            window: window.max(2),
            scale: ScaleFollower::new(floor, 0.1),
            percent_mode,
        }
    }

    /// Advance the model to `tick`, appending each provided value to its
    /// trace (order must match `traces`), recompute the autoscale.
    pub fn advance(&mut self, tick: u64, values: &[f64]) {
        self.newest_tick = tick;
        for (tr, &v) in self.traces.iter_mut().zip(values) {
            tr.push(tick, v, self.window * 2); // keep headroom beyond window
        }
        // observed max across the visible window
        let t0 = self.newest_tick.saturating_sub(self.window as u64);
        let mut mx = 0.0f64;
        for tr in &self.traces {
            for (t, v) in &tr.samples {
                if *t >= t0 {
                    mx = mx.max(*v);
                }
            }
        }
        self.scale.observe(mx);
    }

    pub fn tick_window(&self) -> (u64, u64) {
        (
            self.newest_tick.saturating_sub(self.window as u64),
            self.newest_tick,
        )
    }

    /// Paint into `rect`: quarter grid, then each trace as a polyline with
    /// a faint gradient fill under it.
    pub fn paint(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let painter = ui.painter();
        let y_max = self.scale.current();
        let ticks = self.tick_window();

        // background + border
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(18));
        painter.rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(1.0_f32, egui::Color32::from_gray(60)),
            egui::StrokeKind::Inside,
        );

        // horizontal gridlines
        let lines = 4; // 4 divisions on the y-axis in both modes
        for i in 0..=lines {
            let f = i as f32 / lines as f32;
            let y = rect.bottom() - f * rect.height();
            let color = if i == 0 {
                egui::Color32::from_gray(70)
            } else {
                egui::Color32::from_gray(40)
            };
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                egui::Stroke::new(1.0_f32, color),
            );
            if self.percent_mode {
                let label = format!("{:.0}", f * 100.0_f32);
                painter.text(
                    egui::pos2(rect.left() + 3.0, y - 8.0),
                    egui::Align2::LEFT_TOP,
                    label,
                    egui::FontId::proportional(9.0),
                    egui::Color32::from_gray(110),
                );
            }
        }

        // traces — NaN marks a missing sample (a tick whose read failed):
        // excluded from the polyline, which leaves a visible gap.
        let (t0, t1) = ticks;
        for tr in &self.traces {
            let pts: Vec<egui::Pos2> = tr
                .samples
                .iter()
                .filter(|(t, v)| *t >= t0 && *t <= t1 && v.is_finite())
                .map(|(t, v)| window_to_ui(rect, *t, *v, ticks, y_max))
                .collect();
            if pts.len() >= 2 {
                // fill under the line
                let mut poly = pts.clone();
                poly.push(egui::pos2(pts.last().unwrap().x, rect.bottom()));
                poly.push(egui::pos2(pts[0].x, rect.bottom()));
                painter.add(egui::Shape::convex_polygon(
                    poly,
                    tr.color.linear_multiply(0.16),
                    egui::Stroke::NONE,
                ));
                // trace line as filled quad segments (not a stroked
                // Shape::line): stroked paths tessellate to feather-AA
                // quads that sample the atlas edge, which the software
                // capture rasterizer clips out. Solid quads render
                // identically in egui's GPU path and in the capture.
                let half = 0.75f32; // ~1.5 px
                for w in pts.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let (dx, dy) = (b.x - a.x, b.y - a.y);
                    let len = (dx * dx + dy * dy).max(1e-6).sqrt();
                    let (nx, ny) = (-dy / len * half, dx / len * half);
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(a.x + nx, a.y + ny),
                            egui::pos2(b.x + nx, b.y + ny),
                            egui::pos2(b.x - nx, b.y - ny),
                            egui::pos2(a.x - nx, a.y - ny),
                        ],
                        tr.color,
                        egui::Stroke::NONE,
                    ));
                }
            }
        }
    }
}

/// Convenience widget: reserve space and paint a `GraphModel`.
pub fn graph_ui(ui: &mut egui::Ui, model: &GraphModel, height: f32) {
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    model.paint(ui, rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_to_ui_maps_ends_and_clamps() {
        let rect = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 50.0));
        // oldest tick -> left edge; value 0 -> bottom
        let p = window_to_ui(rect, 10, 0.0, (10, 20), 100.0);
        assert_eq!((p.x, p.y), (0.0, 50.0));
        // newest tick -> right edge; value == ymax -> top
        let p = window_to_ui(rect, 20, 100.0, (10, 20), 100.0);
        assert_eq!((p.x, p.y), (100.0, 0.0));
        // mid
        let p = window_to_ui(rect, 15, 50.0, (10, 20), 100.0);
        assert_eq!((p.x, p.y), (50.0, 25.0));
        // out-of-range clamps
        let p = window_to_ui(rect, 5, 999.0, (10, 20), 100.0);
        assert_eq!((p.x, p.y), (0.0, 0.0));
    }

    #[test]
    fn scale_follower_rises_fast_falls_slow() {
        let mut s = ScaleFollower::new(100.0, 0.1);
        assert_eq!(s.observe(50.0), 100.0); // floor held
        assert_eq!(s.observe(250.0), 250.0); // rises immediately
        let after = s.observe(50.0);
        assert!(after < 250.0 && after > 200.0, "falls slowly, got {after}");
    }

    #[test]
    fn trace_push_trims_to_max_len() {
        let mut tr = Trace::new("t", egui::Color32::WHITE);
        for i in 0..10u64 {
            tr.push(i, i as f64, 4);
        }
        assert_eq!(tr.samples.len(), 4);
        assert_eq!(tr.samples[0].0, 6); // oldest kept is 6
        assert_eq!(tr.samples[3].0, 9);
    }

    #[test]
    fn advance_updates_window_and_scale() {
        let mut g = GraphModel::new(vec![Trace::new("a", egui::Color32::WHITE)], 5, false);
        g.advance(1, &[10.0]);
        g.advance(2, &[80.0]);
        assert_eq!(g.newest_tick, 2);
        assert_eq!(g.tick_window(), (0, 2));
        assert!(g.scale.current() >= 80.0);
    }

    #[test]
    fn advance_trims_older_than_headroom() {
        let mut g = GraphModel::new(vec![Trace::new("a", egui::Color32::WHITE)], 3, false);
        for t in 0..20u64 {
            g.advance(t, &[t as f64]);
        }
        let g_first = g.traces[0].samples.first().unwrap().0;
        // window*2 headroom = keep ~6 newest
        assert!(g_first >= 14, "oldest kept {g_first}");
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    /// The real bug repro: three same-sized graph widgets in one row must
    /// all paint (CPU + GPU + temp). Previously the GPU column drew its
    /// frame but no trace — suspected egui id collision with the CPU graph
    /// via allocate_exact_size.
    #[test]
    fn three_graphs_in_a_row_all_paint() {
        let ctx = egui::Context::default();
        let mk = |name: &str, c: egui::Color32, v: f64| {
            let mut tr = Trace::new(name, c);
            tr.samples = (0u64..60).map(|t| (t, v)).collect();
            let mut m = GraphModel::new(vec![tr], 60, true);
            m.newest_tick = 59;
            m
        };
        let cpu = mk("cpu", egui::Color32::from_rgb(80, 200, 120), 15.0);
        let gpu = mk("gpu", egui::Color32::from_rgb(150, 120, 230), 26.0);
        let full_output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for m in [&cpu, &gpu] {
                        ui.vertical(|ui| {
                            graph_ui(ui, m, 90.0);
                        });
                    }
                });
            });
        });
        let clipped = ctx.tessellate(full_output.shapes, 1.0);
        // count filled quads tinted GPU-purple vs CPU-green
        let mut purple = 0u32;
        let mut green = 0u32;
        for c in &clipped {
            let mesh = match &c.primitive {
                egui::epaint::Primitive::Mesh(m) => m,
                egui::epaint::Primitive::Callback(_) => continue,
            };
            for v in mesh.vertices.iter() {
                let (r, g, b, _a) = (v.color.r(), v.color.g(), v.color.b(), v.color.a());
                if r > 120 && r < 180 && g > 90 && g < 150 && b > 200 {
                    purple += 1;
                }
                if r < 110 && g > 160 && b > 80 && b < 160 {
                    green += 1;
                }
            }
        }
        assert!(green > 0, "CPU trace vertices missing");
        assert!(purple > 0, "GPU trace vertices missing (the bug)");
    }
}

/// `graph_ui` with an explicit width (0 = fill available).
pub fn graph_ui_w(ui: &mut egui::Ui, model: &GraphModel, height: f32, width: f32) {
    let w = if width > 0.0 {
        width
    } else {
        ui.available_width()
    };
    let (rect, _resp) = ui.allocate_exact_size(egui::vec2(w, height), egui::Sense::hover());
    model.paint(ui, rect);
}
