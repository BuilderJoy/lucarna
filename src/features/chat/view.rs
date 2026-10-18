use eframe::egui::{self, Key, Rect, RichText};
use lucide_icons::Icon;

use super::protocol::{self, Msg};
use crate::session::Outbound;

/// Chat window with the person at the remote machine.
#[derive(Default)]
pub struct ChatView {
    pub open: bool,
    /// Where the window was last drawn, so the screen view can stop forwarding the pointer under it.
    pub rect: Option<Rect>,
    log: Vec<(bool, String)>,
    input: String,
}

impl ChatView {
    /// The server opens, closes or writes to the chat on its own; any text pops the window up.
    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Open => self.open = true,
            Msg::Close | Msg::Finished => self.open = false,
            Msg::Text(t) if t.trim().is_empty() => {}
            Msg::Text(t) => {
                self.log.push((false, t.trim_end().to_string()));
                self.open = true;
            }
        }
    }

    /// The bar button. Opening or closing asks the server to do the same on its side.
    pub fn toggle(&mut self, out: &Outbound) {
        self.open = !self.open;
        if self.open { protocol::open(out) } else { protocol::close(out) }
    }

    pub fn show(&mut self, ctx: &egui::Context, out: &Outbound) {
        if !self.open {
            self.rect = None;
            return;
        }
        let mut open = true;
        let window = egui::Window::new(format!("{} Chat", char::from(Icon::MessageCircle)))
            .open(&mut open)
            .default_size([340.0, 300.0])
            .show(ctx, |ui| {
                let accent = ui.visuals().selection.bg_fill;
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink(false).max_height(ui.available_height() - 36.0).show(ui, |ui| {
                    for (mine, line) in &self.log {
                        let who = if *mine { RichText::new("me").strong().color(accent) } else { RichText::new("remote").strong() };
                        ui.horizontal_wrapped(|ui| {
                            ui.label(who);
                            ui.label(line);
                        });
                    }
                });
                let reply = ui.add(egui::TextEdit::singleline(&mut self.input).hint_text("Type and press Enter").char_limit(2000).desired_width(f32::INFINITY));
                if reply.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let line = std::mem::take(&mut self.input);
                    if !line.trim().is_empty() {
                        protocol::say(out, &line);
                        self.log.push((true, line));
                    }
                    reply.request_focus();
                }
            });
        if !open {
            self.open = false;
            protocol::close(out);
        }
        self.rect = window.map(|w| w.response.rect);
    }
}
