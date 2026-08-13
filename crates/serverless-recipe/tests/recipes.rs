use serde_json::{json, Value};
use serverless_recipe::{Engine, EventContext, RecipeDb, Secrets, parse_recipe};
use serverless_recipe::actions::match_public;

fn recipe_json() -> serde_json::Value {
    json!({
        "name": "paid-invoice",
        "when": "record.created",
        "match": {"$.status": {"eq": "paid"}},
        "actions": [
            {"$compute": {"total": "$.price * $.qty"}},
            {"$compute": {"label": "concat(upper($.customer), '-', $.id)"}},
            {"$push": {"path": "$.tags", "value": "auto"}}
        ]
    })
}

#[test]
fn parse_and_kind() {
    let r = parse_recipe(&recipe_json()).unwrap();
    assert_eq!(r.name, "paid-invoice");
    assert_eq!(r.event_kind(), Some(serverless_recipe::EventKind::Created));
}

#[test]
fn match_filter_combos() {
    let p = json!({"status": "paid", "qty": 5, "tags": ["a"], "sku": 10});
    assert!(match_public(&p, &json!({"$.status": {"eq": "paid"}})).unwrap());
    assert!(match_public(&p, &json!({"$.qty": {"gte": 3}, "$.tags": {"contains": "a"}})).unwrap());
    assert!(match_public(&p, &json!({"$.sku": {"in": [1, 10, 20]}})).unwrap());
    assert!(!match_public(&p, &json!({"$.qty": {"lt": 1}})).unwrap());
}

#[test]
fn secrets_do_not_leak_in_report() {
    let mut s = Secrets::new();
    s.insert("KEY", "sekret");
    assert!(!serde_json::to_string(&s.masked()).unwrap().contains("sekret"));
    assert_eq!(s.substitute("Bearer ${KEY}"), "Bearer sekret");
}

/// In-memory `RecipeDb` for engine tests (dry-run + persist path).
#[derive(Default)]
struct MockDb {
    sets: std::sync::Mutex<Vec<(i64, Value)>>,
}

#[async_trait::async_trait]
impl RecipeDb for MockDb {
    async fn board_owner(&self, _board_id: &str) -> anyhow::Result<String> {
        Ok("owner".to_string())
    }
    async fn record_insert(&self, _board_id: &str, _payload: &Value, _writer: Option<&str>, _upsert: bool) -> anyhow::Result<i64> {
        Ok(99)
    }
    async fn record_set(&self, _board_id: &str, seq: i64, payload: &Value, _writer: Option<&str>) -> anyhow::Result<()> {
        self.sets.lock().unwrap().push((seq, payload.clone()));
        Ok(())
    }
    async fn http_call(&self, _board_id: &str, _owner: &str, _method: &str, _url: &str, _headers: &[(String, String)], _body: Option<&str>, _timeout_ms: u64) -> anyhow::Result<serverless_recipe::engine::CallOutcome> {
        Ok(serverless_recipe::engine::CallOutcome { status: 200, duration_ms: 1 })
    }
    async fn job_add(&self, _board_id: &str, _name: &str, _schedule: &str, _action: &Value) -> anyhow::Result<i64> {
        Ok(1)
    }
    async fn recipe_ran(&self, _board_id: &str, _recipe: &str, _key: &Value) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn recipe_mark_ran(&self, _board_id: &str, _recipe: &str, _key: &Value) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn engine_dry_run_applies_compute() {
    let db = MockDb::default();
    let recipe = parse_recipe(&recipe_json()).unwrap();
    let ctx = EventContext::record_created("b", 1, &json!({"status": "paid", "price": 10, "qty": 3, "customer": "acme", "id": 7}), None);
    let engine = Engine { db: &db };
    let outcomes = engine.run_recipes(&[recipe], &Secrets::new(), &ctx, true).await;
    assert_eq!(outcomes.len(), 1);
    let (name, out) = &outcomes[0];
    assert_eq!(name, "paid-invoice");
    assert!(out.matched);
    assert!(out.report.actions_run.iter().any(|a| a.contains("Compute")));
    assert!(db.sets.lock().unwrap().is_empty(), "dry-run must not persist");
}

#[tokio::test]
async fn engine_persists_changes() {
    let db = MockDb::default();
    let recipe = parse_recipe(&recipe_json()).unwrap();
    let ctx = EventContext::record_created("b", 7, &json!({"status": "paid", "price": 10, "qty": 3, "customer": "acme", "id": 7}), None);
    let engine = Engine { db: &db };
    let outcomes = engine.run_recipes(&[recipe], &Secrets::new(), &ctx, false).await;
    let (_, out) = &outcomes[0];
    assert!(out.matched);
    let sets = db.sets.lock().unwrap();
    assert_eq!(sets.len(), 1);
    let (seq, payload) = &sets[0];
    assert_eq!(*seq, 7);
    assert_eq!(payload["total"], json!(30.0));
    assert_eq!(payload["label"], json!("ACME-7"));
    assert_eq!(payload["tags"], json!(["auto"]));
}