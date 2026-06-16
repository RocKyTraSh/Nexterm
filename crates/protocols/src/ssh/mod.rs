//! SSH transport.
//!
//! - [`mock`] — an in-process fake used by tests and the CLI demo (no network).
//! - `russh_impl` (feature `ssh-russh`) — the real transport (shell, SFTP,
//!   jump-host chaining, and the reusable [`SshConnection`] primitive).

pub mod mock;

#[cfg(feature = "ssh-russh")]
pub mod russh_impl;

pub use mock::{MockConnector, MockSftp};

#[cfg(feature = "ssh-russh")]
pub use russh_impl::{DirectTcpipStream, RusshConnector, RusshSession, RusshSftp, SshConnection};
