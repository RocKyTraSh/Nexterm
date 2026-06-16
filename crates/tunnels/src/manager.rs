//! SSH port-forwarding (tunnel) model and manager.
//!
//! Three forwarding kinds are modeled — local (`-L`), remote (`-R`), and
//! dynamic SOCKS (`-D`). The [`TunnelManager`] tracks specs and status and
//! drives them through a [`TunnelDriver`]; the real driver opens `direct-tcpip`
//! channels over an SSH session (see `russh_impl.rs`). A mock driver is provided
//! so the manager is fully testable without a network.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Result, TunnelError};

/// Direction / type of a forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelKind {
    /// Local listener forwarded to a remote host:port (`ssh -L`).
    Local,
    /// Remote listener forwarded back to a local host:port (`ssh -R`).
    Remote,
    /// Local SOCKS proxy (`ssh -D`).
    Dynamic,
}

/// A serializable tunnel definition (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelSpec {
    pub id: Uuid,
    pub name: String,
    pub kind: TunnelKind,
    /// SSH profile that provides the transport.
    pub ssh_profile_id: Uuid,
    /// Address to bind locally (Local/Dynamic) or remotely (Remote).
    pub bind_address: String,
    pub bind_port: u16,
    /// Target host (unused for Dynamic).
    #[serde(default)]
    pub target_host: Option<String>,
    /// Target port (unused for Dynamic).
    #[serde(default)]
    pub target_port: Option<u16>,
    #[serde(default)]
    pub autostart: bool,
}

impl TunnelSpec {
    /// Helper to build a local (`-L`) forward.
    pub fn new_local(
        name: impl Into<String>,
        ssh_profile_id: Uuid,
        bind_port: u16,
        target_host: impl Into<String>,
        target_port: u16,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: TunnelKind::Local,
            ssh_profile_id,
            bind_address: "127.0.0.1".into(),
            bind_port,
            target_host: Some(target_host.into()),
            target_port: Some(target_port),
            autostart: false,
        }
    }
}

/// Runtime status of a tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Stopped,
    Running,
    Failed,
}

/// Backend that actually establishes/tears down forwardings.
#[async_trait]
pub trait TunnelDriver: Send + Sync {
    async fn start(&self, spec: &TunnelSpec) -> Result<()>;
    async fn stop(&self, id: Uuid) -> Result<()>;
}

/// Tracks tunnel specs and their status, driving them via a [`TunnelDriver`].
pub struct TunnelManager {
    driver: Box<dyn TunnelDriver>,
    tunnels: HashMap<Uuid, (TunnelSpec, TunnelStatus)>,
}

impl TunnelManager {
    pub fn new(driver: Box<dyn TunnelDriver>) -> Self {
        Self { driver, tunnels: HashMap::new() }
    }

    /// Register a spec (initially stopped). Returns its id.
    pub fn add(&mut self, spec: TunnelSpec) -> Uuid {
        let id = spec.id;
        self.tunnels.insert(id, (spec, TunnelStatus::Stopped));
        id
    }

    pub async fn start(&mut self, id: Uuid) -> Result<()> {
        let (spec, status) = self.tunnels.get_mut(&id).ok_or(TunnelError::NotFound(id))?;
        if *status == TunnelStatus::Running {
            return Err(TunnelError::AlreadyRunning(id));
        }
        match self.driver.start(spec).await {
            Ok(()) => {
                *status = TunnelStatus::Running;
                Ok(())
            }
            Err(e) => {
                *status = TunnelStatus::Failed;
                Err(e)
            }
        }
    }

    pub async fn stop(&mut self, id: Uuid) -> Result<()> {
        let (_, status) = self.tunnels.get_mut(&id).ok_or(TunnelError::NotFound(id))?;
        self.driver.stop(id).await?;
        *status = TunnelStatus::Stopped;
        Ok(())
    }

    pub fn status(&self, id: Uuid) -> Option<TunnelStatus> {
        self.tunnels.get(&id).map(|(_, s)| *s)
    }

    pub fn list(&self) -> Vec<(TunnelSpec, TunnelStatus)> {
        self.tunnels.values().cloned().collect()
    }

    /// Start every tunnel marked `autostart`. Returns ids that failed to start.
    pub async fn start_autostart(&mut self) -> Vec<Uuid> {
        let ids: Vec<Uuid> = self
            .tunnels
            .values()
            .filter(|(s, _)| s.autostart)
            .map(|(s, _)| s.id)
            .collect();
        let mut failed = Vec::new();
        for id in ids {
            if self.start(id).await.is_err() {
                failed.push(id);
            }
        }
        failed
    }

    /// Export all specs as JSON (secret-free; safe to persist).
    pub fn export_specs(&self) -> Result<String> {
        let specs: Vec<&TunnelSpec> = self.tunnels.values().map(|(s, _)| s).collect();
        Ok(serde_json::to_string_pretty(&specs)?)
    }

    /// Import specs from JSON, replacing the current set (all stopped).
    pub fn import_specs(&mut self, json: &str) -> Result<()> {
        let specs: Vec<TunnelSpec> = serde_json::from_str(json)?;
        self.tunnels = specs.into_iter().map(|s| (s.id, (s, TunnelStatus::Stopped))).collect();
        Ok(())
    }
}

/// A no-op driver for tests/UI development: records nothing, always succeeds.
#[derive(Default)]
pub struct MockTunnelDriver;

#[async_trait]
impl TunnelDriver for MockTunnelDriver {
    async fn start(&self, _spec: &TunnelSpec) -> Result<()> {
        Ok(())
    }
    async fn stop(&self, _id: Uuid) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lifecycle_with_mock_driver() {
        let mut mgr = TunnelManager::new(Box::new(MockTunnelDriver));
        let id = mgr.add(TunnelSpec::new_local("web", Uuid::new_v4(), 8080, "10.0.0.5", 80));
        assert_eq!(mgr.status(id), Some(TunnelStatus::Stopped));
        mgr.start(id).await.unwrap();
        assert_eq!(mgr.status(id), Some(TunnelStatus::Running));
        // Starting an already-running tunnel is an error.
        assert!(mgr.start(id).await.is_err());
        mgr.stop(id).await.unwrap();
        assert_eq!(mgr.status(id), Some(TunnelStatus::Stopped));

        let json = mgr.export_specs().unwrap();
        let mut mgr2 = TunnelManager::new(Box::new(MockTunnelDriver));
        mgr2.import_specs(&json).unwrap();
        assert_eq!(mgr2.list().len(), 1);
    }
}
