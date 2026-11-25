//! The assistant over the session: a character, its speech bubble and a message box in the
//! window's bottom right corner, plus markers where the AI points (docs/ai-computer-use-plan.md
//! §4). It is drawn on the local window only; the session and the model never see it.

use std::collections::VecDeque;

use super::canvas::{Canvas, Color, Rect, Text, mix};
use super::icons::Glyph;
use crate::agent::State;
use crate::agent::action::Point;

/// Sizes in logical points.
const MARGIN: f32 = 20.0;
/// How close to the window edge the character may be dragged.
const EDGE: f32 = 4.0;
/// Leaves the remote taskbar visible below the assistant.
const BOTTOM_MARGIN: f32 = 56.0;
const AVATAR: f32 = 52.0;
const GAP: f32 = 10.0;
const PANEL_WIDTH: f32 = 380.0;
const PADDING: f32 = 14.0;
const FONT: f32 = 14.0;
const SMALL_FONT: f32 = 12.0;
const INPUT_HEIGHT: f32 = 40.0;
const BUTTON_HEIGHT: f32 = 30.0;
/// The icon in a capsule button.
const BUTTON_ICON: f32 = 14.0;
/// Corner radius of the glass panel, and of a message bubble.
const RADIUS: f32 = 20.0;
const BUBBLE_RADIUS: f32 = 15.0;
/// Space inside a message bubble beside its text.
const BUBBLE_PAD: f32 = 11.0;
/// Lines of conversation the panel shows at most, and at least where the window is small;
/// older ones are reached with the mouse wheel.
const MAX_LINES: usize = 16;
const MIN_LINES: usize = 3;
/// How long the panel takes to fade in or out, in seconds.
const FADE: f32 = 0.18;
/// Messages kept for the panel.
const MAX_MESSAGES: usize = 30;
/// How long the AI's pointer takes to glide to a new place, and a click's ripple after it, in
/// seconds.
const GLIDE: f32 = 0.22;
const RIPPLE: f32 = 0.45;
/// The AI's pointer: an arrow with its tip at the origin, in points.
const ARROW: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 16.0),
    (4.0, 12.3),
    (6.8, 18.3),
    (9.3, 17.2),
    (6.6, 11.4),
    (11.8, 11.4),
];

const WHITE: Color = 0xff_ff_ff;
const BLACK: Color = 0x00_00_00;
/// The system orange, for the approval state's character and dot.
const ORANGE: Color = 0xff_9f_0a;
/// Where a proposed action acts, on the session and beside it in the list: dark enough under a
/// white number (5.3:1) and against a light screen.
const MARKER: Color = 0xc9_34_00;

/// Colors of the glass surfaces, after the system appearance.
struct Theme {
    /// Tint of the glass and how much of what is behind it shows through.
    glass: Color,
    glass_strength: f32,
    label: Color,
    secondary: Color,
    /// Hairlines and quiet fills are the label color at these strengths.
    hairline: f32,
    fill: f32,
    accent: Color,
    /// Text in orange and red, legible on this glass (WCAG AA, 4.5:1 or more).
    orange: Color,
    red: Color,
    /// How strongly a tinted button is filled with its text color.
    tint: f32,
}

// The glass lets the session through, so its strength and the text colors are chosen to keep
// text at 4.5:1 or more over any screen behind it, black or white.
const LIGHT: Theme = Theme {
    glass: 0xfb_fb_fd,
    glass_strength: 0.92,
    label: 0x1d_1d_1f,
    secondary: 0x63_63_66,
    hairline: 0.10,
    fill: 0.06,
    // The app's own light accent: white on it is 4.7:1.
    accent: 0x00_71_e3,
    orange: 0xb8_30_00,
    red: 0xc4_00_0f,
    tint: 0.05,
};

const DARK: Theme = Theme {
    glass: 0x1c_1c_1e,
    glass_strength: 0.94,
    label: 0xf5_f5_f7,
    secondary: 0xa1_a1_a6,
    hairline: 0.16,
    fill: 0.12,
    accent: 0x0a_84_ff,
    orange: 0xff_9f_0a,
    red: 0xff_69_61,
    tint: 0.08,
};

/// What a click on the overlay means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Hit {
    /// The character: shows or hides the bubble.
    Avatar,
    Input,
    /// The send button in the message box.
    Send,
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
    /// The AI said something while the bubble was hidden.
    unread: bool,
    /// Lines the conversation is scrolled back from the newest, and how far it can go.
    scroll: usize,
    max_scroll: usize,
    state: State,
    /// What the panel says before anything else is said: what to do, and where the screen goes.
    greeting: String,
    messages: VecDeque<(Who, String)>,
    proposal: Vec<(String, Option<Point>)>,
    /// The AI's pointer on the session.
    pointer: Option<AiPointer>,
    /// Session size, to place markers on the stretched frame.
    session: (u16, u16),
    input: String,
    /// Text the input method is still composing.
    preedit: String,
    focused: bool,
    /// Where each clickable part was drawn last.
    hits: Vec<(Rect, Hit)>,
    /// The system uses its dark appearance.
    dark: bool,
    /// Seconds since the session began, for the motion of the working states.
    time: f32,
    /// How much of the panel shows, from 0 to 1, while it fades in or out.
    visibility: f32,
    /// A fade under way: the visibility it began at, and when (set at the next paint).
    fade: Option<(f32, Option<f32>)>,
}

/// The AI's pointer on the session: where it goes, where it set out from (session pixels), since
/// when (set at the next paint), and whether it clicked there.
struct AiPointer {
    to: Point,
    from: (f32, f32),
    since: Option<f32>,
    pressed: bool,
}

impl AiPointer {
    fn elapsed(&self, time: f32) -> f32 {
        self.since.map_or(0.0, |since| (time - since).max(0.0))
    }

    /// Where it shows at `time`, slowing down as it arrives.
    fn position(&self, time: f32) -> (f32, f32) {
        let progress = (self.elapsed(time) / GLIDE).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
        let to = (self.to.x as f32, self.to.y as f32);
        (self.from.0 + (to.0 - self.from.0) * eased, self.from.1 + (to.1 - self.from.1) * eased)
    }

    /// How far a click's ripple has spread (0 to 1), while it shows.
    fn ripple(&self, time: f32) -> Option<f32> {
        let spread = (self.elapsed(time) - GLIDE) / RIPPLE;
        (self.pressed && (0.0..1.0).contains(&spread)).then_some(spread)
    }

    fn moving(&self, time: f32) -> bool {
        let settled = if self.pressed { GLIDE + RIPPLE } else { GLIDE };
        self.since.is_none() || self.elapsed(time) < settled
    }
}

impl Overlay {
    pub(super) fn new(text: Option<Text>, endpoint: &str) -> Self {
        Self {
            text,
            scale: 1.0,
            corner: (MARGIN, BOTTOM_MARGIN),
            window: None,
            // Only the character at first; it opens by a click, or when the person is needed.
            expanded: false,
            unread: false,
            scroll: 0,
            max_scroll: 0,
            state: State::Idle,
            greeting: format!(
                "할 일을 아래에 적어 주세요. 화면은 {endpoint}로 전송되고, 동작은 실행하기 전에 먼저 보여 드립니다."
            ),
            messages: VecDeque::new(),
            proposal: Vec::new(),
            pointer: None,
            session: (0, 0),
            input: String::new(),
            preedit: String::new(),
            focused: false,
            hits: Vec::new(),
            dark: false,
            time: 0.0,
            visibility: 0.0,
            fade: None,
        }
    }

    /// The messages to show: the greeting until the first one.
    fn conversation(&self) -> Vec<(Who, &str)> {
        if self.messages.is_empty() {
            return vec![(Who::HopDesk, self.greeting.as_str())];
        }
        self.messages.iter().map(|(who, text)| (*who, text.as_str())).collect()
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

    pub(super) fn set_dark(&mut self, dark: bool) {
        self.dark = dark;
    }

    pub(super) fn set_time(&mut self, seconds: f32) {
        self.time = seconds;
    }

    /// Whether the overlay moves on its own, so the window keeps redrawing it.
    pub(super) fn animating(&self) -> bool {
        matches!(self.state, State::Thinking | State::Acting)
            || self.fade.is_some()
            || self.pointer.as_ref().is_some_and(|pointer| pointer.moving(self.time))
    }

    pub(super) fn input_is_empty(&self) -> bool {
        self.input.trim().is_empty() && self.preedit.is_empty()
    }

    fn theme(&self) -> &'static Theme {
        if self.dark { &DARK } else { &LIGHT }
    }

    pub(super) fn set_session(&mut self, size: (u16, u16)) {
        self.session = size;
    }

    pub(super) fn state(&self) -> State {
        self.state
    }

    pub(super) fn set_state(&mut self, state: State) {
        self.state = state;
        if state == State::TakenOver {
            self.expand();
        }
        if state != State::Approval {
            self.proposal.clear();
        }
        if state == State::Idle {
            self.pointer = None;
        }
    }

    pub(super) fn say(&mut self, text: String) {
        self.push(Who::Ai, plain(&text));
        if !self.expanded {
            self.unread = true;
        }
    }

    /// HopDesk's own messages (stops, errors) open the bubble.
    pub(super) fn notice(&mut self, text: String) {
        self.push(Who::HopDesk, text);
        self.expand();
    }

    /// Actions waiting for approval open the bubble, where the buttons are.
    pub(super) fn propose(&mut self, proposal: Vec<(String, Option<Point>)>) {
        self.proposal = proposal;
        self.expand();
    }

    fn expand(&mut self) {
        if !self.expanded {
            self.fade = Some((self.visibility, None));
        }
        self.expanded = true;
        self.unread = false;
    }

    /// Scrolls the conversation by `lines`, positive toward older ones.
    pub(super) fn scroll_by(&mut self, lines: i32) {
        let scroll = i64::try_from(self.scroll).unwrap_or(0) + i64::from(lines);
        self.scroll = usize::try_from(scroll.max(0)).unwrap_or(0).min(self.max_scroll);
    }

    /// Moves the AI's pointer to `to`, gliding from where it shows now; `pressed` marks a click
    /// there.
    pub(super) fn set_pointer(&mut self, to: Point, pressed: bool) {
        #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
        let from = self
            .pointer
            .as_ref()
            .map_or((to.x as f32, to.y as f32), |pointer| pointer.position(self.time));
        self.pointer = Some(AiPointer {
            to,
            from,
            since: None,
            pressed,
        });
    }

    fn push(&mut self, who: Who, text: String) {
        // Something new: back to the newest lines.
        self.scroll = 0;
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
        // What was drawn last is on top: a button wins over the bubble behind it.
        self.hits.iter().rev().find(|(rect, _)| rect.contains(x, y)).map(|(_, hit)| *hit)
    }

    pub(super) fn toggle(&mut self) {
        if self.expanded {
            self.expanded = false;
            self.fade = Some((self.visibility, None));
            self.set_focused(false);
        } else {
            self.expand();
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
        // A glide starts at the first frame that shows it.
        if let Some(pointer) = &mut self.pointer {
            pointer.since.get_or_insert(self.time);
        }
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
        // The panel fades in and out; without a fade it is all there or not at all.
        let target = if self.expanded { 1.0 } else { 0.0 };
        self.visibility = match &mut self.fade {
            Some((from, start)) => {
                let progress =
                    ((self.time - *start.get_or_insert(self.time)) / FADE).clamp(0.0, 1.0);
                let eased = progress * progress * (3.0 - 2.0 * progress);
                let visibility = *from + (target - *from) * eased;
                if progress >= 1.0 {
                    self.fade = None;
                }
                visibility
            }
            None => target,
        };
        if self.visibility <= 0.0 {
            return;
        }
        let clickable = self.hits.len();
        // The message box and panel open on the side of the character with more room.
        let room_left = avatar.x - (EDGE + GAP) * s;
        let room_right = w - (avatar.x + avatar.w) - (EDGE + GAP) * s;
        let left = room_left >= PANEL_WIDTH * s || room_left >= room_right;
        let panel_width = (PANEL_WIDTH * s).min(room_left.max(room_right)).max(160.0 * s);
        let x = if left { avatar.x - GAP * s - panel_width } else { avatar.x + avatar.w + GAP * s };
        let input = Rect {
            x,
            y: avatar.y + (avatar.h - INPUT_HEIGHT * s) / 2.0,
            w: panel_width,
            h: INPUT_HEIGHT * s,
        };
        // Mid-fade, what was there before is kept and mixed back over the finished panel.
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped screen coordinates"
        )]
        let (fade_left, fade_right) = (
            ((x - 30.0 * s).max(0.0) as usize).min(width),
            ((x + panel_width + 30.0 * s).max(0.0) as usize).min(width),
        );
        let before: Option<Vec<u32>> = (self.visibility < 1.0).then(|| {
            canvas
                .pixels
                .chunks_exact(width)
                .flat_map(|row| row[fade_left..fade_right].iter().copied())
                .collect()
        });
        self.paint_input(&mut canvas, input);
        self.paint_panel(&mut canvas, x, input, panel_width);
        if let Some(before) = before {
            let columns = fade_right - fade_left;
            for (row, saved) in canvas
                .pixels
                .chunks_exact_mut(width)
                .zip(before.chunks_exact(columns.max(1)))
            {
                for (pixel, old) in row[fade_left..fade_right].iter_mut().zip(saved) {
                    *pixel = mix(*old, *pixel, self.visibility);
                }
            }
        }
        // A panel on its way out no longer takes clicks.
        if !self.expanded {
            self.hits.truncate(clickable);
        }
    }

    /// The conversation above the message box, or below it when there is no room above.
    fn paint_panel(&mut self, canvas: &mut Canvas<'_>, x: f32, input: Rect, width: f32) {
        /// A line of the conversation; `message` tells the lines of one message apart.
        struct Line {
            message: usize,
            who: Who,
            text: String,
        }
        let s = self.scale;
        let theme = self.theme();
        let Some(text) = &self.text else { return };
        let inner = width - 2.0 * PADDING * s;
        let line = text.line_height(FONT * s);
        let small_line = text.line_height(SMALL_FONT * s);
        let pad = BUBBLE_PAD * s;

        // The person's messages sit in bubbles on the right, the AI's as plain text on the left,
        // with an empty line between messages.
        let mut lines: Vec<Line> = Vec::new();
        for (index, (who, message)) in self.conversation().into_iter().enumerate() {
            if index > 0 {
                lines.push(Line {
                    message: usize::MAX,
                    who: Who::HopDesk,
                    text: String::new(),
                });
            }
            let wrap_width = if who == Who::Person {
                inner * 0.82 - 2.0 * pad
            } else {
                inner
            };
            for wrapped in text.wrap(message, FONT * s, wrap_width) {
                lines.push(Line {
                    message: index,
                    who,
                    text: wrapped,
                });
            }
        }
        // Each proposed action after a numbered chip, the number it has on the session: the
        // action's index on its first line, whether it acts at a place, and the text.
        let chip_room = 26.0 * s;
        let mut proposal: Vec<(Option<usize>, bool, String)> = Vec::new();
        for (index, (description, point)) in self.proposal.iter().enumerate() {
            for (row, wrapped) in text.wrap(description, FONT * s, inner - chip_room).into_iter().enumerate() {
                proposal.push(((row == 0).then_some(index), point.is_some(), wrapped));
            }
        }
        // The first button is the default one, drawn rightmost.
        let (buttons, hint) = controls(self.state, !self.input_is_empty());
        let header = small_line + 18.0 * s;
        #[expect(clippy::cast_precision_loss, reason = "a few lines")]
        let proposal_height = if proposal.is_empty() {
            0.0
        } else {
            small_line + 6.0 * s + proposal.len() as f32 * line + 6.0 * s
        };
        let buttons_height = if buttons.is_empty() {
            0.0
        } else {
            (BUTTON_HEIGHT + 10.0) * s
        };
        let fixed = 10.0 * s + header + proposal_height + buttons_height + PADDING * s;

        // As many lines as fit above or below the message box, then scrolled with the wheel.
        let window_height = self.window.map_or(f32::MAX, |(_, h)| h * s);
        let room = (input.y - GAP * s - EDGE * s).max(window_height - (input.y + input.h + GAP * s) - EDGE * s);
        // Room for the two "more" hints too.
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a few lines")]
        let fit = (((room - fixed - 2.0 * small_line) / line).floor().max(0.0) as usize).clamp(MIN_LINES, MAX_LINES);
        let shown = lines.len().min(fit);
        self.max_scroll = lines.len() - shown;
        self.scroll = self.scroll.min(self.max_scroll);
        let start = lines.len() - shown - self.scroll;
        let older = start > 0;
        let newer = self.scroll > 0;
        let lines = &lines[start..start + shown];

        #[expect(clippy::cast_precision_loss, reason = "a few lines")]
        let body = lines.len() as f32 * line + f32::from(u8::from(older) + u8::from(newer)) * small_line;
        let height = fixed + body;
        let above = input.y - GAP * s - height;
        let rect = Rect {
            x,
            y: if above >= EDGE * s { above } else { input.y + input.h + GAP * s },
            w: width,
            h: height,
        };
        self.paint_glass(canvas, rect, RADIUS * s);
        self.hits.push((rect, Hit::Panel));

        // Header: the name, and the state with its dot on the right.
        let left = rect.x + PADDING * s;
        let right = rect.x + rect.w - PADDING * s;
        let mut y = rect.y + 10.0 * s;
        canvas.icon(
            Glyph::Sparkles,
            left + 6.5 * s,
            y + small_line / 2.0,
            13.0 * s,
            theme.accent,
            1.0,
        );
        text.draw(
            canvas,
            left + 18.0 * s,
            y,
            "HopDesk AI",
            SMALL_FONT * s,
            theme.label,
        );
        let mut state = state_label(self.state).to_owned();
        if self.state == State::Thinking {
            // Dots that come and go while the model works.
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small count"
            )]
            let dots = (self.time * 2.5) as usize % 4;
            state.push_str(&"·".repeat(dots));
        }
        let state_width =
            text.width(state_label(self.state), SMALL_FONT * s) + text.width("···", SMALL_FONT * s);
        let state_x = right - state_width;
        canvas.circle(
            state_x - 8.0 * s,
            y + small_line / 2.0 - 1.0 * s,
            3.5 * s,
            state_color(self.state),
            1.0,
        );
        text.draw(canvas, state_x, y, &state, SMALL_FONT * s, theme.secondary);
        y += small_line + 8.0 * s;
        let hairline = |canvas: &mut Canvas<'_>, y: f32| {
            let rule = Rect {
                x: rect.x,
                y,
                w: rect.w,
                h: 1.0_f32.max(0.5 * s),
            };
            canvas.round_rect(rule, 0.0, theme.label, theme.hairline);
        };
        hairline(canvas, y);
        y += 10.0 * s;

        if older {
            text.draw(
                canvas,
                left,
                y,
                "이전 내용은 휠로 스크롤",
                SMALL_FONT * s,
                theme.secondary,
            );
            y += small_line;
        }
        // Bubbles behind the person's messages, then every line of text.
        let mut index = 0;
        while index < lines.len() {
            let first = &lines[index];
            let mut end = index + 1;
            while end < lines.len() && lines[end].message == first.message {
                end += 1;
            }
            #[expect(clippy::cast_precision_loss, reason = "a few lines")]
            let (top, count) = (y + index as f32 * line, (end - index) as f32);
            if first.who == Who::Person {
                let widest = lines[index..end]
                    .iter()
                    .map(|l| text.width(&l.text, FONT * s))
                    .fold(0.0, f32::max);
                let bubble = Rect {
                    x: right - widest - 2.0 * pad,
                    y: top - 4.0 * s,
                    w: widest + 2.0 * pad,
                    h: count * line + 7.0 * s,
                };
                canvas.round_rect(bubble, BUBBLE_RADIUS * s, theme.accent, 1.0);
                for (row, l) in lines[index..end].iter().enumerate() {
                    #[expect(clippy::cast_precision_loss, reason = "a few lines")]
                    text.draw(
                        canvas,
                        bubble.x + pad,
                        top + row as f32 * line,
                        &l.text,
                        FONT * s,
                        WHITE,
                    );
                }
            } else {
                let color = if first.who == Who::Ai {
                    theme.label
                } else {
                    theme.secondary
                };
                for (row, l) in lines[index..end].iter().enumerate() {
                    #[expect(clippy::cast_precision_loss, reason = "a few lines")]
                    text.draw(
                        canvas,
                        left,
                        top + row as f32 * line,
                        &l.text,
                        FONT * s,
                        color,
                    );
                }
            }
            index = end;
        }
        #[expect(clippy::cast_precision_loss, reason = "a few lines")]
        {
            y += lines.len() as f32 * line;
        }
        if newer {
            text.draw(
                canvas,
                left,
                y,
                "아래에 최근 내용",
                SMALL_FONT * s,
                theme.secondary,
            );
            y += small_line;
        }
        if !proposal.is_empty() {
            y += 6.0 * s;
            canvas.icon(
                Glyph::Zap,
                left + 6.5 * s,
                y + small_line / 2.0,
                13.0 * s,
                theme.orange,
                1.0,
            );
            text.draw(
                canvas,
                left + 18.0 * s,
                y,
                "실행할 동작",
                SMALL_FONT * s,
                theme.orange,
            );
            y += small_line;
            for (first, placed, line_text) in &proposal {
                if let Some(index) = first {
                    // Orange where it acts at a place marked on the session; grey otherwise.
                    let (fill, ink) = if *placed {
                        ((MARKER, 1.0), WHITE)
                    } else {
                        ((theme.label, theme.fill + 0.06), theme.label)
                    };
                    let center = (left + 8.0 * s, y + line / 2.0 - 2.5 * s);
                    paint_number(canvas, text, center, 7.5 * s, index + 1, fill, ink, s);
                }
                text.draw(canvas, left + chip_room, y, line_text, FONT * s, theme.label);
                y += line;
            }
        }

        // Capsule buttons from the right, the default one first; the keys are named on the left.
        let button_y = rect.y + rect.h - PADDING * s - BUTTON_HEIGHT * s;
        let mut button_right = right;
        for (index, (label, hit)) in buttons.iter().enumerate() {
            // An icon before the label: its width, and the gap after it.
            let icon_room = BUTTON_ICON * s + 6.0 * s;
            let button = Rect {
                x: button_right - text.width(label, FONT * s) - 30.0 * s - icon_room,
                y: button_y,
                w: text.width(label, FONT * s) + 30.0 * s + icon_room,
                h: BUTTON_HEIGHT * s,
            };
            let primary = index == 0 && *hit == Hit::Approve;
            // A lone stop is a red tinted button, as stopping is all there is to do.
            let lone_stop = buttons.len() == 1 && matches!(hit, Hit::Stop | Hit::Decline);
            let ink = if primary {
                WHITE
            } else if lone_stop {
                theme.red
            } else {
                theme.label
            };
            if primary {
                canvas.round_rect(button, button.h / 2.0, theme.accent, 1.0);
            } else if lone_stop {
                canvas.round_rect(button, button.h / 2.0, theme.red, theme.tint);
            } else {
                canvas.round_rect(button, button.h / 2.0, theme.label, theme.fill + 0.04);
            }
            let label_y = button.y + (button.h - line) / 2.0 + 2.0 * s;
            canvas.icon(
                button_glyph(*hit),
                button.x + 14.0 * s + BUTTON_ICON * s / 2.0,
                button.y + button.h / 2.0,
                BUTTON_ICON * s,
                ink,
                1.0,
            );
            text.draw(
                canvas,
                button.x + 14.0 * s + icon_room,
                label_y,
                label,
                FONT * s,
                ink,
            );
            self.hits.push((button, *hit));
            button_right = button.x - 8.0 * s;
        }
        if !hint.is_empty() {
            let hint_y = button_y + (BUTTON_HEIGHT * s - small_line) / 2.0 + 1.0 * s;
            text.draw(canvas, left, hint_y, &hint, SMALL_FONT * s, theme.secondary);
        }
    }

    /// The message box: a glass capsule with a send button once something is typed.
    fn paint_input(&mut self, canvas: &mut Canvas<'_>, rect: Rect) {
        let s = self.scale;
        let theme = self.theme();
        self.paint_glass(canvas, rect, rect.h / 2.0);
        self.hits.push((rect, Hit::Input));
        if self.focused {
            canvas.round_rect_stroke(rect, rect.h / 2.0, 2.0 * s, theme.accent, 0.75);
        }
        let Some(text) = &self.text else { return };
        let size = FONT * s;
        let y = rect.y + (rect.h - text.line_height(size)) / 2.0 + 2.0 * s;
        let x = rect.x + 16.0 * s;
        let send = Rect {
            x: rect.x + rect.w - rect.h + 5.0 * s,
            y: rect.y + 5.0 * s,
            w: rect.h - 10.0 * s,
            h: rect.h - 10.0 * s,
        };
        let width = send.x - x - 8.0 * s;
        let empty = self.input.is_empty() && self.preedit.is_empty();
        if empty && !self.focused {
            text.draw(canvas, x, y, "무엇을 도와드릴까요?", size, theme.secondary);
            return;
        }
        // The end of what is typed stays in view.
        let mut shown: String = format!("{}{}", self.input, self.preedit);
        while text.width(&shown, size) > width - 4.0 * s && !shown.is_empty() {
            shown.remove(0);
        }
        let composing = shown.chars().count().min(self.preedit.chars().count());
        let typed: String = shown
            .chars()
            .take(shown.chars().count() - composing)
            .collect();
        let end = text.draw(canvas, x, y, &typed, size, theme.label);
        let preedit: String = shown
            .chars()
            .skip(shown.chars().count() - composing)
            .collect();
        let end_of_preedit = text.draw(canvas, end, y, &preedit, size, theme.label);
        if !preedit.is_empty() {
            let underline = Rect {
                x: end,
                y: y + text.line_height(size) - 3.0 * s,
                w: end_of_preedit - end,
                h: 1.0 * s,
            };
            canvas.round_rect(underline, 0.0, theme.accent, 1.0);
        }
        if self.focused {
            let caret = Rect {
                x: end_of_preedit + 1.0 * s,
                y: y + 1.0 * s,
                w: 2.0 * s,
                h: text.line_height(size) - 3.0 * s,
            };
            canvas.round_rect(caret, 1.0 * s, theme.accent, 1.0);
        }
        if !empty {
            canvas.circle(
                send.x + send.w / 2.0,
                send.y + send.h / 2.0,
                send.w / 2.0,
                theme.accent,
                1.0,
            );
            canvas.icon(
                Glyph::ArrowUp,
                send.x + send.w / 2.0,
                send.y + send.h / 2.0,
                18.0 * s,
                WHITE,
                1.0,
            );
            self.hits.push((send, Hit::Send));
        }
    }

    /// A glass surface: a soft shadow, what is behind it blurred and tinted, and a hairline.
    fn paint_glass(&self, canvas: &mut Canvas<'_>, rect: Rect, radius: f32) {
        let s = self.scale;
        let theme = self.theme();
        // Many faint layers make one soft shadow, a little below the surface.
        let below = Rect {
            y: rect.y + 5.0 * s,
            ..rect
        };
        canvas.soft_shadow(below, radius, 1.5 * s, 10, 0.012);
        canvas.frost(rect, radius, theme.glass, theme.glass_strength);
        canvas.round_rect_stroke(
            rect,
            radius,
            1.0_f32.max(0.5 * s),
            theme.label,
            theme.hairline,
        );
    }

    /// The character: a glossy sphere whose colors follow the state and swirl while it works.
    fn paint_avatar(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let s = self.scale;
        let theme = self.theme();
        let (cx, cy, r) = (
            rect.x + rect.w / 2.0,
            rect.y + rect.h / 2.0,
            rect.w / 2.0 - 3.0 * s,
        );
        let (colors, speed) = match self.state {
            State::Idle => ([0x5a_c8_fa, 0x00_7a_ff, 0x5e_5c_e6], 0.0),
            State::Thinking => ([0xbf_5a_f2, 0x5e_5c_e6, 0xff_37_5f], 2.6),
            State::Acting => ([0x30_d1_58, 0x64_d2_ff, 0x00_c7_be], 1.8),
            State::Approval => ([0xff_9f_0a, 0xff_d6_0a, 0xff_37_5f], 0.0),
            State::TakenOver => ([0x8e_8e_93, 0xae_ae_b2, 0x63_63_66], 0.0),
        };
        let phase = self.time * speed;
        // A glow that breathes while it works, then the sphere on its shadow.
        let breath = if speed > 0.0 {
            0.5 + 0.5 * (self.time * 3.0).sin()
        } else {
            0.4
        };
        canvas.circle(
            cx,
            cy,
            r + (3.0 + 2.0 * breath) * s,
            colors[1],
            0.14 + 0.12 * breath,
        );
        canvas.circle(cx, cy + 3.0 * s, r, BLACK, 0.18);
        canvas.orb(cx, cy, r, colors, phase);
        canvas.ring(cx, cy, r - 0.5 * s, 1.0 * s, WHITE, 0.35);
        // What it is doing, in the middle; the sparkle breathes while the model thinks.
        let (glyph, glyph_alpha) = match self.state {
            State::Idle => (Glyph::Sparkles, 0.95),
            State::Thinking => (Glyph::Sparkles, 0.7 + 0.3 * (self.time * 4.0).sin()),
            State::Acting => (Glyph::Pointer, 0.95),
            State::Approval => (Glyph::Check, 0.95),
            State::TakenOver => (Glyph::Hand, 0.95),
        };
        canvas.icon(glyph, cx, cy, r * 0.95, WHITE, glyph_alpha);
        let (dot_x, dot_y) = (rect.x + rect.w - 8.0 * s, rect.y + 8.0 * s);
        // Something unread; an approval needs no dot, as it opens the bubble and the character
        // already turns orange.
        if self.unread {
            canvas.circle(dot_x, dot_y, 7.0 * s, WHITE, 1.0);
            canvas.circle(dot_x, dot_y, 5.0 * s, theme.accent, 1.0);
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
            // White under the color, edged with a faint shadow, keeps it visible on any screen.
            // A fine ring around the exact spot (the dot), small enough to show what is under it;
            // the number, the one in the list, sits off to the side.
            canvas.ring(x, y, 7.0 * s, 5.0 * s, BLACK, 0.22);
            canvas.ring(x, y, 7.0 * s, 3.0 * s, WHITE, 0.95);
            canvas.ring(x, y, 7.0 * s, 1.5 * s, MARKER, 1.0);
            canvas.circle(x, y, 1.75 * s, WHITE, 1.0);
            canvas.circle(x, y, 1.0 * s, MARKER, 1.0);
            let badge = (x + 14.0 * s, y - 14.0 * s);
            canvas.circle(badge.0, badge.1, 10.0 * s, BLACK, 0.22);
            canvas.circle(badge.0, badge.1, 9.0 * s, WHITE, 1.0);
            if let Some(text) = &self.text {
                paint_number(canvas, text, badge, 7.75 * s, index + 1, (MARKER, 1.0), WHITE, s);
            }
        }
        if let Some(pointer) = &self.pointer {
            let (px, py) = pointer.position(self.time);
            let (x, y) = ((px + 0.5) * w / sw, (py + 0.5) * h / sh);
            self.paint_pointer(canvas, (x, y), pointer.ripple(self.time));
        }
    }

    /// The AI's pointer as an arrow whose tip is at (`x`, `y`), tagged "AI" so it is not taken
    /// for the person's; `ripple` (0 to 1) spreads a ring from the tip after a click.
    fn paint_pointer(&self, canvas: &mut Canvas<'_>, (x, y): (f32, f32), ripple: Option<f32>) {
        let s = self.scale;
        let color = state_color(State::Acting);
        if let Some(ripple) = ripple {
            let eased = 1.0 - (1.0 - ripple).powi(2);
            canvas.ring(x, y, (4.0 + 18.0 * eased) * s, 2.5 * s, color, 0.85 * (1.0 - ripple));
        }
        let arrow = |dx: f32, dy: f32| ARROW.map(|(ax, ay)| (x + (ax + dx) * s, y + (ay + dy) * s));
        canvas.polygon(&arrow(0.6, 1.4), 1.8 * s, BLACK, 0.25);
        canvas.polygon(&arrow(0.0, 0.0), 1.5 * s, WHITE, 1.0);
        canvas.polygon(&arrow(0.0, 0.0), 0.0, color, 1.0);
        let Some(text) = &self.text else { return };
        let size = 10.0 * s;
        let tag = Rect {
            x: x + 12.0 * s,
            y: y + 16.0 * s,
            w: text.width("AI", size) + 10.0 * s,
            h: 16.0 * s,
        };
        let edge = Rect {
            x: tag.x - 1.0 * s,
            y: tag.y - 1.0 * s,
            w: tag.w + 2.0 * s,
            h: tag.h + 2.0 * s,
        };
        canvas.round_rect(edge, edge.h / 2.0, WHITE, 1.0);
        canvas.round_rect(tag, tag.h / 2.0, color, 1.0);
        text.draw(canvas, tag.x + 5.0 * s, tag.y + (tag.h - text.line_height(size)) / 2.0, "AI", size, WHITE);
    }

    /// "AI 조작 중" in a dark glass capsule at the top of the window, with a stop button.
    fn paint_badge(&mut self, canvas: &mut Canvas<'_>, w: f32) {
        let s = self.scale;
        let Some(text) = &self.text else { return };
        let label = "AI 조작 중";
        let stop = "중단";
        let size = SMALL_FONT * s;
        let line = text.line_height(size);
        let stop_width = text.width(stop, size) + 32.0 * s;
        let width = 32.0 * s + text.width(label, size) + 12.0 * s + stop_width + 5.0 * s;
        let badge = Rect {
            x: (w - width) / 2.0,
            y: 10.0 * s,
            w: width,
            h: 32.0 * s,
        };
        canvas.frost(badge, badge.h / 2.0, 0x1c_1c_1e, 0.82);
        canvas.round_rect_stroke(badge, badge.h / 2.0, 1.0_f32.max(0.5 * s), WHITE, 0.16);
        let pulse = 0.6 + 0.4 * (self.time * 3.0).sin();
        canvas.icon(
            Glyph::Pointer,
            badge.x + 17.0 * s,
            badge.y + badge.h / 2.0,
            14.0 * s,
            state_color(State::Acting),
            pulse,
        );
        let y = badge.y + (badge.h - line) / 2.0 + 1.0 * s;
        text.draw(canvas, badge.x + 31.0 * s, y, label, size, WHITE);
        let button = Rect {
            x: badge.x + badge.w - stop_width - 5.0 * s,
            y: badge.y + 5.0 * s,
            w: stop_width,
            h: badge.h - 10.0 * s,
        };
        canvas.round_rect(button, button.h / 2.0, WHITE, 0.18);
        canvas.icon(
            Glyph::Square,
            button.x + 14.0 * s,
            button.y + button.h / 2.0,
            10.0 * s,
            WHITE,
            1.0,
        );
        text.draw(
            canvas,
            button.x + 22.0 * s,
            button.y + (button.h - line) / 2.0 + 1.0 * s,
            stop,
            size,
            WHITE,
        );
        self.hits.push((button, Hit::Stop));
    }
}

/// Model text as plain text: the bubble shows no Markdown, so its marks go.
fn plain(text: &str) -> String {
    text.lines()
        .map(|line| {
            let line = line.trim_start_matches('#').trim_start_matches(' ');
            line.replace("**", "").replace("__", "").replace('`', "")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The icon of a capsule button.
fn button_glyph(hit: Hit) -> Glyph {
    match hit {
        Hit::Stop => Glyph::Square,
        Hit::Decline => Glyph::X,
        _ => Glyph::Check,
    }
}

/// A number in a filled circle: where a proposed action acts, on the session and in the list.
#[expect(clippy::too_many_arguments, reason = "a drawing primitive")]
fn paint_number(
    canvas: &mut Canvas<'_>,
    text: &Text,
    (x, y): (f32, f32),
    radius: f32,
    number: usize,
    (fill, alpha): (Color, f32),
    ink: Color,
    s: f32,
) {
    canvas.circle(x, y, radius, fill, alpha);
    let label = number.to_string();
    let size = SMALL_FONT * s;
    text.draw(
        canvas,
        x - text.width(&label, size) / 2.0,
        y - text.line_height(size) / 2.0 + 1.0 * s,
        &label,
        size,
        ink,
    );
}

/// The buttons under the conversation, the default one first, and what the keys do now: with
/// something typed, Return sends it instead of answering.
fn controls(state: State, typing: bool) -> (&'static [(&'static str, Hit)], String) {
    let (buttons, enter, escape): (&'static [(&'static str, Hit)], Option<&str>, &str) = match state {
        State::Approval => (&[("실행", Hit::Approve), ("거절", Hit::Decline)], Some("실행"), "거절"),
        State::TakenOver => (&[("계속", Hit::Approve), ("중단", Hit::Stop)], Some("계속"), "중단"),
        State::Thinking | State::Acting => (&[("중단", Hit::Stop)], None, "중단"),
        State::Idle => return (&[], String::new()),
    };
    let first = match (typing, enter) {
        (true, _) => "Enter 전송".to_owned(),
        (false, Some(enter)) => format!("Enter {enter}"),
        (false, None) => return (buttons, format!("Esc {escape}")),
    };
    (buttons, format!("{first} · Esc {escape}"))
}

fn state_label(state: State) -> &'static str {
    match state {
        State::Idle => "대기 중",
        State::Thinking => "생각 중",
        State::Acting => "조작 중",
        State::Approval => "확인 필요",
        State::TakenOver => "일시 정지",
    }
}

fn state_color(state: State) -> Color {
    match state {
        State::Idle => 0x8e_8e_93,
        State::Thinking => 0xbf_5a_f2,
        State::Acting => 0x30_d1_58,
        State::Approval => ORANGE,
        State::TakenOver => 0x8e_8e_93,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paints once, and again a second later, when any fade has finished.
    fn painted(overlay: &mut Overlay) {
        let mut pixels = vec![0u32; 1280 * 800];
        overlay.paint(&mut pixels, 1280, 800);
        overlay.set_time(overlay.time + 1.0);
        overlay.paint(&mut pixels, 1280, 800);
    }

    /// Draws the overlay in each state over a mock desktop into `$HOPDESK_SHOTS`, to look at:
    /// `HOPDESK_SHOTS=<dir> cargo test --bin hopdesk overlay_screenshots -- --ignored`.
    #[test]
    #[ignore = "writes images for a person to look at"]
    fn overlay_screenshots() {
        let Some(dir) = std::env::var_os("HOPDESK_SHOTS") else {
            return;
        };
        let (width, height) = (1280usize, 800usize);
        // Name, state, dark appearance, dark screen behind, and whether anything was said and typed.
        let scenes: [(&str, State, bool, bool, bool); 9] = [
            ("o1-approval", State::Approval, false, false, true),
            ("o2-acting", State::Acting, false, false, true),
            ("o3-thinking-dark", State::Thinking, true, true, true),
            ("o4-approval-dark", State::Approval, true, true, true),
            ("o5-takenover", State::TakenOver, false, false, true),
            ("o6-idle", State::Idle, false, false, true),
            ("o7-greeting", State::Idle, false, false, false),
            // The glass over a screen of the other brightness: the text must still read.
            ("o8-approval-on-dark-screen", State::Approval, false, true, false),
            ("o9-acting-dark-on-light-screen", State::Acting, true, false, false),
        ];
        for (name, state, dark, dark_screen, talked) in scenes {
            let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
            overlay.set_dark(dark);
            overlay.set_session((1920, 1080));
            overlay.toggle();
            if talked || state != State::Idle {
                overlay.say("메모장을 열어서 안녕하세요라고 입력할게요.".into());
            }
            overlay.set_state(state);
            if state == State::Approval {
                overlay.propose(vec![
                    ("클릭".into(), Some(Point { x: 600, y: 400 })),
                    ("입력: 안녕하세요".into(), None),
                ]);
            }
            if state == State::Acting {
                overlay.set_pointer(Point { x: 900, y: 500 }, false);
            }
            if talked {
                overlay.type_text("창을 닫아줘");
            }
            // A mock desktop: a light or dark gradient with a window on it.
            let dark = dark_screen;
            let base: u32 = if dark { 0x20_24_30 } else { 0xc9_d6_ea };
            let mut pixels: Vec<u32> = (0..width * height)
                .map(|i| {
                    mix(
                        base,
                        if dark { 0x0a_0c_14 } else { 0xf4_f7_fc },
                        (i / width) as f32 / height as f32,
                    )
                })
                .collect();
            let mut canvas = Canvas {
                pixels: &mut pixels,
                width,
                height,
            };
            canvas.round_rect(
                Rect {
                    x: 120.0,
                    y: 90.0,
                    w: 760.0,
                    h: 520.0,
                },
                12.0,
                if dark { 0x2c_2c_2e } else { 0xff_ff_ff },
                1.0,
            );
            painted(&mut overlay);
            overlay.paint(&mut pixels, width, height);
            let mut rgb = Vec::with_capacity(width * height * 3);
            for pixel in &pixels {
                rgb.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8]);
            }
            let image = image::RgbImage::from_raw(width as u32, height as u32, rgb).unwrap();
            image
                .save(std::path::Path::new(&dir).join(format!("{name}.png")))
                .unwrap();
        }
    }

    /// An overlay showing the AI's pointer over a black 1280x800 session, the window's size.
    fn pointing() -> (Overlay, Vec<u32>) {
        let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
        overlay.set_session((1280, 800));
        // A state that does not move by itself, so only the pointer does.
        overlay.set_state(State::Approval);
        (overlay, vec![0u32; 1280 * 800])
    }

    #[test]
    fn the_ai_pointer_glides_to_its_new_place() {
        let (mut overlay, mut pixels) = pointing();
        overlay.set_pointer(Point { x: 100, y: 100 }, false);
        painted(&mut overlay);
        assert!(!overlay.animating(), "it stands still once there");
        let start = overlay.time;
        overlay.set_pointer(Point { x: 300, y: 100 }, false);
        overlay.paint(&mut pixels, 1280, 800);
        assert!(overlay.animating());
        overlay.set_time(start + GLIDE / 2.0);
        let (x, y) = overlay.pointer.as_ref().unwrap().position(overlay.time);
        assert!(100.0 < x && x < 300.0 && (y - 100.0).abs() < f32::EPSILON, "on its way: {x}");
        overlay.set_time(start + GLIDE);
        assert_eq!(overlay.pointer.as_ref().unwrap().position(overlay.time), (300.0, 100.0));
        assert!(!overlay.animating());
    }

    #[test]
    fn a_click_ripples_once_the_pointer_arrives() {
        let (mut overlay, mut pixels) = pointing();
        overlay.set_pointer(Point { x: 100, y: 100 }, true);
        overlay.paint(&mut pixels, 1280, 800);
        let start = overlay.time;
        overlay.set_time(start + GLIDE + RIPPLE / 2.0);
        assert!(overlay.animating(), "rippling");
        overlay.set_time(start + GLIDE + RIPPLE);
        assert!(!overlay.animating());
    }

    #[test]
    fn the_arrow_tip_is_on_the_spot() {
        let (mut overlay, mut pixels) = pointing();
        overlay.set_pointer(Point { x: 600, y: 400 }, false);
        painted(&mut overlay);
        overlay.paint(&mut pixels, 1280, 800);
        let at = |x: usize, y: usize| pixels[y * 1280 + x];
        assert_ne!(at(600, 400), 0, "the tip covers the pixel it points at");
        assert_eq!(at(603, 410), state_color(State::Acting), "the arrow body below the tip");
        assert_eq!(at(597, 397), 0, "nothing above and left of the tip");
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
        // Clicking a button inside the bubble is that button, not the bubble.
        for wanted in [Hit::Approve, Hit::Decline, Hit::Input] {
            let rect = overlay.hits.iter().find(|(_, hit)| *hit == wanted).unwrap().0;
            assert_eq!(overlay.hit(rect.x + rect.w / 2.0, rect.y + rect.h / 2.0), Some(wanted));
        }
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
        overlay.toggle();
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
    fn the_bubble_starts_hidden_and_opens_when_the_person_is_needed() {
        let mut overlay = Overlay::new(None, "api.anthropic.com");
        assert!(!overlay.expanded);
        overlay.say("**Done**: opened `Notepad`".into());
        assert!(overlay.unread && !overlay.expanded, "a reply only marks the character");
        assert_eq!(overlay.messages.back().unwrap().1, "Done: opened Notepad", "no Markdown marks");
        overlay.propose(vec![("클릭".into(), None)]);
        assert!(overlay.expanded && !overlay.unread, "approval opens the bubble");
        overlay.toggle();
        overlay.notice("오류: 연결 실패".into());
        assert!(overlay.expanded, "errors open the bubble");
    }

    #[test]
    fn a_long_conversation_scrolls_back_and_returns_on_new_lines() {
        let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
        if overlay.text.is_none() {
            return;
        }
        overlay.toggle();
        for index in 0..40 {
            overlay.say(format!("line {index}"));
        }
        painted(&mut overlay);
        assert!(overlay.max_scroll > 0, "not everything fits");
        overlay.scroll_by(5);
        assert_eq!(overlay.scroll, 5);
        overlay.scroll_by(1000);
        assert_eq!(overlay.scroll, overlay.max_scroll, "no further than the first line");
        overlay.scroll_by(-1000);
        assert_eq!(overlay.scroll, 0);
        overlay.scroll_by(3);
        overlay.say("new".into());
        assert_eq!(overlay.scroll, 0, "new lines bring the newest into view");
    }

    #[test]
    fn the_panel_fades_in() {
        let mut overlay = Overlay::new(Text::load(), "api.anthropic.com");
        if overlay.text.is_none() {
            return;
        }
        let mut pixels = vec![0u32; 1280 * 800];
        overlay.set_state(State::Approval);
        overlay.propose(vec![("클릭".into(), None)]);
        overlay.paint(&mut pixels, 1280, 800);
        assert!(overlay.animating() && overlay.visibility < 1.0, "fading in");
        overlay.set_time(FADE / 2.0);
        overlay.paint(&mut pixels, 1280, 800);
        assert!((0.0..1.0).contains(&overlay.visibility) && overlay.visibility > 0.0);
        overlay.set_time(1.5);
        overlay.paint(&mut pixels, 1280, 800);
        assert!((overlay.visibility - 1.0).abs() < f32::EPSILON && overlay.fade.is_none());
    }

    #[test]
    fn the_hint_names_what_the_keys_do_now() {
        let labels = |state, typing| {
            let (buttons, hint) = controls(state, typing);
            (buttons.iter().map(|(label, _)| *label).collect::<Vec<_>>(), hint)
        };
        assert_eq!(labels(State::Approval, false), (vec!["실행", "거절"], "Enter 실행 · Esc 거절".to_owned()));
        // With something typed, Return sends it instead of answering.
        assert_eq!(labels(State::Approval, true).1, "Enter 전송 · Esc 거절");
        // Stopping is "중단" in every state.
        assert_eq!(labels(State::TakenOver, false), (vec!["계속", "중단"], "Enter 계속 · Esc 중단".to_owned()));
        assert_eq!(labels(State::TakenOver, true).1, "Enter 전송 · Esc 중단");
        assert_eq!(labels(State::Acting, false), (vec!["중단"], "Esc 중단".to_owned()));
        assert_eq!(labels(State::Thinking, true).1, "Enter 전송 · Esc 중단");
        assert_eq!(labels(State::Idle, false), (vec![], String::new()));
    }

    #[test]
    fn the_greeting_shows_only_until_the_conversation_starts() {
        let mut overlay = Overlay::new(None, "api.anthropic.com");
        assert!(overlay.messages.is_empty(), "the greeting is not a message");
        assert!(overlay.greeting.contains("api.anthropic.com"), "it names where the screen goes");
        assert_eq!(overlay.conversation().len(), 1);
        overlay.type_text("메모장 열어 줘");
        overlay.take_input();
        let conversation = overlay.conversation();
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0].0, Who::Person);
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
