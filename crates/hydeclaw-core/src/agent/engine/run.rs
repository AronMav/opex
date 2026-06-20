//! Three thin adapter methods on AgentEngine. Each constructs an EventSink
//! and delegates to pipeline::execute. See spec §3 and the implementation
//! plan (Tasks 7–9) for rationale.

use anyhow::Result;
use hydeclaw_types::IncomingMessage;
use uuid::Uuid;

use super::stream::ProcessingPhase;
use super::AgentEngine;
use crate::agent::engine_event_sender::EngineEventSender;
use crate::agent::pipeline::behaviour::BehaviourLayers;
use crate::agent::pipeline::bootstrap::{self, BootstrapContext, BootstrapOutcome};
use crate::agent::pipeline::sink::{self, EventSink, PipelineEvent};
use crate::agent::pipeline::{execute, finalize};
use crate::agent::stream_event::StreamEvent;

impl AgentEngine {
    /// Handle message via SSE: thin adapter over pipeline::{bootstrap, execute, finalize}.
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
        let hook_event = crate::agent::hooks::HookEvent::BeforeMessage;
        let action = self.hooks().fire(&hook_event);
        self.hooks().fire_webhooks(&hook_event);
        if let crate::agent::hooks::HookAction::Block(reason) = action {
            anyhow::bail!("blocked by hook: {}", reason);
        }
        let _cancel_guard = self.state.register_request();

        // Publish the event sender so approval_manager can broadcast tool-approval
        // requests while the SSE stream is live. Cleared after finalize so idle
        // agents don't keep a dangling reference. Previously lost during the
        // pipeline refactor; restored 2026-04-20.
        //
        // C1: wrap inner logic so sse_event_tx is cleared on ALL exit paths
        // (including bootstrap error, finalize error, etc.) — previously a `?`
        // anywhere in the body could bypass the clear at the end of the function.
        *self.sse_event_tx().lock().await = Some(event_tx.clone());
        let result = self.handle_sse_inner(msg, event_tx, resume_session_id, force_new_session, cancel).await;
        *self.sse_event_tx().lock().await = None;
        result
    }

    async fn handle_sse_inner(
        &self,
        msg: &IncomingMessage,
        event_tx: EngineEventSender,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<Uuid> {
        let mut s = sink::SseSink::new(event_tx);

        let boot = bootstrap::bootstrap(
            self,
            BootstrapContext {
                msg,
                resume_session_id,
                force_new_session,
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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("bootstrap always sets lifecycle_guard");
        let mut compressor = compressor;

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
        };

        // Slash-command early exit
        if let Some(text) = command_output.take() {
            let slash_msg_id = hydeclaw_types::ids::MessageId::new();
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
                },
                &mut s,
                &mut lifecycle_guard,
            )
            .await?;
            return Ok(session_id);
        }

        // Full pipeline — `cancel` token is the same one registered with the
        // StreamRegistry, so POST /api/chat/{id}/abort propagates here (C3).
        // We MUST always emit a Finish event before returning so the SSE stream
        // closes cleanly for the frontend. On any error path (execute/finalize
        // returning Err, panic via `?`), the early-return would close the sink
        // without Finish — frontend then loops trying to resume a finalized
        // session. Wrap the pipeline so Finish is guaranteed on every exit.
        let pipeline_result: anyhow::Result<()> = async {
            let outcome = execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &BehaviourLayers::none()).await?;
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
            finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await?;
            Ok(())
        }
        .await;

        if let Err(ref e) = pipeline_result {
            // Hard error path — execute/finalize threw. Tell the client so the
            // UI can render an error banner and stop the loading animation.
            let msg = format!("pipeline error: {}", e);
            tracing::error!(session = %session_id, error = %e, "pipeline failed");
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

        Ok(session_id)
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
        let tool = match crate::tools::yaml_tools::find_yaml_tool(&self.cfg().workspace_dir, "synthesize_speech").await {
            Some(t) => t,
            None => {
                tracing::warn!("auto-tts: synthesize_speech tool not found");
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
    ) -> Result<String> {
        self.cfg().approval_manager.prune_stale().await;

        let hook_event = crate::agent::hooks::HookEvent::BeforeMessage;
        let action = self.hooks().fire(&hook_event);
        self.hooks().fire_webhooks(&hook_event);
        if let crate::agent::hooks::HookAction::Block(reason) = action {
            anyhow::bail!("blocked by hook: {}", reason);
        }
        let _cancel_guard = self.state.register_request();

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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("set by bootstrap");
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
        };

        // Channel adapters render slash commands as plain TextDelta
        if let Some(text) = command_output.take() {
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
                },
                &mut s,
                &mut lifecycle_guard,
            )
            .await;
        }

        let cancel = tokio_util::sync::CancellationToken::new();
        let outcome = execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &BehaviourLayers::none()).await?;

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
        self.maybe_trim_session(session_id).await;
        if let Ok(ref final_text) = result {
            self.maybe_auto_tts(msg, final_text).await;
        }
        result
    }

    /// Handle with streaming: sends content chunks via mpsc channel for progressive display.
    ///
    /// Thin adapter over pipeline::{bootstrap, execute, finalize} using `ChunkSink`.
    pub async fn handle_streaming(
        &self,
        msg: &IncomingMessage,
        chunk_tx: tokio::sync::mpsc::Sender<String>,
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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("set by bootstrap");
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
        };

        if let Some(text) = command_output.take() {
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
                },
                &mut s,
                &mut lifecycle_guard,
            )
            .await;
        }

        let cancel = tokio_util::sync::CancellationToken::new();
        let outcome = execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &BehaviourLayers::none()).await?;

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
        // Fresh session (cron RPC semantics).
        self.run_isolated_pipeline(msg, None, true).await
    }

    /// Run ONE autonomous goal turn that CONTINUES an existing session (history
    /// loaded) and returns the final assistant text. Used by the `/goal` driver.
    pub async fn run_goal_turn(&self, session_id: Uuid, prompt: &str) -> Result<String> {
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
        self.run_isolated_pipeline(&msg, Some(session_id), false).await
    }

    /// Shared RPC-style pipeline body: `NoopSink`, cron behaviour layers, returns the
    /// final assistant text. `handle_isolated_via_pipeline` calls it with a fresh
    /// session; `run_goal_turn` resumes an existing one.
    async fn run_isolated_pipeline(
        &self,
        msg: &IncomingMessage,
        resume_session_id: Option<Uuid>,
        force_new_session: bool,
    ) -> Result<String> {
        let mut s = sink::NoopSink::new();
        let cancel = tokio_util::sync::CancellationToken::new();

        let boot = bootstrap::bootstrap(
            self,
            BootstrapContext {
                msg,
                resume_session_id,
                force_new_session,
            },
            &mut s,
        )
        .await?;

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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("bootstrap always sets lifecycle_guard");
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
        };

        let outcome = execute::execute(self, boot_for_execute, &mut s, cancel, &mut compressor, &layers).await?;
        let fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            Some(outcome.final_parent_msg_id),
            compressor,
            outcome.assistant_message_id,
        );
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
    use std::path::Path;

    fn source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agent/engine/run.rs");
        std::fs::read_to_string(&path).expect("read engine/run.rs")
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
