use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, Rect, RichText};
use lucide_icons::Icon;

use super::protocol::{self, Entry};
use super::transfer::{self, FileOps, Update};
use crate::shared::theme;

/// The server stays silent when file transfer is disabled, so we give up waiting after this.
const PATIENCE: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct LocalEntry {
    name: String,
    path: PathBuf,
    size: u64,
    is_dir: bool,
}

enum Prompt {
    NewFolder(String),
    Rename { old: String, new: String },
    Delete { name: String, is_dir: bool },
}

/// Two-pane (Local | Remote) file browser and transfer window.
pub struct FilesView {
    pub open: bool,
    /// Where the window was last drawn, so the screen view can stop forwarding the pointer under it.
    pub rect: Option<Rect>,
    access: Option<bool>,
    drives: Vec<(String, char)>,
    path: String,
    entries: Vec<Entry>,
    local_dir: PathBuf,
    local_entries: Vec<LocalEntry>,
    selected_local: Option<PathBuf>,
    prompt: Option<Prompt>,
    upload_queue: VecDeque<PathBuf>,
    waiting_since: Option<Instant>,
    progress: Option<(String, u64, u64)>,
    status: String,
}

impl Default for FilesView {
    fn default() -> Self {
        let local_dir = transfer::downloads_dir();
        let local_entries = read_local_dir(&local_dir);
        Self {
            open: false,
            rect: None,
            access: None,
            drives: Vec::new(),
            path: String::new(),
            entries: Vec::new(),
            local_dir,
            local_entries,
            selected_local: None,
            prompt: None,
            upload_queue: VecDeque::new(),
            waiting_since: None,
            progress: None,
            status: String::new(),
        }
    }
}

impl FilesView {
    pub fn apply(&mut self, update: Update, files: &dyn FileOps) {
        self.waiting_since = None;
        match update {
            Update::Access(ok) => self.access = Some(ok),
            Update::Drives(d) => self.drives = d,
            Update::Listing { path, mut entries } => {
                entries.sort_by_key(|e| (e.name != "..", !e.is_dir, e.name.to_lowercase()));
                (self.path, self.entries) = (path, entries);
            }
            Update::Inaccessible => self.status = "That folder can't be read on the server.".into(),
            Update::Progress { name, done, total } => self.progress = Some((name, done, total)),
            Update::Downloaded(dest) => {
                self.progress = None;
                self.status = format!("Saved to {}", dest.display());
                self.refresh_local();
            }
            Update::Uploaded(name) => {
                self.progress = None;
                self.status = format!("Uploaded {name}");
                if !self.path.is_empty() {
                    files.list(&self.path);
                }
                self.start_next_upload(files);
            }
            Update::Command { ok, message } => {
                self.status = message;
                if ok && !self.path.is_empty() {
                    files.list(&self.path);
                }
            }
            Update::Failed(e) => {
                self.progress = None;
                self.upload_queue.clear();
                self.status = e;
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, files: &dyn FileOps) {
        if !self.open {
            self.rect = None;
            return;
        }
        if self.access.is_none() && self.waiting_since.is_none() {
            files.open();
            self.waiting_since = Some(Instant::now());
        }
        if self.access == Some(true) && !self.path.is_empty() {
            let dropped: Vec<PathBuf> = ctx.input(|i| {
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect()
            });
            if !dropped.is_empty() {
                self.upload_queue.extend(dropped);
                self.start_next_upload(files);
            }
        }
        let mut open = self.open;
        let viewport_id = egui::ViewportId::from_hash_of("lucarna_files_window");
        let mut builder = egui::ViewportBuilder::default()
            .with_title("Files - Lucarna")
            .with_app_id("lucarna-files")
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([720.0, 480.0]);

        if let Ok(icon) = eframe::icon_data::from_png_bytes(crate::LOGO_PNG) {
            builder = builder.with_icon(icon);
        }

        ctx.show_viewport_immediate(viewport_id, builder, |ctx, _class| {
            if ctx.input(|i| i.viewport().close_requested()) {
                open = false;
            }
            if self.access == Some(true) && !self.path.is_empty() {
                let dropped: Vec<PathBuf> = ctx.input(|i| {
                    i.raw
                        .dropped_files
                        .iter()
                        .map(|f| f.path().to_path_buf())
                        .collect()
                });
                if !dropped.is_empty() {
                    self.upload_queue.extend(dropped);
                    self.start_next_upload(files);
                }
            }
            let frame = egui::Frame::NONE.fill(theme::palette(ctx).bg);
            egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
                self.contents(ui, files);
            });
            if !self.open {
                open = false;
            }
        });
        self.open = open;
        self.rect = None;
    }

    fn contents(&mut self, ui: &mut egui::Ui, files: &dyn FileOps) {
        let p = theme::palette(ui.ctx());
        if theme::window_header(ui, Icon::Folder, "Files", |ui| {
            theme::tag(
                ui,
                Icon::Network,
                "UltraVNC file transfer",
                p.text2,
                p.elevated,
            );
        }) {
            self.open = false;
        }
        let note = |ui: &mut egui::Ui, text: RichText| {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(text);
            });
        };
        let waited = self.waiting_since.map(|t| t.elapsed());
        if waited.is_some_and(|w| w > PATIENCE) {
            note(
                ui,
                RichText::new(
                    "No answer from the server. File transfer may be disabled in its UltraVNC settings.",
                )
                .color(ui.visuals().warn_fg_color),
            );
            return;
        }
        match self.access {
            None => {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    ui.spinner();
                    ui.label("Asking the server for file transfer…");
                });
                return;
            }
            Some(false) => {
                note(
                    ui,
                    RichText::new("The server refused file transfer.")
                        .color(ui.visuals().warn_fg_color),
                );
                return;
            }
            Some(true) => {}
        }

        egui::Panel::bottom("files_transfers")
            .frame(egui::Frame::NONE.fill(p.surface))
            .resizable(false)
            .show(ui, |ui| {
                ui.add(egui::Separator::default().spacing(0.0));
                self.transfers(ui, files);
            });
        egui::Frame::NONE
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                let pane_height = (ui.available_height() - 8.0).max(200.0);
                ui.columns(2, |cols| {
                    card(&mut cols[0], |ui| self.local_pane(ui, files, pane_height));
                    card(&mut cols[1], |ui| self.remote_pane(ui, files, pane_height));
                });
            });
    }

    /// Bottom strip: the running transfer as a card, or a hint when idle.
    fn transfers(&mut self, ui: &mut egui::Ui, files: &dyn FileOps) {
        let p = theme::palette(ui.ctx());
        egui::Frame::NONE
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Transfers").size(13.0).strong());
                    let active = usize::from(self.progress.is_some());
                    theme::count_pill(ui, active + self.upload_queue.len());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new("Drop files on this window to upload")
                                .size(12.0)
                                .color(p.text3),
                        );
                    });
                });
                ui.add_space(8.0);
                if let Some((name, done, total)) = self.progress.clone() {
                    let queued = self.upload_queue.len();
                    egui::Frame::NONE
                        .fill(p.card)
                        .stroke(egui::Stroke::new(1.0, p.border))
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(char::from(Icon::ArrowUpDown).to_string())
                                        .color(p.accent_text),
                                );
                                ui.vertical(|ui| {
                                    ui.set_width((ui.available_width() - 90.0).max(120.0));
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(&name).size(12.0).strong());
                                        ui.with_layout(
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                let q = if queued > 0 {
                                                    format!(" · +{queued} queued")
                                                } else {
                                                    String::new()
                                                };
                                                ui.label(
                                                    RichText::new(format!(
                                                        "{} / {}{q}",
                                                        human_size(done),
                                                        human_size(total)
                                                    ))
                                                    .size(12.0)
                                                    .color(p.text2),
                                                );
                                            },
                                        );
                                    });
                                    let fraction = if total == 0 {
                                        1.0
                                    } else {
                                        done as f32 / total as f32
                                    };
                                    ui.add(
                                        egui::ProgressBar::new(fraction)
                                            .desired_height(6.0)
                                            .corner_radius(egui::CornerRadius::same(3))
                                            .fill(theme::ACCENT),
                                    );
                                });
                                if ui.add(theme::danger(ui, "Cancel")).clicked() {
                                    self.upload_queue.clear();
                                    files.cancel();
                                }
                            });
                        });
                } else if !self.status.is_empty() {
                    ui.label(RichText::new(&self.status).color(p.text2));
                } else {
                    ui.label(
                        RichText::new(
                            "Nothing running. Pick a file on the left and press Upload, or double-click a file to send or fetch it.",
                        )
                        .size(12.0)
                        .color(p.text3),
                    );
                }
            });
    }

    fn local_pane(&mut self, ui: &mut egui::Ui, files: &dyn FileOps, height: f32) {
        let busy = self.progress.is_some();
        let can_upload = !busy && !self.path.is_empty();
        let mut upload = None;

        ui.horizontal(|ui| {
            pane_title(ui, Icon::Laptop, "Local");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let sel = self.selected_local.clone();
                let tip = if sel.is_none() {
                    "Select a file below to upload (or double-click it)"
                } else {
                    "Upload the selected file to the remote folder"
                };
                if ui
                    .add_enabled(
                        can_upload && sel.is_some(),
                        theme::primary(format!("{} Upload", char::from(Icon::Upload)))
                            .min_size(egui::vec2(0.0, 32.0)),
                    )
                    .on_hover_text(tip)
                    .on_disabled_hover_text(tip)
                    .clicked()
                {
                    upload = sel;
                }
                if icon_square(ui, Icon::RefreshCw, "Refresh local folder") {
                    self.refresh_local();
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let mut go = None;
            let downloads = transfer::downloads_dir();
            if theme::chip(ui, Icon::Download, "Downloads", self.local_dir == downloads).clicked() {
                go = Some(downloads);
            }
            if let Some(home) = std::env::home_dir() {
                let desktop = home.join("Desktop");
                if desktop.is_dir()
                    && theme::chip(ui, Icon::Monitor, "Desktop", self.local_dir == desktop)
                        .clicked()
                {
                    go = Some(desktop);
                }
                if theme::chip(ui, Icon::House, "Home", self.local_dir == home).clicked() {
                    go = Some(home);
                }
            }
            if let Some(dir) = go {
                self.set_local_dir(dir);
            }
        });
        ui.add_space(8.0);
        path_bar(ui, &self.local_dir.display().to_string());
        ui.add_space(8.0);

        // Sticky column headers
        let local_cols = [LOCAL_SIZE_W, LOCAL_ACT_W];
        table_header(ui, &local_cols, ["Name", "Size", ""]);

        let mut go_dir = None;
        let scroll_h = (height - 214.0).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("local_scroll")
            .auto_shrink(false)
            .max_height(scroll_h)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if let Some(parent) = self.local_dir.parent().filter(|p| *p != self.local_dir) {
                    let (resp, name, _) = table_row(ui, &local_cols, false, true);
                    cell(ui, name, false, |ui| {
                        name_label(ui, Icon::CornerLeftUp, "..", false)
                    });
                    if resp.on_hover_text("Double-click to go up").double_clicked() {
                        go_dir = Some(parent.to_path_buf());
                    }
                }
                for e in &self.local_entries {
                    let selected = !e.is_dir && self.selected_local.as_ref() == Some(&e.path);
                    let (resp, name, cols) = table_row(ui, &local_cols, selected, true);
                    let icon = if e.is_dir { Icon::Folder } else { Icon::File };
                    cell(ui, name, false, |ui| name_label(ui, icon, &e.name, false));
                    if e.is_dir {
                        if resp.on_hover_text("Double-click to open").double_clicked() {
                            go_dir = Some(e.path.clone());
                        }
                        continue;
                    }
                    cell(ui, cols[0], true, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(human_size(e.size)).weak())
                                .selectable(false),
                        );
                    });
                    let mut clicked_btn = false;
                    cell(ui, cols[1], false, |ui| {
                        let hint = if can_upload {
                            "Upload to the remote folder"
                        } else {
                            "Select a remote folder first"
                        };
                        if icon_button(ui, Icon::Upload, hint, can_upload) {
                            upload = Some(e.path.clone());
                            clicked_btn = true;
                        }
                    });
                    if clicked_btn {
                        continue;
                    }
                    if resp.clicked() {
                        self.selected_local = Some(e.path.clone());
                    }
                    if resp.double_clicked() {
                        upload = Some(e.path.clone());
                    }
                }
            });
        pane_footer(ui, &format!("{} items", self.local_entries.len()));
        if let Some(dir) = go_dir {
            self.set_local_dir(dir);
        }
        if let Some(path) = upload {
            self.upload_queue.push_back(path);
            self.start_next_upload(files);
        }
    }

    fn remote_pane(&mut self, ui: &mut egui::Ui, files: &dyn FileOps, height: f32) {
        let busy = self.progress.is_some();
        let has_dir = !self.path.is_empty();
        let mut go = None;
        ui.horizontal(|ui| {
            pane_title(ui, Icon::Server, "Remote");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy && has_dir,
                        egui::Button::new(format!("{} Folder", char::from(Icon::FolderPlus)))
                            .min_size(egui::vec2(0.0, 32.0)),
                    )
                    .on_hover_text("Create a folder here")
                    .clicked()
                {
                    self.prompt = Some(Prompt::NewFolder(String::new()));
                }
                if ui
                    .add_enabled(
                        !busy && has_dir,
                        icon_square_button(Icon::RefreshCw)
                            .stroke(egui::Stroke::new(1.0, theme::palette(ui.ctx()).border)),
                    )
                    .on_hover_text("Refresh remote folder")
                    .clicked()
                {
                    go = Some(self.path.clone());
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for (drive, kind) in &self.drives {
                let (label, tip, icon) = match kind {
                    'h' => (
                        "Home".to_string(),
                        "Home folder: files you upload land here",
                        Icon::House,
                    ),
                    'd' => (
                        drive.rsplit('/').next().unwrap_or(drive).to_string(),
                        "Shortcut",
                        Icon::Folder,
                    ),
                    'l' if drive == "/" => {
                        ("Root /".to_string(), "Filesystem root", Icon::HardDrive)
                    }
                    'l' => (drive.clone(), "Local disk", Icon::HardDrive),
                    'f' => (drive.clone(), "Removable", Icon::Usb),
                    'c' => (drive.clone(), "CD-ROM", Icon::Disc),
                    'n' => (drive.clone(), "Network", Icon::Network),
                    _ => (drive.clone(), "Drive", Icon::HardDrive),
                };
                let here = self.path.trim_end_matches('/');
                let active = if drive == "/" {
                    self.path == "/"
                } else if matches!(kind, 'h' | 'd') {
                    here == drive.as_str()
                } else {
                    self.path.starts_with(drive.as_str())
                };
                if theme::chip(ui, icon, &label, active)
                    .on_hover_text(format!("{tip}\n{drive}"))
                    .clicked()
                {
                    go = Some(drive.clone());
                }
            }
        });
        ui.add_space(8.0);
        path_bar(
            ui,
            if self.path.is_empty() {
                "Pick a drive"
            } else {
                &self.path
            },
        );
        self.prompt_bar(ui, files);
        ui.add_space(8.0);

        // Sticky column headers
        let remote_cols = [REMOTE_SIZE_W, REMOTE_DATE_W, REMOTE_ACT_W];
        table_header(ui, &remote_cols, ["Name", "Size", "Modified (UTC)", ""]);

        let mut fetch = None;
        let scroll_h = (height - if self.prompt.is_some() { 266.0 } else { 214.0 }).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("remote_scroll")
            .auto_shrink(false)
            .max_height(scroll_h)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for e in &self.entries {
                    let is_up = e.is_dir && e.name == "..";
                    let (resp, name, cols) = table_row(ui, &remote_cols, false, true);
                    let icon = if is_up {
                        Icon::CornerLeftUp
                    } else if e.is_dir {
                        Icon::Folder
                    } else {
                        Icon::File
                    };
                    cell(ui, name, false, |ui| {
                        name_label(ui, icon, &e.name, e.inaccessible)
                    });
                    if is_up {
                        if resp.on_hover_text("Double-click to go up").double_clicked() {
                            go = Some(child(&self.path, &e.name));
                        }
                        continue;
                    }
                    if !e.is_dir {
                        cell(ui, cols[0], true, |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(human_size(e.size)).weak())
                                    .selectable(false),
                            );
                        });
                    }
                    cell(ui, cols[1], false, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(filetime(e.modified)).weak())
                                .selectable(false),
                        );
                    });
                    let mut clicked_btn = false;
                    cell(ui, cols[2], false, |ui| {
                        let (tip, path) = if e.is_dir {
                            (
                                "Download the folder as a .zip",
                                format!("{}[ {} ]", self.path, e.name),
                            )
                        } else {
                            (
                                "Download to your computer",
                                format!("{}{}", self.path, e.name),
                            )
                        };
                        if icon_button(ui, Icon::Download, tip, !busy) {
                            fetch = Some(path);
                            clicked_btn = true;
                        }
                        if icon_button(ui, Icon::Pencil, "Rename", !busy) {
                            self.prompt = Some(Prompt::Rename {
                                old: e.name.clone(),
                                new: e.name.clone(),
                            });
                            clicked_btn = true;
                        }
                        if icon_button(ui, Icon::Trash2, "Delete", !busy) {
                            self.prompt = Some(Prompt::Delete {
                                name: e.name.clone(),
                                is_dir: e.is_dir,
                            });
                            clicked_btn = true;
                        }
                    });
                    if clicked_btn {
                        continue;
                    }
                    if resp.double_clicked() {
                        if e.is_dir {
                            go = Some(child(&self.path, &e.name));
                        } else {
                            fetch = Some(format!("{}{}", self.path, e.name));
                        }
                    }
                }
            });
        pane_footer(ui, &format!("{} items", self.entries.len()));
        if let Some(path) = go {
            self.prompt = None;
            self.request(files, &path);
        }
        if let Some(path) = fetch {
            files.download(&path, &self.local_dir);
            self.status.clear();
        }
    }

    fn prompt_bar(&mut self, ui: &mut egui::Ui, files: &dyn FileOps) {
        let Some(prompt) = &mut self.prompt else {
            return;
        };
        let mut close = false;
        ui.horizontal(|ui| match prompt {
            Prompt::NewFolder(name) => {
                ui.label("New folder:");
                let resp = ui.add(egui::TextEdit::singleline(name).desired_width(160.0));
                let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.small_button("Create").clicked() || submit {
                    let trimmed = name.trim();
                    if valid_name(trimmed) {
                        files.create_dir(&format!("{}{trimmed}", self.path));
                        close = true;
                    } else {
                        self.status = "Invalid folder name.".into();
                    }
                }
                if ui.small_button("Cancel").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            }
            Prompt::Rename { old, new } => {
                ui.label(format!("Rename {old} to:"));
                let resp = ui.add(egui::TextEdit::singleline(new).desired_width(160.0));
                let submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.small_button("Rename").clicked() || submit {
                    let trimmed = new.trim();
                    if valid_name(trimmed) && trimmed != old {
                        files.rename(
                            &format!("{}{old}", self.path),
                            &format!("{}{trimmed}", self.path),
                        );
                        close = true;
                    } else if trimmed == old {
                        close = true;
                    } else {
                        self.status = "Invalid name.".into();
                    }
                }
                if ui.small_button("Cancel").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            }
            Prompt::Delete { name, is_dir } => {
                let kind = if *is_dir { "folder" } else { "file" };
                ui.label(format!("Delete {kind} {name}?"));
                if ui.small_button("Delete").clicked() {
                    files.delete(&format!("{}{name}", self.path));
                    close = true;
                }
                if ui.small_button("Cancel").clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    close = true;
                }
            }
        });
        if close {
            self.prompt = None;
        }
    }

    fn set_local_dir(&mut self, dir: PathBuf) {
        self.selected_local = None;
        self.local_entries = read_local_dir(&dir);
        self.local_dir = dir;
    }

    fn refresh_local(&mut self) {
        self.local_entries = read_local_dir(&self.local_dir);
    }

    fn start_next_upload(&mut self, files: &dyn FileOps) {
        if self.progress.is_some() || self.path.is_empty() {
            return;
        }
        if let Some(local) = self.upload_queue.pop_front() {
            self.status.clear();
            files.upload(&local, &self.path);
        }
    }

    fn request(&mut self, files: &dyn FileOps, path: &str) {
        files.list(path);
        self.waiting_since = Some(Instant::now());
    }
}

fn read_local_dir(dir: &Path) -> Vec<LocalEntry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<LocalEntry> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            if name.starts_with('.') {
                return None;
            }
            let meta = e.metadata().ok()?;
            Some(LocalEntry {
                name,
                path: e.path(),
                size: meta.len(),
                is_dir: meta.is_dir(),
            })
        })
        .collect();
    out.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
    out
}

/// Rejects path separators, `*` (the wire delimiter in `CFileRename`), and `.` / `..`.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '*', '\0'])
}

/// `C:\a\` + `b` → `C:\a\b\`; `..` goes up one level.
/// `C:\a\` + `b` → `C:\a\b\`; `/home/` + `user` → `/home/user/`; `..` goes up one level.
fn child(path: &str, name: &str) -> String {
    if name == ".." {
        let trimmed = path.trim_end_matches(['\\', '/']);
        return match trimmed.rfind(['\\', '/']) {
            Some(i) => trimmed[..=i].to_string(),
            None => {
                if path.contains('/') {
                    "/".to_string()
                } else {
                    format!("{trimmed}\\")
                }
            }
        };
    }
    let sep = if path.contains('/') { '/' } else { '\\' };
    if path.ends_with(sep) {
        format!("{path}{name}{sep}")
    } else {
        format!("{path}{sep}{name}{sep}")
    }
}

const ROW_H: f32 = 36.0;
const PAD: f32 = 8.0;
const GAP: f32 = 8.0;
const LOCAL_SIZE_W: f32 = 64.0;
const LOCAL_ACT_W: f32 = 28.0;
const REMOTE_SIZE_W: f32 = 64.0;
const REMOTE_DATE_W: f32 = 112.0;
const REMOTE_ACT_W: f32 = 92.0;

/// Allocates one table row and returns its response, the (flexible) name
/// rect and the rects of the fixed-width columns to its right, so every row
/// and the header share the same column positions.
fn table_row(
    ui: &mut egui::Ui,
    fixed: &[f32],
    selected: bool,
    interactive: bool,
) -> (egui::Response, egui::Rect, Vec<egui::Rect>) {
    let sense = if interactive {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), sense);
    if selected {
        ui.painter().rect_filled(
            rect,
            6.0,
            crate::shared::theme::palette(ui.ctx()).accent_soft,
        );
    } else if interactive && resp.hovered() {
        ui.painter()
            .rect_filled(rect, 6.0, theme::palette(ui.ctx()).elevated);
    }
    let mut right = rect.right() - PAD;
    let mut cols = vec![egui::Rect::NOTHING; fixed.len()];
    for (i, w) in fixed.iter().enumerate().rev() {
        cols[i] = egui::Rect::from_min_max(
            egui::pos2(right - w, rect.top()),
            egui::pos2(right, rect.bottom()),
        );
        right -= w + GAP;
    }
    let name = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD, rect.top()),
        egui::pos2(right.max(rect.left() + PAD + 40.0), rect.bottom()),
    );
    (resp, name, cols)
}

/// Draws content inside a column rect, left- or right-aligned, centred vertically.
fn cell(ui: &mut egui::Ui, rect: egui::Rect, right: bool, add: impl FnOnce(&mut egui::Ui)) {
    let layout = if right {
        Layout::right_to_left(Align::Center)
    } else {
        Layout::left_to_right(Align::Center)
    };
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(layout), add);
}

fn table_header<const N: usize>(ui: &mut egui::Ui, fixed: &[f32], titles: [&str; N]) {
    let (_, name, cols) = table_row(ui, fixed, false, false);
    let title = |ui: &mut egui::Ui, t: &str| {
        let c = theme::palette(ui.ctx()).text3;
        ui.add(
            egui::Label::new(RichText::new(t.to_uppercase()).size(11.0).strong().color(c))
                .selectable(false),
        );
    };
    cell(ui, name, false, |ui| title(ui, titles[0]));
    for (i, rect) in cols.iter().enumerate() {
        // The first fixed column is Size, which is right-aligned.
        cell(ui, *rect, i == 0, |ui| title(ui, titles[i + 1]));
    }
    ui.add(egui::Separator::default().spacing(2.0));
}

fn name_label(ui: &mut egui::Ui, icon: Icon, name: &str, dim: bool) {
    let p = theme::palette(ui.ctx());
    if matches!(icon, Icon::Folder) && !dim {
        ui.label(RichText::new(char::from(icon).to_string()).color(p.accent_text));
        ui.add(egui::Label::new(name).truncate().selectable(false));
        return;
    }
    let text = RichText::new(format!("{} {name}", char::from(icon)));
    let text = if dim { text.weak() } else { text };
    ui.add(egui::Label::new(text).truncate().selectable(false));
}

/// Rounded surface that holds one pane of the file browser.
fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui.ctx());
    egui::Frame::NONE
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::same(12))
        .show(ui, add);
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str, enabled: bool) -> bool {
    let mut text = RichText::new(char::from(icon).to_string());
    if matches!(icon, Icon::Trash2) {
        text = text.color(theme::palette(ui.ctx()).danger);
    }
    ui.add_enabled(enabled, egui::Button::new(text).small().frame(false))
        .on_hover_text(tip)
        .clicked()
}

fn pane_title(ui: &mut egui::Ui, icon: Icon, title: &str) {
    let c = theme::palette(ui.ctx()).text2;
    ui.label(
        RichText::new(char::from(icon).to_string())
            .size(15.0)
            .color(c),
    );
    ui.label(RichText::new(title).size(14.0).strong());
}

fn icon_square_button(icon: Icon) -> egui::Button<'static> {
    egui::Button::new(char::from(icon).to_string())
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, egui::Color32::TRANSPARENT))
        .min_size(egui::vec2(32.0, 32.0))
}

fn icon_square(ui: &mut egui::Ui, icon: Icon, tip: &str) -> bool {
    let b =
        icon_square_button(icon).stroke(egui::Stroke::new(1.0, theme::palette(ui.ctx()).border));
    ui.add(b).on_hover_text(tip).clicked()
}

/// Read-only path field in monospace.
fn path_bar(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    egui::Frame::NONE
        .fill(p.input_bg)
        .stroke(egui::Stroke::new(1.0, p.border))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(text).monospace().size(12.0).color(p.text2))
                    .truncate(),
            );
        });
}

fn pane_footer(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui.ctx());
    ui.add_space(4.0);
    ui.add(egui::Separator::default().spacing(0.0));
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(12.0).color(p.text3));
}

fn human_size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}

/// FILETIME (100 ns ticks since 1601 UTC) as "YYYY-MM-DD HH:MM".
fn filetime(ft: u64) -> String {
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    if ft == 0 || secs <= 0 {
        return String::new();
    }
    let (y, m, d, hh, mm) = protocol::utc_parts(secs);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation() {
        assert_eq!(child("C:\\a\\", "b"), "C:\\a\\b\\");
        assert_eq!(child("C:\\a\\b\\", ".."), "C:\\a\\");
        assert_eq!(child("C:\\a\\", ".."), "C:\\");
        assert_eq!(child("/home/", "user"), "/home/user/");
        assert_eq!(child("/home/user/", ".."), "/home/");
        assert_eq!(child("/home/", ".."), "/");
        assert_eq!(child("/", "var"), "/var/");
    }

    #[test]
    fn formatting() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        // 2026-10-17 14:05 UTC.
        assert_eq!(
            filetime((1_792_245_900 + 11_644_473_600) * 10_000_000),
            "2026-10-17 14:05"
        );
    }

    #[test]
    fn name_validation() {
        assert!(valid_name("report.pdf"));
        assert!(valid_name("My Folder"));
        assert!(!valid_name(""));
        assert!(!valid_name("."));
        assert!(!valid_name(".."));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("a\\b"));
        assert!(!valid_name("a*b"));
    }
}
