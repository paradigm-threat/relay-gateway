//! Scenario YAML corpus removed (Linode fleet ops redesign). Stub kept for
//! optional brain binary linkage.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ScenarioHit {
    pub id: String,
    pub title: String,
    pub priority: i32,
    pub requires_confirm: bool,
    pub blocked: bool,
    pub steps: Vec<String>,
    pub primary_action: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ScenarioStore;

impl ScenarioStore {
    pub fn load(_dir: Option<&Path>) -> Self {
        Self
    }

    pub fn match_flags(&self, _flags: &InputFlags) -> Option<ScenarioHit> {
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct InputFlags;

pub fn scenarios_dir(_checkout: Option<&Path>) -> PathBuf {
    PathBuf::from("ops/scenarios")
}

pub fn load_default(_checkout: Option<&Path>) -> ScenarioStore {
    ScenarioStore
}
