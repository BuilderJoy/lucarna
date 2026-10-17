use std::sync::Mutex;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};

use super::Framebuffer;

/// How the remote screen is fitted into the window.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Scale {
    /// Shrink or grow to fill the window.
    #[default]
    Fit,
    /// One remote pixel per physical screen pixel, with scroll bars when it doesn't fit.
    Actual,
}

/// Where the remote screen is drawn locally; maps local points to remote pixels.
pub struct Viewport {
    rect: Rect,
    /// The part of `rect` that is actually on screen (smaller than `rect` when scrolled).
    visible: Rect,
    scale: f32,
    width: f32,
    height: f32,
}

impl Viewport {
    pub fn contains(&self, p: Pos2) -> bool {
        self.rect.contains(p) && self.visible.contains(p)
    }

    pub fn to_remote(&self, p: Pos2) -> (u16, u16) {
        let v = (p - self.rect.min) / self.scale;
        (v.x.clamp(0.0, self.width - 1.0) as u16, v.y.clamp(0.0, self.height - 1.0) as u16)
    }
}

#[derive(Default)]
pub struct ScreenView {
    texture: Option<TextureHandle>,
}

impl ScreenView {
    /// Paints the screen according to `mode`.
    pub fn show(&mut self, ui: &mut egui::Ui, screen: &Mutex<Framebuffer>, mode: Scale) -> Viewport {
        let (width, height) = {
            let mut fb = screen.lock().unwrap();
            let full = [0, 0, fb.width, fb.height];
            let dirty = fb.dirty.take();
            let image = |r: [usize; 4]| ColorImage::from_rgba_unmultiplied([r[2] - r[0], r[3] - r[1]], &fb.rgba(r));
            match &mut self.texture {
                None => self.texture = Some(ui.ctx().load_texture("screen", image(full), TextureOptions::LINEAR)),
                // Whole screen or a resize (DesktopSize sets the full region dirty).
                Some(t) if dirty == Some(full) || t.size() != [fb.width, fb.height] => t.set(image(full), TextureOptions::LINEAR),
                Some(t) => {
                    if let Some(r) = dirty.filter(|r| r[0] < r[2] && r[1] < r[3]) {
                        t.set_partial([r[0], r[1]], image(r), TextureOptions::LINEAR);
                    }
                }
            }
            (fb.width as f32, fb.height as f32)
        };

        let id = self.texture.as_ref().unwrap().id();
        let paint = |ui: &egui::Ui, rect: Rect| {
            ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        };
        match mode {
            Scale::Fit => {
                let avail = ui.available_rect_before_wrap();
                let scale = (avail.width() / width).min(avail.height() / height);
                let rect = Rect::from_center_size(avail.center(), egui::vec2(width * scale, height * scale));
                paint(ui, rect);
                Viewport { rect, visible: rect, scale, width, height }
            }
            Scale::Actual => {
                let scale = 1.0 / ui.ctx().pixels_per_point();
                let size = egui::vec2(width * scale, height * scale);
                let avail = ui.available_size();
                // Dragging and the mouse wheel belong to the remote machine, so only the scroll bars scroll.
                egui::ScrollArea::both().auto_shrink(false).scroll_source(egui::scroll_area::ScrollSource::SCROLL_BAR).show(ui, |ui| {
                    let (area, _) = ui.allocate_exact_size(size.max(avail), egui::Sense::hover());
                    let rect = Rect::from_center_size(area.center(), size);
                    paint(ui, rect);
                    Viewport { rect, visible: ui.clip_rect(), scale, width, height }
                }).inner
            }
        }
    }
}
