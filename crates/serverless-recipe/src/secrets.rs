use crate::context::EventContext;
use crate::rule::EventKind;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

const PREFIX: &str = "secret:";

fn hex(d: &[u8]) -> String {
    const MAP: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(d.len() * 2);
    for b in d {
        out.push(MAP[(b >> 4) as usize] as char);
        out.push(MAP[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn fingerprint(secret: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    hex(&h.finalize())
}

#[derive(Debug, Clone, Default)]
pub struct Secrets {
    map: HashMap<String, String>,
}

impl Secrets {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, name: &str, value: &str) {
        self.map.insert(name.to_uppercase(), value.to_string());
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.map.get(&name.to_uppercase()).map(|s| s.as_str())
    }

    pub fn names(&self) -> Vec<String> {
        let mut n: Vec<String> = self.map.keys().cloned().collect();
        n.sort();
        n
    }

    /// Mask for read responses: names only, never values.
    pub fn masked(&self) -> Value {
        let mut obj = serde_json::Map::new();
        for name in self.names() {
            obj.insert(name, Value::String(PREFIX.to_string()));
        }
        Value::Object(obj)
    }

    /// Substitute `{NAME}` / `${NAME}` / `{{NAME}}` in a string with the secret value.
    /// Unknown names are left verbatim. Longest pattern wins so `${X}`/`{{X}}` are
    /// not half-consumed by the bare `{X}` replacement.
    pub fn substitute(&self, input: &str) -> String {
        let mut out = input.to_string();
        for name in self.names().iter() {
            if let Some(val) = self.get(name) {
                for pat in [format!("{{{{{name}}}}}"), format!("${{{name}}}"), format!("{{{name}}}")] {
                    out = out.replace(&pat, val);
                }
            }
        }
        out
    }
}

/// Recursively substitute secrets in a JSON value (strings only, keys untouched).
pub fn substitute_value(secrets: &Secrets, v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(secrets.substitute(s)),
        Value::Array(arr) => Value::Array(arr.iter().map(|x| substitute_value(secrets, x)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                out.insert(k.clone(), substitute_value(secrets, val));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Context templating: `{{payload.a.b}}`, `{{seq}}`, `{{kind}}`, `{{board}}`,
// `{{writer}}`, `{{cron}}` plus the legacy `{{NAME}}` secret syntax. A string
// that is exactly one token resolves to its raw JSON value (numbers stay
// numbers, so `{"$inc": {"stock": "{{payload.qty}}"}}` works); tokens inside
// longer strings resolve to their string form. Unknown tokens fail the
// substitution (the caller skips the action and logs).
// ---------------------------------------------------------------------------

pub enum SubstitutionError {
    Unknown(String),
}

impl std::fmt::Display for SubstitutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubstitutionError::Unknown(token) => write!(f, "unknown template token {token:?}"),
        }
    }
}

impl EventKind {
    /// `record.created` / `record.updated` / `record.deleted` / `inbound` /
    /// `cron:<job>` — the same strings recipes store in `when_json`.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Created => "record.created",
            EventKind::Updated => "record.updated",
            EventKind::Deleted => "record.deleted",
            EventKind::Inbound => "inbound",
            EventKind::Cron => "cron",
        }
    }
}

fn ctx_scalar(ctx: &EventContext, name: &str) -> Option<Value> {
    Some(match name {
        "seq" => Value::Number(ctx.seq?.into()),
        "kind" => Value::String(ctx.kind.as_str().to_string()),
        "board" => Value::String(ctx.board_id.clone()),
        "writer" => Value::String(ctx.writer.clone().unwrap_or_default()),
        "cron" => Value::String(ctx.cron_name.clone().unwrap_or_default()),
        _ => return None,
    })
}

/// Resolve one `{{token}}` to its raw value: payload paths first, then the
/// context scalars, then the secrets map (legacy bare names).
pub fn resolve_token(
    secrets: &Secrets,
    ctx: &EventContext,
    token: &str,
) -> Result<Option<Value>, SubstitutionError> {
    let t = token.trim();
    if let Some(path) = t.strip_prefix("payload") {
        let path = path.trim_start_matches('.');
        let v = if path.is_empty() {
            ctx.payload.clone()
        } else if path.starts_with('$') {
            serverless_expr::get_path(&ctx.payload, path)
        } else {
            serverless_expr::get_path(&ctx.payload, &format!("$.{path}"))
        };
        if v.is_null() {
            return Err(SubstitutionError::Unknown(t.to_string()));
        }
        return Ok(Some(v));
    }
    if let Some(v) = ctx_scalar(ctx, t) {
        return Ok(Some(v));
    }
    if let Some(v) = secrets.get(t) {
        return Ok(Some(Value::String(v.to_string())));
    }
    Err(SubstitutionError::Unknown(t.to_string()))
}

/// String form of a resolved value: strings as-is, everything else JSON.
fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Substitute context + secrets into one string. A string that is exactly
/// one `{{token}}` returns the token's raw value (caller decides JSON-ness).
pub fn substitute_string(
    secrets: &Secrets,
    ctx: &EventContext,
    input: &str,
) -> Result<Value, SubstitutionError> {
    if let Some(token) = input.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")) {
        if !token.contains("{{") {
            if let Some(v) = resolve_token(secrets, ctx, token)? {
                return Ok(v);
            }
        }
    }
    let mut out = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(rest);
            return Ok(Value::String(out));
        };
        let token = &after[..end];
        let v = resolve_token(secrets, ctx, token)?;
        match v {
            Some(v) => out.push_str(&value_str(&v)),
            None => return Err(SubstitutionError::Unknown(token.to_string())),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

/// Recursively substitute context + secrets into a JSON value. Strings that
/// are exactly one template token become the token's raw JSON value; other
/// strings get inline substitution.
pub fn substitute_value_ctx(
    secrets: &Secrets,
    ctx: &EventContext,
    v: &Value,
) -> Result<Value, SubstitutionError> {
    match v {
        Value::String(s) => substitute_string(secrets, ctx, s),
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for x in arr {
                out.push(substitute_value_ctx(secrets, ctx, x)?);
            }
            Ok(Value::Array(out))
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                out.insert(k.clone(), substitute_value_ctx(secrets, ctx, val)?);
            }
            Ok(Value::Object(out))
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_all_syntaxes() {
        let mut s = Secrets::new();
        s.insert("STRIPE", "sk_test_123");
        let out = s.substitute("Bearer ${STRIPE} and {STRIPE} and {{STRIPE}}");
        assert_eq!(out, "Bearer sk_test_123 and sk_test_123 and sk_test_123");
    }

    #[test]
    fn masked_never_leaks() {
        let mut s = Secrets::new();
        s.insert("TOKEN", "super-secret");
        assert!(!serde_json::to_string(&s.masked()).unwrap().contains("super-secret"));
        assert!(s.masked().as_object().unwrap().contains_key("TOKEN"));
    }

    #[test]
    fn case_insensitive_lookup() {
        let mut s = Secrets::new();
        s.insert("TOKEN", "abc");
        assert_eq!(s.get("token"), Some("abc"));
    }
}
