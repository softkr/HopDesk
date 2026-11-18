//! VNC: the target's screen in a window, through the jump host or directly.
//!
//! The server sends screen updates in the client's pixel format (RGBA); the window scales the
//! screen to fit and sends keys as X11 keysyms of their position on a US keyboard, so the
//! remote input method (e.g. Hangul) does the typing, as with RDP scancodes.

use core::time::Duration;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context as _, bail};
use eframe::egui::{self, Color32, ColorImage, Event, Key, Modifiers, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;
use tracing::{info, warn};
use vnc::{ClientKeyEvent, ClientMouseEvent, PixelFormat, VncClient, VncConnector, VncEncoding, VncError, VncEvent, X11Event};

use crate::profile::Profile;
use crate::status::{self, Phase};
use crate::tunnel::Tunnel;

/// How often the window asks the server for screen changes.
const REFRESH_INTERVAL: Duration = Duration::from_millis(33);
/// Keys and pointer moves waiting for the network.
const INPUT_QUEUE: usize = 256;
/// Wheel distance in points that counts as one notch.
const WHEEL_NOTCH: f32 = 40.0;

type PasswordFuture = std::future::Ready<Result<String, VncError>>;

/// A non-empty `user` logs on with that macOS account (Apple Remote Desktop authentication),
/// which only happens through the jump host; otherwise the VNC password is used.
pub fn run(profile: &Profile, password: Option<String>, tunnel: Option<Arc<Tunnel>>) -> anyhow::Result<()> {
    let target = &profile.target;
    let password = password.filter(|p| !p.is_empty());
    // ARD does not authenticate the server: whoever intercepts the connection learns the macOS
    // account password. Only the SSH channel through the jump host protects it.
    let ard_user = (!target.user.is_empty()).then(|| target.user.clone());
    if ard_user.is_some() && tunnel.is_none() {
        bail!(
            "macOS account login (ARD) is only used through the SSH jump host; \
             remove the user name to log on with the VNC password"
        );
    }
    if ard_user.is_some() && password.is_none() {
        bail!("no stored macOS account password (use `set-password --target`)");
    }
    if tunnel.is_none() {
        warn!(host = %target.host, "VNC without the jump host is not encrypted");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;
    if let Some(tunnel) = &tunnel {
        status::emit(Phase::Ssh, "");
        rt.block_on(tunnel.ensure_connected())?;
        rt.spawn(Arc::clone(tunnel).report_latency());
    }
    status::emit(Phase::Remote, "");
    let client = rt.block_on(async {
        info!(host = %target.host, port = target.port, macos_account = ard_user.is_some(), "Connecting to VNC target");
        match &tunnel {
            Some(tunnel) => handshake(tunnel.open(&target.host, target.port).await?, password, ard_user).await,
            None => {
                let stream = tokio::net::TcpStream::connect((target.host.as_str(), target.port))
                    .await
                    .context("TCP connect to VNC target")?;
                stream.set_nodelay(true)?;
                handshake(crate::stats::Counted::new(stream), password, ard_user).await
            }
        }
    })?;
    info!(name = client.server_name(), "VNC connected");
    status::emit(Phase::Connected, "");

    let screen = Arc::new(Mutex::new(Screen::default()));
    let (input_sender, input_receiver) = mpsc::channel(INPUT_QUEUE);
    let name = profile.name.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("HopDesk — {}", profile.name))
            .with_app_id("hopdesk")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([320.0, 200.0])
            .with_icon(eframe::icon_data::from_png_bytes(crate::gui::WINDOW_ICON).expect("bundled icon is a valid PNG")),
        ..Default::default()
    };
    eframe::run_native(
        "HopDesk",
        options,
        Box::new(move |cc| {
            // ⌘-/⌘+ belong to the remote desktop.
            cc.egui_ctx.options_mut(|options| options.zoom_with_keyboard = false);
            #[cfg(target_os = "macos")]
            crate::dock::mark_session(&name);
            #[cfg(not(target_os = "macos"))]
            let _ = name;
            rt.spawn(pump(client, Arc::clone(&screen), input_receiver, cc.egui_ctx.clone()));
            Ok(Box::new(Window {
                screen,
                texture: None,
                input: input_sender,
                keyboard: Keyboard::default(),
                pointer: None,
                wheel: Vec2::ZERO,
                sized: false,
                _runtime: rt,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("VNC window failed: {e}"))?;
    #[cfg(target_os = "macos")]
    crate::dock::leave_dock();
    Ok(())
}

async fn handshake<S>(stream: S, password: Option<String>, ard_user: Option<String>) -> anyhow::Result<VncClient>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + Sync + 'static,
{
    // Tight is left out: its JPEG rectangles would need an image decoder.
    let mut connector = VncConnector::<S, PasswordFuture>::new(stream)
        .allow_shared(true)
        .set_pixel_format(PixelFormat::rgba())
        .add_encoding(VncEncoding::Zrle)
        .add_encoding(VncEncoding::Trle)
        .add_encoding(VncEncoding::CopyRect)
        .add_encoding(VncEncoding::Raw)
        .add_encoding(VncEncoding::DesktopSizePseudo);
    if let Some(password) = &password {
        connector = connector.set_auth_method(std::future::ready(Ok(password.clone())));
    }
    if let (Some(user), Some(password)) = (ard_user, password) {
        connector = connector.set_ard_credentials(user, password);
    }
    match connector.build()?.try_start().await {
        Ok(state) => Ok(state.finish()?),
        Err(error @ VncError::NoPassword) => {
            Err(anyhow::Error::new(error).context("no stored VNC password (use `set-password --target`)"))
        }
        Err(error @ (VncError::WrongPassword | VncError::ArdAuthFailed(_))) => {
            Err(anyhow::Error::new(error).context("VNC authentication failed"))
        }
        Err(error) => Err(anyhow::Error::new(error).context("VNC handshake failed")),
    }
}

/// Applies screen updates and sends input until either side closes.
async fn pump(client: VncClient, screen: Arc<Mutex<Screen>>, mut input: mpsc::Receiver<X11Event>, ctx: egui::Context) {
    let mut refresh = tokio::time::interval(REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // `recv_event` holds the client lock while it waits, so input is sent from this task only,
    // after the waiting receive was dropped.
    let reason = loop {
        tokio::select! {
            event = client.recv_event() => match event {
                Ok(VncEvent::Error(message)) => break message,
                Ok(event) => {
                    if lock(&screen).apply(event) {
                        ctx.request_repaint();
                    }
                }
                Err(error) => break error.to_string(),
            },
            _ = refresh.tick() => {
                if let Err(error) = client.input(X11Event::Refresh).await {
                    break error.to_string();
                }
            }
            Some(event) = input.recv() => {
                if let Err(error) = client.input(event).await {
                    break error.to_string();
                }
            }
        }
    };
    info!(%reason, "VNC session ended");
    let _ = client.close().await;
    status::emit(Phase::Ended, &reason);
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

fn lock(screen: &Mutex<Screen>) -> MutexGuard<'_, Screen> {
    screen.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The remote screen as RGBA pixels, plus what the window still has to pick up.
#[derive(Default)]
struct Screen {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
    /// Pixels changed since the window last uploaded them.
    dirty: bool,
    /// Text the remote side copied, for the local clipboard.
    clipboard: Option<String>,
}

impl Screen {
    /// Returns whether the window should redraw.
    fn apply(&mut self, event: VncEvent) -> bool {
        match event {
            VncEvent::SetResolution(size) => self.resize(usize::from(size.width), usize::from(size.height)),
            VncEvent::RawImage(rect, data) => self.put(rect, &data),
            VncEvent::Copy(dst, src) => self.copy(dst, src),
            VncEvent::Text(text) => self.clipboard = Some(text),
            _ => return false,
        }
        true
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels = vec![0; width * height * 4];
        self.dirty = true;
    }

    /// The rectangle as pixel offsets, or `None` when it does not fit the screen (data from the
    /// server is not trusted to stay in bounds).
    fn bounds(&self, rect: vnc::Rect) -> Option<(usize, usize, usize, usize)> {
        let (x, y, w, h) = (usize::from(rect.x), usize::from(rect.y), usize::from(rect.width), usize::from(rect.height));
        (x + w <= self.width && y + h <= self.height).then_some((x, y, w, h))
    }

    fn put(&mut self, rect: vnc::Rect, data: &[u8]) {
        let Some((x, y, w, h)) = self.bounds(rect) else {
            warn!(?rect, "Ignoring a screen update outside the screen");
            return;
        };
        if data.len() < w * h * 4 {
            warn!(?rect, len = data.len(), "Ignoring a short screen update");
            return;
        }
        for row in 0..h {
            let source = &data[row * w * 4..(row + 1) * w * 4];
            let start = ((y + row) * self.width + x) * 4;
            let line = &mut self.pixels[start..start + w * 4];
            line.copy_from_slice(source);
            // The fourth byte is padding in the pixel format: make every pixel opaque.
            line.iter_mut().skip(3).step_by(4).for_each(|alpha| *alpha = 255);
        }
        self.dirty = true;
    }

    fn copy(&mut self, dst: vnc::Rect, src: vnc::Rect) {
        let (Some((dx, dy, w, h)), Some((sx, sy, sw, sh))) = (self.bounds(dst), self.bounds(src)) else {
            warn!(?dst, ?src, "Ignoring a copy outside the screen");
            return;
        };
        if (w, h) != (sw, sh) {
            return;
        }
        // The areas may overlap: read the source before writing.
        let mut area = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let start = ((sy + row) * self.width + sx) * 4;
            area.extend_from_slice(&self.pixels[start..start + w * 4]);
        }
        for row in 0..h {
            let start = ((dy + row) * self.width + dx) * 4;
            self.pixels[start..start + w * 4].copy_from_slice(&area[row * w * 4..(row + 1) * w * 4]);
        }
        self.dirty = true;
    }
}

struct Window {
    screen: Arc<Mutex<Screen>>,
    texture: Option<TextureHandle>,
    input: mpsc::Sender<X11Event>,
    keyboard: Keyboard,
    /// Last pointer state sent: position on the remote screen and button mask.
    pointer: Option<(u16, u16, u8)>,
    /// Wheel distance not yet amounting to a notch.
    wheel: Vec2,
    /// The window was fitted to the remote screen size once.
    sized: bool,
    /// Runs the network task; dropped (and the connection with it) when the window closes.
    _runtime: tokio::runtime::Runtime,
}

impl Window {
    fn send(&self, event: X11Event) {
        // Blocks only while the network is behind. A closed queue means the session is over;
        // the window closes on its own.
        let _ = self.input.blocking_send(event);
    }

    /// Uploads changed pixels; returns the remote screen size and copied text.
    fn sync_screen(&mut self, ctx: &egui::Context) -> ((usize, usize), Option<String>) {
        let mut screen = lock(&self.screen);
        if screen.dirty && screen.width > 0 {
            screen.dirty = false;
            let image = ColorImage::from_rgba_unmultiplied([screen.width, screen.height], &screen.pixels);
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("vnc-screen", image, TextureOptions::LINEAR)),
            }
        }
        ((screen.width, screen.height), screen.clipboard.take())
    }

    /// Fits the window to the remote screen once, within the monitor.
    fn fit_window(&mut self, ctx: &egui::Context, (width, height): (usize, usize)) {
        if self.sized || width == 0 {
            return;
        }
        self.sized = true;
        #[expect(clippy::cast_precision_loss, reason = "screen sizes")]
        let size = Vec2::new(width as f32, height as f32);
        let room = ctx.input(|i| i.viewport().monitor_size).unwrap_or(size) * 0.9;
        let scale = (room.x / size.x).min(room.y / size.y).min(1.0);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size * scale));
    }

    fn pointer(&mut self, input: &egui::InputState, image: Rect, (width, height): (usize, usize)) {
        let Some(pos) = input.pointer.latest_pos().filter(|pos| image.contains(*pos)) else {
            return;
        };
        let (x, y) = to_remote(pos, image, (width, height));
        let mask = u8::from(input.pointer.primary_down())
            | u8::from(input.pointer.middle_down()) << 1
            | u8::from(input.pointer.secondary_down()) << 2;
        if self.pointer != Some((x, y, mask)) {
            self.pointer = Some((x, y, mask));
            self.send(X11Event::PointerEvent(ClientMouseEvent {
                position_x: x,
                position_y: y,
                bottons: mask,
            }));
        }
        for event in &input.events {
            if let Event::MouseWheel { unit, delta, .. } = event {
                self.wheel += match unit {
                    egui::MouseWheelUnit::Point => *delta / WHEEL_NOTCH,
                    egui::MouseWheelUnit::Line => *delta,
                    egui::MouseWheelUnit::Page => *delta * 10.0,
                };
            }
        }
        // Buttons 4/5 scroll up/down, 6/7 left/right: one press and release per notch.
        for (amount, positive, negative) in [(&mut self.wheel.y, 1 << 3, 1 << 4), (&mut self.wheel.x, 1 << 5, 1 << 6)] {
            while amount.abs() >= 1.0 {
                let button = if *amount > 0.0 { positive } else { negative };
                *amount -= amount.signum();
                for buttons in [mask | button, mask] {
                    let _ = self.input.blocking_send(X11Event::PointerEvent(ClientMouseEvent {
                        position_x: x,
                        position_y: y,
                        bottons: buttons,
                    }));
                }
            }
        }
    }
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (size, copied) = self.sync_screen(&ctx);
        if let Some(text) = copied {
            ctx.copy_text(text);
        }
        self.fit_window(&ctx, size);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                ui.allocate_rect(area, egui::Sense::hover());
                let Some(texture) = &self.texture else { return };
                let image = fit(area, size);
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                ui.painter().image(texture.id(), image, uv, Color32::WHITE);

                let input = ctx.input(Clone::clone);
                for out in self.keyboard.handle(&input.events, input.modifiers, input.focused) {
                    match out {
                        KeyOut::Key(keysym, down) => self.send(X11Event::KeyEvent(ClientKeyEvent { keycode: keysym, down })),
                        KeyOut::Clipboard(text) => self.send(X11Event::CopyText(text)),
                    }
                }
                self.pointer(&input, image, size);
            });
    }
}

/// The largest rectangle with the screen's aspect ratio, centered in `area`.
fn fit(area: Rect, (width, height): (usize, usize)) -> Rect {
    if width == 0 || height == 0 {
        return area;
    }
    #[expect(clippy::cast_precision_loss, reason = "screen sizes")]
    let size = Vec2::new(width as f32, height as f32);
    let scale = (area.width() / size.x).min(area.height() / size.y);
    Rect::from_center_size(area.center(), size * scale)
}

fn to_remote(pos: Pos2, image: Rect, (width, height): (usize, usize)) -> (u16, u16) {
    let fraction = (pos - image.min) / image.size();
    #[expect(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "clamped to the screen")]
    let coordinate = |f: f32, max: usize| ((f * max as f32) as usize).min(max.saturating_sub(1)) as u16;
    (coordinate(fraction.x, width), coordinate(fraction.y, height))
}

#[derive(Debug, PartialEq, Eq)]
enum KeyOut {
    /// X11 keysym pressed (`true`) or released.
    Key(u32, bool),
    /// Local clipboard text for the remote clipboard.
    Clipboard(String),
}

const SHIFT: u32 = 0xffe1;
const CONTROL: u32 = 0xffe3;
const ALT: u32 = 0xffe9;
/// macOS Command; Super is what X11 and macOS VNC servers take for it.
const SUPER: u32 = 0xffeb;

/// Turns egui key events into key presses and releases on the remote side.
#[derive(Default)]
struct Keyboard {
    /// Modifier state the remote side has been told about.
    modifiers: Modifiers,
    /// Keys held down and the keysym sent for each, so the release matches the press even when
    /// Shift changed in between.
    held: Vec<(Key, u32)>,
}

impl Keyboard {
    fn handle(&mut self, events: &[Event], modifiers: Modifiers, focused: bool) -> Vec<KeyOut> {
        let mut out = Vec::new();
        if !focused {
            // Nothing stays pressed on the remote side while another window has the keyboard.
            out.extend(self.held.drain(..).map(|(_, keysym)| KeyOut::Key(keysym, false)));
            self.sync_modifiers(Modifiers::NONE, &mut out);
            return out;
        }
        for event in events {
            match event {
                Event::Key {
                    key,
                    physical_key,
                    pressed,
                    modifiers,
                    ..
                } => {
                    self.sync_modifiers(*modifiers, &mut out);
                    let key = physical_key.unwrap_or(*key);
                    self.key(key, *pressed, modifiers.shift, &mut out);
                }
                // ⌘C/⌘X (Ctrl+C/X elsewhere) arrive as clipboard events: send the key so the
                // remote side copies; its clipboard comes back as text.
                Event::Copy => self.tap(Key::C, modifiers, &mut out),
                Event::Cut => self.tap(Key::X, modifiers, &mut out),
                // Paste: hand the local clipboard over, then let the remote side paste it.
                Event::Paste(text) => {
                    out.push(KeyOut::Clipboard(text.clone()));
                    self.tap(Key::V, modifiers, &mut out);
                }
                _ => {}
            }
        }
        self.sync_modifiers(modifiers, &mut out);
        out
    }

    fn key(&mut self, key: Key, pressed: bool, shift: bool, out: &mut Vec<KeyOut>) {
        if pressed {
            let Some(keysym) = keysym(key, shift) else { return };
            if !self.held.iter().any(|(k, _)| *k == key) {
                self.held.push((key, keysym));
            }
            out.push(KeyOut::Key(keysym, true));
        } else if let Some(index) = self.held.iter().position(|(k, _)| *k == key) {
            let (_, keysym) = self.held.remove(index);
            out.push(KeyOut::Key(keysym, false));
        }
    }

    fn tap(&mut self, key: Key, modifiers: Modifiers, out: &mut Vec<KeyOut>) {
        self.sync_modifiers(modifiers, out);
        self.key(key, true, modifiers.shift, out);
        self.key(key, false, modifiers.shift, out);
    }

    fn sync_modifiers(&mut self, now: Modifiers, out: &mut Vec<KeyOut>) {
        let pairs = [
            (self.modifiers.shift, now.shift, SHIFT),
            (self.modifiers.ctrl, now.ctrl, CONTROL),
            (self.modifiers.alt, now.alt, ALT),
            (self.modifiers.mac_cmd, now.mac_cmd, SUPER),
        ];
        for (before, after, keysym) in pairs {
            if before != after {
                out.push(KeyOut::Key(keysym, after));
            }
        }
        self.modifiers = now;
    }
}

/// X11 keysym of a key at its position on a US keyboard.
fn keysym(key: Key, shift: bool) -> Option<u32> {
    let pick = |plain: char, shifted: char| u32::from(if shift { shifted } else { plain });
    Some(match key {
        Key::Enter => 0xff0d,
        Key::Tab => 0xff09,
        Key::Backspace => 0xff08,
        Key::Escape => 0xff1b,
        Key::Delete => 0xffff,
        Key::Insert => 0xff63,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,
        Key::ArrowLeft => 0xff51,
        Key::ArrowUp => 0xff52,
        Key::ArrowRight => 0xff53,
        Key::ArrowDown => 0xff54,
        Key::Space => 0x20,
        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,
        Key::Num1 => pick('1', '!'),
        Key::Num2 => pick('2', '@'),
        Key::Num3 => pick('3', '#'),
        Key::Num4 => pick('4', '$'),
        Key::Num5 => pick('5', '%'),
        Key::Num6 => pick('6', '^'),
        Key::Num7 => pick('7', '&'),
        Key::Num8 => pick('8', '*'),
        Key::Num9 => pick('9', '('),
        Key::Num0 => pick('0', ')'),
        Key::Minus => pick('-', '_'),
        Key::Equals => pick('=', '+'),
        Key::OpenBracket => pick('[', '{'),
        Key::CloseBracket => pick(']', '}'),
        Key::Backslash => pick('\\', '|'),
        Key::Semicolon => pick(';', ':'),
        Key::Quote => pick('\'', '"'),
        Key::Comma => pick(',', '<'),
        Key::Period => pick('.', '>'),
        Key::Slash => pick('/', '?'),
        Key::Backtick => pick('`', '~'),
        _ => match key.name().as_bytes() {
            [letter] if letter.is_ascii_uppercase() => {
                u32::from(if shift { *letter } else { letter.to_ascii_lowercase() })
            }
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    fn rect(x: u16, y: u16, width: u16, height: u16) -> vnc::Rect {
        vnc::Rect { x, y, width, height }
    }

    fn pixel(screen: &Screen, x: usize, y: usize) -> [u8; 4] {
        let at = (y * screen.width + x) * 4;
        screen.pixels[at..at + 4].try_into().expect("4 bytes")
    }

    #[test]
    fn updates_land_in_place_and_become_opaque() {
        let mut screen = Screen::default();
        screen.resize(4, 3);
        screen.put(rect(1, 1, 2, 1), &[10, 20, 30, 0, 40, 50, 60, 0]);
        assert_eq!(pixel(&screen, 1, 1), [10, 20, 30, 255]);
        assert_eq!(pixel(&screen, 2, 1), [40, 50, 60, 255]);
        assert_eq!(pixel(&screen, 0, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn updates_outside_the_screen_are_ignored() {
        let mut screen = Screen::default();
        screen.resize(4, 3);
        screen.dirty = false;
        screen.put(rect(3, 0, 2, 1), &[1; 8]);
        screen.put(rect(0, 0, 2, 2), &[1; 4]);
        screen.copy(rect(0, 0, 2, 2), rect(3, 2, 2, 2));
        assert!(!screen.dirty);
        assert!(screen.pixels.iter().all(|b| *b == 0));
    }

    #[test]
    fn an_overlapping_copy_reads_before_writing() {
        let mut screen = Screen::default();
        screen.resize(3, 1);
        screen.put(rect(0, 0, 3, 1), &[1, 1, 1, 0, 2, 2, 2, 0, 3, 3, 3, 0]);
        screen.copy(rect(1, 0, 2, 1), rect(0, 0, 2, 1));
        assert_eq!([pixel(&screen, 0, 0)[0], pixel(&screen, 1, 0)[0], pixel(&screen, 2, 0)[0]], [1, 1, 2]);
    }

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn keys_are_sent_by_position_with_their_modifiers() {
        let mut keyboard = Keyboard::default();
        let shift = Modifiers::SHIFT;
        let out = keyboard.handle(&[key(Key::Num1, true, shift)], shift, true);
        assert_eq!(out, [KeyOut::Key(SHIFT, true), KeyOut::Key(u32::from('!'), true)]);
        // Shift is let go before the key: the release still names the pressed keysym.
        let out = keyboard.handle(&[key(Key::Num1, false, Modifiers::NONE)], Modifiers::NONE, true);
        assert_eq!(out, [KeyOut::Key(SHIFT, false), KeyOut::Key(u32::from('!'), false)]);
        let out = keyboard.handle(&[key(Key::A, true, Modifiers::NONE)], Modifiers::NONE, true);
        assert_eq!(out, [KeyOut::Key(u32::from('a'), true)]);
    }

    #[test]
    fn command_paste_hands_over_the_clipboard_first() {
        let mut keyboard = Keyboard::default();
        let command = Modifiers::MAC_CMD;
        let out = keyboard.handle(&[Event::Paste("hi".into())], command, true);
        assert_eq!(
            out,
            [
                KeyOut::Clipboard("hi".into()),
                KeyOut::Key(SUPER, true),
                KeyOut::Key(u32::from('v'), true),
                KeyOut::Key(u32::from('v'), false),
            ]
        );
    }

    #[test]
    fn losing_focus_releases_everything() {
        let mut keyboard = Keyboard::default();
        keyboard.handle(&[key(Key::A, true, Modifiers::CTRL)], Modifiers::CTRL, true);
        let out = keyboard.handle(&[], Modifiers::CTRL, false);
        assert_eq!(out, [KeyOut::Key(u32::from('a'), false), KeyOut::Key(CONTROL, false)]);
    }

    #[test]
    fn the_image_keeps_the_screen_aspect_ratio() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        let image = fit(area, (400, 400));
        assert_eq!(image, Rect::from_min_max(Pos2::new(50.0, 0.0), Pos2::new(150.0, 100.0)));
        assert_eq!(to_remote(Pos2::new(150.0, 100.0), image, (400, 400)), (399, 399));
        assert_eq!(to_remote(Pos2::new(100.0, 50.0), image, (400, 400)), (200, 200));
    }

    /// A minimal RFB 3.8 server without authentication: one 2x1 raw update, then waits.
    async fn serve(mut stream: tokio::io::DuplexStream) -> std::io::Result<()> {
        stream.write_all(b"RFB 003.008\n").await?;
        let mut version = [0u8; 12];
        stream.read_exact(&mut version).await?;
        stream.write_all(&[1, 1]).await?; // one security type: None
        let mut choice = [0u8; 1];
        stream.read_exact(&mut choice).await?;
        stream.write_u32(0).await?; // security result: ok
        let mut shared = [0u8; 1];
        stream.read_exact(&mut shared).await?;
        stream.write_u16(2).await?;
        stream.write_u16(1).await?;
        // 32 bpp, depth 24, little endian, true color, max 255, shifts r16 g8 b0
        stream.write_all(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]).await?;
        stream.write_u32(4).await?;
        stream.write_all(b"test").await?;
        // FramebufferUpdate: one raw rectangle covering the screen, in the client's RGBA format.
        stream.write_all(&[0, 0]).await?;
        stream.write_u16(1).await?;
        for value in [0u16, 0, 2, 1] {
            stream.write_u16(value).await?;
        }
        stream.write_i32(0).await?;
        stream.write_all(&[200, 100, 50, 0, 1, 2, 3, 0]).await?;
        // Drain client messages until it hangs up.
        let mut sink = [0u8; 1024];
        while stream.read(&mut sink).await? > 0 {}
        Ok(())
    }

    #[test]
    fn a_session_shows_what_the_server_sends() {
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let (client_side, server_side) = tokio::io::duplex(64 * 1024);
        rt.spawn(serve(server_side));
        let client = rt.block_on(handshake(client_side, None, None)).expect("handshake");
        let screen = Arc::new(Mutex::new(Screen::default()));
        let (_sender, receiver) = mpsc::channel(INPUT_QUEUE);
        rt.spawn(pump(client, Arc::clone(&screen), receiver, egui::Context::default()));
        let shown = rt.block_on(async {
            for _ in 0..100 {
                if lock(&screen).pixels.first() == Some(&200) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        });
        let screen = lock(&screen);
        assert!(shown, "no update arrived");
        assert_eq!((screen.width, screen.height), (2, 1));
        assert_eq!(pixel(&screen, 0, 0), [200, 100, 50, 255]);
        assert_eq!(pixel(&screen, 1, 0), [1, 2, 3, 255]);
    }
}
