//! The AI co-worker in an RDP session: the overlay on the window, the agent on the session's
//! runtime, and the screen and input they share.

use core::time::Duration;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use anyhow::{Context as _, bail};
use ironrdp_client::rdp::{RdpInputEvent, RdpInputSender};
use ironrdp_viewer::app::{App, OverlayPainter};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{Key, NamedKey};

use super::SessionEvent;
use super::agent_input::AgentDriver;
use super::canvas::Text;
use super::overlay::{Hit, Overlay};
use crate::agent::action::{Action, Point};
use crate::agent::capture::Pixels;
use crate::agent::{self, Command, Event, Options, Remote, State, provider};

/// Time between redraws while the overlay moves on its own.
const ANIMATION_FRAME: Duration = Duration::from_millis(33);
/// How long proposed actions wait to be declined before they run by themselves.
const AUTO_APPROVE_AFTER: Duration = Duration::from_secs(3);
/// Bubble lines per wheel notch, and trackpad points per line.
const LINES_PER_NOTCH: f32 = 3.0;
const PIXELS_PER_LINE: f32 = 20.0;
/// A press on the character: where it began, the character's corner then, and whether the
/// pointer has moved far enough to be a drag.
type Drag = ((f32, f32), (f32, f32), bool);

/// How far the pointer moves (physical pixels) before pressing the character drags it.
const DRAG_THRESHOLD: f32 = 4.0;
/// Fast-path input events per message, within the PDU header's count.
const EVENTS_PER_MESSAGE: usize = 15;
/// How long AI input waits for room in the session's input queue.
const INPUT_QUEUE_WAIT: Duration = Duration::from_secs(2);
const INPUT_QUEUE_POLL: Duration = Duration::from_millis(10);

/// The assistant's half of the session window.
pub(super) struct Assistant {
    overlay: Rc<RefCell<Overlay>>,
    commands: mpsc::UnboundedSender<Command>,
    /// Set when the person uses the session while the AI works.
    taken_over: Arc<AtomicBool>,
    /// Set while the overlay moves on its own.
    animating: Arc<AtomicBool>,
    /// Last pointer position over the window, in physical pixels.
    cursor: (f32, f32),
    /// The release of a click the overlay took.
    swallow_release: bool,
    /// The character is pressed: where the press began, the character's corner then, and
    /// whether it has moved far enough to be a drag.
    drag: Option<Drag>,
    /// The input method composes text (macOS sends typed text as commits then).
    ime_active: bool,
    /// Wheel movement over the bubble not yet amounting to a line.
    wheel: f32,
}

/// Starts the assistant when it is turned on in the settings and an API key is stored; `None`
/// leaves the session without one.
pub(super) fn start(
    runtime: &tokio::runtime::Handle,
    proxy: EventLoopProxy<SessionEvent>,
    input: RdpInputSender,
) -> Option<(Assistant, OverlayPainter)> {
    let settings = crate::profile::ProfileStore::load().map(|store| store.ai().clone()).unwrap_or_default();
    if !settings.enabled {
        info!("AI assistant is off (HopDesk settings)");
        return None;
    }
    let key = match crate::secret_store::get(settings.kind.key_account()) {
        Ok(Some(key)) if !key.trim().is_empty() => key.trim().to_owned(),
        // A compatible server may run without a key.
        Ok(_) if !settings.kind.needs_key() => String::new(),
        Ok(_) => {
            warn!(provider = ?settings.kind, "AI assistant is on but no API key is stored; the session runs without it");
            return None;
        }
        Err(error) => {
            warn!(error = %format!("{error:#}"), "AI API key unavailable");
            return None;
        }
    };
    let provider = match provider::build(&settings, key) {
        Ok(provider) => provider,
        Err(error) => {
            warn!(error = %format!("{error:#}"), "AI assistant unavailable");
            return None;
        }
    };
    let text = Text::load();
    if text.is_none() {
        warn!("No UI font found; the assistant shows no text");
    }
    let overlay = Rc::new(RefCell::new(Overlay::new(text, &provider::destination(&settings))));
    let painter = {
        let overlay = Rc::clone(&overlay);
        let started = std::time::Instant::now();
        Box::new(move |pixels: &mut [u32], width: usize, height: usize| {
            let mut overlay = overlay.borrow_mut();
            overlay.set_time(started.elapsed().as_secs_f32());
            overlay.paint(pixels, width, height);
        }) as OverlayPainter
    };
    // While the overlay moves on its own (thinking, acting), the window is redrawn in step.
    let animating = Arc::new(AtomicBool::new(false));
    {
        let (animating, proxy) = (Arc::clone(&animating), proxy.clone());
        runtime.spawn(async move {
            let mut frames = tokio::time::interval(ANIMATION_FRAME);
            frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                frames.tick().await;
                if animating.load(Ordering::Relaxed)
                    && proxy.send_event(SessionEvent::Tick).is_err()
                {
                    break;
                }
            }
        });
    }
    let (commands, receiver) = mpsc::unbounded_channel();
    let taken_over = Arc::new(AtomicBool::new(false));
    let remote = RdpRemote {
        proxy: proxy.clone(),
        input,
        driver: AgentDriver::new(),
    };
    let events = Mutex::new(proxy);
    let emit = move |event: Event| {
        let proxy = events.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = proxy.send_event(SessionEvent::Agent(event));
    };
    let options = Options {
        approve: true,
        auto_approve: settings.auto_approve.then_some(AUTO_APPROVE_AFTER),
        coordinates: provider::coordinates(&settings),
    };
    runtime.spawn(agent::run(provider, remote, receiver, Arc::clone(&taken_over), emit, options));
    info!(provider = ?settings.kind, model = %settings.model, "AI assistant ready");
    let assistant = Assistant {
        overlay,
        commands,
        taken_over,
        animating,
        cursor: (0.0, 0.0),
        swallow_release: false,
        drag: None,
        ime_active: false,
        wheel: 0.0,
    };
    Some((assistant, painter))
}

impl Assistant {
    /// Handles a window event meant for the overlay; `true` keeps it from the session.
    pub(super) fn window_event(&mut self, app: &App, event: &WindowEvent) -> bool {
        let focused = self.overlay.borrow().focused();
        let consumed = match event {
            WindowEvent::CursorMoved { position, .. } => {
                #[expect(clippy::cast_possible_truncation, reason = "window coordinates")]
                let cursor = (position.x as f32, position.y as f32);
                self.cursor = cursor;
                self.drag_to(cursor)
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let hit = self.overlay.borrow().hit(self.cursor.0, self.cursor.1);
                if hit == Some(Hit::Avatar) {
                    self.swallow_release = true;
                    self.drag = Some((self.cursor, self.overlay.borrow().corner(), false));
                    true
                } else if let Some(hit) = hit {
                    self.swallow_release = true;
                    self.click(app, hit);
                    true
                } else {
                    if focused {
                        self.set_focus(app, false);
                    }
                    self.person_acted();
                    false
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.swallow_release => {
                self.swallow_release = false;
                // A press on the character that did not move is a click: show or hide the bubble.
                if let Some((_, _, false)) = self.drag.take() {
                    self.overlay.borrow_mut().toggle();
                }
                true
            }
            WindowEvent::MouseWheel { delta, .. } if self.overlay.borrow().hit(self.cursor.0, self.cursor.1) == Some(Hit::Panel) => {
                self.scroll_bubble(*delta);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
            | WindowEvent::MouseWheel { .. } => {
                self.person_acted();
                false
            }
            WindowEvent::KeyboardInput { event, .. } if focused => {
                self.key(app, event);
                true
            }
            WindowEvent::ThemeChanged(theme) => {
                self.overlay
                    .borrow_mut()
                    .set_dark(*theme == winit::window::Theme::Dark);
                false
            }
            WindowEvent::ModifiersChanged(_) if focused => true,
            WindowEvent::Ime(ime) => {
                self.ime(ime);
                focused
            }
            // Esc always reaches the assistant while it works or waits: stop, or decline.
            WindowEvent::KeyboardInput { event, .. }
                if event.logical_key == Key::Named(NamedKey::Escape)
                    && self.overlay.borrow().state() != State::Idle =>
            {
                if event.state == ElementState::Pressed {
                    self.escape();
                }
                true
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    self.person_acted();
                }
                false
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                #[expect(clippy::cast_possible_truncation, reason = "a display scale")]
                self.overlay.borrow_mut().set_scale(*scale_factor as f32);
                false
            }
            _ => false,
        };
        if consumed {
            self.sync_animation();
            redraw(app);
        }
        consumed
    }

    /// Scrolls the conversation in the bubble: up shows older lines.
    fn scroll_bubble(&mut self, delta: MouseScrollDelta) {
        let mut overlay = self.overlay.borrow_mut();
        self.wheel += match delta {
            MouseScrollDelta::LineDelta(_, y) => y * LINES_PER_NOTCH,
            #[expect(clippy::cast_possible_truncation, reason = "a scroll distance")]
            MouseScrollDelta::PixelDelta(position) => position.y as f32 / (PIXELS_PER_LINE * overlay.scale()),
        };
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        #[expect(clippy::cast_possible_truncation, reason = "a few lines")]
        overlay.scroll_by(whole as i32);
    }

    /// Moves the character while it is dragged; `true` keeps the pointer move from the session.
    fn drag_to(&mut self, cursor: (f32, f32)) -> bool {
        let Some((start, corner, moved)) = &mut self.drag else { return false };
        let (dx, dy) = (cursor.0 - start.0, cursor.1 - start.1);
        *moved |= dx.hypot(dy) >= DRAG_THRESHOLD;
        if *moved {
            let mut overlay = self.overlay.borrow_mut();
            let scale = overlay.scale();
            // The corner is measured from the right and bottom: moving right shrinks it.
            overlay.set_corner((corner.0 - dx / scale, corner.1 - dy / scale));
        }
        true
    }

    /// Keeps the overlay in step with the session after its output was handled.
    pub(super) fn after_output(&self, app: &App) {
        let mut overlay = self.overlay.borrow_mut();
        if let Some((_, size)) = app.frame() {
            overlay.set_session(size);
        }
        if let Some(window) = app.window() {
            #[expect(clippy::cast_possible_truncation, reason = "a display scale")]
            overlay.set_scale(window.scale_factor() as f32);
            overlay.set_dark(window.theme() == Some(winit::window::Theme::Dark));
        }
    }

    pub(super) fn agent_event(&mut self, app: &App, event: Event) {
        let mut answer = false;
        {
            let mut overlay = self.overlay.borrow_mut();
            match event {
                Event::State(state) => {
                    overlay.set_state(state);
                    answer = state == State::TakenOver;
                }
                Event::Say(text) => overlay.say(text),
                Event::Notice(text) => overlay.notice(text),
                Event::Proposal(proposal) => {
                    overlay.propose(proposal);
                    answer = true;
                }
                Event::Pointer(point) => overlay.set_pointer(point),
                Event::AutoApprove(after) => {
                    overlay.set_countdown(after.map(|after| after.as_secs_f32()))
                }
            }
        }
        self.sync_animation();
        // The person is asked: the keyboard goes to the assistant, so Return and Esc answer.
        if answer {
            self.set_focus(app, true);
        }
        redraw(app);
    }

    /// A frame of the overlay's own motion.
    pub(super) fn tick(&self, app: &App) {
        redraw(app);
        self.sync_animation();
    }

    /// Esc: stops the AI while it works, declines what it proposes.
    fn escape(&self) {
        let command = match self.overlay.borrow().state() {
            State::Approval => Command::Decline,
            _ => Command::Stop,
        };
        let _ = self.commands.send(command);
    }

    fn send_input(&self) {
        if let Some(text) = self.overlay.borrow_mut().take_input() {
            let _ = self.commands.send(Command::Say(text));
        }
    }

    /// The person used the session: while the AI works, that takes the screen back.
    fn person_acted(&self) {
        if matches!(self.overlay.borrow().state(), State::Thinking | State::Acting) {
            self.taken_over.store(true, Ordering::Relaxed);
        }
        self.hold();
    }

    /// The person is busy while a countdown runs: the proposal waits for an answer instead.
    fn hold(&self) {
        if self.overlay.borrow().counting() {
            let _ = self.commands.send(Command::Hold);
        }
    }

    /// Keeps the redraw ticks in step with whether the overlay moves on its own.
    fn sync_animation(&self) {
        self.animating
            .store(self.overlay.borrow().animating(), Ordering::Relaxed);
    }

    fn click(&mut self, app: &App, hit: Hit) {
        let command = match hit {
            // Handled as a press and release, since pressing it also starts a drag.
            Hit::Avatar => None,
            Hit::Input => {
                self.set_focus(app, true);
                None
            }
            Hit::Send => {
                self.send_input();
                None
            }
            Hit::Approve => Some(Command::Approve),
            Hit::Decline => Some(Command::Decline),
            Hit::Stop => Some(Command::Stop),
            Hit::Panel => {
                self.hold();
                None
            }
        };
        if let Some(command) = command {
            let _ = self.commands.send(command);
        }
    }

    fn set_focus(&mut self, app: &App, focused: bool) {
        self.overlay.borrow_mut().set_focused(focused);
        if let Some(window) = app.window() {
            window.set_ime_allowed(focused);
        }
        if !focused {
            self.ime_active = false;
        }
        redraw(app);
    }

    fn key(&mut self, app: &App, event: &KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let state = self.overlay.borrow().state();
        let mut overlay = self.overlay.borrow_mut();
        match &event.logical_key {
            // With nothing typed, Return answers what the assistant asks.
            Key::Named(NamedKey::Enter)
                if overlay.input_is_empty()
                    && matches!(state, State::Approval | State::TakenOver) =>
            {
                let _ = self.commands.send(Command::Approve);
            }
            Key::Named(NamedKey::Enter) => {
                drop(overlay);
                self.send_input();
            }
            Key::Named(NamedKey::Escape) if state != State::Idle => {
                drop(overlay);
                self.escape();
            }
            Key::Named(NamedKey::Escape) => {
                drop(overlay);
                self.set_focus(app, false);
            }
            Key::Named(NamedKey::Backspace) => overlay.backspace(),
            _ => {
                // Writing an answer: the proposal waits for it.
                if overlay.counting() {
                    let _ = self.commands.send(Command::Hold);
                }
                // With an input method on macOS, typed text arrives as commits instead.
                let commits = self.ime_active && cfg!(target_os = "macos");
                if let (Some(text), false) = (&event.text, commits) {
                    overlay.type_text(text);
                }
            }
        }
    }

    fn ime(&mut self, ime: &Ime) {
        let mut overlay = self.overlay.borrow_mut();
        match ime {
            Ime::Enabled => self.ime_active = true,
            Ime::Disabled => self.ime_active = false,
            Ime::Preedit(text, _) => overlay.set_preedit(text.clone()),
            Ime::Commit(text) => {
                overlay.set_preedit(String::new());
                overlay.type_text(text);
            }
        }
    }
}

fn redraw(app: &App) {
    if let Some(window) = app.window() {
        window.request_redraw();
    }
}

/// The session's latest frame as RGBA for the agent.
pub(super) fn snapshot(app: &App) -> anyhow::Result<Pixels> {
    let (frame, (width, height)) = app.frame().context("no screen yet")?;
    let rgba = frame
        .iter()
        .flat_map(|pixel| {
            let [_, r, g, b] = pixel.to_be_bytes();
            [r, g, b, 255]
        })
        .collect();
    Ok(Pixels {
        width: u32::from(width),
        height: u32::from(height),
        rgba,
    })
}

/// The session as the agent sees it: frames through the window's event loop, input straight
/// into the session's input queue.
struct RdpRemote {
    proxy: EventLoopProxy<SessionEvent>,
    input: RdpInputSender,
    driver: AgentDriver,
}

impl Remote for RdpRemote {
    async fn snapshot(&mut self) -> anyhow::Result<Pixels> {
        let (sender, receiver) = oneshot::channel();
        if self.proxy.send_event(SessionEvent::Snapshot(sender)).is_err() {
            bail!("the session window closed");
        }
        receiver.await.context("the session window closed")?
    }

    async fn act(&mut self, action: &Action) -> anyhow::Result<()> {
        let events = self.driver.input(action);
        for chunk in events.chunks(EVENTS_PER_MESSAGE) {
            let mut event = RdpInputEvent::FastPath(chunk.iter().copied().collect());
            let deadline = tokio::time::Instant::now() + INPUT_QUEUE_WAIT;
            loop {
                match self.input.try_send(event) {
                    Ok(()) => break,
                    Err(mpsc::error::TrySendError::Full(back)) if tokio::time::Instant::now() < deadline => {
                        event = back;
                        tokio::time::sleep(INPUT_QUEUE_POLL).await;
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => bail!("the session does not take input"),
                    Err(mpsc::error::TrySendError::Closed(_)) => bail!("the session ended"),
                }
            }
        }
        Ok(())
    }

    fn cursor(&self) -> Point {
        self.driver.cursor()
    }
}

