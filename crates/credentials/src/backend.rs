use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Result;
use crate::secret::Secret;

/// Abstraction over a secret store.
///
/// Implementations: an ephemeral in-memory store (default) and the OS keyring
/// (`keyring-os` feature). A Windows Credential Manager backend will implement
/// this same trait without changing any caller.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Store (or replace) the secret for `id`.
    async fn set_secret(&self, id: Uuid, secret: &Secret) -> Result<()>;
    /// Retrieve the secret for `id`, or `None` if absent.
    async fn get_secret(&self, id: Uuid) -> Result<Option<Secret>>;
    /// Remove the secret for `id` (no error if absent).
    async fn delete_secret(&self, id: Uuid) -> Result<()>;
}
