//! App shell: switches between the dashboard and a live session.

use std::sync::{mpsc, Arc, Mutex};

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::features::connection::{Credentials, Dashboard, ServerAddress};
use crate::features::input::{self, InputForwarder};
use crate::features::remote_screen::{Framebuffer, Scale, ScreenView};
use crate::features::clipboard;
use crate::features::file_transfer::{FilesView, RemoteFiles};
use crate::session::{self, Event, Outbound};

struct LiveSession {
    name: String,
    outbound: Arc<Outbound>,
    screen: Arc<Mutex<Framebuffer>>,
    view: ScreenView,
    input: InputForwarder,
    files: Arc<RemoteFiles>,
    files_view: FilesView,
    /// Don't forward keyboard, mouse or clipboard to the server.
    view_only: bool,
    scale: Scale,
    /// Where the session bar floats in full screen, so clicks on it don't reach the remote machine.
    bar_rect: Option<egui::Rect>,
    /// False until the first FramebufferUpdate lands; a full Raw frame can take seconds on slow links.
    has_frame: bool,
}

#[derive(Default)]
pub struct App {
    dashboard: Dashboard,
    events: Option<mpsc::Receiver<Event>>,
    live: Option<LiveSession>,
}

impl App {
    fn connect(&mut self, ctx: &egui::Context, address: ServerAddress, creds: Credentials) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || session::run(&address, &creds, tx, move || ctx.request_repaint()));
        self.events = Some(rx);
        self.dashboard.status.clear();
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.events else { return };
        let mut closed = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Connected { name, outbound, screen, files } => {
                    self.dashboard.status.clear();
                    #[rustfmt::skip]
                    let live = LiveSession { name, outbound, screen, view: ScreenView::default(), input: InputForwarder::default(), files, files_view: FilesView::default(), view_only: false, scale: Scale::default(), bar_rect: None, has_frame: false };
                    self.live = Some(live);
                }
                Event::Files(update) => {
                    if let Some(l) = &mut self.live {
                        l.files_view.apply(update, &l.files);
                    }
                }
                Event::ScreenUpdated => self.live.iter_mut().for_each(|l| l.has_frame = true),
                Event::ServerClipboard(text) => ctx.copy_text(text),
                Event::Closed(reason) => {
                    self.dashboard.status = reason;
                    self.live = None;
                    closed = true;
                }
            }
        }
        if closed {
            self.events = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_events(&ctx);
        let Some(live) = &mut self.live else {
            if let Some((address, creds)) = self.dashboard.show(ui, self.events.is_some()) {
                self.connect(&ctx, address, creds);
            }
            return;
        };
        // Our own shortcut: consumed before input forwarding so the remote machine never sees it.
        let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
        let toggle = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::F));
        if toggle {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        let bar = egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(12, 6));
        if !fullscreen {
            live.bar_rect = None;
            egui::Panel::top("bar").frame(bar).show(ui, |ui| bar_contents(ui, live, fullscreen));
        } else {
            // In full screen the bar floats over the screen and only appears when the pointer reaches the top.
            let near_top = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y < 48.0));
            let show = near_top || ctx.any_popup_open() || !live.has_frame;
            live.bar_rect = show.then(|| {
                egui::Area::new(egui::Id::unique("fullscreen_bar")).anchor(egui::Align2::LEFT_TOP, egui::Vec2::ZERO).order(egui::Order::Foreground).show(&ctx, |ui| {
                    bar.show(ui, |ui| {
                        ui.set_width(ctx.content_rect().width() - 24.0);
                        bar_contents(ui, live, fullscreen);
                    });
                }).response.rect
            });
        }
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(Color32::BLACK)).show(ui, |ui| {
            if !live.has_frame {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() / 2.0 - 40.0);
                    ui.add(egui::Spinner::new().size(32.0));
                    ui.add_space(8.0);
                    ui.label(RichText::new("Waiting for the remote screen…").weak());
                });
                return;
            }
            let viewport = live.view.show(ui, &live.screen, live.scale);
            // Clicks on our own windows and typing in our text inputs must not reach the remote machine.
            let over_ui = [live.files_view.rect, live.bar_rect].into_iter().flatten().any(|r| ui.input(|i| i.pointer.hover_pos().is_some_and(|p| r.contains(p))));
            live.view.draw_cursor(ui, &viewport, !over_ui);
            if !over_ui && !live.view_only && !ui.ctx().egui_wants_keyboard_input() {
                clipboard::forward_paste(ui, &live.outbound);
                live.input.forward(ui, &viewport, &live.outbound);
            }
        });
        live.files_view.show(&ctx, &live.files);
    }
}

fn bar_contents(ui: &mut egui::Ui, live: &mut LiveSession, fullscreen: bool) {
    ui.horizontal(|ui| {
        let dot = if live.has_frame { Color32::from_rgb(80, 200, 120) } else { ui.visuals().warn_fg_color };
        ui.label(RichText::new("●").color(dot));
        ui.label(RichText::new(&live.name).strong());
        let (w, h) = live.screen.lock().unwrap().size();
        ui.label(RichText::new(format!("{w}×{h}")).weak());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Disconnect").clicked() {
                live.outbound.close();
            }
            if ui.selectable_label(live.files_view.open, "Files").on_hover_text("Browse, download and upload files").clicked() {
                live.files_view.open = !live.files_view.open;
            }
            ui.menu_button("Keys", |ui| {
                for (label, keys) in input::SHORTCUTS {
                    if ui.button(*label).clicked() {
                        let _ = input::combo(&live.outbound, keys);
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Send key combinations your own system would intercept");
            if ui.button("Ctrl+Alt+Del").on_hover_text("Send Ctrl+Alt+Del to the remote machine").clicked() {
                let _ = input::ctrl_alt_del(&live.outbound);
            }
            ui.separator();
            let shortcut = if cfg!(target_os = "macos") { "⌘⇧F" } else { "Ctrl+Shift+F" };
            if ui.selectable_label(fullscreen, "Full screen").on_hover_text(shortcut).clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
            }
            let (label, tip) = match live.scale {
                Scale::Fit => ("Fit", "Fit to window. Click for actual size (1:1, scroll bars)"),
                Scale::Actual => ("1:1", "Actual size. Click to fit the window"),
            };
            if ui.button(label).on_hover_text(tip).clicked() {
                live.scale = if live.scale == Scale::Fit { Scale::Actual } else { Scale::Fit };
            }
            ui.selectable_label(live.view_only, "View only").on_hover_text("Don't send keyboard, mouse or clipboard to the server").clicked().then(|| live.view_only = !live.view_only);
        });
    });
}

/// Dark theme with the amber accent of the logo.
pub fn apply_theme(ctx: &egui::Context) {
    let accent = Color32::from_rgb(0xff, 0xbe, 0x3d);
    let mut v = egui::Visuals::dark();
    v.selection.bg_fill = accent;
    v.selection.stroke.color = Color32::BLACK;
    v.hyperlink_color = accent;
    v.panel_fill = Color32::from_rgb(0x10, 0x16, 0x22);
    v.window_fill = Color32::from_rgb(0x18, 0x21, 0x33);
    v.extreme_bg_color = Color32::from_rgb(0x0b, 0x10, 0x1a);
    v.widgets.hovered.bg_stroke.color = accent;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals(v);
}
