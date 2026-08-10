//! Errors surfaced by the serverless board store.

use thiserror::Error;

/// Anything that can go wrong in a board store operation.
#[derive(Debug, Error)]
pub enum ServerlessError {
    #[error("board not found: {0}")]
    NotFound(String),

    #[error("forbidden: {0}")]
    Forbidden(String),

    #[error("invalid key: {0}")]
    BadKey(String),

    #[error("board quota exceeded: {0}")]
    Quota(String),

    #[error("invalid payload: {0}")]
    BadPayload(String),

    #[error("storage I/O error: {0}")]
    Io(String),

    #[error("database error: {0}")]
    Db(String),
}

impl From<std::io::Error> for ServerlessError {
    fn from(err: std::io::Error) -> Self {
        ServerlessError::Io(err.to_string())
    }
}