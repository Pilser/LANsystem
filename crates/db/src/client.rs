use crate::error::Result;use helix_db::QueryRequest;

/// Connection settings for a Helix instance.
#[derive(Clone)]
pub struct DbConfig {
    pub endpoint: String,
    /// Gateway query route; the SDK pins a default but the
    /// enterprise-dev image serves its query route at `v1/query`.
    pub query_path: String,
    pub api_key: Option<String>,
}

impl DbConfig {
    /// Load config from `HELIX_ENDPOINT` / `HELIX_QUERY_PATH` / `HELIX_API_KEY` env vars.
    ///
    /// `HELIX_ENDPOINT` defaults to `http://localhost:6969` when unset.
    /// `HELIX_QUERY_PATH` lets a deployment redirect the query route (e.g.
    /// `v1/query` for the enterprise-dev image, `v2/query` for the standalone
    /// image or Helix Cloud).
    pub fn from_env() -> Result<Self> {
        let endpoint = std::env::var("HELIX_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:6969".to_string());
        let query_path =
            std::env::var("HELIX_QUERY_PATH").unwrap_or_else(|_| "v1/query".to_string());
        let api_key = std::env::var("HELIX_API_KEY").ok();
        Ok(Self {
            endpoint,
            query_path,
            api_key,
        })
    }

    /// Build a config for explicit values (used by tests and tools).
    pub fn basic(
        endpoint: impl Into<String>,
        query_path: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            query_path: query_path.into(),
            api_key,
        }
    }
}

/// Thin client wrapper over the Helix gateway.
///
/// Posts dynamic [`QueryRequest`]s serially over HTTP for embedding-deployed
/// instances (in-memory/embedded execution lives in the `db` layer of the
/// Helix SDK and is not wired here).
#[derive(Clone)]
pub struct Db {
    config: DbConfig,
    http: reqwest::Client,
}

impl Db {
    /// Build a client from config. The request body and route resolution happen
    /// per-request, so no connection is established eagerly.
    pub fn connect(config: &DbConfig) -> Result<Self> {
        let http = reqwest::Client::builder().build().map_err(crate::Error::Http)?;
        Ok(Self {
            config: DbConfig {
                endpoint: config.endpoint.clone(),
                query_path: config.query_path.clone(),
                api_key: config.api_key.clone(),
            },
            http,
        })
    }

    /// Run a query into a typed response.
    ///
    /// `request` is usually built by a `#[query]`-annotated function. The
    /// response JSON body is deserialized into `R`.
    pub async fn query<R>(&self, request: QueryRequest) -> Result<R>
    where
        R: for<'de2> serde::Deserialize<'de2>,
    {
        let url = format!("{}/{}", self.config.endpoint, self.config.query_path);
        let body = request.to_json_bytes()?;

        let mut builder = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);

        if let Some(api_key) = &self.config.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let response = builder.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(crate::Error::server(status, &bytes));
        }

        serde_json::from_slice(&bytes).map_err(crate::Error::from)
    }
}