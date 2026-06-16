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
//!   `remove` / `rename` / `chmod`) via [`RusshSftp`];
//! * multi-hop jump-host (`ProxyJump`) chaining over `direct-tcpip` channels,
//!   for both **shell** ([`Connector::connect_shell_via_jump_chain`]) and
//!   **SFTP** ([`Connector::connect_sftp_via_jump_chain`]); single-hop helpers
//!   ([`Connector::connect_shell_via_jump`], [`RusshSftp::connect_via_jump`])
//!   delegate to the chain path. Up to [`MAX_JUMP_CHAIN`] gateways;
//! * a reusable `direct-tcpip` forwarding stream ([`SshConnection::open_forward_stream`])
//!   that backs the `rrs-tunnels` russh driver.
//!
//! The connection lifecycle (TCP/stream connect → host-key check → auth) lives
//! in one reusable primitive, [`SshConnection`], so shells, SFTP, jump-host
//! chaining, and tunnels all share the same code path (no copy-pasted auth /
//! known_hosts logic). Each hop in a chain is verified and authenticated
//! independently.
//!
//! Not yet implemented (clear errors / TODOs, no fake behavior):
//! * agent forwarding into the channel.
//!
//! Secrets: passwords / passphrases come from [`ResolvedCredentials`] only, are
//! never read from the profile, and are never logged.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use russh::client::{self, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::AgentClient;
use russh::keys::{load_secret_key, ssh_key, PrivateKeyWithHashAlg};
use russh::{Channel, ChannelMsg, ChannelStream, Disconnect};
use tokio::io::{AsyncRead, AsyncWrite};

use rrs_core::model::{AuthMethod, ConnectionProfile, ProtocolSettings, SshSettings};

use crate::error::{ProtocolError, Result};
use crate::traits::{
    Connector, DirEntry, EntryKind, JumpHop, RemoteSession, ResolvedCredentials, SftpClient,
    MAX_JUMP_CHAIN,
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

impl ClientHandler {
    /// Build a host-key-verifying handler from SSH settings.
    fn new(ssh: &SshSettings) -> Self {
        Self {
            host: ssh.host.clone(),
            port: ssh.port,
            strict: ssh.strict_host_key_checking,
            hostkey_status: Arc::new(AtomicU8::new(HK_PENDING)),
        }
    }
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

/// An `AsyncRead + AsyncWrite` stream carrying a single `direct-tcpip`
/// forwarding. Returned by [`SshConnection::open_forward_stream`] and pumped
/// byte-for-byte by the tunnel driver; it is `Unpin`, so it works directly with
/// `tokio::io::copy_bidirectional`.
pub type DirectTcpipStream = ChannelStream<client::Msg>;

/// A live, authenticated SSH connection: the single reusable primitive behind
/// shells, SFTP, jump-host chaining, and tunnels.
///
/// Construct one with [`connect`](Self::connect) (direct TCP) or
/// [`connect_via_jump_host`](Self::connect_via_jump_host) (through a gateway),
/// then open whatever you need on top: a [shell](Self::open_shell), an
/// [SFTP session](Self::open_sftp), or a [forwarding stream](Self::open_forward_stream).
pub struct SshConnection {
    handle: Handle<ClientHandler>,
    /// An optional underlying connection (e.g. a jump host) kept alive for this
    /// connection's lifetime: the target SSH runs over a `direct-tcpip` channel
    /// of the gateway, so dropping the gateway would tear down the transport.
    _via: Option<Box<SshConnection>>,
}

impl SshConnection {
    /// Connect over a fresh TCP socket, verify the host key, and authenticate.
    pub async fn connect(ssh: &SshSettings, creds: &ResolvedCredentials) -> Result<Self> {
        let config = Arc::new(client::Config::default());
        let mut handle = client::connect(
            config,
            (ssh.host.clone(), ssh.port),
            ClientHandler::new(ssh),
        )
        .await
        .map_err(|e| ProtocolError::Connect(e.to_string()))?;
        authenticate(&mut handle, ssh, creds).await?;
        Ok(Self { handle, _via: None })
    }

    /// Run the SSH handshake over an already-established byte stream (e.g. a
    /// jump host's `direct-tcpip` channel), then verify and authenticate. The
    /// `via` connection is retained so the underlying transport stays open.
    async fn connect_over_stream<R>(
        ssh: &SshSettings,
        creds: &ResolvedCredentials,
        stream: R,
        via: SshConnection,
    ) -> Result<Self>
    where
        R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let config = Arc::new(client::Config::default());
        let mut handle = client::connect_stream(config, stream, ClientHandler::new(ssh))
            .await
            .map_err(|e| ProtocolError::Connect(e.to_string()))?;
        authenticate(&mut handle, ssh, creds).await?;
        Ok(Self {
            handle,
            _via: Some(Box::new(via)),
        })
    }

    /// Open a `direct-tcpip` channel on this connection to `host:port` and run a
    /// full SSH session (host-key check + auth) over it, returning the new
    /// connection (which keeps `self` alive as its underlying transport).
    async fn hop_to(
        self,
        ssh: &SshSettings,
        creds: &ResolvedCredentials,
        what: &str,
    ) -> Result<Self> {
        let channel = self
            .handle
            .channel_open_direct_tcpip(ssh.host.clone(), ssh.port as u32, "127.0.0.1", 0)
            .await
            .map_err(|e| {
                ProtocolError::Connect(format!(
                    "direct-tcpip to {what} {}:{} failed: {e}",
                    ssh.host, ssh.port
                ))
            })?;
        SshConnection::connect_over_stream(ssh, creds, channel.into_stream(), self).await
    }

    /// Connect to `target_ssh` through the single gateway `jump_ssh`. Thin
    /// wrapper over [`connect_via_jump_chain`](Self::connect_via_jump_chain).
    pub async fn connect_via_jump_host(
        jump_ssh: &SshSettings,
        jump_creds: &ResolvedCredentials,
        target_ssh: &SshSettings,
        target_creds: &ResolvedCredentials,
    ) -> Result<Self> {
        SshConnection::connect_via_jump_chain(&[(jump_ssh, jump_creds)], target_ssh, target_creds)
            .await
    }

    /// Connect to `target_ssh` through an ordered chain of gateways
    /// (`gateways[0] → gateways[1] → … → target`).
    ///
    /// The first gateway is reached over a fresh TCP socket; every subsequent
    /// gateway (and finally the target) is reached over a `direct-tcpip` channel
    /// of the previous hop. Each hop is verified against its own `known_hosts`
    /// policy and authenticated from its own [`ResolvedCredentials`]; no
    /// auth/host-key logic is duplicated (every hop goes through
    /// [`connect`](Self::connect) / [`connect_over_stream`](Self::connect_over_stream)).
    ///
    /// An empty `gateways` slice degenerates to a direct [`connect`](Self::connect).
    pub async fn connect_via_jump_chain(
        gateways: &[(&SshSettings, &ResolvedCredentials)],
        target_ssh: &SshSettings,
        target_creds: &ResolvedCredentials,
    ) -> Result<Self> {
        let gw_settings: Vec<&SshSettings> = gateways.iter().map(|(s, _)| *s).collect();
        validate_jump_chain_endpoints(&gw_settings, target_ssh)?;

        let Some(((first_ssh, first_creds), rest)) = gateways.split_first() else {
            // No gateways → plain direct connect.
            return SshConnection::connect(target_ssh, target_creds).await;
        };

        // First gateway over a fresh TCP socket.
        let mut conn = SshConnection::connect(first_ssh, first_creds).await?;
        // Each subsequent gateway over the previous hop.
        for (next_ssh, next_creds) in rest {
            conn = conn.hop_to(next_ssh, next_creds, "jump host").await?;
        }
        // Final hop to the target over the last gateway.
        conn.hop_to(target_ssh, target_creds, "target").await
    }

    /// Open a bare session channel on this connection.
    async fn open_session_channel(&self) -> Result<Channel<client::Msg>> {
        self.handle
            .channel_open_session()
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))
    }

    /// Open an interactive PTY shell, consuming the connection (the resulting
    /// [`RusshSession`] keeps it — and any gateway — alive).
    pub async fn open_shell(self) -> Result<RusshSession> {
        let channel = self.open_session_channel().await?;
        channel
            .request_pty(false, DEFAULT_TERM, DEFAULT_COLS, DEFAULT_ROWS, 0, 0, &[])
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        channel
            .request_shell(true)
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        Ok(RusshSession {
            conn: self,
            channel,
            closed: false,
        })
    }

    /// Open an SFTP subsystem, consuming the connection.
    pub async fn open_sftp(self) -> Result<RusshSftp> {
        let channel = self.open_session_channel().await?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        let session = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| ProtocolError::Sftp(e.to_string()))?;
        Ok(RusshSftp {
            _conn: self,
            session,
        })
    }

    /// Open a `direct-tcpip` forwarding stream from the remote end to
    /// `host:port`. Each call yields an independent stream; the tunnel driver
    /// opens one per accepted local connection.
    pub async fn open_forward_stream(&self, host: &str, port: u16) -> Result<DirectTcpipStream> {
        let channel = self
            .handle
            .channel_open_direct_tcpip(host.to_string(), port as u32, "127.0.0.1", 0)
            .await
            .map_err(|e| ProtocolError::Channel(e.to_string()))?;
        Ok(channel.into_stream())
    }

    /// Send a clean SSH disconnect (best-effort).
    pub async fn disconnect(&self) {
        let _ = self
            .handle
            .disconnect(Disconnect::ByApplication, "", "")
            .await;
    }
}

/// Validate an ordered chain of gateway endpoints plus the final target before
/// opening any socket. Pure (unit-tested).
///
/// Checks: depth bound ([`MAX_JUMP_CHAIN`]); no empty host on any hop; and no
/// two *adjacent* endpoints identical (a hop to the same `host:port` is a
/// self-loop / no-op). Full cycle detection (by profile identity) is the
/// orchestration layer's job — here we only see resolved settings.
pub fn validate_jump_chain_endpoints(
    gateways: &[&SshSettings],
    target: &SshSettings,
) -> Result<()> {
    if gateways.len() > MAX_JUMP_CHAIN {
        return Err(ProtocolError::Connect(format!(
            "jump chain too deep: {} gateways (max {MAX_JUMP_CHAIN})",
            gateways.len()
        )));
    }
    for g in gateways {
        if g.host.trim().is_empty() {
            return Err(ProtocolError::Connect("jump host address is empty".into()));
        }
    }
    if target.host.trim().is_empty() {
        return Err(ProtocolError::Connect(
            "target host address is empty".into(),
        ));
    }
    // Walk gateway endpoints then the target, rejecting adjacent duplicates.
    let endpoints = gateways
        .iter()
        .map(|g| (g.host.as_str(), g.port))
        .chain(std::iter::once((target.host.as_str(), target.port)));
    let mut prev: Option<(&str, u16)> = None;
    for ep in endpoints {
        if Some(ep) == prev {
            return Err(ProtocolError::Connect(format!(
                "two consecutive jump endpoints are identical ({}:{})",
                ep.0, ep.1
            )));
        }
        prev = Some(ep);
    }
    Ok(())
}

/// Validate a single-hop jump chain. Thin wrapper over
/// [`validate_jump_chain_endpoints`]; kept for the single-hop call sites.
pub fn validate_jump_chain(jump: &SshSettings, target: &SshSettings) -> Result<()> {
    validate_jump_chain_endpoints(&[jump], target)
}

/// SSH connector backed by `russh`.
#[derive(Default)]
pub struct RusshConnector;

#[async_trait]
impl Connector for RusshConnector {
    async fn connect_shell(
        &self,
        profile: &ConnectionProfile,
        creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        let ssh = ssh_settings(profile)?;
        if ssh.jump_host.is_some() {
            // A jump host is referenced by another profile's id; resolving that
            // profile (and its secret) needs the profile/credential stores,
            // which the `Connector` trait deliberately does not expose. The
            // orchestration layer (`AppCore`) resolves both hops and calls
            // `connect_shell_via_jump` instead.
            return Err(ProtocolError::NotImplemented(
                "jump-host shell from a single profile — the orchestration layer must resolve \
                 both hops and call Connector::connect_shell_via_jump",
            ));
        }

        Ok(Box::new(
            SshConnection::connect(ssh, creds)
                .await?
                .open_shell()
                .await?,
        ))
    }

    /// Connect to `target` through the gateway `jump`, returning a shell on the
    /// **target** (not the gateway). Single-hop convenience over the chain path.
    async fn connect_shell_via_jump(
        &self,
        jump: &ConnectionProfile,
        jump_creds: &ResolvedCredentials,
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        self.connect_shell_via_jump_chain(&[JumpHop::new(jump, jump_creds)], target, target_creds)
            .await
    }

    async fn connect_sftp(
        &self,
        profile: &ConnectionProfile,
        creds: &ResolvedCredentials,
    ) -> Result<Box<dyn SftpClient>> {
        Ok(Box::new(RusshSftp::connect(profile, creds).await?))
    }

    async fn connect_sftp_via_jump(
        &self,
        jump: &ConnectionProfile,
        jump_creds: &ResolvedCredentials,
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Box<dyn SftpClient>> {
        self.connect_sftp_via_jump_chain(&[JumpHop::new(jump, jump_creds)], target, target_creds)
            .await
    }

    async fn connect_shell_via_jump_chain(
        &self,
        gateways: &[JumpHop<'_>],
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        let conn = connect_chain(gateways, target, target_creds).await?;
        Ok(Box::new(conn.open_shell().await?))
    }

    async fn connect_sftp_via_jump_chain(
        &self,
        gateways: &[JumpHop<'_>],
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Box<dyn SftpClient>> {
        let conn = connect_chain(gateways, target, target_creds).await?;
        Ok(Box::new(conn.open_sftp().await?))
    }
}

/// Resolve profiles to SSH settings and establish a connection to `target`
/// through the ordered `gateways`. Shared by the shell and SFTP chain paths.
async fn connect_chain(
    gateways: &[JumpHop<'_>],
    target: &ConnectionProfile,
    target_creds: &ResolvedCredentials,
) -> Result<SshConnection> {
    let target_ssh = ssh_settings(target)?;
    let mut hops: Vec<(&SshSettings, &ResolvedCredentials)> = Vec::with_capacity(gateways.len());
    for hop in gateways {
        hops.push((ssh_settings(hop.profile)?, hop.creds));
    }
    SshConnection::connect_via_jump_chain(&hops, target_ssh, target_creds).await
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

/// A live SSH shell exposed as a [`RemoteSession`]. Holds the connection
/// (kept alive for the channel's lifetime, including any jump host) and the
/// session channel.
pub struct RusshSession {
    conn: SshConnection,
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
        self.conn.disconnect().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SFTP
// ---------------------------------------------------------------------------

/// SFTP client over a russh connection. Construct with [`RusshSftp::connect`].
pub struct RusshSftp {
    _conn: SshConnection,
    session: russh_sftp::client::SftpSession,
}

impl RusshSftp {
    /// Open a direct (non-jump) SFTP session for `profile` using transient
    /// `creds`.
    ///
    /// A profile that names a jump host is rejected here on purpose: resolving
    /// the gateway profile + secret is the orchestration layer's job. Use
    /// [`connect_via_jump`](Self::connect_via_jump) (or
    /// [`Connector::connect_sftp_via_jump`]) with both hops resolved instead.
    pub async fn connect(profile: &ConnectionProfile, creds: &ResolvedCredentials) -> Result<Self> {
        let ssh = ssh_settings(profile)?;
        if ssh.jump_host.is_some() {
            return Err(ProtocolError::NotImplemented(
                "SFTP from a single jump-host profile — the orchestration layer must resolve \
                 both hops and call RusshSftp::connect_via_jump",
            ));
        }
        SshConnection::connect(ssh, creds).await?.open_sftp().await
    }

    /// Open an SFTP session on `target` through the gateway `jump` (single-hop).
    /// Reuses [`SshConnection::connect_via_jump_host`], so auth and host-key
    /// policy are identical to the shell path (no duplication).
    pub async fn connect_via_jump(
        jump: &ConnectionProfile,
        jump_creds: &ResolvedCredentials,
        target: &ConnectionProfile,
        target_creds: &ResolvedCredentials,
    ) -> Result<Self> {
        let jump_ssh = ssh_settings(jump)?;
        let target_ssh = ssh_settings(target)?;
        SshConnection::connect_via_jump_host(jump_ssh, jump_creds, target_ssh, target_creds)
            .await?
            .open_sftp()
            .await
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

    #[test]
    fn jump_chain_validation() {
        let jump = SshSettings {
            host: "gw.example".into(),
            port: 22,
            ..SshSettings::default()
        };
        let target = SshSettings {
            host: "10.0.0.5".into(),
            port: 22,
            ..SshSettings::default()
        };

        // Distinct, non-empty endpoints are fine.
        assert!(validate_jump_chain(&jump, &target).is_ok());

        // Empty target host is rejected.
        let mut bad = target.clone();
        bad.host = "  ".into();
        assert!(validate_jump_chain(&jump, &bad).is_err());

        // Empty jump host is rejected.
        let mut bad_jump = jump.clone();
        bad_jump.host = String::new();
        assert!(validate_jump_chain(&bad_jump, &target).is_err());

        // Jump == target (same host:port) is a config mistake.
        assert!(validate_jump_chain(&jump, &jump).is_err());
        // ...but the same host on a different port is allowed.
        let mut other_port = jump.clone();
        other_port.port = 2222;
        assert!(validate_jump_chain(&jump, &other_port).is_ok());
    }

    #[test]
    fn jump_chain_endpoints_validation() {
        let gw = |h: &str| SshSettings {
            host: h.into(),
            port: 22,
            ..SshSettings::default()
        };
        let g1 = gw("gw1");
        let g2 = gw("gw2");
        let g3 = gw("gw3");
        let target = gw("target");

        // A valid multi-hop chain.
        assert!(validate_jump_chain_endpoints(&[&g1, &g2, &g3], &target).is_ok());
        // No gateways (degenerate) is allowed — direct connect.
        assert!(validate_jump_chain_endpoints(&[], &target).is_ok());

        // Empty host anywhere is rejected.
        let empty = gw("  ");
        assert!(validate_jump_chain_endpoints(&[&g1, &empty], &target).is_err());

        // Adjacent identical endpoints (self-loop) are rejected...
        assert!(validate_jump_chain_endpoints(&[&g1, &g1], &target).is_err());
        assert!(validate_jump_chain_endpoints(&[&g1, &target], &target).is_err());
        // ...but a non-adjacent repeat is left to the orchestration layer.
        assert!(validate_jump_chain_endpoints(&[&g1, &g2, &g1], &target).is_ok());

        // Too many gateways is rejected.
        let many: Vec<SshSettings> = (0..MAX_JUMP_CHAIN + 1)
            .map(|i| gw(&format!("g{i}")))
            .collect();
        let refs: Vec<&SshSettings> = many.iter().collect();
        assert!(validate_jump_chain_endpoints(&refs, &target).is_err());
        // Exactly MAX_JUMP_CHAIN gateways is allowed.
        let max: Vec<SshSettings> = (0..MAX_JUMP_CHAIN).map(|i| gw(&format!("g{i}"))).collect();
        let max_refs: Vec<&SshSettings> = max.iter().collect();
        assert!(validate_jump_chain_endpoints(&max_refs, &target).is_ok());
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

    /// Live single-hop jump-host round-trip. Ignored by default (needs two
    /// reachable sshd endpoints, where the *jump* host can reach the *target*).
    /// Connects target-through-jump, opens a shell, and checks a command runs on
    /// the target. Run with publickey auth, e.g.:
    ///
    /// ```text
    /// NEXTERM_JUMP_TEST_HOST=gw.example   NEXTERM_JUMP_TEST_USER=$USER \
    /// NEXTERM_JUMP_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// NEXTERM_TARGET_TEST_HOST=10.0.0.5   NEXTERM_TARGET_TEST_USER=root \
    /// NEXTERM_TARGET_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// cargo test -p rrs-protocols --features ssh-russh -- --ignored jump_host_roundtrip
    /// ```
    #[tokio::test]
    #[ignore = "requires two reachable sshd endpoints; see doc comment"]
    async fn jump_host_roundtrip() {
        fn hop(prefix: &str) -> (SshSettings, ResolvedCredentials) {
            let host = std::env::var(format!("{prefix}_HOST")).expect("HOST");
            let user = std::env::var(format!("{prefix}_USER")).expect("USER");
            let key = std::env::var(format!("{prefix}_KEY")).ok();
            let ssh = SshSettings {
                host,
                username: user,
                private_key_path: key,
                strict_host_key_checking: false,
                ..SshSettings::default()
            };
            (ssh, ResolvedCredentials::default())
        }

        let (jump_ssh, jump_creds) = hop("NEXTERM_JUMP_TEST");
        let (target_ssh, target_creds) = hop("NEXTERM_TARGET_TEST");

        let conn = SshConnection::connect_via_jump_host(
            &jump_ssh,
            &jump_creds,
            &target_ssh,
            &target_creds,
        )
        .await
        .expect("jump connect");
        let mut session = conn.open_shell().await.expect("open shell");

        session.write(b"echo JUMP_OK\nexit\n").await.expect("write");
        let mut out = Vec::new();
        loop {
            let chunk = session.read().await.expect("read");
            if chunk.is_empty() {
                break;
            }
            out.extend_from_slice(&chunk);
        }
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("JUMP_OK"), "unexpected target output: {text}");
        session.close().await.expect("close");
    }

    /// Live single-hop **SFTP**-through-jump round-trip. Ignored by default
    /// (needs two reachable sshd endpoints, gateway able to reach the target).
    /// Lists a directory on the target via SFTP tunnelled through the gateway.
    /// Run with publickey auth, e.g.:
    ///
    /// ```text
    /// NEXTERM_JUMP_TEST_HOST=gw.example   NEXTERM_JUMP_TEST_USER=$USER \
    /// NEXTERM_JUMP_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// NEXTERM_TARGET_TEST_HOST=10.0.0.5   NEXTERM_TARGET_TEST_USER=root \
    /// NEXTERM_TARGET_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// NEXTERM_TARGET_TEST_SFTP_PATH=/tmp \
    /// cargo test -p rrs-protocols --features ssh-russh -- --ignored sftp_jump_roundtrip
    /// ```
    #[tokio::test]
    #[ignore = "requires two reachable sshd endpoints; see doc comment"]
    async fn sftp_jump_roundtrip() {
        fn hop(prefix: &str) -> (ConnectionProfile, ResolvedCredentials) {
            let host = std::env::var(format!("{prefix}_HOST")).expect("HOST");
            let user = std::env::var(format!("{prefix}_USER")).expect("USER");
            let key = std::env::var(format!("{prefix}_KEY")).ok();
            let mut profile = ConnectionProfile::new_ssh(prefix, &host, &user);
            if let ProtocolSettings::Ssh(s) = &mut profile.settings {
                s.private_key_path = key;
                s.strict_host_key_checking = false;
            }
            (profile, ResolvedCredentials::default())
        }

        let (jump, jump_creds) = hop("NEXTERM_JUMP_TEST");
        let (target, target_creds) = hop("NEXTERM_TARGET_TEST");
        let path = std::env::var("NEXTERM_TARGET_TEST_SFTP_PATH").unwrap_or_else(|_| "/".into());

        let sftp = RusshSftp::connect_via_jump(&jump, &jump_creds, &target, &target_creds)
            .await
            .expect("sftp via jump connect");
        // Listing must succeed; "." / ".." are usually present but not required.
        let listing = sftp.list_dir(&path).await.expect("list_dir over jump");
        println!("listed {} entries in {path} via jump host", listing.len());
    }

    /// Build a chain hop's SSH settings from `PREFIX_HOST/USER/KEY` env vars.
    #[cfg(test)]
    fn chain_hop(prefix: &str) -> (SshSettings, ResolvedCredentials) {
        let host = std::env::var(format!("{prefix}_HOST")).expect("HOST");
        let user = std::env::var(format!("{prefix}_USER")).expect("USER");
        let key = std::env::var(format!("{prefix}_KEY")).ok();
        let ssh = SshSettings {
            host,
            username: user,
            private_key_path: key,
            strict_host_key_checking: false,
            ..SshSettings::default()
        };
        (ssh, ResolvedCredentials::default())
    }

    /// Live two-gateway shell chain round-trip (`gw1 → gw2 → target`). Ignored by
    /// default (needs three reachable sshd endpoints, where gw1 reaches gw2 and
    /// gw2 reaches the target). Run with publickey auth, e.g.:
    ///
    /// ```text
    /// NEXTERM_CHAIN_JUMP1_HOST=gw1 NEXTERM_CHAIN_JUMP1_USER=$USER NEXTERM_CHAIN_JUMP1_KEY=~/.ssh/id_ed25519 \
    /// NEXTERM_CHAIN_JUMP2_HOST=gw2 NEXTERM_CHAIN_JUMP2_USER=$USER NEXTERM_CHAIN_JUMP2_KEY=~/.ssh/id_ed25519 \
    /// NEXTERM_CHAIN_TARGET_HOST=t  NEXTERM_CHAIN_TARGET_USER=root NEXTERM_CHAIN_TARGET_KEY=~/.ssh/id_ed25519 \
    /// cargo test -p rrs-protocols --features ssh-russh -- --ignored jump_chain_roundtrip
    /// ```
    #[tokio::test]
    #[ignore = "requires three reachable sshd endpoints; see doc comment"]
    async fn jump_chain_roundtrip() {
        let (gw1, gw1c) = chain_hop("NEXTERM_CHAIN_JUMP1");
        let (gw2, gw2c) = chain_hop("NEXTERM_CHAIN_JUMP2");
        let (target, tc) = chain_hop("NEXTERM_CHAIN_TARGET");

        let conn =
            SshConnection::connect_via_jump_chain(&[(&gw1, &gw1c), (&gw2, &gw2c)], &target, &tc)
                .await
                .expect("chain connect");
        let mut session = conn.open_shell().await.expect("open shell");
        session
            .write(b"echo CHAIN_OK\nexit\n")
            .await
            .expect("write");
        let mut out = Vec::new();
        loop {
            let chunk = session.read().await.expect("read");
            if chunk.is_empty() {
                break;
            }
            out.extend_from_slice(&chunk);
        }
        assert!(
            String::from_utf8_lossy(&out).contains("CHAIN_OK"),
            "unexpected target output"
        );
        session.close().await.expect("close");
    }

    /// Live two-gateway **SFTP** chain round-trip. Ignored by default. Same env
    /// as [`jump_chain_roundtrip`] plus `NEXTERM_CHAIN_TARGET_SFTP_PATH`.
    #[tokio::test]
    #[ignore = "requires three reachable sshd endpoints; see doc comment"]
    async fn sftp_jump_chain_roundtrip() {
        let (gw1, gw1c) = chain_hop("NEXTERM_CHAIN_JUMP1");
        let (gw2, gw2c) = chain_hop("NEXTERM_CHAIN_JUMP2");
        let (target, tc) = chain_hop("NEXTERM_CHAIN_TARGET");
        let path = std::env::var("NEXTERM_CHAIN_TARGET_SFTP_PATH").unwrap_or_else(|_| "/".into());

        let sftp =
            SshConnection::connect_via_jump_chain(&[(&gw1, &gw1c), (&gw2, &gw2c)], &target, &tc)
                .await
                .expect("chain connect")
                .open_sftp()
                .await
                .expect("open sftp");
        let listing = sftp.list_dir(&path).await.expect("list_dir over chain");
        println!("listed {} entries in {path} via 2-hop chain", listing.len());
    }
}
