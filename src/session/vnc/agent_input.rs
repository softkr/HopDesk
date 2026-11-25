//! AI actions as VNC input (docs/ai-computer-use-plan.md §5.3).

// Until the agent loop drives it (step 5 of the plan), only the tests use this.
#![cfg_attr(not(test), expect(dead_code, reason = "used by the agent loop"))]

use vnc::{ClientKeyEvent, ClientMouseEvent, X11Event};

use crate::agent::action::{self, Action, Button, Direction, KeyCombo, Point};

/// Input the AI sends to the server, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentInput {
    /// Pointer position and button mask.
    Pointer(u16, u16, u8),
    /// X11 keysym pressed (`true`) or released.
    Key(u32, bool),
    /// Text for the remote clipboard.
    Clipboard(String),
}

impl From<AgentInput> for X11Event {
    fn from(input: AgentInput) -> Self {
        match input {
            AgentInput::Pointer(x, y, buttons) => X11Event::PointerEvent(ClientMouseEvent {
                position_x: x,
                position_y: y,
                bottons: buttons,
            }),
            AgentInput::Key(keycode, down) => X11Event::KeyEvent(ClientKeyEvent { keycode, down }),
            AgentInput::Clipboard(text) => X11Event::CopyText(text),
        }
    }
}

const LEFT_BUTTON: u8 = 1;
const MIDDLE_BUTTON: u8 = 1 << 1;
const RIGHT_BUTTON: u8 = 1 << 2;

/// Characters a US keyboard types with Shift; held for them like a person would.
const SHIFTED: &str = "~!@#$%^&*()_+{}|:\"<>?";

/// Turns AI actions into VNC input, keeping the pointer position and the held left button
/// between actions like a mouse does.
pub(super) struct AgentDriver {
    pointer: (u16, u16),
    buttons: u8,
    /// Pastes the remote clipboard: text outside Latin-1 (e.g. Hangul) is pasted, since keys
    /// are sent by position and keysyms of other scripts are not typed by every server.
    paste: KeyCombo,
}

impl AgentDriver {
    pub(super) fn new(paste: KeyCombo) -> Self {
        Self {
            pointer: (0, 0),
            buttons: 0,
            paste,
        }
    }

    pub(super) fn cursor(&self) -> Point {
        Point {
            x: u32::from(self.pointer.0),
            y: u32::from(self.pointer.1),
        }
    }

    /// The input for `action`; empty for actions that only look at the screen or wait.
    pub(super) fn input(&mut self, action: &Action) -> Vec<AgentInput> {
        let mut out = Vec::new();
        match action {
            Action::Screenshot | Action::Zoom { .. } | Action::CursorPosition | Action::Wait { .. } => {}
            Action::MouseMove { to } => self.move_to(*to, &mut out),
            Action::MouseDown => self.set_buttons(self.buttons | LEFT_BUTTON, &mut out),
            Action::MouseUp => self.set_buttons(self.buttons & !LEFT_BUTTON, &mut out),
            Action::Click {
                at,
                button,
                count,
                modifiers,
            } => {
                if let Some(at) = at {
                    self.move_to(*at, &mut out);
                }
                let bit = match button {
                    Button::Left => LEFT_BUTTON,
                    Button::Middle => MIDDLE_BUTTON,
                    Button::Right => RIGHT_BUTTON,
                };
                let held = self.buttons;
                hold(modifiers, &mut out, |out| {
                    for _ in 0..*count {
                        self.set_buttons(held | bit, out);
                        self.set_buttons(held, out);
                    }
                });
            }
            Action::Drag { from, to, modifiers } => {
                self.move_to(*from, &mut out);
                hold(modifiers, &mut out, |out| {
                    self.set_buttons(self.buttons | LEFT_BUTTON, out);
                    // A step in between: some apps start a drag only after the pointer moved.
                    let middle = Point {
                        x: from.x.midpoint(to.x),
                        y: from.y.midpoint(to.y),
                    };
                    self.move_to(middle, out);
                    self.move_to(*to, out);
                    self.set_buttons(self.buttons & !LEFT_BUTTON, out);
                });
            }
            Action::Scroll {
                at,
                direction,
                amount,
                modifiers,
            } => {
                if let Some(at) = at {
                    self.move_to(*at, &mut out);
                }
                // Buttons 4/5 scroll up/down, 6/7 left/right: one press and release per notch.
                let bit = match direction {
                    Direction::Up => 1 << 3,
                    Direction::Down => 1 << 4,
                    Direction::Left => 1 << 5,
                    Direction::Right => 1 << 6,
                };
                let held = self.buttons;
                hold(modifiers, &mut out, |out| {
                    for _ in 0..*amount {
                        self.set_buttons(held | bit, out);
                        self.set_buttons(held, out);
                    }
                });
            }
            Action::Type { text } => self.type_text(text, &mut out),
            Action::Key { combo, repeat } => {
                for _ in 0..*repeat {
                    hold(&combo.modifiers, &mut out, |out| tap(combo.key, out));
                }
            }
        }
        out
    }

    fn move_to(&mut self, to: Point, out: &mut Vec<AgentInput>) {
        let clamp = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        self.pointer = (clamp(to.x), clamp(to.y));
        out.push(AgentInput::Pointer(self.pointer.0, self.pointer.1, self.buttons));
    }

    fn set_buttons(&mut self, buttons: u8, out: &mut Vec<AgentInput>) {
        self.buttons = buttons;
        out.push(AgentInput::Pointer(self.pointer.0, self.pointer.1, buttons));
    }

    /// Latin-1 text is typed key by key; runs of other characters go through the clipboard.
    fn type_text(&self, text: &str, out: &mut Vec<AgentInput>) {
        let mut pasted = String::new();
        for c in text.chars() {
            let typed = (u32::from(c) <= 0xff).then(|| action::char_keysym(c)).flatten();
            let Some(keysym) = typed else {
                pasted.push(c);
                continue;
            };
            self.paste_run(&mut pasted, out);
            let shift = c.is_ascii_uppercase() || SHIFTED.contains(c);
            let modifiers: &[u32] = if shift { &[action::keysym::SHIFT] } else { &[] };
            hold(modifiers, out, |out| tap(keysym, out));
        }
        self.paste_run(&mut pasted, out);
    }

    fn paste_run(&self, pasted: &mut String, out: &mut Vec<AgentInput>) {
        if pasted.is_empty() {
            return;
        }
        out.push(AgentInput::Clipboard(core::mem::take(pasted)));
        hold(&self.paste.modifiers, out, |out| tap(self.paste.key, out));
    }
}

/// Presses `modifiers`, runs `inner`, then releases them in reverse order.
fn hold(modifiers: &[u32], out: &mut Vec<AgentInput>, inner: impl FnOnce(&mut Vec<AgentInput>)) {
    out.extend(modifiers.iter().map(|keysym| AgentInput::Key(*keysym, true)));
    inner(out);
    out.extend(modifiers.iter().rev().map(|keysym| AgentInput::Key(*keysym, false)));
}

fn tap(keysym: u32, out: &mut Vec<AgentInput>) {
    out.push(AgentInput::Key(keysym, true));
    out.push(AgentInput::Key(keysym, false));
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::AgentInput::{Clipboard, Key, Pointer};
    use super::*;
    use crate::agent::action::keysym::{CONTROL, RETURN, SHIFT};

    fn driver() -> AgentDriver {
        AgentDriver::new(action::parse_combo("ctrl+v").unwrap())
    }

    fn at(x: u32, y: u32) -> Point {
        Point { x, y }
    }

    fn click(point: Option<Point>, button: Button, count: u8, modifiers: Vec<u32>) -> Action {
        Action::Click {
            at: point,
            button,
            count,
            modifiers,
        }
    }

    #[test]
    fn a_click_moves_then_presses_and_releases() {
        let mut driver = driver();
        let out = driver.input(&click(Some(at(10, 20)), Button::Left, 1, vec![]));
        assert_eq!(out, [Pointer(10, 20, 0), Pointer(10, 20, 1), Pointer(10, 20, 0)]);
        assert_eq!(driver.cursor(), at(10, 20));
        // Without a position the click lands where the pointer is.
        let out = driver.input(&click(None, Button::Right, 2, vec![]));
        assert_eq!(
            out,
            [Pointer(10, 20, 4), Pointer(10, 20, 0), Pointer(10, 20, 4), Pointer(10, 20, 0)]
        );
    }

    #[test]
    fn modifiers_are_held_around_the_click_only() {
        let mut driver = driver();
        let out = driver.input(&click(Some(at(1, 1)), Button::Left, 1, vec![SHIFT, CONTROL]));
        assert_eq!(
            out,
            [
                Pointer(1, 1, 0),
                Key(SHIFT, true),
                Key(CONTROL, true),
                Pointer(1, 1, 1),
                Pointer(1, 1, 0),
                Key(CONTROL, false),
                Key(SHIFT, false),
            ]
        );
    }

    #[test]
    fn a_drag_passes_through_the_middle() {
        let mut driver = driver();
        let out = driver.input(&Action::Drag {
            from: at(0, 0),
            to: at(100, 50),
            modifiers: vec![],
        });
        assert_eq!(
            out,
            [
                Pointer(0, 0, 0),
                Pointer(0, 0, 1),
                Pointer(50, 25, 1),
                Pointer(100, 50, 1),
                Pointer(100, 50, 0),
            ]
        );
    }

    #[test]
    fn the_left_button_stays_down_until_released() {
        let mut driver = driver();
        assert_eq!(driver.input(&Action::MouseDown), [Pointer(0, 0, 1)]);
        assert_eq!(driver.input(&Action::MouseMove { to: at(5, 5) }), [Pointer(5, 5, 1)]);
        assert_eq!(driver.input(&Action::MouseUp), [Pointer(5, 5, 0)]);
    }

    #[test]
    fn scrolling_presses_a_wheel_button_per_notch() {
        let mut driver = driver();
        let out = driver.input(&Action::Scroll {
            at: Some(at(3, 4)),
            direction: Direction::Down,
            amount: 2,
            modifiers: vec![],
        });
        assert_eq!(
            out,
            [Pointer(3, 4, 0), Pointer(3, 4, 16), Pointer(3, 4, 0), Pointer(3, 4, 16), Pointer(3, 4, 0)]
        );
    }

    #[test]
    fn text_is_typed_with_shift_where_a_keyboard_needs_it() {
        let out = driver().input(&Action::Type { text: "aB!\n".into() });
        let a = u32::from('a');
        let b = u32::from('B');
        let bang = u32::from('!');
        assert_eq!(
            out,
            [
                Key(a, true),
                Key(a, false),
                Key(SHIFT, true),
                Key(b, true),
                Key(b, false),
                Key(SHIFT, false),
                Key(SHIFT, true),
                Key(bang, true),
                Key(bang, false),
                Key(SHIFT, false),
                Key(RETURN, true),
                Key(RETURN, false),
            ]
        );
    }

    #[test]
    fn hangul_is_pasted_through_the_clipboard() {
        let out = driver().input(&Action::Type { text: "a한글b".into() });
        let v = u32::from('v');
        assert_eq!(
            out,
            [
                Key(u32::from('a'), true),
                Key(u32::from('a'), false),
                Clipboard("한글".into()),
                Key(CONTROL, true),
                Key(v, true),
                Key(v, false),
                Key(CONTROL, false),
                Key(u32::from('b'), true),
                Key(u32::from('b'), false),
            ]
        );
    }

    #[test]
    fn a_key_combination_repeats() {
        let combo = action::parse_combo("ctrl+z").unwrap();
        let z = u32::from('z');
        let out = driver().input(&Action::Key { combo, repeat: 2 });
        let once = [Key(CONTROL, true), Key(z, true), Key(z, false), Key(CONTROL, false)];
        assert_eq!(out, [once.clone(), once].concat());
    }

    #[test]
    fn looking_and_waiting_send_nothing() {
        let mut driver = driver();
        for action in [
            Action::Screenshot,
            Action::CursorPosition,
            Action::Wait {
                duration: Duration::from_secs(1),
            },
        ] {
            assert_eq!(driver.input(&action), []);
        }
    }

    #[test]
    fn positions_beyond_the_protocol_range_are_clamped() {
        let out = driver().input(&Action::MouseMove { to: at(70_000, 3) });
        assert_eq!(out, [Pointer(u16::MAX, 3, 0)]);
    }
}
