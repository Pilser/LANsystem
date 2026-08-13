use crate::context::EventContext;
use crate::secrets::{Secrets, substitute_value};
use serde_json::{Map, Value};
use serverless_expr::ast::Expr;
use serverless_expr::path::parse_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionKind {
    Compute,
    Copy,
    Move,
    Format,
    Merge,
    Push,
    Pull,
    Sort,
    Slice,
    UpsertOther,
    PatchOther,
    Notify,
    Call,
    Schedule,
    SetState,
    Log,
}

impl ActionKind {
    pub fn parse(key: &str) -> Option<ActionKind> {
        Some(match key {
            "$compute" => ActionKind::Compute,
            "$copy" => ActionKind::Copy,
            "$move" => ActionKind::Move,
            "$format" => ActionKind::Format,
            "$merge" => ActionKind::Merge,
            "$push" => ActionKind::Push,
            "$pull" => ActionKind::Pull,
            "$sort" => ActionKind::Sort,
            "$slice" => ActionKind::Slice,
            "$upsert_other" => ActionKind::UpsertOther,
            "$patch_other" => ActionKind::PatchOther,
            "$notify" => ActionKind::Notify,
            "$call" => ActionKind::Call,
            "$schedule" => ActionKind::Schedule,
            "$set_state" => ActionKind::SetState,
            "$log" => ActionKind::Log,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub changed: bool,
    pub actions_run: Vec<String>,
    pub external: Vec<Value>,
    pub logs: Vec<String>,
}

impl RunReport {
    pub fn push(&mut self, kind: &ActionKind, detail: &str) {
        self.changed = true;
        self.actions_run.push(format!("{kind:?} {detail}"));
    }
}

fn obj_arg<'a>(name: &str, action: &'a Value) -> anyhow::Result<&'a Map<String, Value>> {
    action
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{name} must be an object"))
}

fn first_val(map: &Map<String, Value>) -> &Value {
    map.values().next().unwrap_or(&Value::Null)
}

fn path_str(v: &Value) -> anyhow::Result<String> {
    v.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("expected a json path string"))
}

fn eval_string(expr: &str, payload: &Value) -> anyhow::Result<Value> {
    let e: Expr = serverless_expr::parse(expr).map_err(|e| anyhow::anyhow!("bad expr '{expr}': {e}"))?;
    Ok(serverless_expr::apply(&e, payload)?)
}

fn normalize(p: &str) -> String {
    if p.starts_with('$') {
        p.to_string()
    } else {
        format!("$.{}", p)
    }
}

fn get_at(payload: &Value, path: &str) -> Value {
    match parse_path(&normalize(path)) {
        Ok(segs) => serverless_expr::eval::resolve_path(payload, &segs),
        Err(_) => Value::Null,
    }
}

fn set_at(payload: &mut Value, path: &str, value: Value) -> anyhow::Result<()> {
    let segs = parse_path(&normalize(path))?;
    if segs.is_empty() {
        *payload = value;
        return Ok(());
    }
    let mut cur = payload;
    for (i, seg) in segs.iter().enumerate() {
        let last = i == segs.len() - 1;
        use serverless_expr::ast::PathSeg;
        match seg {
            PathSeg::Key(k) => {
                if !cur.is_object() {
                    *cur = Value::Object(Map::new());
                }
                let obj = cur.as_object_mut().unwrap();
                if last {
                    obj.insert(k.clone(), value);
                    return Ok(());
                }
                let entry = obj.entry(k.clone()).or_insert(Value::Null);
                cur = entry;
            }
            PathSeg::Index(i) => {
                if !cur.is_array() {
                    *cur = Value::Array(Vec::new());
                }
                let arr = cur.as_array_mut().unwrap();
                if *i >= arr.len() {
                    arr.resize(*i + 1, Value::Null);
                }
                if last {
                    arr[*i] = value;
                    return Ok(());
                }
                cur = &mut arr[*i];
            }
            PathSeg::Wild => return Err(anyhow::anyhow!("cannot write to wildcard path '{path}'")),
        }
    }
    Ok(())
}

fn remove_at(payload: &mut Value, path: &str) -> anyhow::Result<()> {
    let segs = parse_path(&normalize(path))?;
    let mut cur = payload;
    for (i, seg) in segs.iter().enumerate() {
        let last = i == segs.len() - 1;
        use serverless_expr::ast::PathSeg;
        match seg {
            PathSeg::Key(k) => {
                if !cur.is_object() {
                    return Ok(());
                }
                let obj = cur.as_object_mut().unwrap();
                if last {
                    obj.remove(k);
                    return Ok(());
                }
                match obj.get_mut(k) {
                    Some(next) => cur = next,
                    None => return Ok(()),
                }
            }
            PathSeg::Index(i) => {
                if !cur.is_array() {
                    return Ok(());
                }
                let arr = cur.as_array_mut().unwrap();
                if last {
                    if *i < arr.len() {
                        arr.remove(*i);
                    }
                    return Ok(());
                }
                match arr.get_mut(*i) {
                    Some(next) => cur = next,
                    None => return Ok(()),
                }
            }
            PathSeg::Wild => return Err(anyhow::anyhow!("cannot remove from wildcard path '{path}'")),
        }
    }
    Ok(())
}

fn deep_merge(base: &mut Value, patch: &Value) {
    match (base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(existing) => deep_merge(existing, v),
                    None => { b.insert(k.clone(), v.clone()); }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

fn match_filter(payload: &Value, filter: &Value) -> anyhow::Result<bool> {
    let obj = filter
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("recipe match must be an object of conditions"))?;
    for (path, cond) in obj {
        let actual = get_at(payload, path);
        let result = match cond {
            Value::Object(ops) => {
                let mut ok = true;
                for (op, want) in ops {
                    let r = apply_op(op, &actual, want)?;
                    if !r {
                        ok = false;
                        break;
                    }
                }
                ok
            }
            other => apply_op("eq", &actual, other)?,
        };
        if !result {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Public entry for matching a recipe `match` filter against a payload.
pub fn match_public(payload: &Value, filter: &Value) -> anyhow::Result<bool> {
    match_filter(payload, filter)
}

fn apply_op(op: &str, actual: &Value, want: &Value) -> anyhow::Result<bool> {
    let eq = |a: &Value, b: &Value| -> bool {
        match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        }
    };
    let cmp = |a: &Value, b: &Value| -> Option<std::cmp::Ordering> {
        if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) {
            x.partial_cmp(&y)
        } else {
            let sa = a.as_str();
            let sb = b.as_str();
            match (sa, sb) {
                (Some(x), Some(y)) => Some(x.cmp(y)),
                _ => None,
            }
        }
    };
    Ok(match op {
        "eq" | "==" => eq(actual, want),
        "neq" | "!=" | "ne" => !eq(actual, want),
        "gt" => cmp(actual, want).map(|o| o.is_gt()).unwrap_or(false),
        "gte" => cmp(actual, want).map(|o| !o.is_lt()).unwrap_or(false),
        "lt" => cmp(actual, want).map(|o| o.is_lt()).unwrap_or(false),
        "lte" => cmp(actual, want).map(|o| !o.is_gt()).unwrap_or(false),
        "contains" => match actual {
            Value::String(s) => want.as_str().map(|w| s.contains(w)).unwrap_or(false),
            Value::Array(arr) => arr.iter().any(|x| eq(x, want)),
            _ => false,
        },
        "not_contains" => match actual {
            Value::String(s) => want.as_str().map(|w| !s.contains(w)).unwrap_or(true),
            Value::Array(arr) => !arr.iter().any(|x| eq(x, want)),
            _ => true,
        },
        "in" => want
            .as_array()
            .map(|arr| arr.iter().any(|x| eq(x, actual)))
            .unwrap_or(false),
        "exists" => !actual.is_null(),
        "search" => false,
        other => return Err(anyhow::anyhow!("unsupported match operator '{other}'")),
    })
}

/// Apply one action to the running payload. External/side-effect actions are
/// reported into `report.external` for the caller to execute (DB calls etc).
/// Returns Ok when the action was handled; Err on malformed config.
pub fn apply_action(
    kind: &ActionKind,
    action: &Value,
    payload: &mut Value,
    secrets: &Secrets,
    report: &mut RunReport,
    _ctx: &EventContext,
) -> anyhow::Result<()> {
    let map = obj_arg(match kind {
        ActionKind::Log => "$log",
        other => name_for(other),
    }, action)?;
    match kind {
        ActionKind::Compute => {
            for (field, expr) in map {
                let e = expr.as_str().ok_or_else(|| anyhow::anyhow!("$compute value for '{field}' must be a string expr"))?;
                let v = eval_string(e, payload)?;
                set_at(payload, field, v)?;
            }
            report.push(kind, "computed fields");
        }
        ActionKind::Copy | ActionKind::Move => {
            let from = path_str(map.get("from").unwrap_or(&Value::Null))?;
            let to = path_str(map.get("to").unwrap_or(&Value::Null))?;
            let v = get_at(payload, &from);
            set_at(payload, &to, v)?;
            if *kind == ActionKind::Move {
                remove_at(payload, &from)?;
            }
            report.push(kind, &format!("{from} -> {to}"));
        }
        ActionKind::Format => {
            let field = path_str(map.get("field").unwrap_or(&Value::Null))?;
            let pattern = map
                .get("pattern")
                .and_then(|p| p.as_str())
                .ok_or_else(|| anyhow::anyhow!("$format needs a pattern string"))?;
            let rendered = render_template(pattern, payload)?;
            set_at(payload, &field, Value::String(rendered.clone()))?;
            report.push(kind, &format!("{field} = \"{rendered}\""));
        }
        ActionKind::Merge => {
            let into = path_str(map.get("into").unwrap_or(&Value::Null))?;
            let patch = first_val(map);
            let mut target = get_at(payload, &into);
            deep_merge(&mut target, patch);
            set_at(payload, &into, target)?;
            report.push(kind, &format!("merge into {into}"));
        }
        ActionKind::Push => {
            let path = path_str(map.get("path").unwrap_or(&Value::Null))?;
            let value = map.get("value").cloned().unwrap_or(Value::Null);
            let mut arr = get_at(payload, &path);
            if !arr.is_array() {
                arr = Value::Array(Vec::new());
            }
            arr.as_array_mut().unwrap().push(value);
            set_at(payload, &path, arr)?;
            report.push(kind, &format!("push to {path}"));
        }
        ActionKind::Pull => {
            let path = path_str(map.get("path").unwrap_or(&Value::Null))?;
            let m = map.get("match").cloned().unwrap_or(Value::Null);
            let arr = get_at(payload, &path);
            if arr.is_array() {
                let keep: Vec<Value> = arr
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|x| !matches!(match_filter(x, &m), Ok(true)))
                    .cloned()
                    .collect();
                set_at(payload, &path, Value::Array(keep))?;
            }
            report.push(kind, &format!("pull from {path}"));
        }
        ActionKind::Sort => {
            let path = path_str(map.get("path").unwrap_or(&Value::Null))?;
            let by = map.get("by").and_then(|b| b.as_str()).map(|s| s.to_string());
            let desc = map.get("order").and_then(|o| o.as_str()).map(|o| o == "desc").unwrap_or(false);
            let mut arr = get_at(payload, &path);
            if arr.is_array() {
                let list = arr.as_array_mut().unwrap();
                list.sort_by(|a, b| {
                    let va = match &by {
                        Some(p) => get_at(a, p),
                        None => a.clone(),
                    };
                    let vb = match &by {
                        Some(p) => get_at(b, p),
                        None => b.clone(),
                    };
                    let ord = match (va.as_f64(), vb.as_f64()) {
                        (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                        _ => va.to_string().cmp(&vb.to_string()),
                    };
                    if desc { ord.reverse() } else { ord }
                });
            }
            report.push(kind, &format!("sort {path}"));
        }
        ActionKind::Slice => {
            let path = path_str(map.get("path").unwrap_or(&Value::Null))?;
            let start = map.get("start").and_then(|s| s.as_i64()).unwrap_or(0).max(0) as usize;
            let end = map.get("end").and_then(|e| e.as_i64()).map(|e| e.max(0) as usize);
            let arr = get_at(payload, &path);
            if arr.is_array() {
                let list = arr.as_array().unwrap();
                let kept: Vec<Value> = match end {
                    Some(e) => list.iter().skip(start).take(e.saturating_sub(start)).cloned().collect(),
                    None => list.iter().skip(start).cloned().collect(),
                };
                set_at(payload, &path, Value::Array(kept))?;
            }
            report.push(kind, &format!("slice {path}"));
        }
        ActionKind::SetState => {
            let state = map.get("state").cloned().unwrap_or(Value::Null);
            set_at(payload, "state", state)?;
            report.push(kind, "set state");
        }
        ActionKind::Log => {
            let msg = map.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            let rendered = secrets.substitute(&render_template(&msg, payload)?);
            report.logs.push(rendered.clone());
            report.actions_run.push(format!("$log {rendered}"));
        }
        ActionKind::UpsertOther
        | ActionKind::PatchOther
        | ActionKind::Notify
        | ActionKind::Call
        | ActionKind::Schedule => {
            let substituted = substitute_value(secrets, action);
            report.external.push(substituted);
            report.push(kind, "queued external action");
        }
    }
    Ok(())
}

fn name_for(kind: &ActionKind) -> &'static str {
    match kind {
        ActionKind::Compute => "$compute",
        ActionKind::Copy => "$copy",
        ActionKind::Move => "$move",
        ActionKind::Format => "$format",
        ActionKind::Merge => "$merge",
        ActionKind::Push => "$push",
        ActionKind::Pull => "$pull",
        ActionKind::Sort => "$sort",
        ActionKind::Slice => "$slice",
        ActionKind::UpsertOther => "$upsert_other",
        ActionKind::PatchOther => "$patch_other",
        ActionKind::Notify => "$notify",
        ActionKind::Call => "$call",
        ActionKind::Schedule => "$schedule",
        ActionKind::SetState => "$set_state",
        ActionKind::Log => "$log",
    }
}

fn render_template(pattern: &str, payload: &Value) -> anyhow::Result<String> {
    let mut out = String::new();
    let mut rest = pattern;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start..].find('}') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 1..start + end];
        if let Some(stripped) = inner.strip_prefix('$') {
            let path = format!("${stripped}");
            out.push_str(&get_at(payload, &path).to_string());
        } else {
            out.push_str(&get_at(payload, inner).to_string());
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> EventContext {
        EventContext::record_created("b", 1, &json!({"a": 1}), None)
    }

    #[test]
    fn compute_merge_push_format() {
        let mut payload = json!({"price": 10, "qty": 3});
        let mut report = RunReport::default();
        apply_action(&ActionKind::Compute, &json!({"total": "$.price * $.qty"}), &mut payload, &Secrets::new(), &mut report, &ctx()).unwrap();
        assert_eq!(payload["total"], json!(30.0));
        apply_action(&ActionKind::Format, &json!({"field": "label", "pattern": "{$.price} x {$.qty}"}), &mut payload, &Secrets::new(), &mut report, &ctx()).unwrap();
        assert_eq!(payload["label"], json!("10 x 3"));
        apply_action(&ActionKind::Push, &json!({"path": "$.items", "value": 5}), &mut payload, &Secrets::new(), &mut report, &ctx()).unwrap();
        assert_eq!(payload["items"], json!([5]));
    }

    #[test]
    fn match_filter_semantics() {
        let payload = json!({"status": "paid", "qty": 5});
        assert!(match_filter(&payload, &json!({"$.status": {"eq": "paid"}})).unwrap());
        assert!(match_filter(&payload, &json!({"$.qty": {"gte": 3}})).unwrap());
        assert!(!match_filter(&payload, &json!({"$.qty": {"gte": 10}})).unwrap());
    }
}
