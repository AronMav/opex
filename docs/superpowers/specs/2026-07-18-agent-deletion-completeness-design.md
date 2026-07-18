# Agent Deletion Completeness — Design Spec

**Date:** 2026-07-18
**Status:** design (pending user review)
**Source plan:** `docs/architecture/2026-07-17-agent-deletion-completeness-plan.md` (rev.2, verified against HEAD + prod schema)
**Related:** audit v2 `docs/architecture/2026-07-18-architecture-audit-v2.md` (A6 Lana tails, A7 rename-test/m090 ordering)

## 1. Problem

Deleting an agent via `DELETE /api/agents/{name}` (`api_delete_agent`, `crates/opex-core/src/gateway/handlers/agents/crud.rs`) leaves orphaned state. The handler clears the TOML, `agent_channels`/`agent_model_overrides` (by `agent_name`), the `TABLES_TO_DELETE_BY_AGENT_ID` const list, `uploads` (agent_icon), and the vault scope — but NOT:

- **`agent_name`-keyed tables** — `handler_config`, `handler_jobs`, `tool_quality`, `pending_skill_repairs` are not catalogued anywhere. **Confirmed live orphan: 5 `tool_quality` rows for the deleted agent Lana persist on prod today.**
- **Ephemeral `agent_id` state** not in the delete list — `agent_emotion_state`, `memory_chunks`, `outbound_queue`, `stream_jobs`, `pairing_codes`, `pending_approvals`.
- **The workspace directory** `workspace/agents/{name}/` (SOUL/SELF/MEMORY/avatar) — never touched; a soul-content leak.
- **`profiles`** — the same-named profile survives (Lana's profile is still in prod `profiles`).
- **Session-scoped subagents** (`SessionAgentPool`) of the deleted agent in other sessions — only the main engine handle is stopped.

Two systemic root causes: (1) the classification is an implicit hand-maintained subset with no "schema → list" guard; (2) `agent_name` bindings were never modelled. A third hazard (audit v2 A7): the deprecated `pending_messages` table sits under m090 DROP while still listed in `TABLES_WITH_AGENT_ID_NOT_NULL`, and `test_rename_mid_failure_leaves_pre_rename_state` pins a stale hand-written table list — so a naive drop breaks agent rename/delete.

## 2. Goals & non-goals

**Goals (this spec, v1 = full scope):** delete removes ALL ephemeral per-agent state (both binding columns); a `purge_history` option removes history/audit on request; a two-tier drift guard prevents future orphan classes; the 4 deprecated tables are dropped (m090) in the correct order; UI exposes the purge option and a profile hint.

**Non-goals:** deleting an agent's messages inside OTHER agents' sessions (foreign history); auto-deleting profiles (they are shared entities); dropping `eventbak_prune` in code (out-of-migration manual table — runbook DROP after decay confirmation); a general soft-delete/undo system.

## 3. Architecture

### 3.1 Three-class classification over both binding columns

Replace the implicit subset with an explicit class per **every** table that binds to an agent, across BOTH the `agent_id` and `agent_name` columns:

```rust
enum AgentDataClass {
    Ephemeral,  // per-agent runtime/state — DELETE on agent delete, always
    History,    // compliance/history — kept by default; removed only on purge_history=true
    DropRipe,   // deprecated table pending m090 DROP — excluded from all operations
}
```

- **Ephemeral (agent_id):** `agent_emotion_state`, `memory_chunks` (see §3.3), `outbound_queue`, `stream_jobs`, `pairing_codes`, `pending_approvals`, `scheduled_jobs`, `webhooks`, `agent_oauth_bindings`, `gmail_triggers`, `agent_github_repos`, `approval_allowlist`, `channel_allowed_users`, `agent_plans`.
- **Ephemeral (agent_name)** — NEW `const TABLES_WITH_AGENT_NAME`: `agent_channels`, `agent_model_overrides`, `handler_config`, `handler_jobs`, `tool_quality`, `pending_skill_repairs`.
- **History (agent_id):** `sessions`, `messages`, `audit_log`, `audit_events`, `usage_log`, `session_failures`, `cron_runs`.
- **DropRipe:** `pending_messages`, `video_jobs` (+ column-less `file_scenarios`, `file_scenario_outcomes`) — never added to the delete path; removed from `TABLES_WITH_AGENT_ID_NOT_NULL` first, then m090.
- **Non-column bindings** handled explicitly: `uploads` (agent_icon owner), vault scope, `profiles` (name convention — hint only).

The classification is the single source of truth consumed by the delete path, the purge path, and the drift guard. Ground truth for membership is prod `information_schema` (verified: 23 `agent_id` tables + 7 `agent_name` tables at HEAD).

### 3.2 Delete path (ephemeral, always)

`api_delete_agent` expansion, in the existing transaction where possible:
- Iterate `TABLES_WITH_AGENT_ID_NOT_NULL` (minus DropRipe) `DELETE ... WHERE agent_id=$1`, and the new `TABLES_WITH_AGENT_NAME` `DELETE ... WHERE agent_name=$1`.
- `memory_chunks` (§3.3) is a special case — not a plain `WHERE agent_id=$1`.
- After DB: best-effort remove `workspace/agents/{name}/` via the existing path-guard (§3.4); kill session-scoped subagents of the agent via `session_pools` (§3.5).
- `profiles`: not deleted; the API response / UI signals the same-named profile still exists.

### 3.3 memory_chunks scope handling

`memory_chunks.agent_id TEXT NOT NULL DEFAULT ''`. Private facts and soul rows (`kind` in `event`/`reflection`) belong to the agent; shared knowledge may carry the author's real `agent_id` but is used by others (confirmed on prod). So:
- `DELETE FROM memory_chunks WHERE agent_id=$1 AND scope='private'` — removes private facts + the agent's soul biography.
- `UPDATE memory_chunks SET agent_id='' WHERE agent_id=$1 AND scope='shared'` — anonymize: shared knowledge survives its author.

Note: the direct SQL bypasses the fail-closed `refuse_if_biography` guard. That is intended for deletion, but for soul-enabled agents the pre-delete backup (§4.2) is mandatory, consistent with `docs/runbooks/soul-quarantine.md`.

### 3.4 Workspace directory removal

After the DB work, best-effort delete of `workspace/agents/{name}/`. Reuse the existing path-guard in `agent/workspace.rs` (`dunce::canonicalize` of the root, parent-canonicalize against symlink escape — the pattern already used by `is_read_only`). Never fail the delete on an FS error (DB state is already gone); log the outcome. Skills the agent authored under `workspace/skills/*` are out-of-scope (runbook item §6) — they are not under `workspace/agents/`.

### 3.5 Session-scoped subagents

`api_delete_agent` currently stops only the main engine handle. Extend it to walk `AppState.session_pools` and kill any `LiveAgent` whose agent name matches the deleted agent (its cancellation token), so no background subagent task of a deleted agent survives in another session's pool.

### 3.6 purge_history

`DELETE /api/agents/{name}?purge_history=true` (default `false` = current behaviour). On `true`, within a transaction:
- `DELETE FROM sessions WHERE agent_id=$1` — FK `ON DELETE CASCADE` removes `messages`, `session_timeline`, `session_failures`, `session_shares`, `session_goals`, `session_todos`, `stream_jobs`, `pending_approvals` (verified prod FKs); `usage_log.session_id` is `SET NULL`.
- Separately (no covering CASCADE): `usage_log WHERE agent_id=$1`, `handler_jobs WHERE agent_name=$1` (session_id is `NOT NULL` but has NO FK → would orphan), `audit_log`/`audit_events`/`cron_runs WHERE agent_id=$1`.
- **Multi-agent decision (confirmed): delete the whole session** where the agent is `sessions.agent_id` (primary) — CASCADE takes co-participants' turns with it. Foreign sessions where the agent only participated (`messages.agent_id` nullable, agent not the session's `agent_id`) are NOT touched. The UI warns that co-participant turns in the agent's own sessions are lost.

### 3.7 Two-tier drift guard

- **(a) sqlx drift test** — `#[sqlx::test(migrations = "../../migrations")]`: query `information_schema.columns WHERE column_name IN ('agent_id','agent_name')` on the migration-built DB; assert every such table is in exactly one class. Catches future **migration** tables.
- **(b) prod-side check in `GET /api/doctor`** — the same information_schema query on the live DB, with an explicit allowlist for known out-of-migration tables (`eventbak_prune`); warn on any binding table not classified and not allowlisted. This is the ONLY layer that catches out-of-migration manual tables — the sqlx test literally cannot see `eventbak_prune` (it is not in migrations), and adding it to a code const would break test (a).
- **(c) rename-test fix** — rewrite `test_rename_mid_failure_leaves_pre_rename_state` to import the real constants instead of the stale hand-written 20-table list (closes audit v2 A7).

### 3.8 m090 migration (ordering-critical)

New migration `090_drop_deprecated_tables.sql` DROPs `pending_messages`, `video_jobs`, `file_scenarios`, `file_scenario_outcomes`. **Must run only AFTER** `pending_messages`/`video_jobs` are removed from `TABLES_WITH_AGENT_ID_NOT_NULL`/classification and the drift test/rename-test updated — otherwise rename/delete of any agent throws `relation does not exist` + rollback. Export the 4 `file_scenarios` + 11 `video_jobs` legacy rows before the drop (history-preserving, like the m089 pattern).

### 3.9 UI

Agent delete dialog (`ui/`): add a checkbox "Также удалить историю переписки и аудит" (default off) with an irreversibility warning that also notes co-participant turns in this agent's sessions will be lost. After a successful delete, if a same-named profile still exists, show a hint "профиль '{name}' существует — удалить отдельно?" linking to the profiles page.

## 4. Data flow & error handling

- **Ordering (hard requirement):** (1) classification consts + drift test (a) + doctor check (b) + rename-test fix (c) land first — they catch regressions before any destructive change; (2) remove DropRipe from consts, THEN m090; (3) delete-path expansion; (4) workspace-dir + subagent kill; (5) purge_history + UI.
- **workspace-dir:** best-effort, never fails the delete.
- **soul agents:** pre-delete backup of Ephemeral rows (min. `memory_chunks` event/reflection) is mandatory; others optional (runbook).
- **Transaction:** ephemeral deletes + purge deletes run in a single transaction with rollback on any error, mirroring the existing rename transaction.
- **One-shot Lana cleanup** (`tool_quality` 5 rows, profile, `lana-*.md` skills) is a manual runbook step, may run ahead of the code — closes audit v2 A6.

## 5. Testing

- `test_every_agent_binding_is_classified` — information_schema (agent_id + agent_name) ↔ classification, exhaustive (§3.7a).
- `test_ephemeral_delete_removes_all_state` — seed rows across all Ephemeral tables (both columns), delete, assert zero remain; assert a `shared` `memory_chunks` row survives with `agent_id=''`.
- `test_history_preserved_by_default` — default delete leaves `sessions`/`messages`/`usage_log`.
- `test_purge_history_cascades` — `purge_history=true` removes sessions and (via CASCADE) `session_timeline`/`session_failures`/`session_shares` with no explicit DELETE for them; `usage_log`/`handler_jobs` gone.
- `test_workspace_dir_removed_on_delete` + negative symlink-escape guard.
- rename-test rewritten on real constants (§3.7c).
- Doctor check: unit-test the classification-vs-schema diff logic (pure function over a table-name set).

## 6. Runbook (`docs/runbooks/agent-deletion.md`)

Document: the purge_history semantics + irreversibility; the mandatory soul-agent backup; the out-of-scope tails (author-created `workspace/skills/*`, `eventbak_prune` manual DROP after decay); the m090 ordering; and the 2026-07-17 Lana/Arty incident as precedent. Link from `docs/ARCHITECTURE.md`.

## 7. Open items (resolved in design, listed for the reviewer)

- Scope = full (1+2+3+4) — owner-confirmed.
- Multi-agent purge = whole session — owner-confirmed.
- memory_chunks = delete private + anonymize shared — design choice (§3.3).
- profiles = UI hint, not auto-delete — design choice (§3.9); rationale: profiles are shareable independent entities.
- `eventbak_prune` = doctor allowlist + manual runbook DROP — design choice (§3.7b), consistent with audit v2 §D.
