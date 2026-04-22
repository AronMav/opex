//! Main LLM+tools loop. Transport-agnostic via EventSink.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §3, §5.
//!
//! # Scope
//!
//! This module implements the **safe subset** of the tool loop:
//! - Happy path: N LLM calls with tool-call iterations
//! - Cancellation check at top of each iteration
//! - Sink closed → Interrupted
//! - LLM provider error (after retry exhaustion) → Failed
//! - LoopDetector trip (after max nudges) → Failed
//! - Turn limit reached → Done with finish_reason = "turn_limit"
//!
//! # Explicitly omitted (deferred to Phase 66)
//!
//! - Fallback provider switching on consecutive_failures (`using_fallback` path).
//!   The thin adapters in `engine/run.rs` use a single provider per session entry.
//! - SessionCorruption recovery (messages reset + retry). Pipeline path treats it
//!   as a regular LLM error → `ExecuteStatus::Failed`.
//! - Empty-response auto-retry (`empty_retry_count` path).
//! - Auto-continue detection (`looks_incomplete` / nudge path).
//! - WAL warm-up replay into LoopDetector (bootstrap owns that; execute receives
//!   the already-warmed detector via `BootstrapOutcome::loop_detector`).
//! - Thinking-block stripping from `IncomingMessage` directives. Content is passed
//!   to DB as-is; callers that need stripping should do it in finalize.

use crate::agent::engine::AgentEngine;
use crate::agent::engine::LoopBreak;
use crate::agent::pipeline::bootstrap::BootstrapOutcome;
use crate::agent::pipeline::sink::{EventSink, PipelineEvent, SinkError};
use crate::agent::stream_event::StreamEvent;
use crate::agent::tool_executor::ToolExecutor as _;
use hydeclaw_types::{Message, MessageRole};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// ── Outcome types ────────────────────────────────────────────────────────────

pub struct ExecuteOutcome {
    pub status: ExecuteStatus,
    pub final_text: String,
    /// Thinking blocks from extended thinking (Anthropic only).
    pub thinking_json: Option<serde_json::Value>,
    pub messages_len_at_end: usize,
    /// Parent id for the final assistant message — tracks the end of the
    /// intermediate chain (last tool result or last intermediate assistant)
    /// so finalize can link the final reply correctly.
    pub final_parent_msg_id: Uuid,
}

#[derive(Debug)]
pub enum ExecuteStatus {
    Done,
    Failed(String),
    /// Execution stopped before finishing. Reason is a static label for logging.
    Interrupted(&'static str),
}

// ── execute() ────────────────────────────────────────────────────────────────

/// Run the LLM+tools loop and stream results into `sink`.
///
/// Implements the safe subset of the `handle_sse` tool loop (see module doc).
/// Callers that need the full feature set (fallback provider, auto-continue,
/// session corruption recovery) should use `handle_sse` directly until Phase 66.
pub async fn execute<S: EventSink>(
    engine: &AgentEngine,
    bootstrap_outcome: BootstrapOutcome,
    sink: &mut S,
    cancel: CancellationToken,
) -> anyhow::Result<ExecuteOutcome> {
    let BootstrapOutcome {
        session_id,
        mut messages,
        tools,
        mut loop_detector,
        processing_guard: _processing_guard, // Drop handles cleanup
        lifecycle_guard: _lifecycle_guard,
        enriched_text: _,
        command_output: _,
        user_message_id,
    } = bootstrap_outcome;

    // last_msg_id threads the DB parent chain through intermediate assistant
    // (with tool_calls) and tool-result messages so reload-from-active-path
    // can reconstruct the full turn, not just user → final assistant.
    let sm = crate::agent::session_manager::SessionManager::new(engine.cfg().db.clone());
    let agent_name = engine.cfg().agent.name.clone();
    let mut last_msg_id: uuid::Uuid = user_message_id;

    // Bail early if cancel was already signalled before we start.
    if cancel.is_cancelled() {
        return Ok(ExecuteOutcome {
            status: ExecuteStatus::Interrupted("cancel_token"),
            final_text: String::new(),
            thinking_json: None,
            messages_len_at_end: messages.len(),
            final_parent_msg_id: last_msg_id,
        });
    }

    // Signal the start of a message to the sink.
    let msg_id = format!("msg_{}", Uuid::new_v4());
    match sink
        .emit(PipelineEvent::Stream(StreamEvent::MessageStart { message_id: msg_id }))
        .await
    {
        Ok(()) => {}
        Err(SinkError::Closed) | Err(SinkError::Full) => {
            return Ok(ExecuteOutcome {
                status: ExecuteStatus::Interrupted("sink_closed"),
                final_text: String::new(),
                thinking_json: None,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
            });
        }
        Err(e) => return Err(e.into()),
    }

    // ── Mutable loop state ───────────────────────────────────────────────────
    let loop_config = engine.tool_loop_config();
    let mut final_text = String::new();
    let mut final_thinking_blocks: Vec<hydeclaw_types::ThinkingBlock> = vec![];
    let mut context_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
    let mut loop_nudge_count: usize = 0;

    // ── Turn loop ────────────────────────────────────────────────────────────
    for iteration in 0..loop_config.effective_max_iterations() {
        // 1. Check cancellation (graceful shutdown / SIGHUP drain)
        if cancel.is_cancelled() {
            tracing::info!(session = %session_id, "request cancelled — breaking tool loop");
            return Ok(ExecuteOutcome {
                status: ExecuteStatus::Interrupted("cancel_token"),
                final_text,
                thinking_json: None,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
            });
        }

        // 2. Emit StepStart
        let step_id = format!("step_{}", iteration);
        match sink
            .emit(PipelineEvent::Stream(StreamEvent::StepStart {
                step_id: step_id.clone(),
            }))
            .await
        {
            Ok(()) => {}
            Err(SinkError::Closed) => {
                return Ok(ExecuteOutcome {
                    status: ExecuteStatus::Interrupted("sink_closed"),
                    final_text,
                    thinking_json: None,
                    messages_len_at_end: messages.len(),
                    final_parent_msg_id: last_msg_id,
                });
            }
            Err(e) => return Err(e.into()),
        }

        // 3. Compact tool results to stay within context budget
        crate::agent::pipeline::context::compact_tool_results(
            &engine.cfg().agent.model,
            engine.cfg().agent.compaction.as_ref(),
            &mut messages,
            &mut context_chars,
        );

        // 4. Call LLM with a forwarder that emits chunks directly to the sink
        //    as they arrive. No spawned task, no oneshot, no batching — the
        //    sink stays owned by `execute` and `forward_chunks_into_sink` drives
        //    a `tokio::select!` over (chunk_rx, llm_fut) so `TextDelta`s land in
        //    the sink interleaved with the LLM call. Contract pinned by
        //    `tests::streams_chunks_individually_during_no_tool_turn` and
        //    `tests::emits_reasoning_text_before_tool_call` below.
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let provider = engine.cfg().provider.as_ref();
        let run_max = provider.run_max_duration_secs();
        let llm_fut = crate::agent::pipeline::llm_call::chat_stream_with_deadline_retry(
            provider,
            &mut messages,
            &tools,
            chunk_tx,
            engine,
            &cancel,
            run_max,
            session_id,
            &sm,
        );

        // 5. Drive the LLM future and the chunk forwarder concurrently.
        let (llm_result, partial, sink_fatal) =
            forward_chunks_into_sink(llm_fut, chunk_rx, sink).await;
        if let Some(e) = sink_fatal {
            return Err(e);
        }

        // 6. Handle LLM result
        //
        // Omitted from Task 6b:
        //   - Fallback provider switching (consecutive_failures threshold)
        //   - SessionCorruption recovery (did_reset_session + messages.retain)
        //
        // Both are handled by engine_sse.rs for the SSE call-site.
        let response = match llm_result {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, iteration, "pipeline LLM call failed");
                let reason = format!("LLM call failed: {e}");
                // Emit the error text as TextDelta so the UI shows it
                let user_msg = crate::agent::error_classify::format_user_error(&e);
                match sink
                    .emit(PipelineEvent::Stream(StreamEvent::TextDelta(user_msg.clone())))
                    .await
                {
                    Ok(()) | Err(SinkError::Closed) => {}
                    Err(e2) => return Err(e2.into()),
                }
                let _ = sink
                    .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                        step_id,
                        finish_reason: "error".into(),
                    }))
                    .await;
                return Ok(ExecuteOutcome {
                    status: ExecuteStatus::Failed(reason),
                    final_text: user_msg,
                    thinking_json: None,
                    messages_len_at_end: messages.len(),
                    final_parent_msg_id: last_msg_id,
                });
            }
        };

        // Fire-and-forget usage recording (mirrors engine_sse.rs line 405)
        if let Some(ref usage) = response.usage {
            let db = engine.cfg().db.clone();
            let agent = engine.cfg().agent.name.clone();
            let provider_name = response.provider.clone()
                .unwrap_or_else(|| engine.cfg().provider.name().to_string());
            let model = response.model.clone().unwrap_or_default();
            let input = usage.input_tokens;
            let output = usage.output_tokens;
            tokio::spawn(async move {
                if let Err(e) = crate::db::usage::record_usage(
                    &db, &agent, &provider_name, &model, input, output, Some(session_id),
                )
                .await
                {
                    tracing::debug!(error = %e, "failed to record usage");
                }
            });
        }

        // 7. No tool calls → final text response.
        //    `partial` has already been streamed chunk-by-chunk to the sink by
        //    `forward_chunks_into_sink` during the LLM call — do NOT re-emit a
        //    batched TextDelta here or the UI will duplicate the whole response.
        if response.tool_calls.is_empty() {
            final_text = partial;
            final_thinking_blocks = response.thinking_blocks.clone();

            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                    step_id,
                    finish_reason: "stop".into(),
                }))
                .await;

            // Emit Finish — this is the normal done path
            match sink
                .emit(PipelineEvent::Stream(StreamEvent::Finish {
                    finish_reason: "stop".into(),
                    continuation: false,
                }))
                .await
            {
                Ok(()) => {}
                Err(SinkError::Closed) => {
                    return Ok(ExecuteOutcome {
                        status: ExecuteStatus::Interrupted("sink_closed"),
                        final_text,
                        thinking_json: None,
                        messages_len_at_end: messages.len(),
                        final_parent_msg_id: last_msg_id,
                    });
                }
                Err(e) => return Err(e.into()),
            }

            let thinking_json = if final_thinking_blocks.is_empty() {
                None
            } else {
                serde_json::to_value(&final_thinking_blocks).ok()
            };
            return Ok(ExecuteOutcome {
                status: ExecuteStatus::Done,
                final_text,
                thinking_json,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
            });
        }

        // 8. Tool calls present — append assistant message to context
        tracing::info!(
            iteration,
            max = loop_config.effective_max_iterations(),
            tools = response.tool_calls.len(),
            "executing tool calls (pipeline)"
        );

        // `partial` has already been streamed to the sink chunk-by-chunk by
        // `forward_chunks_into_sink` during the LLM call. We push it into the
        // in-memory `messages` vec and persist it to DB below for LLM-context
        // replay — we MUST NOT re-emit it to the sink (the UI would render the
        // reasoning text twice: once live, once duplicated before the tool card).
        tracing::debug!(
            bytes = partial.len(),
            "reasoning text already streamed to sink during LLM call; persisting to DB only"
        );
        messages.push(Message {
            role: MessageRole::Assistant,
            content: partial.clone(),
            tool_calls: Some(response.tool_calls.clone()),
            tool_call_id: None,
            thinking_blocks: vec![],
        });
        context_chars += partial.chars().count();

        // Persist the intermediate assistant (with tool_calls) to DB so
        // reload-from-active-path can reconstruct tool-use history.
        // Errors are logged but non-fatal — the in-memory context is already
        // correct, only DB replay degrades.
        let tc_json = serde_json::to_value(&response.tool_calls).ok();
        match sm
            .save_message_ex(
                session_id,
                "assistant",
                &partial,
                tc_json.as_ref(),
                None,
                Some(&agent_name),
                None,
                Some(last_msg_id),
            )
            .await
        {
            Ok(id) => last_msg_id = id,
            Err(e) => tracing::warn!(
                error = %e, session_id = %session_id,
                "failed to save intermediate assistant to DB"
            ),
        }

        // 9. Emit ToolCallStart + ToolCallArgs for each tool (UI feedback)
        for tc in &response.tool_calls {
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::ToolCallStart {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                }))
                .await;
            let args_text = serde_json::to_string(&tc.arguments).unwrap_or_default();
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::ToolCallArgs {
                    id: tc.id.clone(),
                    args_text,
                }))
                .await;
        }

        // 10. Execute tool batch via ToolExecutor (loop detection inside execute_batch)
        let tool_executor = engine
            .tool_executor
            .get()
            .expect("tool_executor not initialized");

        let loop_broken = match tool_executor
            .execute_batch(
                &response.tool_calls,
                &serde_json::Value::Null, // context — callers with msg can pass msg.context; Task 6b uses Null
                session_id,
                "",   // channel — not available in the unified pipeline path
                messages.iter().map(|m| m.content.len()).sum(),
                &mut loop_detector,
                loop_config.detect_loops,
            )
            .await
        {
            Ok(results) => {
                for (tc_id, tool_result) in &results {
                    // Extract rich-card / __file__: markers and emit File/RichCard
                    // stream events; for plain text both halves are the raw string.
                    let ToolResultParts {
                        display_result,
                        db_result,
                    } = extract_tool_result_events(tool_result, sink).await;

                    let _ = sink
                        .emit(PipelineEvent::Stream(StreamEvent::ToolResult {
                            id: tc_id.clone(),
                            result: display_result.clone(),
                        }))
                        .await;

                    let display_len = display_result.chars().count();
                    messages.push(Message {
                        role: MessageRole::Tool,
                        content: display_result,
                        tool_calls: None,
                        tool_call_id: Some(tc_id.clone()),
                        thinking_blocks: vec![],
                    });
                    context_chars += display_len;

                    // Persist tool result to DB with raw markers preserved so
                    // reload-from-active-path can reinstate File/RichCard events.
                    match sm
                        .save_message_ex(
                            session_id,
                            "tool",
                            &db_result,
                            None,
                            Some(tc_id),
                            None,
                            None,
                            Some(last_msg_id),
                        )
                        .await
                    {
                        Ok(id) => last_msg_id = id,
                        Err(e) => tracing::warn!(
                            error = %e, session_id = %session_id,
                            "failed to save tool result to DB"
                        ),
                    }
                }
                false // loop continues
            }
            Err(LoopBreak(reason)) => {
                if loop_nudge_count < loop_config.max_loop_nudges {
                    // Inject nudge message and continue (mirrors engine_sse.rs lines 575-599)
                    messages.push(Message {
                        role: MessageRole::System,
                        content: build_loop_nudge_message(reason.as_deref()),
                        tool_calls: None,
                        tool_call_id: None,
                        thinking_blocks: vec![],
                    });
                    loop_nudge_count += 1;
                    tracing::warn!(
                        agent = %engine.cfg().agent.name,
                        nudge_count = loop_nudge_count,
                        reason = ?reason,
                        "loop nudge injected (pipeline path)"
                    );
                    false // continue — nudge was injected
                } else {
                    // Max nudges exhausted — treat as Failed
                    tracing::error!(
                        agent = %engine.cfg().agent.name,
                        nudge_count = loop_nudge_count,
                        "max loop nudges reached, force-stopping agent (pipeline path)"
                    );
                    true // broken
                }
            }
        };

        let _ = sink
            .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                step_id: step_id.clone(),
                finish_reason: "tool-calls".into(),
            }))
            .await;

        // Loop break after max nudges → terminate with Failed
        if loop_broken {
            let reason = "loop_detected_max_nudges".to_string();
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::Finish {
                    finish_reason: "loop_detected".into(),
                    continuation: false,
                }))
                .await;
            return Ok(ExecuteOutcome {
                status: ExecuteStatus::Failed(reason),
                final_text,
                thinking_json: None,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
            });
        }
    }

    // ── Turn limit reached ────────────────────────────────────────────────────
    // All iterations exhausted without a clean stop. Emit Finish and return Done
    // with finish_reason = "turn_limit" (mirrors engine_sse.rs forced-final-call path,
    // but omits the extra LLM call — that optimization is Task 6b omitted scope).
    tracing::warn!(
        agent = %engine.cfg().agent.name,
        max = loop_config.effective_max_iterations(),
        "pipeline turn limit reached"
    );
    let _ = sink
        .emit(PipelineEvent::Stream(StreamEvent::Finish {
            finish_reason: "turn_limit".into(),
            continuation: false,
        }))
        .await;

    Ok(ExecuteOutcome {
        status: ExecuteStatus::Done,
        final_text,
        thinking_json: None,
        messages_len_at_end: messages.len(),
        final_parent_msg_id: last_msg_id,
    })
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Result of processing a tool result for SSE event emission.
///
/// `display_result` is what goes into the LLM context (markers stripped) and
/// the `ToolResult` event. `db_result` is preserved verbatim for DB storage
/// so reload/active_path rebuild can recover the full marker set.
struct ToolResultParts {
    display_result: String,
    db_result: String,
}

/// Extract inline markers (`__rich_card__:`, `__file__:`) from a tool result,
/// emit corresponding `StreamEvent`s via the sink, and return cleaned display
/// + raw DB text. Ported from the old `pipeline::entry::extract_tool_result_events`
///
/// Ensures the pipeline path has parity with the deleted engine_sse.rs behaviour.
async fn extract_tool_result_events<S: EventSink>(
    tool_result: &str,
    sink: &mut S,
) -> ToolResultParts {
    use crate::agent::engine::{FILE_PREFIX, RICH_CARD_PREFIX};

    if let Some(json_str) = tool_result.strip_prefix(RICH_CARD_PREFIX) {
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) {
            let card_type = data
                .get("card_type")
                .and_then(|v| v.as_str())
                .unwrap_or("table")
                .to_string();
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::RichCard {
                    card_type,
                    data,
                }))
                .await;
        }
        ToolResultParts {
            display_result: "Rich card displayed".to_string(),
            db_result: tool_result.to_string(),
        }
    } else if tool_result.contains(FILE_PREFIX) {
        let db_result = tool_result.to_string();
        let mut clean_lines: Vec<&str> = Vec::new();
        for line in tool_result.lines() {
            if let Some(json_str) = line.strip_prefix(FILE_PREFIX) {
                if let Ok(meta) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let url = meta.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let media_type = meta
                        .get("mediaType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("application/octet-stream");
                    if !url.is_empty() {
                        let _ = sink
                            .emit(PipelineEvent::Stream(StreamEvent::File {
                                url: url.to_string(),
                                media_type: media_type.to_string(),
                            }))
                            .await;
                    }
                }
            } else {
                clean_lines.push(line);
            }
        }
        let text = clean_lines.join("\n");
        let display_result = if text.is_empty() {
            "Image displayed inline in the chat. Do NOT use canvas or other tools to show it again.".to_string()
        } else {
            text
        };
        ToolResultParts {
            display_result,
            db_result,
        }
    } else {
        ToolResultParts {
            display_result: tool_result.to_string(),
            db_result: tool_result.to_string(),
        }
    }
}

/// Build the system nudge message injected when a tool-call loop is detected.
fn build_loop_nudge_message(reason: Option<&str>) -> String {
    let nudge_desc = reason.unwrap_or("repeating pattern");
    format!(
        "LOOP DETECTED: You have repeated the same sequence of actions ({desc}). \
         Change your approach entirely. If the task is too large for a single session, \
         tell the user and suggest breaking it into smaller steps. Do NOT retry the same approach.",
        desc = nudge_desc
    )
}

// ── Chunk forwarding helper ─────────────────────────────────────────────────
//
// Extracted so the forwarding contract can be pinned by unit tests without
// constructing a live `AgentEngine`. `execute()` uses this helper today to
// drive the LLM stream and emit `TextDelta` events into the sink.
//
// Contract pinned by the tests below:
//   * Each chunk arriving on `chunk_rx` is forwarded as a separate
//     `StreamEvent::TextDelta` emission to `sink` IN ORDER.
//   * Forwarding happens CONCURRENTLY with `llm_fut`, so text preceding a
//     tool-call is visible in the sink BEFORE the LLM future resolves.
//   * Sink `Closed` is swallowed (forwarding becomes a no-op, LLM future
//     continues to completion). Other `SinkError`s are returned as the
//     third tuple slot via `anyhow::Error` so the caller can decide to
//     bail. Fatal sink errors do NOT abort the LLM future.
//   * When `llm_fut` resolves, any chunks that raced past the select! tick
//     are drained via `try_recv` and emitted before the helper returns.
//
// Returns `(llm_result, concatenated_partial_text, first_fatal_sink_error)`.
pub(crate) async fn forward_chunks_into_sink<S, F, T, E>(
    llm_fut: F,
    mut chunk_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    sink: &mut S,
) -> (Result<T, E>, String, Option<anyhow::Error>)
where
    S: EventSink,
    F: std::future::Future<Output = Result<T, E>>,
{
    tokio::pin!(llm_fut);
    let mut partial = String::new();
    let mut first_err: Option<anyhow::Error> = None;

    // Emit a single chunk to the sink, updating `partial` and `first_err`.
    // Swallows `Closed` (forwarding becomes a no-op for future chunks); records
    // the first non-Closed SinkError so the caller can surface it after the
    // LLM future resolves. We intentionally do NOT abort the LLM future on sink
    // errors — that would leak the in-flight provider call.
    //
    // Reconnecting signal: if the chunk starts with `RECONNECTING_PREFIX`, it is
    // a control signal injected by `chat_stream_with_deadline_retry`. Emit a
    // `StreamEvent::Reconnecting` event instead of `TextDelta` and do NOT add
    // the chunk to `partial` (it must not appear in the accumulated response text).
    async fn emit_chunk<S: EventSink>(
        sink: &mut S,
        chunk: String,
        partial: &mut String,
        first_err: &mut Option<anyhow::Error>,
    ) {
        if let Some(rest) = chunk.strip_prefix(crate::agent::pipeline::llm_call::RECONNECTING_PREFIX) {
            let mut parts = rest.splitn(2, ':');
            let attempt = parts.next().and_then(|s| s.parse::<u32>().ok()).unwrap_or(1);
            let delay_ms = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(2000);
            match sink.emit(PipelineEvent::Stream(StreamEvent::Reconnecting { attempt, delay_ms })).await {
                Ok(()) | Err(SinkError::Closed) => {}
                Err(other) if first_err.is_none() => {
                    *first_err = Some(anyhow::Error::new(other));
                }
                Err(_) => {}
            }
            return; // Do NOT add to partial text accumulator
        }
        partial.push_str(&chunk);
        match sink.emit(PipelineEvent::Stream(StreamEvent::TextDelta(chunk))).await {
            Ok(()) | Err(SinkError::Closed) => {}
            Err(other) if first_err.is_none() => {
                *first_err = Some(anyhow::Error::new(other));
            }
            Err(_) => {}
        }
    }

    let res = loop {
        tokio::select! {
            // Bias the branch order so we drain pending chunks before polling
            // llm_fut again — otherwise a fast LLM that ships tokens and
            // resolves in the same tick could starve the chunk branch.
            biased;
            maybe_chunk = chunk_rx.recv() => {
                match maybe_chunk {
                    Some(chunk) => emit_chunk(sink, chunk, &mut partial, &mut first_err).await,
                    None => {
                        // Sender dropped; llm_fut must be about to resolve.
                        // Fall through and let the other branch win next tick.
                    }
                }
            }
            res = &mut llm_fut => {
                // Drain any buffered chunks that raced past the select! tick
                // so we don't lose trailing deltas.
                while let Ok(chunk) = chunk_rx.try_recv() {
                    emit_chunk(sink, chunk, &mut partial, &mut first_err).await;
                }
                break res;
            }
        }
    };

    (res, partial, first_err)
}

// ── Tests ────────────────────────────────────────────────────────────────────
//
// These tests pin the streaming contract of `forward_chunks_into_sink` without
// constructing a live AgentEngine. The `pipeline::execute::execute()` function
// itself still requires an engine for end-to-end testing — covered by the
// human-verified smoke checkpoint in the quick task plan.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::pipeline::sink::test_support::MockSink;
    use hydeclaw_types::{LlmResponse, ToolCall};
    use tokio::sync::mpsc;

    /// Build a minimal `LlmResponse` with the given tool_calls. Other fields are
    /// the serde defaults — the forwarder never inspects them.
    fn mk_response(tool_calls: Vec<ToolCall>) -> LlmResponse {
        LlmResponse {
            content: String::new(),
            tool_calls,
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

    fn text_deltas(sink: &MockSink) -> Vec<String> {
        sink.events
            .iter()
            .filter_map(|e| match e {
                PipelineEvent::Stream(StreamEvent::TextDelta(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Test A: Chunks arriving on `chunk_rx` during an LLM stream are forwarded
    /// to the sink as SEPARATE `TextDelta` events, not batched into a single
    /// end-of-turn emit. A batched implementation MUST fail this assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn streams_chunks_individually_during_no_tool_turn() {
        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel::<String>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        // LLM future: push 3 chunks, then wait for a signal before resolving
        // (with no tool_calls — mimics a plain text reply).
        let llm_fut = async move {
            chunk_tx.send("Hel".to_string()).unwrap();
            chunk_tx.send("lo ".to_string()).unwrap();
            chunk_tx.send("world".to_string()).unwrap();
            // Yield so the forwarder select! has a chance to tick.
            done_rx.await.unwrap();
            drop(chunk_tx); // close sender so recv() returns None
            Ok::<LlmResponse, anyhow::Error>(mk_response(vec![]))
        };

        let sink = MockSink::new();
        // Run the forwarder and the "signal-done" side in parallel.
        let forward = tokio::spawn(async move {
            let mut s = sink;
            let out = forward_chunks_into_sink(llm_fut, chunk_rx, &mut s).await;
            (out, s)
        });

        // Give the spawned forwarder time to observe all 3 chunks BEFORE
        // llm_fut resolves. This is the whole point: per-chunk emission
        // happens during the LLM call, not after.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        done_tx.send(()).unwrap();

        let ((llm_result, partial, sink_err), sink) = forward.await.unwrap();
        assert!(sink_err.is_none(), "no fatal sink error expected");
        assert!(llm_result.is_ok(), "llm future must resolve Ok");
        assert_eq!(partial, "Hello world");

        let deltas = text_deltas(&sink);
        // Per-chunk contract: at least 3 distinct TextDelta emissions whose
        // concatenated payloads equal "Hello world". A single batched
        // TextDelta("Hello world") MUST FAIL this assertion.
        assert!(
            deltas.len() >= 3,
            "expected >=3 TextDelta emissions (per-chunk), got {} (batched?): {:?}",
            deltas.len(),
            deltas
        );
        assert_eq!(deltas.concat(), "Hello world");
    }

    /// A sink that records events into a shared `Arc<Mutex<Vec<_>>>` so tests
    /// can observe emissions live while the LLM future is still pending.
    struct SharedSink {
        events: std::sync::Arc<std::sync::Mutex<Vec<PipelineEvent>>>,
    }
    impl EventSink for SharedSink {
        async fn emit(&mut self, ev: PipelineEvent) -> Result<(), SinkError> {
            self.events.lock().unwrap().push(ev);
            Ok(())
        }
    }

    fn deltas_of(events: &[PipelineEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                PipelineEvent::Stream(StreamEvent::TextDelta(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    /// Test C: A `__reconnecting__:` prefixed chunk must emit a
    /// `StreamEvent::Reconnecting` event and must NOT contribute to the
    /// partial text accumulator. A subsequent plain text chunk IS accumulated.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconnecting_prefix_emits_reconnecting_event_and_skips_partial() {
        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel::<String>();

        // LLM future: send reconnecting signal then a normal text chunk.
        let llm_fut = async move {
            chunk_tx.send("__reconnecting__:1:2000".to_string()).unwrap();
            chunk_tx.send("Hello".to_string()).unwrap();
            drop(chunk_tx);
            Ok::<LlmResponse, anyhow::Error>(mk_response(vec![]))
        };

        let sink = MockSink::new();
        let (out, sink) = {
            let mut s = sink;
            let out = forward_chunks_into_sink(llm_fut, chunk_rx, &mut s).await;
            (out, s)
        };

        let (_llm_result, partial, sink_err) = out;
        assert!(sink_err.is_none(), "no fatal sink error expected");

        // Partial text must be "Hello" only — the reconnecting signal must NOT be included.
        assert_eq!(partial, "Hello", "partial must not contain the reconnecting signal");

        // Exactly one Reconnecting event must have been emitted.
        let reconnecting_events: Vec<_> = sink.events.iter().filter_map(|e| {
            if let PipelineEvent::Stream(StreamEvent::Reconnecting { attempt, delay_ms }) = e {
                Some((*attempt, *delay_ms))
            } else {
                None
            }
        }).collect();
        assert_eq!(reconnecting_events.len(), 1, "expected exactly 1 Reconnecting event, got: {:?}", reconnecting_events);
        assert_eq!(reconnecting_events[0], (1, 2000), "Reconnecting payload mismatch");

        // Exactly one TextDelta("Hello") must have been emitted.
        let deltas = text_deltas(&sink);
        assert_eq!(deltas, vec!["Hello".to_string()], "expected TextDelta(Hello), got: {deltas:?}");
    }

    /// Test B: When the LLM call returns tool_calls, any text chunks that
    /// arrived during the call must be visible in the sink as TextDelta
    /// BEFORE the LLM future resolves — NOT silently swallowed and NOT
    /// batch-emitted only after the future returns.
    ///
    /// The test observes the shared sink WHILE the LLM future is blocked
    /// on `done_rx`. A batched implementation emits 0 TextDeltas at this
    /// observation point, which MUST fail this assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn emits_reasoning_text_before_tool_call() {
        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel::<String>();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let llm_fut = async move {
            chunk_tx.send("Let me think. ".to_string()).unwrap();
            // Block until the test releases us — the sink MUST already
            // contain the TextDelta by the time this await completes, or
            // the forwarder batched instead of streaming.
            done_rx.await.unwrap();
            drop(chunk_tx);
            Ok::<LlmResponse, anyhow::Error>(mk_response(vec![ToolCall {
                id: "tc_1".to_string(),
                name: "mock_tool".to_string(),
                arguments: serde_json::Value::Null,
            }]))
        };

        let shared = std::sync::Arc::new(std::sync::Mutex::new(Vec::<PipelineEvent>::new()));
        let mut sink = SharedSink { events: shared.clone() };
        let shared_probe = shared.clone();

        let forward = tokio::spawn(async move {
            forward_chunks_into_sink(llm_fut, chunk_rx, &mut sink).await
        });

        // Let the forwarder tick, observe the sink BEFORE the LLM future
        // resolves. Per-chunk emission MUST have pushed a TextDelta into
        // the shared vec by now.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        {
            let events = shared_probe.lock().unwrap();
            let deltas = deltas_of(&events);
            assert!(
                !deltas.is_empty(),
                "expected TextDelta in sink BEFORE llm_fut resolves, got events: {:?}",
                *events
            );
            assert_eq!(
                deltas.concat(),
                "Let me think. ",
                "reasoning text preceding tool-call must be streamed to sink before tool call, got deltas: {:?}",
                deltas
            );
        }

        // Now release the future so the test can finish cleanly.
        done_tx.send(()).unwrap();
        let (llm_result, partial, sink_err) = forward.await.unwrap();
        assert!(sink_err.is_none());
        let resp = llm_result.expect("llm future ok");
        assert_eq!(resp.tool_calls.len(), 1, "fixture returns one tool call");
        assert_eq!(partial, "Let me think. ");
    }
}
