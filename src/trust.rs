//! Trust-on-first-use pinning for SSH host keys and RDP server certificates.
//!
//! The first time a host is seen its key/certificate fingerprint is recorded in `trust.toml`.
//! Later connections must present the same fingerprint, otherwise they are refused
//! (like OpenSSH `StrictHostKeyChecking=accept-new`).
//! Remove the entry with `hopdesk forget <host>` after a legitimate key change.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    /// `host:port` → OpenSSH-style `SHA256:...` fingerprint.
    #[serde(default)]
    ssh: BTreeMap<String, String>,
    /// RDP server name → SHA-256 of the DER leaf certificate (hex).
    #[serde(default)]
    rdp: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ssh,
    Rdp,
}

pub enum Verdict {
    /// Fingerprint matches the pinned one.
    Known,
    /// Host never seen before; fingerprint is now pinned.
    NewlyPinned,
    /// Pinned fingerprint differs: possible man-in-the-middle.
    Mismatch { pinned: String },
}

/// Process-wide store; the RDP certificate callback runs on a runtime worker thread.
pub struct TrustStore {
    path: PathBuf,
    file: Mutex<TrustFile>,
}

impl TrustStore {
    pub fn load() -> anyhow::Result<Self> {
        let path = crate::config_dir()?.join("trust.toml");
        let file = if path.exists() {
            let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?
        } else {
            TrustFile::default()
        };
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    pub fn check(&self, kind: Kind, host: &str, fingerprint: &str) -> anyhow::Result<Verdict> {
        let mut file = self.file.lock().expect("trust store lock poisoned");
        let map = match kind {
            Kind::Ssh => &mut file.ssh,
            Kind::Rdp => &mut file.rdp,
        };
        match map.get(host) {
            Some(pinned) if pinned == fingerprint => Ok(Verdict::Known),
            Some(pinned) => Ok(Verdict::Mismatch { pinned: pinned.clone() }),
            None => {
                map.insert(host.to_owned(), fingerprint.to_owned());
                let text = toml::to_string_pretty(&*file)?;
                crate::write_private_file(&self.path, text.as_bytes())?;
                Ok(Verdict::NewlyPinned)
            }
        }
    }

    /// Removes pinned fingerprints for `host`; returns how many entries were removed.
    pub fn forget(&self, host: &str) -> anyhow::Result<usize> {
        let mut file = self.file.lock().expect("trust store lock poisoned");
        let before = file.ssh.len() + file.rdp.len();
        // Keys are `host` or `host:port` depending on the source; match both forms.
        let matches = |k: &String| k == host || k.rsplit_once(':').map(|(h, _)| h) == Some(host);
        file.ssh.retain(|k, _| !matches(k));
        file.rdp.retain(|k, _| !matches(k));
        let removed = before - (file.ssh.len() + file.rdp.len());
        if removed > 0 {
            let text = toml::to_string_pretty(&*file)?;
            crate::write_private_file(&self.path, text.as_bytes())?;
        }
        Ok(removed)
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}
