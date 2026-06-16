//! Error types shared across the core domain.

use thiserror::Error;

/// Errors produced by core models, config, and storage.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("(de)serialization error: {0}")]
    Serde(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("storage error: {0}")]
    Store(String),

    #[error("entity not found: {0}")]
    NotFound(String),
}

// We map (de)serialization errors to a string so secret-adjacent details never
// leak through error chains, and so callers depend on one stable variant.
impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Serde(e.to_string())
    }
}
impl From<toml::de::Error> for CoreError {
    fn from(e: toml::de::Error) -> Self {
        CoreError::Serde(e.to_string())
    }
}
impl From<toml::ser::Error> for CoreError {
    fn from(e: toml::ser::Error) -> Self {
        CoreError::Serde(e.to_string())
    }
}

/// Convenience alias for core results.
pub type Result<T> = std::result::Result<T, CoreError>;
