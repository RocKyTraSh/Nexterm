//! `rrs-core`: domain models, configuration, events, session registry, and the
//! profile-storage abstraction. UI- and protocol-agnostic.

pub mod config;
pub mod error;
pub mod event;
pub mod model;
pub mod registry;
pub mod store;

pub use error::{CoreError, Result};
