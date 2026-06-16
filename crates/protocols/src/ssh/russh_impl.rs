//! Real SSH transport via `russh` — integration scaffold.
//!
//! Compiled only with the `ssh-russh` feature. It currently returns
//! [`ProtocolError::NotImplemented`]; the real implementation is the immediate
//! next step. The intended flow is documented below so it can be filled in
//! without redesigning anything — everything outside this file depends solely
//! on the `Connector` / `RemoteSession` traits.
//!
//! Implementation outline (verify every signature against the pinned version):
//!
//! 1. Add dependencies (pin and check the latest API):
//!        russh = "..."          # SSH client/server
//!        russh-keys = "..."     # key parsing / agent (if separate in that version)
//!        russh-sftp = "..."     # SFTP subsystem
//!
//! 2. Implement `russh::client::Handler` for a small struct. `check_server_key`
//!    is where `known_hosts` verification happens (honor
//!    `SshSettings::strict_host_key_checking`).
//!
//! 3. Connect:
//!        let config = std::sync::Arc::new(russh::client::Config::default());
//!        let mut handle = russh::client::connect(config, (host, port), handler).await?;
//!
//! 4. Authenticate in `auth_methods` order: agent -> publickey -> password ->
//!    keyboard-interactive. Pull the password / passphrase from
//!    `ResolvedCredentials` (never from the profile / config).
//!
//! 5. Jump host (`SshSettings::jump_host`): connect to the gateway first, then
//!    open a `direct-tcpip` channel to the target and run the second SSH session
//!    over that stream. The same primitive backs local/remote/dynamic
//!    forwarding in `rrs-tunnels` (implement `TunnelDriver` here too).
//!
//! 6. Shell: open a session channel, `request_pty`, `request_shell`, then bridge
//!    `ChannelMsg::Data` <-> the `RemoteSession` read/write methods.

use async_trait::async_trait;

use rrs_core::model::ConnectionProfile;

use crate::error::{ProtocolError, Result};
use crate::traits::{Connector, RemoteSession, ResolvedCredentials};

/// SSH connector backed by `russh` (work in progress).
#[derive(Default)]
pub struct RusshConnector;

#[async_trait]
impl Connector for RusshConnector {
    async fn connect_shell(
        &self,
        _profile: &ConnectionProfile,
        _creds: &ResolvedCredentials,
    ) -> Result<Box<dyn RemoteSession>> {
        Err(ProtocolError::NotImplemented(
            "russh SSH transport — see crates/protocols/src/ssh/russh_impl.rs",
        ))
    }
}
