//! The session process of a profile (`hopdesk connect`), one module per remote control protocol.

mod rdp;
mod ssh;

use std::sync::Arc;

use anyhow::{Context as _, bail};

use crate::profile::{Profile, Protocol};
use crate::trust::TrustStore;
use crate::tunnel::Tunnel;

/// Runs the session window of `profile` until it closes.
///
/// `screen` is the client screen size in logical points; without `tunnel` the session connects
/// straight to the target.
pub fn run(
    profile: &Profile,
    screen: Option<(u16, u16)>,
    password: Option<String>,
    tunnel: Option<Arc<Tunnel>>,
    trust: Arc<TrustStore>,
) -> anyhow::Result<()> {
    match profile.protocol {
        Protocol::Rdp => {
            let password = password.context("no stored RDP password (use `set-password --rdp`)")?;
            rdp::run(profile, screen, password, tunnel, trust)
        }
        Protocol::Ssh => ssh::run(profile, password, tunnel, trust),
        Protocol::Vnc => bail!("VNC sessions are not supported yet"),
    }
}
