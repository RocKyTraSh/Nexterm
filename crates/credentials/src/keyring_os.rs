//! OS secret store backend.
//!
//! On Linux this talks to the Secret Service D-Bus API, which is implemented by
//! KWallet (KDE) and GNOME Keyring. The blocking `keyring` calls run on a
//! blocking thread pool so they never stall the async runtime (this is also why
//! we never do blocking secret I/O on a UI thread).
//!
//! API NOTE: `keyring` 3.x exposes `Entry::new`, `set_password`, `get_password`,
//! `delete_credential`, and a `keyring::Error::NoEntry` variant for missing
//! keys. Verify these against the pinned version; if they change, **only this
//! file** needs updating — everything else depends on `CredentialStore`.

use async_trait::async_trait;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::backend::CredentialStore;
use crate::error::{CredentialError, Result};
use crate::secret::Secret;

const SERVICE: &str = "rust-remote-suite";

/// Secret store backed by the operating system keyring.
#[derive(Clone, Default)]
pub struct OsKeyringStore;

impl OsKeyringStore {
    pub fn new() -> Self {
        Self
    }
}

fn entry_for(id: Uuid) -> std::result::Result<keyring::Entry, keyring::Error> {
    keyring::Entry::new(SERVICE, &id.to_string())
}

#[async_trait]
impl CredentialStore for OsKeyringStore {
    async fn set_secret(&self, id: Uuid, secret: &Secret) -> Result<()> {
        // Zeroizing<String> keeps the transient copy zeroed when dropped.
        let value = Zeroizing::new(secret.expose().to_string());
        tokio::task::spawn_blocking(move || {
            let entry = entry_for(id).map_err(|e| CredentialError::Backend(e.to_string()))?;
            entry
                .set_password(value.as_str())
                .map_err(|e| CredentialError::Backend(e.to_string()))
        })
        .await
        .map_err(|e| CredentialError::Join(e.to_string()))?
    }

    async fn get_secret(&self, id: Uuid) -> Result<Option<Secret>> {
        tokio::task::spawn_blocking(move || {
            let entry = entry_for(id).map_err(|e| CredentialError::Backend(e.to_string()))?;
            match entry.get_password() {
                Ok(pw) => Ok(Some(Secret::new(pw))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(CredentialError::Backend(e.to_string())),
            }
        })
        .await
        .map_err(|e| CredentialError::Join(e.to_string()))?
    }

    async fn delete_secret(&self, id: Uuid) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            let entry = entry_for(id).map_err(|e| CredentialError::Backend(e.to_string()))?;
            match entry.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(CredentialError::Backend(e.to_string())),
            }
        })
        .await
        .map_err(|e| CredentialError::Join(e.to_string()))?
    }
}
