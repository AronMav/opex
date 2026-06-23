/// Workspace-based infrastructure service loader.
///
/// Service configs live flat in `workspace/tools/*.yaml` alongside YAML tools.
/// Each file defines one service endpoint: name, type, url, `max_concurrent`, healthcheck.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::config::ToolConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceFileEntry {
    pub name: String,
    #[serde(rename = "type", default = "default_type")]
    pub tool_type: String,
    pub url: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub healthcheck: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_path: Option<String>,
}

fn default_type() -> String {
    "external".to_string()
}

fn default_max_concurrent() -> u32 {
    1
}

impl ServiceFileEntry {
    pub fn to_tool_config(&self) -> ToolConfig {
        ToolConfig {
            tool_type: self.tool_type.clone(),
            url: self.url.clone(),
            max_concurrent: self.max_concurrent,
            healthcheck: self.healthcheck.clone(),
            api_key_env: None,
            protocol: None,
            depends_on: self.depends_on.clone(),
            ui_path: self.ui_path.clone(),
        }
    }
}

fn services_dir(_workspace_dir: &str) -> std::path::PathBuf {
    Path::new("config").join("services")
}

/// Load all service entries from config/services/*.yaml.
pub async fn load_service_entries(workspace_dir: &str) -> Vec<ServiceFileEntry> {
    let dir = services_dir(workspace_dir);
    let mut entries = Vec::new();

    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(d) => d,
        Err(_) => return entries,
    };

    while let Ok(Some(entry)) = rd.next_entry().await {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "failed to read service config");
                continue;
            }
        };

        if let Ok(entry) = serde_yaml::from_str::<ServiceFileEntry>(&content) {
            tracing::debug!(service = %entry.name, "loaded service config");
            entries.push(entry);
        } else {
            // Silently skip non-service YAML files (e.g. tool definitions)
        }
    }

    entries
}

/// Load service configs as a `HashMap` keyed by name (for `ToolRegistry`).
pub async fn load_service_map(workspace_dir: &str) -> HashMap<String, ToolConfig> {
    let entries = load_service_entries(workspace_dir).await;
    entries
        .into_iter()
        .map(|e| (e.name.clone(), e.to_tool_config()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_type_is_external() {
        assert_eq!(default_type(), "external");
    }

    #[test]
    fn default_max_concurrent_is_one() {
        assert_eq!(default_max_concurrent(), 1);
    }

    #[test]
    fn to_tool_config_maps_all_fields() {
        let entry = ServiceFileEntry {
            name: "test-svc".to_string(),
            tool_type: "external".to_string(),
            url: "http://localhost:9000".to_string(),
            max_concurrent: 3,
            healthcheck: Some("/health".to_string()),
            depends_on: vec!["db".to_string()],
            ui_path: Some("/ui".to_string()),
        };
        let cfg = entry.to_tool_config();
        assert_eq!(cfg.tool_type, "external");
        assert_eq!(cfg.url, "http://localhost:9000");
        assert_eq!(cfg.max_concurrent, 3);
        assert_eq!(cfg.healthcheck, Some("/health".to_string()));
        assert_eq!(cfg.api_key_env, None);
        assert_eq!(cfg.protocol, None);
        assert_eq!(cfg.depends_on, vec!["db".to_string()]);
        assert_eq!(cfg.ui_path, Some("/ui".to_string()));
    }

    #[test]
    fn yaml_deserialize_minimal() {
        let yaml = "name: minimal\nurl: http://example.com\n";
        let entry: ServiceFileEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.name, "minimal");
        assert_eq!(entry.url, "http://example.com");
        assert_eq!(entry.tool_type, "external");
        assert_eq!(entry.max_concurrent, 1);
        assert!(entry.healthcheck.is_none());
        assert!(entry.depends_on.is_empty());
    }

    #[test]
    fn yaml_roundtrip() {
        let original = ServiceFileEntry {
            name: "roundtrip".to_string(),
            tool_type: "external".to_string(),
            url: "http://rt.example.com".to_string(),
            max_concurrent: 2,
            healthcheck: Some("/ping".to_string()),
            depends_on: vec![],
            ui_path: None,
        };
        let yaml = serde_yaml::to_string(&original).unwrap();
        let parsed: ServiceFileEntry = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.name, original.name);
        assert_eq!(parsed.url, original.url);
        assert_eq!(parsed.max_concurrent, original.max_concurrent);
        assert_eq!(parsed.healthcheck, original.healthcheck);
    }
}
