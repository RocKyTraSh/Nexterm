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

    /// Helper to build a dynamic SOCKS (`-D`) proxy. No fixed target — the
    /// destination comes from each SOCKS request.
    pub fn new_dynamic(
        name: impl Into<String>,
        ssh_profile_id: Uuid,
        bind_address: impl Into<String>,
        bind_port: u16,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: TunnelKind::Dynamic,
            ssh_profile_id,
            bind_address: bind_address.into(),
            bind_port,
            target_host: None,
            target_port: None,
            autostart: false,
        }
    }

    /// Helper to build a remote (`-R`) forward. `bind_*` is the **remote** listen
    /// endpoint on the SSH server; `target_*` is the **local** destination on the
    /// machine running Nexterm.
    pub fn new_remote(
        name: impl Into<String>,
        ssh_profile_id: Uuid,
        remote_bind_address: impl Into<String>,
        remote_bind_port: u16,
        local_target_host: impl Into<String>,
        local_target_port: u16,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: TunnelKind::Remote,
            ssh_profile_id,
            bind_address: remote_bind_address.into(),
            bind_port: remote_bind_port,
            target_host: Some(local_target_host.into()),
            target_port: Some(local_target_port),
            autostart: false,
        }
    }

    /// The `(address, port)` this tunnel binds locally. Errors on an empty
    /// address. Pure — no socket is opened (unit-tested).
    pub fn bind_endpoint(&self) -> Result<(String, u16)> {
        if self.bind_address.trim().is_empty() {
            return Err(TunnelError::InvalidSpec("empty bind address".into()));
        }
        Ok((self.bind_address.clone(), self.bind_port))
    }

    /// The `(host, port)` a **local** forward must reach. Errors if the target
    /// fields are missing (they are required for `-L`) or the kind is not
    /// `Local`. Pure (unit-tested).
    pub fn local_forward_target(&self) -> Result<(String, u16)> {
        if self.kind != TunnelKind::Local {
            return Err(TunnelError::Unsupported(format!("{:?}", self.kind)));
        }
        let host = self
            .target_host
            .clone()
            .filter(|h| !h.trim().is_empty())
            .ok_or_else(|| {
                TunnelError::InvalidSpec("local forward requires a target host".into())
            })?;
        let port = self.target_port.ok_or_else(|| {
            TunnelError::InvalidSpec("local forward requires a target port".into())
        })?;
        Ok((host, port))
    }

    /// For a **remote** (`-R`) forward: `((remote_bind_host, remote_bind_port),
    /// (local_target_host, local_target_port))`. Pure (unit-tested).
    ///
    /// The remote bind port may be `0` (the server picks a port); the local
    /// target host must be non-empty and its port non-zero.
    pub fn remote_forward_endpoints(&self) -> Result<((String, u16), (String, u16))> {
        if self.kind != TunnelKind::Remote {
            return Err(TunnelError::Unsupported(format!("{:?}", self.kind)));
        }
        if self.bind_address.trim().is_empty() {
            return Err(TunnelError::InvalidSpec(
                "remote forward requires a remote bind address".into(),
            ));
        }
        let local_host = self
            .target_host
            .clone()
            .filter(|h| !h.trim().is_empty())
            .ok_or_else(|| {
                TunnelError::InvalidSpec("remote forward requires a local target host".into())
            })?;
        let local_port = self.target_port.filter(|p| *p != 0).ok_or_else(|| {
            TunnelError::InvalidSpec("remote forward requires a non-zero local target port".into())
        })?;
        Ok((
            (self.bind_address.clone(), self.bind_port),
            (local_host, local_port),
        ))
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
        Self {
            driver,
            tunnels: HashMap::new(),
        }
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
        self.tunnels = specs
            .into_iter()
            .map(|s| (s.id, (s, TunnelStatus::Stopped)))
            .collect();
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
        let id = mgr.add(TunnelSpec::new_local(
            "web",
            Uuid::new_v4(),
            8080,
            "10.0.0.5",
            80,
        ));
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

    #[test]
    fn local_forward_target_requires_target_fields() {
        let spec = TunnelSpec::new_local("web", Uuid::new_v4(), 8080, "10.0.0.5", 80);
        assert_eq!(
            spec.local_forward_target().unwrap(),
            ("10.0.0.5".to_string(), 80)
        );
        assert_eq!(
            spec.bind_endpoint().unwrap(),
            ("127.0.0.1".to_string(), 8080)
        );

        // Missing target host → InvalidSpec.
        let mut bad = spec.clone();
        bad.target_host = None;
        assert!(matches!(
            bad.local_forward_target(),
            Err(TunnelError::InvalidSpec(_))
        ));

        // Empty target host → InvalidSpec.
        let mut empty = spec.clone();
        empty.target_host = Some("  ".into());
        assert!(matches!(
            empty.local_forward_target(),
            Err(TunnelError::InvalidSpec(_))
        ));

        // Missing target port → InvalidSpec.
        let mut noport = spec.clone();
        noport.target_port = None;
        assert!(matches!(
            noport.local_forward_target(),
            Err(TunnelError::InvalidSpec(_))
        ));
    }

    #[test]
    fn dynamic_and_remote_are_unsupported_as_local() {
        let mut spec = TunnelSpec::new_local("d", Uuid::new_v4(), 1080, "x", 1);
        spec.kind = TunnelKind::Dynamic;
        assert!(matches!(
            spec.local_forward_target(),
            Err(TunnelError::Unsupported(_))
        ));
        spec.kind = TunnelKind::Remote;
        assert!(matches!(
            spec.local_forward_target(),
            Err(TunnelError::Unsupported(_))
        ));
    }

    #[test]
    fn empty_bind_address_is_rejected() {
        let mut spec = TunnelSpec::new_local("web", Uuid::new_v4(), 8080, "10.0.0.5", 80);
        spec.bind_address = "  ".into();
        assert!(matches!(
            spec.bind_endpoint(),
            Err(TunnelError::InvalidSpec(_))
        ));
    }

    #[tokio::test]
    async fn dynamic_spec_is_accepted_by_manager() {
        let spec = TunnelSpec::new_dynamic("socks", Uuid::new_v4(), "127.0.0.1", 1080);
        assert_eq!(spec.kind, TunnelKind::Dynamic);
        assert_eq!(spec.target_host, None);
        // A dynamic proxy still has a bind endpoint but no fixed target.
        assert_eq!(spec.bind_endpoint().unwrap(), ("127.0.0.1".into(), 1080));

        // The manager (with the mock driver) drives a dynamic spec like any other.
        let mut mgr = TunnelManager::new(Box::new(MockTunnelDriver));
        let id = mgr.add(spec);
        mgr.start(id).await.unwrap();
        assert_eq!(mgr.status(id), Some(TunnelStatus::Running));
        mgr.stop(id).await.unwrap();

        // Specs round-trip through JSON (secret-free).
        let json = mgr.export_specs().unwrap();
        let mut mgr2 = TunnelManager::new(Box::new(MockTunnelDriver));
        mgr2.import_specs(&json).unwrap();
        assert_eq!(mgr2.list()[0].0.kind, TunnelKind::Dynamic);
    }

    #[test]
    fn remote_forward_endpoints_validation() {
        // bind_* is the REMOTE listen endpoint; target_* is the LOCAL target.
        let spec =
            TunnelSpec::new_remote("r", Uuid::new_v4(), "127.0.0.1", 18080, "127.0.0.1", 8080);
        assert_eq!(spec.kind, TunnelKind::Remote);
        assert_eq!(
            spec.remote_forward_endpoints().unwrap(),
            (
                ("127.0.0.1".to_string(), 18080),
                ("127.0.0.1".to_string(), 8080)
            )
        );

        // Remote bind port 0 is allowed (the server assigns one).
        let mut zero_bind = spec.clone();
        zero_bind.bind_port = 0;
        assert_eq!(
            zero_bind.remote_forward_endpoints().unwrap().0,
            ("127.0.0.1".to_string(), 0)
        );

        // Empty remote bind host → InvalidSpec.
        let mut bad_bind = spec.clone();
        bad_bind.bind_address = "  ".into();
        assert!(matches!(
            bad_bind.remote_forward_endpoints(),
            Err(TunnelError::InvalidSpec(_))
        ));

        // Missing / empty local target host → InvalidSpec.
        let mut no_target = spec.clone();
        no_target.target_host = None;
        assert!(matches!(
            no_target.remote_forward_endpoints(),
            Err(TunnelError::InvalidSpec(_))
        ));

        // Zero local target port → InvalidSpec (can't connect to port 0).
        let mut zero_target = spec.clone();
        zero_target.target_port = Some(0);
        assert!(matches!(
            zero_target.remote_forward_endpoints(),
            Err(TunnelError::InvalidSpec(_))
        ));

        // Calling it on a non-remote spec is Unsupported.
        let local = TunnelSpec::new_local("l", Uuid::new_v4(), 8080, "x", 1);
        assert!(matches!(
            local.remote_forward_endpoints(),
            Err(TunnelError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn remote_spec_is_accepted_by_manager() {
        let spec =
            TunnelSpec::new_remote("r", Uuid::new_v4(), "127.0.0.1", 18080, "127.0.0.1", 8080);
        let mut mgr = TunnelManager::new(Box::new(MockTunnelDriver));
        let id = mgr.add(spec);
        mgr.start(id).await.unwrap();
        assert_eq!(mgr.status(id), Some(TunnelStatus::Running));
        mgr.stop(id).await.unwrap();

        let json = mgr.export_specs().unwrap();
        let mut mgr2 = TunnelManager::new(Box::new(MockTunnelDriver));
        mgr2.import_specs(&json).unwrap();
        assert_eq!(mgr2.list()[0].0.kind, TunnelKind::Remote);
    }
}
