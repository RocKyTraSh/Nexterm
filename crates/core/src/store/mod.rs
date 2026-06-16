//! Persistence of profiles and groups.
//!
//! The [`ProfileStore`] trait abstracts storage so the MVP can use a JSON file
//! while a future SQLite backend (with migrations) slots in behind the same
//! interface. **No secrets are ever written here** — profiles only carry
//! `CredentialRef`s.

mod file;

pub use file::FileProfileStore;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::Result;
use crate::model::{ConnectionProfile, Group};

#[async_trait]
pub trait ProfileStore: Send + Sync {
    async fn list_profiles(&self) -> Result<Vec<ConnectionProfile>>;
    async fn get_profile(&self, id: Uuid) -> Result<Option<ConnectionProfile>>;
    async fn upsert_profile(&self, profile: ConnectionProfile) -> Result<()>;
    async fn delete_profile(&self, id: Uuid) -> Result<()>;

    async fn list_groups(&self) -> Result<Vec<Group>>;
    async fn upsert_group(&self, group: Group) -> Result<()>;
    async fn delete_group(&self, id: Uuid) -> Result<()>;
}
