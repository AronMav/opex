//! Pipeline step: subagent execution (run_subagent, run_subagent_with_session).
//! Extracted from engine_subagent.rs as free functions taking &CommandContext.

use anyhow::Result;
use std::sync::Arc;

use hydeclaw_types::{Message, MessageRole};
use super::CommandContext;
use crate::agent::context_builder::ContextBuilderDeps;
use crate::agent::engine::AgentEngine;
use crate::agent::subagent_state;
use crate::agent::thinking::extract_result_text;
use crate::agent::tool_loop::LoopDetector;
use crate::agent::workspace;

/// Sentinel prefix for subagent cancellation errors -- matched in spawned task.
const SUBAGENT_CANCELLED: &str = "subagent cancelled";

/// Run an in-process subagent with isolated LLM context.
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn run_subagent(
    ctx: &CommandContext<'_>,
    executor: &AgentEngine,
    task: &str,
    max_iterations: usize,
    deadline: Option<std::time::Instant>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    handle: Option<Arc<tokio::sync::RwLock<subagent_state::SubagentHandle>>>,
    allowed_tools: Option<Vec<String>>,
) -> Result<String> {
    run_subagent_with_session(ctx, executor, task, max_iterations, deadline, cancel, handle, allowed_tools, None).await
}

/// Like `run_subagent` but with an explicit session_id for tool context enrichment.
/// When `session_id` is Some, it is passed to `execute_tool_calls_partitioned` so tools
/// like `agent` can find the correct SessionAgentPool via enriched `_context`.
#[allow(clippy::too_many_arguments)]
pub async fn run_subagent_with_session(
    ctx: &CommandContext<'_>,
    executor: &AgentEngine,
    task: &str,
    max_iterations: usize,
    deadline: Option<std::time::Instant>,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    handle: Option<Arc<tokio::sync::RwLock<subagent_state::SubagentHandle>>>,
    allowed_tools: Option<Vec<String>>,
    session_id: Option<uuid::Uuid>,
) -> Result<String> {
    let cfg = ctx.cfg;
    let ws_prompt =
        workspace::load_workspace_prompt(&cfg.workspace_dir, &cfg.agent.name).await?;
    let capabilities = workspace::CapabilityFlags {
        has_search: executor.has_tool("search_web").await || executor.has_tool("search_web_fresh").await,
        has_memory: cfg.memory_store.is_available(),
        has_message_actions: ctx.state.channel_router.is_some(),
        has_cron: cfg.scheduler.is_some(),
        has_yaml_tools: true,
        has_browser: super::canvas::browser_renderer_url() != "disabled",
        has_host_exec: cfg.agent.base && ctx.tex.sandbox.is_none(),
        is_base: cfg.agent.base,
    };
    let runtime = workspace::RuntimeContext {
        agent_name: cfg.agent.name.clone(),
        owner_id: cfg.agent.access.as_ref().and_then(|a| a.owner_id.clone()),
        channel: "agent".to_string(),
        model: cfg.provider.current_model(),
        datetime_display: workspace::format_local_datetime(&cfg.default_timezone),
        formatting_prompt: None,
        channels: vec![],
    };
    let system_prompt =
        workspace::build_system_prompt(&ws_prompt, &[], &capabilities, &cfg.agent.language, &runtime);

    let mut messages = vec![
        Message {
            role: MessageRole::System,
            content: format!(
                "{}\n\nYou are a subagent. Complete the task and return the result concisely.",
                system_prompt
            ),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
        Message {
            role: MessageRole::User,
            content: task.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        },
    ];

    let mut available_tools = executor.internal_tool_definitions_for_subagent(allowed_tools.as_deref());
    let yaml_tools: Vec<crate::tools::yaml_tools::YamlToolDef> = {
        let cache = ctx.tex.yaml_tools_cache.read().await;
        if cache.0.elapsed() < std::time::Duration::from_secs(30) && !cache.1.is_empty() {
            cache.1.values().cloned().collect()
        } else {
            drop(cache);
            let loaded = crate::tools::yaml_tools::load_yaml_tools(&cfg.workspace_dir, false).await;
            let map: std::collections::HashMap<String, crate::tools::yaml_tools::YamlToolDef> =
                loaded.iter().cloned().map(|t| (t.name.clone(), t)).collect();
            *ctx.tex.yaml_tools_cache.write().await = (std::time::Instant::now(), std::sync::Arc::new(map));
            loaded
        }
    };
    available_tools.extend(yaml_tools.into_iter().map(|t| t.to_tool_definition()));
    if let Some(mcp) = &ctx.tex.mcp {
        available_tools.extend(mcp.all_tool_definitions().await);
    }
    available_tools = executor.filter_tools_by_policy(available_tools);

    let loop_config = executor.tool_loop_config();
    let effective_max = max_iterations.min(loop_config.effective_max_iterations());
    let mut detector = LoopDetector::new(&loop_config);
    let mut loop_nudge_count: usize = 0;

    for iteration in 0..effective_max {
        // Cancel check
        if let Some(ref c) = cancel
            && c.load(std::sync::atomic::Ordering::Relaxed) {
                tracing::info!(iteration, "subagent cancelled by parent");
                anyhow::bail!("{} at iteration {}", SUBAGENT_CANCELLED, iteration);
            }
        // Deadline check (only if set)
        if let Some(dl) = deadline
            && std::time::Instant::now() > dl {
                tracing::warn!(iteration, "subagent deadline reached, returning partial result");
                let forced = cfg.provider.chat(&messages, &[]).await?;
                return Ok(extract_result_text(&forced.content, &messages));
            }

        let response = if loop_config.compact_on_overflow {
            executor.chat_with_overflow_recovery(&mut messages, &available_tools).await?
        } else {
            cfg.provider.chat(&messages, &available_tools).await?
        };

        if response.tool_calls.is_empty() {
            return Ok(extract_result_text(&response.content, &messages));
        }

        tracing::info!(
            iteration,
            max = effective_max,
            tools = response.tool_calls.len(),
            "subagent executing tool calls"
        );

        messages.push(Message {
            role: MessageRole::Assistant,
            content: response.content.clone(),
            tool_calls: Some(response.tool_calls.clone()),
            tool_call_id: None,
            thinking_blocks: vec![],
        });

        // Use an empty object (not Null) so enrich_tool_args can inject session_id into _context.
        let effective_session_id = session_id.unwrap_or_else(uuid::Uuid::nil);
        let subagent_context = serde_json::json!({});
        let loop_broken = match executor.execute_tool_calls_partitioned(
            &response.tool_calls, &subagent_context, effective_session_id, crate::agent::channel_kind::channel::INTER_AGENT,
            messages.iter().map(|m| m.content.len()).sum(),
            &mut detector, loop_config.detect_loops,
        ).await {
            Ok(results) => {
                for (tc_id, tool_result) in &results {
                    messages.push(Message {
                        role: MessageRole::Tool,
                        content: tool_result.clone(),
                        tool_calls: None,
                        tool_call_id: Some(tc_id.clone()),
                        thinking_blocks: vec![],
                    });
                }
                false
            }
            Err(crate::agent::pipeline::parallel::LoopBreak(reason)) => {
                if loop_nudge_count < loop_config.max_loop_nudges {
                    let nudge_desc = reason.as_deref().unwrap_or("repeating pattern");
                    let nudge_msg = format!(
                        "LOOP DETECTED: You have repeated the same sequence of actions ({desc}). \
                         Change your approach entirely. If the task is too large for a single session, \
                         tell the user and suggest breaking it into smaller steps. Do NOT retry the same approach.",
                        desc = nudge_desc
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
                        nudge_count = loop_nudge_count,
                        reason = ?reason,
                        "subagent loop nudge injected"
                    );
                    false // continue loop
                } else {
                    tracing::error!(
                        nudge_count = loop_nudge_count,
                        "subagent max loop nudges reached, force-stopping"
                    );
                    true // broken
                }
            }
        };

        // Log iteration to handle (if managed)
        if let Some(ref h) = handle {
            let tool_names: Vec<String> = response.tool_calls.iter().map(|tc| tc.name.clone()).collect();
            let preview: String = response.content.chars().take(200).collect();
            let mut hh = h.write().await;
            hh.log.push(subagent_state::SubagentLogEntry {
                iteration,
                timestamp: chrono::Utc::now(),
                tool_calls: tool_names,
                content_preview: preview,
            });
        }

        if loop_broken || iteration == effective_max - 1 {
            let forced = cfg.provider.chat(&messages, &[]).await?;
            return Ok(extract_result_text(&forced.content, &messages));
        }
    }

    anyhow::bail!("subagent exceeded max iterations")
}
