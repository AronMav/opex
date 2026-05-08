//! Pipeline step: subagent — subagent orchestration (migrated from engine_subagent.rs).
//!
//! Free functions extracted from `AgentEngine` methods in `engine_subagent.rs`.

use anyhow::Result;

use crate::agent::url_tools::{enrich_with_attachments, extract_urls};
use crate::config::DelegationConfig;

/// Tools denied to subagents by default (prevent recursive spawning, destructive operations, and dangerous ops).
/// workspace_write and workspace_edit are allowed so subagents can write shared state files (SUB-01).
///
/// `code_exec` is included even though Docker provides a sandbox, because:
///   1. parents are the only callers expected to run arbitrary code,
///   2. Setup wizard's auto-deny for non-base agents already lists code_exec
///      — subagents inherit that intent: dangerous-by-default, opt-in via
///      `[agent.delegation] blocked_tools_override = []` if needed.
pub const SUBAGENT_DENIED_TOOLS: &[&str] = &[
    "workspace_delete",
    "workspace_rename",
    "cron",
    "secret_set",
    "process",
    "code_exec",
];

/// Compute effective deny list for subagent tool filtering, given a delegation config.
///
/// Logic:
/// - If `blocked_tools_override` is non-empty: use it as the complete deny list
///   (replaces SUBAGENT_DENIED_TOOLS)
/// - Otherwise: SUBAGENT_DENIED_TOOLS + blocked_tools_extra (deduplicated)
///
/// **Deprecated for runtime use.** Audit 2026-05-08 (6th pass) found that
/// honouring `blocked_tools_override` in any runtime path (visibility list,
/// dispatcher rewrite) lets a subagent author weaken SUBAGENT_DENIED_TOOLS.
/// All runtime call sites now use `runtime_subagent_denylist` instead. This
/// function is retained only because its `blocked_tools_override` semantics
/// are tested in this module — if a future caller wants the
/// "override-respecting" semantics for an operator-facing view (not runtime),
/// they can still use it explicitly.
#[allow(dead_code)]
pub fn compute_denied_tools(cfg: &DelegationConfig) -> Vec<String> {
    if !cfg.blocked_tools_override.is_empty() {
        return cfg.blocked_tools_override.clone();
    }

    let mut denied: Vec<String> = SUBAGENT_DENIED_TOOLS.iter().map(|s| s.to_string()).collect();
    for extra in &cfg.blocked_tools_extra {
        if !denied.contains(extra) {
            denied.push(extra.clone());
        }
    }
    denied
}

/// Strict subagent runtime deny list — always SUBAGENT_DENIED_TOOLS, regardless
/// of what the subagent's own `[agent.delegation]` config says.
///
/// Audit 2026-05-08 (4th pass) found that the runner reached for the
/// SUBAGENT'S OWN `compute_denied_tools(&executor.cfg().agent.delegation)`,
/// which let a subagent author set `blocked_tools_override = ["x"]` to
/// effectively grant themselves every dangerous tool (cron, secret_set,
/// process, code_exec, …) — `blocked_tools_override` was meant for the
/// SPAWNING parent to apply restrictions, not for the subagent to weaken
/// its own. Until parent's delegation is plumbed through the spawn chain
/// (live agents in `session_agent_pool` only carry the subagent's engine
/// today), the runner-side deny list is hard-anchored to SUBAGENT_DENIED_TOOLS.
///
/// The subagent is still allowed to add its own additional restrictions via
/// `blocked_tools_extra` (more, never fewer).
pub fn runtime_subagent_denylist(cfg: &DelegationConfig) -> Vec<String> {
    let mut denied: Vec<String> = SUBAGENT_DENIED_TOOLS.iter().map(|s| s.to_string()).collect();
    for extra in &cfg.blocked_tools_extra {
        if !denied.contains(extra) {
            denied.push(extra.clone());
        }
    }
    denied
}

/// Parse a duration string like "2m", "30s" for subagent timeout.
/// Defaults to 2m (120s) on invalid input — matches the config default.
pub(crate) fn parse_subagent_timeout(s: &str) -> std::time::Duration {
    let s = s.trim();
    if let Some(mins) = s.strip_suffix('m')
        && let Ok(n) = mins.parse::<u64>() {
        return std::time::Duration::from_secs(n * 60);
    }
    if let Some(secs) = s.strip_suffix('s')
        && let Ok(n) = secs.parse::<u64>() {
        return std::time::Duration::from_secs(n);
    }
    std::time::Duration::from_secs(120) // default 2m
}

/// Delegate HTML readability extraction to toolgate's unified `POST /web` endpoint
/// (`mode: "read"`). Toolgate handles SSRF validation + 2 MiB body cap + readability
/// extraction via `readability-lxml`, returning `{title, content, url}`.
///
/// Returns `Ok("")` when both `title` and `content` are absent (e.g. paywalled page),
/// `Err` on network failure or non-2xx response. Never panics.
async fn fetch_via_toolgate_web(
    http_client: &reqwest::Client,
    toolgate_url: &str,
    url: &str,
    timeout_secs: u64,
) -> Result<String> {
    let endpoint = format!("{}/web", toolgate_url.trim_end_matches('/'));
    // Inject W3C traceparent so the toolgate /web span attaches to the
    // current Core parent. No-op without `otel`.
    let req = http_client
        .post(&endpoint)
        .json(&serde_json::json!({
            "url": url,
            "mode": "read",
            "timeout": timeout_secs,
        }))
        .timeout(std::time::Duration::from_secs(timeout_secs + 5));
    let req = crate::trace_propagation::inject_trace_context(req);
    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("toolgate /web request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("toolgate /web HTTP {status}: {body}");
    }

    #[derive(serde::Deserialize)]
    struct WebResp {
        title: Option<String>,
        content: Option<String>,
    }
    let parsed: WebResp = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("toolgate /web bad JSON: {e}"))?;

    let mut out = String::new();
    if let Some(t) = parsed.title.as_deref() {
        let t = t.trim();
        if !t.is_empty() {
            out.push_str(&format!("Title: {t}\n"));
        }
    }
    if let Some(c) = parsed.content {
        out.push_str(&c);
    }
    Ok(out)
}

/// Fetch URL content via toolgate `/web` (readability mode), truncate for LLM context.
/// Uses 10s timeout to avoid blocking message processing on slow URLs.
///
/// `gateway_listen` is the gateway listen address (e.g. "0.0.0.0:18789") used to
/// short-circuit Core API self-calls (`/api/doctor` etc.) — those bypass toolgate
/// and use the plain `http_client` directly.
///
/// SSRF + size-limit enforcement for external URLs is toolgate's responsibility
/// (`validate_url_ssrf` + `download_limited(max_bytes=2 MiB)` in `routers/fetch.py`).
pub async fn fetch_url_content(
    http_client: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    url: &str,
) -> Result<String> {
    // Only allow localhost on Core API port — block access to internal services.
    // Parse port from gateway listen address (e.g. "0.0.0.0:18789" → 18789)
    let core_port = gateway_listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(18789);
    let is_core_api = url.starts_with(&format!("http://localhost:{}", core_port))
        || url.starts_with(&format!("http://127.0.0.1:{}", core_port));

    // Core API self-call: bypass toolgate, fetch directly. Toolgate's SSRF guard
    // would reject the Pi's loopback anyway.
    let text = if is_core_api {
        let resp = http_client
            .get(url)
            .header("User-Agent", "HydeClaw/0.1 (link-preview)")
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }
        resp.text().await?
    } else {
        fetch_via_toolgate_web(http_client, toolgate_url, url, 10).await?
    };

    // Truncate to ~4000 bytes for LLM context (safe UTF-8 boundary)
    let truncated = if text.len() > 4000 {
        let boundary = text.floor_char_boundary(4000);
        format!(
            "{}...\n[truncated, {} characters total]",
            &text[..boundary],
            text.chars().count()
        )
    } else {
        text
    };

    Ok(crate::tools::content_security::wrap_external_content(
        &truncated,
        &format!("web_fetch:{}", url),
    ))
}

/// Enrich user text: auto-fetch URLs (max 2), add attachment descriptions.
pub async fn enrich_message_text(
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    agent_language: &str,
    user_text: &str,
    attachments: &[hydeclaw_types::MediaAttachment],
) -> String {
    let mut enriched = user_text.to_string();

    // PII redaction before sending to external LLM
    let (redacted, pii_count) = crate::agent::pii::redact(&enriched);
    if pii_count > 0 {
        tracing::info!(count = pii_count, "redacted PII from user message");
        enriched = redacted;
    }

    let urls: Vec<String> = extract_urls(user_text);
    for url in urls.iter().take(2) {
        match fetch_url_content(http_client, toolgate_url, gateway_listen, url).await {
            Ok(content) => {
                tracing::info!(url = %url, len = content.len(), "fetched URL content");
                enriched.push_str(&format!("\n\n[Content of URL {}]:\n{}", url, content));
            }
            Err(e) => {
                tracing::warn!(url = %url, error = %e, "failed to fetch URL");
            }
        }
    }
    enrich_with_attachments(&mut enriched, attachments);

    // Auto-transcribe voice messages via toolgate STT
    crate::agent::url_tools::auto_transcribe_audio(
        &mut enriched, attachments, toolgate_url, agent_language, http_client, gateway_listen,
    ).await;

    // Auto-describe images via toolgate vision
    crate::agent::url_tools::auto_describe_images(
        &mut enriched, attachments, toolgate_url, agent_language, http_client, gateway_listen,
    ).await;

    enriched
}

/// Fetch a URL and return text content (tool handler).
///
/// Delegates HTML readability extraction to toolgate `POST /web` (mode=read) for
/// all non-core-api URLs. Toolgate handles SSRF validation + 2 MiB body cap.
/// Core API self-calls (`/api/doctor` on loopback at the configured core port)
/// bypass toolgate and use `http_client` directly.
pub async fn handle_web_fetch(
    http_client: &reqwest::Client,
    toolgate_url: &str,
    gateway_listen: &str,
    args: &serde_json::Value,
) -> String {
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return "Error: 'url' parameter is required.".to_string(),
    };
    let max_length = args
        .get("max_length")
        .and_then(|v| v.as_u64())
        .unwrap_or(50000) as usize;

    tracing::info!(url = %url, "web_fetch: fetching URL");

    // Determine if this is a local Core API call (e.g., /api/doctor).
    // Only allow localhost on Core API port (18789) — block access to internal services
    // like toolgate (9011), postgres, redis, etc.
    // Parse port from gateway listen address (e.g. "0.0.0.0:18789" → 18789)
    let core_port = gateway_listen
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(18789);
    let is_core_api = url.starts_with(&format!("http://localhost:{}", core_port))
        || url.starts_with(&format!("http://127.0.0.1:{}", core_port));

    let text = if is_core_api {
        // Core API self-call — bypass toolgate, fetch raw body directly.
        let resp = match http_client
            .get(url)
            .header("User-Agent", "HydeClaw/1.0")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return format!("Error fetching URL: {}", e),
        };
        if !resp.status().is_success() {
            return format!("HTTP error {}", resp.status());
        }
        // Guard against unbounded response bodies (OOM protection).
        let body_limit = max_length * 2;
        if let Some(cl) = resp.content_length()
            && cl as usize > body_limit
        {
            return format!("Error: response too large ({} bytes, limit {})", cl, body_limit);
        }
        match resp.text().await {
            Ok(t) if t.len() > body_limit => {
                let boundary = t.floor_char_boundary(body_limit);
                t[..boundary].to_string()
            }
            Ok(t) => t,
            Err(e) => return format!("Error reading response: {}", e),
        }
    } else {
        // External URL — delegate to toolgate /web (readability + SSRF + size cap).
        match fetch_via_toolgate_web(http_client, toolgate_url, url, 30).await {
            Ok(t) => t,
            Err(e) => return format!("Error: {}", e),
        }
    };

    // Truncate if too long (safe UTF-8 boundary)
    let trimmed = if text.len() > max_length {
        let boundary = text.floor_char_boundary(max_length);
        format!(
            "{}...\n\n[Truncated at {} chars, total {}]",
            &text[..boundary],
            max_length,
            text.len()
        )
    } else {
        text
    };

    // Wrap in content-security boundary to mitigate prompt injection
    crate::tools::content_security::wrap_external_content(&trimmed, &format!("web_fetch:{}", url))
}

/// Score tools by cosine similarity against the query embedding.
pub async fn select_by_embedding(
    embedder: &dyn crate::memory::EmbeddingService,
    tool_embed_cache: &crate::tools::embedding::ToolEmbeddingCache,
    tools: &[hydeclaw_types::ToolDefinition],
    query: &str,
    k: usize,
) -> anyhow::Result<Vec<hydeclaw_types::ToolDefinition>> {
    let query_vec = embedder.embed(query).await?;

    let mut scored: Vec<(f32, usize)> = Vec::with_capacity(tools.len());
    for (idx, tool) in tools.iter().enumerate() {
        let tool_text = format!("{} {}", tool.name, tool.description);
        let cache_key = format!("tool::{}", tool.name);
        let tool_vec = tool_embed_cache
            .get_or_embed(&cache_key, &tool_text, embedder)
            .await?;
        let sim = crate::tools::embedding::cosine_similarity(&query_vec, &tool_vec);
        scored.push((sim, idx));
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);

    let result = scored
        .into_iter()
        .map(|(_, idx)| tools[idx].clone())
        .collect();
    Ok(result)
}

/// Select top-K tools using embedding-based cosine similarity.
/// Falls back to keyword scoring when the embedding service is unavailable.
pub async fn select_top_k_tools_semantic(
    embedder: &dyn crate::memory::EmbeddingService,
    tool_embed_cache: &crate::tools::embedding::ToolEmbeddingCache,
    memory_available: bool,
    tools: Vec<hydeclaw_types::ToolDefinition>,
    query: &str,
    k: usize,
) -> Vec<hydeclaw_types::ToolDefinition> {
    // Always include core tools — must match `static_core_tool_names()` exactly
    // so subagent context-build matches the dispatcher's static-core partition.
    const ALWAYS_INCLUDE: &[&str] = &[
        "workspace_read", "workspace_write", "workspace_edit", "workspace_list",
        "code_exec", "memory", "agent", "skill_use", "web_fetch", "tool_use",
    ];

    let mut always = Vec::new();
    let mut candidates: Vec<hydeclaw_types::ToolDefinition> = Vec::new();
    for tool in tools {
        if ALWAYS_INCLUDE.contains(&tool.name.as_str()) {
            always.push(tool);
        } else {
            candidates.push(tool);
        }
    }

    let remaining_slots = k.saturating_sub(always.len());
    if remaining_slots == 0 || candidates.is_empty() {
        return always;
    }

    // Try embedding-based selection if memory store is available
    if memory_available {
        match select_by_embedding(embedder, tool_embed_cache, &candidates, query, remaining_slots).await {
            Ok(selected) => {
                tracing::debug!(
                    total = always.len() + selected.len(),
                    k,
                    method = "embedding",
                    "semantic top-K tool selection applied"
                );
                let mut result = always;
                result.extend(selected);
                return result;
            }
            Err(e) => {
                tracing::debug!(error = %e, "embedding unavailable, falling back to keyword scoring");
            }
        }
    }

    // Fallback: keyword scoring
    let selected = select_top_k_by_keywords(candidates, query, remaining_slots);
    tracing::debug!(
        total = always.len() + selected.len(),
        k,
        method = "keyword",
        "keyword top-K tool selection applied"
    );
    let mut result = always;
    result.extend(selected);
    result
}

/// Variant of `select_top_k_tools_semantic` that does NOT force-include
/// any tools from a hardcoded ALWAYS_INCLUDE list. For use by callers (e.g.
/// the dispatcher search handler) where the input is already filtered to
/// the relevant subset and force-include would starve the embedding-ranking
/// (returning the same system tools regardless of query).
pub async fn select_top_k_tools_semantic_no_force(
    embedder: &dyn crate::memory::EmbeddingService,
    tool_embed_cache: &crate::tools::embedding::ToolEmbeddingCache,
    memory_available: bool,
    candidates: Vec<hydeclaw_types::ToolDefinition>,
    query: &str,
    k: usize,
) -> Vec<hydeclaw_types::ToolDefinition> {
    if k == 0 || candidates.is_empty() {
        return Vec::new();
    }

    if memory_available {
        match select_by_embedding(embedder, tool_embed_cache, &candidates, query, k).await {
            Ok(selected) => {
                tracing::debug!(
                    total = selected.len(),
                    k,
                    method = "embedding-no-force",
                    "tool search top-K applied"
                );
                return selected;
            }
            Err(e) => {
                tracing::debug!(error = %e, "embedding unavailable, falling back to keyword scoring");
            }
        }
    }

    // Fallback: keyword scoring across ALL candidates (no ALWAYS_INCLUDE split).
    let selected = select_top_k_by_keywords(candidates, query, k);
    tracing::debug!(
        total = selected.len(),
        k,
        method = "keyword-no-force",
        "tool search top-K applied (fallback)"
    );
    selected
}

/// Keyword-based top-K fallback (original algorithm).
pub fn select_top_k_by_keywords(
    tools: Vec<hydeclaw_types::ToolDefinition>,
    query: &str,
    k: usize,
) -> Vec<hydeclaw_types::ToolDefinition> {
    let query_words: Vec<String> = query
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .collect();

    let mut scored: Vec<(usize, hydeclaw_types::ToolDefinition)> = tools
        .into_iter()
        .map(|t| {
            let haystack = format!("{} {}", t.name, t.description).to_lowercase();
            let score = query_words.iter().filter(|w| haystack.contains(w.as_str())).count();
            (score, t)
        })
        .collect();

    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    scored.truncate(k);
    scored.into_iter().map(|(_, t)| t).collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    // ── select_top_k_by_keywords ─────────────────────────────────────────────

    fn make_tool(name: &str, description: &str) -> hydeclaw_types::ToolDefinition {
        hydeclaw_types::ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: serde_json::json!({}),
        }
    }

    #[test]
    fn select_top_k_empty_tools_returns_empty() {
        let result = select_top_k_by_keywords(vec![], "search web", 5);
        assert!(result.is_empty());
    }

    #[test]
    fn select_top_k_returns_top_two_by_keyword_match() {
        let tools = vec![
            make_tool("web_search", "search the web for information"),
            make_tool("weather_get", "get current weather data"),
            make_tool("calculator", "perform arithmetic calculations"),
        ];
        let result = select_top_k_by_keywords(tools, "search web information", 2);
        assert_eq!(result.len(), 2);
        // web_search matches 3 words; weather_get matches 0; calculator matches 0
        assert_eq!(result[0].name, "web_search");
    }

    #[test]
    fn select_top_k_short_words_ignored() {
        let tools = vec![
            make_tool("web_search", "search the web"),
            make_tool("do_it", "do it now"),
        ];
        // "do" and "it" are <3 chars, should not contribute to score
        let result = select_top_k_by_keywords(tools, "do it", 2);
        // Neither tool matches; order is stable from sort, but both have score 0
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn select_top_k_no_matches_returns_up_to_k_tools() {
        let tools = vec![
            make_tool("alpha", "does alpha things"),
            make_tool("beta", "does beta things"),
            make_tool("gamma", "does gamma things"),
        ];
        // Query matches nothing — all score 0, still returns up to k
        let result = select_top_k_by_keywords(tools, "zzz yyy xxx", 2);
        assert_eq!(result.len(), 2);
    }

    // ── select_top_k_tools_semantic_no_force ────────────────────────────────

    /// Stub embedder that always returns an error, forcing the
    /// no_force variant down the keyword-fallback path.
    struct ErroringEmbedder;
    #[async_trait::async_trait]
    impl crate::memory::EmbeddingService for ErroringEmbedder {
        fn is_available(&self) -> bool { false }
        fn embed_dim(&self) -> u32 { 0 }
        fn embed_model_name(&self) -> Option<String> { None }
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("embedding disabled in test")
        }
        async fn embed_batch(&self, _texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            anyhow::bail!("embedding disabled in test")
        }
    }

    #[tokio::test]
    async fn no_force_ranks_relevant_tool_above_system_tools() {
        // Verifies the bug fix: with the OLD `select_top_k_tools_semantic`,
        // `cron`/`session`/`agents_list` are in ALWAYS_INCLUDE and would be
        // force-returned regardless of query. The no_force variant must
        // rank by query relevance only.
        let candidates = vec![
            make_tool("cron", "schedule recurring jobs"),
            make_tool("session", "manage sessions"),
            make_tool("agents_list", "list all agents"),
            make_tool("github_create_issue", "create a github issue in a repository"),
            make_tool("slack_send_message", "send a slack message to a channel"),
        ];

        let embedder = ErroringEmbedder;
        let cache = crate::tools::embedding::ToolEmbeddingCache::new();

        let result = select_top_k_tools_semantic_no_force(
            &embedder,
            &cache,
            false, // memory_available = false → keyword fallback
            candidates,
            "github",
            5,
        )
        .await;

        // github_create_issue must appear (it's the only relevant match).
        assert!(
            result.iter().any(|t| t.name == "github_create_issue"),
            "expected 'github_create_issue' in results, got: {:?}",
            result.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
        // And it should rank first (only tool with a keyword match).
        assert_eq!(
            result[0].name, "github_create_issue",
            "expected 'github_create_issue' to rank first; got order: {:?}",
            result.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn no_force_returns_empty_when_no_candidates() {
        let embedder = ErroringEmbedder;
        let cache = crate::tools::embedding::ToolEmbeddingCache::new();
        let result = select_top_k_tools_semantic_no_force(
            &embedder,
            &cache,
            false,
            Vec::new(),
            "anything",
            5,
        )
        .await;
        assert!(result.is_empty());
    }

    #[test]
    fn denied_tools_list_contains_critical_entries() {
        // Safety: subagent, workspace_delete, workspace_rename, cron must always be denied
        // "agent" is NOT denied — pool agents need it for peer-to-peer communication.
        // Session context is provided via enriched _context.
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"agent"));
        assert!(SUBAGENT_DENIED_TOOLS.contains(&"workspace_delete"));
        assert!(SUBAGENT_DENIED_TOOLS.contains(&"workspace_rename"));
        assert!(SUBAGENT_DENIED_TOOLS.contains(&"cron"));
        assert!(SUBAGENT_DENIED_TOOLS.contains(&"secret_set"));
        assert!(SUBAGENT_DENIED_TOOLS.contains(&"process"));
    }

    #[test]
    fn denied_tools_do_not_block_safe_tools() {
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"memory"));
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"web_fetch"));
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"workspace_read"));
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"workspace_list"));
        // SUB-01: workspace_write and workspace_edit unlocked for subagents
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"workspace_write"));
        assert!(!SUBAGENT_DENIED_TOOLS.contains(&"workspace_edit"));
    }

    // ── parse_subagent_timeout ───────────────────────────────────────────────

    #[test]
    fn parse_subagent_timeout_minutes() {
        assert_eq!(parse_subagent_timeout("2m"), std::time::Duration::from_secs(120));
    }

    #[test]
    fn parse_subagent_timeout_seconds() {
        assert_eq!(parse_subagent_timeout("30s"), std::time::Duration::from_secs(30));
    }

    #[test]
    fn parse_subagent_timeout_invalid_defaults() {
        assert_eq!(parse_subagent_timeout("invalid"), std::time::Duration::from_secs(120));
    }

    #[test]
    fn parse_subagent_timeout_whitespace() {
        assert_eq!(parse_subagent_timeout(" 5m "), std::time::Duration::from_secs(300));
    }

    // ── fetch_via_toolgate_web (KQ3) ─────────────────────────────────────────
    //
    // Tests the private helper that delegates HTML readability to toolgate's
    // `POST /web` endpoint. See .planning/quick/260420-kq3-*/260420-kq3-PLAN.md.

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn fetch_via_toolgate_parses_html_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/web"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "title": "T",
                "content": "body",
                "url": "https://x"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let out = fetch_via_toolgate_web(&client, &server.uri(), "https://x", 10)
            .await
            .expect("expected Ok");
        assert!(out.contains("Title: T"), "expected 'Title: T' in {out:?}");
        assert!(out.contains("body"), "expected 'body' in {out:?}");
        // First line is the Title:
        assert!(out.starts_with("Title: T"), "Title must be first line: {out:?}");
    }

    #[tokio::test]
    async fn fetch_via_toolgate_parses_non_html_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/web"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": "raw json string",
                "url": "https://api.example.com"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let out = fetch_via_toolgate_web(&client, &server.uri(), "https://api.example.com", 10)
            .await
            .expect("expected Ok");
        assert!(out.contains("raw json string"), "expected content body in {out:?}");
        assert!(!out.contains("Title:"), "must NOT prepend Title when absent: {out:?}");
    }

    #[tokio::test]
    async fn fetch_via_toolgate_non_2xx_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/web"))
            .respond_with(ResponseTemplate::new(502).set_body_json(serde_json::json!({
                "error": "Web error: blocked"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let err = fetch_via_toolgate_web(&client, &server.uri(), "https://bad.example", 10)
            .await
            .expect_err("expected Err on HTTP 502");
        let msg = format!("{err}");
        assert!(
            msg.contains("toolgate /web") || msg.contains("blocked") || msg.contains("502"),
            "error should mention toolgate /web / upstream substring: {msg}"
        );
    }

    #[tokio::test]
    async fn fetch_via_toolgate_connection_error_returns_err() {
        // Bind to an ephemeral port, then drop the listener so nothing listens there.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let unreachable = format!("http://{addr}");

        let client = reqwest::Client::new();
        let err = fetch_via_toolgate_web(&client, &unreachable, "https://x", 2)
            .await
            .expect_err("expected Err on connection failure");
        // No panic (getting here means no panic); confirm err is surfaced.
        let _ = format!("{err}");
    }

    #[tokio::test]
    async fn fetch_via_toolgate_empty_content_ok_empty_string() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/web"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "url": "https://paywalled.example"
            })))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let out = fetch_via_toolgate_web(&client, &server.uri(), "https://paywalled.example", 10)
            .await
            .expect("expected Ok even when content missing");
        assert_eq!(out, "", "no title + no content → empty string, not Err");
    }

    // ── compute_denied_tools ────────────────────────────────────────────────

    #[test]
    fn compute_denied_tools_default_matches_const() {
        let denied = compute_denied_tools(&DelegationConfig::default());
        for tool in SUBAGENT_DENIED_TOOLS {
            assert!(denied.iter().any(|d| d == *tool),
                "default DelegationConfig must include all SUBAGENT_DENIED_TOOLS, missing: {}", tool);
        }
        assert_eq!(denied.len(), SUBAGENT_DENIED_TOOLS.len(),
            "default has no extras, length must match SUBAGENT_DENIED_TOOLS");
    }

    #[test]
    fn default_delegation_blocks_recursion() {
        let cfg = DelegationConfig::default();
        assert_eq!(cfg.max_depth, 1, "default max_depth must be 1 (no nested subagents by default)");
    }

    #[test]
    fn extra_blocked_tools_extend_default() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["code_exec".into(), "cron".into()], // cron is already in default
            blocked_tools_override: vec![],
            subagent_dispatcher_enabled: None,
        };
        let denied = compute_denied_tools(&cfg);
        // All default tools must still be present
        for default in SUBAGENT_DENIED_TOOLS {
            assert!(denied.iter().any(|d| d == *default),
                "default-denied {} must remain when extra is set", default);
        }
        // Extra entry is added
        assert!(denied.iter().any(|d| d == "code_exec"),
            "code_exec must be added via blocked_tools_extra");
        // Duplicate "cron" must NOT appear twice
        assert_eq!(denied.iter().filter(|d| *d == "cron").count(), 1,
            "duplicate entries in blocked_tools_extra must be deduped against default list");
    }

    #[test]
    fn override_replaces_default_deny_list_entirely() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["this_should_be_ignored".into()],
            blocked_tools_override: vec!["only_this".into()],
            subagent_dispatcher_enabled: None,
        };
        let denied = compute_denied_tools(&cfg);
        // Only override entries
        assert_eq!(denied, vec!["only_this".to_string()]);
        // Default tools NOT present (override replaces)
        for default in SUBAGENT_DENIED_TOOLS {
            assert!(!denied.iter().any(|d| d == *default),
                "default-denied {} must be replaced by override", default);
        }
        // Extra ignored when override set
        assert!(!denied.iter().any(|d| d == "this_should_be_ignored"),
            "blocked_tools_extra must be IGNORED when blocked_tools_override is non-empty");
    }

    #[test]
    fn empty_override_falls_back_to_default_plus_extra() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["code_exec".into()],
            blocked_tools_override: vec![],  // empty = fall back to default + extra
            subagent_dispatcher_enabled: None,
        };
        let denied = compute_denied_tools(&cfg);
        assert!(denied.iter().any(|d| d == "code_exec"));
        assert!(denied.iter().any(|d| d == "workspace_delete"),
            "empty override must NOT bypass default deny list");
    }

    // ── runtime_subagent_denylist ──────────────────────────────────────────
    //
    // Audit 2026-05-08 (5th pass): security-critical helper added in the 4th
    // pass had no tests. These regressions guard the contract that
    // `blocked_tools_override` cannot weaken SUBAGENT_DENIED_TOOLS at runtime,
    // even though `compute_denied_tools` (used elsewhere for visibility) still
    // honours it.

    #[test]
    fn runtime_denylist_default_matches_const() {
        let denied = runtime_subagent_denylist(&DelegationConfig::default());
        assert_eq!(denied.len(), SUBAGENT_DENIED_TOOLS.len());
        for tool in SUBAGENT_DENIED_TOOLS {
            assert!(denied.iter().any(|d| d == *tool),
                "runtime denylist must include '{tool}' by default");
        }
    }

    #[test]
    fn runtime_denylist_includes_blocked_tools_extra() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["custom_tool".into()],
            blocked_tools_override: vec![],
            subagent_dispatcher_enabled: None,
        };
        let denied = runtime_subagent_denylist(&cfg);
        for tool in SUBAGENT_DENIED_TOOLS {
            assert!(denied.iter().any(|d| d == *tool));
        }
        assert!(denied.iter().any(|d| d == "custom_tool"),
            "blocked_tools_extra must be additive at runtime");
    }

    #[test]
    fn runtime_denylist_ignores_blocked_tools_override() {
        // Critical regression guard: a subagent author setting `override`
        // MUST NOT be able to weaken the runtime safety net.
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec![],
            blocked_tools_override: vec!["only_this".into()],
            subagent_dispatcher_enabled: None,
        };
        let denied = runtime_subagent_denylist(&cfg);
        for tool in SUBAGENT_DENIED_TOOLS {
            assert!(denied.iter().any(|d| d == *tool),
                "runtime denylist must NOT honour blocked_tools_override — '{tool}' should still be denied");
        }
        assert!(!denied.iter().any(|d| d == "only_this"),
            "runtime denylist must not pull in override-only entries");
    }

    #[test]
    fn runtime_denylist_dedupes_extra_against_const() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["cron".into()],  // already in SUBAGENT_DENIED_TOOLS
            blocked_tools_override: vec![],
            subagent_dispatcher_enabled: None,
        };
        let denied = runtime_subagent_denylist(&cfg);
        assert_eq!(denied.iter().filter(|d| *d == "cron").count(), 1,
            "duplicate entries between extra and SUBAGENT_DENIED_TOOLS must be deduped");
    }
}
