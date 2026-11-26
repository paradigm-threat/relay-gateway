//! Fleet brain disabled (Linode redesign). Kept so `relay-brain` / CLI `brain` subcommands
//! compile and report disabled instead of auto-remediating.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainRequest {
    pub id: String,
    #[serde(default)]
    pub fqdn: String,
    #[serde(default)]
    pub scenario_id: Option<String>,
    #[serde(default)]
    pub message: String,
}

pub fn handoff_prompt(req: &BrainRequest) -> String {
    format!(
        "Brain disabled. Manual ops for {} ({})",
        req.fqdn,
        req.scenario_id.as_deref().unwrap_or("none")
    )
}

pub async fn run_serve(_execute: bool) -> Result<()> {
    eprintln!("relay-brain is disabled (Linode fleet ops redesign). Use relay-ops serve UI.");
    Err(anyhow!("brain disabled"))
}

pub fn status_json() -> serde_json::Value {
    json!({
        "ok": false,
        "disabled": true,
        "message": "brain disabled — use thin Fleet UI observe/repair actions"
    })
}
