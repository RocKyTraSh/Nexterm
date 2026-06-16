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
use rrs_protocols::{Connector, RemoteSession, ResolvedCredentials, SftpClient};

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
    ///
    /// If the profile is SSH and names a `jump_host`, the gateway profile and
    /// both hops' secrets are resolved here (orchestration stays in `AppCore`,
    /// not the `Connector`) and the session is opened on the **target** through
    /// the gateway.
    pub async fn connect(
        &self,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<(Uuid, Box<dyn RemoteSession>)> {
        // Resolve the connector first so an unsupported profile fails before we
        // register a dangling session.
        let connector = self.connector_for(profile)?;
        let session = RuntimeSession::new(profile.name.clone(), profile.kind(), Some(profile.id));
        let id = self.sessions.register(session).await;

        match self.open_shell(connector.as_ref(), profile).await {
            Ok(transport) => {
                self.sessions.set_state(id, SessionState::Connected).await;
                Ok((id, transport))
            }
            Err(e) => {
                // Resolution failures (jump profile missing / not SSH / chain too
                // long) and transport failures alike mark the session Failed.
                self.sessions
                    .set_state(id, SessionState::Failed(e.to_string()))
                    .await;
                Err(anyhow::anyhow!("connect failed: {e}"))
            }
        }
    }

    /// Open a shell for `profile`, dispatching to the jump-host path when the
    /// profile names a gateway. Pure orchestration over the [`Connector`] trait.
    async fn open_shell(
        &self,
        connector: &dyn Connector,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<Box<dyn RemoteSession>> {
        if let Some(jump_id) = ssh_jump_host(profile) {
            let (jump, jump_creds, target_creds) = self.resolve_jump(profile, jump_id).await?;
            connector
                .connect_shell_via_jump(&jump, &jump_creds, profile, &target_creds)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        } else {
            let creds = self.resolve_credentials(profile).await?;
            connector
                .connect_shell(profile, &creds)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        }
    }

    /// Open an SFTP client for `profile` (UI-friendly facade for the file
    /// browser). Works directly or, when the profile names a `jump_host`,
    /// through the gateway — the same orchestration as [`connect`](Self::connect).
    ///
    /// SFTP is SSH-only, so this always uses the main SSH connector; a non-SSH
    /// profile yields a clear error from the connector.
    pub async fn connect_sftp(
        &self,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<Box<dyn SftpClient>> {
        let connector = Arc::clone(&self.connector);
        if let Some(jump_id) = ssh_jump_host(profile) {
            let (jump, jump_creds, target_creds) = self.resolve_jump(profile, jump_id).await?;
            connector
                .connect_sftp_via_jump(&jump, &jump_creds, profile, &target_creds)
                .await
                .map_err(|e| anyhow::anyhow!("sftp connect failed: {e}"))
        } else {
            let creds = self.resolve_credentials(profile).await?;
            connector
                .connect_sftp(profile, &creds)
                .await
                .map_err(|e| anyhow::anyhow!("sftp connect failed: {e}"))
        }
    }

    /// Resolve a single-hop jump chain: load the gateway profile from the store,
    /// validate it, and resolve transient credentials for both hops.
    ///
    /// Errors are explicit: gateway not found, gateway is not SSH, or the chain
    /// is longer than one hop (the gateway itself names a jump host).
    async fn resolve_jump(
        &self,
        target: &ConnectionProfile,
        jump_id: Uuid,
    ) -> anyhow::Result<(ConnectionProfile, ResolvedCredentials, ResolvedCredentials)> {
        let jump = self
            .profiles
            .get_profile(jump_id)
            .await
            .context("loading jump host profile")?
            .with_context(|| format!("jump host profile {jump_id} not found"))?;

        match &jump.settings {
            ProtocolSettings::Ssh(s) => {
                if s.jump_host.is_some() {
                    anyhow::bail!(
                        "jump host chains longer than one hop are not supported yet \
                         (gateway '{}' itself names a jump host)",
                        jump.name
                    );
                }
            }
            other => anyhow::bail!(
                "jump host profile '{}' is not an SSH profile (it is {:?})",
                jump.name,
                other.kind()
            ),
        }

        let jump_creds = self.resolve_credentials(&jump).await?;
        let target_creds = self.resolve_credentials(target).await?;
        Ok((jump, jump_creds, target_creds))
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

/// The jump-host gateway id for an SSH profile, if any. Non-SSH profiles never
/// have one.
fn ssh_jump_host(profile: &ConnectionProfile) -> Option<Uuid> {
    match &profile.settings {
        ProtocolSettings::Ssh(s) => s.jump_host,
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests: jump-host orchestration (no network — a spy connector records the path)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    use rrs_core::error::Result as CoreResult;
    use rrs_core::model::{Group, SshSettings};
    use rrs_credentials::MemoryCredentialStore;
    use rrs_protocols::{DirEntry, EntryKind};

    /// In-memory `ProfileStore` for orchestration tests.
    #[derive(Default)]
    struct MemProfileStore {
        profiles: StdMutex<Vec<ConnectionProfile>>,
    }

    impl MemProfileStore {
        fn with(profiles: Vec<ConnectionProfile>) -> Self {
            Self {
                profiles: StdMutex::new(profiles),
            }
        }
    }

    #[async_trait]
    impl ProfileStore for MemProfileStore {
        async fn list_profiles(&self) -> CoreResult<Vec<ConnectionProfile>> {
            Ok(self.profiles.lock().unwrap().clone())
        }
        async fn get_profile(&self, id: Uuid) -> CoreResult<Option<ConnectionProfile>> {
            Ok(self
                .profiles
                .lock()
                .unwrap()
                .iter()
                .find(|p| p.id == id)
                .cloned())
        }
        async fn upsert_profile(&self, profile: ConnectionProfile) -> CoreResult<()> {
            self.profiles.lock().unwrap().push(profile);
            Ok(())
        }
        async fn delete_profile(&self, _id: Uuid) -> CoreResult<()> {
            Ok(())
        }
        async fn list_groups(&self) -> CoreResult<Vec<Group>> {
            Ok(vec![])
        }
        async fn upsert_group(&self, _group: Group) -> CoreResult<()> {
            Ok(())
        }
        async fn delete_group(&self, _id: Uuid) -> CoreResult<()> {
            Ok(())
        }
    }

    /// Connector that records which path was taken instead of touching a network.
    #[derive(Default)]
    struct SpyConnector {
        last: StdMutex<Option<String>>,
    }

    impl SpyConnector {
        fn last(&self) -> Option<String> {
            self.last.lock().unwrap().clone()
        }
        fn record(&self, what: String) {
            *self.last.lock().unwrap() = Some(what);
        }
    }

    struct NullSession;

    #[async_trait]
    impl RemoteSession for NullSession {
        async fn write(&mut self, _data: &[u8]) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn read(&mut self) -> rrs_protocols::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        async fn resize(&mut self, _cols: u16, _rows: u16) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn close(&mut self) -> rrs_protocols::Result<()> {
            Ok(())
        }
    }

    struct NullSftp;

    #[async_trait]
    impl SftpClient for NullSftp {
        async fn list_dir(&self, _path: &str) -> rrs_protocols::Result<Vec<DirEntry>> {
            Ok(Vec::new())
        }
        async fn stat(&self, path: &str) -> rrs_protocols::Result<DirEntry> {
            Ok(DirEntry {
                name: path.to_string(),
                kind: EntryKind::Dir,
                size: 0,
                permissions: None,
                modified_unix: None,
            })
        }
        async fn read_file(&self, _path: &str) -> rrs_protocols::Result<Vec<u8>> {
            Ok(Vec::new())
        }
        async fn write_file(&self, _path: &str, _data: &[u8]) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn make_dir(&self, _path: &str) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn remove_file(&self, _path: &str) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn remove_dir(&self, _path: &str) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn rename(&self, _from: &str, _to: &str) -> rrs_protocols::Result<()> {
            Ok(())
        }
        async fn set_permissions(&self, _path: &str, _mode: u32) -> rrs_protocols::Result<()> {
            Ok(())
        }
    }

    #[async_trait]
    impl Connector for SpyConnector {
        async fn connect_shell(
            &self,
            profile: &ConnectionProfile,
            _creds: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn RemoteSession>> {
            self.record(format!("shell:{}", profile.name));
            Ok(Box::new(NullSession))
        }
        async fn connect_shell_via_jump(
            &self,
            jump: &ConnectionProfile,
            _jc: &ResolvedCredentials,
            target: &ConnectionProfile,
            _tc: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn RemoteSession>> {
            self.record(format!("jump:{}->{}", jump.name, target.name));
            Ok(Box::new(NullSession))
        }
        async fn connect_sftp(
            &self,
            profile: &ConnectionProfile,
            _creds: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn SftpClient>> {
            self.record(format!("sftp:{}", profile.name));
            Ok(Box::new(NullSftp))
        }
        async fn connect_sftp_via_jump(
            &self,
            jump: &ConnectionProfile,
            _jc: &ResolvedCredentials,
            target: &ConnectionProfile,
            _tc: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn SftpClient>> {
            self.record(format!("sftp_jump:{}->{}", jump.name, target.name));
            Ok(Box::new(NullSftp))
        }
    }

    fn core_with(store: MemProfileStore, spy: Arc<SpyConnector>) -> AppCore {
        let creds: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::new());
        AppCore::new(AppConfig::default(), Arc::new(store), creds, spy)
    }

    fn ssh(name: &str) -> ConnectionProfile {
        ConnectionProfile::new_ssh(name, "host.example", "user")
    }

    fn with_jump(mut p: ConnectionProfile, jump_id: Uuid) -> ConnectionProfile {
        if let ProtocolSettings::Ssh(s) = &mut p.settings {
            s.jump_host = Some(jump_id);
        }
        p
    }

    #[tokio::test]
    async fn connect_without_jump_uses_direct_shell() {
        let spy = Arc::new(SpyConnector::default());
        let core = core_with(MemProfileStore::default(), spy.clone());
        core.connect(&ssh("direct")).await.unwrap();
        assert_eq!(spy.last().as_deref(), Some("shell:direct"));
    }

    #[tokio::test]
    async fn connect_with_jump_resolves_gateway() {
        let spy = Arc::new(SpyConnector::default());
        let gw = ssh("gw");
        let target = with_jump(ssh("target"), gw.id);
        let core = core_with(MemProfileStore::with(vec![gw]), spy.clone());
        core.connect(&target).await.unwrap();
        assert_eq!(spy.last().as_deref(), Some("jump:gw->target"));
    }

    #[tokio::test]
    async fn jump_profile_not_found_errors() {
        let spy = Arc::new(SpyConnector::default());
        let target = with_jump(ssh("target"), Uuid::new_v4());
        let core = core_with(MemProfileStore::default(), spy.clone());
        let err = core
            .connect(&target)
            .await
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "unexpected error: {err}");
        assert_eq!(spy.last(), None, "connector must not be called");
    }

    #[tokio::test]
    async fn jump_profile_not_ssh_errors() {
        let spy = Arc::new(SpyConnector::default());
        let gw = ConnectionProfile::new_local_shell("local-gw", None);
        let target = with_jump(ssh("target"), gw.id);
        let core = core_with(MemProfileStore::with(vec![gw]), spy.clone());
        let err = core
            .connect(&target)
            .await
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("not an SSH profile"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn jump_chain_longer_than_one_hop_errors() {
        let spy = Arc::new(SpyConnector::default());
        let gw2 = ssh("gw2");
        let gw1 = with_jump(ssh("gw1"), gw2.id);
        let target = with_jump(ssh("target"), gw1.id);
        let core = core_with(MemProfileStore::with(vec![gw2, gw1]), spy.clone());
        let err = core
            .connect(&target)
            .await
            .map(|_| ())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("longer than one hop"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn connect_sftp_picks_direct_vs_jump() {
        // Direct.
        let spy = Arc::new(SpyConnector::default());
        let core = core_with(MemProfileStore::default(), spy.clone());
        core.connect_sftp(&ssh("direct")).await.unwrap();
        assert_eq!(spy.last().as_deref(), Some("sftp:direct"));

        // Via jump.
        let spy2 = Arc::new(SpyConnector::default());
        let gw = ssh("gw");
        let target = with_jump(ssh("target"), gw.id);
        let core2 = core_with(MemProfileStore::with(vec![gw]), spy2.clone());
        core2.connect_sftp(&target).await.unwrap();
        assert_eq!(spy2.last().as_deref(), Some("sftp_jump:gw->target"));
    }

    #[test]
    fn ssh_jump_host_only_for_ssh_profiles() {
        assert_eq!(ssh_jump_host(&ssh("x")), None);
        let id = Uuid::new_v4();
        assert_eq!(ssh_jump_host(&with_jump(ssh("x"), id)), Some(id));
        let local = ConnectionProfile::new_local_shell("l", None);
        assert_eq!(ssh_jump_host(&local), None);
        // Sanity: a default SshSettings has no jump host.
        assert!(SshSettings::default().jump_host.is_none());
    }
}
