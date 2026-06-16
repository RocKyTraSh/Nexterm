//! Domain models: connection profiles, groups, and runtime sessions.
//!
//! Design rule: **profiles and groups never contain secrets**. A profile only
//! holds a [`CredentialRef`] pointing at an entry in the OS secret store; the
//! actual password / key material is resolved at connect time and kept
//! transient (see `rrs-credentials` and `rrs-protocols`).

mod group;
mod profile;
mod session;

pub use group::Group;
pub use profile::{
    AuthMethod, ConnectionProfile, CredentialRef, ProtocolKind, ProtocolSettings, SshSettings,
};
pub use session::{RuntimeSession, SessionState};
