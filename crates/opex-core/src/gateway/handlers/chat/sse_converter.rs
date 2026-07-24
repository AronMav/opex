//! `StreamEvent` → SSE-JSON converter task body extracted from `sse.rs`.
//!
//! The converter is the sole consumer of `event_rx` (engine → coalescer →
//! converter pipeline). It:
//!
//!   - Maps `StreamEvent` variants to AI SDK v3 wire JSON via
//!     [`SseStreamWriter`] (typed builder owns the contextual state:
//!     text-id counter, tool_name_map, current_responding_agent).
//!   - Maintains the open-text-block invariant: consecutive `TextDelta`s
//!     share one `text-start..text-end` pair (writer-owned).
//!   - Buffers every emitted event into [`StreamRegistry`] regardless of
//!     client-connection state, so reconnects via
//!     `/api/chat/{id}/stream` see a complete reply.
//!   - Periodically flushes streaming text to the `messages` row (every
//!     2 s in append mode).
//!   - Honours the 30 s cancel-grace window for explicit `/abort`. Client
//!     disconnect does NOT abort: the engine runs to natural completion
//!     (result buffered + persisted); only `/abort` or the engine's own
//!     limits (max_iterations, loop-detection, tool timeouts) stop a run.
//!
//! AUDIT:SSE-01 / SSE-02 / SSE-03 invariants are preserved verbatim from
//! the pre-extraction inline body — see comments below.

use std::str::FromStr;
use std::sync::Arc;

use axum::response::sse::Event;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::streaming_db::{
    StreamingMessageGuard, build_tools_json, read_streaming_content, upsert_streaming_append,
};
use super::sse_writer::SseStreamWriter;
use crate::agent::engine::StreamEvent;
use crate::gateway::stream_registry::StreamRegistry;

/// Threading state for [`run_converter`]. Bundled into a struct because the
/// raw arg list violates clippy's `too_many_arguments` lint and obscures
/// callsite intent.
pub(super) struct ConverterCtx {
    pub(super) db: sqlx::PgPool,
    pub(super) invite_db: sqlx::PgPool,
    pub(super) registry: Arc<StreamRegistry>,
    pub(super) sse_tx: tokio::sync::mpsc::Sender<Result<Event, std::convert::Infallible>>,
    pub(super) pipeline_cancel: CancellationToken,
    /// T3: `job_id` is now captured by the POST handler at registration time
    /// (registration moved out of this converter) and threaded in here so the
    /// Finish/Error/exit `stream_jobs::set_content` resume-persist still fires.
    pub(super) job_id: uuid::Uuid,
    pub(super) agent_name: String,
    pub(super) mentioned_for_invite: Option<String>,
    pub(super) user_text_for_title: String,
}

#[allow(unused_assignments)]
// reviewed: floor_char_boundary-bounded log preview in send_and_buffer! macro — char boundary
#[allow(clippy::string_slice)]
pub(super) async fn run_converter(
    ctx: ConverterCtx,
    mut event_rx: tokio::sync::mpsc::UnboundedReceiver<StreamEvent>,
    engine_handle: tokio::task::JoinHandle<()>,
) {
    let ConverterCtx {
        db,
        invite_db,
        registry,
        sse_tx,
        pipeline_cancel,
        job_id,
        agent_name,
        mentioned_for_invite,
        user_text_for_title,
    } = ctx;

    // Typed builder for SSE wire emission. Owns text-id counter,
    // tool_name_map, current_responding_agent — replaces the four
    // pre-T5 mutable locals.
    let mut writer = SseStreamWriter::new(agent_name.clone());
    tracing::debug!(current_responding_agent = %writer.current_agent(), "converter: initial agent for SSE");
    let mut session_id_str: Option<String> = None;
    #[allow(unused_assignments)]
    // Client disconnect no longer aborts the engine (browser drop ≠ cancel).
    // This bool only gates a one-time "client disconnected, continuing" log.
    let mut client_gone_logged = false;

    // Helper: send SSE event to client (if connected) and always buffer in registry
    macro_rules! send_and_buffer {
        ($json_str:expr) => {{
            let s: &str = &$json_str;
            tracing::debug!(target: "SSE-OUT", agent = %agent_name, sid = ?session_id_str, event = %&s[..s.floor_char_boundary(180)], "emit");
            let seq: u64 = if let Some(ref sid) = session_id_str {
                registry.push_event(sid, &$json_str).await
            } else {
                0
            };
            if !sse_tx.is_closed() {
                client_gone_logged = false;
                // SSE `id:` field kept for wire-format/devtools completeness
                // only — no consumer relies on Last-Event-ID anymore. T3's
                // `sse.rs` drops `sse_rx` immediately after spawning this
                // converter, so this branch is effectively unreachable in
                // production; the authoritative reconnect path is
                // `GET /api/chat/{id}/stream` (`stream.rs`), which always
                // does a full envelope replay and ignores Last-Event-ID
                // entirely (T4). seq=0 (no session yet) emits no id.
                let event = if seq > 0 {
                    Event::default().id(seq.to_string()).data($json_str)
                } else {
                    Event::default().data($json_str)
                };
                sse_tx.try_send(Ok(event)).is_ok()
            } else {
                // Client disconnected — keep buffering for DB save + resume.
                // Do NOT abort the engine: let it finish naturally so the result
                // is saved to DB and the frontend picks it up via polling on reload.
                // Engine has its own limits (max_iterations, loop-detection, tool
                // timeouts); a browser drop is not a cancel, so there is no
                // client-gone timeout-abort — only explicit `/abort` stops a run.
                if !client_gone_logged {
                    client_gone_logged = true;
                    tracing::info!("SSE client disconnected, continuing engine to completion (result saved to DB)");
                }
                true // always keep going — abort only via cancel API
            }
        }};
    }

    let mut finished_sent = false;
    // T3: the /abort backstop (30s-grace + force engine_handle.abort()) is driven
    // by this token. It used to be set only inside the register block that has
    // since moved to the POST handler; without re-establishing it here the
    // backstop would silently never arm. Set unconditionally from the same
    // pipeline_cancel the handler registered, so POST /abort still cascades.
    let cancel_token: Option<CancellationToken> = Some(pipeline_cancel.clone());
    let chat_db = db.clone();
    let mut accumulated_text = String::new();
    let mut accumulated_tools: Vec<serde_json::Value> = Vec::new();
    let mut tools_flushed_count: usize = 0;
    let mut cached_tools_json: Option<serde_json::Value> = None;
    // Periodic DB flush for streaming messages (LibreChat-style)
    let mut streaming_msg_id = uuid::Uuid::new_v4();
    let mut streaming_guard = StreamingMessageGuard::new(db.clone(), streaming_msg_id);
    let mut last_db_flush = std::time::Instant::now();
    let mut session_uuid: Option<uuid::Uuid> = None;
    let flush_interval = std::time::Duration::from_secs(2);
    // On explicit API cancel (POST /api/chat/{id}/abort) we do NOT
    // hard-abort `engine_handle` immediately. The CancellationToken
    // cascades through providers' `stream_with_cancellation` and raises
    // `LlmCallError::UserCancelled` with partial_state; the engine's error
    // path then persists the aborted message row and writes an aborted
    // usage_log entry. We give it a bounded window (CANCEL_GRACE) to
    // finish naturally, then hard-abort if it's wedged. This guards
    // against tool loops or sync blocks that ignore the cancel token
    // (code_exec, workspace_write, std::sync::Mutex contention). This is the
    // ONLY hard-abort path — a client disconnect never aborts the engine.
    // Backstop only: the engine now observes the cancel token MID-STREAM
    // (forward_chunks_into_sink drops the provider future on cancel), so a user
    // Stop halts generation within a tick and the engine emits its aborted
    // Finish promptly. This grace is the last-resort window for a genuinely
    // wedged engine that ignores the token — 60s (audit 2026-07-22). Was 5s,
    // which hard-aborted mid-tool-call (generate_image, code_exec) before the
    // tool could finish/persist, producing spurious `cancel_grace_exceeded`
    // interrupts. 60s aligns with the channel-path grace (15-20s) while
    // covering the default 120s tool-timeout for the common fast-tool case.
    const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(60);
    let mut logged_cancel_drain = false;
    let mut cancel_deadline: Option<tokio::time::Instant> = None;
    loop {
        // Record the cancel-observation + deadline once.
        if cancel_token
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
            && !logged_cancel_drain
        {
            logged_cancel_drain = true;
            cancel_deadline = Some(tokio::time::Instant::now() + CANCEL_GRACE);
            tracing::info!(
                session_id = ?session_id_str,
                "user cancel received; engine has {}s to emit aborted message + Finish",
                CANCEL_GRACE.as_secs(),
            );
        }

        // Race the engine's next event against the cancel-grace
        // deadline (if set). The helper encapsulates the three
        // outcomes so the logic can be unit-tested in isolation
        // via `tokio::time::pause()`.
        use super::super::cancel_grace::{
            GracePollResult, poll_event_with_cancel_grace,
        };
        let event = match poll_event_with_cancel_grace(&mut event_rx, cancel_deadline).await
        {
            GracePollResult::Event(ev) => ev,
            GracePollResult::Closed => break, // engine finished / channel closed
            GracePollResult::GraceExceeded => {
                // Grace window exceeded with no event progress — the
                // engine is wedged ignoring the cancel token. Force
                // an abort so the task / semaphore permit are freed.
                //
                // This branch fires ONLY after a user-initiated cancel
                // (cancel_deadline is set above only when cancel_token
                // is cancelled). Mark the session `'interrupted'`
                // BEFORE hard-abort so the UI shows "INTERRUPTED"
                // instead of "ERROR" — the engine task's
                // `SessionLifecycleGuard` is about to drop, and its
                // Drop impl would otherwise mark the session `'failed'`.
                //
                // cleanup_session_terminated uses an atomic
                // `WHERE run_status = 'running'` claim (step 1), so
                // when the guard's Drop fires afterwards it observes
                // 'interrupted' and its claim returns Ok(false) — no
                // overwrite.
                tracing::warn!(
                    session_id = ?session_id_str,
                    "cancel grace window ({}s) exceeded, hard-aborting engine",
                    CANCEL_GRACE.as_secs(),
                );
                // T2 ownership gate: this cancel-grace hard-abort also fires on
                // the OLD stream when a same-session supersede cancels its token.
                // If a NEWER stream_job superseded this turn, the newer turn owns
                // the (still-running) session row — marking it `interrupted` here
                // would strand that turn. Only pre-mark when we are still the
                // active turn for the session.
                let superseded = crate::gateway::stream_jobs::is_superseded(&db, job_id)
                    .await
                    .unwrap_or(false);
                if let Some(sid) = session_uuid
                    && !superseded
                    && let Err(e) = crate::db::sessions::cleanup_session_terminated(
                        &db,
                        sid,
                        "interrupted",
                        "cancel_grace_exceeded",
                    )
                    .await
                {
                    tracing::warn!(
                        session_id = %sid,
                        error = %e,
                        "failed to mark session interrupted before hard-abort"
                    );
                }
                engine_handle.abort();
                break;
            }
        };

        // NOTE: no client-gone timeout-abort here. When the SSE client
        // disconnects the engine keeps running to natural completion (events
        // buffered to the registry for DB persist + reconnect-replay). A browser
        // drop is a transport event, not a cancel — only explicit `/abort`
        // (cancel-grace branch above) or the engine's own limits stop a run.
        // Close the open text block before any non-text event. Consecutive
        // TextDelta events keep the block open and share the same id.
        if !matches!(event, StreamEvent::TextDelta(_))
            && let Some(end_frame) = writer.build_text_end_if_open()
        {
            let _ = send_and_buffer!(end_frame);
        }

        match event {
            StreamEvent::SessionId { session_id: sid, context_limit } => {
                let parsed_uuid = uuid::Uuid::from_str(&sid).ok();
                // T3: stream registration + cancel-token/job_id capture moved to
                // the POST handler (before it responds 202), so a GET /{id}/stream
                // is guaranteed to find the stream. The converter no longer
                // registers here — it only wires up the session_id/session_uuid
                // used below for push_event buffering, auto-invite, and the
                // initial streaming-message upsert.
                session_id_str = Some(sid.clone());
                session_uuid = parsed_uuid;
                if let Some(sid_uuid) = session_uuid {
                    streaming_guard.set_session_id(sid_uuid);
                }
                // Auto-invite the mentioned agent if it differs from the session owner
                if let Some(sid_uuid) = session_uuid
                    && let Some(ref mentioned) = mentioned_for_invite
                {
                    let invite_db_clone = invite_db.clone();
                    let agent = mentioned.clone();
                    // AUDIT-FF-008: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
                    tokio::spawn(async move {
                        let _ = crate::db::sessions::add_participant(&invite_db_clone, sid_uuid, &agent, None).await;
                    });
                }
                // Write empty streaming record immediately — gives frontend a persistent DB signal
                // before the first token arrives. Single source of truth for "is agent thinking?".
                if let Some(sid_uuid) = session_uuid
                    && let Err(e) = crate::db::sessions::upsert_streaming_message(
                        &chat_db, streaming_msg_id, sid_uuid, &agent_name, "", None
                    ).await {
                        tracing::warn!(error = %e, "failed to upsert initial streaming message to DB");
                    }
                let frame = writer.build_session_id(sid, Some(context_limit));
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::MessageStart { message_id } => {
                let frame = writer.build_start(message_id);
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::StepStart { iteration } => {
                // Boundary between LLM tool-loop iterations. Open text block
                // was already closed by the non-TextDelta guard at the top
                // of the loop. CRITICAL: stepId wire format is `step_{N}`
                // (writer formats it).
                let frame = writer.build_step_start(iteration);
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::TextDelta(ref text) => {
                if session_uuid.is_none() && accumulated_text.is_empty() {
                    tracing::error!("TextDelta received but session_uuid is None — DB flush will be skipped");
                }
                // AI SDK v3: text-start → text-delta* → text-end
                // Writer opens a new text block only if there isn't one open
                // already; all consecutive deltas of the same logical text
                // block share one id.
                let (start_frame, delta_frame) = writer.build_text_delta(text.clone());
                if let Some(start) = start_frame {
                    let _ = send_and_buffer!(start);
                }
                // Skip empty delta frames — `build_text_delta` returns an empty
                // string when it cannot recover a `current_text_id` after the
                // TextStart fallback path, and emitting it would advance the
                // SSE cursor with no content.
                if !delta_frame.is_empty() {
                    let _ = send_and_buffer!(delta_frame);
                }
                accumulated_text.push_str(text);
                // Periodic DB flush every 2s so reload shows partial response
                // Uses append-mode SQL so accumulated_text can be cleared after flush (bounded memory)
                if last_db_flush.elapsed() >= flush_interval
                    && let Some(sid) = session_uuid {
                        let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                        if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                            tracing::warn!(error = %e, "failed to flush streaming message to DB");
                        } else {
                            // Only clear after successful flush -- on failure, text stays for retry
                            accumulated_text.clear();
                        }
                        last_db_flush = std::time::Instant::now();
                    }
                continue;
            }
            StreamEvent::ToolCallStart { id, name, parallel_batch_id } => {
                let frame = writer.build_tool_input_start(id, name, parallel_batch_id);
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::ToolCallArgs { id, args_text } => {
                let delta_frame = writer.build_tool_input_delta(id.clone(), args_text.clone());
                let _ = send_and_buffer!(delta_frame);

                let input: serde_json::Value = serde_json::from_str(&args_text)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let avail_frame = writer.build_tool_input_available(id, input);
                let _ = send_and_buffer!(avail_frame);
                continue;
            }
            StreamEvent::ToolResult { id, result } => {
                // Accumulate tool calls in-memory (single DB write at finish)
                let id_str = id.as_str().to_string();
                let tname = writer.tool_name_for(&id_str).unwrap_or_default();
                accumulated_tools.push(json!({"toolCallId": id_str, "toolName": tname, "output": &result}));
                cached_tools_json = None; // Invalidate cache when new tool arrives
                let frame = writer.build_pure(StreamEvent::ToolResult { id, result });
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::StepFinish { step_id: _, finish_reason: _ } => {
                continue;
            }
            StreamEvent::RichCard { card_type, data } => {
                let frame = writer.build_rich_card(card_type, data);
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::File { url, media_type, filename } => {
                let frame = writer.build_pure(StreamEvent::File { url, media_type, filename });
                let _ = send_and_buffer!(frame);
                continue;
            }
            // Retained for API compatibility — not currently emitted.
            StreamEvent::AgentSwitch { agent_name: new_agent } => {
                writer.set_agent(new_agent);
                continue; // Internal event — don't emit SSE
            }
            StreamEvent::ClarifyNeeded { clarify_id, question, choices, timeout_ms } => {
                let frame = writer.build_pure(StreamEvent::ClarifyNeeded {
                    clarify_id,
                    question,
                    choices,
                    timeout_ms,
                });
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::ApprovalNeeded { approval_id, tool_name, tool_input, timeout_ms } => {
                let frame = writer.build_pure(StreamEvent::ApprovalNeeded {
                    approval_id,
                    tool_name,
                    tool_input,
                    timeout_ms,
                });
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::ApprovalResolved { approval_id, action, modified_input } => {
                let frame = writer.build_pure(StreamEvent::ApprovalResolved {
                    approval_id,
                    action,
                    modified_input,
                });
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::Finish { .. } => {
                // Close any still-open text block before Finish.
                if let Some(end_frame) = writer.build_text_end_if_open() {
                    let _ = send_and_buffer!(end_frame);
                }
                let finish_frame = writer.build_finish();
                let _ = send_and_buffer!(finish_frame);
                // Final flush of streaming message + mark complete
                // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
                if let Some(sid) = session_uuid {
                    let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                    // Step 1: Flush remaining text delta to streaming message (APPEND mode)
                    if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                        tracing::warn!(error = %e, "failed to upsert streaming message on Finish");
                    }
                    // Step 2: Read back full aggregated text BEFORE the row is deleted
                    let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
                    // Step 3: Persist full content to stream_jobs (needs complete text)
                    if let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, job_id, &full_text, &accumulated_tools).await {
                        tracing::warn!(error = %e, "failed to set stream job content on Finish");
                    }
                    // Step 4: NOW safe to finalize (DELETE) the streaming message row
                    if let Err(e) = crate::db::sessions::finalize_streaming_message(&chat_db, streaming_msg_id).await {
                        tracing::warn!(error = %e, "failed to finalize streaming message on Finish");
                    }
                }
                streaming_guard.mark_finalized();
                // DON'T break here — session-scoped agents may send more events.
                // The loop exits naturally when event_tx is dropped (engine task completes).
                // Send [DONE] only after all turns are done (handled in post-loop block).
                // Reset accumulated state for next agent turn:
                accumulated_text.clear();
                accumulated_tools.clear();
                tools_flushed_count = 0;
                cached_tools_json = None;
                streaming_msg_id = uuid::Uuid::new_v4();
                // Reset guard for next turn to prevent streaming row leak
                streaming_guard = StreamingMessageGuard::new(chat_db.clone(), streaming_msg_id);
                if let Some(sid) = session_uuid {
                    streaming_guard.set_session_id(sid);
                }
                continue;
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                reasoning_tokens,
            } => {
                let frame = writer.build_usage(
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                    reasoning_tokens,
                );
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::Reconnecting { attempt, delay_ms } => {
                let frame = writer.build_pure(StreamEvent::Reconnecting { attempt, delay_ms });
                let _ = send_and_buffer!(frame);
                continue;
            }
            StreamEvent::Error(ref text) => {
                let err_frame = writer.build_error(text.clone());
                let _ = send_and_buffer!(err_frame);
                if let Some(ref sid) = session_id_str {
                    registry.mark_error(sid, text).await;
                }
                // Finalize streaming message on error too
                // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
                if let Some(sid) = session_uuid {
                    let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
                    // Step 1: Flush remaining text delta (APPEND mode)
                    if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                        tracing::warn!(error = %e, "failed to upsert streaming message on Error");
                    }
                    // Step 2: Read back full aggregated text BEFORE the row is deleted
                    let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
                    // Step 3: Persist full content to stream_jobs
                    if let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, job_id, &full_text, &accumulated_tools).await {
                        tracing::warn!(error = %e, "failed to set stream job content on Error");
                    }
                    // Step 4: NOW safe to finalize (DELETE) the streaming message row
                    if let Err(e) = crate::db::sessions::finalize_streaming_message(&chat_db, streaming_msg_id).await {
                        tracing::warn!(error = %e, "failed to finalize streaming message on Error");
                    }
                }
                streaming_guard.mark_finalized();
                finished_sent = true;
                break;
            }
        }
    }

    // Only send [DONE] and mark_finished if the Finish branch didn't already do it
    if !finished_sent {
        // Finalize streaming message on unexpected exit
        // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
        //
        // Skip the upsert/finalize chain when the previous turn ended via
        // `StreamEvent::Finish` and nothing accumulated since: the Finish
        // branch already finalized that row, and a new `streaming_msg_id`
        // was allocated eagerly for the (now non-existent) next turn —
        // upserting against it would insert a phantom empty
        // `streaming_messages` row that has to be cleaned up later.
        let has_pending_state = !accumulated_text.is_empty() || !accumulated_tools.is_empty();
        if has_pending_state && let Some(sid) = session_uuid {
            let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
            // Step 1: Flush remaining text delta (APPEND mode)
            if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                tracing::warn!(error = %e, "failed to upsert streaming message on unexpected exit");
            }
            // Step 2: Read back full aggregated text BEFORE the row is deleted
            let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
            // Step 3: Persist full content to stream_jobs
            if let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, job_id, &full_text, &accumulated_tools).await {
                tracing::warn!(error = %e, "failed to set stream job content on unexpected exit");
            }
            // Step 4: NOW safe to finalize (DELETE) the streaming message row
            if let Err(e) = crate::db::sessions::finalize_streaming_message(&chat_db, streaming_msg_id).await {
                tracing::warn!(error = %e, "failed to finalize streaming message on unexpected exit");
            }
        }
        streaming_guard.mark_finalized();
        if let Some(ref sid) = session_id_str {
            registry.mark_finished(sid).await;
        }
        // Flush any remaining text-end (if stream ended without Finish event)
        if let Some(end_frame) = writer.build_text_end_if_open() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                sse_tx.send(Ok(Event::default().data(end_frame)))
            ).await;
        }
        // [DONE] is critical — use timeout to avoid blocking if client gone
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            sse_tx.send(Ok(Event::default().data("[DONE]")))
        ).await;
    }

    // Auto-title: set session title from first user message if not already titled
    if let Some(sid) = session_uuid {
        let title_db = chat_db.clone();
        // spawn_traced so the auto-title query appears under the
        // originating request's trace (lets us see how long the
        // auto-title query took relative to the rest of the turn).
        crate::trace_propagation::spawn_traced(async move {
            if let Err(e) = crate::db::sessions::auto_title_session(&title_db, sid, &user_text_for_title).await {
                tracing::debug!(error = %e, "auto-title failed");
            }
        });
    }

    // Session agent pool is NOT cleaned up here — agents live until
    // explicitly killed via agent(action: "kill") or session expiry.
}
