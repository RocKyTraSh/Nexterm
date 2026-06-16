use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A pointer to secret material stored in the OS secret service.
///
/// This struct is safe to serialize into the profile database: it contains no
/// secret, only a stable id (the key in the secret store) and a human label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialRef {
    /// Stable identifier; used as the account/key in the secret store.
    pub id: Uuid,
    /// Human-friendly, non-secret label (e.g. "prod-root").
    #[serde(default)]
    pub label: String,
}

impl CredentialRef {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            label: label.into(),
        }
    }
}

/// Supported / planned protocols. The enum is additive: new protocols are new
/// variants and do not break previously serialized data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    Ssh,
    Telnet,
    Rlogin,
    Rdp,
    Vnc,
    Ftp,
    Sftp,
    Serial,
    LocalShell,
}

/// SSH authentication methods, in the order they should be attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    PublicKey,
    Agent,
    KeyboardInteractive,
}

/// SSH-specific connection settings (no secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshSettings {
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    /// Path to a private key file on disk.
    #[serde(default)]
    pub private_key_path: Option<String>,
    /// Reference to the **private key's passphrase** secret, if the key is
    /// encrypted. A pointer into the secret store — never the passphrase itself.
    ///
    /// This is independent from the profile's password credential
    /// ([`ConnectionProfile::credential`]): a password and a key passphrase are
    /// different secrets and must never be confused. The password ref currently
    /// lives at the profile level; renaming/moving it is deferred to avoid churn
    /// (TODO: a clearer per-purpose credential layout once a GUI exists).
    #[serde(default)]
    pub key_passphrase: Option<CredentialRef>,
    /// Optional jump host (gateway). Points at another profile's id, enabling
    /// `ProxyJump`-style chaining. Chains of length > 1 are a roadmap item.
    #[serde(default)]
    pub jump_host: Option<Uuid>,
    /// Verify the server key against `known_hosts` (recommended).
    #[serde(default = "default_true")]
    pub strict_host_key_checking: bool,
    /// Forward the local SSH agent to the remote.
    #[serde(default)]
    pub agent_forwarding: bool,
}

impl Default for SshSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            username: String::new(),
            auth_methods: vec![
                AuthMethod::Agent,
                AuthMethod::PublicKey,
                AuthMethod::Password,
            ],
            private_key_path: None,
            key_passphrase: None,
            jump_host: None,
            strict_host_key_checking: true,
            agent_forwarding: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Protocol-specific settings. Tagged by `kind` for forward-compatible storage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolSettings {
    Ssh(SshSettings),
    /// A local shell session (no network).
    LocalShell {
        /// Program to launch; `None` means the user's default shell.
        #[serde(default)]
        program: Option<String>,
    },
    // Telnet/Rlogin/Rdp/Vnc/Ftp/Sftp/Serial are added here as they are
    // implemented. Because this is an externally-tagged enum keyed on `kind`,
    // adding variants does not invalidate existing stored profiles.
}

impl ProtocolSettings {
    /// The protocol family for this settings value.
    pub fn kind(&self) -> ProtocolKind {
        match self {
            ProtocolSettings::Ssh(_) => ProtocolKind::Ssh,
            ProtocolSettings::LocalShell { .. } => ProtocolKind::LocalShell,
        }
    }
}

/// A saved connection definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionProfile {
    pub id: Uuid,
    pub name: String,
    /// Group this profile belongs to (folder in the session tree).
    #[serde(default)]
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub settings: ProtocolSettings,
    /// Reference to secret material. Never contains the secret itself.
    #[serde(default)]
    pub credential: Option<CredentialRef>,
    #[serde(default)]
    pub description: String,
}

impl ConnectionProfile {
    /// Create a new SSH profile with sensible defaults.
    pub fn new_ssh(
        name: impl Into<String>,
        host: impl Into<String>,
        username: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            group_id: None,
            tags: Vec::new(),
            settings: ProtocolSettings::Ssh(SshSettings {
                host: host.into(),
                username: username.into(),
                ..SshSettings::default()
            }),
            credential: None,
            description: String::new(),
        }
    }

    /// Create a new local-shell profile. `program` of `None` launches the
    /// user's default shell.
    pub fn new_local_shell(name: impl Into<String>, program: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            group_id: None,
            tags: Vec::new(),
            settings: ProtocolSettings::LocalShell { program },
            credential: None,
            description: String::new(),
        }
    }

    pub fn kind(&self) -> ProtocolKind {
        self.settings.kind()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_forwarding_defaults_off() {
        // Safe default: agent forwarding is opt-in.
        assert!(!SshSettings::default().agent_forwarding);
        let profile = ConnectionProfile::new_ssh("h", "host", "user");
        if let ProtocolSettings::Ssh(s) = &profile.settings {
            assert!(!s.agent_forwarding);
        } else {
            panic!("expected SSH settings");
        }
    }

    #[test]
    fn old_profile_without_agent_forwarding_deserializes_to_false() {
        // An SshSettings JSON predating the field must still load (serde default).
        let json = r#"{"host":"h","port":22,"username":"u"}"#;
        let s: SshSettings = serde_json::from_str(json).expect("deserialize legacy settings");
        assert!(!s.agent_forwarding);
        assert!(s.strict_host_key_checking, "strict default preserved");
    }

    #[test]
    fn agent_forwarding_roundtrips_when_enabled() {
        let mut s = SshSettings {
            host: "h".into(),
            username: "u".into(),
            ..SshSettings::default()
        };
        s.agent_forwarding = true;
        let json = serde_json::to_string(&s).unwrap();
        let back: SshSettings = serde_json::from_str(&json).unwrap();
        assert!(back.agent_forwarding);
    }

    #[test]
    fn key_passphrase_defaults_to_none() {
        assert!(SshSettings::default().key_passphrase.is_none());
        let profile = ConnectionProfile::new_ssh("h", "host", "user");
        if let ProtocolSettings::Ssh(s) = &profile.settings {
            assert!(s.key_passphrase.is_none());
        } else {
            panic!("expected SSH settings");
        }
    }

    #[test]
    fn old_profile_without_key_passphrase_deserializes() {
        // A pre-existing SshSettings JSON (with a key path but no passphrase
        // field) must still load — the field is `#[serde(default)]`.
        let json = r#"{"host":"h","port":22,"username":"u","private_key_path":"/k"}"#;
        let s: SshSettings = serde_json::from_str(json).expect("deserialize legacy settings");
        assert!(s.key_passphrase.is_none());
        assert_eq!(s.private_key_path.as_deref(), Some("/k"));
    }

    #[test]
    fn key_passphrase_ref_roundtrips_and_coexists_with_password() {
        // The key-passphrase ref (in SshSettings) and the password ref (on the
        // profile) are independent and can both be set.
        let mut profile = ConnectionProfile::new_ssh("h", "host", "user");
        profile.credential = Some(CredentialRef::new("password"));
        let pp_ref = CredentialRef::new("key-passphrase");
        if let ProtocolSettings::Ssh(s) = &mut profile.settings {
            s.key_passphrase = Some(pp_ref.clone());
        }

        let json = serde_json::to_string(&profile).unwrap();
        // Sanity: the JSON carries only refs (ids/labels), never a secret value.
        assert!(json.contains("key-passphrase"));
        assert!(json.contains("password"));

        let back: ConnectionProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.credential.unwrap().label, "password");
        if let ProtocolSettings::Ssh(s) = &back.settings {
            assert_eq!(s.key_passphrase.as_ref().unwrap().id, pp_ref.id);
        } else {
            panic!("expected SSH settings");
        }
    }
}
