//! AI actions as RDP input (docs/ai-computer-use-plan.md §5.3).
//!
//! Keys of a combination go as scancodes of a US keyboard, like a person's keys; typed text
//! goes as Unicode key events, so Hangul and other scripts arrive as they are.

use ironrdp_input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use ironrdp_pdu::input::fast_path::FastPathInputEvent;

use crate::agent::action::{Action, Button, Direction, Point, keysym};

/// One wheel notch, as Windows counts it.
const WHEEL_NOTCH: i16 = 120;

/// Turns AI actions into RDP input, keeping its own record of what it holds down, apart from
/// the person's input.
pub(super) struct AgentDriver {
    database: Database,
    pointer: Point,
}

impl AgentDriver {
    pub(super) fn new() -> Self {
        Self {
            database: Database::new(),
            pointer: Point { x: 0, y: 0 },
        }
    }

    pub(super) fn cursor(&self) -> Point {
        self.pointer
    }

    /// The input for `action`; empty for actions that only look at the screen or wait.
    pub(super) fn input(&mut self, action: &Action) -> Vec<FastPathInputEvent> {
        let operations = self.operations(action);
        self.database.apply(operations).into_vec()
    }

    fn operations(&mut self, action: &Action) -> Vec<Operation> {
        let mut out = Vec::new();
        match action {
            Action::Screenshot | Action::Zoom { .. } | Action::CursorPosition | Action::Wait { .. } => {}
            Action::MouseMove { to } => self.move_to(*to, &mut out),
            Action::MouseDown => out.push(Operation::MouseButtonPressed(MouseButton::Left)),
            Action::MouseUp => out.push(Operation::MouseButtonReleased(MouseButton::Left)),
            Action::Click {
                at,
                button,
                count,
                modifiers,
            } => {
                if let Some(at) = at {
                    self.move_to(*at, &mut out);
                }
                let button = match button {
                    Button::Left => MouseButton::Left,
                    Button::Middle => MouseButton::Middle,
                    Button::Right => MouseButton::Right,
                };
                hold(modifiers, &mut out, |out| {
                    for _ in 0..*count {
                        out.push(Operation::MouseButtonPressed(button));
                        out.push(Operation::MouseButtonReleased(button));
                    }
                });
            }
            Action::Drag { from, to, modifiers } => {
                self.move_to(*from, &mut out);
                hold(modifiers, &mut out, |out| {
                    out.push(Operation::MouseButtonPressed(MouseButton::Left));
                    // A step in between: some apps start a drag only after the pointer moved.
                    let middle = Point {
                        x: from.x.midpoint(to.x),
                        y: from.y.midpoint(to.y),
                    };
                    self.move_to(middle, out);
                    self.move_to(*to, out);
                    out.push(Operation::MouseButtonReleased(MouseButton::Left));
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
                // Positive turns scroll up, or right.
                let (is_vertical, rotation_units) = match direction {
                    Direction::Up => (true, WHEEL_NOTCH),
                    Direction::Down => (true, -WHEEL_NOTCH),
                    Direction::Right => (false, WHEEL_NOTCH),
                    Direction::Left => (false, -WHEEL_NOTCH),
                };
                hold(modifiers, &mut out, |out| {
                    for _ in 0..*amount {
                        out.push(Operation::WheelRotations(WheelRotations {
                            is_vertical,
                            rotation_units,
                        }));
                    }
                });
            }
            Action::Type { text } => {
                for c in text.chars() {
                    match c {
                        '\n' => tap(scancode(keysym::RETURN).0, &mut out),
                        '\t' => tap(scancode(keysym::TAB).0, &mut out),
                        '\r' => {}
                        c => {
                            out.push(Operation::UnicodeKeyPressed(c));
                            out.push(Operation::UnicodeKeyReleased(c));
                        }
                    }
                }
            }
            Action::Key { combo, repeat } => {
                for _ in 0..*repeat {
                    hold(&combo.modifiers, &mut out, |out| press_key(combo.key, out));
                }
            }
        }
        out
    }

    fn move_to(&mut self, to: Point, out: &mut Vec<Operation>) {
        let clamp = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
        self.pointer = to;
        out.push(Operation::MouseMove(MousePosition {
            x: clamp(to.x),
            y: clamp(to.y),
        }));
    }
}

/// Presses the modifier keys, runs `inner`, then releases them in reverse order.
fn hold(modifiers: &[u32], out: &mut Vec<Operation>, inner: impl FnOnce(&mut Vec<Operation>)) {
    let codes: Vec<Scancode> = modifiers.iter().filter_map(|keysym| known_scancode(*keysym)).collect();
    out.extend(codes.iter().map(|code| Operation::KeyPressed(*code)));
    inner(out);
    out.extend(codes.iter().rev().map(|code| Operation::KeyReleased(*code)));
}

fn tap(code: Scancode, out: &mut Vec<Operation>) {
    out.push(Operation::KeyPressed(code));
    out.push(Operation::KeyReleased(code));
}

/// A key by its scancode (with Shift for shifted symbols), or as a Unicode key without one.
fn press_key(keysym: u32, out: &mut Vec<Operation>) {
    match scancode_and_shift(keysym) {
        Some((code, false)) => tap(code, out),
        Some((code, true)) => {
            let shift = scancode(keysym::SHIFT).0;
            out.push(Operation::KeyPressed(shift));
            tap(code, out);
            out.push(Operation::KeyReleased(shift));
        }
        None => {
            let c = char::from_u32(keysym.checked_sub(keysym::UNICODE).unwrap_or(keysym));
            if let Some(c) = c {
                out.push(Operation::UnicodeKeyPressed(c));
                out.push(Operation::UnicodeKeyReleased(c));
            }
        }
    }
}

fn known_scancode(keysym: u32) -> Option<Scancode> {
    scancode_and_shift(keysym).map(|(code, _)| code)
}

/// The scancode of a key every keyboard has.
fn scancode(keysym: u32) -> (Scancode, bool) {
    scancode_and_shift(keysym).expect("a key every keyboard has")
}

/// Set 1 scancode of `keysym` on a US keyboard, and whether Shift types it.
fn scancode_and_shift(keysym: u32) -> Option<(Scancode, bool)> {
    let plain = |code: u8| Some((Scancode::from_u8(false, code), false));
    let shifted = |code: u8| Some((Scancode::from_u8(false, code), true));
    let extended = |code: u8| Some((Scancode::from_u8(true, code), false));
    if let Some(c) = char::from_u32(keysym).filter(char::is_ascii_lowercase) {
        const LETTERS: &[u8; 26] = &[
            0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
            0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
        ];
        return plain(LETTERS[usize::try_from(u32::from(c) - u32::from('a')).ok()?]);
    }
    if let Some(c) = char::from_u32(keysym).filter(char::is_ascii_uppercase) {
        return scancode_and_shift(u32::from(c.to_ascii_lowercase())).map(|(code, _)| (code, true));
    }
    // F1-F10 are 0x3b-0x44, F11 and F12 0x57 and 0x58.
    if (0xffbe..=0xffc7).contains(&keysym) {
        return plain(0x3b + u8::try_from(keysym - 0xffbe).ok()?);
    }
    match keysym {
        keysym::SHIFT => plain(0x2a),
        keysym::CONTROL => plain(0x1d),
        keysym::ALT => plain(0x38),
        keysym::SUPER => extended(0x5b),
        keysym::RETURN => plain(0x1c),
        keysym::TAB => plain(0x0f),
        0xffc8 => plain(0x57),
        0xffc9 => plain(0x58),
        0xff8d => extended(0x1c),
        0xff1b => plain(0x01),
        0xff08 => plain(0x0e),
        0xffff => extended(0x53),
        0xff63 => extended(0x52),
        0xff50 => extended(0x47),
        0xff57 => extended(0x4f),
        0xff55 => extended(0x49),
        0xff56 => extended(0x51),
        0xff51 => extended(0x4b),
        0xff52 => extended(0x48),
        0xff53 => extended(0x4d),
        0xff54 => extended(0x50),
        0xffe5 => plain(0x3a),
        0xff61 => extended(0x37),
        0xff67 => extended(0x5d),
        _ => match char::from_u32(keysym)? {
            ' ' => plain(0x39),
            '1' => plain(0x02),
            '2' => plain(0x03),
            '3' => plain(0x04),
            '4' => plain(0x05),
            '5' => plain(0x06),
            '6' => plain(0x07),
            '7' => plain(0x08),
            '8' => plain(0x09),
            '9' => plain(0x0a),
            '0' => plain(0x0b),
            '-' => plain(0x0c),
            '=' => plain(0x0d),
            '[' => plain(0x1a),
            ']' => plain(0x1b),
            ';' => plain(0x27),
            '\'' => plain(0x28),
            '`' => plain(0x29),
            '\\' => plain(0x2b),
            ',' => plain(0x33),
            '.' => plain(0x34),
            '/' => plain(0x35),
            '!' => shifted(0x02),
            '@' => shifted(0x03),
            '#' => shifted(0x04),
            '$' => shifted(0x05),
            '%' => shifted(0x06),
            '^' => shifted(0x07),
            '&' => shifted(0x08),
            '*' => shifted(0x09),
            '(' => shifted(0x0a),
            ')' => shifted(0x0b),
            '_' => shifted(0x0c),
            '+' => shifted(0x0d),
            '{' => shifted(0x1a),
            '}' => shifted(0x1b),
            ':' => shifted(0x27),
            '"' => shifted(0x28),
            '~' => shifted(0x29),
            '|' => shifted(0x2b),
            '<' => shifted(0x33),
            '>' => shifted(0x34),
            '?' => shifted(0x35),
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::action;

    /// The events `operations` make from a fresh input state.
    fn events(operations: Vec<Operation>) -> Vec<FastPathInputEvent> {
        Database::new().apply(operations).into_vec()
    }

    fn code(code: u8) -> Scancode {
        Scancode::from_u8(false, code)
    }

    #[test]
    fn a_click_moves_then_presses_and_releases() {
        let mut driver = AgentDriver::new();
        let click = Action::Click {
            at: Some(Point { x: 10, y: 20 }),
            button: Button::Left,
            count: 2,
            modifiers: vec![keysym::CONTROL],
        };
        let expected = events(vec![
            Operation::MouseMove(MousePosition { x: 10, y: 20 }),
            Operation::KeyPressed(code(0x1d)),
            Operation::MouseButtonPressed(MouseButton::Left),
            Operation::MouseButtonReleased(MouseButton::Left),
            Operation::MouseButtonPressed(MouseButton::Left),
            Operation::MouseButtonReleased(MouseButton::Left),
            Operation::KeyReleased(code(0x1d)),
        ]);
        assert_eq!(driver.input(&click), expected);
        assert_eq!(driver.cursor(), Point { x: 10, y: 20 });
    }

    #[test]
    fn text_goes_as_unicode_and_new_lines_as_enter() {
        let mut driver = AgentDriver::new();
        let expected = events(vec![
            Operation::UnicodeKeyPressed('한'),
            Operation::UnicodeKeyReleased('한'),
            Operation::KeyPressed(code(0x1c)),
            Operation::KeyReleased(code(0x1c)),
            Operation::UnicodeKeyPressed('A'),
            Operation::UnicodeKeyReleased('A'),
        ]);
        assert_eq!(driver.input(&Action::Type { text: "한\nA".into() }), expected);
    }

    #[test]
    fn key_combinations_go_as_scancodes() {
        let mut driver = AgentDriver::new();
        let combo = action::parse_combo("ctrl+shift+esc").unwrap();
        let expected = events(vec![
            Operation::KeyPressed(code(0x1d)),
            Operation::KeyPressed(code(0x2a)),
            Operation::KeyPressed(code(0x01)),
            Operation::KeyReleased(code(0x01)),
            Operation::KeyReleased(code(0x2a)),
            Operation::KeyReleased(code(0x1d)),
        ]);
        assert_eq!(driver.input(&Action::Key { combo, repeat: 1 }), expected);
        // The Windows key is an extended scancode.
        let combo = action::parse_combo("super+r").unwrap();
        let win = Scancode::from_u8(true, 0x5b);
        let expected = events(vec![
            Operation::KeyPressed(win),
            Operation::KeyPressed(code(0x13)),
            Operation::KeyReleased(code(0x13)),
            Operation::KeyReleased(win),
        ]);
        assert_eq!(driver.input(&Action::Key { combo, repeat: 1 }), expected);
    }

    #[test]
    fn shifted_symbols_hold_shift() {
        let mut driver = AgentDriver::new();
        let combo = action::parse_combo("ctrl++").unwrap();
        let expected = events(vec![
            Operation::KeyPressed(code(0x1d)),
            Operation::KeyPressed(code(0x2a)),
            Operation::KeyPressed(code(0x0d)),
            Operation::KeyReleased(code(0x0d)),
            Operation::KeyReleased(code(0x2a)),
            Operation::KeyReleased(code(0x1d)),
        ]);
        assert_eq!(driver.input(&Action::Key { combo, repeat: 1 }), expected);
    }

    #[test]
    fn scrolling_turns_the_wheel_a_notch_at_a_time() {
        let mut driver = AgentDriver::new();
        let scroll = Action::Scroll {
            at: None,
            direction: Direction::Down,
            amount: 2,
            modifiers: vec![],
        };
        let notch = Operation::WheelRotations(WheelRotations {
            is_vertical: true,
            rotation_units: -WHEEL_NOTCH,
        });
        assert_eq!(driver.input(&scroll), events(vec![notch.clone(), notch]));
    }

    #[test]
    fn every_key_name_has_a_scancode() {
        for name in [
            "Return", "Tab", "Escape", "BackSpace", "Delete", "Home", "End", "Page_Up", "Page_Down", "Left", "Up",
            "Right", "Down", "F1", "F10", "F11", "F12", "space", "a", "z", "0", "minus",
        ] {
            let combo = action::parse_combo(name).unwrap();
            assert!(known_scancode(combo.key).is_some(), "{name}");
        }
    }
}
