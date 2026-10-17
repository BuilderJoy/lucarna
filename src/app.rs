//! App shell: switches between the connect form and a live session.

use std::sync::{mpsc, Arc, Mutex};

use eframe::egui::{self, Align, Color32, Layout, RichText};

use crate::features::connection::{ConnectForm, ServerAddress};
use crate::features::input::{self, InputForwarder};
use crate::features::remote_screen::{Framebuffer, ScreenView};
use crate::features::clipboard;
use crate::session::{self, Event, Outbound};

struct LiveSession {
    name: String,
    outbound: Arc<Outbound>,
    screen: Arc<Mutex<Framebuffer>>,
    view: ScreenView,
    input: InputForwarder,
    /// False until the first FramebufferUpdate lands; a full Raw frame can take seconds on slow links.
    has_frame: bool,
}

#[derive(Default)]
pub struct App {
    form: ConnectForm,
    events: Option<mpsc::Receiver<Event>>,
    live: Option<LiveSession>,
}

impl App {
    fn connect(&mut self, ctx: &egui::Context, address: ServerAddress, password: String) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || session::run(&address, &password, tx, || ctx.request_repaint()));
        self.events = Some(rx);
        self.form.status.clear();
    }

    fn poll_events(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.events else { return };
        let mut closed = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Connected { name, outbound, screen } => {
                    self.form.status.clear();
                    self.live = Some(LiveSession { name, outbound, screen, view: ScreenView::default(), input: InputForwarder::default(), has_frame: false });
                }
                Event::ScreenUpdated => self.live.iter_mut().for_each(|l| l.has_frame = true),
                Event::ServerClipboard(text) => ctx.copy_text(text),
                Event::Closed(reason) => {
                    self.form.status = reason;
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
            if let Some((address, password)) = self.form.show(ui, self.events.is_some()) {
                self.connect(&ctx, address, password);
            }
            return;
        };
        let bar = egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(12, 6));
        egui::Panel::top("bar").frame(bar).show(ui, |ui| {
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
                    if ui.button("Ctrl+Alt+Del").on_hover_text("Send Ctrl+Alt+Del to the remote machine").clicked() {
                        let _ = input::ctrl_alt_del(&live.outbound);
                    }
                });
            });
        });
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
            let viewport = live.view.show(ui, &live.screen);
            clipboard::forward_paste(ui, &live.outbound);
            live.input.forward(ui, &viewport, &live.outbound);
        });
    }
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
