//! Pipeline step: subagent — subagent orchestration (migrated from engine_subagent.rs).
//!
//! Free functions extracted from `AgentEngine` methods in `engine_subagent.rs`.

use anyhow::Result;

use crate::agent::url_tools::extract_urls;
use crate::config::DelegationConfig;

/// Tools denied to subagents by default (prevent recursive spawning, destructive operations, and dangerous ops).
/// workspace_write and workspace_edit are allowed so subagents can write shared state files (SUB-01).
///
/// `code_exec` is included even though Docker provides a sandbox, because:
///   1. parents are the only callers expected to run arbitrary code,
///   2. Setup wizard's auto-deny for non-base agents already lists code_exec
///      — subagents inherit that intent: dangerous-by-default.
pub const SUBAGENT_DENIED_TOOLS: &[&str] = &[
    "workspace_delete",
    "workspace_rename",
    "cron",
    "secret_set",
    "process",
    "code_exec",
    "generate_image",
    "synthesize_speech",
    "analyze_image",
    "transcribe_audio",
    "search_web",
    // Clarify blocks the caller waiting for human input; a subagent calling it
    // would hang indefinitely with no user present to answer.
    "clarify",
];

/// Strict subagent runtime deny list — always SUBAGENT_DENIED_TOOLS, regardless
/// of what the subagent's own `[agent.delegation]` config says.
///
/// Hard-anchored to SUBAGENT_DENIED_TOOLS so the subagent cannot weaken its
/// own safety net. The subagent may only add further restrictions via
/// `blocked_tools_extra` (more, never fewer). Audit 2026-05-08, groups T and FF.
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
/// Build a loopback-only client that does NOT follow redirects.
///
/// The core-API self-call shortcut below fetches `http://127.0.0.1:<core>` with a
/// plain client (no SSRF DNS filter) after only a `starts_with` check on the
/// INITIAL url. reqwest's default policy follows up to 10 redirects, so a 3xx
/// from a core endpoint could bounce the request to an internal host
/// (toolgate :9011, `169.254.169.254`, …) and return its body to the model.
/// Disabling redirects turns any 3xx into a non-2xx that the caller rejects.
/// Falls back to cloning `base` only if the builder fails (never in practice).
fn no_redirect_client(base: &reqwest::Client) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap_or_else(|_| base.clone())
}

/// SSRF + size-limit enforcement for external URLs is toolgate's responsibility
/// (`validate_url_ssrf` + `download_limited(max_bytes=2 MiB)` in `routers/fetch.py`).
// reviewed: floor_char_boundary-bounded truncation — char boundary
#[allow(clippy::string_slice)]
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
        let resp = no_redirect_client(http_client)
            .get(url)
            .header("User-Agent", "OPEX/0.1 (link-preview)")
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

/// Result of `enrich_message_text`: the enriched LLM text (PII-redacted user
/// text + fetched-URL content + attachment / file-handler hints).
pub struct EnrichResult {
    pub text: String,
}

/// Enrich user text: auto-fetch URLs (max 2), add attachment hints, and — when a
/// video/file link is detected — add a hint pointing the model at the
/// `file_handler` tool. Returns an `EnrichResult` with the enriched LLM text.
///
/// Handler selection is model-driven and uniform across every channel: the model
/// calls `file_handler(action="list", source_url=…)` to fetch the available
/// handlers, presents them to the user, and runs the chosen one via
/// `file_handler(action="run", …)`. No adapter-specific inline buttons and no
/// auto-processing.
#[allow(clippy::too_many_arguments)]
pub async fn enrich_message_text(
    http_client: &reqwest::Client,
    gateway_listen: &str,
    toolgate_url: &str,
    user_text: &str,
    attachments: &[opex_types::MediaAttachment],
    handler_registry: &crate::agent::handler_registry::HandlerRegistry,
    db: &sqlx::PgPool,
    lang: &str,
) -> EnrichResult {
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

    // Enrich with file attachment context: resolve available handlers from the
    // registry and inject a structured hint so the model can call
    // file_handler(action="run") directly instead of going through
    // action="list" first.
    crate::agent::url_tools::enrich_with_attachments(
        &mut enriched,
        attachments,
        handler_registry,
        db,
        lang,
    )
    .await;

    // Video/file link → hand off to the model-driven handler menu. Uses the
    // ORIGINAL user_text link (pre-PII-redaction) so the source URL is real.
    let detected = detect_video_links(user_text);
    if let Some(first) = detected.first() {
        enriched.push_str(&format!(
            "\n\n[Пользователь прислал ссылку, которую можно обработать: {}. \
             Не обрабатывай её сам и не пересказывай содержимое. Вызови инструмент \
             file_handler с action=\"list\" и source_url=\"{}\" — он сам покажет \
             пользователю интерактивное меню выбора. После этого вызова НЕ пиши \
             вообще никакого текста (даже «Ожидаю выбора» — не надо): меню и есть \
             твой ответ, а любой текст только засоряет чат и устаревает после клика. \
             Просто остановись. Когда пользователь сделает выбор, вызови file_handler \
             с action=\"run\", тем же source_url и выбранным handler_id.]",
            detected.join(", "),
            first,
        ));
    }

    EnrichResult { text: enriched }
}

/// v1 video-URL allowlist: YouTube only (SSRF surface — see spec §9).
///
/// True for hosts we accept as downloadable video links: YouTube and Yandex Disk
/// public-share links (both handled by yt-dlp extractors). YouTube allows the
/// exact label or any dot-prefixed subdomain; Yandex Disk hosts are matched
/// EXACTLY (no suffix/prefix rule) so `disk.yandex.evil.com` cannot sneak through.
fn is_supported_video_host(host: &str) -> bool {
    // YouTube
    host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtu.be"
        || host.ends_with(".youtu.be")
        // Yandex Disk public-share links (yt-dlp `YandexDisk` extractor) — exact hosts only.
        || host == "yadi.sk"
        || host == "disk.yandex.ru"
        || host == "disk.yandex.com"
        || host == "disk.yandex.kz"
        || host == "disk.yandex.by"
        || host == "disk.yandex.uz"
        || host == "disk.360.yandex.ru"
}

/// Filters `extract_urls(text)` keeping only supported video hosts (YouTube,
/// Yandex Disk). The host is parsed with a real URL parser so that userinfo
/// (`youtube.com@evil.com`), case, trailing dot, ports and byte-suffix attacks
/// (`notayoutube.com`, `disk.yandex.evil.com`) are all rejected.
fn detect_video_links(text: &str) -> Vec<String> {
    extract_urls(text)
        .into_iter()
        .filter(|u| {
            let Ok(parsed) = url::Url::parse(u) else { return false };
            if !matches!(parsed.scheme(), "http" | "https") {
                return false;
            }
            let Some(h) = parsed.host_str() else { return false };
            let host = h.trim_end_matches('.').to_ascii_lowercase();
            is_supported_video_host(&host)
        })
        .collect()
}

/// Fetch a URL and return text content (tool handler).
///
/// Delegates HTML readability extraction to toolgate `POST /web` (mode=read) for
/// all non-core-api URLs. Toolgate handles SSRF validation + 2 MiB body cap.
/// Core API self-calls (`/api/doctor` on loopback at the configured core port)
/// bypass toolgate and use `http_client` directly.
// reviewed: floor_char_boundary-bounded truncation — char boundaries
#[allow(clippy::string_slice)]
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
        // No-redirect client: a 3xx from a core endpoint must not bounce this
        // non-SSRF-filtered request to an internal host (see no_redirect_client).
        let resp = match no_redirect_client(http_client)
            .get(url)
            .header("User-Agent", "OPEX/1.0")
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
    tools: &[opex_types::ToolDefinition],
    query: &str,
    k: usize,
) -> anyhow::Result<Vec<opex_types::ToolDefinition>> {
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
    tools: Vec<opex_types::ToolDefinition>,
    query: &str,
    k: usize,
) -> Vec<opex_types::ToolDefinition> {
    // Always include core tools — must match `static_core_tool_names()` exactly
    // so subagent context-build matches the dispatcher's static-core partition.
    const ALWAYS_INCLUDE: &[&str] = &[
        "workspace_read", "workspace_write", "workspace_edit", "workspace_list",
        "code_exec", "memory", "agent", "skill_use", "web_fetch", "tool_use",
    ];

    let mut always = Vec::new();
    let mut candidates: Vec<opex_types::ToolDefinition> = Vec::new();
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
    candidates: Vec<opex_types::ToolDefinition>,
    query: &str,
    k: usize,
) -> Vec<opex_types::ToolDefinition> {
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
    tools: Vec<opex_types::ToolDefinition>,
    query: &str,
    k: usize,
) -> Vec<opex_types::ToolDefinition> {
    let query_words: Vec<String> = query
        .split_whitespace()
        .filter(|w| w.len() >= 3)
        .map(|w| w.to_lowercase())
        .collect();

    let mut scored: Vec<(usize, opex_types::ToolDefinition)> = tools
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

    fn make_tool(name: &str, description: &str) -> opex_types::ToolDefinition {
        opex_types::ToolDefinition {
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
        fn embed_provider_display(&self) -> Option<String> { None }
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
    fn clarify_denied_to_subagents() {
        // Subagents must not be able to call `clarify`: they run headlessly and
        // there is no user present to answer the question, so a clarify call
        // would block the subagent until the waiter times out.
        assert!(
            SUBAGENT_DENIED_TOOLS.contains(&"clarify"),
            "'clarify' must be in SUBAGENT_DENIED_TOOLS"
        );
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

    // ── runtime_subagent_denylist ──────────────────────────────────────────

    #[test]
    fn default_delegation_blocks_recursion() {
        let cfg = DelegationConfig::default();
        assert_eq!(cfg.max_depth, 1, "default max_depth must be 1 (no nested subagents by default)");
    }

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
    fn runtime_denylist_dedupes_extra_against_const() {
        let cfg = DelegationConfig {
            max_depth: 1,
            blocked_tools_extra: vec!["cron".into()],  // already in SUBAGENT_DENIED_TOOLS
            subagent_dispatcher_enabled: None,
        };
        let denied = runtime_subagent_denylist(&cfg);
        assert_eq!(denied.iter().filter(|d| *d == "cron").count(), 1,
            "duplicate entries between extra and SUBAGENT_DENIED_TOOLS must be deduped");
    }

    #[test]
    fn capability_tools_denied_to_subagents() {
        for name in ["generate_image", "synthesize_speech", "analyze_image", "transcribe_audio"] {
            assert!(SUBAGENT_DENIED_TOOLS.contains(&name), "{name} must be denied to subagents");
        }
    }

    // ── detect_video_links ──────────────────────────────────────────────────

    #[test]
    fn detect_video_links_youtube_only() {
        let text = "смотри https://www.youtube.com/watch?v=abc123 и https://example.com/x.mp4";
        let links = detect_video_links(text);
        assert_eq!(links.len(), 1);
        assert!(links[0].contains("youtube.com/watch?v=abc123"));

        assert!(detect_video_links("https://youtu.be/xyz").len() == 1, "youtu.be allowed");
        assert!(detect_video_links("нет ссылок тут").is_empty());

        // Byte-suffix attack rejection: these hosts end with "youtube.com" as bytes,
        // but lack domain-label boundary (no leading dot), so they are not YouTube domains.
        assert!(detect_video_links("https://notayoutube.com/watch").is_empty(), "byte-suffix attack rejected");
        assert!(detect_video_links("https://fakeyoutube.com/x").is_empty(), "byte-suffix attack rejected");

        // URL-parser hardening: userinfo confusion, case-insensitivity, scheme.
        assert!(detect_video_links("https://youtube.com@evil.com/x").is_empty(), "userinfo confusion rejected");
        assert!(detect_video_links("https://YOUTUBE.com/watch?v=z").len() == 1, "uppercase host accepted");
        assert!(detect_video_links("ftp://youtube.com/x").is_empty(), "non-http scheme rejected");
    }

    #[test]
    fn detect_video_links_accepts_yandex_disk() {
        // Public Yandex Disk share links (yt-dlp YandexDisk extractor).
        assert_eq!(detect_video_links("видео: https://disk.yandex.ru/i/abc123").len(), 1, "disk.yandex.ru");
        assert_eq!(detect_video_links("https://yadi.sk/i/xyz789").len(), 1, "yadi.sk short link");
        assert_eq!(detect_video_links("https://disk.yandex.com/d/folderId").len(), 1, "disk.yandex.com");
        assert_eq!(detect_video_links("https://disk.360.yandex.ru/i/q").len(), 1, "yandex 360");
        assert_eq!(detect_video_links("https://DISK.Yandex.RU/i/A").len(), 1, "case-insensitive");

        // Security: exact-host match → suffix/userinfo confusion rejected.
        assert!(detect_video_links("https://disk.yandex.evil.com/i/x").is_empty(), "suffix attack rejected");
        assert!(detect_video_links("https://disk.yandex.ru@evil.com/x").is_empty(), "userinfo confusion rejected");
        assert!(detect_video_links("https://notyadi.sk/i/x").is_empty(), "byte-suffix attack rejected");
        assert!(detect_video_links("ftp://disk.yandex.ru/i/x").is_empty(), "non-http scheme rejected");
    }

    // ── enrich_message_text → EnrichResult ──────────────────────────────────

    /// Helper: build a HandlerRegistry that returns an empty manifest list
    /// (no toolgate reachable in unit tests).
    fn empty_handler_registry() -> crate::agent::handler_registry::HandlerRegistry {
        crate::agent::handler_registry::HandlerRegistry::new(
            "http://127.0.0.1:1".to_string(),
            reqwest::Client::new(),
        )
    }

    /// Helper: lazy PgPool for tests (never connects — attachments are empty
    /// in these tests so the pool is never actually queried).
    fn test_db() -> sqlx::PgPool {
        sqlx::PgPool::connect_lazy("postgres://invalid")
            .expect("lazy pool for test")
    }

    /// A YouTube link in the user text adds a hint pointing the model at the
    /// `file_handler` tool (model-driven menu). It never auto-processes.
    #[tokio::test]
    async fn enrich_youtube_link_adds_file_handler_hint() {
        let client = reqwest::Client::new();
        let reg = empty_handler_registry();
        let db = test_db();
        let result = enrich_message_text(
            &client,
            "127.0.0.1:18789",
            "http://localhost:9011",
            "сделай конспект https://www.youtube.com/watch?v=abc123",
            &[],
            &reg,
            &db,
            "ru",
        )
        .await;

        assert!(
            result.text.contains("file_handler"),
            "hint must point at the file_handler tool: {:?}", result.text
        );
        assert!(
            result.text.contains("action=\"list\""),
            "hint must instruct action=list: {:?}", result.text
        );
        assert!(
            result.text.contains("youtube.com"),
            "hint must contain the URL: {:?}", result.text
        );
    }

    /// A plain-text message with no link / no attachments adds no handler hint.
    #[tokio::test]
    async fn enrich_plain_text_adds_no_handler_hint() {
        let client = reqwest::Client::new();
        let reg = empty_handler_registry();
        let db = test_db();
        let result = enrich_message_text(
            &client,
            "127.0.0.1:18789",
            "http://localhost:9011",
            "привет, как дела?",
            &[],
            &reg,
            &db,
            "ru",
        )
        .await;
        assert!(!result.text.contains("file_handler"), "no hint for plain text");
    }
}
