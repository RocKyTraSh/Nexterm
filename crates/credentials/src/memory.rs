use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::backend::CredentialStore;
use crate::error::Result;
use crate::secret::Secret;

/// Ephemeral, in-process secret store.
///
/// Secrets live only in memory and are dropped (and zeroed) with the process.
/// This is the **secure-by-default** backend: nothing is ever written to disk.
/// Use the `keyring-os` backend for persistence.
#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    inner: Arc<Mutex<HashMap<Uuid, Secret>>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn set_secret(&self, id: Uuid, secret: &Secret) -> Result<()> {
        self.inner.lock().await.insert(id, secret.clone());
        Ok(())
    }

    async fn get_secret(&self, id: Uuid) -> Result<Option<Secret>> {
        Ok(self.inner.lock().await.get(&id).cloned())
    }

    async fn delete_secret(&self, id: Uuid) -> Result<()> {
        self.inner.lock().await.remove(&id);
        Ok(())
    }
}
