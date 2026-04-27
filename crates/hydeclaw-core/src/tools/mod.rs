pub mod content_security;
pub mod embedding;
pub mod mcp_workspace;
pub mod semantic_cache;
pub mod service_registry;
pub mod ssrf;
pub mod yaml_tools;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

use crate::config::ToolConfig;

#[derive(Clone)]
pub struct ToolRegistry {
    tools: Arc<tokio::sync::RwLock<HashMap<String, ToolEntry>>>,
    client: reqwest::Client,
}

struct ToolEntry {
    pub url: String,
    pub tool_type: String,
    pub max_concurrent: u32,
    pub semaphore: Arc<Semaphore>,
    pub available: AtomicBool,
    pub healthcheck: Option<String>,
    pub depends_on: Vec<String>,
    pub ui_path: Option<String>,
}

impl ToolRegistry {
    fn build_map(tools: &HashMap<String, ToolConfig>) -> HashMap<String, ToolEntry> {
        let mut map = HashMap::new();
        for (name, cfg) in tools {
            let entry = ToolEntry {
                url: cfg.url.clone(),
                tool_type: cfg.tool_type.clone(),
                max_concurrent: cfg.max_concurrent,
                semaphore: Arc::new(Semaphore::new(cfg.max_concurrent as usize)),
                available: AtomicBool::new(true),
                healthcheck: cfg.healthcheck.clone(),
                depends_on: cfg.depends_on.clone(),
                ui_path: cfg.ui_path.clone(),
            };
            map.insert(name.clone(), entry);
            tracing::info!(tool = %name, url = %cfg.url, max = cfg.max_concurrent, "registered tool");
        }
        map
    }

    /// Create an empty registry with no tools registered.
    /// Used in tests and as a placeholder before config is loaded.
    #[cfg(test)]
    pub fn empty() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            tools: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            client,
        }
    }

    pub fn from_config(tools: &HashMap<String, ToolConfig>) -> Self {
        let map = Self::build_map(tools);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        Self {
            tools: Arc::new(tokio::sync::RwLock::new(map)),
            client,
        }
    }

    /// Reload registry from config, preserving health status for unchanged tools.
    pub async fn reload(&self, tools: &HashMap<String, ToolConfig>) {
        let new_map = Self::build_map(tools);
        let mut guard = self.tools.write().await;
        // Preserve health status for tools that still exist with same URL
        for (name, new_entry) in &new_map {
            if let Some(old) = guard.get(name)
                && old.url == new_entry.url {
                    new_entry.available.store(
                        old.available.load(Ordering::Relaxed),
                        Ordering::Relaxed,
                    );
                }
        }
        *guard = new_map;
    }

    pub async fn len(&self) -> usize {
        self.tools.read().await.len()
    }

    /// Call a tool with concurrency control via semaphore.
    pub async fn call(
        &self,
        name: &str,
        input: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        let (url, semaphore, available) = {
            let guard = self.tools.read().await;
            let tool = guard
                .get(name)
                .ok_or_else(|| anyhow::anyhow!("tool not found: {name}"))?;
            if !tool.available.load(Ordering::Relaxed) {
                anyhow::bail!("tool unavailable: {name}");
            }
            (tool.url.clone(), tool.semaphore.clone(), true)
        };

        if !available {
            anyhow::bail!("tool unavailable: {name}");
        }

        let _permit = semaphore.acquire().await?;
        tracing::debug!(tool = %name, "acquired semaphore permit");

        let resp = self
            .client
            .post(&url)
            .json(input)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("tool {name} returned {status}: {body}");
        }

        let result: serde_json::Value = resp.json().await?;
        Ok(result)
    }

    /// Returns all registered tool entries for API listing.
    pub async fn entries(&self) -> Vec<serde_json::Value> {
        let guard = self.tools.read().await;
        guard.iter().map(|(name, tool)| {
            serde_json::json!({
                "name": name,
                "tool_type": tool.tool_type,
                "url": tool.url,
                "concurrency_limit": tool.max_concurrent,
                "healthy": tool.available.load(Ordering::Relaxed),
                "healthcheck": tool.healthcheck,
                "depends_on": tool.depends_on,
                "ui_path": tool.ui_path,
            })
        }).collect()
    }

    /// Periodic health check for external tools.
    pub async fn health_check(&self) {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap_or_default();

        let guard = self.tools.read().await;
        for (name, tool) in guard.iter() {
            let check_url = match &tool.healthcheck {
                Some(path) => format!("{}{}", tool.url, path),
                None => continue,
            };

            let was_available = tool.available.load(Ordering::Relaxed);
            let is_available = client.get(&check_url).send().await.is_ok();
            tool.available.store(is_available, Ordering::Relaxed);

            if was_available && !is_available {
                tracing::warn!(tool = %name, "tool became unavailable");
            } else if !was_available && is_available {
                tracing::info!(tool = %name, "tool is available again");
            }
        }
    }
}
