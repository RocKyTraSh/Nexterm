use thiserror::Error;

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("secret store backend error: {0}")]
    Backend(String),

    #[error("background task join error: {0}")]
    Join(String),
}

pub type Result<T> = std::result::Result<T, CredentialError>;
