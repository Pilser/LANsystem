use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Recipe {
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_when")]
    pub when: String,
    #[serde(default)]
    pub match_json: Option<Value>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub dedup_on: Option<String>,
    #[serde(default)]
    pub actions: Vec<Value>,
}

fn default_when() -> String {
    "record.created".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Created,
    Updated,
    Deleted,
    Inbound,
    Cron,
}

impl Recipe {
    pub fn event_kind(&self) -> Option<EventKind> {
        let w = self.when.trim().to_lowercase();
        if w == "record.created" {
            Some(EventKind::Created)
        } else if w == "record.updated" {
            Some(EventKind::Updated)
        } else if w == "record.deleted" {
            Some(EventKind::Deleted)
        } else if w == "inbound" {
            Some(EventKind::Inbound)
        } else if w.starts_with("cron:") {
            Some(EventKind::Cron)
        } else {
            None
        }
    }

    pub fn is_cron_for(&self, job_name: &str) -> bool {
        self.when == format!("cron:{job_name}")
    }
}

pub fn parse_recipe(v: &Value) -> anyhow::Result<Recipe> {
    let r: Recipe = serde_json::from_value(v.clone())
        .map_err(|e| anyhow::anyhow!("invalid recipe json: {e}"))?;
    if r.event_kind().is_none() {
        anyhow::bail!("recipe when must be one of: record.created, record.updated, record.deleted, inbound, cron:<name>");
    }
    if r.actions.is_empty() {
        anyhow::bail!("recipe needs at least one action");
    }
    Ok(r)
}
