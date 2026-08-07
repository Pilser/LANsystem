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

    #[derive(serde::Deserialize)]
    struct Items {
        items: serde_json::Value,
    }

    let config = db::DbConfig::from_env()?;
    let client = db::Db::connect(&config)?;
    let response: Items = client.query(request).await?;
    println!("{}", serde_json::to_string_pretty(&response.items).unwrap());
    Ok(())
}