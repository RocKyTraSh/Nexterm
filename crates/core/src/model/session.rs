use std::time::SystemTime;

use uuid::Uuid;

use super::profile::ProtocolKind;

/// Lifecycle state of a runtime session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Connecting,
    Connected,
    Disconnected,
    Failed(String),
}

/// A live connection instance, distinct from a saved `ConnectionProfile`.
///
/// Runtime-only: not serialized. The actual transport (SSH channel, PTY, ...)
/// is owned elsewhere; this is the registry-visible metadata.
#[derive(Debug, Clone)]
pub struct RuntimeSession {
    pub id: Uuid,
    pub profile_id: Option<Uuid>,
    pub title: String,
    pub kind: ProtocolKind,
    pub state: SessionState,
    pub created_at: SystemTime,
}

impl RuntimeSession {
    pub fn new(title: impl Into<String>, kind: ProtocolKind, profile_id: Option<Uuid>) -> Self {
        Self {
            id: Uuid::new_v4(),
            profile_id,
            title: title.into(),
            kind,
            state: SessionState::Connecting,
            created_at: SystemTime::now(),
        }
    }
}
