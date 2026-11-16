//! SSH jump host tunnel: one SSH session, one `direct-tcpip` channel per connection attempt to the target.

use core::time::Duration;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, load_secret_key};
use tokio::io::DuplexStream;
use tracing::{error, info, warn};

use crate::trust::{Kind, TrustStore, Verdict};

pub struct JumpParams {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: Option<PathBuf>,
    /// Password, or key passphrase when `key_path` is set.
    pub secret: Option<String>,
}

/// Which SSH server a host key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Jump,
    /// The remote host of an SSH profile.
    Target,
}

/// Pins the server's host key on first use and refuses a changed one.
pub struct HostKeyVerifier {
    host_port: String,
    trust: Arc<TrustStore>,
    role: Role,
}

impl HostKeyVerifier {
    pub fn new(host: &str, port: u16, trust: Arc<TrustStore>, role: Role) -> Self {
        Self {
            host_port: format!("{host}:{port}"),
            trust,
            role,
        }
    }
}

impl client::Handler for HostKeyVerifier {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        match self.trust.check(Kind::Ssh, &self.host_port, &fingerprint)? {
            Verdict::Known => Ok(true),
            Verdict::NewlyPinned => {
                warn!(host = %self.host_port, %fingerprint, "Pinned new SSH host key (first connection)");
                Ok(true)
            }
            Verdict::Mismatch { pinned } => {
                // The launcher tells the two apart by these messages.
                match self.role {
                    Role::Jump => error!(
                        host = %self.host_port, %fingerprint, %pinned,
                        "SSH HOST KEY CHANGED - possible man-in-the-middle; refusing to connect \
                         (run `hopdesk forget <host>` if the change is legitimate)"
                    ),
                    Role::Target => error!(
                        host = %self.host_port, %fingerprint, %pinned,
                        "TARGET SSH HOST KEY CHANGED - possible man-in-the-middle; refusing to connect \
                         (run `hopdesk forget <host>` if the change is legitimate)"
                    ),
                }
                Ok(false)
            }
        }
    }
}

/// SSH client settings shared by the jump host and SSH sessions.
pub fn client_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        // Input and frame acknowledgements are small packets; Nagle would delay them.
        nodelay: true,
        ..Default::default()
    })
}

/// How to log on to an SSH server.
pub enum Credential<'a> {
    Key { path: &'a Path, passphrase: Option<&'a str> },
    Password(&'a str),
}

/// Authenticates `user`; `Ok(false)` when the server rejects the credential.
pub async fn authenticate(
    handle: &mut Handle<HostKeyVerifier>,
    user: &str,
    credential: Credential<'_>,
) -> anyhow::Result<bool> {
    let result = match credential {
        Credential::Key { path, passphrase } => {
            let key = load_secret_key(path, passphrase).with_context(|| format!("load private key {}", path.display()))?;
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            handle
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
        }
        Credential::Password(password) => handle.authenticate_password(user, password).await?,
    };
    Ok(result.success())
}

pub struct Tunnel {
    params: JumpParams,
    trust: Arc<TrustStore>,
    handle: tokio::sync::Mutex<Option<Handle<HostKeyVerifier>>>,
}

impl Tunnel {
    pub fn new(params: JumpParams, trust: Arc<TrustStore>) -> Arc<Self> {
        Arc::new(Self {
            params,
            trust,
            handle: tokio::sync::Mutex::new(None),
        })
    }

    async fn connect_ssh(&self) -> anyhow::Result<Handle<HostKeyVerifier>> {
        let p = &self.params;
        let verifier = HostKeyVerifier::new(&p.host, p.port, Arc::clone(&self.trust), Role::Jump);

        info!(host = %p.host, port = p.port, user = %p.user, "Connecting to SSH jump host");
        let mut handle = client::connect(client_config(), (p.host.as_str(), p.port), verifier)
            .await
            .context("SSH connect to jump host")?;

        let credential = match &p.key_path {
            Some(path) => Credential::Key { path, passphrase: p.secret.as_deref() },
            None => Credential::Password(
                p.secret
                    .as_deref()
                    .context("no stored jump host password (use `set-password --jump`)")?,
            ),
        };
        ensure!(authenticate(&mut handle, &p.user, credential).await?, "jump host authentication failed");

        info!("SSH jump host authenticated");
        Ok(handle)
    }

    /// Establishes the SSH session now so configuration errors surface before the window opens.
    pub async fn ensure_connected(&self) -> anyhow::Result<()> {
        let mut guard = self.handle.lock().await;
        if guard.as_ref().is_none_or(|h| h.is_closed()) {
            *guard = Some(self.connect_ssh().await?);
        }
        Ok(())
    }

    /// Opens a `direct-tcpip` channel from the jump host to `host:port`.
    ///
    /// Re-establishes the SSH session when it was closed, so auto-reconnect keeps working.
    pub async fn open(&self, host: &str, port: u16) -> io::Result<DuplexStream> {
        let mut guard = self.handle.lock().await;
        if guard.as_ref().is_none_or(|h| h.is_closed()) {
            *guard = Some(self.connect_ssh().await.map_err(|e| io::Error::other(format!("{e:#}")))?);
        }
        let handle = guard.as_ref().expect("SSH handle set above");

        info!(%host, port, "Opening SSH direct-tcpip channel");
        let channel = handle
            .channel_open_direct_tcpip(host, u32::from(port), "127.0.0.1", 0)
            .await
            .map_err(|e| io::Error::other(format!("open direct-tcpip channel to {host}:{port}: {e}")))?;

        // Bridge through an in-memory duplex so callers get a `Send + Sync` stream.
        let mut channel_stream = channel.into_stream();
        let (near, mut far) = tokio::io::duplex(256 * 1024);
        tokio::spawn(async move {
            if let Err(e) = tokio::io::copy_bidirectional(&mut channel_stream, &mut far).await {
                tracing::debug!(error = %e, "SSH channel bridge closed");
            }
        });

        Ok(near)
    }
}
