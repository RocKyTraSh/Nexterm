//! Embedded "mini-server" framework.
//!
//! MobaXterm bundles small servers (TFTP/FTP/HTTP/NFS/...). We model each as a
//! [`MiniServer`] with a uniform lifecycle and a secret-free [`MiniServerConfig`].
//! The HTTP file server is implemented; others (TFTP/FTP/SSH/Telnet/NFS/VNC)
//! plug in behind the same trait. **Safe by default**: configs bind loopback.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{MiniServerError, Result};

/// The kind of embedded server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiniServerKind {
    Http,
    Tftp,
    Ftp,
    Ssh,
    Telnet,
    Nfs,
    Vnc,
    Scheduler,
}

/// Common, secret-free configuration for a mini-server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiniServerConfig {
    pub id: Uuid,
    pub name: String,
    pub kind: MiniServerKind,
    /// Bind address. Defaults to loopback for safety.
    pub bind_address: String,
    pub port: u16,
    /// Served root directory, where applicable.
    #[serde(default)]
    pub root_dir: Option<String>,
    /// Read-only mode where applicable (e.g. file servers).
    #[serde(default = "default_true")]
    pub read_only: bool,
    #[serde(default)]
    pub autostart: bool,
}

fn default_true() -> bool {
    true
}

impl MiniServerConfig {
    /// A safe-by-default HTTP file server config (loopback, read-only).
    pub fn http(name: impl Into<String>, port: u16, root: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            kind: MiniServerKind::Http,
            bind_address: "127.0.0.1".into(),
            port,
            root_dir: Some(root.into()),
            read_only: true,
            autostart: false,
        }
    }

    /// A human-readable warning when the server is exposed beyond loopback.
    /// The UI should surface this prominently before starting such a server.
    pub fn security_warning(&self) -> Option<String> {
        let exposed = self.bind_address != "127.0.0.1" && self.bind_address != "::1";
        if exposed {
            Some(format!(
                "Server '{}' binds {} (not loopback) and will be reachable from \
                 the network. Ensure this is intended and access is controlled.",
                self.name, self.bind_address
            ))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    Stopped,
    Running,
}

/// Lifecycle contract for an embedded server.
#[async_trait]
pub trait MiniServer: Send + Sync {
    fn config(&self) -> &MiniServerConfig;
    fn state(&self) -> ServerState;
    async fn start(&mut self) -> Result<()>;
    async fn stop(&mut self) -> Result<()>;
}

/// Registers and controls multiple mini-servers.
#[derive(Default)]
pub struct MiniServerManager {
    servers: HashMap<Uuid, Box<dyn MiniServer>>,
}

impl MiniServerManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, server: Box<dyn MiniServer>) -> Uuid {
        let id = server.config().id;
        self.servers.insert(id, server);
        id
    }

    pub async fn start(&mut self, id: Uuid) -> Result<()> {
        self.servers.get_mut(&id).ok_or(MiniServerError::NotFound(id))?.start().await
    }

    pub async fn stop(&mut self, id: Uuid) -> Result<()> {
        self.servers.get_mut(&id).ok_or(MiniServerError::NotFound(id))?.stop().await
    }

    pub fn list(&self) -> Vec<(MiniServerConfig, ServerState)> {
        self.servers.values().map(|s| (s.config().clone(), s.state())).collect()
    }
}
