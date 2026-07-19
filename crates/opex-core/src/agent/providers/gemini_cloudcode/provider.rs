//! `GeminiCloudCodeProvider` — the `LlmProvider` integration layer for
//! Google Code Assist OAuth.
//!
//! # Call sequence (both `chat` and `chat_stream`):
//! 1. Acquire valid access token via `oauth::refresh::get_valid_access_token`.
//! 2. Resolve `ProjectContext` (lazy, Mutex-guarded, cached after first call).
//! 3. Translate messages/tools with `code_assist::request`.
//! 4. Build the Code Assist request envelope.
//! 5. POST via `crate::agent::providers::http::send_with_retry`.
//! 6. Translate response with `code_assist::response`.
//!
//! For `chat_stream`, step 5 returns a byte stream that is parsed by
//! `stream::sse_parser` and synthesized into deltas by `stream::delta`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use opex_types::{LlmResponse, Message, ThinkingBlock, ToolCall, ToolDefinition};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::agent::providers::{
    CallOptions, HttpTransport, LlmProvider, ModelOverride, ProviderOverrides, TimeoutsConfig,
    build_provider_clients,
    cancellable_stream::{CancelSlot, stream_with_cancellation},
    error::{CancelReason, LlmCallError, PartialState},
    http::{RETRYABLE_OPENAI, SendError},
    timeouts::ProviderOptions,
};
use crate::secrets::SecretsManager;

use super::code_assist::types::{CodeAssistError, ProjectContext};
use super::code_assist::{request as ca_request, response as ca_response};
use super::oauth::refresh::get_valid_access_token;
use super::stream::{parse_sse_events, events_to_deltas};

const STREAM_ENDPOINT: &str = "v1internal:streamGenerateContent";
const GENERATE_ENDPOINT: &str = "v1internal:generateContent";

// ── Provider struct ───────────────────────────────────────────────────────────

pub struct GeminiCloudCodeProvider {
    client: Arc<dyn HttpTransport>,
    streaming_client: Arc<dyn HttpTransport>,
    model: ModelOverride,
    temperature: f64,
    max_tokens: Option<u32>,
    cancel: CancellationToken,
    max_retries: u32,
    timeouts: TimeoutsConfig,
    /// Base URL — defaults to `https://cloudcode-pa.googleapis.com`.
    base_url: String,
    /// Lazy-initialized on first call; mutex-guarded so only one concurrent
    /// caller performs the project-resolution LRO.
    project_ctx: tokio::sync::Mutex<Option<ProjectContext>>,
    #[allow(dead_code)]
    secrets: Arc<SecretsManager>,
}

impl GeminiCloudCodeProvider {
    /// Canonical constructor. Signature honored verbatim by `factory::build_provider`.
    pub(crate) fn new_from_row(
        row: &crate::db::providers::ProviderRow,
        secrets: Arc<SecretsManager>,
        timeouts: TimeoutsConfig,
        cancel: CancellationToken,
        opts: ProviderOptions,
        overrides: ProviderOverrides,
    ) -> Result<Self> {
        let base_url = row
            .base_url
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://cloudcode-pa.googleapis.com".to_string());

        let model = overrides
            .model
            .clone()
            .unwrap_or_else(|| {
                row.default_model
                    .clone()
                    .unwrap_or_else(|| "gemini-2.5-pro".to_string())
            });

        let temperature = overrides.temperature.unwrap_or(0.7);
        let max_tokens = overrides.max_tokens;

        let (client, streaming_client) = build_provider_clients(&timeouts);

        Ok(Self {
            client,
            streaming_client,
            model: ModelOverride::new(model),
            temperature,
            max_tokens,
            cancel,
            max_retries: opts.max_retries,
            timeouts,
            base_url,
            project_ctx: tokio::sync::Mutex::new(None),
            secrets,
        })
    }

    /// Ensure `project_ctx` is populated, performing lazy onboarding on the
    /// first call. Subsequent calls return the cached value without performing
    /// HTTP calls.
    ///
    /// Per the controller spec: calls `ensure_project_ctx(access_token, stored_project_id)`.
    /// `stored_project_id` is extracted from the OAuth credentials' packed refresh field;
    /// if absent (fresh credential, first login), passes `None` and lets Module 2 onboard.
    async fn resolve_and_cache_project_ctx(&self, access_token: &str) -> Result<ProjectContext> {
        let mut guard = self.project_ctx.lock().await;
        if let Some(ref ctx) = *guard {
            return Ok(ctx.clone());
        }
        // Extract optional project_id from packed refresh token (RefreshParts).
        // load_credentials() returns Option<GoogleCredentials> (not Result).
        // RefreshParts::unpack returns RefreshParts struct — check empty string for absence.
        let stored_project_id: Option<String> =
            super::oauth::storage::load_credentials().and_then(|c| {
                let parts =
                    super::oauth::types::RefreshParts::unpack(&c.refresh);
                if parts.project_id.is_empty() {
                    None
                } else {
                    Some(parts.project_id)
                }
            });
        let ctx = super::code_assist::project::ensure_project_ctx(
            access_token,
            stored_project_id.as_deref(),
        )
        .await
        .map_err(anyhow::Error::new)?;
        *guard = Some(ctx.clone());
        Ok(ctx)
    }

    /// Build the full URL for a Code Assist API method.
    fn method_url(&self, method: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), method)
    }
}

// ── LlmProvider impl ─────────────────────────────────────────────────────────

#[async_trait]
impl LlmProvider for GeminiCloudCodeProvider {
    fn name(&self) -> &str {
        "gemini-cloudcode"
    }

    fn set_model_override(&self, model: Option<String>) {
        self.model.set(model);
    }

    fn current_model(&self) -> String {
        self.model.effective()
    }

    fn run_max_duration_secs(&self) -> u64 {
        self.timeouts.run_max_duration_secs
    }

    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        _opts: CallOptions,
    ) -> Result<LlmResponse> {
        let access_token = get_valid_access_token(false).await?;
        let ctx = self.resolve_and_cache_project_ctx(&access_token).await?;
        let model = self.model.effective();

        let user_prompt_id = uuid::Uuid::new_v4().to_string();
        let gen_cfg = serde_json::json!({
            "temperature": self.temperature,
            "maxOutputTokens": self.max_tokens
        });
        let inner = ca_request::build_gemini_request(messages, tools, None, gen_cfg);
        let body =
            ca_request::wrap_code_assist_request(&ctx.project_id, &model, &user_prompt_id, inner);

        let url = self.method_url(GENERATE_ENDPOINT);
        // Per D10: clone token outside closure, borrow inside so the closure is Fn
        // (reusable across retries without moving the token).
        let token = access_token.clone();
        let auth_headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
        let raw = self.client
            .post_json(
                &url,
                &body,
                &auth_headers,
                "gemini-cloudcode",
                RETRYABLE_OPENAI,
                self.max_retries,
            )
            .await
            .map_err(|e| {
                // Parse the error message to detect 429 quota exhaustion.
                // post_json returns anyhow::Error with "gemini-cloudcode API error {status}: {body}".
                let msg = e.to_string();
                if msg.contains("429") {
                    let redacted = crate::redact::redact_oauth_str(&msg);
                    if redacted.contains("Quota exceeded") && redacted.contains("per-user-per-day") {
                        return anyhow::Error::new(CodeAssistError::FreeTierQuotaExhausted {
                            reset_at: None,
                        });
                    }
                }
                e
            })?;

        let raw_value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("gemini-cloudcode: invalid JSON response: {e}"))?;
        let response = ca_response::translate_gemini_response(raw_value);
        Ok(response)
    }

    async fn chat_stream(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        chunk_tx: mpsc::Sender<String>,
        _opts: CallOptions,
    ) -> Result<LlmResponse> {
        let access_token = get_valid_access_token(false).await?;
        let ctx = self.resolve_and_cache_project_ctx(&access_token).await?;
        let model = self.model.effective();

        let user_prompt_id = uuid::Uuid::new_v4().to_string();
        let gen_cfg = serde_json::json!({
            "temperature": self.temperature,
            "maxOutputTokens": self.max_tokens
        });
        let inner = ca_request::build_gemini_request(messages, tools, None, gen_cfg);
        let body =
            ca_request::wrap_code_assist_request(&ctx.project_id, &model, &user_prompt_id, inner);

        let url = format!("{}?alt=sse", self.method_url(STREAM_ENDPOINT));
        // Per D10: clone token outside the closure, borrow inside so the closure is FnMut
        // (reusable across retries without moving).
        let token = access_token.clone();
        let auth_headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];

        tracing::info!(
            provider = "gemini-cloudcode",
            model = %model,
            messages = messages.len(),
            tools = tools.len(),
            "calling LLM API (streaming)"
        );

        let resp = self.streaming_client
            .post_json_stream(
                &url,
                &body,
                &auth_headers,
                "gemini-cloudcode",
                RETRYABLE_OPENAI,
                self.max_retries,
            )
            .await
            .map_err(|e| match e {
                SendError::Http { status, .. }
                    if status == 401 || status == 403 =>
                {
                    anyhow::Error::new(LlmCallError::AuthError {
                        provider: "gemini-cloudcode".to_string(),
                        status,
                    })
                }
                SendError::Http { status, body: b, retry_after: _ } => {
                    // Detect free-tier 429 quota error.
                    let redacted = crate::redact::redact_oauth_str(&b);
                    if status == 429
                        && redacted.contains("Quota exceeded")
                        && redacted.contains("per-user-per-day")
                    {
                        return anyhow::Error::new(CodeAssistError::FreeTierQuotaExhausted {
                            reset_at: None,
                        });
                    }
                    tracing::debug!(
                        provider = "gemini-cloudcode",
                        status,
                        body = %redacted,
                        "HTTP error from Code Assist API"
                    );
                    anyhow::Error::new(LlmCallError::Server5xx {
                        provider: "gemini-cloudcode".to_string(),
                        status,
                    })
                }
                SendError::Network(e) => {
                    anyhow::Error::new(crate::agent::providers::classify_reqwest_err(
                        e,
                        "gemini-cloudcode",
                        self.timeouts.connect_secs,
                        self.timeouts.request_secs,
                    ))
                }
            })?;

        // SSE stream consumption — same pattern as openai/chat_stream.rs.
        let slot = CancelSlot::new();
        let byte_stream = stream_with_cancellation(
            resp.bytes_stream(),
            self.cancel.child_token(),
            slot.clone(),
            self.timeouts,
        );
        let mut byte_stream = std::pin::pin!(byte_stream);

        use tokio_stream::StreamExt as _;

        let mut buffer = String::new();
        let mut full_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let thinking_blocks: Vec<ThinkingBlock> = Vec::new();
        let mut finish_reason: Option<String> = None;
        let mut usage: Option<opex_types::TokenUsage> = None;

        'outer: loop {
            match byte_stream.next().await {
                None => break 'outer,
                Some(Err(e)) => return Err(anyhow::Error::new(LlmCallError::from(e))),
                Some(Ok(bytes)) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                }
            }
            // Process all complete SSE blocks in the buffer.
            // Split on the last `\n\n`; leave the incomplete tail for the next iteration.
            let (complete, remainder) = split_on_last_double_newline(&buffer);
            if !complete.is_empty() {
                process_sse_chunk(
                    complete,
                    &mut full_content,
                    &mut tool_calls,
                    &mut finish_reason,
                    &mut usage,
                    &chunk_tx,
                )
                .await;
            }
            buffer = remainder.to_string();
        }

        // Process any remaining bytes after EOF.
        if !buffer.is_empty() {
            process_sse_chunk(
                &buffer,
                &mut full_content,
                &mut tool_calls,
                &mut finish_reason,
                &mut usage,
                &chunk_tx,
            )
            .await;
        }

        // Cancellation check (same pattern as openai/chat_stream.rs).
        if let Some(reason) = slot.get() {
            let partial_state = if !tool_calls.is_empty() {
                PartialState::ToolUse
            } else if !full_content.is_empty() {
                PartialState::Text(full_content.clone())
            } else {
                PartialState::Empty
            };
            let err = match reason {
                CancelReason::InactivityTimeout { silent_secs } => {
                    LlmCallError::InactivityTimeout {
                        provider: "gemini-cloudcode".to_string(),
                        silent_secs,
                        partial_state,
                    }
                }
                CancelReason::MaxDurationExceeded { elapsed_secs } => {
                    LlmCallError::MaxDurationExceeded {
                        provider: "gemini-cloudcode".to_string(),
                        elapsed_secs,
                        partial_state,
                    }
                }
                CancelReason::UserCancelled => LlmCallError::UserCancelled { partial_state },
                CancelReason::ShutdownDrain => LlmCallError::ShutdownDrain { partial_state },
            };
            return Err(anyhow::Error::new(err));
        }

        Ok(LlmResponse {
            content: full_content,
            tool_calls,
            usage,
            model: Some(model),
            provider: Some("gemini-cloudcode".to_string()),
            fallback_notice: None,
            finish_reason,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks,
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Split `s` at the last occurrence of `\n\n`, returning (complete, remainder).
/// `complete` is everything up to and including the last `\n\n`.
/// `remainder` is everything after (the partial block for the next chunk).
fn split_on_last_double_newline(s: &str) -> (&str, &str) {
    match s.rfind("\n\n") {
        Some(pos) => (&s[..pos + 2], &s[pos + 2..]),
        None => ("", s),
    }
}

/// Parse a complete SSE chunk, update accumulators, and forward text deltas.
async fn process_sse_chunk(
    chunk: &str,
    full_content: &mut String,
    tool_calls: &mut Vec<ToolCall>,
    finish_reason: &mut Option<String>,
    usage: &mut Option<opex_types::TokenUsage>,
    chunk_tx: &mpsc::Sender<String>,
) {
    let events = parse_sse_events(chunk);
    let deltas = events_to_deltas(events);
    for delta in deltas {
        if !delta.text.is_empty() {
            full_content.push_str(&delta.text);
            chunk_tx.send(delta.text.clone()).await.ok();
        }
        if let Some(tc) = delta.tool_call {
            tool_calls.push(ToolCall {
                id: opex_types::ids::ToolCallId::from(tc.id),
                name: tc.name,
                arguments: tc.arguments,
                thought_signature: None,
            });
        }
        if let Some(fr) = delta.finish_reason {
            *finish_reason = Some(fr);
        }
        if let Some(u) = delta.usage {
            *usage = Some(opex_types::TokenUsage {
                input_tokens: u.input,
                output_tokens: u.output,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                reasoning_tokens: None,
            });
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::providers::{ProviderOverrides, TimeoutsConfig, timeouts::ProviderOptions};
    use crate::secrets::SecretsManager;
    use std::sync::Arc;
    use uuid::Uuid;

    fn make_row(model: &str) -> crate::db::providers::ProviderRow {
        crate::db::providers::ProviderRow {
            id: Uuid::new_v4(),
            name: "gemini-cloudcode-test".to_string(),
            category: "llm".to_string(),
            provider_type: "gemini-cloudcode".to_string(),
            base_url: None,
            default_model: Some(model.to_string()),
            options: serde_json::Value::Object(Default::default()),
            enabled: true,
            notes: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[tokio::test]
    async fn new_from_row_uses_default_model_when_no_override() {
        let row = make_row("gemini-2.5-pro");
        let secrets = Arc::new(SecretsManager::new_noop());
        let timeouts = TimeoutsConfig::default();
        let cancel = tokio_util::sync::CancellationToken::new();
        let opts = ProviderOptions::default();
        let overrides = ProviderOverrides::default();

        let provider =
            GeminiCloudCodeProvider::new_from_row(&row, secrets, timeouts, cancel, opts, overrides)
                .expect("new_from_row must not fail");
        assert_eq!(provider.current_model(), "gemini-2.5-pro");
        assert_eq!(provider.name(), "gemini-cloudcode");
    }

    #[tokio::test]
    async fn new_from_row_override_model_wins_over_row_default() {
        let row = make_row("gemini-2.5-pro");
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();
        let overrides = ProviderOverrides {
            model: Some("gemini-2.5-flash".to_string()),
            ..Default::default()
        };

        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig::default(),
            cancel,
            ProviderOptions::default(),
            overrides,
        )
        .expect("new_from_row must not fail");
        assert_eq!(provider.current_model(), "gemini-2.5-flash");
    }

    #[tokio::test]
    async fn new_from_row_falls_back_to_hardcoded_default_when_row_has_no_model() {
        let mut row = make_row("gemini-2.5-pro");
        row.default_model = None;
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();
        let overrides = ProviderOverrides::default();

        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig::default(),
            cancel,
            ProviderOptions::default(),
            overrides,
        )
        .expect("new_from_row must not fail");
        assert_eq!(provider.current_model(), "gemini-2.5-pro");
    }

    #[tokio::test]
    async fn new_from_row_uses_row_base_url_when_set() {
        let mut row = make_row("gemini-2.5-pro");
        row.base_url = Some("https://custom.example.com".to_string());
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();

        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig::default(),
            cancel,
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .expect("new_from_row must not fail");
        assert_eq!(provider.base_url, "https://custom.example.com");
    }

    #[tokio::test]
    async fn new_from_row_uses_default_base_url_when_row_base_url_is_none() {
        let row = make_row("gemini-2.5-pro");
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();

        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig::default(),
            cancel,
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .expect("new_from_row must not fail");
        assert_eq!(provider.base_url, "https://cloudcode-pa.googleapis.com");
    }

    #[tokio::test]
    async fn set_model_override_changes_current_model() {
        let row = make_row("gemini-2.5-pro");
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();
        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig::default(),
            cancel,
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .unwrap();

        provider.set_model_override(Some("gemini-2.0-flash".to_string()));
        assert_eq!(provider.current_model(), "gemini-2.0-flash");

        provider.set_model_override(None);
        assert_eq!(provider.current_model(), "gemini-2.5-pro");
    }

    #[tokio::test]
    async fn run_max_duration_secs_returns_timeouts_value() {
        let row = make_row("gemini-2.5-pro");
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();
        let timeouts = TimeoutsConfig { run_max_duration_secs: 1234, ..Default::default() };
        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            timeouts,
            cancel,
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .unwrap();
        assert_eq!(provider.run_max_duration_secs(), 1234);
    }

    #[test]
    fn split_on_last_double_newline_splits_correctly() {
        let s = "data: {}\n\ndata: {}\n\nincomplete";
        let (complete, remainder) = split_on_last_double_newline(s);
        assert!(complete.ends_with("\n\n"));
        assert_eq!(remainder, "incomplete");
    }

    #[test]
    fn split_on_last_double_newline_no_separator_returns_empty_complete() {
        let s = "data: {partial";
        let (complete, remainder) = split_on_last_double_newline(s);
        assert_eq!(complete, "");
        assert_eq!(remainder, s);
    }

    #[test]
    fn split_on_last_double_newline_single_terminator() {
        let s = "data: {}\n\n";
        let (complete, remainder) = split_on_last_double_newline(s);
        assert_eq!(complete, "data: {}\n\n");
        assert_eq!(remainder, "");
    }

    #[tokio::test]
    async fn method_url_strips_trailing_slash() {
        let row = make_row("gemini-2.5-pro");
        let mut row2 = row.clone();
        row2.base_url = Some("https://cloudcode-pa.googleapis.com/".to_string());
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = tokio_util::sync::CancellationToken::new();
        let provider = GeminiCloudCodeProvider::new_from_row(
            &row2,
            secrets,
            TimeoutsConfig::default(),
            cancel,
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .unwrap();
        let url = provider.method_url("v1internal:generateContent");
        assert_eq!(
            url,
            "https://cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
    }
}

// ── Integration tests (wiremock) ──────────────────────────────────────────────

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::agent::providers::{ProviderOverrides, TimeoutsConfig, timeouts::ProviderOptions};
    use crate::secrets::SecretsManager;
    use serial_test::serial;
    use std::sync::Arc;
    use uuid::Uuid;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Build a provider pointed at a mock server's base URL.
    fn make_provider(base_url: &str, secrets: Arc<SecretsManager>) -> GeminiCloudCodeProvider {
        let row = crate::db::providers::ProviderRow {
            id: Uuid::new_v4(),
            name: "test".to_string(),
            category: "llm".to_string(),
            provider_type: "gemini-cloudcode".to_string(),
            base_url: Some(base_url.to_string()),
            default_model: Some("gemini-2.5-pro".to_string()),
            options: serde_json::Value::Object(Default::default()),
            enabled: true,
            notes: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig::default(),
            tokio_util::sync::CancellationToken::new(),
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .unwrap()
    }

    /// Minimal fake `generateContent` response body.
    fn fake_generate_response(text: &str) -> serde_json::Value {
        serde_json::json!({
            "response": {
                "candidates": [{
                    "content": {"parts": [{"text": text}]},
                    "finishReason": "STOP"
                }],
                "usageMetadata": {
                    "promptTokenCount": 5,
                    "candidatesTokenCount": 3,
                    "totalTokenCount": 8
                }
            }
        })
    }

    /// Minimal SSE streaming body: each `part` becomes one SSE event.
    /// The last part carries `finishReason: "STOP"`; others carry `null`.
    fn fake_stream_body(parts: &[&str]) -> String {
        let last = *parts.last().unwrap();
        parts
            .iter()
            .map(|p| {
                let finish_reason: serde_json::Value = if *p == last {
                    serde_json::Value::String("STOP".to_string())
                } else {
                    serde_json::Value::Null
                };
                let json = serde_json::json!({
                    "response": {
                        "candidates": [{
                            "content": {"parts": [{"text": p}]},
                            "finishReason": finish_reason
                        }],
                        "usageMetadata": null
                    }
                });
                format!("data: {}\n\n", json)
            })
            .collect()
    }

    // ── chat_returns_translated_response ──────────────────────────────────────

    #[tokio::test]
    #[serial(gemini_cloudcode_test_token)]
    async fn chat_returns_translated_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fake_generate_response("Hello!")),
            )
            .mount(&server)
            .await;

        unsafe { std::env::set_var("OPEX_GEMINI_TEST_ACCESS_TOKEN", "test-token") };
        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = make_provider(&server.uri(), secrets);

        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "Hi".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let result = provider.chat(&msgs, &[], CallOptions::default()).await;
        unsafe { std::env::remove_var("OPEX_GEMINI_TEST_ACCESS_TOKEN") };

        let resp = result.expect("chat must succeed");
        assert_eq!(resp.content, "Hello!");
        assert_eq!(resp.provider.as_deref(), Some("gemini-cloudcode"));
        assert!(resp.usage.is_some());
    }

    // ── chat_lazy_loads_project_ctx_on_first_call ─────────────────────────────

    #[tokio::test]
    #[serial(gemini_cloudcode_test_token)]
    async fn chat_lazy_loads_project_ctx_on_first_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fake_generate_response("ok")),
            )
            .mount(&server)
            .await;

        unsafe { std::env::set_var("OPEX_GEMINI_TEST_ACCESS_TOKEN", "tok") };
        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = make_provider(&server.uri(), secrets);

        // project_ctx starts as None.
        {
            let guard = provider.project_ctx.lock().await;
            assert!(guard.is_none(), "project_ctx must start as None");
        }

        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "test".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let _ = provider.chat(&msgs, &[], CallOptions::default()).await;
        unsafe { std::env::remove_var("OPEX_GEMINI_TEST_ACCESS_TOKEN") };

        // After the first call, project_ctx must be populated.
        let guard = provider.project_ctx.lock().await;
        assert!(guard.is_some(), "project_ctx must be populated after first call");
    }

    // ── chat_propagates_429_as_quota_error ────────────────────────────────────

    #[tokio::test]
    #[serial(gemini_cloudcode_test_token)]
    async fn chat_propagates_429_as_quota_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(
                ResponseTemplate::new(429)
                    .set_body_string("Quota exceeded per-user-per-day limit"),
            )
            .mount(&server)
            .await;

        unsafe { std::env::set_var("OPEX_GEMINI_TEST_ACCESS_TOKEN", "tok") };
        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = make_provider(&server.uri(), secrets);

        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "q".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let result = provider.chat(&msgs, &[], CallOptions::default()).await;
        unsafe { std::env::remove_var("OPEX_GEMINI_TEST_ACCESS_TOKEN") };

        let err = result.expect_err("429 quota must be an error");
        let is_quota = err
            .downcast_ref::<super::super::code_assist::types::CodeAssistError>()
            .map(|e| {
                matches!(
                    e,
                    super::super::code_assist::types::CodeAssistError::FreeTierQuotaExhausted {
                        ..
                    }
                )
            })
            .unwrap_or(false);
        assert!(is_quota, "expected FreeTierQuotaExhausted, got {err}");
    }

    // ── chat_stream_yields_chunks_in_order ────────────────────────────────────

    #[tokio::test]
    #[serial(gemini_cloudcode_test_token)]
    async fn chat_stream_yields_chunks_in_order() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .and(query_param("alt", "sse"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fake_stream_body(&["Hello", ", ", "world"]))
                    .append_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        unsafe { std::env::set_var("OPEX_GEMINI_TEST_ACCESS_TOKEN", "tok") };
        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = make_provider(&server.uri(), secrets);

        let (tx, mut rx) = mpsc::channel::<String>(64);
        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "hi".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let resp = provider
            .chat_stream(&msgs, &[], tx, CallOptions::default())
            .await
            .expect("chat_stream must succeed");
        unsafe { std::env::remove_var("OPEX_GEMINI_TEST_ACCESS_TOKEN") };

        // Collect all chunks sent during streaming.
        let mut chunks = Vec::new();
        while let Ok(c) = rx.try_recv() {
            chunks.push(c);
        }
        let joined: String = chunks.join("");
        assert_eq!(joined, "Hello, world", "chunks must join to full text in order");
        assert!(resp.finish_reason.is_some());
    }

    // ── chat_stream_handles_cancellation_mid_flight ───────────────────────────

    #[tokio::test]
    #[serial(gemini_cloudcode_test_token)]
    async fn chat_stream_handles_cancellation_mid_flight() {
        use tokio_util::sync::CancellationToken;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fake_stream_body(&["partial text"]))
                    .append_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        unsafe { std::env::set_var("OPEX_GEMINI_TEST_ACCESS_TOKEN", "tok") };
        let secrets = Arc::new(SecretsManager::new_noop());
        let cancel = CancellationToken::new();
        let row = crate::db::providers::ProviderRow {
            id: Uuid::new_v4(),
            name: "test".to_string(),
            category: "llm".to_string(),
            provider_type: "gemini-cloudcode".to_string(),
            base_url: Some(server.uri()),
            default_model: Some("gemini-2.5-pro".to_string()),
            options: serde_json::Value::Object(Default::default()),
            enabled: true,
            notes: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let provider = GeminiCloudCodeProvider::new_from_row(
            &row,
            secrets,
            TimeoutsConfig { stream_inactivity_secs: 30, ..TimeoutsConfig::default() },
            cancel.clone(),
            ProviderOptions::default(),
            ProviderOverrides::default(),
        )
        .unwrap();

        let (tx, _rx) = mpsc::channel::<String>(64);
        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "q".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];

        // Cancel immediately before calling chat_stream.
        cancel.cancel();
        let result = provider.chat_stream(&msgs, &[], tx, CallOptions::default()).await;
        unsafe { std::env::remove_var("OPEX_GEMINI_TEST_ACCESS_TOKEN") };

        // Must return either UserCancelled or succeed with partial data if the
        // full response arrived before cancellation was observed.
        match result {
            Err(e) => {
                let is_cancelled = e
                    .downcast_ref::<LlmCallError>()
                    .map(|e| matches!(e, LlmCallError::UserCancelled { .. }))
                    .unwrap_or(false);
                assert!(is_cancelled, "expected UserCancelled error, got {e}");
            }
            Ok(_) => {
                // Acceptable: the full response arrived before cancellation fired.
            }
        }
    }

    // ── chat_stream_recovers_from_transient_5xx ───────────────────────────────

    #[tokio::test(start_paused = true)]
    #[serial(gemini_cloudcode_test_token)]
    async fn chat_stream_recovers_from_transient_5xx() {
        let server = MockServer::start().await;
        // First request → 503.
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .respond_with(ResponseTemplate::new(503).set_body_string("overloaded"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // Second request → 200 success.
        Mock::given(method("POST"))
            .and(path("/v1internal:streamGenerateContent"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(fake_stream_body(&["recovered"]))
                    .append_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        unsafe { std::env::set_var("OPEX_GEMINI_TEST_ACCESS_TOKEN", "tok") };
        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = make_provider(&server.uri(), secrets);

        let (tx, mut rx) = mpsc::channel::<String>(64);
        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "test".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];
        let result = provider.chat_stream(&msgs, &[], tx, CallOptions::default()).await;
        unsafe { std::env::remove_var("OPEX_GEMINI_TEST_ACCESS_TOKEN") };

        result.expect("must succeed after 503 retry");
        let mut all = String::new();
        while let Ok(c) = rx.try_recv() {
            all.push_str(&c);
        }
        assert_eq!(all, "recovered");
    }

    // ── chat_refreshes_token_if_near_expiry ───────────────────────────────────

    #[tokio::test]
    #[serial(gemini_cloudcode_test_token, oauth_creds_path)]
    async fn chat_refreshes_token_if_near_expiry() {
        use crate::agent::providers::gemini_cloudcode::oauth::storage::save_credentials;
        use crate::agent::providers::gemini_cloudcode::oauth::types::GoogleCredentials;

        // Token endpoint mock — must receive exactly one refresh call.
        let token_server = MockServer::start().await;
        let fresh_token = "fresh-access-token-xyz";
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": fresh_token,
                "expires_in": 3600,
                "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&token_server)
            .await;

        // Code Assist mock — receives the generateContent call after refresh.
        let ca_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1internal:generateContent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(fake_generate_response("ok")),
            )
            .mount(&ca_server)
            .await;

        // Redirect the token endpoint and credentials path to test-isolated locations.
        let storage_dir = tempfile::tempdir().expect("tempdir");
        let creds_path = storage_dir.path().join("google_oauth.json");

        unsafe {
            std::env::set_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT", format!("{}/token", token_server.uri()));
            std::env::set_var("OPEX_OAUTH_CREDENTIALS_PATH", &creds_path);
        }

        // Seed expired credentials so the refresh fires on first chat() call.
        // expires_ms is 1 ms in the past → is_near_expiry returns true.
        let expired_creds = GoogleCredentials {
            refresh: "test-refresh||".to_string(),
            access: "stale-token".to_string(),
            expires_ms: (chrono::Utc::now() - chrono::Duration::milliseconds(1))
                .timestamp_millis(),
            email: "test@example.com".to_string(),
        };
        save_credentials(&expired_creds).expect("save_credentials must succeed");

        let secrets = Arc::new(SecretsManager::new_noop());
        let provider = make_provider(&ca_server.uri(), secrets);
        let msgs = vec![opex_types::Message {
            role: opex_types::MessageRole::User,
            content: "q".to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }];

        let result = provider.chat(&msgs, &[], CallOptions::default()).await;

        unsafe {
            std::env::remove_var("OPEX_GEMINI_TEST_TOKEN_ENDPOINT");
            std::env::remove_var("OPEX_OAUTH_CREDENTIALS_PATH");
        }

        result.expect("chat must succeed after token refresh");
        // Wiremock verifies the .expect(1) on drop — exactly one refresh call fired.
        assert_eq!(
            ca_server.received_requests().await.unwrap().len(),
            1,
            "exactly one generateContent call must have been made"
        );
    }
}
