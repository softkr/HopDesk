//! Session processes started from the launcher and their progress.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use eframe::egui;
use uuid::Uuid;

use crate::profile::Protocol;
use crate::status::{self, Phase};

const MAX_LOG_LINES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Process started, nothing reported yet.
    Starting,
    Ssh,
    Remote,
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

/// How many traffic samples a session keeps: one a second, a minute of history.
pub const TRAFFIC_SAMPLES: usize = 60;

/// A session's recent traffic, from the totals its process reports (see [`crate::stats`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Traffic {
    /// `(download, upload)` in bits per second, oldest first.
    pub samples: std::collections::VecDeque<(f64, f64)>,
    /// The last totals and when they arrived.
    last: Option<(u64, u64, std::time::Instant)>,
}

impl Traffic {
    /// Takes the byte totals reported at `now`; the first report only sets the baseline.
    fn record(&mut self, received: u64, sent: u64, now: std::time::Instant) {
        if let Some((last_received, last_sent, at)) = self.last {
            let secs = now.duration_since(at).as_secs_f64();
            if secs > 0.0 {
                #[expect(clippy::cast_precision_loss, reason = "a second's bytes are far below 2^52")]
                let rate = |total: u64, last: u64| total.saturating_sub(last) as f64 * 8.0 / secs;
                self.samples.push_back((rate(received, last_received), rate(sent, last_sent)));
                if self.samples.len() > TRAFFIC_SAMPLES {
                    self.samples.pop_front();
                }
            }
        }
        self.last = Some((received, sent, now));
    }
}

pub struct Session {
    pub state: State,
    pub log: Vec<String>,
    /// When the session first connected; kept through reconnects, for its uptime.
    connected_since: Option<std::time::Instant>,
    /// Set once the session's process reports traffic.
    traffic: Option<Traffic>,
    /// The last reported round trip to the tunnel's far end.
    latency: Option<std::time::Duration>,
    /// Decides how failures are explained.
    protocol: Protocol,
    child: Option<Arc<Mutex<Child>>>,
    cancelled: bool,
    /// Start the session again once its process has ended (see [`Sessions::restart`]).
    restart: bool,
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

    pub fn connected_since(&self, id: Uuid) -> Option<std::time::Instant> {
        self.lock().sessions.get(&id).and_then(|s| s.connected_since)
    }

    pub fn traffic(&self, id: Uuid) -> Option<Traffic> {
        self.lock().sessions.get(&id).and_then(|s| s.traffic.clone())
    }

    pub fn latency(&self, id: Uuid) -> Option<std::time::Duration> {
        self.lock().sessions.get(&id).and_then(|s| s.latency)
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
                stop(child);
            }
        }
    }

    /// Ends a running session and starts it again as soon as its process has gone.
    pub fn restart(&self, id: Uuid) {
        if let Some(session) = self.lock().sessions.get_mut(&id) {
            session.restart = true;
        }
        self.cancel(id);
    }

    #[cfg(test)]
    pub fn set_for_test(&self, id: Uuid, state: State, log: Vec<String>) {
        let connected_since = (state == State::Connected).then(std::time::Instant::now);
        self.lock()
            .sessions
            .insert(id, Session { state, log, connected_since, traffic: None, latency: None, protocol: Protocol::Rdp, child: None, cancelled: false, restart: false });
    }

    #[cfg(test)]
    pub fn set_traffic_for_test(&self, id: Uuid, samples: &[(f64, f64)]) {
        if let Some(session) = self.lock().sessions.get_mut(&id) {
            session.traffic = Some(Traffic { samples: samples.iter().copied().collect(), last: None });
        }
    }

    #[cfg(test)]
    pub fn set_latency_for_test(&self, id: Uuid, latency: std::time::Duration) {
        if let Some(session) = self.lock().sessions.get_mut(&id) {
            session.latency = Some(latency);
        }
    }

    #[cfg(test)]
    pub fn failure_for_test(detail: &str, at: &State) -> FailureInfo {
        explain(detail, &[], at, Protocol::Rdp)
    }

    /// Starts `hopdesk connect <name>` for the profile and tracks its progress.
    pub fn start(&self, id: Uuid, name: &str, protocol: Protocol, ctx: &egui::Context) -> anyhow::Result<()> {
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
        let session_log = session_log_file(name);

        self.lock().sessions.insert(
            id,
            Session {
                state: State::Starting,
                log: Vec::new(),
                connected_since: None,
                traffic: None,
                latency: None,
                protocol,
                child: Some(Arc::clone(&child)),
                cancelled: false,
                restart: false,
            },
        );

        let this = self.clone();
        let ctx = ctx.clone();
        let name = name.to_owned();
        std::thread::spawn(move || {
            let mut session_log = session_log;
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                // Traffic and latency reports arrive every few seconds: kept out of the logs.
                if let Some((received, sent)) = status::parse_stats(&line) {
                    this.on_stats(id, received, sent);
                    ctx.request_repaint();
                    continue;
                }
                if let Some(latency) = status::parse_latency(&line) {
                    if let Some(session) = this.lock().sessions.get_mut(&id) {
                        session.latency = Some(latency);
                    }
                    ctx.request_repaint();
                    continue;
                }
                if let Some(log) = session_log.as_mut() {
                    let _ = writeln!(log, "{line}");
                }
                this.on_line(id, line);
                ctx.request_repaint();
            }
            let exit = child.lock().map(|mut c| c.wait());
            let restart = this.on_exit(id, &name, matches!(exit, Ok(Ok(s)) if s.success()));
            if restart && let Err(e) = this.start(id, &name, protocol, &ctx) {
                tracing::error!(error = %format!("{e:#}"), "Restarting the session failed");
            }
            ctx.request_repaint();
        });
        Ok(())
    }

    fn on_stats(&self, id: Uuid, received: u64, sent: u64) {
        if let Some(session) = self.lock().sessions.get_mut(&id) {
            session.traffic.get_or_insert_default().record(received, sent, std::time::Instant::now());
        }
    }

    fn on_line(&self, id: Uuid, line: String) {
        let mut inner = self.lock();
        let Some(session) = inner.sessions.get_mut(&id) else { return };
        match status::parse(&line) {
            Some((phase, detail)) => {
                let next = match phase {
                    Phase::Ssh => Some(State::Ssh),
                    Phase::Remote => Some(State::Remote),
                    Phase::Connected | Phase::LoggedIn => Some(State::Connected),
                    Phase::Reconnecting => Some(State::Reconnecting),
                    Phase::Failed => Some(State::Failed(explain(&detail, &session.log, &session.state, session.protocol))),
                    Phase::Ended => None,
                };
                // Keep the first failure; later lines are usually consequences of it.
                if let Some(next) = next
                    && !matches!(session.state, State::Failed(_))
                {
                    if next == State::Connected && session.connected_since.is_none() {
                        session.connected_since = Some(std::time::Instant::now());
                    }
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

    /// Records the end of a session process; `true` when the session is to be started again.
    fn on_exit(&self, id: Uuid, name: &str, success: bool) -> bool {
        let mut inner = self.lock();
        let Some(session) = inner.sessions.get_mut(&id) else { return false };
        session.child = None;
        let restart = session.restart;
        let failed = matches!(session.state, State::Failed(_));
        if session.cancelled || (!failed && success) {
            let was_connected = matches!(session.state, State::Connected | State::Reconnecting);
            inner.sessions.remove(&id);
            if was_connected && !restart {
                inner.notices.push(Notice::Ended { name: name.to_owned() });
            }
        } else if !failed {
            let detail = session.log.last().cloned().unwrap_or_default();
            session.state = State::Failed(explain(&detail, &session.log, &session.state, session.protocol));
        }
        restart
    }
}

/// How long a session asked to quit gets before it is killed (macOS).
#[cfg(target_os = "macos")]
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// macOS: asks the session process to quit (SIGTERM), so it leaves the Dock first: a killed
/// process stays in the Dock as a "recent app" next to the launcher. Killed after `STOP_GRACE`.
#[cfg(target_os = "macos")]
fn stop(child: &Arc<Mutex<Child>>) {
    let Ok(mut process) = child.lock() else { return };
    // Not yet reaped (checked under the lock), so the pid still names this process.
    if !matches!(process.try_wait(), Ok(None)) {
        return;
    }
    let Ok(pid) = libc::pid_t::try_from(process.id()) else {
        let _ = process.kill();
        return;
    };
    // SAFETY: kill(2) with a valid signal on the pid of our own, unreaped child.
    if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
        let _ = process.kill();
        return;
    }
    drop(process);
    let child = Arc::clone(child);
    std::thread::spawn(move || {
        std::thread::sleep(STOP_GRACE);
        if let Ok(mut process) = child.lock()
            && matches!(process.try_wait(), Ok(None))
        {
            tracing::warn!("Session did not quit in time, killing it");
            let _ = process.kill();
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn stop(child: &Arc<Mutex<Child>>) {
    let _ = child.lock().map(|mut c| c.kill());
}

/// A file the session's log is copied into, one per session, when the debug bundle asks for it
/// with `HOPDESK_LOG_DIR`. The connection log in the window keeps only the last lines; the file
/// keeps everything, so a file-transfer problem can be read after the fact.
fn session_log_file(name: &str) -> Option<std::fs::File> {
    let dir = std::env::var_os("HOPDESK_LOG_DIR")?;
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' })
        .collect();
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let path = std::path::Path::new(&dir).join(format!("session-{safe}-{secs}.log"));
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

/// Turns raw error text into a short Korean explanation with a next step.
fn explain(detail: &str, log: &[String], state: &State, protocol: Protocol) -> FailureInfo {
    let all = format!("{detail}\n{}", log.join("\n"));
    let has = |needle: &str| all.to_lowercase().contains(&needle.to_lowercase());

    let target = super::target_noun(protocol);
    let (service, account) = match protocol {
        Protocol::Rdp => ("원격 데스크톱이", "Windows 사용자 이름, 도메인, 비밀번호를 확인하세요."),
        Protocol::Ssh => ("SSH 서버가", "사용자 이름과 비밀번호(또는 개인 키)를 확인하세요."),
        Protocol::Vnc => (
            "화면 공유(VNC)가",
            "VNC 비밀번호를, macOS 계정으로 로그인한다면 계정 이름과 비밀번호를 확인하세요.",
        ),
    };
    // The jump host message is part of the target one, so tell them apart first.
    let target_key_changed = has("TARGET SSH HOST KEY CHANGED");
    let jump_key_changed = has("SSH HOST KEY CHANGED") && !target_key_changed;
    let target_unreachable = has("SSH connect to target") || has("TCP connect");
    let vpn_settings_wrong = has("VPN private key") || has("VPN peer public key") || has("VPN target must be");
    let key_hint = |what: &str| {
        format!(
            "{what}를 교체한 것이 확실하다면 ‘호스트 키 초기화’ 후 다시 연결하세요. \
             그렇지 않다면 중간자 공격일 수 있으니 관리자에게 먼저 확인하세요."
        )
    };

    let (title, hint, key_changed): (String, String, bool) = if target_key_changed {
        (format!("{target}의 SSH 키가 바뀌었습니다"), key_hint("키"), true)
    } else if jump_key_changed {
        (
            "점프 호스트의 SSH 키가 바뀌었습니다".into(),
            "서버를 새로 설치했거나 키를 교체했다면 ‘호스트 키 초기화’ 후 다시 연결하세요. \
             그렇지 않다면 중간자 공격일 수 있으니 관리자에게 먼저 확인하세요."
                .into(),
            true,
        )
    } else if has("RDP CERTIFICATE CHANGED") {
        ("원격 PC의 인증서가 바뀌었습니다".into(), key_hint("인증서"), true)
    } else if has("no stored VPN private key") {
        (
            "저장된 VPN 개인키가 없습니다".into(),
            "연결 설정에서 개인키를 다시 입력하세요.".into(),
            false,
        )
    } else if vpn_settings_wrong {
        (
            "VPN 설정이 올바르지 않습니다".into(),
            "연결 설정에서 키와 대상 주소(VPN 안에서는 IP 주소)를 확인하세요.".into(),
            false,
        )
    } else if has("VPN handshake timed out") {
        (
            "VPN에 연결할 수 없습니다".into(),
            "VPN 서버 주소·포트와 두 키가 맞는지, 서버가 켜져 있고 UDP 포트가 열려 있는지 확인하세요.".into(),
            false,
        )
    } else if has("VPN endpoint lookup failed") {
        (
            "VPN 서버 주소를 찾을 수 없습니다".into(),
            "서버 주소가 맞는지, 인터넷에 연결되어 있는지 확인하세요.".into(),
            false,
        )
    } else if has("no stored") {
        (
            "저장된 비밀번호가 없습니다".into(),
            "연결 설정에서 비밀번호를 다시 입력하세요.".into(),
            false,
        )
    } else if has("load private key") {
        (
            "개인 키를 열 수 없습니다".into(),
            "키 파일 경로가 맞는지, 키 암호가 올바른지 확인하세요.".into(),
            false,
        )
    } else if has("jump host authentication failed") {
        (
            "점프 호스트 로그인에 실패했습니다".into(),
            "SSH 사용자 이름과 비밀번호(또는 개인 키)를 확인하세요.".into(),
            false,
        )
    } else if has("SSH connect to jump host") {
        (
            "점프 호스트에 연결할 수 없습니다".into(),
            "주소와 포트가 맞는지, 네트워크(VPN 등)에 연결되어 있는지 확인하세요.".into(),
            false,
        )
    } else if target_unreachable {
        (
            format!("{target}에 연결할 수 없습니다"),
            format!("주소와 포트가 맞는지, 네트워크(VPN 등)와 {service} 켜져 있는지 확인하세요."),
            false,
        )
    } else if has("direct-tcpip") {
        (
            format!("{target}에 도달할 수 없습니다"),
            format!("점프 호스트에서 본 {target} 주소·포트가 맞는지, {service} 켜져 있는지 확인하세요."),
            false,
        )
    } else if has("logon") || has("credssp") || has("authentication") || has("password") {
        (format!("{target} 로그인에 실패했습니다"), account.into(), false)
    } else {
        (
            "연결하지 못했습니다".into(),
            "아래 자세한 내용을 확인하거나 잠시 후 다시 시도하세요.".into(),
            false,
        )
    };

    FailureInfo {
        title,
        hint,
        detail: detail.trim().to_owned(),
        key_changed,
        fix_in_settings: has("no stored")
            || vpn_settings_wrong
            || has("load private key")
            || has("jump host authentication failed")
            || (!key_changed
                && !has("SSH connect to jump host")
                && !has("direct-tcpip")
                && !target_unreachable
                && (has("logon") || has("credssp") || has("authentication") || has("password"))),
        at_remote: matches!(state, State::Remote | State::Connected | State::Reconnecting) && !jump_key_changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traffic_rates_come_from_the_totals_between_reports() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let mut traffic = Traffic::default();
        traffic.record(1000, 100, start);
        assert!(traffic.samples.is_empty(), "the first report is the baseline");
        traffic.record(1000 + 125_000, 100 + 1250, start + Duration::from_secs(1));
        traffic.record(1000 + 125_000, 100 + 1250, start + Duration::from_secs(3));
        assert_eq!(traffic.samples, [(1_000_000.0, 10_000.0), (0.0, 0.0)]);
        for i in 0..2 * TRAFFIC_SAMPLES as u64 {
            traffic.record(200_000, 2000, start + Duration::from_secs(4 + i));
        }
        assert_eq!(traffic.samples.len(), TRAFFIC_SAMPLES);
    }

    #[test]
    fn rdp_wording_is_unchanged() {
        let info = explain("TCP connect: refused", &[], &State::Remote, Protocol::Rdp);
        assert_eq!(info.title, "원격 PC에 연결할 수 없습니다");
        assert_eq!(info.hint, "주소와 포트가 맞는지, 네트워크(VPN 등)와 원격 데스크톱이 켜져 있는지 확인하세요.");
        let info = explain("logon failed", &[], &State::Remote, Protocol::Rdp);
        assert_eq!(info.hint, "Windows 사용자 이름, 도메인, 비밀번호를 확인하세요.");
        assert!(info.fix_in_settings);
    }

    #[test]
    fn vpn_failures_name_the_vpn_and_say_what_to_check() {
        let info = explain("VPN handshake timed out (check the endpoint 1.2.3.4:51820)", &[], &State::Ssh, Protocol::Rdp);
        assert_eq!(info.title, "VPN에 연결할 수 없습니다");
        assert!(!info.at_remote, "the target was never reached");
        assert!(!info.fix_in_settings, "the network may be at fault, so retrying is offered");

        let info = explain("VPN endpoint lookup failed for vpn.example.com:51820", &[], &State::Ssh, Protocol::Rdp);
        assert_eq!(info.title, "VPN 서버 주소를 찾을 수 없습니다");

        let info = explain("no stored VPN private key (edit the profile and enter the key again)", &[], &State::Starting, Protocol::Rdp);
        assert_eq!(info.title, "저장된 VPN 개인키가 없습니다");
        assert!(info.fix_in_settings);

        let info = explain("VPN peer public key: not valid base64", &[], &State::Ssh, Protocol::Rdp);
        assert_eq!(info.title, "VPN 설정이 올바르지 않습니다");
        assert!(info.fix_in_settings);

        let info = explain("VPN target must be an IP address, got `server.internal`", &[], &State::Remote, Protocol::Ssh);
        assert_eq!(info.title, "VPN 설정이 올바르지 않습니다");
    }

    #[test]
    fn a_refused_connection_inside_the_vpn_blames_the_target() {
        let info = explain("TCP connect: VPN TCP connection refused", &[], &State::Remote, Protocol::Rdp);
        assert_eq!(info.title, "원격 PC에 연결할 수 없습니다");
        assert!(info.at_remote);
    }

    #[test]
    fn ssh_target_failures_name_the_server() {
        let info = explain("SSH authentication failed on the remote host", &[], &State::Remote, Protocol::Ssh);
        assert_eq!(info.title, "원격 서버 로그인에 실패했습니다");
        assert!(info.hint.contains("개인 키"), "{}", info.hint);
        assert!(info.fix_in_settings && info.at_remote);

        let info = explain("SSH connect to target: Connection refused", &[], &State::Remote, Protocol::Ssh);
        assert_eq!(info.title, "원격 서버에 연결할 수 없습니다");
        assert!(info.hint.contains("SSH 서버가"), "{}", info.hint);
        assert!(!info.fix_in_settings);
    }

    #[test]
    fn vnc_failures_point_at_the_screen_sharing_login() {
        let info = explain("VNC authentication failed: Wrong password", &[], &State::Remote, Protocol::Vnc);
        assert_eq!(info.title, "원격 PC 로그인에 실패했습니다");
        assert!(info.hint.contains("VNC 비밀번호"), "{}", info.hint);
        assert!(info.fix_in_settings);
        let info = explain("TCP connect to VNC target: refused", &[], &State::Remote, Protocol::Vnc);
        assert!(info.hint.contains("화면 공유(VNC)가"), "{}", info.hint);
    }

    #[test]
    fn a_changed_target_key_is_not_blamed_on_the_jump_host() {
        let log = ["ERROR TARGET SSH HOST KEY CHANGED - possible man-in-the-middle".to_owned()];
        let info = explain("SSH connect to target: unknown key", &log, &State::Remote, Protocol::Ssh);
        assert_eq!(info.title, "원격 서버의 SSH 키가 바뀌었습니다");
        assert!(info.key_changed && info.at_remote);

        let log = ["ERROR SSH HOST KEY CHANGED - possible man-in-the-middle".to_owned()];
        let info = explain("SSH connect to jump host", &log, &State::Ssh, Protocol::Ssh);
        assert_eq!(info.title, "점프 호스트의 SSH 키가 바뀌었습니다");
        assert!(info.key_changed && !info.at_remote);
    }
}
