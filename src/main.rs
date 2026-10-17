mod app;
mod features;
mod session;
mod shared;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 768.0]),
        ..Default::default()
    };
    eframe::run_native("Lucarna", options, Box::new(|cc| {
        app::apply_theme(&cc.egui_ctx);
        Ok(Box::<app::App>::default())
    }))
}
