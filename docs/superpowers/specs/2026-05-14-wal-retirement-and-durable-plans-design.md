# WAL retirement and durable plans — design

**Date:** 2026-05-14
**Status:** **split** — Part A extracted to its own focused spec (being implemented); Part B is forward-looking design (on hold).
**Scope:** one design doc covering two causally linked changes.

> **Split note (2026-05-14):** Operator chose to implement Part A immediately
> (technical-debt cleanup, no behaviour change) and defer Part B until a
> concrete long-running-agent scenario emerges. Part A now lives as a focused,
> ready-to-implement spec at
> [`2026-05-14-session-timeline-rename-design.md`](2026-05-14-session-timeline-rename-design.md).
> This combined document is preserved as the reference design for Part B
> (durable plans / `plans` / `plan_steps` / ACTIVE PLAN block) and as the
> rationale narrative tying both halves together. When Part B is picked up,
> revive this doc — most of the design work is already done.

## Summary

Two changes shipped together because the second answers the question raised by the first.

**Part A — WAL retirement.** The codebase advertises Write-Ahead-Log crash recovery for sessions (CLAUDE.md, `session_wal.rs` doc-comment, m013 SQL comment). The implementation never matched the promise: only `LoopDetector` warm-up is replayed; full state recovery is not implemented and intentionally never will be. Honest naming and documentation: rename `session_events` to `session_timeline`, replace WAL framing with "chronological session timeline used for diagnostics, LoopDetector warm-up, and audit".

**Part B — durable plans.** Long-running agent work (hours-scale autonomous tasks, multi-step subagent plans, cron jobs with many calls, long interactive sessions) needs to survive process restarts. Full WAL-style replay is rejected: replaying non-idempotent tool calls is unsafe, in-flight LLM streams cannot be resumed by any major provider, and fresh-start semantics on session re-entry are an intentional design choice (CLAUDE.md). Instead: introduce explicit `plans` / `plan_steps` tables, a `plan` system-tool, and an "active plan" block injected into the system prompt on re-entry. Progress survives at step boundaries — never mid-step.

Acceptance: no production code or docs reference WAL or crash recovery; an agent can create a plan, the core can be restarted mid-plan, on next session entry the agent sees the unfinished plan and decides whether to continue, finish, or abort.

## Background

### What `session_events` actually does today

- Table created by [m013](../../../migrations/013_session_wal.sql); 86 references across 20 files; module file [`crates/opex-db/src/session_wal.rs`](../../../crates/opex-db/src/session_wal.rs).
- `log_event` / `log_event_tx` write rows during bootstrap (`running`), tool start/end, and finalize (`done`/`failed`/`interrupted`). Also bumps `sessions.activity_at` (debounced to ~10 s).
- `load_tool_events` warms the `LoopDetector` on session re-entry by replaying `tool_end` rows (BUG-026 fix) — this is the only "recovery" feature that exists.
- `prune_old_events_batched` runs hourly under `[cleanup]` config to keep the table bounded.

### What it does NOT do

- No replay of in-flight tool calls.
- No restoration of the LLM's intended next step.
- No reconstruction of session state beyond `LoopDetector` warm-up.
- The doc-comment at `session_wal.rs:1-6` promises "On crash recovery, this WAL is read to identify what was in-flight and reconstruct state cleanly — no synthetic '[interrupted]' messages are injected." This is aspirational, not implemented. CLAUDE.md line 298 acknowledges: "crash recovery via WAL replay is not yet implemented."

### Why full WAL recovery is rejected (not deferred)

1. **Single-tenant Raspberry Pi deployment** — crashes are rare and recovery cost is "user retries". Replay machinery would never pay back its complexity.
2. **Replaying tool calls is dangerous** — tools like `send_photo`, `code_exec`, channel actions, YAML POST/PUT have non-idempotent side effects. Replay would double-send, double-charge, etc.
3. **LLM streams are not resumable** — Anthropic / OpenAI / Google APIs do not expose offset-resume. Any "resume" boils down to a fresh call with the same context, which already works via `ExplicitResume`.
4. **Fresh-start semantics are intentional.** CLAUDE.md: "`NewSession` and `NewTurnAfterDone` get a fresh LoopDetector so prior tool errors don't pollute the next turn." Replay would directly conflict.

### Why durable plans is the right substitute

The real problem behind "WAL would be nice" is **long-running work losing progress on crash**. The correct fix is checkpointable plan state, not event replay:

- Checkpoint at logical step boundaries the agent itself defines.
- Replay nothing — completed steps are not re-run; in-flight steps are surfaced to the agent for an explicit decision.
- Side effects already persisted (workspace files, memory chunks, channel messages, DB rows) survive crash naturally; only the *intent to continue* is added.

This matches the existing `memory-worker` pattern: durable task state in DB, recovery by promoting stuck rows back to `pending`, idempotent task design.

## Part A — WAL retirement

### A.1 Migration m049: rename table

`migrations/049_rename_session_events_to_timeline.sql`:

```sql
ALTER TABLE IF EXISTS session_events RENAME TO session_timeline;
ALTER INDEX IF EXISTS idx_session_events_session RENAME TO idx_session_timeline_session;
ALTER INDEX IF EXISTS idx_session_events_type    RENAME TO idx_session_timeline_type;
```

- Idempotent via `IF EXISTS` so reruns are safe.
- Column names (`id`, `session_id`, `event_type`, `payload`, `created_at`) unchanged — they accurately describe their data.
- `event_type` *values* (`running`, `tool_start`, `tool_end`, `done`, `failed`, `interrupted`) unchanged.
- Old migrations m013 and m030 left untouched (history is append-only).
- `ALTER TABLE RENAME` is metadata-only in PostgreSQL — atomic, no data copy.

### A.2 Module rename

- Move `crates/opex-db/src/session_wal.rs` → `crates/opex-db/src/session_timeline.rs`.
- Update `crates/opex-db/src/lib.rs`: `pub mod session_wal;` → `pub mod session_timeline;`.
- Public function names stay (`log_event`, `log_event_tx`, `load_tool_events`, `prune_old_events_batched`, `WalToolEvent`). The `WalToolEvent` struct renames to `TimelineToolEvent` for consistency; callers are within the workspace and update accordingly.
- Module-level doc-comment is replaced with the new framing (see A.4).

### A.3 SQL string updates

All 86 references in 20 files; the SQL strings to update are concentrated in:

- `crates/opex-db/src/session_timeline.rs` (formerly `session_wal.rs`): 5 SQL string literals.
- `crates/opex-db/src/sessions.rs`: 4 SQL string literals + 1 test that drops the table (`DROP TABLE session_events` → `DROP TABLE session_timeline`).
- Test files: `tests/integration_session_events_cleanup.rs` → `tests/integration_session_timeline_cleanup.rs` (file rename + content).
- Test `tests/integration_dashboard_metrics.rs`: assertion strings.

### A.4 Documentation rewrites

The phrase-anchor that replaces "WAL" / "crash recovery":

> *"Session timeline — chronological log of session lifecycle events. Used for LoopDetector warm-up after restart (preserves loop-break decisions across crashes), diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery; completed work is preserved by persisted side effects, not event replay."*

Concrete edits:

| File | Lines | Change |
|---|---|---|
| `CLAUDE.md` | ~86, ~88 | replace "WAL `running`" / "WAL `done\|failed\|interrupted`" with "timeline `running`" / "timeline `done\|failed\|interrupted`" |
| `CLAUDE.md` | ~96 | replace "WAL records lifecycle events for diagnostics. LoopDetector resets..." with the phrase-anchor above |
| `CLAUDE.md` | ~294 | "session_events (WAL journal)" → "session_timeline (chronological lifecycle log)" |
| `CLAUDE.md` | ~298 | rewrite m013 paragraph using the phrase-anchor; drop "crash recovery via WAL replay is not yet implemented" |
| `docs/ARCHITECTURE.md` | 77, 339, 434, 736, 759 | analogous rewrites; metric name `session_events_table_size_bytes` → `session_timeline_table_size_bytes` |
| `docs/CONFIGURATION.md` | 270–275 | rename config keys (see A.5); replace "WAL-журнала" with "хронологического лога событий" |
| `docs/API.md` | 171 | JSON example field rename to `session_timeline_table_size_bytes` |
| `migrations/013_session_wal.sql` | header comment | replace "Used for crash recovery instead of injecting synthetic '[interrupted]' messages" with the phrase-anchor |

### A.5 Public API and config breaking changes

These are tracked as breaking changes; documented in release notes for the operator.

**Dashboard metric** (`GET /api/dashboard/metrics`):
- Field renamed: `session_events_table_size_bytes` → `session_timeline_table_size_bytes`.
- No alias for one release. UI does not consume this field (verified — no matches in `ui/src` for `session_events`).

**Config keys** in `opex.toml` `[cleanup]` section:
- `session_events_retention_days` → `session_timeline_retention_days`
- `session_events_batch_size` → `session_timeline_batch_size`
- Cron job key `session_events_cleanup_hourly` → `session_timeline_cleanup_hourly`.

**Startup PreCheck**: in `crates/opex-core/src/config/mod.rs::load`, before normal serde parsing, scan the raw TOML for any of the three old keys. If found, return a `ConfigError` with a clear actionable message instead of falling through to a bare serde "unknown field" error:

```
config error: [cleanup] key `session_events_retention_days` was renamed to
`session_timeline_retention_days` in this release. Update opex.toml.
```

This avoids the bare serde "unknown field" error and helps the single operator.

### A.6 Test impact

- Rename `tests/integration_session_events_cleanup.rs` → `tests/integration_session_timeline_cleanup.rs`; update SQL strings and CLI hint comments.
- `tests/integration_dashboard_metrics.rs`: assertion strings on metric name.
- All other tests pass without changes — they use `log_event_tx` / `load_tool_events` / etc., whose signatures are unchanged.
- Add a migration-integrity test: apply schema up to m048, insert rows in `session_events`, apply m049, verify table now named `session_timeline`, rows preserved, indexes renamed.

## Part B — durable plans

### B.1 Migration m050: tables

`migrations/050_plans.sql`:

```sql
CREATE TABLE plans (
    id            UUID PRIMARY KEY,
    session_id    UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    agent_id      TEXT NOT NULL,
    goal          TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'running'
                    CHECK (status IN ('running', 'done', 'failed', 'aborted')),
    abort_reason  TEXT,
    metadata      JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at  TIMESTAMPTZ
);

CREATE UNIQUE INDEX plans_one_running_per_session
    ON plans (session_id) WHERE status = 'running';

CREATE INDEX idx_plans_session       ON plans (session_id);
CREATE INDEX idx_plans_agent_running ON plans (agent_id) WHERE status = 'running';

CREATE TABLE plan_steps (
    id              UUID PRIMARY KEY,
    plan_id         UUID NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    ord             INTEGER NOT NULL,
    title           TEXT NOT NULL,
    description     TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                      CHECK (status IN ('pending', 'running', 'done', 'failed', 'skipped')),
    result_summary  TEXT,
    error           TEXT,
    heartbeat_at    TIMESTAMPTZ,
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX plan_steps_unique_ord ON plan_steps (plan_id, ord);
CREATE INDEX idx_plan_steps_plan_status   ON plan_steps (plan_id, status);
CREATE INDEX idx_plan_steps_running_heartbeat
    ON plan_steps (heartbeat_at) WHERE status = 'running';
```

**Invariants**

1. At most one `running` plan per session — enforced by `plans_one_running_per_session` partial unique index.
2. `(plan_id, ord)` unique — two steps cannot share position. Initial inserts use `ord = 10, 20, 30, ...` leaving gaps for later insertion.
3. Cascade delete via FK — when `sessions` rows are pruned by existing `cleanup_old_sessions`, plans and steps go with them. No separate cleanup job for MVP.
4. No DAG dependencies in MVP (linear by `ord`). Future `depends_on UUID[]` migration is cheap.
5. `heartbeat_at` is the source of truth for stall detection; agents update it during long steps.

### B.2 DB layer: `crates/opex-db/src/plans.rs`

New module. Functions all take `&PgPool` or `&mut Transaction`; each mutation runs in a single transaction and bumps `plans.updated_at`.

```rust
pub struct Plan { /* id, session_id, agent_id, goal, status, ... */ }
pub struct PlanStep { /* id, plan_id, ord, title, status, ... */ }
pub struct PlanWithSteps { pub plan: Plan, pub steps: Vec<PlanStep> }

pub async fn create_plan(db: &PgPool, session_id: Uuid, agent_id: &str, goal: &str, steps: &[NewStep]) -> Result<PlanWithSteps>;
pub async fn add_steps(db: &PgPool, plan_id: Uuid, steps: &[NewStep], after_ord: Option<i32>) -> Result<Vec<PlanStep>>;
pub async fn start_step(db: &PgPool, plan_id: Uuid, step_id: Uuid) -> Result<()>;
pub async fn heartbeat_step(db: &PgPool, plan_id: Uuid, step_id: Uuid) -> Result<()>;
pub async fn finish_step(db: &PgPool, plan_id: Uuid, step_id: Uuid, outcome: StepOutcome, summary: Option<&str>, error: Option<&str>) -> Result<()>;
pub async fn complete_plan(db: &PgPool, plan_id: Uuid, summary: Option<&str>) -> Result<()>;
pub async fn abort_plan(db: &PgPool, plan_id: Uuid, reason: &str) -> Result<()>;  // cascades step finish in same tx
pub async fn get_running_plan_for_session(db: &PgPool, session_id: Uuid) -> Result<Option<PlanWithSteps>>;
pub async fn list_plans_for_session(db: &PgPool, session_id: Uuid) -> Result<Vec<Plan>>;
pub async fn list_running_plans(db: &PgPool, agent_id: Option<&str>, limit: i64) -> Result<Vec<Plan>>;
```

Errors:
- `UniqueViolation` on `create_plan` when active plan exists → mapped to `PlanError::ActivePlanExists`.
- State transition violations (`finish_step` on non-running step) → checked in SQL `WHERE status = 'running'` → if 0 rows affected → `PlanError::BadState`.
- Cross-session `step_id` access → all mutations include `WHERE plan_id IN (SELECT id FROM plans WHERE session_id = $1)` guard.

### B.3 Tool: `plan` system-tool

New file `crates/opex-core/src/agent/pipeline/plan_handler.rs` (not appended to the already-1200-line `handlers.rs`).

Registered in `agent/tool_registry.rs` SystemToolRegistry alongside `agent`, `memory_*`, `workspace_*`. JSON schema in `pipeline/tool_defs.rs`. Visible to all agents (not `required_base`). Subagent denylist not extended — plans are useful in delegated work.

Multi-action dispatch (same pattern as `agent` tool). All actions are scoped to the **current session**: the tool handler reads `session_id` from `CommandContext`, finds the running plan via `get_running_plan_for_session`, and operates on that plan. The agent never has to pass `session_id` or `plan_id` itself — they are inferred.

| `action` | params | DB call |
|---|---|---|
| `create` | `{ goal: str, steps: [{title, description?}, ...] }` | `create_plan` |
| `add_steps` | `{ steps: [...], after_ord?: i32 }` | `add_steps` on the session's running plan |
| `step_start` | `{ step_id: Uuid }` | `start_step` (step must belong to the session's running plan) |
| `step_heartbeat` | `{ step_id: Uuid }` | `heartbeat_step` (debounced to 10 s — second call within 10 s is a no-op) |
| `step_finish` | `{ step_id, outcome: "done"\|"failed"\|"skipped", summary?, error? }` | `finish_step` |
| `complete` | `{ summary? }` | `complete_plan` on the session's running plan |
| `abort` | `{ reason: str }` | `abort_plan` on the session's running plan, with cascade |
| `status` | `{}` | `get_running_plan_for_session` — returns `{ active: bool, plan_id?, goal?, status?, steps[]? }` |

All errors come back as `tool error: <code>: <message>` text; no panics.

### B.4 Re-entry: active-plan block

Integration point: [`crates/opex-core/src/agent/pipeline/bootstrap.rs`](../../../crates/opex-core/src/agent/pipeline/bootstrap.rs) or [`crates/opex-core/src/agent/engine/context_builder.rs`](../../../crates/opex-core/src/agent/engine/context_builder.rs), whichever assembles the system-message list for the upcoming LLM call. After history load and session claim, before the first LLM call:

```rust
// Pseudocode — exact API depends on which module owns system-message assembly.
if let Some(active_plan) = plans::get_running_plan_for_session(&db, session_id).await? {
    let block = render_active_plan_block(&active_plan, now);
    // Append `block` to the list of system messages for this turn.
    // Do NOT persist to the `messages` table.
}
```

The block is **transient** — it lives in the system prompt for this turn only. It is not written to the `messages` table. Subsequent turns query fresh. The exact insertion API will be chosen during implementation by reading the current shape of `context_builder.rs`; the design constraint is "per-turn system content, not persisted".

#### When to inject

| Re-entry mode | Inject? |
|---|:-:|
| `NewSession` | no — no plan can exist |
| `NewTurnAfterDone` | **yes** — plan may have outlived previous turn |
| `ExplicitResume` (UI reopen) | **yes** |
| `ResumeRunning` (crash recovery) | **yes** — the primary case |
| Cron `handle_isolated_via_pipeline` | no (MVP) — cron uses `force_new_session=true` |
| Forked session (`POST /api/sessions/{id}/fork`) | no — fork starts plan-free |

Cost: one indexed SQL `SELECT` per bootstrap. Negligible.

#### Block format

Text block — readable in JSON logs, identical behaviour across providers, easy snapshot-testable:

```
=== ACTIVE PLAN ===
Goal: Index repository X
Plan ID: 7f2e...  ·  Status: running  ·  Started: 47 min ago

Progress: 1 done · 1 running · 2 pending · 0 failed

Steps:
✓ [10] Clone repo — done (3 min): "cloned to /workspace/repo-x, 1240 files"
▶ [20] Walk file tree — running (started 41 min ago, last heartbeat 38 min ago — STALLED)
○ [30] Embed files — pending
○ [40] Write summary — pending

This plan was active when the session paused or crashed. Decide how to proceed:
  • Continue step 20 → call `plan.step_heartbeat({step_id})` then resume your work.
  • Mark step 20 finished → `plan.step_finish({step_id, outcome:"done"|"failed", summary|error})`.
  • Abandon the plan → `plan.abort({reason})`.
  • Inspect details → `plan.status()`.

Do NOT silently ignore this block. Either act on the plan or explicitly abort it.
===================
```

Renderer is a pure function `render_active_plan_block(plan: &PlanWithSteps, now: DateTime<Utc>) -> String` in `agent/plan_render.rs` (or co-located in `plan_handler.rs`). Snapshot-tested per scenario.

#### Scaffold convention

To make new agents reliable consumers of the block, add a short "Plan handling" section to:

- `crates/opex-core/scaffold/base/SOUL.md`
- `crates/opex-core/scaffold/regular/SOUL.md`

Content: "If you see a `=== ACTIVE PLAN ===` block in the system prompt, it is not advisory — it is a commitment. Read the steps, decide what to do, and act via the `plan` tool."

Existing agents' SOUL.md files are **not** modified (they are operator-owned). The convention is documented in CLAUDE.md and the operator decides whether to backport.

### B.5 Watchdog and observability

#### Stuck-step detection

Add to existing [opex-watchdog](../../../crates/opex-watchdog/) a check that runs every `watchdog_interval_minutes` (default 5):

```sql
SELECT ps.id, ps.plan_id, ps.title, p.session_id, p.agent_id,
       COALESCE(ps.heartbeat_at, ps.started_at) AS last_signal
FROM plan_steps ps
JOIN plans p ON p.id = ps.plan_id
WHERE ps.status = 'running'
  AND COALESCE(ps.heartbeat_at, ps.started_at) < NOW() - make_interval(mins => $1);
```

Uses `idx_plan_steps_running_heartbeat`. For each row:

1. Watchdog **does not** change step status — it cannot tell whether the agent is genuinely stuck or just thinking long.
2. It creates a notification via the existing `notify(...)` API: `notification_type = 'plan_step_stalled'`, body identifies plan/step/duration.
3. If the parent session is itself inactive (`activity_at` stale AND no `running` process), watchdog also fires the existing alert channel (operator gets pinged).

#### Abandoned-plan auto-abort

A safety net for plans whose session has gone terminal but the plan was left dangling:

```sql
UPDATE plans
SET status = 'aborted',
    abort_reason = 'auto-aborted: session terminal and plan idle >24h',
    completed_at = NOW(),
    updated_at = NOW()
WHERE status = 'running'
  AND updated_at < NOW() - INTERVAL '24 hours'
  AND session_id IN (
      SELECT id FROM sessions
      WHERE run_status IN ('done','failed','interrupted','timeout','cancelled')
  );
-- cascade: matching plan_steps with status='running' get
-- status='failed', error='plan auto-aborted' in the same transaction.
```

Logged at `tracing::info`. No operator alert (this is normal cleanup).

#### Config additions

New `[plans]` section in `opex.toml`:

```toml
[plans]
step_stall_timeout_minutes  = 30   # threshold for stalled-step notification
abandoned_plan_timeout_hours = 24  # threshold for auto-abort
watchdog_interval_minutes   = 5    # how often the watchdog scans
```

#### API endpoints

Read-only (Bearer-auth):

| Method | Path | Returns |
|---|---|---|
| `GET` | `/api/sessions/{id}/plan` | active plan + steps; 404 if none |
| `GET` | `/api/sessions/{id}/plans` | all plans for the session (history + current) |
| `GET` | `/api/plans?status=running&agent_id=X` | list, limit 100 |
| `POST` | `/api/plans/{id}/abort` | external abort (UI "Stop plan" button). Body: `{reason}`. Calls the same `plans::abort_plan` as the tool action — single source of truth. |

#### Metrics

Add to `DashboardMetrics` (`crates/opex-core/src/metrics.rs`) and the JSON response of `/api/dashboard/metrics`:

- `plans_running_total: u64`
- `plans_completed_24h: u64`
- `plans_aborted_24h: u64`
- `plan_steps_stalled_total: u64`
- `plans_table_size_bytes: u64`
- `plan_steps_table_size_bytes: u64`

Documented in `docs/API.md`.

#### Tracing

- Every `plans::*` mutation logs `tracing::info!(plan_id=%id, action=%action, ...)`.
- Block injection logs `tracing::debug!(session_id, plan_id, "plan context injected")`.
- Stall detection logs `tracing::warn!(plan_id, step_id, idle_min, "stalled plan step")`.
- Auto-abort logs `tracing::info!(plan_id, "abandoned plan auto-aborted")`.

### B.6 Heartbeat debounce

`plan.step_heartbeat` is rate-limited inside the tool handler the same way `session_timeline::log_event_tx` debounces `activity_at` (10 s minimum interval per step). Even if the agent calls it every second, only one DB update fires per 10 s. The debounce state lives in process memory (`DashMap<Uuid, Instant>`) — survives only within a process; restart is fine because the next call after restart unconditionally writes.

## Implementation order

A natural breakdown into independent commits:

1. **m049 + module rename + SQL string updates** (Part A backend).
2. **PreCheck + config key renames + doc rewrites** (Part A operator-facing).
3. **m050 + `plans.rs` DB module** (Part B foundation).
4. **`plan` tool handler + tool_defs schema + tool_registry registration** (Part B tool).
5. **Re-entry block injection in bootstrap** (Part B re-entry).
6. **Watchdog stall check + auto-abort + `[plans]` config + metrics + API endpoints** (Part B observability).
7. **Tests across the above + acceptance criteria check**.

Steps 1–2 are independent of 3–7 and could land first as their own PR.

## Acceptance criteria

1. No production source or doc file (excluding the historical `migrations/` directory) contains the phrases "session WAL", "WAL replay", or "crash recovery via WAL". `grep -wr 'WAL' crates/ docs/ CLAUDE.md --exclude-dir=migrations` returns no matches related to the session timeline concept.
2. `grep -wr "session_events" crates/ docs/ CLAUDE.md --exclude-dir=migrations` returns no matches.
3. CLAUDE.md sections covering bootstrap/finalize and m013 use the new phrase-anchor.
4. `opex.toml` examples and `docs/CONFIGURATION.md` reference the new `session_timeline_*` keys and the new `[plans]` section.
5. End-to-end test: agent creates a plan with 4 steps → starts step 1 → core restart → on next bootstrap the agent's first LLM input contains `=== ACTIVE PLAN ===` and step 1 marked running → agent finishes the plan via `plan.complete` → DB shows `status='done'`.
6. `make test-db` is green.
7. Watchdog catches an abandoned plan (terminal session + plan idle >24 h) and auto-aborts it.
8. Stalled-step notification fires for a `running` step with `heartbeat_at` older than `step_stall_timeout_minutes`.

## Test plan

### Part A

- Migration test: apply schema up to m048, insert into `session_events`, apply m049, verify table is `session_timeline`, rows preserved, indexes renamed. Idempotency on re-run.
- PreCheck test: TOML with old `session_events_retention_days` fails startup with the specific error message; new keys succeed.
- Dashboard metrics test (`integration_dashboard_metrics.rs`): field is `session_timeline_table_size_bytes`; old field absent.

### Part B

**Unit (`plans.rs`)**
- `create_plan` fails with `ActivePlanExists` when a running plan already exists in the session.
- `add_steps` auto-assigns gaps; with `after_ord` inserts a step at the midpoint of the gap; rejects when the gap is too narrow to subdivide.
- `start_step` / `finish_step` / `heartbeat_step` reject invalid state transitions via 0-row-affected detection.
- `abort_plan` cascades step-finish in the same transaction; partial failure rolls back all changes.
- `get_running_plan_for_session` plan inspection (`EXPLAIN`) shows use of the partial unique index.

**Snapshot (`plan_render`)**
- Fresh plan (all pending), mixed plan (done/running/pending), stalled-running step (>timeout), all-done-not-completed, single-step plan.
- Timing-string formats: "3 min ago", "47 min ago", "STALLED — last heartbeat 38 min ago".

**Integration**
- `integration_plan_tool.rs` — full lifecycle: create → step_start → heartbeat × N → step_finish(done) → complete. DB-state assertions after each action.
- `integration_plan_reentry.rs` — create a session with a running plan; flip `sessions.run_status='running'` without a live process; run bootstrap; the system prompt of the next LLM call contains `=== ACTIVE PLAN ===` and the step titles. Negative case: forked session has no block.
- `integration_plan_fork.rs` — `POST /api/sessions/{id}/fork`: parent retains the plan; fork has none.
- `integration_plan_api.rs` — read endpoints (200/404), `POST /api/plans/{id}/abort` cascade. Re-abort on already-aborted returns 200 (idempotent for UI friendliness).
- `integration_plan_watchdog.rs` — stalled step → notification row created, step status unchanged. Abandoned plan + terminal session → auto-abort fires with cascaded step-failures.

**Doc test**
- Inline example in `render_active_plan_block` docstring is executed by `cargo test --doc`.

## Out of scope (deferred to future specs)

| Not in this spec | Future home |
|---|---|
| Idempotency keys for tools | Separate "tool idempotency framework" spec, after a real duplicate incident |
| `tool_call ↔ plan_step` linkage / drilldown | When detailed UI needs it |
| DAG dependencies between steps (`depends_on`) | Extension migration when an agent needs parallel steps |
| Auto-resume of mid-step tool execution | **Explicitly never** — this spec closes the door |
| Plan Timeline UI page | Frontend spec after backend bakes in production |
| Cron jobs owning a plan across multiple firings | After a concrete use case emerges |
| Quota / limits (max plans per session, max steps per plan) | Config-only addition if we hit it |
| Subagent inheritance via `metadata.parent_plan_id` | When subagent pool needs hierarchy |
| Prometheus exporter for plan metrics | After OTEL feature expansion |

## Known risks and mitigations

1. **Agents may ignore the block.** Mitigation: imperative language ("Do NOT silently ignore"), SOUL.md scaffold convention, snapshot tests on common provider behaviour, doc-of-record in CLAUDE.md.
2. **One running plan per session is sometimes restrictive.** Mitigation: explicit MVP invariant; the constraint is a partial unique index easy to relax in a future migration if a real parallel-plans use case emerges.
3. **Heartbeat write amplification.** Mitigation: 10 s debounce in the tool handler.
4. **Existing sessions unaffected.** Plans table starts empty; bootstrap query returns `None`; behaviour identical to today until the first plan is created.
