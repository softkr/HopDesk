//! SSH: a shell on the target in a terminal window.

use std::sync::Arc;

use anyhow::{Context as _, ensure};
use eframe::egui;
use russh::client::{self, Handle};
use russh::{Channel, ChannelMsg};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::profile::Profile;
use crate::status::{self, Phase};
use crate::terminal::{self, Input, Parser, Screen, TerminalView};
use crate::trust::TrustStore;
use crate::tunnel::{self, Credential, HostKeyVerifier, Role, Tunnel};

/// Screen size the shell starts with, before the window has measured itself.
const INITIAL_COLUMNS: u16 = 100;
const INITIAL_LINES: u16 = 30;
/// Keystrokes and resizes waiting for the network.
const INPUT_QUEUE: usize = 256;
const TERM: &str = "xterm-256color";

/// Without `tunnel` the session connects straight to the target.
pub fn run(
    profile: &Profile,
    password: Option<String>,
    tunnel: Option<Arc<Tunnel>>,
    trust: Arc<TrustStore>,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("create tokio runtime")?;

    // Connect and log on before any window opens, so failures show in the launcher.
    if let Some(tunnel) = &tunnel {
        status::emit(Phase::Ssh, "");
        rt.block_on(tunnel.ensure_connected())?;
    }
    status::emit(Phase::Remote, "");
    let (handle, channel) = rt.block_on(open_shell(profile, password.as_deref(), tunnel, trust))?;
    status::emit(Phase::Connected, "");

    let (input_sender, input_receiver) = mpsc::channel(INPUT_QUEUE);
    let screen = terminal::new_screen(INITIAL_COLUMNS, INITIAL_LINES, input_sender.clone());
    let name = profile.name.clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("HopDesk — {}", profile.name))
            .with_app_id("hopdesk")
            .with_inner_size([900.0, 560.0])
            .with_min_inner_size([320.0, 200.0])
            .with_icon(eframe::icon_data::from_png_bytes(crate::gui::WINDOW_ICON).expect("bundled icon is a valid PNG")),
        ..Default::default()
    };
    eframe::run_native(
        "HopDesk",
        options,
        Box::new(move |cc| {
            crate::gui::theme::install_fonts(&cc.egui_ctx);
            // Ctrl+-/+ belong to the shell.
            cc.egui_ctx.options_mut(|options| options.zoom_with_keyboard = false);
            // The remote side decides what is printed: a character no font has is drawn as a box.
            cc.egui_ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
            #[cfg(target_os = "macos")]
            crate::dock::mark_session(&name);
            #[cfg(not(target_os = "macos"))]
            let _ = name;
            rt.spawn(pump(handle, channel, Arc::clone(&screen), input_receiver, cc.egui_ctx.clone()));
            Ok(Box::new(Window {
                view: TerminalView::new(screen, input_sender, INITIAL_COLUMNS, INITIAL_LINES),
                _runtime: rt,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("terminal window failed: {e}"))?;
    #[cfg(target_os = "macos")]
    crate::dock::leave_dock();
    Ok(())
}

async fn open_shell(
    profile: &Profile,
    password: Option<&str>,
    tunnel: Option<Arc<Tunnel>>,
    trust: Arc<TrustStore>,
) -> anyhow::Result<(Handle<HostKeyVerifier>, Channel<client::Msg>)> {
    let target = &profile.target;
    let verifier = HostKeyVerifier::new(&target.host, target.port, trust, Role::Target);
    info!(host = %target.host, port = target.port, user = %target.user, "Connecting to SSH target");
    let connection = match tunnel {
        Some(tunnel) => {
            let stream = tunnel.open(&target.host, target.port).await?;
            client::connect_stream(tunnel::client_config(), stream, verifier).await
        }
        None => client::connect(tunnel::client_config(), (target.host.as_str(), target.port), verifier).await,
    };
    let mut handle = connection.context("SSH connect to target")?;

    let secret = password.filter(|s| !s.is_empty());
    let credential = match &target.key_path {
        Some(path) => Credential::Key { path, passphrase: secret },
        None => Credential::Password(secret.context("no stored SSH password (use `set-password --target`)")?),
    };
    ensure!(
        tunnel::authenticate(&mut handle, &target.user, credential).await?,
        "SSH authentication failed on the remote host"
    );
    info!("SSH target authenticated");

    let channel = handle.channel_open_session().await.context("open SSH session channel")?;
    channel
        .request_pty(false, TERM, u32::from(INITIAL_COLUMNS), u32::from(INITIAL_LINES), 0, 0, &[])
        .await
        .context("request terminal")?;
    channel.request_shell(false).await.context("start remote shell")?;
    Ok((handle, channel))
}

/// Moves shell output onto the screen and keystrokes to the shell until either side closes.
async fn pump(
    handle: Handle<HostKeyVerifier>,
    channel: Channel<client::Msg>,
    screen: Screen,
    mut input: mpsc::Receiver<Input>,
    ctx: egui::Context,
) {
    let (mut reader, writer) = channel.split();
    let mut parser = Parser::new();
    let reason = loop {
        tokio::select! {
            message = reader.wait() => match message {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    parser.feed(&screen, &data);
                    ctx.request_repaint();
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => info!(exit_status, "Remote shell exited"),
                Some(ChannelMsg::Close) | None => break "shell closed",
                Some(_) => {}
            },
            Some(next) = input.recv() => {
                let sent = match next {
                    Input::Bytes(bytes) => writer.data_bytes(bytes).await,
                    Input::Resize { columns, lines } => {
                        writer.window_change(u32::from(columns), u32::from(lines), 0, 0).await
                    }
                };
                if let Err(error) = sent {
                    warn!(%error, "Sending to the remote shell failed");
                    break "connection lost";
                }
            }
        }
    };
    info!(reason, "SSH session ended");
    status::emit(Phase::Ended, reason);
    drop(handle);
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

struct Window {
    view: TerminalView,
    /// Runs the network task; dropped (and the connection with it) when the window closes.
    _runtime: tokio::runtime::Runtime,
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.view.ui(ui));
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions as _;
    use alacritty_terminal::index::{Column, Line, Point};

    use super::*;
    use crate::profile::{DisplayMode, JumpHost, Protocol, Target};
    use crate::tunnel::JumpParams;

    /// `user@host:port` and a private key without passphrase, from the environment.
    fn server() -> (String, String, u16, std::path::PathBuf) {
        let spec = std::env::var("HOPDESK_TEST_SSH").expect("HOPDESK_TEST_SSH=user@host:port");
        let key = std::env::var("HOPDESK_TEST_SSH_KEY").expect("HOPDESK_TEST_SSH_KEY=path");
        let (user, rest) = spec.split_once('@').expect("user@host:port");
        let (host, port) = rest.rsplit_once(':').expect("host:port");
        (user.into(), host.into(), port.parse().expect("port"), key.into())
    }

    fn screen_text(screen: &Screen) -> String {
        let term = screen.lock().expect("screen lock");
        let grid = term.grid();
        (0..grid.screen_lines())
            .map(|line| {
                (0..grid.columns())
                    .map(|column| &grid[Point::new(Line(line as i32), Column(column))])
                    .filter(|cell| !cell.flags.contains(alacritty_terminal::term::cell::Flags::WIDE_CHAR_SPACER))
                    .map(|cell| cell.c)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn run_shell(jump: bool) -> String {
        let (user, host, port, key) = server();
        let dir = std::env::temp_dir().join(format!("hopdesk-ssh-test-{}-{jump}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let trust = Arc::new(TrustStore::at(dir.join("trust.toml")));
        let profile = Profile {
            id: uuid::Uuid::new_v4(),
            name: "test".into(),
            protocol: Protocol::Ssh,
            use_jump: jump,
            jump: JumpHost::unset(),
            vpn: None,
            target: Target {
                host: host.clone(),
                port,
                user: user.clone(),
                domain: None,
                key_path: Some(key.clone()),
            },
            width: 0,
            height: 0,
            display: DisplayMode::Fixed,
        };
        let tunnel = jump.then(|| {
            Tunnel::jump(
                JumpParams {
                    host: host.clone(),
                    port,
                    user: user.clone(),
                    key_path: Some(key.clone()),
                    secret: None,
                },
                Arc::clone(&trust),
            )
        });
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let (handle, channel) = rt.block_on(open_shell(&profile, None, tunnel, trust)).expect("open shell");
        let (sender, receiver) = mpsc::channel(INPUT_QUEUE);
        let screen = terminal::new_screen(INITIAL_COLUMNS, INITIAL_LINES, sender.clone());
        let pumping = rt.spawn(pump(handle, channel, Arc::clone(&screen), receiver, egui::Context::default()));
        rt.block_on(async {
            sender.send(Input::Resize { columns: 60, lines: 20 }).await.expect("send");
            let command = "stty size; echo hop-$((40+2)) 한글; exit\r";
            sender.send(Input::Bytes(command.as_bytes().to_vec())).await.expect("send");
            tokio::time::timeout(core::time::Duration::from_secs(20), pumping)
                .await
                .expect("shell exits")
                .expect("pump task");
        });
        screen_text(&screen)
    }

    #[test]
    #[ignore = "needs an SSH server: HOPDESK_TEST_SSH=user@host:port HOPDESK_TEST_SSH_KEY=key"]
    fn shell_runs_commands_directly() {
        let text = run_shell(false);
        assert!(text.contains("hop-42 한글"), "{text}");
        assert!(text.contains("20 60"), "window size reaches the shell: {text}");
    }

    #[test]
    #[ignore = "needs an SSH server that allows TCP forwarding to itself"]
    fn shell_runs_commands_through_the_jump_host() {
        let text = run_shell(true);
        assert!(text.contains("hop-42 한글"), "{text}");
    }
}
