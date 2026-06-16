use thiserror::Error;

#[derive(Debug, Error)]
pub enum MiniServerError {
    #[error("server not found: {0}")]
    NotFound(uuid::Uuid),

    #[error("failed to start server: {0}")]
    Start(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, MiniServerError>;
