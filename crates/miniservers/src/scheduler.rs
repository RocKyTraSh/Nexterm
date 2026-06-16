//! A minimal interval scheduler mini-server.
//!
//! Demonstrates the framework with no extra dependencies: registered callbacks
//! run every N seconds on the async runtime. Full cron support is a follow-up.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::info;

use crate::error::Result;
use crate::service::{MiniServer, MiniServerConfig, ServerState};

/// A periodic task.
pub struct ScheduledTask {
    pub name: String,
    pub every: Duration,
    /// Work to perform on each tick.
    pub action: Arc<dyn Fn() + Send + Sync>,
}

/// Runs [`ScheduledTask`]s on fixed intervals.
pub struct SchedulerServer {
    config: MiniServerConfig,
    state: ServerState,
    tasks: Vec<ScheduledTask>,
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl SchedulerServer {
    pub fn new(config: MiniServerConfig, tasks: Vec<ScheduledTask>) -> Self {
        Self { config, state: ServerState::Stopped, tasks, handles: Arc::new(Mutex::new(Vec::new())) }
    }
}

#[async_trait]
impl MiniServer for SchedulerServer {
    fn config(&self) -> &MiniServerConfig {
        &self.config
    }

    fn state(&self) -> ServerState {
        self.state
    }

    async fn start(&mut self) -> Result<()> {
        if self.state == ServerState::Running {
            return Ok(());
        }
        let mut handles = self.handles.lock().await;
        for task in &self.tasks {
            let action = Arc::clone(&task.action);
            let every = task.every;
            let name = task.name.clone();
            info!(task = %name, ?every, "scheduling task");
            let handle = tokio::spawn(async move {
                let mut interval = tokio::time::interval(every);
                loop {
                    interval.tick().await;
                    (action)();
                }
            });
            handles.push(handle);
        }
        self.state = ServerState::Running;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        let mut handles = self.handles.lock().await;
        for h in handles.drain(..) {
            h.abort();
        }
        self.state = ServerState::Stopped;
        Ok(())
    }
}
