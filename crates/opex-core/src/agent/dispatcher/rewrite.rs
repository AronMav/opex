//! Rewrite step that translates `tool_use(action="call", name=X, arguments=Y)`
//! into a synthetic `ToolCall { name: X, arguments: Y }`, with runtime
//! deny-gate as defense-in-depth.

use crate::config::AgentToolPolicy;
use opex_types::ToolCall;
use serde_json::json;

/// Outcome per tool call after rewrite.
// allow(dead_code): consumed by pipeline/parallel.rs.
#[allow(dead_code)]
pub enum RewriteResult {
    /// Either an unmodified original call or a successfully rewritten one.
    Direct(ToolCall),
    /// Rewrite was rejected. A synthetic tool_result block is emitted with
    /// `reason` as content; the call never reaches dispatch.
    Denied { id: String, reason: String },
}

/// Rewrite a batch of tool calls, performing the runtime deny-gate.
///
/// `known_tools` is a synchronous lookup set provided by the caller — it
/// must contain every tool name reachable on this agent (system + visible
/// YAML + visible MCP). The pipeline pre-builds it before this call.
// allow(dead_code): consumed by pipeline/parallel.rs.
#[allow(dead_code)]
pub fn rewrite_tool_use_calls(
    calls: &[ToolCall],
    policy: Option<&AgentToolPolicy>,
    known_tools: &std::collections::HashSet<String>,
    extra_deny: &[String],
) -> Vec<RewriteResult> {
    calls.iter().map(|tc| rewrite_one(tc, policy, known_tools, extra_deny)).collect()
}

/// Runtime deny-gate for a resolved tool name. Returns the denial reason when
/// `name` is blocked by the agent's `policy.deny` or the subagent `extra_deny`
/// list, else `None`. Applied to the effective dispatched tool on BOTH the
/// direct-call path and the `tool_use(action="call")` inner-name path — the
/// gate must never depend on which call shape the model chose (F006).
fn deny_reason(
    name: &str,
    policy: Option<&AgentToolPolicy>,
    extra_deny: &[String],
) -> Option<String> {
    if let Some(p) = policy
        && p.deny.iter().any(|d| d == name)
    {
        return Some(format!("tool '{name}' is denied by agent policy"));
    }
    // Audit 2026-05-08: subagent isolation gate — `extra_deny` carries the
    // result of `runtime_subagent_denylist(&subagent.delegation)` when this
    // engine is running as a subagent. Without this gate, a subagent could
    // execute a tool from SUBAGENT_DENIED_TOOLS even though the
    // visibility-filtered tool list correctly hid it.
    if extra_deny.iter().any(|d| d == name) {
        return Some(format!("tool '{name}' is denied for subagents"));
    }
    None
}

// allow(dead_code): consumed by pipeline/parallel.rs.
#[allow(dead_code)]
fn rewrite_one(
    tc: &ToolCall,
    policy: Option<&AgentToolPolicy>,
    known_tools: &std::collections::HashSet<String>,
    extra_deny: &[String],
) -> RewriteResult {
    if tc.name != "tool_use" {
        // F006: a DIRECT tool call bypasses the dispatcher rewrite, so the
        // deny gate MUST run here too — otherwise a subagent (or a
        // policy.deny agent) can reach a denied tool simply by emitting the
        // call directly instead of via `tool_use(action="call")`. The
        // visibility filter is not a runtime enforcement boundary.
        if let Some(reason) = deny_reason(&tc.name, policy, extra_deny) {
            return RewriteResult::Denied {
                id: tc.id.as_str().to_string(),
                reason,
            };
        }
        return RewriteResult::Direct(tc.clone());
    }

    let args = &tc.arguments;
    if args.get("action").and_then(|v| v.as_str()) != Some("call") {
        return RewriteResult::Direct(tc.clone());
    }

    let inner_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let inner_args = args.get("arguments").cloned().unwrap_or(json!({}));

    if !crate::agent::dispatcher::is_valid_tool_name(inner_name) {
        return RewriteResult::Denied {
            id: tc.id.as_str().to_string(),
            reason: format!("invalid tool name '{inner_name}'"),
        };
    }
    if inner_name == "tool_use" {
        return RewriteResult::Denied {
            id: tc.id.as_str().to_string(),
            reason: "tool_use cannot dispatch to itself".to_string(),
        };
    }
    if !known_tools.contains(inner_name) {
        return RewriteResult::Denied {
            id: tc.id.as_str().to_string(),
            reason: format!("tool '{inner_name}' not found"),
        };
    }

    if let Some(reason) = deny_reason(inner_name, policy, extra_deny) {
        return RewriteResult::Denied {
            id: tc.id.as_str().to_string(),
            reason,
        };
    }

    RewriteResult::Direct(ToolCall {
        id: tc.id.clone(),
        name: inner_name.to_string(),
        arguments: inner_args,
        thought_signature: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::HashSet;

    fn known(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn tc(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: opex_types::ids::ToolCallId::new(format!("call_{name}")),
            name: name.to_string(),
            arguments: args,
            thought_signature: None,
        }
    }

    #[test]
    fn passes_non_tool_use_calls_through() {
        let calls = vec![tc("workspace_read", json!({"filename": "x.md"}))];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["workspace_read"]), &[]);
        assert_eq!(r.len(), 1);
        assert!(matches!(&r[0], RewriteResult::Direct(t) if t.name == "workspace_read"));
    }

    #[test]
    fn passes_search_and_describe_through_as_tool_use() {
        let calls = vec![
            tc("tool_use", json!({"action": "search", "query": "cron"})),
            tc("tool_use", json!({"action": "describe", "name": "cron"})),
        ];
        let r = rewrite_tool_use_calls(&calls, None, &known(&[]), &[]);
        assert!(r.iter().all(|x| matches!(x, RewriteResult::Direct(t) if t.name == "tool_use")));
    }

    #[test]
    fn rewrites_call_action() {
        let calls = vec![tc("tool_use", json!({
            "action": "call",
            "name": "cron",
            "arguments": {"action": "list"}
        }))];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["cron"]), &[]);
        match &r[0] {
            RewriteResult::Direct(t) => {
                assert_eq!(t.name, "cron");
                assert_eq!(t.id.as_str(), "call_tool_use", "tc.id must be preserved");
                assert_eq!(t.arguments["action"], "list");
            }
            _ => panic!("expected Direct"),
        }
    }

    #[test]
    fn rejects_call_to_tool_use() {
        let calls = vec![tc("tool_use", json!({
            "action": "call", "name": "tool_use", "arguments": {}
        }))];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["tool_use"]), &[]);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, .. } if reason.contains("itself")));
    }

    #[test]
    fn rejects_unknown_tool() {
        let calls = vec![tc("tool_use", json!({
            "action": "call", "name": "nonexistent", "arguments": {}
        }))];
        let r = rewrite_tool_use_calls(&calls, None, &known(&[]), &[]);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, .. } if reason.contains("not found")));
    }

    #[test]
    fn rejects_invalid_name() {
        let calls = vec![tc("tool_use", json!({
            "action": "call", "name": "../etc/passwd", "arguments": {}
        }))];
        let r = rewrite_tool_use_calls(&calls, None, &known(&[]), &[]);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, .. } if reason.contains("invalid")));
    }

    #[test]
    fn enforces_runtime_deny_gate() {
        let policy = AgentToolPolicy {
            deny: vec!["process".to_string()],
            ..Default::default()
        };
        let calls = vec![tc("tool_use", json!({
            "action": "call", "name": "process", "arguments": {}
        }))];
        let r = rewrite_tool_use_calls(&calls, Some(&policy), &known(&["process"]), &[]);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, .. } if reason.contains("denied by agent policy")));
    }

    #[test]
    fn enforces_subagent_extra_deny() {
        // Audit 2026-05-08: when extra_deny carries the parent's
        // SUBAGENT_DENIED_TOOLS list, the dispatcher must refuse the inner
        // call even if the subagent's own tool_policy.deny is empty.
        let calls = vec![tc("tool_use", json!({
            "action": "call", "name": "code_exec", "arguments": {}
        }))];
        let extra_deny = vec!["code_exec".to_string(), "cron".to_string()];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["code_exec"]), &extra_deny);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, .. } if reason.contains("denied for subagents")));
    }

    #[test]
    fn denies_direct_call_to_subagent_denied_tool() {
        // F006: a DIRECT tool call (name != "tool_use") to a tool in the
        // subagent runtime denylist must be refused — not just the
        // tool_use(action=call) form. A prompt-injected subagent that emits
        // `{name:"code_exec", ...}` directly must be blocked identically.
        let calls = vec![tc("code_exec", json!({"command": "rm -rf /"}))];
        let extra_deny = vec!["code_exec".to_string(), "cron".to_string()];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["code_exec"]), &extra_deny);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, id }
            if reason.contains("denied for subagents") && id == "call_code_exec"),
            "direct call to a subagent-denied tool must be Denied, got: {:?}",
            matches!(&r[0], RewriteResult::Direct(_)));
    }

    #[test]
    fn denies_direct_call_to_policy_denied_tool() {
        // F006: the agent's own policy.deny must also gate DIRECT calls, not
        // only tool_use(action=call).
        let policy = AgentToolPolicy {
            deny: vec!["secret_set".to_string()],
            ..Default::default()
        };
        let calls = vec![tc("secret_set", json!({"name": "X", "value": "y"}))];
        let r = rewrite_tool_use_calls(&calls, Some(&policy), &known(&["secret_set"]), &[]);
        assert!(matches!(&r[0], RewriteResult::Denied { reason, .. }
            if reason.contains("denied by agent policy")),
            "direct call to a policy-denied tool must be Denied");
    }

    #[test]
    fn allows_direct_call_to_permitted_tool() {
        // Regression guard: the new direct-call deny gate must NOT reject a
        // normal, permitted direct call.
        let calls = vec![tc("workspace_read", json!({"filename": "x.md"}))];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["workspace_read"]), &["code_exec".to_string()]);
        assert!(matches!(&r[0], RewriteResult::Direct(t) if t.name == "workspace_read"));
    }

    #[test]
    fn preserves_tc_id_on_all_paths() {
        let calls = vec![
            tc("tool_use", json!({"action": "search", "query": "x"})),
            tc("tool_use", json!({"action": "call", "name": "cron", "arguments": {}})),
            tc("tool_use", json!({"action": "call", "name": "unknown", "arguments": {}})),
        ];
        let r = rewrite_tool_use_calls(&calls, None, &known(&["cron"]), &[]);
        match &r[0] { RewriteResult::Direct(t) => assert_eq!(t.id.as_str(), "call_tool_use"), _ => panic!() }
        match &r[1] { RewriteResult::Direct(t) => assert_eq!(t.id.as_str(), "call_tool_use"), _ => panic!() }
        match &r[2] { RewriteResult::Denied { id, .. } => assert_eq!(id, "call_tool_use"), _ => panic!() }
    }
}
