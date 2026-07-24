//! Three thin adapter methods on AgentEngine. Each constructs an EventSink
//! and delegates to pipeline::execute. See spec §3 and the implementation
//! plan (Tasks 7–9) for rationale.

use anyhow::Result;
use opex_types::IncomingMessage;
use uuid::Uuid;

use super::stream::ProcessingPhase;
use super::AgentEngine;
use crate::agent::commands::spec::CommandOutcome;
use crate::agent::engine_event_sender::EngineEventSender;
use crate::agent::pipeline::behaviour::BehaviourLayers;
use crate::agent::pipeline::bootstrap::{self, BootstrapContext, BootstrapOutcome};
use crate::agent::pipeline::sink::{self, EventSink, PipelineEvent};

/// Wall-clock ceiling for the synchronous bootstrap phase (session resolve,
/// timeline/compaction state load, context build + enhancements). 60s is the
/// compromise between "long enough for cold toolgate + multi-agent startup"
/// and "short enough to surface a stuck turn before the user gives up".
///
/// All auxiliary work inside bootstrap is fail-soft (logged + skipped on
/// error), so 60s is purely the worst-case bound, not the typical latency.
const BOOTSTRAP_HARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Upper bound a user turn waits to acquire the per-session goal lock before
/// bailing out as `Interrupted`. Long-running tools (`generate_image`,
/// `code_exec`, long LLM streams) can legitimately hold the lock for minutes;
/// 30s killed concurrent turns mid-tool-call (audit 2026-07-22). The
/// `cancel` token still wins on explicit `/stop` / disconnect — this is only
/// the safety net for an abandoned wedged holder.
const GOAL_LOCK_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
use crate::agent::pipeline::{execute, finalize};
use crate::agent::stream_event::StreamEvent;

/// F036: RAII guard that removes this session's SSE sender from the per-session
/// map on every exit path (normal return, `?` error, panic-unwind), so a
/// finished turn releases only its OWN entry and never wipes a concurrent
/// session's sender.
struct SseSenderGuard {
    map: std::sync::Arc<dashmap::DashMap<Uuid, EngineEventSender>>,
    session_id: Uuid,
    /// The sender this guard installed. Compared against the live map value on
    /// drop so a superseding turn's sender cannot be evicted by an older turn
    /// unwinding (H1 fix). `EngineEventSender` is itself `Clone` sharing the
    /// same underlying mpsc channel — identity is established via
    /// `same_channel`, not Arc pointer equality.
    installed: EngineEventSender,
}
impl Drop for SseSenderGuard {
    fn drop(&mut self) {
        // Compare-and-swap: only remove the entry if it still points at OUR
        // sender. If a newer turn has already installed its own sender for the
        // same session_id (the narrow TOCTOU window `claim_session_with_retry`
        // admits), evicting unconditionally would dead-route approvals to the
        // live turn. Remove iff the channel identity matches.
        //
        // `remove_if` performs the check atomically under the DashMap shard
        // lock so a concurrent `insert` cannot race between check and remove.
        self.map.remove_if(&self.session_id, |_, v| v.same_channel(&self.installed));
    }
}

/// Builds Telegram `send_buttons` specs for a `command_args_menu` card that
/// carries a choice-valve `options` + `token` (dispatch.rs `try_handler_command`,
/// Task 2). Returns `None` when the card lacks either — e.g. the
/// missing-source prompt card — so callers keep the plain-text fallback.
///
/// Button `data` is `cm:<token>:<value>` (mirrors the `hm:<token>:<id>` shape
/// used by the handler-menu flow), recovered by `POST /api/commands/menu-run`.
fn argsmenu_buttons(card: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    let token = card.get("token").and_then(|v| v.as_str())?;
    let options = card.get("options").and_then(|v| v.as_array())?;
    if options.is_empty() {
        return None;
    }
    Some(
        options
            .iter()
            .map(|o| {
                let value = o.get("value").and_then(|v| v.as_str()).unwrap_or("");
                let label = o.get("label").and_then(|v| v.as_str()).unwrap_or(value);
                serde_json::json!({ "text": label, "data": format!("cm:{token}:{value}") })
            })
            .collect(),
    )
}

impl AgentEngine {
    /// Hard-error path helper: when `execute()`/`finalize()` bubble an `Err` out
    /// of an entry point, the normal `finalize` failure machinery never runs and
    /// the `SessionLifecycleGuard::Drop` fallback records only an opaque
    /// `guard_dropped` / "guard dropped (early exit)" row with all-NULL
    /// diagnostics. This captures the REAL error reason + provider/model instead.
    /// Idempotent — no-op if the guard already transitioned out of `Running`.
    /// See `finalize::record_hard_error_failure` for the full rationale.
    async fn record_hard_error(
        &self,
        guard: &mut crate::agent::session_manager::SessionLifecycleGuard,
        session_id: Uuid,
        reason: String,
    ) {
        finalize::record_hard_error_failure(
            guard,
            self.cfg().db.clone(),
            session_id,
            self.cfg().agent.name.clone(),
            reason,
            Some(self.cfg().provider.name().to_string()),
            Some(self.current_model()),
            &self.state().bg_tasks,
        )
        .await;
    }

    /// Handle message via SSE: thin, behaviourally-identical wrapper over the
    /// T3 split (`bootstrap_sse` + `execute_sse`), retained for
    /// `api_retry_session` (sessions.rs), which drives a resume turn with a
    /// throwaway drain channel and does not need the 202/registration split.
    ///
    /// Phase 62 RES-01: `event_tx` is an `EngineEventSender` wrapping a bounded
    /// `mpsc::Sender<StreamEvent>` (capacity 256 in chat.rs).
    ///
    /// `cancel` must be the same token registered with `StreamRegistry::register_with_token`
    /// so that `POST /api/chat/{id}/abort` propagates into the pipeline (C3).
    pub async fn handle_sse(
        &self,
        msg: &IncomingMessage,
        event_tx: EngineEventSender,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Uuid> {
        let boot = self.bootstrap_sse(msg, resume_session_id, force_new_session, None).await?;
        let session_id = boot.session_id;
        // Retry wrapper: no StreamRegistry stream_job is registered for this
        // path, so there is no supersede ownership to gate (None).
        self.execute_sse(boot, event_tx, cancel, None).await?;
        Ok(session_id)
    }

    /// T3 — SYNCHRONOUS bootstrap half of the SSE turn. Fires the
    /// `BeforeMessage` hook (honouring `HookAction::Block`) and runs
    /// `pipeline::bootstrap`, returning the `BootstrapOutcome`. `POST /api/chat`
    /// calls this BEFORE responding `202` so `session_id` + `user_message_id`
    /// are known and the stream is registered in `StreamRegistry` before the
    /// client can issue `GET /{id}/stream` (T4).
    ///
    /// Bootstrap runs on a `NoopSink` — its ONLY sink emit is `Phase(Thinking)`
    /// (bootstrap.rs), which never reaches the wire or the registry. So there is
    /// no "events emitted before registration" hazard: bootstrap is DELIBERATELY
    /// not connected to the live SSE converter. The live `SseSink` is built later
    /// in `execute_sse`, after the POST handler has registered the stream.
    ///
    /// `model_override` — one-shot per-turn model override (Wave-2 Task 12),
    /// sourced from `POST /api/chat`'s `ChatSseRequest.model` (already
    /// normalized: trimmed, empty string → `None`). Stamped onto the
    /// returned `BootstrapOutcome.turn_model_override` AFTER `bootstrap()`
    /// runs — `bootstrap()` itself has no notion of it — so it never touches
    /// session/provider state, only this turn's `execute()` call.
    pub async fn bootstrap_sse(
        &self,
        msg: &IncomingMessage,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
        model_override: Option<String>,
    ) -> Result<BootstrapOutcome> {
        let hook_event = crate::agent::hooks::HookEvent::BeforeMessage;
        let action = self.hooks().fire(&hook_event);
        self.hooks().fire_webhooks(&hook_event);
        if let crate::agent::hooks::HookAction::Block(reason) = action {
            anyhow::bail!("blocked by hook: {}", reason);
        }

        let mut s = sink::NoopSink::new();
        let mut boot = match tokio::time::timeout(
            BOOTSTRAP_HARD_TIMEOUT,
            bootstrap::bootstrap(
                self,
                BootstrapContext {
                    msg,
                    resume_session_id,
                    force_new_session,
                },
                &mut s,
            ),
        )
        .await
        {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                anyhow::bail!(
                    "bootstrap exceeded {}s hard timeout; toolgate or embedding may be unresponsive",
                    BOOTSTRAP_HARD_TIMEOUT.as_secs()
                );
            }
        };
        boot.turn_model_override = model_override;
        Ok(boot)
    }

    /// T3 — post-bootstrap execution half of the SSE turn. Owns EVERYTHING after
    /// `bootstrap`: the request-drain guard, the live `SseSink`, per-session
    /// sender publish, the `SessionId` emit, the slash-command early branch, and
    /// the full `execute` → `finalize` pipeline. Consumes `boot` by value so it
    /// can be moved into the engine task the POST handler spawns AFTER
    /// registering the stream (hence every event this method emits lands in the
    /// already-registered `StreamRegistry` buffer).
    pub async fn execute_sse(
        &self,
        boot: BootstrapOutcome,
        tx: EngineEventSender,
        cancel: tokio_util::sync::CancellationToken,
        stream_job_id: Option<Uuid>,
    ) -> Result<()> {
        // R-DRAIN: register the REAL pipeline cancel token (not an orphan) and
        // hold the RAII guard for the whole turn so graceful-shutdown
        // `cancel_all_requests` propagates into `execute()` and the request is
        // unregistered on every exit path (fixes the active_requests leak +
        // always-full drain timeout). MUST live here, NOT in `bootstrap_sse`:
        // the guard has to outlive the POST 202 response for the whole engine
        // turn — placing it in the synchronous bootstrap half would drop it the
        // instant POST returns.
        let _req_guard = self.state.register_request_guarded(cancel.clone());

        // F036: keep a clone of the sender to publish per-session (below); the
        // original is moved into the sink. The per-session SSE sender is
        // published keyed by session_id and removed via an RAII guard on every
        // exit path — replaces the single Option slot that concurrent sessions
        // of the same agent used to clobber (cross-delivering approval/clarify
        // events and wiping each other's sender on exit).
        let sse_sender = tx.clone();
        let mut s = sink::SseSink::new(tx);

        let BootstrapOutcome {
            session_id,
            messages,
            tools,
            loop_detector,
            processing_guard,
            lifecycle_guard,
            mut command_output,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor,
            claude_md_content,
            turn_model_override,
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.ok_or_else(|| anyhow::anyhow!("bootstrap did not set lifecycle_guard"))?;
        // T2: ownership-gate this turn's terminal run_status writes against
        // same-session supersede. `stream_job_id` is `Some` only on the real
        // POST /api/chat path (set after `register_with_token`); `None` for the
        // retry wrapper (`handle_sse`) and non-SSE transports, which never share
        // a session row with a concurrent registry supersede.
        lifecycle_guard.set_stream_job_id(stream_job_id);
        let mut compressor = compressor;

        // F036: publish THIS session's SSE sender so the approval/clarify
        // managers can target it specifically. The RAII guard removes only this
        // session's entry on every exit path (bootstrap here already succeeded).
        let installed_sse_sender = sse_sender.clone();
        self.sse_event_tx().insert(session_id, sse_sender);
        let _sse_sender_guard = SseSenderGuard {
            map: self.sse_event_tx().clone(),
            session_id,
            installed: installed_sse_sender,
        };

        // Session-recovery prompt for the interactive behaviour layer. Sourced
        // from bootstrap's `enriched_text` (post PII-redaction / URL-enrichment)
        // because `execute_sse` no longer receives the raw `IncomingMessage`
        // (the split keeps `msg` in `bootstrap_sse`). Only consumed on the rare
        // session-corruption recovery path; the small divergence from the raw
        // `msg.text` the pre-split code used is immaterial there.
        let recovery_text = enriched_text.clone();

        // Emit SessionId so the UI can track which session is active and display the context bar.
        let _ = s
            .emit(PipelineEvent::Stream(StreamEvent::SessionId {
                session_id: session_id.to_string(),
                context_limit: compressor.context_limit,
            }))
            .await;

        let boot_for_execute = BootstrapOutcome {
            lifecycle_guard: None,
            command_output: None,
            session_id,
            messages,
            tools,
            loop_detector,
            processing_guard,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor: crate::agent::compressor::Compressor::new(0), // placeholder; real compressor passed separately
            claude_md_content,
            turn_model_override,
        };

        // Slash-command early exit
        if let Some(outcome) = command_output.take() {
            match outcome {
                CommandOutcome::Menu { card } => {
                    let card_type = card
                        .get("card_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("command_args_menu")
                        .to_string();
                    let slash_msg_id = opex_types::ids::MessageId::new();
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::MessageStart {
                            message_id: slash_msg_id,
                        }))
                        .await;
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::RichCard { card_type, data: card }))
                        .await;
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::Finish {
                            finish_reason: "command".to_string(),
                            continuation: false,
                        }))
                        .await;

                    let fin_ctx = finalize::finalize_context_from_engine(
                        self,
                        session_id,
                        boot_for_execute.messages.len(),
                        Some(user_message_id),
                        compressor,
                        slash_msg_id.as_uuid(), // same UUID as MessageStart so DB row ID matches SSE event
                    );
                    // The card IS the response — no assistant text to persist.
                    finalize::finalize(
                        fin_ctx,
                        finalize::FinalizeOutcome::Done {
                            assistant_text: String::new(),
                            thinking_json: None,
                            turn_limited: false,
                        },
                        &mut s,
                        &mut lifecycle_guard,
                    )
                    .await?;
                    return Ok(());
                }
                CommandOutcome::Text(text) => {
                    let slash_msg_id = opex_types::ids::MessageId::new();
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::MessageStart {
                            message_id: slash_msg_id,
                        }))
                        .await;
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                        .await;
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::Finish {
                            finish_reason: "command".to_string(),
                            continuation: false,
                        }))
                        .await;

                    let fin_ctx = finalize::finalize_context_from_engine(
                        self,
                        session_id,
                        boot_for_execute.messages.len(),
                        Some(user_message_id),
                        compressor,
                        slash_msg_id.as_uuid(), // same UUID as MessageStart so DB row ID matches SSE event
                    );
                    finalize::finalize(
                        fin_ctx,
                        finalize::FinalizeOutcome::Done {
                            assistant_text: text,
                            thinking_json: None,
                            turn_limited: false,
                        },
                        &mut s,
                        &mut lifecycle_guard,
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        // Full pipeline — `cancel` token is the same one registered with the
        // StreamRegistry, so POST /api/chat/{id}/abort propagates here (C3).
        // We MUST always emit a Finish event before returning so the SSE stream
        // closes cleanly for the frontend. On any error path (execute/finalize
        // returning Err, panic via `?`), the early-return would close the sink
        // without Finish — frontend then loops trying to resume a finalized
        // session. Wrap the pipeline so Finish is guaranteed on every exit.
        // Serialize this user turn against the goal driver (no-op when the engine
        // has no goal infrastructure). Acquired unconditionally — independent of
        // pool membership — to close the driver spawn-window TOCTOU (FIX C2).
        // H4 fix: cancel- and timeout-aware — a stuck goal driver must not block
        // the user turn indefinitely with no UI recourse.
        let _goal_guard = match crate::agent::goal::pool::user_turn_goal_guard_cancelable(
            self.cfg().goal_locks.as_ref(),
            session_id,
            cancel.clone(),
            GOAL_LOCK_ACQUIRE_TIMEOUT,
        )
        .await
        {
            Ok(g) => g,
            Err(reason) => {
                tracing::warn!(
                    session = %session_id,
                    reason,
                    "goal guard acquisition bailed out; finalizing as interrupted"
                );
                let fin_ctx = finalize::finalize_context_from_engine(
                    self,
                    session_id,
                    boot_for_execute.messages.len(),
                    Some(user_message_id),
                    crate::agent::compressor::Compressor::new(0),
                    uuid::Uuid::nil(),
                );
                let _: anyhow::Result<String> = finalize::finalize(
                    fin_ctx,
                    finalize::FinalizeOutcome::Interrupted {
                        partial: String::new(),
                        reason: reason.to_string(),
                    },
                    &mut s,
                    &mut lifecycle_guard,
                )
                .await;
                return Ok(());
            }
        };
        // Interactive layers: fallback provider + session-corruption recovery
        // (no-op without a configured fallback). Stops a recoverable provider
        // outage / corrupt-context error from failing the live web turn.
        let interactive_layers =
            BehaviourLayers::for_interactive(&self.tool_loop_config(), recovery_text);
        let pipeline_result: anyhow::Result<()> = async {
            let outcome = execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &interactive_layers).await?;
            let mut fin_ctx = finalize::finalize_context_from_engine(
                self,
                session_id,
                outcome.messages_len_at_end,
                // Final assistant parent = end of intermediate chain (last tool
                // result or intermediate assistant with tool_calls) persisted by
                // pipeline::execute. For no-tool turns this equals user_message_id.
                Some(outcome.final_parent_msg_id),
                compressor,
                outcome.assistant_message_id,
            );
            // Z3 fix: attribute usage/failure rows to the provider that
            // actually served the final call, not always the primary.
            if let Some(ref name) = outcome.effective_provider_name {
                fin_ctx.llm_provider = Some(name.clone());
            }
            let fin_outcome = finalize::execute_status_to_finalize(
                outcome.status,
                outcome.final_text,
                outcome.thinking_json,
            );
            finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await?;
            Ok(())
        }
        .await;

        if let Err(ref e) = pipeline_result {
            // Hard error path — execute/finalize threw. Tell the client so the
            // UI can render an error banner and stop the loading animation.
            let msg = format!("pipeline error: {}", e);
            tracing::error!(session = %session_id, error = %e, "pipeline failed");
            // Capture the REAL failure reason in session_failures instead of the
            // guard's opaque "guard dropped (early exit)" fallback (idempotent —
            // no-op if finalize already resolved the guard).
            self.record_hard_error(&mut lifecycle_guard, session_id, msg.clone()).await;
            let _ = s.emit(PipelineEvent::Stream(StreamEvent::Error(msg))).await;
            let _ = s
                .emit(PipelineEvent::Stream(StreamEvent::Finish {
                    finish_reason: "error".to_string(),
                    continuation: false,
                }))
                .await;
        }

        pipeline_result?;

        // Trim old messages if the agent's session.max_messages is configured.
        // Missed during the pipeline refactor (Tasks 7-10 dropped the tail call).
        self.maybe_trim_session(session_id).await;

        Ok(())
    }

    /// If the chat has voice mode `on`, dispatch the final assistant text as a
    /// voice message by reusing the `synthesize_speech` YAML tool's channel-action
    /// path (background TTS → `send_voice`). Best-effort: never blocks or fails the turn.
    async fn maybe_auto_tts(&self, msg: &IncomingMessage, final_text: &str) {
        if final_text.trim().is_empty() {
            return;
        }
        let chat_id = match msg.context.get("chat_id") {
            Some(v) => v.to_string().trim_matches('"').to_string(),
            None => return, // web/UI turn — no chat to voice
        };
        if chat_id.is_empty() || chat_id == "null" {
            return;
        }
        let mode = crate::db::channel_voice_modes::get_voice_mode(&self.cfg().db, &msg.channel, &chat_id)
            .await
            .unwrap_or_else(|_| "off".to_string());
        if mode != "on" {
            return;
        }
        let tool = match crate::agent::capability_tools::resolve_tool(
            &self.cfg().workspace_dir, &self.cfg().profile_slots, "synthesize_speech",
        ).await {
            Some(t) => t,
            None => {
                tracing::warn!("auto-tts: synthesize_speech unavailable (no tts provider active?)");
                return;
            }
        };
        let Some(ca) = tool.channel_action.clone() else {
            tracing::warn!("auto-tts: synthesize_speech has no channel_action");
            return;
        };
        let ctx = crate::agent::pipeline::CommandContext {
            cfg: self.cfg(),
            state: self.state(),
            tex: self.tex(),
            subagent_depth: 0,
        };
        let args = serde_json::json!({ "text": final_text, "_context": msg.context });
        let result =
            crate::agent::pipeline::channel_actions::execute_yaml_channel_action(&ctx, &tool, &args, &ca)
                .await;
        tracing::debug!(channel = %msg.channel, "auto-tts dispatched: {result}");
    }

    /// Handle with optional status callback for real-time phase updates.
    /// `chunk_tx` — optional channel for streaming response chunks to the caller.
    ///
    /// Thin adapter over pipeline::{bootstrap, execute, finalize} using `ChannelStatusSink`.
    pub async fn handle_with_status(
        &self,
        msg: &IncomingMessage,
        status_tx: Option<tokio::sync::mpsc::UnboundedSender<ProcessingPhase>>,
        chunk_tx: Option<tokio::sync::mpsc::Sender<String>>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String> {
        self.cfg().approval_manager.prune_stale().await;

        let hook_event = crate::agent::hooks::HookEvent::BeforeMessage;
        let action = self.hooks().fire(&hook_event);
        self.hooks().fire_webhooks(&hook_event);
        if let crate::agent::hooks::HookAction::Block(reason) = action {
            anyhow::bail!("blocked by hook: {}", reason);
        }
        // R-DRAIN: register the caller's real cancel token + RAII guard (see
        // handle_sse). Graceful shutdown can now interrupt a live channel turn.
        let _req_guard = self.state.register_request_guarded(cancel.clone());

        let mut s = sink::ChannelStatusSink::new(status_tx, chunk_tx);

        let boot = bootstrap::bootstrap(
            self,
            BootstrapContext {
                msg,
                resume_session_id: None,
                force_new_session: false,
            },
            &mut s,
        )
        .await?;

        let BootstrapOutcome {
            session_id,
            messages,
            tools,
            loop_detector,
            processing_guard,
            lifecycle_guard,
            mut command_output,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor,
            claude_md_content,
            turn_model_override,
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.ok_or_else(|| anyhow::anyhow!("bootstrap did not set lifecycle_guard"))?;
        let mut compressor = compressor;
        let boot_for_execute = BootstrapOutcome {
            lifecycle_guard: None,
            command_output: None,
            session_id,
            messages,
            tools,
            loop_detector,
            processing_guard,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor: crate::agent::compressor::Compressor::new(0), // placeholder; real compressor passed separately
            claude_md_content,
            turn_model_override,
        };

        // Channel adapters render slash commands as plain TextDelta (Text)
        // or a rich menu card (Menu — emitted as a RichCard; button
        // conversion is a later task).
        if let Some(outcome) = command_output.take() {
            match outcome {
                CommandOutcome::Menu { card } => {
                    // Choice-valve command menu (Task 2's `command_args_menu`
                    // card with `options` + `token`): render clickable inline
                    // buttons via the existing send_buttons channel action
                    // (mirrors the handler_menu flow below) instead of dumping
                    // the prompt as plain text.
                    let text = card.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if let Some(buttons) = argsmenu_buttons(&card) {
                        // `channel_router_ref()` is constructed per-agent
                        // unconditionally, so it's essentially always `Some`
                        // regardless of whether a channel is actually
                        // connected. The real signal is `router.send()`
                        // returning `Err` when the target channel has no
                        // registered adapter. Fall back to plain text on
                        // either `None` router or a failed send, so the
                        // caller is never left with a silent no-op turn.
                        let delivered = match self.channel_router_ref() {
                            Some(router) => {
                                let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
                                let action = crate::agent::channel_actions::ChannelAction {
                                    name: "send_buttons".to_string(),
                                    params: serde_json::json!({ "text": text, "buttons": buttons }),
                                    context: msg.context.clone(),
                                    reply: reply_tx,
                                    target_channel: Some(msg.channel.clone()),
                                };
                                router.send(action).await.is_ok()
                            }
                            None => false,
                        };
                        if !delivered {
                            let _ = s
                                .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                                .await;
                        }
                        let fin_ctx = finalize::finalize_context_from_engine(
                            self,
                            session_id,
                            boot_for_execute.messages.len(),
                            Some(user_message_id),
                            compressor,
                            uuid::Uuid::new_v4(), // slash-command path: no MessageStart was sent
                        );
                        return finalize::finalize(
                            fin_ctx,
                            finalize::FinalizeOutcome::Done {
                                assistant_text: if delivered { String::new() } else { text },
                                thinking_json: None,
                                turn_limited: false,
                            },
                            &mut s,
                            &mut lifecycle_guard,
                        )
                        .await;
                    }
                    // No options/token (e.g. the missing-source prompt): fall
                    // back to plain text so the user isn't left with a silent
                    // no-op turn (e.g. bare `/summarize_video` on Telegram).
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                        .await;
                    let fin_ctx = finalize::finalize_context_from_engine(
                        self,
                        session_id,
                        boot_for_execute.messages.len(),
                        Some(user_message_id),
                        compressor,
                        uuid::Uuid::new_v4(), // slash-command path: no MessageStart was sent
                    );
                    return finalize::finalize(
                        fin_ctx,
                        finalize::FinalizeOutcome::Done {
                            assistant_text: text,
                            thinking_json: None,
                            turn_limited: false,
                        },
                        &mut s,
                        &mut lifecycle_guard,
                    )
                    .await;
                }
                CommandOutcome::Text(text) => {
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                        .await;
                    let fin_ctx = finalize::finalize_context_from_engine(
                        self,
                        session_id,
                        boot_for_execute.messages.len(),
                        Some(user_message_id),
                        compressor,
                        uuid::Uuid::new_v4(), // slash-command path: no MessageStart was sent
                    );
                    return finalize::finalize(
                        fin_ctx,
                        finalize::FinalizeOutcome::Done {
                            assistant_text: text,
                            thinking_json: None,
                            turn_limited: false,
                        },
                        &mut s,
                        &mut lifecycle_guard,
                    )
                    .await;
                }
            }
        }

        // Serialize this user turn against the goal driver for the session.
        // Acquired unconditionally — independent of pool membership — so the
        // autonomous loop and a real user turn never execute on the same session
        // concurrently, even in the driver spawn window before it joins the pool
        // (FIX C2). No-op when the engine has no goal infrastructure.
        // H4 fix: cancel- and timeout-aware so a stuck goal driver cannot block
        // the channel turn indefinitely — the dispatcher's `cancel` and a 30s
        // upper bound bail out cleanly with a visible Interrupted outcome.
        let _goal_guard = match crate::agent::goal::pool::user_turn_goal_guard_cancelable(
            self.cfg().goal_locks.as_ref(),
            session_id,
            cancel.clone(),
            GOAL_LOCK_ACQUIRE_TIMEOUT,
        )
        .await
        {
            Ok(g) => g,
            Err(reason) => {
                tracing::warn!(
                    session = %session_id,
                    reason,
                    "goal guard acquisition bailed out; finalizing as interrupted"
                );
                let fin_ctx = finalize::finalize_context_from_engine(
                    self,
                    session_id,
                    boot_for_execute.messages.len(),
                    Some(user_message_id),
                    crate::agent::compressor::Compressor::new(0),
                    uuid::Uuid::nil(),
                );
                let _: anyhow::Result<String> = finalize::finalize(
                    fin_ctx,
                    finalize::FinalizeOutcome::Interrupted {
                        partial: String::new(),
                        reason: reason.to_string(),
                    },
                    &mut s,
                    &mut lifecycle_guard,
                )
                .await;
                return Err(anyhow::anyhow!("goal_guard_{}", reason));
            }
        };

        // `cancel` is supplied by the caller (channel dispatcher) so a request
        // timeout / WS disconnect / `/stop` can break this turn COOPERATIVELY —
        // execute() observes the token, returns Interrupted, and finalize marks
        // the session 'interrupted' (resumable) instead of the dispatcher
        // hard-aborting the task and the guard Drop marking it 'failed'.
        // Same interactive layers as the SSE path — channel turns equally need
        // fallback + session-corruption recovery so a recoverable error doesn't
        // fail the turn and (with R-CONTINUITY) keeps the conversation alive.
        let interactive_layers =
            BehaviourLayers::for_interactive(&self.tool_loop_config(), msg.text.clone().unwrap_or_default());
        // Intercept a hard `Err` from execute BEFORE `compressor` is consumed by
        // finalize below, so we can record the REAL failure reason instead of the
        // guard's opaque "early exit" fallback (the telegram/channel path bug).
        let outcome = match execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &interactive_layers).await {
            Ok(o) => o,
            Err(e) => {
                // H3 fix: route the error through finalize so partial text (if
                // any — execute may have streamed some before the Err) is
                // persisted, the session lifecycle transitions cleanly, and
                // the channel-adapter / UI reload stays consistent. Returning
                // Err bare would rely on the lifecycle-guard Drop fallback and
                // produce an opaque "guard_dropped" diagnostic with no assistant
                // row — leaving an orphan user message in the conversation.
                let reason = format!("pipeline error: {}", e);
                tracing::error!(session = %session_id, error = %e, "pipeline failed");
                let fin_ctx = finalize::finalize_context_from_engine(
                    self,
                    session_id,
                    0,
                    Some(user_message_id),
                    compressor,
                    uuid::Uuid::nil(),
                );
                let _: anyhow::Result<String> = finalize::finalize(
                    fin_ctx,
                    finalize::FinalizeOutcome::Failed {
                        partial: String::new(),
                        reason: reason.clone(),
                    },
                    &mut s,
                    &mut lifecycle_guard,
                )
                .await;
                return Err(e);
            }
        };

        let fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            // Final assistant parent = end of intermediate chain (last tool
            // result or intermediate assistant with tool_calls) persisted by
            // pipeline::execute. For no-tool turns this equals user_message_id.
            Some(outcome.final_parent_msg_id),
            compressor,
            outcome.assistant_message_id,
        );
        let fin_outcome = finalize::execute_status_to_finalize(
            outcome.status,
            outcome.final_text,
            outcome.thinking_json,
        );
        let result =
            finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await;

        // Channel handler-menu → clickable inline buttons on channels that render
        // them (e.g. Telegram). Reuses the existing `send_buttons` action; the
        // button `data` carries only `hm:<token>:<handler_id>` (Telegram's
        // callback_data ≤64 bytes), with the source/session/agent stashed under
        // the token and recovered by POST /api/files/menu-run on click.
        if let Some(mut menu) = s.menu.take()
            && let Some(router) = self.channel_router_ref()
            && let Some(handlers) = menu.get("handlers").and_then(|v| v.as_array()).cloned()
            && !handlers.is_empty()
        {
            // Bind the menu to the originating chat so a leaked token can't be
            // replayed from another chat (verified in run_menu_token_handler).
            if let Some(chat) = msg.context.get("chat_id").cloned()
                && let Some(obj) = menu.as_object_mut()
            {
                obj.insert("_chat_id".to_string(), chat);
            }
            let token = crate::gateway::handlers::files::store_menu_ctx(menu.clone());
            let buttons: Vec<serde_json::Value> = handlers
                .iter()
                .map(|h| {
                    let id = h.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let label = h.get("label").and_then(|v| v.as_str()).unwrap_or(id);
                    serde_json::json!({ "text": label, "data": format!("hm:{token}:{id}") })
                })
                .collect();
            let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
            let action = crate::agent::channel_actions::ChannelAction {
                name: "send_buttons".to_string(),
                params: serde_json::json!({ "text": "Выберите действие:", "buttons": buttons }),
                context: msg.context.clone(),
                reply: reply_tx,
                target_channel: Some(msg.channel.clone()),
            };
            let _ = router.send(action).await;
        }

        self.maybe_trim_session(session_id).await;
        if let Ok(ref final_text) = result {
            self.maybe_auto_tts(msg, final_text).await;
        }
        result
    }

    /// Handle with streaming: sends content chunks via mpsc channel for progressive display.
    ///
    /// Thin adapter over pipeline::{bootstrap, execute, finalize} using `ChunkSink`.
    ///
    /// `cancel` is the caller-controlled cancellation token (typically owned by
    /// the WS handler). It is registered with `state` so graceful shutdown
    /// (`cancel_all_requests`) can also reach this turn — closing C3 (the
    /// streaming path used to spawn a fresh token that nobody could cancel).
    pub async fn handle_streaming(
        &self,
        msg: &IncomingMessage,
        chunk_tx: tokio::sync::mpsc::Sender<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String> {
        let mut s = sink::ChunkSink::new(chunk_tx);

        let boot = bootstrap::bootstrap(
            self,
            BootstrapContext {
                msg,
                resume_session_id: None,
                force_new_session: false,
            },
            &mut s,
        )
        .await?;

        let BootstrapOutcome {
            session_id,
            messages,
            tools,
            loop_detector,
            processing_guard,
            lifecycle_guard,
            mut command_output,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor,
            claude_md_content,
            turn_model_override,
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.ok_or_else(|| anyhow::anyhow!("bootstrap did not set lifecycle_guard"))?;
        let mut compressor = compressor;
        let boot_for_execute = BootstrapOutcome {
            lifecycle_guard: None,
            command_output: None,
            session_id,
            messages,
            tools,
            loop_detector,
            processing_guard,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor: crate::agent::compressor::Compressor::new(0), // placeholder; real compressor passed separately
            claude_md_content,
            turn_model_override,
        };

        if let Some(outcome) = command_output.take() {
            match outcome {
                CommandOutcome::Menu { card } => {
                    // Choice-valve command menu: same treatment as
                    // handle_with_status — render inline buttons via the
                    // channel router when the card carries options+token and
                    // a router is available, else fall back to plain text
                    // (chunk transport can't render the RichCard).
                    let text = card.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if let Some(buttons) = argsmenu_buttons(&card) {
                        // `channel_router_ref()` is constructed per-agent
                        // unconditionally, so it's essentially always `Some`
                        // regardless of whether a channel is actually
                        // connected — notably `handle_streaming` also serves
                        // the admin "Chat" WS test surface with
                        // `channel = "ui"`, which no adapter registers. The
                        // real signal is `router.send()` returning `Err`.
                        // Fall back to plain text on either `None` router or
                        // a failed send so the caller is never left with a
                        // silent no-op turn.
                        let delivered = match self.channel_router_ref() {
                            Some(router) => {
                                let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
                                let action = crate::agent::channel_actions::ChannelAction {
                                    name: "send_buttons".to_string(),
                                    params: serde_json::json!({ "text": text, "buttons": buttons }),
                                    context: msg.context.clone(),
                                    reply: reply_tx,
                                    target_channel: Some(msg.channel.clone()),
                                };
                                router.send(action).await.is_ok()
                            }
                            None => false,
                        };
                        if !delivered {
                            let _ = s
                                .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                                .await;
                        }
                        let fin_ctx = finalize::finalize_context_from_engine(
                            self,
                            session_id,
                            boot_for_execute.messages.len(),
                            Some(user_message_id),
                            compressor,
                            uuid::Uuid::new_v4(), // slash-command path: no MessageStart was sent
                        );
                        return finalize::finalize(
                            fin_ctx,
                            finalize::FinalizeOutcome::Done {
                                assistant_text: if delivered { String::new() } else { text },
                                thinking_json: None,
                                turn_limited: false,
                            },
                            &mut s,
                            &mut lifecycle_guard,
                        )
                        .await;
                    }
                    // Chunk transport can't render the RichCard; deliver the
                    // prompt text instead so the user isn't left with a silent
                    // no-op turn.
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                        .await;
                    let fin_ctx = finalize::finalize_context_from_engine(
                        self,
                        session_id,
                        boot_for_execute.messages.len(),
                        Some(user_message_id),
                        compressor,
                        uuid::Uuid::new_v4(), // slash-command path: no MessageStart was sent
                    );
                    return finalize::finalize(
                        fin_ctx,
                        finalize::FinalizeOutcome::Done {
                            assistant_text: text,
                            thinking_json: None,
                            turn_limited: false,
                        },
                        &mut s,
                        &mut lifecycle_guard,
                    )
                    .await;
                }
                CommandOutcome::Text(text) => {
                    let _ = s
                        .emit(PipelineEvent::Stream(StreamEvent::TextDelta(text.clone())))
                        .await;
                    let fin_ctx = finalize::finalize_context_from_engine(
                        self,
                        session_id,
                        boot_for_execute.messages.len(),
                        Some(user_message_id),
                        compressor,
                        uuid::Uuid::new_v4(), // slash-command path: no MessageStart was sent
                    );
                    return finalize::finalize(
                        fin_ctx,
                        finalize::FinalizeOutcome::Done {
                            assistant_text: text,
                            thinking_json: None,
                            turn_limited: false,
                        },
                        &mut s,
                        &mut lifecycle_guard,
                    )
                    .await;
                }
            }
        }

        let cancel = cancel;
        // C3 fix: register the streaming turn's cancel token with state so
        // graceful shutdown (cancel_all_requests on SIGTERM/SIGHUP) can reach
        // it — same invariant the SSE and channel-status paths already uphold.
        let _req_guard = self.state.register_request_guarded(cancel.clone());
        let interactive_layers =
            BehaviourLayers::for_interactive(&self.tool_loop_config(), msg.text.clone().unwrap_or_default());
        // Record the real reason on hard error rather than the guard's opaque
        // "early exit" fallback (see handle_sse / handle_with_status).
        let outcome = match execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &interactive_layers).await {
            Ok(o) => o,
            Err(e) => {
                // H3 fix: route through finalize so the partial reply (if any)
                // is persisted and the session lifecycle transitions cleanly
                // instead of relying on the lifecycle-guard Drop fallback that
                // produces an opaque diagnostic with no assistant row.
                let reason = format!("pipeline error: {}", e);
                tracing::error!(session = %session_id, error = %e, "pipeline failed");
                let fin_ctx = finalize::finalize_context_from_engine(
                    self,
                    session_id,
                    0,
                    Some(user_message_id),
                    compressor,
                    uuid::Uuid::nil(),
                );
                let _: anyhow::Result<String> = finalize::finalize(
                    fin_ctx,
                    finalize::FinalizeOutcome::Failed {
                        partial: String::new(),
                        reason: reason.clone(),
                    },
                    &mut s,
                    &mut lifecycle_guard,
                )
                .await;
                return Err(e);
            }
        };

        let mut fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            // Final assistant parent = end of intermediate chain (last tool
            // result or intermediate assistant with tool_calls) persisted by
            // pipeline::execute. For no-tool turns this equals user_message_id.
            Some(outcome.final_parent_msg_id),
            compressor,
            outcome.assistant_message_id,
        );
        // Z3 fix: attribute to the effective provider, not always primary.
        if let Some(ref name) = outcome.effective_provider_name {
            fin_ctx.llm_provider = Some(name.clone());
        }
        let fin_outcome = finalize::execute_status_to_finalize(
            outcome.status,
            outcome.final_text,
            outcome.thinking_json,
        );
        finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await
    }

    /// RPC-style isolated turn — used by cron jobs and other callers that
    /// just want a final assistant string back. Same `pipeline::{bootstrap,
    /// execute, finalize}` route as `handle_sse`, but with:
    ///
    ///   * `force_new_session: true` so each call gets a clean session with
    ///     no prior message history.
    ///   * `BehaviourLayers::for_cron(...)` enabled — fallback provider,
    ///     auto-continue, session-corruption recovery, tool-policy override,
    ///     forced-final-call all engaged with the same defaults the legacy
    ///     `handle_isolated` used.
    ///   * `NoopSink` instead of `SseSink` — the caller doesn't observe
    ///     stream events, only the final text.
    ///
    /// Returns the final assistant text (or a graceful user-facing error
    /// message when the LLM call failed unrecoverably). The same DB
    /// row-shape and timeline lifecycle the SSE path produces — cron runs are
    /// now first-class sessions in `messages` and `session_timeline`.
    pub async fn handle_isolated_via_pipeline(
        &self,
        msg: &IncomingMessage,
    ) -> Result<String> {
        let hook_event = crate::agent::hooks::HookEvent::BeforeMessage;
        let action = self.hooks().fire(&hook_event);
        self.hooks().fire_webhooks(&hook_event);
        if let crate::agent::hooks::HookAction::Block(reason) = action {
            anyhow::bail!("blocked by hook: {}", reason);
        }
        // Fresh session (cron RPC semantics). Cron has no external cancel
        // token, so pass a fresh (never-cancelled) one.
        self.run_isolated_pipeline(msg, None, true, tokio_util::sync::CancellationToken::new()).await
    }

    /// Run ONE autonomous goal turn that CONTINUES an existing session (history
    /// loaded) and returns the final assistant text. Used by the `/goal` driver.
    ///
    /// `cancel` is the driver's cancellation token (R-GOAL): when `/goal stop`
    /// fires, it propagates into `execute()` so a long in-flight turn breaks
    /// cooperatively and reaches `finalize` (marking the session `interrupted`,
    /// not `failed` via a hard task abort).
    pub async fn run_goal_turn(
        &self,
        session_id: Uuid,
        prompt: &str,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String> {
        let msg = IncomingMessage {
            user_id: "system".to_string(),
            context: serde_json::Value::Null,
            text: Some(prompt.to_string()),
            attachments: vec![],
            agent_id: self.cfg().agent.name.clone(),
            channel: crate::agent::channel_kind::channel::CRON.to_string(),
            timestamp: chrono::Utc::now(),
            formatting_prompt: None,
            tool_policy_override: None,
            leaf_message_id: None,
            user_message_id: None,
        };
        self.run_isolated_pipeline(&msg, Some(session_id), false, cancel).await
    }

    /// Shared RPC-style pipeline body: `NoopSink`, cron behaviour layers, returns the
    /// final assistant text. `handle_isolated_via_pipeline` calls it with a fresh
    /// session; `run_goal_turn` resumes an existing one.
    async fn run_isolated_pipeline(
        &self,
        msg: &IncomingMessage,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<String> {
        // R-DRAIN: register the pipeline cancel token so graceful shutdown's
        // `cancel_all_requests` propagates into this turn's `execute()` and
        // `wait_drain` blocks for it. The other three entry points
        // (handle_sse / handle_with_status / handle_streaming) already do
        // this; the isolated RPC path (cron one-shot + `/goal` turns) was
        // missing it, so in-flight cron/goal turns were invisible to shutdown
        // — they kept running while toolgate/DB were torn down underneath
        // them, and a `cron_runs` row could stick in 'running' forever.
        let _req_guard = self.state.register_request_guarded(cancel.clone());

        let mut s = sink::NoopSink::new();

        let boot = match tokio::time::timeout(
            BOOTSTRAP_HARD_TIMEOUT,
            bootstrap::bootstrap(
                self,
                BootstrapContext {
                    msg,
                    resume_session_id,
                    force_new_session,
                },
                &mut s,
            ),
        )
        .await
        {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                anyhow::bail!(
                    "bootstrap exceeded {}s hard timeout; toolgate or embedding may be unresponsive",
                    BOOTSTRAP_HARD_TIMEOUT.as_secs()
                );
            }
        };

        let BootstrapOutcome {
            session_id,
            mut messages,
            mut tools,
            loop_detector,
            processing_guard,
            lifecycle_guard,
            command_output: _,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor,
            claude_md_content,
            turn_model_override,
            // Cron/goal RPC turns run the LLM loop unconditionally — the async-video
            // short-circuit is a live-user-turn affordance only.
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.ok_or_else(|| anyhow::anyhow!("bootstrap did not set lifecycle_guard"))?;
        let mut compressor = compressor;

        // Build behaviour layers for the cron-style call site.
        let loop_config = self.tool_loop_config();
        let layers = BehaviourLayers::for_cron(&loop_config, msg);

        // Apply tool-policy override at the bootstrap boundary (A4 in the
        // divergent-feature map). The layer carries the policy; we apply
        // it to `tools` here so `pipeline::execute` sees the narrowed set.
        // Logged so cron-job operators see the override taking effect.
        if let Some(ref override_layer) = layers.tool_policy_override {
            let before = tools.len();
            tools = self.apply_tool_policy_override(tools, &override_layer.policy);
            if tools.len() != before {
                tracing::info!(
                    agent = %self.cfg().agent.name,
                    before,
                    after = tools.len(),
                    "cron tool policy override applied"
                );
            }
        }

        let boot_for_execute = BootstrapOutcome {
            lifecycle_guard: None,
            command_output: None,
            session_id,
            messages: std::mem::take(&mut messages),
            tools,
            loop_detector,
            processing_guard,
            enriched_text,
            user_message_id,
            incoming_context,
            channel,
            compressor: crate::agent::compressor::Compressor::new(0), // placeholder; real compressor passed separately
            claude_md_content,
            turn_model_override,
        };

        let outcome = match execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &layers).await {
            Ok(o) => o,
            Err(e) => {
                // Cron / agent-to-agent path: record the real failure reason so
                // the guard's opaque "early exit" fallback is not the only trace.
                let msg = format!("pipeline error: {}", e);
                tracing::error!(session = %session_id, error = %e, "pipeline failed");
                self.record_hard_error(&mut lifecycle_guard, session_id, msg).await;
                return Err(e);
            }
        };
        let mut fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            Some(outcome.final_parent_msg_id),
            compressor,
            outcome.assistant_message_id,
        );
        // Z3 fix: attribute to the effective provider, not always primary.
        if let Some(ref name) = outcome.effective_provider_name {
            fin_ctx.llm_provider = Some(name.clone());
        }
        let fin_outcome = finalize::execute_status_to_finalize(
            outcome.status,
            outcome.final_text,
            outcome.thinking_json,
        );
        finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await
    }
}

#[cfg(test)]
mod tests {
    use super::argsmenu_buttons;
    use std::path::Path;

    fn source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agent/engine/run.rs");
        std::fs::read_to_string(&path).expect("read engine/run.rs")
    }

    #[test]
    fn argsmenu_buttons_builds_cm_callback_data() {
        let card = serde_json::json!({
            "card_type": "command_args_menu",
            "command": "transcribe",
            "text": "Выберите значение «lang» для /transcribe:",
            "options": [
                {"value": "ru", "label": "ru"},
                {"value": "en", "label": "en"},
            ],
            "token": "abc123",
        });
        let buttons = argsmenu_buttons(&card).expect("options+token card must yield buttons");
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0]["text"], "ru");
        assert_eq!(buttons[0]["data"], "cm:abc123:ru");
        assert_eq!(buttons[1]["data"], "cm:abc123:en");
    }

    #[test]
    fn argsmenu_buttons_none_without_token() {
        let card = serde_json::json!({
            "card_type": "command_args_menu",
            "command": "transcribe",
            "text": "Пришлите ссылку или файл для /transcribe.",
        });
        assert!(argsmenu_buttons(&card).is_none());
    }

    #[test]
    fn argsmenu_buttons_none_with_empty_options() {
        let card = serde_json::json!({
            "card_type": "command_args_menu",
            "text": "prompt",
            "options": [],
            "token": "abc123",
        });
        assert!(argsmenu_buttons(&card).is_none());
    }

    #[test]
    fn handle_sse_uses_bootstrap_and_execute() {
        let src = source();
        assert!(
            src.contains("bootstrap::bootstrap"),
            "handle_sse must call pipeline::bootstrap"
        );
        assert!(
            src.contains("execute::execute"),
            "handle_sse must call pipeline::execute"
        );
        assert!(
            src.contains("finalize::finalize"),
            "handle_sse must call pipeline::finalize"
        );
    }

    #[test]
    fn handle_sse_emits_session_id() {
        let src = source();
        assert!(
            src.contains("StreamEvent::SessionId"),
            "handle_sse must emit SessionId so UI can track the session"
        );
    }

    #[test]
    fn slash_command_path_emits_finish() {
        let src = source();
        assert!(
            src.contains(r#"finish_reason: "command""#),
            "slash-command path must emit Finish with finish_reason=command"
        );
    }
}
