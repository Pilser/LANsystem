use db::schema::{labels, props};
use db::{g, read_batch, Error, Order, Predicate, PropertyProjection, QueryRequest};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let q = read_batch()
        .var_as(
            "items",
            g().n_with_label(labels::MEDIA_ITEM)
                .where_(Predicate::is_null(props::DELETED_AT))
                .order_by(props::RELEASE_YEAR, Order::Desc)
                .limit(100usize)
                .project(vec![
                    PropertyProjection::new(props::MEDIA_ITEM_ID),
                    PropertyProjection::new(props::TITLE),
                    PropertyProjection::new(props::RELEASE_YEAR),
                    PropertyProjection::new(props::DURATION_SEC),
                    PropertyProjection::new(props::STREAM_KEY),
                ]),
        )
        .returning(["items"]);
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
    let status = response.status();
    println!("--- HEADERS ---");
    for (k, v) in response.headers() {
        println!("{k}: {v:?}");
    }
    let bytes = response.bytes().await.map_err(|e| db::Error::from(e))?;
    println!("--- RESPONSE ({status}) ---");
    println!("{}", String::from_utf8_lossy(&bytes[..bytes.len().min(600)]));
    Ok(())
}