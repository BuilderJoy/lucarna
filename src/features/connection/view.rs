use eframe::egui::{self, Align, ColorImage, Key, Layout, RichText, TextureHandle, TextureOptions};

use super::saved::{self, SavedConnection};
use super::ServerAddress;

/// Start screen: saved connections on the left, a form to connect, add or edit on the right.
pub struct Dashboard {
    saved: Vec<SavedConnection>,
    /// Index in `saved` of the connection loaded into the form.
    selected: Option<usize>,
    name: String,
    address: String,
    password: String,
    logo: Option<TextureHandle>,
    pub status: String,
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            saved: saved::load(),
            selected: None,
            name: String::new(),
            address: String::new(),
            password: String::new(),
            logo: None,
            status: String::new(),
        }
    }
}

type Request = Option<(ServerAddress, String)>;

impl Dashboard {
    /// Returns the target when the user asks to connect.
    pub fn show(&mut self, ui: &mut egui::Ui, busy: bool) -> Request {
        let mut request = None;
        egui::CentralPanel::default().show(ui, |ui| {
            let width = ui.available_width().min(820.0);
            ui.horizontal_top(|ui| {
                ui.add_space((ui.available_width() - width) / 2.0);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space(32.0);
                    self.header(ui);
                    ui.add_space(24.0);
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.columns(2, |cols| {
                            request = self.list(&mut cols[0]).or(self.form(&mut cols[1]));
                        });
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
        request
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let logo = self.logo.get_or_insert_with(|| {
            let icon = eframe::icon_data::from_png_bytes(crate::LOGO_PNG).expect("valid logo.png");
            let image = ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            ui.ctx().load_texture("logo", image, TextureOptions::LINEAR)
        });
        ui.horizontal(|ui| {
            ui.image((logo.id(), egui::vec2(56.0, 56.0)));
            ui.vertical(|ui| {
                ui.label(RichText::new("Lucarna").size(28.0).strong().color(ui.visuals().selection.bg_fill));
                ui.label(RichText::new("VNC viewer").weak());
            });
        });
    }

    fn list(&mut self, ui: &mut egui::Ui) -> Request {
        ui.label(RichText::new("Saved connections").size(16.0).strong());
        ui.add_space(8.0);
        if self.saved.is_empty() {
            ui.label(RichText::new("Nothing saved yet. Fill in the form and press Save.").weak());
            return None;
        }
        let (mut request, mut edit) = (None, None);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, c) in self.saved.iter().enumerate() {
                let mut card = egui::Frame::group(ui.style()).corner_radius(8).inner_margin(10);
                if self.selected == Some(i) {
                    card = card.stroke(egui::Stroke::new(1.0, ui.visuals().selection.bg_fill));
                }
                card.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&c.name).strong());
                            ui.label(RichText::new(c.address.as_str()).weak());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Connect").clicked() {
                                request = Some((c.address.clone(), saved::password(&c.address).unwrap_or_default()));
                            }
                            if ui.button("Edit").clicked() {
                                edit = Some(i);
                            }
                        });
                    });
                });
                ui.add_space(6.0);
            }
        });
        if let Some(i) = edit {
            let c = &self.saved[i];
            (self.name, self.address) = (c.name.clone(), c.address.as_str().into());
            self.password = saved::password(&c.address).unwrap_or_default();
            self.selected = Some(i);
            self.status.clear();
        }
        request
    }

    fn form(&mut self, ui: &mut egui::Ui) -> Request {
        let mut request = None;
        let card = egui::Frame::window(ui.style()).corner_radius(12).inner_margin(20);
        card.show(ui, |ui| {
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                let title = if self.selected.is_some() { "Edit connection" } else { "New connection" };
                ui.label(RichText::new(title).size(16.0).strong());
                ui.add_space(12.0);

                let field = |ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str, secret: bool| {
                    ui.label(label);
                    let r = ui.add(
                        egui::TextEdit::singleline(text)
                            .hint_text(hint)
                            .password(secret)
                            .desired_width(f32::INFINITY)
                            .margin(egui::vec2(8.0, 6.0)),
                    );
                    ui.add_space(8.0);
                    r
                };
                field(ui, "Name", &mut self.name, "optional, e.g. Office PC", false);
                let addr = field(ui, "Server", &mut self.address, "host, host:display or host::port", false);
                let pw = field(ui, "Password", &mut self.password, "", true);
                let enter = (addr.lost_focus() || pw.lost_focus()) && ui.input(|i| i.key_pressed(Key::Enter));
                let target = ServerAddress::parse(&self.address);
                ui.add_space(8.0);

                let connect = egui::Button::new(RichText::new("Connect").strong().color(egui::Color32::BLACK))
                    .fill(ui.visuals().selection.bg_fill)
                    .min_size(egui::vec2(ui.available_width(), 34.0));
                if ui.add_enabled(target.is_some(), connect).clicked() || enter {
                    request = target.clone().map(|a| (a, self.password.clone()));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(target.is_some(), egui::Button::new("Save")).clicked() {
                        self.save(target.clone().unwrap());
                    }
                    if self.selected.is_some() {
                        if ui.button("Delete").clicked() {
                            self.delete();
                        }
                        if ui.button("New").clicked() {
                            self.clear();
                        }
                    }
                });
            });
        });
        request
    }

    /// Adds or updates the connection; the address identifies it, so saving an existing address replaces it.
    fn save(&mut self, address: ServerAddress) {
        let mut errors = Vec::new();
        if let Some(old) = self.selected.map(|i| self.saved.remove(i))
            && old.address != address
        {
            errors.extend(saved::set_password(&old.address, "").err());
        }
        self.saved.retain(|c| c.address != address);
        let name = self.name.trim();
        let name = if name.is_empty() { address.as_str().to_string() } else { name.to_string() };
        self.saved.push(SavedConnection { name, address: address.clone() });
        self.saved.sort_by_key(|c| c.name.to_lowercase());
        self.selected = self.saved.iter().position(|c| c.address == address);
        errors.extend(saved::set_password(&address, &self.password).err());
        errors.extend(saved::store(&self.saved).err());
        self.status = errors.join("\n");
    }

    fn delete(&mut self) {
        let Some(old) = self.selected.map(|i| self.saved.remove(i)) else { return };
        let errors: Vec<_> = [saved::set_password(&old.address, ""), saved::store(&self.saved)].into_iter().filter_map(Result::err).collect();
        self.clear();
        self.status = errors.join("\n");
    }

    fn clear(&mut self) {
        self.selected = None;
        self.name.clear();
        self.address.clear();
        self.password.clear();
        self.status.clear();
    }
}
