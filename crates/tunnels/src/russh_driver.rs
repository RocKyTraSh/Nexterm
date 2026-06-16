//! Real port-forwarding driver over an SSH session (feature `ssh-russh`).
//!
//! Implements local forwarding (`ssh -L`): bind a local TCP listener and, for
//! every accepted connection, open a `direct-tcpip` channel on a shared SSH
//! connection and pump bytes both ways with [`tokio::io::copy_bidirectional`].
//!
//! Scope (minimal, by design):
//! * **Local** forwarding only. Remote (`-R`) and dynamic SOCKS (`-D`) return
//!   [`TunnelError::Unsupported`] with a clear message.
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
use crate::manager::{TunnelDriver, TunnelSpec};

/// Bookkeeping for one running local forward.
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
        // Errors for non-Local kinds and for missing target fields.
        let (target_host, target_port) = spec.local_forward_target()?;

        let listener = TcpListener::bind((bind_addr.as_str(), bind_port))
            .await
            .map_err(|e| {
                TunnelError::Driver(format!("bind {bind_addr}:{bind_port} failed: {e}"))
            })?;
        let local = listener
            .local_addr()
            .map_err(|e| TunnelError::Driver(e.to_string()))?;
        tracing::info!(
            tunnel = %spec.id,
            name = %spec.name,
            bind = %local,
            target = %format!("{target_host}:{target_port}"),
            "local forward started"
        );

        let (shutdown, _) = broadcast::channel::<()>(1);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            self.conn.clone(),
            target_host,
            target_port,
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
            tracing::info!(tunnel = %id, "local forward stopped");
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
    target_host: String,
    target_port: u16,
    shutdown: broadcast::Sender<()>,
    id: Uuid,
) {
    let mut shutdown_rx = shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            accept = listener.accept() => match accept {
                Ok((sock, peer)) => {
                    tokio::spawn(forward_conn(
                        conn.clone(),
                        sock,
                        target_host.clone(),
                        target_port,
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

/// Pump one accepted connection over a fresh `direct-tcpip` channel until either
/// side closes or the tunnel is stopped.
async fn forward_conn(
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
    tokio::select! {
        res = copy_bidirectional(&mut sock, &mut stream) => match res {
            Ok((up, down)) => {
                tracing::debug!(tunnel = %id, %peer, up, down, "forward closed");
            }
            Err(e) => {
                tracing::debug!(tunnel = %id, %peer, "forward error: {e}");
            }
        },
        _ = shutdown_rx.recv() => {
            tracing::debug!(tunnel = %id, %peer, "forward cancelled by stop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::TunnelManager;
    use rrs_core::model::SshSettings;

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
}
