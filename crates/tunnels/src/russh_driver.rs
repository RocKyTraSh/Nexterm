//! Real port-forwarding driver over an SSH session (feature `ssh-russh`).
//!
//! Binds a local TCP listener and, for every accepted connection, opens a
//! `direct-tcpip` channel on a shared SSH connection and pumps bytes both ways
//! with [`tokio::io::copy_bidirectional`]. The destination depends on the kind:
//! * **Local** (`ssh -L`): a fixed `host:port` from the spec;
//! * **Dynamic** (`ssh -D`): a SOCKS5 proxy — each connection negotiates its own
//!   destination (see [`crate::socks5`]).
//!
//! Scope (minimal, by design):
//! * Remote (`-R`) returns [`TunnelError::Unsupported`].
//! * One driver holds **one** SSH connection and forwards every spec over it;
//!   the spec's `ssh_profile_id` is not re-resolved here (that belongs to the
//!   orchestration layer). Construct one driver per SSH endpoint.
//!
//! Lifecycle / safety:
//! * each tunnel runs an accept loop plus one task per live connection;
//! * [`stop`](RusshTunnelDriver::stop) signals every task (via a broadcast
//!   channel) and aborts the accept loop — no task is left looping;
//! * dropping the driver aborts the accept loops best-effort;
//! * nothing blocks the async runtime, and no secret is logged.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

use rrs_core::model::SshSettings;
use rrs_protocols::{ResolvedCredentials, SshConnection};

use crate::error::{Result, TunnelError};
use crate::manager::{TunnelDriver, TunnelKind, TunnelSpec};
use crate::socks5;

/// What a tunnel's accepted connections should do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ForwardMode {
    /// `-L`: every connection forwards to a fixed `host:port`.
    Local { host: String, port: u16 },
    /// `-D`: every connection is a SOCKS5 request choosing its own destination.
    Dynamic,
}

/// Decide the forwarding mode for a spec (pure; unit-tested). Remote (`-R`) is
/// not implemented yet.
fn forward_mode_for(spec: &TunnelSpec) -> Result<ForwardMode> {
    match spec.kind {
        TunnelKind::Local => {
            let (host, port) = spec.local_forward_target()?;
            Ok(ForwardMode::Local { host, port })
        }
        TunnelKind::Dynamic => Ok(ForwardMode::Dynamic),
        TunnelKind::Remote => Err(TunnelError::Unsupported(
            "remote forwarding (ssh -R)".into(),
        )),
    }
}

/// Bookkeeping for one running tunnel.
struct RunningTunnel {
    /// Send `()` (or drop) to ask the accept loop and all forwards to stop.
    shutdown: broadcast::Sender<()>,
    accept_task: JoinHandle<()>,
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

#[async_trait]
impl TunnelDriver for RusshTunnelDriver {
    async fn start(&self, spec: &TunnelSpec) -> Result<()> {
        let (bind_addr, bind_port) = spec.bind_endpoint()?;
        // Errors for Remote (`-R`) and for a Local spec missing its target.
        let mode = forward_mode_for(spec)?;

        let listener = TcpListener::bind((bind_addr.as_str(), bind_port))
            .await
            .map_err(|e| {
                TunnelError::Driver(format!("bind {bind_addr}:{bind_port} failed: {e}"))
            })?;
        let local = listener
            .local_addr()
            .map_err(|e| TunnelError::Driver(e.to_string()))?;
        match &mode {
            ForwardMode::Local { host, port } => tracing::info!(
                tunnel = %spec.id, name = %spec.name, bind = %local,
                target = %format!("{host}:{port}"), "local forward started"
            ),
            ForwardMode::Dynamic => tracing::info!(
                tunnel = %spec.id, name = %spec.name, bind = %local,
                "dynamic SOCKS proxy started"
            ),
        }

        let (shutdown, _) = broadcast::channel::<()>(1);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            self.conn.clone(),
            mode,
            shutdown.clone(),
            spec.id,
        ));

        self.running.lock().expect("tunnel map poisoned").insert(
            spec.id,
            RunningTunnel {
                shutdown,
                accept_task,
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
            // Signal every forward task, then make sure the accept loop is gone.
            let _ = rt.shutdown.send(());
            rt.accept_task.abort();
            let _ = rt.accept_task.await;
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
    mode: ForwardMode,
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
    mode: ForwardMode,
    shutdown_rx: broadcast::Receiver<()>,
    id: Uuid,
    peer: SocketAddr,
) {
    match mode {
        ForwardMode::Local { host, port } => {
            forward_local(conn, sock, host, port, shutdown_rx, id, peer).await
        }
        ForwardMode::Dynamic => forward_dynamic(conn, sock, shutdown_rx, id, peer).await,
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

        // Remote (`-R`) is not implemented yet.
        let mut remote = TunnelSpec::new_local("r", profile, 8080, "x", 1);
        remote.kind = TunnelKind::Remote;
        assert!(matches!(
            forward_mode_for(&remote),
            Err(TunnelError::Unsupported(_))
        ));

        // A Local spec missing its target is rejected before any socket opens.
        let mut bad_local = TunnelSpec::new_local("b", profile, 8080, "x", 1);
        bad_local.target_host = None;
        assert!(matches!(
            forward_mode_for(&bad_local),
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
}
