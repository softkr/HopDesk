//! SSH jump host tunnel: one SSH session, one `direct-tcpip` channel per RDP connection attempt.

use core::net::SocketAddr;
use core::time::Duration;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, ensure};
use ironrdp_client::config::BoxedRdpStream;
use russh::client::{self, Handle};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, load_secret_key};
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

struct HostKeyVerifier {
    host_port: String,
    trust: Arc<TrustStore>,
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
                error!(
                    host = %self.host_port, %fingerprint, %pinned,
                    "SSH HOST KEY CHANGED - possible man-in-the-middle; refusing to connect \
                     (run `hopdesk forget <host>` if the change is legitimate)"
                );
                Ok(false)
            }
        }
    }
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
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            ..Default::default()
        });
        let verifier = HostKeyVerifier {
            host_port: format!("{}:{}", p.host, p.port),
            trust: Arc::clone(&self.trust),
        };

        info!(host = %p.host, port = p.port, user = %p.user, "Connecting to SSH jump host");
        let mut handle = client::connect(config, (p.host.as_str(), p.port), verifier)
            .await
            .context("SSH connect to jump host")?;

        let result = if let Some(key_path) = &p.key_path {
            let key = load_secret_key(key_path, p.secret.as_deref())
                .with_context(|| format!("load private key {}", key_path.display()))?;
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            handle
                .authenticate_publickey(&p.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
        } else {
            let password = p
                .secret
                .as_deref()
                .context("no stored jump host password (use `set-password --jump`)")?;
            handle.authenticate_password(&p.user, password).await?
        };
        ensure!(result.success(), "jump host authentication failed");

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
    /// Re-establishes the SSH session when it was closed, so RDP auto-reconnect keeps working.
    pub async fn open(&self, host: &str, port: u16) -> io::Result<(BoxedRdpStream, SocketAddr)> {
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

        // Bridge through an in-memory duplex so the engine gets a `Send + Sync` stream.
        let mut channel_stream = channel.into_stream();
        let (near, mut far) = tokio::io::duplex(256 * 1024);
        tokio::spawn(async move {
            if let Err(e) = tokio::io::copy_bidirectional(&mut channel_stream, &mut far).await {
                tracing::debug!(error = %e, "SSH channel bridge closed");
            }
        });

        // The real client address is the jump host's; RDP only uses this for Client Info.
        Ok((Box::new(near), SocketAddr::from(([127, 0, 0, 1], 0))))
    }
}
