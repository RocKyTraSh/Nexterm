//! `rrs-credentials`: secret storage abstraction.
//!
//! - [`Secret`] — zero-on-drop secret wrapper that never logs its value.
//! - [`CredentialStore`] — backend trait.
//! - [`MemoryCredentialStore`] — ephemeral default (no disk persistence).
//! - `OsKeyringStore` (feature `keyring-os`) — OS secret service backend.

mod backend;
mod error;
mod memory;
mod secret;

#[cfg(feature = "keyring-os")]
mod keyring_os;

use std::sync::Arc;

pub use backend::CredentialStore;
pub use error::{CredentialError, Result};
pub use memory::MemoryCredentialStore;
pub use secret::Secret;

#[cfg(feature = "keyring-os")]
pub use keyring_os::OsKeyringStore;

/// Construct the recommended credential store for this build.
///
/// Returns the OS keyring when compiled with `keyring-os`, otherwise the
/// ephemeral in-memory store.
pub fn default_store() -> Arc<dyn CredentialStore> {
    #[cfg(feature = "keyring-os")]
    {
        Arc::new(OsKeyringStore::new())
    }
    #[cfg(not(feature = "keyring-os"))]
    {
        Arc::new(MemoryCredentialStore::new())
    }
}
