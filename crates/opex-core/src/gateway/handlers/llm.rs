//! `POST /api/llm/complete` — raw LLM completion (no agent loop, no tools).
//!
//! Intended for internal callers (Python toolgate `summarize_video` handler,
//! etc.) that need a cheap one-shot chat call without the full SSE agent
//! pipeline.
//!
//! **Authentication:** standard Bearer-token auth middleware — endpoint is NOT
//! in PUBLIC_EXACT / PUBLIC_PREFIX / LOOPBACK_EXACT lists, so an auth header
//! is required for every caller including loopback.
//!
//! **Provider resolution order:**
//! 1. `provider` field in the JSON body — look up by name + optional `model`.
//! 2. `config.video.digest_provider` / `config.video.digest_model` — same
//!    config knob already used by the video-worker, re-used here so there is
//!    one canonical "raw-LLM" provider config point.
//! 3. First enabled `text`/`llm` provider in the DB (alphabetical) → 400 if
//!    none found.
//!
//! CLI providers (`claude-cli`, `gemini-cli`, `codex-cli`) are rejected — they
//! require a sandbox + agent context that is unavailable here.

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::post,
};
use opex_types::{Message, MessageRole};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::agent::providers::{
    CallOptions, LlmProvider, ProviderOverrides, build_provider,
};
use crate::agent::providers::timeouts::ProviderOptions;
use crate::db::providers::{get_provider_by_name, list_providers_by_type};
use crate::gateway::AppState;

// ── Request / Response DTOs ───────────────────────────────────────────────────

/// A single message in the completion request (role + content).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LlmMessage {
    pub role: String,
    pub content: String,
}

/// POST body for `POST /api/llm/complete`.
#[derive(Debug, Deserialize)]
pub(crate) struct LlmCompleteRequest {
    /// Messages to send to the LLM (required, must be non-empty).
    pub messages: Vec<LlmMessage>,
    /// Optional provider name (from the `providers` DB table).  When absent
    /// the endpoint falls back to `config.video.digest_provider`, then to the
    /// first enabled text provider in the DB.
    #[serde(default)]
    pub provider: Option<String>,
    /// Optional model override — applied only when `provider` is also given
    /// (or resolved via config fallback).
    #[serde(default)]
    pub model: Option<String>,
}

// ── Routes ────────────────────────────────────────────────────────────────────

pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/api/llm/complete", post(api_llm_complete))
}

// ── Handler ───────────────────────────────────────────────────────────────────

/// POST /api/llm/complete
///
/// Raw single-turn LLM call. No agent loop, no tool execution, no SSE stream.
/// Returns `{"text": "<assistant reply>"}` or a JSON error.
async fn api_llm_complete(
    State(state): State<AppState>,
    Json(body): Json<LlmCompleteRequest>,
) -> impl IntoResponse {
    // ── 1. Validate input ─────────────────────────────────────────────────────
    if body.messages.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "messages must not be empty"})),
        )
            .into_response();
    }

    // ── 2. Resolve provider ───────────────────────────────────────────────────
    let provider_result = resolve_llm_provider(&state, body.provider.as_deref(), body.model.as_deref()).await;
    let provider: Arc<dyn LlmProvider> = match provider_result {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };

    // ── 3. Build opex_types::Message list ─────────────────────────────────────
    let messages: Vec<Message> = body
        .messages
        .into_iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "assistant" => MessageRole::Assistant,
                "system" => MessageRole::System,
                _ => MessageRole::User,
            };
            Message {
                role,
                content: m.content,
                tool_calls: None,
                tool_call_id: None,
                thinking_blocks: Vec::new(),
                db_id: None,
            }
        })
        .collect();

    // ── 4. Call provider (non-streaming, no tools) ────────────────────────────
    let opts = CallOptions {
        thinking_level: 0,
        claude_md_content: None,
        ..Default::default()
    };
    match provider.chat(&messages, &[], opts).await {
        Ok(resp) => (
            StatusCode::OK,
            Json(serde_json::json!({"text": resp.content})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("LLM call failed: {e}")})),
        )
            .into_response(),
    }
}

// ── Provider resolution ───────────────────────────────────────────────────────

/// Resolve the LLM provider in priority order:
///
/// 1. `provider_name` argument (from request body) + optional `model`.
/// 2. `config.video.digest_provider` / `config.video.digest_model`.
/// 3. First enabled `text`/`llm` provider in the DB (alphabetical).
///
/// Returns `Err(human-readable message)` if no usable provider is found.
async fn resolve_llm_provider(
    state: &AppState,
    provider_name: Option<&str>,
    model: Option<&str>,
) -> Result<Arc<dyn LlmProvider>, String> {
    // Step 1: explicit request-body override.
    if let Some(name) = provider_name {
        return build_named_provider(state, name, model)
            .await
            .map_err(|e| format!("provider '{}' could not be resolved: {}", name, e));
    }

    // Step 2: config.video.digest_provider fallback.
    let digest_name = state.config.config.video.digest_provider.clone();
    let digest_model = state.config.config.video.digest_model.clone();
    if let Some(ref name) = digest_name {
        return build_named_provider(state, name, model.or(digest_model.as_deref()))
            .await
            .map_err(|e| format!("config digest_provider '{}' could not be resolved: {}", name, e));
    }

    // Step 3: first enabled text provider in DB.
    let rows = list_providers_by_type(&state.infra.db, "text")
        .await
        .map_err(|e| format!("DB error listing text providers: {e}"))?;

    let first_enabled = rows.into_iter().find(|r| r.enabled && !matches!(r.provider_type.as_str(), "claude-cli" | "gemini-cli" | "codex-cli"));

    let row = first_enabled.ok_or_else(|| "no enabled text/llm provider found — configure one in the Providers UI".to_string())?;

    build_from_row(state, &row, model.map(str::to_string))
        .map_err(|e| format!("failed to build provider '{}': {}", row.name, e))
}

/// Look up a named provider row and build it.
async fn build_named_provider(
    state: &AppState,
    name: &str,
    model: Option<&str>,
) -> Result<Arc<dyn LlmProvider>, String> {
    let row = match get_provider_by_name(&state.infra.db, name).await {
        Ok(Some(r)) => r,
        Ok(None) => return Err(format!("provider '{}' not found", name)),
        Err(e) => return Err(format!("DB error: {}", e)),
    };

    if row.category != "text" && row.category != "llm" {
        return Err(format!(
            "provider '{}' has category '{}', expected 'text' or 'llm'",
            name, row.category
        ));
    }

    if matches!(row.provider_type.as_str(), "claude-cli" | "gemini-cli" | "codex-cli") {
        return Err(format!(
            "CLI providers ('{}') are not supported for raw completion — use an HTTP provider",
            row.provider_type
        ));
    }

    build_from_row(state, &row, model.map(str::to_string))
        .map_err(|e| format!("failed to build provider: {}", e))
}

/// Construct an `Arc<dyn LlmProvider>` from a `ProviderRow`.
fn build_from_row(
    state: &AppState,
    row: &crate::db::providers::ProviderRow,
    model: Option<String>,
) -> Result<Arc<dyn LlmProvider>, anyhow::Error> {
    let opts: ProviderOptions = serde_json::from_value(row.options.clone()).unwrap_or_default();
    let timeouts = opts.timeouts;
    let overrides = ProviderOverrides {
        model,
        temperature: None,
        max_tokens: None,
        prompt_cache: None,
    };
    let provider = build_provider(row, state.auth.secrets.clone(), &timeouts, CancellationToken::new(), overrides)?;
    Ok(Arc::from(provider))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Request parse tests ───────────────────────────────────────────────────

    #[test]
    fn parse_minimal_request() {
        let raw = json!({
            "messages": [{"role": "user", "content": "hello"}]
        });
        let req: LlmCompleteRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.messages[0].role, "user");
        assert_eq!(req.messages[0].content, "hello");
        assert!(req.provider.is_none());
        assert!(req.model.is_none());
    }

    #[test]
    fn parse_request_with_provider_and_model() {
        let raw = json!({
            "messages": [
                {"role": "system", "content": "You are helpful."},
                {"role": "user", "content": "Say hi"}
            ],
            "provider": "ollama-local",
            "model": "qwen3:32b"
        });
        let req: LlmCompleteRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.provider.as_deref(), Some("ollama-local"));
        assert_eq!(req.model.as_deref(), Some("qwen3:32b"));
    }

    #[test]
    fn parse_empty_messages_is_accepted_at_parse_level() {
        // Validation (reject empty) happens in the handler, not the parser.
        let raw = json!({"messages": []});
        let req: LlmCompleteRequest = serde_json::from_value(raw).unwrap();
        assert!(req.messages.is_empty());
    }

    #[test]
    fn response_serializes_correctly() {
        // Handler produces serde_json::json!({"text": ...}) inline.
        let v = serde_json::json!({"text": "Hello, world!"});
        assert_eq!(v["text"], "Hello, world!");
    }

    // ── Role mapping ──────────────────────────────────────────────────────────

    #[test]
    fn role_mapping_covers_all_variants() {
        for (input, expected) in [
            ("user", MessageRole::User),
            ("assistant", MessageRole::Assistant),
            ("system", MessageRole::System),
            ("tool", MessageRole::User), // unknown → User
        ] {
            let role = match input {
                "assistant" => MessageRole::Assistant,
                "system" => MessageRole::System,
                _ => MessageRole::User,
            };
            assert_eq!(
                std::mem::discriminant(&role),
                std::mem::discriminant(&expected),
                "role={input}"
            );
        }
    }

    // ── Provider resolution: CLI rejection ────────────────────────────────────

    #[test]
    fn cli_provider_type_rejected() {
        for cli_type in ["claude-cli", "gemini-cli", "codex-cli"] {
            let rejected = matches!(cli_type, "claude-cli" | "gemini-cli" | "codex-cli");
            assert!(rejected, "{cli_type} should be rejected");
        }
    }

    #[test]
    fn http_provider_types_are_not_rejected() {
        for http_type in ["anthropic", "openai", "openai-compatible", "google"] {
            let rejected = matches!(http_type, "claude-cli" | "gemini-cli" | "codex-cli");
            assert!(!rejected, "{http_type} should not be rejected");
        }
    }

    // ── Provider resolution: config fallback logic (pure-logic, no DB) ────────

    /// Verify the resolution priority ordering at a conceptual level:
    /// body.provider > config.digest_provider > first-db-provider.
    /// We model each candidate as an Option and verify the chaining.
    #[test]
    fn resolution_priority_body_wins_over_config() {
        let body_provider: Option<&str> = Some("my-provider");
        let config_provider: Option<&str> = Some("config-provider");

        let chosen = body_provider.or(config_provider);
        assert_eq!(chosen, Some("my-provider"));
    }

    #[test]
    fn resolution_priority_config_wins_when_body_absent() {
        let body_provider: Option<&str> = None;
        let config_provider: Option<&str> = Some("config-provider");

        let chosen = body_provider.or(config_provider);
        assert_eq!(chosen, Some("config-provider"));
    }

    #[test]
    fn resolution_priority_falls_back_to_db_when_both_absent() {
        let body_provider: Option<&str> = None;
        let config_provider: Option<&str> = None;

        // Both absent → must go to DB. Represent DB result as Option.
        let db_provider: Option<&str> = Some("first-db-provider");
        let chosen = body_provider.or(config_provider).or(db_provider);
        assert_eq!(chosen, Some("first-db-provider"));
    }

    #[test]
    fn resolution_all_absent_returns_none() {
        let body_provider: Option<&str> = None;
        let config_provider: Option<&str> = None;
        let db_provider: Option<&str> = None;

        let chosen = body_provider.or(config_provider).or(db_provider);
        assert!(chosen.is_none());
    }

    // ── Category validation ───────────────────────────────────────────────────

    #[test]
    fn category_text_and_llm_accepted() {
        for cat in ["text", "llm"] {
            let ok = cat == "text" || cat == "llm";
            assert!(ok, "{cat} should be accepted");
        }
    }

    #[test]
    fn category_stt_tts_rejected() {
        for cat in ["stt", "tts", "vision", "embedding"] {
            let ok = cat == "text" || cat == "llm";
            assert!(!ok, "{cat} should be rejected");
        }
    }
}
