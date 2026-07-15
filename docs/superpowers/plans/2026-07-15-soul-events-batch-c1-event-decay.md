# Soul Events — Batch C1 (importance-weighted event decay) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> PREREQUISITE: none hard (independent of Batch A), but the spec sequences Batch A first. Either order deploys cleanly.

**Goal:** Make soul `event` rows ephemeral — the daily cleanup cron deletes events on an age × importance schedule (trivial fade in ~2 weeks, significant persist ~10 weeks, hard cap 180 days) while `reflection` rows stay permanent.

**Architecture:** Add two module consts + one `event` DELETE to `run_memory_decay_cleanup`. Retention = `created_at < now() - LEAST(importance * K, MAX_AGE)` days. Reflections are never in the predicate. The agent/UI anti-tamper guards are untouched — only this system cron gains event coverage.

**Tech Stack:** Rust 2024, sqlx (Postgres), `make_interval`. rustls-tls only — no new deps.

## Global Constraints

- Rust + rustls-tls only — no new external dependency.
- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push, do NOT deploy — controller runs server tests + deploy after review, on explicit user approval.
- Windows dev host cannot run `#[sqlx::test]` (needs live Postgres) or the bin-target suite — authority is the Linux server. Local `cargo check --all-targets` + `cargo clippy --all-targets -- -D warnings` only.
- Exact retention consts: `EVENT_RETENTION_DAYS_PER_IMPORTANCE: f64 = 7.0`, `EVENT_MAX_AGE_DAYS: i64 = 180`.
- The event sweep predicate MUST be `kind = 'event'` only — `reflection` rows are NEVER deleted (permanent durable biography). The existing fact-only DELETE stays unchanged.
- `run_memory_decay_cleanup`/`add_memory_decay_cleanup` signatures stay unchanged — the SQL reads the consts directly (retention is a global storage policy, not per-agent config; see spec §3.1 config note).
- Anti-tamper guards (`memory(delete)` tool, `refuse_if_biography` UI/API routes) are NOT modified — they still refuse event/reflection.
- Source spec: `docs/superpowers/specs/2026-07-15-soul-events-ephemeral-observation-stream-design.md` §3.

## File Structure

- `crates/opex-core/src/scheduler/mod.rs` (modify) — two consts near the decay fns (~line 1700), the event DELETE inside `run_memory_decay_cleanup` (~line 1744), and the `soul_guard_tests` update (~line 1759).

---

### Task 1: Event retention sweep in `run_memory_decay_cleanup`

**Files:**
- Modify: `crates/opex-core/src/scheduler/mod.rs` (`run_memory_decay_cleanup` ~1744–1752; consts above it; `soul_guard_tests` ~1759+)

**Interfaces:**
- Consumes: nothing new.
- Produces: `run_memory_decay_cleanup(db)` now also deletes expired events; returns the TOTAL rows deleted (facts + events).

**Background:** `run_memory_decay_cleanup` currently deletes only `kind='fact'` rows (`relevance_score < 0.1 AND accessed_at < 180 days`). The existing `soul_guard_tests::decay_cleanup_spares_soul_kinds` asserts that an `event` row survives — that assertion INVERTS under C1 (events now age out; only reflections are spared). This task adds the event sweep and rewrites that test to cover the new three-way behaviour.

- [ ] **Step 1: Rewrite the failing test**

Replace the existing `decay_cleanup_spares_soul_kinds` test in the `soul_guard_tests` module (~line 1765) with a broader one asserting: fact deleted, low-importance old event deleted, high-importance young event spared, any event past max-age deleted, reflection always spared:

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn cleanup_ages_events_by_importance_but_spares_reflections(db: PgPool) {
        // helper: insert one chunk with explicit kind/importance/created_at age.
        async fn ins(db: &PgPool, tag: &str, kind: &str, importance: f32, age_days: i64) {
            sqlx::query(
                "INSERT INTO memory_chunks \
                 (id, agent_id, content, source, pinned, scope, relevance_score, kind, importance, created_at, accessed_at) \
                 VALUES (gen_random_uuid(), 'A', $1, 'soul_event:s', false, 'private', 0.01, $2, $3, \
                         now() - make_interval(days => $4::int), now() - make_interval(days => $4::int))",
            )
            .bind(tag).bind(kind).bind(importance).bind(age_days)
            .execute(db).await.unwrap();
        }

        // fact: eligible by score/age → deleted.
        ins(&db, "fact-old", "fact", 5.0, 200).await;
        // event imp 2, 20 days: 20 > 2*7(=14) → deleted.
        ins(&db, "evt-trivial-old", "event", 2.0, 20).await;
        // event imp 10, 20 days: 20 < 10*7(=70) → SPARED.
        ins(&db, "evt-significant-young", "event", 10.0, 20).await;
        // event imp 10, 200 days: past MAX_AGE(180) → deleted.
        ins(&db, "evt-significant-ancient", "event", 10.0, 200).await;
        // reflection, ancient + low score → ALWAYS spared.
        ins(&db, "refl-ancient", "reflection", 3.0, 300).await;

        run_memory_decay_cleanup(&db).await.unwrap();

        async fn exists(db: &PgPool, tag: &str) -> bool {
            let n: i64 = sqlx::query_scalar("SELECT count(*) FROM memory_chunks WHERE content = $1")
                .bind(tag).fetch_one(db).await.unwrap();
            n > 0
        }
        assert!(!exists(&db, "fact-old").await, "old fact must be deleted");
        assert!(!exists(&db, "evt-trivial-old").await, "trivial old event must be deleted");
        assert!(exists(&db, "evt-significant-young").await, "significant young event must survive");
        assert!(!exists(&db, "evt-significant-ancient").await, "event past max-age must be deleted");
        assert!(exists(&db, "refl-ancient").await, "reflection must NEVER be deleted");
    }
```

(The old fact-delete predicate also requires `accessed_at < 180 days` and `relevance_score < 0.1`; the helper sets `accessed_at` to the same age and score 0.01, so `fact-old` at 200 days qualifies. If the `memory_chunks` INSERT needs columns not listed, add them per `\d memory_chunks`; the assertions are the contract.)

- [ ] **Step 2: Run the test to verify it fails**

Run (SERVER): `cargo test -p opex-core --bin opex-core cleanup_ages_events_by_importance -- --nocapture`
Expected: FAIL — the event assertions fail (events currently all survive) and/or the old test name is gone.

- [ ] **Step 3: Add the consts**

In `scheduler/mod.rs`, just above `run_memory_decay` (~line 1703):

```rust
/// Event retention (spec §3): a soul `event` is deleted once its age exceeds
/// `importance * EVENT_RETENTION_DAYS_PER_IMPORTANCE` days, capped at
/// `EVENT_MAX_AGE_DAYS`. Importance-weighted so trivial episodic memories fade
/// fast while significant ones persist long enough to be consolidated into a
/// (permanent) reflection. Reflections are exempt. Global storage policy, not
/// per-agent config.
const EVENT_RETENTION_DAYS_PER_IMPORTANCE: f64 = 7.0;
const EVENT_MAX_AGE_DAYS: i64 = 180;
```

- [ ] **Step 4: Add the event sweep**

In `run_memory_decay_cleanup` (~line 1744), after the existing fact DELETE, add the event DELETE and return the combined count:

```rust
pub(crate) async fn run_memory_decay_cleanup(db: &PgPool) -> Result<u64> {
    // Facts: very old, low-score (unchanged).
    let facts = sqlx::query(
        "DELETE FROM memory_chunks WHERE pinned = false AND relevance_score < 0.1 \
         AND accessed_at < now() - interval '180 days' AND kind = 'fact'",
    )
    .execute(db)
    .await?
    .rows_affected();

    // Events (spec §3): age out on an importance-weighted schedule. Reflections
    // are NOT included — they are permanent durable biography.
    let events = sqlx::query(
        "DELETE FROM memory_chunks \
         WHERE kind = 'event' AND pinned = false \
           AND created_at < now() - make_interval(days => \
                 LEAST(importance::float8 * $1::float8, $2::float8)::int)",
    )
    .bind(EVENT_RETENTION_DAYS_PER_IMPORTANCE)
    .bind(EVENT_MAX_AGE_DAYS)
    .execute(db)
    .await?
    .rows_affected();

    Ok(facts + events)
}
```

(Keep the fn doc comment; update it to mention events age out while reflections are exempt. `make_interval(days => ...::int)` — the days arg is `int`; `$2` binds `EVENT_MAX_AGE_DAYS: i64` as `int8`, cast `::float8` in the `LEAST` then the whole expression `::int` — matches the "make_interval wants int" gotcha.)

- [ ] **Step 5: Run the test to verify it passes**

Run (SERVER): `cargo test -p opex-core --bin opex-core cleanup_ages_events_by_importance -- --nocapture`
Expected: PASS.

- [ ] **Step 6: Local check + clippy**

Run: `cargo check -p opex-core --all-targets` then `cargo clippy -p opex-core --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/opex-core/src/scheduler/mod.rs
git commit -m "feat(soul): age-x-importance event decay in cleanup cron; reflections exempt (#C1)"
```

---

## Post-implementation (controller, after whole-branch review + user approval)

- Server test session (throttled): `cargo test -p opex-core --bin opex-core cleanup_ages_events_by_importance` + `cargo clippy --all-targets -D warnings`.
- Deploy: throttled release build + `server-deploy.sh --skip-build` + restart. No migration in this batch.
- Post-deploy verify: the 08:00 UTC cleanup logs a non-zero `deleted` once events age past their retention; confirm `SELECT count(*) FROM memory_chunks WHERE kind='reflection'` is unchanged by the sweep and `kind='event'` trends down over days. (Batch A already curbs new-event volume; Step-1 manual prune already bounded existing rows.)
