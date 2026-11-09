use std::time::SystemTime;

use eframe::egui::{self, Rect, RichText};

use crate::shared::theme;
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
    filter: Filter,
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

    /// `active` is whether clipboard sharing is currently on for the session (shown as Synced / Paused).
    pub fn show(&mut self, ctx: &egui::Context, clipboard: &RemoteClipboard, active: bool) {
        if !self.open {
            self.rect = None;
            return;
        }
        let p = theme::palette(ctx);
        let mut close = false;
        let mut input_id = None;
        let frame = egui::Frame::window(&ctx.global_style())
            .fill(p.surface)
            .inner_margin(egui::Margin::ZERO)
            .corner_radius(egui::CornerRadius::same(14));
        let window = egui::Window::new("clipboard_window")
            .title_bar(false)
            .frame(frame)
            .resizable(true)
            .default_size([440.0, 620.0])
            .min_size([340.0, 420.0])
            .show(ctx, |ui| {
                close = theme::window_header(ui, Icon::Clipboard, "Clipboard", |ui| {
                    let (text, fg, bg) = if active {
                        ("Synced", p.success, p.success.gamma_multiply(0.16))
                    } else {
                        ("Paused", p.text3, p.elevated)
                    };
                    theme::tag(ui, Icon::Circle, text, fg, bg);
                });
                input_id = self.send_section(ui, ctx, clipboard);
                ui.add(egui::Separator::default().spacing(0.0));
                self.history_section(ui, ctx, clipboard);
            });
        if close {
            self.open = false;
        }
        self.rect = window.map(|w| w.response.rect);
        // A click outside the window must give the keyboard back to the remote screen, or Ctrl+V keeps pasting into the box.
        if let Some(id) = input_id
            && ctx.input(|i| i.pointer.any_pressed())
            && !ctx.input(|i| {
                i.pointer
                    .interact_pos()
                    .is_some_and(|p| self.rect.is_some_and(|r| r.contains(p)))
            })
        {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
    }

    fn send_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        clipboard: &RemoteClipboard,
    ) -> Option<egui::Id> {
        let p = theme::palette(ctx);
        let mut input_id = None;
        egui::Frame::NONE
            .inner_margin(egui::Margin::same(16))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Send text to the remote clipboard")
                        .size(12.0)
                        .strong()
                        .color(p.text2),
                );
                ui.add_space(6.0);
                let edit = egui::TextEdit::multiline(&mut self.manual_input)
                    .hint_text("Type or paste text…")
                    .font(egui::FontId::monospace(12.0))
                    .desired_rows(4)
                    .margin(egui::vec2(12.0, 10.0))
                    .desired_width(f32::INFINITY);
                input_id = Some(ui.add(edit).id);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} chars · Latin-1",
                            self.manual_input.chars().count()
                        ))
                        .size(11.0)
                        .color(p.text3),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let can_send = !self.manual_input.trim().is_empty();
                        if ui
                            .add_enabled(
                                can_send,
                                theme::primary(format!(
                                    "{} Send to remote",
                                    char::from(Icon::Send)
                                ))
                                .min_size(egui::vec2(0.0, 34.0)),
                            )
                            .clicked()
                        {
                            let text = std::mem::take(&mut self.manual_input);
                            let _ = clipboard.send_client_text(&text);
                            ctx.copy_text(text.clone());
                            self.push(text, false);
                        }
                        if ui
                            .add(egui::Button::new("Clear").min_size(egui::vec2(0.0, 34.0)))
                            .clicked()
                        {
                            self.manual_input.clear();
                        }
                    });
                });
            });
        input_id
    }

    fn history_section(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        clipboard: &RemoteClipboard,
    ) {
        let p = theme::palette(ctx);
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("History").size(13.0).strong());
                    theme::count_pill(ui, self.history.len());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let items = [
                            (Filter::All, "All".to_string(), "Everything"),
                            (
                                Filter::FromRemote,
                                "From remote".to_string(),
                                "Copied on the remote machine",
                            ),
                            (
                                Filter::ToRemote,
                                "To remote".to_string(),
                                "Sent from this computer",
                            ),
                        ];
                        theme::segmented(ui, &mut self.filter, &items);
                    });
                });
            });

        let shown: Vec<usize> = (0..self.history.len())
            .filter(|&i| match self.filter {
                Filter::All => true,
                Filter::FromRemote => self.history[i].is_from_server,
                Filter::ToRemote => !self.history[i].is_from_server,
            })
            .collect();
        let (mut to_send, mut to_copy) = (None, None);
        let list_h = (ui.available_height() - 44.0).max(60.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(list_h)
            .show(ui, |ui| {
                ui.add_space(0.0);
                egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(12, 0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if shown.is_empty() {
                            ui.add_space(36.0);
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    RichText::new("Nothing here yet")
                                        .strong()
                                        .color(p.text2),
                                );
                                ui.label(
                                    RichText::new(
                                        "Copy text on this computer or on the remote machine and it appears here.",
                                    )
                                    .size(12.0)
                                    .color(p.text3),
                                );
                            });
                        }
                        for &i in &shown {
                            let item = &self.history[i];
                            if let Some(a) = item_card(ui, item) {
                                match a {
                                    ItemAction::Copy => to_copy = Some(item.text.clone()),
                                    ItemAction::Send => to_send = Some(item.text.clone()),
                                }
                            }
                            ui.add_space(8.0);
                        }
                    });
            });
        if let Some(text) = to_send {
            let _ = clipboard.send_client_text(&text);
            self.push(text, false);
        }
        if let Some(text) = to_copy {
            ctx.copy_text(text);
        }

        ui.add(egui::Separator::default().spacing(0.0));
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(16, 8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} {} in the session pastes straight to the remote",
                            char::from(Icon::Info),
                            if cfg!(target_os = "macos") {
                                "⌘V"
                            } else {
                                "Ctrl+V"
                            }
                        ))
                        .size(11.0)
                        .color(p.text3),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(
                                !self.history.is_empty(),
                                egui::Button::new(RichText::new("Clear history").size(12.0))
                                    .fill(egui::Color32::TRANSPARENT)
                                    .stroke(egui::Stroke::NONE),
                            )
                            .clicked()
                        {
                            self.history.clear();
                        }
                    });
                });
            });
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Filter {
    #[default]
    All,
    FromRemote,
    ToRemote,
}

enum ItemAction {
    Copy,
    Send,
}

/// One history entry as a card: direction tag, time, Copy / Send, and a three-line preview.
fn item_card(ui: &mut egui::Ui, item: &ClipboardItem) -> Option<ItemAction> {
    let p = theme::palette(ui.ctx());
    let mut action = None;
    egui::Frame::NONE
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if item.is_from_server {
                    theme::tag(
                        ui,
                        Icon::ArrowDown,
                        "From remote",
                        p.success,
                        p.success.gamma_multiply(0.16),
                    );
                } else {
                    theme::tag(ui, Icon::ArrowUp, "To remote", p.accent_text, p.accent_soft);
                }
                ui.label(RichText::new(&item.time_str).size(11.0).color(p.text3));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add(
                            egui::Button::new(format!("{} Send", char::from(Icon::Send)))
                                .min_size(egui::vec2(0.0, 28.0)),
                        )
                        .on_hover_text("Send this text to the remote computer")
                        .clicked()
                    {
                        action = Some(ItemAction::Send);
                    }
                    if ui
                        .add(
                            egui::Button::new(format!("{} Copy", char::from(Icon::Copy)))
                                .min_size(egui::vec2(0.0, 28.0)),
                        )
                        .on_hover_text("Copy to your clipboard")
                        .clicked()
                    {
                        action = Some(ItemAction::Copy);
                    }
                });
            });
            ui.add_space(6.0);
            ui.add(egui::Label::new(
                RichText::new(preview(&item.text)).monospace().size(12.0),
            ));
        });
    action
}

/// First three lines, at most 240 characters.
fn preview(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: String = lines.iter().take(3).copied().collect::<Vec<_>>().join("\n");
    let cut = out.chars().count() > 240;
    if cut {
        out = out.chars().take(240).collect();
    }
    if cut || lines.len() > 3 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_keeps_three_lines_and_marks_the_cut() {
        assert_eq!(preview("a\nb"), "a\nb");
        assert_eq!(preview("1\n2\n3\n4"), "1\n2\n3…");
        assert_eq!(preview(&"x".repeat(300)).chars().count(), 241);
    }
}
