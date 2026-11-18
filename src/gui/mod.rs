//! Launcher window (`hopdesk` without arguments): connection list, details, add/edit.
//!
//! Each connection runs as a child process (`hopdesk connect <name>`): the session window (RDP
//! viewer or SSH terminal) owns its own winit event loop, and winit allows only one per process.

mod detail;
mod form;
mod motion;
mod platform;
mod sessions;
pub(crate) mod theme;
#[cfg(any(target_os = "macos", windows))]
mod tray;
mod widgets;

#[cfg(test)]
mod screenshots;

use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use eframe::egui::{
    self, Align2, CornerRadius, FontId, Id, Key, Margin, Modifiers, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2,
};
use uuid::Uuid;

use self::form::Form;
use self::sessions::{Notice, Sessions, State};
use self::theme::{Bold as _, Palette, bold_family, palette};
use crate::profile::{Profile, ProfileStore};
use crate::trust::TrustStore;
use crate::update::{self, Release};

fn protocol_name(protocol: crate::profile::Protocol) -> &'static str {
    match protocol {
        crate::profile::Protocol::Rdp => "RDP",
        crate::profile::Protocol::Ssh => "SSH",
        crate::profile::Protocol::Vnc => "VNC",
    }
}

/// How a tunneled profile reaches its target; `None` for a direct connection.
fn route_name(profile: &Profile) -> Option<&'static str> {
    if profile.vpn.is_some() {
        Some("VPN 경유")
    } else if profile.use_jump {
        Some("점프 호스트 경유")
    } else {
        None
    }
}

/// Color and wording of a session state, shared by the sidebar and the detail header.
fn state_badge(pal: &Palette, state: &State) -> (egui::Color32, &'static str) {
    match state {
        State::Connected => (pal.success, "연결됨"),
        State::Failed(_) => (pal.danger, "연결 실패"),
        State::Reconnecting => (pal.warning, "다시 연결하는 중"),
        _ => (pal.accent, "연결하는 중"),
    }
}

/// What the target of a profile is called in the UI: "원격 PC" (desktop) or "원격 서버" (SSH).
fn target_noun(protocol: crate::profile::Protocol) -> &'static str {
    match protocol {
        crate::profile::Protocol::Ssh => "원격 서버",
        _ => "원격 PC",
    }
}

/// macOS draws icons on Apple's grid (with margin); elsewhere the tile fills the square.
#[cfg(target_os = "macos")]
pub(crate) const WINDOW_ICON: &[u8] = include_bytes!("../../assets/icon/icon-256.png");
#[cfg(not(target_os = "macos"))]
pub(crate) const WINDOW_ICON: &[u8] = include_bytes!("../../assets/icon/icon-full-256.png");

/// The app icon's small rendition, for the sidebar heading.
const LOGO: egui::ImageSource<'static> = egui::ImageSource::Bytes {
    uri: std::borrow::Cow::Borrowed("bytes://hopdesk/icon-small.svg"),
    bytes: egui::load::Bytes::Static(include_bytes!("../../assets/icon/icon-small.svg")),
};

/// Dialogs grow from this scale while their backdrop darkens to this alpha.
const DIALOG_START_SCALE: f32 = 0.96;
const DIALOG_BACKDROP_ALPHA: u8 = 100;

/// How often a running launcher looks for a newer release (it may stay in the menu bar for days).
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub fn run() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("HopDesk")
            // Matches hopdesk.desktop so Linux desktops show the right icon and name.
            .with_app_id("hopdesk")
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(eframe::icon_data::from_png_bytes(WINDOW_ICON).expect("bundled icon is a valid PNG")),
        ..Default::default()
    };
    eframe::run_native(
        "HopDesk",
        options,
        Box::new(|cc| {
            motion::follow_system();
            theme::apply(&cc.egui_ctx);
            #[cfg_attr(not(any(target_os = "macos", windows)), expect(unused_mut, reason = "no tray"))]
            let mut launcher = Launcher::new();
            #[cfg_attr(not(any(target_os = "macos", windows)), expect(unused_variables, reason = "no tray"))]
            let check_now = launcher.start_update_checks(&cc.egui_ctx);
            #[cfg(any(target_os = "macos", windows))]
            match tray::Tray::new(&cc.egui_ctx, check_now) {
                Ok(tray) => launcher.tray = Some(tray),
                // Without the icon, closing the window quits as before.
                Err(error) => tracing::warn!(error = %format!("{error:#}"), "Tray icon unavailable"),
            }
            Ok(Box::new(launcher))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}

enum View {
    Detail,
    Edit { form: Box<Form>, original: Box<Form> },
}

/// Where the user wants to go; may need confirmation when the form has unsaved changes.
#[derive(Clone, Copy)]
enum Nav {
    Select(Uuid),
    New,
    Edit(Uuid),
    CloseEditor,
}

#[derive(Clone, Copy)]
enum Dialog {
    Delete(Uuid),
    ForgetKeys(Uuid),
    Discard(Nav),
}

#[derive(Clone, Copy, PartialEq)]
enum ToastKind {
    Success,
    Info,
    Error,
}

/// Progress of the update offered in the sidebar.
#[derive(Clone)]
enum UpdateState {
    None,
    Available(Release),
    Installing(Release),
    Failed(Release, String),
}

struct Toast {
    /// Stable identity while toasts above it come and go.
    id: u64,
    text: String,
    kind: ToastKind,
    /// When it appeared (egui time); toasts from startup show without an entrance.
    shown: f64,
    until: f64,
    /// When it started leaving (expired or clicked); it is dropped once faded out.
    leaving: Option<f64>,
}

impl Toast {
    fn new(id: u64, kind: ToastKind, text: String, shown: f64, until: f64) -> Self {
        Self { id, text, kind, shown, until, leaving: None }
    }
}

pub struct Launcher {
    store: ProfileStore,
    selected: Option<Uuid>,
    view: View,
    search: String,
    dialog: Option<Dialog>,
    toasts: Vec<Toast>,
    next_toast: u64,
    sessions: Sessions,
    update: Arc<Mutex<UpdateState>>,
    /// Results of update checks the user asked for, shown as toasts.
    update_notices: Arc<Mutex<Vec<(ToastKind, String)>>>,
    /// Menu bar / notification area icon; while present, closing the window hides it.
    #[cfg(any(target_os = "macos", windows))]
    tray: Option<tray::Tray>,
}

impl Launcher {
    fn new() -> Self {
        let mut toasts = Vec::new();
        let store = ProfileStore::load().unwrap_or_else(|e| {
            let text = format!("설정 파일을 읽지 못했습니다: {e:#}");
            toasts.push(Toast::new(toasts.len() as u64, ToastKind::Error, text, f64::NEG_INFINITY, f64::INFINITY));
            ProfileStore::default()
        });
        // Before any window opens: the Keychain may ask for access once per stored secret.
        if let Err(e) = store.migrate_secrets() {
            let text = format!("저장된 비밀번호를 옮기지 못했습니다: {e:#}");
            toasts.push(Toast::new(toasts.len() as u64, ToastKind::Error, text, f64::NEG_INFINITY, f64::INFINITY));
        }
        let mut launcher = Self::with_store(store, Sessions::default());
        launcher.next_toast = toasts.len() as u64;
        launcher.toasts = toasts;
        launcher
    }

    fn with_store(store: ProfileStore, sessions: Sessions) -> Self {
        let selected = store.profiles().first().map(|p| p.id);
        Self {
            store,
            selected,
            view: View::Detail,
            search: String::new(),
            dialog: None,
            toasts: Vec::new(),
            next_toast: 0,
            sessions,
            update: Arc::new(Mutex::new(UpdateState::None)),
            update_notices: Arc::default(),
            #[cfg(any(target_os = "macos", windows))]
            tray: None,
        }
    }

    fn update_state(&self) -> UpdateState {
        self.update.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set_update_state(&self, state: UpdateState) {
        *self.update.lock().unwrap_or_else(PoisonError::into_inner) = state;
    }

    /// Looks for a newer release in the background now, every [`UPDATE_CHECK_INTERVAL`] after that,
    /// and whenever the returned sender asks; asked-for checks report their result as a toast.
    /// Development builds never check.
    fn start_update_checks(&self, ctx: &egui::Context) -> mpsc::Sender<()> {
        update::clean_up_previous();
        let (requests, requested) = mpsc::channel();
        // The thread holds a sender itself, so waiting only ever ends by a request or the interval.
        let keep_open = requests.clone();
        let (state, notices, ctx) = (self.update.clone(), self.update_notices.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _keep_open = keep_open;
            let mut asked = false;
            loop {
                let notice = if cfg!(debug_assertions) {
                    asked.then(|| (ToastKind::Info, "개발 빌드는 업데이트를 확인하지 않습니다".to_owned()))
                } else {
                    check_update_once(&state, asked)
                };
                if let Some(notice) = notice {
                    notices.lock().unwrap_or_else(PoisonError::into_inner).push(notice);
                }
                ctx.request_repaint();
                asked = requested.recv_timeout(UPDATE_CHECK_INTERVAL).is_ok();
            }
        });
        requests
    }

    /// Installs `release` in the background; the app restarts into the new version on success.
    fn install_update(&self, ctx: &egui::Context, release: Release) {
        self.set_update_state(UpdateState::Installing(release.clone()));
        let this = self.update.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || match update::install(&release) {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                *this.lock().unwrap_or_else(PoisonError::into_inner) =
                    UpdateState::Failed(release, format!("{error:#}"));
                ctx.request_repaint();
            }
        });
    }

    /// Update offer pinned to the bottom of the sidebar.
    fn update_card(&mut self, ui: &mut Ui) {
        let state = self.update_state();
        let release = match &state {
            UpdateState::None => return,
            UpdateState::Available(r) | UpdateState::Installing(r) | UpdateState::Failed(r, _) => r.clone(),
        };
        let pal = palette(ui);
        egui::Panel::bottom("update")
            .show_separator_line(false)
            .frame(egui::Frame::new().inner_margin(Margin { top: 8, ..Margin::ZERO }))
            .show(ui, |ui| {
                widgets::banner(ui, pal.accent_soft, pal.accent, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    ui.label(RichText::new(format!("새 버전 {}", release.version)).bold().color(pal.text));
                    match &state {
                        UpdateState::Installing(_) => {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0).color(pal.accent));
                                ui.label(RichText::new("내려받아 설치하는 중…").color(pal.text_weak));
                            });
                        }
                        UpdateState::Failed(_, error) => {
                            ui.label(RichText::new("업데이트하지 못했습니다").color(pal.danger));
                            ui.label(RichText::new(error).small().color(pal.text_weak));
                        }
                        _ => {}
                    }
                    if !matches!(state, UpdateState::Installing(_)) {
                        ui.horizontal(|ui| {
                            let busy = self.sessions.any_active();
                            let label = if matches!(state, UpdateState::Failed(..)) { "다시 시도" } else { "업데이트" };
                            let button = ui
                                .add_enabled_ui(!busy, |ui| widgets::primary_button(ui, label))
                                .inner
                                .on_hover_text("내려받아 설치한 뒤 HopDesk를 다시 시작합니다")
                                .on_disabled_hover_text("접속을 모두 종료한 뒤 업데이트하세요");
                            if button.clicked() {
                                self.install_update(ui.ctx(), release.clone());
                            }
                            if ui.link("변경 사항").clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(&release.page_url));
                            }
                        });
                    }
                });
            });
    }

    fn profile(&self, id: Uuid) -> Option<&Profile> {
        self.store.profiles().iter().find(|p| p.id == id)
    }

    fn toast(&mut self, ctx: &egui::Context, kind: ToastKind, text: impl Into<String>) {
        let now = ctx.input(|i| i.time);
        let secs = if kind == ToastKind::Error { 6.0 } else { 3.0 };
        self.toasts.push(Toast::new(self.next_toast, kind, text.into(), now, now + secs));
        self.next_toast += 1;
    }

    // --- navigation ---------------------------------------------------------------------------

    fn navigate(&mut self, nav: Nav) {
        if let View::Edit { form, original } = &self.view
            && form.is_dirty(original)
        {
            self.dialog = Some(Dialog::Discard(nav));
            return;
        }
        self.apply_nav(nav);
    }

    fn apply_nav(&mut self, nav: Nav) {
        match nav {
            Nav::Select(id) => {
                self.selected = Some(id);
                self.view = View::Detail;
            }
            Nav::New => {
                let form = Box::new(Form::new_profile());
                self.view = View::Edit { original: form.clone(), form };
            }
            Nav::Edit(id) => {
                if let Some(p) = self.profile(id) {
                    let form = Box::new(Form::from_profile(p));
                    self.selected = Some(id);
                    self.view = View::Edit { original: form.clone(), form };
                }
            }
            Nav::CloseEditor => self.view = View::Detail,
        }
    }

    fn filtered(&self) -> Vec<&Profile> {
        let q = self.search.trim().to_lowercase();
        self.store
            .profiles()
            .iter()
            .filter(|p| {
                q.is_empty()
                    || [&p.name, &p.target.host, &p.target.user, &p.jump.host]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&q))
                    || p.vpn.as_ref().is_some_and(|vpn| vpn.endpoint.to_lowercase().contains(&q))
            })
            .collect()
    }

    // --- actions ------------------------------------------------------------------------------

    fn connect(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some((name, protocol)) = self.profile(id).map(|p| (p.name.clone(), p.protocol)) else { return };
        self.sessions.dismiss(id);
        if let Err(e) = self.sessions.start(id, &name, protocol, ctx) {
            self.toast(ctx, ToastKind::Error, format!("연결을 시작하지 못했습니다: {e:#}"));
        }
    }

    fn save(&mut self, ctx: &egui::Context) {
        let View::Edit { form, .. } = &mut self.view else { return };
        let profile = match form.validate(&self.store) {
            Ok(profile) => profile,
            Err(errors) => {
                form.set_errors(errors);
                return;
            }
        };
        let is_new = form.is_new();
        match form.commit(&profile, &mut self.store) {
            Ok(()) => {
                self.selected = Some(profile.id);
                self.view = View::Detail;
                let text = if is_new { "연결을 추가했습니다" } else { "변경 사항을 저장했습니다" };
                self.toast(ctx, ToastKind::Success, text);
            }
            Err(e) => {
                // Drop the in-memory change so the list matches what is on disk.
                if let Ok(store) = ProfileStore::load() {
                    self.store = store;
                }
                self.toast(ctx, ToastKind::Error, format!("저장하지 못했습니다: {e:#}"));
            }
        }
    }

    fn delete(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some(profile) = self.profile(id).cloned() else { return };
        let result = self
            .store
            .remove(&profile.name)
            .and_then(|_| profile.delete_secrets())
            .and_then(|()| self.store.save());
        match result {
            Ok(()) => {
                self.sessions.dismiss(id);
                self.selected = self.store.profiles().first().map(|p| p.id);
                self.view = View::Detail;
                self.toast(ctx, ToastKind::Info, format!("‘{}’ 연결을 삭제했습니다", profile.name));
            }
            Err(e) => {
                if let Ok(store) = ProfileStore::load() {
                    self.store = store;
                }
                self.toast(ctx, ToastKind::Error, format!("삭제하지 못했습니다: {e:#}"));
            }
        }
    }

    fn forget_keys(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some(profile) = self.profile(id).cloned() else { return };
        let result = TrustStore::load()
            .and_then(|trust| {
                let jump = if profile.use_jump { trust.forget(&profile.jump.host)? } else { 0 };
                Ok(jump + trust.forget(&profile.target.host)?)
            });
        match result {
            Ok(0) => self.toast(ctx, ToastKind::Info, "저장된 호스트 키가 없습니다"),
            Ok(n) => {
                self.sessions.dismiss(id);
                self.toast(
                    ctx,
                    ToastKind::Success,
                    format!("호스트 키 {n}개를 초기화했습니다. 다음 연결 때 새 키를 저장합니다"),
                );
            }
            Err(e) => self.toast(ctx, ToastKind::Error, format!("초기화하지 못했습니다: {e:#}")),
        }
    }

    // --- drawing ------------------------------------------------------------------------------

    pub fn draw(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let update_notices = std::mem::take(&mut *self.update_notices.lock().unwrap_or_else(PoisonError::into_inner));
        for (kind, text) in update_notices {
            self.toast(&ctx, kind, text);
        }
        for notice in self.sessions.take_notices() {
            match notice {
                Notice::Ended { name } => self.toast(&ctx, ToastKind::Info, format!("‘{name}’ 세션이 종료되었습니다")),
            }
        }
        self.shortcuts(&ctx);

        let pal = palette(ui);
        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(272.0)
            .size_range(220.0..=380.0)
            .frame(egui::Frame::new().fill(pal.sidebar).inner_margin(Margin::symmetric(12, 14)))
            .show(ui, |ui| self.sidebar(ui));

        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(pal.bg).inner_margin(Margin::symmetric(28, 0)))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(760.0));
                self.main(ui);
            });

        self.dialogs(&ctx);
        self.draw_toasts(&ctx);
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.dialog.is_some() {
            return;
        }
        let typing = ctx.text_edit_focused();
        let pressed = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));

        if pressed(Modifiers::COMMAND, Key::N) {
            self.navigate(Nav::New);
        }
        if pressed(Modifiers::COMMAND, Key::F) {
            ctx.memory_mut(|m| m.request_focus(Id::unique("search")));
        }
        match &self.view {
            View::Edit { .. } => {
                if pressed(Modifiers::COMMAND, Key::S) {
                    self.save(ctx);
                } else if pressed(Modifiers::NONE, Key::Escape) {
                    self.navigate(Nav::CloseEditor);
                }
            }
            View::Detail => {
                let Some(id) = self.selected else { return };
                if pressed(Modifiers::COMMAND, Key::E) {
                    self.navigate(Nav::Edit(id));
                }
                if typing {
                    return;
                }
                if pressed(Modifiers::NONE, Key::Enter) && !self.sessions.state(id).is_some_and(|s| s.is_active()) {
                    self.connect(ctx, id);
                }
                let step = if pressed(Modifiers::NONE, Key::ArrowDown) {
                    1
                } else if pressed(Modifiers::NONE, Key::ArrowUp) {
                    -1
                } else {
                    0
                };
                if step != 0 {
                    let list = self.filtered();
                    if let Some(pos) = list.iter().position(|p| p.id == id) {
                        let next = (pos as isize + step).clamp(0, list.len() as isize - 1) as usize;
                        let next = list[next].id;
                        self.selected = Some(next);
                    }
                }
            }
        }
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let pal = palette(ui);
        ui.horizontal(|ui| {
            ui.add(egui::Image::new(LOGO).fit_to_exact_size(Vec2::splat(26.0)));
            ui.add_space(2.0);
            ui.label(RichText::new("HopDesk").size(theme::TITLE_SIZE).bold().color(pal.text));
            let version = concat!("v", env!("CARGO_PKG_VERSION"));
            widgets::tag(ui, version, pal.text_weak, pal.surface);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, widgets::Icon::Plus, 30.0)
                    .on_hover_text(format!("새 연결  {}", platform::shortcut("N")))
                    .clicked()
                {
                    self.navigate(Nav::New);
                }
            });
        });
        ui.add_space(6.0);

        if !self.store.profiles().is_empty() {
            let search = widgets::text_input(ui, &mut self.search, "이름이나 주소로 검색", ui.available_width(), false)
                .id(Id::unique("search"))
                .leading_icon(widgets::Icon::Search)
                .shortcut(platform::shortcut("F"));
            let response = ui.add(search);
            if response.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.search.clear();
                response.surrender_focus();
            }
            ui.add_space(6.0);
        }

        self.update_card(ui);

        let list: Vec<Profile> = self.filtered().into_iter().cloned().collect();
        if !self.store.profiles().is_empty() {
            let open = self.store.profiles().iter().filter(|p| self.sessions.state(p.id) == Some(State::Connected)).count();
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("연결 · {}", self.store.profiles().len())).small().bold().color(pal.text_weak));
                if open > 0 {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{open}개 사용 중")).small().color(pal.success));
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                        ui.painter().circle_filled(rect.center(), 3.0, pal.success);
                    });
                }
            });
            ui.add_space(2.0);
        }
        let mut clicked = None;
        let mut double = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            // Painted under the rows once their positions are known, so it can slide between them.
            let highlight = ui.painter().add(egui::Shape::Noop);
            let mut selected_row = None;
            for p in &list {
                let state = self.sessions.state(p.id);
                let selected = self.selected == Some(p.id) && matches!(self.view, View::Detail)
                    || matches!(&self.view, View::Edit { form, .. } if form.id == Some(p.id));
                let response = list_row(ui, &pal, p, selected, state.as_ref());
                if selected {
                    selected_row = Some(response.rect);
                }
                if response.clicked() {
                    clicked = Some(p.id);
                }
                if response.double_clicked() {
                    double = Some(p.id);
                }
            }
            paint_selection(ui, &pal, highlight, selected_row);
            if list.is_empty() && !self.store.profiles().is_empty() {
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("검색 결과가 없습니다").color(pal.text_weak));
                });
            }
        });
        if let Some(id) = clicked {
            self.navigate(Nav::Select(id));
        }
        if let Some(id) = double
            && matches!(self.view, View::Detail)
            && !self.sessions.state(id).is_some_and(|s| s.is_active())
        {
            self.connect(ui.ctx(), id);
        }
    }

    /// The main pane; switching between connections, the editor and the empty state cross-fades.
    fn main(&mut self, ui: &mut Ui) {
        let key = match &self.view {
            View::Edit { form, .. } => ("edit", form.id),
            View::Detail => ("detail", self.selected),
        };
        motion::fade_in(ui, Id::unique("main_view"), key, |ui| self.main_view(ui));
    }

    fn main_view(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        match &mut self.view {
            View::Edit { form, .. } => match form.ui(ui, &self.store) {
                form::Action::Save => self.save(&ctx),
                form::Action::Cancel => self.navigate(Nav::CloseEditor),
                form::Action::None => {}
            },
            View::Detail => {
                let Some(profile) = self.selected.and_then(|id| self.profile(id)).cloned() else {
                    self.empty_state(ui);
                    return;
                };
                let state = self.sessions.state(profile.id);
                let log = self.sessions.log(profile.id);
                let shared_folder = self
                    .store
                    .shared_folder()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| crate::shared_folder::default_path().unwrap_or_default());
                let since = self.sessions.connected_since(profile.id);
                match detail::ui(ui, &profile, state.as_ref(), since, &log, &shared_folder) {
                    detail::Action::Connect => self.connect(&ctx, profile.id),
                    detail::Action::Cancel => self.sessions.cancel(profile.id),
                    detail::Action::Dismiss => self.sessions.dismiss(profile.id),
                    detail::Action::Edit => self.navigate(Nav::Edit(profile.id)),
                    detail::Action::Delete => self.dialog = Some(Dialog::Delete(profile.id)),
                    detail::Action::ForgetKeys => self.dialog = Some(Dialog::ForgetKeys(profile.id)),
                    detail::Action::OpenSharedFolder => reveal_in_file_manager(&shared_folder),
                    detail::Action::PickSharedFolder => self.pick_shared_folder(&ctx),
                    detail::Action::None => {}
                }
            }
        }
    }

    /// Asks where the shared folder should live and remembers the choice. Picking nothing
    /// keeps the current location; the reset path (back to the default) is clearing the file.
    fn pick_shared_folder(&mut self, ctx: &egui::Context) {
        let Some(dir) = rfd::FileDialog::new().set_title("공유 폴더 위치 선택").pick_folder() else {
            return;
        };
        let display = dir.display().to_string();
        if let Err(error) = self.store.set_shared_folder(Some(dir)) {
            self.toast(ctx, ToastKind::Error, format!("공유 폴더 위치를 저장하지 못했습니다: {error}"));
        } else {
            self.toast(ctx, ToastKind::Info, format!("공유 폴더 위치를 {display}(으)로 바꿨습니다"));
        }
    }

    fn empty_state(&mut self, ui: &mut Ui) {
        let pal = palette(ui);
        let first_run = self.store.profiles().is_empty();
        ui.add_space((ui.available_height() * 0.28).max(40.0));
        ui.vertical_centered(|ui| {
            widgets::icon_badge(ui, widgets::Icon::Monitor, 72.0);
            ui.add_space(12.0);
            if first_run {
                ui.label(RichText::new("첫 연결을 만들어 보세요").size(theme::HEADING_SIZE).bold().color(pal.text));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("SSH 점프 호스트나 VPN을 거쳐 원격 PC나 서버에 안전하게 접속합니다.\n비밀번호는 {}에 저장됩니다.", platform::SECRET_STORE))
                        .line_height(Some(22.0))
                        .color(pal.text_weak),
                );
                ui.add_space(16.0);
                if widgets::hero_button(ui, widgets::Icon::Plus, "새 연결 만들기", Some(&platform::shortcut("N")), 240.0).clicked() {
                    self.navigate(Nav::New);
                }
            } else {
                ui.label(RichText::new("왼쪽 목록에서 연결을 선택하세요").size(theme::TITLE_SIZE).color(pal.text_weak));
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let shown = motion::toggle(ctx, Id::unique("dialog_shown"), self.dialog.is_some(), motion::BASE);
        let Some(dialog) = self.dialog else { return };
        let (title, body, confirm, danger) = match dialog {
            Dialog::Delete(id) => (
                format!("‘{}’ 연결을 삭제할까요?", self.profile(id).map_or("", |p| p.name.as_str())),
                format!("{}에 저장된 비밀번호도 함께 삭제되며, 되돌릴 수 없습니다.", platform::SECRET_STORE_SHORT),
                "삭제",
                true,
            ),
            Dialog::ForgetKeys(id) => {
                let hosts = self
                    .profile(id)
                    .map(|p| if p.use_jump { format!("{}, {}", p.jump.host, p.target.host) } else { p.target.host.clone() })
                    .unwrap_or_default();
                (
                    "호스트 키를 초기화할까요?".to_owned(),
                    format!(
                        "{hosts}에 대해 저장된 SSH 호스트 키와 RDP 인증서를 지웁니다.\n\
                         서버를 다시 설치했거나 인증서를 교체한 경우에만 사용하세요. \
                         이유 없이 키가 바뀌었다면 중간자 공격일 수 있습니다."
                    ),
                    "초기화",
                    true,
                )
            }
            Dialog::Discard(_) => (
                "저장하지 않은 변경 사항이 있습니다".to_owned(),
                "변경 사항을 버리고 이동할까요?".to_owned(),
                "버리기",
                true,
            ),
        };

        let mut confirmed = false;
        let mut cancelled = false;
        // The backdrop fades on its own layer, so the dialog can grow into place without it.
        let backdrop = ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, Id::unique("dialog_backdrop")));
        backdrop.rect_filled(ctx.content_rect(), 0.0, egui::Color32::from_black_alpha(DIALOG_BACKDROP_ALPHA).gamma_multiply(shown));
        let modal = egui::Modal::new(Id::unique("dialog")).backdrop_color(egui::Color32::TRANSPARENT).show(ctx, |ui| {
            let pal = palette(ui);
            ui.set_width(380.0);
            ui.label(RichText::new(&title).size(16.0).bold().color(pal.text));
            ui.add_space(4.0);
            ui.label(RichText::new(&body).color(pal.text_weak));
            ui.add_space(14.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let button = if danger {
                    widgets::danger_button(ui, confirm)
                } else {
                    widgets::primary_button(ui, confirm)
                };
                if button.clicked() {
                    confirmed = true;
                }
                // Enter is the safe answer, not the dangerous one (Esc and outside clicks too).
                if ui.button("취소").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    cancelled = true;
                }
            });
        });
        if modal.should_close() {
            cancelled = true;
        }
        let scale = 1.0 - (1.0 - DIALOG_START_SCALE) * (1.0 - shown) * motion::travel();
        let center = modal.response.rect.center().to_vec2();
        let grow = egui::emath::TSTransform::from_translation(center)
            * egui::emath::TSTransform::from_scaling(scale)
            * egui::emath::TSTransform::from_translation(-center);
        ctx.set_transform_layer(modal.response.layer_id, grow);

        if confirmed {
            self.dialog = None;
            match dialog {
                Dialog::Delete(id) => self.delete(ctx, id),
                Dialog::ForgetKeys(id) => self.forget_keys(ctx, id),
                Dialog::Discard(nav) => self.apply_nav(nav),
            }
        } else if cancelled {
            self.dialog = None;
        }
    }

    /// Toasts stack up from the bottom-right corner. Each one rises and fades in, fades out when it
    /// expires or is clicked, and the others glide into the space it leaves.
    fn draw_toasts(&mut self, ctx: &egui::Context) {
        const GAP: f32 = 8.0;
        const RISE: f32 = 16.0;
        let now = ctx.input(|i| i.time);
        let leave_secs = motion::BASE;
        for toast in &mut self.toasts {
            if toast.leaving.is_none() && toast.until <= now {
                toast.leaving = Some(now);
            }
        }
        // A faded-out toast is gone; without motion it goes at once.
        self.toasts
            .retain(|t| t.leaving.is_none_or(|left| motion::since(ctx, left, leave_secs) < 1.0));
        if self.toasts.is_empty() {
            return;
        }
        let next = self.toasts.iter().filter(|t| t.leaving.is_none()).map(|t| t.until).fold(f64::INFINITY, f64::min);
        if next.is_finite() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((next - now).max(0.05)));
        }

        let mut stack = 0.0;
        let mut close = None;
        for (i, toast) in self.toasts.iter().enumerate().rev() {
            let appear = motion::since(ctx, toast.shown, motion::SLOW);
            let leave = toast.leaving.map_or(0.0, |left| motion::since(ctx, left, leave_secs));
            let y = motion::value(ctx, Id::unique(("toast_y", toast.id)), stack, motion::movement(motion::BASE));
            let rise = (1.0 - appear) * RISE * motion::travel();
            let area = egui::Area::new(Id::unique(("toast", toast.id)))
                .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-20.0, -20.0 - y + rise))
                .order(egui::Order::Foreground)
                .fade_in(false)
                .interactable(toast.leaving.is_none())
                .show(ctx, |ui| {
                    ui.multiply_opacity(appear * (1.0 - leave));
                    let pal = palette(ui);
                    let (color, icon) = match toast.kind {
                        ToastKind::Success => (pal.success, widgets::Icon::Success),
                        ToastKind::Info => (pal.accent, widgets::Icon::Info),
                        ToastKind::Error => (pal.danger, widgets::Icon::Alert),
                    };
                    egui::Frame::new()
                        .fill(pal.surface)
                        .stroke(Stroke::new(1.0, pal.border))
                        .corner_radius(CornerRadius::same(theme::FLOAT_RADIUS))
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(40),
                        })
                        .inner_margin(Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);
                            ui.horizontal(|ui| {
                                ui.add(icon.image(widgets::ICON_MD, color));
                                ui.add(egui::Label::new(RichText::new(&toast.text).color(pal.text)).wrap());
                            });
                        })
                        .response
                        .interact(Sense::click())
                });
            if area.inner.clicked() {
                close = Some(i);
            }
            if toast.leaving.is_none() {
                stack += area.response.rect.height() + GAP;
            }
        }
        if let Some(i) = close {
            self.toasts[i].leaving = Some(now);
            ctx.request_repaint();
        }
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        #[cfg(any(target_os = "macos", windows))]
        if let Some(tray) = &self.tray {
            tray.hide_on_close(ui.ctx());
        }
        self.draw(ui);
    }
}

/// Opens `path` in the platform file manager, so the shared folder is one click away.
fn reveal_in_file_manager(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    if let Err(error) = std::process::Command::new(program).arg(path).spawn() {
        tracing::warn!(%error, path = %path.display(), "Unable to open the file manager");
    }
}

/// Runs one update check and records a newer release; returns the toast for a check the user asked for.
fn check_update_once(state: &Mutex<UpdateState>, asked: bool) -> Option<(ToastKind, String)> {
    let notice = match update::check() {
        Ok(Some(release)) => {
            let notice = (ToastKind::Info, format!("새 버전이 있습니다: {}", release.version));
            offer_release(&mut state.lock().unwrap_or_else(PoisonError::into_inner), release);
            notice
        }
        Ok(None) => (ToastKind::Success, format!("최신 버전입니다 ({})", env!("CARGO_PKG_VERSION"))),
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "Update check failed");
            (ToastKind::Error, format!("업데이트를 확인하지 못했습니다: {error:#}"))
        }
    };
    asked.then_some(notice)
}

/// Shows `release` in the sidebar, unless an install is under way or that version is already shown
/// (which keeps a failed install's error visible).
fn offer_release(state: &mut UpdateState, release: Release) {
    let shown = match state {
        UpdateState::None => None,
        UpdateState::Installing(_) => return,
        UpdateState::Available(r) | UpdateState::Failed(r, _) => Some(r),
    };
    if shown.is_none_or(|r| r.version != release.version) {
        *state = UpdateState::Available(release);
    }
}

/// One profile in the sidebar: its monogram, the name with the session state right after it, and
/// the address with the protocol as a small badge. The selected row shows a chevron.
fn list_row(ui: &mut Ui, pal: &Palette, p: &Profile, selected: bool, state: Option<&State>) -> egui::Response {
    const HEIGHT: f32 = 60.0;
    const TILE: f32 = 36.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEIGHT), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, selected, &p.name));
    let ctx = ui.ctx().clone();

    // The selected row's background is the sliding highlight (`paint_selection`).
    let hover = motion::toggle(&ctx, response.id.with("hover"), response.hovered() && !selected, motion::FAST);
    if hover > 0.0 {
        ui.painter().rect_filled(rect, CornerRadius::same(ROW_RADIUS), pal.subtle.gamma_multiply(hover));
    }

    let tile = Rect::from_min_size(Pos2::new(rect.left() + 12.0, rect.center().y - TILE / 2.0), Vec2::splat(TILE));
    widgets::paint_monogram(ui, tile, &p.name, selected);

    // Shape differs per state, so it does not rely on color alone.
    #[derive(Clone, Copy)]
    enum Mark {
        Dot,
        Spinner,
        Alert,
    }
    let mark = state.map(|s| {
        let (color, label) = state_badge(pal, s);
        let mark = match s {
            State::Connected => Mark::Dot,
            State::Failed(_) => Mark::Alert,
            _ => Mark::Spinner,
        };
        (color, label, mark)
    });
    // A session reaching its end state pops or fades its mark in, so the change catches the eye.
    let connected = motion::pop(&ctx, Id::unique(("mark_connected", p.id)), matches!(state, Some(State::Connected)), motion::SLOW);
    let failed = motion::toggle(&ctx, Id::unique(("mark_failed", p.id)), matches!(state, Some(State::Failed(_))), motion::BASE);

    const MARK_ROOM: f32 = 22.0;
    let text_left = tile.right() + 12.0;
    let text_right = rect.right() - if selected { 30.0 } else { 12.0 };
    let max_w = (text_right - text_left).max(20.0);
    let name = single_line(ui, &p.name, FontId::new(14.0, bold_family()), pal.text, max_w - MARK_ROOM);
    let mono = FontId::monospace(11.5);
    let protocol = protocol_name(p.protocol);
    let badge = widgets::tag_size(ui, protocol, &FontId::monospace(10.0));
    let address = format!("{}:{}", p.target.host, p.target.port);
    let address = single_line(ui, &address, mono, pal.text_weak, max_w - badge.x - 8.0);
    let total = name.size().y + 5.0 + address.size().y;
    let top = rect.center().y - total / 2.0;
    let name_size = name.size();
    ui.painter().galley(Pos2::new(text_left, top), name, pal.text);
    let line2 = top + name_size.y + 5.0;
    let address_size = address.size();
    ui.painter().galley(Pos2::new(text_left, line2), address, pal.text_weak);
    let badge_rect = Rect::from_min_size(Pos2::new(text_left + address_size.x + 8.0, line2 + (address_size.y - badge.y) / 2.0), badge);
    let (badge_text, badge_fill, badge_stroke) = if selected {
        (pal.accent, pal.accent_soft, pal.accent.gamma_multiply(0.3))
    } else {
        (pal.text_weak, pal.subtle, pal.subtle)
    };
    widgets::paint_tag(ui, badge_rect, protocol, FontId::monospace(10.0), badge_text, badge_fill, badge_stroke);

    if let Some((color, label, mark)) = mark {
        let c = Pos2::new(text_left + name_size.x + 11.0, top + name_size.y / 2.0);
        match mark {
            Mark::Dot => {
                ui.painter().circle_filled(c, 4.0 * connected, color);
            }
            Mark::Spinner => {
                egui::Spinner::new().color(color).paint_at(ui, Rect::from_center_size(c, Vec2::splat(12.0)));
            }
            Mark::Alert => {
                widgets::paint_icon(ui, widgets::Icon::Alert, c, 15.0, color.gamma_multiply(failed));
            }
        }
        let hit = Rect::from_center_size(c, Vec2::splat(18.0));
        if ui.rect_contains_pointer(hit) {
            response.clone().on_hover_text(label);
        }
    }
    if selected {
        widgets::paint_icon(ui, widgets::Icon::Chevron, Pos2::new(rect.right() - 16.0, rect.center().y), widgets::ICON_SM, pal.text_weak);
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

/// The selected row's background. It slides to a newly selected row and fades out where it was
/// when nothing in the list is selected. Positions are relative to the list, so scrolling moves
/// it with the rows instead of animating.
fn paint_selection(ui: &Ui, pal: &Palette, slot: egui::layers::ShapeIdx, row: Option<Rect>) {
    let ctx = ui.ctx();
    let origin = Vec2::new(0.0, ui.min_rect().top());
    let shown = motion::toggle(ctx, Id::unique("selection_shown"), row.is_some(), motion::FAST);
    let last = Id::unique("selection_row");
    let target = match row {
        Some(row) => {
            let target = row.translate(-origin);
            ctx.data_mut(|d| d.insert_temp(last, target));
            target
        }
        None => match ctx.data(|d| d.get_temp::<Rect>(last)) {
            Some(target) => target,
            None => return,
        },
    };
    let top = motion::value(ctx, Id::unique("selection_top"), target.top(), motion::movement(motion::BASE));
    if shown > 0.0 {
        let rect = Rect::from_min_size(Pos2::new(target.left(), top), target.size()).translate(origin);
        let fill = pal.surface.lerp_to_gamma(pal.accent_soft, 0.7).gamma_multiply(shown);
        let stroke = Stroke::new(1.0, pal.accent.gamma_multiply(0.5 * shown));
        let shape = egui::epaint::RectShape::new(rect, CornerRadius::same(ROW_RADIUS), fill, stroke, egui::StrokeKind::Inside);
        ui.painter().set(slot, shape);
    }
}

/// Corner radius of sidebar rows and their highlight.
const ROW_RADIUS: u8 = 10;

fn single_line(ui: &Ui, text: &str, font: FontId, color: egui::Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(font, color));
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
    ui.painter().layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::{UpdateState, offer_release};
    use crate::update::Release;

    fn shown_version(state: &UpdateState) -> Option<&str> {
        match state {
            UpdateState::None => None,
            UpdateState::Available(r) | UpdateState::Installing(r) | UpdateState::Failed(r, _) => Some(&r.version),
        }
    }

    #[test]
    fn a_found_release_is_offered() {
        let mut state = UpdateState::None;
        offer_release(&mut state, Release::for_test("0.2.0"));
        assert!(matches!(state, UpdateState::Available(_)));
        assert_eq!(shown_version(&state), Some("0.2.0"));
    }

    #[test]
    fn a_later_release_replaces_the_offer() {
        let mut state = UpdateState::Failed(Release::for_test("0.2.0"), "network".to_owned());
        offer_release(&mut state, Release::for_test("0.3.0"));
        assert!(matches!(state, UpdateState::Available(_)));
        assert_eq!(shown_version(&state), Some("0.3.0"));
    }

    #[test]
    fn the_same_release_keeps_a_failed_install_visible() {
        let mut state = UpdateState::Failed(Release::for_test("0.2.0"), "network".to_owned());
        offer_release(&mut state, Release::for_test("0.2.0"));
        assert!(matches!(state, UpdateState::Failed(..)));
    }

    #[test]
    fn an_install_in_progress_is_not_interrupted() {
        let mut state = UpdateState::Installing(Release::for_test("0.2.0"));
        offer_release(&mut state, Release::for_test("0.3.0"));
        assert!(matches!(state, UpdateState::Installing(_)));
        assert_eq!(shown_version(&state), Some("0.2.0"));
    }
}
