//! Protocol-agnostic connection and file-transfer abstractions.
//!
//! Concrete transports (SSH, Telnet, ...) implement these traits. UI code talks
//! only to the traits, so adding a protocol never requires touching the UI.

use async_trait::async_trait;

use rrs_core::model::ConnectionProfile;
use rrs_credentials::Secret;

use crate::error::{ProtocolError, Result};

/// Secrets resolved transiently for a single connection attempt.
///
/// Built by the orchestration layer from the OS secret store and handed to a
/// [`Connector`]. Never stored or logged (note the redacting `Debug`).
#[derive(Default)]
pub struct ResolvedCredentials {
    pub password: Option<Secret>,
    pub key_passphrase: Option<Secret>,
}

impl std::fmt::Debug for ResolvedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedCredentials")
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field(
                "key_passphrase",
                &self.key_passphrase.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

/// An interactive remote shell session: a bidirectional byte stream plus resize.
#[async_trait]
pub trait RemoteSession: Send {
    /// Send bytes to the remote (e.g. keystrokes).
    async fn write(&mut self, data: &[u8]) -> Result<()>;
    /// Read the next chunk of output. Implementations may block until data is
    /// available or the session closes (returning an empty `Vec` on clean EOF).
    async fn read(&mut self) -> Result<Vec<u8>>;
    /// Inform the remote of a new terminal size.
    async fn resize(&mut self, cols: u16, rows: u16) -> Result<()>;
    /// Close the session.
    async fn close(&mut self) -> Result<()>;
}

/// Opens connections for a particular protocol.
///
/// The jump-host and SFTP methods have default implementations that report
/// `NotImplemented`, so a connector only overrides what it actually supports.
/// Orchestration (resolving the second hop's profile + secret) lives in the
/// caller — a `Connector` is handed fully-resolved profiles and credentials and
/// never touches the profile/credential stores itself.
#[async_trait]
pub trait Connector: Send + Sync {
    /// Open an interactive shell using `profile` and transient `creds`.
    async fn connect_shell(
        &self,
        profile: &ConnectionProfile,
        creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>>;

    /// Open an interactive shell on `target` by tunnelling through the gateway
    /// `jump` (single-hop `ProxyJump`). Each hop carries its own transient
    /// credentials. Both hosts are verified independently.
    async fn connect_shell_via_jump(
        &self,
        jump: &ConnectionProfile,
        jump_creds: &ResolvedCredentials,
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        let _ = (jump, jump_creds, target, target_creds);
        Err(ProtocolError::NotImplemented(
            "jump-host shell is not supported by this connector",
        ))
    }

    /// Open an SFTP client for `profile` using transient `creds`.
    async fn connect_sftp(
        &self,
        profile: &ConnectionProfile,
        creds: &ResolvedCredentials,
    ) -> Result<Box<dyn SftpClient>> {
        let _ = (profile, creds);
        Err(ProtocolError::NotImplemented(
            "SFTP is not supported by this connector",
        ))
    }

    /// Open an SFTP client on `target` through the gateway `jump` (single-hop).
    async fn connect_sftp_via_jump(
        &self,
        jump: &ConnectionProfile,
        jump_creds: &ResolvedCredentials,
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Box<dyn SftpClient>> {
        let _ = (jump, jump_creds, target, target_creds);
        Err(ProtocolError::NotImplemented(
            "SFTP over a jump host is not supported by this connector",
        ))
    }
}

/// Kind of a remote filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// A single remote directory entry (for the SFTP browser).
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Unix permission bits (e.g. 0o644), if known.
    pub permissions: Option<u32>,
    /// Modified time as seconds since the Unix epoch, if known.
    pub modified_unix: Option<i64>,
}

/// Remote file operations backing the SFTP browser.
///
/// Mirrors the operations the UI needs: list / stat / read / write / mkdir /
/// rename / remove / chmod. `chown` is modeled in a follow-up (needs numeric
/// uid/gid plus name resolution).
#[async_trait]
pub trait SftpClient: Send + Sync {
    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>>;
    async fn stat(&self, path: &str) -> Result<DirEntry>;
    async fn read_file(&self, path: &str) -> Result<Vec<u8>>;
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()>;
    async fn make_dir(&self, path: &str) -> Result<()>;
    async fn remove_file(&self, path: &str) -> Result<()>;
    async fn remove_dir(&self, path: &str) -> Result<()>;
    async fn rename(&self, from: &str, to: &str) -> Result<()>;
    async fn set_permissions(&self, path: &str, mode: u32) -> Result<()>;
}
