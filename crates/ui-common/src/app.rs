//! Application facade shared by all frontends.
//!
//! `AppCore` wires together config, the profile store, the credential store,
//! the session registry, and mini-servers, and exposes the high-level
//! operations the UI needs. Frontends (Qt/GTK/CLI) hold an `Arc<AppCore>` and
//! never touch transports or storage directly — which is what lets us add
//! protocols and swap backends without changing UI code.

use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;
use uuid::Uuid;

use rrs_core::config::AppConfig;
use rrs_core::event::EventBus;
use rrs_core::model::{
    ConnectionProfile, ProtocolKind, ProtocolSettings, RuntimeSession, SessionState,
};
use rrs_core::registry::SessionRegistry;
use rrs_core::store::{FileProfileStore, ProfileStore};
use rrs_credentials::{CredentialStore, Secret};
use rrs_miniservers::MiniServerManager;
use rrs_protocols::{Connector, RemoteSession, ResolvedCredentials};

/// The central application object.
pub struct AppCore {
    pub config: AppConfig,
    pub events: EventBus,
    pub sessions: SessionRegistry,
    profiles: Arc<dyn ProfileStore>,
    credentials: Arc<dyn CredentialStore>,
    /// The SSH connector to use. Swappable (mock vs russh) without touching
    /// callers.
    connector: Arc<dyn Connector>,
    /// Optional connector for local-shell profiles (e.g. the PTY-backed
    /// `LocalShellConnector`). Without one, connecting a local-shell profile
    /// fails with a clear error.
    local_connector: Option<Arc<dyn Connector>>,
    pub miniservers: Mutex<MiniServerManager>,
}

impl AppCore {
    /// Build an `AppCore` from its parts.
    pub fn new(
        config: AppConfig,
        profiles: Arc<dyn ProfileStore>,
        credentials: Arc<dyn CredentialStore>,
        connector: Arc<dyn Connector>,
    ) -> Self {
        let events = EventBus::default();
        let sessions = SessionRegistry::new(events.clone());
        Self {
            config,
            events,
            sessions,
            profiles,
            credentials,
            connector,
            local_connector: None,
            miniservers: Mutex::new(MiniServerManager::new()),
        }
    }

    /// Attach a connector to use for local-shell profiles. Builder-style so the
    /// app crate can wire it under a feature without changing `new`'s signature.
    pub fn with_local_connector(mut self, connector: Arc<dyn Connector>) -> Self {
        self.local_connector = Some(connector);
        self
    }

    /// Pick the connector for `profile`'s protocol family.
    fn connector_for(&self, profile: &ConnectionProfile) -> anyhow::Result<Arc<dyn Connector>> {
        match profile.kind() {
            ProtocolKind::LocalShell => self
                .local_connector
                .clone()
                .context("local-shell support is not enabled in this build"),
            _ => Ok(Arc::clone(&self.connector)),
        }
    }

    /// Convenience constructor using the default file store plus the provided
    /// connector and credential store.
    pub async fn with_defaults(
        credentials: Arc<dyn CredentialStore>,
        connector: Arc<dyn Connector>,
    ) -> anyhow::Result<Self> {
        let config_path = AppConfig::default_path();
        let config = AppConfig::load_or_default(&config_path)
            .await
            .context("loading config")?;
        let profiles = Arc::new(FileProfileStore::default_store());
        Ok(Self::new(config, profiles, credentials, connector))
    }

    /// Access the profile store (e.g. for the connection manager UI).
    pub fn profiles(&self) -> Arc<dyn ProfileStore> {
        Arc::clone(&self.profiles)
    }

    /// Resolve a profile's secret from the credential store (transiently).
    ///
    /// The returned [`ResolvedCredentials`] holds zero-on-drop secrets and is
    /// meant to be passed straight into a connector, never stored or logged.
    async fn resolve_credentials(
        &self,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<ResolvedCredentials> {
        let mut resolved = ResolvedCredentials::default();
        if let Some(cref) = &profile.credential {
            if let Some(secret) = self.credentials.get_secret(cref.id).await? {
                // For SSH we treat the stored secret as the password by default;
                // key passphrases use a separate ref in a later iteration.
                resolved.password = Some(secret);
            }
        }
        Ok(resolved)
    }

    /// Connect a profile and register the resulting session.
    ///
    /// Returns the new session id and the live transport. The caller (frontend)
    /// owns the [`RemoteSession`] and pumps its read/write loop into a terminal
    /// widget.
    pub async fn connect(
        &self,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<(Uuid, Box<dyn RemoteSession>)> {
        // Resolve the connector first so an unsupported profile fails before we
        // register a dangling session.
        let connector = self.connector_for(profile)?;
        let session = RuntimeSession::new(profile.name.clone(), profile.kind(), Some(profile.id));
        let id = self.sessions.register(session).await;

        let creds = self.resolve_credentials(profile).await?;
        match connector.connect_shell(profile, &creds).await {
            Ok(transport) => {
                self.sessions.set_state(id, SessionState::Connected).await;
                Ok((id, transport))
            }
            Err(e) => {
                self.sessions
                    .set_state(id, SessionState::Failed(e.to_string()))
                    .await;
                Err(anyhow::anyhow!("connect failed: {e}"))
            }
        }
    }

    /// Store a secret for a profile's credential reference.
    pub async fn set_profile_secret(
        &self,
        profile: &ConnectionProfile,
        secret: Secret,
    ) -> anyhow::Result<()> {
        let cref = profile
            .credential
            .as_ref()
            .context("profile has no credential reference")?;
        self.credentials.set_secret(cref.id, &secret).await?;
        Ok(())
    }

    /// Whether a profile is SSH (small helper for the UI).
    pub fn is_ssh(profile: &ConnectionProfile) -> bool {
        matches!(profile.settings, ProtocolSettings::Ssh(_))
    }
}
