//! Small reusable building blocks on top of egui, styled with the theme tokens.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Response, RichText, Sense, Stroke, TextEdit, Ui, Vec2,
};

use super::theme::{Bold as _, CARD_RADIUS, Palette, RADIUS, bold_family, palette};

/// Raised container with a border.
pub fn card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let pal = palette(ui);
    Frame::new()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

/// Colored banner (errors, notices).
pub fn banner<R>(ui: &mut Ui, fill: Color32, stroke: Color32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
}

pub fn section_title(ui: &mut Ui, title: &str, help: Option<&str>) {
    let pal = palette(ui);
    ui.label(RichText::new(title).size(15.0).bold().color(pal.text));
    if let Some(help) = help {
        ui.label(RichText::new(help).small().color(pal.text_weak));
    }
    ui.add_space(4.0);
}

pub fn primary_button(ui: &mut Ui, text: &str) -> Response {
    filled_button(ui, text, palette(ui).accent)
}

pub fn danger_button(ui: &mut Ui, text: &str) -> Response {
    filled_button(ui, text, palette(ui).danger)
}

pub fn large_primary_button(ui: &mut Ui, text: &str, width: f32) -> Response {
    let pal = palette(ui);
    let button = egui::Button::new(RichText::new(text).size(16.0).bold().color(pal.on_accent))
        .fill(pal.accent)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(RADIUS + 2))
        .min_size(Vec2::new(width, 44.0));
    hover_feedback(ui.add(button), ui)
}

fn filled_button(ui: &mut Ui, text: &str, fill: Color32) -> Response {
    let pal = palette(ui);
    let button = egui::Button::new(RichText::new(text).bold().color(pal.on_accent))
        .fill(fill)
        .stroke(Stroke::NONE)
        .min_size(Vec2::new(72.0, 32.0));
    hover_feedback(ui.add(button), ui)
}

/// egui keeps a custom button fill constant; lighten it on hover and darken it while pressed.
/// The overlay is white/black so the white label stays readable.
fn hover_feedback(response: Response, ui: &Ui) -> Response {
    if response.enabled() {
        let overlay = if response.is_pointer_button_down_on() {
            Some(Color32::from_black_alpha(36))
        } else if response.hovered() {
            Some(Color32::from_white_alpha(28))
        } else {
            None
        };
        if let Some(overlay) = overlay {
            ui.painter().rect_filled(response.rect, CornerRadius::same(RADIUS), overlay);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response
}

/// Quiet button without background until hovered.
pub fn ghost_button(ui: &mut Ui, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Button::new(text).frame_when_inactive(false))
}

const AVATAR_COLORS: [Color32; 8] = [
    Color32::from_rgb(0x25, 0x63, 0xEB),
    Color32::from_rgb(0x7C, 0x3A, 0xED),
    Color32::from_rgb(0xDB, 0x27, 0x77),
    Color32::from_rgb(0xEA, 0x58, 0x0C),
    Color32::from_rgb(0x05, 0x96, 0x69),
    Color32::from_rgb(0x08, 0x91, 0xB2),
    Color32::from_rgb(0x65, 0x73, 0x8B),
    Color32::from_rgb(0xCA, 0x8A, 0x04),
];

/// Circle with the first letter of `name`, colored by a stable hash of the name.
pub fn paint_avatar(ui: &Ui, center: egui::Pos2, radius: f32, name: &str) {
    let hash = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    let color = AVATAR_COLORS[hash as usize % AVATAR_COLORS.len()];
    ui.painter().circle_filled(center, radius, color);
    let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    ui.painter().text(
        center,
        Align2::CENTER_CENTER,
        initial,
        FontId::new(radius * 0.95, bold_family()),
        Color32::WHITE,
    );
}

pub fn avatar(ui: &mut Ui, size: f32, name: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_avatar(ui, rect.center(), size / 2.0, name);
}

/// Pill with a colored dot and text, sized to its content.
pub fn status_pill(ui: &mut Ui, color: Color32, text: &str) -> Response {
    let font = FontId::new(12.0, bold_family());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let (pad_x, pad_y, dot, gap) = (10.0, 5.0, 8.0, 6.0);
    let size = Vec2::new(pad_x * 2.0 + dot + gap + galley.size().x, pad_y * 2.0 + galley.size().y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(255), color.gamma_multiply(0.14));
    painter.circle_filled(egui::Pos2::new(rect.left() + pad_x + dot / 2.0, rect.center().y), dot / 2.0, color);
    let text_pos = egui::Pos2::new(rect.left() + pad_x + dot + gap, rect.center().y - galley.size().y / 2.0);
    painter.galley(text_pos, galley, color);
    response
}

/// Label above a field, optional error below it.
pub fn field<R>(ui: &mut Ui, label: &str, error: Option<&str>, add_field: impl FnOnce(&mut Ui) -> R) -> R {
    let pal = palette(ui);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(RichText::new(label).small().bold().color(pal.text_weak));
        let r = add_field(ui);
        if let Some(error) = error {
            ui.label(RichText::new(error).small().color(pal.danger));
        }
        r
    })
    .inner
}

/// Styled single-line text input; red border when `invalid`.
pub fn text_input<'t>(ui: &Ui, value: &'t mut String, hint: &str, width: f32, invalid: bool) -> TextEdit<'t> {
    let pal = palette(ui);
    let mut edit = TextEdit::singleline(value)
        .hint_text(RichText::new(hint).color(pal.text_weak.gamma_multiply(0.8)))
        .desired_width(width)
        .margin(Margin::symmetric(10, 7));
    if invalid {
        edit = edit.background_color(pal.danger_soft);
    }
    edit
}

/// Two or more mutually exclusive options shown as one segmented control.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let pal = palette(ui);
    let mut changed = false;
    Frame::new()
        .fill(pal.subtle)
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (option, label) in options {
                    let selected = *value == *option;
                    let text = if selected {
                        RichText::new(*label).bold().color(pal.text)
                    } else {
                        RichText::new(*label).color(pal.text_weak)
                    };
                    let mut button = egui::Button::new(text)
                        .corner_radius(CornerRadius::same(RADIUS - 2))
                        .min_size(Vec2::new(96.0, 28.0));
                    button = if selected {
                        button.fill(pal.surface).stroke(Stroke::new(1.0, pal.border))
                    } else {
                        button.frame_when_inactive(false)
                    };
                    if ui.add(button).clicked() && !selected {
                        *value = *option;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Key/value row used in detail cards.
pub fn info_row(ui: &mut Ui, pal: &Palette, key: &str, value: &str) {
    ui.label(RichText::new(key).color(pal.text_weak));
    ui.label(RichText::new(value).color(pal.text));
    ui.end_row();
}

/// Vector icons (egui's bundled fonts have no emoji).
#[derive(Clone, Copy)]
pub enum Icon {
    Laptop,
    Gateway,
    Monitor,
    Check,
}

pub fn paint_icon(painter: &egui::Painter, icon: Icon, center: egui::Pos2, size: f32, color: Color32) {
    use egui::{Pos2, Rect};
    let s = size;
    let stroke = Stroke::new((s / 11.0).max(1.4), color);
    let r = |x0: f32, y0: f32, x1: f32, y1: f32| {
        Rect::from_min_max(center + Vec2::new(x0 * s, y0 * s), center + Vec2::new(x1 * s, y1 * s))
    };
    let p = |x: f32, y: f32| center + Vec2::new(x * s, y * s);
    match icon {
        Icon::Laptop => {
            painter.rect_stroke(r(-0.36, -0.30, 0.36, 0.16), CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            painter.line_segment([p(-0.48, 0.28), p(0.48, 0.28)], stroke);
        }
        Icon::Gateway => {
            // Server box with a lock shackle on top.
            painter.rect_stroke(r(-0.32, -0.06, 0.32, 0.36), CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            let shackle: Vec<Pos2> = (0..=12)
                .map(|i| {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 12.0);
                    p(0.18 * a.cos(), -0.06 + 0.24 * a.sin())
                })
                .collect();
            painter.add(egui::Shape::line(shackle, stroke));
            painter.circle_filled(p(0.0, 0.15), s * 0.05, color);
        }
        Icon::Monitor => {
            painter.rect_stroke(r(-0.40, -0.32, 0.40, 0.18), CornerRadius::same(2), stroke, egui::StrokeKind::Middle);
            painter.line_segment([p(0.0, 0.18), p(0.0, 0.32)], stroke);
            painter.line_segment([p(-0.18, 0.34), p(0.18, 0.34)], stroke);
        }
        Icon::Check => {
            painter.add(egui::Shape::line(vec![p(-0.30, 0.0), p(-0.08, 0.22), p(0.32, -0.20)], stroke));
        }
    }
}

pub fn icon(ui: &mut Ui, icon: Icon, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_icon(ui.painter(), icon, rect.center(), size, color);
}
