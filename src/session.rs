//! The session process of a profile (`hopdesk connect`), one module per remote control protocol.

mod rdp;

use std::sync::Arc;

use anyhow::bail;

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
    password: String,
    tunnel: Option<Arc<Tunnel>>,
    trust: Arc<TrustStore>,
) -> anyhow::Result<()> {
    match profile.protocol {
        Protocol::Rdp => rdp::run(profile, screen, password, tunnel, trust),
        Protocol::Ssh | Protocol::Vnc => bail!("{:?} sessions are not supported yet", profile.protocol),
    }
}
