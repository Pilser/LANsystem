use db::schema::{labels, props};
use db::{g, read_batch, PropertyProjection, QueryRequest};

#[tokio::main]
async fn main() -> Result<(), db::Error> {
    let q = read_batch()
        .var_as(
            "tenants",
            g().n_with_label(labels::TENANT).project(vec![
                PropertyProjection::new(props::TENANT_ID),
                PropertyProjection::new(props::TENANT_NAME),
                PropertyProjection::new(props::STATUS),
                PropertyProjection::new(props::STORAGE_QUOTA_BYTES),
                PropertyProjection::new(props::STORAGE_USED_BYTES),
                PropertyProjection::new(props::BILLING_ENABLED),
                PropertyProjection::new(props::AUTH_WEBHOOK_URL),
                PropertyProjection::new(props::CREATED_AT),
            ]),
        )
        .var_as(
            "users",
            g().n_with_label(labels::USER).project(vec![
                PropertyProjection::new(props::USER_ID),
                PropertyProjection::new(props::USERNAME),
                PropertyProjection::new(props::TENANT_ID),
            ]),
        )
        .returning(["tenants", "users"]);
    let request = QueryRequest::read(q);
    println!("--- REQUEST ---\n{}", request.to_json_string()?);

    let config = db::DbConfig::from_env()?;
    let url = format!("{}/{}", config.endpoint, config.query_path);
    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request.to_json_bytes()?)
        .send()
        .await
        .map_err(|e| db::Error::from(e))?;
    let bytes = response.bytes().await.map_err(|e| db::Error::from(e))?;
    println!("--- RESPONSE ---\n{}", String::from_utf8_lossy(&bytes[..bytes.len().min(4000)]));
    Ok(())
}