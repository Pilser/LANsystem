use std::time::Duration;

use db::{Db, DbConfig};
use platform_auth::queries;
use storage::{Storage, StorageConfig};

use crate::services::Services;

const BUCKET_RETRIES: usize = 3;
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Opens the shared database and storage clients from environment config,
/// then boots the identity service (indexes + seed owner account).
///
/// Bucket creation is best-effort: it is retried a bounded number of times and
/// then degrades to a logged warning so a read-only deployment can still boot.
pub async fn connect() -> crate::Result<Services> {
    let db_config = DbConfig::from_env()?;
    let storage_config = StorageConfig::from_env()?;

    let db = Db::connect(&db_config)?;
    let storage = Storage::connect(storage_config).await?;

    let auth = platform_auth::Auth::new(db.clone());
    ensure_auth_indexes(&db).await;
    auth.seed_owner()
        .await
        .map_err(|error| crate::Error::Service(error.to_string()))?;

    let services = Services { db, storage, auth };
    ensure_bucket(&services).await;
    Ok(services)
}

async fn ensure_auth_indexes(db: &Db) {
    match queries::ensure_auth_indexes() {
        Ok(request) => {
            if let Err(error) = db.query::<serde_json::Value>(request).await {
                tracing::warn!(
                    error = %error,
                    "auth: could not ensure indexes; queries may fall back to scans",
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "auth: could not build index query; queries may fall back to scans",
            );
        }
    }
}

async fn ensure_bucket(services: &Services) {
    let mut attempts = 0usize;
    loop {
        match services.storage.ensure_bucket().await {
            Ok(()) => return,
            Err(storage_err) => {
                attempts += 1;
                if attempts >= BUCKET_RETRIES {
                    tracing::warn!(
                        error = %storage_err,
                        "could not ensure storage bucket; continuing without it",
                    );
                    return;
                }
                tokio::time::sleep(RETRY_DELAY).await;
            }
        }
    }
}