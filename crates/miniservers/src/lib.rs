//! `rrs-miniservers`: embedded-server framework plus an HTTP file server and a
//! minimal interval scheduler. Additional servers (TFTP/FTP/SSH/Telnet/NFS/VNC)
//! implement the same [`MiniServer`] trait.

pub mod error;
pub mod http;
pub mod scheduler;
pub mod service;

pub use error::{MiniServerError, Result};
pub use http::HttpFileServer;
pub use scheduler::{ScheduledTask, SchedulerServer};
pub use service::{MiniServer, MiniServerConfig, MiniServerKind, MiniServerManager, ServerState};
