//! Pure computed / validate / redact functions — the `serverless-expr` port
//! of zerowrapper's `zw-db::logic`. No SQL, no state; the store calls these
//! on write (`prepare_payload`) and on public reads (`redact`).

use serde_json::{Map, Value};
use serverless_expr::ast::PathSeg;
use serverless_expr::path::parse_path;

/// Apply `computed` fields (map of field -> expr) to a payload, in place.
/// Missing inputs resolve to Null; computed fields may reference earlier ones.
pub(crate) fn apply_computed(computed: &Value, payload: &mut Value) -> Result<(), String> {
    let Some(map) = computed.as_object() else {
        return Err("computed must be an object of {field: expr}".to_string());
    };
    for (field, expr) in map {
        let expr = expr.as_str().ok_or_else(|| format!("computed field '{field}' must be a string expression"))?;
        let e = serverless_expr::parse(expr).map_err(|e| format!("computed field '{field}': {e}"))?;
        let v = serverless_expr::apply(&e, payload).map_err(|e| format!("computed field '{field}': {e}"))?;
        set_nested(payload, field, v)?;
    }
    Ok(())
}

/// Run `validate` rules (array of {when, error}) against a payload.
/// Returns the first failing message, or None if all pass.
pub(crate) fn validate_rules(rules: &Value, payload: &Value) -> Result<Option<String>, String> {
    let Some(arr) = rules.as_array() else {
        return Err("validate must be an array of {when, error}".to_string());
    };
    for rule in arr {
        let when = rule
            .get("when")
            .and_then(|w| w.as_str())
            .ok_or_else(|| "validate rule needs a \"when\" expression".to_string())?;
        let msg = rule
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("validation failed")
            .to_string();
        if !serverless_expr::truthy(when, payload).map_err(|e| format!("validate 'when': {e}"))? {
            continue;
        }
        return Ok(Some(msg));
    }
    Ok(None)
}

/// Redact `redact` paths (array of json paths) in place by replacing with "***".
pub(crate) fn apply_redact(redact: &Value, payload: &mut Value) {
    let Some(paths) = redact.as_array() else {
        return;
    };
    for p in paths {
        if let Some(path) = p.as_str() {
            let segs = match parse_path(path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if segs.is_empty() {
                *payload = Value::String("***".to_string());
                continue;
            }
            set_last(payload, &segs, Value::String("***".to_string()));
        }
    }
}

fn set_nested(payload: &mut Value, field: &str, value: Value) -> Result<(), String> {
    let path = if field.starts_with('$') { field.to_string() } else { format!("$.{field}") };
    let segs = parse_path(&path).map_err(|e| e.to_string())?;
    if segs.is_empty() {
        *payload = value;
        return Ok(());
    }
    set_last(payload, &segs, value);
    Ok(())
}

fn set_last(payload: &mut Value, segs: &[PathSeg], value: Value) {
    if segs.is_empty() {
        return;
    }
    let mut cur = payload;
    for seg in &segs[..segs.len() - 1] {
        match seg {
            PathSeg::Key(k) => {
                if !cur.is_object() {
                    *cur = Value::Object(Map::new());
                }
                let obj = cur.as_object_mut().unwrap();
                cur = obj.entry(k.clone()).or_insert(Value::Object(Map::new()));
            }
            PathSeg::Index(i) => {
                if !cur.is_array() {
                    *cur = Value::Array(Vec::new());
                }
                let arr = cur.as_array_mut().unwrap();
                if *i >= arr.len() {
                    arr.resize(*i + 1, Value::Null);
                }
                cur = &mut arr[*i];
            }
            PathSeg::Wild => return,
        }
    }
    match &segs[segs.len() - 1] {
        PathSeg::Key(k) => {
            if !cur.is_object() {
                *cur = Value::Object(Map::new());
            }
            cur.as_object_mut().unwrap().insert(k.clone(), value);
        }
        PathSeg::Index(i) => {
            if !cur.is_array() {
                *cur = Value::Array(Vec::new());
            }
            let arr = cur.as_array_mut().unwrap();
            if *i >= arr.len() {
                arr.resize(*i + 1, Value::Null);
            }
            arr[*i] = value;
        }
        PathSeg::Wild => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn computed_materializes_and_chains() {
        let mut p = json!({"price": 5, "qty": 6});
        apply_computed(&json!({"total": "$.price * $.qty", "tag": "'prod-'", "total2": "$.total * 2"}), &mut p).unwrap();
        assert_eq!(p["total"], json!(30.0));
        assert_eq!(p["total2"], json!(60.0));
        assert_eq!(p["tag"], json!("prod-"));
    }

    #[test]
    fn computed_missing_inputs_resolve_null() {
        let mut p = json!({"a": 2});
        apply_computed(&json!({"b": "$.a * $.c"}), &mut p).unwrap();
        assert_eq!(p["b"], Value::Null);
    }

    #[test]
    fn computed_nested_path_set() {
        let mut p = json!({"o": {"a": [1, 2]}});
        apply_computed(&json!({"o.a[1]": "$.o.a[0] * 10"}), &mut p).unwrap();
        assert_eq!(p["o"]["a"][1], json!(10.0));
    }

    #[test]
    fn validate_first_failure_wins() {
        let rules = json!([
            {"when": "$.age < 0", "error": "age negative"},
            {"when": "$.age < 18", "error": "must be adult"}
        ]);
        assert_eq!(validate_rules(&rules, &json!({"age": 12})).unwrap().as_deref(), Some("must be adult"));
        assert_eq!(validate_rules(&rules, &json!({"age": 25})).unwrap(), None);
        assert!(validate_rules(&json!([]), &json!({})).is_ok());
    }

    #[test]
    fn redact_masks_paths() {
        let mut p = json!({"token": "abc", "nested": {"pin": 1234}, "items": [{"k": "v1"}, {"k": "v2"}]});
        apply_redact(&json!(["$.token", "$.nested.pin", "$.items[*].k"]), &mut p);
        assert_eq!(p["token"], json!("***"));
        assert_eq!(p["nested"]["pin"], json!("***"));
        assert_eq!(p["items"][0]["k"], json!("v1"), "wild paths are skipped");
        assert_eq!(p["items"][1]["k"], json!("v2"));
    }

    #[test]
    fn redact_whole_payload() {
        let mut p = json!({"a": 1});
        apply_redact(&json!(["$"]), &mut p);
        assert_eq!(p, json!("***"));
    }
}
