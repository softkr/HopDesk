//! Session processes started from the launcher and their progress.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use eframe::egui;
use uuid::Uuid;

use crate::status::{self, Phase};

const MAX_LOG_LINES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Process started, nothing reported yet.
    Starting,
    Ssh,
    Rdp,
    Connected,
    Reconnecting,
    Failed(FailureInfo),
}

impl State {
    pub fn is_active(&self) -> bool {
        !matches!(self, State::Failed(_))
    }
}

/// User-facing explanation of a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureInfo {
    pub title: String,
    pub hint: String,
    /// Raw error text for the details view.
    pub detail: String,
    /// The fix is to clear the pinned key/certificate.
    pub key_changed: bool,
    /// Retrying unchanged would fail again: the fix is in the connection settings.
    pub fix_in_settings: bool,
    /// Failed after the jump host was reached (between jump host and remote PC).
    pub at_remote: bool,
}

pub struct Session {
    pub state: State,
    pub log: Vec<String>,
    child: Option<Arc<Mutex<Child>>>,
    cancelled: bool,
}

/// Something the UI should announce once (toast).
pub enum Notice {
    Ended { name: String },
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<Uuid, Session>,
    notices: Vec<Notice>,
}

#[derive(Clone, Default)]
pub struct Sessions(Arc<Mutex<Inner>>);

impl Sessions {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether any session is connecting or connected.
    pub fn any_active(&self) -> bool {
        self.lock().sessions.values().any(|s| s.state.is_active())
    }

    pub fn state(&self, id: Uuid) -> Option<State> {
        self.lock().sessions.get(&id).map(|s| s.state.clone())
    }

    pub fn log(&self, id: Uuid) -> Vec<String> {
        self.lock().sessions.get(&id).map(|s| s.log.clone()).unwrap_or_default()
    }

    pub fn take_notices(&self) -> Vec<Notice> {
        std::mem::take(&mut self.lock().notices)
    }

    /// Forgets a finished (failed) session so the profile shows as idle again.
    pub fn dismiss(&self, id: Uuid) {
        let mut inner = self.lock();
        if inner.sessions.get(&id).is_some_and(|s| s.child.is_none()) {
            inner.sessions.remove(&id);
        }
    }

    /// Stops a running session (connection attempt or open desktop window).
    pub fn cancel(&self, id: Uuid) {
        let mut inner = self.lock();
        if let Some(session) = inner.sessions.get_mut(&id) {
            session.cancelled = true;
            if let Some(child) = &session.child {
                let _ = child.lock().map(|mut c| c.kill());
            }
        }
    }

    #[cfg(test)]
    pub fn set_for_test(&self, id: Uuid, state: State, log: Vec<String>) {
        self.lock().sessions.insert(id, Session { state, log, child: None, cancelled: false });
    }

    #[cfg(test)]
    pub fn failure_for_test(detail: &str, at: &State) -> FailureInfo {
        explain(detail, &[], at)
    }

    /// Starts `hopdesk connect <name>` for the profile and tracks its progress.
    pub fn start(&self, id: Uuid, name: &str, ctx: &egui::Context) -> anyhow::Result<()> {
        if self.state(id).is_some_and(|s| s.is_active()) {
            return Ok(());
        }
        let exe = std::env::current_exe().context("locate hopdesk executable")?;
        let mut command = Command::new(exe);
        command.args(["connect", name]);
        // Monitor size in points, so fullscreen sessions start at the right resolution.
        if let Some(size) = ctx.input(|i| i.viewport().monitor_size) {
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "monitor sizes fit u16")]
            command.arg(format!("--screen={}x{}", size.x.round() as u16, size.y.round() as u16));
        }
        let mut child = command
            .env(status::ENV_VAR, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("start session process")?;
        let stderr = child.stderr.take().context("session stderr")?;
        let child = Arc::new(Mutex::new(child));

        self.lock().sessions.insert(
            id,
            Session {
                state: State::Starting,
                log: Vec::new(),
                child: Some(Arc::clone(&child)),
                cancelled: false,
            },
        );

        let this = self.clone();
        let ctx = ctx.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                this.on_line(id, line);
                ctx.request_repaint();
            }
            let exit = child.lock().map(|mut c| c.wait());
            this.on_exit(id, &name, matches!(exit, Ok(Ok(s)) if s.success()));
            ctx.request_repaint();
        });
        Ok(())
    }

    fn on_line(&self, id: Uuid, line: String) {
        let mut inner = self.lock();
        let Some(session) = inner.sessions.get_mut(&id) else { return };
        match status::parse(&line) {
            Some((phase, detail)) => {
                let next = match phase {
                    Phase::Ssh => Some(State::Ssh),
                    Phase::Rdp => Some(State::Rdp),
                    Phase::Connected | Phase::LoggedIn => Some(State::Connected),
                    Phase::Reconnecting => Some(State::Reconnecting),
                    Phase::Failed => Some(State::Failed(explain(&detail, &session.log, &session.state))),
                    Phase::Ended => None,
                };
                // Keep the first failure; later lines are usually consequences of it.
                if let Some(next) = next
                    && !matches!(session.state, State::Failed(_))
                {
                    session.state = next;
                }
            }
            None => {
                session.log.push(line);
                if session.log.len() > MAX_LOG_LINES {
                    let excess = session.log.len() - MAX_LOG_LINES;
                    session.log.drain(..excess);
                }
            }
        }
    }

    fn on_exit(&self, id: Uuid, name: &str, success: bool) {
        let mut inner = self.lock();
        let Some(session) = inner.sessions.get_mut(&id) else { return };
        session.child = None;
        let failed = matches!(session.state, State::Failed(_));
        if session.cancelled || (!failed && success) {
            let was_connected = matches!(session.state, State::Connected | State::Reconnecting);
            inner.sessions.remove(&id);
            if was_connected {
                inner.notices.push(Notice::Ended { name: name.to_owned() });
            }
        } else if !failed {
            let detail = session.log.last().cloned().unwrap_or_default();
            session.state = State::Failed(explain(&detail, &session.log, &session.state));
        }
    }
}

/// Turns raw error text into a short Korean explanation with a next step.
fn explain(detail: &str, log: &[String], state: &State) -> FailureInfo {
    let all = format!("{detail}\n{}", log.join("\n"));
    let has = |needle: &str| all.to_lowercase().contains(&needle.to_lowercase());

    let (title, hint, key_changed) = if has("SSH HOST KEY CHANGED") {
        (
            "점프 호스트의 SSH 키가 바뀌었습니다",
            "서버를 새로 설치했거나 키를 교체했다면 ‘호스트 키 초기화’ 후 다시 연결하세요. \
             그렇지 않다면 중간자 공격일 수 있으니 관리자에게 먼저 확인하세요.",
            true,
        )
    } else if has("RDP CERTIFICATE CHANGED") {
        (
            "원격 PC의 인증서가 바뀌었습니다",
            "인증서를 교체한 것이 확실하다면 ‘호스트 키 초기화’ 후 다시 연결하세요. \
             그렇지 않다면 중간자 공격일 수 있으니 관리자에게 먼저 확인하세요.",
            true,
        )
    } else if has("no stored") {
        (
            "저장된 비밀번호가 없습니다",
            "연결을 편집해서 비밀번호를 다시 입력하세요.",
            false,
        )
    } else if has("load private key") {
        (
            "개인 키를 열 수 없습니다",
            "키 파일 경로가 맞는지, 키 암호가 올바른지 확인하세요.",
            false,
        )
    } else if has("jump host authentication failed") {
        (
            "점프 호스트 로그인에 실패했습니다",
            "SSH 사용자 이름과 비밀번호(또는 개인 키)를 확인하세요.",
            false,
        )
    } else if has("SSH connect to jump host") {
        (
            "점프 호스트에 연결할 수 없습니다",
            "주소와 포트가 맞는지, 네트워크(VPN 등)에 연결되어 있는지 확인하세요.",
            false,
        )
    } else if has("TCP connect") {
        (
            "원격 PC에 연결할 수 없습니다",
            "주소와 포트가 맞는지, 네트워크(VPN 등)와 원격 데스크톱이 켜져 있는지 확인하세요.",
            false,
        )
    } else if has("direct-tcpip") {
        (
            "원격 PC에 도달할 수 없습니다",
            "점프 호스트에서 본 원격 PC 주소·포트가 맞는지, 원격 데스크톱이 켜져 있는지 확인하세요.",
            false,
        )
    } else if has("logon") || has("credssp") || has("authentication") || has("password") {
        (
            "원격 PC 로그인에 실패했습니다",
            "Windows 사용자 이름, 도메인, 비밀번호를 확인하세요.",
            false,
        )
    } else {
        (
            "연결하지 못했습니다",
            "아래 자세한 내용을 확인하거나 잠시 후 다시 시도하세요.",
            false,
        )
    };

    FailureInfo {
        title: title.to_owned(),
        hint: hint.to_owned(),
        detail: detail.trim().to_owned(),
        key_changed,
        fix_in_settings: has("no stored")
            || has("load private key")
            || has("jump host authentication failed")
            || (!key_changed
                && !has("SSH connect to jump host")
                && !has("direct-tcpip")
                && !has("TCP connect")
                && (has("logon") || has("credssp") || has("authentication") || has("password"))),
        at_remote: matches!(state, State::Rdp | State::Connected | State::Reconnecting) && !has("SSH HOST KEY CHANGED"),
    }
}
