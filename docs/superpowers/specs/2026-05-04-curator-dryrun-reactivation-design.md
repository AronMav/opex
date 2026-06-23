# Curator Dry-Run & Skill Reactivation — Design Spec

**Date:** 2026-05-04
**Status:** Approved

## Problem

1. **No dry-run:** `POST /api/curator/run` always executes destructively. Operators
   cannot preview what the Curator would do before it runs.
2. **No reactivation:** Once a skill is archived (by Phase 1 or Phase 3), it never
   returns to active automatically. An agent explicitly loading an archived skill, or
   a user message matching its triggers, silently ignores the skill.

## Goal

1. Add `POST /api/curator/preview` — runs all three Curator phases in simulation
   mode without writing any changes. Returns a `[DRY-RUN]`-annotated report
   identical in structure to a real run.
2. Add `reactivate_skill()` — called from two points: trigger match in
   `context_builder.rs` and explicit `skill_use(action="load")` in `handlers.rs`.
   Atomically restores state to active and updates `last_used_at`.

---

## Part 1 — Dry-Run

### Architecture

Add `dry_run: bool` parameter to `run_curator()` and propagate it to each phase.
At each write site, gate the write behind `if !dry_run`. The same function signatures,
same data paths, same result structs — no code duplication.

### `run_curator()` signature change

```rust
pub async fn run_curator(
    db: &PgPool,
    cfg: &CuratorConfig,
    agents: Arc<AgentCore>,
    workspace_dir: &str,
    dry_run: bool,          // NEW
) -> anyhow::Result<CuratorRunSummary>
```

### Phase 1 — dry-run behaviour

`phase_transitions::run()` gains `dry_run: bool`.

When `dry_run=true`:
- Computes `decide_transition()` as normal
- Skips `write_skill()` and `save_version()`
- Prefixes each log line with `[DRY-RUN]`
- Returns same `TransitionResult { transitions, log }`

### Phase 2 — dry-run behaviour

`phase_repairs::run()` gains `dry_run: bool`.

When `dry_run=true`:
- Does not launch any agent repair sessions
- Reads `pending_skill_repairs WHERE status='pending'` from DB (count only)
- Returns `RepairResult { applied: 0, log: ["[DRY-RUN] N pending repairs (not executed)"] }`

### Phase 3 — dry-run behaviour

`phase_consolidation::run()` gains `dry_run: bool`.

When `dry_run=true`:
- Analyst pass: runs normally (reads skills, calls LLM for proposals) — proposals are informative
- Verifier pass: runs normally (validates proposals deterministically)
- Executor pass: **skipped entirely**
- Each would-be action prefixed with `[DRY-RUN]` in log

### DB schema change

Add `dry_run BOOLEAN NOT NULL DEFAULT false` to `curator_runs` table.

Migration: `ALTER TABLE curator_runs ADD COLUMN dry_run BOOLEAN NOT NULL DEFAULT false;`

The `finish_run()` helper that writes the final row needs to accept and persist this field.
`dry_run: bool` is captured in the `tokio::spawn` closure from the HTTP endpoint handler
(same scope where `run_id` is created) and passed into `run_curator()` and then
forwarded to `finish_run()`.

### New HTTP endpoint

```
POST /api/curator/preview
```

No request body. Same concurrency guard as `POST /api/curator/run` — returns 409 if
either a real run or a preview is already in progress.

Response:
```json
{"run_id": "uuid"}
```

Status codes: 202 (accepted, running async), 409 (conflict), 500 (DB error).

Progress and result: poll existing `GET /api/curator/runs/{id}`. The `report_md`
field will contain `[DRY-RUN]` prefixed lines. The `dry_run` field on the run row
distinguishes preview from real runs.

### What is NOT simulated

- Phase 2 agent sessions: LLM repair calls are never made in dry-run — only the
  count of pending repairs is reported.
- Phase 3 Analyst LLM call: this IS made (it reads skill content to generate
  proposals). The proposals are shown but not applied.

---

## Part 2 — Skill Reactivation

### New function `reactivate_skill()`

**File:** `crates/opex-core/src/skills/mod.rs`

```rust
/// Restore an archived skill to active state and update last_used_at.
/// No-op if the skill is not archived. Fire-and-forget safe (all errors → warn).
pub async fn reactivate_skill(
    workspace_dir: &str,
    name: &str,
    db: &sqlx::PgPool,
    agent_name: &str,
    now_iso: &str,
)
```

**Algorithm:**
1. Read `{workspace_dir}/skills/{name}.md`, parse via `SkillDef::parse()`
2. If `state != Archived` → return immediately (noop)
3. Build new `SkillFrontmatter` copying all fields from `skill_def.meta`, overriding
   only `state: Active` and `last_used_at: Some(now_iso.to_string())`
4. Call `write_skill(workspace_dir, name, &frontmatter, &skill_def.instructions)`
   — instructions are preserved from the parsed file, not overwritten
5. Insert `curator_decisions` row: `action="reactivated"`, `reason="re-used by {agent_name}"`
6. Log `info!(skill = %name, agent = %agent_name, "skill reactivated from archived")`
7. All errors → `tracing::warn!`, never propagate

### Trigger point 1 — `context_builder.rs`

**Current:** Trigger matching only looks at `Active` and `Stale` skills
(`load_skills()` filters archived). Archived skills never match triggers.

**Change:**
- Load skills including archived for trigger check only: use a new
  `load_skills_all_states()` (or pass an `include_archived: bool` flag to
  `load_skills()`).
- Check triggers against all skills including archived.
- If an archived skill matches:
  - Spawn `reactivate_skill()` (fire-and-forget, same `tokio::spawn` pattern as
    `update_skill_last_used_if_stale`).
  - Inject the `skill_use(action="load", name="...")` hint into the system prompt
    (same as for active/stale skills).
- If an active/stale skill matches: behaviour unchanged.

### Trigger point 2 — `handle_skill_use(action="load")` in `handlers.rs`

**Current:** `handle_skill_use` calls `load_skills()` which filters archived.
Explicit `skill_use(action="load", name="archived-skill")` silently returns
`"Skill not found"`.

**Change:**
- When action is `"load"` and the skill is not found in the filtered list, do a
  second lookup: `load_skills_all_states()` to check if it exists but is archived.
- If found and archived:
  - Call `reactivate_skill()` (fire-and-forget).
  - Return the skill instructions to the agent (same format as active skill).
  - Add note to returned text: `*(This skill was archived and has been reactivated.)*`
- If not found in either lookup: existing "Skill not found" response unchanged.

**Dispatch-level interception for `"load"` action:** Following the same pattern used
for `action="capture"` (Task 3 of the prior feature), intercept `action="load"` in
`engine_dispatch.rs` before calling `handle_skill_use`. When `action="load"`:

1. Call the existing `handle_skill_use` path first to get the result.
2. If the result is `"Skill '...' not found"`, do a second lookup via
   `load_skills_all_states()` in-place at the dispatch site.
3. If found and archived: spawn `reactivate_skill(workspace_dir, name, &self.cfg().db, &agent_name, &now_iso)` fire-and-forget, return skill instructions with reactivation note.

`handle_skill_use` signature is unchanged. All reactivation logic lives in
`engine_dispatch.rs` where `self.cfg().db` and `self.cfg().agent.name` are already
available.

### `load_skills_all_states()`

**File:** `crates/opex-core/src/skills/mod.rs`

New function that returns all skills including archived. Reuses the existing file
glob + parse logic from `load_skills()`, just removes the archived filter.

---

## File Map

| Action | Path | Responsibility |
| --- | --- | --- |
| Modify | `crates/opex-core/src/curator/mod.rs` | Add `dry_run: bool` to `run_curator()` |
| Modify | `crates/opex-core/src/curator/phase_transitions.rs` | Add `dry_run` gate around writes |
| Modify | `crates/opex-core/src/curator/phase_repairs.rs` | Add `dry_run` reporting path |
| Modify | `crates/opex-core/src/curator/phase_consolidation.rs` | Add `dry_run` gate around executor |
| Modify | `crates/opex-core/src/gateway/handlers/curator.rs` | Add `POST /api/curator/preview`, `dry_run` field in DB row |
| New migration | `migrations/` | `ALTER TABLE curator_runs ADD COLUMN dry_run BOOLEAN NOT NULL DEFAULT false` |
| Modify | `crates/opex-core/src/skills/mod.rs` | Add `reactivate_skill()` + `load_skills_all_states()` |
| Modify | `crates/opex-core/src/agent/context_builder.rs` | Trigger match includes archived; spawn reactivation |
| Modify | `crates/opex-core/src/agent/engine_dispatch.rs` | Intercept `action="load"` for archived reactivation (dispatch-level, no handler sig change) |

---

## Testing

| Test | Assertion |
| --- | --- |
| `run_curator(dry_run=true)` with stale skill | TransitionResult.log has `[DRY-RUN]`, skill file unchanged |
| `POST /api/curator/preview` | Returns 202 with run_id; run row has `dry_run=true` |
| `GET /api/curator/runs/{id}` after preview | report_md contains `[DRY-RUN]` lines |
| `reactivate_skill()` on archived skill | State → active, last_used_at set, curator_decisions row inserted |
| `reactivate_skill()` on active skill | Noop — file unchanged |
| Trigger match with archived skill | Skill reactivated, hint injected into system prompt |
| `skill_use(action="load")` on archived | Returns instructions + reactivation note, skill reactivated |
| `skill_use(action="load")` on nonexistent | "Skill not found" unchanged |
| Concurrent `POST /api/curator/preview` | 409 if run already in progress |

---

## Error Handling

| Scenario | Behaviour |
| --- | --- |
| `reactivate_skill()` write fails | `warn!`, noop — skill stays archived |
| Phase 3 Analyst LLM timeout in preview | Preview report shows timeout, other phases still run |
| `load_skills_all_states()` parse error | Skip unparseable file (same as `load_skills()`) |
| `dry_run` migration column missing | `finish_run()` logs error, run still completes |

---

## Out of Scope

- UI changes for dry-run report display (existing run viewer shows `report_md` as-is)
- Reactivation of config/skills/ (base-agent-only, intentionally excluded)
- Manual reactivation via `PATCH /api/skills/{name}/state` — already possible via PUT
- Reactivation count in Curator Phase 1 summary (reactivation is external to Phase 1)
