# Session timeline rename (WAL retirement) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename `session_events` table to `session_timeline`, rename the `session_wal` Rust module to `session_timeline`, rename the dashboard metric and config keys, and rewrite all documentation that frames this table as a "Write-Ahead Log" with "crash recovery". Pure technical-debt cleanup — no runtime behaviour change.

**Architecture:** Single-shot rename, no aliasing. PostgreSQL `ALTER TABLE RENAME` is metadata-only and atomic. Public API (one dashboard metric field) and config keys (three keys in `[cleanup]`) are renamed in the same release with a Startup PreCheck that produces a clear error if the operator still uses the old config keys. Documentation is rewritten using the phrase-anchor: *"Session timeline — chronological log of session lifecycle events. Used for LoopDetector warm-up after restart, diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery."*

**Tech Stack:** Rust 2024 edition, sqlx 0.8 (PostgreSQL), Axum, anyhow, tracing. Migrations in `migrations/*.sql`. Tests via `cargo test` and `cargo test --test <name>` with `DATABASE_URL` set for sqlx::test.

**Spec reference:** [docs/superpowers/specs/2026-05-14-session-timeline-rename-design.md](../specs/2026-05-14-session-timeline-rename-design.md)

**Commit policy:** Plan approval implies authorization for the `git commit` steps below — the executor SHOULD NOT prompt before each commit but MUST prompt before any `git push`, `gh pr create`, or destructive git operation (reset, force-push, branch delete). This aligns with CLAUDE.md "commit only when requested" by treating plan approval as the request.

**Pre-flight:** Confirm `DATABASE_URL` is exported (so `cargo test` runs `#[sqlx::test]` tests). Without it, the migration test in Task 1 is silently skipped — bad for TDD. Run `echo $DATABASE_URL` first; if empty, start the test DB via `make test-db` in a separate terminal or set the variable.

---

## Task 1: Migration m049 — table and index rename + integrity test

**Files:**

- Create: `migrations/049_rename_session_events_to_timeline.sql`
- Modify: `crates/hydeclaw-db/src/session_wal.rs` (add an `#[sqlx::test]` integrity test at the bottom of the existing `#[cfg(test)] mod tests` block; we will rename the file in Task 2, but adding the test here keeps Task 1 reviewable in isolation)

- [ ] **Step 1: Write the failing migration-integrity test**

Add to `crates/hydeclaw-db/src/session_wal.rs` inside the existing `mod tests` block (before the closing `}`):

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn m049_renames_session_events_to_session_timeline(pool: sqlx::PgPool) {
        // After all migrations run, the table must be `session_timeline` and the
        // old `session_events` name must not resolve.
        let exists_new: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables \
             WHERE table_name = 'session_timeline')"
        ).fetch_one(&pool).await.unwrap();
        assert!(exists_new, "session_timeline table must exist after m049");

        let exists_old: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM information_schema.tables \
             WHERE table_name = 'session_events')"
        ).fetch_one(&pool).await.unwrap();
        assert!(!exists_old, "session_events table must be gone after m049");

        // Indexes renamed.
        let idx_session: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM pg_indexes \
             WHERE indexname = 'idx_session_timeline_session')"
        ).fetch_one(&pool).await.unwrap();
        assert!(idx_session, "idx_session_timeline_session must exist after m049");

        let idx_type: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT FROM pg_indexes \
             WHERE indexname = 'idx_session_timeline_type')"
        ).fetch_one(&pool).await.unwrap();
        assert!(idx_type, "idx_session_timeline_type must exist after m049");
    }
```

- [ ] **Step 2: Run the test, confirm it fails**

Run: `cargo test -p hydeclaw-db m049_renames_session_events_to_session_timeline -- --nocapture`

Expected: FAIL because m049 doesn't exist yet — the migration will not run, `session_timeline` will not exist, and the first assertion will fail.

- [ ] **Step 3: Create the migration file**

Create `migrations/049_rename_session_events_to_timeline.sql` with:

```sql
-- Migration 049: rename session_events to session_timeline.
--
-- Honest naming: this table is a chronological log of session lifecycle
-- events used for LoopDetector warm-up after restart, diagnostics, and
-- audit. It is NOT a Write-Ahead Log — there is no replay-based recovery.
-- The "WAL" framing it inherited from m013 overpromised; this rename
-- removes the misleading vocabulary.
--
-- Column names and event_type values are unchanged. Old migrations
-- (m013, m030) are append-only history and stay as-is.
--
-- ALTER TABLE RENAME is metadata-only in PostgreSQL — atomic, no data
-- copy. Idempotent via IF EXISTS so reruns are safe.

ALTER TABLE  IF EXISTS session_events                RENAME TO session_timeline;
ALTER INDEX  IF EXISTS idx_session_events_session    RENAME TO idx_session_timeline_session;
ALTER INDEX  IF EXISTS idx_session_events_type       RENAME TO idx_session_timeline_type;
```

- [ ] **Step 4: Run the test, confirm it passes**

Run: `cargo test -p hydeclaw-db m049_renames_session_events_to_session_timeline -- --nocapture`

Expected: PASS. All four assertions hold.

- [ ] **Step 5: Run the existing tests in `session_wal.rs` to confirm they still pass**

Run: `cargo test -p hydeclaw-db --lib session_wal::tests -- --nocapture`

Expected: PASS for all four existing tests (`log_event_tx_updates_activity_at`, `log_event_tx_debounce_skips_recent`, `log_event_tx_does_not_resurrect_terminal`, `log_event_tx_heartbeats_when_activity_at_is_null`).

Wait — these tests `INSERT INTO sessions ...` and then call `log_event_tx`, which `INSERT INTO session_events ...`. They will FAIL after m049 because the table no longer exists under the old name. This is expected; we fix them in Task 2 when we rewrite the module's SQL strings.

For now, only the new test (`m049_renames_...`) needs to pass. The existing tests are temporarily broken and will be fixed atomically with the module rename in Task 2.

To unblock the commit, accept this: run `cargo test -p hydeclaw-db --lib session_wal::tests::m049_renames_session_events_to_session_timeline -- --nocapture` and confirm only that one test passes.

- [ ] **Step 6: Commit (with bisect-skip marker in body)**

```bash
git add migrations/049_rename_session_events_to_timeline.sql crates/hydeclaw-db/src/session_wal.rs
git commit -m "feat(db): m049 rename session_events to session_timeline [bisect-skip]

Metadata-only rename via ALTER TABLE/INDEX. Old migrations (m013, m030)
left intact as append-only history. Module file rename and SQL string
updates follow in the next commit (Task 2) — workspace tests targeting
session_wal::tests::log_event_tx_* are temporarily broken in THIS
commit only and pass again at the end of Task 2.

For git bisect: skip this commit (it is a transient broken-tests
state between two halves of an atomic operation that could not fit
in a single commit cleanly). The combined Task 1 + Task 2 pair is
the actual atomic unit of correctness."
```

Bisect users can skip this commit via `git bisect skip <sha>` if they hit it.

---

## Task 2: Rename Rust module + atomic update of all consumers

This is the largest single task because the module rename and every consumer of `session_wal::*` / `WalToolEvent` / `session_events` (SQL string) must change together or `cargo build` fails. We do it as one atomic commit.

**Files** (audited against `grep -rn 'session_events\|session_wal\|WalToolEvent\|warm_up_from_wal' crates/`):

- Rename: `crates/hydeclaw-db/src/session_wal.rs` → `crates/hydeclaw-db/src/session_timeline.rs`
- Modify: `crates/hydeclaw-db/src/lib.rs` (module declaration)
- Modify: `crates/hydeclaw-db/src/session_timeline.rs` (post-rename — internal content)
- Modify: `crates/hydeclaw-db/src/sessions.rs` (6 places — lines 873–877 sibling-module call + lines 1839, 1860, 1927, 2187, 2199–2206, 2292)
- Modify: `crates/hydeclaw-core/src/db/mod.rs` (line 6 — re-export `pub use hydeclaw_db::session_wal;`)
- Modify: `crates/hydeclaw-core/src/lib.rs` (line 93 — second re-export `pub use hydeclaw_db::session_wal;`)
- Modify: `crates/hydeclaw-core/src/agent/tool_loop.rs` (function rename + 6 lines in test fn names + 8 `WalToolEvent` constructor calls)
- Modify: `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs:268` (1 import path)
- Modify: `crates/hydeclaw-core/src/agent/pipeline/parallel.rs` (4 call sites + 1 comment at lines 360, 498, 557, 638, 1067)
- Modify: `crates/hydeclaw-core/src/agent/pipeline/finalize.rs:200, 220, 648` (3 SQL strings)
- Modify: `crates/hydeclaw-core/src/agent/engine/run.rs:447` (1 doc-comment)
- Modify: `crates/hydeclaw-core/src/agent/history.rs:698` (1 doc-comment)
- Modify: `crates/hydeclaw-core/src/agent/session_manager.rs` (7 places — lines 166, 244, 290, 319, 344, 473, 549)
- Modify: `crates/hydeclaw-core/src/agent/request_context.rs:11, 45` (use + call)
- Modify: `crates/hydeclaw-core/src/skills/evolution.rs:193, 197` (1 comment + 1 SQL string)
- Modify: `crates/hydeclaw-core/src/gateway/handlers/sessions.rs:170` (1 SQL string `FROM session_events`)
- Modify: `crates/hydeclaw-core/tests/integration_session_cleanup.rs:14` (1 `use` of `session_wal::log_event_tx` — note: distinct file from `integration_session_events_cleanup.rs` which is handled in Task 7)

The plan's previous draft mistakenly listed `crates/hydeclaw-core/src/agent/pipeline/llm_call.rs` — grep confirmed it has **zero** matches. Removed.

- [ ] **Step 1: Rename the file**

```bash
git mv crates/hydeclaw-db/src/session_wal.rs crates/hydeclaw-db/src/session_timeline.rs
```

- [ ] **Step 2: Update `lib.rs` export**

Edit `crates/hydeclaw-db/src/lib.rs` line 7:

```rust
// before:
pub mod session_wal;

// after:
pub mod session_timeline;
```

- [ ] **Step 3: Rewrite the module's header doc-comment**

Edit the top of `crates/hydeclaw-db/src/session_timeline.rs` (replace the first 6 lines):

```rust
//! Session timeline — chronological log of session lifecycle events.
//!
//! During normal operation, session state transitions (running, `tool_start`,
//! `tool_end`, done, failed) are appended to `session_timeline`. The table is
//! used for:
//!   * LoopDetector warm-up on session re-entry (preserves loop-break
//!     decisions across restarts — see `load_tool_events`).
//!   * Diagnostics: a per-session audit trail of what happened and when.
//!   * The UI Timeline view (future).
//!
//! This is NOT a Write-Ahead Log: there is no replay-based recovery. On
//! crash, completed work is preserved by the persisted side effects
//! (workspace files, memory chunks, channel messages, DB rows), not by
//! replaying events from this table. The `session_events` legacy name and
//! "WAL" framing have been retired (migration m049).
```

- [ ] **Step 4: Rename `WalToolEvent` to `TimelineToolEvent`**

In `crates/hydeclaw-db/src/session_timeline.rs`, replace the struct definition (line 127):

```rust
// before:
pub struct WalToolEvent {

// after:
pub struct TimelineToolEvent {
```

And update the return type at line 134:

```rust
// before:
pub async fn load_tool_events(db: &PgPool, session_id: Uuid) -> Result<Vec<WalToolEvent>> {

// after:
pub async fn load_tool_events(db: &PgPool, session_id: Uuid) -> Result<Vec<TimelineToolEvent>> {
```

And the constructor in the body (the `.map(|(name, success)| ...` line):

```rust
// before:
        .map(|(name, success)| WalToolEvent {

// after:
        .map(|(name, success)| TimelineToolEvent {
```

- [ ] **Step 5: Update all SQL strings inside `session_timeline.rs`**

Find every occurrence of `session_events` in SQL string literals inside `session_timeline.rs` and replace with `session_timeline`. There are 5 such occurrences:

```rust
// line 39:
"INSERT INTO session_events (session_id, event_type, payload) VALUES ($1, $2, $3)",
// becomes:
"INSERT INTO session_timeline (session_id, event_type, payload) VALUES ($1, $2, $3)",

// line ~99-101:
DELETE FROM session_events
WHERE id IN (
    SELECT id FROM session_events
// becomes:
DELETE FROM session_timeline
WHERE id IN (
    SELECT id FROM session_timeline

// line 140:
FROM session_events
// becomes:
FROM session_timeline
```

Update the doc-comment at line 69 (`/// Phase 62 RES-03: batched DELETE for session_events rows older than days.`):

```rust
/// Phase 62 RES-03: batched DELETE for `session_timeline` rows older than `days`.
```

- [ ] **Step 6: Update existing tests inside `session_timeline.rs` to use the new table name**

The four existing `#[sqlx::test]` tests (`log_event_tx_updates_activity_at`, etc.) don't reference `session_events` directly — they call `log_event_tx`, which itself writes to the table. After Step 5, those tests automatically target the new table. No change needed.

- [ ] **Step 7: Update `crates/hydeclaw-db/src/sessions.rs`**

This file has **two** clusters of references — the first around line 873 (sibling-module call) and the second from line 1839 onwards (SQL strings + test).

Cluster 1 — sibling-module call (lines 873–877):

- Lines 873–874 doc-comment: `// session_wal is a sibling module ...` → `// session_timeline is a sibling module ...` (also update `hydeclaw-db/src/lib.rs declares both` reference inside the comment — the lib.rs `pub mod` is being updated in Step 2).
- Line 877: `crate::session_wal::log_event_tx(...)` → `crate::session_timeline::log_event_tx(...)`.

Cluster 2 — SQL strings + test (from line 1839 onwards):

- Line ~1839: doc-comment `/// Insert a session_events WAL record ...` → `/// Insert a session_timeline record for a compression boundary.`
- Line ~1860: `"INSERT INTO session_events (...)` → `"INSERT INTO session_timeline (...)`
- Line ~1927: `FROM session_events` → `FROM session_timeline`
- Line ~2187: `"SELECT event_type, payload FROM session_events ...` → `"SELECT event_type, payload FROM session_timeline ...`
- Lines ~2199–2201: the test comment talks about dropping `session_events` to force a WAL insert failure. Replace `session_events` with `session_timeline` in the comment AND in the `DROP TABLE` statement at line 2206: `sqlx::query("DROP TABLE session_events")` → `sqlx::query("DROP TABLE session_timeline")`. Also rewrite the comment: replace "WAL insert (step 4)" with "timeline insert (step 4)".
- Line ~2292: `"SELECT event_type FROM session_events ...` → `"SELECT event_type FROM session_timeline ...`

After editing, run `grep -n 'session_events\|session_wal' crates/hydeclaw-db/src/sessions.rs` and confirm zero matches.

- [ ] **Step 8: Update re-exports in hydeclaw-core (TWO critical lines — without these, every `crate::db::session_wal::` path breaks)**

- `crates/hydeclaw-core/src/db/mod.rs:6` — `pub use hydeclaw_db::session_wal;` → `pub use hydeclaw_db::session_timeline;`.
- `crates/hydeclaw-core/src/lib.rs:93` — `pub use hydeclaw_db::session_wal;` → `pub use hydeclaw_db::session_timeline;`.

These are the re-export points that make `crate::db::session_wal::*` and `hydeclaw_core::db::session_wal::*` resolve everywhere else. Skip them and every subsequent file fails to compile.

- [ ] **Step 9: Update all `session_wal::` / `WalToolEvent` / SQL `session_events` consumers in core**

For each file below, replace every `session_wal` with `session_timeline`, every `WalToolEvent` with `TimelineToolEvent`, every `warm_up_from_wal` with `warm_up_from_timeline`, and every SQL `session_events` with `session_timeline`. After each file, run `grep -n 'session_events\|session_wal\|WalToolEvent\|warm_up_from_wal' <file>` and confirm zero matches.

- `crates/hydeclaw-core/src/agent/tool_loop.rs` — 12 places total:
  1. Line 138 — function declaration `pub fn warm_up_from_wal(...)` → `pub fn warm_up_from_timeline(...)`. Parameter type `&[hydeclaw_db::session_wal::WalToolEvent]` → `&[hydeclaw_db::session_timeline::TimelineToolEvent]`.
  2. Line 189 — `use hydeclaw_db::session_wal::WalToolEvent;` → `use hydeclaw_db::session_timeline::TimelineToolEvent;`.
  3. Line 230 — test fn `fn warm_up_from_wal_restores_error_streak()` → `fn warm_up_from_timeline_restores_error_streak()`.
  4. Lines 233, 234 — `WalToolEvent { ... }` → `TimelineToolEvent { ... }`.
  5. Line 236 — `LoopDetector::warm_up_from_wal(...)` → `LoopDetector::warm_up_from_timeline(...)`.
  6. Line 245 — test fn `fn warm_up_from_wal_empty_events_gives_fresh_detector()` → `fn warm_up_from_timeline_empty_events_gives_fresh_detector()`.
  7. Line 247 — `let events: Vec<WalToolEvent>` → `let events: Vec<TimelineToolEvent>`.
  8. Line 248 — `LoopDetector::warm_up_from_wal(...)` → `LoopDetector::warm_up_from_timeline(...)`.
  9. Line 256 — test fn `fn warm_up_from_wal_success_resets_streak()` → `fn warm_up_from_timeline_success_resets_streak()`.
  10. Lines 259, 260, 261 — `WalToolEvent { ... }` constructors → `TimelineToolEvent { ... }`.
  11. Line 263 — `LoopDetector::warm_up_from_wal(...)` → `LoopDetector::warm_up_from_timeline(...)`.

- `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs:268` — `crate::db::session_wal::load_tool_events` → `crate::db::session_timeline::load_tool_events`.

- `crates/hydeclaw-core/src/agent/pipeline/parallel.rs` — lines 360, 498, 557, 638 (`crate::db::session_wal::log_event` calls) and line 1067 (1 comment mentioning `crate::db::session_wal::log_event(...)`).

- `crates/hydeclaw-core/src/agent/pipeline/finalize.rs:200, 220, 648` — three SQL string literals `FROM session_events` → `FROM session_timeline`.

- `crates/hydeclaw-core/src/agent/engine/run.rs:447` — doc-comment `/// now first-class sessions in messages and session_events.` → `/// now first-class sessions in messages and session_timeline.`.

- `crates/hydeclaw-core/src/agent/history.rs:698` — doc-comment `(mark messages compressed, insert session_events WAL record).` → `(mark messages compressed, insert session_timeline record).`.

- `crates/hydeclaw-core/src/agent/session_manager.rs` — 7 places:
  - Line 166 — `crate::db::session_wal::log_event(...)` → `crate::db::session_timeline::log_event(...)`.
  - Line 244 — doc-comment mentioning `session_events` → `session_timeline`.
  - Line 290 — `crate::db::session_wal::log_event(...)` → `crate::db::session_timeline::log_event(...)`.
  - Line 319 — same call rename.
  - Line 344 — same call rename.
  - Line 473 — SQL string `FROM session_events` → `FROM session_timeline`.
  - Line 549 — same SQL rename.

- `crates/hydeclaw-core/src/agent/request_context.rs` — line 11 (`use crate::db::session_wal;` → `use crate::db::session_timeline;`) and line 45 (`session_wal::load_tool_events(...)` → `session_timeline::load_tool_events(...)`).

- `crates/hydeclaw-core/src/skills/evolution.rs:193, 197` — line 193 comment → `// 4. Tool names from session_timeline.`; line 197 SQL `FROM session_events` → `FROM session_timeline`.

- `crates/hydeclaw-core/src/gateway/handlers/sessions.rs:170` — SQL `FROM session_events \` → `FROM session_timeline \`.

- `crates/hydeclaw-core/tests/integration_session_cleanup.rs:14` — `use hydeclaw_core::db::session_wal::log_event_tx;` → `use hydeclaw_core::db::session_timeline::log_event_tx;`. (This is a **different file** from `integration_session_events_cleanup.rs` handled in Task 7 — do not confuse them.)

After all files: run `grep -rn 'session_events\|session_wal\|WalToolEvent\|warm_up_from_wal' crates/ --include='*.rs'` and confirm only matches are inside `crates/hydeclaw-db/src/session_timeline.rs` (the module itself, which legitimately uses the new names) and possibly inside `migrations/013_session_wal.sql` (handled in Task 8). Anywhere else means a missed reference — fix it.

- [ ] **Step 10: Verify the workspace builds**

Run: `cargo build --workspace --all-targets`

Expected: PASS, no compile errors. If something fails, the message will name the file and line — go back and fix the missed reference.

- [ ] **Step 11: Run all lib tests (no DB-backed tests)**

Run: `cargo test --workspace --lib -- --nocapture`

Expected: PASS for non-DB tests. `#[sqlx::test]` tests will be skipped because `DATABASE_URL` may be unset; that's OK — Task 1's test is the canonical migration check.

- [ ] **Step 12: Run DB-backed tests if DATABASE_URL is set**

Run: `cargo test -p hydeclaw-db --lib session_timeline -- --nocapture`

Expected: PASS for all 5 tests in the `session_timeline` module (the 4 originals + `m049_renames_session_events_to_session_timeline`). The originals previously inserted into `session_events` via `log_event_tx`; they now insert into `session_timeline` automatically because the SQL string was updated in Step 5.

- [ ] **Step 13: Commit (atomic — closes the bisect window opened by Task 1)**

```bash
git add -A
git commit -m "refactor(db): rename session_wal module to session_timeline

Module file, type WalToolEvent (-> TimelineToolEvent), function
warm_up_from_wal (-> warm_up_from_timeline), the two re-exports in
hydeclaw-core (db/mod.rs, lib.rs), and all SQL string literals
referring to the old table name are updated in lockstep with m049.
Workspace builds and tests pass. Bisect window opened by [bisect-skip]
in the previous commit closes here."
```

---

## Task 3: Rename scheduler function + main.rs callers + tracing job key

**Files:**

- Modify: `crates/hydeclaw-core/src/scheduler/mod.rs:465` (function rename + body)
- Modify: `crates/hydeclaw-core/src/main.rs:1179–1192` (call site + tracing job string)

- [ ] **Step 1: Rename the function in `scheduler/mod.rs`**

Edit `crates/hydeclaw-core/src/scheduler/mod.rs` line 465 and the surrounding doc-comment + body:

```rust
// before:
    pub async fn add_session_events_cleanup_hourly(

// after:
    pub async fn add_session_timeline_cleanup_hourly(
```

Update the function's doc-comment (lines ~458–464) to use "session_timeline" instead of "WAL" and "session_events".

Inside the body, update the tracing strings:

- Line 472: `"session_events hourly cleanup disabled ..."` → `"session_timeline hourly cleanup disabled ..."`
- Line 478: `"scheduling hourly session_events cleanup (RES-03)"` → `"scheduling hourly session_timeline cleanup (RES-03)"`
- Line 492: `"session_events hourly cleanup completed"` → `"session_timeline hourly cleanup completed"`

The `crate::db::session_wal::prune_old_events_batched` call at line 484 was already updated in Task 2 (Step 8) — verify it now reads `crate::db::session_timeline::prune_old_events_batched`.

- [ ] **Step 2: Update the call site in `main.rs`**

Edit `crates/hydeclaw-core/src/main.rs` lines 1179–1192:

```rust
// before:
    // Phase 62 RES-03: hourly batched session_events WAL cleanup. Runs
    // alongside the legacy daily add_session_cleanup (which still handles
    // cleanup_old_sessions) — the hourly job only prunes session_events,
    // with bounded LIMIT per batch to avoid long table locks.
    if let Err(e) = sched
        .add_session_events_cleanup_hourly(
            db.clone(),
            state.config.config.cleanup.session_events_retention_days,
            state.config.config.cleanup.session_events_batch_size,
        )
        .await
    {
        tracing::warn!(error = %e, job = "session_events_cleanup_hourly", "failed to register cron job");
    }

// after:
    // Phase 62 RES-03: hourly batched session_timeline cleanup. Runs
    // alongside the legacy daily add_session_cleanup (which still handles
    // cleanup_old_sessions) — the hourly job only prunes session_timeline,
    // with bounded LIMIT per batch to avoid long table locks.
    if let Err(e) = sched
        .add_session_timeline_cleanup_hourly(
            db.clone(),
            state.config.config.cleanup.session_events_retention_days,
            state.config.config.cleanup.session_events_batch_size,
        )
        .await
    {
        tracing::warn!(error = %e, job = "session_timeline_cleanup_hourly", "failed to register cron job");
    }
```

Note: `session_events_retention_days` and `session_events_batch_size` field accesses stay for now — those are renamed in Task 5.

- [ ] **Step 3: Verify the workspace builds**

Run: `cargo build --workspace --all-targets`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/scheduler/mod.rs crates/hydeclaw-core/src/main.rs
git commit -m "refactor(scheduler): rename add_session_events_cleanup_hourly

Follows the m049 table rename. The cron job's tracing key
\`session_events_cleanup_hourly\` becomes \`session_timeline_cleanup_hourly\`."
```

---

## Task 4: Rename dashboard metric field + JSON + SQL query + test

**Files:**

- Modify: `crates/hydeclaw-core/src/metrics.rs:926–928, 1021`
- Modify: `crates/hydeclaw-core/src/gateway/handlers/monitoring/mod.rs:146, 182–183, 211, 229`
- Modify: `crates/hydeclaw-core/tests/integration_dashboard_metrics.rs:149, 166, 190, 212, 263, 315`

- [ ] **Step 1: Update the failing test first (TDD on the API rename)**

In `crates/hydeclaw-core/tests/integration_dashboard_metrics.rs`, search-and-replace **`session_events_table_size_bytes`** with **`session_timeline_table_size_bytes`**. The grep above showed 6 occurrences: lines 149 (comment), 166, 190, 212, 263, 315.

After replacing, run: `cargo test -p hydeclaw-core --test integration_dashboard_metrics`

Expected: FAIL — compile error because the struct field still has the old name.

- [ ] **Step 2: Rename the struct field and JSON key in `metrics.rs`**

Edit `crates/hydeclaw-core/src/metrics.rs`:

- Lines 926–928: update the doc-comment + field:

```rust
// before:
    /// `pg_total_relation_size('session_events')` — Postgres-reported
    /// on-disk size of the SSE WAL table, in bytes.
    pub session_events_table_size_bytes: u64,

// after:
    /// `pg_total_relation_size('session_timeline')` — Postgres-reported
    /// on-disk size of the session timeline table, in bytes.
    pub session_timeline_table_size_bytes: u64,
```

- Line ~1021: the JSON serialization line:

```rust
// before:
        "session_events_table_size_bytes": snap.session_events_table_size_bytes,

// after:
        "session_timeline_table_size_bytes": snap.session_timeline_table_size_bytes,
```

- [ ] **Step 3: Update the SQL query and field in `monitoring/mod.rs`**

Edit `crates/hydeclaw-core/src/gateway/handlers/monitoring/mod.rs`:

- Line 146 (doc-comment example): `"session_events_table_size_bytes": <u64>` → `"session_timeline_table_size_bytes": <u64>`.
- Line 182 (local binding): `let session_events_table_size_bytes: u64 = ...` → `let session_timeline_table_size_bytes: u64 = ...`.
- Line 183 (SQL query): `"SELECT pg_total_relation_size('session_events')"` → `"SELECT pg_total_relation_size('session_timeline')"`.
- Line 211 (comment): `// Same posture as session_events_table_size_bytes ...` → `// Same posture as session_timeline_table_size_bytes ...`.
- Line 229 (struct init): `session_events_table_size_bytes,` → `session_timeline_table_size_bytes,`.

- [ ] **Step 4: Run the dashboard test, confirm it passes**

Run: `cargo test -p hydeclaw-core --test integration_dashboard_metrics`

Expected: PASS.

- [ ] **Step 5: Run the workspace build to catch any missed reference**

Run: `cargo build --workspace --all-targets`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/metrics.rs \
        crates/hydeclaw-core/src/gateway/handlers/monitoring/mod.rs \
        crates/hydeclaw-core/tests/integration_dashboard_metrics.rs
git commit -m "refactor(metrics): rename session_events_table_size_bytes

Public field in GET /api/dashboard/metrics. Operator-visible breaking
change documented in release notes. UI does not consume this field."
```

---

## Task 5: Rename CleanupConfig fields + main.rs references + internal docstrings

**Files:**

- Modify: `crates/hydeclaw-core/src/config/mod.rs:55, 258–283`
- Modify: `crates/hydeclaw-core/src/main.rs:1186–1187` (field access on `state.config.config.cleanup`)
- Modify: `crates/hydeclaw-db/src/session_timeline.rs` (doc-comment mentioning "session_events WAL retention" if any remained after Task 2)

- [ ] **Step 1: Rename the struct fields and helper fns in `config/mod.rs`**

Edit `crates/hydeclaw-core/src/config/mod.rs` lines 256–283:

```rust
// before:
// ── CleanupConfig ─────────────────────────────────────────────────────────────

/// Phase 62 RES-03: batched cleanup tuning for the hourly `session_events` WAL
/// prune cron. Both fields have operator-friendly defaults; `retention_days = 0`
/// disables the hourly cleanup entirely.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CleanupConfig {
    /// Retention for `session_events` WAL rows in days. `0` disables cleanup.
    /// Default: 7 days.
    #[serde(default = "default_session_events_retention_days")]
    pub session_events_retention_days: u32,
    /// Rows deleted per batch iteration — keeps lock hold-time short and
    /// autovacuum-friendly. Must be `> 0`. Default: 5000.
    #[serde(default = "default_session_events_batch_size")]
    pub session_events_batch_size: i64,
}

fn default_session_events_retention_days() -> u32 { 7 }
fn default_session_events_batch_size() -> i64 { 5000 }

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            session_events_retention_days: default_session_events_retention_days(),
            session_events_batch_size: default_session_events_batch_size(),
        }
    }
}

// after:
// ── CleanupConfig ─────────────────────────────────────────────────────────────

/// Phase 62 RES-03: batched cleanup tuning for the hourly `session_timeline`
/// prune cron. Both fields have operator-friendly defaults; `retention_days = 0`
/// disables the hourly cleanup entirely.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct CleanupConfig {
    /// Retention for `session_timeline` rows in days. `0` disables cleanup.
    /// Default: 7 days.
    #[serde(default = "default_session_timeline_retention_days")]
    pub session_timeline_retention_days: u32,
    /// Rows deleted per batch iteration — keeps lock hold-time short and
    /// autovacuum-friendly. Must be `> 0`. Default: 5000.
    #[serde(default = "default_session_timeline_batch_size")]
    pub session_timeline_batch_size: i64,
}

fn default_session_timeline_retention_days() -> u32 { 7 }
fn default_session_timeline_batch_size() -> i64 { 5000 }

impl Default for CleanupConfig {
    fn default() -> Self {
        Self {
            session_timeline_retention_days: default_session_timeline_retention_days(),
            session_timeline_batch_size: default_session_timeline_batch_size(),
        }
    }
}
```

Also update the comment at line 55 in `AppConfig`:

```rust
// before:
    /// Phase 62 RES-03 cleanup scheduler tuning (session_events WAL retention).

// after:
    /// Phase 62 RES-03 cleanup scheduler tuning (session_timeline retention).
```

- [ ] **Step 2: Update `main.rs` field accesses**

Edit `crates/hydeclaw-core/src/main.rs`:

- Line 1151 (passed into `add_session_cleanup` for batch size): `state.config.config.cleanup.session_events_batch_size` → `state.config.config.cleanup.session_timeline_batch_size`.
- Line 1186: `state.config.config.cleanup.session_events_retention_days` → `state.config.config.cleanup.session_timeline_retention_days`.
- Line 1187: `state.config.config.cleanup.session_events_batch_size` → `state.config.config.cleanup.session_timeline_batch_size`.

- [ ] **Step 3: Verify the workspace builds**

Run: `cargo build --workspace --all-targets`

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/src/config/mod.rs crates/hydeclaw-core/src/main.rs
git commit -m "refactor(config): rename [cleanup] keys to session_timeline_*

Operator-visible breaking change: hydeclaw.toml [cleanup] keys
session_events_retention_days/session_events_batch_size are renamed.
A startup PreCheck for the old keys is added in the next commit so
operators get a clear error instead of a serde 'unknown field'."
```

---

## Task 6: Add Startup PreCheck for old config keys (TDD)

**Files:**

- Modify: `crates/hydeclaw-core/src/config/mod.rs:1264–1272` (the `AppConfig::load` function)
- Modify: `crates/hydeclaw-core/src/config/mod.rs` (add `#[cfg(test)] mod precheck_tests` at the bottom of the file)

- [ ] **Step 1: Add the failing PreCheck test**

Append to the end of `crates/hydeclaw-core/src/config/mod.rs` (after the last existing item):

```rust
#[cfg(test)]
mod precheck_tests {
    use super::AppConfig;
    use std::io::Write;

    fn write_temp_toml(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("temp file");
        f.write_all(content.as_bytes()).expect("write toml");
        f
    }

    #[test]
    fn precheck_rejects_old_session_events_retention_days() {
        let toml = r#"
[cleanup]
session_events_retention_days = 14
"#;
        let f = write_temp_toml(toml);
        let err = AppConfig::load(f.path()).expect_err("must reject old key");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("session_events_retention_days")
                && msg.contains("renamed")
                && msg.contains("session_timeline_retention_days"),
            "PreCheck error must name old AND new key. Got: {msg}"
        );
    }

    #[test]
    fn precheck_rejects_old_session_events_batch_size() {
        let toml = r#"
[cleanup]
session_events_batch_size = 1000
"#;
        let f = write_temp_toml(toml);
        let err = AppConfig::load(f.path()).expect_err("must reject old key");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("session_events_batch_size")
                && msg.contains("session_timeline_batch_size"),
            "PreCheck error must name old AND new key. Got: {msg}"
        );
    }

    #[test]
    fn precheck_accepts_new_session_timeline_keys() {
        let toml = r#"
[cleanup]
session_timeline_retention_days = 14
session_timeline_batch_size = 1000
"#;
        let f = write_temp_toml(toml);
        let cfg = AppConfig::load(f.path()).expect("new keys must parse");
        assert_eq!(cfg.cleanup.session_timeline_retention_days, 14);
        assert_eq!(cfg.cleanup.session_timeline_batch_size, 1000);
    }
}
```

`tempfile` is already a `[dev-dependencies]` entry in `crates/hydeclaw-core/Cargo.toml` (currently `tempfile = "3.27.0"`). The test uses it directly via `tempfile::NamedTempFile`. No `Cargo.toml` change needed; verify with:

```bash
grep -n "tempfile" crates/hydeclaw-core/Cargo.toml
```

Expected output: `141:tempfile = "3.27.0"` (or a later version — whatever is committed).

- [ ] **Step 2: Run the test, confirm it fails**

Run: `cargo test -p hydeclaw-core --lib config::precheck_tests -- --nocapture`

Expected: FAIL because the PreCheck isn't implemented — `AppConfig::load` will fail with a bare serde "unknown field" error, not the targeted message.

- [ ] **Step 3: Implement the PreCheck in `AppConfig::load`**

Edit `crates/hydeclaw-core/src/config/mod.rs` line 1264. Replace the existing `load` function:

```rust
// before:
impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read config: {}", path.as_ref().display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| "failed to parse config TOML")?;
        Ok(config)
    }
}

// after:
impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path.as_ref())
            .with_context(|| format!("failed to read config: {}", path.as_ref().display()))?;
        Self::check_renamed_keys(&content)?;
        let config: Self = toml::from_str(&content)
            .with_context(|| "failed to parse config TOML")?;
        Ok(config)
    }

    /// Catch operator-facing key renames before the bare serde error surfaces.
    /// Each entry maps an old key to its new name + the [section] it lives in.
    fn check_renamed_keys(raw_toml: &str) -> Result<()> {
        const RENAMES: &[(&str, &str, &str)] = &[
            // (old key, new key, section)
            ("session_events_retention_days", "session_timeline_retention_days", "[cleanup]"),
            ("session_events_batch_size",     "session_timeline_batch_size",     "[cleanup]"),
        ];
        for (old, new, section) in RENAMES {
            // Match the old key at the start of a line (allowing leading
            // whitespace), followed by optional spaces and `=`. This avoids
            // false positives from comments or inline strings.
            let found_as_key = raw_toml.lines().any(|line| {
                let trimmed = line.trim_start();
                trimmed.starts_with(old)
                    && trimmed
                        .get(old.len()..)
                        .map(|tail| tail.trim_start().starts_with('='))
                        .unwrap_or(false)
            });
            if found_as_key {
                anyhow::bail!(
                    "config error: {section} key `{old}` was renamed to \
                     `{new}` in this release. Update hydeclaw.toml.",
                );
            }
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run the tests, confirm they pass**

Run: `cargo test -p hydeclaw-core --lib config::precheck_tests -- --nocapture`

Expected: PASS — all three tests.

- [ ] **Step 5: Run the workspace build**

Run: `cargo build --workspace --all-targets`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hydeclaw-core/src/config/mod.rs
git commit -m "feat(config): startup PreCheck for renamed [cleanup] keys

Catches the operator using the pre-m049 key names
(session_events_retention_days, session_events_batch_size) and emits
an actionable error naming the new keys, instead of the bare serde
'unknown field' message."
```

---

## Task 7: Rename integration test file + content

**Files:**

- Rename: `crates/hydeclaw-core/tests/integration_session_events_cleanup.rs` → `crates/hydeclaw-core/tests/integration_session_timeline_cleanup.rs`

- [ ] **Step 1: Rename the file with git mv**

```bash
git mv crates/hydeclaw-core/tests/integration_session_events_cleanup.rs \
       crates/hydeclaw-core/tests/integration_session_timeline_cleanup.rs
```

- [ ] **Step 2: Update file content**

Inside `crates/hydeclaw-core/tests/integration_session_timeline_cleanup.rs`:

- Line 1 (header doc-comment): replace `verify batched DELETE prune_old_events_batched against a real PG` framing if it mentions WAL — keep the RES-03 reference, just s/WAL/session timeline/.
- Line 11: `use hydeclaw_core::db::session_wal::prune_old_events_batched;` → `use hydeclaw_core::db::session_timeline::prune_old_events_batched;`.
- Line 16 (doc-comment): `Insert a session row (FK target for session_events) ...` → `Insert a session row (FK target for session_timeline) ...`.
- Line 32 (doc-comment): `FK churn — session_events.session_id references sessions.id ...` → `FK churn — session_timeline.session_id references sessions.id ...`.
- Line 38: SQL `INSERT INTO session_events (...)` → `INSERT INTO session_timeline (...)`.
- Lines 68, 92, 154: SQL `SELECT COUNT(*)::bigint FROM session_events` → `SELECT COUNT(*)::bigint FROM session_timeline`.
- Line 130 (doc-comment with cargo command): `cargo test -p hydeclaw-core --test integration_session_events_cleanup` → `cargo test -p hydeclaw-core --test integration_session_timeline_cleanup`.

Run `grep -n "session_events\|session_wal" crates/hydeclaw-core/tests/integration_session_timeline_cleanup.rs` after editing to confirm zero remaining matches.

- [ ] **Step 3: Run the renamed test**

Run: `cargo test -p hydeclaw-core --test integration_session_timeline_cleanup -- --ignored`

Expected: PASS (the `--ignored` flag is required if the test was marked with `#[ignore]` previously; check the file header for the test markers).

- [ ] **Step 4: Commit**

```bash
git add crates/hydeclaw-core/tests/integration_session_timeline_cleanup.rs
# Note: git mv records the rename automatically; no separate add needed.
git commit -m "test(timeline): rename integration test for session_timeline

Mirrors the m049 + module rename. Test logic unchanged."
```

---

## Task 8: Documentation rewrites

**Files:**

- Modify: `CLAUDE.md` (4 paragraphs near lines 86, 88, 96, 294, 298)
- Modify: `docs/ARCHITECTURE.md` (5 lines: 77, 339, 434, 736, 759)
- Modify: `docs/CONFIGURATION.md` (3 lines near 270–275)
- Modify: `docs/API.md` (1 JSON example field at line 171)
- Modify: `migrations/013_session_wal.sql` (header SQL comment only — file name stays)

**Explicit exclusions:**

- `docs/architecture/2026-05-06-architecture-review.md` and `docs/architecture/2026-05-06-llm-loop-unification-plan.md` — these are **dated snapshots** (the directory pattern is `YYYY-MM-DD-…`). Do **not** rewrite them. They reflect what the codebase looked like on their date. Future readers expect snapshots to be immutable. The Task 9 grep AC excludes `docs/architecture/` for this reason.
- Existing items inside `docs/superpowers/plans/` and `docs/superpowers/specs/` that reference the old names — these are also dated, historical, and left alone.

Use this phrase-anchor where the WAL/recovery framing is being replaced:

> Session timeline — chronological log of session lifecycle events. Used for LoopDetector warm-up after restart (preserves loop-break decisions across crashes), diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery; completed work is preserved by persisted side effects, not event replay.

- [ ] **Step 1: Update `CLAUDE.md`**

Open `CLAUDE.md`. The four sections to rewrite are around lines 86–88 (bootstrap/finalize bullets), line 96 (LLM-loop unification paragraph), line 294 (Key tables list), and line 298 (Session WAL paragraph).

Concrete edits — search and replace:

- Replace `` WAL `running` `` → `` timeline `running` `` and `` WAL `done|failed|interrupted` `` → `` timeline `done|failed|interrupted` `` (in the bootstrap/finalize bullets around lines 86–88).
- The line `session_events (WAL journal)` in the Key tables list becomes `session_timeline (chronological lifecycle log)`.
- Rewrite the "Session WAL (m013)" paragraph at line 298:

```text
// before:
**Session WAL (m013):** `session_events` logs lifecycle transitions (running, tool_start, tool_end, done, failed). WAL records lifecycle events for diagnostics. LoopDetector resets on each session entry (crash recovery via WAL replay is not yet implemented).

// after:
**Session timeline (m013, renamed by m049):** `session_timeline` is a chronological log of session lifecycle events (running, tool_start, tool_end, done, failed, interrupted). Used for LoopDetector warm-up after restart (preserves loop-break decisions across crashes), diagnostics, audit, and the UI Timeline view. Not a Write-Ahead Log: no replay-based recovery; completed work is preserved by persisted side effects, not event replay.
```

- Any other line in `CLAUDE.md` containing `session_events` or `WAL` related to this concept — update with the phrase-anchor or simply replace `session_events` with `session_timeline` and `WAL` with `timeline`.

After editing, verify: `grep -nwE 'session_events|WAL' CLAUDE.md` returns zero matches. (The `-w` flag uses word boundaries so we don't false-positive on any unrelated word containing the substring `WAL`.)

- [ ] **Step 2: Update `docs/ARCHITECTURE.md`**

Open `docs/ARCHITECTURE.md`. The grep above identified five lines:

- Line 77: `Recover stale session_events WAL entries from previous crash` — rewrite as `Stale session_timeline entries from a previous crash are not replayed; LoopDetector is warmed from tool_end events only.`
- Line 339: `**Session WAL** (\`session_events\` table, migration m013): ...` — replace using the phrase-anchor; mention m049 rename.
- Line 434: `- \`session_events_table_size_bytes\`` → `- \`session_timeline_table_size_bytes\``.
- Line 736: `Session cleanup | hourly | Prune \`session_events\` WAL rows older than \`retention_days\`` → `Session cleanup | hourly | Prune \`session_timeline\` rows older than \`retention_days\``.
- Line 759: `\`session_events\` | WAL journal | ...` → `\`session_timeline\` | chronological lifecycle log | ...`.

After editing, verify: `grep -nwE 'session_events|session WAL|WAL replay' docs/ARCHITECTURE.md` returns zero matches. Note this targets `docs/ARCHITECTURE.md` (the live doc) — not the `docs/architecture/*.md` snapshots.

- [ ] **Step 3: Update `docs/CONFIGURATION.md`**

Open `docs/CONFIGURATION.md`. Around lines 270–275:

```markdown
// before:
Настройки очистки WAL-журнала (`session_events`).

| Ключ | Тип | По умолчанию | Описание |
|------|-----|--------------|----------|
| `session_events_retention_days` | u32 | `7` | Хранить WAL-записи N дней. `0` = отключить очистку |
| `session_events_batch_size` | i64 | `5000` | Строк удаляется за одну batch-итерацию (минимизирует удержание блокировок) |

// after:
Настройки очистки таблицы хронологического лога событий сессий (`session_timeline`).

| Ключ | Тип | По умолчанию | Описание |
|------|-----|--------------|----------|
| `session_timeline_retention_days` | u32 | `7` | Хранить timeline-записи N дней. `0` = отключить очистку |
| `session_timeline_batch_size` | i64 | `5000` | Строк удаляется за одну batch-итерацию (минимизирует удержание блокировок) |
```

- [ ] **Step 4: Update `docs/API.md`**

Edit `docs/API.md` line 171 in the JSON example:

```json
// before:
  "session_events_table_size_bytes": 204800,

// after:
  "session_timeline_table_size_bytes": 204800,
```

- [ ] **Step 5: Update `migrations/013_session_wal.sql` header comment**

This is the only modification we make to a historical migration — we only rewrite the comment, not the SQL. Replace the existing two header comment lines:

```sql
// before:
-- Session Write-Ahead Log: journal table for session lifecycle events.
-- Used for crash recovery instead of injecting synthetic "[interrupted]" messages.

// after:
-- Session timeline (historical name: session_events, renamed by m049).
-- Chronological log of session lifecycle events. Used for LoopDetector
-- warm-up after restart, diagnostics, and audit. NOT a Write-Ahead Log:
-- no replay-based recovery. The "WAL" framing this migration originally
-- carried was misleading and was removed by m049 + accompanying docs.
```

The file name `013_session_wal.sql` is intentionally **not** changed — migration file names are identity in sqlx and must not move.

- [ ] **Step 6: Verify the acceptance-criteria greps**

Run:

```bash
grep -wr 'WAL' crates/ docs/ CLAUDE.md \
    --exclude-dir=migrations --exclude-dir=architecture \
    --exclude-dir=plans --exclude-dir=specs --exclude-dir=target
grep -wr 'session_events' crates/ docs/ CLAUDE.md \
    --exclude-dir=migrations --exclude-dir=architecture \
    --exclude-dir=plans --exclude-dir=specs --exclude-dir=target
```

Expected: zero matches related to the session-timeline concept.

`--exclude-dir=architecture`, `--exclude-dir=plans`, `--exclude-dir=specs` skip the dated snapshots inside `docs/architecture/`, `docs/superpowers/plans/`, and `docs/superpowers/specs/` — these are historical artifacts and intentionally not rewritten (the rationale is in this task's *Explicit exclusions* section). If `WAL` appears in any unrelated context (e.g., PostgreSQL's own WAL in some Docker config), leave it; the grep needs visual inspection of the remaining matches if any.

- [ ] **Step 7: Commit**

```bash
git add CLAUDE.md docs/ARCHITECTURE.md docs/CONFIGURATION.md docs/API.md migrations/013_session_wal.sql
git commit -m "docs: rewrite WAL/crash-recovery framing as session timeline

Honest description of what the table actually does: chronological log
for LoopDetector warm-up + diagnostics + audit. No replay-based crash
recovery exists or is planned; the rejection rationale lives in the
companion design doc."
```

---

## Task 9: Final verification (acceptance criteria check)

**Files:** none modified; verification only.

- [ ] **Step 1: Run the full grep audit**

```bash
EXCLUDES="--exclude-dir=migrations --exclude-dir=architecture \
          --exclude-dir=plans --exclude-dir=specs --exclude-dir=target"

echo "--- session_events outside migrations + dated docs (must be empty): ---"
grep -wrn 'session_events' crates/ docs/ CLAUDE.md $EXCLUDES

echo "--- session_wal outside migrations + dated docs (must be empty): ---"
grep -wrn 'session_wal' crates/ docs/ CLAUDE.md $EXCLUDES

echo "--- session-timeline-related WAL mentions outside dated docs (must be empty): ---"
grep -rnE 'session WAL|WAL journal|WAL replay|WAL recovery|crash recovery via WAL' \
    crates/ docs/ CLAUDE.md $EXCLUDES
```

Expected: all three searches print zero matches.

Exclusions explained:

- `migrations/` — m013 and m030 are append-only history.
- `docs/architecture/` — dated snapshots (file names start with the date the snapshot captured).
- `docs/superpowers/plans/` and `docs/superpowers/specs/` — dated planning artifacts; once committed they reflect the moment, not current truth.
- `target/` — build artifacts.

- [ ] **Step 2: Run the full workspace build**

Run: `cargo build --workspace --all-targets`

Expected: PASS.

- [ ] **Step 3: Run all non-DB tests**

Run: `cargo test --workspace --lib`

Expected: PASS.

- [ ] **Step 4: Run DB-backed tests**

With `DATABASE_URL` set:

```bash
make test-db
```

Expected: PASS for the full suite, including:

- `m049_renames_session_events_to_session_timeline` (Task 1)
- The four existing tests in `session_timeline.rs`
- `integration_session_timeline_cleanup` (Task 7)
- `integration_dashboard_metrics` (Task 4)
- All `config::precheck_tests` (Task 6)

- [ ] **Step 5: Smoke-test PreCheck against a hand-crafted bad config**

Create a throwaway TOML and confirm the binary fails clearly:

```bash
cat > /tmp/bad-cleanup.toml <<'EOF'
[cleanup]
session_events_retention_days = 14
EOF
cargo run -p hydeclaw-core --bin hydeclaw-core -- --config /tmp/bad-cleanup.toml 2>&1 | head -5
```

Expected: the process exits non-zero with output containing `session_events_retention_days was renamed to session_timeline_retention_days`. If the binary doesn't accept `--config`, replace this with a unit-level smoke test of `AppConfig::load("/tmp/bad-cleanup.toml")`. Either way, the message must mention both the old and new key.

- [ ] **Step 6: No commit needed — this is a verification-only task**

If all five steps pass, the rename is done. If any step surfaces a missed reference, fix it inline and append a follow-up commit:

```bash
git commit -m "fix: <description of missed reference>"
```

---

## Out of scope

Already locked in the spec — do not expand into:

- Backward-compatible aliasing of the old metric or config keys.
- Renaming the m013 migration file itself.
- Implementing `plans`/`plan_steps`/ACTIVE PLAN block (Part B from the combined design doc — deferred).

## Self-review summary

This plan covers every section of the spec:

- Migration m049 — Task 1.
- Module file rename + atomic update of all Rust consumers — Task 2.
- Scheduler function rename + main.rs caller — Task 3.
- Public metric rename + SQL + tests — Task 4.
- Config field rename + main.rs references — Task 5.
- Startup PreCheck — Task 6.
- Test file rename — Task 7.
- Documentation rewrites (CLAUDE.md / ARCHITECTURE.md / CONFIGURATION.md / API.md / m013 SQL comment) — Task 8.
- Final acceptance-criteria checks — Task 9.

The plan keeps each commit's workspace in a compilable, testable state with **one** intentional exception: Task 1's commit is marked `[bisect-skip]` in its commit body, because the migration must land before the module rename but the two cannot fit in a single commit cleanly. Task 2's final step explicitly closes that bisect window with an "all tests pass" commit. Bisect users who hit Task 1's hash should `git bisect skip` it.

## Audit trail

This plan was patched on 2026-05-14 after a `/review` pass against `grep -rn 'session_events\|session_wal\|WalToolEvent\|warm_up_from_wal' crates/`.

Patches added:

- Four files originally missed: `crates/hydeclaw-core/src/db/mod.rs`, `crates/hydeclaw-core/src/lib.rs`, `crates/hydeclaw-core/src/gateway/handlers/sessions.rs`, `crates/hydeclaw-core/tests/integration_session_cleanup.rs`.
- Additional lines in already-listed files: `sessions.rs:873–877`, `session_manager.rs:244, 344, 473, 549`, `tool_loop.rs:230, 236, 245, 256, 263`.
- Removed `llm_call.rs` from the consumer list (false positive — zero matches).
- Explicit exclusion of dated docs (`docs/architecture/`, `docs/superpowers/plans/`, `docs/superpowers/specs/`) from the grep ACs.
- Bisect-skip marker in Task 1's commit message.
- Confirmed `tempfile` is already a dev-dependency — no Cargo.toml change needed.
- Switched grep ACs to `-w` word-boundary matching.
