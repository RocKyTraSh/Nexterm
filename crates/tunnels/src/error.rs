use thiserror::Error;

#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("tunnel not found: {0}")]
    NotFound(uuid::Uuid),

    #[error("tunnel already running: {0}")]
    AlreadyRunning(uuid::Uuid),

    #[error("driver error: {0}")]
    Driver(String),

    #[error("(de)serialization error: {0}")]
    Serde(String),
}

impl From<serde_json::Error> for TunnelError {
    fn from(e: serde_json::Error) -> Self {
        TunnelError::Serde(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, TunnelError>;
