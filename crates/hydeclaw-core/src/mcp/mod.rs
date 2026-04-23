use anyhow::Result;
use hydeclaw_types::ToolDefinition;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::containers::ContainerManager;

/// Manages MCP discovery and tool call routing.
pub struct McpRegistry {
    container_manager: Arc<ContainerManager>,
    /// Cached tool definitions from MCP servers (`mcp_name` → tools).
    tool_cache: Arc<RwLock<HashMap<String, Vec<ToolDefinition>>>>,
    /// Shared HTTP client with timeouts for MCP calls.
    http_client: reqwest::Client,
}

impl McpRegistry {
    pub fn new(container_manager: Arc<ContainerManager>) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self {
            container_manager,
            tool_cache: Arc::new(RwLock::new(HashMap::new())),
            http_client,
        }
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

        // Cache the result
        self.tool_cache
            .write()
            .await
            .insert(mcp_name.to_string(), tools.clone());

        tracing::info!(mcp = %mcp_name, tools = tools.len(), "discovered MCP tools");
        Ok(tools)
    }

    /// Call an MCP server tool via MCP tools/call.
    pub async fn call_tool(
        &self,
        mcp_name: &str,
        tool_name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String> {
        let base_url = self.container_manager.ensure_running(mcp_name).await?;

        let resp = self
            .http_client
            .post(format!("{base_url}/mcp"))
            .header("Accept", "application/json, text/event-stream")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": arguments
                },
                "id": 2
            }))
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("MCP tools/call failed for {mcp_name}/{tool_name}: {status} {body}");
        }

        let body = parse_mcp_response(resp).await?;

        // Extract text content from MCP response
        let content = body
            .pointer("/result/content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("");

        Ok(content.to_string())
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
