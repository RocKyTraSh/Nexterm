use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::{CoreError, Result};
use crate::model::{ConnectionProfile, Group};

use super::ProfileStore;

/// On-disk shape of the profile database (secret-free).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct StoreData {
    #[serde(default)]
    profiles: Vec<ConnectionProfile>,
    #[serde(default)]
    groups: Vec<Group>,
}

/// JSON-file backed [`ProfileStore`].
///
/// Simple and dependency-free; suitable for the MVP. A SQLite backend with
/// migrations is planned for v0.2 behind the same trait.
pub struct FileProfileStore {
    path: PathBuf,
    // Serialize all writes; the whole file is rewritten atomically.
    lock: Mutex<()>,
}

impl FileProfileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), lock: Mutex::new(()) }
    }

    /// Default location: `<data_dir>/profiles.json`.
    pub fn default_store() -> Self {
        Self::new(rrs_platform::data_dir().join("profiles.json"))
    }

    async fn read_data(&self) -> Result<StoreData> {
        match tokio::fs::read_to_string(&self.path).await {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreData::default()),
            Err(e) => Err(CoreError::Io(e)),
        }
    }

    async fn write_data(&self, data: &StoreData) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let text = serde_json::to_string_pretty(data)?;
        // Write to a temp file then rename for atomicity.
        let tmp = tmp_path(&self.path);
        tokio::fs::write(&tmp, text).await?;
        tokio::fs::rename(&tmp, &self.path).await?;
        Ok(())
    }
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

#[async_trait]
impl ProfileStore for FileProfileStore {
    async fn list_profiles(&self) -> Result<Vec<ConnectionProfile>> {
        Ok(self.read_data().await?.profiles)
    }

    async fn get_profile(&self, id: Uuid) -> Result<Option<ConnectionProfile>> {
        Ok(self.read_data().await?.profiles.into_iter().find(|p| p.id == id))
    }

    async fn upsert_profile(&self, profile: ConnectionProfile) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut data = self.read_data().await?;
        if let Some(existing) = data.profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            data.profiles.push(profile);
        }
        self.write_data(&data).await
    }

    async fn delete_profile(&self, id: Uuid) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut data = self.read_data().await?;
        data.profiles.retain(|p| p.id != id);
        self.write_data(&data).await
    }

    async fn list_groups(&self) -> Result<Vec<Group>> {
        Ok(self.read_data().await?.groups)
    }

    async fn upsert_group(&self, group: Group) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut data = self.read_data().await?;
        if let Some(existing) = data.groups.iter_mut().find(|g| g.id == group.id) {
            *existing = group;
        } else {
            data.groups.push(group);
        }
        self.write_data(&data).await
    }

    async fn delete_group(&self, id: Uuid) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut data = self.read_data().await?;
        data.groups.retain(|g| g.id != id);
        self.write_data(&data).await
    }
}

// Note on the storage layer: the `_guard` held across read+write serializes
// concurrent upserts/deletes (last-writer-wins is avoided within the process).
