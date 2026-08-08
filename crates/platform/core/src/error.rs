use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),
    #[error("db: {0}")]
    Db(#[from] db::Error),
    #[error("storage: {0}")]
    Storage(#[from] storage::Error),
    #[error("service: {0}")]
    Service(String),
}

pub type Result<T> = std::result::Result<T, Error>;