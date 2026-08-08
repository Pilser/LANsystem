use db::schema::{labels, props};
use db::{g, read_batch, Db, DbConfig, Order, Predicate, PropertyProjection, QueryRequest};

async fn run(db: &Db, label: &str, q: db::ReadBatch) -> Result<(), db::Error> {
    let request = QueryRequest::read(q);
    let raw: serde_json::Value = db.query(request).await?;
    let n = raw
        .as_object()
        .and_then(|o| o.get("items"))
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    println!("{label}: {n} items");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), db::Error> {
    let config = DbConfig::from_env()?;
    let db = Db::connect(&config)?;

    run(
        &db,
        "plain label",
        read_batch().var_as("items", g().n_with_label(labels::MEDIA_ITEM)).returning(["items"]),
    )
    .await?;

    run(
        &db,
        "is_null DELETED_AT only",
        read_batch()
            .var_as(
                "items",
                g().n_with_label(labels::MEDIA_ITEM)
                    .where_(Predicate::is_null(props::DELETED_AT)),
            )
            .returning(["items"]),
    )
    .await?;

    run(
        &db,
        "is_null + order + limit + project (mirror list_library)",
        read_batch()
            .var_as(
                "items",
                g().n_with_label(labels::MEDIA_ITEM)
                    .where_(Predicate::is_null(props::DELETED_AT))
                    .order_by(props::RELEASE_YEAR, Order::Desc)
                    .limit(100usize)
                    .project(vec![PropertyProjection::new(props::TITLE)]),
            )
            .returning(["items"]),
    )
    .await?;

    run(
        &db,
        "eq STATUS active only",
        read_batch()
            .var_as(
                "items",
                g().n_with_label(labels::MEDIA_ITEM)
                    .where_(Predicate::eq(props::STATUS, "active")),
            )
            .returning(["items"]),
    )
    .await?;

    run(
        &db,
        "mirror list_library EXACT (incl. IS_PUBLIC projection)",
        read_batch()
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
                        PropertyProjection::new(props::TENANT_ID),
                        PropertyProjection::new(props::IS_PUBLIC),
                    ]),
            )
            .returning(["items"]),
    )
    .await?;
    Ok(())
}