# Soul Events — Batch A (incremental extraction) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make knowledge extraction incremental — each run processes only messages newer than a per-session watermark, gated so it waits for ≥ `MIN_NEW_MESSAGES` new messages, eliminating the re-summarization of the same window every turn.

**Architecture:** Add a nullable `sessions.last_extracted_at` watermark (migration 085) + two `opex-db` accessors. In `extract_and_save_inner`, read the watermark, select only newer user/assistant messages via a pure `select_new_messages` helper (returns `None` when < `MIN_NEW_MESSAGES`), extract on that span, and advance the watermark to the newest included message **only on success**.

**Tech Stack:** Rust 2024, sqlx (Postgres), chrono. rustls-tls only — no new deps.

## Global Constraints

- Rust + rustls-tls only — no new external dependency.
- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push, do NOT deploy — the controller runs the server test session + deploy after review, on explicit user approval.
- Windows dev host cannot run the Rust bin-target test suite or `#[sqlx::test]` (needs live Postgres) — authority is the Linux server (`~/opex-src`, throttled `CARGO_BUILD_JOBS=4 nice ionice`). Local `cargo check --all-targets` + `cargo clippy --all-targets -- -D warnings` only; run test modules under `--all-targets` so they compile-check.
- `MIN_NEW_MESSAGES: usize = 4` and `MAX_CONTEXT_MESSAGES: usize = 20` (existing) are the exact values.
- Watermark advances ONLY after a successful extraction+save; a failure (`?` early-return) must leave `last_extracted_at` unchanged so the span retries next turn.
- Migration is additive + history-preserving: `ADD COLUMN ... TIMESTAMPTZ` nullable, no default. Never edit an already-applied migration.
- Extraction runs for ALL agents (facts/emotion/summary/initiative), not just soul — the watermark is general, not soul-gated.
- Source spec: `docs/superpowers/specs/2026-07-15-soul-events-ephemeral-observation-stream-design.md` §2.

## File Structure

- `migrations/085_sessions_last_extracted_at.sql` (new) — the watermark column.
- `crates/opex-db/src/sessions.rs` (modify) — `get_last_extracted_at` + `set_last_extracted_at`.
- `crates/opex-core/src/agent/knowledge_extractor.rs` (modify) — `MIN_NEW_MESSAGES` const, pure `select_new_messages`, and the `extract_and_save_inner` rewrite + unit tests.

---

### Task 1: Migration + watermark accessors

**Files:**
- Create: `migrations/085_sessions_last_extracted_at.sql`
- Modify: `crates/opex-db/src/sessions.rs` (add two fns near `load_messages`, ~line 699)
- Test: `crates/opex-db/src/sessions.rs` `#[cfg(test)]` (sqlx, server-run)

**Interfaces:**
- Produces:
  - `pub async fn get_last_extracted_at(db: &PgPool, session_id: Uuid) -> anyhow::Result<Option<chrono::DateTime<chrono::Utc>>>`
  - `pub async fn set_last_extracted_at(db: &PgPool, session_id: Uuid, ts: chrono::DateTime<chrono::Utc>) -> anyhow::Result<()>`
  - `sessions.last_extracted_at TIMESTAMPTZ NULL`

- [ ] **Step 1: Write the migration**

Create `migrations/085_sessions_last_extracted_at.sql`:

```sql
-- Per-session watermark for incremental knowledge extraction: the created_at of
-- the newest message already summarized. NULL = never extracted (extract from
-- the start). Additive, history-preserving.
ALTER TABLE sessions ADD COLUMN last_extracted_at TIMESTAMPTZ;
```

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)]` module in `crates/opex-db/src/sessions.rs` (create the module if absent; match the crate's existing `#[sqlx::test(migrations = "../../migrations")]` convention — a bare `#[sqlx::test]` creates an empty DB and the column won't exist):

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn last_extracted_at_roundtrips(db: sqlx::PgPool) {
        // A session row is required (FK / row must exist to UPDATE).
        let sid = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO sessions (id, agent_id, status) VALUES ($1, 'A', 'done')")
            .bind(sid)
            .execute(&db)
            .await
            .unwrap();

        // NULL initially.
        assert!(get_last_extracted_at(&db, sid).await.unwrap().is_none());

        let ts = chrono::Utc::now();
        set_last_extracted_at(&db, sid, ts).await.unwrap();
        let got = get_last_extracted_at(&db, sid).await.unwrap().expect("some");
        // Postgres timestamptz is microsecond precision — compare within 1ms.
        assert!((got - ts).num_milliseconds().abs() <= 1, "roundtrip ts mismatch: {got} vs {ts}");
    }
```

(If the `sessions` INSERT fails on a NOT NULL column this test doesn't set, add the minimal required columns — inspect `\d sessions`; `id`, `agent_id`, `status` are the likely-required set. The assertion is the contract.)

- [ ] **Step 3: Run the test to verify it fails**

Run (SERVER): `cargo test -p opex-db last_extracted_at_roundtrips -- --nocapture`
Expected: FAIL to compile — `get_last_extracted_at`/`set_last_extracted_at` not defined.

- [ ] **Step 4: Add the accessors**

In `crates/opex-db/src/sessions.rs`, after `load_messages` (~line 699):

```rust
/// Read a session's incremental-extraction watermark (created_at of the newest
/// message already summarized). `None` = never extracted.
pub async fn get_last_extracted_at(
    db: &PgPool,
    session_id: Uuid,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let row: Option<(Option<chrono::DateTime<chrono::Utc>>,)> =
        sqlx::query_as("SELECT last_extracted_at FROM sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(db)
            .await?;
    Ok(row.and_then(|r| r.0))
}

/// Advance a session's incremental-extraction watermark. Called only after a
/// successful extraction+save (spec §2.1).
pub async fn set_last_extracted_at(
    db: &PgPool,
    session_id: Uuid,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    sqlx::query("UPDATE sessions SET last_extracted_at = $1 WHERE id = $2")
        .bind(ts)
        .bind(session_id)
        .execute(db)
        .await?;
    Ok(())
}
```

(`Result` here is the crate's existing alias — match whatever `load_messages` returns, i.e. `anyhow::Result` or the module's `Result`. `PgPool`/`Uuid` are already imported in this file.)

- [ ] **Step 5: Run the test to verify it passes**

Run (SERVER): `cargo test -p opex-db last_extracted_at_roundtrips -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Local check + clippy**

Run: `cargo check -p opex-db --all-targets` then `cargo clippy -p opex-db --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add migrations/085_sessions_last_extracted_at.sql crates/opex-db/src/sessions.rs
git commit -m "feat(soul): sessions.last_extracted_at watermark + accessors (incremental extraction)"
```

---

### Task 2: Pure `select_new_messages` + `MIN_NEW_MESSAGES`

**Files:**
- Modify: `crates/opex-core/src/agent/knowledge_extractor.rs` (add const near line 21 next to `MAX_CONTEXT_MESSAGES`; add the fn; add `#[cfg(test)]` tests)

**Interfaces:**
- Consumes: `crate::db::sessions::MessageRow` (fields `.role: String`, `.created_at: chrono::DateTime<chrono::Utc>`).
- Produces:
  - `const MIN_NEW_MESSAGES: usize = 4;`
  - `fn select_new_messages(rows: &[MessageRow], watermark: Option<chrono::DateTime<chrono::Utc>>) -> Option<Vec<&MessageRow>>`

- [ ] **Step 1: Write the failing tests**

Add a `#[cfg(test)]` module in `knowledge_extractor.rs` (or extend the existing one). A helper builds a minimal `MessageRow`:

```rust
#[cfg(test)]
mod incremental_tests {
    use super::*;
    use crate::db::sessions::MessageRow;

    fn msg(role: &str, secs: i64) -> MessageRow {
        MessageRow {
            id: uuid::Uuid::new_v4(),
            role: role.to_string(),
            content: "x".to_string(),
            tool_calls: None,
            tool_call_id: None,
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(1_700_000_000 + secs, 0).unwrap(),
            agent_id: Some("A".to_string()),
            feedback: None,
            edited_at: None,
            status: "done".to_string(),
            thinking_blocks: None,
            parent_message_id: None,
            branch_from_message_id: None,
            abort_reason: None,
            is_mirror: false,
        }
    }

    #[test]
    fn none_watermark_takes_all_relevant() {
        let rows = vec![msg("user", 1), msg("assistant", 2), msg("tool", 3), msg("user", 4), msg("assistant", 5)];
        // 4 user/assistant ≥ MIN_NEW_MESSAGES(4) → Some, tool filtered out.
        let sel = select_new_messages(&rows, None).expect("some");
        assert_eq!(sel.len(), 4);
        assert!(sel.iter().all(|m| m.role == "user" || m.role == "assistant"));
    }

    #[test]
    fn below_min_new_returns_none() {
        let rows = vec![msg("user", 1), msg("assistant", 2), msg("user", 3)]; // 3 < 4
        assert!(select_new_messages(&rows, None).is_none());
    }

    #[test]
    fn watermark_excludes_older_and_gates() {
        let rows = vec![msg("user", 1), msg("assistant", 2), msg("user", 3), msg("assistant", 4), msg("user", 5)];
        let wm = rows[1].created_at; // exclude first two (created_at <= wm)
        // remaining strictly-newer user/assistant: secs 3,4,5 = 3 < MIN_NEW(4) → None
        assert!(select_new_messages(&rows, Some(wm)).is_none());
        // With an earlier watermark: exclude only secs<=1 → 4 remain → Some
        let wm2 = rows[0].created_at;
        let sel = select_new_messages(&rows, Some(wm2)).expect("some");
        assert_eq!(sel.len(), 4);
        assert!(sel.iter().all(|m| m.created_at > wm2));
    }

    #[test]
    fn caps_at_max_context() {
        let rows: Vec<MessageRow> = (0..30).map(|i| msg("user", i)).collect();
        let sel = select_new_messages(&rows, None).expect("some");
        assert_eq!(sel.len(), MAX_CONTEXT_MESSAGES); // last 20
        assert_eq!(sel[0].created_at, rows[10].created_at); // dropped oldest 10
    }
}
```

(If `MessageRow` has additional fields not listed here, fill them with the obvious default; the assertions are the contract. If it derives no public constructor path, the fields are all `pub` — construct directly as shown.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p opex-core --bin opex-core incremental_tests -- --nocapture` (SERVER, or local `cargo check --all-targets` to see the compile error)
Expected: FAIL — `select_new_messages` / `MIN_NEW_MESSAGES` not defined.

- [ ] **Step 3: Add the const + fn**

Near `MAX_CONTEXT_MESSAGES` (line 21) add:

```rust
/// Minimum NEW user/assistant messages (since the session watermark) required
/// before an extraction run fires. Batches extraction so overlapping windows
/// are not re-summarized every turn (spec §2.1).
const MIN_NEW_MESSAGES: usize = 4;
```

Add the pure selector (e.g. just above `extract_and_save_inner`):

```rust
/// Select the user/assistant messages newer than `watermark` for extraction.
/// Returns `None` when fewer than `MIN_NEW_MESSAGES` new messages exist (caller
/// skips this run until more accumulate). Otherwise returns the last
/// `MAX_CONTEXT_MESSAGES` of them, chronological order. Pure — unit-tested.
fn select_new_messages(
    rows: &[crate::db::sessions::MessageRow],
    watermark: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<Vec<&crate::db::sessions::MessageRow>> {
    let new_relevant: Vec<&crate::db::sessions::MessageRow> = rows
        .iter()
        .filter(|m| m.role == "user" || m.role == "assistant")
        .filter(|m| watermark.is_none_or(|w| m.created_at > w))
        .collect();
    if new_relevant.len() < MIN_NEW_MESSAGES {
        return None;
    }
    let start = new_relevant.len().saturating_sub(MAX_CONTEXT_MESSAGES);
    Some(new_relevant[start..].to_vec())
}
```

(If `Option::is_none_or` is unavailable on the toolchain, use `watermark.map_or(true, |w| m.created_at > w)`.)

- [ ] **Step 4: Run tests to verify they pass**

Run (SERVER): `cargo test -p opex-core --bin opex-core incremental_tests -- --nocapture`
Expected: PASS (4 tests).

- [ ] **Step 5: Local check + clippy**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: clean. (`select_new_messages` is unused until Task 3 — expect a `dead_code` warning ONLY under `-D warnings`; silence it by wiring Task 3 in the SAME branch before the clippy gate, or temporarily `#[allow(dead_code)]` and remove it in Task 3. Prefer: land Task 2 + Task 3 back-to-back and run clippy once after Task 3. Note this in the report.)

- [ ] **Step 6: Commit**

```bash
git add crates/opex-core/src/agent/knowledge_extractor.rs
git commit -m "feat(soul): pure select_new_messages + MIN_NEW_MESSAGES gate"
```

---

### Task 3: Wire the watermark into `extract_and_save_inner`

**Files:**
- Modify: `crates/opex-core/src/agent/knowledge_extractor.rs` (`extract_and_save_inner`, lines ~103–190)

**Interfaces:**
- Consumes: `select_new_messages` (Task 2), `crate::db::sessions::{get_last_extracted_at, set_last_extracted_at}` (Task 1).

**Background:** Today (lines 104–119) it loads all messages, `if rows.len() < MIN_MESSAGES return`, filters user/assistant, takes the last `MAX_CONTEXT_MESSAGES`. Rewrite to read the watermark, use `select_new_messages`, and advance the watermark on success.

- [ ] **Step 1: Replace the message-selection block**

Replace lines 104–119 (the `load_messages` + `< MIN_MESSAGES` + `relevant`/`start_idx`/`context_msgs` block) with:

```rust
    // 1. Load messages (whole session — the watermark filter is applied purely).
    let rows = crate::db::sessions::load_messages(db, session_id, None).await?;
    if rows.len() < MIN_MESSAGES {
        return Ok(());
    }

    // 1b. Incremental gate: only NEW user/assistant messages since the session
    // watermark, and only once ≥ MIN_NEW_MESSAGES have accumulated (spec §2).
    let watermark = crate::db::sessions::get_last_extracted_at(db, session_id).await?;
    let Some(context_msgs) = select_new_messages(&rows, watermark) else {
        return Ok(()); // not enough new material yet — wait for the next turn
    };
    // Newest included message → the watermark to persist on success.
    let new_watermark = context_msgs.last().map(|m| m.created_at);
```

Keep the existing conversation-building loop (lines ~121–137) — it already iterates `context_msgs` and skips non-user/assistant, which is now a no-op filter but harmless. Leave it unchanged.

- [ ] **Step 2: Advance the watermark on success**

At the END of `extract_and_save_inner`, immediately before the final `Ok(())` (after events/facts/emotion/initiative have been saved), add:

```rust
    // Advance the watermark ONLY here — reached only when every `?` step above
    // succeeded. A failure earlier returns Err and leaves the watermark, so the
    // same span is retried next turn (spec §2.1).
    if let Some(ts) = new_watermark {
        if let Err(e) = crate::db::sessions::set_last_extracted_at(db, session_id, ts).await {
            tracing::warn!(agent = agent_name, error = %e, "failed to advance extraction watermark");
        }
    }
    Ok(())
```

(If the fn currently ends with an implicit `Ok(())` or a different tail expression, adapt so the watermark advance runs on the success path and the fn still returns `Ok(())`. A `set_last_extracted_at` failure is logged, not propagated — the extraction already succeeded; a missed advance just re-extracts one span next turn, bounded by Batch C1 decay.)

- [ ] **Step 3: Build + clippy (whole crate)**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: clean — `select_new_messages` is now used (no `dead_code`).

- [ ] **Step 4: Commit**

```bash
git add crates/opex-core/src/agent/knowledge_extractor.rs
git commit -m "feat(soul): incremental extraction — read/advance session watermark (#A)"
```

---

## Post-implementation (controller, after whole-branch review + user approval)

- Server test session (throttled): `cargo test -p opex-db last_extracted_at_roundtrips` + `cargo test -p opex-core --bin opex-core incremental_tests` + `cargo clippy --all-targets -D warnings`.
- Deploy: throttled release build + `server-deploy.sh --skip-build` (migration 085 syncs to `~/opex/migrations` and auto-applies on core restart — verify `\d sessions` shows `last_extracted_at`).
- E2E smoke: run a multi-turn Telegram session; confirm `sessions.last_extracted_at` advances and event/fact rows for that session grow sub-linearly vs turn count (not ~10/turn).
