//! Helper predicates and lookups for the tool dispatcher.

use opex_types::ToolDefinition;

/// Validate that a tool name is safe for path/URL contexts and matches our
/// naming convention. Identical to the validator used by /api/tools endpoints.
// allow(dead_code): consumed by tool_handlers/tool_use.rs.
#[allow(dead_code)]
pub fn is_valid_tool_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// Build the list of extension tools visible to an agent for search/describe.
///
/// Filters in order: deny-list, required_base for non-base, drop static core,
/// drop currently-promoted tools (they are in per-session core for this turn).
/// Capability tools shadow same-named YAML files.
/// Sorted alphabetically by name.
// allow(dead_code): consumed by tool_handlers/tool_use.rs.
#[allow(dead_code)]
pub async fn build_extension_tool_list(
    is_base_agent: bool,
    deny: &[String],
    promoted: &std::collections::HashSet<String>,
    workspace_dir: &str,
    slots: &crate::db::profiles::Slots,
    mcp: Option<&crate::mcp::McpRegistry>,
) -> Vec<ToolDefinition> {
    let core = crate::agent::pipeline::tool_defs::static_core_tool_names();
    let mut out: Vec<ToolDefinition> = Vec::new();

    // System extension tools = all_system - static_core.
    let all_sys = crate::agent::pipeline::tool_defs::all_system_tool_names();
    for sys_name in all_sys {
        if core.contains(sys_name) {
            continue;
        }
        if deny.iter().any(|d| d == sys_name) {
            continue;
        }
        if promoted.contains(*sys_name) {
            continue;
        }
        // Placeholder description — caller fills these from internal_tool_definitions().
        out.push(ToolDefinition {
            name: sys_name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({}),
        });
    }

    // YAML tools (capability-named files are skipped — they are added below).
    let yaml = crate::tools::yaml_tools::load_yaml_tools(workspace_dir, false).await;
    for t in yaml {
        if (!t.required_base || is_base_agent)
            && !deny.iter().any(|d| d == &t.name)
            && !promoted.contains(&t.name)
            && !crate::agent::capability_tools::is_capability_tool(&t.name)
        {
            out.push(t.to_tool_definition());
        }
    }

    // Built-in capability tools (gated by the agent's profile slots).
    for def in crate::agent::capability_tools::capability_tool_defs(slots) {
        if (!def.required_base || is_base_agent)
            && !deny.iter().any(|d| d == &def.name)
            && !promoted.contains(&def.name)
        {
            out.push(def.to_tool_definition());
        }
    }

    // MCP tools.
    if let Some(reg) = mcp {
        for d in reg.all_tool_definitions().await {
            if !deny.iter().any(|de| de == &d.name) && !promoted.contains(&d.name) {
                out.push(d);
            }
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Linear lookup. Used by describe handler. The list is < 100 entries in
/// practice — no index is warranted.
// allow(dead_code): consumed by tool_handlers/tool_use.rs.
#[allow(dead_code)]
pub async fn find_extension_tool(
    name: &str,
    is_base_agent: bool,
    deny: &[String],
    promoted: &std::collections::HashSet<String>,
    workspace_dir: &str,
    slots: &crate::db::profiles::Slots,
    mcp: Option<&crate::mcp::McpRegistry>,
) -> Option<ToolDefinition> {
    build_extension_tool_list(is_base_agent, deny, promoted, workspace_dir, slots, mcp)
        .await
        .into_iter()
        .find(|t| t.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_names_accepted() {
        assert!(is_valid_tool_name("cron"));
        assert!(is_valid_tool_name("workspace_read"));
        assert!(is_valid_tool_name("github-create-issue"));
        assert!(is_valid_tool_name("Tool1"));
    }

    #[test]
    fn invalid_names_rejected() {
        assert!(!is_valid_tool_name(""));
        assert!(!is_valid_tool_name("1cron"));
        assert!(!is_valid_tool_name("../etc/passwd"));
        assert!(!is_valid_tool_name("tool with spaces"));
        assert!(!is_valid_tool_name("tool/sub"));
        assert!(!is_valid_tool_name(&"a".repeat(65)));
    }
}
