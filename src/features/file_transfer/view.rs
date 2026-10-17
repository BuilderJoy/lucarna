use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, Rect, RichText};

use super::protocol::Entry;
use super::transfer::{RemoteFiles, Update};

/// The server stays silent when file transfer is disabled, so we give up waiting after this.
const PATIENCE: Duration = Duration::from_secs(10);

/// Remote file browser window (read-only: browse and download).
#[derive(Default)]
pub struct FilesView {
    pub open: bool,
    /// Where the window was last drawn, so the screen view can stop forwarding the pointer under it.
    pub rect: Option<Rect>,
    access: Option<bool>,
    drives: Vec<(String, char)>,
    path: String,
    entries: Vec<Entry>,
    waiting_since: Option<Instant>,
    progress: Option<(String, u64, u64)>,
    status: String,
}

impl FilesView {
    pub fn apply(&mut self, update: Update) {
        self.waiting_since = None;
        match update {
            Update::Access(ok) => self.access = Some(ok),
            Update::Drives(d) => self.drives = d,
            Update::Listing { path, mut entries } => {
                entries.sort_by_key(|e| (e.name != "..", !e.is_dir, e.name.to_lowercase()));
                (self.path, self.entries) = (path, entries);
                self.status.clear();
            }
            Update::Inaccessible => self.status = "That folder can't be read on the server.".into(),
            Update::Progress { name, done, total } => self.progress = Some((name, done, total)),
            Update::Downloaded(dest) => {
                self.progress = None;
                self.status = format!("Saved to {}", dest.display());
            }
            Update::Failed(e) => {
                self.progress = None;
                self.status = e;
            }
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, files: &RemoteFiles) {
        if !self.open {
            self.rect = None;
            return;
        }
        if self.access.is_none() && self.waiting_since.is_none() {
            files.open();
            self.waiting_since = Some(Instant::now());
        }
        let mut open = self.open;
        let window = egui::Window::new("Remote files").open(&mut open).default_size([560.0, 420.0]).show(ctx, |ui| self.contents(ui, files));
        self.open = open;
        self.rect = window.map(|w| w.response.rect);
    }

    fn contents(&mut self, ui: &mut egui::Ui, files: &RemoteFiles) {
        let waited = self.waiting_since.map(|t| t.elapsed());
        if waited.is_some_and(|w| w > PATIENCE) {
            ui.label(RichText::new("No answer from the server. File transfer may be disabled in its UltraVNC settings.").color(ui.visuals().warn_fg_color));
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
                ui.label(RichText::new("The server refused file transfer.").color(ui.visuals().warn_fg_color));
                return;
            }
            Some(true) => {}
        }

        let busy = self.progress.is_some();
        let mut go = None;
        ui.horizontal_wrapped(|ui| {
            for (drive, kind) in &self.drives {
                let tip = match kind {
                    'l' => "Local disk",
                    'f' => "Removable",
                    'c' => "CD-ROM",
                    'n' => "Network",
                    _ => "Drive",
                };
                if ui.selectable_label(self.path.starts_with(drive.as_str()), drive.as_str()).on_hover_text(tip).clicked() {
                    go = Some(drive.clone());
                }
            }
        });
        ui.label(RichText::new(if self.path.is_empty() { "Pick a drive" } else { &self.path }).monospace());
        ui.separator();

        let mut fetch = None;
        egui::ScrollArea::vertical().auto_shrink(false).max_height(ui.available_height() - 64.0).show(ui, |ui| {
            egui::Grid::new("files").striped(true).num_columns(4).show(ui, |ui| {
                ui.label(RichText::new("Name").strong());
                ui.label(RichText::new("Size").strong());
                ui.label(RichText::new("Modified (UTC)").strong());
                ui.label("");
                ui.end_row();
                for e in &self.entries {
                    if e.is_dir {
                        let label = format!("📁 {}", e.name);
                        let text = if e.inaccessible { RichText::new(label).weak() } else { RichText::new(label) };
                        if ui.add(egui::Label::new(text).sense(egui::Sense::click())).on_hover_text("Double-click to open").double_clicked() {
                            go = Some(child(&self.path, &e.name));
                        }
                        ui.label("");
                    } else {
                        ui.label(format!("📄 {}", e.name));
                        ui.label(human_size(e.size));
                    }
                    ui.label(filetime(e.modified));
                    if !e.is_dir && ui.add_enabled(!busy, egui::Button::new("Download").small()).clicked() {
                        fetch = Some(format!("{}{}", self.path, e.name));
                    }
                    ui.end_row();
                }
            });
        });
        if let Some(path) = go {
            self.request(files, &path);
        }
        if let Some(path) = fetch {
            files.download(&path);
            self.status.clear();
        }

        ui.separator();
        if let Some((name, done, total)) = &self.progress {
            ui.horizontal(|ui| {
                let fraction = if *total == 0 { 1.0 } else { *done as f32 / *total as f32 };
                ui.add(egui::ProgressBar::new(fraction).text(format!("{name}  {} / {}", human_size(*done), human_size(*total))).desired_width(ui.available_width() - 80.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        files.cancel();
                    }
                });
            });
        } else if !self.status.is_empty() {
            ui.label(&self.status);
        } else {
            ui.label(RichText::new("Downloads go to your Downloads folder.").weak());
        }
    }

    fn request(&mut self, files: &RemoteFiles, path: &str) {
        files.list(path);
        self.waiting_since = Some(Instant::now());
    }
}

/// `C:\a\` + `b` → `C:\a\b\`; `..` goes up one level.
fn child(path: &str, name: &str) -> String {
    if name == ".." {
        let trimmed = path.trim_end_matches('\\');
        return match trimmed.rfind('\\') {
            Some(i) => trimmed[..=i].to_string(),
            None => format!("{trimmed}\\"),
        };
    }
    format!("{path}{name}\\")
}

fn human_size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[u]) }
}

/// FILETIME (100 ns ticks since 1601 UTC) as "YYYY-MM-DD HH:MM".
fn filetime(ft: u64) -> String {
    if ft == 0 {
        return String::new();
    }
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation() {
        assert_eq!(child("C:\\a\\", "b"), "C:\\a\\b\\");
        assert_eq!(child("C:\\a\\b\\", ".."), "C:\\a\\");
        assert_eq!(child("C:\\a\\", ".."), "C:\\");
    }

    #[test]
    fn formatting() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        // 2026-10-17 14:05 UTC.
        assert_eq!(filetime((1_792_245_900 + 11_644_473_600) * 10_000_000), "2026-10-17 14:05");
    }
}
