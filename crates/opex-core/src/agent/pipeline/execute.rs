//! Main LLM+tools loop. Transport-agnostic via EventSink.
//!
//! See docs/superpowers/specs/2026-04-20-execution-pipeline-unification-design.md §3, §5
//! and docs/architecture/2026-05-06-llm-loop-unification-plan.md (Phase 66).
//!
//! # Scope
//!
//! This module implements the unified tool loop used by ALL transports:
//! - Happy path: N LLM calls with tool-call iterations
//! - Cancellation check at top of each iteration
//! - Sink closed → Interrupted
//! - LLM provider error (after retry exhaustion) → Failed
//! - LoopDetector trip (after max nudges) → Failed
//! - Turn limit reached → Done with finish_reason = "turn_limit"
//!
//! # Behaviour layers (Phase 66 — completed 2026-05-06)
//!
//! Five opt-in policies activated through [`BehaviourLayers`]:
//!
//! - [`FallbackPolicy`](super::behaviour::FallbackPolicy) — fallback provider
//!   switching on consecutive failures (consumed at line ~362).
//! - [`SessionRecoveryPolicy`](super::behaviour::SessionRecoveryPolicy) — message
//!   reset + retry on SessionCorruption (consumed at line ~331).
//! - [`AutoContinuePolicy`](super::behaviour::AutoContinuePolicy) — empty-response
//!   retry + nudge with `AUTO_CONTINUE_NUDGE` (consumed at line ~501).
//! - [`ToolPolicyOverride`](super::behaviour::ToolPolicyOverride) — applied at the
//!   bootstrap boundary in `engine/run.rs` (per-session policy override).
//! - [`ForcedFinalCallPolicy`](super::behaviour::ForcedFinalCallPolicy) — extra
//!   LLM call on loop break / turn limit to coerce a final response (consumed at
//!   lines ~852, ~909).
//!
//! SSE callers pass [`BehaviourLayers::none()`] for byte-identical legacy
//! semantics. Cron / RPC callers (`handle_isolated_via_pipeline` in
//! `engine/run.rs`) pass [`BehaviourLayers::for_cron`] to enable the cron-only
//! features above.
//!
//! Timeline warm-up replay is owned by bootstrap; execute receives the
//! already-warmed detector via [`BootstrapOutcome::loop_detector`].
//! Thinking-block stripping is owned by finalize.

use crate::agent::engine::AgentEngine;
use crate::agent::pipeline::behaviour::{BehaviourLayers, LayerRuntimeState};
use crate::agent::pipeline::bootstrap::BootstrapOutcome;
use crate::agent::pipeline::sink::{EventSink, PipelineEvent, SinkError};
use crate::agent::pipeline::tool_loop_helpers as helpers;
use crate::agent::stream_event::StreamEvent;
use crate::agent::tool_executor::ToolExecutor as _;
use opex_types::{Message, MessageRole};
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
    /// UUID pre-generated for the final assistant DB row.
    /// Matches the `messageId` sent in the `MessageStart` SSE event
    /// so the frontend's live buffer ID equals the DB row ID.
    pub assistant_message_id: Uuid,
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
/// All transports route through this function (Phase 66 unified path —
/// completed 2026-05-06). Callers configure cron-style behaviours via the
/// `layers` argument; SSE callers pass `BehaviourLayers::none()`.
/// See module doc for the full layer catalogue.
#[tracing::instrument(
    name = "pipeline.execute",
    skip_all,
    fields(
        session_id = %bootstrap_outcome.session_id,
        agent = %engine.cfg().agent.name,
        // Iteration count and final assistant id are recorded as the loop
        // progresses via `tracing::Span::current().record(...)`.
        iterations = tracing::field::Empty,
        assistant_message_id = tracing::field::Empty,
    )
)]
pub async fn execute<S: EventSink>(
    engine: &AgentEngine,
    bootstrap_outcome: BootstrapOutcome,
    sink: &mut S,
    cancel: CancellationToken,
    compressor: &mut crate::agent::compressor::Compressor,
    layers: &BehaviourLayers,
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
        incoming_context,
        channel,
        compressor: _, // passed separately as &mut parameter
        // CACHE-02: per-agent CLAUDE.md content for the third cache breakpoint.
        // Same value across the whole session (CLAUDE.md is invariant per turn);
        // cloned into each CallOptions site below so the value lives long enough
        // for both the main-loop call and the two forced-final-call paths.
        claude_md_content,
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
            assistant_message_id: uuid::Uuid::nil(),
        });
    }

    // Track the UUID of the most recent iteration's row — used as
    // assistant_message_id in finalize when the loop exits. Updated to a
    // freshly pre-allocated UUID at the top of every iteration so the SSE
    // step-start event, the live buffer assistantId, and the DB row id all
    // match. Eliminates the cross-source ID gap between intermediate live
    // ChatMessages and DB intermediate rows that previously forced
    // content-based dedup heuristics in the frontend.
    let mut assistant_msg_id = Uuid::nil();

    // ── Mutable loop state ───────────────────────────────────────────────────
    let loop_config = engine.tool_loop_config();
    let mut final_text = String::new();
    let mut final_thinking_blocks: Vec<opex_types::ThinkingBlock> = vec![];
    let mut context_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
    let mut loop_nudge_count: usize = 0;
    // Per-turn mutable state owned by the behaviour layers. Every counter
    // here is gated by `layers.<feature>.is_some()` checks below — when the
    // layer is `None`, the counter never advances and the legacy "no fallback"
    // / "no auto-continue" / "no recovery" semantics are preserved exactly.
    let mut layer_state = LayerRuntimeState::default();

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
                assistant_message_id: assistant_msg_id,
            });
        }

        // R-WATCHDOG heartbeat: refresh `activity_at` at the top of every
        // iteration. The watchdog reaps 'running' sessions whose
        // COALESCE(activity_at, last_message_at) is older than the per-agent
        // inactivity threshold; previously ONLY streaming TextDelta flushes
        // touched it (upsert_streaming_append), so a long silent step — a slow
        // LLM with delayed first token, a long tool chain, a subagent that
        // emits no text — starved the heartbeat and a perfectly live session
        // was killed as 'timeout'. Debounced to 10s in the DB and gated on
        // run_status='running', so it's near-free and cannot resurrect a
        // terminal session. Gives each iteration a full threshold window.
        crate::db::sessions::touch_session_activity(&engine.cfg().db, session_id)
            .await
            .ok();

        // 2. Pre-allocate the UUID this iteration's row will eventually be
        //    persisted under. Frontend uses this id to open a fresh live
        //    ChatMessage; once the row is saved (intermediate via
        //    spawn_persist_assistant_message, final via finalize), DB row
        //    id == live ChatMessage id → ID-based dedup just works.
        let iter_msg_id = Uuid::new_v4();
        assistant_msg_id = iter_msg_id;

        // S2 T2: bundle (iteration index, message_id) into a single struct.
        // Threaded into StepStart, the intermediate-row step_id DB column,
        // and any other site that needs to identify "which iteration is this".
        let iteration_id = opex_types::ids::IterationId {
            index: iteration as u32,
            message_id: opex_types::ids::MessageId::from(iter_msg_id),
        };

        // For the very first iteration emit a legacy `MessageStart` event so
        // existing frontend code paths that bind on the SSE `start` discriminator
        // continue to work. Subsequent iterations rely on the per-step
        // message_id field below.
        if iteration == 0 {
            match sink
                .emit(PipelineEvent::Stream(StreamEvent::MessageStart {
                    message_id: opex_types::ids::MessageId::from(iter_msg_id),
                }))
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
                        assistant_message_id: iter_msg_id,
                    });
                }
                Err(e) => return Err(e.into()),
            }
        }

        // 3. Emit StepStart with the iteration's row UUID. Frontend opens a
        //    new live ChatMessage with this id (committing the previous one
        //    as done) so each iteration is structurally isolated.
        //
        // `step_id` (the legacy `step_{N}` String) is still used by the
        // StepFinish events below — wire format unchanged. StepStart now
        // carries an `IterationId` struct (T2): the SSE converter rebuilds
        // the legacy `step_{N}` string from `iteration.index` so frontends
        // observe a byte-identical payload.
        let step_id = format!("step_{}", iteration_id.index);
        match sink
            .emit(PipelineEvent::Stream(StreamEvent::StepStart {
                iteration: iteration_id,
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
                    assistant_message_id: assistant_msg_id,
                });
            }
            Err(e) => return Err(e.into()),
        }

        // 3. Compact tool results to stay within context budget.
        //    Use the effective model so the window lookup matches the value
        //    resolved at bootstrap (override-aware).
        crate::agent::pipeline::context::compact_tool_results(
            &engine.current_model(),
            engine.cfg().agent.compaction.as_ref(),
            &mut messages,
            &mut context_chars,
        );

        // Proactive compression: check token budget from last response
        if let Some(cmp_cfg) = engine
            .cfg()
            .agent
            .compaction
            .as_ref()
            .filter(|c| c.enabled && compressor.should_compress(c))
        {
            let active_provider: &dyn crate::agent::providers::LlmProvider =
                engine.cfg().compaction_provider
                    .as_deref()
                    .unwrap_or_else(|| engine.cfg().provider.as_ref());
            if let Err(e) = crate::agent::history::compress_messages(
                &mut messages,
                compressor,
                cmp_cfg,
                active_provider,
                Some(engine.cfg().agent.language.as_str()),
                &engine.cfg().db,
                session_id,
            ).await {
                tracing::warn!(error = %e, "proactive compression failed, continuing");
            }
        }

        // 4. Call LLM with a forwarder that emits chunks directly to the sink
        //    as they arrive. No spawned task, no oneshot, no batching — the
        //    sink stays owned by `execute` and `forward_chunks_into_sink` drives
        //    a `tokio::select!` over (chunk_rx, llm_fut) so `TextDelta`s land in
        //    the sink interleaved with the LLM call. Contract pinned by
        //    `tests::streams_chunks_individually_during_no_tool_turn` and
        //    `tests::emits_reasoning_text_before_tool_call` below.
        // Bounded channel (1024 chunks): provides backpressure so slow sinks
        // cannot cause unbounded memory growth with large LLM responses.
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<String>(1024);
        // When the fallback layer is engaged for this turn, the live
        // provider points at the fallback Arc instead of the engine's
        // primary. Either way, `chat_stream_with_deadline_retry` takes a
        // `&dyn LlmProvider`, so the call shape is identical.
        let primary_provider = engine.cfg().provider.as_ref();
        let live_provider: &dyn crate::agent::providers::LlmProvider =
            match (layer_state.using_fallback, layer_state.fallback_provider.as_ref()) {
                (true, Some(fb)) => fb.as_ref(),
                _ => primary_provider,
            };
        let run_max = live_provider.run_max_duration_secs();
        let call_opts = crate::agent::providers::CallOptions {
            thinking_level: engine.state().thinking_level.load(std::sync::atomic::Ordering::Relaxed),
            // CACHE-02: thread CLAUDE.md (loaded once during bootstrap) into
            // every LLM call. Anthropic uses it as a third cache breakpoint;
            // other providers ignore the field (CACHE-04).
            claude_md_content: claude_md_content.clone(),
        };
        let llm_fut = crate::agent::pipeline::llm_call::chat_stream_with_deadline_retry(
            live_provider,
            &mut messages,
            &tools,
            chunk_tx,
            engine,
            &cancel,
            run_max,
            session_id,
            &sm,
            call_opts,
        );

        // 5. Drive the LLM future and the chunk forwarder concurrently.
        let (llm_result, partial, sink_fatal) =
            forward_chunks_into_sink(llm_fut, chunk_rx, sink).await;
        if let Some(e) = sink_fatal {
            return Err(e);
        }

        // 6. Handle LLM result
        //
        // Behaviour layers consulted (when enabled by the caller):
        //   * `fallback_provider` — on consecutive failures, lazily build a
        //     fallback provider and swap the live provider for the rest of
        //     the turn. SSE callers leave the layer `None` and get the
        //     unchanged "first error returns Failed" semantics.
        //   * `session_recovery` — on `LlmErrorClass::SessionCorruption`,
        //     reset to system+user once and continue. (Wired in Phase 5.)
        let response = match llm_result {
            Ok(r) => {
                // Successful call — reset the consecutive-failure counter so
                // a temporary blip on the primary doesn't permanently push us
                // toward the fallback. No-op when the fallback layer is off.
                layer_state.consecutive_failures = 0;
                r
            }
            Err(e) => {
                // Session-recovery layer (A3 in the divergent feature map).
                // Must run BEFORE the fallback layer — a SessionCorruption
                // error shouldn't increment the consecutive-failure counter
                // that drives fallback.
                if let Some(ref recovery) = layers.session_recovery {
                    let class = crate::agent::error_classify::classify(&e);
                    if class == crate::agent::error_classify::LlmErrorClass::SessionCorruption
                        && !layer_state.did_reset_session
                    {
                        layer_state.did_reset_session = true;
                        tracing::warn!(error = %e, "session corrupted, resetting context");
                        messages.retain(|m| m.role == MessageRole::System);
                        messages.push(Message {
                            role: MessageRole::User,
                            content: recovery.original_user_text.clone(),
                            tool_calls: None,
                            tool_call_id: None,
                            thinking_blocks: vec![],
                            db_id: None,
                        });
                        context_chars =
                            messages.iter().map(|m| m.content.chars().count()).sum();
                        let _ = sink
                            .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                                step_id,
                                finish_reason: "session_recovery".into(),
                            }))
                            .await;
                        continue;
                    }
                }

                // Fallback layer (A1 in the divergent feature map). Increment
                // the counter and, on threshold, lazily build the fallback
                // provider and `continue` the turn with it.
                if let Some(ref fb_policy) = layers.fallback_provider {
                    layer_state.consecutive_failures += 1;
                    if !layer_state.using_fallback
                        && layer_state.consecutive_failures >= fb_policy.consecutive_failure_threshold
                    {
                        if layer_state.fallback_provider.is_none() {
                            layer_state.fallback_provider =
                                engine.create_fallback_provider().await;
                        }
                        if layer_state.fallback_provider.is_some() {
                            layer_state.using_fallback = true;
                            layer_state.consecutive_failures = 0;
                            tracing::warn!(
                                agent = %engine.cfg().agent.name,
                                iteration,
                                "switching to fallback provider after consecutive failures"
                            );
                            // Emit StepFinish for the failed step so the
                            // frontend stops the spinner; then `continue`
                            // — the next iteration will use the fallback
                            // provider via the live_provider selector above.
                            let _ = sink
                                .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                                    step_id,
                                    finish_reason: "fallback_switch".into(),
                                }))
                                .await;
                            continue;
                        }
                    }
                }

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
                    assistant_message_id: assistant_msg_id,
                });
            }
        };

        // Emit Usage event BEFORE fire-and-forget DB record so a sink disconnect
        // during the DB await doesn't lose the event for the UI context bar.
        if let Some(ref usage) = response.usage {
            let _ = sink.emit(PipelineEvent::Stream(StreamEvent::Usage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: usage.cache_read_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                reasoning_tokens: usage.reasoning_tokens,
            })).await;
            // Update compressor's token count so proactive compression can fire
            // on the next iteration when the context budget is exceeded.
            compressor.update_token_count(usage.input_tokens);
        } else {
            // Model didn't report usage (e.g. some Ollama models). Fall back to a
            // char-based estimate (÷4) so compression can still trigger when the
            // context grows large.
            let estimated = (context_chars / 4) as u32;
            if estimated > compressor.last_prompt_tokens {
                compressor.update_token_count(estimated);
            }
        }

        // Fire-and-forget usage recording (mirrors engine_sse.rs line 405)
        if let Some(ref usage) = response.usage {
            let db = engine.cfg().db.clone();
            let agent = engine.cfg().agent.name.clone();
            let provider_name = response.provider.clone()
                .unwrap_or_else(|| engine.cfg().provider.name().to_string());
            let model = response.model.clone().unwrap_or_default();
            // Clone usage by-value so the spawned task gets owned data.
            // Extended fields are subsets of input/output (NOT additive); persisting
            // them lets dashboards split the bill by cache hits + reasoning.
            let usage = usage.clone();
            // spawn_traced so the usage INSERT shows up as a child of
            // the current `pipeline.execute` span in Jaeger — lets us
            // correlate "slow request" traces with "slow DB write" tail.
            crate::trace_propagation::spawn_traced(async move {
                if let Err(e) = crate::db::usage::record_usage(
                    &db, &agent, &provider_name, &model, Some(session_id), &usage,
                )
                .await
                {
                    tracing::warn!(
                        error = %e,
                        agent = %agent,
                        provider = %provider_name,
                        model = %model,
                        session_id = ?session_id,
                        "failed to record usage"
                    );
                }
            });
        }

        // 7. No tool calls → final text response.
        //    `partial` has already been streamed chunk-by-chunk to the sink by
        //    `forward_chunks_into_sink` during the LLM call — do NOT re-emit a
        //    batched TextDelta here or the UI will duplicate the whole response.
        if response.tool_calls.is_empty() {
            // Strip thinking-block content from the final text. Some
            // providers (Anthropic extended thinking) emit reasoning
            // text alongside the regular response; we keep the thinking
            // structurally separate via `response.thinking_blocks`.
            let stripped = crate::agent::thinking::strip_thinking(&partial);

            // ── Auto-continue / empty-retry layers ────────────────────────
            //
            // When `AutoContinuePolicy` is engaged, two recovery paths fire
            // before we accept the response as final:
            //   * If the response is empty AND the policy has `retry_on_empty`
            //     enabled, we retry once — some providers return empty bodies
            //     on transient backpressure (Ollama 503-like states) and the
            //     next attempt usually succeeds.
            //   * If the response is non-empty but `looks_incomplete()` —
            //     describes remaining work without executing it — we push the
            //     `AUTO_CONTINUE_NUDGE` system message and retry.
            //
            // SSE callers leave the layer `None` and skip both paths entirely.
            if let Some(ref ac_policy) = layers.auto_continue {
                // Empty-retry path (one shot per turn).
                if ac_policy.retry_on_empty
                    && stripped.is_empty()
                    && layer_state.empty_retry_count < 1
                {
                    layer_state.empty_retry_count += 1;
                    tracing::warn!(
                        iteration,
                        "LLM returned empty response, retrying once"
                    );
                    let _ = sink
                        .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                            step_id,
                            finish_reason: "empty_retry".into(),
                        }))
                        .await;
                    continue;
                }

                // Auto-continue path (capped per-turn).
                if layer_state.auto_continue_count < ac_policy.max_continues
                    && !stripped.is_empty()
                    && crate::agent::thinking::looks_incomplete(&stripped)
                {
                    layer_state.auto_continue_count += 1;
                    tracing::info!(
                        iteration,
                        count = layer_state.auto_continue_count,
                        max = ac_policy.max_continues,
                        "auto-continue: response looks incomplete, nudging LLM"
                    );

                    // Spawn the operator notification, same way
                    // handle_isolated did. `spawn_traced` keeps the trace
                    // context attached so the notification INSERT shows up
                    // under the originating `pipeline.execute` span.
                    if let Some(ref ui_tx) = engine.state().ui_event_tx {
                        let db = engine.cfg().db.clone();
                        let ui_tx = ui_tx.clone();
                        let agent_name_for_notify = engine.cfg().agent.name.clone();
                        let cnt = layer_state.auto_continue_count;
                        let max_cnt = ac_policy.max_continues;
                        crate::trace_propagation::spawn_traced(async move {
                            crate::gateway::notify(
                                &db,
                                &ui_tx,
                                "auto_continue",
                                &format!("Auto-continue: {agent_name_for_notify}"),
                                &format!(
                                    "Agent continued unfinished task (attempt {cnt}/{max_cnt})"
                                ),
                                serde_json::json!({"agent": agent_name_for_notify}),
                            )
                            .await
                            .ok();
                        });
                    }

                    // Push nudge into LLM context, advance budget tracker,
                    // emit StepFinish for this iteration so the next iteration
                    // opens a clean StepStart.
                    messages.push(Message {
                        role: MessageRole::User,
                        content: crate::agent::pipeline::behaviour::AUTO_CONTINUE_NUDGE.to_string(),
                        tool_calls: None,
                        tool_call_id: None,
                        thinking_blocks: vec![],
                        db_id: None,
                    });
                    context_chars += crate::agent::pipeline::behaviour::AUTO_CONTINUE_NUDGE.len();
                    let _ = sink
                        .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                            step_id,
                            finish_reason: "auto_continue".into(),
                        }))
                        .await;
                    continue;
                }
            }

            // Either no auto-continue layer was engaged, or the response
            // passed the layer's checks — accept it as final.
            final_text = stripped;
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
                        assistant_message_id: assistant_msg_id,
                    });
                }
                Err(e) => return Err(e.into()),
            }

            let thinking_json = if final_thinking_blocks.is_empty() {
                None
            } else {
                serde_json::to_value(&final_thinking_blocks).ok()
            };
            // Record final span fields so the OTel trace shows iteration
            // count and the assistant message id without parsing logs.
            tracing::Span::current().record("iterations", iteration + 1);
            tracing::Span::current()
                .record("assistant_message_id", tracing::field::display(assistant_msg_id));
            return Ok(ExecuteOutcome {
                status: ExecuteStatus::Done,
                final_text,
                thinking_json,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
                assistant_message_id: assistant_msg_id,
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
            thinking_blocks: response.thinking_blocks.clone(),
            db_id: None,
        });
        context_chars += partial.chars().count();

        // Persist the intermediate assistant (with tool_calls) to DB so
        // reload-from-active-path can reconstruct tool-use history.
        //
        // Detached via `spawn_persist_assistant_message` so the insert
        // survives parent-task cancellation (e.g. SSE client disconnect →
        // engine task abort) between here and the spawned tool-result inserts
        // below. A synchronous `save_message_ex(...).await` here would leave
        // a window: cancel during the await drops the assistant row, then
        // tool messages persisted by `execute_batch` reference a parent_id
        // that doesn't exist → chain broken on reload.
        //
        // Idempotency: pre-generated UUID + `ON CONFLICT (id) DO NOTHING` in
        // `save_message_ex_with_id` mean retries are safe.
        let (tc_json, tb_json) = helpers::encode_intermediate_persist_payload(
            &response.tool_calls,
            &response.thinking_blocks,
        );
        // Use the per-iteration UUID we already emitted in StepStart so the
        // intermediate DB row id matches the live ChatMessage id the frontend
        // built from the same SSE event. Pure ID-based dedup downstream.
        // step_id = iteration index lets analytics group intermediate rows
        // of one turn by their tool-loop position.
        crate::agent::pipeline::parallel::spawn_persist_assistant_message(
            &engine.cfg().db,
            iter_msg_id,
            session_id,
            &agent_name,
            &partial,
            tc_json.as_ref(),
            tb_json.as_ref(),
            Some(last_msg_id),
            Some(iteration_id.index as i32),
        );
        last_msg_id = iter_msg_id;

        // 9. Emit ToolCallStart + ToolCallArgs for each tool (UI feedback)
        //
        // T3: allocate one ParallelBatchId per turn IF this turn has ≥2 tool
        // calls (the threshold the spec calls out). Stamped onto every
        // ToolCallStart SSE event AND threaded into `execute_batch` so the
        // persistence layer can attach it to `messages.parallel_batch_id`
        // for tools that actually run in the parallel `join_all`. Single
        // tool turns leave it None — wire format stays byte-identical to
        // pre-T3.
        let parallel_batch_id: Option<opex_types::ids::ParallelBatchId> =
            if response.tool_calls.len() >= 2 {
                Some(opex_types::ids::ParallelBatchId::new())
            } else {
                None
            };
        for tc in &response.tool_calls {
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::ToolCallStart {
                    id: tc.id.clone(),
                    name: tc.name.clone(),
                    parallel_batch_id,
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
        // T6: above takes ToolCallId via ToolCall.id (newtype). No conversion
        // needed — tc.id is already typed.

        // 9b. Interrupted-verify guard (autonomous re-drive only). If the most
        //     recent tool outcome is an un-cleared [interrupted:verify] marker AND
        //     this batch includes a non-idempotent tool, the prior side-effect may
        //     already have happened — refuse to auto-dispatch and inject a verify
        //     nudge as the result for every call instead, then continue. Bounded:
        //     the injected result is not an [interrupted:verify] marker, so the
        //     guard won't fire again next iteration. Defense-in-depth atop the
        //     committed-result cache-replay (a persisted role='tool' never re-runs).
        if layers.interrupted_verify_guard.is_some()
            && crate::agent::pipeline::behaviour::should_block_interrupted_batch(
                &messages,
                &response.tool_calls,
            )
        {
            tracing::warn!(
                session = %session_id,
                tools = response.tool_calls.len(),
                "interrupted-verify guard: refusing to auto-repeat batch after a lost tool result"
            );
            for tc in &response.tool_calls {
                let _ = sink
                    .emit(PipelineEvent::Stream(StreamEvent::ToolResult {
                        id: tc.id.clone(),
                        result: crate::agent::pipeline::behaviour::INTERRUPTED_VERIFY_BLOCK_RESULT
                            .to_string(),
                    }))
                    .await;
                let blocked_id = Uuid::new_v4();
                crate::agent::pipeline::parallel::spawn_persist_tool_message(
                    &engine.cfg().db,
                    blocked_id,
                    session_id,
                    agent_name.as_str(),
                    tc.id.as_str(),
                    crate::agent::pipeline::behaviour::INTERRUPTED_VERIFY_BLOCK_RESULT,
                    Some(last_msg_id),
                    parallel_batch_id,
                );
                last_msg_id = blocked_id;
                messages.push(Message {
                    role: MessageRole::Tool,
                    content: crate::agent::pipeline::behaviour::INTERRUPTED_VERIFY_BLOCK_RESULT
                        .to_string(),
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                    thinking_blocks: vec![],
                    db_id: None,
                });
                context_chars += crate::agent::pipeline::behaviour::INTERRUPTED_VERIFY_BLOCK_RESULT
                    .chars()
                    .count();
            }
            continue;
        }

        // 10. Execute tool batch via ToolExecutor (loop detection inside execute_batch)
        //
        // Pass `persist_ctx` so each tool result is persisted via a detached
        // `tokio::spawn` BEFORE `execute_batch` returns. This closes the
        // cancellation gap that previously left tool messages unsaved when
        // the engine task was aborted between batch return and the for-loop
        // below (e.g. SSE client disconnect). See parallel.rs::ToolPersistCtx
        // and `spawn_persist_tool_message`.
        let tool_executor = engine
            .tool_executor
            .get()
            .expect("tool_executor not initialized");

        // Cancel check immediately before tool dispatch so a user pressing Stop
        // during a long tool run (code_exec, heavy workspace_write) gets an
        // ~1s response instead of waiting for the full tool to complete.
        if cancel.is_cancelled() {
            tracing::info!(session = %session_id, "request cancelled — before tool dispatch");
            return Ok(ExecuteOutcome {
                status: ExecuteStatus::Interrupted("cancel_token"),
                final_text,
                thinking_json: None,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
                assistant_message_id: assistant_msg_id,
            });
        }

        // R-WATCHDOG heartbeat (second touch): refresh `activity_at` right
        // before dispatching the tool batch. When the LLM call already took
        // >10s, the iteration-top touch is now stale, so this gives the tool
        // batch (which may include long tools / approval waits) its own fresh
        // watchdog window. Debounced — coalesces with the top touch when the
        // LLM call was fast.
        crate::db::sessions::touch_session_activity(&engine.cfg().db, session_id)
            .await
            .ok();

        let persist_ctx = crate::agent::pipeline::parallel::ToolPersistCtx {
            agent_name: agent_name.as_str(),
            initial_parent: Some(last_msg_id),
        };
        let outcome = tool_executor
            .execute_batch(
                &response.tool_calls,
                &incoming_context, // chat_id/message_id from originating channel (Telegram, etc.)
                session_id,
                channel.as_str(),
                messages.iter().map(|m| m.content.len()).sum(),
                &mut loop_detector,
                loop_config.detect_loops,
                Some(&persist_ctx),
                parallel_batch_id,
                &[], // top-level (handle_sse / handle_with_status), not a subagent
            )
            .await;
        // Always emit ToolResult for completed tools, even if a loop break
        // happened mid-batch. Otherwise the frontend's per-tool spinner stays
        // forever for any tool that finished but landed in the same batch
        // as the loop-break trigger.
        let loop_broken = {
            {
                for batch in &outcome.results {
                    let tc_id = &batch.tool_call_id;
                    let tool_result = &batch.result;
                    // Extract rich-card / __file__: markers and emit File/RichCard
                    // stream events; for plain text both halves are the raw string.
                    let ToolResultParts {
                        display_result,
                        db_result: _, // already persisted in execute_batch
                    } = extract_tool_result_events(tool_result, sink).await;

                    let tc_id_typed = opex_types::ids::ToolCallId::new(tc_id.clone());
                    let _ = sink
                        .emit(PipelineEvent::Stream(StreamEvent::ToolResult {
                            id: tc_id_typed.clone(),
                            result: display_result.clone(),
                        }))
                        .await;

                    let display_len = display_result.chars().count();
                    messages.push(Message {
                        role: MessageRole::Tool,
                        content: display_result,
                        tool_calls: None,
                        tool_call_id: Some(tc_id_typed),
                        thinking_blocks: vec![],
            db_id: None,

                    });
                    context_chars += display_len;

                    // Tool message persistence is now handled inside
                    // `execute_batch` (detached `tokio::spawn` so it
                    // survives parent-task cancellation). We only thread
                    // `last_msg_id` through the chain so the final
                    // assistant reply links onto the tail.
                    if let Some(persisted) = batch.tool_msg_id {
                        last_msg_id = persisted;
                    }
                }
            }
            // Loop-nudge bookkeeping is shared with the legacy
            // `handle_isolated` path via `tool_loop_helpers::apply_loop_nudge`
            // — single source of truth for the system-message wording and the
            // detector reset on nudge.
            helpers::apply_loop_nudge(
                &mut messages,
                &outcome.loop_break,
                &mut loop_nudge_count,
                loop_config.max_loop_nudges,
                &mut loop_detector,
                &engine.cfg().agent.name,
            )
            .loop_broken
        };

        let _ = sink
            .emit(PipelineEvent::Stream(StreamEvent::StepFinish {
                step_id: step_id.clone(),
                finish_reason: "tool-calls".into(),
            }))
            .await;

        // Loop break after max nudges → terminate.
        if loop_broken {
            // Forced-final-call layer also fires on the loop-broken path
            // so cron jobs return a natural-language explanation rather
            // than a raw "loop_detected_max_nudges" reason string. SSE
            // callers leave the layer `None` and get the legacy Failed
            // status with the reason intact.
            let (status, finish_reason) = if layers.forced_final_call.is_some() {
                match engine
                    .cfg()
                    .provider
                    .chat(
                        &messages,
                        &[],
                        crate::agent::providers::CallOptions {
                            // CACHE-02: forced-final-call (loop-broken path) is on
                            // the SAME engine.cfg().provider. For Anthropic agents
                            // with prompt_cache, omitting claude_md_content here
                            // would silently invalidate the third breakpoint on
                            // this code path (cost regression on loop-break
                            // sessions). Reuse the bootstrap-bound value.
                            claude_md_content: claude_md_content.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    Ok(forced) => {
                        final_text = crate::agent::thinking::strip_thinking(&forced.content);
                        (ExecuteStatus::Done, "loop_detected")
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "forced final LLM call after loop-break failed");
                        final_text = crate::agent::error_classify::format_user_error(&e);
                        (ExecuteStatus::Done, "loop_detected")
                    }
                }
            } else {
                (
                    ExecuteStatus::Failed("loop_detected_max_nudges".to_string()),
                    "loop_detected",
                )
            };
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::Finish {
                    finish_reason: finish_reason.into(),
                    continuation: false,
                }))
                .await;
            return Ok(ExecuteOutcome {
                status,
                final_text,
                thinking_json: None,
                messages_len_at_end: messages.len(),
                final_parent_msg_id: last_msg_id,
                assistant_message_id: assistant_msg_id,
            });
        }
    }

    // ── Turn limit reached ────────────────────────────────────────────────────
    // All iterations exhausted without a clean stop.
    tracing::warn!(
        agent = %engine.cfg().agent.name,
        max = loop_config.effective_max_iterations(),
        "pipeline turn limit reached"
    );

    // Forced-final-call layer (A5 in the divergent feature map). When
    // engaged, makes one extra non-tools LLM call to coax a final
    // natural-language summary into `final_text` before we return. SSE
    // callers leave the layer `None` and get the legacy "no extra call,
    // just emit Finish { reason: turn_limit }" semantics.
    if layers.forced_final_call.is_some() {
        match engine
            .cfg()
            .provider
            .chat(
                &messages,
                &[],
                crate::agent::providers::CallOptions {
                    // CACHE-02: same rationale as the loop-break forced-final
                    // path — the third breakpoint must be present on every
                    // call to engine.cfg().provider so cache hits cover this
                    // code path too.
                    claude_md_content: claude_md_content.clone(),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(forced) => {
                final_text = crate::agent::thinking::strip_thinking(&forced.content);
            }
            Err(e) => {
                tracing::error!(error = %e, "forced final LLM call failed");
                final_text = crate::agent::error_classify::format_user_error(&e);
            }
        }
    }

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
        assistant_message_id: assistant_msg_id,
    })
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Result of processing a tool result for SSE event emission.
///
/// `display_result` is what goes into the LLM context (markers stripped) and
/// the `ToolResult` event. `db_result` is preserved verbatim for DB storage
/// so reload/active_path rebuild can recover the full marker set.
#[allow(dead_code)] // db_result kept for tests that assert marker preservation.
struct ToolResultParts {
    display_result: String,
    /// Raw tool result with markers preserved. Persistence now happens
    /// upstream (detached, inside `execute_batch`) so this field is unused
    /// in the production path; kept for tests that assert marker preservation.
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
    use crate::agent::engine::{FILE_PREFIX, RICH_CARD_PREFIX, TOOL_CALL_PREFIX};

    if let Some(json_str) = tool_result.strip_prefix(RICH_CARD_PREFIX) {
        // A card may carry a `text` field — the readable version shown to the
        // model (and text-only channels). Falls back to a generic note.
        let mut display = "Rich card displayed".to_string();
        if let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) {
            let card_type = data
                .get("card_type")
                .and_then(|v| v.as_str())
                .unwrap_or("table")
                .to_string();
            if let Some(t) = data.get("text").and_then(|v| v.as_str()) {
                display = t.to_string();
            }
            let _ = sink
                .emit(PipelineEvent::Stream(StreamEvent::RichCard {
                    card_type,
                    data,
                }))
                .await;
        }
        ToolResultParts {
            display_result: display,
            db_result: tool_result.to_string(),
        }
    } else if tool_result.contains(FILE_PREFIX) || tool_result.contains(TOOL_CALL_PREFIX) {
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
                    // F042: only honor a File marker whose URL is a signed
                    // local upload path (`/api/uploads/…`, what
                    // save_binary_to_uploads produces). An untrusted tool
                    // (web/browser/MCP) can splice a `__file__:` line into its
                    // output with an ARBITRARY absolute URL; emitting it verbatim
                    // would load an attacker-controlled resource inline in the
                    // operator's browser (IP/UA leak, phishing, forged "tool
                    // produced this file" UI). Reject anything not root-relative
                    // /api/uploads/.
                    if url.starts_with("/api/uploads/") {
                        let _ = sink
                            .emit(PipelineEvent::Stream(StreamEvent::File {
                                url: url.to_string(),
                                media_type: media_type.to_string(),
                            }))
                            .await;
                    } else if !url.is_empty() {
                        tracing::warn!(
                            url = %url,
                            "rejected File marker URL (not a signed /api/uploads path) — possible forged marker in untrusted tool output"
                        );
                    }
                }
            } else if let Some(json_str) = line.strip_prefix(TOOL_CALL_PREFIX) {
                // Nested tool-call event from codemode. Emit as a
                // ToolCallStart/ToolResult pair so the UI timeline shows the
                // nested calls under the parent code_orchestrate tool call.
                if let Ok(evt) = serde_json::from_str::<serde_json::Value>(json_str) {
                    let _ = sink
                        .emit(PipelineEvent::Stream(StreamEvent::ToolCallArgs {
                            id: opex_types::ids::ToolCallId::new(
                                evt.get("tool").and_then(|v| v.as_str()).unwrap_or("unknown"),
                            ),
                            args_text: evt.get("input").map(|v| v.to_string()).unwrap_or_default(),
                        }))
                        .await;
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

// `build_loop_nudge_message` moved to `pipeline::tool_loop_helpers` so the
// streaming and RPC paths share the wording. See that module's tests.

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
    mut chunk_rx: tokio::sync::mpsc::Receiver<String>,
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
    use opex_types::{LlmResponse, ToolCall};
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
        let (chunk_tx, chunk_rx) = mpsc::channel::<String>(1024);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        // LLM future: push 3 chunks, then wait for a signal before resolving
        // (with no tool_calls — mimics a plain text reply).
        let llm_fut = async move {
            chunk_tx.send("Hel".to_string()).await.unwrap();
            chunk_tx.send("lo ".to_string()).await.unwrap();
            chunk_tx.send("world".to_string()).await.unwrap();
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
        let (chunk_tx, chunk_rx) = mpsc::channel::<String>(1024);

        // LLM future: send reconnecting signal then a normal text chunk.
        let llm_fut = async move {
            chunk_tx.send("__reconnecting__:1:2000".to_string()).await.unwrap();
            chunk_tx.send("Hello".to_string()).await.unwrap();
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
        let (chunk_tx, chunk_rx) = mpsc::channel::<String>(1024);
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let llm_fut = async move {
            chunk_tx.send("Let me think. ".to_string()).await.unwrap();
            // Block until the test releases us — the sink MUST already
            // contain the TextDelta by the time this await completes, or
            // the forwarder batched instead of streaming.
            done_rx.await.unwrap();
            drop(chunk_tx);
            Ok::<LlmResponse, anyhow::Error>(mk_response(vec![ToolCall {
                id: "tc_1".into(),
                name: "mock_tool".to_string(),
                arguments: serde_json::Value::Null,
                thought_signature: None,
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

    // `loop_nudge_*` tests moved to `pipeline::tool_loop_helpers::tests`.

    // ── extract_tool_result_events tests ──────────────────────────────────────

    #[tokio::test]
    async fn tool_result_plain_text_passes_through() {
        let mut sink = MockSink::new();
        let parts = extract_tool_result_events("hello world", &mut sink).await;
        assert_eq!(parts.display_result, "hello world");
        assert_eq!(parts.db_result, "hello world");
        // Plain text: no special events emitted
        assert!(sink.events.is_empty(), "expected no events for plain text, got: {:?}", sink.events);
    }

    #[tokio::test]
    async fn tool_result_rich_card_emits_event_and_sets_display() {
        let mut sink = MockSink::new();
        let payload = r#"__rich_card__:{"card_type":"table","data":[]}"#;
        let parts = extract_tool_result_events(payload, &mut sink).await;
        assert_eq!(parts.display_result, "Rich card displayed");
        assert_eq!(parts.db_result, payload);
        // A RichCard event must have been emitted with correct card_type
        let rich_card_events: Vec<_> = sink.events.iter().filter(|e| {
            matches!(e, PipelineEvent::Stream(StreamEvent::RichCard { .. }))
        }).collect();
        assert!(
            !rich_card_events.is_empty(),
            "expected a RichCard event, got: {:?}",
            sink.events
        );
        if let PipelineEvent::Stream(StreamEvent::RichCard { card_type, .. }) = &rich_card_events[0] {
            assert_eq!(card_type, "table", "card_type must match JSON field");
        }
    }

    #[tokio::test]
    async fn tool_result_invalid_rich_card_json_no_event() {
        let mut sink = MockSink::new();
        let payload = "__rich_card__:not-valid-json";
        let parts = extract_tool_result_events(payload, &mut sink).await;
        // Invalid JSON: no RichCard event emitted, db_result preserved verbatim
        assert_eq!(parts.db_result, payload);
        let rich_card_events: Vec<_> = sink.events.iter().filter(|e| {
            matches!(e, PipelineEvent::Stream(StreamEvent::RichCard { .. }))
        }).collect();
        assert!(
            rich_card_events.is_empty(),
            "expected no RichCard event for invalid JSON, got: {:?}",
            sink.events
        );
    }

    #[tokio::test]
    async fn tool_result_file_strips_file_lines_from_display() {
        let mut sink = MockSink::new();
        let file_json = r#"{"url":"/api/uploads/test.png?sig=x&exp=9","mediaType":"image/png"}"#;
        let payload = format!("Some text\n__file__:{}\nMore text", file_json);
        let parts = extract_tool_result_events(&payload, &mut sink).await;
        // Display should contain surrounding text but not the __file__ line
        assert!(
            parts.display_result.contains("Some text"),
            "display_result missing 'Some text': {:?}",
            parts.display_result
        );
        assert!(
            parts.display_result.contains("More text"),
            "display_result missing 'More text': {:?}",
            parts.display_result
        );
        assert!(
            !parts.display_result.contains("__file__"),
            "display_result must not contain __file__ marker: {:?}",
            parts.display_result
        );
        // DB result must preserve the full original payload
        assert!(
            parts.db_result.contains("__file__"),
            "db_result must preserve __file__ marker: {:?}",
            parts.db_result
        );
        // A File event must have been emitted
        let file_events: Vec<_> = sink.events.iter().filter(|e| {
            matches!(e, PipelineEvent::Stream(StreamEvent::File { .. }))
        }).collect();
        assert!(
            !file_events.is_empty(),
            "expected a File event, got: {:?}",
            sink.events
        );
    }

    #[tokio::test]
    async fn tool_result_file_only_shows_image_message() {
        let mut sink = MockSink::new();
        let file_json = r#"{"url":"/api/uploads/img.jpg?sig=x&exp=9","mediaType":"image/jpeg"}"#;
        let payload = format!("__file__:{}", file_json);
        let parts = extract_tool_result_events(&payload, &mut sink).await;
        // When only a __file__ line is present (no surrounding text), display gets the fallback message
        assert!(
            parts.display_result.contains("Image displayed inline"),
            "expected 'Image displayed inline' fallback, got: {:?}",
            parts.display_result
        );
        assert_eq!(parts.db_result, payload);
        // A File event must have been emitted
        let file_events: Vec<_> = sink.events.iter().filter(|e| {
            matches!(e, PipelineEvent::Stream(StreamEvent::File { .. }))
        }).collect();
        assert!(
            !file_events.is_empty(),
            "expected a File event, got: {:?}",
            sink.events
        );
    }

    #[tokio::test]
    async fn f042_forged_external_file_url_is_rejected() {
        // An untrusted tool splices a __file__ marker with an attacker URL.
        // No File event must be emitted (it would load an external resource in
        // the operator's browser), though the marker line is still stripped.
        let mut sink = MockSink::new();
        let file_json = r#"{"url":"https://evil.example/track.png","mediaType":"image/png"}"#;
        let payload = format!("hi\n__file__:{}\nbye", file_json);
        let parts = extract_tool_result_events(&payload, &mut sink).await;
        let file_events: Vec<_> = sink
            .events
            .iter()
            .filter(|e| matches!(e, PipelineEvent::Stream(StreamEvent::File { .. })))
            .collect();
        assert!(
            file_events.is_empty(),
            "a forged external File URL must NOT emit a File event, got: {:?}",
            sink.events
        );
        // The marker line is still removed from the display text.
        assert!(!parts.display_result.contains("__file__"));
        assert!(!parts.display_result.contains("evil.example"));
    }
}
