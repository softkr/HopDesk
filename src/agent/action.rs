//! What the AI asks of the remote screen, independent of the AI provider and of the remote
//! control protocol (docs/ai-computer-use-plan.md §5.3).
//!
//! Positions are remote screen pixels: providers convert the model's coordinates with
//! [`Scale`]. Keys are X11 keysyms, which VNC sends as they are.

use core::time::Duration;

use anyhow::{Context as _, bail};

/// Longest wait the model may ask for (Claude's limit for `wait`).
pub const MAX_WAIT: Duration = Duration::from_secs(300);
/// Most presses of one key combination per action (Claude's limit for `key`).
pub const MAX_REPEAT: u16 = 100;
/// Normalized coordinates run from 0 to this value across the screenshot.
pub const NORMALIZED_RANGE: f64 = 1000.0;

/// X11 keysyms of the keys the AI names.
pub mod keysym {
    pub const SHIFT: u32 = 0xffe1;
    pub const CONTROL: u32 = 0xffe3;
    pub const ALT: u32 = 0xffe9;
    /// Windows / Command key.
    pub const SUPER: u32 = 0xffeb;
    pub const RETURN: u32 = 0xff0d;
    pub const TAB: u32 = 0xff09;
    /// Keysyms of characters outside Latin-1 are this plus the code point.
    pub const UNICODE: u32 = 0x0100_0000;
}

/// A position on the remote screen, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

/// An area of the remote screen; `right` and `bottom` are exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Region {
    pub fn width(&self) -> u32 {
        self.right - self.left
    }

    pub fn height(&self) -> u32 {
        self.bottom - self.top
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Modifier keysyms held around `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyCombo {
    pub modifiers: Vec<u32>,
    pub key: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// The whole screen as the model sees it.
    Screenshot,
    /// An area at up to full resolution, scaled like a screenshot.
    Zoom { region: Region },
    /// Where the pointer is, in screenshot pixels.
    CursorPosition,
    /// `at` moves the pointer first; `modifiers` are held for the click only.
    Click {
        at: Option<Point>,
        button: Button,
        count: u8,
        modifiers: Vec<u32>,
    },
    /// Left button pressed at `from`, released at `to`.
    Drag { from: Point, to: Point, modifiers: Vec<u32> },
    MouseMove { to: Point },
    /// Left button pressed at the pointer, until [`Action::MouseUp`].
    MouseDown,
    MouseUp,
    /// `amount` wheel notches.
    Scroll {
        at: Option<Point>,
        direction: Direction,
        amount: u16,
        modifiers: Vec<u32>,
    },
    /// Literal text at the keyboard focus.
    Type { text: String },
    Key { combo: KeyCombo, repeat: u16 },
    Wait { duration: Duration },
}

/// How a model gives positions on the screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Coordinates {
    /// Screenshot pixels (Claude, GPT).
    #[default]
    Pixels,
    /// 0 to [`NORMALIZED_RANGE`] across each side, whatever the screenshot size.
    #[cfg_attr(not(test), expect(dead_code, reason = "for models that give normalized positions"))]
    Normalized,
}

/// Maps the model's positions on the screenshot to remote screen pixels and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale {
    screen: (u32, u32),
    shot: (u32, u32),
    coordinates: Coordinates,
}

impl Scale {
    pub fn new(screen: (u32, u32), shot: (u32, u32), coordinates: Coordinates) -> Self {
        Self {
            screen,
            shot,
            coordinates,
        }
    }

    /// The remote pixel under the model's position; positions off the screenshot are refused,
    /// so the model learns of its mistake instead of clicking somewhere else.
    pub fn point(&self, x: f64, y: f64) -> anyhow::Result<Point> {
        let (fx, fy) = self.fractions(x, y)?;
        let pixel = |fraction: f64, size: u32| scale_down(fraction, size).min(size.saturating_sub(1));
        Ok(Point {
            x: pixel(fx, self.screen.0),
            y: pixel(fy, self.screen.1),
        })
    }

    /// The remote area between two of the model's corners.
    pub fn region(&self, [x0, y0, x1, y1]: [f64; 4]) -> anyhow::Result<Region> {
        let (left, top) = self.fractions(x0, y0)?;
        let (right, bottom) = self.fractions(x1, y1)?;
        let region = Region {
            left: scale_down(left, self.screen.0),
            top: scale_down(top, self.screen.1),
            right: scale_up(right, self.screen.0),
            bottom: scale_up(bottom, self.screen.1),
        };
        if region.left >= region.right || region.top >= region.bottom {
            bail!("region [{x0}, {y0}, {x1}, {y1}] is empty: give the top left corner first");
        }
        Ok(region)
    }

    /// The model's position of a remote pixel.
    pub fn to_model(self, point: Point) -> (u32, u32) {
        let (width, height) = match self.coordinates {
            Coordinates::Pixels => self.shot,
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a small constant")]
            Coordinates::Normalized => (NORMALIZED_RANGE as u32, NORMALIZED_RANGE as u32),
        };
        let convert = |value: u32, from: u32, to: u32| {
            if from == 0 {
                return 0;
            }
            u32::try_from(u64::from(value) * u64::from(to) / u64::from(from)).unwrap_or(u32::MAX)
        };
        (convert(point.x, self.screen.0, width), convert(point.y, self.screen.1, height))
    }

    /// The position as fractions of the screenshot's width and height.
    fn fractions(&self, x: f64, y: f64) -> anyhow::Result<(f64, f64)> {
        let (width, height) = match self.coordinates {
            Coordinates::Pixels => (f64::from(self.shot.0), f64::from(self.shot.1)),
            Coordinates::Normalized => (NORMALIZED_RANGE, NORMALIZED_RANGE),
        };
        let inside = |value: f64, size: f64| value.is_finite() && (0.0..=size).contains(&value);
        if width <= 0.0 || height <= 0.0 || !inside(x, width) || !inside(y, height) {
            bail!("position ({x}, {y}) is outside the screenshot ({width}x{height})");
        }
        Ok((x / width, y / height))
    }
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a fraction of a u32 size")]
fn scale_down(fraction: f64, size: u32) -> u32 {
    (fraction * f64::from(size)).floor() as u32
}

#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a fraction of a u32 size")]
fn scale_up(fraction: f64, size: u32) -> u32 {
    ((fraction * f64::from(size)).ceil() as u32).min(size)
}

/// A wait or key hold the model asked for, in seconds.
pub fn duration(seconds: f64) -> anyhow::Result<Duration> {
    let duration = Duration::try_from_secs_f64(seconds).ok().filter(|d| *d <= MAX_WAIT);
    duration.with_context(|| format!("duration {seconds} s is not between 0 and {} s", MAX_WAIT.as_secs()))
}

/// A key combination such as `ctrl+shift+t`, `Return` or `cmd+space` (xdotool names and the
/// usual spellings, any case).
pub fn parse_combo(text: &str) -> anyhow::Result<KeyCombo> {
    let mut names = split_combo(text)?;
    let key = names.pop().context("no key")?;
    let modifiers = names.into_iter().map(modifier).collect::<anyhow::Result<_>>()?;
    let key = named_key(key).with_context(|| format!("unknown key {key:?} in {text:?}"))?;
    Ok(KeyCombo { modifiers, key })
}

/// Modifiers held during a click, drag or scroll, such as `shift` or `ctrl+alt`; empty for none.
pub fn parse_modifiers(text: &str) -> anyhow::Result<Vec<u32>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    split_combo(text)?.into_iter().map(modifier).collect()
}

/// `a+b+c` into its names; a trailing `+` (as in `ctrl++`) is the plus key itself.
fn split_combo(text: &str) -> anyhow::Result<Vec<&str>> {
    let text = text.trim();
    let mut names: Vec<&str> = match text.strip_suffix("++") {
        Some(rest) => rest.split('+').chain(["+"]).collect(),
        None if text == "+" => vec!["+"],
        None => text.split('+').collect(),
    };
    for name in &mut names {
        *name = name.trim();
    }
    if names.iter().any(|name| name.is_empty()) {
        bail!("malformed key combination {text:?}");
    }
    Ok(names)
}

fn modifier(name: &str) -> anyhow::Result<u32> {
    Ok(match normalize(name).as_str() {
        "shift" => keysym::SHIFT,
        "ctrl" | "control" => keysym::CONTROL,
        "alt" | "option" | "opt" => keysym::ALT,
        "super" | "cmd" | "command" | "meta" | "win" | "windows" => keysym::SUPER,
        _ => bail!("{name:?} is not a modifier key"),
    })
}

/// Lower case without `_`, `-` and spaces, so `Page_Down`, `page-down` and `PAGEDOWN` match.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '_' | '-' | ' '))
        .flat_map(char::to_lowercase)
        .collect()
}

fn named_key(name: &str) -> Option<u32> {
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return char_keysym(c.to_ascii_lowercase());
    }
    if let Ok(modifier) = modifier(name) {
        return Some(modifier);
    }
    let name = normalize(name);
    if let Some(number) = name.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        // F1 is 0xffbe, up to F35.
        return (1..=35).contains(&number).then(|| 0xffbe + number - 1);
    }
    Some(match name.as_str() {
        "return" | "enter" => keysym::RETURN,
        "kpenter" => 0xff8d,
        "tab" => keysym::TAB,
        "escape" | "esc" => 0xff1b,
        "backspace" => 0xff08,
        "delete" | "del" => 0xffff,
        "insert" | "ins" => 0xff63,
        "home" => 0xff50,
        "end" => 0xff57,
        "pageup" | "prior" => 0xff55,
        "pagedown" | "next" => 0xff56,
        "left" | "arrowleft" => 0xff51,
        "up" | "arrowup" => 0xff52,
        "right" | "arrowright" => 0xff53,
        "down" | "arrowdown" => 0xff54,
        "space" => 0x20,
        "capslock" => 0xffe5,
        "print" | "printscreen" => 0xff61,
        "menu" => 0xff67,
        "minus" => u32::from('-'),
        "plus" => u32::from('+'),
        "equal" | "equals" => u32::from('='),
        "comma" => u32::from(','),
        "period" => u32::from('.'),
        "slash" => u32::from('/'),
        "backslash" => u32::from('\\'),
        "semicolon" => u32::from(';'),
        "apostrophe" | "quote" => u32::from('\''),
        "grave" | "backtick" => u32::from('`'),
        "bracketleft" => u32::from('['),
        "bracketright" => u32::from(']'),
        _ => return None,
    })
}

/// The keysym that types `c`: Latin-1 characters are their own keysym.
pub fn char_keysym(c: char) -> Option<u32> {
    match c {
        '\n' => Some(keysym::RETURN),
        '\t' => Some(keysym::TAB),
        c if c.is_control() => None,
        c if u32::from(c) <= 0xff => Some(u32::from(c)),
        c => Some(keysym::UNICODE + u32::from(c)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshot_pixels_map_onto_the_remote_screen() {
        let scale = Scale::new((2560, 1600), (1280, 800), Coordinates::Pixels);
        assert_eq!(scale.point(0.0, 0.0).unwrap(), Point { x: 0, y: 0 });
        assert_eq!(scale.point(640.0, 400.0).unwrap(), Point { x: 1280, y: 800 });
        // The far edge is still on the screen.
        assert_eq!(scale.point(1280.0, 800.0).unwrap(), Point { x: 2559, y: 1599 });
        assert_eq!(scale.to_model(Point { x: 1280, y: 800 }), (640, 400));
    }

    #[test]
    fn normalized_positions_ignore_the_screenshot_size() {
        let scale = Scale::new((1920, 1080), (1280, 720), Coordinates::Normalized);
        assert_eq!(scale.point(500.0, 500.0).unwrap(), Point { x: 960, y: 540 });
        assert_eq!(scale.point(999.0, 0.0).unwrap(), Point { x: 1918, y: 0 });
        assert_eq!(scale.to_model(Point { x: 960, y: 540 }), (500, 500));
    }

    #[test]
    fn positions_off_the_screenshot_are_refused() {
        let scale = Scale::new((1920, 1080), (1280, 720), Coordinates::Pixels);
        for (x, y) in [(-1.0, 0.0), (1281.0, 0.0), (0.0, 721.0), (f64::NAN, 0.0)] {
            assert!(scale.point(x, y).is_err(), "({x}, {y})");
        }
    }

    #[test]
    fn regions_cover_every_pixel_between_the_corners() {
        let scale = Scale::new((1920, 1080), (1280, 720), Coordinates::Pixels);
        let region = scale.region([100.0, 100.0, 201.0, 150.0]).unwrap();
        assert_eq!(
            region,
            Region {
                left: 150,
                top: 150,
                right: 302,
                bottom: 225
            }
        );
        assert_eq!((region.width(), region.height()), (152, 75));
        assert!(scale.region([200.0, 100.0, 100.0, 150.0]).is_err());
        assert!(scale.region([100.0, 100.0, 100.0, 150.0]).is_err());
    }

    #[test]
    fn key_combinations_accept_the_usual_spellings() {
        let combo = |modifiers: &[u32], key: u32| KeyCombo {
            modifiers: modifiers.to_vec(),
            key,
        };
        assert_eq!(parse_combo("ctrl+s").unwrap(), combo(&[keysym::CONTROL], u32::from('s')));
        assert_eq!(
            parse_combo("CTRL + Shift + T").unwrap(),
            combo(&[keysym::CONTROL, keysym::SHIFT], u32::from('t'))
        );
        assert_eq!(parse_combo("Return").unwrap(), combo(&[], keysym::RETURN));
        assert_eq!(parse_combo("Page_Down").unwrap(), combo(&[], 0xff56));
        assert_eq!(parse_combo("cmd+space").unwrap(), combo(&[keysym::SUPER], 0x20));
        assert_eq!(parse_combo("alt+F4").unwrap(), combo(&[keysym::ALT], 0xffc1));
        assert_eq!(parse_combo("ctrl++").unwrap(), combo(&[keysym::CONTROL], u32::from('+')));
        assert_eq!(parse_combo("super").unwrap(), combo(&[], keysym::SUPER));
    }

    #[test]
    fn malformed_key_combinations_are_refused() {
        for text in ["", "ctrl+", "s+ctrl", "ctrl+nosuchkey", "f36"] {
            assert!(parse_combo(text).is_err(), "{text:?}");
        }
        assert_eq!(parse_modifiers("").unwrap(), Vec::<u32>::new());
        assert_eq!(parse_modifiers("shift+alt").unwrap(), [keysym::SHIFT, keysym::ALT]);
        assert!(parse_modifiers("shift+a").is_err());
    }

    #[test]
    fn characters_type_as_their_keysyms() {
        assert_eq!(char_keysym('a'), Some(0x61));
        assert_eq!(char_keysym('é'), Some(0xe9));
        assert_eq!(char_keysym('한'), Some(0x0100_d55c));
        assert_eq!(char_keysym('\n'), Some(keysym::RETURN));
        assert_eq!(char_keysym('\u{7}'), None);
    }

    #[test]
    fn waits_stay_within_the_limit() {
        assert_eq!(duration(1.5).unwrap(), Duration::from_millis(1500));
        assert!(duration(-1.0).is_err());
        assert!(duration(301.0).is_err());
        assert!(duration(f64::NAN).is_err());
    }
}
