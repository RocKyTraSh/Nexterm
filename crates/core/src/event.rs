//! Application-wide event bus.
//!
//! Frontends (Qt/GTK/CLI) subscribe to [`AppEvent`]s instead of polling. The
//! bus is a `tokio::sync::broadcast` channel so multiple subscribers each get
//! their own copy.

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::model::SessionState;

/// Events emitted by core subsystems.
#[derive(Debug, Clone)]
pub enum AppEvent {
    SessionStateChanged {
        session_id: Uuid,
        state: SessionState,
    },
    SessionClosed {
        session_id: Uuid,
    },
    TunnelStatusChanged {
        tunnel_id: Uuid,
        status: String,
    },
    MiniServerStateChanged {
        server_id: Uuid,
        running: bool,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// A cloneable handle used to publish events.
#[derive(Debug, Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AppEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Subscribe to receive future events.
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }

    /// Publish an event. A send error means "no current subscribers", which is
    /// benign for a broadcast bus, so we intentionally ignore it.
    pub fn publish(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}
