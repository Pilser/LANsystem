use reqwest::StatusCode;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("helix client error: {0}")]
    Client(#[from] helix_db::HelixError),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("http transport error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("query request serialization failed: {0}")]
    Query(#[from] helix_db::QueryError),
    #[error("helix gateway returned {status}: {body}")]
    Server { status: StatusCode, body: String },
}

impl Error {
    pub(crate) fn server(status: StatusCode, bytes: &[u8]) -> Self {
        let body = String::from_utf8_lossy(bytes).into_owned();
        Self::Server { status, body }
    }
}

pub type Result<T> = std::result::Result<T, Error>;