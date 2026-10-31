//! egui/eframe frontend. Skeleton: an empty window; pages are added as the
//! core collectors land.

pub fn run() -> anyhow::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native("rsTMOG", opts, Box::new(|_cc| Ok(Box::new(App))))
        .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

struct App;

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("rsTMOG");
            ui.label("Task Manager OG, in Rust. Collectors landing soon.");
        });
    }
}
