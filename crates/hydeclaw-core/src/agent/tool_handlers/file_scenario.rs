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
    handle_create_inner(deps.db, deps.agent_name, args).await
}

/// Inner implementation extracted for DB-backed testing: only needs the pool
/// and agent name, not the full `ToolDeps` graph.
pub(crate) async fn handle_create_inner(
    db: &sqlx::PgPool,
    agent_name: &str,
    args: &Value,
) -> String {
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
    let enabled_allowlist = crate::agent::fse::get_enabled_allowlist(db).await;
    if let Err(e) = crate::agent::fse::validate_binding_write(
        AGENT_AUTHORED_EXECUTOR, // "skill" — never "tool"
        &action_ref,
        false, // is_default always false for agent-authored rows
        &enabled_allowlist,
    ) {
        return format!("Error: scenario rejected: {e}");
    }

    let created_by = format!("agent:{agent_name}");
    match crate::db::file_scenarios::create(
        db,
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
                db.clone(),
                agent_name.to_string(),
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

    // ── DB-backed: constrained-write + no arg-order swap ─────────────────────
    //
    // Task 7.3 coverage gap: handle_create calls db::file_scenarios::create with
    // 9 positional args. An arg-order swap (e.g. match_type ↔ executor ↔
    // action_ref) compiles silently and persists a malformed row.  The four
    // existing tests above are DB-free (base-gate + arg-parse only).  This
    // test exercises the actual persist path via handle_create_inner — the
    // thin wrapper handle_create delegates to — so it catches:
    //
    //   1. Constrained-write invariant: even if the caller passes
    //      executor="tool" and is_default=true in args, the persisted row
    //      must have executor="skill" and is_default=false.
    //   2. No argument-order swap: persisted match_type / action_ref / label
    //      equal the values in args (catches positional transposition).
    //   3. created_by: persisted as "agent:<agent_name>".
    //   4. Return value: contains the new scenario UUID.
    //   5. Audit row: FILE_SCENARIO_CREATED event lands in audit_events.
    //
    // Runs only under `make test-db` (needs DATABASE_URL pointing at the
    // isolated Postgres on :5434), consistent with the 8 other sqlx::test
    // gates documented in CLAUDE.md.
    #[sqlx::test(migrations = "../../migrations")]
    async fn handle_create_inner_persists_correct_row(pool: sqlx::PgPool) {
        // Args include executor="tool" and is_default=true — both must be
        // IGNORED by handle_create_inner (constrained authoring invariant).
        let args = serde_json::json!({
            "action":    "create",
            "match_type": "image/*",
            "action_ref": "my_skill",
            "label":      "Describe image",
            "executor":   "tool",    // must be overridden → "skill"
            "is_default": true       // must be overridden → false
        });

        let result = handle_create_inner(&pool, "TestAgent", &args).await;

        // ── 4. Return value contains a UUID ──────────────────────────────
        assert!(
            !result.starts_with("Error"),
            "handle_create_inner returned an error: {result}"
        );

        // Extract the UUID from the return string.
        let scenario_id: uuid::Uuid = result
            .split_whitespace()
            .nth(2) // "Created scenario <UUID> — ..."
            .expect("UUID token present")
            .parse()
            .expect("token is a valid UUID");

        // ── Fetch the persisted row ───────────────────────────────────────
        let row = crate::db::file_scenarios::get_by_id(&pool, scenario_id)
            .await
            .expect("DB read")
            .expect("row must exist");

        // ── 2. No argument-order swap ─────────────────────────────────────
        assert_eq!(row.match_type, "image/*",   "match_type persisted correctly");
        assert_eq!(row.action_ref, "my_skill",  "action_ref persisted correctly");
        assert_eq!(row.label,      "Describe image", "label persisted correctly");

        // ── 1. Constrained-write invariant ───────────────────────────────
        assert_eq!(
            row.executor, "skill",
            "executor must always be 'skill' regardless of args"
        );
        assert!(
            !row.is_default,
            "is_default must always be false regardless of args"
        );

        // ── 3. created_by ────────────────────────────────────────────────
        assert_eq!(
            row.created_by, "agent:TestAgent",
            "created_by must be 'agent:<agent_name>'"
        );

        // ── 5. Audit row ─────────────────────────────────────────────────
        // audit_spawn is fire-and-forget; yield to let the spawned task run.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let audit_rows = crate::db::audit::query_events(
            &pool,
            Some("TestAgent"),
            Some(crate::db::audit::event_types::FILE_SCENARIO_CREATED),
            10,
            0,
        )
        .await
        .expect("audit query");
        assert!(
            !audit_rows.is_empty(),
            "FILE_SCENARIO_CREATED audit event must be written"
        );
        let ev = &audit_rows[0];
        assert_eq!(
            ev.details.get("scenario_id").and_then(|v| v.as_str()),
            Some(scenario_id.to_string().as_str()),
            "audit details must include the scenario_id"
        );
        assert_eq!(
            ev.details.get("executor").and_then(|v| v.as_str()),
            Some("skill"),
            "audit details must record executor='skill'"
        );
    }
}
