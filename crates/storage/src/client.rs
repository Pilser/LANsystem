use std::time::Duration;

use aws_sdk_s3::config::{Credentials, Region};
use aws_sdk_s3::operation::create_bucket::CreateBucketError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use bytes::Bytes;

use crate::{Error, Result, StorageConfig};

#[derive(Clone)]
pub struct Storage {
    inner: aws_sdk_s3::Client,
    config: StorageConfig,
}

pub struct PutResult {
    pub e_tag: Option<String>,
    pub key: String,
}

pub struct GetResult {
    pub key: String,
    pub etag: String,
    pub data: Bytes,
}

impl Storage {
    pub async fn connect(config: StorageConfig) -> Result<Self> {
        let credentials = Credentials::new(
            config.access_key.clone(),
            config.secret_key.clone(),
            None,
            None,
            "rustfs",
        );
        let conf = aws_sdk_s3::config::Builder::new()
            .credentials_provider(credentials)
            .region(Region::new(config.region.clone()))
            .endpoint_url(config.endpoint.clone())
            .force_path_style(config.path_style)
            .behavior_version_latest()
            .build();
        let inner = aws_sdk_s3::Client::from_conf(conf);
        Ok(Self { inner, config })
    }

    pub async fn ensure_bucket(&self) -> Result<()> {
        match self
            .inner
            .create_bucket()
            .bucket(&self.config.bucket)
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => match e.as_service_error() {
                Some(CreateBucketError::BucketAlreadyOwnedByYou(_))
                | Some(CreateBucketError::BucketAlreadyExists(_)) => Ok(()),
                _ => Err(crate::error::s3_error(e)),
            },
        }
    }

    pub async fn put(&self, key: &str, body: Bytes, content_type: &str) -> Result<PutResult> {
        let resp = self
            .inner
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .body(ByteStream::from(body))
            .content_type(content_type)
            .send()
            .await
            .map_err(crate::error::s3_error)?;
        Ok(PutResult {
            e_tag: resp.e_tag().map(str::to_owned),
            key: key.to_string(),
        })
    }

    pub async fn get(&self, key: &str) -> Result<GetResult> {
        let resp = self
            .inner
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(crate::error::s3_error)?;
        let etag = resp.e_tag().map(str::to_owned).unwrap_or_default();
        let data = resp
            .body
            .collect()
            .await
            .map_err(|e| Error::Other(e.to_string()))?
            .into_bytes();
        Ok(GetResult {
            key: key.to_string(),
            etag,
            data,
        })
    }

    pub async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>> {
        match self
            .inner
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(resp) => {
                let data = resp
                    .body
                    .collect()
                    .await
                    .map_err(|e| Error::Other(e.to_string()))?
                    .into_bytes();
                Ok(Some(data))
            }
            Err(e) if e.as_service_error().is_some_and(is_no_such_key) => Ok(None),
            Err(e) => Err(crate::error::s3_error(e)),
        }
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.inner
            .delete_object()
            .bucket(&self.config.bucket)
            .key(key)
            .send()
            .await
            .map_err(crate::error::s3_error)?;
        Ok(())
    }

    pub async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let mut keys = Vec::new();
        let mut continuation_token: Option<String> = None;
        loop {
            let mut request = self
                .inner
                .list_objects_v2()
                .bucket(&self.config.bucket)
                .prefix(prefix);
            if let Some(token) = continuation_token {
                request = request.continuation_token(token);
            }
            let resp = request.send().await.map_err(crate::error::s3_error)?;
            for object in resp.contents() {
                if let Some(key) = object.key() {
                    keys.push(key.to_owned());
                }
            }
            if resp.is_truncated() == Some(true) {
                continuation_token = resp.next_continuation_token().map(str::to_owned);
                if continuation_token.is_none() {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(keys)
    }

    pub async fn presign_get(&self, key: &str, expires_secs: u64) -> Result<String> {
        let config = PresigningConfig::expires_in(Duration::from_secs(expires_secs))
            .map_err(|e| Error::Presign(e.to_string()))?;
        let request = self
            .inner
            .get_object()
            .bucket(&self.config.bucket)
            .key(key)
            .presigned(config)
            .await
            .map_err(|e| Error::Presign(e.to_string()))?;
        Ok(request.uri().to_string())
    }

    pub async fn presign_put(
        &self,
        key: &str,
        content_type: &str,
        expires_secs: u64,
    ) -> Result<String> {
        let config = PresigningConfig::expires_in(Duration::from_secs(expires_secs))
            .map_err(|e| Error::Presign(e.to_string()))?;
        let request = self
            .inner
            .put_object()
            .bucket(&self.config.bucket)
            .key(key)
            .content_type(content_type)
            .presigned(config)
            .await
            .map_err(|e| Error::Presign(e.to_string()))?;
        Ok(request.uri().to_string())
    }
}

fn is_no_such_key(err: &GetObjectError) -> bool {
    matches!(err, GetObjectError::NoSuchKey(_))
}
