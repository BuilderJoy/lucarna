// Release builds on Windows are GUI apps: no console window behind Lucarna.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod features;
mod session;
mod shared;

use eframe::egui;

pub const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 768.0])
            // Matches StartupWMClass in packaging/linux/lucarna.desktop, so the desktop shows our icon.
            .with_app_id("lucarna")
            .with_icon(eframe::icon_data::from_png_bytes(LOGO_PNG).expect("valid logo.png")),
        ..Default::default()
    };
    eframe::run_native("Lucarna", options, Box::new(|cc| {
        app::apply_theme(&cc.egui_ctx);
        Ok(Box::<app::App>::default())
    }))
}
