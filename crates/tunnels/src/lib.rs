//! `rrs-tunnels`: SSH tunnel (port-forwarding) model and manager.

pub mod error;
pub mod manager;
pub mod socks5;

#[cfg(feature = "ssh-russh")]
pub mod russh_driver;

pub use error::{Result, TunnelError};
pub use manager::{
    MockTunnelDriver, TunnelDriver, TunnelKind, TunnelManager, TunnelSpec, TunnelStatus,
};

#[cfg(feature = "ssh-russh")]
pub use russh_driver::RusshTunnelDriver;
