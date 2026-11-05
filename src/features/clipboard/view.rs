use std::time::SystemTime;

use eframe::egui::{self, Color32, Rect, RichText};
use lucide_icons::Icon;

use super::RemoteClipboard;

#[derive(Clone, Debug)]
pub struct ClipboardItem {
    pub text: String,
    pub is_from_server: bool,
    pub time_str: String,
}

#[derive(Default)]
pub struct ClipboardView {
    pub open: bool,
    pub rect: Option<Rect>,
    pub history: Vec<ClipboardItem>,
    pub manual_input: String,
}

impl ClipboardView {
    pub fn push(&mut self, text: String, is_from_server: bool) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(first) = self.history.first()
            && first.text == text
            && first.is_from_server == is_from_server
        {
            return;
        }
        let now_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let secs = now_secs % 86400;
        let time_str = format!(
            "{:02}:{:02}:{:02}",
            secs / 3600,
            (secs % 3600) / 60,
            secs % 60
        );

        self.history.insert(
            0,
            ClipboardItem {
                text,
                is_from_server,
                time_str,
            },
        );
        if self.history.len() > 50 {
            self.history.truncate(50);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, clipboard: &RemoteClipboard) {
        if !self.open {
            self.rect = None;
            return;
        }
        let mut open = true;
        let window = egui::Window::new(format!("{} Clipboard Sync", char::from(Icon::Clipboard)))
            .open(&mut open)
            .default_size([440.0, 480.0])
            .min_size([320.0, 300.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Send text to remote clipboard:").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.history.is_empty()
                            && ui
                                .small_button(format!("{} Clear", char::from(Icon::Trash2)))
                                .on_hover_text("Clear clipboard history")
                                .clicked()
                        {
                            self.history.clear();
                        }
                    });
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::multiline(&mut self.manual_input)
                        .hint_text("Paste or type text here to send to remote computer…")
                        .desired_rows(3)
                        .desired_width(f32::INFINITY);
                    ui.add(edit);
                });

                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    let can_send = !self.manual_input.trim().is_empty();
                    if ui
                        .add_enabled(
                            can_send,
                            egui::Button::new(
                                RichText::new(format!("{} Send to Remote", char::from(Icon::Send)))
                                    .strong(),
                            ),
                        )
                        .clicked()
                    {
                        let text = std::mem::take(&mut self.manual_input);
                        let _ = clipboard.send_client_text(&text);
                        ctx.copy_text(text.clone());
                        self.push(text, false);
                    }
                    if ui
                        .button(format!("{} Clear input", char::from(Icon::X)))
                        .clicked()
                    {
                        self.manual_input.clear();
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.label(
                    RichText::new(format!("History ({} items)", self.history.len()))
                        .strong()
                        .weak(),
                );
                ui.add_space(4.0);

                if self.history.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.label(RichText::new("No clipboard history yet").italics().weak());
                        ui.label(
                            RichText::new(
                                "Copy text on your computer or on the remote machine to see it appear here.",
                            )
                            .size(12.0)
                            .weak(),
                        );
                    });
                } else {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let mut to_send = None;
                            let mut to_copy = None;

                            for item in &self.history {
                                egui::Frame::group(ui.style()).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let (tag, color) = if item.is_from_server {
                                            ("Remote ➔ Local", Color32::from_rgb(76, 175, 80))
                                        } else {
                                            ("Local ➔ Remote", ui.visuals().selection.bg_fill)
                                        };
                                        ui.label(
                                            RichText::new(tag).size(11.0).strong().color(color),
                                        );
                                        ui.label(RichText::new(&item.time_str).size(10.0).weak());

                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                if ui
                                                    .small_button(format!(
                                                        "{} Send",
                                                        char::from(Icon::Send)
                                                    ))
                                                    .on_hover_text(
                                                        "Send this text to the remote computer",
                                                    )
                                                    .clicked()
                                                {
                                                    to_send = Some(item.text.clone());
                                                }
                                                if ui
                                                    .small_button(format!(
                                                        "{} Copy",
                                                        char::from(Icon::Copy)
                                                    ))
                                                    .on_hover_text("Copy to local clipboard")
                                                    .clicked()
                                                {
                                                    to_copy = Some(item.text.clone());
                                                }
                                            },
                                        );
                                    });

                                    ui.add_space(2.0);
                                    let preview = if item.text.len() > 300 {
                                        format!("{}…", &item.text[..300])
                                    } else {
                                        item.text.clone()
                                    };
                                    ui.label(RichText::new(preview).monospace().size(12.0));
                                });
                                ui.add_space(4.0);
                            }

                            if let Some(text) = to_send {
                                let _ = clipboard.send_client_text(&text);
                                self.push(text, false);
                            }
                            if let Some(text) = to_copy {
                                ctx.copy_text(text);
                            }
                        });
                }
            });

        if !open {
            self.open = false;
        }
        self.rect = window.map(|w| w.response.rect);
    }
}
