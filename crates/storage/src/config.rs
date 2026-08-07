use std::env;

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub endpoint: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
    pub path_style: bool,
}

impl StorageConfig {
    pub fn from_env() -> Result<Self> {
        let var = |name: &str| -> Result<String> {
            env::var(name)
                .map_err(|_| Error::Config(format!("missing required environment variable {name}")))
        };
        let path_style = env::var("RUSTFS_PATH_STYLE")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(true);
        Ok(Self {
            endpoint: var("RUSTFS_ENDPOINT")?,
            region: var("RUSTFS_REGION")?,
            access_key: var("RUSTFS_ACCESS_KEY")?,
            secret_key: var("RUSTFS_SECRET_KEY")?,
            bucket: var("RUSTFS_BUCKET")?,
            path_style,
        })
    }

    pub fn basic(
        endpoint: impl Into<String>,
        region: impl Into<String>,
        access_key: impl Into<String>,
        secret_key: impl Into<String>,
        bucket: impl Into<String>,
        path_style: bool,
    ) -> Result<Self> {
        let config = Self {
            endpoint: endpoint.into(),
            region: region.into(),
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            bucket: bucket.into(),
            path_style,
        };
        if config.endpoint.is_empty()
            || config.region.is_empty()
            || config.access_key.is_empty()
            || config.secret_key.is_empty()
            || config.bucket.is_empty()
        {
            return Err(Error::Config(
                "endpoint, region, access_key, secret_key, and bucket must all be non-empty"
                    .to_string(),
            ));
        }
        Ok(config)
    }
}
