//! Small painted pieces of the address book: label colours, tile wallpapers, filled star, chips.

use eframe::egui::{self, Color32, CornerRadius, Pos2, RichText, Stroke, epaint::Mesh};

use crate::shared::theme;

const LABEL_COLORS: [Color32; 8] = [
    Color32::from_rgb(0xf5, 0x9e, 0x0b),
    Color32::from_rgb(0x3b, 0x82, 0xf6),
    Color32::from_rgb(0xa8, 0x55, 0xf7),
    Color32::from_rgb(0x10, 0xb9, 0x81),
    Color32::from_rgb(0xec, 0x48, 0x99),
    Color32::from_rgb(0x14, 0xb8, 0xa6),
    Color32::from_rgb(0xef, 0x44, 0x44),
    Color32::from_rgb(0x64, 0x74, 0x8b),
];

const WALLPAPERS: [Color32; 6] = [
    Color32::from_rgb(0x1e, 0x4e, 0x79),
    Color32::from_rgb(0x3b, 0x2f, 0x5c),
    Color32::from_rgb(0x1f, 0x5f, 0x52),
    Color32::from_rgb(0x6b, 0x3a, 0x2e),
    Color32::from_rgb(0x3a, 0x4a, 0x5e),
    Color32::from_rgb(0x4a, 0x46, 0x36),
];

fn hash(s: &str) -> usize {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.to_lowercase().bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    h as usize
}

/// Stable colour for a label, so the sidebar dot and the tile chip always agree.
pub fn label_color(name: &str) -> Color32 {
    LABEL_COLORS[hash(name) % LABEL_COLORS.len()]
}

/// Stand-in desktop colour for a connection that has no saved thumbnail yet.
pub fn wallpaper(name: &str) -> Color32 {
    WALLPAPERS[hash(name) % WALLPAPERS.len()]
}

/// Filled five-point star (Lucide only has the outline).
pub fn star(painter: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(c, color);
    for i in 0..10 {
        let a = std::f32::consts::PI * (i as f32) / 5.0 - std::f32::consts::FRAC_PI_2;
        let rad = if i % 2 == 0 { r } else { r * 0.45 };
        mesh.colored_vertex(c + egui::vec2(a.cos(), a.sin()) * rad, color);
    }
    for i in 0..10u32 {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % 10);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Rounded toggle chip (SSH tunnel, View only, ...). Returns true when clicked.
pub fn chip(ui: &mut egui::Ui, icon: lucide_icons::Icon, label: &str, on: bool) -> bool {
    let p = theme::palette(ui.ctx());
    let (fg, fill, stroke) = if on {
        (
            p.accent_text,
            p.accent_soft,
            Stroke::new(1.0, theme::ACCENT),
        )
    } else {
        (p.text2, Color32::TRANSPARENT, Stroke::new(1.0, p.border))
    };
    let text = RichText::new(format!("{} {label}", char::from(icon)))
        .size(12.0)
        .color(fg);
    ui.add(
        egui::Button::new(text)
            .fill(fill)
            .stroke(stroke)
            .corner_radius(CornerRadius::same(15))
            .min_size(egui::vec2(0.0, 28.0)),
    )
    .clicked()
}

/// Keyboard key hint such as `F2`.
pub fn keycap(ui: &mut egui::Ui, key: &str) {
    let p = theme::palette(ui.ctx());
    egui::Frame::NONE
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(key).size(11.0).color(p.text3));
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_color_is_stable_and_case_blind() {
        assert_eq!(label_color("Lab"), label_color("lab"));
        assert_eq!(wallpaper("Linux"), wallpaper("linux"));
    }
}
