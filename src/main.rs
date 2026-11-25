//! HopDesk: RDP desktop client that reaches Windows hosts through an SSH jump host, or directly.
//!
//! ```text
//! hopdesk                (no arguments: profile manager window)
//! hopdesk add <name> [--jump user@bastion[:22] [--jump-key ~/.ssh/id_ed25519]]
//!                        --target 10.0.0.5[:3389] --user Administrator [--domain CORP]
//! hopdesk add <name> --protocol ssh [--jump ...] --target 10.0.0.7[:22] --user me [--key ~/.ssh/id_ed25519]
//! hopdesk add <name> --protocol vnc [--jump ... [--user <macOS account>]] --target 10.0.0.9[:5900]
//! hopdesk list
//! hopdesk connect <name>
//! hopdesk set-password <name> --jump | --target
//! hopdesk remove <name>
//! hopdesk forget <host>
//! hopdesk set-ai-key [--remove]
//! ```

// Windows: no console window when the launcher is double-clicked (see `attach_parent_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod agent;
#[cfg(target_os = "macos")]
mod clipboard;
#[cfg(target_os = "macos")]
mod dock;
#[cfg(any(target_os = "macos", test))]
mod file_offer;
#[cfg(any(target_os = "macos", test))]
mod file_pull;
mod file_drop;
mod gui;
mod profile;
mod secret_store;
mod session;
mod shared_folder;
mod stats;
mod status;
mod terminal;
mod trust;
mod tunnel;
mod update;
mod vpn;

use std::fs;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::{Args, Parser, Subcommand};

use crate::profile::{DisplayMode, JumpHost, Profile, ProfileStore, Protocol, Secret, Target};
use crate::trust::TrustStore;
use crate::tunnel::{JumpParams, Tunnel};
use crate::vpn::VpnParams;

#[derive(Parser)]
#[command(version, name = "hopdesk", about = "HopDesk - RDP client through an SSH jump host")]
struct Cli {
    /// Without a subcommand the profile manager window opens.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Add a connection profile and store its passwords in the OS credential store.
    Add(AddArgs),
    /// List connection profiles.
    List,
    /// Remove a profile and delete its stored passwords.
    Remove { name: String },
    /// Replace a stored password.
    SetPassword {
        name: String,
        #[arg(long, conflicts_with = "rdp", required_unless_present = "rdp")]
        jump: bool,
        /// Password on the target (or its key passphrase); `--rdp` is the older name.
        #[arg(long, visible_alias = "target")]
        rdp: bool,
    },
    /// Connect using a profile (automatic login with the stored credentials).
    Connect {
        name: String,
        /// Screen size in logical points (`WIDTHxHEIGHT`) used as the initial session size for
        /// fullscreen and maximized profiles; the launcher passes its monitor size.
        #[arg(long, value_parser = parse_screen_size)]
        screen: Option<(u16, u16)>,
    },
    /// Forget pinned SSH host keys / RDP certificates of a host.
    Forget { host: String },
    /// Store the Anthropic API key for the AI assistant in RDP sessions in the OS credential store.
    SetAiKey {
        /// Delete the stored key instead; sessions then run without the assistant.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Args)]
struct AddArgs {
    name: String,
    /// Jump host as `user@host[:port]`; without it the profile connects straight to the target.
    #[arg(long)]
    jump: Option<String>,
    /// Private key for the jump host (password authentication when omitted).
    #[arg(long)]
    jump_key: Option<PathBuf>,
    /// WireGuard configuration file (`wg0.conf`): connect through the built-in VPN instead of a
    /// jump host. The private key goes to the OS credential store, not to the profile.
    #[arg(long, conflicts_with_all = ["jump", "jump_key"])]
    vpn_config: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t)]
    protocol: Protocol,
    /// Target as seen from the jump host, `host[:port]` (default port by protocol).
    #[arg(long)]
    target: String,
    /// User name on the target. VNC: a macOS account (through the jump host only); without it
    /// the VNC password is used.
    #[arg(long)]
    user: Option<String>,
    /// Windows domain (RDP).
    #[arg(long)]
    domain: Option<String>,
    /// Private key for the target (SSH; password authentication when omitted).
    #[arg(long)]
    key: Option<PathBuf>,
    #[arg(long, default_value_t = 1920)]
    width: u16,
    #[arg(long, default_value_t = 1080)]
    height: u16,
    /// How the session window starts; --width/--height apply to `fixed`.
    #[arg(long, value_enum, default_value_t)]
    display: DisplayMode,
}

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_parent_console();
    // One TLS crypto provider for the whole process: IronRDP's TLS and the AI client's HTTPS.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,russh=warn".into()),
        )
        // Plain text when the GUI captures a session's log output.
        .with_ansi(std::io::stderr().is_terminal())
        .init();

    let Some(command) = Cli::parse().command else {
        return gui::run();
    };
    match command {
        Command::Add(args) => add(args),
        Command::List => list(),
        Command::Remove { name } => remove(&name),
        Command::SetPassword { name, jump, .. } => {
            set_password(&name, if jump { Secret::Jump } else { Secret::Target })
        }
        Command::Connect { name, screen } => connect(&name, screen).inspect_err(|e| status::emit(status::Phase::Failed, &format!("{e:#}"))),
        Command::Forget { host } => {
            let removed = TrustStore::load()?.forget(&host)?;
            println!("removed {removed} pinned entr{} for `{host}`", if removed == 1 { "y" } else { "ies" });
            Ok(())
        }
        Command::SetAiKey { remove } => set_ai_key(remove),
    }
}

fn set_ai_key(remove: bool) -> anyhow::Result<()> {
    if remove {
        secret_store::delete(&[agent::API_KEY_ACCOUNT.to_owned()])?;
        println!("AI API key removed; sessions run without the assistant");
        return Ok(());
    }
    let key = rpassword::prompt_password("Anthropic API key: ")?;
    let key = key.trim();
    if key.is_empty() {
        bail!("API key must not be empty");
    }
    secret_store::set(agent::API_KEY_ACCOUNT, key)?;
    println!("AI API key stored; turn the assistant on in HopDesk's AI settings");
    Ok(())
}

fn add(args: AddArgs) -> anyhow::Result<()> {
    if args.domain.is_some() && args.protocol != Protocol::Rdp {
        bail!("--domain applies to RDP only");
    }
    if args.key.is_some() && args.protocol != Protocol::Ssh {
        bail!("--key applies to SSH only");
    }
    let user = match (args.user, args.protocol) {
        (Some(user), _) => user,
        (None, Protocol::Vnc) => String::new(),
        (None, _) => bail!("--user is required"),
    };
    // A macOS account password must not cross the network without the SSH channel or the VPN.
    if args.protocol == Protocol::Vnc && !user.is_empty() && args.jump.is_none() && args.vpn_config.is_none() {
        bail!("a macOS account (--user) needs --jump or --vpn-config; without it the VNC password is used");
    }
    let vpn = args
        .vpn_config
        .as_deref()
        .map(|path| {
            let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            vpn::parse_config(&text).with_context(|| format!("read {}", path.display()))
        })
        .transpose()?;
    let mut store = ProfileStore::load()?;
    let jump = match &args.jump {
        Some(jump) => {
            let (user, host, port) = parse_user_host_port(jump, 22)?;
            JumpHost {
                host,
                port,
                user,
                key_path: args.jump_key,
            }
        }
        None => JumpHost::unset(),
    };
    let (target_host, target_port) = parse_host_port(&args.target, args.protocol.default_port())?;

    let profile = Profile {
        id: uuid::Uuid::new_v4(),
        name: args.name,
        protocol: args.protocol,
        use_jump: args.jump.is_some(),
        jump,
        vpn: vpn.as_ref().map(|config| profile::Vpn {
            endpoint: config.endpoint.clone(),
            addresses: config.addresses.clone(),
            peer_public_key: config.peer_public_key.clone(),
            keepalive: config.keepalive,
        }),
        target: Target {
            host: target_host,
            port: target_port,
            user,
            domain: args.domain,
            key_path: args.key,
        },
        width: args.width,
        height: args.height,
        display: args.display,
    };
    store.add(profile.clone())?;

    if profile.use_jump {
        prompt_and_store(&profile, Secret::Jump)?;
    }
    if let Some(config) = &vpn {
        profile.set_secret(Secret::Vpn, &config.private_key)?;
    }
    prompt_and_store(&profile, Secret::Target)?;
    store.save()?;
    println!("added profile `{}`", profile.name);
    Ok(())
}

fn list() -> anyhow::Result<()> {
    let store = ProfileStore::load()?;
    if store.profiles().is_empty() {
        println!("no profiles (use `hopdesk add`)");
    }
    for p in store.profiles() {
        let route = if let Some(vpn) = &p.vpn {
            format!("vpn {}", vpn.endpoint)
        } else if p.use_jump {
            let auth = if p.jump.key_path.is_some() { "key" } else { "password" };
            format!("{}@{}:{} ({auth})", p.jump.user, p.jump.host, p.jump.port)
        } else {
            "direct".to_owned()
        };
        // A VNC profile without a user logs on with the VNC password only.
        let account = match (&p.target.domain, p.target.user.as_str()) {
            (_, "") => String::new(),
            (Some(domain), user) => format!("{domain}\\{user}@"),
            (None, user) => format!("{user}@"),
        };
        println!(
            "{:<16} {:<4} {route} -> {account}{}:{}",
            p.name,
            protocol_name(p.protocol),
            p.target.host,
            p.target.port,
        );
    }
    Ok(())
}

fn remove(name: &str) -> anyhow::Result<()> {
    let mut store = ProfileStore::load()?;
    let profile = store.remove(name)?;
    profile.delete_secrets()?;
    store.save()?;
    println!("removed profile `{name}` and its stored passwords");
    Ok(())
}

fn set_password(name: &str, which: Secret) -> anyhow::Result<()> {
    let store = ProfileStore::load()?;
    prompt_and_store(store.get(name)?, which)?;
    println!("password updated");
    Ok(())
}

fn connect(name: &str, screen: Option<(u16, u16)>) -> anyhow::Result<()> {
    let store = ProfileStore::load()?;
    let profile = store.get(name)?.clone();
    let trust = Arc::new(TrustStore::load()?);

    let password = profile.secret(Secret::Target)?;

    let tunnel = if let Some(vpn) = &profile.vpn {
        let private_key = profile
            .secret(Secret::Vpn)?
            .filter(|key| !key.is_empty())
            .context("no stored VPN private key (edit the profile and enter the key again)")?;
        Some(Tunnel::vpn(VpnParams {
            endpoint: vpn.endpoint.clone(),
            addresses: vpn.addresses.clone(),
            peer_public_key: vpn.peer_public_key.clone(),
            private_key,
            keepalive: vpn.keepalive,
        }))
    } else if profile.use_jump {
        let jump_secret = profile.secret(Secret::Jump)?.filter(|s| !s.is_empty());
        Some(Tunnel::jump(
            JumpParams {
                host: profile.jump.host.clone(),
                port: profile.jump.port,
                user: profile.jump.user.clone(),
                key_path: profile.jump.key_path.clone(),
                secret: jump_secret,
            },
            Arc::clone(&trust),
        ))
    } else {
        None
    };

    session::run(&profile, screen, password, tunnel, trust)
}

fn prompt_and_store(profile: &Profile, which: Secret) -> anyhow::Result<()> {
    let key_based = match which {
        Secret::Jump => profile.jump.key_path.is_some(),
        Secret::Target => profile.target.key_path.is_some(),
        Secret::Vpn => unreachable!("the VPN key is read from a file, never typed"),
    };
    let prompt = match which {
        Secret::Vpn => unreachable!("the VPN key is read from a file, never typed"),
        _ if key_based => "Private key passphrase (empty if none): ".to_owned(),
        Secret::Jump => format!("Jump host password for {}@{}: ", profile.jump.user, profile.jump.host),
        Secret::Target if profile.protocol == Protocol::Vnc && profile.target.user.is_empty() => {
            format!("VNC password for {} (empty if none): ", profile.target.host)
        }
        Secret::Target if profile.protocol == Protocol::Vnc => {
            format!("macOS password for {}@{}: ", profile.target.user, profile.target.host)
        }
        Secret::Target => format!(
            "{} password for {}@{}: ",
            protocol_name(profile.protocol).to_uppercase(),
            profile.target.user,
            profile.target.host
        ),
    };
    let value = rpassword::prompt_password(prompt)?;
    // A VNC server may ask for no password at all.
    let optional = key_based || (which == Secret::Target && profile.protocol == Protocol::Vnc && profile.target.user.is_empty());
    if value.is_empty() && !optional {
        bail!("password must not be empty");
    }
    profile.set_secret(which, &value)
}

fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Rdp => "rdp",
        Protocol::Ssh => "ssh",
        Protocol::Vnc => "vnc",
    }
}

fn parse_host_port(s: &str, default_port: u16) -> anyhow::Result<(String, u16)> {
    match s.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            Ok((host.to_owned(), port.parse().with_context(|| format!("invalid port in `{s}`"))?))
        }
        _ => Ok((s.trim_matches(['[', ']']).to_owned(), default_port)),
    }
}

/// Parses `WIDTHxHEIGHT` within the range RDP accepts for a desktop.
fn parse_screen_size(s: &str) -> Result<(u16, u16), String> {
    const MIN: u16 = 200;
    const MAX: u16 = 8192;
    let (w, h) = s.split_once('x').ok_or_else(|| format!("expected WIDTHxHEIGHT, got `{s}`"))?;
    let parse = |v: &str| {
        v.trim()
            .parse::<u16>()
            .ok()
            .filter(|n| (MIN..=MAX).contains(n))
            .ok_or_else(|| format!("size must be {MIN}–{MAX}, got `{v}`"))
    };
    Ok((parse(w)?, parse(h)?))
}

fn parse_user_host_port(s: &str, default_port: u16) -> anyhow::Result<(String, String, u16)> {
    let (user, rest) = s.split_once('@').with_context(|| format!("expected `user@host[:port]`, got `{s}`"))?;
    let (host, port) = parse_host_port(rest, default_port)?;
    Ok((user.to_owned(), host, port))
}

/// The Windows build uses the GUI subsystem, so it starts without a console. When it is run from
/// a shell, attach to that shell's console so command-line output and password prompts work.
/// Handles the parent already passed in (the launcher pipes the stderr of `connect`) are kept.
#[cfg(windows)]
fn attach_parent_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: plain Win32 calls without pointer arguments.
    unsafe {
        if GetStdHandle(STD_ERROR_HANDLE).is_null() {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Per-user configuration directory, created on demand.
pub fn config_dir() -> anyhow::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "HopDesk").context("no home directory")?;
    let dir = dirs.config_dir().to_owned();
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    Ok(dir)
}

/// Writes a file readable only by the current user (0600 on Unix).
pub fn write_private_file(path: &Path, content: &[u8]) -> anyhow::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).with_context(|| format!("open {}", path.display()))?;
    file.write_all(content).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::parse_screen_size;

    #[test]
    fn screen_size_parses_within_rdp_limits() {
        assert_eq!(parse_screen_size("1470x956"), Ok((1470, 956)));
        assert!(parse_screen_size("1470").is_err());
        assert!(parse_screen_size("100x956").is_err());
        assert!(parse_screen_size("9000x956").is_err());
    }
}
