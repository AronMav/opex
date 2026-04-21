//! Three thin adapter methods on AgentEngine. Each constructs an EventSink
//! and delegates to pipeline::execute. See spec §3 and the implementation
//! plan (Tasks 7–9) for rationale.

use anyhow::Result;
use hydeclaw_types::IncomingMessage;
use uuid::Uuid;

use super::stream::ProcessingPhase;
use super::AgentEngine;
use crate::agent::engine_event_sender::EngineEventSender;
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
        if let crate::agent::hooks::HookAction::Block(reason) =
            self.hooks().fire(&crate::agent::hooks::HookEvent::BeforeMessage)
        {
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
                use_history: true,
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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("bootstrap always sets lifecycle_guard");

        // Emit SessionId so the UI can track which session is active.
        let _ = s
            .emit(PipelineEvent::Stream(StreamEvent::SessionId(
                session_id.to_string(),
            )))
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
        };

        // Slash-command early exit
        if let Some(text) = command_output.take() {
            let msg_id = format!("msg_{}", Uuid::new_v4());
            let _ = s
                .emit(PipelineEvent::Stream(StreamEvent::MessageStart {
                    message_id: msg_id,
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
        let outcome = execute::execute(self, boot_for_execute, &mut s, cancel).await?;

        let fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            // Final assistant parent = end of intermediate chain (last tool
            // result or intermediate assistant with tool_calls) persisted by
            // pipeline::execute. For no-tool turns this equals user_message_id.
            Some(outcome.final_parent_msg_id),
        );
        let fin_outcome = finalize::execute_status_to_finalize(
            outcome.status,
            outcome.final_text,
            outcome.thinking_json,
        );
        finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await?;

        // Trim old messages if the agent's session.max_messages is configured.
        // Missed during the pipeline refactor (Tasks 7-10 dropped the tail call).
        self.maybe_trim_session(session_id).await;

        Ok(session_id)
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

        if let crate::agent::hooks::HookAction::Block(reason) =
            self.hooks().fire(&crate::agent::hooks::HookEvent::BeforeMessage)
        {
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
                use_history: true,
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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("set by bootstrap");
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
        let outcome = execute::execute(self, boot_for_execute, &mut s, cancel).await?;

        let fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            // Final assistant parent = end of intermediate chain (last tool
            // result or intermediate assistant with tool_calls) persisted by
            // pipeline::execute. For no-tool turns this equals user_message_id.
            Some(outcome.final_parent_msg_id),
        );
        let fin_outcome = finalize::execute_status_to_finalize(
            outcome.status,
            outcome.final_text,
            outcome.thinking_json,
        );
        let result =
            finalize::finalize(fin_ctx, fin_outcome, &mut s, &mut lifecycle_guard).await;
        self.maybe_trim_session(session_id).await;
        result
    }

    /// Handle with streaming: sends content chunks via mpsc channel for progressive display.
    ///
    /// Thin adapter over pipeline::{bootstrap, execute, finalize} using `ChunkSink`.
    /// Uses `use_history: false` (matches old behaviour — streaming callers get no prior context).
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
                use_history: false,
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
        } = boot;
        let mut lifecycle_guard = lifecycle_guard.expect("set by bootstrap");
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
        let outcome = execute::execute(self, boot_for_execute, &mut s, cancel).await?;

        let fin_ctx = finalize::finalize_context_from_engine(
            self,
            session_id,
            outcome.messages_len_at_end,
            // Final assistant parent = end of intermediate chain (last tool
            // result or intermediate assistant with tool_calls) persisted by
            // pipeline::execute. For no-tool turns this equals user_message_id.
            Some(outcome.final_parent_msg_id),
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
