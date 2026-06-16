//! Real port-forwarding driver over an SSH session (feature `ssh-russh`),
//! supporting all three forwarding kinds:
//! * **Local** (`ssh -L`): a local listener; each connection opens a
//!   `direct-tcpip` channel to a fixed `host:port` from the spec;
//! * **Dynamic** (`ssh -D`): a local SOCKS5 proxy — each connection negotiates
//!   its own destination over `direct-tcpip` (see [`crate::socks5`]);
//! * **Remote** (`ssh -R`): the server listens (`tcpip-forward`) and opens a
//!   `forwarded-tcpip` channel back to us per connection; we dial the local
//!   target and pump bytes. The `forwarded-tcpip` channels arrive through the
//!   SSH connection's handler (see [`rrs_protocols::ForwardedConnection`]).
//!
//! Bytes are pumped both ways with [`tokio::io::copy_bidirectional`]. One driver
//! holds **one** SSH connection and forwards every spec over it; the spec's
//! `ssh_profile_id` is not re-resolved here (that belongs to the orchestration
//! layer). Construct one driver per SSH endpoint. A single SSH connection hosts
//! at most one active remote (`-R`) forward (the forwarded-channel receiver is
//! taken once).
//!
//! Lifecycle / safety:
//! * each tunnel runs an accept/forwarded loop plus one task per live connection;
//! * [`stop`](RusshTunnelDriver::stop) signals every task (via a broadcast
//!   channel), aborts the loop, and (for `-R`) cancels the server-side bind —
//!   no task is left looping;
//! * dropping the driver aborts the loops best-effort;
//! * nothing blocks the async runtime, and no secret is logged.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use uuid::Uuid;

use rrs_core::model::SshSettings;
use rrs_protocols::{ForwardedConnection, ResolvedCredentials, SshConnection};

use crate::error::{Result, TunnelError};
use crate::manager::{TunnelDriver, TunnelKind, TunnelSpec};
use crate::socks5;

/// The forwarding kind resolved from a spec (pure; unit-tested).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ForwardMode {
    /// `-L`: every local connection forwards to a fixed `host:port`.
    Local { host: String, port: u16 },
    /// `-D`: every local connection is a SOCKS5 request choosing its destination.
    Dynamic,
    /// `-R`: the server listens on `remote_*` and forwards connections back to
    /// us; we connect them to `local_*` on this machine.
    Remote {
        remote_host: String,
        remote_port: u16,
        local_host: String,
        local_port: u16,
    },
}

/// What an *accepted local* connection should do (the listener-based modes).
#[derive(Debug, Clone)]
enum ConnMode {
    Local { host: String, port: u16 },
    Dynamic,
}

/// Decide the forwarding mode for a spec (pure; unit-tested).
fn forward_mode_for(spec: &TunnelSpec) -> Result<ForwardMode> {
    match spec.kind {
        TunnelKind::Local => {
            let (host, port) = spec.local_forward_target()?;
            Ok(ForwardMode::Local { host, port })
        }
        TunnelKind::Dynamic => Ok(ForwardMode::Dynamic),
        TunnelKind::Remote => {
            let ((remote_host, remote_port), (local_host, local_port)) =
                spec.remote_forward_endpoints()?;
            Ok(ForwardMode::Remote {
                remote_host,
                remote_port,
                local_host,
                local_port,
            })
        }
    }
}

/// Bookkeeping for one running tunnel.
struct RunningTunnel {
    /// Send `()` (or drop) to ask the accept loop and all forwards to stop.
    shutdown: broadcast::Sender<()>,
    /// The accept loop (local listener) or the forwarded-channel loop (remote).
    accept_task: JoinHandle<()>,
    /// For remote (`-R`) forwards: the `(bind address, assigned port)` to cancel
    /// on the server when stopping.
    remote_cancel: Option<(String, u16)>,
}

/// Local port-forwarding driver over a single shared SSH connection.
pub struct RusshTunnelDriver {
    conn: Arc<SshConnection>,
    running: Mutex<HashMap<Uuid, RunningTunnel>>,
}

impl RusshTunnelDriver {
    /// Establish a fresh SSH transport for all forwards.
    pub async fn connect(ssh: &SshSettings, creds: &ResolvedCredentials) -> Result<Self> {
        let conn = SshConnection::connect(ssh, creds)
            .await
            .map_err(|e| TunnelError::Driver(e.to_string()))?;
        Ok(Self::from_connection(Arc::new(conn)))
    }

    /// Establish a transport to `target` **through an ordered chain of
    /// gateways** (`gateways[0] → … → target`), and drive forwards over the
    /// final target SSH session.
    ///
    /// This is what makes remote (`ssh -R`) forwarding work through a jump
    /// chain: the resulting connection is the target's, so `tcpip-forward` is
    /// requested on the target and `forwarded-tcpip` channels arrive back over
    /// the target session (tunnelled through the chain). Local/dynamic forwards
    /// over a chain also work — their `direct-tcpip` opens on the target.
    pub async fn connect_via_jump_chain(
        gateways: &[(&SshSettings, &ResolvedCredentials)],
        target: &SshSettings,
        target_creds: &ResolvedCredentials,
    ) -> Result<Self> {
        let conn = SshConnection::connect_via_jump_chain(gateways, target, target_creds)
            .await
            .map_err(|e| TunnelError::Driver(e.to_string()))?;
        Ok(Self::from_connection(Arc::new(conn)))
    }

    /// Build a driver over an already-established connection (e.g. one shared
    /// with a shell, or a jump-host connection).
    pub fn from_connection(conn: Arc<SshConnection>) -> Self {
        Self {
            conn,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Number of currently-running tunnels (test/UI helper).
    pub fn running_count(&self) -> usize {
        self.running.lock().expect("tunnel map poisoned").len()
    }
}

impl RusshTunnelDriver {
    /// Bind a local listener for `-L`/`-D` and spawn its accept loop.
    async fn spawn_listener(
        &self,
        spec: &TunnelSpec,
        mode: ConnMode,
        shutdown: &broadcast::Sender<()>,
    ) -> Result<JoinHandle<()>> {
        let (bind_addr, bind_port) = spec.bind_endpoint()?;
        let listener = TcpListener::bind((bind_addr.as_str(), bind_port))
            .await
            .map_err(|e| {
                TunnelError::Driver(format!("bind {bind_addr}:{bind_port} failed: {e}"))
            })?;
        let local = listener
            .local_addr()
            .map_err(|e| TunnelError::Driver(e.to_string()))?;
        match &mode {
            ConnMode::Local { host, port } => tracing::info!(
                tunnel = %spec.id, name = %spec.name, bind = %local,
                target = %format!("{host}:{port}"), "local forward started"
            ),
            ConnMode::Dynamic => tracing::info!(
                tunnel = %spec.id, name = %spec.name, bind = %local,
                "dynamic SOCKS proxy started"
            ),
        }
        Ok(tokio::spawn(accept_loop(
            listener,
            self.conn.clone(),
            mode,
            shutdown.clone(),
            spec.id,
        )))
    }

    /// Request a remote (`-R`) forward and spawn the loop handling the
    /// `forwarded-tcpip` channels the server opens back to us.
    async fn spawn_remote(
        &self,
        spec: &TunnelSpec,
        remote_host: String,
        remote_port: u16,
        local_host: String,
        local_port: u16,
        shutdown: &broadcast::Sender<()>,
    ) -> Result<(JoinHandle<()>, Option<(String, u16)>)> {
        // Take the receiver first so a second remote forward fails cleanly
        // before we ask the server to bind.
        let rx = self
            .conn
            .take_forwarded_connections()
            .await
            .ok_or_else(|| {
                TunnelError::Driver(
                    "a remote forward is already active on this SSH connection".into(),
                )
            })?;
        let assigned = self
            .conn
            .request_remote_forward(&remote_host, remote_port)
            .await
            .map_err(|e| TunnelError::Driver(e.to_string()))?;
        tracing::info!(
            tunnel = %spec.id, name = %spec.name,
            remote_bind = %format!("{remote_host}:{assigned}"),
            local_target = %format!("{local_host}:{local_port}"),
            "remote forward started"
        );
        let task = tokio::spawn(forwarded_loop(
            rx,
            local_host,
            local_port,
            self.conn.clone(),
            shutdown.clone(),
            spec.id,
        ));
        Ok((task, Some((remote_host, assigned))))
    }
}

#[async_trait]
impl TunnelDriver for RusshTunnelDriver {
    async fn start(&self, spec: &TunnelSpec) -> Result<()> {
        // Resolves endpoints; errors for malformed specs before any I/O.
        let mode = forward_mode_for(spec)?;
        let (shutdown, _) = broadcast::channel::<()>(1);

        let (accept_task, remote_cancel) = match mode {
            ForwardMode::Local { host, port } => (
                self.spawn_listener(spec, ConnMode::Local { host, port }, &shutdown)
                    .await?,
                None,
            ),
            ForwardMode::Dynamic => (
                self.spawn_listener(spec, ConnMode::Dynamic, &shutdown)
                    .await?,
                None,
            ),
            ForwardMode::Remote {
                remote_host,
                remote_port,
                local_host,
                local_port,
            } => {
                self.spawn_remote(
                    spec,
                    remote_host,
                    remote_port,
                    local_host,
                    local_port,
                    &shutdown,
                )
                .await?
            }
        };

        self.running.lock().expect("tunnel map poisoned").insert(
            spec.id,
            RunningTunnel {
                shutdown,
                accept_task,
                remote_cancel,
            },
        );
        Ok(())
    }

    async fn stop(&self, id: Uuid) -> Result<()> {
        let running = self
            .running
            .lock()
            .expect("tunnel map poisoned")
            .remove(&id);
        if let Some(rt) = running {
            // Signal every forward task, then make sure the loop is gone.
            let _ = rt.shutdown.send(());
            rt.accept_task.abort();
            let _ = rt.accept_task.await;
            // For remote forwards, ask the server to stop listening.
            if let Some((addr, port)) = rt.remote_cancel {
                self.conn.cancel_remote_forward(&addr, port).await;
            }
            tracing::info!(tunnel = %id, "forward stopped");
        }
        Ok(())
    }
}

impl Drop for RusshTunnelDriver {
    fn drop(&mut self) {
        // Best-effort: abort any accept loops still running so they don't leak.
        if let Ok(mut map) = self.running.lock() {
            for (_, rt) in map.drain() {
                let _ = rt.shutdown.send(());
                rt.accept_task.abort();
            }
        }
    }
}

/// Accept connections until cancelled, spawning a forward task per connection.
async fn accept_loop(
    listener: TcpListener,
    conn: Arc<SshConnection>,
    mode: ConnMode,
    shutdown: broadcast::Sender<()>,
    id: Uuid,
) {
    let mut shutdown_rx = shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            accept = listener.accept() => match accept {
                Ok((sock, peer)) => {
                    tokio::spawn(handle_conn(
                        conn.clone(),
                        sock,
                        mode.clone(),
                        shutdown.subscribe(),
                        id,
                        peer,
                    ));
                }
                Err(e) => {
                    tracing::warn!(tunnel = %id, "accept failed, stopping forward: {e}");
                    break;
                }
            },
        }
    }
}

/// Dispatch one accepted connection to the right forwarder.
async fn handle_conn(
    conn: Arc<SshConnection>,
    sock: TcpStream,
    mode: ConnMode,
    shutdown_rx: broadcast::Receiver<()>,
    id: Uuid,
    peer: SocketAddr,
) {
    match mode {
        ConnMode::Local { host, port } => {
            forward_local(conn, sock, host, port, shutdown_rx, id, peer).await
        }
        ConnMode::Dynamic => forward_dynamic(conn, sock, shutdown_rx, id, peer).await,
    }
}

/// Copy bytes between `sock` and an open forward `stream` until either side
/// closes or the tunnel is stopped.
async fn pump_until_stop(
    sock: &mut TcpStream,
    stream: &mut rrs_protocols::DirectTcpipStream,
    shutdown_rx: &mut broadcast::Receiver<()>,
    id: Uuid,
    peer: SocketAddr,
) {
    tokio::select! {
        res = copy_bidirectional(sock, stream) => match res {
            Ok((up, down)) => tracing::debug!(tunnel = %id, %peer, up, down, "forward closed"),
            Err(e) => tracing::debug!(tunnel = %id, %peer, "forward error: {e}"),
        },
        _ = shutdown_rx.recv() => tracing::debug!(tunnel = %id, %peer, "forward cancelled by stop"),
    }
}

/// Local (`-L`) forward: a fixed destination opened immediately.
async fn forward_local(
    conn: Arc<SshConnection>,
    mut sock: TcpStream,
    host: String,
    port: u16,
    mut shutdown_rx: broadcast::Receiver<()>,
    id: Uuid,
    peer: SocketAddr,
) {
    let mut stream = match conn.open_forward_stream(&host, port).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(tunnel = %id, %peer, "direct-tcpip to {host}:{port} failed: {e}");
            return;
        }
    };
    pump_until_stop(&mut sock, &mut stream, &mut shutdown_rx, id, peer).await;
}

/// Dynamic (`-D`) forward: negotiate a SOCKS5 CONNECT, then forward to the
/// requested destination. The SOCKS success reply is sent only after the
/// `direct-tcpip` channel opens; failures send the matching SOCKS reply.
async fn forward_dynamic(
    conn: Arc<SshConnection>,
    mut sock: TcpStream,
    mut shutdown_rx: broadcast::Receiver<()>,
    id: Uuid,
    peer: SocketAddr,
) {
    if let Err(e) = socks5::negotiate_method(&mut sock).await {
        tracing::debug!(tunnel = %id, %peer, "socks5 method negotiation failed: {e}");
        return;
    }
    let req = match socks5::read_request(&mut sock).await {
        Ok(r) => r,
        Err(e) => {
            let _ = socks5::write_reply(&mut sock, e.reply_code()).await;
            tracing::debug!(tunnel = %id, %peer, "socks5 request rejected: {e}");
            return;
        }
    };

    let (host, port) = (req.host(), req.port);
    let mut stream = match conn.open_forward_stream(&host, port).await {
        Ok(s) => s,
        Err(e) => {
            let _ = socks5::write_reply(&mut sock, socks5::ReplyCode::HostUnreachable).await;
            tracing::debug!(tunnel = %id, %peer, "socks5 direct-tcpip to {host}:{port} failed: {e}");
            return;
        }
    };
    if let Err(e) = socks5::write_reply(&mut sock, socks5::ReplyCode::Succeeded).await {
        tracing::debug!(tunnel = %id, %peer, "socks5 success reply failed: {e}");
        return;
    }
    pump_until_stop(&mut sock, &mut stream, &mut shutdown_rx, id, peer).await;
}

/// Remote (`-R`) loop: receive `forwarded-tcpip` channels the server opens back
/// to us and connect each to the local target. Holds the `SshConnection` alive
/// for its lifetime (the channels' sender lives in the connection's handler).
async fn forwarded_loop(
    mut rx: mpsc::UnboundedReceiver<ForwardedConnection>,
    local_host: String,
    local_port: u16,
    _conn: Arc<SshConnection>,
    shutdown: broadcast::Sender<()>,
    id: Uuid,
) {
    let mut shutdown_rx = shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            maybe = rx.recv() => match maybe {
                Some(fwd) => {
                    tokio::spawn(handle_forwarded(
                        fwd,
                        local_host.clone(),
                        local_port,
                        shutdown.subscribe(),
                        id,
                    ));
                }
                None => {
                    // Sender dropped → the SSH connection is gone.
                    tracing::warn!(tunnel = %id, "remote forward source closed (SSH connection ended)");
                    break;
                }
            },
        }
    }
}

/// One incoming forwarded connection: dial the local target and pump bytes.
async fn handle_forwarded(
    mut fwd: ForwardedConnection,
    local_host: String,
    local_port: u16,
    mut shutdown_rx: broadcast::Receiver<()>,
    id: Uuid,
) {
    let origin = format!("{}:{}", fwd.originator_address, fwd.originator_port);
    let mut local = match TcpStream::connect((local_host.as_str(), local_port)).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                tunnel = %id, origin = %origin,
                "remote forward: local connect to {local_host}:{local_port} failed: {e}"
            );
            // Dropping fwd.stream closes the SSH channel.
            return;
        }
    };
    tokio::select! {
        res = copy_bidirectional(&mut local, &mut fwd.stream) => match res {
            Ok((up, down)) => tracing::debug!(tunnel = %id, origin = %origin, up, down, "remote forward closed"),
            Err(e) => tracing::debug!(tunnel = %id, origin = %origin, "remote forward error: {e}"),
        },
        _ = shutdown_rx.recv() => tracing::debug!(tunnel = %id, origin = %origin, "remote forward cancelled by stop"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::TunnelManager;
    use rrs_core::model::SshSettings;

    #[test]
    fn forward_mode_dispatch() {
        let profile = Uuid::new_v4();

        let local = TunnelSpec::new_local("l", profile, 8080, "10.0.0.5", 80);
        assert_eq!(
            forward_mode_for(&local).unwrap(),
            ForwardMode::Local {
                host: "10.0.0.5".into(),
                port: 80
            }
        );

        let dynamic = TunnelSpec::new_dynamic("d", profile, "127.0.0.1", 1080);
        assert_eq!(forward_mode_for(&dynamic).unwrap(), ForwardMode::Dynamic);

        // Remote (`-R`): remote bind + local target.
        let remote = TunnelSpec::new_remote("r", profile, "127.0.0.1", 18080, "127.0.0.1", 8080);
        assert_eq!(
            forward_mode_for(&remote).unwrap(),
            ForwardMode::Remote {
                remote_host: "127.0.0.1".into(),
                remote_port: 18080,
                local_host: "127.0.0.1".into(),
                local_port: 8080,
            }
        );

        // A Local spec missing its target is rejected before any socket opens.
        let mut bad_local = TunnelSpec::new_local("b", profile, 8080, "x", 1);
        bad_local.target_host = None;
        assert!(matches!(
            forward_mode_for(&bad_local),
            Err(TunnelError::InvalidSpec(_))
        ));

        // A Remote spec missing its local target is rejected.
        let mut bad_remote = TunnelSpec::new_remote("br", profile, "127.0.0.1", 1, "x", 1);
        bad_remote.target_port = None;
        assert!(matches!(
            forward_mode_for(&bad_remote),
            Err(TunnelError::InvalidSpec(_))
        ));
    }

    /// Live local-forward round-trip against a real sshd. Ignored by default
    /// (needs a server). It forwards a local port to the sshd's *own* listener
    /// (`127.0.0.1:<ssh port>`), connects through the tunnel, and checks the SSH
    /// identification banner comes back — proving bytes flow end-to-end over a
    /// `direct-tcpip` channel. Run with publickey auth, e.g.:
    ///
    /// ```text
    /// NEXTERM_SSH_TEST_HOST=127.0.0.1 \
    /// NEXTERM_SSH_TEST_USER=$USER \
    /// NEXTERM_SSH_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// cargo test -p rrs-tunnels --features ssh-russh -- --ignored local_tunnel_roundtrip
    /// ```
    #[tokio::test]
    #[ignore = "requires a reachable sshd; see doc comment"]
    async fn local_tunnel_roundtrip() {
        use tokio::io::AsyncReadExt;
        use tokio::net::TcpStream;

        let host = std::env::var("NEXTERM_SSH_TEST_HOST").expect("NEXTERM_SSH_TEST_HOST");
        let user = std::env::var("NEXTERM_SSH_TEST_USER").expect("NEXTERM_SSH_TEST_USER");
        let key = std::env::var("NEXTERM_SSH_TEST_KEY").expect("NEXTERM_SSH_TEST_KEY");
        let ssh_port: u16 = std::env::var("NEXTERM_SSH_TEST_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);

        let ssh = SshSettings {
            host: host.clone(),
            port: ssh_port,
            username: user,
            private_key_path: Some(key),
            strict_host_key_checking: false,
            ..SshSettings::default()
        };
        let creds = ResolvedCredentials::default();

        let driver = RusshTunnelDriver::connect(&ssh, &creds)
            .await
            .expect("ssh connect");
        let mut mgr = TunnelManager::new(Box::new(driver));

        // Forward an ephemeral local port to the sshd's own listener.
        let mut spec = TunnelSpec::new_local("roundtrip", Uuid::new_v4(), 0, "127.0.0.1", ssh_port);
        spec.bind_address = "127.0.0.1".into();
        // bind_port 0 lets the OS pick; but we need to learn it, so pick one.
        spec.bind_port = 18099;
        let id = mgr.add(spec);
        mgr.start(id).await.expect("start tunnel");

        // Connect through the tunnel and read the SSH banner of the target sshd.
        let mut conn = TcpStream::connect(("127.0.0.1", 18099))
            .await
            .expect("connect to local bind");
        let mut buf = [0u8; 16];
        let n = conn.read(&mut buf).await.expect("read banner");
        assert!(
            buf[..n].starts_with(b"SSH-"),
            "expected an SSH banner through the tunnel, got {:?}",
            &buf[..n]
        );

        mgr.stop(id).await.expect("stop tunnel");
    }

    /// Live dynamic-SOCKS round-trip against a real sshd. Ignored by default
    /// (needs a server with outbound network). Starts a SOCKS5 proxy over SSH,
    /// then drives the in-process SOCKS5 handshake against the sshd's own
    /// listener (`127.0.0.1:<ssh port>`) and checks the SSH banner comes back —
    /// proving the proxy forwards bytes end-to-end. Run e.g.:
    ///
    /// ```text
    /// NEXTERM_SSH_TEST_HOST=127.0.0.1 \
    /// NEXTERM_SSH_TEST_USER=$USER \
    /// NEXTERM_SSH_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// cargo test -p rrs-tunnels --features ssh-russh -- --ignored dynamic_socks_roundtrip
    /// ```
    ///
    /// `NEXTERM_SOCKS_TEST_URL` is honored by the manual `curl` recipe in the
    /// README; this in-process test does not need it.
    #[tokio::test]
    #[ignore = "requires a reachable sshd; see doc comment"]
    async fn dynamic_socks_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let host = std::env::var("NEXTERM_SSH_TEST_HOST").expect("NEXTERM_SSH_TEST_HOST");
        let user = std::env::var("NEXTERM_SSH_TEST_USER").expect("NEXTERM_SSH_TEST_USER");
        let key = std::env::var("NEXTERM_SSH_TEST_KEY").expect("NEXTERM_SSH_TEST_KEY");
        let ssh_port: u16 = std::env::var("NEXTERM_SSH_TEST_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);

        let ssh = SshSettings {
            host,
            port: ssh_port,
            username: user,
            private_key_path: Some(key),
            strict_host_key_checking: false,
            ..SshSettings::default()
        };
        let driver = RusshTunnelDriver::connect(&ssh, &ResolvedCredentials::default())
            .await
            .expect("ssh connect");
        let mut mgr = TunnelManager::new(Box::new(driver));

        let mut spec = TunnelSpec::new_dynamic("socks", Uuid::new_v4(), "127.0.0.1", 0);
        spec.bind_port = 11080;
        let id = mgr.add(spec);
        mgr.start(id).await.expect("start socks");

        // SOCKS5 handshake to the proxy, asking it to CONNECT to the sshd itself.
        let mut s = TcpStream::connect(("127.0.0.1", 11080u16))
            .await
            .expect("connect to socks bind");
        // Greeting: VER=5, 1 method, NO_AUTH.
        s.write_all(&[0x05, 0x01, 0x00]).await.expect("greeting");
        let mut method = [0u8; 2];
        s.read_exact(&mut method).await.expect("method reply");
        assert_eq!(method, [0x05, 0x00]);
        // CONNECT 127.0.0.1:<ssh_port>.
        let mut req = vec![0x05, 0x01, 0x00, 0x01, 127, 0, 0, 1];
        req.extend_from_slice(&ssh_port.to_be_bytes());
        s.write_all(&req).await.expect("connect request");
        let mut reply = [0u8; 10];
        s.read_exact(&mut reply).await.expect("connect reply");
        assert_eq!(reply[1], 0x00, "expected SOCKS success reply");

        // Now the stream is the sshd connection — read its banner.
        let mut buf = [0u8; 16];
        let n = s.read(&mut buf).await.expect("read banner via socks");
        assert!(
            buf[..n].starts_with(b"SSH-"),
            "expected an SSH banner through the SOCKS proxy, got {:?}",
            &buf[..n]
        );

        mgr.stop(id).await.expect("stop socks");
    }

    /// Live remote-forward round-trip against a real sshd. Ignored by default
    /// (needs a server where `AllowTcpForwarding` permits remote forwarding).
    /// Starts a local TCP echo server, requests a remote bind on the server, and
    /// connects to that bind *through the SSH connection itself* (a direct-tcpip
    /// to the remote bind) — the server then forwards back to our echo server.
    /// Verifies the echo. Run e.g.:
    ///
    /// ```text
    /// NEXTERM_SSH_TEST_HOST=127.0.0.1 NEXTERM_SSH_TEST_USER=$USER \
    /// NEXTERM_SSH_TEST_KEY=$HOME/.ssh/id_ed25519 \
    /// NEXTERM_REMOTE_TEST_BIND=127.0.0.1:18080 \
    /// cargo test -p rrs-tunnels --features ssh-russh -- --ignored remote_tunnel_roundtrip
    /// ```
    ///
    /// `NEXTERM_REMOTE_TEST_BIND` (default `127.0.0.1:18080`) is the remote bind;
    /// the local target is the echo server this test starts. `GatewayPorts` only
    /// matters when binding the remote side to a non-loopback address.
    #[tokio::test]
    #[ignore = "requires a reachable sshd with TCP forwarding; see doc comment"]
    async fn remote_tunnel_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let host = std::env::var("NEXTERM_SSH_TEST_HOST").expect("NEXTERM_SSH_TEST_HOST");
        let user = std::env::var("NEXTERM_SSH_TEST_USER").expect("NEXTERM_SSH_TEST_USER");
        let key = std::env::var("NEXTERM_SSH_TEST_KEY").expect("NEXTERM_SSH_TEST_KEY");
        let ssh_port: u16 = std::env::var("NEXTERM_SSH_TEST_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);
        let remote_bind =
            std::env::var("NEXTERM_REMOTE_TEST_BIND").unwrap_or_else(|_| "127.0.0.1:18080".into());
        let (rbind_host, rbind_port) = remote_bind.rsplit_once(':').expect("host:port");
        let rbind_port: u16 = rbind_port.parse().expect("port");

        // Local echo server (the `-R` local target).
        let echo = TcpListener::bind("127.0.0.1:0").await.expect("echo bind");
        let echo_addr = echo.local_addr().expect("echo addr");
        tokio::spawn(async move {
            while let Ok((mut c, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 1024];
                    while let Ok(n) = c.read(&mut b).await {
                        if n == 0 || c.write_all(&b[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        let ssh = SshSettings {
            host: host.clone(),
            port: ssh_port,
            username: user,
            private_key_path: Some(key),
            strict_host_key_checking: false,
            ..SshSettings::default()
        };
        // Keep a second connection to reach the remote bind for the test probe.
        let probe_conn = Arc::new(
            SshConnection::connect(&ssh, &ResolvedCredentials::default())
                .await
                .expect("probe ssh connect"),
        );
        let driver = RusshTunnelDriver::connect(&ssh, &ResolvedCredentials::default())
            .await
            .expect("ssh connect");
        let mut mgr = TunnelManager::new(Box::new(driver));

        let spec = TunnelSpec::new_remote(
            "remote",
            Uuid::new_v4(),
            rbind_host,
            rbind_port,
            "127.0.0.1",
            echo_addr.port(),
        );
        let id = mgr.add(spec);
        mgr.start(id).await.expect("start remote forward");

        // Connect to the remote bind (from the server's perspective) via the
        // probe connection's direct-tcpip, then echo a payload.
        let mut stream = probe_conn
            .open_forward_stream(rbind_host, rbind_port)
            .await
            .expect("reach remote bind");
        stream.write_all(b"ping").await.expect("write");
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.expect("read echo");
        assert_eq!(&buf, b"ping", "echo through remote forward mismatch");

        mgr.stop(id).await.expect("stop remote forward");
    }

    /// Live remote-forward-through-jump-chain round-trip. Ignored by default
    /// (needs reachable gateway(s) + target sshd, where the target permits
    /// `AllowTcpForwarding`). The `tcpip-forward` is requested on the **target**
    /// (the last hop), and `forwarded-tcpip` channels come back through the
    /// chain. Run e.g.:
    ///
    /// ```text
    /// NEXTERM_CHAIN_JUMP1_HOST=gw1 NEXTERM_CHAIN_JUMP1_USER=$USER NEXTERM_CHAIN_JUMP1_KEY=~/.ssh/id_ed25519 \
    /// NEXTERM_CHAIN_JUMP2_HOST=gw2 NEXTERM_CHAIN_JUMP2_USER=$USER NEXTERM_CHAIN_JUMP2_KEY=~/.ssh/id_ed25519 \
    /// NEXTERM_CHAIN_TARGET_HOST=t  NEXTERM_CHAIN_TARGET_USER=root NEXTERM_CHAIN_TARGET_KEY=~/.ssh/id_ed25519 \
    /// NEXTERM_REMOTE_CHAIN_TEST_BIND=127.0.0.1:18080 \
    /// cargo test -p rrs-tunnels --features ssh-russh -- --ignored remote_tunnel_chain_roundtrip
    /// ```
    ///
    /// `NEXTERM_CHAIN_JUMP2_*` is optional (use a single gateway if unset).
    #[tokio::test]
    #[ignore = "requires reachable gateway(s) + target sshd with TCP forwarding; see doc comment"]
    async fn remote_tunnel_chain_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        fn hop(prefix: &str) -> SshSettings {
            SshSettings {
                host: std::env::var(format!("{prefix}_HOST")).expect("HOST"),
                username: std::env::var(format!("{prefix}_USER")).expect("USER"),
                private_key_path: std::env::var(format!("{prefix}_KEY")).ok(),
                strict_host_key_checking: false,
                ..SshSettings::default()
            }
        }

        // Gateways in connection order (JUMP2 optional).
        let mut gw_settings = vec![hop("NEXTERM_CHAIN_JUMP1")];
        if std::env::var_os("NEXTERM_CHAIN_JUMP2_HOST").is_some() {
            gw_settings.push(hop("NEXTERM_CHAIN_JUMP2"));
        }
        let target = hop("NEXTERM_CHAIN_TARGET");
        let nocreds = ResolvedCredentials::default();
        let gateways: Vec<(&SshSettings, &ResolvedCredentials)> =
            gw_settings.iter().map(|s| (s, &nocreds)).collect();

        let remote_bind = std::env::var("NEXTERM_REMOTE_CHAIN_TEST_BIND")
            .unwrap_or_else(|_| "127.0.0.1:18080".into());
        let (rbind_host, rbind_port) = remote_bind.rsplit_once(':').expect("host:port");
        let rbind_port: u16 = rbind_port.parse().expect("port");

        // Local echo server (the `-R` local target on this machine).
        let echo = TcpListener::bind("127.0.0.1:0").await.expect("echo bind");
        let echo_addr = echo.local_addr().expect("echo addr");
        tokio::spawn(async move {
            while let Ok((mut c, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let mut b = [0u8; 1024];
                    while let Ok(n) = c.read(&mut b).await {
                        if n == 0 || c.write_all(&b[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        // The driver's connection is the target's, reached through the chain.
        let driver = RusshTunnelDriver::connect_via_jump_chain(&gateways, &target, &nocreds)
            .await
            .expect("chain connect for tunnel");
        let mut mgr = TunnelManager::new(Box::new(driver));

        let spec = TunnelSpec::new_remote(
            "remote-chain",
            Uuid::new_v4(),
            rbind_host,
            rbind_port,
            "127.0.0.1",
            echo_addr.port(),
        );
        let id = mgr.add(spec);
        mgr.start(id).await.expect("start remote forward via chain");

        // A second chain connection to reach the target's remote bind and probe.
        let probe = Arc::new(
            SshConnection::connect_via_jump_chain(&gateways, &target, &nocreds)
                .await
                .expect("probe chain connect"),
        );
        let mut stream = probe
            .open_forward_stream(rbind_host, rbind_port)
            .await
            .expect("reach remote bind via target");
        stream.write_all(b"ping").await.expect("write");
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.expect("read echo");
        assert_eq!(
            &buf, b"ping",
            "echo through remote forward via chain mismatch"
        );

        mgr.stop(id).await.expect("stop remote forward via chain");
    }
}
