use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, Rect, RichText};
use lucide_icons::Icon;

use super::protocol::{self, Entry};
use super::transfer::{self, FileOps, Update};

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
        let window = egui::Window::new("Files")
            .open(&mut open)
            .default_size([860.0, 480.0])
            .show(ctx, |ui| self.contents(ui, files));
        self.open = open;
        self.rect = window.map(|w| w.response.rect);
    }

    fn contents(&mut self, ui: &mut egui::Ui, files: &dyn FileOps) {
        let waited = self.waiting_since.map(|t| t.elapsed());
        if waited.is_some_and(|w| w > PATIENCE) {
            ui.label(
                RichText::new("No answer from the server. File transfer may be disabled in its UltraVNC settings.")
                    .color(ui.visuals().warn_fg_color),
            );
            return;
        }
        match self.access {
            None => {
                ui.ctx().request_repaint_after(Duration::from_millis(500));
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Asking the server for file transfer…");
                });
                return;
            }
            Some(false) => {
                ui.label(
                    RichText::new("The server refused file transfer.")
                        .color(ui.visuals().warn_fg_color),
                );
                return;
            }
            Some(true) => {}
        }

        let pane_height = (ui.available_height() - 42.0).max(160.0);
        ui.columns(2, |cols| {
            self.local_pane(&mut cols[0], files, pane_height);
            self.remote_pane(&mut cols[1], files, pane_height);
        });

        ui.separator();
        if let Some((name, done, total)) = &self.progress {
            let queued = self.upload_queue.len();
            let suffix = if queued > 0 {
                format!(" (+{queued} queued)")
            } else {
                String::new()
            };
            ui.horizontal(|ui| {
                let fraction = if *total == 0 {
                    1.0
                } else {
                    *done as f32 / *total as f32
                };
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .text(format!(
                            "{name}  {} / {}{suffix}",
                            human_size(*done),
                            human_size(*total)
                        ))
                        .desired_width(ui.available_width() - 80.0),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        self.upload_queue.clear();
                        files.cancel();
                    }
                });
            });
        } else if !self.status.is_empty() {
            ui.label(&self.status);
        } else {
            ui.label(RichText::new("Drop files onto this window or click Upload to send them to the active remote folder.").weak());
        }
    }

    fn local_pane(&mut self, ui: &mut egui::Ui, files: &dyn FileOps, height: f32) {
        let busy = self.progress.is_some();
        let can_upload = !busy && !self.path.is_empty();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Local").strong());
            if ui.small_button("Downloads").clicked() {
                self.set_local_dir(transfer::downloads_dir());
            }
            if let Some(home) = std::env::home_dir() {
                let desktop = home.join("Desktop");
                if desktop.is_dir() && ui.small_button("Desktop").clicked() {
                    self.set_local_dir(desktop);
                }
                if ui.small_button("Home").clicked() {
                    self.set_local_dir(home);
                }
            }
            if ui
                .small_button(char::from(Icon::RefreshCw).to_string())
                .on_hover_text("Refresh local folder")
                .clicked()
            {
                self.refresh_local();
            }
        });
        ui.label(RichText::new(self.local_dir.display().to_string()).monospace());
        ui.separator();

        let mut go_dir = None;
        let mut upload = None;
        let scroll_h = (height - 64.0).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("local_scroll")
            .auto_shrink(false)
            .max_height(scroll_h)
            .show(ui, |ui| {
                egui::Grid::new("local_files")
                    .striped(true)
                    .num_columns(3)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Name").strong());
                        ui.label(RichText::new("Size").strong());
                        ui.label("");
                        ui.end_row();
                        if let Some(parent) =
                            self.local_dir.parent().filter(|p| *p != self.local_dir)
                        {
                            if ui
                                .add(
                                    egui::Label::new(format!("{} ..", char::from(Icon::Folder)))
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Double-click to go up")
                                .double_clicked()
                            {
                                go_dir = Some(parent.to_path_buf());
                            }
                            ui.label("");
                            ui.label("");
                            ui.end_row();
                        }
                        for e in &self.local_entries {
                            if e.is_dir {
                                if ui
                                    .add(
                                        egui::Label::new(format!(
                                            "{} {}",
                                            char::from(Icon::Folder),
                                            e.name
                                        ))
                                        .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text("Double-click to open")
                                    .double_clicked()
                                {
                                    go_dir = Some(e.path.clone());
                                }
                                ui.label("");
                                ui.label("");
                            } else {
                                ui.label(format!("{} {}", char::from(Icon::File), e.name));
                                ui.label(human_size(e.size));
                                if ui
                                    .add_enabled(
                                        can_upload,
                                        egui::Button::new(format!(
                                            "{} Upload",
                                            char::from(Icon::Upload)
                                        ))
                                        .small(),
                                    )
                                    .clicked()
                                {
                                    upload = Some(e.path.clone());
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
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
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Remote").strong());
            for (drive, kind) in &self.drives {
                let tip = match kind {
                    'l' => "Local disk",
                    'f' => "Removable",
                    'c' => "CD-ROM",
                    'n' => "Network",
                    _ => "Drive",
                };
                if ui
                    .selectable_label(self.path.starts_with(drive.as_str()), drive.as_str())
                    .on_hover_text(tip)
                    .clicked()
                {
                    go = Some(drive.clone());
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(
                        !busy && has_dir,
                        egui::Button::new(char::from(Icon::RefreshCw).to_string()).small(),
                    )
                    .on_hover_text("Refresh remote folder")
                    .clicked()
                {
                    go = Some(self.path.clone());
                }
                if ui
                    .add_enabled(
                        !busy && has_dir,
                        egui::Button::new(format!("{} Folder", char::from(Icon::FolderPlus)))
                            .small(),
                    )
                    .clicked()
                {
                    self.prompt = Some(Prompt::NewFolder(String::new()));
                }
            });
        });
        ui.label(
            RichText::new(if self.path.is_empty() {
                "Pick a drive"
            } else {
                &self.path
            })
            .monospace(),
        );
        self.prompt_bar(ui, files);
        ui.separator();

        let mut fetch = None;
        let scroll_h = (height - if self.prompt.is_some() { 96.0 } else { 64.0 }).max(80.0);
        egui::ScrollArea::vertical()
            .id_salt("remote_scroll")
            .auto_shrink(false)
            .max_height(scroll_h)
            .show(ui, |ui| {
                egui::Grid::new("remote_files")
                    .striped(true)
                    .num_columns(4)
                    .show(ui, |ui| {
                        ui.label(RichText::new("Name").strong());
                        ui.label(RichText::new("Size").strong());
                        ui.label(RichText::new("Modified (UTC)").strong());
                        ui.label("");
                        ui.end_row();
                        for e in &self.entries {
                            if e.is_dir {
                                let label = format!("{} {}", char::from(Icon::Folder), e.name);
                                let text = if e.inaccessible {
                                    RichText::new(label).weak()
                                } else {
                                    RichText::new(label)
                                };
                                if ui
                                    .add(egui::Label::new(text).sense(egui::Sense::click()))
                                    .on_hover_text("Double-click to open")
                                    .double_clicked()
                                {
                                    go = Some(child(&self.path, &e.name));
                                }
                                ui.label("");
                            } else {
                                ui.label(format!("{} {}", char::from(Icon::File), e.name));
                                ui.label(human_size(e.size));
                            }
                            ui.label(filetime(e.modified));
                            ui.horizontal(|ui| {
                                let tip = if e.is_dir {
                                    "Download the folder as a .zip"
                                } else {
                                    "Download to your Mac"
                                };
                                if e.name != ".."
                                    && ui
                                        .add_enabled(
                                            !busy,
                                            egui::Button::new(format!(
                                                "{} Download",
                                                char::from(Icon::Download)
                                            ))
                                            .small(),
                                        )
                                        .on_hover_text(tip)
                                        .clicked()
                                {
                                    // UltraVNC zips a folder requested as `[ name ]` and sends the zip.
                                    fetch = Some(if e.is_dir {
                                        format!("{}[ {} ]", self.path, e.name)
                                    } else {
                                        format!("{}{}", self.path, e.name)
                                    });
                                }
                                if e.name != ".." {
                                    if ui
                                        .add_enabled(!busy, egui::Button::new("Rename").small())
                                        .clicked()
                                    {
                                        self.prompt = Some(Prompt::Rename {
                                            old: e.name.clone(),
                                            new: e.name.clone(),
                                        });
                                    }
                                    if ui
                                        .add_enabled(!busy, egui::Button::new("Delete").small())
                                        .clicked()
                                    {
                                        self.prompt = Some(Prompt::Delete {
                                            name: e.name.clone(),
                                            is_dir: e.is_dir,
                                        });
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
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
    if ft == 0 {
        return String::new();
    }
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
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
