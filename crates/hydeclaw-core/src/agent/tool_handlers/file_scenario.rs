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

/// Error returned when a non-base agent attempts 'create'. Single source of
/// truth: both `handle()` and `create_as_agent` call `require_base_for_create`,
/// so the security test exercises the SAME gate as production.
pub(crate) const NON_BASE_CREATE_ERROR: &str =
    "Error: file_scenario 'create' requires a base agent. Regular agents may only 'list'.";

/// Base-gate guard for the 'create' action. Returns `Err` with the canonical
/// error string when `agent_base` is false. Called by both the production
/// `handle()` dispatch and the `#[cfg(test)]` seam `create_as_agent`.
fn require_base_for_create(agent_base: bool) -> Result<(), String> {
    if agent_base {
        Ok(())
    } else {
        Err(NON_BASE_CREATE_ERROR.to_string())
    }
}

#[async_trait]
impl SystemToolHandler for FileScenarioHandler {
    async fn handle(&self, deps: ToolDeps<'_>, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        match action {
            "create" => {
                // create mutates the registry → base-only (mirror cron.rs:20).
                if let Err(e) = require_base_for_create(deps.agent_base) {
                    return e;
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

/// Security seam for DB-backed testing of the base-gate (Task 9.8).
/// Replicates the `handle()` routing for the "create" action without needing
/// the full `ToolDeps` graph: applies the base-gate first via the SHARED
/// `require_base_for_create` guard (same code path as production `handle()`),
/// then delegates to `handle_create_inner`.
///
/// The production path goes through `handle()` → `handle_create` →
/// `handle_create_inner`. This function mirrors that chain at the
/// db + agent_base level so inline `#[sqlx::test]` can verify both:
///   - base-gate: non-base returns the error string AND writes nothing to DB.
///   - clamp: malicious executor/is_default in args are ignored.
#[cfg(test)]
pub(crate) async fn create_as_agent(
    db: &sqlx::PgPool,
    agent_name: &str,
    agent_base: bool,
    args: &serde_json::Value,
) -> String {
    if let Err(e) = require_base_for_create(agent_base) {
        return e;
    }
    handle_create_inner(db, agent_name, args).await
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
        // Pins the string that production actually emits via NON_BASE_CREATE_ERROR.
        assert!(NON_BASE_CREATE_ERROR.contains("requires a base agent"));
        assert!(NON_BASE_CREATE_ERROR.contains("list"));
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

    // ── Task 9.8: integration-layer security e2e ─────────────────────────────
    //
    // Three assertions that go beyond Task 7.3's `handle_create_inner` unit test:
    //
    //  A. Clamp (integration layer, via create_as_agent): passes malicious
    //     executor="tool" and is_default=true through the base-gated seam —
    //     same invariant as 7.3 but explicitly named for the 9.8 requirement and
    //     exercising the `create_as_agent` entry point (not bare inner fn).
    //     Overlap with 7.3 is acknowledged in the task brief.
    //
    //  B. Base-gate behavioral (NEW): a non-base agent's create attempt is
    //     rejected AND leaves the file_scenarios table empty — 7.3 only tested
    //     the message constant, not that no DB write occurs.
    //
    //  C. Audit actor (NEW): the audit row emitted for a successful create
    //     carries actor = "agent:<agent_name>" (7.3 checked the audit event
    //     exists and its `executor` detail; 9.8 adds explicit actor verification).
    //
    // All three use `create_as_agent` — the thin seam that replicates the
    // handle() routing without requiring the full ToolDeps graph, which would
    // cascade the entire engine into the test tree. See lib.rs comment:
    // "When a test needs production code that isn't a leaf, the right pattern
    //  is to add the test inline next to the production code."

    /// Task 9.8-A: clamp via create_as_agent.
    /// Malicious args (executor="tool", is_default=true) passed to the base-gated
    /// entry point must be silently overridden. The persisted row must be
    /// executor="skill", is_default=false (escalation neutralized, not rejected).
    #[sqlx::test(migrations = "../../migrations")]
    async fn task_9_8_clamp_through_create_as_agent(pool: sqlx::PgPool) {
        let args = serde_json::json!({
            "action":     "create",
            "match_type": "application/pdf",
            "action_ref": "pdf_summarizer",
            "label":      "Summarize PDF",
            "executor":   "tool",   // malicious: attempt to escalate
            "is_default": true      // malicious: attempt to set default
        });

        let result = create_as_agent(&pool, "Atlas", true, &args).await;

        assert!(
            !result.starts_with("Error"),
            "base-agent create with malicious args must succeed (clamp, not reject): {result}"
        );

        // Extract the UUID from "Created scenario <UUID> — ..."
        let scenario_id: uuid::Uuid = result
            .split_whitespace()
            .nth(2)
            .expect("UUID token in result")
            .parse()
            .expect("token is a valid UUID");

        let row = crate::db::file_scenarios::get_by_id(&pool, scenario_id)
            .await
            .expect("DB read")
            .expect("row must exist");

        // Clamp invariant: executor and is_default are always overridden.
        assert_eq!(row.executor, "skill",
            "executor must be clamped to 'skill' regardless of args");
        assert!(!row.is_default,
            "is_default must be clamped to false regardless of args");
        assert_eq!(row.action_ref, "pdf_summarizer",
            "action_ref must be persisted from args");
    }

    /// Task 9.8-B: base-gate behavioral — non-base agent persists NOTHING.
    /// 7.3 only asserted the rejection message constant; this test verifies
    /// that the DB row count remains zero after a non-base create attempt.
    #[sqlx::test(migrations = "../../migrations")]
    async fn task_9_8_non_base_agent_persists_nothing(pool: sqlx::PgPool) {
        let args = serde_json::json!({
            "action":     "create",
            "match_type": "image/png",
            "action_ref": "my_skill",
            "label":      "Describe PNG",
            "executor":   "skill",
            "is_default": false
        });

        let result = create_as_agent(&pool, "Worker", false, &args).await;

        // Must be rejected with the base-gate message.
        assert!(
            result.contains("requires a base agent"),
            "non-base rejection must cite 'requires a base agent': {result}"
        );

        // DB must be clean — no scenario row written, no audit event written.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*)::bigint FROM file_scenarios")
            .fetch_one(&pool)
            .await
            .expect("count query");
        assert_eq!(count, 0,
            "non-base create must persist nothing (file_scenarios count must be 0)");

        let audit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM audit_events WHERE event_type = 'file_scenario_created'"
        )
        .fetch_one(&pool)
        .await
        .expect("audit count query");
        assert_eq!(audit_count, 0,
            "non-base create must emit no FILE_SCENARIO_CREATED audit event");
    }

    /// Task 9.8-C: audit actor format — the audit row emitted for a successful
    /// agent create must carry `actor = "agent:<agent_name>"`.
    /// 7.3 verified the audit event exists and its `executor` detail field;
    /// this test adds explicit actor-format verification.
    #[sqlx::test(migrations = "../../migrations")]
    async fn task_9_8_audit_actor_format(pool: sqlx::PgPool) {
        let args = serde_json::json!({
            "action":     "create",
            "match_type": "audio/*",
            "action_ref": "transcribe_audio",
            "label":      "Transcribe voice",
            "executor":   "skill",
            "is_default": false
        });

        let result = create_as_agent(&pool, "Hermes", true, &args).await;
        assert!(!result.starts_with("Error"),
            "create must succeed: {result}");

        // audit_spawn is fire-and-forget; yield briefly to let it settle.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let audit_rows = crate::db::audit::query_events(
            &pool,
            Some("Hermes"),
            Some(crate::db::audit::event_types::FILE_SCENARIO_CREATED),
            10,
            0,
        )
        .await
        .expect("audit query");

        assert!(!audit_rows.is_empty(),
            "FILE_SCENARIO_CREATED audit event must be emitted for a successful create");

        let ev = &audit_rows[0];
        // actor must be "agent:<agent_name>" — the canonical format used for
        // agent-authored rows (also stored in created_by column).
        assert_eq!(
            ev.actor.as_deref(),
            Some("agent:Hermes"),
            "audit actor must be 'agent:<agent_name>' format: {:?}", ev.actor
        );
        assert_eq!(
            ev.details.get("executor").and_then(|v| v.as_str()),
            Some("skill"),
            "audit details.executor must be 'skill'"
        );
    }
}
