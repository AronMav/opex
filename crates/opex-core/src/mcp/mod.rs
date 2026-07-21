use anyhow::Result;
use opex_types::ToolDefinition;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

use crate::containers::ContainerManager;

mod path_rewrite;

/// Validates an MCP name: only `[a-zA-Z0-9_-]+` characters allowed.
fn is_valid_mcp_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Best-effort canonicalization. Falls back to the absolute, non-canonical
/// path if the filesystem entry does not yet exist (e.g. during tests or before
/// the workspace dir is created).
fn canonicalize_or_abs(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Manages MCP discovery and tool call routing.
pub struct McpRegistry {
    container_manager: Option<Arc<ContainerManager>>,
    /// Cached tool definitions from MCP servers (`mcp_name` → tools).
    tool_cache: Arc<RwLock<HashMap<String, Vec<ToolDefinition>>>>,
    /// Shared HTTP client with timeouts for MCP calls.
    http_client: reqwest::Client,
    /// Directory for persisting per-MCP tool definition caches.
    cache_dir: PathBuf,
    /// Host workspace root, canonicalized. Used to rewrite host paths sent to
    /// filesystem/git MCP containers into their container mount points.
    workspace_dir: PathBuf,
    /// Optional host source-tree root (e.g. `~/opex-src`), canonicalized. Mapped
    /// to `/src` inside the `mcp-git` container so git tools can inspect the
    /// deploy source repo without exposing arbitrary host paths.
    source_mount_dir: Option<PathBuf>,
}

impl McpRegistry {
    #[cfg(test)]
    pub fn new(
        container_manager: Option<Arc<ContainerManager>>,
        cache_dir: impl Into<PathBuf>,
        workspace_dir: impl AsRef<Path>,
    ) -> Self {
        Self::with_optional_source_dir(container_manager, cache_dir, workspace_dir, None::<PathBuf>)
    }

    /// Constructor with an optional source-tree mount. Pass `None` when the
    /// deploy source tree is not available — path rewriting for `/src` is
    /// then disabled (only `/workspace` rewriting remains active).
    pub fn with_optional_source_dir<S: AsRef<Path>>(
        container_manager: Option<Arc<ContainerManager>>,
        cache_dir: impl Into<PathBuf>,
        workspace_dir: impl AsRef<Path>,
        source_mount_dir: Option<S>,
    ) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        let workspace_dir = canonicalize_or_abs(workspace_dir.as_ref());
        let source_mount_dir = source_mount_dir.map(|p| canonicalize_or_abs(p.as_ref()));
        Self {
            container_manager,
            tool_cache: Arc::new(RwLock::new(HashMap::new())),
            http_client,
            cache_dir: cache_dir.into(),
            workspace_dir,
            source_mount_dir,
        }
    }

    /// Test-only constructor that lets callers inject a pre-built `reqwest::Client`.
    ///
    /// Used by tests to set short connect timeouts so transport-error retries finish quickly,
    /// and to verify the retry loop without requiring a live Docker daemon.
    #[cfg(test)]
    pub(crate) fn with_http_client(
        container_manager: Option<Arc<ContainerManager>>,
        cache_dir: impl Into<PathBuf>,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            container_manager,
            tool_cache: Arc::new(RwLock::new(HashMap::new())),
            http_client,
            cache_dir: cache_dir.into(),
            workspace_dir: std::env::temp_dir(),
            source_mount_dir: None,
        }
    }

    fn rewrite_arguments(
        &self,
        mcp_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        path_rewrite::rewrite_tool_arguments(
            mcp_name,
            tool_name,
            arguments,
            &self.workspace_dir,
            self.source_mount_dir.as_deref(),
        )
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
        let cm = self.container_manager.as_ref().ok_or_else(|| {
            anyhow::anyhow!("MCP '{mcp_name}' requires Docker, which is unavailable")
        })?;
        let base_url = cm.ensure_running(mcp_name).await?;

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

        // Cache the result in memory (Bug 22: hard cap to prevent unbounded growth
        // from repeated add/remove cycles; evict the oldest entry when at limit).
        const TOOL_CACHE_MAX: usize = 256;
        {
            let mut cache = self.tool_cache.write().await;
            if cache.len() >= TOOL_CACHE_MAX
                && !cache.contains_key(mcp_name)
                && let Some(oldest) = cache.keys().next().cloned()
            {
                tracing::warn!(
                    evicted = %oldest,
                    cap = TOOL_CACHE_MAX,
                    "MCP tool cache at capacity, evicting oldest entry"
                );
                cache.remove(&oldest);
            }
            cache.insert(mcp_name.to_string(), tools.clone());
        }

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
        let cm = self.container_manager.as_ref().ok_or_else(|| {
            anyhow::anyhow!("MCP '{mcp_name}' requires Docker, which is unavailable")
        })?;
        let base_url = cm.ensure_running(mcp_name).await?;

        // OPEX injects an internal `_context` object (session_id, chat_id,
        // subagent_depth, etc.) into every tool call for routing/audit. MCP
        // servers validate their tool schemas strictly and reject unknown
        // parameters such as `_context` (observed with context7's
        // resolve-library-id / query-docs). Strip it before forwarding.
        let mut mcp_arguments = arguments.clone();
        if let Some(obj) = mcp_arguments.as_object_mut() {
            obj.remove("_context");
        }

        // Filesystem/git MCP containers only see their container mounts
        // (`/workspace`, `/src`). Host paths like `/home/aronmav/opex/...`
        // or relative paths resolved against `/bridge` must be rewritten
        // before forwarding, otherwise every call is rejected with
        // "Access denied - path outside allowed directories" or similar.
        mcp_arguments = self.rewrite_arguments(mcp_name, tool_name, &mcp_arguments)?;

        // Retry delays for the startup gap: 300ms → 700ms → 1500ms
        const RETRY_DELAYS_MS: [u64; 3] = [300, 700, 1500];
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": { "name": tool_name, "arguments": mcp_arguments },
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
                    // Check for JSON-RPC application-level error (HTTP 200 but
                    // `{"error":{"code":...,"message":"..."}}` — no `result`).
                    if let Some(err) = body.get("error") {
                        let msg = err
                            .get("message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("unknown MCP error");
                        anyhow::bail!("MCP tool error: {msg}");
                    }
                    // Join ALL text blocks: taking only the first silently dropped
                    // the rest of a multi-block MCP response.
                    let content = body
                        .pointer("/result/content")
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|item| item.get("text").and_then(|t| t.as_str()))
                                .collect::<Vec<_>>()
                                .join("\n")
                        })
                        .unwrap_or_default();
                    // MCP tool-level failure: HTTP 200 + result.isError=true, error
                    // text in content (e.g. GitPython NoSuchPathError renders as the
                    // bare repo path). Masking it as success fed agents error strings
                    // as if they were tool output — surface it as a tool error.
                    let is_error = body
                        .pointer("/result/isError")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if is_error {
                        anyhow::bail!("MCP tool error: {content}");
                    }
                    return Ok(content);
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

    /// Find which MCP provides a given tool name.
    pub async fn find_mcp_for_tool(&self, tool_name: &str) -> Option<String> {
        let cache = self.tool_cache.read().await;
        // F040: `tool_cache` is a HashMap, so its iteration order is
        // nondeterministic — when two MCP servers expose the same tool name
        // (`search`/`fetch`/… are common), the old first-match returned a
        // random, restart-varying server, dispatching the call to the wrong
        // one. Collect all matches, pick deterministically (lexicographically
        // smallest mcp name), and warn so the collision is visible.
        let mut matches: Vec<&String> = cache
            .iter()
            .filter(|(_, tools)| tools.iter().any(|t| t.name == tool_name))
            .map(|(mcp_name, _)| mcp_name)
            .collect();
        matches.sort();
        if matches.len() > 1 {
            tracing::warn!(
                tool = %tool_name,
                servers = ?matches,
                "MCP tool-name collision — routing deterministically to the first; namespace tool names to disambiguate"
            );
        }
        matches.first().map(|s| (*s).clone())
    }

    /// Clear cached tools for an MCP server.
    pub async fn invalidate_mcp_cache(&self, mcp_name: &str) {
        self.tool_cache.write().await.remove(mcp_name);
    }

    /// Force re-discover tools for an MCP server (invalidate cache + discover).
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

    // F041: cap the buffered MCP body so a large/runaway response can't OOM the
    // single opex-core process. Stream with a hard byte limit (robust against a
    // chunked response that advertises no content-length).
    const MAX_MCP_RESPONSE_BYTES: usize = 10 * 1024 * 1024; // 10 MB
    if let Some(len) = resp.content_length()
        && len > MAX_MCP_RESPONSE_BYTES as u64
    {
        anyhow::bail!("MCP response too large: {len} bytes (cap {MAX_MCP_RESPONSE_BYTES})");
    }
    let text = {
        use futures_util::StreamExt as _;
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if buf.len() + chunk.len() > MAX_MCP_RESPONSE_BYTES {
                anyhow::bail!("MCP response exceeded {MAX_MCP_RESPONSE_BYTES} byte cap");
            }
            buf.extend_from_slice(&chunk);
        }
        String::from_utf8_lossy(&buf).into_owned()
    };

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
        McpRegistry::new(Some(cm), cache_dir, std::env::temp_dir())
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

    // ── call_tool retry-loop tests ─────────────────────────────────────────────

    use crate::config::McpConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build a McpConfig with `url` set (URL-based MCP — skips Docker entirely).
    fn url_mcp(url: &str) -> McpConfig {
        McpConfig {
            url: Some(url.to_string()),
            container: None,
            port: None,
            mode: "on-demand".to_string(),
            idle_timeout: Some("5m".to_string()),
            protocol: "mcp".to_string(),
            enabled: true,
        }
    }

    /// Build a registry pre-registered with a URL-based MCP entry.
    /// Uses a fast http_client with short timeouts so retry tests finish quickly.
    async fn registry_with_url_mcp(name: &str, url: &str) -> (McpRegistry, Arc<crate::containers::ContainerManager>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cm = Arc::new(
            crate::containers::ContainerManager::new("http://127.0.0.1:1", HashMap::new())
                .expect("ContainerManager::new"),
        );
        cm.add_or_update_mcp(name.to_string(), url_mcp(url)).await;

        // Fast http client: short connect/total timeouts so retry tests finish quickly.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(200))
            .timeout(Duration::from_secs(2))
            .build()
            .expect("client");

        // Forget the tempdir — it will be cleaned at process exit.
        let dir_path = dir.path().to_path_buf();
        std::mem::forget(dir);
        let reg = McpRegistry::with_http_client(Some(cm.clone()), dir_path, client);
        (reg, cm)
    }

    /// Standard MCP tools/call success response body.
    fn mcp_ok_body(text: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{"type": "text", "text": text}]
            }
        })
    }

    /// Pick a port that is NOT listening by binding then immediately dropping.
    async fn pick_closed_port() -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral");
        let p = listener.local_addr().expect("addr").port();
        drop(listener);
        p
    }

    /// Spawn a TCP server on an ephemeral port that:
    /// - Drops the first `drop_count` connections immediately after accept
    ///   (causes `is_request()` / `is_connect()` on the client side).
    /// - For subsequent connections, serves a valid HTTP 200 response with `body`.
    ///
    /// Returns the server URL (`http://127.0.0.1:<port>`).
    async fn flaky_tcp_server(drop_count: usize, body: serde_json::Value) -> String {
        use tokio::io::AsyncWriteExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind flaky server");
        let port = listener.local_addr().expect("addr").port();

        let body_str = serde_json::to_string(&body).expect("json");
        tokio::spawn(async move {
            let mut drops_remaining = drop_count;
            while let Ok((mut stream, _)) = listener.accept().await {
                if drops_remaining > 0 {
                    // Drop the stream immediately — client gets connection reset.
                    drops_remaining -= 1;
                    drop(stream);
                } else {
                    // Drain the request bytes then respond with HTTP 200.
                    let mut buf = vec![0u8; 4096];
                    // Read until we see end of HTTP headers (double CRLF).
                    // On Windows, recv may return short; loop a few times.
                    let mut raw = Vec::new();
                    for _ in 0..20 {
                        use tokio::io::AsyncReadExt;
                        match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                raw.extend_from_slice(&buf[..n]);
                                if raw.windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                            }
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body_str.len(),
                        body_str
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.flush().await;
                }
            }
        });

        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn call_tool_surfaces_result_is_error_as_tool_error() {
        // Regression: mcp-server-git returns HTTP 200 + result.isError=true with
        // the exception text in content (NoSuchPathError renders as the bare
        // path). call_tool previously ignored isError and returned the error
        // string as SUCCESSFUL output — agents saw "/workspace/zettelkasten"
        // as the "result" of git_status/git_add/git_commit.
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [{"type": "text", "text": "/workspace/zettelkasten"}],
                "isError": true
            }
        });
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let (reg, _cm) = registry_with_url_mcp("test-mcp", &server.uri()).await;

        let err = reg
            .call_tool("test-mcp", "git_status", &serde_json::json!({}))
            .await
            .expect_err("result.isError=true must surface as Err");
        let msg = err.to_string();
        assert!(msg.contains("MCP tool error"), "{msg}");
        assert!(msg.contains("/workspace/zettelkasten"), "{msg}");
    }

    #[tokio::test]
    async fn call_tool_joins_all_content_blocks() {
        // Multi-block responses previously lost everything after content[0].
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "content": [
                    {"type": "text", "text": "block one"},
                    {"type": "image", "data": "ignored-no-text-field"},
                    {"type": "text", "text": "block two"}
                ]
            }
        });
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let (reg, _cm) = registry_with_url_mcp("test-mcp", &server.uri()).await;

        let result = reg
            .call_tool("test-mcp", "any", &serde_json::json!({}))
            .await
            .expect("multi-block success");
        assert_eq!(result, "block one\nblock two");
    }

    #[tokio::test]
    async fn call_tool_succeeds_on_first_attempt_when_server_returns_200() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mcp_ok_body("hello")))
            .mount(&server)
            .await;
        let (reg, _cm) = registry_with_url_mcp("test-mcp", &server.uri()).await;

        let result = reg
            .call_tool("test-mcp", "any", &serde_json::json!({}))
            .await
            .expect("call_tool should succeed on 200");
        assert_eq!(result, "hello");

        let received = server.received_requests().await.expect("requests");
        assert_eq!(received.len(), 1, "expected exactly 1 request (no retries), got {}", received.len());
    }

    #[tokio::test]
    async fn call_tool_retries_on_connect_error_then_succeeds() {
        // Spawn a TCP server that drops the first 1 connection then serves HTTP 200.
        // The client will see a connection-reset / broken-pipe error on attempt 0,
        // then succeed on attempt 1 (after the 300ms retry delay).
        let server_url = flaky_tcp_server(1, mcp_ok_body("recovered")).await;
        let (reg, _cm) = registry_with_url_mcp("flaky-mcp", &server_url).await;

        let result = reg
            .call_tool("flaky-mcp", "any", &serde_json::json!({}))
            .await;
        let body = result.expect("should eventually succeed after retry");
        assert_eq!(body, "recovered");
    }

    #[tokio::test]
    async fn call_tool_returns_err_after_all_retries_exhausted() {
        let dead_port = pick_closed_port().await;
        let dead_url = format!("http://127.0.0.1:{dead_port}");
        let (reg, _cm) = registry_with_url_mcp("dead-mcp", &dead_url).await;

        let start = std::time::Instant::now();
        let result = reg.call_tool("dead-mcp", "any", &serde_json::json!({})).await;
        let elapsed = start.elapsed();

        assert!(result.is_err(), "expected Err after all retries exhausted");
        // Retry delays: 0 + 300 + 700 + 1500 = 2500ms minimum (plus connect attempts ~200ms each).
        // Cap at 6s to catch regressions while leaving headroom for slow CI.
        assert!(
            elapsed < Duration::from_secs(6),
            "test took too long: {elapsed:?} — retry loop may be unbounded"
        );
        assert!(
            elapsed >= Duration::from_millis(2400),
            "test finished too fast — retries may not be running: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn call_tool_does_not_retry_on_http_4xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .mount(&server)
            .await;
        let (reg, _cm) = registry_with_url_mcp("err-mcp", &server.uri()).await;

        let err = reg
            .call_tool("err-mcp", "any", &serde_json::json!({}))
            .await
            .expect_err("400 should error immediately");
        assert!(
            err.to_string().contains("400"),
            "error should mention 400 status: {err}"
        );

        let received = server.received_requests().await.expect("requests");
        assert_eq!(
            received.len(),
            1,
            "4xx must not trigger retries — got {} requests",
            received.len()
        );
    }

    #[tokio::test]
    async fn call_tool_does_not_retry_on_http_5xx_either() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&server)
            .await;
        let (reg, _cm) = registry_with_url_mcp("five-mcp", &server.uri()).await;

        let err = reg
            .call_tool("five-mcp", "any", &serde_json::json!({}))
            .await
            .expect_err("500 should error immediately (no retry on HTTP errors)");
        assert!(err.to_string().contains("500"), "error should mention 500: {err}");

        let received = server.received_requests().await.expect("requests");
        assert_eq!(
            received.len(),
            1,
            "5xx must not trigger retries (transport-only retry policy) — got {} requests",
            received.len()
        );
    }

    /// C1: HTTP 200 with JSON-RPC error body must surface as Err, not Ok("").
    #[tokio::test]
    async fn call_tool_surfaces_jsonrpc_error_as_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "jsonrpc": "2.0",
                "error": {"code": -32000, "message": "boom"},
                "id": 2
            })))
            .mount(&server)
            .await;
        let (reg, _cm) = registry_with_url_mcp("err-body-mcp", &server.uri()).await;

        let err = reg
            .call_tool("err-body-mcp", "any", &serde_json::json!({}))
            .await
            .expect_err("JSON-RPC error body on HTTP 200 must yield Err");
        assert!(
            err.to_string().contains("boom"),
            "error message should propagate: {err}"
        );
    }
}
