use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("connection failed: {0}")]
    Connect(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("channel error: {0}")]
    Channel(String),

    #[error("agent forwarding error: {0}")]
    Agent(String),

    #[error("sftp error: {0}")]
    Sftp(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("feature not implemented yet: {0}")]
    NotImplemented(&'static str),
}

pub type Result<T> = std::result::Result<T, ProtocolError>;
