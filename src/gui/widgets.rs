//! Small reusable building blocks on top of egui, styled with the theme tokens.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Frame, Margin, Response, RichText, Sense, Stroke, TextEdit, Ui, Vec2,
};

use super::motion;
use super::theme::{Bold as _, CARD_RADIUS, FLOAT_RADIUS, Palette, RADIUS, TITLE_SIZE, bold_family, palette};

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
    ui.label(RichText::new(title).size(TITLE_SIZE).bold().color(pal.text));
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

/// The one call to action of a screen: an ink-colored button (near black in light mode, near white
/// in dark mode) with its icon, label and, when it has one, the keyboard shortcut as a keycap.
pub fn hero_button(ui: &mut Ui, icon: Icon, text: &str, shortcut: Option<&str>, width: f32) -> Response {
    const HEIGHT: f32 = 46.0;
    const GAP: f32 = 10.0;
    let pal = palette(ui);
    let (ink, on_ink) = (pal.text, pal.surface);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, HEIGHT), Sense::click());
    let hover = motion::toggle(ui.ctx(), response.id.with("hover"), response.hovered(), motion::FAST);
    // Toward the mid gray: lighter on the dark ink, darker on the light one.
    let mut fill = ink.lerp_to_gamma(pal.text_weak, 0.22 * hover);
    if response.is_pointer_button_down_on() {
        fill = ink.lerp_to_gamma(pal.text_weak, 0.4);
    }
    let painter = ui.painter();
    let radius = CornerRadius::same(FLOAT_RADIUS);
    painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 10, spread: 0, color: Color32::from_black_alpha(36) }.as_shape(rect, radius));
    painter.rect_filled(rect, radius, fill);

    let label = painter.layout_no_wrap(text.to_owned(), FontId::new(15.0, bold_family()), on_ink);
    let key = shortcut.map(|k| keycap_layout(ui, k, on_ink.gamma_multiply(0.75)));
    let key_width = key.as_ref().map_or(0.0, |(_, size)| GAP + size.x);
    let total = ICON_MD + 8.0 + label.size().x + key_width;
    let mut x = rect.center().x - total / 2.0;
    paint_icon(ui, icon, egui::Pos2::new(x + ICON_MD / 2.0, rect.center().y), ICON_MD, on_ink);
    x += ICON_MD + 8.0;
    let label_width = label.size().x;
    painter.galley(egui::Pos2::new(x, rect.center().y - label.size().y / 2.0), label, on_ink);
    if let Some((galley, size)) = key {
        let key_rect = egui::Rect::from_min_size(egui::Pos2::new(x + label_width + GAP, rect.center().y - size.y / 2.0), size);
        paint_keycap(ui, key_rect, galley, on_ink.gamma_multiply(0.12), on_ink.gamma_multiply(0.22));
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// A keyboard shortcut drawn as a keycap, e.g. `⌘F` inside the search field.
fn keycap_layout(ui: &Ui, text: &str, color: Color32) -> (std::sync::Arc<egui::Galley>, Vec2) {
    const PAD: Vec2 = Vec2::new(6.0, 2.0);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(11.5), color);
    let size = galley.size() + PAD * 2.0;
    (galley, size)
}

fn paint_keycap(ui: &Ui, rect: egui::Rect, galley: std::sync::Arc<egui::Galley>, fill: Color32, stroke: Color32) {
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(5), fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let pos = rect.center() - galley.size() / 2.0;
    painter.galley(pos, galley, Color32::PLACEHOLDER);
}

/// Card heading: an icon and a title, with optional live details on the right.
pub fn card_header(ui: &mut Ui, icon: Icon, title: &str, right: impl FnOnce(&mut Ui)) {
    let pal = palette(ui);
    ui.horizontal(|ui| {
        ui.add(icon.image(ICON_MD, pal.text));
        ui.add_space(4.0);
        ui.label(RichText::new(title).size(TITLE_SIZE).bold().color(pal.text));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
    });
}

fn filled_button(ui: &mut Ui, text: &str, fill: Color32) -> Response {
    let pal = palette(ui);
    let button = egui::Button::new(RichText::new(text).bold().color(pal.on_accent))
        .fill(fill)
        .stroke(Stroke::NONE)
        .min_size(Vec2::new(72.0, 32.0));
    hover_feedback(ui.add(button), ui)
}

/// egui keeps a custom button fill constant; lighten it on hover (fading in and out) and darken it
/// at once while pressed. The overlay is white/black so the white label stays readable.
fn hover_feedback(response: Response, ui: &Ui) -> Response {
    if response.enabled() {
        let hover = motion::toggle(ui.ctx(), response.id.with("hover"), response.hovered(), motion::FAST);
        let overlay = if response.is_pointer_button_down_on() {
            Color32::from_black_alpha(36)
        } else {
            Color32::from_white_alpha(28).gamma_multiply(hover)
        };
        if overlay.a() > 0 {
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
    Color32::from_rgb(0x19, 0x40, 0x51),
    Color32::from_rgb(0x18, 0x64, 0x72),
    Color32::from_rgb(0x3D, 0x6C, 0x8E),
    Color32::from_rgb(0x5B, 0x62, 0x8F),
    Color32::from_rgb(0x7A, 0x5C, 0x86),
    Color32::from_rgb(0x8A, 0x5A, 0x5A),
    Color32::from_rgb(0x8C, 0x6D, 0x3F),
    Color32::from_rgb(0x4E, 0x7A, 0x5E),
];

/// Same hues, brightened so the discs stay visible on a dark sidebar.
const AVATAR_COLORS_DARK: [Color32; 8] = [
    Color32::from_rgb(0x2E, 0x6E, 0x85),
    Color32::from_rgb(0x2B, 0x82, 0x96),
    Color32::from_rgb(0x4E, 0x87, 0xAC),
    Color32::from_rgb(0x70, 0x78, 0xB0),
    Color32::from_rgb(0x96, 0x73, 0xA3),
    Color32::from_rgb(0xA8, 0x70, 0x70),
    Color32::from_rgb(0xAA, 0x85, 0x56),
    Color32::from_rgb(0x5E, 0x96, 0x76),
];

/// The avatar color of `name`, from a stable hash of the name.
fn avatar_color(ui: &Ui, name: &str) -> Color32 {
    // FNV-1a spreads similar names over the palette better than a plain multiply-add.
    let hash = name.bytes().fold(0x811C_9DC5u32, |h, b| (h ^ u32::from(b)).wrapping_mul(0x0100_0193));
    let colors = if ui.visuals().dark_mode {
        &AVATAR_COLORS_DARK
    } else {
        &AVATAR_COLORS
    };
    colors[(hash >> 16) as usize % colors.len()]
}

/// Up to two letters standing for `name`: the first two syllables of a Korean name ("사무실 PC" →
/// "사무"), the initials of a Latin one ("Lab Windows 11" → "LW"), or a short acronym as is ("NAS").
pub fn monogram(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let Some(first) = words.first() else { return String::new() };
    if !first.chars().next().is_some_and(|c| c.is_ascii()) {
        return first.chars().take(2).collect();
    }
    let acronym = first.chars().count() <= 3 && first.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if acronym {
        return (*first).to_owned();
    }
    words.iter().take(2).filter_map(|w| w.chars().next()).filter(char::is_ascii_alphanumeric).collect::<String>().to_uppercase()
}

/// Rounded square with the monogram of `name` in its avatar hue: solid with white letters, or a
/// light tint with letters in the hue (sidebar rows that are not selected).
pub fn paint_monogram(ui: &Ui, rect: egui::Rect, name: &str, solid: bool) {
    let hue = avatar_color(ui, name);
    let letters = monogram(name);
    let (fill, text) = if solid { (hue, Color32::WHITE) } else { (hue.gamma_multiply(0.16), hue) };
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same((rect.width() * 0.28) as u8), fill);
    let scale = if letters.chars().count() > 1 { 0.36 } else { 0.46 };
    let font = FontId::new(rect.width() * scale, bold_family());
    painter.text(rect.center(), Align2::CENTER_CENTER, letters, font, text);
}

/// The detail header's identity tile: the solid monogram with the session's status dot, ringed in
/// the card color, on its bottom-right corner.
pub fn avatar_tile(ui: &mut Ui, size: f32, name: &str, status: Option<Color32>) {
    let pal = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_monogram(ui, rect, name, true);
    if let Some(color) = status {
        let center = rect.right_bottom() - Vec2::splat(size * 0.06);
        ui.painter().circle(center, size * 0.12, color, Stroke::new(3.0, pal.surface));
    }
}

/// Small rounded label naming a property of the connection, e.g. its protocol or route. Painted
/// at its own size: a layout row would stretch it to a button's height.
pub fn tag(ui: &mut Ui, text: &str, color: Color32, fill: Color32) {
    let font = FontId::proportional(12.0);
    let size = tag_size(ui, text, &font);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_tag(ui, rect, text, font, color, fill, palette(ui).border);
}

const TAG_PAD: Vec2 = Vec2::new(8.0, 3.0);

pub fn tag_size(ui: &Ui, text: &str, font: &FontId) -> Vec2 {
    ui.painter().layout_no_wrap(text.to_owned(), font.clone(), Color32::PLACEHOLDER).size() + TAG_PAD * 2.0
}

/// Paints a tag (see [`tag`]) into `rect`, e.g. one centered on a link of the route diagram.
pub fn paint_tag(ui: &Ui, rect: egui::Rect, text: &str, font: FontId, color: Color32, fill: Color32, stroke: Color32) {
    let painter = ui.painter();
    painter.rect(rect, CornerRadius::same(6), fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    let galley = painter.layout_no_wrap(text.to_owned(), font, color);
    painter.galley(rect.center() - galley.size() / 2.0, galley, color);
}

/// Outlined button with a leading icon, for a header's actions. Its border turns accent on hover.
pub fn outline_button(pal: &Palette, icon: Icon, text: &str) -> egui::Button<'static> {
    labeled_button(icon, text, pal.text).fill(pal.surface).min_size(Vec2::new(0.0, 34.0))
}

/// Outlined button in the danger color for ending something, e.g. a session.
pub fn danger_outline_button(pal: &Palette, icon: Icon, text: &str) -> egui::Button<'static> {
    labeled_button(icon, text, pal.danger)
        .fill(pal.danger_soft)
        .stroke(Stroke::new(1.0, pal.danger.gamma_multiply(0.45)))
        .min_size(Vec2::new(0.0, 34.0))
}

/// Pill with a colored dot and text, sized to its content. The dot carries the color; the text
/// stays `pal.text` so it passes contrast on the tinted background.
pub fn status_pill(ui: &mut Ui, color: Color32, text: &str) -> Response {
    let pal = palette(ui);
    let font = FontId::new(13.0, bold_family());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, pal.text);
    let (pad_x, pad_y, dot, gap) = (10.0, 5.0, 8.0, 6.0);
    let size = Vec2::new(pad_x * 2.0 + dot + gap + galley.size().x, pad_y * 2.0 + galley.size().y);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(255), color.gamma_multiply(0.14));
    painter.circle_filled(egui::Pos2::new(rect.left() + pad_x + dot / 2.0, rect.center().y), dot / 2.0, color);
    let text_pos = egui::Pos2::new(rect.left() + pad_x + dot + gap, rect.center().y - galley.size().y / 2.0);
    painter.galley(text_pos, galley, pal.text);
    response
}

/// Square quiet button showing an icon; the background and the icon's contrast fade in on hover,
/// and the icon gives a little while pressed.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32) -> Response {
    const PRESS_SCALE: f32 = 0.08;
    let pal = palette(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let ctx = ui.ctx();
    let hover = motion::toggle(ctx, response.id.with("hover"), response.hovered(), motion::FAST);
    let press = motion::toggle(ctx, response.id.with("press"), response.is_pointer_button_down_on(), motion::FAST);
    if hover > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), pal.subtle.gamma_multiply(hover));
    }
    if press > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), Color32::from_black_alpha(20).gamma_multiply(press));
    }
    let color = pal.text_weak.lerp_to_gamma(pal.text, hover);
    let scale = 1.0 - PRESS_SCALE * press * motion::travel();
    paint_icon_scaled(ui, icon, rect.center(), (size * 0.6).round(), scale, color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// [`icon_button`] with a label after the icon: the same height, hover fill and contrast, for a
/// header action that sits next to one.
pub fn quiet_button(ui: &mut Ui, icon: Icon, text: &str, height: f32) -> Response {
    const PAD_X: f32 = 8.0;
    const GAP: f32 = 6.0;
    let pal = palette(ui);
    let label = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(13.5), Color32::PLACEHOLDER);
    let width = PAD_X * 2.0 + ICON_SM + GAP + label.size().x;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    let ctx = ui.ctx();
    let hover = motion::toggle(ctx, response.id.with("hover"), response.hovered(), motion::FAST);
    let press = motion::toggle(ctx, response.id.with("press"), response.is_pointer_button_down_on(), motion::FAST);
    if hover > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), pal.subtle.gamma_multiply(hover));
    }
    if press > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(RADIUS), Color32::from_black_alpha(20).gamma_multiply(press));
    }
    let color = pal.text_weak.lerp_to_gamma(pal.text, hover);
    let icon_center = egui::Pos2::new(rect.left() + PAD_X + ICON_SM / 2.0, rect.center().y);
    paint_icon(ui, icon, icon_center, ICON_SM, color);
    let text_pos = egui::Pos2::new(rect.left() + PAD_X + ICON_SM + GAP, rect.center().y - label.size().y / 2.0);
    ui.painter().galley(text_pos, label, color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// Round tinted disc with a centered icon, for empty states and hero illustrations.
pub fn icon_badge(ui: &mut Ui, icon: Icon, diameter: f32) {
    let pal = palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::hover());
    ui.painter().circle_filled(rect.center(), diameter / 2.0, pal.accent_soft);
    paint_icon(ui, icon, rect.center(), (diameter * 0.5).round(), pal.accent);
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

/// Styled single-line text input; red outline when `invalid`.
pub struct Input<'t> {
    edit: TextEdit<'t>,
    invalid: bool,
    icon: Option<Icon>,
    shortcut: Option<String>,
}

impl<'t> Input<'t> {
    pub fn password(mut self, password: bool) -> Self {
        self.edit = self.edit.password(password);
        self
    }

    pub fn id(mut self, id: egui::Id) -> Self {
        self.edit = self.edit.id(id);
        self
    }

    /// Shows `icon` inside the field, before the text.
    pub fn leading_icon(mut self, icon: Icon) -> Self {
        self.edit = self.edit.margin(Margin { left: 32, ..INPUT_MARGIN });
        self.icon = Some(icon);
        self
    }

    /// Shows the shortcut that focuses the field as a keycap at its right end, while it is empty.
    pub fn shortcut(mut self, keys: String) -> Self {
        self.shortcut = Some(keys);
        self
    }
}

impl egui::Widget for Input<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let pal = palette(ui);
        let danger = pal.danger;
        let response = ui.add(self.edit);
        if let Some(icon) = self.icon {
            let center = egui::Pos2::new(response.rect.left() + 17.0, response.rect.center().y);
            paint_icon(ui, icon, center, ICON_SM, pal.text_weak);
        }
        if let Some(keys) = self.shortcut
            && !response.has_focus()
        {
            let (galley, size) = keycap_layout(ui, &keys, pal.text_weak);
            let min = egui::Pos2::new(response.rect.right() - 8.0 - size.x, response.rect.center().y - size.y / 2.0);
            paint_keycap(ui, egui::Rect::from_min_size(min, size), galley, pal.bg, pal.border);
        }
        if self.invalid && !response.has_focus() {
            ui.painter().rect_stroke(
                response.rect,
                CornerRadius::same(RADIUS),
                Stroke::new(1.5, danger),
                egui::StrokeKind::Inside,
            );
        }
        response
    }
}

const INPUT_MARGIN: Margin = Margin::symmetric(10, 7);

pub fn text_input<'t>(ui: &Ui, value: &'t mut String, hint: &str, width: f32, invalid: bool) -> Input<'t> {
    let pal = palette(ui);
    let edit = TextEdit::singleline(value)
        .hint_text(RichText::new(hint).color(pal.text_weak))
        .desired_width(width)
        .margin(INPUT_MARGIN);
    Input { edit, invalid, icon: None, shortcut: None }
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

/// Icon sizes: inline with body text, standalone controls, hero spots.
pub const ICON_SM: f32 = 16.0;
pub const ICON_MD: f32 = 20.0;

/// Lucide icons (assets/icons/lucide, ISC; fetched by scripts/fetch-icons.sh). They share one
/// 24 px grid and 2 px stroke, so every icon has the same optical size and weight. resvg rasterizes
/// them at the exact pixel size; the white strokes take the palette color as a tint.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Laptop,
    /// How the target is reached.
    Route,
    /// The selected row in a list.
    Chevron,
    /// Ending a session.
    Power,
    /// Starting a session over.
    Refresh,
    /// Live traffic.
    Activity,
    /// A connection's settings.
    Settings,
    /// Cancel.
    Close,
    /// SSH jump host.
    Server,
    /// WireGuard VPN.
    Shield,
    Monitor,
    /// SSH targets.
    Terminal,
    Check,
    Plus,
    Ellipsis,
    Plug,
    Key,
    Trash,
    Copy,
    Search,
    Success,
    Info,
    Alert,
}

macro_rules! lucide {
    ($name:literal) => {
        egui::ImageSource::Bytes {
            uri: std::borrow::Cow::Borrowed(concat!("bytes://lucide/", $name, ".svg")),
            bytes: egui::load::Bytes::Static(include_bytes!(concat!("../../assets/icons/lucide/", $name, ".svg"))),
        }
    };
}

impl Icon {
    fn source(self) -> egui::ImageSource<'static> {
        match self {
            Icon::Laptop => lucide!("laptop"),
            Icon::Route => lucide!("route"),
            Icon::Chevron => lucide!("chevron-right"),
            Icon::Power => lucide!("power"),
            Icon::Refresh => lucide!("refresh-cw"),
            Icon::Activity => lucide!("activity"),
            Icon::Settings => lucide!("sliders-horizontal"),
            Icon::Close => lucide!("x"),
            Icon::Server => lucide!("server"),
            Icon::Shield => lucide!("shield-check"),
            Icon::Monitor => lucide!("monitor"),
            Icon::Terminal => lucide!("square-terminal"),
            Icon::Check => lucide!("check"),
            Icon::Plus => lucide!("plus"),
            Icon::Ellipsis => lucide!("ellipsis-vertical"),
            Icon::Plug => lucide!("plug"),
            Icon::Key => lucide!("key-round"),
            Icon::Trash => lucide!("trash-2"),
            Icon::Copy => lucide!("copy"),
            Icon::Search => lucide!("search"),
            Icon::Success => lucide!("circle-check"),
            Icon::Info => lucide!("info"),
            Icon::Alert => lucide!("circle-alert"),
        }
    }

    /// The icon as an image widget of `size` points in `color`.
    pub fn image(self, size: f32, color: Color32) -> egui::Image<'static> {
        egui::Image::new(self.source()).fit_to_exact_size(Vec2::splat(size)).tint(color)
    }
}

/// Paints `icon` centered on `center`, `size` points square.
pub fn paint_icon(ui: &Ui, icon: Icon, center: egui::Pos2, size: f32, color: Color32) {
    icon.image(size, color).paint_at(ui, egui::Rect::from_center_size(center, Vec2::splat(size)));
}

/// Paints `icon` rasterized for `size` points but drawn `scale` times as large. Pop and press motion
/// goes through here: drawing each in-between size directly would rasterize the SVG for every one.
pub fn paint_icon_scaled(ui: &Ui, icon: Icon, center: egui::Pos2, size: f32, scale: f32, color: Color32) {
    let pixels = (Vec2::splat(size) * ui.pixels_per_point()).round();
    let hint = egui::SizeHint::Size { width: pixels.x as u32, height: pixels.y as u32, maintain_aspect_ratio: false };
    // Same options as `Image`, so both share one rasterization per size.
    let Ok(egui::load::TexturePoll::Ready { texture }) = icon.source().load(ui.ctx(), egui::TextureOptions::default(), hint)
    else {
        return;
    };
    let rect = egui::Rect::from_center_size(center, Vec2::splat(size * scale));
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
    ui.painter().image(texture.id, rect, uv, color);
}

/// Standard button with a leading icon, both in `color`.
pub fn labeled_button(icon: Icon, text: impl Into<RichText>, color: Color32) -> egui::Button<'static> {
    egui::Button::image_and_text(icon.image(ICON_SM, color), text.into().color(color))
}
