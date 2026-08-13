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
    async fn record_patch_other(&self, _board_id: &str, _filter: &Value, patch: &Value, _writer: Option<&str>) -> anyhow::Result<Option<i64>> {
        self.sets.lock().unwrap().push((-1, patch.clone()));
        Ok(Some(42))
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
#[tokio::test]
async fn action_payload_templates_context_and_payload() {
    let db = MockDb::default();
    let recipe = parse_recipe(&json!({
        "name": "tpl",
        "when": "record.created",
        "actions": [
            {"$upsert_other": {"payload": {"order": "{{payload.id}}", "qty_num": "{{payload.qty}}", "board_ref": "{{board}}", "ev": "{{kind}}", "w": "{{writer}}"}}},
            {"$patch_other": {"filter": [{"field": "sku", "op": "eq", "value": "A1"}], "patch": {"$inc": {"stock": "{{payload.qty}}"}}}}
        ]
    })).unwrap();
    let ctx = EventContext::record_created("b1", 7, &json!({"id": 3, "qty": 2}), Some("alice"));
    let out = Engine { db: &db }.run_recipe(&recipe, &Secrets::new(), &ctx, false).await;
    assert!(out.matched, "expected recipe to match");
    assert!(out.report.logs.is_empty(), "no logs expected, got {:#?}", out.report.logs);
    let external = &out.report.external;
    assert_eq!(external.len(), 2);
    assert_eq!(external[0]["payload"]["order"], json!(3));
    assert_eq!(external[0]["payload"]["qty_num"], json!(2));
    assert_eq!(external[0]["payload"]["board_ref"], json!("b1"));
    assert_eq!(external[0]["payload"]["ev"], json!("record.created"));
    assert_eq!(external[0]["payload"]["w"], json!("alice"));
    assert_eq!(external[1]["patch"]["$inc"]["stock"], json!(2));
}

#[tokio::test]
async fn unknown_template_token_skips_action_with_log() {
    let db = MockDb::default();
    let recipe = parse_recipe(&json!({
        "name": "bad-tpl",
        "when": "inbound",
        "actions": [{"$upsert_other": {"payload": {"x": "{{payload.nope.missing}}"}}}]
    })).unwrap();
    let ctx = EventContext::inbound("b1", &json!({"a": 1}));
    let out = Engine { db: &db }.run_recipe(&recipe, &Secrets::new(), &ctx, false).await;
    assert!(out.matched);
    assert!(out.report.logs.iter().any(|l| l.contains("unknown template token") && l.contains("action skipped")), "logs: {:#?}", out.report.logs);
    assert!(out.report.external.is_empty(), "action must not run on bad template");
}

#[tokio::test]
async fn secrets_and_context_substitute_together() {
    let mut secrets = Secrets::new();
    secrets.insert("API", "sk-1");
    let db = MockDb::default();
    let recipe = parse_recipe(&json!({
        "name": "sec-tpl",
        "when": "record.created",
        "actions": [{"$call": {"url": "https://x.example/h/{{payload.id}}", "headers": {"auth": "Bearer {{API}}"}, "body": {"amt": "{{payload.total}}"}}}]
    })).unwrap();
    let ctx = EventContext::record_created("b1", 1, &json!({"id": 9, "total": 12.5}), None);
    let out = Engine { db: &db }.run_recipe(&recipe, &secrets, &ctx, false).await;
    let ext = &out.report.external[0];
    assert_eq!(ext["url"], json!("https://x.example/h/9"));
    assert_eq!(ext["headers"]["auth"], json!("Bearer sk-1"));
    assert_eq!(ext["body"]["amt"], json!(12.5));
}
