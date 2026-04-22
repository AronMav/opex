//! Pipeline step: llm_call — provider call, retry, fallback (migrated from engine_provider.rs).
//!
//! Free functions that encapsulate LLM retry/fallback/budget logic without depending on
//! `&AgentEngine`.  The engine methods in `engine_provider.rs` become thin delegations.

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

use hydeclaw_types::{Message, ToolDefinition};
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

/// Default context window size based on model name.
pub fn default_context_for_model(model: &str) -> usize {
    if model.contains("claude") {
        200_000
    } else if model.contains("gpt-4") {
        128_000
    } else if model.contains("MiniMax") || model.contains("M2.5") || model.contains("gemini") {
        1_000_000
    } else {
        128_000
    }
}

// ── Overflow recovery (non-streaming) ───────────────────────────────

/// Call LLM with automatic context overflow recovery.
/// On context overflow (400), invokes `compact` and retries up to 3 times.
pub async fn chat_with_overflow_recovery(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    compact: &impl Compactor,
) -> Result<hydeclaw_types::LlmResponse> {
    let max_compact_attempts: u8 = 3;
    let mut last_error = None;

    for compact_attempt in 0..=max_compact_attempts {
        let result = provider.chat(messages, tools).await;
        match result {
            Ok(resp) => return Ok(resp),
            Err(e)
                if crate::agent::tool_loop::is_context_overflow(&e)
                    && compact_attempt < max_compact_attempts =>
            {
                tracing::warn!(
                    attempt = compact_attempt + 1,
                    max = max_compact_attempts,
                    "context overflow — compacting"
                );
                compact.compact(messages).await;
                last_error = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!(
            "context overflow after {} compaction attempts",
            max_compact_attempts
        )
    }))
}

// ── Overflow recovery (streaming) ───────────────────────────────────

/// Streaming variant of [`chat_with_overflow_recovery`].
pub async fn chat_stream_with_overflow_recovery(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
) -> Result<hydeclaw_types::LlmResponse> {
    let max_compact_attempts: u8 = 3;
    let mut last_error = None;

    for compact_attempt in 0..=max_compact_attempts {
        let result = provider
            .chat_stream(messages, tools, chunk_tx.clone())
            .await;
        match result {
            Ok(resp) => return Ok(resp),
            Err(e)
                if crate::agent::tool_loop::is_context_overflow(&e)
                    && compact_attempt < max_compact_attempts =>
            {
                tracing::warn!(
                    attempt = compact_attempt + 1,
                    max = max_compact_attempts,
                    "context overflow — compacting (stream)"
                );
                compact.compact(messages).await;
                last_error = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!(
            "context overflow after {} compaction attempts (stream)",
            max_compact_attempts
        )
    }))
}

// ── Transient retry (non-streaming) ─────────────────────────────────

/// Call LLM with exponential backoff retry (up to 5 attempts, 500ms–32s).
/// Wraps [`chat_with_overflow_recovery`] to add engine-level transient retry.
/// RateLimit (429) uses full 60s cooldown; Retry-After header overrides both.
pub async fn chat_with_transient_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    compact: &impl Compactor,
) -> Result<hydeclaw_types::LlmResponse> {
    let config = error_classify::RetryConfig::default();
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..config.max_attempts {
        let result =
            chat_with_overflow_recovery(provider, messages, tools, compact).await;
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
                    "retrying LLM call"
                );
                last_error = Some(e);
                if attempt < config.max_attempts - 1 {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("LLM call failed after retries")))
}

// ── Transient retry (streaming) ─────────────────────────────────────

/// Streaming variant of [`chat_with_transient_retry`].
pub async fn chat_stream_with_transient_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
) -> Result<hydeclaw_types::LlmResponse> {
    let config = error_classify::RetryConfig::default();
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..config.max_attempts {
        let result = chat_stream_with_overflow_recovery(
            provider,
            messages,
            tools,
            chunk_tx.clone(),
            compact,
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

// ── Transient retry with explicit provider (non-streaming) ──────────

/// Variant of [`chat_with_transient_retry`] that uses an explicit provider.
/// Used for fallback provider switching without modifying engine state.
pub async fn chat_with_transient_retry_using(
    provider: &Arc<dyn LlmProvider>,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    compact: &impl Compactor,
) -> Result<hydeclaw_types::LlmResponse> {
    let config = error_classify::RetryConfig::default();
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..config.max_attempts {
        let result = match provider.chat(messages, tools).await {
            Ok(resp) => Ok(resp),
            Err(e) if crate::agent::tool_loop::is_context_overflow(&e) => {
                tracing::warn!("context overflow on fallback provider, compacting and retrying");
                compact.compact(messages).await;
                provider.chat(messages, tools).await
            }
            Err(e) => Err(e),
        };
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
                    "retrying LLM call (fallback provider)"
                );
                last_error = Some(e);
                if attempt < config.max_attempts - 1 {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!("LLM call failed after retries (fallback provider)")
    }))
}

// ── Transient retry with explicit provider (streaming) ──────────────

/// Streaming variant of [`chat_with_transient_retry_using`].
#[allow(dead_code)]
pub async fn chat_stream_with_transient_retry_using(
    provider: &Arc<dyn LlmProvider>,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
) -> Result<hydeclaw_types::LlmResponse> {
    let config = error_classify::RetryConfig::default();
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..config.max_attempts {
        let result = match provider
            .chat_stream(messages, tools, chunk_tx.clone())
            .await
        {
            Ok(resp) => Ok(resp),
            Err(e) if crate::agent::tool_loop::is_context_overflow(&e) => {
                tracing::warn!(
                    "context overflow on fallback provider (stream), compacting and retrying"
                );
                compact.compact(messages).await;
                provider.chat_stream(messages, tools, chunk_tx.clone()).await
            }
            Err(e) => Err(e),
        };
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
                    "retrying LLM call (fallback provider, stream)"
                );
                last_error = Some(e);
                if attempt < config.max_attempts - 1 {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        anyhow::anyhow!("LLM stream call failed after retries (fallback provider)")
    }))
}

// ── Compactor trait ─────────────────────────────────────────────────

/// Trait abstracting message compaction so free functions don't depend on `AgentEngine`.
/// Implemented by `AgentEngine` (delegates to `compact_messages`).
#[async_trait::async_trait]
pub trait Compactor: Send + Sync {
    /// Compact the message list in-place (e.g. summarize, drop old messages).
    async fn compact(&self, messages: &mut Vec<Message>);
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

/// Inner timeout-retry loop without WAL logging — extracted for unit testability.
/// Production callers use `chat_stream_with_deadline_retry` which wraps this.
#[allow(clippy::too_many_arguments)]
async fn deadline_retry_inner(
    provider: &dyn LlmProvider,
    messages: &mut Vec<hydeclaw_types::Message>,
    tools: &[hydeclaw_types::ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
    on_retry: impl Fn(u32, u64),
) -> Result<hydeclaw_types::LlmResponse> {
    use crate::agent::providers::error::{LlmCallError, PartialState};
    use hydeclaw_types::{Message, MessageRole};

    let base_messages = messages.clone();
    let mut attempt: u32 = 0;
    let run_started = std::time::Instant::now();
    let mut prepare_messages: Option<Vec<hydeclaw_types::Message>> = None;

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
                        let _ = chunk_tx.send(signal);

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
/// backoff (2s base, 30s cap). Logs a WAL `llm_retry` event on each retry.
/// Stops when the model succeeds, `run_max_duration_secs` is exceeded, or
/// `session_cancel` fires (user Stop).
///
/// Non-timeout errors (ConnectTimeout, AuthError, etc.) are returned immediately
/// so the routing layer can fail over.
#[allow(clippy::too_many_arguments)]
pub async fn chat_stream_with_deadline_retry(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
    session_id: uuid::Uuid,
    sm: &crate::agent::session_manager::SessionManager,
) -> Result<hydeclaw_types::LlmResponse> {
    deadline_retry_inner(
        provider, messages, tools, chunk_tx, compact, session_cancel, run_max_duration_secs,
        |attempt, delay_ms| {
            let sm_db = sm.db().clone();
            tokio::spawn(async move {
                let details = serde_json::json!({
                    "attempt": attempt,
                    "delay_ms": delay_ms,
                });
                let sm2 = crate::agent::session_manager::SessionManager::new(sm_db);
                sm2.log_wal_event(session_id, "llm_retry", Some(&details)).await.ok();
            });
        },
    ).await
}

#[cfg(test)]
pub(crate) async fn chat_stream_with_deadline_retry_no_wal(
    provider: &dyn LlmProvider,
    messages: &mut Vec<Message>,
    tools: &[ToolDefinition],
    chunk_tx: mpsc::UnboundedSender<String>,
    compact: &impl Compactor,
    session_cancel: &tokio_util::sync::CancellationToken,
    run_max_duration_secs: u64,
) -> Result<hydeclaw_types::LlmResponse> {
    deadline_retry_inner(
        provider, messages, tools, chunk_tx, compact, session_cancel, run_max_duration_secs,
        |_attempt, _delay_ms| {},
    ).await
}

#[cfg(test)]
mod deadline_retry_tests {
    use super::*;
    use crate::agent::providers::error::{LlmCallError, PartialState};
    use hydeclaw_types::LlmResponse;
    use tokio_util::sync::CancellationToken;

    struct NoopCompact;
    #[async_trait::async_trait]
    impl Compactor for NoopCompact {
        async fn compact(&self, _messages: &mut Vec<hydeclaw_types::Message>) {}
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
        async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> {
            Ok(ok_response())
        }
        async fn chat_stream(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                return Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                    provider: "test".into(),
                    silent_secs: 60,
                    partial_state: PartialState::Empty,
                }));
            }
            tx.send("done".into()).ok();
            Ok(ok_response())
        }
        fn name(&self) -> &str { "retry-once" }
    }

    #[tokio::test]
    async fn deadline_retry_succeeds_on_second_attempt() {
        let provider = RetryOnceProvider::new();
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
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
            async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> { Ok(ok_response()) }
            async fn chat_stream(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], _tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
                Err(anyhow::Error::new(LlmCallError::InactivityTimeout {
                    provider: "test".into(), silent_secs: 60, partial_state: PartialState::Empty,
                }))
            }
            fn name(&self) -> &str { "always-inactive" }
        }

        let cancel = CancellationToken::new();
        let (chunk_tx, _) = tokio::sync::mpsc::unbounded_channel::<String>();
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
            async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> { Ok(ok_response()) }
            async fn chat_stream(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], _tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
                Err(anyhow::Error::new(LlmCallError::ConnectTimeout { provider: "test".into(), elapsed_secs: 10 }))
            }
            fn name(&self) -> &str { "connect-fail" }
        }

        let cancel = CancellationToken::new();
        let (chunk_tx, _) = tokio::sync::mpsc::unbounded_channel::<String>();
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
            received_messages: Arc<Mutex<Vec<Vec<hydeclaw_types::Message>>>>,
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
            async fn chat(&self, _m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition]) -> anyhow::Result<LlmResponse> {
                Ok(ok_response())
            }
            async fn chat_stream(&self, m: &[hydeclaw_types::Message], _t: &[hydeclaw_types::ToolDefinition], tx: tokio::sync::mpsc::UnboundedSender<String>) -> anyhow::Result<LlmResponse> {
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
                tx.send("continuation".into()).ok();
                Ok(ok_response())
            }
            fn name(&self) -> &str { "prefill-capturing" }
            fn supports_prefill(&self) -> bool { true }
        }

        let provider = PrefillCapturingProvider::new();
        let captured = Arc::clone(&provider.received_messages);
        let (chunk_tx, _) = tokio::sync::mpsc::unbounded_channel::<String>();
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
        assert_eq!(prefill_msg.role, hydeclaw_types::MessageRole::Assistant);
        assert_eq!(prefill_msg.content, "partial response");
    }
}
