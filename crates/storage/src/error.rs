#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),
    #[error("s3 error: {0}")]
    S3(#[from] aws_sdk_s3::Error),
    #[error("presign error: {0}")]
    Presign(String),
    #[error("other: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn s3_error(e: impl Into<aws_sdk_s3::Error>) -> Error {
    Error::S3(e.into())
}
