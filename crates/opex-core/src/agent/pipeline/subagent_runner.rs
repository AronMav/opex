//! Pipeline step: subagent execution (run_subagent, run_subagent_with_session).
//! Extracted from engine_subagent.rs as free functions taking &CommandContext.

use anyhow::Result;
use std::sync::Arc;

use opex_types::{Message, MessageRole};
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
    depth: u8,
) -> Result<String> {
    run_subagent_with_session(
        ctx, executor, task, max_iterations, deadline, cancel, handle, allowed_tools, None, depth,
    ).await
}

/// Like `run_subagent` but with an explicit session_id for tool context enrichment.
/// When `session_id` is Some, it is passed to `execute_tool_calls_partitioned` so tools
/// like `agent` can find the correct SessionAgentPool via enriched `_context`.
///
/// `depth` is the subagent recursion depth carried in the inner `CommandContext`.
/// Used by max-depth enforcement (see Tasks 8/9 of the iso-subagent-isolation plan).
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
    depth: u8,
) -> Result<String> {
    // `depth` is the subagent recursion depth this run is operating at.
    // Threaded into `subagent_context` below so any `agent` tool calls
    // emitted from within this subagent see their parent depth via enriched
    // `_context.subagent_depth` and `check_depth_limit` can gate further
    // spawns via `[agent.delegation] max_depth`.
    let cfg = ctx.cfg;
    let ws_prompt =
        workspace::load_workspace_prompt(&cfg.workspace_dir, &cfg.agent.name, cfg.agent.base).await?;
    let capabilities = workspace::CapabilityFlags {
        has_search: executor.has_tool("search_web").await,
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
    let system_prompt = workspace::build_system_prompt(
        &ws_prompt,
        &[],
        &capabilities,
        &cfg.agent.language,
        &runtime,
        None,
    );

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
            db_id: None,
        },
        Message {
            role: MessageRole::User,
            content: task.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        },
    ];

    // `executor` is the subagent's own engine — its `agent.base` flag
    // determines whether the called subagent gets full tools or the
    // dispatcher's compact array. Base subagents always get full tools
    // regardless of the parent's dispatcher settings.
    let subagent_is_base = executor.cfg().agent.base;
    // Mirror the parent-side decision: when the dispatcher is engaged for
    // this subagent, the array stays at static-core only — yaml/mcp are
    // discovered via `tool_use` and never preloaded. Without this gate, the
    // `extends` below re-injected yaml+mcp unconditionally and the
    // subagent's array stayed full-size, defeating the dispatcher's whole
    // point on the subagent path.
    let dispatch_for_subagent = crate::agent::engine::tool_executor::dispatch_for_subagent_decision(
        subagent_is_base,
        executor.cfg().agent.tool_dispatcher.enabled,
        executor.cfg().agent.delegation.subagent_dispatcher_enabled,
    );
    let mut available_tools = executor
        .internal_tool_definitions_for_subagent(allowed_tools.as_deref(), subagent_is_base);
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
    // Subagent-specific deny list: SUBAGENT_DENIED_TOOLS plus the agent's
    // own `blocked_tools_extra`. `runtime_subagent_denylist` hard-anchors
    // SUBAGENT_DENIED_TOOLS — the subagent can only ADD restrictions via
    // `blocked_tools_extra`, never remove them. Used for two things:
    //   1. visibility-filter YAML/MCP tools so the LLM doesn't see them in
    //      the non-dispatcher path,
    //   2. runtime-gate at the dispatcher rewrite step (`extra_deny`).
    //
    // Base-agent carve-out: `code_exec` lives in SUBAGENT_DENIED_TOOLS to
    // keep ordinary subagents away from arbitrary code execution. Base
    // (system) agents legitimately need it (host-level operator role
    // documented in scaffold/base/SOUL.md), so for `agent.base = true`
    // subagents we strip `code_exec` from the runtime deny list. This
    // restores the legitimate base→base delegation path that group V was
    // about to lose.
    let mut denied_for_subagent = crate::agent::pipeline::subagent::runtime_subagent_denylist(
        &executor.cfg().agent.delegation,
    );
    if subagent_is_base {
        denied_for_subagent.retain(|t| t != "code_exec");
    }
    if !dispatch_for_subagent {
        let denied_set: std::collections::HashSet<&str> =
            denied_for_subagent.iter().map(String::as_str).collect();
        // Capability tools intentionally NOT injected for subagents — all are in SUBAGENT_DENIED_TOOLS.
        available_tools.extend(
            yaml_tools
                .into_iter()
                .filter(|t| !denied_set.contains(t.name.as_str()))
                .map(|t| t.to_tool_definition()),
        );
        if let Some(mcp) = &ctx.tex.mcp {
            let mcp_defs = mcp.all_tool_definitions().await;
            available_tools.extend(
                mcp_defs.into_iter().filter(|t| !denied_set.contains(t.name.as_str())),
            );
        }
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
                // Use streaming client for the forced-finish call too — no total-body timeout.
                let (tx, _rx) = tokio::sync::mpsc::channel::<String>(1024);
                let forced = cfg.provider.chat_stream(&messages, &[], tx, crate::agent::providers::CallOptions::default()).await?;
                return Ok(extract_result_text(&forced.content, &messages));
            }

        // Use chat_stream() instead of chat() so the streaming HTTP client is used.
        // The streaming client has no per-request total timeout, whereas the non-streaming
        // client has request_secs=120s.  Thinking-mode models (DeepSeek V4 Pro) generate
        // very large reasoning_content, and parallel subagents can easily exceed 120s
        // waiting for the full non-streaming body — leading to "error decoding response body".
        // Chunks are discarded (subagents don't stream to the UI); only the final LlmResponse is used.
        // Bounded channel; chunks are discarded (subagents don't stream to the UI).
        let (chunk_tx, _chunk_rx) = tokio::sync::mpsc::channel::<String>(1024);
        let response = if loop_config.compact_on_overflow {
            crate::agent::pipeline::llm_call::chat_stream_with_overflow_recovery(
                cfg.provider.as_ref(),
                &mut messages,
                &available_tools,
                chunk_tx,
                executor,
                crate::agent::providers::CallOptions::default(),
            ).await?
        } else {
            cfg.provider.chat_stream(&messages, &available_tools, chunk_tx, crate::agent::providers::CallOptions::default()).await?
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
            thinking_blocks: response.thinking_blocks.clone(),
            db_id: None,
        });

        // Use an empty object (not Null) so enrich_tool_args can inject session_id into _context.
        // Inject `subagent_depth` so nested `agent` tool calls see the parent depth
        // and `check_depth_limit` enforces `[agent.delegation] max_depth`.
        // Read from `ctx.subagent_depth` (single source of truth maintained by
        // the engine wrapper); `depth` parameter is asserted to match in debug builds.
        debug_assert_eq!(
            ctx.subagent_depth, depth,
            "ctx.subagent_depth ({}) must match runner depth param ({})",
            ctx.subagent_depth, depth
        );
        let effective_session_id = session_id.unwrap_or_else(uuid::Uuid::nil);
        let subagent_context = serde_json::json!({ "subagent_depth": ctx.subagent_depth });
        // Subagent runner does not persist tool messages to the DB
        // (subagent context is in-memory only) — pass `None`.
        // T3: subagent context is in-memory only, so parallel_batch_id has no
        // observable effect; allocate when ≥2 tool calls for consistency.
        let parallel_batch_id: Option<opex_types::ids::ParallelBatchId> =
            if response.tool_calls.len() >= 2 {
                Some(opex_types::ids::ParallelBatchId::new())
            } else {
                None
            };
        let outcome = executor.execute_tool_calls_partitioned(
            &response.tool_calls, &subagent_context, effective_session_id, crate::agent::channel_kind::channel::INTER_AGENT,
            messages.iter().map(|m| m.content.len()).sum(),
            &mut detector, loop_config.detect_loops, None,
            parallel_batch_id,
            &denied_for_subagent,
        ).await;
        for batch in &outcome.results {
            messages.push(Message {
                role: MessageRole::Tool,
                content: batch.result.clone(),
                tool_calls: None,
                tool_call_id: Some(opex_types::ids::ToolCallId::new(
                    batch.tool_call_id.clone(),
                )),
                thinking_blocks: vec![],
                db_id: None,
            });
        }
        let loop_broken = if let Some(reason) = outcome.loop_break {
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
                    db_id: None,
                });
                loop_nudge_count += 1;
                // Do NOT reset the detector — the regression test
                // `loop_detector_persists_history_across_nudge` requires history
                // to persist across nudges, otherwise an agent can perform
                // max_loop_nudges × break_threshold identical iterations.
                tracing::warn!(
                    nudge_count = loop_nudge_count,
                    reason = ?reason,
                    "subagent loop nudge injected"
                );
                false
            } else {
                tracing::error!(
                    nudge_count = loop_nudge_count,
                    "subagent max loop nudges reached, force-stopping"
                );
                true
            }
        } else {
            false
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
            let forced = cfg.provider.chat(&messages, &[], crate::agent::providers::CallOptions::default()).await?;
            return Ok(extract_result_text(&forced.content, &messages));
        }
    }

    anyhow::bail!("subagent exceeded max iterations")
}
