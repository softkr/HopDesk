//! The assistant over the session: a character, its speech bubble and a message box in the
//! window's bottom right corner, plus markers where the AI points (docs/ai-computer-use-plan.md
//! §4). It is drawn on the local window only; the session and the model never see it.

use std::collections::VecDeque;

use super::canvas::{Canvas, Color, Rect, Text};
use crate::agent::State;
use crate::agent::action::Point;

/// Sizes in logical points.
const MARGIN: f32 = 20.0;
/// How close to the window edge the character may be dragged.
const EDGE: f32 = 4.0;
/// Leaves the remote taskbar visible below the assistant.
const BOTTOM_MARGIN: f32 = 56.0;
const AVATAR: f32 = 56.0;
const GAP: f32 = 10.0;
const PANEL_WIDTH: f32 = 360.0;
const PADDING: f32 = 12.0;
const FONT: f32 = 14.0;
const SMALL_FONT: f32 = 12.0;
const INPUT_HEIGHT: f32 = 38.0;
const BUTTON_HEIGHT: f32 = 28.0;
const RADIUS: f32 = 12.0;
/// Lines of conversation the bubble shows; older ones scroll away.
const MAX_LINES: usize = 8;
/// Messages kept for the bubble.
const MAX_MESSAGES: usize = 30;

const WHITE: Color = 0xff_ff_ff;
const INK: Color = 0x1f_29_37;
const MUTED: Color = 0x6b_72_80;
const LINE: Color = 0xd1_d5_db;
const BLUE: Color = 0x25_63_eb;
const SHADOW: Color = 0x00_00_00;

/// What a click on the overlay means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hit {
    /// The character: shows or hides the bubble.
    Avatar,
    Input,
    Approve,
    Decline,
    Stop,
    /// Anywhere else on the bubble.
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    Ai,
    Person,
    HopDesk,
}

pub(super) struct Overlay {
    text: Option<Text>,
    /// Physical pixels per logical point.
    scale: f32,
    /// Where the character sits: its distance from the window's right and bottom edges, in
    /// points, so it keeps its corner when the window is resized.
    corner: (f32, f32),
    /// The window size in points at the last paint, to keep a dragged character inside it.
    window: Option<(f32, f32)>,
    expanded: bool,
    state: State,
    messages: VecDeque<(Who, String)>,
    proposal: Vec<(String, Option<Point>)>,
    /// The AI's pointer on the session.
    pointer: Option<Point>,
    /// Session size, to place markers on the stretched frame.
    session: (u16, u16),
    input: String,
    /// Text the input method is still composing.
    preedit: String,
    focused: bool,
    /// Where each clickable part was drawn last.
    hits: Vec<(Rect, Hit)>,
}

impl Overlay {
    pub(super) fn new(text: Option<Text>, endpoint: &str) -> Self {
        let mut overlay = Self {
            text,
            scale: 1.0,
            corner: (MARGIN, BOTTOM_MARGIN),
            window: None,
            expanded: true,
            state: State::Idle,
            messages: VecDeque::new(),
            proposal: Vec::new(),
            pointer: None,
            session: (0, 0),
            input: String::new(),
            preedit: String::new(),
            focused: false,
            hits: Vec::new(),
        };
        overlay.notice(format!(
            "무엇을 도와드릴까요? 아래에 할 일을 적어 주세요. 화면은 {endpoint}로 전송되고, 동작은 실행 전에 확인을 받습니다."
        ));
        overlay
    }

    pub(super) fn set_scale(&mut self, scale: f32) {
        self.scale = scale.max(0.5);
    }

    pub(super) fn corner(&self) -> (f32, f32) {
        self.corner
    }

    /// Moves the character, no further than the window's edges (so dragging back responds at
    /// once).
    pub(super) fn set_corner(&mut self, (right, bottom): (f32, f32)) {
        let limit = |value: f32, room: Option<f32>| match room {
            Some(room) => value.clamp(EDGE, (room - AVATAR - EDGE).max(EDGE)),
            None => value.max(EDGE),
        };
        self.corner = (limit(right, self.window.map(|w| w.0)), limit(bottom, self.window.map(|w| w.1)));
    }

    pub(super) fn scale(&self) -> f32 {
        self.scale
    }

    pub(super) fn set_session(&mut self, size: (u16, u16)) {
        self.session = size;
    }

    pub(super) fn state(&self) -> State {
        self.state
    }

    pub(super) fn set_state(&mut self, state: State) {
        self.state = state;
        if state != State::Approval {
            self.proposal.clear();
        }
        if state == State::Idle {
            self.pointer = None;
        }
    }

    pub(super) fn say(&mut self, text: String) {
        self.push(Who::Ai, text);
    }

    pub(super) fn notice(&mut self, text: String) {
        self.push(Who::HopDesk, text);
    }

    pub(super) fn propose(&mut self, proposal: Vec<(String, Option<Point>)>) {
        self.proposal = proposal;
    }

    pub(super) fn set_pointer(&mut self, point: Point) {
        self.pointer = Some(point);
    }

    fn push(&mut self, who: Who, text: String) {
        self.messages.push_back((who, text));
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
        }
    }

    pub(super) fn focused(&self) -> bool {
        self.focused
    }

    pub(super) fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.preedit.clear();
        }
    }

    pub(super) fn type_text(&mut self, text: &str) {
        self.input.extend(text.chars().filter(|c| !c.is_control()));
    }

    pub(super) fn set_preedit(&mut self, text: String) {
        self.preedit = text;
    }

    pub(super) fn backspace(&mut self) {
        self.input.pop();
    }

    /// The message to send, which leaves the box empty; `None` when there is nothing to send.
    pub(super) fn take_input(&mut self) -> Option<String> {
        let text = core::mem::take(&mut self.input).trim().to_owned();
        if text.is_empty() {
            return None;
        }
        self.push(Who::Person, text.clone());
        Some(text)
    }

    /// The part of the overlay at a window position (physical pixels).
    pub(super) fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        self.hits.iter().find(|(rect, _)| rect.contains(x, y)).map(|(_, hit)| *hit)
    }

    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
        if !self.expanded {
            self.set_focused(false);
        }
    }

    /// Draws the overlay over a frame of the window's size.
    pub(super) fn paint(&mut self, pixels: &mut [u32], width: usize, height: usize) {
        self.hits.clear();
        let mut canvas = Canvas { pixels, width, height };
        #[expect(clippy::cast_precision_loss, reason = "window sizes")]
        let (w, h) = (width as f32, height as f32);
        let s = self.scale;
        self.window = Some((w / s, h / s));
        self.paint_markers(&mut canvas, w, h);
        if self.state == State::Acting {
            self.paint_badge(&mut canvas, w);
        }

        // The character stays whole inside the window wherever it was dragged.
        let size = AVATAR * s;
        let clamp = |value: f32, room: f32| value.clamp(EDGE * s, (room - size - EDGE * s).max(EDGE * s));
        let avatar = Rect {
            x: clamp(w - self.corner.0 * s - size, w),
            y: clamp(h - self.corner.1 * s - size, h),
            w: size,
            h: size,
        };
        self.paint_avatar(&mut canvas, avatar);
        self.hits.push((avatar, Hit::Avatar));
        if !self.expanded {
            return;
        }
        // The message box and bubble open on the side of the character with more room.
        let room_left = avatar.x - (EDGE + GAP) * s;
        let room_right = w - (avatar.x + avatar.w) - (EDGE + GAP) * s;
        let left = room_left >= PANEL_WIDTH * s || room_left >= room_right;
        let panel_width = (PANEL_WIDTH * s).min(room_left.max(room_right)).max(160.0 * s);
        let x = if left { avatar.x - GAP * s - panel_width } else { avatar.x + avatar.w + GAP * s };
        let input = Rect {
            x,
            y: avatar.y + avatar.h - INPUT_HEIGHT * s,
            w: panel_width,
            h: INPUT_HEIGHT * s,
        };
        self.paint_input(&mut canvas, input);
        self.hits.push((input, Hit::Input));
        self.paint_bubble(&mut canvas, x, input, panel_width);
    }

    /// The bubble above the message box, or below it when there is no room above.
    fn paint_bubble(&mut self, canvas: &mut Canvas<'_>, x: f32, input: Rect, width: f32) {
        let s = self.scale;
        let Some(text) = &self.text else { return };
        let inner = width - 2.0 * PADDING * s;
        let line = text.line_height(FONT * s);
        let small_line = text.line_height(SMALL_FONT * s);

        let mut lines: Vec<(Color, String)> = Vec::new();
        for (who, message) in &self.messages {
            let (color, prefix) = match who {
                Who::Ai => (INK, ""),
                Who::Person => (BLUE, "나: "),
                Who::HopDesk => (MUTED, ""),
            };
            for wrapped in text.wrap(&format!("{prefix}{message}"), FONT * s, inner) {
                lines.push((color, wrapped));
            }
        }
        let skip = lines.len().saturating_sub(MAX_LINES);
        let lines = &lines[skip..];
        let mut proposal: Vec<String> = Vec::new();
        for (index, (description, _)) in self.proposal.iter().enumerate() {
            proposal.extend(text.wrap(&format!("{}. {description}", index + 1), FONT * s, inner));
        }
        let buttons: &[(&str, Hit)] = match self.state {
            State::Approval => &[("실행", Hit::Approve), ("거절", Hit::Decline)],
            State::TakenOver => &[("계속", Hit::Approve), ("멈추기", Hit::Stop)],
            State::Thinking | State::Acting => &[("중단", Hit::Stop)],
            State::Idle => &[],
        };

        #[expect(clippy::cast_precision_loss, reason = "a few lines")]
        let body = (lines.len() + proposal.len()) as f32 * line;
        let buttons_height = if buttons.is_empty() { 0.0 } else { (BUTTON_HEIGHT + GAP) * s };
        let height = 2.0 * PADDING * s + small_line + GAP * s * 0.5 + body + buttons_height;
        let above = input.y - GAP * s - height;
        let rect = Rect {
            x,
            y: if above >= EDGE * s { above } else { input.y + input.h + GAP * s },
            w: width,
            h: height,
        };
        self.paint_card(canvas, rect);
        self.hits.push((rect, Hit::Panel));

        let mut y = rect.y + PADDING * s;
        let left = rect.x + PADDING * s;
        let after = text.draw(canvas, left, y, "HopDesk AI", SMALL_FONT * s, INK);
        text.draw(canvas, after + 8.0 * s, y, state_label(self.state), SMALL_FONT * s, state_color(self.state));
        y += small_line + GAP * s * 0.5;
        for (color, line_text) in lines {
            text.draw(canvas, left, y, line_text, FONT * s, *color);
            y += line;
        }
        for line_text in &proposal {
            text.draw(canvas, left, y, line_text, FONT * s, 0xb4_53_09);
            y += line;
        }
        let mut button_x = left;
        let button_y = rect.y + rect.h - PADDING * s - BUTTON_HEIGHT * s;
        for (index, (label, hit)) in buttons.iter().enumerate() {
            let button = Rect {
                x: button_x,
                y: button_y,
                w: text.width(label, FONT * s) + 28.0 * s,
                h: BUTTON_HEIGHT * s,
            };
            let primary = index == 0 && *hit == Hit::Approve;
            let (fill, ink) = if primary { (BLUE, WHITE) } else { (0xf3_f4_f6, INK) };
            canvas.round_rect(button, 8.0 * s, fill, 1.0);
            let label_y = button.y + (button.h - text.line_height(FONT * s)) / 2.0 + 2.0 * s;
            text.draw(canvas, button.x + 14.0 * s, label_y, label, FONT * s, ink);
            self.hits.push((button, *hit));
            button_x += button.w + 8.0 * s;
        }
    }

    fn paint_input(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let s = self.scale;
        self.paint_card(canvas, rect);
        if self.focused {
            let ring = Rect {
                x: rect.x - 1.0 * s,
                y: rect.y - 1.0 * s,
                w: rect.w + 2.0 * s,
                h: rect.h + 2.0 * s,
            };
            canvas.round_rect(ring, RADIUS * s, BLUE, 0.35);
            canvas.round_rect(rect, RADIUS * s, WHITE, 1.0);
        }
        let Some(text) = &self.text else { return };
        let size = FONT * s;
        let y = rect.y + (rect.h - text.line_height(size)) / 2.0 + 2.0 * s;
        let x = rect.x + PADDING * s;
        let width = rect.w - 2.0 * PADDING * s;
        if self.input.is_empty() && self.preedit.is_empty() && !self.focused {
            text.draw(canvas, x, y, "할 일을 입력하고 Enter", size, MUTED);
            return;
        }
        // The end of what is typed stays in view.
        let mut shown: String = format!("{}{}", self.input, self.preedit);
        while text.width(&shown, size) > width - 4.0 * s && !shown.is_empty() {
            shown.remove(0);
        }
        let composing = shown.chars().count().min(self.preedit.chars().count());
        let typed: String = shown.chars().take(shown.chars().count() - composing).collect();
        let end = text.draw(canvas, x, y, &typed, size, INK);
        let preedit: String = shown.chars().skip(shown.chars().count() - composing).collect();
        let end_of_preedit = text.draw(canvas, end, y, &preedit, size, BLUE);
        if !preedit.is_empty() {
            let underline = Rect {
                x: end,
                y: y + text.line_height(size) - 3.0 * s,
                w: end_of_preedit - end,
                h: 1.0 * s,
            };
            canvas.round_rect(underline, 0.0, BLUE, 1.0);
        }
        if self.focused {
            let caret = Rect {
                x: end_of_preedit + 1.0 * s,
                y: y + 2.0 * s,
                w: 1.5 * s,
                h: text.line_height(size) - 4.0 * s,
            };
            canvas.round_rect(caret, 0.0, INK, 1.0);
        }
    }

    /// A white card with a soft shadow.
    fn paint_card(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let s = self.scale;
        let shadow = Rect {
            y: rect.y + 2.0 * s,
            ..rect
        };
        canvas.round_rect(shadow, RADIUS * s, SHADOW, 0.18);
        canvas.round_rect(rect, RADIUS * s, LINE, 1.0);
        let inside = Rect {
            x: rect.x + 1.0 * s,
            y: rect.y + 1.0 * s,
            w: rect.w - 2.0 * s,
            h: rect.h - 2.0 * s,
        };
        canvas.round_rect(inside, RADIUS * s - 1.0 * s, WHITE, 0.97);
    }

    /// A round character whose color and eyes follow the state.
    fn paint_avatar(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let s = self.scale;
        let (cx, cy, r) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0, rect.w / 2.0);
        canvas.circle(cx, cy + 2.0 * s, r, SHADOW, 0.2);
        canvas.circle(cx, cy, r, state_color(self.state), 1.0);
        canvas.circle(cx - r * 0.3, cy - r * 0.35, r * 0.35, WHITE, 0.18);
        // Eyes look up while thinking and toward the screen while acting.
        let (look_x, look_y) = match self.state {
            State::Thinking => (0.0, -0.45),
            State::Acting => (-0.4, 0.0),
            _ => (0.0, 0.1),
        };
        for side in [-1.0, 1.0] {
            let (ex, ey) = (cx + side * r * 0.32, cy - r * 0.08);
            canvas.circle(ex, ey, r * 0.2, WHITE, 1.0);
            canvas.circle(ex + look_x * r * 0.09, ey + look_y * r * 0.09, r * 0.1, INK, 1.0);
        }
        if self.state == State::Approval {
            canvas.circle(rect.x + rect.w - 6.0 * s, rect.y + 6.0 * s, 7.0 * s, 0xef_44_44, 1.0);
        }
    }

    /// The AI's pointer, and the numbered places of the proposed actions.
    fn paint_markers(&self, canvas: &mut Canvas<'_>, w: f32, h: f32) {
        let s = self.scale;
        let (sw, sh) = (f32::from(self.session.0), f32::from(self.session.1));
        if sw <= 0.0 || sh <= 0.0 {
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
        let place = |point: Point| ((point.x as f32 + 0.5) * w / sw, (point.y as f32 + 0.5) * h / sh);
        for (index, (_, point)) in self.proposal.iter().enumerate() {
            let Some(point) = point else { continue };
            let (x, y) = place(*point);
            canvas.ring(x, y, 14.0 * s, 3.0 * s, 0xf5_9e_0b, 0.9);
            canvas.circle(x + 14.0 * s, y - 14.0 * s, 9.0 * s, 0xf5_9e_0b, 1.0);
            if let Some(text) = &self.text {
                let label = (index + 1).to_string();
                let size = SMALL_FONT * s;
                let lx = x + 14.0 * s - text.width(&label, size) / 2.0;
                text.draw(canvas, lx, y - 14.0 * s - text.line_height(size) / 2.0 + 1.0 * s, &label, size, WHITE);
            }
        }
        if let Some(point) = self.pointer {
            let (x, y) = place(point);
            canvas.ring(x, y, 10.0 * s, 3.0 * s, state_color(State::Acting), 0.95);
            canvas.circle(x, y, 3.0 * s, state_color(State::Acting), 1.0);
        }
    }

    /// "AI 조작 중" at the top of the window while the AI operates the screen.
    fn paint_badge(&self, canvas: &mut Canvas<'_>, w: f32) {
        let s = self.scale;
        let Some(text) = &self.text else { return };
        let label = "AI 조작 중 · 마우스나 키보드를 쓰면 멈춥니다";
        let size = SMALL_FONT * s;
        let badge = Rect {
            x: (w - text.width(label, size)) / 2.0 - 14.0 * s,
            y: 10.0 * s,
            w: text.width(label, size) + 28.0 * s,
            h: 26.0 * s,
        };
        canvas.round_rect(badge, 13.0 * s, state_color(State::Acting), 0.95);
        let y = badge.y + (badge.h - text.line_height(size)) / 2.0 + 1.0 * s;
        text.draw(canvas, badge.x + 14.0 * s, y, label, size, WHITE);
    }
}

fn state_label(state: State) -> &'static str {
    match state {
        State::Idle => "대기 중",
        State::Thinking => "생각 중…",
        State::Acting => "조작 중",
        State::Approval => "확인해 주세요",
        State::TakenOver => "멈춤",
    }
}

fn state_color(state: State) -> Color {
    match state {
        State::Idle => 0x0e_a5_e9,
        State::Thinking => 0x8b_5c_f6,
        State::Acting => 0x10_b9_81,
        State::Approval => 0xf5_9e_0b,
        State::TakenOver => 0x6b_72_80,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painted(overlay: &mut Overlay) {
        let mut pixels = vec![0u32; 1280 * 800];
        overlay.paint(&mut pixels, 1280, 800);
    }

    #[test]
    fn the_character_sits_in_the_bottom_right_corner() {
        let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
        painted(&mut overlay);
        assert_eq!(overlay.hit(1280.0 - MARGIN - 10.0, 800.0 - BOTTOM_MARGIN - 10.0), Some(Hit::Avatar));
        assert_eq!(overlay.hit(10.0, 10.0), None, "the rest of the screen stays the session's");
    }

    #[test]
    fn the_message_box_and_buttons_follow_the_state() {
        let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
        if overlay.text.is_none() {
            return;
        }
        overlay.set_state(State::Approval);
        overlay.propose(vec![("클릭".into(), Some(Point { x: 5, y: 5 }))]);
        painted(&mut overlay);
        let hits: Vec<Hit> = overlay.hits.iter().map(|(_, hit)| *hit).collect();
        assert!(hits.contains(&Hit::Input));
        assert!(hits.contains(&Hit::Approve) && hits.contains(&Hit::Decline));
        overlay.toggle();
        painted(&mut overlay);
        assert_eq!(overlay.hits.len(), 1, "only the character while hidden");
    }

    fn hit_kinds(overlay: &Overlay) -> Vec<(Rect, Hit)> {
        overlay.hits.clone()
    }

    #[test]
    fn the_character_can_be_moved_and_stays_in_the_window() {
        let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
        overlay.set_corner((1000.0, 600.0));
        painted(&mut overlay);
        let avatar = hit_kinds(&overlay).into_iter().find(|(_, hit)| *hit == Hit::Avatar).unwrap().0;
        assert_eq!((avatar.x, avatar.y), (1280.0 - 1000.0 - AVATAR, 800.0 - 600.0 - AVATAR));
        // Dragged past the top left corner: it stops at the edge, and so does its position.
        overlay.set_corner((5000.0, 5000.0));
        assert_eq!(overlay.corner(), (1280.0 - AVATAR - EDGE, 800.0 - AVATAR - EDGE));
        painted(&mut overlay);
        let avatar = hit_kinds(&overlay).into_iter().find(|(_, hit)| *hit == Hit::Avatar).unwrap().0;
        assert_eq!((avatar.x, avatar.y), (EDGE, EDGE));
        if overlay.text.is_some() {
            // Near the top left, the message box opens to the right and the bubble below it.
            let input = hit_kinds(&overlay).into_iter().find(|(_, hit)| *hit == Hit::Input).unwrap().0;
            let panel = hit_kinds(&overlay).into_iter().find(|(_, hit)| *hit == Hit::Panel).unwrap().0;
            assert!(input.x > avatar.x + avatar.w);
            assert!(panel.y > input.y);
        }
    }

    #[test]
    fn sending_clears_the_box_and_keeps_the_message() {
        let mut overlay = Overlay::new(None, "api.anthropic.com");
        overlay.type_text("메모장 열어 줘");
        overlay.backspace();
        assert_eq!(overlay.take_input().as_deref(), Some("메모장 열어"));
        assert_eq!(overlay.take_input(), None);
        assert_eq!(overlay.messages.back().map(|(who, _)| *who), Some(Who::Person));
    }
}
