use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{
    self, Align, Color32, ColorImage, CornerRadius, FontId, Id, Key, Layout, Modifiers, Order, Pos2, Rect, RichText, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};

use super::saved::{self, SavedConnection, SessionOptions};
use super::{Credentials, ServerAddress};

#[derive(Clone)]
pub struct ConnectRequest {
    pub address: ServerAddress,
    pub creds: Credentials,
    pub options: SessionOptions,
}

#[derive(Clone, PartialEq)]
enum Filter {
    All,
    Favorites,
    Recent,
    Label(String),
}

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Grid,
    List,
}

#[derive(Clone, Copy, PartialEq)]
enum SortBy {
    Name,
    Recent,
    Address,
}

#[derive(Clone, Copy, PartialEq)]
enum PropertiesTab {
    General,
    Options,
}

struct PropertiesEditor {
    open: bool,
    /// `Some(i)` when editing `saved[i]`, `None` when creating a new connection.
    editing: Option<usize>,
    tab: PropertiesTab,
    name: String,
    address: String,
    user: String,
    password: String,
    show_password: bool,
    labels_input: String,
    favorite: bool,
    options: SessionOptions,
}

impl Default for PropertiesEditor {
    fn default() -> Self {
        Self {
            open: false,
            editing: None,
            tab: PropertiesTab::General,
            name: String::new(),
            address: String::new(),
            user: String::new(),
            password: String::new(),
            show_password: false,
            labels_input: String::new(),
            favorite: false,
            options: SessionOptions::default(),
        }
    }
}

enum Modal {
    Rename { index: usize, name: String },
    RenameLabel { old: String, new: String },
    NewLabel(String),
    ImportExport { import: bool, path: String },
}

enum TileAction {
    Select(usize),
    Connect(usize, Option<bool>),
    ToggleFavorite(usize),
    ToggleLabel(usize, String),
    AddLabel(usize, String),
    Rename(usize),
    Duplicate(usize),
    ClearPreview(usize),
    Delete(usize),
    Properties(usize),
}

/// RealVNC-inspired Address Book: sidebar with labels/groups, search & quick-connect omni-bar,
/// desktop preview tiles (Grid or List), context menus, and per-connection Properties dialog.
pub struct Dashboard {
    saved: Vec<SavedConnection>,
    selected: Option<usize>,
    filter: Filter,
    view_mode: ViewMode,
    sort_by: SortBy,
    omni: String,
    extra_labels: Vec<String>,
    new_tag_inline: String,
    editor: PropertiesEditor,
    modal: Option<Modal>,
    logo: Option<TextureHandle>,
    previews: HashMap<String, Option<TextureHandle>>,
    pub status: String,
}

impl Default for Dashboard {
    fn default() -> Self {
        let saved = saved::load();
        let open_new = saved.is_empty();
        let mut d = Self {
            saved,
            selected: None,
            filter: Filter::All,
            view_mode: ViewMode::Grid,
            sort_by: SortBy::Name,
            omni: String::new(),
            extra_labels: Vec::new(),
            new_tag_inline: String::new(),
            editor: PropertiesEditor::default(),
            modal: None,
            logo: None,
            previews: HashMap::new(),
            status: String::new(),
        };
        if open_new {
            d.open_new_connection(None);
        }
        d
    }
}

impl Dashboard {
    /// Updates the `last_connected` timestamp for any saved entry matching `address`.
    pub fn mark_connected(&mut self, address: &ServerAddress) {
        let now = now_secs();
        let mut changed = false;
        for c in &mut self.saved {
            if &c.address == address {
                c.last_connected = now;
                changed = true;
            }
        }
        if changed {
            let _ = saved::store(&self.saved);
        }
    }

    /// Saves a new desktop thumbnail for `address` and refreshes the in-memory texture cache.
    pub fn update_preview(&mut self, ctx: &egui::Context, address: &ServerAddress, width: u16, height: u16, rgba: &[u8]) {
        let _ = saved::save_preview(address, width, height, rgba);
        let img = ColorImage::from_rgba_unmultiplied([usize::from(width), usize::from(height)], rgba);
        let tex = ctx.load_texture(format!("preview:{}", address.as_str()), img, TextureOptions::LINEAR);
        self.previews.insert(address.as_str().to_string(), Some(tex));
    }

    /// Draws the Address Book and returns a `ConnectRequest` when the user initiates a connection.
    pub fn show(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<ConnectRequest> {
        let ctx = ui.ctx().clone();
        let mut request = None;

        // Top omni-bar: logo, quick-connect / search bar, view controls, and + New button.
        let top_frame = egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(16, 10));
        egui::Panel::top("address_book_top").frame(top_frame).show(ui, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if let Some(r) = self.top_bar(ui) {
                    request = Some(r);
                }
            });
        });

        // Bottom status bar: device count, shortcuts hint, and connection status/error.
        let bottom_frame = egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(16, 6));
        egui::Panel::bottom("address_book_bottom").frame(bottom_frame).show(ui, |ui| {
            self.bottom_bar(ui, busy);
        });

        // Left sidebar: Address book categories & user labels.
        let side_frame = egui::Frame::side_top_panel(ui.style())
            .fill(Color32::from_rgb(0x13, 0x1a, 0x28))
            .inner_margin(egui::Margin::symmetric(12, 12));
        egui::Panel::left("address_book_sidebar")
            .resizable(false)
            .exact_size(210.0)
            .frame(side_frame)
            .show(ui, |ui| {
                ui.add_enabled_ui(!busy, |ui| self.sidebar(ui));
            });

        // Main canvas: connection tiles (Grid) or rows (List).
        let center_frame = egui::Frame::central_panel(ui.style())
            .fill(Color32::from_rgb(0x0e, 0x14, 0x20))
            .inner_margin(egui::Margin::symmetric(20, 16));
        egui::CentralPanel::default().frame(center_frame).show(ui, |ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if let Some(r) = self.canvas(ui) {
                    request = Some(r);
                }
            });
        });

        // Keyboard shortcuts when no modal or text input is active.
        if !busy
            && !self.editor.open
            && self.modal.is_none()
            && !ctx.egui_wants_keyboard_input()
            && let Some(r) = self.handle_shortcuts(&ctx)
        {
            request = Some(r);
        }

        // Properties dialog and quick modals.
        if !busy {
            if let Some(r) = self.show_properties_window(&ctx) {
                request = Some(r);
            }
            self.show_modal(&ctx);
        }

        request
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) -> Option<ConnectRequest> {
        let mut request = None;
        let accent = ui.visuals().selection.bg_fill;
        let logo_id = self
            .logo
            .get_or_insert_with(|| {
                let icon = eframe::icon_data::from_png_bytes(crate::LOGO_PNG).expect("valid logo.png");
                let image = ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
                ui.ctx().load_texture("logo", image, TextureOptions::LINEAR)
            })
            .id();

        ui.horizontal(|ui| {
            ui.image((logo_id, egui::vec2(32.0, 32.0)));
            ui.label(RichText::new("Lucarna").size(20.0).strong().color(accent));
            ui.add_space(12.0);

            // Omni search & quick-connect bar.
            let right_controls_width = 330.0;
            let omni_width = (ui.available_width() - right_controls_width).clamp(180.0, 520.0);
            let parsed_addr = ServerAddress::parse(&self.omni);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.omni)
                    .hint_text("🔍 Enter a VNC Server address or search connections…")
                    .desired_width(omni_width)
                    .margin(egui::vec2(10.0, 6.0)),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if !self.omni.is_empty() && ui.small_button("✕").on_hover_text("Clear search").clicked() {
                self.omni.clear();
            }
            if let Some(addr) = parsed_addr {
                let connect_btn = egui::Button::new(RichText::new("Connect ↵").strong().color(Color32::BLACK)).fill(accent);
                if ui.add(connect_btn).on_hover_text(format!("Connect directly to {}", addr.as_str())).clicked() || enter {
                    request = Some(self.quick_connect_request(addr.clone()));
                }
                if ui.button("+ Save").on_hover_text("Save this address to your Address Book").clicked() {
                    self.open_new_connection(Some(addr.as_str()));
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button("⋯", |ui| {
                    if ui.button("➕ New connection…\t⌘N").clicked() {
                        self.open_new_connection(None);
                        ui.close();
                    }
                    if ui.button("🏷 New label…").clicked() {
                        self.modal = Some(Modal::NewLabel(String::new()));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("📥 Import connections…").clicked() {
                        let default_path = default_backup_path();
                        self.modal = Some(Modal::ImportExport {
                            import: true,
                            path: default_path,
                        });
                        ui.close();
                    }
                    if ui.button("📤 Export connections…").clicked() {
                        let default_path = default_backup_path();
                        self.modal = Some(Modal::ImportExport {
                            import: false,
                            path: default_path,
                        });
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Address book actions");

                let new_btn = egui::Button::new(RichText::new("+ New").strong().color(Color32::BLACK)).fill(accent);
                if ui.add(new_btn).on_hover_text("Create a new connection (⌘N / Ctrl+N)").clicked() {
                    self.open_new_connection(None);
                }

                ui.separator();

                let sort_label = match self.sort_by {
                    SortBy::Name => "Sort: Name",
                    SortBy::Recent => "Sort: Recent",
                    SortBy::Address => "Sort: Server",
                };
                ui.menu_button(sort_label, |ui| {
                    if ui.selectable_label(self.sort_by == SortBy::Name, "Name (A–Z)").clicked() {
                        self.sort_by = SortBy::Name;
                        ui.close();
                    }
                    if ui.selectable_label(self.sort_by == SortBy::Recent, "Recently connected").clicked() {
                        self.sort_by = SortBy::Recent;
                        ui.close();
                    }
                    if ui.selectable_label(self.sort_by == SortBy::Address, "Server address").clicked() {
                        self.sort_by = SortBy::Address;
                        ui.close();
                    }
                });

                if ui.selectable_label(self.view_mode == ViewMode::List, "☰ List").on_hover_text("List view").clicked() {
                    self.view_mode = ViewMode::List;
                }
                if ui
                    .selectable_label(self.view_mode == ViewMode::Grid, "⊞ Grid")
                    .on_hover_text("Thumbnail grid view")
                    .clicked()
                {
                    self.view_mode = ViewMode::Grid;
                }
            });
        });

        request
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("ADDRESS BOOK").size(11.0).strong().weak());
        ui.add_space(6.0);

        let total = self.saved.len();
        let favs = self.saved.iter().filter(|c| c.favorite).count();
        let recent = self.saved.iter().filter(|c| c.last_connected > 0).count();

        if sidebar_item(ui, "⊞  All connections", total, self.filter == Filter::All).clicked() {
            self.filter = Filter::All;
        }
        if sidebar_item(ui, "★  Favorites", favs, self.filter == Filter::Favorites).clicked() {
            self.filter = Filter::Favorites;
        }
        if sidebar_item(ui, "🕒  Recent", recent, self.filter == Filter::Recent).clicked() {
            self.filter = Filter::Recent;
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("LABELS").size(11.0).strong().weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("+").on_hover_text("Create a new label").clicked() {
                    self.modal = Some(Modal::NewLabel(String::new()));
                }
            });
        });
        ui.add_space(6.0);

        let labels = self.all_labels();
        if labels.is_empty() {
            ui.label(
                RichText::new("No labels yet.\nRight-click a desktop or click + to organize by location, team or role.")
                    .size(12.0)
                    .weak(),
            );
        } else {
            let mut label_to_rename = None;
            let mut label_to_delete = None;
            egui::ScrollArea::vertical().id_salt("labels_scroll").show(ui, |ui| {
                for (label, count) in labels {
                    let active = matches!(&self.filter, Filter::Label(l) if l.eq_ignore_ascii_case(&label));
                    let resp = sidebar_item(ui, &format!("🏷  {label}"), count, active);
                    if resp.clicked() {
                        self.filter = Filter::Label(label.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button("✏ Rename label…").clicked() {
                            label_to_rename = Some(label.clone());
                            ui.close();
                        }
                        if ui.button("🗑 Delete label").clicked() {
                            label_to_delete = Some(label.clone());
                            ui.close();
                        }
                    });
                }
            });
            if let Some(l) = label_to_rename {
                self.modal = Some(Modal::RenameLabel { old: l.clone(), new: l });
            }
            if let Some(l) = label_to_delete {
                self.remove_label_everywhere(&l);
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) -> Option<ConnectRequest> {
        let indices = self.visible_indices();
        let active_title = match &self.filter {
            Filter::All => "All connections".to_string(),
            Filter::Favorites => "Favorites".to_string(),
            Filter::Recent => "Recently connected".to_string(),
            Filter::Label(l) => format!("🏷 {l}"),
        };

        ui.horizontal(|ui| {
            ui.label(RichText::new(&active_title).size(18.0).strong());
            ui.label(RichText::new(format!("({})", indices.len())).weak());
            if let Filter::Label(l) = &self.filter {
                let l = l.clone();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.small_button("+ Add connection here").clicked() {
                        self.open_new_connection(None);
                        self.editor.labels_input = l;
                    }
                });
            }
        });
        ui.add_space(12.0);

        if indices.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(64.0);
                ui.label(RichText::new("No connections in this view").size(16.0).strong());
                ui.add_space(4.0);
                ui.label(RichText::new("Add a remote computer or type an address in the top bar to connect.").weak());
                ui.add_space(12.0);
                let btn = egui::Button::new(RichText::new("+ New connection").strong().color(Color32::BLACK))
                    .fill(ui.visuals().selection.bg_fill)
                    .min_size(egui::vec2(160.0, 34.0));
                if ui.add(btn).clicked() {
                    self.open_new_connection(None);
                }
            });
            return None;
        }

        let known_labels: Vec<String> = self.all_labels().into_iter().map(|(l, _)| l).collect();
        let mut action = None;

        egui::ScrollArea::vertical()
            .id_salt("canvas_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.view_mode {
                ViewMode::Grid => {
                    let card_w = 228.0;
                    let card_h = 204.0;
                    let spacing = 14.0;
                    let avail = ui.available_width().max(card_w);
                    let cols = ((avail + spacing) / (card_w + spacing)).floor().max(1.0) as usize;
                    for row in indices.chunks(cols) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = spacing;
                            for &idx in row {
                                if let Some(a) = self.draw_tile(ui, idx, Vec2::new(card_w, card_h), &known_labels) {
                                    action = Some(a);
                                }
                            }
                        });
                        ui.add_space(spacing);
                    }
                }
                ViewMode::List => {
                    for &idx in &indices {
                        if let Some(a) = self.draw_list_row(ui, idx, &known_labels) {
                            action = Some(a);
                        }
                        ui.add_space(6.0);
                    }
                }
            });

        action.and_then(|a| self.apply_action(a))
    }

    fn draw_tile(&mut self, ui: &mut egui::Ui, idx: usize, size: Vec2, known_labels: &[String]) -> Option<TileAction> {
        let mut action = None;
        let is_selected = self.selected == Some(idx);
        let c = self.saved[idx].clone();
        let has_preview = self.ensure_preview(ui.ctx(), &c.address).is_some();

        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let hovered = resp.hovered();
        let accent = ui.visuals().selection.bg_fill;

        let bg = if is_selected {
            Color32::from_rgb(0x1f, 0x2b, 0x42)
        } else if hovered {
            Color32::from_rgb(0x1a, 0x24, 0x38)
        } else {
            Color32::from_rgb(0x15, 0x1d, 0x2d)
        };
        let stroke = if is_selected {
            Stroke::new(2.0, accent)
        } else if hovered {
            Stroke::new(1.0, accent.gamma_multiply(0.6))
        } else {
            Stroke::new(1.0, Color32::from_rgb(0x26, 0x32, 0x48))
        };
        ui.painter().rect(rect, CornerRadius::same(10), bg, stroke, StrokeKind::Inside);

        // Preview screen inside top portion of the card (16:10 ratio).
        let preview_rect = Rect::from_min_size(rect.min + Vec2::new(10.0, 10.0), Vec2::new(size.x - 20.0, 122.0));
        self.paint_desktop_preview(ui, preview_rect, &c);

        // Text metadata below the preview.
        let text_left = rect.left() + 12.0;
        let text_right = rect.right() - 12.0;
        let title_y = preview_rect.bottom() + 8.0;

        ui.painter().text(
            Pos2::new(text_left, title_y),
            egui::Align2::LEFT_TOP,
            truncate_str(&c.name, 22),
            FontId::proportional(14.5),
            Color32::WHITE,
        );

        let who = if c.user.is_empty() {
            c.address.as_str().to_string()
        } else {
            format!("{}@{}", c.user, c.address.as_str())
        };
        ui.painter().text(
            Pos2::new(text_left, title_y + 19.0),
            egui::Align2::LEFT_TOP,
            truncate_str(&who, 28),
            FontId::monospace(11.0),
            Color32::from_rgb(0x94, 0xa3, 0xb8),
        );

        // Bottom row: labels on the left, relative time on the right.
        let footer_y = rect.bottom() - 22.0;
        let mut tag_x = text_left;
        for label in c.labels.iter().take(2) {
            let tag_text = truncate_str(label, 10);
            let pill_w = (tag_text.len() as f32 * 6.5 + 14.0).min(82.0);
            let pill_rect = Rect::from_min_size(Pos2::new(tag_x, footer_y), Vec2::new(pill_w, 16.0));
            ui.painter().rect_filled(pill_rect, CornerRadius::same(4), Color32::from_rgb(0x24, 0x32, 0x4a));
            ui.painter()
                .text(pill_rect.center(), egui::Align2::CENTER_CENTER, tag_text, FontId::proportional(10.5), accent);
            tag_x += pill_w + 4.0;
        }
        if c.labels.len() > 2 {
            ui.painter().text(
                Pos2::new(tag_x + 2.0, footer_y + 8.0),
                egui::Align2::LEFT_CENTER,
                format!("+{}", c.labels.len() - 2),
                FontId::proportional(10.0),
                Color32::from_rgb(0x94, 0xa3, 0xb8),
            );
        }

        let rel = relative_time(c.last_connected);
        ui.painter().text(
            Pos2::new(text_right, footer_y + 8.0),
            egui::Align2::RIGHT_CENTER,
            rel,
            FontId::proportional(10.5),
            Color32::from_rgb(0x64, 0x74, 0x8b),
        );

        // Favorite star button in top-right of the title row.
        let fav_rect = Rect::from_min_size(Pos2::new(rect.right() - 52.0, title_y - 2.0), Vec2::new(20.0, 20.0));
        let fav_resp = ui.interact(fav_rect, ui.scope_id().with(("fav", idx)), Sense::click());
        let star_color = if c.favorite {
            accent
        } else if fav_resp.hovered() {
            Color32::WHITE
        } else {
            Color32::from_rgb(0x64, 0x74, 0x8b)
        };
        ui.painter().text(
            fav_rect.center(),
            egui::Align2::CENTER_CENTER,
            if c.favorite { "★" } else { "☆" },
            FontId::proportional(14.0),
            star_color,
        );
        if fav_resp.on_hover_text("Toggle favorite").clicked() {
            return Some(TileAction::ToggleFavorite(idx));
        }

        // Context menu button "⋯" in the title row.
        let dots_rect = Rect::from_min_size(Pos2::new(rect.right() - 30.0, title_y - 2.0), Vec2::new(20.0, 20.0));
        let dots_resp = ui.interact(dots_rect, ui.scope_id().with(("dots", idx)), Sense::click());
        ui.painter().text(
            dots_rect.center(),
            egui::Align2::CENTER_CENTER,
            "⋯",
            FontId::proportional(15.0),
            if dots_resp.hovered() {
                Color32::WHITE
            } else {
                Color32::from_rgb(0x94, 0xa3, 0xb8)
            },
        );
        if dots_resp.clicked() {
            self.selected = Some(idx);
            self.open_properties(idx);
        }
        dots_resp.on_hover_text("Properties (Alt+Enter) · Right-click card for all options");

        if resp.double_clicked() {
            action = Some(TileAction::Connect(idx, None));
        } else if resp.clicked() {
            action = Some(TileAction::Select(idx));
        }

        resp.on_hover_text("Double-click to connect · Right-click for options").context_menu(|ui| {
            if let Some(a) = Self::connection_context_menu(ui, idx, &c, has_preview, known_labels, &mut self.new_tag_inline) {
                action = Some(a);
            }
        });

        action
    }

    fn draw_list_row(&mut self, ui: &mut egui::Ui, idx: usize, known_labels: &[String]) -> Option<TileAction> {
        let mut action = None;
        let is_selected = self.selected == Some(idx);
        let c = self.saved[idx].clone();
        let has_preview = self.ensure_preview(ui.ctx(), &c.address).is_some();
        let accent = ui.visuals().selection.bg_fill;

        let row_h = 58.0;
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
        let hovered = resp.hovered();

        let bg = if is_selected {
            Color32::from_rgb(0x1f, 0x2b, 0x42)
        } else if hovered {
            Color32::from_rgb(0x1a, 0x24, 0x38)
        } else {
            Color32::from_rgb(0x15, 0x1d, 0x2d)
        };
        let stroke = if is_selected {
            Stroke::new(1.5, accent)
        } else {
            Stroke::new(1.0, Color32::from_rgb(0x26, 0x32, 0x48))
        };
        ui.painter().rect(rect, CornerRadius::same(8), bg, stroke, StrokeKind::Inside);

        // Mini 16:10 desktop preview on the left.
        let prev_rect = Rect::from_min_size(rect.min + Vec2::new(8.0, 7.0), Vec2::new(70.0, 44.0));
        self.paint_desktop_preview(ui, prev_rect, &c);

        let text_x = prev_rect.right() + 12.0;
        ui.painter().text(
            Pos2::new(text_x, rect.top() + 11.0),
            egui::Align2::LEFT_TOP,
            &c.name,
            FontId::proportional(14.5),
            Color32::WHITE,
        );
        let who = if c.user.is_empty() {
            c.address.as_str().to_string()
        } else {
            format!("{} @ {}", c.user, c.address.as_str())
        };
        ui.painter().text(
            Pos2::new(text_x, rect.top() + 31.0),
            egui::Align2::LEFT_TOP,
            who,
            FontId::monospace(11.5),
            Color32::from_rgb(0x94, 0xa3, 0xb8),
        );

        // Right-side controls inside the row.
        let controls_rect = Rect::from_min_max(Pos2::new(rect.right() - 280.0, rect.top()), rect.max);
        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(controls_rect).layout(Layout::right_to_left(Align::Center)));
        child_ui.add_space(10.0);
        if child_ui.button("Properties").clicked() {
            action = Some(TileAction::Properties(idx));
        }
        let connect_btn = egui::Button::new(RichText::new("Connect").strong().color(Color32::BLACK)).fill(accent);
        if child_ui.add(connect_btn).clicked() {
            action = Some(TileAction::Connect(idx, None));
        }
        if child_ui
            .selectable_label(c.favorite, if c.favorite { "★" } else { "☆" })
            .on_hover_text("Toggle favorite")
            .clicked()
        {
            action = Some(TileAction::ToggleFavorite(idx));
        }
        for label in c.labels.iter().rev() {
            child_ui.label(RichText::new(format!("🏷 {label}")).size(11.0).color(accent));
        }

        if resp.double_clicked() {
            action = Some(TileAction::Connect(idx, None));
        } else if resp.clicked() && action.is_none() {
            action = Some(TileAction::Select(idx));
        }

        resp.context_menu(|ui| {
            if let Some(a) = Self::connection_context_menu(ui, idx, &c, has_preview, known_labels, &mut self.new_tag_inline) {
                action = Some(a);
            }
        });

        action
    }

    fn paint_desktop_preview(&mut self, ui: &mut egui::Ui, rect: Rect, c: &SavedConnection) {
        let tex = self.ensure_preview(ui.ctx(), &c.address);
        let p = ui.painter();

        // Outer monitor bezel.
        p.rect(
            rect,
            CornerRadius::same(6),
            Color32::from_rgb(0x09, 0x0d, 0x16),
            Stroke::new(1.0, Color32::from_rgb(0x28, 0x35, 0x4c)),
            StrokeKind::Inside,
        );
        let screen = rect.shrink(3.0);

        if let Some(tex) = tex {
            // Aspect-fit the captured remote desktop inside `screen`.
            let [tw, th] = tex.size();
            let scale = (screen.width() / tw as f32).min(screen.height() / th as f32);
            let img_size = Vec2::new(tw as f32 * scale, th as f32 * scale);
            let img_rect = Rect::from_center_size(screen.center(), img_size);
            p.image(tex.id(), img_rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else {
            // Stylized remote desktop illustration when no live snapshot has been saved yet.
            p.rect_filled(screen, CornerRadius::same(4), Color32::from_rgb(0x11, 0x1b, 0x2e));
            if screen.width() > 100.0 {
                let win_w = screen.width() * 0.62;
                let win_h = screen.height() * 0.56;
                let win_rect = Rect::from_center_size(screen.center() - Vec2::new(0.0, 4.0), Vec2::new(win_w, win_h));
                p.rect(
                    win_rect,
                    CornerRadius::same(4),
                    Color32::from_rgb(0x19, 0x26, 0x3e),
                    Stroke::new(1.0, Color32::from_rgb(0x33, 0x46, 0x66)),
                    StrokeKind::Inside,
                );
                let bar_rect = Rect::from_min_size(win_rect.min, Vec2::new(win_w, 10.0));
                p.rect_filled(bar_rect, CornerRadius::same(4), Color32::from_rgb(0x24, 0x34, 0x52));
                p.circle_filled(Pos2::new(bar_rect.left() + 6.0, bar_rect.center().y), 2.0, Color32::from_rgb(0xff, 0xbe, 0x3d));
                p.circle_filled(Pos2::new(bar_rect.left() + 12.0, bar_rect.center().y), 2.0, Color32::from_rgb(0x64, 0x74, 0x8b));
                p.text(
                    win_rect.center() + Vec2::new(0.0, 3.0),
                    egui::Align2::CENTER_CENTER,
                    initials(&c.name),
                    FontId::proportional(16.0),
                    Color32::from_rgb(0xff, 0xbe, 0x3d),
                );
                // Taskbar strip at the bottom of the simulated screen.
                let taskbar = Rect::from_min_max(Pos2::new(screen.left() + 4.0, screen.bottom() - 8.0), screen.max - Vec2::splat(2.0));
                p.rect_filled(taskbar, CornerRadius::same(2), Color32::from_rgb(0x0b, 0x11, 0x1e));
            } else {
                p.text(
                    screen.center(),
                    egui::Align2::CENTER_CENTER,
                    initials(&c.name),
                    FontId::proportional(12.0),
                    Color32::from_rgb(0xff, 0xbe, 0x3d),
                );
            }
        }

        // Option badges overlaid on the bottom-left of the screen if non-default.
        if screen.width() > 100.0 {
            let mut bx = screen.left() + 5.0;
            let by = screen.bottom() - 17.0;
            for (flag, label) in [
                (c.options.view_only, "VIEW ONLY"),
                (c.options.scale_1_1, "1:1"),
                (c.options.fullscreen, "FULL"),
            ] {
                if flag {
                    let bw = label.len() as f32 * 5.8 + 8.0;
                    let brect = Rect::from_min_size(Pos2::new(bx, by), Vec2::new(bw, 13.0));
                    p.rect_filled(brect, CornerRadius::same(3), Color32::from_black_alpha(190));
                    p.text(
                        brect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        FontId::proportional(9.0),
                        Color32::from_rgb(0xe2, 0xe8, 0xf0),
                    );
                    bx += bw + 4.0;
                }
            }
        }
    }

    fn connection_context_menu(
        ui: &mut egui::Ui,
        idx: usize,
        c: &SavedConnection,
        has_preview: bool,
        known_labels: &[String],
        new_tag_inline: &mut String,
    ) -> Option<TileAction> {
        let mut action = None;
        ui.set_min_width(190.0);

        if ui.button(RichText::new("▶  Connect\tEnter").strong()).clicked() {
            action = Some(TileAction::Connect(idx, None));
            ui.close();
        }
        if ui.button("👁  Connect (View only)").clicked() {
            action = Some(TileAction::Connect(idx, Some(true)));
            ui.close();
        }
        ui.separator();

        let fav_label = if c.favorite {
            "☆  Remove from Favorites"
        } else {
            "★  Add to Favorites"
        };
        if ui.button(fav_label).clicked() {
            action = Some(TileAction::ToggleFavorite(idx));
            ui.close();
        }

        ui.menu_button("🏷  Labels", |ui| {
            ui.set_min_width(160.0);
            for label in known_labels {
                let mut checked = c.labels.iter().any(|l| l.eq_ignore_ascii_case(label));
                if ui.checkbox(&mut checked, label).clicked() {
                    action = Some(TileAction::ToggleLabel(idx, label.clone()));
                }
            }
            if !known_labels.is_empty() {
                ui.separator();
            }
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(new_tag_inline).hint_text("New label…").desired_width(105.0));
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui.small_button("+").clicked() || enter) && !new_tag_inline.trim().is_empty() {
                    action = Some(TileAction::AddLabel(idx, new_tag_inline.trim().to_string()));
                    new_tag_inline.clear();
                    ui.close();
                }
            });
        });

        ui.separator();
        if ui.button("✏  Rename…\tF2").clicked() {
            action = Some(TileAction::Rename(idx));
            ui.close();
        }
        let dup_shortcut = if cfg!(target_os = "macos") { "⌘D" } else { "Ctrl+D" };
        if ui.button(format!("⧉  Duplicate\t{dup_shortcut}")).clicked() {
            action = Some(TileAction::Duplicate(idx));
            ui.close();
        }
        if ui.add_enabled(has_preview, egui::Button::new("🖼  Clear desktop preview")).clicked() {
            action = Some(TileAction::ClearPreview(idx));
            ui.close();
        }
        if ui.button("🗑  Delete\t⌫").clicked() {
            action = Some(TileAction::Delete(idx));
            ui.close();
        }
        ui.separator();
        if ui.button("⚙  Properties…\tAlt+Enter").clicked() {
            action = Some(TileAction::Properties(idx));
            ui.close();
        }

        action
    }

    fn apply_action(&mut self, action: TileAction) -> Option<ConnectRequest> {
        match action {
            TileAction::Select(idx) => {
                self.selected = Some(idx);
                None
            }
            TileAction::Connect(idx, force_view_only) => {
                self.selected = Some(idx);
                let c = self.saved.get(idx)?.clone();
                let mut options = c.options.clone();
                if let Some(vo) = force_view_only {
                    options.view_only = vo;
                }
                let creds = Credentials {
                    user: c.user,
                    password: saved::password(&c.address).unwrap_or_default(),
                };
                Some(ConnectRequest {
                    address: c.address,
                    creds,
                    options,
                })
            }
            TileAction::ToggleFavorite(idx) => {
                if let Some(c) = self.saved.get_mut(idx) {
                    c.favorite = !c.favorite;
                    self.persist();
                }
                None
            }
            TileAction::ToggleLabel(idx, label) => {
                if let Some(c) = self.saved.get_mut(idx) {
                    if let Some(pos) = c.labels.iter().position(|l| l.eq_ignore_ascii_case(&label)) {
                        c.labels.remove(pos);
                    } else {
                        c.labels.push(label);
                    }
                    self.persist();
                }
                None
            }
            TileAction::AddLabel(idx, label) => {
                let clean = label.replace(['\t', '\n', '\r', ','], " ").trim().to_string();
                if !clean.is_empty()
                    && let Some(c) = self.saved.get_mut(idx)
                    && !c.labels.iter().any(|l| l.eq_ignore_ascii_case(&clean))
                {
                    c.labels.push(clean);
                    self.persist();
                }
                None
            }
            TileAction::Rename(idx) => {
                if let Some(c) = self.saved.get(idx) {
                    self.selected = Some(idx);
                    self.modal = Some(Modal::Rename {
                        index: idx,
                        name: c.name.clone(),
                    });
                }
                None
            }
            TileAction::Duplicate(idx) => {
                self.duplicate_connection(idx);
                None
            }
            TileAction::ClearPreview(idx) => {
                if let Some(c) = self.saved.get(idx) {
                    saved::delete_preview(&c.address);
                    self.previews.insert(c.address.as_str().to_string(), None);
                }
                None
            }
            TileAction::Delete(idx) => {
                self.delete_connection(idx);
                None
            }
            TileAction::Properties(idx) => {
                self.selected = Some(idx);
                self.open_properties(idx);
                None
            }
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) -> Option<ConnectRequest> {
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.open_new_connection(None);
            return None;
        }
        let idx = self.selected?;
        if idx >= self.saved.len() {
            return None;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, Key::Enter)) {
            self.open_properties(idx);
            return None;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2)) {
            return self.apply_action(TileAction::Rename(idx));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::D)) {
            return self.apply_action(TileAction::Duplicate(idx));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
            return self.apply_action(TileAction::Delete(idx));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
            return self.apply_action(TileAction::Connect(idx, None));
        }
        None
    }

    fn show_properties_window(&mut self, ctx: &egui::Context) -> Option<ConnectRequest> {
        if !self.editor.open {
            return None;
        }
        let mut request = None;
        let mut keep_open = self.editor.open;
        let title = if self.editor.editing.is_some() {
            "Connection Properties"
        } else {
            "New Connection"
        };
        let known_labels: Vec<String> = self.all_labels().into_iter().map(|(l, _)| l).collect();

        egui::Window::new(title)
            .id(Id::unique("connection_properties_window"))
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .default_width(420.0)
            .order(Order::Foreground)
            .show(ctx, |ui| {
                let accent = ui.visuals().selection.bg_fill;
                ui.set_min_width(400.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.editor.tab, PropertiesTab::General, "⚙ General");
                    ui.selectable_value(&mut self.editor.tab, PropertiesTab::Options, "🖥 Options & Display");
                });
                ui.separator();
                ui.add_space(6.0);

                match self.editor.tab {
                    PropertiesTab::General => {
                        let field = |ui: &mut Ui, label: &str, text: &mut String, hint: &str| {
                            ui.label(RichText::new(label).strong());
                            let r = ui.add(
                                egui::TextEdit::singleline(text)
                                    .hint_text(hint)
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::vec2(8.0, 6.0)),
                            );
                            ui.add_space(6.0);
                            r
                        };
                        let addr_resp = field(ui, "VNC Server", &mut self.editor.address, "host, host:display or host::port");
                        field(ui, "Friendly Name", &mut self.editor.name, "optional, e.g. Office PC");
                        field(ui, "Username", &mut self.editor.user, "optional, for MS-Logon (Windows login)");

                        ui.label(RichText::new("Password").strong());
                        ui.horizontal(|ui| {
                            let pw_w = (ui.available_width() - 44.0).max(120.0);
                            let show_pw = self.editor.show_password;
                            let pw_resp = ui.add(
                                egui::TextEdit::singleline(&mut self.editor.password)
                                    .password(!show_pw)
                                    .desired_width(pw_w)
                                    .margin(egui::vec2(8.0, 6.0)),
                            );
                            if ui.selectable_label(self.editor.show_password, "👁").on_hover_text("Show / hide password").clicked() {
                                self.editor.show_password = !self.editor.show_password;
                            }
                            if (addr_resp.lost_focus() || pw_resp.lost_focus())
                                && ui.input(|i| i.key_pressed(Key::Enter))
                                && let Some(target) = ServerAddress::parse(&self.editor.address)
                            {
                                let opts = self.editor.options.clone();
                                let creds = Credentials {
                                    user: self.editor.user.trim().into(),
                                    password: self.editor.password.clone(),
                                };
                                self.save_editor(target.clone());
                                request = Some(ConnectRequest {
                                    address: target,
                                    creds,
                                    options: opts,
                                });
                            }
                        });
                        ui.add_space(6.0);

                        field(ui, "Labels", &mut self.editor.labels_input, "comma-separated, e.g. Cambridge, London");
                        if !known_labels.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Quick toggle:").size(11.5).weak());
                                let current = saved::parse_labels(&self.editor.labels_input);
                                for k in &known_labels {
                                    let active = current.iter().any(|l| l.eq_ignore_ascii_case(k));
                                    if ui.selectable_label(active, format!("🏷 {k}")).clicked() {
                                        let mut next = current.clone();
                                        if let Some(p) = next.iter().position(|l| l.eq_ignore_ascii_case(k)) {
                                            next.remove(p);
                                        } else {
                                            next.push(k.clone());
                                        }
                                        self.editor.labels_input = next.join(", ");
                                    }
                                }
                            });
                            ui.add_space(4.0);
                        }

                        ui.checkbox(&mut self.editor.favorite, "★ Pin to Favorites");
                    }
                    PropertiesTab::Options => {
                        ui.label(RichText::new("Picture & Scaling").strong());
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.editor.options.scale_1_1, false, "Fit to window");
                            ui.selectable_value(&mut self.editor.options.scale_1_1, true, "Actual size (1:1)");
                        });
                        ui.add_space(10.0);

                        ui.label(RichText::new("Input & Session Behavior").strong());
                        ui.checkbox(&mut self.editor.options.view_only, "View only (do not send keyboard, mouse or clipboard)");
                        ui.checkbox(&mut self.editor.options.fullscreen, "Start session in full screen mode");
                        ui.checkbox(&mut self.editor.options.share_clipboard, "Share clipboard text with remote computer");
                        ui.add_space(10.0);

                        ui.label(RichText::new("Desktop Preview").strong());
                        ui.checkbox(&mut self.editor.options.save_preview, "Save desktop thumbnail preview when connected");
                        if let Some(target) = ServerAddress::parse(&self.editor.address) {
                            let has_prev = self.previews.get(target.as_str()).and_then(|t| t.as_ref()).is_some();
                            if ui.add_enabled(has_prev, egui::Button::new("Clear saved preview image")).clicked() {
                                saved::delete_preview(&target);
                                self.previews.insert(target.as_str().to_string(), None);
                            }
                        }
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);

                let target = ServerAddress::parse(&self.editor.address);
                let mut should_close = false;
                ui.horizontal(|ui| {
                    let connect_btn = egui::Button::new(RichText::new("Save & Connect").strong().color(Color32::BLACK)).fill(accent);
                    if ui.add_enabled(target.is_some(), connect_btn).clicked()
                        && let Some(t) = target.clone()
                    {
                        let opts = self.editor.options.clone();
                        let creds = Credentials {
                            user: self.editor.user.trim().into(),
                            password: self.editor.password.clone(),
                        };
                        self.save_editor(t.clone());
                        request = Some(ConnectRequest {
                            address: t,
                            creds,
                            options: opts,
                        });
                        should_close = true;
                    }
                    if ui.add_enabled(target.is_some(), egui::Button::new("Save")).clicked()
                        && let Some(t) = target.clone()
                    {
                        self.save_editor(t);
                        should_close = true;
                    }
                    if let Some(idx) = self.editor.editing
                        && ui.button("Delete").clicked()
                    {
                        self.delete_connection(idx);
                        should_close = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            should_close = true;
                        }
                    });
                });
                if should_close {
                    self.editor.open = false;
                }
            });

        if !keep_open {
            self.editor.open = false;
        }
        request
    }

    fn show_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = &mut self.modal else { return };
        let mut close = false;
        let mut apply_rename = None;
        let mut apply_label_rename = None;
        let mut apply_new_label = None;
        let mut apply_transfer = None;

        let title = match modal {
            Modal::Rename { .. } => "Rename connection",
            Modal::RenameLabel { .. } => "Rename label",
            Modal::NewLabel(_) => "New label",
            Modal::ImportExport { import: true, .. } => "Import connections",
            Modal::ImportExport { import: false, .. } => "Export connections",
        };

        egui::Window::new(title)
            .id(Id::unique("address_book_modal"))
            .collapsible(false)
            .resizable(false)
            .order(Order::Foreground)
            .show(ctx, |ui| match modal {
                Modal::Rename { index, name } => {
                    ui.label("Connection name:");
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(260.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if (ui.button("Rename").clicked() || enter) && !name.trim().is_empty() {
                            apply_rename = Some((*index, name.trim().to_string()));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    });
                }
                Modal::RenameLabel { old, new } => {
                    ui.label(format!("Rename label \"{old}\" to:"));
                    let r = ui.add(egui::TextEdit::singleline(new).desired_width(260.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if (ui.button("Rename").clicked() || enter) && !new.trim().is_empty() {
                            apply_label_rename = Some((old.clone(), new.trim().to_string()));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    });
                }
                Modal::NewLabel(name) => {
                    ui.label("Label name (e.g. Cambridge, London, Servers):");
                    let r = ui.add(egui::TextEdit::singleline(name).desired_width(260.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if (ui.button("Create").clicked() || enter) && !name.trim().is_empty() {
                            apply_new_label = Some(name.trim().replace([',', '\t'], " "));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    });
                }
                Modal::ImportExport { import, path } => {
                    ui.label(if *import {
                        "File path to import from:"
                    } else {
                        "File path to export to:"
                    });
                    let r = ui.add(egui::TextEdit::singleline(path).desired_width(320.0));
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let btn = if *import { "Import" } else { "Export" };
                        if (ui.button(btn).clicked() || enter) && !path.trim().is_empty() {
                            apply_transfer = Some((*import, PathBuf::from(path.trim())));
                            close = true;
                        }
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape)) {
                            close = true;
                        }
                    });
                }
            });

        if let Some((idx, new_name)) = apply_rename
            && let Some(c) = self.saved.get_mut(idx)
        {
            c.name = new_name;
            self.persist();
        }
        if let Some((old, new)) = apply_label_rename {
            self.rename_label_everywhere(&old, &new);
        }
        if let Some(label) = apply_new_label
            && !label.is_empty()
        {
            if !self.extra_labels.iter().any(|l| l.eq_ignore_ascii_case(&label)) {
                self.extra_labels.push(label.clone());
            }
            self.filter = Filter::Label(label);
        }
        if let Some((import, path)) = apply_transfer {
            if import {
                match saved::import_from(&path) {
                    Ok(list) => {
                        let count = list.len();
                        for item in list {
                            if !self.saved.iter().any(|c| c.address == item.address && c.name == item.name) {
                                self.saved.push(item);
                            }
                        }
                        self.persist();
                        self.status = format!("Imported {count} connection(s) from {}", path.display());
                    }
                    Err(e) => self.status = e,
                }
            } else {
                match saved::export_to(&path, &self.saved) {
                    Ok(()) => self.status = format!("Exported {} connection(s) to {}", self.saved.len(), path.display()),
                    Err(e) => self.status = e,
                }
            }
        }
        if close {
            self.modal = None;
        }
    }

    fn bottom_bar(&self, ui: &mut egui::Ui, busy: bool) {
        let count = self.visible_indices().len();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{count} device(s)")).strong().size(12.0));
            ui.separator();
            if busy {
                ui.spinner();
                ui.label("Connecting to remote desktop…");
            } else if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(ui.visuals().warn_fg_color));
            } else {
                let dup = if cfg!(target_os = "macos") { "⌘D" } else { "Ctrl+D" };
                ui.label(
                    RichText::new(format!(
                        "Double-click: Connect · Right-click: Options · F2: Rename · {dup}: Duplicate · Alt+Enter: Properties"
                    ))
                    .size(11.5)
                    .weak(),
                );
            }
        });
    }

    fn visible_indices(&self) -> Vec<usize> {
        let q = self.omni.trim().to_lowercase();
        let mut indices: Vec<usize> = self
            .saved
            .iter()
            .enumerate()
            .filter(|(_, c)| match &self.filter {
                Filter::All => true,
                Filter::Favorites => c.favorite,
                Filter::Recent => c.last_connected > 0,
                Filter::Label(l) => c.labels.iter().any(|cl| cl.eq_ignore_ascii_case(l)),
            })
            .filter(|(_, c)| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.address.as_str().to_lowercase().contains(&q)
                    || c.user.to_lowercase().contains(&q)
                    || c.labels.iter().any(|l| l.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect();

        let effective_sort = if self.filter == Filter::Recent {
            SortBy::Recent
        } else {
            self.sort_by
        };
        match effective_sort {
            SortBy::Name => indices.sort_by_key(|&i| (!self.saved[i].favorite, self.saved[i].name.to_lowercase())),
            SortBy::Recent => indices.sort_by_key(|&i| std::cmp::Reverse(self.saved[i].last_connected)),
            SortBy::Address => indices.sort_by_key(|&i| self.saved[i].address.as_str().to_lowercase()),
        }
        indices
    }

    fn all_labels(&self) -> Vec<(String, usize)> {
        let mut map: Vec<(String, usize)> = Vec::new();
        for l in &self.extra_labels {
            if !map.iter().any(|(k, _)| k.eq_ignore_ascii_case(l)) {
                map.push((l.clone(), 0));
            }
        }
        for c in &self.saved {
            for l in &c.labels {
                if let Some(entry) = map.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(l)) {
                    entry.1 += 1;
                } else {
                    map.push((l.clone(), 1));
                }
            }
        }
        map.sort_by_key(|(k, _)| k.to_lowercase());
        map
    }

    fn open_new_connection(&mut self, prefill_address: Option<&str>) {
        let initial_label = match &self.filter {
            Filter::Label(l) => l.clone(),
            _ => String::new(),
        };
        self.editor = PropertiesEditor {
            open: true,
            editing: None,
            tab: PropertiesTab::General,
            name: String::new(),
            address: prefill_address.unwrap_or_default().to_string(),
            user: String::new(),
            password: String::new(),
            show_password: false,
            labels_input: initial_label,
            favorite: self.filter == Filter::Favorites,
            options: SessionOptions::default(),
        };
    }

    fn open_properties(&mut self, idx: usize) {
        let Some(c) = self.saved.get(idx) else { return };
        self.editor = PropertiesEditor {
            open: true,
            editing: Some(idx),
            tab: PropertiesTab::General,
            name: c.name.clone(),
            address: c.address.as_str().to_string(),
            user: c.user.clone(),
            password: saved::password(&c.address).unwrap_or_default(),
            show_password: false,
            labels_input: c.labels.join(", "),
            favorite: c.favorite,
            options: c.options.clone(),
        };
    }

    fn save_editor(&mut self, address: ServerAddress) {
        let mut errors = Vec::new();
        let name = self.editor.name.trim();
        let name = if name.is_empty() {
            address.as_str().to_string()
        } else {
            name.to_string()
        };
        let labels = saved::parse_labels(&self.editor.labels_input);
        let user = self.editor.user.trim().to_string();
        let favorite = self.editor.favorite;
        let options = self.editor.options.clone();

        if let Some(idx) = self.editor.editing
            && idx < self.saved.len()
        {
            let old_addr = self.saved[idx].address.clone();
            let last_connected = self.saved[idx].last_connected;
            if old_addr != address && !self.saved.iter().enumerate().any(|(i, c)| i != idx && c.address == old_addr) {
                errors.extend(saved::set_password(&old_addr, "").err());
            }
            self.saved[idx] = SavedConnection {
                name,
                address: address.clone(),
                user,
                labels,
                favorite,
                options,
                last_connected,
            };
            self.selected = Some(idx);
        } else {
            self.saved.push(SavedConnection {
                name,
                address: address.clone(),
                user,
                labels,
                favorite,
                options,
                last_connected: 0,
            });
            self.selected = Some(self.saved.len() - 1);
        }

        errors.extend(saved::set_password(&address, &self.editor.password).err());
        errors.extend(saved::store(&self.saved).err());
        self.status = errors.join("\n");
    }

    fn duplicate_connection(&mut self, idx: usize) {
        let Some(orig) = self.saved.get(idx).cloned() else { return };
        let mut copy = orig;
        copy.name = format!("{} (Copy)", copy.name);
        self.saved.insert(idx + 1, copy);
        self.selected = Some(idx + 1);
        self.persist();
    }

    fn delete_connection(&mut self, idx: usize) {
        if idx >= self.saved.len() {
            return;
        }
        let old = self.saved.remove(idx);
        let still_uses_addr = self.saved.iter().any(|c| c.address == old.address);
        let mut errors = Vec::new();
        if !still_uses_addr {
            errors.extend(saved::set_password(&old.address, "").err());
            saved::delete_preview(&old.address);
            self.previews.remove(old.address.as_str());
        }
        errors.extend(saved::store(&self.saved).err());
        self.selected = None;
        self.status = errors.join("\n");
    }

    fn rename_label_everywhere(&mut self, old: &str, new: &str) {
        for c in &mut self.saved {
            for l in &mut c.labels {
                if l.eq_ignore_ascii_case(old) {
                    *l = new.to_string();
                }
            }
            c.labels = saved::parse_labels(&c.labels.join(","));
        }
        for l in &mut self.extra_labels {
            if l.eq_ignore_ascii_case(old) {
                *l = new.to_string();
            }
        }
        if matches!(&self.filter, Filter::Label(cur) if cur.eq_ignore_ascii_case(old)) {
            self.filter = Filter::Label(new.to_string());
        }
        self.persist();
    }

    fn remove_label_everywhere(&mut self, target: &str) {
        for c in &mut self.saved {
            c.labels.retain(|l| !l.eq_ignore_ascii_case(target));
        }
        self.extra_labels.retain(|l| !l.eq_ignore_ascii_case(target));
        if matches!(&self.filter, Filter::Label(cur) if cur.eq_ignore_ascii_case(target)) {
            self.filter = Filter::All;
        }
        self.persist();
    }

    fn persist(&mut self) {
        if let Err(e) = saved::store(&self.saved) {
            self.status = e;
        }
    }

    fn quick_connect_request(&self, address: ServerAddress) -> ConnectRequest {
        if let Some(c) = self.saved.iter().find(|c| c.address == address) {
            ConnectRequest {
                address: c.address.clone(),
                creds: Credentials {
                    user: c.user.clone(),
                    password: saved::password(&c.address).unwrap_or_default(),
                },
                options: c.options.clone(),
            }
        } else {
            ConnectRequest {
                creds: Credentials {
                    user: String::new(),
                    password: saved::password(&address).unwrap_or_default(),
                },
                address,
                options: SessionOptions::default(),
            }
        }
    }

    fn ensure_preview(&mut self, ctx: &egui::Context, address: &ServerAddress) -> Option<TextureHandle> {
        let key = address.as_str();
        if !self.previews.contains_key(key) {
            let loaded = saved::load_preview(address).map(|(w, h, rgba)| {
                let img = ColorImage::from_rgba_unmultiplied([usize::from(w), usize::from(h)], &rgba);
                ctx.load_texture(format!("preview:{key}"), img, TextureOptions::LINEAR)
            });
            self.previews.insert(key.to_string(), loaded);
        }
        self.previews.get(key).and_then(Clone::clone)
    }
}

fn sidebar_item(ui: &mut Ui, label: &str, count: usize, selected: bool) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 28.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    let accent = ui.visuals().selection.bg_fill;
    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(6), accent.gamma_multiply(0.18));
        let bar = Rect::from_min_size(rect.min + Vec2::new(0.0, 5.0), Vec2::new(3.0, rect.height() - 10.0));
        ui.painter().rect_filled(bar, CornerRadius::same(2), accent);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(0x1c, 0x26, 0x3b));
    }
    let text_color = if selected { accent } else { Color32::from_rgb(0xe2, 0xe8, 0xf0) };
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate_str(label, 21),
        FontId::proportional(13.0),
        text_color,
    );
    ui.painter().text(
        Pos2::new(rect.right() - 8.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::proportional(11.5),
        Color32::from_rgb(0x64, 0x74, 0x8b),
    );
    resp
}

fn initials(name: &str) -> String {
    let mut chars = name.split_whitespace().filter_map(|w| w.chars().next());
    let first = chars.next().unwrap_or('V');
    match chars.next() {
        Some(second) => format!("{first}{second}").to_uppercase(),
        None => name.chars().take(2).collect::<String>().to_uppercase(),
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn relative_time(epoch_secs: u64) -> String {
    if epoch_secs == 0 {
        return "Never".into();
    }
    let diff = now_secs().saturating_sub(epoch_secs);
    if diff < 60 {
        "Just now".into()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86_400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86_400)
    }
}

fn default_backup_path() -> String {
    std::env::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lucarna-connections.txt")
        .display()
        .to_string()
}
