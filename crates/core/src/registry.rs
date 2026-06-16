//! Thread-safe registry of active runtime sessions.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use crate::event::{AppEvent, EventBus};
use crate::model::{RuntimeSession, SessionState};

/// In-memory registry of live sessions, shared across the app via `Arc`.
#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<RwLock<HashMap<Uuid, RuntimeSession>>>,
    bus: EventBus,
}

impl SessionRegistry {
    pub fn new(bus: EventBus) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            bus,
        }
    }

    pub async fn register(&self, session: RuntimeSession) -> Uuid {
        let id = session.id;
        self.inner.write().await.insert(id, session);
        id
    }

    pub async fn set_state(&self, id: Uuid, state: SessionState) {
        if let Some(s) = self.inner.write().await.get_mut(&id) {
            s.state = state.clone();
            self.bus.publish(AppEvent::SessionStateChanged {
                session_id: id,
                state,
            });
        }
    }

    pub async fn get(&self, id: Uuid) -> Option<RuntimeSession> {
        self.inner.read().await.get(&id).cloned()
    }

    pub async fn list(&self) -> Vec<RuntimeSession> {
        self.inner.read().await.values().cloned().collect()
    }

    pub async fn remove(&self, id: Uuid) -> Option<RuntimeSession> {
        let removed = self.inner.write().await.remove(&id);
        if removed.is_some() {
            self.bus.publish(AppEvent::SessionClosed { session_id: id });
        }
        removed
    }
}
