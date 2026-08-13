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
