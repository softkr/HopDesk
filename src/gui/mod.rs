//! Launcher window (`hopdesk` without arguments): connection list, details, add/edit.
//!
//! Each connection runs as a child process (`hopdesk connect <name>`): the RDP viewer owns its own
//! winit event loop, and winit allows only one event loop per process.

mod detail;
mod form;
mod platform;
mod sessions;
mod theme;
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

/// macOS draws icons on Apple's grid (with margin); elsewhere the tile fills the square.
#[cfg(target_os = "macos")]
const WINDOW_ICON: &[u8] = include_bytes!("../../assets/icon/icon-256.png");
#[cfg(not(target_os = "macos"))]
const WINDOW_ICON: &[u8] = include_bytes!("../../assets/icon/icon-full-256.png");

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
    text: String,
    kind: ToastKind,
    until: f64,
}

pub struct Launcher {
    store: ProfileStore,
    selected: Option<Uuid>,
    view: View,
    search: String,
    dialog: Option<Dialog>,
    toasts: Vec<Toast>,
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
            toasts.push(Toast {
                text: format!("설정 파일을 읽지 못했습니다: {e:#}"),
                kind: ToastKind::Error,
                until: f64::INFINITY,
            });
            ProfileStore::default()
        });
        // Before any window opens: the Keychain may ask for access once per stored secret.
        if let Err(e) = store.migrate_secrets() {
            toasts.push(Toast {
                text: format!("저장된 비밀번호를 옮기지 못했습니다: {e:#}"),
                kind: ToastKind::Error,
                until: f64::INFINITY,
            });
        }
        let mut launcher = Self::with_store(store, Sessions::default());
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
        self.toasts.push(Toast {
            text: text.into(),
            kind,
            until: now + secs,
        });
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
            })
            .collect()
    }

    // --- actions ------------------------------------------------------------------------------

    fn connect(&mut self, ctx: &egui::Context, id: Uuid) {
        let Some(name) = self.profile(id).map(|p| p.name.clone()) else { return };
        self.sessions.dismiss(id);
        if let Err(e) = self.sessions.start(id, &name, ctx) {
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
            ui.label(RichText::new("HopDesk").size(theme::TITLE_SIZE).bold().color(pal.text));
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
            let search = widgets::text_input(ui, &mut self.search, &format!("검색  {}", platform::shortcut("F")), ui.available_width(), false)
                .id(Id::unique("search"));
            let response = ui.add(search);
            if response.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.search.clear();
                response.surrender_focus();
            }
            ui.add_space(6.0);
        }

        self.update_card(ui);

        let list: Vec<Profile> = self.filtered().into_iter().cloned().collect();
        let mut clicked = None;
        let mut double = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for p in &list {
                let state = self.sessions.state(p.id);
                let selected = self.selected == Some(p.id) && matches!(self.view, View::Detail)
                    || matches!(&self.view, View::Edit { form, .. } if form.id == Some(p.id));
                let response = list_row(ui, &pal, p, selected, state.as_ref());
                if response.clicked() {
                    clicked = Some(p.id);
                }
                if response.double_clicked() {
                    double = Some(p.id);
                }
            }
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

    fn main(&mut self, ui: &mut Ui) {
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
                match detail::ui(ui, &profile, state.as_ref(), &log, &shared_folder) {
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
                    RichText::new(format!("SSH 점프 호스트를 거쳐 원격 Windows PC에 안전하게 접속합니다.\n비밀번호는 {}에 저장됩니다.", platform::SECRET_STORE))
                        .line_height(Some(22.0))
                        .color(pal.text_weak),
                );
                ui.add_space(16.0);
                if widgets::large_primary_button(ui, "새 연결 만들기", 200.0).clicked() {
                    self.navigate(Nav::New);
                }
            } else {
                ui.label(RichText::new("왼쪽 목록에서 연결을 선택하세요").size(theme::TITLE_SIZE).color(pal.text_weak));
            }
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
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
        let modal = egui::Modal::new(Id::unique("dialog")).show(ctx, |ui| {
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

    fn draw_toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        self.toasts.retain(|t| t.until > now);
        if self.toasts.is_empty() {
            return;
        }
        let next = self.toasts.iter().map(|t| t.until).fold(f64::INFINITY, f64::min);
        if next.is_finite() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((next - now).max(0.05)));
        }

        let mut close = None;
        egui::Area::new(Id::unique("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-20.0, -20.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let pal = palette(ui);
                for (i, toast) in self.toasts.iter().enumerate() {
                    let color = match toast.kind {
                        ToastKind::Success => pal.success,
                        ToastKind::Info => pal.accent,
                        ToastKind::Error => pal.danger,
                    };
                    let frame = egui::Frame::new()
                        .fill(pal.surface)
                        .stroke(Stroke::new(1.0, pal.border))
                        .corner_radius(CornerRadius::same(theme::FLOAT_RADIUS))
                        .shadow(egui::Shadow {
                            offset: [0, 4],
                            blur: 16,
                            spread: 0,
                            color: egui::Color32::from_black_alpha(40),
                        })
                        .inner_margin(Margin::symmetric(14, 10));
                    let response = frame
                        .show(ui, |ui| {
                            ui.set_max_width(360.0);
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(Vec2::new(4.0, 18.0), Sense::hover());
                                ui.painter().rect_filled(rect, CornerRadius::same(2), color);
                                ui.add(egui::Label::new(RichText::new(&toast.text).color(pal.text)).wrap());
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if response.clicked() {
                        close = Some(i);
                    }
                    ui.add_space(6.0);
                }
            });
        if let Some(i) = close {
            self.toasts.remove(i);
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

/// One profile in the sidebar: avatar, name, target, session status.
fn list_row(ui: &mut Ui, pal: &Palette, p: &Profile, selected: bool, state: Option<&State>) -> egui::Response {
    let height = 56.0;
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::ListBoxOption, true, selected, &p.name));

    if selected {
        ui.painter().rect_filled(rect, CornerRadius::same(8), pal.accent_soft);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(8), pal.subtle);
    }

    let avatar_r = 17.0;
    let avatar_c = Pos2::new(rect.left() + 10.0 + avatar_r, rect.center().y);
    widgets::paint_avatar(ui, avatar_c, avatar_r, &p.name);

    // Shape differs per state, so it does not rely on color alone.
    #[derive(Clone, Copy)]
    enum Mark {
        Dot,
        Ring,
        Alert,
    }
    let dot = state.map(|s| match s {
        State::Connected => (pal.success, "연결됨", Mark::Dot),
        State::Failed(_) => (pal.danger, "연결 실패", Mark::Alert),
        State::Reconnecting => (pal.warning, "다시 연결하는 중", Mark::Ring),
        _ => (pal.accent, "연결하는 중", Mark::Ring),
    });
    let text_left = avatar_c.x + avatar_r + 10.0;
    let text_right = rect.right() - if dot.is_some() { 26.0 } else { 10.0 };
    let max_w = (text_right - text_left).max(20.0);

    let name = single_line(ui, &p.name, FontId::new(14.0, bold_family()), pal.text, max_w);
    let sub_text = format!("{}@{}", p.target.user, p.target.host);
    let sub = single_line(ui, &sub_text, FontId::proportional(13.0), pal.text_weak, max_w);
    let total = name.size().y + 2.0 + sub.size().y;
    let top = rect.center().y - total / 2.0;
    ui.painter().galley(Pos2::new(text_left, top), name.clone(), pal.text);
    ui.painter().galley(Pos2::new(text_left, top + name.size().y + 2.0), sub, pal.text_weak);

    if let Some((color, label, mark)) = dot {
        let c = Pos2::new(rect.right() - 15.0, rect.center().y);
        match mark {
            Mark::Dot => {
                ui.painter().circle_filled(c, 5.0, color);
            }
            Mark::Ring => {
                ui.painter().circle_stroke(c, 4.5, Stroke::new(2.0, color));
            }
            Mark::Alert => {
                ui.painter().circle_filled(c, 7.0, color);
                let font = FontId::new(11.0, bold_family());
                ui.painter().text(c, Align2::CENTER_CENTER, "!", font, pal.surface);
            }
        }
        let hit = Rect::from_center_size(c, Vec2::splat(18.0));
        if ui.rect_contains_pointer(hit) {
            response.clone().on_hover_text(label);
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

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
