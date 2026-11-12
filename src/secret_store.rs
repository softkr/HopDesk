//! Profile secrets in the OS credential store.
//!
//! macOS keeps every secret in one Keychain item. An app without an Apple Team ID gets a Keychain
//! partition tied to its exact build, so after each update macOS asks for the login password once
//! per item; with one item that is one prompt per update. Earlier versions stored one item per
//! secret, and [`migrate`] moves those into the single item.
//!
//! Windows and Linux keep one item per secret: the Windows Credential Manager limits an item to
//! 2560 bytes, which a combined item for many profiles could exceed.

use anyhow::Context as _;

const SERVICE: &str = "HopDesk";

/// Items of the OS credential store, by account name under [`SERVICE`].
trait Items {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>>;
    fn set(&self, account: &str, value: &str) -> anyhow::Result<()>;
    /// A missing item is not an error.
    fn delete(&self, account: &str) -> anyhow::Result<()>;
}

/// The OS credential store, under a service name.
struct Keyring(&'static str);

impl Items for Keyring {
    fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
        match keyring::Entry::new(self.0, account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("read secret from the OS credential store"),
        }
    }

    fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
        keyring::Entry::new(self.0, account)?
            .set_password(value)
            .context("write secret to the OS credential store")
    }

    fn delete(&self, account: &str) -> anyhow::Result<()> {
        match keyring::Entry::new(self.0, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context("delete secret from the OS credential store"),
        }
    }
}

#[cfg(target_os = "macos")]
pub fn get(account: &str) -> anyhow::Result<Option<String>> {
    vault::get(&Keyring(SERVICE), account)
}

#[cfg(target_os = "macos")]
pub fn set(account: &str, value: &str) -> anyhow::Result<()> {
    vault::set(&Keyring(SERVICE), account, value)
}

#[cfg(target_os = "macos")]
pub fn delete(accounts: &[String]) -> anyhow::Result<()> {
    vault::delete(&Keyring(SERVICE), accounts)
}

/// Moves secrets stored by earlier versions into the single item. Only the launcher calls this,
/// so one process writes the item while sessions read it.
#[cfg(target_os = "macos")]
pub fn migrate(accounts: &[String]) -> anyhow::Result<()> {
    vault::migrate(&Keyring(SERVICE), accounts)
}

#[cfg(not(target_os = "macos"))]
pub fn get(account: &str) -> anyhow::Result<Option<String>> {
    Keyring(SERVICE).get(account)
}

#[cfg(not(target_os = "macos"))]
pub fn set(account: &str, value: &str) -> anyhow::Result<()> {
    Keyring(SERVICE).set(account, value)
}

#[cfg(not(target_os = "macos"))]
pub fn delete(accounts: &[String]) -> anyhow::Result<()> {
    accounts.iter().try_for_each(|account| Keyring(SERVICE).delete(account))
}

#[cfg(not(target_os = "macos"))]
pub fn migrate(_accounts: &[String]) -> anyhow::Result<()> {
    Ok(())
}

/// All secrets as a JSON object (account → secret) in the item named [`vault::ACCOUNT`].
#[cfg(any(target_os = "macos", test))]
mod vault {
    use std::collections::BTreeMap;

    use anyhow::Context as _;

    use super::Items;

    pub const ACCOUNT: &str = "secrets";

    type Vault = BTreeMap<String, String>;

    /// An unreadable vault is an error, so a write never replaces secrets it could not parse.
    fn load(items: &impl Items) -> anyhow::Result<Vault> {
        match items.get(ACCOUNT)? {
            Some(json) => serde_json::from_str(&json).context("parse the stored secrets"),
            None => Ok(Vault::new()),
        }
    }

    fn save(items: &impl Items, vault: &Vault) -> anyhow::Result<()> {
        items.set(ACCOUNT, &serde_json::to_string(vault)?)
    }

    pub fn get(items: &impl Items, account: &str) -> anyhow::Result<Option<String>> {
        match load(items)?.remove(account) {
            Some(value) => Ok(Some(value)),
            // Not moved yet: a session or command ran before the launcher migrated it.
            None => items.get(account),
        }
    }

    pub fn set(items: &impl Items, account: &str, value: &str) -> anyhow::Result<()> {
        let mut vault = load(items)?;
        vault.insert(account.to_owned(), value.to_owned());
        save(items, &vault)
    }

    pub fn delete(items: &impl Items, accounts: &[String]) -> anyhow::Result<()> {
        let mut vault = load(items)?;
        let before = vault.len();
        vault.retain(|account, _| !accounts.contains(account));
        if vault.len() != before {
            save(items, &vault)?;
        }
        accounts.iter().try_for_each(|account| items.delete(account))
    }

    /// Copies each old item into the vault, saves it, and only then deletes the old items. A value
    /// already in the vault was written later, so it wins and the old item is only deleted.
    pub fn migrate(items: &impl Items, accounts: &[String]) -> anyhow::Result<()> {
        let mut vault = load(items)?;
        let mut moved = false;
        for account in accounts {
            if vault.contains_key(account) {
                continue;
            }
            if let Some(value) = items.get(account)? {
                vault.insert(account.clone(), value);
                moved = true;
            }
        }
        if moved {
            save(items, &vault)?;
        }
        accounts
            .iter()
            .filter(|a| vault.contains_key(*a))
            .try_for_each(|account| items.delete(account))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::{Items, Keyring, vault};

    #[derive(Default)]
    struct Memory(RefCell<BTreeMap<String, String>>);

    impl Memory {
        fn with(items: &[(&str, &str)]) -> Self {
            Self(RefCell::new(items.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()))
        }

        fn has(&self, account: &str) -> bool {
            self.0.borrow().contains_key(account)
        }
    }

    impl Items for Memory {
        fn get(&self, account: &str) -> anyhow::Result<Option<String>> {
            Ok(self.0.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, value: &str) -> anyhow::Result<()> {
            self.0.borrow_mut().insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> anyhow::Result<()> {
            self.0.borrow_mut().remove(account);
            Ok(())
        }
    }

    fn accounts(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn secrets_are_kept_in_one_item() {
        let items = Memory::default();
        vault::set(&items, "a:rdp", "one").unwrap();
        vault::set(&items, "a:jump", "").unwrap();
        assert_eq!(vault::get(&items, "a:rdp").unwrap().as_deref(), Some("one"));
        assert_eq!(vault::get(&items, "a:jump").unwrap().as_deref(), Some(""));
        assert_eq!(vault::get(&items, "b:rdp").unwrap(), None);
        assert_eq!(items.0.borrow().keys().collect::<Vec<_>>(), [vault::ACCOUNT]);
    }

    #[test]
    fn migration_moves_old_items_into_the_vault() {
        let items = Memory::with(&[("a:rdp", "one"), ("a:jump", "two"), ("other", "kept")]);
        vault::migrate(&items, &accounts(&["a:rdp", "a:jump", "b:rdp"])).unwrap();
        assert!(!items.has("a:rdp") && !items.has("a:jump"));
        assert!(items.has("other"), "only the listed accounts are touched");
        assert_eq!(vault::get(&items, "a:rdp").unwrap().as_deref(), Some("one"));
        assert_eq!(vault::get(&items, "a:jump").unwrap().as_deref(), Some("two"));
        assert_eq!(vault::get(&items, "b:rdp").unwrap(), None);
    }

    #[test]
    fn a_newer_vault_value_wins_over_a_leftover_old_item() {
        let items = Memory::with(&[("a:rdp", "old")]);
        vault::set(&items, "a:rdp", "new").unwrap();
        vault::migrate(&items, &accounts(&["a:rdp"])).unwrap();
        assert!(!items.has("a:rdp"));
        assert_eq!(vault::get(&items, "a:rdp").unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn an_old_item_is_read_before_migration() {
        let items = Memory::with(&[("a:rdp", "one")]);
        assert_eq!(vault::get(&items, "a:rdp").unwrap().as_deref(), Some("one"));
    }

    #[test]
    fn delete_removes_vault_entries_and_old_items() {
        let items = Memory::with(&[("a:jump", "old")]);
        vault::set(&items, "a:rdp", "one").unwrap();
        vault::set(&items, "b:rdp", "two").unwrap();
        vault::delete(&items, &accounts(&["a:rdp", "a:jump"])).unwrap();
        assert_eq!(vault::get(&items, "a:rdp").unwrap(), None);
        assert_eq!(vault::get(&items, "a:jump").unwrap(), None);
        assert_eq!(vault::get(&items, "b:rdp").unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn an_unreadable_vault_is_never_overwritten() {
        let items = Memory::with(&[(vault::ACCOUNT, "not json")]);
        assert!(vault::set(&items, "a:rdp", "one").is_err());
        assert!(vault::migrate(&items, &accounts(&["a:rdp"])).is_err());
        assert_eq!(items.0.borrow().get(vault::ACCOUNT).map(String::as_str), Some("not json"));
    }

    /// Migrates through the real OS credential store under a separate service name, so the app's
    /// own secrets are not touched. Run with `cargo test -- --ignored real_credential_store`.
    #[test]
    #[ignore = "uses the OS credential store"]
    fn real_credential_store_migration() {
        let items = Keyring("HopDesk-test");
        let all = accounts(&["t:rdp", "t:jump", vault::ACCOUNT]);
        all.iter().for_each(|a| items.delete(a).unwrap());

        items.set("t:rdp", "one").unwrap();
        items.set("t:jump", "").unwrap();
        vault::migrate(&items, &accounts(&["t:rdp", "t:jump"])).unwrap();
        assert_eq!(items.get("t:rdp").unwrap(), None, "old item deleted");
        assert_eq!(vault::get(&items, "t:rdp").unwrap().as_deref(), Some("one"));
        assert_eq!(vault::get(&items, "t:jump").unwrap().as_deref(), Some(""));
        vault::delete(&items, &accounts(&["t:rdp", "t:jump"])).unwrap();
        assert_eq!(vault::get(&items, "t:rdp").unwrap(), None);

        all.iter().for_each(|a| items.delete(a).unwrap());
    }
}
