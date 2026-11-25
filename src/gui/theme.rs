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

/// macOS system colors: grouped gray backgrounds, white surfaces, hairline separators and the
/// system blue as the accent. Text colors are the accessible variants (at least 4.5:1).
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xF5, 0xF5, 0xF7),
    sidebar: Color32::from_rgb(0xEC, 0xEC, 0xF0),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    subtle: Color32::from_rgb(0xE8, 0xE8, 0xED),
    border: Color32::from_rgb(0xE2, 0xE2, 0xE7),
    control_border: Color32::from_rgb(0xC7, 0xC7, 0xCC),
    text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
    text_weak: Color32::from_rgb(0x6E, 0x6E, 0x73),
    accent: Color32::from_rgb(0x00, 0x71, 0xE3),
    accent_soft: Color32::from_rgb(0xDD, 0xEB, 0xFF),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x24, 0x8A, 0x3D),
    warning: Color32::from_rgb(0xC9, 0x34, 0x00),
    danger: Color32::from_rgb(0xD7, 0x00, 0x15),
    danger_soft: Color32::from_rgb(0xFF, 0xE9, 0xEA),
};

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x1C, 0x1C, 0x1E),
    sidebar: Color32::from_rgb(0x15, 0x15, 0x17),
    surface: Color32::from_rgb(0x2C, 0x2C, 0x2E),
    subtle: Color32::from_rgb(0x3A, 0x3A, 0x3C),
    border: Color32::from_rgb(0x3A, 0x3A, 0x3D),
    control_border: Color32::from_rgb(0x5A, 0x5A, 0x5F),
    text: Color32::from_rgb(0xF5, 0xF5, 0xF7),
    text_weak: Color32::from_rgb(0xA1, 0xA1, 0xA6),
    accent: Color32::from_rgb(0x0A, 0x84, 0xFF),
    accent_soft: Color32::from_rgb(0x12, 0x33, 0x5C),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x30, 0xD1, 0x58),
    warning: Color32::from_rgb(0xFF, 0x9F, 0x0A),
    danger: Color32::from_rgb(0xFF, 0x69, 0x61),
    danger_soft: Color32::from_rgb(0x3A, 0x1D, 0x1F),
};

pub fn palette(ui: &Ui) -> Palette {
    if ui.visuals().dark_mode { DARK } else { LIGHT }
}

/// Type scale: 13 (small), 14 (body), 16 (titles), 22 (page heading).
pub const TITLE_SIZE: f32 = 16.0;
pub const HEADING_SIZE: f32 = 22.0;

pub const RADIUS: u8 = 9;
pub const CARD_RADIUS: u8 = 16;
/// Hero controls (large buttons) and floating overlays (toasts): slightly rounder than inputs.
pub const FLOAT_RADIUS: u8 = 12;

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
pub(crate) fn install_fonts(ctx: &egui::Context) {
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
            // Hangul in the terminal; Latin stays in the monospace font.
            if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
                list.push("ui-regular".into());
            }
            bold.insert(0, "ui-bold".into());
        }
    }
    fonts.families.insert(bold_family(), bold);
    ctx.set_fonts(fonts);
}

/// How far right of the scrolled content the scroll bar's outer edge sits, in the margin every
/// scroll area has beside it (12 points or more): 7 points of space between content and bar.
const SCROLL_BAR_OFFSET: f32 = 12.0;
/// The scroll bar stays this long after the last scroll, then fades out over `SCROLL_BAR_FADE`.
const SCROLL_BAR_LINGER: f32 = 0.8;
const SCROLL_BAR_FADE: f32 = 0.3;
/// The knob's opacity while shown.
const SCROLL_BAR_OPACITY: f32 = 0.35;

/// Shows the scroll bar of the area under the pointer while it scrolls and fades it out after;
/// called every frame. Pointing at the bar itself still shows it, to drag it.
pub fn fade_scroll_bars(ctx: &egui::Context) {
    let idle = ctx.input(|i| i.time_since_last_scroll());
    let shown = (1.0 - (idle - SCROLL_BAR_LINGER) / SCROLL_BAR_FADE).clamp(0.0, 1.0);
    let opacity = SCROLL_BAR_OPACITY * shown;
    if (ctx.global_style().spacing.scroll.active_handle_opacity - opacity).abs() > f32::EPSILON {
        ctx.all_styles_mut(|style| style.spacing.scroll.active_handle_opacity = opacity);
    }
    // Keep drawing until it has faded.
    if shown > 0.0 {
        ctx.request_repaint();
    }
}

pub fn apply(ctx: &egui::Context) {
    install_fonts(ctx);
    // Icons are SVGs (see `widgets::Icon`).
    egui_extras::install_image_loaders(ctx);
    for (theme, pal) in [(Theme::Light, LIGHT), (Theme::Dark, DARK)] {
        ctx.style_mut_of(theme, |style| {
            // egui's own transitions (fades of areas and popups) match ours.
            style.animation_time = super::motion::BASE;

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
            // Overlay scroll bars like macOS: a thin translucent grey knob that widens under the
            // pointer, out in the margin beside the content rather than on it. It shows while
            // scrolling and fades once scrolling stops (`fade_scroll_bars`).
            s.scroll = egui::style::ScrollStyle {
                floating_width: 5.0,
                bar_width: 8.0,
                bar_outer_margin: -SCROLL_BAR_OFFSET,
                active_handle_opacity: 0.0,
                interact_handle_opacity: 0.5,
                active_background_opacity: 0.0,
                interact_background_opacity: 0.0,
                ..egui::style::ScrollStyle::floating()
            };

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
                Color32::from_rgb(0xDE, 0xDE, 0xE3)
            };
            w.hovered.weak_bg_fill = hover_fill;
            w.hovered.bg_fill = hover_fill;
            // Hovering changes the fill only; the accent outline is for keyboard focus.
            w.hovered.bg_stroke = Stroke::new(1.0, pal.control_border);
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
