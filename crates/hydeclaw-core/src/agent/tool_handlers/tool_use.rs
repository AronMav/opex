//! Handler for the `tool_use` meta-tool (search and describe actions).
//!
//! The `call` action is intercepted before this handler by the rewrite step
//! in `pipeline/parallel.rs` and never reaches here. If it does, we return a
//! diagnostic error so the misconfiguration surfaces immediately.

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt::Write;

use crate::agent::dispatcher;
use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

/// Number of search results returned by `tool_use(action="search")`.
const SEARCH_TOP_K: usize = 5;

pub struct ToolUseHandler;

#[async_trait]
impl SystemToolHandler for ToolUseHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                handle_search(deps, query).await
            }
            "describe" => {
                let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                handle_describe(deps, name).await
            }
            "call" => {
                "Error: tool_use(action=\"call\") must be intercepted by the dispatcher rewrite. \
                 If you see this, the rewrite is misconfigured.".to_string()
            }
            _ => format!(
                "Error: unknown action '{action}'. Use: search, describe, call."
            ),
        }
    }
}

async fn promoted_set(deps: &ToolDeps<'_>) -> HashSet<String> {
    match deps.session_tool_state.as_ref() {
        Some(s) => s.promoted.read().await.clone(),
        None => HashSet::new(),
    }
}

fn deny_list(deps: &ToolDeps<'_>) -> Vec<String> {
    // Effective deny = agent.tools.deny ∪ SUBAGENT_DENIED_TOOLS (via delegation).
    // Without this union, the catalogue / describe endpoints would leak the
    // existence of delegation-denied tools to subagents (or to the rewrite
    // step that falls back here in some paths). Mirrors the union applied
    // in `engine/tool_executor.rs::execute_tool_calls_partitioned` and the
    // trigger-hint logic in `context_builder::cfg_deny_list`.
    let mut d = deps.cfg.agent.tools.as_ref()
        .map(|p| p.deny.clone())
        .unwrap_or_default();
    let delegation_denied = crate::agent::pipeline::subagent::compute_denied_tools(
        &deps.cfg.agent.delegation,
    );
    for x in delegation_denied {
        if !d.contains(&x) {
            d.push(x);
        }
    }
    d
}

async fn handle_search(deps: ToolDeps<'_>, query: &str) -> String {
    if query.is_empty() {
        return "Error: search requires a non-empty query string.".to_string();
    }

    let promoted = promoted_set(&deps).await;
    let deny = deny_list(&deps);

    let candidates = dispatcher::build_extension_tool_list(
        deps.agent_base,
        &deny,
        &promoted,
        deps.workspace_dir,
        deps.mcp,
    ).await;

    // System-tool entries from `build_extension_tool_list` carry empty
    // descriptions (the lookup helper has no access to per-agent context).
    // Fill them from the agent's snapshotted `internal_tool_definitions()`.
    let candidates = fill_system_descriptions(candidates, &deps.full_internal_tools);

    if candidates.is_empty() {
        return "No matching tools found. Try a different query, or check that the tool you need is not in the deny-list.".to_string();
    }

    let top_k = crate::agent::pipeline::subagent::select_top_k_tools_semantic_no_force(
        deps.embedder,
        deps.tool_embed_cache,
        deps.memory_available,
        candidates,
        query,
        SEARCH_TOP_K,
    ).await;

    if top_k.is_empty() {
        return "No matching tools found.".to_string();
    }

    let mut out = String::from("Found tools:\n");
    for t in &top_k {
        let _ = writeln!(out, "- {} — {}", t.name, t.description);
    }
    out.push_str("\nUse tool_use(action=\"describe\", name=\"X\") for full schema.");
    out
}

async fn handle_describe(deps: ToolDeps<'_>, name: &str) -> String {
    if !dispatcher::is_valid_tool_name(name) {
        return format!("Invalid tool name: '{name}'.");
    }

    if let Some(state) = deps.session_tool_state.as_ref() {
        let cache = state.describe_cache.read().await;
        if let Some(cached) = cache.get(name) {
            return cached.clone();
        }
    }

    let promoted = promoted_set(&deps).await;
    let deny = deny_list(&deps);

    let tool = dispatcher::find_extension_tool(
        name,
        deps.agent_base,
        &deny,
        &promoted,
        deps.workspace_dir,
        deps.mcp,
    ).await;

    let result = match tool {
        Some(mut def) => {
            if def.description.is_empty()
                && let Some(sys) = deps.full_internal_tools.iter().find(|t| t.name == def.name)
            {
                def = sys.clone();
            }
            format!(
                "Tool: {}\n\nDescription: {}\n\nInput schema:\n```json\n{}\n```\n\n\
                 Call with: tool_use(action=\"call\", name=\"{}\", arguments={{...}})",
                def.name, def.description,
                serde_json::to_string_pretty(&def.input_schema).unwrap_or_default(),
                def.name,
            )
        }
        None => format!(
            "Tool '{name}' not found. Use tool_use(action=\"search\") to discover available tools."
        ),
    };

    if let Some(state) = deps.session_tool_state.as_ref() {
        let mut cache = state.describe_cache.write().await;
        cache.insert(name.to_string(), result.clone());
    }

    result
}

/// Replace empty descriptions on system-tool entries with full descriptions
/// from the agent's `internal_tool_definitions()`.
fn fill_system_descriptions(
    mut tools: Vec<hydeclaw_types::ToolDefinition>,
    full_sys: &[hydeclaw_types::ToolDefinition],
) -> Vec<hydeclaw_types::ToolDefinition> {
    for t in &mut tools {
        if t.description.is_empty()
            && let Some(sys) = full_sys.iter().find(|s| s.name == t.name)
        {
            t.description = sys.description.clone();
        }
    }
    tools
}
