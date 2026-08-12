//! Record DSL: JSON-path lookups, filter predicates, aggregate ops and
//! JSON-patch operations — pure, shared by the store, REST and the CLI.

use serde_json::Value as Json;

/// Split one dotted segment like `list[1]` or `a[0][2]` into its base key
/// and bracket indices.
fn seg_parts(seg: &str) -> (String, Vec<usize>) {
    let seg = seg.trim();
    let base: String = seg.chars().take_while(|c| *c != '[').collect();
    let mut idxs = Vec::new();
    let mut rest = &seg[base.len()..];
    while let Some(open) = rest.find('[') {
        let close = rest[open + 1..].find(']');
        let Some(close) = close else { break };
        let inner = &rest[open + 1..open + 1 + close];
        if let Ok(i) = inner.trim().parse::<usize>() {
            idxs.push(i);
        }
        rest = &rest[open + 1 + close + 1..];
    }
    (base, idxs)
}

/// Walk a JSON path like `$.a.b[2].c` (or `a.b`, or `$` for the root).
/// Numbers in brackets index arrays; every other segment is an object key.
pub fn json_path<'a>(value: &'a Json, path: &str) -> Option<&'a Json> {
    let p = path.trim().trim_start_matches('$');
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        return Some(value);
    }
    let mut cur = value;
    for seg in p.split('.') {
        if seg.trim().is_empty() {
            continue;
        }
        let (base, idxs) = seg_parts(seg);
        if !base.is_empty() {
            cur = cur.get(base)?;
        }
        for i in idxs {
            cur = cur.get(i)?;
        }
    }
    Some(cur)
}

/// Extract a JSON path as a SQL `json_extract` text-safe path (`$` root,
/// `$.a.b[2]`), or None when the path is not a simple dotted/array walk.
pub fn sql_json_path(path: &str) -> Option<String> {
    let p = path.trim().trim_start_matches('$');
    let p = p.strip_prefix('.').unwrap_or(p);
    let mut out = String::from("$");
    for seg in p.split('.') {
        if seg.trim().is_empty() {
            continue;
        }
        let (base, idxs) = seg_parts(seg);
        if !base.is_empty() {
            out.push('.');
            out.push_str(&base);
        }
        for i in idxs {
            // SQLite json_extract uses $.a.b.2 for array indexes.
            out.push('.');
            out.push_str(&i.to_string());
        }
    }
    Some(out)
}

/// A record filter predicate.
#[derive(Debug, Clone)]
pub enum Op {
    Eq(Json),
    Neq(Json),
    Gt(Json),
    Gte(Json),
    Lt(Json),
    Lte(Json),
    Contains(String),
    NotContains(String),
    In(Vec<Json>),
}

#[derive(Debug, Clone, Default)]
pub struct Cond {
    pub path: String,
    pub op: Op,
}

impl Default for Op {
    fn default() -> Self {
        Op::Eq(Json::Null)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub conds: Vec<Cond>,
    pub order_path: Option<String>,
    pub order_desc: bool,
}

/// Parse one condition object or an `and`-list of them (zerowrapper shape):
/// `{"field":"$.price","op":"gt","value":100}`.
pub fn parse_filter(value: &Json) -> Result<Filter, String> {
    let items: Vec<&Json> = match value {
        Json::Array(items) => items.iter().collect(),
        Json::Object(_) => vec![value],
        _ => return Err("filter must be an object or an array of objects".to_string()),
    };
    let mut conds = Vec::new();
    for item in items {
        let Some(field) = item.get("field").and_then(Json::as_str) else {
            return Err("filter condition needs a `field`".to_string());
        };
        let op = item.get("op").and_then(Json::as_str).unwrap_or("eq");
        let cond_value = item.get("value").ok_or_else(|| format!("condition `{field}` needs a `value`"))?;
        let cond = match op {
            "eq" => Cond { path: field.to_string(), op: Op::Eq(cond_value.clone()) },
            "neq" => Cond { path: field.to_string(), op: Op::Neq(cond_value.clone()) },
            "gt" => Cond { path: field.to_string(), op: Op::Gt(cond_value.clone()) },
            "gte" => Cond { path: field.to_string(), op: Op::Gte(cond_value.clone()) },
            "lt" => Cond { path: field.to_string(), op: Op::Lt(cond_value.clone()) },
            "lte" => Cond { path: field.to_string(), op: Op::Lte(cond_value.clone()) },
            "contains" => Cond {
                path: field.to_string(),
                op: Op::Contains(cond_value.as_str().unwrap_or_default().to_string()),
            },
            "not_contains" => Cond {
                path: field.to_string(),
                op: Op::NotContains(cond_value.as_str().unwrap_or_default().to_string()),
            },
            "in" => match cond_value.as_array() {
                Some(list) => Cond { path: field.to_string(), op: Op::In(list.clone()) },
                None => return Err(format!("`in` value for `{field}` must be an array")),
            },
            other => return Err(format!("unknown op `{other}` (eq/neq/gt/gte/lt/lte/contains/not_contains/in)")),
        };
        conds.push(cond);
    }
    Ok(Filter { conds, ..Default::default() })
}

/// In-memory predicate check (used for SSE catch-up filtering and the
/// LIKE-fallback search path).
pub fn matches(payload: &Json, conds: &[Cond]) -> bool {
    conds.iter().all(|c| match &c.op {
        Op::Eq(v) => json_path(payload, &c.path).is_some_and(|got| got == v),
        Op::Neq(v) => json_path(payload, &c.path).is_none_or(|got| got != v),
        Op::Gt(v) => cmp_num(payload, &c.path, v, |a, b| a > b),
        Op::Gte(v) => cmp_num(payload, &c.path, v, |a, b| a >= b),
        Op::Lt(v) => cmp_num(payload, &c.path, v, |a, b| a < b),
        Op::Lte(v) => cmp_num(payload, &c.path, v, |a, b| a <= b),
        Op::Contains(s) => json_path(payload, &c.path)
            .and_then(Json::as_str)
            .is_some_and(|got| got.to_lowercase().contains(&s.to_lowercase())),
        Op::NotContains(s) => json_path(payload, &c.path)
            .and_then(Json::as_str)
            .is_none_or(|got| !got.to_lowercase().contains(&s.to_lowercase())),
        Op::In(list) => json_path(payload, &c.path).is_some_and(|got| list.iter().any(|v| v == got)),
    })
}

fn cmp_num(payload: &Json, path: &str, want: &Json, cmp: fn(f64, f64) -> bool) -> bool {
    let Some(got) = json_path(payload, path) else { return false };
    let (Some(a), Some(b)) = (got.as_f64(), want.as_f64()) else { return false };
    cmp(a, b)
}

/// Aggregate operations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Agg {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl Agg {
    pub fn parse(s: &str) -> Option<Agg> {
        match s {
            "count" => Some(Agg::Count),
            "sum" => Some(Agg::Sum),
            "avg" => Some(Agg::Avg),
            "min" => Some(Agg::Min),
            "max" => Some(Agg::Max),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Agg::Count => "count",
            Agg::Sum => "sum",
            Agg::Avg => "avg",
            Agg::Min => "min",
            Agg::Max => "max",
        }
    }
}

/// Build an integral JSON number when `f` is whole, else a float.
pub fn num_from_f64(f: f64) -> Json {
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Json::from(f as i64)
    } else {
        serde_json::Number::from_f64(f).map(Json::Number).unwrap_or(Json::Null)
    }
}

/// Apply `$set/$inc/$dec/$mul/$unset` patch ops (Mongo-ish, zerowrapper
/// shape) to a payload. `$inc/$dec/$mul` target a dotted path *or* a flat
/// top-level key. Returns the new payload.
pub fn apply_patch(payload: &mut Json, ops: &Json) -> Result<(), String> {
    let Some(ops) = ops.as_object() else {
        return Err("patch body must be an object of $set/$inc/$dec/$mul/$unset".to_string());
    };
    for (op, value) in ops {
        let Some(patch_map) = value.as_object() else {
            return Err(format!("`{op}` must be an object of field → value"));
        };
        match op.as_str() {
            "$set" => {
                for (k, v) in patch_map {
                    set_path(payload, k, v.clone())?;
                }
            }
            "$inc" | "$dec" | "$mul" => {
                for (k, v) in patch_map {
                    let delta = match op.as_str() {
                        "$inc" => v.as_f64().ok_or_else(|| format!("`{op}` {k} is not a number"))?,
                        "$dec" => -v.as_f64().ok_or_else(|| format!("`{op}` {k} is not a number"))?,
                        _ => v.as_f64().ok_or_else(|| format!("`$mul` {k} is not a number"))?,
                    };
                    let target = get_mut_path(payload, k).ok_or_else(|| format!("`{op}` has no field `{k}`"))?;
                    match target {
                        Json::Number(n) => {
                            let cur = n.as_f64().unwrap_or(0.0);
                            *target = num_from_f64(if op == "$mul" { cur * delta } else { cur + delta });
                        }
                        Json::Null => *target = num_from_f64(delta),
                        _ => return Err(format!("`{op}` field `{k}` is not numeric")),
                    }                }
            }
            "$unset" => {
                for k in patch_map.keys() {
                    unset_path(payload, k);
                }
            }
            other => return Err(format!("unknown patch op `{other}` ($set/$inc/$dec/$mul/$unset)")),
        }
    }
    Ok(())
}

fn set_path(root: &mut Json, path: &str, value: Json) -> Result<(), String> {
    let p = path.trim().trim_start_matches('$');
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        return Err("$set path cannot be empty".to_string());
    }
    let segs: Vec<&str> = p.split('.').collect();
    let mut cur = root;
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i == segs.len() - 1;
        let (base, idxs) = seg_parts(seg);
        if !base.is_empty() {
            let obj = cur.as_object_mut().ok_or_else(|| format!("path `{path}`: `{seg}` is not an object"))?;
            if is_last && idxs.is_empty() {
                obj.insert(base, value);
                return Ok(());
            }
            let entry = obj.entry(base).or_insert(Json::Null);
            if !idxs.is_empty() && entry.is_null() {
                *entry = Json::Array(Vec::new());
            }
            cur = entry;
        }
        for (n, idx) in idxs.iter().enumerate() {
            let arr = cur.as_array_mut().ok_or_else(|| format!("path `{path}`: `[{idx}]` is not an array"))?;
            if *idx >= arr.len() {
                arr.resize(idx + 1, Json::Null);
            }
            let at_last = is_last && n == idxs.len() - 1;
            if at_last {
                arr[*idx] = value.clone();
                return Ok(());
            }
            cur = &mut arr[*idx];
        }
    }
    Ok(())
}

fn get_mut_path<'a>(root: &'a mut Json, path: &str) -> Option<&'a mut Json> {
    let p = path.trim().trim_start_matches('$');
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in p.split('.') {
        if seg.trim().is_empty() {
            continue;
        }
        let (base, idxs) = seg_parts(seg);
        if !base.is_empty() {
            cur = cur.get_mut(base)?;
        }
        for i in idxs {
            cur = cur.get_mut(i)?;
        }
    }
    Some(cur)
}

fn unset_path(root: &mut Json, path: &str) {
    let p = path.trim().trim_start_matches('$');
    let p = p.strip_prefix('.').unwrap_or(p);
    if p.is_empty() {
        return;
    }
    let segs: Vec<&str> = p.split('.').collect();
    let mut cur = root;
    for (i, seg) in segs.iter().enumerate() {
        let is_last = i == segs.len() - 1;
        let (base, idxs) = seg_parts(seg);
        if !base.is_empty() {
            let Some(obj) = cur.as_object_mut() else { return };
            if is_last && idxs.is_empty() {
                obj.remove(&base);
                return;
            }
            let Some(next) = obj.get_mut(&base) else { return };
            cur = next;
        }
        for (n, idx) in idxs.iter().enumerate() {
            let Some(arr) = cur.as_array_mut() else { return };
            if *idx >= arr.len() {
                return;
            }
            if is_last && n == idxs.len() - 1 {
                arr.remove(*idx);
                return;
            }
            cur = &mut arr[*idx];
        }
    }
}

/// Build the `value` SQL for an aggregate `field` path, defaulting to a
/// numeric cast of the whole payload when absent.
pub fn agg_sql_field(field: Option<&str>) -> String {
    match field {
        Some(f) => format!("CAST(json_extract(payload, {}) AS REAL)", quote(sql_json_path(f).unwrap_or_else(|| "$".to_string()))),
        None => "1.0".to_string(),
    }
}

pub fn quote(s: String) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn path_lookup() {
        let v = json!({"a": {"b": [10, 20, {"c": "x"}]}});
        assert_eq!(json_path(&v, "$.a.b[2].c"), Some(&json!("x")));
        assert_eq!(json_path(&v, "a.b[1]"), Some(&json!(20)));
        assert_eq!(json_path(&v, "$"), Some(&v));
        assert_eq!(json_path(&v, "$.a.b"), Some(&json!([10, 20, {"c": "x"}])));
        assert_eq!(json_path(&v, "$.missing"), None);
        assert_eq!(sql_json_path("$.a.b[2].c").as_deref(), Some("$.a.b.2.c"));
    }

    #[test]
    fn filter_matches() {
        let filter = parse_filter(&json!([
            {"field": "$.price", "op": "gt", "value": 10},
            {"field": "$.tag", "op": "in", "value": ["a", "b"]}
        ]))
        .unwrap();
        assert!(matches(&json!({"price": 20, "tag": "b"}), &filter.conds));
        assert!(!matches(&json!({"price": 5, "tag": "b"}), &filter.conds));
        assert!(!matches(&json!({"price": 20, "tag": "c"}), &filter.conds));
        let contains = parse_filter(&json!({"field": "$.name", "op": "contains", "value": "le"}))
            .unwrap();
        assert!(matches(&json!({"name": "Apple"}), &contains.conds));
        assert!(!matches(&json!({"name": "Pear"}), &contains.conds));
    }

    #[test]
    fn patch_ops() {
        let mut v = json!({"count": 1, "name": "a", "nested": {"x": 2}});
        apply_patch(
            &mut v,
            &json!({
                "$set": {"name": "b", "nested.y": 3, "list[1]": "two"},
                "$inc": {"count": 4},
                "$mul": {"nested.x": 5},
                "$unset": {"old": null}
            }),
        )
        .unwrap();
        assert_eq!(v, json!({"count": 5, "name": "b", "nested": {"x": 10, "y": 3}, "list": [null, "two"]}));
        let mut bad = json!({"n": 1});
        assert!(apply_patch(&mut bad, &json!({"$inc": {"missing": 1}})).is_err());
    }

    #[test]
    fn aggregate_parse() {
        assert_eq!(Agg::parse("sum"), Some(Agg::Sum));
        assert_eq!(Agg::parse("avg"), Some(Agg::Avg));
        assert_eq!(Agg::parse("bogus"), None);
    }
}
