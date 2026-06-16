//! HTTP file server mini-server (axum + tower-http `ServeDir`).
//!
//! Safe by default: binds loopback and serves read-only. Exposing it on a
//! non-loopback address triggers [`MiniServerConfig::security_warning`].

use async_trait::async_trait;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tower_http::services::ServeDir;
use tracing::info;

use crate::error::{MiniServerError, Result};
use crate::service::{MiniServer, MiniServerConfig, ServerState};

/// A read-only static-file HTTP server.
pub struct HttpFileServer {
    config: MiniServerConfig,
    state: ServerState,
    shutdown: Option<oneshot::Sender<()>>,
}

impl HttpFileServer {
    pub fn new(config: MiniServerConfig) -> Self {
        Self { config, state: ServerState::Stopped, shutdown: None }
    }
}

#[async_trait]
impl MiniServer for HttpFileServer {
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
        let root = self.config.root_dir.clone().unwrap_or_else(|| ".".to_string());
        let serve_dir = ServeDir::new(root);
        // If a future axum version rejects a bare Router here, use
        // `app.into_make_service()`.
        let app = Router::new().fallback_service(serve_dir);

        let listener = TcpListener::bind((self.config.bind_address.as_str(), self.config.port))
            .await
            .map_err(|e| {
                MiniServerError::Start(format!(
                    "bind {}:{}: {e}",
                    self.config.bind_address, self.config.port
                ))
            })?;

        let (tx, rx) = oneshot::channel::<()>();
        self.shutdown = Some(tx);

        info!(server = %self.config.name, addr = %self.config.bind_address, port = self.config.port, "starting HTTP file server");
        tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = rx.await;
            });
            if let Err(e) = server.await {
                tracing::error!(error = %e, "HTTP file server stopped with error");
            }
        });

        self.state = ServerState::Running;
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.state = ServerState::Stopped;
        Ok(())
    }
}
