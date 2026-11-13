//! Connection profiles (jump host + RDP target) and their secrets.
//!
//! Non-secret settings live in `profiles.toml` under the per-user config directory.
//! Passwords never touch that file: they are stored in the OS credential store
//! (macOS Keychain, Windows Credential Manager, Linux Secret Service); see [`crate::secret_store`].

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::secret_store;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub name: String,
    /// Connect through the SSH jump host; otherwise straight to the RDP target.
    /// The jump host settings are kept while disabled.
    #[serde(default = "default_use_jump")]
    pub use_jump: bool,
    pub jump: JumpHost,
    pub target: RdpTarget,
    #[serde(default = "default_width")]
    pub width: u16,
    #[serde(default = "default_height")]
    pub height: u16,
    #[serde(default)]
    pub display: DisplayMode,
}

/// How the session window starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DisplayMode {
    /// Window and session of `width` x `height`.
    Fixed,
    /// Maximized window, session sized to the monitor.
    Maximized,
    /// Borderless fullscreen, session sized to the monitor (Ctrl+Alt+Enter toggles).
    #[default]
    Fullscreen,
}

impl DisplayMode {
    pub fn label(self) -> &'static str {
        match self {
            DisplayMode::Fixed => "고정 해상도",
            DisplayMode::Maximized => "자동 (창 최대화)",
            DisplayMode::Fullscreen => "전체 화면",
        }
    }
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

fn default_use_jump() -> bool {
    true
}

impl JumpHost {
    /// Placeholder for a profile created without a jump host.
    pub fn unset() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            key_path: None,
        }
    }
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
        secret_store::get(&which.account(self.id))
    }

    pub fn set_secret(&self, which: Secret, value: &str) -> anyhow::Result<()> {
        secret_store::set(&which.account(self.id), value)
    }

    pub fn delete_secrets(&self) -> anyhow::Result<()> {
        secret_store::delete(&self.secret_accounts())
    }

    fn secret_accounts(&self) -> Vec<String> {
        [Secret::Jump, Secret::Rdp].map(|which| which.account(self.id)).into()
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProfileStore {
    #[serde(default, rename = "profile")]
    profiles: Vec<Profile>,
    /// Where the shared folder lives, when set somewhere other than the default under the
    /// home directory. Received files land here and the session shares it as a drive.
    #[serde(default)]
    shared_folder: Option<PathBuf>,
}

impl ProfileStore {
    #[cfg(test)]
    pub fn from_profiles(profiles: Vec<Profile>) -> Self {
        Self { profiles, shared_folder: None }
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

    /// The configured shared folder location, if the user picked one.
    pub fn shared_folder(&self) -> Option<&std::path::Path> {
        self.shared_folder.as_deref()
    }

    /// Sets (or, with `None`, clears) the shared folder location and saves the store.
    pub fn set_shared_folder(&mut self, folder: Option<PathBuf>) -> anyhow::Result<()> {
        self.shared_folder = folder;
        self.save()
    }

    /// Moves the profiles' secrets stored by earlier versions to where this version keeps them.
    pub fn migrate_secrets(&self) -> anyhow::Result<()> {
        let accounts: Vec<String> = self.profiles.iter().flat_map(Profile::secret_accounts).collect();
        secret_store::migrate(&accounts)
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

#[cfg(test)]
mod tests {
    use super::*;

    const PROFILE: &str = r#"
        [[profile]]
        id = "00000000-0000-0000-0000-000000000001"
        name = "office"
        jump = { host = "bastion", port = 22, user = "me" }
        target = { host = "10.0.0.5", port = 3389, user = "Administrator" }
    "#;

    #[test]
    fn profile_without_display_opens_fullscreen() {
        let store: ProfileStore = toml::from_str(PROFILE).expect("parse");
        assert_eq!(store.profiles()[0].display, DisplayMode::Fullscreen);
    }

    #[test]
    fn profile_without_use_jump_keeps_the_jump_host() {
        let store: ProfileStore = toml::from_str(PROFILE).expect("parse");
        assert!(store.profiles()[0].use_jump);
    }

    #[test]
    fn display_mode_round_trips_as_lowercase() {
        let mut store: ProfileStore = toml::from_str(PROFILE).expect("parse");
        store.profiles[0].display = DisplayMode::Maximized;
        let text = toml::to_string_pretty(&store).expect("serialize");
        assert!(text.contains("display = \"maximized\""), "{text}");
        let store: ProfileStore = toml::from_str(&text).expect("parse");
        assert_eq!(store.profiles()[0].display, DisplayMode::Maximized);
    }
}
