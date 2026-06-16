//! Application facade shared by all frontends.
//!
//! `AppCore` wires together config, the profile store, the credential store,
//! the session registry, and mini-servers, and exposes the high-level
//! operations the UI needs. Frontends (Qt/GTK/CLI) hold an `Arc<AppCore>` and
//! never touch transports or storage directly — which is what lets us add
//! protocols and swap backends without changing UI code.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;
use uuid::Uuid;

use rrs_core::config::AppConfig;
use rrs_core::event::EventBus;
use rrs_core::model::{
    ConnectionProfile, CredentialRef, ProtocolKind, ProtocolSettings, RuntimeSession, SessionState,
};
use rrs_core::registry::SessionRegistry;
use rrs_core::store::{FileProfileStore, ProfileStore};
use rrs_credentials::{CredentialStore, Secret};
use rrs_miniservers::MiniServerManager;
use rrs_protocols::{
    Connector, JumpHop, RemoteSession, ResolvedCredentials, SftpClient, MAX_JUMP_CHAIN,
};

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

    /// Fetch the secret behind a credential ref, transiently.
    ///
    /// A dangling reference — the profile names a credential the secret store
    /// doesn't have — is a configuration error, surfaced clearly (never logged
    /// with the secret, because there is none to log).
    async fn resolve_ref(&self, cref: &CredentialRef) -> anyhow::Result<Secret> {
        self.credentials
            .get_secret(cref.id)
            .await
            .with_context(|| format!("reading secret for credential '{}'", cref.label))?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "credential '{}' ({}) is referenced by the profile but is not in the \
                     secret store",
                    cref.label,
                    cref.id
                )
            })
    }

    /// Resolve a profile's secrets from the credential store (transiently).
    ///
    /// Resolves the **password** (the profile-level [`ConnectionProfile::credential`])
    /// and, for SSH, the **private-key passphrase** ([`SshSettings::key_passphrase`])
    /// into two *independent* fields — they are never mixed. The returned
    /// [`ResolvedCredentials`] holds zero-on-drop secrets and is meant to be
    /// passed straight into a connector, never stored or logged.
    async fn resolve_credentials(
        &self,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<ResolvedCredentials> {
        let mut resolved = ResolvedCredentials::default();
        // Password: the profile-level credential reference.
        if let Some(cref) = &profile.credential {
            resolved.password = Some(self.resolve_ref(cref).await?);
        }
        // Private-key passphrase: the SSH-specific reference (independent secret).
        if let ProtocolSettings::Ssh(s) = &profile.settings {
            if let Some(cref) = &s.key_passphrase {
                resolved.key_passphrase = Some(self.resolve_ref(cref).await?);
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

    /// Open a shell for `profile`, dispatching to the jump-chain path when the
    /// profile names a gateway. Pure orchestration over the [`Connector`] trait.
    async fn open_shell(
        &self,
        connector: &dyn Connector,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<Box<dyn RemoteSession>> {
        if ssh_jump_host(profile).is_some() {
            let (gateways, target_creds) = self.resolve_jump_chain(profile).await?;
            let hops: Vec<JumpHop<'_>> = gateways.iter().map(|(p, c)| JumpHop::new(p, c)).collect();
            connector
                .connect_shell_via_jump_chain(&hops, profile, &target_creds)
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
    /// through the gateway chain — the same orchestration as [`connect`](Self::connect).
    ///
    /// SFTP is SSH-only, so this always uses the main SSH connector; a non-SSH
    /// profile yields a clear error from the connector.
    pub async fn connect_sftp(
        &self,
        profile: &ConnectionProfile,
    ) -> anyhow::Result<Box<dyn SftpClient>> {
        let connector = Arc::clone(&self.connector);
        if ssh_jump_host(profile).is_some() {
            let (gateways, target_creds) = self.resolve_jump_chain(profile).await?;
            let hops: Vec<JumpHop<'_>> = gateways.iter().map(|(p, c)| JumpHop::new(p, c)).collect();
            connector
                .connect_sftp_via_jump_chain(&hops, profile, &target_creds)
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

    /// Resolve the ordered gateway chain for `target` from the profile store.
    ///
    /// Walks `jump_host` links starting at `target` (target → its gateway → that
    /// gateway's gateway → …), then returns the gateways in **connection order**
    /// (`gateway1 → gateway2 → … → target`) each paired with transient
    /// credentials, plus the target's credentials.
    ///
    /// Errors are explicit: gateway not found, gateway is not SSH, a cycle in
    /// the `jump_host` links, or a chain deeper than [`MAX_JUMP_CHAIN`].
    async fn resolve_jump_chain(
        &self,
        target: &ConnectionProfile,
    ) -> anyhow::Result<(
        Vec<(ConnectionProfile, ResolvedCredentials)>,
        ResolvedCredentials,
    )> {
        // Walk links: collected in walk order (gateway nearest the target first).
        let mut walk: Vec<ConnectionProfile> = Vec::new();
        let mut seen: HashSet<Uuid> = HashSet::new();
        seen.insert(target.id);

        let mut next = ssh_jump_host(target);
        while let Some(jid) = next {
            if walk.len() >= MAX_JUMP_CHAIN {
                anyhow::bail!(
                    "jump chain too deep for '{}': more than {MAX_JUMP_CHAIN} gateways",
                    target.name
                );
            }
            if !seen.insert(jid) {
                anyhow::bail!(
                    "jump chain cycle detected while resolving '{}' (gateway {jid} revisited)",
                    target.name
                );
            }
            let gw = self
                .profiles
                .get_profile(jid)
                .await
                .context("loading jump host profile")?
                .with_context(|| format!("jump host profile {jid} not found"))?;
            next = match &gw.settings {
                ProtocolSettings::Ssh(s) => s.jump_host,
                other => anyhow::bail!(
                    "jump host profile '{}' is not an SSH profile (it is {:?})",
                    gw.name,
                    other.kind()
                ),
            };
            walk.push(gw);
        }

        // Reverse walk order → connection order (gateway1 first), resolving creds.
        let mut gateways = Vec::with_capacity(walk.len());
        for gw in walk.into_iter().rev() {
            let creds = self.resolve_credentials(&gw).await?;
            gateways.push((gw, creds));
        }
        let target_creds = self.resolve_credentials(target).await?;
        Ok((gateways, target_creds))
    }

    /// Store the **password** secret for a profile's credential reference.
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

    /// Store the **private-key passphrase** secret for an SSH profile's
    /// `key_passphrase` credential reference (set the ref on the profile first).
    pub async fn set_profile_key_passphrase(
        &self,
        profile: &ConnectionProfile,
        secret: Secret,
    ) -> anyhow::Result<()> {
        let cref = match &profile.settings {
            ProtocolSettings::Ssh(s) => s.key_passphrase.as_ref(),
            _ => None,
        }
        .context("profile has no key-passphrase credential reference")?;
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
        async fn connect_sftp(
            &self,
            profile: &ConnectionProfile,
            _creds: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn SftpClient>> {
            self.record(format!("sftp:{}", profile.name));
            Ok(Box::new(NullSftp))
        }
        async fn connect_shell_via_jump_chain(
            &self,
            gateways: &[JumpHop<'_>],
            target: &ConnectionProfile,
            _tc: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn RemoteSession>> {
            self.record(format!(
                "shell_chain:{}->{}",
                chain_names(gateways),
                target.name
            ));
            Ok(Box::new(NullSession))
        }
        async fn connect_sftp_via_jump_chain(
            &self,
            gateways: &[JumpHop<'_>],
            target: &ConnectionProfile,
            _tc: &ResolvedCredentials,
        ) -> rrs_protocols::Result<Box<dyn SftpClient>> {
            self.record(format!(
                "sftp_chain:{}->{}",
                chain_names(gateways),
                target.name
            ));
            Ok(Box::new(NullSftp))
        }
    }

    /// Join the gateway profile names in connection order (for assertions).
    fn chain_names(gateways: &[JumpHop<'_>]) -> String {
        gateways
            .iter()
            .map(|h| h.profile.name.as_str())
            .collect::<Vec<_>>()
            .join(",")
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

    /// Build a linear chain of `n` gateways in connection order (`gw1 → … →
    /// gwN → target`) wired through `jump_host`. Returns the target plus the
    /// gateway profiles to seed the store with.
    fn linear_chain(n: usize) -> (ConnectionProfile, Vec<ConnectionProfile>) {
        let mut gateways = Vec::new();
        let mut prev_id: Option<Uuid> = None;
        for i in 1..=n {
            let mut gw = ssh(&format!("gw{i}"));
            if let Some(id) = prev_id {
                gw = with_jump(gw, id);
            }
            prev_id = Some(gw.id);
            gateways.push(gw);
        }
        let target = with_jump(ssh("target"), prev_id.expect("at least one gateway"));
        (target, gateways)
    }

    fn connect_err(core_result: anyhow::Result<(Uuid, Box<dyn RemoteSession>)>) -> String {
        core_result.map(|_| ()).unwrap_err().to_string()
    }

    #[tokio::test]
    async fn connect_without_jump_uses_direct_shell() {
        let spy = Arc::new(SpyConnector::default());
        let core = core_with(MemProfileStore::default(), spy.clone());
        core.connect(&ssh("direct")).await.unwrap();
        assert_eq!(spy.last().as_deref(), Some("shell:direct"));
    }

    #[tokio::test]
    async fn one_hop_chain_resolves_gateway() {
        let spy = Arc::new(SpyConnector::default());
        let (target, gws) = linear_chain(1);
        let core = core_with(MemProfileStore::with(gws), spy.clone());
        core.connect(&target).await.unwrap();
        assert_eq!(spy.last().as_deref(), Some("shell_chain:gw1->target"));
    }

    #[tokio::test]
    async fn two_hop_chain_is_ordered() {
        let spy = Arc::new(SpyConnector::default());
        let (target, gws) = linear_chain(2);
        let core = core_with(MemProfileStore::with(gws), spy.clone());
        core.connect(&target).await.unwrap();
        // Connection order gw1 → gw2 → target.
        assert_eq!(spy.last().as_deref(), Some("shell_chain:gw1,gw2->target"));
    }

    #[tokio::test]
    async fn three_hop_chain_is_ordered() {
        let spy = Arc::new(SpyConnector::default());
        let (target, gws) = linear_chain(3);
        let core = core_with(MemProfileStore::with(gws), spy.clone());
        core.connect(&target).await.unwrap();
        assert_eq!(
            spy.last().as_deref(),
            Some("shell_chain:gw1,gw2,gw3->target")
        );
    }

    #[tokio::test]
    async fn sftp_chain_is_ordered() {
        let spy = Arc::new(SpyConnector::default());
        let (target, gws) = linear_chain(2);
        let core = core_with(MemProfileStore::with(gws), spy.clone());
        core.connect_sftp(&target).await.unwrap();
        assert_eq!(spy.last().as_deref(), Some("sftp_chain:gw1,gw2->target"));

        // Direct SFTP still works.
        let spy2 = Arc::new(SpyConnector::default());
        let core2 = core_with(MemProfileStore::default(), spy2.clone());
        core2.connect_sftp(&ssh("direct")).await.unwrap();
        assert_eq!(spy2.last().as_deref(), Some("sftp:direct"));
    }

    #[tokio::test]
    async fn jump_profile_not_found_errors() {
        let spy = Arc::new(SpyConnector::default());
        let target = with_jump(ssh("target"), Uuid::new_v4());
        let core = core_with(MemProfileStore::default(), spy.clone());
        let err = connect_err(core.connect(&target).await);
        assert!(err.contains("not found"), "unexpected error: {err}");
        assert_eq!(spy.last(), None, "connector must not be called");
    }

    #[tokio::test]
    async fn jump_profile_not_ssh_errors() {
        let spy = Arc::new(SpyConnector::default());
        let gw = ConnectionProfile::new_local_shell("local-gw", None);
        let target = with_jump(ssh("target"), gw.id);
        let core = core_with(MemProfileStore::with(vec![gw]), spy.clone());
        let err = connect_err(core.connect(&target).await);
        assert!(
            err.contains("not an SSH profile"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn jump_chain_cycle_is_detected() {
        let spy = Arc::new(SpyConnector::default());
        // gw1.jump = gw2, gw2.jump = gw1 (a cycle), target.jump = gw1.
        let mut gw1 = ssh("gw1");
        let mut gw2 = ssh("gw2");
        gw1 = with_jump(gw1, gw2.id);
        gw2 = with_jump(gw2, gw1.id);
        let target = with_jump(ssh("target"), gw1.id);
        let core = core_with(MemProfileStore::with(vec![gw1, gw2]), spy.clone());
        let err = connect_err(core.connect(&target).await);
        assert!(err.contains("cycle"), "unexpected error: {err}");
        assert_eq!(spy.last(), None);
    }

    #[tokio::test]
    async fn jump_chain_too_deep_is_rejected() {
        let spy = Arc::new(SpyConnector::default());
        // One more gateway than MAX_JUMP_CHAIN allows.
        let (target, gws) = linear_chain(MAX_JUMP_CHAIN + 1);
        let core = core_with(MemProfileStore::with(gws), spy.clone());
        let err = connect_err(core.connect(&target).await);
        assert!(err.contains("too deep"), "unexpected error: {err}");
        assert_eq!(spy.last(), None);
    }

    #[tokio::test]
    async fn max_depth_chain_is_allowed() {
        let spy = Arc::new(SpyConnector::default());
        let (target, gws) = linear_chain(MAX_JUMP_CHAIN);
        let core = core_with(MemProfileStore::with(gws), spy.clone());
        core.connect(&target).await.unwrap();
        let recorded = spy.last().unwrap();
        assert!(recorded.starts_with("shell_chain:gw1,"), "{recorded}");
        assert!(recorded.ends_with("->target"), "{recorded}");
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

    // -----------------------------------------------------------------------
    // Credential resolution: password vs key passphrase (independent secrets)
    // -----------------------------------------------------------------------

    fn core_with_store(profiles: MemProfileStore) -> (AppCore, Arc<MemoryCredentialStore>) {
        let store = Arc::new(MemoryCredentialStore::new());
        let creds: Arc<dyn CredentialStore> = store.clone();
        let core = AppCore::new(
            AppConfig::default(),
            Arc::new(profiles),
            creds,
            Arc::new(SpyConnector::default()),
        );
        (core, store)
    }

    fn with_password_ref(mut p: ConnectionProfile, cref: CredentialRef) -> ConnectionProfile {
        p.credential = Some(cref);
        p
    }

    fn with_passphrase_ref(mut p: ConnectionProfile, cref: CredentialRef) -> ConnectionProfile {
        if let ProtocolSettings::Ssh(s) = &mut p.settings {
            s.key_passphrase = Some(cref);
        }
        p
    }

    #[tokio::test]
    async fn resolves_password_only() {
        let (core, store) = core_with_store(MemProfileStore::default());
        let pw = CredentialRef::new("pw");
        store
            .set_secret(pw.id, &Secret::new("hunter2"))
            .await
            .unwrap();
        let profile = with_password_ref(ssh("p"), pw);

        let creds = core.resolve_credentials(&profile).await.unwrap();
        assert_eq!(creds.password.as_ref().map(|s| s.expose()), Some("hunter2"));
        assert!(creds.key_passphrase.is_none());
    }

    #[tokio::test]
    async fn resolves_key_passphrase_only() {
        let (core, store) = core_with_store(MemProfileStore::default());
        let pp = CredentialRef::new("pp");
        store
            .set_secret(pp.id, &Secret::new("topsecret"))
            .await
            .unwrap();
        let profile = with_passphrase_ref(ssh("p"), pp);

        let creds = core.resolve_credentials(&profile).await.unwrap();
        assert!(creds.password.is_none());
        assert_eq!(
            creds.key_passphrase.as_ref().map(|s| s.expose()),
            Some("topsecret")
        );
    }

    #[tokio::test]
    async fn resolves_both_without_mixing() {
        let (core, store) = core_with_store(MemProfileStore::default());
        let pw = CredentialRef::new("pw");
        let pp = CredentialRef::new("pp");
        store
            .set_secret(pw.id, &Secret::new("the-password"))
            .await
            .unwrap();
        store
            .set_secret(pp.id, &Secret::new("the-passphrase"))
            .await
            .unwrap();
        let profile = with_passphrase_ref(with_password_ref(ssh("p"), pw), pp);

        let creds = core.resolve_credentials(&profile).await.unwrap();
        assert_eq!(
            creds.password.as_ref().map(|s| s.expose()),
            Some("the-password")
        );
        assert_eq!(
            creds.key_passphrase.as_ref().map(|s| s.expose()),
            Some("the-passphrase")
        );
    }

    #[tokio::test]
    async fn old_profile_without_refs_resolves_to_none() {
        let (core, _store) = core_with_store(MemProfileStore::default());
        let creds = core.resolve_credentials(&ssh("plain")).await.unwrap();
        assert!(creds.password.is_none());
        assert!(creds.key_passphrase.is_none());
    }

    #[tokio::test]
    async fn missing_password_secret_is_clean_error() {
        let (core, _store) = core_with_store(MemProfileStore::default());
        // Ref present, but nothing stored under it.
        let profile = with_password_ref(ssh("p"), CredentialRef::new("pw"));
        let err = core
            .resolve_credentials(&profile)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not in the"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn missing_key_passphrase_secret_is_clean_error() {
        let (core, _store) = core_with_store(MemProfileStore::default());
        let profile = with_passphrase_ref(ssh("p"), CredentialRef::new("pp"));
        let err = core
            .resolve_credentials(&profile)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not in the"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn jump_chain_resolves_per_hop_key_passphrase() {
        // gateway and target each have their own encrypted key (distinct
        // passphrase secrets); the chain resolution keeps them per-hop.
        let gw_pp = CredentialRef::new("gw-pp");
        let target_pp = CredentialRef::new("target-pp");

        let gw = with_passphrase_ref(ssh("gw1"), gw_pp.clone());
        let target = with_passphrase_ref(with_jump(ssh("target"), gw.id), target_pp.clone());

        let (core, store) = core_with_store(MemProfileStore::with(vec![gw]));
        store
            .set_secret(gw_pp.id, &Secret::new("gw-secret"))
            .await
            .unwrap();
        store
            .set_secret(target_pp.id, &Secret::new("target-secret"))
            .await
            .unwrap();

        let (gateways, target_creds) = core.resolve_jump_chain(&target).await.unwrap();
        assert_eq!(gateways.len(), 1);
        assert_eq!(
            gateways[0].1.key_passphrase.as_ref().map(|s| s.expose()),
            Some("gw-secret")
        );
        assert_eq!(
            target_creds.key_passphrase.as_ref().map(|s| s.expose()),
            Some("target-secret")
        );
        // Passwords stay None (no password refs set).
        assert!(gateways[0].1.password.is_none());
        assert!(target_creds.password.is_none());
    }
}
