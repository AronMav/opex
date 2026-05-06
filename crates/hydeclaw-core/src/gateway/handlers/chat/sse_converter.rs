//! `StreamEvent` → SSE-JSON converter task body extracted from `sse.rs`.
//!
//! The converter is the sole consumer of `event_rx` (engine → coalescer →
//! converter pipeline). It:
//!
//!   - Maps `StreamEvent` variants to AI SDK v3 wire JSON.
//!   - Maintains the open-text-block invariant: consecutive `TextDelta`s
//!     share one `text-start..text-end` pair (`current_text_id`).
//!   - Buffers every emitted event into [`StreamRegistry`] regardless of
//!     client-connection state, so reconnects via
//!     `/api/chat/{id}/stream` see a complete reply.
//!   - Periodically flushes streaming text to the `messages` row (every
//!     2 s in append mode).
//!   - Honours the 30 s cancel-grace window and the 600 s
//!     client-gone runaway-protection window.
//!
//! AUDIT:SSE-01 / SSE-02 / SSE-03 invariants are preserved verbatim from
//! the pre-extraction inline body — see comments below.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use axum::response::sse::Event;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::streaming_db::{
    StreamingMessageGuard, build_tools_json, read_streaming_content, upsert_streaming_append,
};
use super::super::super::sse_types;
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
    pub(super) agent_name: String,
    pub(super) mentioned_for_invite: Option<String>,
    pub(super) user_text_for_title: String,
}

#[allow(unused_assignments)]
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
        agent_name,
        mentioned_for_invite,
        user_text_for_title,
    } = ctx;

    let mut text_id_counter: usize = 0;
    // Tracks the OPEN text block so consecutive TextDelta events all carry the
    // same id. None = no open block; Some(id) = block id `id` is currently open.
    let mut current_text_id: Option<String> = None;
    let mut tool_name_map: HashMap<String, String> = HashMap::new();
    let mut session_id_str: Option<String> = None;
    // Tracks which agent is currently responding (updated on AgentSwitch)
    let mut current_responding_agent = agent_name.clone();
    tracing::debug!(current_responding_agent = %current_responding_agent, "converter: initial agent for SSE");
    #[allow(unused_assignments)]
    let mut client_gone_since: Option<std::time::Instant> = None;

    // Helper: send SSE event to client (if connected) and always buffer in registry
    macro_rules! send_and_buffer {
        ($json_str:expr) => {{
            let s: &str = &$json_str;
            tracing::debug!(target: "SSE-OUT", agent = %agent_name, sid = ?session_id_str, event = %&s[..s.len().min(180)], "emit");
            let seq: u64 = if let Some(ref sid) = session_id_str {
                registry.push_event(sid, &$json_str).await
            } else {
                0
            };
            if !sse_tx.is_closed() {
                client_gone_since = None;
                // SSE `id:` field — client tracks via Last-Event-ID for
                // dedup-free reconnect. seq=0 (no session yet) emits no id.
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
                // Engine has its own limits (max_iterations, subagent timeout).
                if client_gone_since.is_none() {
                    client_gone_since = Some(std::time::Instant::now());
                    tracing::info!("SSE client disconnected, continuing engine for DB save");
                }
                true // always keep going — abort only via cancel API
            }
        }};
    }

    let mut finished_sent = false;
    let mut cancel_token: Option<CancellationToken> = None;
    let mut job_id: Option<uuid::Uuid> = None;
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
    // (code_exec, workspace_write, std::sync::Mutex contention) — the
    // pre-existing `client_gone_since > 600 s` check only ran inside
    // `event_rx.recv().await`, so a silent wedge bypassed it.
    const CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
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
                // The guard uses `mark_session_run_status_if_running`,
                // so once we write `'interrupted'` here the guard's
                // update affects 0 rows.
                tracing::warn!(
                    session_id = ?session_id_str,
                    "cancel grace window ({}s) exceeded, hard-aborting engine",
                    CANCEL_GRACE.as_secs(),
                );
                if let Some(sid) = session_uuid
                    && let Err(e) = crate::db::sessions::mark_session_run_status_if_running(
                        &db,
                        sid,
                        "interrupted",
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

        // AUDIT:SSE-03 (verified 2026-03-30): Safety net for client disconnect.
        // See stream_registry.rs for full SSE-03 audit. This 10-minute timeout
        // ensures no hanging tasks if client disconnects and never reconnects.
        // Safety net: abort if client gone for 10+ minutes (runaway engine protection)
        if client_gone_since.is_some_and(|t| t.elapsed().as_secs() > 600) {
            tracing::warn!("SSE client gone for 10min, aborting runaway engine");
            engine_handle.abort();
            break;
        }
        // Close the open text block before any non-text event. Consecutive
        // TextDelta events keep the block open and share the same id.
        if !matches!(event, StreamEvent::TextDelta(_))
            && let Some(text_id) = current_text_id.take() {
            let end_data = json!({"type": sse_types::TEXT_END, "id": text_id}).to_string();
            let _ = send_and_buffer!(end_data);
        }

        let data = match event {
            StreamEvent::SessionId { session_id: sid, context_limit } => {
                let parsed_uuid = uuid::Uuid::from_str(&sid).ok();
                // Register stream in registry for resume + abort support (C3).
                // Use register_with_token so the registry stores the SAME token
                // that was passed to handle_sse: POST /api/chat/{id}/abort →
                // registry.cancel() cancels pipeline_cancel → propagates into execute().
                if let Some(uuid) = parsed_uuid
                    && let Some(jid) = registry.register_with_token(uuid, &agent_name, pipeline_cancel.clone()).await {
                        cancel_token = Some(pipeline_cancel.clone());
                        job_id = Some(jid);
                    }
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
                    tokio::spawn(async move {
                        let _ = crate::db::sessions::add_participant(&invite_db_clone, sid_uuid, &agent).await;
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
                // Custom data part: session_id for UI to track the active session
                json!({"type": sse_types::DATA_SESSION_ID, "data": {"sessionId": sid, "contextLimit": context_limit}, "transient": true})
            }
            StreamEvent::MessageStart { message_id } => {
                json!({"type": sse_types::START, "messageId": message_id, "agentName": current_responding_agent})
            }
            StreamEvent::StepStart { step_id, message_id } => {
                // Boundary between LLM tool-loop iterations. `messageId`
                // is the pre-allocated DB row UUID for the iteration —
                // frontend opens a fresh live ChatMessage with this id so
                // it matches the eventual DB row. Open text block was
                // already closed by the non-TextDelta guard at the top
                // of the loop.
                json!({"type": sse_types::STEP_START, "stepId": step_id, "messageId": message_id, "agentName": current_responding_agent})
            }
            StreamEvent::TextDelta(ref text) => {
                if session_uuid.is_none() && accumulated_text.is_empty() {
                    tracing::error!("TextDelta received but session_uuid is None — DB flush will be skipped");
                }
                // AI SDK v3: text-start → text-delta* → text-end
                // Open a new text block only if there isn't one open already; all
                // consecutive deltas of the same logical text block share one id.
                let text_id = match current_text_id.as_ref() {
                    Some(id) => id.clone(),
                    None => {
                        text_id_counter += 1;
                        let new_id = format!("text-{text_id_counter}");
                        let start_data = json!({"type": sse_types::TEXT_START, "id": new_id.clone(), "agentName": current_responding_agent}).to_string();
                        let _ = send_and_buffer!(start_data);
                        current_text_id = Some(new_id.clone());
                        new_id
                    }
                };
                let delta_data = json!({"type": sse_types::TEXT_DELTA, "id": text_id, "delta": text}).to_string();
                let _ = send_and_buffer!(delta_data);
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
            StreamEvent::ToolCallStart { id, name } => {
                tool_name_map.insert(id.clone(), name.clone());
                json!({
                    "type": sse_types::TOOL_INPUT_START,
                    "toolCallId": id,
                    "toolName": name,
                    "agentName": current_responding_agent,
                })
            }
            StreamEvent::ToolCallArgs { id, args_text } => {
                let delta_data = json!({
                    "type": sse_types::TOOL_INPUT_DELTA,
                    "toolCallId": id,
                    "inputTextDelta": args_text
                }).to_string();
                let _ = send_and_buffer!(delta_data);

                let input: serde_json::Value = serde_json::from_str(&args_text)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let tool_name = tool_name_map.get(&id).cloned().unwrap_or_default();
                json!({
                    "type": sse_types::TOOL_INPUT_AVAILABLE,
                    "toolCallId": id,
                    "toolName": tool_name,
                    "input": input
                })
            }
            StreamEvent::ToolResult { ref id, ref result } => {
                // Accumulate tool calls in-memory (single DB write at finish)
                let tname = tool_name_map.get(id).cloned().unwrap_or_default();
                accumulated_tools.push(json!({"toolCallId": id, "toolName": tname, "output": result}));
                cached_tools_json = None; // Invalidate cache when new tool arrives
                json!({
                    "type": sse_types::TOOL_OUTPUT_AVAILABLE,
                    "toolCallId": id,
                    "output": result
                })
            }
            StreamEvent::StepFinish { step_id: _, finish_reason: _ } => {
                continue;
            }
            StreamEvent::RichCard { card_type, data } => {
                json!({
                    "type": sse_types::RICH_CARD,
                    "cardType": card_type,
                    "data": data
                })
            }
            StreamEvent::File { url, media_type } => {
                json!({
                    "type": sse_types::FILE,
                    "url": url,
                    "mediaType": media_type
                })
            }
            // Retained for API compatibility — not currently emitted.
            StreamEvent::AgentSwitch { agent_name: new_agent } => {
                current_responding_agent = new_agent;
                continue; // Internal event — don't emit SSE
            }
            StreamEvent::ApprovalNeeded { approval_id, tool_name, tool_input, timeout_ms } => {
                let data = json!({
                    "type": sse_types::APPROVAL_NEEDED,
                    "approvalId": approval_id,
                    "toolName": tool_name,
                    "toolInput": tool_input,
                    "timeoutMs": timeout_ms,
                }).to_string();
                let _ = send_and_buffer!(data);
                continue;
            }
            StreamEvent::ApprovalResolved { approval_id, action, modified_input } => {
                let data = json!({
                    "type": sse_types::APPROVAL_RESOLVED,
                    "approvalId": approval_id,
                    "action": action,
                    "modifiedInput": modified_input,
                }).to_string();
                let _ = send_and_buffer!(data);
                continue;
            }
            StreamEvent::Finish { .. } => {
                // Close any still-open text block before Finish.
                if let Some(text_id) = current_text_id.take() {
                    let end_data = json!({"type": sse_types::TEXT_END, "id": text_id}).to_string();
                    let _ = send_and_buffer!(end_data);
                }
                let finish_data = json!({"type": sse_types::FINISH, "agentName": current_responding_agent}).to_string();
                let _ = send_and_buffer!(finish_data);
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
                    if let Some(jid) = job_id
                        && let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, jid, &full_text, &accumulated_tools).await {
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
                text_id_counter = 0;
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
                // agentName: tag with the currently-responding agent so the UI
                // can route this usage to the correct agent's tokenUsage state.
                // Without it, in a multi-agent session a usage event from agent
                // B fired while A is mid-stream would overwrite A's state.
                let mut payload = serde_json::json!({
                    "type": sse_types::USAGE,
                    "inputTokens": input_tokens,
                    "outputTokens": output_tokens,
                    "agentName": current_responding_agent.clone(),
                });
                // Extended fields — subsets of input/output (not additive). Only emit
                // when present so older clients see no change in payload size for
                // providers that don't report them.
                if let Some(v) = cache_read_tokens {
                    payload["cacheReadTokens"] = serde_json::Value::from(v);
                }
                if let Some(v) = cache_creation_tokens {
                    payload["cacheCreationTokens"] = serde_json::Value::from(v);
                }
                if let Some(v) = reasoning_tokens {
                    payload["reasoningTokens"] = serde_json::Value::from(v);
                }
                let data = payload.to_string();
                let _ = send_and_buffer!(data);
                continue;
            }
            StreamEvent::Reconnecting { attempt, delay_ms } => {
                let data = serde_json::json!({
                    "type": sse_types::RECONNECTING,
                    "attempt": attempt,
                    "delay_ms": delay_ms,
                }).to_string();
                let _ = send_and_buffer!(data);
                continue;
            }
            StreamEvent::Error(ref text) => {
                let err_data = json!({"type": sse_types::ERROR, "errorText": text}).to_string();
                let _ = send_and_buffer!(err_data);
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
                    if let Some(jid) = job_id
                        && let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, jid, &full_text, &accumulated_tools).await {
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
        };

        let json_str = data.to_string();
        let _ = send_and_buffer!(json_str);
    }

    // Only send [DONE] and mark_finished if the Finish branch didn't already do it
    if !finished_sent {
        // Finalize streaming message on unexpected exit
        // CRITICAL ORDERING: upsert → read_streaming_content → set_content → finalize (DELETE)
        if let Some(sid) = session_uuid {
            let tools_json = build_tools_json(&accumulated_tools, &mut tools_flushed_count, &mut cached_tools_json);
            // Step 1: Flush remaining text delta (APPEND mode)
            if let Err(e) = upsert_streaming_append(&chat_db, streaming_msg_id, sid, &agent_name, &accumulated_text, tools_json.as_ref()).await {
                tracing::warn!(error = %e, "failed to upsert streaming message on unexpected exit");
            }
            // Step 2: Read back full aggregated text BEFORE the row is deleted
            let full_text = read_streaming_content(&chat_db, streaming_msg_id).await;
            // Step 3: Persist full content to stream_jobs
            if let Some(jid) = job_id
                && let Err(e) = crate::gateway::stream_jobs::set_content(&chat_db, jid, &full_text, &accumulated_tools).await {
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
        if let Some(text_id) = current_text_id {
            let end_data = json!({"type": sse_types::TEXT_END, "id": text_id});
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                sse_tx.send(Ok(Event::default().data(end_data.to_string())))
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
