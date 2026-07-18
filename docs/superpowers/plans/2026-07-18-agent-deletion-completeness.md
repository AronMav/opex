# Agent Deletion Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Agent deletion removes ALL ephemeral per-agent state (both `agent_id` and `agent_name` bindings), an opt-in `purge_history` removes history, a two-tier drift guard prevents future orphan classes, and the 4 deprecated tables are dropped (m090) in a safe order.

**Architecture:** Three-class table classification (`Ephemeral`/`History`/`DropRipe`) over both binding columns becomes the single source of truth: the delete path iterates Ephemeral constants (NEVER the full `TABLES_WITH_AGENT_ID_NOT_NULL` catalogue — that contains History and iterating it would destroy audit on a plain delete), purge deletes History via FK CASCADE from `sessions`, an sqlx drift test guards migration tables and a doctor check guards out-of-migration prod tables.

**Tech Stack:** Rust 2024 (opex-core), sqlx/PostgreSQL, Next.js UI. Spec: `docs/superpowers/specs/2026-07-18-agent-deletion-completeness-design.md` (review-fixed rev, commit 19512d9f).

## Global Constraints

- Rust + rustls-tls only; no OpenSSL. No API-contract breakage (new query param is additive).
- Gate: `cargo check -p opex-core --all-targets` + `cargo clippy -p opex-core --all-targets` clean. `#[sqlx::test]` needs DATABASE_URL — authoritative run on the server (`make test-db` / targeted) BEFORE deploying DB-touching code (lesson: tool_quality INT4 incident).
- No push/deploy without explicit operator approval. Work in `master`. No `Co-Authored-By`.
- **Ordering (hard):** Task 1-2 (classification+guards) land BEFORE Task 3 (m090); m090 deploys only after the constants no longer reference `pending_messages`. Prod export (pg_dump) runs before the m090 deploy.
- Prod access: `ssh aronmav@188.246.224.118`; psql via `docker exec docker-postgres-1 psql -U opex -d opex`. `rg` absent on server — use grep. UI deploy = `bash scripts/deploy-ui.sh`; core deploy = `bash ~/opex-src/scripts/server-deploy.sh`.
- `docker compose` on the server: ALWAYS `--no-deps` for services with critical `depends_on` (main-DB incident lesson). Not needed in this plan (no compose changes), listed as a guardrail.

## File Structure

| File | Change |
|---|---|
| `crates/opex-core/src/gateway/handlers/agents/crud.rs` | consts surgery (L113-168), rename unification (~L892-931), `cleanup_agent_data` expansion (L1036-1051), `api_delete_agent` (L1053-1131): soul backup, workspace dir, session_pools kill, purge_history; tests (L1473-1490 rename-test rewrite, new drift test near L1569) |
| `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs` | classification-vs-schema check (handler has `infra.db`, see L47) |
| `migrations/090_drop_deprecated_tables.sql` | DROP 4 deprecated tables |
| `ui/src/app/(authenticated)/agents/page.tsx` | delete dialog (L786-808): purge checkbox + warning + profile hint |
| `ui/src/lib/api.ts` | `deleteAgent(name, purgeHistory)` |
| `ui/src/i18n/locales/{en,ru}.json` | new keys |
| `docs/runbooks/agent-deletion.md` | new runbook |

---

### Task 1: Classification constants + drift test + rename unification

**Files:** Modify `crates/opex-core/src/gateway/handlers/agents/crud.rs`.

**Interfaces — Produces (later tasks consume):**
- `pub(super) const TABLES_TO_DELETE_BY_AGENT_ID: &[&str]` — EXPANDED to all 13 flat Ephemeral(agent_id) tables (memory_chunks excluded — special-cased in Task 4).
- `pub(super) const TABLES_WITH_AGENT_NAME: &[&str]` — NEW, 6 tables.
- `pub(super) const TABLES_HISTORY_AGENT_ID: &[&str]` — NEW, 7 tables (purge + drift test).
- `pub(super) const TABLES_DROP_RIPE: &[&str]` — NEW, 4 tables (drift-test allowance until m090).

- [ ] **Step 1: Const surgery** in `crud.rs`:

(a) Remove `"pending_messages"` from `TABLES_WITH_AGENT_ID_NOT_NULL` (L128) — the const rename iterates; after m090 the relation is gone.

(b) Replace `TABLES_TO_DELETE_BY_AGENT_ID` (L157-168) with the full flat Ephemeral set (keep the doc-comment, update it):

```rust
/// Tables from which to DELETE rows when an agent is deleted.
///
/// This is the Ephemeral(agent_id) class of the deletion-completeness design
/// (docs/superpowers/specs/2026-07-18-agent-deletion-completeness-design.md):
/// per-agent runtime/state with no compliance value. It is a STRICT SUBSET of
/// `TABLES_WITH_AGENT_ID_NOT_NULL`; History tables (`sessions`, `audit_log`,
/// `audit_events`, `usage_log`, `session_failures`, `cron_runs`) must SURVIVE
/// a plain delete and are removed only by purge_history. `memory_chunks` is
/// deliberately NOT here — it needs scope-aware handling (private delete +
/// shared anonymize), see `cleanup_agent_data`.
pub(super) const TABLES_TO_DELETE_BY_AGENT_ID: &[&str] = &[
    "agent_emotion_state",
    "agent_github_repos",
    "agent_oauth_bindings",
    "agent_plans",
    "approval_allowlist",
    "channel_allowed_users",
    "gmail_triggers",
    "outbound_queue",
    "pairing_codes",
    "pending_approvals",
    "scheduled_jobs",
    "stream_jobs",
    "webhooks",
];
```

(c) Add two new consts after it:

```rust
/// Tables keyed by `agent_name` (no agent_id column). All Ephemeral: both
/// rename and delete iterate this list. Unifies the previous ad-hoc handling
/// (agent_channels/agent_model_overrides separate UPDATEs + an inline list in
/// the rename transaction).
pub(super) const TABLES_WITH_AGENT_NAME: &[&str] = &[
    "agent_channels",
    "agent_model_overrides",
    "handler_config",
    "handler_jobs",
    "pending_skill_repairs",
    "tool_quality",
];

/// History/compliance tables (agent_id). Survive plain delete; removed only
/// by purge_history (sessions CASCADE covers the session-child family).
pub(super) const TABLES_HISTORY_AGENT_ID: &[&str] = &[
    "audit_events",
    "audit_log",
    "cron_runs",
    "session_failures",
    "sessions",
    "usage_log",
];

/// Deprecated tables pending the m090 DROP. Excluded from every operation;
/// tolerated by the drift test until the migration lands, then vestigial.
pub(super) const TABLES_DROP_RIPE: &[&str] = &[
    "pending_messages",
    "video_jobs",
    "file_scenarios",
    "file_scenario_outcomes",
];
```

- [ ] **Step 2: Unify rename's agent_name handling.** In the rename transaction (crud.rs ~L889-931): replace the two separate UPDATEs for `agent_channels` (L890) / `agent_model_overrides` (L902) AND the inline 4-table loop (L919: `["handler_config", "tool_quality", "handler_jobs", "pending_skill_repairs"]`) with ONE loop over `TABLES_WITH_AGENT_NAME` (same UPDATE shape, same error handling as the existing inline loop).

- [ ] **Step 3: Rewrite the stale rename test.** `test_rename_mid_failure_leaves_pre_rename_state` (L1473-1490) pins a hand-written 20-table list. Rewrite to derive from the real constants:

```rust
let expected: usize = TABLES_WITH_AGENT_ID_NOT_NULL.len() + TABLES_WITH_AGENT_ID_NULLABLE.len();
// ... existing assertion structure, but counts/membership from the consts,
// so const surgery can never silently diverge from the test again.
```

- [ ] **Step 4: Add the sqlx drift test** (next to `test_tables_with_agent_id_all_exist_in_schema`, L1569 — same `#[sqlx::test(migrations = "../../migrations")]` attribute):

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn test_every_agent_binding_is_classified(pool: sqlx::PgPool) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name, column_name FROM information_schema.columns \
         WHERE table_schema='public' AND column_name IN ('agent_id','agent_name')",
    ).fetch_all(&pool).await.unwrap();

    let classified: std::collections::HashSet<&str> = TABLES_TO_DELETE_BY_AGENT_ID.iter()
        .chain(TABLES_HISTORY_AGENT_ID)
        .chain(TABLES_WITH_AGENT_ID_NULLABLE)   // messages
        .chain(TABLES_WITH_AGENT_NAME)
        .chain(TABLES_DROP_RIPE)
        .copied()
        .chain(std::iter::once("memory_chunks")) // §3.3 special case
        .collect();

    for (table, col) in &rows {
        assert!(
            classified.contains(table.as_str()),
            "table {table} (column {col}) has an agent binding but no AgentDataClass — \
             classify it in crud.rs (Ephemeral/History/DropRipe) before merging"
        );
    }
    // No table may be in two classes.
    let total = TABLES_TO_DELETE_BY_AGENT_ID.len() + TABLES_HISTORY_AGENT_ID.len()
        + TABLES_WITH_AGENT_ID_NULLABLE.len() + TABLES_WITH_AGENT_NAME.len()
        + TABLES_DROP_RIPE.len() + 1;
    assert_eq!(classified.len(), total, "a table is classified twice");
}
```

- [ ] **Step 5: Gate + commit.** `cargo check -p opex-core --all-targets` + clippy clean; local run of non-DB tests. Commit: `feat(agents): three-class table classification + agent_name const + drift test (deletion-completeness T1)`.

### Task 2: Doctor prod-side classification check

**Files:** Modify `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs` (+ small pure fn near the consts in `crud.rs` or in doctor.rs).

**Interfaces — Produces:** `pub(super) fn unclassified_agent_tables(schema: &[String]) -> Vec<String>` (pure, unit-testable) + a `doctor` check entry `agent_table_classification` with `status: ok|warn` and the unclassified list.

- [ ] **Step 1: Pure fn + unit test** (no DB): given a list of table names from information_schema, return those not in (all five consts + memory_chunks) and not in the allowlist `["eventbak_prune"]`. Unit test: known table → empty; fake `"new_agent_table"` → returned.
- [ ] **Step 2: Wire into doctor** — in `api_doctor` (doctor.rs, `infra.db` already used at L47): run the same information_schema query against the LIVE db, call the pure fn, append a check `{name:"agent_table_classification", status, unclassified:[...]}` mirroring the existing check-entry shape in the handler.
- [ ] **Step 3: Gate + commit** `feat(doctor): warn on unclassified agent-bound tables (out-of-migration guard)`.

### Task 3: m090 drop of deprecated tables (ordering-critical)

**Files:** Create `migrations/090_drop_deprecated_tables.sql`.

- [ ] **Step 1: Migration** (documentary preamble in the style of m069/m089):

```sql
-- 090: Drop the four deprecated tables (deletion-completeness design, 2026-07-18).
-- pending_messages (m089-deprecated, never wired), video_jobs (m068, superseded by
-- handler_jobs), file_scenarios + file_scenario_outcomes (m069, superseded by the
-- File Handler Hub). Constants/tests stopped referencing pending_messages in the
-- same release (T1) — deploying this migration on an older binary would break
-- agent RENAME, hence single-release ordering. Operator exported file_scenarios
-- (4 rows) + video_jobs (11 rows) via pg_dump before this migration (runbook).
DROP TABLE IF EXISTS pending_messages;
DROP TABLE IF EXISTS video_jobs;
DROP TABLE IF EXISTS file_scenarios;
DROP TABLE IF EXISTS file_scenario_outcomes;
```

- [ ] **Step 2: Grep guard** — `grep -rn 'pending_messages\|video_jobs\|file_scenarios\|file_scenario_outcomes' crates/ --include=*.rs | grep -v 'DROP_RIPE\|test\|//'` → only comments/consts remain (no live queries). Expected: matches only in `TABLES_DROP_RIPE`, doc-comments, and migration files.
- [ ] **Step 3: Commit** `feat(db): m090 drop deprecated tables (pending_messages, video_jobs, file_scenarios, file_scenario_outcomes)`.
- [ ] **Step 4 (deploy-time, operator):** BEFORE the deploy that carries m090: `ssh aronmav@188.246.224.118 "docker exec docker-postgres-1 pg_dump -U opex -d opex --table=file_scenarios --table=video_jobs --data-only" > экспорт в ~/opex/backups/deprecated-tables-$(date).sql` (runbook step; pending_messages/file_scenario_outcomes are 0 rows).

### Task 4: Delete-path expansion (ephemeral both columns + memory_chunks + soul backup)

**Files:** Modify `crud.rs` (`cleanup_agent_data` L1036-1051, `api_delete_agent` L1053+).

- [ ] **Step 1: Expand `cleanup_agent_data`.** Replace the two hardcoded agent_name DELETEs with a loop over `TABLES_WITH_AGENT_NAME`; the agent_id loop already iterates `TABLES_TO_DELETE_BY_AGENT_ID` (now 13) via `delete_agent_id_in_tables`. Add the memory_chunks special case inside the same transaction:

```rust
    // memory_chunks (§3.3): private facts + soul biography are the agent's —
    // delete; shared knowledge survives its author — anonymize.
    sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1 AND scope = 'private'")
        .bind(agent_name).execute(&mut *tx).await?;
    sqlx::query("UPDATE memory_chunks SET agent_id = '' WHERE agent_id = $1 AND scope = 'shared'")
        .bind(agent_name).execute(&mut *tx).await?;
```

- [ ] **Step 2: Mandatory soul backup BEFORE cleanup.** In `api_delete_agent`, after the base-agent guard (uses the already-parsed TOML): if the config had `[agent.soul] enabled = true`, dump the biography to a file and FAIL CLOSED on error (mandatory per spec §4.2):

```rust
    // Soul biography backup (mandatory for soul agents — the SQL below bypasses
    // the refuse_if_biography guards; consistent with runbooks/soul-quarantine.md).
    if soul_enabled {
        let rows: Vec<(uuid::Uuid, String, String, String)> = sqlx::query_as(
            "SELECT id, kind, content, created_at::text FROM memory_chunks \
             WHERE agent_id = $1 AND kind IN ('event','reflection')")
            .bind(&name).fetch_all(&infra.db).await.unwrap_or_default();
        let dir = std::path::Path::new(&crate::config::WORKSPACE_DIR.to_string())
            .parent().unwrap_or_else(|| std::path::Path::new("."))
            .join("backups").join("agent-deletion");
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| {
            let f = dir.join(format!("{}-{}.json", name, chrono::Utc::now().format("%Y%m%d-%H%M%S")));
            std::fs::write(&f, serde_json::to_vec_pretty(&rows.iter().map(|(id,k,c,t)|
                serde_json::json!({"id":id,"kind":k,"content":c,"created_at":t})).collect::<Vec<_>>())?)
        }) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": format!("soul biography backup failed; deletion aborted: {e}")
            }))).into_response();
        }
    }
```

(Derive `soul_enabled` from the parsed config in the existing base-guard match — restructure it to keep the parsed `AgentConfig` instead of discarding it.)

- [ ] **Step 3: sqlx tests** (server-authoritative): `test_ephemeral_delete_removes_all_state` — seed 1 row into every `TABLES_TO_DELETE_BY_AGENT_ID` + `TABLES_WITH_AGENT_NAME` table + private & shared memory_chunks; call `cleanup_agent_data`; assert 0 ephemeral rows remain, History rows survive, and the shared chunk survives with `agent_id=''`. (Seeding uses minimal INSERTs per table — column defaults cover the rest; where NOT NULL columns exist, supply minimal values; mirror the seeding style of existing sqlx tests in the file.)
- [ ] **Step 4: Gate + commit** `feat(agents): delete removes all ephemeral state (both bindings), scope-aware memory_chunks, mandatory soul backup`.

### Task 5: Workspace dir removal + session_pools kill

**Files:** Modify `crud.rs` (`api_delete_agent`).

- [ ] **Step 1: Workspace dir (best-effort, path-guarded).** After the vault cleanup block (L1104):

```rust
    // Best-effort: remove the agent's workspace directory (SOUL/SELF/MEMORY…).
    // Path-guarded like agent/workspace.rs: canonicalize and require the target
    // to stay under {workspace}/agents/. Never fails the deletion.
    let ws_root = std::path::Path::new(&*crate::config::WORKSPACE_DIR).join("agents");
    let target = ws_root.join(&name);
    match (dunce::canonicalize(&ws_root), dunce::canonicalize(&target)) {
        (Ok(root_c), Ok(target_c)) if target_c.starts_with(&root_c) && target_c != root_c => {
            if let Err(e) = std::fs::remove_dir_all(&target_c) {
                tracing::warn!(agent = %name, error = %e, "workspace dir removal failed (best-effort)");
            } else {
                tracing::info!(agent = %name, "workspace dir removed");
            }
        }
        (Ok(_), Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {} // no dir — fine
        other => tracing::warn!(agent = %name, ?other, "workspace dir path-guard refused removal"),
    }
```

(Match the exact `WORKSPACE_DIR` accessor used elsewhere in the file/crate — it is the same constant `agent_config_path` builds from.)

- [ ] **Step 2: Kill session-scoped subagents.** Next to the engine hot-stop (L1113):

```rust
    // Kill any session-scoped LiveAgent instances of this agent in other
    // sessions' pools (they run background tasks with their own dialog state).
    for pool in agents.session_pools.iter() {
        pool.value().remove(&name).await;
    }
```

(Verify the exact map/API shape against `session_agent_pool.rs:128` `remove(name)` and the `session_pools` field in `clusters/agent_core.rs:24`; adapt iteration accordingly.)

- [ ] **Step 3: Gate + commit** `feat(agents): delete removes workspace dir (path-guarded) and kills session-scoped subagents`.

### Task 6: purge_history backend

**Files:** Modify `crud.rs` (`api_delete_agent` signature + new purge fn).

**Interfaces — Produces:** `DELETE /api/agents/{name}?purge_history=true`.

- [ ] **Step 1: Query param.** Add `Query<DeleteAgentQuery>` extractor (`#[derive(Deserialize)] struct DeleteAgentQuery { #[serde(default)] purge_history: bool }`).
- [ ] **Step 2: Purge fn** (called after `cleanup_agent_data`, own transaction):

```rust
async fn purge_agent_history(db: &sqlx::PgPool, agent_name: &str) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    // Sessions where the agent is primary: FK CASCADE removes messages,
    // session_timeline, session_failures, session_shares, session_goals,
    // session_todos, stream_jobs, pending_approvals (verified prod FKs).
    // Owner decision: multi-agent sessions are deleted whole.
    sqlx::query("DELETE FROM sessions WHERE agent_id = $1").bind(agent_name).execute(&mut *tx).await?;
    // No covering CASCADE (session_failures: CASCADE removes rows tied to the
    // agent's sessions, but rows whose session is already gone would survive —
    // hence the explicit sweep):
    for t in ["usage_log", "audit_log", "audit_events", "cron_runs", "session_failures"] {
        sqlx::query(&format!("DELETE FROM {t} WHERE agent_id = $1"))
            .bind(agent_name).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
```

Messages by this agent inside OTHER agents' sessions are intentionally untouched (foreign history).
- [ ] **Step 3: sqlx tests** — `test_history_preserved_by_default` (plain delete leaves a seeded session+message+usage row) and `test_purge_history_cascades` (purge removes session AND its timeline/failure child rows without explicit child DELETEs; usage_log/audit gone).
- [ ] **Step 4: Gate + commit** `feat(agents): purge_history option — CASCADE-based full history removal`.

### Task 7: UI — purge checkbox + profile hint

**Files:** `ui/src/app/(authenticated)/agents/page.tsx` (delete AlertDialog L786-808), `ui/src/lib/api.ts`, `ui/src/i18n/locales/{en,ru}.json`.

- [ ] **Step 1: api.ts** — `deleteAgent(name: string, purgeHistory = false)` appends `?purge_history=true` when set.
- [ ] **Step 2: Dialog** — add local state `purgeHistory`; render a checkbox inside the existing AlertDialog: label `t("agents.delete_purge_history")`, sub-warning `t("agents.delete_purge_warning")` (irreversible; co-participant turns in this agent's sessions are lost). Default off. Pass to `deleteAgent`.
- [ ] **Step 3: Profile hint** — after successful delete, if the profiles list (existing profiles query/api) contains a profile named exactly like the agent, `toast.info(t("agents.delete_profile_hint", {name}))` with a link/navigate to `/profiles`.
- [ ] **Step 4: i18n** — en: `"agents.delete_purge_history": "Also delete chat history and audit"`, `"agents.delete_purge_warning": "Irreversible. Sessions where this agent was primary are deleted whole — including other participants' turns."`, `"agents.delete_profile_hint": "Profile '{name}' still exists — delete it separately on the Profiles page if unneeded."`; ru: «Также удалить историю переписки и аудит», «Необратимо. Сессии, где агент был основным, удаляются целиком — включая реплики других участников.», «Профиль '{name}' ещё существует — при необходимости удалите его отдельно на странице Профили.»
- [ ] **Step 5: Gate** — `cd ui && npx tsc --noEmit` + `npx vitest run` (existing agents-page tests). Commit `feat(ui): purge-history option + profile hint in agent delete dialog`.

### Task 8: Runbook + docs + one-shot Lana cleanup + deploy

- [ ] **Step 1: Runbook** `docs/runbooks/agent-deletion.md`: purge semantics + irreversibility; mandatory soul backup (path `backups/agent-deletion/`); out-of-scope tails (author-created `workspace/skills/*`, `eventbak_prune` — manual DROP after decay confirmation); m090 export step; the 2026-07-17 Lana/Arty incident as precedent. Link from `docs/ARCHITECTURE.md` (deletion section) + update CLAUDE.md's "~20 DB tables" rename note to reference the constants.
- [ ] **Step 2: Commit docs.**
- [ ] **Step 3 (deploy, operator approval):** push → pg_dump export (Task 3 Step 4) → `server-deploy.sh` (carries code + m090 auto-migration) → smoke: `/health` 200, NRestarts=0, m090 applied (`SELECT version, success FROM _sqlx_migrations WHERE version=90`), doctor shows `agent_table_classification: ok` (eventbak_prune allowlisted), agent rename works (test agent), UI dialog shows checkbox.
- [ ] **Step 4 (one-shot Lana cleanup, server, after deploy):** `DELETE FROM tool_quality WHERE agent_name='Lana'` (5 rows); check `agents_using_profile('Lana')` empty → `DELETE FROM profiles WHERE name='Lana'`; `rm ~/opex/workspace/skills/lana-agent-config-read.md ~/opex/workspace/skills/lana-config-20260716.md` (+ `.bak` leftovers per audit v2 B). Closes audit v2 A6.
- [ ] **Step 5: Server-authoritative sqlx tests** — run the new drift/delete/purge tests on the server (`CARGO_BUILD_JOBS=4 nice ionice` + DATABASE_URL) before declaring done.

---

## Final validation

- Drift test green on server; doctor `agent_table_classification` ok on prod.
- Delete a scratch agent (create → seed a handler_config/tool_quality row via a tool call → delete) → zero ephemeral rows both bindings, workspace dir gone, History intact.
- Same with `purge_history=true` → sessions/usage/audit gone.
- Rename a scratch agent → no `relation does not exist` (m090 ordering held).
