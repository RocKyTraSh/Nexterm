//! Real SSH/SFTP transport via [`russh`] + [`russh-sftp`] (feature `ssh-russh`).
//!
//! Compiled only with the `ssh-russh` feature so the default build stays free of
//! network/crypto dependencies. Everything outside this file depends solely on
//! the `Connector` / `RemoteSession` / `SftpClient` traits.
//!
//! What works now:
//! * TCP connect + SSH handshake to `host:port` from the profile;
//! * host-key verification against `~/.ssh/known_hosts`, honoring
//!   `SshSettings::strict_host_key_checking` (see [`decide_host_key`]);
//! * authentication in the configured order — agent → public key → password →
//!   keyboard-interactive ([`plan_auth`]);
//! * an interactive PTY shell exposed as [`RemoteSession`];
//! * SFTP (`list_dir` / `stat` / `read_file` / `write_file` / `mkdir` /
//!   `remove` / `rename` / `chmod`) via [`RusshSftp`].
//!
//! Not yet implemented (clear errors / TODOs, no fake behavior):
//! * jump-host chaining — connecting returns `NotImplemented` when
//!   `SshSettings::jump_host` is set; the `direct-tcpip` primitive is sketched
//!   in [`RusshConnector::connect_via_jump_host`].
//! * the `TunnelManager` driver (would reuse the same `channel_open_direct_tcpip`).
//!
//! Secrets: passwords / passphrases come from [`ResolvedCredentials`] only, are
//! never read from the profile, and are never logged.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, ssh_key, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg, Disconnect};

use rrs_core::model::{AuthMethod, ConnectionProfile, ProtocolSettings, SshSettings};

use crate::error::{ProtocolError, Result};
use crate::traits::{
    Connector, DirEntry, EntryKind, RemoteSession, ResolvedCredentials, SftpClient,
};

/// Initial PTY geometry; the frontend resizes on attach.
const DEFAULT_COLS: u32 = 80;
const DEFAULT_ROWS: u32 = 24;
const DEFAULT_TERM: &str = "xterm-256color";

// Host-key outcome shared out of the (moved-away) handler.
const HK_PENDING: u8 = 0;
const HK_TRUSTED: u8 = 1;
const HK_ACCEPTED_UNKNOWN: u8 = 2;

// ---------------------------------------------------------------------------
// Pure policy helpers (unit-tested, no I/O)
// ---------------------------------------------------------------------------

/// Result of comparing a server key against `known_hosts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// The host is present and its key matches.
    Trusted,
    /// The host is not in `known_hosts` (or there is no file).
    Unknown,
    /// The host is present but the key differs — a hard failure.
    Changed,
}

/// What to do with a server key, given the verdict and the strict setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// Trusted: accept silently.
    Accept,
    /// Unknown but non-strict: accept and surface a warning.
    AcceptUnknown,
    /// Reject the connection.
    Reject,
}

/// Decide on a server key. A changed key is rejected regardless of strictness
/// (it signals a possible MITM); an unknown key is rejected only in strict mode.
pub fn decide_host_key(verdict: HostKeyVerdict, strict: bool) -> HostKeyDecision {
    match verdict {
        HostKeyVerdict::Trusted => HostKeyDecision::Accept,
        HostKeyVerdict::Changed => HostKeyDecision::Reject,
        HostKeyVerdict::Unknown => {
            if strict {
                HostKeyDecision::Reject
            } else {
                HostKeyDecision::AcceptUnknown
            }
        }
    }
}

/// Filter the configured auth methods down to those that can actually be tried,
/// preserving order and dropping duplicates. `Agent` is always attemptable;
/// `Password`/`KeyboardInteractive` need a password; `PublicKey` needs a key path.
pub fn plan_auth(methods: &[AuthMethod], has_password: bool, has_key: bool) -> Vec<AuthMethod> {
    let mut plan: Vec<AuthMethod> = Vec::new();
    for &m in methods {
        let feasible = match m {
            AuthMethod::Agent => true,
            AuthMethod::PublicKey => has_key,
            AuthMethod::Password | AuthMethod::KeyboardInteractive => has_password,
        };
        if feasible && !plan.contains(&m) {
            plan.push(m);
        }
    }
    plan
}

// ---------------------------------------------------------------------------
// Connection handler
// ---------------------------------------------------------------------------

/// russh client handler: its only job is host-key verification.
struct ClientHandler {
    host: String,
    port: u16,
    strict: bool,
    /// Set to one of the `HK_*` constants from `check_server_key`.
    hostkey_status: Arc<AtomicU8>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &ssh_key::PublicKey,
    ) -> std::result::Result<bool, Self::Error> {
        let host = self.host.clone();
        let port = self.port;
        let key = server_public_key.clone();
        // known_hosts parsing reads a file — keep it off the async worker.
        let verdict = tokio::task::spawn_blocking(move || {
            match russh::keys::check_known_hosts(&host, port, &key) {
                Ok(true) => HostKeyVerdict::Trusted,
                Ok(false) => HostKeyVerdict::Unknown,
                Err(russh::keys::Error::KeyChanged { .. }) => HostKeyVerdict::Changed,
                // Missing file / no home dir / parse error → treat as unknown,
                // which fails closed in strict mode.
                Err(_) => HostKeyVerdict::Unknown,
            }
        })
        .await
        .unwrap_or(HostKeyVerdict::Unknown);

        match decide_host_key(verdict, self.strict) {
            HostKeyDecision::Accept => {
                self.hostkey_status.store(HK_TRUSTED, Ordering::SeqCst);
                Ok(true)
            }
            HostKeyDecision::AcceptUnknown => {
                self.hostkey_status
                    .store(HK_ACCEPTED_UNKNOWN, Ordering::SeqCst);
                tracing::warn!(
                    host = %self.host,
                    port = self.port,
                    "accepting unknown SSH host key (strict_host_key_checking is off)"
                );
                Ok(true)
            }
            HostKeyDecision::Reject => Ok(false),
        }
    }
}

// ---------------------------------------------------------------------------
// Connector
// ---------------------------------------------------------------------------

/// SSH connector backed by `russh`.
#[derive(Default)]
pub struct RusshConnector;

impl RusshConnector {
    /// Jump-host chaining: connect to the gateway, open a `direct-tcpip` channel
    /// to the target, and run the target SSH session over that stream.
    ///
    /// Not implemented yet. The primitive is `Handle::channel_open_direct_tcpip`
    /// then `client::connect_stream` over `channel.into_stream()`; the same
    /// channel type also backs the `rrs-tunnels` driver. Kept as an explicit
    /// seam so the architecture stays open without shipping a fake.
    async fn connect_via_jump_host(
        &self,
        _profile: &ConnectionProfile,
        _creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        Err(ProtocolError::NotImplemented(
            "SSH jump-host chaining (direct-tcpip) — see RusshConnector::connect_via_jump_host",
        ))
    }
}

#[async_trait]
impl Connector for RusshConnector {
    async fn connect_shell(
        &self,
        profile: &ConnectionProfile,
        creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        let ssh = ssh_settings(profile)?;
        if ssh.jump_host.is_some() {
            return self.connect_via_jump_host(profile, creds).await;
        }

        let handle = establish(ssh, creds).await?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        channel
            .request_pty(false, DEFAULT_TERM, DEFAULT_COLS, DEFAULT_ROWS, 0, 0, &[])
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        channel
            .request_shell(true)
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;

        Ok(Box::new(RusshSession {
            _handle: handle,
            channel,
            closed: false,
        }))
    }
}

/// Extract SSH settings or explain why this connector cannot serve the profile.
fn ssh_settings(profile: &ConnectionProfile) -> Result<&SshSettings> {
    match &profile.settings {
        ProtocolSettings::Ssh(s) => Ok(s),
        other => Err(ProtocolError::Connect(format!(
            "RusshConnector cannot open a {:?} profile",
            other.kind()
        ))),
    }
}

/// Connect, verify the host key, and authenticate. Returns the live handle.
async fn establish(
    ssh: &SshSettings,
    creds: &ResolvedCredentials,
) -> Result<Handle<ClientHandler>> {
    let config = Arc::new(client::Config::default());
    let handler = ClientHandler {
        host: ssh.host.clone(),
        port: ssh.port,
        strict: ssh.strict_host_key_checking,
        hostkey_status: Arc::new(AtomicU8::new(HK_PENDING)),
    };

    let mut handle = client::connect(config, (ssh.host.clone(), ssh.port), handler)
        .await
        .map_err(|e| ProtocolError::Connect(e.to_string()))?;

    authenticate(&mut handle, ssh, creds).await?;
    Ok(handle)
}

// ---------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------

async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    ssh: &SshSettings,
    creds: &ResolvedCredentials,
) -> Result<()> {
    let has_password = creds.password.as_ref().is_some_and(|s| !s.is_empty());
    let has_key = ssh.private_key_path.is_some();
    let plan = plan_auth(&ssh.auth_methods, has_password, has_key);

    if plan.is_empty() {
        return Err(ProtocolError::Auth(
            "no usable authentication methods (need an agent, a private key, or a password)".into(),
        ));
    }

    for method in plan {
        let ok = match method {
            AuthMethod::Agent => try_agent_auth(handle, &ssh.username).await?,
            AuthMethod::PublicKey => try_key_auth(handle, ssh, creds).await?,
            AuthMethod::Password => try_password_auth(handle, &ssh.username, creds).await?,
            AuthMethod::KeyboardInteractive => {
                try_keyboard_auth(handle, &ssh.username, creds).await?
            }
        };
        if ok {
            return Ok(());
        }
    }

    Err(ProtocolError::Auth(
        "all authentication methods failed".into(),
    ))
}

async fn try_password_auth(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    creds: &ResolvedCredentials,
) -> Result<bool> {
    let Some(pw) = creds.password.as_ref() else {
        return Ok(false);
    };
    let res = handle
        .authenticate_password(user, pw.expose())
        .await
        .map_err(|e| ProtocolError::Auth(e.to_string()))?;
    Ok(res.success())
}

async fn try_keyboard_auth(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    creds: &ResolvedCredentials,
) -> Result<bool> {
    let Some(pw) = creds.password.as_ref() else {
        return Ok(false);
    };
    let mut resp = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(|e| ProtocolError::Auth(e.to_string()))?;
    loop {
        match resp {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                // Answer every prompt with the password (typical password-over-KI).
                let answers = prompts.iter().map(|_| pw.expose().to_string()).collect();
                resp = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| ProtocolError::Auth(e.to_string()))?;
            }
        }
    }
}

async fn try_key_auth(
    handle: &mut Handle<ClientHandler>,
    ssh: &SshSettings,
    creds: &ResolvedCredentials,
) -> Result<bool> {
    let Some(path) = ssh.private_key_path.clone() else {
        return Ok(false);
    };
    let passphrase = creds
        .key_passphrase
        .as_ref()
        .map(|s| s.expose().to_string());

    // Reading + decrypting the key file is blocking I/O.
    let loaded = tokio::task::spawn_blocking(move || load_secret_key(&path, passphrase.as_deref()))
        .await
        .map_err(|e| ProtocolError::Auth(format!("key load task failed: {e}")))?;
    let key = match loaded {
        Ok(k) => k,
        Err(e) => {
            // A bad key/passphrase is recoverable — fall through to the next method.
            tracing::warn!("failed to load private key: {e}");
            return Ok(false);
        }
    };

    let hash = handle
        .best_supported_rsa_hash()
        .await
        .map_err(|e| ProtocolError::Auth(e.to_string()))?
        .flatten();
    let res = handle
        .authenticate_publickey(
            &ssh.username,
            PrivateKeyWithHashAlg::new(Arc::new(key), hash),
        )
        .await
        .map_err(|e| ProtocolError::Auth(e.to_string()))?;
    Ok(res.success())
}

async fn try_agent_auth(handle: &mut Handle<ClientHandler>, user: &str) -> Result<bool> {
    // No agent (SSH_AUTH_SOCK unset / unreachable) → skip this method.
    let Ok(mut agent) = AgentClient::connect_env().await else {
        return Ok(false);
    };
    let Ok(identities) = agent.request_identities().await else {
        return Ok(false);
    };

    for id in identities {
        let pubkey = id.public_key().into_owned();
        let hash = handle
            .best_supported_rsa_hash()
            .await
            .map_err(|e| ProtocolError::Auth(e.to_string()))?
            .flatten();
        // Sign with the agent; a per-key failure just moves to the next identity.
        if let Ok(res) = handle
            .authenticate_publickey_with(user, pubkey, hash, &mut agent)
            .await
        {
            if res.success() {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

// ---------------------------------------------------------------------------
// Interactive shell session
// ---------------------------------------------------------------------------

/// A live SSH shell exposed as a [`RemoteSession`]. Holds the connection handle
/// (kept alive for the channel's lifetime) and the session channel.
pub struct RusshSession {
    _handle: Handle<ClientHandler>,
    channel: Channel<client::Msg>,
    closed: bool,
}

#[async_trait]
impl RemoteSession for RusshSession {
    async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.channel
            .data_bytes(data.to_vec())
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))
    }

    async fn read(&mut self) -> Result<Vec<u8>> {
        if self.closed {
            return Ok(Vec::new());
        }
        loop {
            match self.channel.wait().await {
                Some(ChannelMsg::Data { data }) => return Ok(data.to_vec()),
                Some(ChannelMsg::ExtendedData { data, .. }) => return Ok(data.to_vec()),
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                    self.closed = true;
                    return Ok(Vec::new());
                }
                // Success / ExitStatus / ExitSignal / WindowAdjust / etc. — keep waiting.
                Some(_) => continue,
            }
        }
    }

    async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.channel
            .window_change(cols as u32, rows as u32, 0, 0)
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))
    }

    async fn close(&mut self) -> Result<()> {
        self.closed = true;
        let _ = self.channel.eof().await;
        let _ = self.channel.close().await;
        let _ = self
            ._handle
            .disconnect(Disconnect::ByApplication, "", "")
            .await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SFTP
// ---------------------------------------------------------------------------

/// SFTP client over a russh connection. Construct with [`RusshSftp::connect`].
pub struct RusshSftp {
    _handle: Handle<ClientHandler>,
    session: russh_sftp::client::SftpSession,
}

impl RusshSftp {
    /// Open an SFTP session for `profile` using transient `creds`.
    pub async fn connect(profile: &ConnectionProfile, creds: &ResolvedCredentials) -> Result<Self> {
        let ssh = ssh_settings(profile)?;
        if ssh.jump_host.is_some() {
            return Err(ProtocolError::NotImplemented(
                "SFTP over a jump host (direct-tcpip) — see RusshConnector::connect_via_jump_host",
            ));
        }
        let handle = establish(ssh, creds).await?;
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        let session = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        Ok(Self {
            _handle: handle,
            session,
        })
    }
}

/// Map an SFTP metadata record to our protocol-agnostic [`DirEntry`].
fn to_entry(name: String, meta: &russh_sftp::protocol::FileAttributes) -> DirEntry {
    use russh_sftp::protocol::FileType;
    let kind = match meta.file_type() {
        FileType::Dir => EntryKind::Dir,
        FileType::File => EntryKind::File,
        FileType::Symlink => EntryKind::Symlink,
        FileType::Other => EntryKind::Other,
    };
    DirEntry {
        name,
        kind,
        size: meta.size.unwrap_or(0),
        permissions: meta.permissions,
        modified_unix: meta.mtime.map(|t| t as i64),
    }
}

#[async_trait]
impl SftpClient for RusshSftp {
    async fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let rd = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        Ok(rd
            .map(|entry| to_entry(entry.file_name(), &entry.metadata()))
            .collect())
    }

    async fn stat(&self, path: &str) -> Result<DirEntry> {
        let meta = self
            .session
            .metadata(path)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        Ok(to_entry(name, &meta))
    }

    async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.session
            .read(path)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        use russh_sftp::protocol::OpenFlags;
        use tokio::io::AsyncWriteExt;

        // The high-level `SftpSession::write` opens with WRITE only (no create /
        // truncate), so it fails for new files; open explicitly instead.
        let mut file = self
            .session
            .open_with_flags(
                path,
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            )
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        file.write_all(data)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        file.shutdown()
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        Ok(())
    }

    async fn make_dir(&self, path: &str) -> Result<()> {
        self.session
            .create_dir(path)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))
    }

    async fn remove_file(&self, path: &str) -> Result<()> {
        self.session
            .remove_file(path)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))
    }

    async fn remove_dir(&self, path: &str) -> Result<()> {
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))
    }

    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.session
            .rename(from, to)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))
    }

    async fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        let attrs = russh_sftp::protocol::FileAttributes {
            size: None,
            uid: None,
            user: None,
            gid: None,
            group: None,
            permissions: Some(mode),
            atime: None,
            mtime: None,
        };
        self.session
            .set_metadata(path, attrs)
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Tests (pure logic only — no network / no sshd)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_mode_rejects_unknown_but_trusts_known() {
        assert_eq!(
            decide_host_key(HostKeyVerdict::Trusted, true),
            HostKeyDecision::Accept
        );
        assert_eq!(
            decide_host_key(HostKeyVerdict::Unknown, true),
            HostKeyDecision::Reject
        );
        assert_eq!(
            decide_host_key(HostKeyVerdict::Changed, true),
            HostKeyDecision::Reject
        );
    }

    #[test]
    fn nonstrict_accepts_unknown_but_still_rejects_changed() {
        assert_eq!(
            decide_host_key(HostKeyVerdict::Unknown, false),
            HostKeyDecision::AcceptUnknown
        );
        // A changed key is a hard failure even in non-strict mode.
        assert_eq!(
            decide_host_key(HostKeyVerdict::Changed, false),
            HostKeyDecision::Reject
        );
        assert_eq!(
            decide_host_key(HostKeyVerdict::Trusted, false),
            HostKeyDecision::Accept
        );
    }

    #[test]
    fn auth_plan_preserves_order_and_drops_infeasible() {
        let methods = [
            AuthMethod::Agent,
            AuthMethod::PublicKey,
            AuthMethod::Password,
            AuthMethod::KeyboardInteractive,
        ];
        // Nothing available except the agent.
        assert_eq!(plan_auth(&methods, false, false), vec![AuthMethod::Agent]);
        // Password available → password + keyboard-interactive become feasible.
        assert_eq!(
            plan_auth(&methods, true, false),
            vec![
                AuthMethod::Agent,
                AuthMethod::Password,
                AuthMethod::KeyboardInteractive
            ]
        );
        // Key available → public key feasible.
        assert_eq!(
            plan_auth(&methods, false, true),
            vec![AuthMethod::Agent, AuthMethod::PublicKey]
        );
    }

    #[test]
    fn auth_plan_dedups_and_keeps_first_position() {
        let methods = [
            AuthMethod::Password,
            AuthMethod::Agent,
            AuthMethod::Password,
        ];
        assert_eq!(
            plan_auth(&methods, true, false),
            vec![AuthMethod::Password, AuthMethod::Agent]
        );
    }

    /// Live SFTP round-trip against a real sshd. Ignored by default (needs a
    /// server); run manually with publickey auth, e.g.:
    ///
    /// ```text
    /// NEXTERM_SSH_TEST_HOST=127.0.0.1 \
    /// NEXTERM_SSH_TEST_USER=$USER \
    /// NEXTERM_SSH_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// cargo test -p rrs-protocols --features ssh-russh -- --ignored sftp_roundtrip
    /// ```
    #[tokio::test]
    #[ignore = "requires a reachable sshd; see doc comment"]
    async fn sftp_roundtrip() {
        use rrs_core::model::{ConnectionProfile, ProtocolSettings};

        let host = std::env::var("NEXTERM_SSH_TEST_HOST").expect("NEXTERM_SSH_TEST_HOST");
        let user = std::env::var("NEXTERM_SSH_TEST_USER").expect("NEXTERM_SSH_TEST_USER");
        let key = std::env::var("NEXTERM_SSH_TEST_KEY").expect("NEXTERM_SSH_TEST_KEY");

        let mut profile = ConnectionProfile::new_ssh("sftp-test", &host, &user);
        if let ProtocolSettings::Ssh(s) = &mut profile.settings {
            s.private_key_path = Some(key);
            s.strict_host_key_checking = false;
        }
        let creds = ResolvedCredentials::default();

        let sftp = RusshSftp::connect(&profile, &creds)
            .await
            .expect("sftp connect");

        let path = format!("/tmp/nexterm_sftp_test_{}.txt", std::process::id());
        let payload = b"nexterm sftp roundtrip";
        sftp.write_file(&path, payload).await.expect("write");
        let got = sftp.read_file(&path).await.expect("read");
        assert_eq!(got, payload);

        let entry = sftp.stat(&path).await.expect("stat");
        assert_eq!(entry.kind, EntryKind::File);
        assert_eq!(entry.size, payload.len() as u64);

        let listing = sftp.list_dir("/tmp").await.expect("list_dir");
        let name = path.rsplit('/').next().unwrap();
        assert!(
            listing.iter().any(|e| e.name == name),
            "uploaded file not listed"
        );

        sftp.remove_file(&path).await.expect("remove");
    }
}
