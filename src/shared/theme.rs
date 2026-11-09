//! Light and dark design tokens (amber accent, navy neutrals) and the egui visuals built from them.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, Margin, RichText, Shadow, Stroke, Theme, ThemePreference,
};

/// Brand accent, identical in both themes; text on it is always `ON_ACCENT`.
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0xbe, 0x3d);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x1b, 0x13, 0x00);

/// One theme's colours. Same names in light and dark; only values change.
pub struct Palette {
    /// Window background behind panels.
    pub bg: Color32,
    /// Panels: top bar, sidebar, status bar.
    pub surface: Color32,
    /// Cards, dialogs, menus.
    pub card: Color32,
    /// Hovered or selected card, chips, segmented-control track.
    pub elevated: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub text3: Color32,
    /// Accent used as text or icon colour (darker amber in light mode for 4.5:1 contrast).
    pub accent_text: Color32,
    pub accent_soft: Color32,
    pub input_bg: Color32,
    pub success: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0b, 0x10, 0x1a),
    surface: Color32::from_rgb(0x10, 0x16, 0x22),
    card: Color32::from_rgb(0x18, 0x21, 0x33),
    elevated: Color32::from_rgb(0x1f, 0x2b, 0x42),
    border: Color32::from_rgb(0x24, 0x30, 0x49),
    border_strong: Color32::from_rgb(0x33, 0x41, 0x5c),
    text: Color32::from_rgb(0xe8, 0xed, 0xf5),
    text2: Color32::from_rgb(0xa3, 0xb1, 0xc6),
    text3: Color32::from_rgb(0x83, 0x91, 0xa8),
    accent_text: ACCENT,
    accent_soft: Color32::from_rgb(0x2e, 0x2a, 0x22),
    input_bg: Color32::from_rgb(0x0b, 0x10, 0x1a),
    success: Color32::from_rgb(0x34, 0xd3, 0x99),
    danger: Color32::from_rgb(0xf8, 0x71, 0x71),
    danger_soft: Color32::from_rgb(0x2e, 0x1a, 0x22),
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf3, 0xf5, 0xf9),
    surface: Color32::WHITE,
    card: Color32::WHITE,
    elevated: Color32::from_rgb(0xee, 0xf2, 0xf7),
    border: Color32::from_rgb(0xe3, 0xe8, 0xef),
    border_strong: Color32::from_rgb(0xcb, 0xd5, 0xe1),
    text: Color32::from_rgb(0x0f, 0x17, 0x2a),
    text2: Color32::from_rgb(0x47, 0x55, 0x69),
    text3: Color32::from_rgb(0x64, 0x74, 0x8b),
    accent_text: Color32::from_rgb(0x9a, 0x5b, 0x00),
    accent_soft: Color32::from_rgb(0xff, 0xf5, 0xdb),
    input_bg: Color32::from_rgb(0xf8, 0xfa, 0xfc),
    success: Color32::from_rgb(0x16, 0xa3, 0x4a),
    danger: Color32::from_rgb(0xdc, 0x26, 0x26),
    danger_soft: Color32::from_rgb(0xfe, 0xf2, 0xf2),
};

/// The palette of the theme egui is currently painting with.
pub fn palette(ctx: &egui::Context) -> &'static Palette {
    match ctx.theme() {
        Theme::Dark => &DARK,
        Theme::Light => &LIGHT,
    }
}

/// Lucide icon font plus light and dark visuals. Leaves the theme preference alone,
/// so the choice made with [`toggle`] survives restarts through egui's persisted memory.
pub fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "lucide".into(),
        Arc::new(
            egui::FontData::from_static(lucide_icons::LUCIDE_FONT_BYTES).tweak(egui::FontTweak {
                y_offset_factor: 0.08,
                ..Default::default()
            }),
        ),
    );
    if let Some(prop) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        prop.insert(1, "lucide".into());
    }
    if let Some(mono) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        mono.push("lucide".into());
    }
    ctx.set_fonts(fonts);
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, egui::Visuals::dark()));
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, egui::Visuals::light()));
    ctx.all_styles_mut(|s| {
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.menu_margin = Margin::same(6);
        s.spacing.window_margin = Margin::same(14);
    });
}

fn visuals(p: &Palette, mut v: egui::Visuals) -> egui::Visuals {
    let r = CornerRadius::same(8);
    v.override_text_color = None;
    v.weak_text_color = Some(p.text3);
    v.hyperlink_color = p.accent_text;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, ON_ACCENT);
    v.panel_fill = p.surface;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.extreme_bg_color = p.input_bg;
    v.text_edit_bg_color = Some(p.input_bg);
    v.faint_bg_color = p.elevated;
    v.code_bg_color = p.elevated;
    v.error_fg_color = p.danger;
    let shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: Color32::from_black_alpha(if p.text == DARK.text { 110 } else { 28 }),
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.card;
    w.noninteractive.weak_bg_fill = p.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = p.elevated;
    w.inactive.weak_bg_fill = p.elevated;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text2);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = p.elevated;
    w.hovered.weak_bg_fill = p.elevated;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.bg_fill = p.border_strong;
    w.active.weak_bg_fill = p.border_strong;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.5, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open = w.hovered;
    v
}

/// Amber call-to-action button with dark text.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text).strong().color(ON_ACCENT)).fill(ACCENT)
}

/// Red-outlined button for leaving or ending a session.
pub fn danger(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let p = palette(ui.ctx());
    egui::Button::new(RichText::new(text).strong().color(p.danger))
        .fill(p.danger_soft)
        .stroke(Stroke::new(1.0, p.danger.gamma_multiply(0.6)))
}

/// Segmented control: a track with one raised segment per item. Returns true when the value changed.
/// Items read in the given order even when the caller sits in a right-to-left layout.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut egui::Ui,
    value: &mut T,
    items: &[(T, String, &str)],
) -> bool {
    let p = palette(ui.ctx());
    let mut changed = false;
    let rtl = ui.layout().prefer_right_to_left();
    egui::Frame::NONE
        .fill(p.elevated)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let ordered: Vec<_> = if rtl {
                    items.iter().rev().collect()
                } else {
                    items.iter().collect()
                };
                for (v, label, tip) in ordered {
                    let on = *value == *v;
                    let text = RichText::new(label).color(if on { p.text } else { p.text3 });
                    let btn = egui::Button::new(text)
                        .fill(if on { p.card } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(7))
                        .min_size(egui::vec2(34.0, 28.0));
                    if ui.add(btn).on_hover_text(*tip).clicked() && !on {
                        *value = *v;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Sun | moon switch; the choice is remembered across restarts.
pub fn switch(ui: &mut egui::Ui) {
    use lucide_icons::Icon;
    let mut theme = ui.ctx().theme();
    let items = [
        (
            Theme::Light,
            char::from(Icon::Sun).to_string(),
            "Light theme",
        ),
        (
            Theme::Dark,
            char::from(Icon::Moon).to_string(),
            "Dark theme",
        ),
    ];
    if segmented(ui, &mut theme, &items) {
        ui.ctx().set_theme(ThemePreference::from(theme));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn lum(c: Color32) -> f32 {
        let ch = |v: u8| {
            let s = v as f32 / 255.0;
            if s <= 0.040_45 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn text_tokens_meet_wcag_aa_on_their_surfaces() {
        for p in [&DARK, &LIGHT] {
            for bg in [p.surface, p.card] {
                for fg in [p.text, p.text2, p.text3, p.accent_text] {
                    assert!(contrast(fg, bg) >= 4.5, "{fg:?} on {bg:?}");
                }
            }
        }
        assert!(contrast(ON_ACCENT, ACCENT) >= 4.5);
    }
}
