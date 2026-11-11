//! HopDesk: RDP desktop client that reaches Windows hosts through an SSH jump host.
//!
//! ```text
//! hopdesk                (no arguments: profile manager window)
//! hopdesk add <name> --jump user@bastion[:22] [--jump-key ~/.ssh/id_ed25519]
//!                        --target 10.0.0.5[:3389] --user Administrator [--domain CORP]
//! hopdesk list
//! hopdesk connect <name>
//! hopdesk set-password <name> --jump | --rdp
//! hopdesk remove <name>
//! hopdesk forget <host>
//! ```

// Windows: no console window when the launcher is double-clicked (see `attach_parent_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod gui;
mod profile;
mod session;
mod status;
mod trust;
mod tunnel;

use std::fs;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, bail};
use clap::{Args, Parser, Subcommand};

use crate::profile::{JumpHost, Profile, ProfileStore, RdpTarget, Secret};
use crate::trust::TrustStore;
use crate::tunnel::{JumpParams, Tunnel};

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
        #[arg(long)]
        rdp: bool,
    },
    /// Connect using a profile (automatic login with the stored credentials).
    Connect { name: String },
    /// Forget pinned SSH host keys / RDP certificates of a host.
    Forget { host: String },
}

#[derive(Args)]
struct AddArgs {
    name: String,
    /// Jump host as `user@host[:port]`.
    #[arg(long)]
    jump: String,
    /// Private key for the jump host (password authentication when omitted).
    #[arg(long)]
    jump_key: Option<PathBuf>,
    /// RDP target as seen from the jump host, `host[:port]`.
    #[arg(long)]
    target: String,
    /// RDP user name.
    #[arg(long)]
    user: String,
    /// RDP domain.
    #[arg(long)]
    domain: Option<String>,
    #[arg(long, default_value_t = 1920)]
    width: u16,
    #[arg(long, default_value_t = 1080)]
    height: u16,
}

fn main() -> anyhow::Result<()> {
    #[cfg(windows)]
    attach_parent_console();

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
            set_password(&name, if jump { Secret::Jump } else { Secret::Rdp })
        }
        Command::Connect { name } => connect(&name).inspect_err(|e| status::emit(status::Phase::Failed, &format!("{e:#}"))),
        Command::Forget { host } => {
            let removed = TrustStore::load()?.forget(&host)?;
            println!("removed {removed} pinned entr{} for `{host}`", if removed == 1 { "y" } else { "ies" });
            Ok(())
        }
    }
}

fn add(args: AddArgs) -> anyhow::Result<()> {
    let mut store = ProfileStore::load()?;
    let (jump_user, jump_host, jump_port) = parse_user_host_port(&args.jump, 22)?;
    let (target_host, target_port) = parse_host_port(&args.target, 3389)?;

    let profile = Profile {
        id: uuid::Uuid::new_v4(),
        name: args.name,
        jump: JumpHost {
            host: jump_host,
            port: jump_port,
            user: jump_user,
            key_path: args.jump_key,
        },
        target: RdpTarget {
            host: target_host,
            port: target_port,
            user: args.user,
            domain: args.domain,
        },
        width: args.width,
        height: args.height,
    };
    store.add(profile.clone())?;

    prompt_and_store(&profile, Secret::Jump)?;
    prompt_and_store(&profile, Secret::Rdp)?;
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
        let auth = if p.jump.key_path.is_some() { "key" } else { "password" };
        println!(
            "{:<16} {}@{}:{} ({auth}) -> {}{}@{}:{}",
            p.name,
            p.jump.user,
            p.jump.host,
            p.jump.port,
            p.target.domain.as_deref().map(|d| format!("{d}\\")).unwrap_or_default(),
            p.target.user,
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

fn connect(name: &str) -> anyhow::Result<()> {
    let store = ProfileStore::load()?;
    let profile = store.get(name)?.clone();
    let trust = Arc::new(TrustStore::load()?);

    let jump_secret = profile.secret(Secret::Jump)?.filter(|s| !s.is_empty());
    let rdp_password = profile
        .secret(Secret::Rdp)?
        .context("no stored RDP password (use `set-password --rdp`)")?;

    let tunnel = Tunnel::new(
        JumpParams {
            host: profile.jump.host.clone(),
            port: profile.jump.port,
            user: profile.jump.user.clone(),
            key_path: profile.jump.key_path.clone(),
            secret: jump_secret,
        },
        Arc::clone(&trust),
    );

    session::run(&profile, rdp_password, tunnel, trust)
}

fn prompt_and_store(profile: &Profile, which: Secret) -> anyhow::Result<()> {
    let prompt = match (which, profile.jump.key_path.is_some()) {
        (Secret::Jump, false) => format!("Jump host password for {}@{}: ", profile.jump.user, profile.jump.host),
        (Secret::Jump, true) => "Private key passphrase (empty if none): ".to_owned(),
        (Secret::Rdp, _) => format!("RDP password for {}@{}: ", profile.target.user, profile.target.host),
    };
    let value = rpassword::prompt_password(prompt)?;
    if value.is_empty() && !(which == Secret::Jump && profile.jump.key_path.is_some()) {
        bail!("password must not be empty");
    }
    profile.set_secret(which, &value)
}

fn parse_host_port(s: &str, default_port: u16) -> anyhow::Result<(String, u16)> {
    match s.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => {
            Ok((host.to_owned(), port.parse().with_context(|| format!("invalid port in `{s}`"))?))
        }
        _ => Ok((s.trim_matches(['[', ']']).to_owned(), default_port)),
    }
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
