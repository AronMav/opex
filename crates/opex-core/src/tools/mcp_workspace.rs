/// Workspace-based MCP config loader.
///
/// MCP server configs live in `workspace/mcp/*.yaml`.
/// Each file defines one server: name, container/url, port, mode, `idle_timeout`.
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use crate::config::{McpConfig, McpFileEntry};

/// Load all MCP entries from workspace/mcp/*.yaml.
pub async fn load_mcp_entries(mcp_dir: &str) -> Vec<McpFileEntry> {
    let dir = Path::new(mcp_dir);
    let mut entries = Vec::new();

    let mut rd = match tokio::fs::read_dir(dir).await {
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
                tracing::warn!(file = %path.display(), error = %e, "failed to read MCP config");
                continue;
            }
        };

        match serde_yaml::from_str::<McpFileEntry>(&content) {
            Ok(entry) => {
                tracing::debug!(mcp = %entry.name, "loaded MCP config");
                entries.push(entry);
            }
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "failed to parse MCP config");
            }
        }
    }

    entries
}

/// Load MCP configs as a `HashMap` keyed by server name (for `ContainerManager`).
/// Only includes enabled entries.
pub async fn load_mcp_map(mcp_dir: &str) -> HashMap<String, McpConfig> {
    let entries = load_mcp_entries(mcp_dir).await;
    entries
        .into_iter()
        .filter(|e| e.enabled)
        .map(|e| (e.name.clone(), e.to_config()))
        .collect()
}

/// Write/update a MCP config file at workspace/mcp/{name}.yaml.
pub async fn save_mcp_entry(mcp_dir: &str, entry: &McpFileEntry) -> Result<()> {
    if !entry.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("invalid MCP entry name: '{}'", entry.name);
    }

    tokio::fs::create_dir_all(mcp_dir).await
        .with_context(|| format!("failed to create mcp dir: {mcp_dir}"))?;

    let yaml = serde_yaml::to_string(entry)
        .with_context(|| format!("failed to serialize MCP entry '{}'", entry.name))?;

    let path = Path::new(mcp_dir).join(format!("{}.yaml", entry.name));
    tokio::fs::write(&path, yaml).await
        .with_context(|| format!("failed to write MCP config: {}", path.display()))?;

    Ok(())
}

/// Delete a MCP config file. Returns true if the file existed and was deleted.
pub async fn delete_mcp_entry(mcp_dir: &str, name: &str) -> Result<bool> {
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        anyhow::bail!("invalid MCP entry name: '{name}'");
    }
    let path = Path::new(mcp_dir).join(format!("{name}.yaml"));
    if !path.exists() {
        return Ok(false);
    }
    tokio::fs::remove_file(&path).await
        .with_context(|| format!("failed to delete MCP config: {}", path.display()))?;
    Ok(true)
}
