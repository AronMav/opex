//! Pipeline step: llm_call — provider call, retry, fallback (migrated from engine_provider.rs).
//!
//! Free functions that encapsulate LLM retry/fallback/budget logic without depending on
//! `&AgentEngine`.  The engine methods in `engine_provider.rs` become thin delegations.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

use opex_types::{Message, ToolDefinition};
use crate::agent::error_classify;
use crate::agent::providers::LlmProvider;

// ── Budget ──────────────────────────────────────────────────────────

/// Check daily token budget before an LLM call.
/// Returns `Ok(())` if budget is unlimited (0) or not yet exhausted.
pub async fn check_budget(db: &sqlx::PgPool, agent_name: &str, daily_budget_tokens: u64) -> Result<()> {
    if daily_budget_tokens == 0 {
        return Ok(());
    }
    let used = crate::db::usage::get_agent_usage_today(db, agent_name)
        .await
        .unwrap_or(0) as u64;
    if used >= daily_budget_tokens {
        anyhow::bail!(
            "Daily token budget exceeded ({}/{} tokens). Resets at midnight.",
            used,
            daily_budget_tokens
        );
    }
    Ok(())
}

// ── Fallback provider ───────────────────────────────────────────────

/// Create a fallback LLM provider by looking up `fallback_provider` in the providers table.
/// Returns `None` if the name is absent, not found, or creation fails.
#[allow(clippy::too_many_arguments)]
pub async fn create_fallback_provider(
    db: &sqlx::PgPool,
    fallback_provider_name: Option<&str>,
    agent_name: &str,
    _temperature: f64,
    _max_tokens: Option<u32>,
    secrets: Arc<crate::secrets::SecretsManager>,
    sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    workspace_dir: &str,
    base: bool,
) -> Option<Arc<dyn LlmProvider>> {
    let fb_name = fallback_provider_name?;
    match crate::db::providers::get_provider_by_name(db, fb_name).await {
        Ok(Some(row)) => {
            use crate::agent::providers::{build_provider, build_cli_provider, CliContext};
            let opts: crate::agent::providers::timeouts::ProviderOptions =
                serde_json::from_value(row.options.clone()).unwrap_or_default();
            let timeouts_cfg = opts.timeouts;
            let cancel = tokio_util::sync::CancellationToken::new();

            let provider_box: Box<dyn LlmProvider> = match row.provider_type.as_str() {
                "claude-cli" | "gemini-cli" | "codex-cli" => {
                    let ctx = CliContext {
                        sandbox,
                        agent_name,
                        workspace_dir,
                        base,
                        secrets: secrets.clone(),
                    };
                    match build_cli_provider(&row, None, ctx).await {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(agent = %agent_name, fallback_provider = %fb_name, error = %e,
                                "failed to build fallback CLI provider");
                            return None;
                        }
                    }
                }
                _ => {
                    // Fallback provider uses the row's default model/tuning —
                    // per-agent temperature/max_tokens are carried by the
                    // primary provider (which this falls back from).
                    match build_provider(
                        &row,
                        secrets,
                        &timeouts_cfg,
                        cancel,
                        crate::agent::providers::ProviderOverrides::default(),
                    ) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(agent = %agent_name, fallback_provider = %fb_name, error = %e,
                                "failed to build fallback provider");
                            return None;
                        }
                    }
                }
            };
            Some(Arc::from(provider_box))
        }
        Ok(None) => {
            tracing::warn!(
                agent = %agent_name,
                fallback_provider = %fb_name,
                "fallback provider not found in providers table"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                agent = %agent_name,
                fallback_provider = %fb_name,
                error = %e,
                "failed to look up fallback provider"
            );
            None
        }
    }
}

// ── Default context window ──────────────────────────────────────────

/// Name-based **fallback** context window (tokens), used only when the
/// provider-delegated `context_limit_hint` (`/api/show`, `/v1/models`) is
/// unavailable or fails. Prefer [`resolve_context_limit`] /
/// [`context_limit_tokens`] which consult the real provider value.
///
/// Case-insensitive `contains` matching on the model id. The values are
/// deliberately conservative for families whose window varies by version
/// (glm/minimax/deepseek) — the provider hint refines them for Ollama/OpenAI
/// models. Ordered most-specific first.
pub fn default_context_for_model(model: &str) -> usize {
    let m = model.to_ascii_lowercase();
    if m.contains("gpt-4.1") || m.contains("gpt-5") {
        1_047_576
    } else if m.contains("gpt-4") || m.contains("gpt-3.5") || m.contains("o1") || m.contains("o3") || m.contains("o4") {
        128_000
    } else if m.contains("claude") {
        200_000
    } else if m.contains("gemini") {
        1_000_000
    } else if m.contains("kimi-k2") {
        262_144
    } else if m.contains("kimi") {
        131_072
    } else if m.contains("minimax") {
        // m2 = 196_608, m3 = 524_288 — provider hint refines for Ollama.
        200_000
    } else if m.contains("glm") {
        // glm-5/5.1 = 202_752, glm-5.2 = 1_000_000 — provider hint refines.
        200_000
    } else if m.contains("deepseek") {
        128_000
    } else if m.contains("qwen") {
        131_072
    } else {
        128_000
    }
}

// ── Context-limit discovery (provider-delegated, cached) ────────────

/// A cached window value. `is_fallback` marks values that came from the
/// name-based heuristic (a failed/absent provider hint) rather than a real
/// provider probe — those expire after [`FALLBACK_TTL`] so a transient
/// `/api/show` outage at startup does not pin the wrong window for the whole
/// process lifetime. Real provider values never expire.
#[derive(Clone, Copy)]
struct CachedLimit {
    value: u32,
    is_fallback: bool,
    at: std::time::Instant,
}

/// How long a heuristic-fallback window stays cached before we re-probe the
/// provider. Real provider values are cached permanently.
const FALLBACK_TTL: std::time::Duration = std::time::Duration::from_secs(300);

static CONTEXT_LIMIT_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, CachedLimit>>>
    = std::sync::OnceLock::new();

fn context_limit_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, CachedLimit>> {
    CONTEXT_LIMIT_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Resolve the real context-window size for `model` via the provider's API.
///
/// Each `LlmProvider` can implement `context_limit_hint` to probe its own API
/// (e.g. Ollama `/api/show`, OpenAI-compat `/v1/models`). Real values are cached
/// in-process by `"{provider_name}::{model}"` key permanently; heuristic
/// fallbacks are cached for [`FALLBACK_TTL`] then re-probed (self-heals after a
/// transient provider outage). Falls back to `default_context_for_model` when
/// the provider returns `None` or the call fails.
///
/// Pass the **effective** model (`engine.current_model()`), not the static
/// config model, so a runtime model override resolves the override's window.
pub async fn resolve_context_limit(
    provider: &dyn crate::agent::providers::LlmProvider,
    model: &str,
) -> u32 {
    let cache_key = format!("{}::{}", provider.name(), model);

    if let Ok(guard) = context_limit_cache().lock()
        && let Some(c) = guard.get(&cache_key)
        && (!c.is_fallback || c.at.elapsed() < FALLBACK_TTL) {
            return c.value;
        }

    let (value, is_fallback) = match provider.context_limit_hint(model).await {
        Some(v) => (v, false),
        None => (default_context_for_model(model) as u32, true),
    };

    if let Ok(mut guard) = context_limit_cache().lock() {
        guard.insert(cache_key, CachedLimit { value, is_fallback, at: std::time::Instant::now() });
    }
    value
}

/// Synchronous best-effort window lookup for hot-path context management
/// (`truncate_tool_result`, `compact_tool_results`, `compaction_params`).
///
/// Returns the provider-resolved value cached by [`resolve_context_limit`]
/// (populated at session bootstrap, before the tool loop runs) so tool-result
/// truncation and reactive compaction use the SAME real window as the proactive
/// `Compressor` — not the stale name heuristic. Falls back to
/// `default_context_for_model` when the model has not been resolved yet.
///
/// Matches any cache entry whose key ends in `"::{model}"` (the key is
/// `"{provider}::{model}"`), so callers need only the model id.
pub fn context_limit_tokens(model: &str) -> u32 {
    let suffix = format!("::{model}");
    if let Ok(guard) = context_limit_cache().lock() {
        for (key, c) in guard.iter() {
            if key.ends_with(&suffix) {
                return c.value;
            }
        }
    }
    default_context_for_model(model) as u32
}

#[cfg(test)]
mod context_window_tests {
    use super::{context_limit_tokens, default_context_for_model};

    #[test]
    fn heuristic_is_case_insensitive() {
        // Real Ollama ids are lowercase; the old code matched "MiniMax"
        // case-sensitively and missed them → wrong 128k default.
        assert_eq!(default_context_for_model("minimax-m3:cloud"), 200_000);
        assert_eq!(default_context_for_model("MINIMAX-M3"), 200_000);
    }

    #[test]
    fn heuristic_covers_current_families() {
        assert_eq!(default_context_for_model("kimi-k2.6"), 262_144);
        assert_eq!(default_context_for_model("kimi-k1.5"), 131_072);
        assert_eq!(default_context_for_model("glm-5.2:cloud"), 200_000); // hint refines to 1M for ollama
        assert_eq!(default_context_for_model("gpt-4.1"), 1_047_576);
        assert_eq!(default_context_for_model("gpt-4o"), 128_000);
        assert_eq!(default_context_for_model("claude-opus-4-8"), 200_000);
        assert_eq!(default_context_for_model("gemini-2.5-pro"), 1_000_000);
        assert_eq!(default_context_for_model("deepseek-v3.1"), 128_000);
        assert_eq!(default_context_for_model("qwen3-coder"), 131_072);
        assert_eq!(default_context_for_model("some-unknown-model"), 128_000);
    }

    #[test]
    fn tokens_falls_back_to_heuristic_when_uncached() {
        // A model never resolved via /api/show is not in the cache → heuristic.
        // (Use a unique unlikely-to-be-cached id so the test is order-independent.)
        assert_eq!(
            context_limit_tokens("zzz-uncached-model-xyz"),
            default_context_for_model("zzz-uncached-model-xyz") as u32
        );
    }
}

// ── Overflow recovery (non-streaming) ───────────────────────────────

/// Whether `e` classifies as a context-overflow error worth a compaction retry.
/// Uses the canonical `error_classify` classifier (shared with the rest of the
/// error-handling pipeline) rather than the narrower `tool_loop::is_context_overflow`
/// regex, so this stays in sync with every other `ContextOverflow`-aware code path.
fn is_recoverable_overflow(e: &anyhow::Error) -> bool {
    crate::agent::error_classify::classify(e) == crate::agent::error_classify::LlmErrorClass::ContextOverflow
}

/// Call LLM with automatic context overflow recovery.
///
/// On context overflow (413 / "context length exceeded" / etc.), force-compacts
/// the message history via `Compactor::compact_force` and retries **exactly
/// once**. `compact_force` bypasses the proactive-compaction token-threshold
/// gate (see its doc comment) so this retry is not a no-op even when the
/// rough token estimate sits below that gate. If the retry still overflows
/// (or overflows again after the one recovery attempt), the error propagates
/// — this function never loops more than once, by construction: the `for`
/// loop is bounded to a single iteration and there is no path back to the
/// top of it.
pub async fn chat_with_overflow_recovery(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    compact: &impl Compactor,
) -> Result<opex_types::LlmResponse> {
    let result = provider.chat(messages, tools, crate::agent::providers::CallOptions::default()).await;
    let e = match result {
        Ok(resp) => return Ok(resp),
        Err(e) => e,
    };
    if !is_recoverable_overflow(&e) {
        return Err(e);
    }

    tracing::warn!(error = %e, "context overflow — force-compacting and retrying once");
    compact.compact_force(messages).await;

    provider.chat(messages, tools, crate::agent::providers::CallOptions::default()).await
        .map_err(|e2| {
            if is_recoverable_overflow(&e2) {
                tracing::warn!(error = %e2, "context overflow persists after one compaction retry — giving up");
            }
            e2
        })
}

// ── Overflow recovery (streaming) ───────────────────────────────────

/// Streaming variant of [`chat_with_overflow_recovery`]. Same one-shot
/// force-compact-then-retry contract; see that function's doc comment for
/// the anti-loop guarantee.
pub async fn chat_stream_with_overflow_recovery(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::Sender<String>,
    compact: &impl Compactor,
    opts: crate::agent::providers::CallOptions,
) -> Result<opex_types::LlmResponse> {
    let result = provider
        .chat_stream(messages, tools, chunk_tx.clone(), opts.clone())
        .await;
    let e = match result {
        Ok(resp) => return Ok(resp),
        Err(e) => e,
    };
    if !is_recoverable_overflow(&e) {
        return Err(e);
    }

    tracing::warn!(error = %e, "context overflow — force-compacting and retrying once (stream)");
    compact.compact_force(messages).await;

    provider
        .chat_stream(messages, tools, chunk_tx, opts)
        .await
        .map_err(|e2| {
            if is_recoverable_overflow(&e2) {
                tracing::warn!(error = %e2, "context overflow persists after one compaction retry — giving up (stream)");
            }
            e2
        })
}

// ── Transient retry (streaming) ─────────────────────────────────────

/// Streaming variant of [`chat_with_transient_retry`].
pub async fn chat_stream_with_transient_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::Sender<String>,
    compact: &impl Compactor,
    opts: crate::agent::providers::CallOptions,
) -> Result<opex_types::LlmResponse> {
    let config = error_classify::RetryConfig::default();
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..config.max_attempts {
        let result = chat_stream_with_overflow_recovery(
            provider,
            messages,
            tools,
            chunk_tx.clone(),
            compact,
            opts.clone(),
        )
        .await;
        match result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let class = error_classify::classify(&e);
                if !error_classify::is_retryable(&class) {
                    return Err(e);
                }
                let delay = error_classify::extract_retry_after(&e.to_string())
                    .unwrap_or_else(|| config.retry_delay_for_error(&class, attempt));
                tracing::warn!(
                    attempt = attempt + 1,
                    max_attempts = config.max_attempts,
                    delay_ms = delay.as_millis() as u64,
                    error_class = ?class,
                    error = %e,
                    "retrying LLM call (stream)"
                );
                last_error = Some(e);
                if attempt < config.max_attempts - 1 {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!("LLM stream call failed after retries")
    }))
}

// ── Compactor trait ─────────────────────────────────────────────────

/// Trait abstracting message compaction so free functions don't depend on `AgentEngine`.
/// Implemented by `AgentEngine` (delegates to `compact_messages`).
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    /// Compact the message list in-place (e.g. summarize, drop old messages).
    /// Gated: no-ops if the current token estimate is below the proactive
    /// compaction threshold — safe to call speculatively on every iteration.
    async fn compact(&self, messages: &mut Vec<Message>);

    /// Force-compact the message list regardless of the token-threshold
    /// gate. Used by reactive context-overflow recovery
    /// (`chat_with_overflow_recovery` / `chat_stream_with_overflow_recovery`):
    /// once the provider has already rejected a call as too large, the
    /// gated [`Compactor::compact`] may see a token *estimate* that still
    /// sits below its own threshold and silently no-op, leaving the retry
    /// doomed to fail with the identical error. Default implementation
    /// falls back to the gated `compact` so existing test doubles (e.g.
    /// `NoopCompact`) keep working unchanged.
    async fn compact_force(&self, messages: &mut Vec<Message>) {
        self.compact(messages).await;
    }
}

// ── Audit ───────────────────────────────────────────────────────────

/// Fire-and-forget audit event recording.
pub fn audit(
    db: sqlx::PgPool,
    agent_name: String,
    event_type: &'static str,
    actor: Option<&str>,
    details: serde_json::Value,
) {
    crate::db::audit::audit_spawn(
        db,
        agent_name,
        event_type,
        actor.map(|s| s.to_string()),
        details,
    );
}

// ── Deadline retry ──────────────────────────────────────────────────

/// Special prefix sent on chunk_tx when the deadline retry loop retries.
/// Handled by `execute::forward_chunks_into_sink` to emit `StreamEvent::Reconnecting`.
pub(crate) const RECONNECTING_PREFIX: &str = "__reconnecting__:";

/// Inner timeout-retry loop without timeline logging — extracted for unit testability.
/// Production callers use `chat_stream_with_deadline_retry` which wraps this.
#[allow(clippy::too_many_arguments)]
async fn deadline_retry_inner(
    provider: &dyn LlmProvider,
    messages: &mut Vec<opex_types::Message>,
    tools: &[opex_types::ToolDefinition],
    chunk_tx: mpsc::Sender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
    on_retry: impl Fn(u32, u64),
    opts: crate::agent::providers::CallOptions,
) -> Result<opex_types::LlmResponse> {
    use crate::agent::providers::error::{LlmCallError, PartialState};
    use opex_types::{Message, MessageRole};

    let base_messages = messages.clone();
    let mut attempt: u32 = 0;
    let run_started = std::time::Instant::now();
    let mut prepare_messages: Option<Vec<opex_types::Message>> = None;

    loop {
        // Check run deadline
        if run_max_duration_secs > 0 {
            let elapsed = run_started.elapsed().as_secs();
            if elapsed >= run_max_duration_secs {
                return Err(anyhow::Error::new(LlmCallError::MaxDurationExceeded {
                    provider: provider.name().to_string(),
                    elapsed_secs: elapsed,
                    partial_state: PartialState::Empty,
                }));
            }
        }

        // Check cancellation before each attempt
        if session_cancel.is_cancelled() {
            return Err(anyhow::Error::new(LlmCallError::UserCancelled {
                partial_state: PartialState::Empty,
            }));
        }

        // Restore messages: use prepared prefill if available, otherwise fall back to base state
        *messages = prepare_messages.take().unwrap_or_else(|| base_messages.clone());

        let result = chat_stream_with_transient_retry(
            provider,
            messages,
            tools,
            chunk_tx.clone(),
            compact,
            opts.clone(),
        ).await;

        match result {
            Ok(r) => return Ok(r),
            Err(e) => {
                let call_err = e.downcast_ref::<LlmCallError>().cloned();
                match call_err {
                    // Propagate user/shutdown cancellations immediately
                    Some(LlmCallError::UserCancelled { .. }) | Some(LlmCallError::ShutdownDrain { .. }) => {
                        return Err(e);
                    }
                    // Timeout → retry
                    Some(ref te) if matches!(te, LlmCallError::InactivityTimeout { .. } | LlmCallError::MaxDurationExceeded { .. }) => {
                        let partial_state = te.partial_state().cloned();
                        let is_resumable = partial_state.as_ref().map(|p| p.is_resumable()).unwrap_or(false);

                        attempt += 1;
                        // Exponential backoff: 2s, 4s, 8s, 16s, 30s cap
                        let delay_ms = (2u64.saturating_pow(attempt) * 1000).min(30_000);

                        on_retry(attempt, delay_ms);

                        // Signal the UI via chunk_tx (handled by forward_chunks_into_sink)
                        let signal = format!("{}{attempt}:{delay_ms}", RECONNECTING_PREFIX);
                        chunk_tx.send(signal).await.ok();

                        tracing::warn!(
                            attempt,
                            delay_ms,
                            is_resumable,
                            reason = te.abort_reason().unwrap_or("unknown"),
                            "LLM call timed out, scheduling retry"
                        );

                        // Prepare messages for next attempt
                        if is_resumable
                            && provider.supports_prefill()
                            && let Some(PartialState::Text(ref partial)) = partial_state
                        {
                            let mut next = base_messages.clone();
                            next.push(Message {
                                role: MessageRole::Assistant,
                                content: partial.clone(),
                                tool_calls: None,
                                tool_call_id: None,
                                thinking_blocks: vec![],
            db_id: None,

                            });
                            prepare_messages = Some(next);
                        }
                        // If not resumable or not prefill-capable, prepare_messages stays None;
                        // the next iteration will restore to base_messages.

                        // Backoff with cancel check
                        tokio::select! {
                            biased;
                            _ = session_cancel.cancelled() => {
                                return Err(anyhow::Error::new(LlmCallError::UserCancelled {
                                    partial_state: PartialState::Empty,
                                }));
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_millis(delay_ms)) => {}
                        }

                        continue;
                    }
                    // Other errors propagate to routing
                    _ => return Err(e),
                }
            }
        }
    }
}

/// Streaming LLM call with deadline retry for timeout errors.
///
/// On `InactivityTimeout` or `MaxDurationExceeded`, retries with exponential
/// backoff (2s base, 30s cap). Logs a timeline `llm_retry` event on each retry.
/// Stops when the model succeeds, `run_max_duration_secs` is exceeded, or
/// `session_cancel` fires (user Stop).
///
/// Non-timeout errors (ConnectTimeout, AuthError, etc.) are returned immediately
/// so the routing layer can fail over.
/// Wraps `deadline_retry_inner` with an OTel span so the LLM provider
/// boundary is visible in Jaeger as a child of `pipeline.execute`. The
/// span captures provider name, model (when known), message count, and
/// final outcome — enough to reason about cost / latency per call without
/// reading logs. The provider itself is external and doesn't propagate
/// `traceparent`, so this span IS the LLM-call observability layer.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(
    name = "llm.call",
    skip_all,
    fields(
        provider = provider.name(),
        message_count = messages.len(),
        tool_count = tools.len(),
        // Recorded after the call completes — providers report their
        // chosen model in `LlmResponse`, which may differ from the
        // configured one (fallback / routing).
        model = tracing::field::Empty,
        finish_reason = tracing::field::Empty,
        input_tokens = tracing::field::Empty,
        output_tokens = tracing::field::Empty,
    )
)]
pub async fn chat_stream_with_deadline_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::Sender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
    session_id: uuid::Uuid,
    sm: &crate::agent::session_manager::SessionManager,
    opts: crate::agent::providers::CallOptions,
) -> Result<opex_types::LlmResponse> {
    let result = deadline_retry_inner(
        provider, messages, tools, chunk_tx, compact, session_cancel, run_max_duration_secs,
        |attempt, delay_ms| {
            let sm_db = sm.db().clone();
            // AUDIT-FF-012: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
            tokio::spawn(async move {
                let details = serde_json::json!({
                    "attempt": attempt,
                    "delay_ms": delay_ms,
                });
                let sm2 = crate::agent::session_manager::SessionManager::new(sm_db);
                match sm2.log_timeline_event(session_id, "llm_retry", Some(&details)).await {
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "failed to log llm_retry timeline event"),
                }
            });
        },
        opts,
    ).await;

    // Record outcome fields on the span before returning. These fields
    // are declared as `Empty` above and populated here — empty-skip lets
    // the span be useful even when the call errored out (no usage).
    if let Ok(ref resp) = result {
        let span = tracing::Span::current();
        if let Some(ref m) = resp.model {
            span.record("model", tracing::field::display(m));
        }
        if let Some(ref fr) = resp.finish_reason {
            span.record("finish_reason", tracing::field::display(fr));
        }
        if let Some(ref usage) = resp.usage {
            span.record("input_tokens", usage.input_tokens);
            span.record("output_tokens", usage.output_tokens);
        }
    }
    result
}

#[cfg(test)]
pub(crate) async fn chat_stream_with_deadline_retry_no_wal(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::Sender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
) -> Result<opex_types::LlmResponse> {
    deadline_retry_inner(
        provider, messages, tools, chunk_tx, compact, session_cancel, run_max_duration_secs,
        |_attempt, _delay_ms| {},
        crate::agent::providers::CallOptions::default(),
    ).await
}

#[cfg(test)]
mod deadline_retry_tests {
    use super::*;
    use crate::agent::providers::error::{LlmCallError, PartialState};
    use opex_types::LlmResponse;
    use tokio_util::sync::CancellationToken;

    struct NoopCompact;
    #[async_trait::async_trait]
    impl Compactor for NoopCompact {
        async fn compact(&self, _messages: &mut Vec<opex_types::Message>) {}
    }

    fn ok_response() -> LlmResponse {
        LlmResponse {
            content: "done".into(),
            tool_calls: vec![],
            usage: None,
            finish_reason: None,
            model: None,
            provider: None,
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        }
    }

    struct RetryOnceProvider {
        calls: std::sync::atomic::AtomicU32,
    }

    impl RetryOnceProvider {
        fn new() -> Self { Self { calls: std::sync::atomic::AtomicU32::new(0) } }
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for RetryOnceProvider {
        async fn chat(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            Ok(ok_response())
        }
        async fn chat_stream(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], tx: tokio::sync::mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                return Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                    provider: "test".into(),
                    silent_secs: 60,
                    partial_state: PartialState::Empty,
                }));
            }
            tx.send("done".into()).await.ok();
            Ok(ok_response())
        }
        fn name(&self) -> &str { "retry-once" }
    }

    #[tokio::test]
    async fn deadline_retry_succeeds_on_second_attempt() {
        let provider = RetryOnceProvider::new();
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<String>(1024);
        let cancel = CancellationToken::new();
        let mut messages = vec![];
        let compact = NoopCompact;

        let result = chat_stream_with_deadline_retry_no_wal(
            &provider,
            &mut messages,
            &[],
            chunk_tx,
            &compact,
            &cancel,
            0,
        ).await;
        assert!(result.is_ok(), "expected Ok on second attempt, got {result:?}");

        let mut chunks = vec![];
        while let Ok(c) = chunk_rx.try_recv() { chunks.push(c); }
        assert!(chunks.iter().any(|c| c.starts_with("__reconnecting__:")),
            "expected __reconnecting__ chunk, got: {chunks:?}");
    }

    #[tokio::test]
    async fn deadline_retry_stops_on_user_cancel() {
        struct AlwaysInactiveProvider;
        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for AlwaysInactiveProvider {
            async fn chat(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> { Ok(ok_response()) }
            async fn chat_stream(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], _tx: tokio::sync::mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                    provider: "test".into(), silent_secs: 60, partial_state: PartialState::Empty,
                }))
            }
            fn name(&self) -> &str { "always-inactive" }
        }

        let cancel = CancellationToken::new();
        let (chunk_tx, _) = tokio::sync::mpsc::channel::<String>(1024);
        let mut messages = vec![];

        let cancel_clone = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = chat_stream_with_deadline_retry_no_wal(
            &AlwaysInactiveProvider,
            &mut messages,
            &[],
            chunk_tx,
            &NoopCompact,
            &cancel,
            0,
        ).await;

        let err = result.unwrap_err();
        assert!(err.downcast_ref::<LlmCallError>().map(|e| matches!(e, LlmCallError::UserCancelled { .. })).unwrap_or(false),
            "expected UserCancelled, got: {err}");
    }

    #[tokio::test]
    async fn deadline_retry_propagates_connect_timeout() {
        struct ConnectFailProvider;
        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for ConnectFailProvider {
            async fn chat(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> { Ok(ok_response()) }
            async fn chat_stream(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], _tx: tokio::sync::mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                Err(anyhow::Error::new(LlmCallError::ConnectTimeout { provider: "test".into(), elapsed_secs: 10 }))
            }
            fn name(&self) -> &str { "connect-fail" }
        }

        let cancel = CancellationToken::new();
        let (chunk_tx, _) = tokio::sync::mpsc::channel::<String>(1024);
        let mut messages = vec![];

        let result = chat_stream_with_deadline_retry_no_wal(
            &ConnectFailProvider,
            &mut messages,
            &[],
            chunk_tx,
            &NoopCompact,
            &cancel,
            0,
        ).await;

        let err = result.unwrap_err();
        assert!(err.downcast_ref::<LlmCallError>().map(|e| matches!(e, LlmCallError::ConnectTimeout { .. })).unwrap_or(false),
            "ConnectTimeout must propagate without retry: {err}");
    }

    #[tokio::test]
    async fn deadline_retry_injects_prefill_for_anthropic_provider() {
        use std::sync::{Arc, Mutex};

        struct PrefillCapturingProvider {
            calls: std::sync::atomic::AtomicU32,
            received_messages: Arc<Mutex<Vec<Vec<opex_types::Message>>>>,
        }

        impl PrefillCapturingProvider {
            fn new() -> Self {
                Self {
                    calls: std::sync::atomic::AtomicU32::new(0),
                    received_messages: Arc::new(Mutex::new(vec![])),
                }
            }
        }

        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for PrefillCapturingProvider {
            async fn chat(&self, _m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                Ok(ok_response())
            }
            async fn chat_stream(&self, m: &[opex_types::Message], _t: &[opex_types::ToolDefinition], tx: tokio::sync::mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                self.received_messages.lock().unwrap().push(m.to_vec());
                let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    // First call: return a partial text timeout
                    return Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                        provider: "test".into(),
                        silent_secs: 30,
                        partial_state: PartialState::Text("partial response".into()),
                    }));
                }
                tx.send("continuation".into()).await.ok();
                Ok(ok_response())
            }
            fn name(&self) -> &str { "prefill-capturing" }
            fn supports_prefill(&self) -> bool { true }
        }

        let provider = PrefillCapturingProvider::new();
        let captured = Arc::clone(&provider.received_messages);
        let (chunk_tx, _) = tokio::sync::mpsc::channel::<String>(1024);
        let cancel = CancellationToken::new();
        let mut messages = vec![];

        let result = chat_stream_with_deadline_retry_no_wal(
            &provider,
            &mut messages,
            &[],
            chunk_tx,
            &NoopCompact,
            &cancel,
            0,
        ).await;
        assert!(result.is_ok(), "expected success, got {result:?}");

        let all_calls = captured.lock().unwrap();
        assert_eq!(all_calls.len(), 2, "expected 2 LLM calls");

        // First call: no prefill (empty message list)
        assert!(all_calls[0].is_empty(), "first call should have empty messages");

        // Second call: should have the assistant prefill message
        assert_eq!(all_calls[1].len(), 1, "second call should have 1 message (the prefill)");
        let prefill_msg = &all_calls[1][0];
        assert_eq!(prefill_msg.role, opex_types::MessageRole::Assistant);
        assert_eq!(prefill_msg.content, "partial response");
    }

    /// Verify that the bounded `chunk_tx` channel (capacity 1024) applies
    /// backpressure: a slow receiver causes `.send().await` to block until
    /// space is available, and no chunks are lost.
    ///
    /// Test design: a provider emits N > capacity chunks as fast as possible.
    /// The receiver sleeps briefly between reads to simulate a slow sink.
    /// After the provider completes, all N chunks must be present in the
    /// receiver — none silently dropped.
    #[tokio::test]
    async fn bounded_chunk_channel_applies_backpressure() {
        const CAPACITY: usize = 1024;
        const CHUNK_COUNT: usize = 1500; // exceeds capacity

        struct BurstProvider { count: usize }

        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for BurstProvider {
            async fn chat(
                &self,
                _m: &[opex_types::Message],
                _t: &[opex_types::ToolDefinition],
                _opts: crate::agent::providers::CallOptions,
            ) -> anyhow::Result<LlmResponse> {
                Ok(ok_response())
            }

            async fn chat_stream(
                &self,
                _m: &[opex_types::Message],
                _t: &[opex_types::ToolDefinition],
                tx: tokio::sync::mpsc::Sender<String>,
                _opts: crate::agent::providers::CallOptions,
            ) -> anyhow::Result<LlmResponse> {
                for i in 0..self.count {
                    // `.send().await` blocks when buffer is full → backpressure.
                    tx.send(format!("chunk-{i}")).await.expect("receiver must be alive");
                }
                Ok(ok_response())
            }

            fn name(&self) -> &str { "burst" }
        }

        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<String>(CAPACITY);
        let cancel = CancellationToken::new();
        let mut messages = vec![];

        // Spawn a slow consumer in the background.
        let consumer = tokio::spawn(async move {
            let mut received = Vec::new();
            while let Some(chunk) = chunk_rx.recv().await {
                received.push(chunk);
                // Simulate a slow sink to guarantee backpressure is exercised.
                if received.len() % 100 == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
            received
        });

        let provider = BurstProvider { count: CHUNK_COUNT };
        let result = chat_stream_with_deadline_retry_no_wal(
            &provider,
            &mut messages,
            &[],
            chunk_tx,
            &NoopCompact,
            &cancel,
            0,
        ).await;

        assert!(result.is_ok(), "provider must succeed: {result:?}");

        let received = consumer.await.expect("consumer task must not panic");
        assert_eq!(
            received.len(),
            CHUNK_COUNT,
            "all {CHUNK_COUNT} chunks must arrive — none dropped under backpressure"
        );
        // Verify order
        for (i, chunk) in received.iter().enumerate() {
            assert_eq!(chunk, &format!("chunk-{i}"), "chunk order must be preserved");
        }
    }
}

// ── Overflow-recovery tests (Batch G / T13 item 4) ──────────────────
//
// Verifies: (1) a single ContextOverflow error triggers exactly one
// force-compact + retry, and the turn succeeds; (2) an LLM that ALWAYS
// overflows gets recovery applied exactly once, then fails — no infinite
// compact/retry cycle. See `is_recoverable_overflow` / `chat_with_overflow_recovery`
// / `chat_stream_with_overflow_recovery` doc comments for the anti-loop
// argument (bounded `for` loop, no path back to the top).
#[cfg(test)]
mod overflow_recovery_tests {
    use super::*;
    use opex_types::LlmResponse;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn ok_response(content: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            tool_calls: vec![],
            usage: None,
            finish_reason: None,
            model: None,
            provider: None,
            fallback_notice: None,
            tools_used: vec![],
            iterations: 0,
            thinking_blocks: vec![],
        }
    }

    fn overflow_error() -> anyhow::Error {
        anyhow::anyhow!("400 request_too_large: prompt is too long, context length exceeded")
    }

    /// Compactor test double: counts calls to the gated `compact` (should
    /// never be invoked by the overflow path — it must use `compact_force`)
    /// and to `compact_force`, and actually shrinks `messages` on force so
    /// the retry has observably different input (mirrors real compaction
    /// replacing old turns with a summary).
    struct CountingCompactor {
        compact_calls: AtomicU32,
        force_calls: AtomicU32,
    }

    impl CountingCompactor {
        fn new() -> Self {
            Self { compact_calls: AtomicU32::new(0), force_calls: AtomicU32::new(0) }
        }
    }

    #[async_trait::async_trait]
    impl Compactor for CountingCompactor {
        async fn compact(&self, _messages: &mut Vec<Message>) {
            self.compact_calls.fetch_add(1, Ordering::SeqCst);
        }
        async fn compact_force(&self, messages: &mut Vec<Message>) {
            self.force_calls.fetch_add(1, Ordering::SeqCst);
            messages.clear();
        }
    }

    /// First call overflows, second call (after force-compact) succeeds.
    struct OverflowThenOkProvider {
        calls: AtomicU32,
    }

    impl OverflowThenOkProvider {
        fn new() -> Self { Self { calls: AtomicU32::new(0) } }
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for OverflowThenOkProvider {
        async fn chat(&self, _m: &[Message], _t: &[ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(overflow_error())
            } else {
                Ok(ok_response("recovered"))
            }
        }
        async fn chat_stream(&self, _m: &[Message], _t: &[ToolDefinition], tx: mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(overflow_error())
            } else {
                tx.send("recovered".into()).await.ok();
                Ok(ok_response("recovered"))
            }
        }
        fn name(&self) -> &str { "overflow-then-ok" }
    }

    /// Always overflows, regardless of how many times it's called or what
    /// the (compacted) message list looks like.
    struct AlwaysOverflowProvider {
        calls: AtomicU32,
    }

    impl AlwaysOverflowProvider {
        fn new() -> Self { Self { calls: AtomicU32::new(0) } }
    }

    #[async_trait::async_trait]
    impl crate::agent::providers::LlmProvider for AlwaysOverflowProvider {
        async fn chat(&self, _m: &[Message], _t: &[ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(overflow_error())
        }
        async fn chat_stream(&self, _m: &[Message], _t: &[ToolDefinition], _tx: mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(overflow_error())
        }
        fn name(&self) -> &str { "always-overflow" }
    }

    fn some_messages() -> Vec<Message> {
        vec![Message {
            role: opex_types::MessageRole::User,
            content: "hello".into(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }]
    }

    #[tokio::test]
    async fn chat_recovers_after_one_force_compact() {
        let provider = OverflowThenOkProvider::new();
        let compactor = CountingCompactor::new();
        let mut messages = some_messages();

        let result = chat_with_overflow_recovery(&provider, &mut messages, &[], &compactor).await;

        assert!(result.is_ok(), "expected recovery to succeed, got {result:?}");
        assert_eq!(result.unwrap().content, "recovered");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "expected exactly 2 provider calls (overflow + retry)");
        assert_eq!(compactor.force_calls.load(Ordering::SeqCst), 1, "expected exactly 1 forced compaction");
        assert_eq!(compactor.compact_calls.load(Ordering::SeqCst), 0, "gated compact() must not be used by overflow recovery");
    }

    #[tokio::test]
    async fn chat_stream_recovers_after_one_force_compact() {
        let provider = OverflowThenOkProvider::new();
        let compactor = CountingCompactor::new();
        let mut messages = some_messages();
        let (tx, mut rx) = mpsc::channel::<String>(16);

        let result = chat_stream_with_overflow_recovery(
            &provider, &mut messages, &[], tx, &compactor,
            crate::agent::providers::CallOptions::default(),
        ).await;

        assert!(result.is_ok(), "expected recovery to succeed, got {result:?}");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "expected exactly 2 provider calls (overflow + retry)");
        assert_eq!(compactor.force_calls.load(Ordering::SeqCst), 1, "expected exactly 1 forced compaction");

        let mut chunks = vec![];
        while let Ok(c) = rx.try_recv() { chunks.push(c); }
        assert_eq!(chunks, vec!["recovered".to_string()]);
    }

    /// Anti-loop guarantee: a provider that ALWAYS overflows must see
    /// recovery applied exactly once (one force-compact, one retry), then
    /// fail — never an unbounded compact/retry cycle.
    #[tokio::test]
    async fn chat_gives_up_after_exactly_one_recovery_attempt() {
        let provider = AlwaysOverflowProvider::new();
        let compactor = CountingCompactor::new();
        let mut messages = some_messages();

        let result = chat_with_overflow_recovery(&provider, &mut messages, &[], &compactor).await;

        assert!(result.is_err(), "expected failure — overflow never resolves");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "expected exactly 2 provider calls total (no infinite loop)");
        assert_eq!(compactor.force_calls.load(Ordering::SeqCst), 1, "expected exactly 1 forced compaction — not repeated");
    }

    #[tokio::test]
    async fn chat_stream_gives_up_after_exactly_one_recovery_attempt() {
        let provider = AlwaysOverflowProvider::new();
        let compactor = CountingCompactor::new();
        let mut messages = some_messages();
        let (tx, _rx) = mpsc::channel::<String>(16);

        let result = chat_stream_with_overflow_recovery(
            &provider, &mut messages, &[], tx, &compactor,
            crate::agent::providers::CallOptions::default(),
        ).await;

        assert!(result.is_err(), "expected failure — overflow never resolves");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2, "expected exactly 2 provider calls total (no infinite loop)");
        assert_eq!(compactor.force_calls.load(Ordering::SeqCst), 1, "expected exactly 1 forced compaction — not repeated");
    }

    /// A non-overflow error must propagate immediately without any compaction.
    #[tokio::test]
    async fn chat_non_overflow_error_skips_recovery() {
        struct AuthFailProvider;
        #[async_trait::async_trait]
        impl crate::agent::providers::LlmProvider for AuthFailProvider {
            async fn chat(&self, _m: &[Message], _t: &[ToolDefinition], _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                Err(anyhow::anyhow!("401 unauthorized: invalid api key"))
            }
            async fn chat_stream(&self, _m: &[Message], _t: &[ToolDefinition], _tx: mpsc::Sender<String>, _opts: crate::agent::providers::CallOptions) -> anyhow::Result<LlmResponse> {
                Err(anyhow::anyhow!("401 unauthorized: invalid api key"))
            }
            fn name(&self) -> &str { "auth-fail" }
        }

        let compactor = CountingCompactor::new();
        let mut messages = some_messages();
        let result = chat_with_overflow_recovery(&AuthFailProvider, &mut messages, &[], &compactor).await;

        assert!(result.is_err());
        assert_eq!(compactor.force_calls.load(Ordering::SeqCst), 0, "non-overflow errors must not trigger compaction");
        assert_eq!(compactor.compact_calls.load(Ordering::SeqCst), 0);
    }
}
