//! Pipeline step: OpenAI-compatible endpoint handler.
//! Extracted from engine_sandbox.rs as a free function taking &CommandContext.

use anyhow::Result;

use hydeclaw_types::{Message, MessageRole};
use super::CommandContext;
use crate::agent::context_builder::ContextBuilderDeps;
use crate::agent::engine::AgentEngine;
use crate::agent::thinking::extract_result_text;
use crate::agent::tool_loop::LoopDetector;
use crate::agent::workspace;

/// Handle an OpenAI-compatible chat completion request.
/// Supports tool execution loop and optional streaming via chunk_tx.
pub async fn handle_openai(
    ctx: &CommandContext<'_>,
    executor: &AgentEngine,
    openai_messages: &[crate::gateway::OpenAiMessage],
    chunk_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
) -> Result<hydeclaw_types::LlmResponse> {
    let cfg = ctx.cfg;

    // 1. Build tool list (same as build_context but without session)
    let yaml_tools = crate::tools::yaml_tools::load_yaml_tools(&cfg.workspace_dir, false).await;
    let mut raw_tools = executor.internal_tool_definitions();
    raw_tools.extend(yaml_tools.into_iter().map(|t| t.to_tool_definition()));
    if let Some(mcp) = &ctx.tex.mcp {
        raw_tools.extend(mcp.all_tool_definitions().await);
    }
    let available_tools = executor.filter_tools_by_policy(raw_tools);

    // 2. Determine the last user query for memory context
    let _last_user_text = openai_messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");

    // 3. Convert OpenAI messages -> internal Message format.
    //    If the caller didn't provide a system message, prepend the agent's system prompt.
    let has_system = openai_messages.iter().any(|m| m.role == "system");
    let mut messages: Vec<Message> = Vec::with_capacity(openai_messages.len() + 1);

    if !has_system {
        let ws_prompt =
            workspace::load_workspace_prompt(&cfg.workspace_dir, &cfg.agent.name)
                .await
                .unwrap_or_default();

        let mcp_schemas: Vec<String> = if let Some(mcp) = &ctx.tex.mcp {
            let defs = mcp.all_tool_definitions().await;
            defs.iter()
                .map(|t| {
                    format!(
                        "- **{}**: {}\n  Parameters: {}",
                        t.name,
                        t.description,
                        serde_json::to_string(&t.input_schema).unwrap_or_default()
                    )
                })
                .collect()
        } else {
            vec![]
        };

        let capabilities = workspace::CapabilityFlags {
            has_search: executor.has_tool("search_web").await || executor.has_tool("search_web_fresh").await,
            has_memory: cfg.memory_store.is_available(),
            has_message_actions: false, // no channel adapter in API mode
            has_cron: cfg.scheduler.is_some(),
            has_yaml_tools: true,
            has_browser: super::canvas::browser_renderer_url() != "disabled",
            has_host_exec: cfg.agent.base && ctx.tex.sandbox.is_none(),
            is_base: cfg.agent.base,
        };

        let runtime = workspace::RuntimeContext {
            agent_name: cfg.agent.name.clone(),
            owner_id: cfg.agent.access.as_ref().and_then(|a| a.owner_id.clone()),
            channel: "api".to_string(),
            model: cfg.provider.current_model(),
            datetime_display: workspace::format_local_datetime(&cfg.default_timezone),
            formatting_prompt: None,
            channels: vec![],
        };
        let system_prompt = workspace::build_system_prompt(
            &ws_prompt,
            &mcp_schemas,
            &capabilities,
            &cfg.agent.language,
            &runtime,
        );

        // Skill auto-injection removed -- skills are loaded on-demand via skill_use tool.

        messages.push(Message {
            role: MessageRole::System,
            content: system_prompt,
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        });
    }

    for m in openai_messages {
        messages.push(Message {
            role: match m.role.as_str() {
                "system" => MessageRole::System,
                "assistant" => MessageRole::Assistant,
                "tool" => MessageRole::Tool,
                _ => MessageRole::User,
            },
            content: m.content.clone().unwrap_or_default(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
        });
    }

    // 4. Tool execution loop (no DB saves)
    let mut final_response = String::new();
    let mut last_usage: Option<hydeclaw_types::TokenUsage> = None;
    let loop_config = executor.tool_loop_config();
    let mut detector = LoopDetector::new(&loop_config);
    let mut tools_used_acc: Vec<String> = Vec::new();
    let mut final_iteration: u32 = 0;

    for iteration in 0..loop_config.effective_max_iterations() {
        let response = if loop_config.compact_on_overflow {
            executor.chat_with_overflow_recovery(&mut messages, &available_tools).await?
        } else {
            cfg.provider.chat(&messages, &available_tools).await?
        };
        last_usage = response.usage.clone();

        if response.tool_calls.is_empty() {
            final_response = response.content.clone();
            break;
        }

        // Accumulate tool names for API response
        for tc in &response.tool_calls {
            if !tools_used_acc.contains(&tc.name) {
                tools_used_acc.push(tc.name.clone());
            }
        }
        final_iteration = iteration as u32 + 1;

        tracing::info!(
            iteration,
            max = loop_config.effective_max_iterations(),
            tools = response.tool_calls.len(),
            "openai api: executing tool calls"
        );

        messages.push(Message {
            role: MessageRole::Assistant,
            content: response.content.clone(),
            tool_calls: Some(response.tool_calls.clone()),
            tool_call_id: None,
            thinking_blocks: vec![],
        });

        let loop_broken = match executor.execute_tool_calls_partitioned(
            &response.tool_calls, &serde_json::Value::Null, uuid::Uuid::nil(), crate::agent::channel_kind::channel::INTER_AGENT,
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
            Err(_) => true,
        };

        if loop_broken || iteration == loop_config.effective_max_iterations() - 1 {
            let forced = cfg.provider.chat(&messages, &[]).await?;
            last_usage = forced.usage.clone();
            final_response = forced.content.clone();
            break;
        }
    }

    let final_response = extract_result_text(&final_response, &messages);

    // Send to chunk consumer if streaming requested (MiniMax sends full response at once)
    if let Some(ref tx) = chunk_tx
        && !final_response.is_empty() {
            tx.send(final_response.clone()).ok();
        }

    Ok(hydeclaw_types::LlmResponse {
        content: final_response,
        tool_calls: vec![],
        usage: last_usage,
        finish_reason: None,
        model: None,
        provider: None,
        fallback_notice: None,
        tools_used: tools_used_acc,
        iterations: final_iteration,
        thinking_blocks: vec![],
    })
}
