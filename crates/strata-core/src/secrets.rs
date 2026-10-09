//! NAS passwords in the operating system's keychain: Keychain on macOS,
//! Credential Manager on Windows and the Secret Service (GNOME Keyring,
//! KWallet) on Linux. Nothing is written to strata's own files.

use anyhow::{Context, Result};
use keyring::Entry;

const SERVICE: &str = "strata";

fn entry(connection: &str) -> Result<Entry> {
    if let Err(e) = Entry::store_status() {
        let hint = if cfg!(target_os = "linux") {
            " — start a Secret Service such as GNOME Keyring or KWallet (WSL has none by default)"
        } else {
            ""
        };
        anyhow::bail!("system keychain unavailable: {e}{hint}");
    }
    Entry::new(SERVICE, &format!("nas:{connection}")).context("system keychain unavailable")
}

/// The saved password for a connection, if any.
pub fn get(connection: &str) -> Option<String> {
    entry(connection).ok()?.get_password().ok()
}

pub fn set(connection: &str, password: &str) -> Result<()> {
    entry(connection)?.set_password(password).context("could not save to the system keychain")
}

/// Removes the saved password. Succeeds if there was none.
pub fn delete(connection: &str) -> Result<()> {
    match entry(connection)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("could not remove from the system keychain"),
    }
}
