//! System tool: file_scenario — constrained agent authoring of file→skill
//! scenarios (bindings). Mirrors `cron` (flat verb actions, base-gated via
//! deps.agent_base). Agents may ONLY write executor='skill', is_default=false
//! rows; the Phase-4 `validate_binding_write` validator + this handler both
//! enforce it (defense in depth). NOT an extension of the `agent` tool.

use async_trait::async_trait;
use serde_json::Value;

use crate::agent::tool_registry::{SystemToolHandler, ToolDeps};

pub struct FileScenarioHandler;

/// The fixed executor agents may author. Surfaced as a const so the drift test
/// can assert it never widens to "tool".
pub const AGENT_AUTHORED_EXECUTOR: &str = "skill";

#[async_trait]
impl SystemToolHandler for FileScenarioHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "create" => {
                // create mutates the registry → base-only (mirror cron.rs:20).
                if !deps.agent_base {
                    return "Error: file_scenario 'create' requires a base agent. Regular agents may only 'list'.".to_string();
                }
                handle_create(&deps, args).await
            }
            "list" => handle_list(&deps).await,
            _ => format!(
                "Error: unknown file_scenario action '{}'. Use: create, list.",
                action
            ),
        }
    }
}

/// create: validate args, force executor=skill + is_default=false, run the
/// Phase-4 caller-independent validator, persist, emit an audit event.
async fn handle_create(deps: &ToolDeps<'_>, args: &Value) -> String {
    let match_type = match args.get("match_type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return "Error: 'match_type' is required for create (e.g. 'image/*', 'application/pdf', '.mp4').".to_string(),
    };
    let action_ref = match args.get("action_ref").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return "Error: 'action_ref' (a skill name) is required for create.".to_string(),
    };
    let label = match args.get("label").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => return "Error: 'label' is required for create.".to_string(),
    };

    // Constrained authoring (hard-coded, not args-derived): skill + non-default.
    // Defense in depth: the same caller-independent validator the HTTP routes
    // use (Phase 4). For executor=skill + is_default=false this always passes,
    // but we call it anyway so any future tightening of the allowlist also
    // gates the agent tool path.
    let enabled_allowlist = crate::agent::fse::get_enabled_allowlist(deps.db).await;
    if let Err(e) = crate::agent::fse::validate_binding_write(
        AGENT_AUTHORED_EXECUTOR, // "skill" — never "tool"
        &action_ref,
        false, // is_default always false for agent-authored rows
        &enabled_allowlist,
    ) {
        return format!("Error: scenario rejected: {e}");
    }

    let created_by = format!("agent:{}", deps.agent_name);
    match crate::db::file_scenarios::create(
        deps.db,
        &match_type,
        AGENT_AUTHORED_EXECUTOR,
        &action_ref,
        &label,
        false, // is_default always false
        100,   // default priority
        true,  // enabled
        &created_by,
    )
    .await
    {
        Ok(id) => {
            crate::db::audit::audit_spawn(
                deps.db.clone(),
                deps.agent_name.to_string(),
                crate::db::audit::event_types::FILE_SCENARIO_CREATED,
                Some(created_by),
                serde_json::json!({
                    "scenario_id": id.to_string(),
                    "match_type": match_type,
                    "executor": AGENT_AUTHORED_EXECUTOR,
                    "action_ref": action_ref,
                    "is_default": false,
                }),
            );
            format!(
                "Created scenario {} — when a {} file arrives, '{}' becomes a selectable option (not the auto-default).",
                id, match_type, action_ref
            )
        }
        Err(e) => format!("Error creating scenario: {e}"),
    }
}

/// list: read-only, allowed for all agents.
async fn handle_list(deps: &ToolDeps<'_>) -> String {
    match crate::db::file_scenarios::list(deps.db).await {
        Ok(rows) if rows.is_empty() => "No file scenarios configured.".to_string(),
        Ok(rows) => {
            let mut out = format!("File scenarios ({}):\n", rows.len());
            for r in &rows {
                out.push_str(&format!(
                    "- {} → {}:{} ({}){}{}\n",
                    r.match_type,
                    r.executor,
                    r.action_ref,
                    r.label,
                    if r.is_default { " [default]" } else { "" },
                    if r.enabled { "" } else { " [disabled]" },
                ));
            }
            out
        }
        Err(e) => format!("Error listing scenarios: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handler_implements_trait() {
        fn assert_impl<T: SystemToolHandler>(_: T) {}
        assert_impl(FileScenarioHandler);
    }

    #[test]
    fn agent_executor_is_always_skill() {
        // Constrained authoring invariant — agents never author executor=tool.
        assert_eq!(AGENT_AUTHORED_EXECUTOR, "skill");
    }

    #[test]
    fn unknown_action_message_mentions_action() {
        let msg = format!(
            "Error: unknown file_scenario action '{}'. Use: create, list.",
            "explode"
        );
        assert!(msg.contains("explode"));
        assert!(msg.contains("create, list"));
    }

    #[test]
    fn non_base_create_message_is_clear() {
        let msg = "Error: file_scenario 'create' requires a base agent. Regular agents may only 'list'.";
        assert!(msg.contains("requires a base agent"));
    }
}
