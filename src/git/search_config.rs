//! Declarative search config from `search.yaml` or `server.search` in `.relay.yaml`.

use crate::types::{RelayConfig, SearchConfig};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

pub const DEFAULT_SEARCH_YAML: &str = "search.yaml";

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SearchFileConfig {
    #[serde(default)]
    pub plugin: Option<String>,
    #[serde(default)]
    pub collections: HashMap<String, SearchCollectionConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SearchCollectionConfig {
    #[serde(default)]
    pub sources: Vec<SearchSource>,
    #[serde(default, rename = "idFields")]
    pub id_fields: Option<Vec<String>>,
    #[serde(default, rename = "ftsFields")]
    pub fts_fields: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SearchSource {
    MetaYaml {
        #[serde(default = "default_meta_glob")]
        glob: String,
    },
    PathIndex {
        #[serde(default = "default_index_json")]
        path: String,
        #[serde(default = "default_replace_mode")]
        mode: IngestMode,
    },
    EventsJson {
        #[serde(default = "default_events_json")]
        path: String,
        #[serde(default = "default_replace_mode")]
        mode: IngestMode,
    },
}

fn default_meta_glob() -> String {
    "data/**/meta.yaml".into()
}
fn default_index_json() -> String {
    "index.json".into()
}
fn default_events_json() -> String {
    "data/events.json".into()
}
fn default_replace_mode() -> IngestMode {
    IngestMode::Replace
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum IngestMode {
    #[default]
    Replace,
    Merge,
}

impl SearchFileConfig {
    /// Built-in defaults when no `search.yaml` / extended `server.search` is present.
    pub fn builtin_defaults() -> Self {
        let mut collections = HashMap::new();
        collections.insert(
            "index".into(),
            SearchCollectionConfig {
                sources: vec![
                    SearchSource::MetaYaml {
                        glob: default_meta_glob(),
                    },
                    SearchSource::PathIndex {
                        path: default_index_json(),
                        mode: IngestMode::Replace,
                    },
                ],
                id_fields: None,
                fts_fields: None,
            },
        );
        collections.insert(
            "events".into(),
            SearchCollectionConfig {
                sources: vec![SearchSource::EventsJson {
                    path: default_events_json(),
                    mode: IngestMode::Replace,
                }],
                id_fields: None,
                fts_fields: None,
            },
        );
        Self {
            plugin: Some("sqlite".into()),
            collections,
        }
    }

    pub fn collection(&self, name: &str) -> Option<&SearchCollectionConfig> {
        self.collections.get(name)
    }
}

/// Parse `search.yaml` body.
pub fn parse_search_yaml(text: &str) -> Result<SearchFileConfig, String> {
    serde_yaml::from_str(text).map_err(|e| format!("search.yaml: {e}"))
}

/// Merge file config over `.relay.yaml` `server.search` (file wins for collections).
pub fn merge_with_relay(file: SearchFileConfig, relay: &RelayConfig) -> SearchFileConfig {
    let mut out = file;
    if out.plugin.is_none() {
        if let Some(SearchConfig {
            plugin: Some(p), ..
        }) = relay.server.as_ref().and_then(|s| s.search.as_ref())
        {
            out.plugin = Some(p.clone());
        }
    }
    if out.collections.is_empty() {
        out = SearchFileConfig::builtin_defaults();
        if let Some(p) = relay
            .server
            .as_ref()
            .and_then(|s| s.search.as_ref())
            .and_then(|s| s.plugin.clone())
        {
            out.plugin = Some(p);
        }
    }
    out
}

/// Load search config from tip: optional override path, else `search.yaml`, else relay + builtins.
pub fn load_search_config(
    repo_path: &Path,
    commit: &str,
    relay: &RelayConfig,
    config_path_override: Option<&str>,
) -> SearchFileConfig {
    let path = config_path_override.unwrap_or(DEFAULT_SEARCH_YAML);
    if let Some(bytes) = git_show_bytes(repo_path, commit, path) {
        let text = String::from_utf8_lossy(&bytes);
        match parse_search_yaml(&text) {
            Ok(parsed) => return merge_with_relay(parsed, relay),
            Err(e) => {
                tracing::warn!(%e, path, "invalid search config; using defaults");
            }
        }
    } else if config_path_override.is_some() {
        tracing::warn!(path, "search config path missing; using defaults");
    }
    merge_with_relay(SearchFileConfig::default(), relay)
}

fn git_show_bytes(repo: &Path, commit: &str, path: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("show")
        .arg(format!("{commit}:{path}"))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_example_search_yaml() {
        let y = r#"
plugin: sqlite
collections:
  index:
    sources:
      - type: meta_yaml
        glob: data/**/meta.yaml
      - type: path_index
        path: index.json
        mode: merge
    idFields: [path, _meta_dir, id]
    ftsFields: [_title, title, _keywords, keywords, description]
  events:
    sources:
      - type: events_json
        path: data/events.json
    idFields: [id]
    ftsFields: [title, keywords, name]
"#;
        let c = parse_search_yaml(y).unwrap();
        assert_eq!(c.plugin.as_deref(), Some("sqlite"));
        let idx = c.collection("index").unwrap();
        assert_eq!(idx.sources.len(), 2);
        assert!(matches!(
            &idx.sources[1],
            SearchSource::PathIndex {
                mode: IngestMode::Merge,
                ..
            }
        ));
    }
}
