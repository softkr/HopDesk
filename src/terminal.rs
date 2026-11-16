//! Terminal emulator widget for SSH sessions: `alacritty_terminal` keeps the screen state, egui
//! draws it and turns keyboard, IME, clipboard and mouse input into bytes for the remote shell.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{Config, Osc52, Term, TermMode, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb, StdSyncHandler};
use eframe::egui::{
    self, Align2, Color32, Event, EventFilter, FontFamily, FontId, ImeEvent, Key, Modifiers, Pos2, Rect, Sense, Stroke,
    Vec2,
};
use tokio::sync::mpsc;
use tracing::warn;

/// Font size of the terminal text in points.
const FONT_SIZE: f32 = 14.0;
/// Space between the window edge and the text.
const PADDING: f32 = 6.0;
/// The smallest screen the remote shell is told about.
const MIN_COLUMNS: u16 = 20;
const MIN_LINES: u16 = 5;

/// What the terminal sends towards the remote shell.
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    Bytes(Vec<u8>),
    /// The window now holds this many cells.
    Resize { columns: u16, lines: u16 },
}

/// The screen state shared by the network task (writes shell output) and the window (draws it).
pub type Screen = Arc<Mutex<Term<Replies>>>;

/// Sends the terminal's answers to the shell's queries (cursor position, colors) back to it.
pub struct Replies {
    input: mpsc::Sender<Input>,
}

impl EventListener for Replies {
    fn send_event(&self, event: TermEvent) {
        let reply = match event {
            TermEvent::PtyWrite(text) => text,
            TermEvent::ColorRequest(index, format) => format(default_rgb(index)),
            _ => return,
        };
        // Runs while the screen is updated on a runtime thread, so it must not block.
        if self.input.try_send(Input::Bytes(reply.into_bytes())).is_err() {
            warn!("Terminal reply dropped: input queue full or closed");
        }
    }
}

struct GridSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub fn new_screen(columns: u16, lines: u16, input: mpsc::Sender<Input>) -> Screen {
    let config = Config {
        // The remote side must not write the local clipboard behind the user's back (OSC 52).
        osc52: Osc52::Disabled,
        ..Config::default()
    };
    let size = GridSize {
        columns: usize::from(columns),
        lines: usize::from(lines),
    };
    Arc::new(Mutex::new(Term::new(config, &size, Replies { input })))
}

fn lock(screen: &Screen) -> MutexGuard<'_, Term<Replies>> {
    screen.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Parses shell output (text and escape sequences) into the screen.
pub struct Parser(Processor<StdSyncHandler>);

impl Parser {
    pub fn new() -> Self {
        Self(Processor::new())
    }

    pub fn feed(&mut self, screen: &Screen, bytes: &[u8]) {
        self.0.advance(&mut *lock(screen), bytes);
    }
}

/// The terminal widget: fills the space it is given and keeps the remote screen size in step.
pub struct TerminalView {
    screen: Screen,
    input: mpsc::Sender<Input>,
    font: FontId,
    /// Cells last reported to the shell.
    size: (u16, u16),
    /// Text the IME is composing (e.g. a Hangul syllable in progress), drawn at the cursor.
    preedit: String,
    /// Scrolled distance not yet amounting to a whole line.
    scroll_remainder: f32,
    focus_requested: bool,
}

impl TerminalView {
    pub fn new(screen: Screen, input: mpsc::Sender<Input>, columns: u16, lines: u16) -> Self {
        Self {
            screen,
            input,
            font: FontId::new(FONT_SIZE, FontFamily::Monospace),
            size: (columns, lines),
            preedit: String::new(),
            scroll_remainder: 0.0,
            focus_requested: false,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let cell = ui
            .ctx()
            .fonts_mut(|fonts| Vec2::new(fonts.glyph_width(&self.font, 'M'), fonts.row_height(&self.font)));
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        let id = response.id;
        if !self.focus_requested {
            self.focus_requested = true;
            ui.memory_mut(|memory| memory.request_focus(id));
        }
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let focused = ui.memory(|memory| memory.has_focus(id));
        if focused {
            // Tab, arrows and Escape belong to the shell, not to egui's focus navigation.
            let filter = EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
            };
            ui.memory_mut(|memory| memory.set_focus_lock_filter(id, filter));
        }

        let origin = rect.min + Vec2::splat(PADDING);
        let columns = cells_that_fit(rect.width() - 2.0 * PADDING, cell.x, MIN_COLUMNS);
        let lines = cells_that_fit(rect.height() - 2.0 * PADDING, cell.y, MIN_LINES);

        let mut outgoing = Vec::new();
        {
            let screen = Arc::clone(&self.screen);
            let mut term = lock(&screen);
            if (columns, lines) != self.size {
                self.size = (columns, lines);
                term.resize(GridSize {
                    columns: usize::from(columns),
                    lines: usize::from(lines),
                });
                outgoing.push(Input::Resize { columns, lines });
            }
            if focused {
                let bytes = self.keyboard(ui, &mut term);
                if !bytes.is_empty() {
                    term.scroll_display(Scroll::Bottom);
                    outgoing.push(Input::Bytes(bytes));
                }
            }
            if response.hovered()
                && let Some(bytes) = self.wheel(ui, &mut term, cell.y)
            {
                outgoing.push(Input::Bytes(bytes));
            }
            select(ui, &response, &mut term, origin, cell);
            let cursor = self.paint(&ui.painter_at(rect), &term, origin, cell, focused);
            if focused {
                ui.ctx().output_mut(|output| {
                    output.ime = Some(egui::output::IMEOutput {
                        purpose: egui::IMEPurpose::Terminal,
                        rect,
                        cursor_rect: cursor,
                        should_interrupt_composition: false,
                    });
                });
            }
        }
        for input in outgoing {
            // Blocks only while the network is behind, like typing into a slow terminal.
            // A closed queue means the session is over; the window closes on its own.
            let _ = self.input.blocking_send(input);
        }
    }

    /// Turns this frame's keyboard, IME and clipboard events into bytes for the shell.
    fn keyboard(&mut self, ui: &egui::Ui, term: &mut Term<Replies>) -> Vec<u8> {
        let mode = *term.mode();
        let mut out = Vec::new();
        for event in ui.input(|input| input.events.clone()) {
            match event {
                Event::Text(text) => out.extend_from_slice(text.as_bytes()),
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(bytes) = key_bytes(key, modifiers, mode.contains(TermMode::APP_CURSOR)) {
                        out.extend(bytes);
                    }
                }
                Event::Paste(text) => out.extend(paste_bytes(&text, mode.contains(TermMode::BRACKETED_PASTE))),
                // Like Windows Terminal: copy when text is selected, otherwise Ctrl+C interrupts.
                // On macOS this is ⌘C, and Ctrl+C arrives as a key.
                Event::Copy => match term.selection_to_string().filter(|text| !text.is_empty()) {
                    Some(text) => {
                        ui.ctx().copy_text(text);
                        term.selection = None;
                    }
                    None if !cfg!(target_os = "macos") => out.push(0x03),
                    None => {}
                },
                Event::Cut if !cfg!(target_os = "macos") => out.push(0x18),
                Event::Ime(ImeEvent::Preedit { text, .. }) => self.preedit = text,
                Event::Ime(ImeEvent::Commit(text)) => {
                    self.preedit.clear();
                    out.extend_from_slice(text.as_bytes());
                }
                _ => {}
            }
        }
        out
    }

    /// Scrolls the history, or on the alternate screen (pagers, editors) sends arrow keys.
    fn wheel(&mut self, ui: &egui::Ui, term: &mut Term<Replies>, line_height: f32) -> Option<Vec<u8>> {
        self.scroll_remainder += ui.input(|input| input.smooth_scroll_delta.y);
        #[expect(clippy::cast_possible_truncation, reason = "a frame scrolls a handful of lines")]
        let lines = (self.scroll_remainder / line_height).trunc() as i32;
        if lines == 0 {
            return None;
        }
        #[expect(clippy::cast_precision_loss, reason = "a handful of lines")]
        let scrolled = lines as f32 * line_height;
        self.scroll_remainder -= scrolled;
        let mode = *term.mode();
        if mode.contains(TermMode::ALT_SCREEN) {
            let key = if lines > 0 { Key::ArrowUp } else { Key::ArrowDown };
            let bytes = key_bytes(key, Modifiers::NONE, mode.contains(TermMode::APP_CURSOR))?;
            return Some(bytes.repeat(lines.unsigned_abs() as usize));
        }
        term.scroll_display(Scroll::Delta(lines));
        None
    }

    /// Draws the screen; returns the cursor cell for the IME candidate window.
    fn paint(&self, painter: &egui::Painter, term: &Term<Replies>, origin: Pos2, cell: Vec2, focused: bool) -> Rect {
        let content = term.renderable_content();
        let colors = content.colors;
        let background = resolve(colors, Color::Named(NamedColor::Background));
        painter.rect_filled(painter.clip_rect(), 0.0, background);

        let cell_rect = |line: usize, column: usize, width: usize| {
            #[expect(clippy::cast_precision_loss, reason = "screen coordinates")]
            Rect::from_min_size(
                origin + Vec2::new(column as f32 * cell.x, line as f32 * cell.y),
                Vec2::new(cell.x * width as f32, cell.y),
            )
        };
        let offset = content.display_offset;
        let cursor_visible = content.mode.contains(TermMode::SHOW_CURSOR) && content.cursor.shape != CursorShape::Hidden;
        let cursor_at = point_to_viewport(offset, content.cursor.point).filter(|_| cursor_visible);
        let selection = content.selection;

        let mut glyphs = Vec::new();
        for indexed in content.display_iter {
            let Some(at) = point_to_viewport(offset, indexed.point) else { continue };
            let flags = indexed.cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let width = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let (mut fg, mut bg) = (resolve(colors, indexed.cell.fg), resolve(colors, indexed.cell.bg));
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if flags.intersects(Flags::DIM) {
                fg = dim(fg);
            }
            let selected = selection.is_some_and(|range| range.contains(indexed.point));
            let block_cursor = focused && cursor_at == Some(at) && self.preedit.is_empty();
            if selected || block_cursor {
                std::mem::swap(&mut fg, &mut bg);
            }
            let rect = cell_rect(at.line, at.column.0, width);
            if bg != background {
                painter.rect_filled(rect, 0.0, bg);
            }
            if flags.contains(Flags::HIDDEN) {
                continue;
            }
            if flags.intersects(Flags::ALL_UNDERLINES) {
                painter.hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, fg));
            }
            if flags.contains(Flags::STRIKEOUT) {
                painter.hline(rect.x_range(), rect.center().y, Stroke::new(1.0, fg));
            }
            let mut text = String::from(indexed.cell.c);
            text.extend(indexed.cell.zerowidth().into_iter().flatten());
            glyphs.push(Glyph {
                line: at.line,
                column: at.column.0,
                width,
                text,
                color: fg,
            });
        }
        for run in text_runs(glyphs) {
            let slot = cell_rect(run.line, run.column, run.width);
            if run.width == 1 {
                painter.text(slot.min, Align2::LEFT_TOP, run.text, self.font.clone(), run.color);
            } else {
                // Hangul is narrower than its two cells in the font: center it in them.
                let galley = painter.layout_no_wrap(run.text, self.font.clone(), run.color);
                let inset = ((slot.width() - galley.size().x) / 2.0).max(0.0);
                painter.galley(slot.min + Vec2::new(inset, 0.0), galley, run.color);
            }
        }

        let cursor = cursor_at.map_or(Rect::from_min_size(origin, cell), |at| cell_rect(at.line, at.column.0, 1));
        if !self.preedit.is_empty() {
            let galley = painter.layout_no_wrap(self.preedit.clone(), self.font.clone(), FOREGROUND);
            let area = Rect::from_min_size(cursor.min, Vec2::new(galley.size().x.max(cell.x), cell.y));
            painter.rect_filled(area, 0.0, background);
            painter.galley(cursor.min, galley, FOREGROUND);
            painter.hline(area.x_range(), area.bottom() - 1.0, Stroke::new(1.0, FOREGROUND));
        } else if cursor_visible && !focused {
            painter.rect_stroke(cursor, 0.0, Stroke::new(1.0, FOREGROUND), egui::StrokeKind::Inside);
        }
        cursor
    }
}

/// Selects text by dragging; a click clears the selection.
fn select(ui: &egui::Ui, response: &egui::Response, term: &mut Term<Replies>, origin: Pos2, cell: Vec2) {
    let offset = term.grid().display_offset();
    let (columns, lines) = (term.columns(), term.screen_lines());
    let to_point = |pos: Pos2| {
        let x = ((pos.x - origin.x) / cell.x).max(0.0);
        let y = ((pos.y - origin.y) / cell.y).max(0.0);
        #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the screen")]
        let (column, line) = ((x as usize).min(columns - 1), (y as usize).min(lines - 1));
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (viewport_to_point(offset, Point::new(line, Column(column))), side)
    };
    if response.drag_started() {
        if let Some(pos) = ui.input(|input| input.pointer.press_origin()) {
            let (point, side) = to_point(pos);
            term.selection = Some(Selection::new(SelectionType::Simple, point, side));
        }
    } else if response.dragged()
        && let (Some(selection), Some(pos)) = (term.selection.as_mut(), response.interact_pointer_pos())
    {
        let (point, side) = to_point(pos);
        selection.update(point, side);
    } else if response.clicked() {
        term.selection = None;
    }
}

fn cells_that_fit(space: f32, cell: f32, min: u16) -> u16 {
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to u16")]
    let count = (space / cell).floor().clamp(0.0, f32::from(u16::MAX)) as u16;
    count.max(min)
}

/// Text to draw at a cell position.
#[derive(Debug, PartialEq, Eq)]
struct Glyph {
    line: usize,
    column: usize,
    /// Cells taken: 2 for wide characters such as Hangul (a joined run counts as 1).
    width: usize,
    text: String,
    color: Color32,
}

/// Joins neighbouring ASCII cells of one color into one run, so a line is drawn in a few pieces.
/// Other characters (Hangul, symbols from fallback fonts) are not exactly one cell wide in the
/// font, so each is drawn at its own cell to keep the columns aligned. Blank cells are skipped.
fn text_runs(glyphs: impl IntoIterator<Item = Glyph>) -> Vec<Glyph> {
    let is_ascii = |text: &str| text.len() == 1 && text.as_bytes()[0].is_ascii_graphic() || text == " ";
    let mut runs: Vec<Glyph> = Vec::new();
    let mut open = false;
    for glyph in glyphs {
        let extends = open
            && runs.last().is_some_and(|run| {
                run.line == glyph.line && run.column + run.text.len() == glyph.column && run.color == glyph.color
            });
        if is_ascii(&glyph.text) && extends {
            if let Some(run) = runs.last_mut() {
                run.text.push_str(&glyph.text);
            }
            continue;
        }
        if glyph.text.trim().is_empty() {
            open = false;
            continue;
        }
        open = is_ascii(&glyph.text);
        runs.push(glyph);
    }
    for run in &mut runs {
        run.text.truncate(run.text.trim_end().len());
    }
    runs
}

/// Bytes an xterm sends for a key that does not produce text; `None` for keys left to egui.
fn key_bytes(key: Key, modifiers: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    // macOS Command shortcuts stay with the app (⌘C, ⌘V, ⌘Q).
    if modifiers.mac_cmd {
        return None;
    }
    let param = 1 + u8::from(modifiers.shift) + 2 * u8::from(modifiers.alt) + 4 * u8::from(modifiers.ctrl);
    let cursor = |c: char| -> Vec<u8> {
        match (param, app_cursor) {
            (1, true) => format!("\x1bO{c}"),
            (1, false) => format!("\x1b[{c}"),
            _ => format!("\x1b[1;{param}{c}"),
        }
        .into_bytes()
    };
    let function = |c: char| -> Vec<u8> {
        if param == 1 { format!("\x1bO{c}") } else { format!("\x1b[1;{param}{c}") }.into_bytes()
    };
    let tilde = |n: u8| -> Vec<u8> {
        if param == 1 { format!("\x1b[{n}~") } else { format!("\x1b[{n};{param}~") }.into_bytes()
    };
    Some(match key {
        Key::Enter => b"\r".to_vec(),
        Key::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Backspace if modifiers.ctrl => vec![0x08],
        Key::Backspace => vec![0x7f],
        Key::Escape => vec![0x1b],
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => function('P'),
        Key::F2 => function('Q'),
        Key::F3 => function('R'),
        Key::F4 => function('S'),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ if modifiers.ctrl => vec![control_byte(key)?],
        _ => return None,
    })
}

/// Ctrl+key as a C0 control character (Ctrl+A = 0x01).
fn control_byte(key: Key) -> Option<u8> {
    match key {
        Key::Space => Some(0x00),
        Key::OpenBracket => Some(0x1b),
        Key::Backslash => Some(0x1c),
        Key::CloseBracket => Some(0x1d),
        _ => match key.name().as_bytes() {
            [letter] if letter.is_ascii_uppercase() => Some(letter - b'A' + 1),
            _ => None,
        },
    }
}

/// Pasted text: wrapped as a bracketed paste when the shell asked for it, so it is not run line
/// by line. An end marker inside the text would end the paste early, so it is removed.
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace("\x1b[201~", "")).into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

const FOREGROUND: Color32 = Color32::from_rgb(0xcc, 0xcc, 0xcc);
const BACKGROUND: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
/// The 16 ANSI colors (normal, then bright).
const ANSI: [Color32; 16] = [
    Color32::from_rgb(0x00, 0x00, 0x00),
    Color32::from_rgb(0xcd, 0x31, 0x31),
    Color32::from_rgb(0x0d, 0xbc, 0x79),
    Color32::from_rgb(0xe5, 0xe5, 0x10),
    Color32::from_rgb(0x24, 0x72, 0xc8),
    Color32::from_rgb(0xbc, 0x3f, 0xbc),
    Color32::from_rgb(0x11, 0xa8, 0xcd),
    Color32::from_rgb(0xe5, 0xe5, 0xe5),
    Color32::from_rgb(0x66, 0x66, 0x66),
    Color32::from_rgb(0xf1, 0x4c, 0x4c),
    Color32::from_rgb(0x23, 0xd1, 0x8b),
    Color32::from_rgb(0xf5, 0xf5, 0x43),
    Color32::from_rgb(0x3b, 0x8e, 0xea),
    Color32::from_rgb(0xd6, 0x70, 0xd6),
    Color32::from_rgb(0x29, 0xb8, 0xdb),
    Color32::from_rgb(0xe5, 0xe5, 0xe5),
];

/// A cell color: the one the shell set with OSC 4/10/11, else the built-in palette.
fn resolve(colors: &Colors, color: Color) -> Color32 {
    let index = match color {
        Color::Spec(rgb) => return to_color32(rgb),
        Color::Indexed(index) => usize::from(index),
        Color::Named(named) => named as usize,
    };
    colors[index].map_or_else(|| to_color32(default_rgb(index)), to_color32)
}

/// Built-in color for an index of [`Colors`]: 256 palette entries, then the named colors.
fn default_rgb(index: usize) -> Rgb {
    let color = match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let level = |v: usize| if v == 0 { 0 } else { 55 + 40 * v as u8 };
            let i = index - 16;
            Color32::from_rgb(level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        #[expect(clippy::cast_possible_truncation, reason = "232..256 gives 8..238")]
        232..=255 => Color32::from_gray(8 + 10 * (index - 232) as u8),
        257 => BACKGROUND,
        // Dim black .. dim white
        259..=266 => dim(ANSI[index - 259]),
        268 => dim(FOREGROUND),
        // Foreground (256), cursor (258), bright foreground (267)
        _ => FOREGROUND,
    };
    Rgb {
        r: color.r(),
        g: color.g(),
        b: color.b(),
    }
}

fn to_color32(rgb: Rgb) -> Color32 {
    Color32::from_rgb(rgb.r, rgb.g, rgb.b)
}

fn dim(color: Color32) -> Color32 {
    let scale = |v: u8| (u16::from(v) * 2 / 3) as u8;
    Color32::from_rgb(scale(color.r()), scale(color.g()), scale(color.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers {
        alt: false,
        ctrl: true,
        shift: false,
        mac_cmd: false,
        command: false,
    };

    #[test]
    fn keys_send_xterm_sequences() {
        assert_eq!(key_bytes(Key::Enter, Modifiers::NONE, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::Backspace, Modifiers::NONE, false), Some(vec![0x7f]));
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, Modifiers::NONE, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(Key::ArrowLeft, CTRL, true), Some(b"\x1b[1;5D".to_vec()));
        assert_eq!(key_bytes(Key::Delete, Modifiers::NONE, false), Some(b"\x1b[3~".to_vec()));
        assert_eq!(key_bytes(Key::F1, Modifiers::NONE, false), Some(b"\x1bOP".to_vec()));
        assert_eq!(key_bytes(Key::F12, Modifiers::SHIFT, false), Some(b"\x1b[24;2~".to_vec()));
        assert_eq!(key_bytes(Key::Tab, Modifiers::SHIFT, false), Some(b"\x1b[Z".to_vec()));
    }

    #[test]
    fn ctrl_letters_become_control_characters() {
        assert_eq!(key_bytes(Key::C, CTRL, false), Some(vec![0x03]));
        assert_eq!(key_bytes(Key::A, CTRL, false), Some(vec![0x01]));
        assert_eq!(key_bytes(Key::OpenBracket, CTRL, false), Some(vec![0x1b]));
        // Plain letters arrive as text; Command shortcuts stay local.
        assert_eq!(key_bytes(Key::C, Modifiers::NONE, false), None);
        assert_eq!(key_bytes(Key::C, Modifiers::MAC_CMD, false), None);
    }

    #[test]
    fn bracketed_paste_cannot_be_ended_early() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
        assert_eq!(
            paste_bytes("x\x1b[201~rm -rf ~\n", true),
            b"\x1b[200~xrm -rf ~\n\x1b[201~".to_vec()
        );
    }

    fn glyph(column: usize, text: &str) -> Glyph {
        Glyph {
            line: 0,
            column,
            width: if text.is_ascii() { 1 } else { 2 },
            text: text.into(),
            color: FOREGROUND,
        }
    }

    #[test]
    fn ascii_cells_are_joined_and_wide_characters_kept_apart() {
        let cells = [
            glyph(0, "l"),
            glyph(1, "s"),
            glyph(2, " "),
            glyph(3, "-"),
            glyph(4, "a"),
            glyph(5, " "),
            glyph(6, "한"),
            glyph(8, "글"),
            glyph(10, "x"),
            glyph(11, " "),
        ];
        let runs: Vec<(usize, String)> = text_runs(cells).into_iter().map(|g| (g.column, g.text)).collect();
        assert_eq!(
            runs,
            [(0, "ls -a".into()), (6, "한".into()), (8, "글".into()), (10, "x".into())]
        );
    }

    fn harness(view: TerminalView) -> egui_kittest::Harness<'static, TerminalView> {
        egui_kittest::Harness::builder()
            .with_size(Vec2::new(640.0, 360.0))
            // As in the session window; CI machines may lack a Hangul font.
            .allow_missing_glyphs()
            .build_ui_state(|ui, view: &mut TerminalView| view.ui(ui), view)
    }

    fn drain(receiver: &mut mpsc::Receiver<Input>) -> Vec<Input> {
        std::iter::from_fn(|| receiver.try_recv().ok()).collect()
    }

    #[test]
    fn the_window_size_and_typed_input_reach_the_shell() {
        let (sender, mut receiver) = mpsc::channel(64);
        let screen = new_screen(100, 30, sender.clone());
        let mut harness = harness(TerminalView::new(screen, sender, 100, 30));
        harness.step();
        let first = drain(&mut receiver);
        assert!(
            matches!(first.as_slice(), [Input::Resize { columns, lines }] if *columns < 100 && *lines < 30),
            "{first:?}"
        );

        harness.event(Event::Text("ls".into()));
        harness.key_press(Key::Enter);
        harness.event(Event::Ime(ImeEvent::Preedit {
            text: "하".into(),
            active_range_chars: None,
        }));
        harness.event(Event::Ime(ImeEvent::Commit("한".into())));
        harness.event(Event::Paste("a\nb".into()));
        harness.step();
        let bytes: Vec<u8> = drain(&mut receiver)
            .into_iter()
            .flat_map(|input| match input {
                Input::Bytes(bytes) => bytes,
                Input::Resize { .. } => Vec::new(),
            })
            .collect();
        assert_eq!(String::from_utf8(bytes).expect("utf-8"), "ls\r한a\rb");
    }

    #[test]
    #[ignore = "design review; needs a GPU"]
    fn screenshot() {
        let (sender, _receiver) = mpsc::channel(64);
        let screen = new_screen(80, 20, sender.clone());
        let output = "\x1b[1;32mme@server\x1b[0m:\x1b[34m~\x1b[0m$ ls -l\r\n\
            drwxr-xr-x  2 me me 4096 Nov 16 \x1b[1;34m문서\x1b[0m\r\n\
            -rw-r--r--  1 me me  220 Nov 16 notes.txt\r\n\
            \x1b[7m inverse \x1b[0m \x1b[4munderline\x1b[0m \x1b[38;5;208m256-color\x1b[0m \
            \x1b[48;2;40;80;160m truecolor \x1b[0m\r\n\
            \x1b[1;32mme@server\x1b[0m:\x1b[34m~\x1b[0m$ echo 안녕하세요\r\n안녕하세요\r\n\
            \x1b[1;32mme@server\x1b[0m:\x1b[34m~\x1b[0m$ ";
        Parser::new().feed(&screen, output.as_bytes());
        let mut harness = egui_kittest::Harness::builder()
            .with_size(Vec2::new(640.0, 300.0))
            .with_pixels_per_point(2.0)
            .wgpu()
            .with_context_setup(crate::gui::theme::install_fonts)
            .build_ui_state(|ui, view: &mut TerminalView| view.ui(ui), TerminalView::new(screen, sender, 80, 20));
        for _ in 0..3 {
            harness.step();
        }
        let dir = std::env::var_os("HOPDESK_SHOTS")
            .map_or_else(|| std::path::PathBuf::from("target/screenshots"), Into::into);
        std::fs::create_dir_all(&dir).expect("create screenshot dir");
        let path = dir.join("terminal.png");
        harness.render().expect("render").save(&path).expect("save png");
        eprintln!("wrote {}", path.display());
    }

    #[test]
    fn color_cube_and_gray_ramp_match_xterm() {
        let rgb = |i| {
            let c = default_rgb(i);
            (c.r, c.g, c.b)
        };
        assert_eq!(rgb(16), (0, 0, 0));
        assert_eq!(rgb(196), (255, 0, 0));
        assert_eq!(rgb(231), (255, 255, 255));
        assert_eq!(rgb(232), (8, 8, 8));
        assert_eq!(rgb(255), (238, 238, 238));
        assert_eq!(rgb(NamedColor::Background as usize), (0x1e, 0x1e, 0x1e));
    }
}
