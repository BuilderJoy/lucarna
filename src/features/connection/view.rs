use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{
    self, Align, Color32, ColorImage, CornerRadius, FontId, Id, Key, Layout, Modifiers, Order,
    Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, TextureOptions, Ui, Vec2,
};
use lucide_icons::Icon;

use super::saved::{self, SavedConnection, SessionOptions};
use super::widgets;
use super::{Credentials, ServerAddress, SshTarget};
use crate::shared::theme;

#[derive(Clone)]
pub struct ConnectRequest {
    pub address: ServerAddress,
    /// UltraVNC repeater to dial first; `address` is then what the repeater should reach.
    pub repeater: Option<ServerAddress>,
    /// `user@host[:port]` to tunnel through, as typed: `App::connect` validates it and refuses to fall back
    /// to a direct connection when it is invalid.
    pub ssh: String,
    pub creds: Credentials,
    pub options: SessionOptions,
}

#[derive(Clone, PartialEq)]
enum Filter {
    All,
    Favorites,
    Recent,
    Online,
    Hidden,
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
    repeater: String,
    ssh_user: String,
    ssh: String,
    ssh_password: String,
    password: String,
    show_password: bool,
    labels_input: String,
    favorite: bool,
    options: SessionOptions,
    test_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    test_result: Option<Result<String, String>>,
}

impl PropertiesEditor {
    /// Why the SSH field can't be used as typed, if it can't.
    /// The SSH target as saved (`user@host[:port]`), with the separate user field folded in.
    fn ssh_spec(&self) -> String {
        let (host, user) = (self.ssh.trim(), self.ssh_user.trim());
        if user.is_empty() || host.is_empty() || host.contains('@') {
            host.to_string()
        } else {
            format!("{user}@{host}")
        }
    }

    fn ssh_problem(&self) -> Option<String> {
        match SshTarget::parse(&self.ssh_spec()) {
            Err(e) => Some(e),
            Ok(Some(_)) if !self.repeater.trim().is_empty() => {
                Some("SSH and a repeater can't be combined: clear one of them".into())
            }
            Ok(_) => None,
        }
    }
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
            repeater: String::new(),
            ssh_user: String::new(),
            ssh: String::new(),
            ssh_password: String::new(),
            password: String::new(),
            show_password: false,
            labels_input: String::new(),
            favorite: false,
            options: SessionOptions::default(),
            test_rx: None,
            test_result: None,
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
    ToggleHidden(usize),
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
    quick: String,
    quick_view_only: bool,
    quick_ssh_on: bool,
    quick_ssh: String,
    quick_rep_on: bool,
    quick_rep: String,
    /// Which saved direct connections answered the last TCP probe (key: address).
    reach: HashMap<String, bool>,
    reach_rx: Option<mpsc::Receiver<(String, bool)>>,
    last_probe: Option<Instant>,
    pub last_request: Option<ConnectRequest>,
    pub status: String,
}

const PROBE_EVERY: Duration = Duration::from_secs(30);
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);

/// True when something accepts TCP connections at `address`. Only opens and closes a socket.
fn reachable(address: &str) -> bool {
    let Some(addr) = ServerAddress::parse(address) else {
        return false;
    };
    let Some((host, port)) = addr.host_port() else {
        return false;
    };
    (host, port)
        .to_socket_addrs()
        .is_ok_and(|mut it| it.any(|a| TcpStream::connect_timeout(&a, PROBE_TIMEOUT).is_ok()))
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
            quick: String::new(),
            quick_view_only: false,
            quick_ssh_on: false,
            quick_ssh: String::new(),
            quick_rep_on: false,
            quick_rep: String::new(),
            reach: HashMap::new(),
            reach_rx: None,
            last_probe: None,
            last_request: None,
            status: String::new(),
        };
        if open_new {
            d.open_new_connection(None);
        }
        d
    }
}

impl Dashboard {
    fn is_online(&self, c: &SavedConnection) -> bool {
        self.reach.get(c.address.as_str()).copied().unwrap_or(false)
    }

    /// Direct connections only: through SSH or a repeater the address isn't reachable from here.
    fn status_of(&self, c: &SavedConnection) -> Option<bool> {
        (c.ssh.is_empty() && c.repeater.is_empty())
            .then(|| self.reach.get(c.address.as_str()).copied())
            .flatten()
    }

    /// Re-checks every 30 s, only while the Address Book is on screen, and never blocks the UI.
    fn probe(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.reach_rx {
            while let Ok((k, v)) = rx.try_recv() {
                self.reach.insert(k, v);
            }
        }
        if self.last_probe.is_none_or(|t| t.elapsed() >= PROBE_EVERY) {
            self.last_probe = Some(Instant::now());
            let (tx, rx) = mpsc::channel();
            self.reach_rx = Some(rx);
            for c in self
                .saved
                .iter()
                .filter(|c| c.ssh.is_empty() && c.repeater.is_empty())
            {
                let (tx, ctx, key) = (tx.clone(), ctx.clone(), c.address.as_str().to_string());
                std::thread::spawn(move || {
                    let ok = reachable(&key);
                    let _ = tx.send((key, ok));
                    ctx.request_repaint();
                });
            }
        }
        ctx.request_repaint_after(PROBE_EVERY);
    }

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
    pub fn update_preview(
        &mut self,
        ctx: &egui::Context,
        address: &ServerAddress,
        width: u16,
        height: u16,
        rgba: &[u8],
    ) {
        let _ = saved::save_preview(address, width, height, rgba);
        let img =
            ColorImage::from_rgba_unmultiplied([usize::from(width), usize::from(height)], rgba);
        let tex = ctx.load_texture(
            format!("preview:{}", address.as_str()),
            img,
            TextureOptions::LINEAR,
        );
        self.previews
            .insert(address.as_str().to_string(), Some(tex));
    }

    /// Draws the Address Book and returns a `ConnectRequest` when the user initiates a connection.
    pub fn show(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<ConnectRequest> {
        let ctx = ui.ctx().clone();
        let p = theme::palette(&ctx);
        self.probe(&ctx);
        let mut request = None;

        // Top omni-bar: logo, quick-connect / search bar, view controls, and + New button.
        let top_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(20, 12));
        egui::Panel::top("address_book_top")
            .frame(top_frame)
            .show(ui, |ui| {
                ui.add_enabled_ui(!busy, |ui| self.top_bar(ui));
            });

        // Bottom status bar: device count, shortcuts hint, connection status/error, and quick reconnect.
        let bottom_frame =
            egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(16, 6));
        egui::Panel::bottom("address_book_bottom")
            .frame(bottom_frame)
            .show(ui, |ui| {
                if let Some(r) = self.bottom_bar(ui, busy) {
                    request = Some(r);
                }
            });

        // Left sidebar: Address book categories & user labels.
        let side_frame = egui::Frame::side_top_panel(ui.style())
            .fill(p.surface)
            .inner_margin(egui::Margin::symmetric(14, 20));
        egui::Panel::left("address_book_sidebar")
            .resizable(false)
            .exact_size(248.0)
            .frame(side_frame)
            .show(ui, |ui| {
                ui.add_enabled_ui(!busy, |ui| self.sidebar(ui));
            });

        // Main canvas: connection tiles (Grid) or rows (List).
        let center_frame = egui::Frame::central_panel(ui.style())
            .fill(p.bg)
            .inner_margin(egui::Margin::symmetric(32, 28));
        egui::CentralPanel::default()
            .frame(center_frame)
            .show(ui, |ui| {
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

        if let Some(r) = &request {
            self.last_request = Some(r.clone());
        }
        request
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        let logo_id = self
            .logo
            .get_or_insert_with(|| {
                let icon =
                    eframe::icon_data::from_png_bytes(crate::LOGO_PNG).expect("valid logo.png");
                let image = ColorImage::from_rgba_unmultiplied(
                    [icon.width as usize, icon.height as usize],
                    &icon.rgba,
                );
                ui.ctx().load_texture("logo", image, TextureOptions::LINEAR)
            })
            .id();

        ui.horizontal(|ui| {
            ui.add(egui::Image::new((logo_id, egui::vec2(34.0, 34.0))).corner_radius(9));
            ui.label(RichText::new("Lucarna").size(18.0).strong());
            ui.add_space(20.0);

            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.omni)
                    .hint_text(format!(
                        "{}  Search connections, labels or addresses",
                        char::from(Icon::Search)
                    ))
                    .desired_width(520.0)
                    .margin(egui::vec2(12.0, 10.0)),
            );
            if ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::K)) {
                resp.request_focus();
            }
            if self.omni.is_empty() {
                ui.painter().text(
                    resp.rect.right_center() - egui::vec2(14.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    if cfg!(target_os = "macos") {
                        "⌘K"
                    } else {
                        "Ctrl K"
                    },
                    FontId::proportional(11.0),
                    p.text3,
                );
            } else if ui
                .small_button(char::from(Icon::X).to_string())
                .on_hover_text("Clear search")
                .clicked()
            {
                self.omni.clear();
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add(
                        theme::primary(format!("{}  New connection", char::from(Icon::Plus)))
                            .min_size(egui::vec2(0.0, 40.0)),
                    )
                    .on_hover_text("⌘N / Ctrl+N")
                    .clicked()
                {
                    self.open_new_connection(None);
                }
                ui.menu_button(char::from(Icon::Settings).to_string(), |ui| {
                    if ui
                        .button(format!("{}  New label…", char::from(Icon::Tag)))
                        .clicked()
                    {
                        self.modal = Some(Modal::NewLabel(String::new()));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button(format!(
                            "{}  Import connections…",
                            char::from(Icon::FolderInput)
                        ))
                        .clicked()
                    {
                        self.modal = Some(Modal::ImportExport {
                            import: true,
                            path: default_backup_path(),
                        });
                        ui.close();
                    }
                    if ui
                        .button(format!(
                            "{}  Export connections…",
                            char::from(Icon::FolderOutput)
                        ))
                        .clicked()
                    {
                        self.modal = Some(Modal::ImportExport {
                            import: false,
                            path: default_backup_path(),
                        });
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Address book actions");
                theme::switch(ui);
            });
        });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("device_card")
            .frame(egui::Frame::NONE)
            .resizable(false)
            .show(ui, Self::device_card);
        self.sidebar_lists(ui);
    }

    /// Entry point of Studio / QuickSupport mode; the host UI lands with BUI-44.
    fn device_card(ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        egui::Frame::NONE
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.scope_builder(
                    egui::UiBuilder::new().layout(Layout::top_down(Align::Min)),
                    |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{}  This device",
                                    char::from(Icon::Monitor)
                                ))
                                .strong()
                                .size(13.0),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new("Not shared").size(11.0).color(p.text3));
                            });
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Let someone connect to this computer with Studio Host.")
                                .size(12.0)
                                .color(p.text2),
                        );
                        ui.add_space(6.0);
                        let w = ui.available_width();
                        ui.add_enabled(
                            false,
                            egui::Button::new("Share this screen").min_size(egui::vec2(w, 34.0)),
                        )
                        .on_disabled_hover_text("Coming with QuickSupport (BUI-44)");
                    },
                );
            });
    }

    fn sidebar_lists(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui.ctx());
        ui.label(
            RichText::new("ADDRESS BOOK")
                .size(11.0)
                .strong()
                .color(p.text3),
        );
        ui.add_space(8.0);

        let total = self.saved.iter().filter(|c| !c.hidden).count();
        let favs = self
            .saved
            .iter()
            .filter(|c| c.favorite && !c.hidden)
            .count();
        let recent = self
            .saved
            .iter()
            .filter(|c| c.last_connected > 0 && !c.hidden)
            .count();
        let hidden = self.saved.iter().filter(|c| c.hidden).count();

        if sidebar_item(
            ui,
            &format!("{}  All connections", char::from(Icon::LayoutGrid)),
            total,
            self.filter == Filter::All,
            None,
        )
        .clicked()
        {
            self.filter = Filter::All;
        }
        if sidebar_item(
            ui,
            &format!("{}  Favorites", char::from(Icon::Star)),
            favs,
            self.filter == Filter::Favorites,
            None,
        )
        .clicked()
        {
            self.filter = Filter::Favorites;
        }
        if sidebar_item(
            ui,
            &format!("{}  Recent", char::from(Icon::Clock)),
            recent,
            self.filter == Filter::Recent,
            None,
        )
        .clicked()
        {
            self.filter = Filter::Recent;
        }
        let online = self
            .saved
            .iter()
            .filter(|c| !c.hidden && self.is_online(c))
            .count();
        if sidebar_item(
            ui,
            &format!("{}  Online", char::from(Icon::Monitor)),
            online,
            self.filter == Filter::Online,
            None,
        )
        .clicked()
        {
            self.filter = Filter::Online;
        }
        if (hidden > 0 || self.filter == Filter::Hidden)
            && sidebar_item(
                ui,
                &format!("{}  Hidden", char::from(Icon::EyeOff)),
                hidden,
                self.filter == Filter::Hidden,
                None,
            )
            .clicked()
        {
            self.filter = Filter::Hidden;
        }

        ui.add_space(22.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("LABELS").size(11.0).strong().color(p.text3));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .small_button(char::from(Icon::Plus).to_string())
                    .on_hover_text("Create a new label")
                    .clicked()
                {
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
                    let resp = sidebar_item(ui, &label, count, active, Some(widgets::label_color(&label)));
                    if resp.clicked() {
                        self.filter = Filter::Label(label.clone());
                    }
                    resp.context_menu(|ui| {
                        if ui.button(format!("{}  Rename label…", char::from(Icon::Pencil))).clicked() {
                            label_to_rename = Some(label.clone());
                            ui.close();
                        }
                        if ui.button(format!("{}  Delete label", char::from(Icon::Trash2))).clicked() {
                            label_to_delete = Some(label.clone());
                            ui.close();
                        }
                    });
                }
            });
            if let Some(l) = label_to_rename {
                self.modal = Some(Modal::RenameLabel {
                    old: l.clone(),
                    new: l,
                });
            }
            if let Some(l) = label_to_delete {
                self.remove_label_everywhere(&l);
            }
        }
    }

    fn canvas(&mut self, ui: &mut egui::Ui) -> Option<ConnectRequest> {
        let quick_request = self.quick_card(ui);
        ui.add_space(24.0);
        let p = theme::palette(ui.ctx());
        let indices = self.visible_indices();
        let active_title = match &self.filter {
            Filter::All => "All connections".to_string(),
            Filter::Favorites => "Favorites".to_string(),
            Filter::Recent => "Recently connected".to_string(),
            Filter::Online => "Online".to_string(),
            Filter::Hidden => "Hidden connections".to_string(),
            Filter::Label(l) => format!("{}  {l}", char::from(Icon::Tag)),
        };

        ui.horizontal(|ui| {
            ui.label(RichText::new(&active_title).size(16.0).strong());
            egui::Frame::NONE
                .fill(p.elevated)
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(8, 1))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(indices.len().to_string())
                            .size(12.0)
                            .strong()
                            .color(p.text2),
                    )
                });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let items = [
                    (
                        ViewMode::Grid,
                        char::from(Icon::LayoutGrid).to_string(),
                        "Grid view",
                    ),
                    (
                        ViewMode::List,
                        char::from(Icon::List).to_string(),
                        "List view",
                    ),
                ];
                theme::segmented(ui, &mut self.view_mode, &items);
                let sort_name = match self.sort_by {
                    SortBy::Name => "Name",
                    SortBy::Recent => "Last connected",
                    SortBy::Address => "Server",
                };
                ui.menu_button(
                    format!("{}  {sort_name}", char::from(Icon::ArrowUpDown)),
                    |ui| {
                        for (by, label) in [
                            (SortBy::Name, "Name (A–Z)"),
                            (SortBy::Recent, "Recently connected"),
                            (SortBy::Address, "Server address"),
                        ] {
                            if ui.selectable_label(self.sort_by == by, label).clicked() {
                                self.sort_by = by;
                                ui.close();
                            }
                        }
                    },
                );
                if let Filter::Label(l) = &self.filter {
                    let l = l.clone();
                    if ui
                        .small_button(format!("{} Add connection here", char::from(Icon::Plus)))
                        .clicked()
                    {
                        self.open_new_connection(None);
                        self.editor.labels_input = l;
                    }
                }
            });
        });
        ui.add_space(16.0);

        if indices.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(64.0);
                ui.label(
                    RichText::new("No connections in this view")
                        .size(16.0)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Add a remote computer or type an address in the top bar to connect.",
                    )
                    .weak(),
                );
                ui.add_space(12.0);
                let btn = theme::primary(format!("{} New connection", char::from(Icon::Plus)))
                    .min_size(egui::vec2(160.0, 34.0));
                if ui.add(btn).clicked() {
                    self.open_new_connection(None);
                }
            });
            return quick_request;
        }

        let known_labels: Vec<String> = self.all_labels().into_iter().map(|(l, _)| l).collect();
        let mut action = None;

        egui::ScrollArea::vertical()
            .id_salt("canvas_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match self.view_mode {
                ViewMode::Grid => {
                    let card_h = 254.0;
                    let spacing = 20.0;
                    let avail = ui.available_width().max(260.0);
                    let cols =
                        (((avail + spacing) / (260.0 + spacing)).floor().max(1.0) as usize).min(5);
                    let card_w = ((avail - spacing * (cols as f32 - 1.0)) / cols as f32).floor();
                    for row in indices.chunks(cols) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = spacing;
                            for &idx in row {
                                if let Some(a) = self.draw_tile(
                                    ui,
                                    idx,
                                    Vec2::new(card_w, card_h),
                                    &known_labels,
                                ) {
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

        quick_request.or(action.and_then(|a| self.apply_action(a)))
    }

    /// "Connect to a device": address field, Connect, and per-connection chips.
    fn quick_card(&mut self, ui: &mut egui::Ui) -> Option<ConnectRequest> {
        let p = theme::palette(ui.ctx());
        let mut request = None;
        egui::Frame::NONE
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(14))
            .inner_margin(egui::Margin::symmetric(24, 20))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(220.0);
                        ui.label(RichText::new("Connect to a device").size(18.0).strong());
                        ui.label(
                            RichText::new("Address, user@host or repeater ID")
                                .size(13.0)
                                .color(p.text2),
                        );
                    });
                    ui.vertical(|ui| {
                        let addr = ServerAddress::parse(&self.quick);
                        let saved_already = addr
                            .as_ref()
                            .is_some_and(|a| self.saved.iter().any(|c| &c.address == a));
                        ui.horizontal(|ui| {
                            let reserve = if addr.is_some() && !saved_already {
                                220.0
                            } else {
                                140.0
                            };
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.quick)
                                    .font(FontId::monospace(15.0))
                                    .hint_text("192.168.1.40:5900")
                                    .desired_width((ui.available_width() - reserve).max(160.0))
                                    .margin(egui::vec2(14.0, 14.0)),
                            );
                            let enter =
                                resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            let go = ui
                                .add_enabled(
                                    addr.is_some(),
                                    theme::primary(format!(
                                        "Connect  {}",
                                        char::from(Icon::ArrowRight)
                                    ))
                                    .min_size(egui::vec2(112.0, 48.0)),
                                )
                                .clicked();
                            if (go || (enter && addr.is_some()))
                                && let Some(a) = addr.clone()
                            {
                                let mut r = self.quick_connect_request(a);
                                if self.quick_view_only {
                                    r.options.view_only = true;
                                }
                                if self.quick_ssh_on && !self.quick_ssh.trim().is_empty() {
                                    r.ssh = self.quick_ssh.trim().to_string();
                                }
                                if self.quick_rep_on {
                                    r.repeater = ServerAddress::parse(self.quick_rep.trim());
                                }
                                request = Some(r);
                            }
                            if let Some(a) = &addr
                                && !saved_already
                                && ui
                                    .add(egui::Button::new("Save").min_size(egui::vec2(0.0, 48.0)))
                                    .on_hover_text("Save this address to your Address Book")
                                    .clicked()
                            {
                                self.open_new_connection(Some(a.as_str()));
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if widgets::chip(ui, Icon::Lock, "SSH tunnel", self.quick_ssh_on) {
                                self.quick_ssh_on = !self.quick_ssh_on;
                            }
                            if widgets::chip(ui, Icon::Eye, "View only", self.quick_view_only) {
                                self.quick_view_only = !self.quick_view_only;
                            }
                            if widgets::chip(ui, Icon::Radio, "Via repeater", self.quick_rep_on) {
                                self.quick_rep_on = !self.quick_rep_on;
                            }
                            if self.quick_ssh_on {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.quick_ssh)
                                        .hint_text("user@host[:port]")
                                        .desired_width(170.0),
                                );
                            }
                            if self.quick_rep_on {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.quick_rep)
                                        .hint_text("repeater host:port")
                                        .desired_width(170.0),
                                );
                            }
                        });
                    });
                });
            });
        request
    }

    fn draw_tile(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        size: Vec2,
        known_labels: &[String],
    ) -> Option<TileAction> {
        let mut action = None;
        let is_selected = self.selected == Some(idx);
        let c = self.saved[idx].clone();
        let has_preview = self.ensure_preview(ui.ctx(), &c.address).is_some();
        let status = self.status_of(&c);
        let p = theme::palette(ui.ctx());

        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let hovered = ui.rect_contains_pointer(rect);
        let stroke = if is_selected || hovered {
            Stroke::new(if is_selected { 2.0 } else { 1.5 }, theme::ACCENT)
        } else {
            Stroke::new(1.0, p.border)
        };
        ui.painter().rect(
            rect,
            CornerRadius::same(14),
            p.card,
            stroke,
            StrokeKind::Inside,
        );

        let preview_rect = Rect::from_min_size(
            rect.min + Vec2::new(8.0, 8.0),
            Vec2::new(size.x - 16.0, 148.0),
        );
        self.paint_desktop_preview(ui, preview_rect, &c);
        if status == Some(false) {
            ui.painter().rect_filled(
                preview_rect,
                CornerRadius::same(9),
                p.bg.gamma_multiply(0.6),
            );
        }
        // Status and SSH badges over the preview.
        let badge =
            |ui: &egui::Ui, at: Pos2, align: egui::Align2, text: &str, dot: Option<Color32>| {
                let galley = ui.painter().layout_no_wrap(
                    text.to_string(),
                    FontId::proportional(11.0),
                    Color32::from_rgb(0xf1, 0xf5, 0xf9),
                );
                let w = galley.size().x + if dot.is_some() { 26.0 } else { 14.0 };
                let r = align.align_size_within_rect(
                    Vec2::new(w, 22.0),
                    Rect::from_min_size(at, Vec2::ZERO),
                );
                ui.painter()
                    .rect_filled(r, CornerRadius::same(11), Color32::from_black_alpha(184));
                let mut x = r.left() + 8.0;
                if let Some(d) = dot {
                    ui.painter()
                        .circle_filled(Pos2::new(x + 3.5, r.center().y), 3.5, d);
                    x += 13.0;
                }
                ui.painter().galley(
                    Pos2::new(x, r.center().y - galley.size().y / 2.0),
                    galley,
                    Color32::WHITE,
                );
            };
        if let Some(on) = status {
            let dot = if on {
                p.success
            } else {
                Color32::from_rgb(0x94, 0xa3, 0xb8)
            };
            badge(
                ui,
                preview_rect.left_top() + Vec2::new(8.0, 8.0),
                egui::Align2::LEFT_TOP,
                if on { "Online" } else { "Offline" },
                Some(dot),
            );
        }
        if !c.ssh.is_empty() {
            badge(
                ui,
                preview_rect.right_top() + Vec2::new(-8.0, 8.0),
                egui::Align2::RIGHT_TOP,
                &format!("{} SSH", char::from(Icon::Lock)),
                None,
            );
        }
        // Hover: dim the preview and offer Connect.
        if hovered {
            ui.painter().rect_filled(
                preview_rect,
                CornerRadius::same(9),
                Color32::from_black_alpha(115),
            );
            let pill = Rect::from_center_size(preview_rect.center(), Vec2::new(112.0, 38.0));
            let pill_resp = ui.interact(pill, ui.scope_id().with(("connect", idx)), Sense::click());
            ui.painter()
                .rect_filled(pill, CornerRadius::same(19), theme::ACCENT);
            ui.painter().text(
                pill.center(),
                egui::Align2::CENTER_CENTER,
                format!("Connect  {}", char::from(Icon::ArrowRight)),
                FontId::proportional(13.0),
                theme::ON_ACCENT,
            );
            if pill_resp.clicked() {
                return Some(TileAction::Connect(idx, None));
            }
        }

        let text_left = rect.left() + 14.0;
        let title_y = preview_rect.bottom() + 13.0;
        let max_chars = ((size.x - 96.0) / 8.0).max(8.0) as usize;
        ui.painter().text(
            Pos2::new(text_left, title_y),
            egui::Align2::LEFT_TOP,
            truncate_str(&c.name, max_chars),
            FontId::proportional(14.5),
            p.text,
        );
        let who = if c.user.is_empty() {
            c.address.as_str().to_string()
        } else {
            format!("{}@{}", c.user, c.address.as_str())
        };
        ui.painter().text(
            Pos2::new(text_left, title_y + 24.0),
            egui::Align2::LEFT_TOP,
            truncate_str(&who, ((size.x - 28.0) / 7.2) as usize),
            FontId::monospace(12.0),
            p.text2,
        );

        // Footer: first label as a coloured chip, time of last connection on the right.
        let footer_y = rect.bottom() - 30.0;
        if let Some(label) = c.labels.first() {
            let tag = truncate_str(label, 12);
            let galley = ui
                .painter()
                .layout_no_wrap(tag, FontId::proportional(11.0), p.text2);
            let chip = Rect::from_min_size(
                Pos2::new(text_left, footer_y),
                Vec2::new(galley.size().x + 28.0, 22.0),
            );
            ui.painter()
                .rect_filled(chip, CornerRadius::same(6), p.elevated);
            ui.painter().rect_filled(
                Rect::from_center_size(
                    Pos2::new(chip.left() + 11.0, chip.center().y),
                    Vec2::splat(7.0),
                ),
                CornerRadius::same(2),
                widgets::label_color(label),
            );
            ui.painter().galley(
                Pos2::new(chip.left() + 20.0, chip.center().y - galley.size().y / 2.0),
                galley,
                p.text2,
            );
        }
        ui.painter().text(
            Pos2::new(rect.right() - 14.0, footer_y + 11.0),
            egui::Align2::RIGHT_CENTER,
            relative_time(c.last_connected),
            FontId::proportional(12.0),
            p.text3,
        );

        // Favorite star and properties dots in the title row.
        let fav_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 62.0, title_y - 5.0),
            Vec2::new(26.0, 26.0),
        );
        let fav_resp = ui.interact(fav_rect, ui.scope_id().with(("fav", idx)), Sense::click());
        if c.favorite {
            widgets::star(
                ui.painter(),
                fav_rect.center(),
                8.0,
                Color32::from_rgb(0xf5, 0xa7, 0x0a),
            );
        } else {
            ui.painter().text(
                fav_rect.center(),
                egui::Align2::CENTER_CENTER,
                char::from(Icon::Star).to_string(),
                FontId::proportional(14.5),
                if fav_resp.hovered() { p.text } else { p.text3 },
            );
        }
        if fav_resp.on_hover_text("Toggle favorite").clicked() {
            return Some(TileAction::ToggleFavorite(idx));
        }
        let dots_rect = Rect::from_min_size(
            Pos2::new(rect.right() - 34.0, title_y - 5.0),
            Vec2::new(26.0, 26.0),
        );
        let dots_resp = ui.interact(dots_rect, ui.scope_id().with(("dots", idx)), Sense::click());
        ui.painter().text(
            dots_rect.center(),
            egui::Align2::CENTER_CENTER,
            char::from(Icon::Ellipsis).to_string(),
            FontId::proportional(14.5),
            if dots_resp.hovered() { p.text } else { p.text3 },
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

        resp.on_hover_text("Double-click to connect · Right-click for options")
            .context_menu(|ui| {
                if let Some(a) = Self::connection_context_menu(
                    ui,
                    idx,
                    &c,
                    has_preview,
                    known_labels,
                    &mut self.new_tag_inline,
                ) {
                    action = Some(a);
                }
            });

        action
    }

    fn draw_list_row(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        known_labels: &[String],
    ) -> Option<TileAction> {
        let mut action = None;
        let is_selected = self.selected == Some(idx);
        let c = self.saved[idx].clone();
        let has_preview = self.ensure_preview(ui.ctx(), &c.address).is_some();
        let accent = ui.visuals().selection.bg_fill;

        let row_h = 58.0;
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click());
        let hovered = resp.hovered();

        let p = theme::palette(ui.ctx());
        let bg = if is_selected || hovered {
            p.elevated
        } else {
            p.card
        };
        let stroke = if is_selected {
            Stroke::new(1.5, accent)
        } else {
            Stroke::new(1.0, p.border)
        };
        ui.painter()
            .rect(rect, CornerRadius::same(8), bg, stroke, StrokeKind::Inside);

        // Mini 16:10 desktop preview on the left.
        let prev_rect = Rect::from_min_size(rect.min + Vec2::new(8.0, 7.0), Vec2::new(70.0, 44.0));
        self.paint_desktop_preview(ui, prev_rect, &c);

        let text_x = prev_rect.right() + 12.0;
        ui.painter().text(
            Pos2::new(text_x, rect.top() + 11.0),
            egui::Align2::LEFT_TOP,
            &c.name,
            FontId::proportional(14.5),
            p.text,
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
            p.text2,
        );

        // Right-side controls inside the row.
        let controls_rect =
            Rect::from_min_max(Pos2::new(rect.right() - 280.0, rect.top()), rect.max);
        let mut child_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(controls_rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        child_ui.add_space(10.0);
        if child_ui
            .button(format!(
                "{} Properties",
                char::from(Icon::SlidersHorizontal)
            ))
            .clicked()
        {
            action = Some(TileAction::Properties(idx));
        }
        let connect_btn = theme::primary(format!("{} Connect", char::from(Icon::Play)));
        if child_ui.add(connect_btn).clicked() {
            action = Some(TileAction::Connect(idx, None));
        }
        if child_ui
            .selectable_label(c.favorite, char::from(Icon::Star).to_string())
            .on_hover_text("Toggle favorite")
            .clicked()
        {
            action = Some(TileAction::ToggleFavorite(idx));
        }
        for label in c.labels.iter().rev() {
            child_ui.label(
                RichText::new(format!("{} {label}", char::from(Icon::Tag)))
                    .size(11.0)
                    .color(p.accent_text),
            );
        }

        if resp.double_clicked() {
            action = Some(TileAction::Connect(idx, None));
        } else if resp.clicked() && action.is_none() {
            action = Some(TileAction::Select(idx));
        }

        resp.context_menu(|ui| {
            if let Some(a) = Self::connection_context_menu(
                ui,
                idx,
                &c,
                has_preview,
                known_labels,
                &mut self.new_tag_inline,
            ) {
                action = Some(a);
            }
        });

        action
    }

    fn paint_desktop_preview(&mut self, ui: &mut egui::Ui, rect: Rect, c: &SavedConnection) {
        let tex = self.ensure_preview(ui.ctx(), &c.address);
        let pal = theme::palette(ui.ctx());
        let p = ui.painter();
        let screen = rect.shrink(3.0);

        if let Some(tex) = tex {
            // Captured remote desktop, aspect-fit on a dark bezel.
            p.rect(
                rect,
                CornerRadius::same(9),
                Color32::from_rgb(0x09, 0x0d, 0x16),
                Stroke::new(1.0, pal.border),
                StrokeKind::Inside,
            );
            let [tw, th] = tex.size();
            let scale = (screen.width() / tw as f32).min(screen.height() / th as f32);
            let img_size = Vec2::new(tw as f32 * scale, th as f32 * scale);
            let img_rect = Rect::from_center_size(screen.center(), img_size);
            p.image(
                tex.id(),
                img_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        } else {
            // No snapshot yet: a tinted wallpaper with two window shapes.
            p.rect_filled(rect, CornerRadius::same(9), widgets::wallpaper(&c.name));
            let at = |fx: f32, fy: f32, fw: f32, fh: f32| {
                Rect::from_min_size(
                    rect.min + Vec2::new(rect.width() * fx, rect.height() * fy),
                    Vec2::new(rect.width() * fw, rect.height() * fh),
                )
            };
            p.rect_filled(
                at(0.14, 0.16, 0.52, 0.54),
                CornerRadius::same(4),
                Color32::from_white_alpha(224),
            );
            p.rect_filled(
                at(0.44, 0.34, 0.44, 0.44),
                CornerRadius::same(4),
                Color32::from_white_alpha(166),
            );
            let strip = Rect::from_min_max(
                Pos2::new(rect.left(), rect.bottom() - 12.0),
                rect.right_bottom(),
            );
            p.rect_filled(
                strip,
                CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: 9,
                    se: 9,
                },
                Color32::from_black_alpha(90),
            );
        }

        // Option badges overlaid on the bottom-left of the screen if non-default.
        if screen.width() > 100.0 {
            let mut bx = screen.left() + 5.0;
            let by = screen.bottom() - 19.0;
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

        if ui
            .button(RichText::new(format!("{}  Connect\tEnter", char::from(Icon::Play))).strong())
            .clicked()
        {
            action = Some(TileAction::Connect(idx, None));
            ui.close();
        }
        if ui
            .button(format!("{}  Connect (View only)", char::from(Icon::Eye)))
            .clicked()
        {
            action = Some(TileAction::Connect(idx, Some(true)));
            ui.close();
        }
        ui.separator();

        let fav_label = if c.favorite {
            format!("{}  Remove from Favorites", char::from(Icon::Star))
        } else {
            format!("{}  Add to Favorites", char::from(Icon::Star))
        };
        if ui.button(fav_label).clicked() {
            action = Some(TileAction::ToggleFavorite(idx));
            ui.close();
        }
        let hide_label = if c.hidden {
            format!("{}  Show in Address Book", char::from(Icon::Eye))
        } else {
            format!("{}  Hide", char::from(Icon::EyeOff))
        };
        if ui.button(hide_label).clicked() {
            action = Some(TileAction::ToggleHidden(idx));
            ui.close();
        }

        ui.menu_button(format!("{}  Labels", char::from(Icon::Tags)), |ui| {
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
                let r = ui.add(
                    egui::TextEdit::singleline(new_tag_inline)
                        .hint_text("New label…")
                        .desired_width(105.0),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (ui
                    .small_button(char::from(Icon::Plus).to_string())
                    .clicked()
                    || enter)
                    && !new_tag_inline.trim().is_empty()
                {
                    action = Some(TileAction::AddLabel(idx, new_tag_inline.trim().to_string()));
                    new_tag_inline.clear();
                    ui.close();
                }
            });
        });

        ui.separator();
        if ui
            .button(format!("{}  Rename…\tF2", char::from(Icon::Pencil)))
            .clicked()
        {
            action = Some(TileAction::Rename(idx));
            ui.close();
        }
        let dup_shortcut = if cfg!(target_os = "macos") {
            "⌘D"
        } else {
            "Ctrl+D"
        };
        if ui
            .button(format!(
                "{}  Duplicate\t{dup_shortcut}",
                char::from(Icon::Copy)
            ))
            .clicked()
        {
            action = Some(TileAction::Duplicate(idx));
            ui.close();
        }
        if ui
            .add_enabled(
                has_preview,
                egui::Button::new(format!(
                    "{}  Clear desktop preview",
                    char::from(Icon::ImageOff)
                )),
            )
            .clicked()
        {
            action = Some(TileAction::ClearPreview(idx));
            ui.close();
        }
        if ui
            .button(format!("{}  Delete\t⌫", char::from(Icon::Trash2)))
            .clicked()
        {
            action = Some(TileAction::Delete(idx));
            ui.close();
        }
        ui.separator();
        if ui
            .button(format!(
                "{}  Properties…\tAlt+Enter",
                char::from(Icon::SlidersHorizontal)
            ))
            .clicked()
        {
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
                    repeater: ServerAddress::parse(&c.repeater),
                    ssh: c.ssh,
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
            TileAction::ToggleHidden(idx) => {
                if let Some(c) = self.saved.get_mut(idx) {
                    c.hidden = !c.hidden;
                    self.selected = None;
                    self.persist();
                }
                None
            }
            TileAction::ToggleLabel(idx, label) => {
                if let Some(c) = self.saved.get_mut(idx) {
                    if let Some(pos) = c.labels.iter().position(|l| l.eq_ignore_ascii_case(&label))
                    {
                        c.labels.remove(pos);
                    } else {
                        c.labels.push(label);
                    }
                    self.persist();
                }
                None
            }
            TileAction::AddLabel(idx, label) => {
                let clean = label
                    .replace(['\t', '\n', '\r', ','], " ")
                    .trim()
                    .to_string();
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
        if ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Delete)
                || i.consume_key(Modifiers::NONE, Key::Backspace)
        }) {
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
                ui.set_min_width(400.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.editor.tab,
                        PropertiesTab::General,
                        format!("{} General", char::from(Icon::SlidersHorizontal)),
                    );
                    ui.selectable_value(
                        &mut self.editor.tab,
                        PropertiesTab::Options,
                        format!("{} Options & Display", char::from(Icon::Monitor)),
                    );
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
                        let addr_resp = field(
                            ui,
                            "VNC Server",
                            &mut self.editor.address,
                            "host, host:display or host::port (ID:1234 behind a repeater)",
                        );
                        field(
                            ui,
                            "Friendly Name",
                            &mut self.editor.name,
                            "optional, e.g. Office PC",
                        );
                        field(
                            ui,
                            "Username",
                            &mut self.editor.user,
                            "optional, for MS-Logon (Windows login)",
                        );
                        field(
                            ui,
                            "Repeater",
                            &mut self.editor.repeater,
                            "optional UltraVNC repeater, e.g. repeater.example.com:5901",
                        );
                        field(
                            ui,
                            "SSH user",
                            &mut self.editor.ssh_user,
                            "optional, the account on the SSH server (e.g. vboxuser)",
                        );
                        field(
                            ui,
                            "SSH",
                            &mut self.editor.ssh,
                            "optional host[:port]: encrypts VNC and files through SSH",
                        );
                        if !self.editor.ssh.trim().is_empty() {
                            ui.label(RichText::new("SSH password").strong());
                            ui.add(
                                egui::TextEdit::singleline(&mut self.editor.ssh_password)
                                    .password(true)
                                    .hint_text("leave empty to use your SSH keys or agent")
                                    .desired_width(f32::INFINITY)
                                    .margin(egui::vec2(8.0, 6.0)),
                            );
                            ui.add_space(6.0);
                        }
                        if let Some(problem) = self.editor.ssh_problem() {
                            ui.label(RichText::new(problem).color(ui.visuals().error_fg_color));
                            ui.add_space(6.0);
                        }

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
                            let eye_icon = if self.editor.show_password {
                                Icon::EyeOff
                            } else {
                                Icon::Eye
                            };
                            if ui
                                .selectable_label(
                                    self.editor.show_password,
                                    char::from(eye_icon).to_string(),
                                )
                                .on_hover_text("Show / hide password")
                                .clicked()
                            {
                                self.editor.show_password = !self.editor.show_password;
                            }
                            if (addr_resp.lost_focus() || pw_resp.lost_focus())
                                && ui.input(|i| i.key_pressed(Key::Enter))
                                && self.editor.ssh_problem().is_none()
                                && let Some(target) = ServerAddress::parse(&self.editor.address)
                            {
                                let opts = self.editor.options.clone();
                                let creds = Credentials {
                                    user: self.editor.user.trim().into(),
                                    password: self.editor.password.clone(),
                                };
                                self.save_editor(target.clone());
                                request = Some(ConnectRequest {
                                    repeater: ServerAddress::parse(&self.editor.repeater),
                                    ssh: self.editor.ssh_spec(),
                                    address: target,
                                    creds,
                                    options: opts,
                                });
                            }
                        });
                        ui.add_space(6.0);

                        field(
                            ui,
                            "Labels",
                            &mut self.editor.labels_input,
                            "comma-separated, e.g. Cambridge, London",
                        );
                        if !known_labels.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Quick toggle:").size(11.5).weak());
                                let current = saved::parse_labels(&self.editor.labels_input);
                                for k in &known_labels {
                                    let active = current.iter().any(|l| l.eq_ignore_ascii_case(k));
                                    if ui
                                        .selectable_label(
                                            active,
                                            format!("{} {k}", char::from(Icon::Tag)),
                                        )
                                        .clicked()
                                    {
                                        let mut next = current.clone();
                                        if let Some(p) =
                                            next.iter().position(|l| l.eq_ignore_ascii_case(k))
                                        {
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

                        ui.checkbox(
                            &mut self.editor.favorite,
                            format!("{} Pin to Favorites", char::from(Icon::Star)),
                        );
                    }
                    PropertiesTab::Options => {
                        ui.label(RichText::new("Picture & Scaling").strong());
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut self.editor.options.scale_1_1,
                                false,
                                "Fit to window",
                            );
                            ui.selectable_value(
                                &mut self.editor.options.scale_1_1,
                                true,
                                "Actual size (1:1)",
                            );
                        });
                        ui.add_space(10.0);

                        ui.label(RichText::new("Input & Session Behavior").strong());
                        ui.checkbox(
                            &mut self.editor.options.view_only,
                            "View only (do not send keyboard, mouse or clipboard)",
                        );
                        ui.checkbox(
                            &mut self.editor.options.fullscreen,
                            "Start session in full screen mode",
                        );
                        ui.checkbox(
                            &mut self.editor.options.share_clipboard,
                            "Share clipboard text with remote computer",
                        );
                        ui.add_space(10.0);

                        ui.label(RichText::new("Desktop Preview").strong());
                        ui.checkbox(
                            &mut self.editor.options.save_preview,
                            "Save desktop thumbnail preview when connected",
                        );
                        if let Some(target) = ServerAddress::parse(&self.editor.address) {
                            let has_prev = self
                                .previews
                                .get(target.as_str())
                                .and_then(|t| t.as_ref())
                                .is_some();
                            if ui
                                .add_enabled(
                                    has_prev,
                                    egui::Button::new("Clear saved preview image"),
                                )
                                .clicked()
                            {
                                saved::delete_preview(&target);
                                self.previews.insert(target.as_str().to_string(), None);
                            }
                        }
                    }
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);

                // An invalid SSH field blocks saving: it must never turn into a silent unencrypted connection.
                let target = ServerAddress::parse(&self.editor.address)
                    .filter(|_| self.editor.ssh_problem().is_none());
                let mut should_close = false;

                if let Some(rx) = &self.editor.test_rx
                    && let Ok(res) = rx.try_recv()
                {
                    self.editor.test_result = Some(res);
                    self.editor.test_rx = None;
                }

                if self.editor.test_rx.is_some() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("Testing connection…").italics());
                    });
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(100));
                    ui.add_space(4.0);
                } else if let Some(res) = &self.editor.test_result {
                    match res {
                        Ok(msg) => {
                            ui.label(
                                RichText::new(format!("✔ {msg}"))
                                    .color(theme::palette(ui.ctx()).success),
                            );
                        }
                        Err(err) => {
                            ui.label(
                                RichText::new(format!("✖ {err}"))
                                    .color(theme::palette(ui.ctx()).danger),
                            );
                        }
                    }
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    let connect_btn = theme::primary("Save & Connect");
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
                            repeater: ServerAddress::parse(&self.editor.repeater),
                            ssh: self.editor.ssh_spec(),
                            address: t,
                            creds,
                            options: opts,
                        });
                        should_close = true;
                    }
                    if ui
                        .add_enabled(target.is_some(), egui::Button::new("Save"))
                        .clicked()
                        && let Some(t) = target.clone()
                    {
                        self.save_editor(t);
                        should_close = true;
                    }
                    let testing = self.editor.test_rx.is_some();
                    let test_btn = egui::Button::new("Test Connection");
                    if ui
                        .add_enabled(target.is_some() && !testing, test_btn)
                        .clicked()
                        && let Some(t) = target.clone()
                    {
                        self.test_connection(t);
                    }
                    if let Some(idx) = self.editor.editing
                        && ui.button("Delete").clicked()
                    {
                        self.delete_connection(idx);
                        should_close = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
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
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
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
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
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
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
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
                        if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(Key::Escape))
                        {
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
            if !self
                .extra_labels
                .iter()
                .any(|l| l.eq_ignore_ascii_case(&label))
            {
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
                            if !self
                                .saved
                                .iter()
                                .any(|c| c.address == item.address && c.name == item.name)
                            {
                                self.saved.push(item);
                            }
                        }
                        self.persist();
                        self.status =
                            format!("Imported {count} connection(s) from {}", path.display());
                    }
                    Err(e) => self.status = e,
                }
            } else {
                match saved::export_to(&path, &self.saved) {
                    Ok(()) => {
                        self.status = format!(
                            "Exported {} connection(s) to {}",
                            self.saved.len(),
                            path.display()
                        )
                    }
                    Err(e) => self.status = e,
                }
            }
        }
        if close {
            self.modal = None;
        }
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui, busy: bool) -> Option<ConnectRequest> {
        let mut request = None;
        let p = theme::palette(ui.ctx());
        let total = self.saved.iter().filter(|c| !c.hidden).count();
        let online = self
            .saved
            .iter()
            .filter(|c| !c.hidden && self.is_online(c))
            .count();
        ui.horizontal(|ui| {
            ui.label(RichText::new("●").size(10.0).color(p.success));
            ui.label(
                RichText::new(format!("{online} of {total} online"))
                    .size(12.0)
                    .color(p.text2),
            );
            if busy {
                ui.spinner();
                ui.label("Connecting to remote desktop…");
            } else if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(ui.visuals().warn_fg_color));
                if let Some(last) = &self.last_request {
                    let btn = theme::primary(format!(
                        "{} Reconnect ({})",
                        char::from(Icon::RefreshCw),
                        last.address.as_str()
                    ))
                    .small();
                    if ui
                        .add(btn)
                        .on_hover_text(format!("Reconnect to {}", last.address.as_str()))
                        .clicked()
                    {
                        request = Some(last.clone());
                    }
                }
                if ui
                    .small_button(char::from(Icon::X).to_string())
                    .on_hover_text("Dismiss message")
                    .clicked()
                {
                    self.status.clear();
                }
            } else {
                let dup = if cfg!(target_os = "macos") {
                    "⌘D"
                } else {
                    "Ctrl+D"
                };
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    for (key, what) in [
                        (
                            if cfg!(target_os = "macos") {
                                "⌥↵"
                            } else {
                                "Alt+↵"
                            },
                            "Properties",
                        ),
                        (dup, "Duplicate"),
                        ("F2", "Rename"),
                    ] {
                        ui.label(RichText::new(what).size(12.0).color(p.text3));
                        widgets::keycap(ui, key);
                    }
                    ui.label(
                        RichText::new("Double-click to connect")
                            .size(12.0)
                            .color(p.text3),
                    );
                });
            }
        });
        request
    }

    fn visible_indices(&self) -> Vec<usize> {
        let q = self.omni.trim().to_lowercase();
        let mut indices: Vec<usize> = self
            .saved
            .iter()
            .enumerate()
            .filter(|(_, c)| match &self.filter {
                Filter::Hidden => c.hidden,
                _ if c.hidden => false,
                Filter::All => true,
                Filter::Favorites => c.favorite,
                Filter::Recent => c.last_connected > 0,
                Filter::Online => self.is_online(c),
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
            SortBy::Name => indices
                .sort_by_key(|&i| (!self.saved[i].favorite, self.saved[i].name.to_lowercase())),
            SortBy::Recent => {
                indices.sort_by_key(|&i| std::cmp::Reverse(self.saved[i].last_connected))
            }
            SortBy::Address => {
                indices.sort_by_key(|&i| self.saved[i].address.as_str().to_lowercase())
            }
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
            repeater: String::new(),
            ssh_user: String::new(),
            ssh: String::new(),
            ssh_password: String::new(),
            password: String::new(),
            show_password: false,
            labels_input: initial_label,
            favorite: self.filter == Filter::Favorites,
            options: SessionOptions::default(),
            test_rx: None,
            test_result: None,
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
            repeater: c.repeater.clone(),
            ssh_user: split_ssh(&c.ssh).0,
            ssh: split_ssh(&c.ssh).1,
            ssh_password: SshTarget::parse(&c.ssh)
                .ok()
                .flatten()
                .and_then(|t| saved::ssh_password(&t))
                .unwrap_or_default(),
            password: saved::password(&c.address).unwrap_or_default(),
            show_password: false,
            labels_input: c.labels.join(", "),
            favorite: c.favorite,
            options: c.options.clone(),
            test_rx: None,
            test_result: None,
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
        let repeater = self.editor.repeater.trim().to_string();
        let ssh = self.editor.ssh_spec();
        let old_ssh = self
            .editor
            .editing
            .and_then(|i| self.saved.get(i))
            .map(|c| c.ssh.clone());
        let favorite = self.editor.favorite;
        let options = self.editor.options.clone();

        if let Some(idx) = self.editor.editing
            && idx < self.saved.len()
        {
            let old_addr = self.saved[idx].address.clone();
            let last_connected = self.saved[idx].last_connected;
            let hidden = self.saved[idx].hidden;
            if old_addr != address
                && !self
                    .saved
                    .iter()
                    .enumerate()
                    .any(|(i, c)| i != idx && c.address == old_addr)
            {
                errors.extend(saved::set_password(&old_addr, "").err());
            }
            self.saved[idx] = SavedConnection {
                name,
                address: address.clone(),
                user,
                repeater,
                ssh: ssh.clone(),
                labels,
                favorite,
                hidden,
                options,
                last_connected,
            };
            self.selected = Some(idx);
        } else {
            self.saved.push(SavedConnection {
                name,
                address: address.clone(),
                user,
                repeater,
                ssh: ssh.clone(),
                labels,
                favorite,
                hidden: false,
                options,
                last_connected: 0,
            });
            self.selected = Some(self.saved.len() - 1);
        }

        errors.extend(saved::set_password(&address, &self.editor.password).err());
        if let Some(old) = old_ssh.filter(|old| *old != ssh) {
            errors.extend(self.forget_ssh_password(&old));
        }
        if let Ok(Some(target)) = SshTarget::parse(&ssh) {
            errors.extend(saved::set_ssh_password(&target, &self.editor.ssh_password).err());
        }
        errors.extend(saved::store(&self.saved).err());
        self.status = errors.join("\n");
    }

    fn test_connection(&mut self, address: ServerAddress) {
        let repeater = ServerAddress::parse(&self.editor.repeater);
        let ssh = SshTarget::parse(&self.editor.ssh_spec()).ok().flatten();
        let password = if self.editor.password.is_empty() {
            saved::password(&address).unwrap_or_default()
        } else {
            self.editor.password.clone()
        };
        let creds = Credentials {
            user: self.editor.user.trim().into(),
            password,
        };
        if let Some(target) = &ssh {
            let ssh_pw = if self.editor.ssh_password.is_empty() {
                saved::ssh_password(target).unwrap_or_default()
            } else {
                self.editor.ssh_password.clone()
            };
            if !ssh_pw.is_empty() {
                let _ = saved::set_ssh_password(target, &ssh_pw);
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.editor.test_rx = Some(rx);
        self.editor.test_result = None;
        std::thread::spawn(move || {
            let res = crate::session::test(&address, repeater.as_ref(), ssh.as_ref(), &creds);
            let _ = tx.send(res);
        });
    }

    fn duplicate_connection(&mut self, idx: usize) {
        let Some(orig) = self.saved.get(idx).cloned() else {
            return;
        };
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
        errors.extend(self.forget_ssh_password(&old.ssh));
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
        self.extra_labels
            .retain(|l| !l.eq_ignore_ascii_case(target));
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

    /// Removes the keychain entry of an SSH target no saved connection uses any more.
    fn forget_ssh_password(&self, ssh: &str) -> Option<String> {
        let target = SshTarget::parse(ssh).ok().flatten()?;
        let in_use = self
            .saved
            .iter()
            .any(|c| SshTarget::parse(&c.ssh).ok().flatten().as_ref() == Some(&target));
        if in_use {
            None
        } else {
            saved::set_ssh_password(&target, "").err()
        }
    }

    fn quick_connect_request(&self, address: ServerAddress) -> ConnectRequest {
        if let Some(c) = self.saved.iter().find(|c| c.address == address) {
            ConnectRequest {
                repeater: ServerAddress::parse(&c.repeater),
                ssh: c.ssh.clone(),
                address: c.address.clone(),
                creds: Credentials {
                    user: c.user.clone(),
                    password: saved::password(&c.address).unwrap_or_default(),
                },
                options: c.options.clone(),
            }
        } else {
            ConnectRequest {
                repeater: None,
                ssh: String::new(),
                creds: Credentials {
                    user: String::new(),
                    password: saved::password(&address).unwrap_or_default(),
                },
                address,
                options: SessionOptions::default(),
            }
        }
    }

    fn ensure_preview(
        &mut self,
        ctx: &egui::Context,
        address: &ServerAddress,
    ) -> Option<TextureHandle> {
        let key = address.as_str();
        if !self.previews.contains_key(key) {
            let loaded = saved::load_preview(address).map(|(w, h, rgba)| {
                let img =
                    ColorImage::from_rgba_unmultiplied([usize::from(w), usize::from(h)], &rgba);
                ctx.load_texture(format!("preview:{key}"), img, TextureOptions::LINEAR)
            });
            self.previews.insert(key.to_string(), loaded);
        }
        self.previews.get(key).and_then(Clone::clone)
    }
}

fn sidebar_item(
    ui: &mut Ui,
    label: &str,
    count: usize,
    selected: bool,
    dot: Option<Color32>,
) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 40.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    let p = theme::palette(ui.ctx());
    if selected {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(9), p.accent_soft);
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(9), p.elevated);
    }
    let (text_color, count_color) = if selected {
        (p.accent_text, p.accent_text)
    } else if resp.hovered() {
        (p.text, p.text3)
    } else {
        (p.text2, p.text3)
    };
    let mut x = rect.left() + 12.0;
    if let Some(d) = dot {
        ui.painter().rect_filled(
            Rect::from_center_size(Pos2::new(x + 5.0, rect.center().y), Vec2::splat(10.0)),
            CornerRadius::same(3),
            d,
        );
        x += 22.0;
    }
    ui.painter().text(
        Pos2::new(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate_str(label, 21),
        FontId::proportional(14.0),
        text_color,
    );
    ui.painter().text(
        Pos2::new(rect.right() - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::proportional(12.0),
        count_color,
    );
    resp
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

/// Splits a saved `user@host[:port]` into (user, host[:port]).
fn split_ssh(spec: &str) -> (String, String) {
    match spec.rsplit_once('@') {
        Some((user, host)) => (user.to_string(), host.to_string()),
        None => (String::new(), spec.to_string()),
    }
}

#[cfg(test)]
mod ssh_user_tests {
    use super::*;

    #[test]
    fn user_field_joins_into_saved_spec_and_splits_back() {
        let mut e = PropertiesEditor {
            ssh_user: "vboxuser".into(),
            ssh: "127.0.0.1:2222".into(),
            ..Default::default()
        };
        assert_eq!(e.ssh_spec(), "vboxuser@127.0.0.1:2222");
        assert_eq!(
            split_ssh(&e.ssh_spec()),
            ("vboxuser".into(), "127.0.0.1:2222".into())
        );
        // A user typed inline in the host field is kept as is, not doubled.
        e.ssh_user = "other".into();
        e.ssh = "davidm@127.0.0.1:2222".into();
        assert_eq!(e.ssh_spec(), "davidm@127.0.0.1:2222");
        // No host, no SSH.
        assert_eq!(
            PropertiesEditor {
                ssh_user: "x".into(),
                ..Default::default()
            }
            .ssh_spec(),
            ""
        );
    }
}
