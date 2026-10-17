use std::sync::Mutex;

use eframe::egui::{self, Color32, ColorImage, Pos2, Rect, TextureHandle, TextureOptions};

use super::Framebuffer;

/// Where the remote screen is drawn locally; maps local points to remote pixels.
pub struct Viewport {
    rect: Rect,
    scale: f32,
    width: f32,
    height: f32,
}

impl Viewport {
    pub fn contains(&self, p: Pos2) -> bool {
        self.rect.contains(p)
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
    /// Paints the screen scaled to fit the available space.
    pub fn show(&mut self, ui: &mut egui::Ui, screen: &Mutex<Framebuffer>) -> Viewport {
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

        let avail = ui.available_rect_before_wrap();
        let scale = (avail.width() / width).min(avail.height() / height);
        let rect = Rect::from_center_size(avail.center(), egui::vec2(width * scale, height * scale));
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        ui.painter().image(self.texture.as_ref().unwrap().id(), rect, uv, Color32::WHITE);
        Viewport { rect, scale, width, height }
    }
}
