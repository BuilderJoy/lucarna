use eframe::egui::{self, Align, Key, Layout, RichText};

use super::ServerAddress;

#[derive(Default)]
pub struct ConnectForm {
    address: String,
    password: String,
    pub status: String,
}

impl ConnectForm {
    /// Returns the target when the user asks to connect.
    pub fn show(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<(ServerAddress, String)> {
        let mut request = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(((ui.available_height() - 380.0) / 2.0).max(24.0));
                let card = egui::Frame::window(ui.style()).corner_radius(12).inner_margin(28);
                card.show(ui, |ui| {
                    ui.set_width(320.0);
                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                        ui.label(RichText::new("Lucarna").size(28.0).strong().color(ui.visuals().selection.bg_fill));
                        ui.label(RichText::new("Connect to a VNC server").weak());
                        ui.add_space(20.0);

                        ui.add_enabled_ui(!busy, |ui| {
                            ui.label("Server");
                            let addr = ui.add(
                                egui::TextEdit::singleline(&mut self.address)
                                    .hint_text("host, host:display or host::port")
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::vec2(8.0, 6.0)),
                            );
                            ui.add_space(8.0);
                            ui.label("Password");
                            let pw = ui.add(
                                egui::TextEdit::singleline(&mut self.password)
                                    .password(true)
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::vec2(8.0, 6.0)),
                            );
                            let enter = (addr.lost_focus() || pw.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
                            ui.add_space(16.0);

                            let accent = ui.visuals().selection.bg_fill;
                            let connect = egui::Button::new(RichText::new("Connect").strong().color(egui::Color32::BLACK))
                                .fill(accent)
                                .min_size(egui::vec2(ui.available_width(), 34.0));
                            if ui.add_enabled(ServerAddress::parse(&self.address).is_some(), connect).clicked() || enter {
                                request = ServerAddress::parse(&self.address).map(|a| (a, self.password.clone()));
                            }
                        });

                        ui.add_space(12.0);
                        if busy {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Connecting…");
                            });
                        } else if !self.status.is_empty() {
                            ui.label(RichText::new(&self.status).color(ui.visuals().warn_fg_color));
                        }
                    });
                });
            });
        });
        request
    }
}
