use thiserror::Error;

#[derive(Debug, Error)]
pub enum TerminalError {
    #[error("pty error: {0}")]
    Pty(String),

    #[error("invalid regex in highlight rule '{name}': {source}")]
    BadRegex { name: String, source: regex::Error },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, TerminalError>;
