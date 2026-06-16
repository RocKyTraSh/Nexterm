//! `rrs-tunnels`: SSH tunnel (port-forwarding) model and manager.

pub mod error;
pub mod manager;

pub use error::{Result, TunnelError};
pub use manager::{
    MockTunnelDriver, TunnelDriver, TunnelKind, TunnelManager, TunnelSpec, TunnelStatus,
};
