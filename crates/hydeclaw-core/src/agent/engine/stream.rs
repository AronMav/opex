//! REF-01 Task 2: handle() / handle_isolated() entry points + `ProcessingPhase`
//! wire enum + `ProcessingGuard` RAII tracker + `StreamEvent` re-export.
//!
//! Extracted from `engine/mod.rs` (was lines 42-61, 135-182, 450-786) as part
//! of plan 66-02. Public API is preserved byte-identically via `pub use` in
//! `engine/mod.rs`.

use anyhow::Result;
use hydeclaw_types::{IncomingMessage, Message, MessageRole};
use std::sync::Arc;

use super::AgentEngine;
use crate::agent::error_classify;
use crate::agent::session_manager::SessionManager;
use crate::agent::thinking::{looks_incomplete, strip_thinking};
use crate::agent::tool_loop::LoopDetector;

/// Phase 62 RES-01: `StreamEvent` extracted to a leaf module
/// (`agent/stream_event.rs`) so the lib facade can expose it to integration
/// tests without cascading the whole `engine.rs` dependency tree. Re-exported
/// here so every existing `crate::agent::engine::StreamEvent` path resolves.
pub use crate::agent::stream_event::StreamEvent;

pub use crate::agent::pipeline::parallel::LoopBreak;

/// Nudge message injected when auto-continue detects incomplete LLM response.
const AUTO_CONTINUE_NUDGE: &str = "[system] You described remaining steps but didn't execute them. Continue and complete the task using tools.";

/// Status phases emitted during message processing.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ProcessingPhase {
    Thinking,
    CallingTool(String),
    Composing,
}

impl ProcessingPhase {
    /// Convert to wire format: (`phase_name`, `optional_tool_name`).
    pub fn to_wire(&self) -> (String, Option<String>) {
        match self {
            ProcessingPhase::Thinking => ("thinking".to_string(), None),
            ProcessingPhase::CallingTool(name) => ("calling_tool".to_string(), Some(name.clone())),
            ProcessingPhase::Composing => ("composing".to_string(), None),
        }
    }
}

/// RAII guard: inserts into processing tracker on creation, removes + broadcasts "end" on drop.
/// Uses `session_id` as tracker key (not `agent_name`) to support concurrent sessions per agent.
pub(crate) struct ProcessingGuard {
    tx: Option<tokio::sync::broadcast::Sender<String>>,
    processing_tracker: Option<crate::gateway::ProcessingTracker>,
    agent_name: String,
    /// Tracker key — `session_id` for unique identification across concurrent sessions.
    tracker_key: String,
    session_id: Option<String>,
}

impl ProcessingGuard {
    pub(crate) fn new(
        tx: Option<tokio::sync::broadcast::Sender<String>>,
        tracker: Option<crate::gateway::ProcessingTracker>,
        agent_name: String,
        start_event: &serde_json::Value,
    ) -> Self {
        let session_id = start_event.get("session_id").and_then(|v| v.as_str()).map(std::string::ToString::to_string);
        // Use session_id as key (supports multiple concurrent sessions for same agent).
        // Fallback to agent_name if session_id is missing (shouldn't happen).
        let tracker_key = session_id.clone().unwrap_or_else(|| agent_name.clone());
        if let Some(ref t) = tracker
            && let Ok(mut map) = t.write() {
                map.insert(tracker_key.clone(), start_event.clone());
                tracing::debug!(agent = %agent_name, key = %tracker_key, "processing_tracker: inserted");
            }
        Self { tx, processing_tracker: tracker, agent_name, tracker_key, session_id }
    }
}

impl Drop for ProcessingGuard {
    fn drop(&mut self) {
        if let Some(ref tracker) = self.processing_tracker
            && let Ok(mut map) = tracker.write() {
                map.remove(&self.tracker_key);
            }
        if let Some(ref tx) = self.tx {
            let mut event = serde_json::json!({
                "type": "agent_processing",
                "agent": self.agent_name,
                "status": "end",
            });
            if let Some(ref sid) = self.session_id {
                event["session_id"] = serde_json::Value::String(sid.clone());
            }
            tx.send(event.to_string()).ok();
        }
    }
}

impl AgentEngine {
    /// Handle an incoming message: build context, call LLM, execute tools, return response.
    pub async fn handle(&self, msg: &IncomingMessage) -> Result<String> {
        self.handle_with_status(msg, None, None).await
    }

    /// Handle a message in a fully isolated session (no history from previous runs).
    /// Used by cron dynamic jobs to prevent context accumulation across invocations.
    pub async fn handle_isolated(&self, msg: &IncomingMessage) -> Result<String> {
        // Hook: BeforeMessage
        if let crate::agent::hooks::HookAction::Block(reason) = self.hooks().fire(&crate::agent::hooks::HookEvent::BeforeMessage) {
            anyhow::bail!("blocked by hook: {reason}");
        }

        let sm = SessionManager::new(self.cfg().db.clone());
        let session_id = sm.create_isolated(&self.cfg().agent.name, &msg.user_id, &msg.channel).await?;

        let ctx = self.build_context(msg, true, Some(session_id), false).await?;
        let mut messages = ctx.messages;
        let mut available_tools = ctx.tools;
        // session_id already bound above (create_isolated result)

        // Apply cron job tool policy override if present
        if let Some(ref policy_json) = msg.tool_policy_override
            && let Ok(override_policy) = serde_json::from_value::<crate::config::AgentToolPolicy>(policy_json.clone()) {
                let before = available_tools.len();
                available_tools = self.apply_tool_policy_override(available_tools, &override_policy);
                if available_tools.len() != before {
                    tracing::info!(
                        agent = %self.cfg().agent.name,
                        before,
                        after = available_tools.len(),
                        "cron tool policy override applied"
                    );
                }
            }

        let user_text = msg.text.clone().unwrap_or_default();
        let enriched_text = {
            let toolgate_url = self.cfg().app_config.toolgate_url.clone()
                .unwrap_or_else(|| "http://localhost:9011".to_string());
            crate::agent::pipeline::subagent::enrich_message_text(
                self.http_client(),
                &self.cfg().app_config.gateway.listen,
                &toolgate_url,
                &self.cfg().agent.language,
                &user_text,
                &msg.attachments,
            ).await
        };

        messages.push(Message {
            role: MessageRole::User,
            content: enriched_text,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        });

        // For inter-agent messages (user_id starts with "agent:"), save the sender agent_id
        let sender_agent_id = if msg.user_id.starts_with("agent:") { Some(msg.user_id.trim_start_matches("agent:")) } else { None };
        sm.save_message_ex(session_id, "user", &user_text, None, None, sender_agent_id, None, None).await?;

        // Context compaction if needed (model-aware token budget)
        self.compact_messages(&mut messages, None).await;

        // LLM loop (with tool calls)
        let mut final_response = String::new();
        let loop_config = self.tool_loop_config();
        let mut detector = LoopDetector::new(&loop_config);
        let mut loop_nudge_count: usize = 0;
        let mut did_reset_session = false;
        let mut empty_retry_count: u8 = 0;
        let mut auto_continue_count: u8 = 0;
        let mut context_chars: usize = messages.iter().map(|m| m.content.chars().count()).sum();
        let mut consecutive_failures: usize = 0;
        let mut using_fallback = false;
        let mut fallback_provider: Option<Arc<dyn crate::agent::providers::LlmProvider>> = None;

        for iteration in 0..loop_config.effective_max_iterations() {
            self.compact_tool_results(&mut messages, &mut context_chars);
            let llm_result = if let Some(ref fb) = fallback_provider {
                self.chat_with_transient_retry_using(fb, &mut messages, &available_tools).await
            } else {
                self.chat_with_transient_retry(&mut messages, &available_tools).await
            };
            let response = match llm_result {
                Ok(r) => {
                    consecutive_failures = 0;
                    r
                }
                Err(e) => {
                    if error_classify::classify(&e) == error_classify::LlmErrorClass::SessionCorruption && !did_reset_session {
                        did_reset_session = true;
                        tracing::warn!(error = %e, "session corrupted, resetting context");
                        messages.retain(|m| m.role == MessageRole::System);
                        messages.push(Message { role: MessageRole::User, content: user_text.clone(), tool_calls: None, tool_call_id: None, thinking_blocks: vec![] });
                        context_chars = messages.iter().map(|m| m.content.chars().count()).sum();
                        continue;
                    }
                    consecutive_failures += 1;
                    if !using_fallback && consecutive_failures >= loop_config.max_consecutive_failures {
                        if fallback_provider.is_none() {
                            fallback_provider = self.create_fallback_provider().await;
                        }
                        if fallback_provider.is_some() {
                            using_fallback = true;
                            consecutive_failures = 0;
                            tracing::warn!(
                                agent = %self.cfg().agent.name,
                                iteration,
                                "switching to fallback provider after consecutive failures"
                            );
                            continue;
                        }
                    }
                    tracing::error!(error = %e, iteration, "isolated LLM call failed, returning fallback");
                    self.hooks().fire(&crate::agent::hooks::HookEvent::OnError);
                    final_response = error_classify::format_user_error(&e);
                    break;
                }
            };
            self.record_usage(&response, Some(session_id));

            if response.tool_calls.is_empty() {
                final_response = strip_thinking(&response.content);

                // Auto-continue: if LLM described remaining work, nudge it to execute
                if auto_continue_count < loop_config.max_auto_continues && !final_response.is_empty() && looks_incomplete(&final_response) {
                    auto_continue_count += 1;
                    tracing::info!(iteration, count = auto_continue_count, max = loop_config.max_auto_continues, "auto-continue: response looks incomplete, nudging LLM");
                    {
                        let db = self.cfg().db.clone();
                        let agent_name = self.cfg().agent.name.clone();
                        let cnt = auto_continue_count;
                        let max = loop_config.max_auto_continues;
                        if let Some(ref ui_tx) = self.state().ui_event_tx {
                            let tx = ui_tx.clone();
                            tokio::spawn(async move {
                                crate::gateway::notify(
                                    &db, &tx, "auto_continue",
                                    &format!("Auto-continue: {agent_name}"),
                                    &format!("Agent continued unfinished task (attempt {cnt}/{max})"),
                                    serde_json::json!({"agent": agent_name}),
                                ).await.ok();
                            });
                        }
                    }
                    messages.push(Message {
                        role: MessageRole::User,
                        content: AUTO_CONTINUE_NUDGE.to_string(),
                        tool_calls: None,
                        tool_call_id: None,
                        thinking_blocks: vec![],
                    });
                    context_chars += AUTO_CONTINUE_NUDGE.len(); // all ASCII
                    continue;
                }

                if final_response.is_empty() && empty_retry_count < 1 {
                    empty_retry_count += 1;
                    tracing::warn!(iteration, "LLM returned empty response, retrying once");
                    continue;
                }
                if final_response.is_empty() {
                    tracing::warn!(iteration, "LLM returned empty response after retry");
                }
                break;
            }

            tracing::info!(
                iteration,
                max = loop_config.effective_max_iterations(),
                tools = response.tool_calls.len(),
                "isolated job: executing tool calls"
            );

            let cleaned_content = strip_thinking(&response.content);

            messages.push(Message {
                role: MessageRole::Assistant,
                content: cleaned_content.clone(),
                tool_calls: Some(response.tool_calls.clone()),
                tool_call_id: None,
                thinking_blocks: response.thinking_blocks.clone(),
            });
            context_chars += cleaned_content.chars().count();

            // Persist intermediate assistant (with tool_calls) via detached spawn
            // — mirrors `pipeline::execute` so the row survives parent-task
            // cancellation between here and the spawned tool-result inserts
            // below. A synchronous `save_message(...).await` here would leave a
            // window: cancel during the await drops the assistant row, then
            // tool messages persisted by `execute_tool_calls_partitioned`
            // reference a parent_id that doesn't exist → chain broken on
            // reload. Idempotent: pre-generated UUID + `ON CONFLICT (id) DO
            // NOTHING` in `save_message_ex_with_id`.
            let tc_json = serde_json::to_value(&response.tool_calls).ok();
            let tb_json = if response.thinking_blocks.is_empty() {
                None
            } else {
                serde_json::to_value(&response.thinking_blocks).ok()
            };
            let assistant_msg_id = uuid::Uuid::new_v4();
            let agent_name_for_persist = self.cfg().agent.name.clone();
            crate::agent::pipeline::parallel::spawn_persist_assistant_message(
                &self.cfg().db,
                assistant_msg_id,
                session_id,
                &agent_name_for_persist,
                &cleaned_content,
                tc_json.as_ref(),
                tb_json.as_ref(),
                None,
            );

            // Legacy stream path — detached persistence in `execute_tool_calls_partitioned`
            // also covers cancellation gaps for this code path.
            let persist_ctx = crate::agent::pipeline::parallel::ToolPersistCtx {
                agent_name: agent_name_for_persist.as_str(),
                initial_parent: Some(assistant_msg_id),
            };
            let loop_broken = match self.execute_tool_calls_partitioned(
                &response.tool_calls, &msg.context, session_id, &msg.channel,
                messages.iter().map(|m| m.content.len()).sum(),
                &mut detector, loop_config.detect_loops,
                Some(&persist_ctx),
            ).await {
                Ok(results) => {
                    for batch in &results {
                        let tc_id = &batch.tool_call_id;
                        let tool_result = &batch.result;
                        messages.push(Message {
                            role: MessageRole::Tool,
                            content: tool_result.clone(),
                            tool_calls: None,
                            tool_call_id: Some(tc_id.clone()),
                            thinking_blocks: vec![],
                        });
                        context_chars += tool_result.chars().count();
                        // tool message already persisted (detached) inside
                        // execute_tool_calls_partitioned.
                    }
                    false
                }
                Err(LoopBreak(reason)) => {
                    if loop_nudge_count < loop_config.max_loop_nudges {
                        let nudge_desc = reason.as_deref().unwrap_or("repeating pattern");
                        let nudge_msg = format!(
                            "LOOP DETECTED: You have repeated the same sequence of actions ({nudge_desc}). \
                             Change your approach entirely. If the task is too large for a single session, \
                             tell the user and suggest breaking it into smaller steps. Do NOT retry the same approach."
                        );
                        messages.push(Message {
                            role: MessageRole::System,
                            content: nudge_msg,
                            tool_calls: None,
                            tool_call_id: None,
                            thinking_blocks: vec![],
                        });
                        loop_nudge_count += 1;
                        detector.reset();
                        tracing::warn!(
                            agent = %self.cfg().agent.name,
                            nudge_count = loop_nudge_count,
                            reason = ?reason,
                            "loop nudge injected, giving model another chance"
                        );
                        false // continue loop
                    } else {
                        tracing::error!(
                            agent = %self.cfg().agent.name,
                            nudge_count = loop_nudge_count,
                            "max loop nudges reached, force-stopping agent"
                        );
                        true // broken
                    }
                }
            };

            if loop_broken || iteration == loop_config.effective_max_iterations() - 1 {
                // Notify if hitting iteration limit (not loop break)
                if !loop_broken && iteration == loop_config.effective_max_iterations() - 1 {
                    tracing::warn!(
                        agent = %self.cfg().agent.name,
                        max_iterations = loop_config.effective_max_iterations(),
                        "agent reached iteration limit"
                    );
                    if let Some(ref ui_tx) = self.state().ui_event_tx {
                        let db = self.cfg().db.clone();
                        let tx = ui_tx.clone();
                        let agent_name = self.cfg().agent.name.clone();
                        let max_iter = loop_config.effective_max_iterations();
                        tokio::spawn(async move {
                            crate::gateway::notify(
                                &db, &tx, "iteration_limit",
                                &format!("Iteration limit: {agent_name}"),
                                &format!("Agent {agent_name} reached its iteration limit ({max_iter} iterations). The task may be incomplete."),
                                serde_json::json!({"agent": agent_name, "max_iterations": max_iter}),
                            ).await.ok();
                        });
                    }
                }
                // Notify if loop was broken after max nudges
                if loop_broken && loop_nudge_count >= loop_config.max_loop_nudges
                    && let Some(ref ui_tx) = self.state().ui_event_tx {
                        let db = self.cfg().db.clone();
                        let tx = ui_tx.clone();
                        let agent_name = self.cfg().agent.name.clone();
                        let sid = session_id;
                        tokio::spawn(async move {
                            crate::gateway::notify(
                                &db, &tx, "agent_loop_detected",
                                &format!("Agent stuck in loop: {agent_name}"),
                                &format!("Agent {agent_name} was stopped after detecting a repeating pattern. Session: {sid}"),
                                serde_json::json!({"agent": agent_name, "session_id": sid.to_string()}),
                            ).await.ok();
                        });
                    }
                match self.cfg().provider.chat(&messages, &[], crate::agent::providers::CallOptions::default()).await {
                    Ok(forced) => {
                        final_response = strip_thinking(&forced.content);
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "isolated forced final LLM call failed");
                        final_response = error_classify::format_user_error(&e);
                    }
                }
                break;
            }
        }

        sm.save_message_ex(session_id, "assistant", &final_response, None, None, Some(&self.cfg().agent.name), None, None)
            .await?;

        // Post-session knowledge extraction (background, non-blocking)
        if messages.len() >= 5 {
            let db = self.cfg().db.clone();
            let provider = self.cfg().provider.clone();
            let memory = self.cfg().memory_store.clone();
            let agent_name = self.cfg().agent.name.clone();
            tokio::spawn(async move {
                crate::agent::knowledge_extractor::extract_and_save(
                    db, session_id, agent_name, provider, memory,
                ).await;
            });
        }

        // Hook: AfterResponse
        self.hooks().fire(&crate::agent::hooks::HookEvent::AfterResponse);

        Ok(final_response)
    }
}
