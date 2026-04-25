//! Pipeline step: dispatch — tool policy helpers (migrated from engine_dispatch.rs).
//!
//! Free functions that don't need `&AgentEngine`:
//! - `needs_approval` — pure config check
//! - `filter_tools_by_policy` — per-agent allow/deny filtering
//! - `apply_tool_policy_override` — merge cron-job override on top of base policy
//! - `classify_tool_result` — detect error in tool output string
//! - `clean_tool_params` — strip internal `_context` from arguments before audit

use crate::config::{AgentToolPolicy, ApprovalConfig};
use crate::agent::channel_kind::ToolCategory;
use hydeclaw_types::ToolDefinition;

// ── Approval ─────────────────────────────────────────────────────────────────

/// Check if a tool requires approval before execution (pure config check).
pub fn needs_approval(approval: Option<&ApprovalConfig>, tool_name: &str) -> bool {
    let approval = match approval {
        Some(a) if a.enabled => a,
        _ => return false,
    };

    // Check explicit tool names
    if approval.require_for.iter().any(|t| t == tool_name) {
        return true;
    }

    // Check categories
    if !approval.require_for_categories.is_empty() {
        let category = ToolCategory::classify(tool_name);
        if approval
            .require_for_categories
            .iter()
            .any(|c| c == category.as_str())
        {
            return true;
        }
    }

    false
}

// ── Tool result classification ───────────────────────────────────────────────

/// Detect whether a tool result string indicates an error.
pub fn classify_tool_result(result: &str) -> bool {
    result.contains("\"status\":\"error\"")
        || result.starts_with("Error:")
        || result.starts_with("Tool '") && result.contains("timed out")
}

/// Strip internal `_context` from tool arguments before audit logging.
pub fn clean_tool_params(arguments: &serde_json::Value) -> serde_json::Value {
    let mut p = arguments.clone();
    if let Some(obj) = p.as_object_mut() {
        obj.remove("_context");
    }
    p
}

// ── Tool policy filtering ────────────────────────────────────────────────────

/// Hardcoded core/system tool names that `filter_tools_by_policy` admits
/// unconditionally (after the deny check). This is a **subset** of the
/// authoritative registry [`crate::agent::pipeline::tool_defs::all_system_tool_names`]:
/// it omits `memory` (gated separately by `memory_available`) and the
/// `tool_*` family (gated by the `tool_management` group), since this
/// function handles those via dedicated branches below.
///
/// **Do not use this for "is X a known system tool?" — use
/// `tool_defs::all_system_tool_names()` instead.** This constant is only
/// the unconditional-admit list for `filter_tools_by_policy`.
pub const SYSTEM_TOOL_NAMES: &[&str] = &[
    "workspace_write", "workspace_read", "workspace_list", "workspace_edit",
    "workspace_delete", "workspace_rename",
    "web_fetch", "agent", "message", "cron", "code_exec", "browser_action",
    "git", "session", "skill", "skill_use", "canvas", "rich_card",
    "agents_list", "secret_set", "process",
];

/// Filter tools based on per-agent allow/deny policy.
///
/// `memory_available` indicates whether the memory store is currently configured
/// (controls visibility of the `memory` tool).
pub fn filter_tools_by_policy(
    tools: Vec<ToolDefinition>,
    policy: Option<&AgentToolPolicy>,
    memory_available: bool,
) -> Vec<ToolDefinition> {
    let policy = match policy {
        Some(p) => p,
        None => return tools,
    };

    let before = tools.len();
    let filtered: Vec<ToolDefinition> = tools
        .into_iter()
        .filter(|t| {
            let name = t.name.as_str();

            // Check deny list first (applies to ALL tools including core)
            if policy.deny.iter().any(|d| d == name) {
                return false;
            }

            // Core internal tools always allowed unless denied above
            if SYSTEM_TOOL_NAMES.contains(&name) {
                return true;
            }

            // Memory tool requires memory_store to be available
            if name == "memory" {
                return memory_available;
            }

            // Tool management tools
            if name.starts_with("tool_") {
                return true;
            }
            // allow_all = everything not denied
            if policy.allow_all {
                return true;
            }
            // deny_all_others = only explicitly allowed
            if policy.deny_all_others {
                return policy.allow.iter().any(|a| a == &t.name);
            }
            // Non-empty allow list = only those
            if !policy.allow.is_empty() {
                return policy.allow.iter().any(|a| a == &t.name);
            }
            true
        })
        .collect();

    if filtered.len() != before {
        tracing::info!(before, after = filtered.len(), "tool policy applied");
    }
    filtered
}

/// Merge a cron-job tool policy override on top of the agent's base policy,
/// then re-filter the already-filtered tool list.
///
/// Logic:
///  - deny list is unioned (base deny ∪ override deny)
///  - allow list: if override has non-empty allow, restrict to those tools only
pub fn apply_tool_policy_override(
    tools: Vec<ToolDefinition>,
    base_deny: Option<&[String]>,
    override_policy: &AgentToolPolicy,
) -> Vec<ToolDefinition> {
    tools
        .into_iter()
        .filter(|t| {
            // Union of deny lists
            if override_policy.deny.iter().any(|d| d == &t.name) {
                return false;
            }
            if let Some(bd) = base_deny
                && bd.iter().any(|d| d == &t.name)
            {
                return false;
            }
            // If override has a non-empty allow list, restrict to those tools only
            if !override_policy.allow.is_empty() {
                return override_policy.allow.iter().any(|a| a == &t.name);
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_approval_disabled() {
        assert!(!needs_approval(None, "workspace_write"));
        let cfg = ApprovalConfig {
            enabled: false,
            require_for: vec!["workspace_write".into()],
            ..default_approval()
        };
        assert!(!needs_approval(Some(&cfg), "workspace_write"));
    }

    #[test]
    fn needs_approval_explicit() {
        let cfg = ApprovalConfig {
            enabled: true,
            require_for: vec!["code_exec".into()],
            ..default_approval()
        };
        assert!(needs_approval(Some(&cfg), "code_exec"));
        assert!(!needs_approval(Some(&cfg), "workspace_read"));
    }

    #[test]
    fn classify_tool_result_errors() {
        assert!(classify_tool_result("{\"status\":\"error\",\"msg\":\"boom\"}"));
        assert!(classify_tool_result("Error: something went wrong"));
        assert!(classify_tool_result("Tool 'foo' timed out after 30s"));
        assert!(!classify_tool_result("ok"));
    }

    #[test]
    fn clean_tool_params_removes_context() {
        let args = serde_json::json!({"query": "test", "_context": {"session_id": "abc"}});
        let clean = clean_tool_params(&args);
        assert!(clean.get("_context").is_none());
        assert_eq!(clean.get("query").unwrap().as_str().unwrap(), "test");
    }

    #[test]
    fn filter_tools_deny_blocks() {
        let tools = vec![
            tool("workspace_write"),
            tool("code_exec"),
            tool("my_tool"),
        ];
        let policy = AgentToolPolicy {
            deny: vec!["code_exec".into()],
            ..default_policy()
        };
        let filtered = filter_tools_by_policy(tools, Some(&policy), true);
        let names: Vec<&str> = filtered.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"workspace_write"));
        assert!(!names.contains(&"code_exec"));
    }

    #[test]
    fn apply_override_union_deny() {
        let tools = vec![tool("a"), tool("b"), tool("c")];
        let override_p = AgentToolPolicy {
            deny: vec!["b".into()],
            ..default_policy()
        };
        let base_deny = vec!["c".into()];
        let filtered = apply_tool_policy_override(tools, Some(&base_deny), &override_p);
        let names: Vec<&str> = filtered.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        }
    }

    fn default_approval() -> ApprovalConfig {
        ApprovalConfig {
            enabled: false,
            require_for: vec![],
            require_for_categories: vec![],
            timeout_seconds: 300,
        }
    }

    fn default_policy() -> AgentToolPolicy {
        AgentToolPolicy {
            allow: vec![],
            deny: vec![],
            allow_all: false,
            deny_all_others: false,
            groups: crate::config::ToolGroups::default(),
        }
    }
}
