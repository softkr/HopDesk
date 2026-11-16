//! Add / edit connection form: state, validation and view.

use std::collections::BTreeMap;
use std::path::PathBuf;

use eframe::egui::{self, RichText, Ui};
use uuid::Uuid;

use super::theme::{Bold as _, palette};
use super::widgets::{self, card, field, section_title, text_input};
use crate::profile::{DisplayMode, JumpHost, Profile, ProfileStore, Protocol, Secret, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Name,
    JumpHost,
    JumpPort,
    JumpUser,
    KeyPath,
    JumpSecret,
    TargetHost,
    TargetPort,
    TargetUser,
    TargetKeyPath,
    TargetSecret,
    Resolution,
}

pub type Errors = BTreeMap<Field, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Auth {
    Password,
    Key,
}

/// Fixed resolutions offered in the list (common Windows App presets).
const RESOLUTIONS: [(u16, u16); 23] = [
    (640, 480),
    (800, 600),
    (1024, 600),
    (1024, 768),
    (1152, 864),
    (1280, 720),
    (1280, 800),
    (1280, 960),
    (1280, 1024),
    (1360, 768),
    (1366, 768),
    (1400, 1050),
    (1440, 900),
    (1600, 900),
    (1600, 1200),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2048, 1536),
    (2560, 1440),
    (2560, 1600),
    (2880, 1800),
    (3840, 2160),
];

#[derive(Clone, PartialEq)]
pub struct Form {
    /// `None` while creating a new profile.
    pub id: Option<Uuid>,
    stored_auth: Option<Auth>,
    /// `use_jump` of the stored profile; `None` while creating a new profile.
    stored_use_jump: Option<bool>,
    name: String,
    use_jump: bool,
    jump_host: String,
    jump_port: String,
    jump_user: String,
    auth: Auth,
    key_path: String,
    jump_secret: String,
    /// Existing profile: the stored secret is kept unless the user chooses to change it.
    change_jump_secret: bool,
    /// Chosen when the profile is created; fixed afterwards, because the stored target secret
    /// means something else for each protocol.
    protocol: Protocol,
    target_host: String,
    target_port: String,
    target_user: String,
    /// RDP only.
    domain: String,
    /// SSH only: how to log on to the target.
    target_auth: Auth,
    stored_target_auth: Option<Auth>,
    target_key_path: String,
    /// Password on the target, or the key passphrase.
    target_secret: String,
    change_target_secret: bool,
    width: String,
    height: String,
    display: DisplayMode,
    custom_resolution: bool,
    /// Errors from the last save attempt; refreshed live afterwards.
    errors: Errors,
    attempted: bool,
    /// Focus the first field once when the form opens.
    focus_pending: bool,
}

pub enum Action {
    None,
    Save,
    Cancel,
}

impl Form {
    pub fn new_profile() -> Self {
        Self {
            id: None,
            stored_auth: None,
            stored_use_jump: None,
            name: String::new(),
            use_jump: true,
            jump_host: String::new(),
            jump_port: "22".into(),
            jump_user: String::new(),
            auth: Auth::Password,
            key_path: String::new(),
            jump_secret: String::new(),
            change_jump_secret: true,
            protocol: Protocol::Rdp,
            target_host: String::new(),
            target_port: Protocol::Rdp.default_port().to_string(),
            target_user: String::new(),
            domain: String::new(),
            target_auth: Auth::Password,
            stored_target_auth: None,
            target_key_path: String::new(),
            target_secret: String::new(),
            change_target_secret: true,
            width: "1920".into(),
            height: "1080".into(),
            display: DisplayMode::default(),
            custom_resolution: false,
            errors: Errors::new(),
            attempted: false,
            focus_pending: true,
        }
    }

    pub fn from_profile(p: &Profile) -> Self {
        let auth = if p.jump.key_path.is_some() { Auth::Key } else { Auth::Password };
        let target_auth = if p.target.key_path.is_some() { Auth::Key } else { Auth::Password };
        Self {
            id: Some(p.id),
            stored_auth: Some(auth),
            stored_use_jump: Some(p.use_jump),
            name: p.name.clone(),
            use_jump: p.use_jump,
            jump_host: p.jump.host.clone(),
            jump_port: p.jump.port.to_string(),
            jump_user: p.jump.user.clone(),
            auth,
            key_path: p.jump.key_path.as_ref().map(|k| display_path(k)).unwrap_or_default(),
            jump_secret: String::new(),
            change_jump_secret: false,
            protocol: p.protocol,
            target_host: p.target.host.clone(),
            target_port: p.target.port.to_string(),
            target_user: p.target.user.clone(),
            domain: p.target.domain.clone().unwrap_or_default(),
            target_auth,
            stored_target_auth: Some(target_auth),
            target_key_path: p.target.key_path.as_ref().map(|k| display_path(k)).unwrap_or_default(),
            target_secret: String::new(),
            change_target_secret: false,
            width: p.width.to_string(),
            height: p.height.to_string(),
            display: p.display,
            custom_resolution: !RESOLUTIONS.contains(&(p.width, p.height)),
            errors: Errors::new(),
            attempted: false,
            focus_pending: false,
        }
    }

    pub fn is_new(&self) -> bool {
        self.id.is_none()
    }

    /// True when the user changed anything compared to `original`.
    pub fn is_dirty(&self, original: &Form) -> bool {
        let mut a = self.clone();
        let mut b = original.clone();
        for f in [&mut a, &mut b] {
            f.errors.clear();
            f.attempted = false;
            f.focus_pending = false;
        }
        a != b
    }

    fn auth_switched(&self) -> bool {
        self.stored_auth.is_some_and(|a| a != self.auth)
    }

    /// The jump host secret must be (re-)entered: new profile, other auth method, or the jump
    /// host was just enabled on a profile that connected directly.
    fn jump_secret_required(&self) -> bool {
        self.is_new() || self.auth_switched() || self.stored_use_jump == Some(false)
    }

    fn target_auth_switched(&self) -> bool {
        self.stored_target_auth.is_some_and(|a| a != self.target_auth)
    }

    /// The target secret must be (re-)entered: new profile, or an SSH target with another auth method.
    fn target_secret_required(&self) -> bool {
        self.is_new() || (self.protocol == Protocol::Ssh && self.target_auth_switched())
    }

    /// SSH targets log on with a key when chosen; RDP always uses the Windows password.
    fn target_uses_key(&self) -> bool {
        self.protocol == Protocol::Ssh && self.target_auth == Auth::Key
    }

    /// Picks the protocol of a new profile; the port follows unless the user typed another one.
    fn set_protocol(&mut self, protocol: Protocol) {
        if self.target_port.trim() == self.protocol.default_port().to_string() {
            self.target_port = protocol.default_port().to_string();
        }
        self.protocol = protocol;
    }

    /// Validates every field and builds the profile.
    pub fn validate(&self, store: &ProfileStore) -> Result<Profile, Errors> {
        let mut errors = Errors::new();
        let mut required = |field: Field, value: &str, message: &str| -> String {
            let v = value.trim();
            if v.is_empty() {
                errors.insert(field, message.to_owned());
            }
            v.to_owned()
        };

        let name = required(Field::Name, &self.name, "이름을 입력하세요");
        // Disabled jump host settings are kept as typed, without validation.
        let (jump_host, jump_user, key_path) = if self.use_jump {
            (
                required(Field::JumpHost, &self.jump_host, "점프 호스트 주소를 입력하세요"),
                required(Field::JumpUser, &self.jump_user, "SSH 사용자 이름을 입력하세요"),
                match self.auth {
                    Auth::Key => Some(required(Field::KeyPath, &self.key_path, "개인 키 파일을 선택하세요")),
                    Auth::Password => None,
                },
            )
        } else {
            let key = self.key_path.trim();
            (
                self.jump_host.trim().to_owned(),
                self.jump_user.trim().to_owned(),
                (self.auth == Auth::Key && !key.is_empty()).then(|| key.to_owned()),
            )
        };
        let noun = super::target_noun(self.protocol);
        let target_host = required(Field::TargetHost, &self.target_host, &format!("{noun} 주소를 입력하세요"));
        let user_message = match self.protocol {
            Protocol::Rdp => "Windows 사용자 이름을 입력하세요",
            _ => "사용자 이름을 입력하세요",
        };
        let target_user = required(Field::TargetUser, &self.target_user, user_message);
        let target_key_path = self
            .target_uses_key()
            .then(|| required(Field::TargetKeyPath, &self.target_key_path, "개인 키 파일을 선택하세요"));

        if !name.is_empty()
            && store
                .profiles()
                .iter()
                .any(|p| p.name == name && Some(p.id) != self.id)
        {
            errors.insert(Field::Name, "같은 이름의 연결이 이미 있습니다".into());
        }
        let jump_port = match parse_port(&self.jump_port) {
            Some(port) => port,
            None if self.use_jump => {
                errors.insert(Field::JumpPort, "1–65535".into());
                0
            }
            None => 22,
        };
        let target_port = parse_port(&self.target_port).unwrap_or_else(|| {
            errors.insert(Field::TargetPort, "1–65535".into());
            0
        });
        let size = |v: &str| v.trim().parse::<u16>().ok().filter(|n| (640..=8192).contains(n));
        let (width, height) = match (size(&self.width), size(&self.height)) {
            (Some(w), Some(h)) => (w, h),
            _ if self.protocol != Protocol::Rdp => (1920, 1080),
            _ => {
                errors.insert(Field::Resolution, "가로·세로는 640–8192 사이여야 합니다".into());
                (0, 0)
            }
        };

        let needs_jump_secret =
            self.use_jump && self.auth == Auth::Password && (self.jump_secret_required() || self.change_jump_secret);
        if needs_jump_secret && self.jump_secret.is_empty() {
            errors.insert(Field::JumpSecret, "비밀번호를 입력하세요".into());
        }
        // A key passphrase may be empty.
        let needs_target_secret =
            !self.target_uses_key() && (self.target_secret_required() || self.change_target_secret);
        if needs_target_secret && self.target_secret.is_empty() {
            errors.insert(Field::TargetSecret, "비밀번호를 입력하세요".into());
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        let domain = self.domain.trim();
        Ok(Profile {
            id: self.id.unwrap_or_else(Uuid::new_v4),
            name,
            protocol: self.protocol,
            use_jump: self.use_jump,
            jump: JumpHost {
                host: jump_host,
                port: jump_port,
                user: jump_user,
                key_path: key_path.map(|k| expand_tilde(&k)),
            },
            target: Target {
                host: target_host,
                port: target_port,
                user: target_user,
                domain: (self.protocol == Protocol::Rdp && !domain.is_empty()).then(|| domain.to_owned()),
                key_path: target_key_path.map(|k| expand_tilde(&k)),
            },
            width,
            height,
            display: self.display,
        })
    }

    /// Writes changed secrets to the credential store, then saves the profile list.
    pub fn commit(&self, profile: &Profile, store: &mut ProfileStore) -> anyhow::Result<()> {
        let write_jump = self.use_jump && (self.jump_secret_required() || self.change_jump_secret);
        let write_target = self.target_secret_required() || self.change_target_secret;
        store.upsert(profile.clone())?;
        if write_jump {
            // A key without passphrase stores an empty secret.
            profile.set_secret(Secret::Jump, &self.jump_secret)?;
        }
        if write_target {
            profile.set_secret(Secret::Target, &self.target_secret)?;
        }
        store.save()
    }

    pub fn set_errors(&mut self, errors: Errors) {
        self.errors = errors;
        self.attempted = true;
    }

    /// Draws the form. `store` is used for live re-validation after a failed save.
    pub fn ui(&mut self, ui: &mut Ui, store: &ProfileStore) -> Action {
        let pal = palette(ui);
        let mut action = Action::None;

        if self.attempted {
            self.errors = self.validate(store).err().unwrap_or_default();
        }

        // Header
        ui.add_space(20.0);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.label(RichText::new(if self.is_new() { "새 연결" } else { "연결 편집" }).size(super::theme::HEADING_SIZE).bold());
                ui.label(
                    RichText::new("원격 PC나 서버에 직접 또는 SSH 점프 호스트를 거쳐 접속합니다.").color(pal.text_weak),
                );
            });
        });
        ui.add_space(12.0);

        // Footer first so it stays pinned at the bottom.
        egui::Panel::bottom("form_footer")
            .show_separator_line(true)
            .frame(egui::Frame::new().fill(pal.bg).inner_margin(egui::Margin::symmetric(4, 12)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !self.errors.is_empty() {
                        ui.label(
                            RichText::new(format!("입력 항목 {}개를 확인하세요", self.errors.len())).color(pal.danger),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let save = if self.is_new() { "연결 추가" } else { "저장" };
                        if widgets::primary_button(ui, save).on_hover_text(super::platform::shortcut("S")).clicked() {
                            action = Action::Save;
                        }
                        if ui.button("취소").on_hover_text("Esc").clicked() {
                            action = Action::Cancel;
                        }
                    });
                });
            });

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            let full = (ui.available_width() - 40.0).max(240.0);
            let errors = self.errors.clone();
            let err = |f: Field| errors.get(&f).map(String::as_str);

            card(ui, |ui| {
                let edit = text_input(ui, &mut self.name, "예: 사무실 PC", full, err(Field::Name).is_some());
                let response = field(ui, "연결 이름", err(Field::Name), |ui| ui.add(edit));
                if std::mem::take(&mut self.focus_pending) {
                    response.request_focus();
                }
                field(ui, "종류", None, |ui| {
                    if self.is_new() {
                        let mut protocol = self.protocol;
                        let options = [(Protocol::Rdp, "원격 데스크톱 (RDP)"), (Protocol::Ssh, "터미널 (SSH)")];
                        if widgets::segmented(ui, &mut protocol, &options) {
                            self.set_protocol(protocol);
                        }
                    } else {
                        ui.label(protocol_label(self.protocol));
                    }
                });
            });

            card(ui, |ui| {
                let description = format!("{}로 들어가는 관문 역할을 하는 SSH 서버입니다.", super::target_noun(self.protocol));
                section_title(ui, "SSH 점프 호스트", Some(description.as_str()));
                field(ui, "사용", None, |ui| {
                    widgets::segmented(ui, &mut self.use_jump, &[(true, "사용"), (false, "사용 안 함 (직접 접속)")]);
                });
                if !self.use_jump {
                    let text = format!(
                        "점프 호스트 없이 {}에 바로 접속합니다. 입력해 둔 설정은 보관됩니다.",
                        super::target_noun(self.protocol)
                    );
                    ui.label(RichText::new(text).color(pal.text_weak));
                    return;
                }
                host_port_row(
                    ui,
                    full,
                    (&mut self.jump_host, "예: bastion.example.com", err(Field::JumpHost)),
                    (&mut self.jump_port, err(Field::JumpPort)),
                );
                let edit = text_input(ui, &mut self.jump_user, "예: ubuntu", full, err(Field::JumpUser).is_some());
                field(ui, "사용자 이름", err(Field::JumpUser), |ui| ui.add(edit));

                field(ui, "인증 방식", None, |ui| {
                    if widgets::segmented(ui, &mut self.auth, &[(Auth::Password, "비밀번호"), (Auth::Key, "개인 키")]) {
                        self.jump_secret.clear();
                        self.change_jump_secret = self.is_new() || self.auth_switched();
                    }
                });

                if self.auth == Auth::Key {
                    field(ui, "개인 키 파일", err(Field::KeyPath), |ui| {
                        input_row(ui, full, |ui| {
                            if ui.button("찾아보기...").clicked()
                                && let Some(path) = pick_key_file(&self.key_path)
                            {
                                self.key_path = display_path(&path);
                            }
                            let invalid = err(Field::KeyPath).is_some();
                            let w = ui.available_width();
                            ui.add(text_input(ui, &mut self.key_path, "예: ~/.ssh/id_ed25519", w, invalid));
                        });
                    });
                }

                let (label, hint) = match self.auth {
                    Auth::Password => ("비밀번호", ""),
                    Auth::Key => ("키 암호", "암호가 없으면 비워 두세요"),
                };
                let force = self.jump_secret_required();
                secret_field(
                    ui,
                    full,
                    label,
                    hint,
                    &mut self.jump_secret,
                    &mut self.change_jump_secret,
                    force,
                    err(Field::JumpSecret),
                );
            });

            card(ui, |ui| {
                let title = match self.protocol {
                    Protocol::Rdp => "원격 PC (RDP)",
                    _ => "원격 서버 (SSH)",
                };
                let noun = super::target_noun(self.protocol);
                let example = match self.protocol {
                    Protocol::Rdp => "pc.example.com",
                    _ => "server.example.com",
                };
                let description = if self.use_jump {
                    format!("{noun} 주소는 점프 호스트에서 접근하는 기준으로 입력하세요. 예: 10.0.0.5")
                } else {
                    format!("이 기기에서 바로 접근할 수 있는 주소를 입력하세요. 예: {example}")
                };
                section_title(ui, title, Some(description.as_str()));
                host_port_row(
                    ui,
                    full,
                    (&mut self.target_host, "예: 10.0.0.5", err(Field::TargetHost)),
                    (&mut self.target_port, err(Field::TargetPort)),
                );
                if self.protocol == Protocol::Rdp {
                    ui.horizontal(|ui| {
                        let half = (full - 8.0) / 2.0;
                        let edit =
                            text_input(ui, &mut self.target_user, "예: Administrator", half, err(Field::TargetUser).is_some());
                        field(ui, "Windows 사용자 이름", err(Field::TargetUser), |ui| ui.add(edit));
                        let edit = text_input(ui, &mut self.domain, "선택 사항", half, false);
                        field(ui, "도메인", None, |ui| ui.add(edit));
                    });
                    let force = self.is_new();
                    secret_field(
                        ui,
                        full,
                        "Windows 비밀번호",
                        "",
                        &mut self.target_secret,
                        &mut self.change_target_secret,
                        force,
                        err(Field::TargetSecret),
                    );
                    return;
                }

                let edit = text_input(ui, &mut self.target_user, "예: ubuntu", full, err(Field::TargetUser).is_some());
                field(ui, "사용자 이름", err(Field::TargetUser), |ui| ui.add(edit));
                field(ui, "인증 방식", None, |ui| {
                    let options = [(Auth::Password, "비밀번호"), (Auth::Key, "개인 키")];
                    if widgets::segmented(ui, &mut self.target_auth, &options) {
                        self.target_secret.clear();
                        self.change_target_secret = self.is_new() || self.target_auth_switched();
                    }
                });
                if self.target_auth == Auth::Key {
                    field(ui, "개인 키 파일", err(Field::TargetKeyPath), |ui| {
                        input_row(ui, full, |ui| {
                            if ui.button("찾아보기...").clicked()
                                && let Some(path) = pick_key_file(&self.target_key_path)
                            {
                                self.target_key_path = display_path(&path);
                            }
                            let invalid = err(Field::TargetKeyPath).is_some();
                            let w = ui.available_width();
                            ui.add(text_input(ui, &mut self.target_key_path, "예: ~/.ssh/id_ed25519", w, invalid));
                        });
                    });
                }
                let (label, hint) = match self.target_auth {
                    Auth::Password => ("비밀번호", ""),
                    Auth::Key => ("키 암호", "암호가 없으면 비워 두세요"),
                };
                let force = self.target_secret_required();
                secret_field(
                    ui,
                    full,
                    label,
                    hint,
                    &mut self.target_secret,
                    &mut self.change_target_secret,
                    force,
                    err(Field::TargetSecret),
                );
            });

            // Screen settings apply to the remote desktop only.
            if self.protocol == Protocol::Rdp {
                card(ui, |ui| {
                    section_title(ui, "화면", None);
                    field(ui, "해상도", err(Field::Resolution), |ui| {
                        ui.horizontal(|ui| {
                            let fixed = self.display == DisplayMode::Fixed;
                            let custom = fixed && self.custom_resolution;
                            let selected = if !fixed {
                                self.display.label().to_owned()
                            } else if custom {
                                "직접 입력".to_owned()
                            } else {
                                format!("{} × {}", self.width.trim(), self.height.trim())
                            };
                            egui::ComboBox::from_id_salt("resolution")
                                .selected_text(selected)
                                .width(180.0)
                                .show_ui(ui, |ui| {
                                    for mode in [DisplayMode::Fullscreen, DisplayMode::Maximized] {
                                        if ui.selectable_label(self.display == mode, mode.label()).clicked() {
                                            self.display = mode;
                                        }
                                    }
                                    for (w, h) in RESOLUTIONS {
                                        let is = fixed
                                            && !self.custom_resolution
                                            && self.width.trim() == w.to_string()
                                            && self.height.trim() == h.to_string();
                                        if ui.selectable_label(is, format!("{w} × {h}")).clicked() {
                                            self.width = w.to_string();
                                            self.height = h.to_string();
                                            self.display = DisplayMode::Fixed;
                                            self.custom_resolution = false;
                                        }
                                    }
                                    if ui.selectable_label(custom, "직접 입력").clicked() {
                                        self.display = DisplayMode::Fixed;
                                        self.custom_resolution = true;
                                    }
                                });
                            if custom {
                                let invalid = err(Field::Resolution).is_some();
                                ui.add(text_input(ui, &mut self.width, "가로", 70.0, invalid));
                                ui.label("×");
                                ui.add(text_input(ui, &mut self.height, "세로", 70.0, invalid));
                            }
                            if self.display == DisplayMode::Fullscreen {
                                let shortcut = if cfg!(target_os = "macos") { "⌘⌥Enter" } else { "Ctrl+Alt+Enter" };
                                ui.label(RichText::new(format!("{shortcut}로 창 모드 전환")).small().color(pal.text_weak));
                            }
                        });
                    });
                });
            }

            ui.label(
                RichText::new(format!("비밀번호는 {}에만 저장되며, 설정 파일에는 기록되지 않습니다.", super::platform::SECRET_STORE))
                    .small()
                    .color(pal.text_weak),
            );
            ui.add_space(8.0);
        });

        action
    }
}

fn protocol_label(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Rdp => "원격 데스크톱 (RDP)",
        Protocol::Ssh => "터미널 (SSH)",
        Protocol::Vnc => "원격 데스크톱 (VNC)",
    }
}

fn host_port_row(
    ui: &mut Ui,
    full: f32,
    (host, host_hint, host_err): (&mut String, &str, Option<&str>),
    (port, port_err): (&mut String, Option<&str>),
) {
    ui.horizontal(|ui| {
        let port_w = 80.0;
        let edit = text_input(ui, host, host_hint, full - port_w - 8.0, host_err.is_some());
        field(ui, "주소", host_err, |ui| ui.add(edit));
        let edit = text_input(ui, port, "", port_w, port_err.is_some());
        field(ui, "포트", port_err, |ui| ui.add(edit));
    });
}

/// Password input; for existing profiles it shows "stored in Keychain" until the user opts to change it.
#[allow(clippy::too_many_arguments)]
fn secret_field(
    ui: &mut Ui,
    full: f32,
    label: &str,
    hint: &str,
    value: &mut String,
    change: &mut bool,
    force: bool,
    error: Option<&str>,
) {
    let pal = palette(ui);
    let id = ui.scope_id().with(label);
    field(ui, label, error, |ui| {
        if !*change && !force {
            ui.horizontal(|ui| {
                widgets::status_pill(ui, pal.success, &format!("{}에 저장됨", super::platform::SECRET_STORE_SHORT));
                if ui.button("변경").clicked() {
                    *change = true;
                }
            });
            return;
        }
        input_row(ui, full, |ui| {
            // Right-to-left: buttons first, then the input takes the remaining width.
            let mut shown = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            if !force && ui.button("취소").on_hover_text("저장된 비밀번호 유지").clicked() {
                *change = false;
                value.clear();
            }
            if ui.button(if shown { "숨기기" } else { "보기" }).clicked() {
                shown = !shown;
                ui.data_mut(|d| d.insert_temp(id, shown));
            }
            let w = ui.available_width();
            ui.add(text_input(ui, value, hint, w, error.is_some()).password(!shown));
        });
    });
}

/// Row of exactly `width`, filled right-to-left (trailing buttons, then a flexible input).
fn input_row(ui: &mut Ui, width: f32, add_contents: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(
        egui::Vec2::new(width, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        add_contents,
    );
}

fn parse_port(s: &str) -> Option<u16> {
    s.trim().parse::<u16>().ok().filter(|p| *p > 0)
}

fn display_path(path: &std::path::Path) -> String {
    match directories::BaseDirs::new() {
        Some(dirs) => match path.strip_prefix(dirs.home_dir()) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), directories::BaseDirs::new()) {
        (Some(rest), Some(dirs)) => dirs.home_dir().join(rest),
        _ => path.into(),
    }
}

fn pick_key_file(current: &str) -> Option<PathBuf> {
    let start = if current.trim().is_empty() {
        directories::BaseDirs::new().map(|d| d.home_dir().join(".ssh"))
    } else {
        expand_tilde(current.trim()).parent().map(std::path::Path::to_path_buf)
    };
    let mut dialog = rfd::FileDialog::new().set_title("SSH 개인 키 선택");
    if let Some(dir) = start.filter(|d| d.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_form() -> Form {
        let mut form = Form::new_profile();
        form.name = "build".into();
        form.use_jump = false;
        form.set_protocol(Protocol::Ssh);
        form.target_host = "10.0.0.7".into();
        form.target_user = "ubuntu".into();
        form
    }

    #[test]
    fn the_port_follows_the_protocol_unless_typed() {
        let mut form = Form::new_profile();
        form.set_protocol(Protocol::Ssh);
        assert_eq!(form.target_port, "22");
        form.target_port = "2222".into();
        form.set_protocol(Protocol::Rdp);
        assert_eq!(form.target_port, "2222");
    }

    #[test]
    fn an_ssh_key_may_have_no_passphrase() {
        let mut form = ssh_form();
        form.target_auth = Auth::Key;
        assert!(form.validate(&ProfileStore::default()).unwrap_err().contains_key(&Field::TargetKeyPath));
        form.target_key_path = "/keys/id_ed25519".into();
        let profile = form.validate(&ProfileStore::default()).expect("valid");
        assert_eq!(profile.protocol, Protocol::Ssh);
        assert_eq!(profile.target.key_path, Some(PathBuf::from("/keys/id_ed25519")));
        assert_eq!(profile.target.domain, None);
    }

    #[test]
    fn an_ssh_password_is_required() {
        let mut form = ssh_form();
        form.domain = "CORP".into();
        assert!(form.validate(&ProfileStore::default()).unwrap_err().contains_key(&Field::TargetSecret));
        form.target_secret = "secret".into();
        let profile = form.validate(&ProfileStore::default()).expect("valid");
        assert_eq!(profile.target.key_path, None);
        assert_eq!(profile.target.domain, None, "a domain is RDP only");
    }

    #[test]
    fn editing_keeps_the_protocol_and_asks_again_after_an_auth_switch() {
        let mut form = ssh_form();
        form.target_secret = "secret".into();
        let profile = form.validate(&ProfileStore::default()).expect("valid");
        let mut edit = Form::from_profile(&profile);
        assert_eq!(edit.protocol, Protocol::Ssh);
        assert!(edit.validate(&ProfileStore::default()).is_ok(), "the stored password is kept");
        edit.target_auth = Auth::Key;
        edit.target_key_path = "/keys/id_ed25519".into();
        assert!(edit.target_secret_required(), "the old password is no key passphrase");
    }
}
