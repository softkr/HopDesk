//! Connection profiles (jump host + RDP target) and their secrets.
//!
//! Non-secret settings live in `profiles.toml` under the per-user config directory.
//! Passwords never touch that file: they are stored in the OS credential store
//! (macOS Keychain, Windows Credential Manager, Linux Secret Service).

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const KEYRING_SERVICE: &str = "HopDesk";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    pub jump: JumpHost,
    pub target: RdpTarget,
    #[serde(default = "default_width")]
    pub width: u16,
    #[serde(default = "default_height")]
    pub height: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JumpHost {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Private key file. When absent, password authentication is used.
    pub key_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RdpTarget {
    /// Host name as seen from the jump host. Also used as the TLS / CredSSP server name.
    pub host: String,
    pub port: u16,
    pub user: String,
    pub domain: Option<String>,
}

fn default_width() -> u16 {
    1920
}

fn default_height() -> u16 {
    1080
}

/// Which stored secret of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secret {
    /// Jump host password, or private key passphrase when `key_path` is set.
    Jump,
    /// RDP (Windows) password.
    Rdp,
}

impl Secret {
    fn account(self, id: Uuid) -> String {
        match self {
            Secret::Jump => format!("{id}:jump"),
            Secret::Rdp => format!("{id}:rdp"),
        }
    }
}

impl Profile {
    pub fn secret(&self, which: Secret) -> anyhow::Result<Option<String>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &which.account(self.id))?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("read secret from the OS credential store"),
        }
    }

    pub fn set_secret(&self, which: Secret, value: &str) -> anyhow::Result<()> {
        keyring::Entry::new(KEYRING_SERVICE, &which.account(self.id))?
            .set_password(value)
            .context("write secret to the OS credential store")
    }

    pub fn delete_secrets(&self) -> anyhow::Result<()> {
        for which in [Secret::Jump, Secret::Rdp] {
            let entry = keyring::Entry::new(KEYRING_SERVICE, &which.account(self.id))?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(e).context("delete secret from the OS credential store"),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProfileStore {
    #[serde(default, rename = "profile")]
    profiles: Vec<Profile>,
}

impl ProfileStore {
    #[cfg(test)]
    pub fn from_profiles(profiles: Vec<Profile>) -> Self {
        Self { profiles }
    }

    fn path() -> anyhow::Result<PathBuf> {
        Ok(crate::config_dir()?.join("profiles.toml"))
    }

    pub fn load() -> anyhow::Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path()?;
        let text = toml::to_string_pretty(self)?;
        crate::write_private_file(&path, text.as_bytes())
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    pub fn get(&self, name: &str) -> anyhow::Result<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.name == name)
            .with_context(|| format!("no profile named `{name}`"))
    }

    pub fn add(&mut self, profile: Profile) -> anyhow::Result<()> {
        if self.profiles.iter().any(|p| p.name == profile.name) {
            bail!("a profile named `{}` already exists", profile.name);
        }
        self.profiles.push(profile);
        Ok(())
    }

    /// Adds the profile, or replaces the one with the same id (names stay unique).
    pub fn upsert(&mut self, profile: Profile) -> anyhow::Result<()> {
        if self.profiles.iter().any(|p| p.name == profile.name && p.id != profile.id) {
            bail!("a profile named `{}` already exists", profile.name);
        }
        match self.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
        Ok(())
    }

    /// Removes the profile and returns it so the caller can delete its secrets.
    pub fn remove(&mut self, name: &str) -> anyhow::Result<Profile> {
        let index = self
            .profiles
            .iter()
            .position(|p| p.name == name)
            .with_context(|| format!("no profile named `{name}`"))?;
        Ok(self.profiles.remove(index))
    }
}
