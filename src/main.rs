//! Lucarna: a VNC (RFB) viewer that can also share this computer.
//!
//! # Where to start reading
//!
//! | You want to understand | Read |
//! |---|---|
//! | How the window, tabs and session bar work | `app.rs` |
//! | How a connection to a server runs | `session/mod.rs` |
//! | One capability (screen, keyboard, clipboard, files, chat, SSH) | its folder under `features/` |
//! | Sharing this computer | `features/host/mod.rs` |
//! | Encryption with a PIN | `shared/crypto.rs` |
//! | Colours, translations, byte helpers | `shared/` |
//!
//! # The three ways this program starts
//!
//! `main` checks them in order:
//! 1. **askpass**: `ssh` ran us to ask for a saved SSH password. Answer and exit; no window.
//! 2. **`--host`**: share this computer from a terminal; no window.
//! 3. **The app**: open the window and hand control to `app::App`.

// Release builds on Windows are GUI apps: no console window behind Lucarna.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod features;
mod session;
mod shared;

use eframe::egui;

pub const LOGO_PNG: &[u8] = include_bytes!("../assets/logo.png");

fn main() -> eframe::Result {
    // ssh runs Lucarna as its askpass helper to get a saved SSH password: answer and exit, no window.
    if let Some(code) = features::ssh::askpass() {
        std::process::exit(code);
    }
    // `--host [addr] [--password <pw>]` runs the embedded RFB 3.8 + Native File Transfer Host.
    if let Some(code) = features::host::cli_host() {
        std::process::exit(code);
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([900.0, 600.0])
            // Matches StartupWMClass in packaging/linux/lucarna.desktop, so the desktop shows our icon.
            .with_app_id("lucarna")
            .with_icon(eframe::icon_data::from_png_bytes(LOGO_PNG).expect("valid logo.png")),
        ..Default::default()
    };
    eframe::run_native(
        "Lucarna",
        options,
        Box::new(|cc| {
            shared::theme::apply(&cc.egui_ctx);
            shared::i18n::init(&cc.egui_ctx);
            Ok(Box::<app::App>::default())
        }),
    )
}
