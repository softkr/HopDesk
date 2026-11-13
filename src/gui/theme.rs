//! Design tokens and the egui style built from them (follows the system light/dark setting).

use eframe::egui::{self, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, Ui, Vec2};

/// Semantic colors for one theme.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Main content background.
    pub bg: Color32,
    /// Sidebar background.
    pub sidebar: Color32,
    /// Raised surfaces (cards, inputs).
    pub surface: Color32,
    /// Quiet fill for secondary buttons / hovered rows.
    pub subtle: Color32,
    pub border: Color32,
    /// Outline of inputs and buttons (at least 3:1 against `surface`).
    pub control_border: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
    /// Tinted background for the selected row / info banners.
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
}

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xF7, 0xF8, 0xFA),
    sidebar: Color32::from_rgb(0xEF, 0xF1, 0xF4),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    subtle: Color32::from_rgb(0xE6, 0xE9, 0xEE),
    border: Color32::from_rgb(0xDC, 0xE0, 0xE6),
    control_border: Color32::from_rgb(0x8A, 0x92, 0x9D),
    text: Color32::from_rgb(0x1F, 0x23, 0x28),
    text_weak: Color32::from_rgb(0x65, 0x6D, 0x78),
    accent: Color32::from_rgb(0x18, 0x64, 0x72),
    accent_soft: Color32::from_rgb(0xD9, 0xEC, 0xEF),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x16, 0xA3, 0x4A),
    warning: Color32::from_rgb(0xD9, 0x77, 0x06),
    danger: Color32::from_rgb(0xDC, 0x26, 0x26),
    danger_soft: Color32::from_rgb(0xFD, 0xEC, 0xEC),
};

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x1B, 0x1D, 0x21),
    sidebar: Color32::from_rgb(0x15, 0x17, 0x1A),
    surface: Color32::from_rgb(0x24, 0x27, 0x2C),
    subtle: Color32::from_rgb(0x2E, 0x32, 0x38),
    border: Color32::from_rgb(0x36, 0x3B, 0x42),
    control_border: Color32::from_rgb(0x6E, 0x77, 0x83),
    text: Color32::from_rgb(0xE6, 0xE8, 0xEB),
    text_weak: Color32::from_rgb(0x9A, 0xA1, 0xAB),
    accent: Color32::from_rgb(0x4F, 0xC3, 0xC6),
    accent_soft: Color32::from_rgb(0x17, 0x3A, 0x42),
    on_accent: Color32::from_rgb(0x08, 0x25, 0x2C),
    success: Color32::from_rgb(0x3F, 0xB9, 0x50),
    warning: Color32::from_rgb(0xE3, 0xA0, 0x08),
    danger: Color32::from_rgb(0xFF, 0x7B, 0x72),
    danger_soft: Color32::from_rgb(0x3A, 0x1F, 0x21),
};

pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

/// Type scale: 13 (small), 14 (body), 16 (titles), 22 (page heading).
pub const TITLE_SIZE: f32 = 16.0;
pub const HEADING_SIZE: f32 = 22.0;

pub const RADIUS: u8 = 8;
pub const CARD_RADIUS: u8 = 12;

pub fn bold_family() -> FontFamily {
    FontFamily::Name("bold".into())
}

/// Semibold text (egui's `strong()` only changes the color).
pub trait Bold {
    fn bold(self) -> Self;
}

impl Bold for egui::RichText {
    fn bold(self) -> Self {
        self.family(bold_family())
    }
}

/// Uses the OS Korean UI font (Apple SD Gothic Neo, Malgun Gothic, Noto Sans CJK) for Latin and
/// Hangul alike, so weights match. Without one, egui's bundled font is used and bold maps to regular.
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let mut bold = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if let Some(font) = super::platform::ui_font() {
        let mut blobs: Vec<(std::path::PathBuf, egui::epaint::text::Blob)> = Vec::new();
        let mut load = |path: &std::path::PathBuf| -> Option<egui::epaint::text::Blob> {
            if let Some((_, blob)) = blobs.iter().find(|(p, _)| p == path) {
                return Some(blob.clone());
            }
            let blob: egui::epaint::text::Blob = std::sync::Arc::new(std::fs::read(path).ok()?);
            blobs.push((path.clone(), blob.clone()));
            Some(blob)
        };
        if let (Some(regular), Some(semibold)) = (load(&font.regular.0), load(&font.bold.0)) {
            fonts
                .font_data
                .insert("ui-regular".into(), egui::FontData::from_blob(regular, font.regular.1).into());
            fonts
                .font_data
                .insert("ui-bold".into(), egui::FontData::from_blob(semibold, font.bold.1).into());
            if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
                list.insert(0, "ui-regular".into());
            }
            bold.insert(0, "ui-bold".into());
        }
    }
    fonts.families.insert(bold_family(), bold);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    for (theme, pal) in [(Theme::Light, LIGHT), (Theme::Dark, DARK)] {
        ctx.style_mut_of(theme, |style| {
            style.text_styles = [
                (TextStyle::Small, FontId::new(13.0, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Heading, FontId::new(22.0, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
            ]
            .into();

            // Labels are plain UI text, not selectable document text: dragging across the
            // window must not select words like on a web page. Inputs stay selectable.
            style.interaction.selectable_labels = false;
            style.interaction.multi_widget_text_select = false;

            let s = &mut style.spacing;
            s.item_spacing = Vec2::new(8.0, 8.0);
            s.button_padding = Vec2::new(14.0, 7.0);
            s.interact_size.y = 32.0;
            s.menu_margin = Margin::same(6);

            let v = &mut style.visuals;
            v.panel_fill = pal.bg;
            v.window_fill = pal.surface;
            v.window_stroke = Stroke::new(1.0, pal.border);
            v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
            v.menu_corner_radius = CornerRadius::same(RADIUS);
            v.extreme_bg_color = pal.surface;
            v.text_edit_bg_color = Some(pal.surface);
            v.faint_bg_color = pal.subtle;
            v.hyperlink_color = pal.accent;
            v.warn_fg_color = pal.warning;
            v.error_fg_color = pal.danger;
            v.selection.bg_fill = pal.accent.gamma_multiply(0.35);
            v.selection.stroke = Stroke::new(1.5, pal.accent);

            let radius = CornerRadius::same(RADIUS);
            let w = &mut v.widgets;
            w.noninteractive.bg_stroke = Stroke::new(1.0, pal.border);
            w.noninteractive.fg_stroke = Stroke::new(1.0, pal.text);
            w.noninteractive.corner_radius = radius;

            w.inactive.weak_bg_fill = pal.subtle;
            w.inactive.bg_fill = pal.subtle;
            w.inactive.bg_stroke = Stroke::new(1.0, pal.control_border);
            w.inactive.fg_stroke = Stroke::new(1.0, pal.text);
            w.inactive.corner_radius = radius;

            let hover_fill = if pal.bg == DARK.bg {
                pal.subtle.gamma_multiply(1.35)
            } else {
                Color32::from_rgb(0xDC, 0xE1, 0xE8)
            };
            w.hovered.weak_bg_fill = hover_fill;
            w.hovered.bg_fill = hover_fill;
            w.hovered.bg_stroke = Stroke::new(1.0, pal.accent);
            w.hovered.fg_stroke = Stroke::new(1.5, pal.text);
            w.hovered.corner_radius = radius;
            w.hovered.expansion = 0.0;

            w.active.weak_bg_fill = hover_fill;
            w.active.bg_fill = hover_fill;
            w.active.bg_stroke = Stroke::new(1.0, pal.accent);
            w.active.fg_stroke = Stroke::new(1.5, pal.text);
            w.active.corner_radius = radius;
            w.active.expansion = 0.0;

            w.open.weak_bg_fill = pal.subtle;
            w.open.bg_fill = pal.subtle;
            w.open.corner_radius = radius;
        });
    }
}
