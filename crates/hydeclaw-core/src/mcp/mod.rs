use anyhow::Result;
use hydeclaw_types::ToolDefinition;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::containers::ContainerManager;

/// Validates an MCP name: only `[a-zA-Z0-9_-]+` characters allowed.
fn is_valid_mcp_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Manages MCP discovery and tool call routing.
pub struct McpRegistry {
    container_manager: Arc<ContainerManager>,
    /// Cached tool definitions from MCP servers (`mcp_name` → tools).
    tool_cache: Arc<RwLock<HashMap<String, Vec<ToolDefinition>>>>,
    /// Shared HTTP client with timeouts for MCP calls.
    http_client: reqwest::Client,
    /// Directory for persisting per-MCP tool definition caches.
    cache_dir: PathBuf,
}

impl McpRegistry {
    pub fn new(container_manager: Arc<ContainerManager>, cache_dir: impl Into<PathBuf>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            container_manager,
            tool_cache: Arc::new(RwLock::new(HashMap::new())),
            http_client,
            cache_dir: cache_dir.into(),
        }
    }

    // ── File-cache helpers ──────────────────────────────────────────────────────

    /// Load cached tool definitions from disk into `tool_cache`.
    ///
    /// Only entries whose file stem is in `enabled_mcps` are loaded;
    /// stale files for deleted/disabled MCPs are silently skipped (not deleted).
    /// Corrupt JSON files are warned and skipped — startup is never aborted.
    ///
    /// Returns the number of MCP entries successfully loaded.
    pub async fn load_cached_tools_from_disk(
        &self,
        enabled_mcps: &HashSet<String>,
    ) -> Result<usize> {
        if !self.cache_dir.exists() {
            return Ok(0);
        }

        let mut dir = tokio::fs::read_dir(&self.cache_dir).await?;
        let mut loaded = 0usize;

        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();

            // Only process .json files
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            // Skip files whose stem is not in the enabled MCPs set
            if !enabled_mcps.contains(&stem) {
                continue;
            }

            let contents = match tokio::fs::read(&path).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read MCP cache file");
                    continue;
                }
            };

            match serde_json::from_slice::<Vec<ToolDefinition>>(&contents) {
                Ok(tools) => {
                    let count = tools.len();
                    self.tool_cache.write().await.insert(stem.clone(), tools);
                    tracing::debug!(mcp = %stem, tools = count, "loaded MCP tools from disk cache");
                    loaded += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "MCP cache file has invalid JSON, skipping"
                    );
                }
            }
        }

        Ok(loaded)
    }

    /// Persist tool definitions for an MCP server to `cache_dir/{name}.json`.
    ///
    /// Best-effort: errors are logged but not propagated — callers must not fail on this.
    async fn persist_cache_to_disk(&self, mcp_name: &str, tools: &[ToolDefinition]) -> Result<()> {
        if !is_valid_mcp_name(mcp_name) {
            anyhow::bail!("invalid MCP name for cache write: '{mcp_name}'");
        }
        tokio::fs::create_dir_all(&self.cache_dir).await?;
        let path = self.cache_dir.join(format!("{mcp_name}.json"));
        let bytes = serde_json::to_vec_pretty(tools)?;
        tokio::fs::write(&path, bytes).await?;
        tracing::debug!(mcp = %mcp_name, path = %path.display(), "persisted MCP tool cache to disk");
        Ok(())
    }

    /// Discover tools from an MCP server via MCP tools/list.
    pub async fn discover_tools(&self, mcp_name: &str) -> Result<Vec<ToolDefinition>> {
        // Check cache first
        {
            let cache = self.tool_cache.read().await;
            if let Some(tools) = cache.get(mcp_name) {
                return Ok(tools.clone());
            }
        }

        // Ensure container is running
        let base_url = self.container_manager.ensure_running(mcp_name).await?;

        // MCP tools/list via JSON-RPC 2.0 (with timeout).
        // Accept header required by MCP Streamable HTTP transport (RFC-compliant servers
        // return 406 if client doesn't advertise text/event-stream support).
        let resp = self
            .http_client
            .post(format!("{base_url}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "tools/list",
                "id": 1
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("MCP tools/list failed for MCP '{mcp_name}'");
        }

        let body = parse_mcp_response(resp).await?;
        let tools = parse_mcp_tools(&body)?;

        // Cache the result in memory
        self.tool_cache
            .write()
            .await
            .insert(mcp_name.to_string(), tools.clone());

        // Persist to disk (best-effort)
        if let Err(e) = self.persist_cache_to_disk(mcp_name, &tools).await {
            tracing::warn!(mcp = %mcp_name, error = %e, "failed to persist MCP tool cache to disk");
        }

        tracing::info!(mcp = %mcp_name, tools = tools.len(), "discovered MCP tools");
        Ok(tools)
    }

    /// Call an MCP server tool via MCP tools/call.
    ///
    /// Retries on connection/timeout errors — the MCP HTTP server inside the container
    /// may not be ready to accept JSON-RPC requests immediately after the Docker
    /// healthcheck passes (healthcheck hits `/health`, MCP endpoint is `/mcp`).
    pub async fn call_tool(
        &self,
        mcp_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String> {
        let base_url = self.container_manager.ensure_running(mcp_name).await?;

        // Retry delays for the startup gap: 300ms → 700ms → 1500ms
        const RETRY_DELAYS_MS: [u64; 3] = [300, 700, 1500];
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": tool_name, "arguments": arguments },
            "id": 2
        });

        let mut last_err: Option<anyhow::Error> = None;
        for (attempt, &delay_ms) in std::iter::once(&0u64)
            .chain(RETRY_DELAYS_MS.iter())
            .enumerate()
        {
            if delay_ms > 0 {
                tracing::debug!(
                    mcp = %mcp_name,
                    tool = %tool_name,
                    attempt,
                    delay_ms,
                    "retrying MCP call after container startup"
                );
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }

            let result = self
                .http_client
                .post(format!("{base_url}/mcp"))
                .header("Accept", "application/json, text/event-stream")
                .json(&payload)
                .send()
                .await;

            match result {
                // Retry on any transport-layer error — container may need a moment
                // after the TCP port opens before the HTTP server handles requests.
                Err(e) if e.is_connect() || e.is_timeout() || e.is_request() => {
                    last_err = Some(e.into());
                    continue;
                }
                Err(e) => return Err(e.into()),
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let body = resp.text().await.unwrap_or_default();
                        anyhow::bail!(
                            "MCP tools/call failed for {mcp_name}/{tool_name}: {status} {body}"
                        );
                    }
                    let body = parse_mcp_response(resp).await?;
                    let content = body
                        .pointer("/result/content")
                        .and_then(|c| c.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|item| item.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("");
                    return Ok(content.to_string());
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("MCP call failed: no attempts made")))
    }

    /// Get all discovered tool definitions (for LLM system prompt).
    pub async fn all_tool_definitions(&self) -> Vec<ToolDefinition> {
        let cache = self.tool_cache.read().await;
        cache.values().flatten().cloned().collect()
    }

    /// Check if an MCP server is configured in the container manager.
    #[allow(dead_code)]
    pub async fn has_mcp(&self, name: &str) -> bool {
        self.container_manager.has_mcp(name).await
    }

    /// Find which MCP provides a given tool name.
    pub async fn find_mcp_for_tool(&self, tool_name: &str) -> Option<String> {
        let cache = self.tool_cache.read().await;
        for (mcp_name, tools) in cache.iter() {
            if tools.iter().any(|t| t.name == tool_name) {
                return Some(mcp_name.clone());
            }
        }
        None
    }

    /// Load MCP.md from an MCP server's workspace directory for additional context.
    #[allow(dead_code)] // Available for future MCP-level prompt injection
    pub async fn load_mcp_prompt(&self, mcp_name: &str, skills_dir: &str) -> Option<String> {
        let path = std::path::Path::new(skills_dir)
            .join(mcp_name)
            .join("MCP.md");
        tokio::fs::read_to_string(&path).await.ok()
    }

    /// Clear cached tools for an MCP server.
    #[allow(dead_code)]
    pub async fn invalidate_mcp_cache(&self, mcp_name: &str) {
        self.tool_cache.write().await.remove(mcp_name);
    }

    /// Force re-discover tools for an MCP server (invalidate cache + discover).
    #[allow(dead_code)]
    pub async fn reload_mcp(&self, mcp_name: &str) -> Result<Vec<ToolDefinition>> {
        self.invalidate_mcp_cache(mcp_name).await;
        self.discover_tools(mcp_name).await
    }
}

/// Parse MCP HTTP response body, handling both JSON and SSE (text/event-stream) transports.
///
/// Some servers (e.g. `DeepWiki`) always respond with SSE even when Accept includes
/// application/json. SSE lines look like:
///   event: message
///   data: {"jsonrpc":"2.0","id":1,"result":{...}}
///
/// We concatenate all `data:` payloads and parse the last complete JSON object.
async fn parse_mcp_response(resp: reqwest::Response) -> Result<serde_json::Value> {
    let is_sse = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));

    let text = resp.text().await?;

    if !is_sse {
        return serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("MCP JSON parse: {e}"));
    }

    // Extract `data:` lines from SSE stream and return the last non-empty JSON payload
    let payload = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .rfind(|s| !s.is_empty() && *s != "[DONE]")
        .ok_or_else(|| anyhow::anyhow!("empty SSE stream from MCP server"))?;

    serde_json::from_str(payload).map_err(|e| anyhow::anyhow!("MCP SSE JSON parse: {e}"))
}

/// Parse MCP tools/list response into ToolDefinitions.
#[cfg(test)]
pub(crate) fn parse_mcp_tools_for_test(body: &serde_json::Value) -> Result<Vec<ToolDefinition>> {
    parse_mcp_tools(body)
}

fn parse_mcp_tools(body: &serde_json::Value) -> Result<Vec<ToolDefinition>> {
    let tools_arr = body
        .pointer("/result/tools")
        .and_then(|t| t.as_array())
        .ok_or_else(|| anyhow::anyhow!("invalid MCP tools/list response"))?;

    let mut definitions = Vec::with_capacity(tools_arr.len());
    for tool in tools_arr {
        let name = tool
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let description = tool
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("")
            .to_string();
        let input_schema = tool
            .get("inputSchema")
            .cloned()
            .unwrap_or(serde_json::json!({"type": "object"}));

        if !name.is_empty() {
            definitions.push(ToolDefinition {
                name,
                description,
                input_schema,
            });
        }
    }

    Ok(definitions)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Convenience alias so tests don't need to call through the wrapper.
    fn parse(body: &serde_json::Value) -> Result<Vec<ToolDefinition>> {
        parse_mcp_tools_for_test(body)
    }

    /// Build a test registry pointing at `cache_dir`. Uses a fake Docker URL —
    /// the ContainerManager constructor does not perform I/O until `ensure_running` is called.
    fn registry_with_cache_dir(cache_dir: std::path::PathBuf) -> McpRegistry {
        let cm = Arc::new(
            crate::containers::ContainerManager::new("http://127.0.0.1:1", HashMap::new())
                .expect("ContainerManager::new should succeed for test"),
        );
        McpRegistry::new(cm, cache_dir)
    }

    fn make_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "search".to_string(),
                description: "Search the web".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
            ToolDefinition {
                name: "fetch".to_string(),
                description: "Fetch a URL".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            },
        ]
    }

    #[tokio::test]
    async fn cache_roundtrip_persists_and_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = registry_with_cache_dir(dir.path().to_path_buf());

        // Persist "fake-mcp" tools to disk
        reg.persist_cache_to_disk("fake-mcp", &make_tools())
            .await
            .expect("persist should succeed");

        // Load from disk filtering to only "fake-mcp"
        let enabled: HashSet<String> = ["fake-mcp".to_string()].into_iter().collect();
        let count = reg
            .load_cached_tools_from_disk(&enabled)
            .await
            .expect("load should succeed");

        assert_eq!(count, 1, "expected 1 MCP loaded");
        let all = reg.all_tool_definitions().await;
        assert_eq!(all.len(), 2, "expected 2 tool definitions");
    }

    #[tokio::test]
    async fn load_cache_filters_stale_entries() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Write two JSON files: one for an enabled MCP, one stale unknown
        let tools = make_tools();
        let bytes = serde_json::to_vec_pretty(&tools).expect("json");
        tokio::fs::write(dir.path().join("fake-mcp.json"), &bytes)
            .await
            .expect("write fake-mcp.json");
        tokio::fs::write(dir.path().join("unknown.json"), &bytes)
            .await
            .expect("write unknown.json");

        let reg = registry_with_cache_dir(dir.path().to_path_buf());
        let enabled: HashSet<String> = ["fake-mcp".to_string()].into_iter().collect();
        let count = reg
            .load_cached_tools_from_disk(&enabled)
            .await
            .expect("load should succeed");

        assert_eq!(count, 1, "only fake-mcp should load; unknown should be filtered");

        let cache = reg.tool_cache.read().await;
        assert!(cache.contains_key("fake-mcp"), "fake-mcp should be in cache");
        assert!(!cache.contains_key("unknown"), "unknown should NOT be in cache");
        // File must still exist (not deleted)
        assert!(dir.path().join("unknown.json").exists(), "stale file should not be deleted");
    }

    #[tokio::test]
    async fn load_cache_handles_corrupt_files_gracefully() {
        let dir = tempfile::tempdir().expect("tempdir");

        // Write an invalid JSON file
        tokio::fs::write(dir.path().join("bad.json"), b"this is not json {{{")
            .await
            .expect("write bad.json");

        let reg = registry_with_cache_dir(dir.path().to_path_buf());
        let enabled: HashSet<String> = ["bad".to_string()].into_iter().collect();

        // Should not panic or error — just return 0 loaded
        let count = reg
            .load_cached_tools_from_disk(&enabled)
            .await
            .expect("should return Ok even with corrupt file");

        assert_eq!(count, 0, "corrupt file should be skipped");
        let all = reg.all_tool_definitions().await;
        assert!(all.is_empty(), "no tools should be loaded from corrupt file");
    }

    #[tokio::test]
    async fn persist_cache_rejects_invalid_mcp_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reg = registry_with_cache_dir(dir.path().to_path_buf());

        // Path traversal attempt
        let result = reg
            .persist_cache_to_disk("../../etc/passwd", &make_tools())
            .await;
        assert!(result.is_err(), "path traversal in name should be rejected");
    }

    #[test]
    fn parse_valid_response_with_two_tools() {
        let body = serde_json::json!({
            "result": {
                "tools": [
                    {
                        "name": "search",
                        "description": "Search the web",
                        "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}
                    },
                    {
                        "name": "fetch",
                        "description": "Fetch a URL",
                        "inputSchema": {"type": "object"}
                    }
                ]
            }
        });
        let tools = parse(&body).expect("should succeed");
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description, "Search the web");
        assert_eq!(tools[1].name, "fetch");
    }

    #[test]
    fn parse_tool_with_empty_name_is_skipped() {
        let body = serde_json::json!({
            "result": {
                "tools": [
                    {"name": "", "description": "nameless"},
                    {"name": "valid-tool", "description": "has a name"}
                ]
            }
        });
        let tools = parse(&body).expect("should succeed");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "valid-tool");
    }

    #[test]
    fn parse_tool_missing_input_schema_gets_default() {
        let body = serde_json::json!({
            "result": {
                "tools": [
                    {"name": "no-schema", "description": "missing schema"}
                ]
            }
        });
        let tools = parse(&body).expect("should succeed");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].input_schema, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn parse_missing_result_tools_returns_err() {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1});
        assert!(parse(&body).is_err());
    }

    #[test]
    fn parse_empty_tools_array_returns_empty_vec() {
        let body = serde_json::json!({
            "result": {
                "tools": []
            }
        });
        let tools = parse(&body).expect("should succeed");
        assert!(tools.is_empty());
    }
}
