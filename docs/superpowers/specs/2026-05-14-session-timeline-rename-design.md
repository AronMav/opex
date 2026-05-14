# Session timeline rename (WAL retirement) — design

**Date:** 2026-05-14
**Status:** approved, ready for `writing-plans`
**Scope:** documentation honesty + table/module rename. No runtime behaviour change.

## What this spec is

Tightly scoped cleanup: rename the `session_events` table to `session_timeline`, rename the Rust module from `session_wal` to `session_timeline`, and rewrite all docs that frame this table as a "Write-Ahead Log" with "crash recovery". The new framing accurately describes what the table actually does today.

This is the deferred companion to [2026-05-14-wal-retirement-and-durable-plans-design.md](2026-05-14-wal-retirement-and-durable-plans-design.md). Part B (durable plans) from that doc is intentionally **not** in this spec — it will be picked up when a concrete long-running-agent use case lands. This spec only covers Part A.

## Why

### What `session_events` actually does today

- Table created by [m013](../../../migrations/013_session_wal.sql); 86 references across 20 files.
- Module file [`crates/hydeclaw-db/src/session_wal.rs`](../../../crates/hydeclaw-db/src/session_wal.rs) — 243 LoC.
- `log_event` / `log_event_tx` write rows during bootstrap (`running`), tool start/end, and finalize (`done` / `failed` / `interrupted`). Also bumps `sessions.activity_at` (debounced to ~10 s).
- `load_tool_events` warms the `LoopDetector` on session re-entry by replaying `tool_end` rows (BUG-026 fix). **This is the only "recovery" feature.**
- `prune_old_events_batched` runs hourly under `[cleanup]` config to keep the table bounded.

### What it does NOT do, despite the docs

- No replay of in-flight tool calls.
- No restoration of LLM intent.
- No reconstruction of session state beyond the `LoopDetector` warm-up.

The doc-comment at `session_wal.rs:1-6` promises *"On crash recovery, this WAL is read to identify what was in-flight and reconstruct state cleanly — no synthetic '[interrupted]' messages are injected."* CLAUDE.md line 298 quietly contradicts this: *"crash recovery via WAL replay is not yet implemented."*

### Why we're not implementing the missing replay

Decision recorded in [the companion design doc](2026-05-14-wal-retirement-and-durable-plans-design.md) and in the conversation that led to it: single-tenant Pi deployment, non-idempotent tools make replay dangerous, LLM providers do not expose stream resume, and fresh-start semantics are an intentional choice (CLAUDE.md). The right substitute (durable plans) is a separate forward-looking spec.

### Value delivered by this rename

1. New code authors stop searching for non-existent replay logic.
2. The phrase "session WAL" is removed from the security/ops narrative — we no longer advertise a recovery feature we can't deliver.
3. The module name finally describes what the code does.

No runtime behaviour change. No bug fix. Pure technical-debt cleanup.

## Concrete changes

### 1. Migration m049 — rename table and indexes

`migrations/049_rename_session_events_to_timeline.sql`:

```sql
ALTER TABLE  IF EXISTS session_events                RENAME TO session_timeline;
ALTER INDEX  IF EXISTS idx_session_events_session    RENAME TO idx_session_timeline_session;
ALTER INDEX  IF EXISTS idx_session_events_type       RENAME TO idx_session_timeline_type;
```

- Idempotent via `IF EXISTS` — safe to re-run.
- Column names (`id`, `session_id`, `event_type`, `payload`, `created_at`) unchanged — they accurately describe their data.
- `event_type` *values* (`running`, `tool_start`, `tool_end`, `done`, `failed`, `interrupted`) unchanged.
- Old migrations m013 and m030 stay untouched — history is append-only.
- `ALTER TABLE RENAME` is metadata-only in PostgreSQL — atomic, no data copy.

### 2. Module rename

- Move `crates/hydeclaw-db/src/session_wal.rs` → `crates/hydeclaw-db/src/session_timeline.rs`.
- Update `crates/hydeclaw-db/src/lib.rs`: `pub mod session_wal;` → `pub mod session_timeline;`.
- Module-level doc-comment is replaced with the new framing (see §4).
- Function signatures unchanged (`log_event`, `log_event_tx`, `load_tool_events`, `prune_old_events_batched`).
- Rename type `WalToolEvent` → `TimelineToolEvent` for consistency. All callers are inside the workspace.

### 3. SQL string and import updates

All 86 references in 20 files. Concentrated locations:

- `crates/hydeclaw-db/src/session_timeline.rs` (formerly `session_wal.rs`): 5 SQL string literals.
- `crates/hydeclaw-db/src/sessions.rs`: 4 SQL string literals + 1 test that drops the table (`DROP TABLE session_events` → `DROP TABLE session_timeline`).
- All `use hydeclaw_db::session_wal` → `use hydeclaw_db::session_timeline` (Rust import paths).
- Tracing fields and inline comments referencing "WAL" are updated to "timeline".

### 4. Documentation rewrites

The phrase-anchor that replaces "WAL" / "crash recovery":

> *"Session timeline — chronological log of session lifecycle events. Used for LoopDetector warm-up after restart (preserves loop-break decisions across crashes), diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery; completed work is preserved by persisted side effects, not event replay."*

Concrete edits:

| File | Lines | Change |
|---|---|---|
| `CLAUDE.md` | ~86, ~88 | replace `WAL running` / `WAL done|failed|interrupted` with `timeline running` / `timeline done|failed|interrupted` |
| `CLAUDE.md` | ~96 | replace WAL/LoopDetector paragraph with the phrase-anchor |
| `CLAUDE.md` | ~294 | `session_events (WAL journal)` → `session_timeline (chronological lifecycle log)` |
| `CLAUDE.md` | ~298 | rewrite m013 paragraph using the phrase-anchor; drop "crash recovery via WAL replay is not yet implemented" |
| `docs/ARCHITECTURE.md` | 77, 339, 434, 736, 759 | analogous rewrites; metric name `session_events_table_size_bytes` → `session_timeline_table_size_bytes` |
| `docs/CONFIGURATION.md` | 270–275 | rename config keys (see §5); replace "WAL-журнала" with "хронологического лога событий" |
| `docs/API.md` | 171 | JSON example field rename to `session_timeline_table_size_bytes` |
| `migrations/013_session_wal.sql` | header comment | replace "Used for crash recovery instead of injecting synthetic '[interrupted]' messages" with the phrase-anchor |
| `crates/hydeclaw-db/src/session_timeline.rs` | header doc-comment | rewrite using the phrase-anchor |

The file `migrations/013_session_wal.sql` itself is **not renamed** (would change the migration's identity); only its in-file SQL comments are edited.

### 5. Public-API and config breaking changes

Operator-facing renames in one shot, no aliases. Documented in release notes.

**Dashboard metric** (`GET /api/dashboard/metrics`):
- Field renamed: `session_events_table_size_bytes` → `session_timeline_table_size_bytes`.
- UI does not consume this field (verified: no matches in `ui/src` for `session_events`).

**Config keys** in `hydeclaw.toml` `[cleanup]` section:
- `session_events_retention_days` → `session_timeline_retention_days`
- `session_events_batch_size` → `session_timeline_batch_size`
- Cron job key `session_events_cleanup_hourly` → `session_timeline_cleanup_hourly`.

**Startup PreCheck**: in `crates/hydeclaw-core/src/config/mod.rs::load`, before normal serde parsing, scan the raw TOML for any of the three old keys. If found, return a `ConfigError` with a clear actionable message:

```text
config error: [cleanup] key `session_events_retention_days` was renamed
to `session_timeline_retention_days` in this release. Update hydeclaw.toml.
```

This replaces the bare serde "unknown field" error and helps the single-operator deployment.

## Test plan

- **Migration integrity**: apply schema up to m048, insert sample rows into `session_events`, apply m049, verify table is now `session_timeline`, rows preserved, indexes renamed. Re-run m049 — second pass is a no-op.
- **PreCheck**: a TOML with the old `session_events_*` keys fails startup with the specific error message; the new keys succeed.
- **Dashboard metrics** ([integration_dashboard_metrics.rs](../../../crates/hydeclaw-core/tests/integration_dashboard_metrics.rs)): JSON response includes `session_timeline_table_size_bytes`; the old field is absent.
- **Rename of test file**: `tests/integration_session_events_cleanup.rs` → `tests/integration_session_timeline_cleanup.rs`; content and SQL updated. Existing test cases pass unchanged.
- **`make test-db` is green** after all changes.

## Acceptance criteria

1. No production source or doc file (excluding the historical `migrations/` directory) contains the phrases "session WAL", "WAL replay", or "crash recovery via WAL". `grep -wr 'WAL' crates/ docs/ CLAUDE.md --exclude-dir=migrations` returns no matches related to the session timeline concept.
2. `grep -wr "session_events" crates/ docs/ CLAUDE.md --exclude-dir=migrations` returns no matches.
3. `cargo build --workspace` and `cargo test --workspace` (with DATABASE_URL set) are green.
4. Startup with a `hydeclaw.toml` that still contains the old `session_events_retention_days` key prints the PreCheck error and exits non-zero, **not** a serde parse error.
5. Startup with a `hydeclaw.toml` that uses the new `session_timeline_*` keys succeeds and the hourly cleanup cron runs.
6. `/api/dashboard/metrics` response contains `session_timeline_table_size_bytes` and does not contain `session_events_table_size_bytes`.

## Implementation order

A natural breakdown into independent commits:

1. **Migration m049 + module rename + SQL string updates** (database and Rust side).
2. **Public API + config key renames + PreCheck** (operator-facing rename).
3. **Documentation rewrites** (CLAUDE.md, docs/, in-file SQL comments).
4. **Test file rename + green CI**.

Each step is independently reviewable. The whole change is one or two PRs.

## Out of scope

- **Part B from the combined design doc** — `plans` / `plan_steps` tables, `plan` system-tool, ACTIVE PLAN re-entry block. Remains a forward-looking design until a concrete long-running-agent scenario emerges. See [`2026-05-14-wal-retirement-and-durable-plans-design.md`](2026-05-14-wal-retirement-and-durable-plans-design.md).
- **Backward-compatible aliasing** of the old metric name or config keys — explicitly rejected for the single-tenant deployment model.
- **Renaming the m013 migration file itself** — would alter migration identity; only in-file comments change.

## Known risks

1. **Operator surprise on first deploy.** Mitigated by the PreCheck error message naming the new key and the release notes.
2. **External tooling reading the old metric name.** Verified: the UI doesn't consume it. If any operator script does, the release notes call out the rename.
3. **Mid-flight rename collision.** If another change lands first in `session_wal.rs`, rebase resolves cleanly because all changes are textual.
