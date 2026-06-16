//! `rrs-protocols`: protocol-agnostic connection and SFTP abstractions plus
//! concrete (and scaffolded) transports.

pub mod error;
pub mod ssh;
pub mod traits;

#[cfg(feature = "local-pty")]
pub mod local;

pub use error::{ProtocolError, Result};
pub use traits::{Connector, DirEntry, EntryKind, RemoteSession, ResolvedCredentials, SftpClient};

#[cfg(feature = "local-pty")]
pub use local::{LocalPtySession, LocalShellConnector};

#[cfg(feature = "ssh-russh")]
pub use ssh::{DirectTcpipStream, RusshConnector, RusshSftp, SshConnection};
