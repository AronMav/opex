# Soul Events as an Ephemeral Observation Stream (C1 + A) — Design

**Date:** 2026-07-15
**Status:** Approved design, pre-implementation
**Area:** `crates/opex-core/src/agent/knowledge_extractor.rs`, `agent/pipeline/finalize.rs`, `scheduler/mod.rs` (decay crons), `db/sessions.rs` + `db/memory_queries.rs`, `config/mod.rs` (SoulConfig), one `sessions` migration.
**Source:** prod diagnosis — soul `event` rows accumulate unboundedly (66 from one session); knowledge extraction re-summarizes the same 20-message window every turn and appends events with no per-session bound, and `event`/`reflection` are exempt from all decay/sweep paths.

## 1. Problem

`extract_and_save_inner` runs on **every** `Done` finalize (i.e. every turn, gated only by `MIN_MESSAGES=5`). Each run loads the whole session, takes the **last 20** user+assistant messages, and makes one LLM call producing facts + events + open_items + emotion + summary. Events (`kind='event'`, `source='soul_event:{session_id}'`) are appended, up to `max_events_per_session=10` **per run**. Two failure modes compound:

1. **Overlapping re-extraction (generation):** the same recent messages are re-summarized every turn, so near-duplicate events pile up ~linearly with turn count.
2. **No decay for events (retention):** `run_memory_decay`'s low-score DELETE and `run_memory_decay_cleanup`'s 180-day DELETE both filter `kind='fact'`, so events are **never** deleted (only their `relevance_score` decays, which `soul_retrieve` ignores — it scores by `created_at`). Events grow forever.

The user chose the **generative-agents model**: events are a **short-term episodic observation stream** that **decays**, and **reflections** (already produced by the reflection engine, `kind='reflection'`, permanent, with `lineage`) are the **durable consolidated biography**. This spec implements that as two cohesive components:

- **A — incremental extraction** (fixes generation): each run processes only *new* messages since a per-session watermark.
- **C1 — importance-weighted temporal decay of events** (fixes retention): events age out on an importance-scaled schedule; reflections stay permanent.

## 2. Component A — Incremental extraction (watermark)

### 2.1 Mechanism
- Add a per-session watermark `last_extracted_at TIMESTAMPTZ NULL` on `sessions` (NULL = never extracted).
- In `extract_and_save_inner`, instead of "last `MAX_CONTEXT_MESSAGES` of all user+assistant messages", select user+assistant messages with `created_at > last_extracted_at` (NULL → from the start), then still cap to the last `MAX_CONTEXT_MESSAGES` of *those* (guards a huge backlog).
- **`MIN_NEW_MESSAGES` gate** (const, default **4**): if fewer than this many *new* user+assistant messages exist since the watermark, return early WITHOUT extracting (wait for more to accumulate). This batches extraction — far fewer runs, non-overlapping windows.
- On a **successful** save, advance `last_extracted_at` to the `created_at` of the newest message included. On any failure (LLM timeout, parse error, save error), do **not** advance — the same span is retried on the next turn (idempotent-enough: a rare double-extract of one span is bounded by C1 decay and the existing per-run `max_events`).

### 2.2 Scope note
Extraction runs for **all** agents (facts/emotion/summary/initiative), not only soul agents, so incremental extraction is a general extractor improvement: it also reduces duplicate `fact` rows and saves LLM tokens. The watermark column is general (`last_extracted_at`), not soul-specific. Emotion/rolling-summary/initiative operate on the new window unchanged; `reflection::maybe_reflect` (which reads `event_importance_since` a marker, independent of the extraction window) is unaffected.

### 2.3 Accepted edge
A session whose final `< MIN_NEW_MESSAGES` messages never reach the gate loses those tail observations (never extracted). `MIN_NEW_MESSAGES=4` keeps the tail small; a forced "final extraction on terminal" is a non-goal for v1 (§8).

## 3. Component C1 — Importance-weighted temporal decay of events

### 3.1 Mechanism
Events become deletable on an **age × importance** schedule (episodic memory that fades), while reflections remain exempt (durable). Add ONE event-retention sweep to the existing daily cleanup cron (`run_memory_decay_cleanup`, 08:00 UTC):

```sql
DELETE FROM memory_chunks
WHERE kind = 'event'
  AND pinned = false
  AND created_at < now() - make_interval(
        days => LEAST(importance * $per_importance_days, $max_age_days)::int);
```

- `importance` is the extractor's 1–10 rating. Retention scales linearly: trivial events (imp 2) live `2 * K` days; significant (imp 10) live `10 * K` days, capped at `max_age_days`.
- `K = event_retention_days_per_importance` (SoulConfig, default **7**) → imp 2 ≈ 14 days, imp 5 ≈ 35 days, imp 10 ≈ 70 days.
- `max_age_days` (SoulConfig, default **180**) → hard ceiling; even the most significant episodic events age out by then (their essence is preserved in reflections written far earlier).
- **Reflections are NOT touched** (no `kind='reflection'` in the predicate) — permanent durable biography.
- Age is `created_at` (matches `soul_retrieve`'s recency basis), NOT `accessed_at`.

The existing `run_memory_decay` `relevance_score` UPDATE already touches events harmlessly (retrieval ignores `relevance_score`); leave it unchanged. The existing fact-only DELETEs are **unchanged**. C1 is purely the one new event sweep above.

### 3.2 Anti-tamper invariant preserved
The deliberate biography protections stay intact: the agent-facing `memory(delete)` tool and the UI/API mutation routes (`refuse_if_biography`) continue to REFUSE `event`/`reflection` rows. C1 adds retention only to the **system decay cron** — an operator/background sweep, never an agent- or user-triggered delete. This is the same distinction the current design already draws (system decay vs. agent-facing immutability); C1 extends the system sweep to events, nothing else.

### 3.3 Retention ⟷ reflection invariant
Event min-retention (`≥ 2*K ≈ 14 days`) must exceed reflection max-latency. `should_reflect` fires when `session_capped_sum(event_importance_since marker) > reflection_threshold (150)` past a cooldown — reached within a session's worth of events (minutes–hours). So every significant event is consolidated into a durable reflection long before it can be swept. Documented as a design invariant; the defaults satisfy it with a wide margin.

## 4. Data flow (after)

```text
turn Done → spawn_knowledge_extraction
  → extract_and_save_inner:
      load user+assistant msgs WHERE created_at > sessions.last_extracted_at
      if new_count < MIN_NEW_MESSAGES: return (no extract, no watermark move)
      else: LLM extract on the new span (cap MAX_CONTEXT_MESSAGES)
            save facts/events/emotion/summary/initiative
            on success → sessions.last_extracted_at = max(created_at of span)
  → maybe_reflect (unchanged): importance since marker > threshold
            → run_cycle → durable reflection rows (lineage) + SELF.md

daily 08:00 cron (run_memory_decay_cleanup):
  DELETE facts (unchanged) ; DELETE events WHERE age > importance*K capped max_age
  reflections never deleted

soul_retrieve (unchanged): created_at recency × importance × relevance
  → recent events surface; aged events already deleted → gone; reflections durable
```

## 5. Config + migration

- **Migration** `085_sessions_last_extracted_at.sql` (next sequential; latest is `084_profiles.sql`): `ALTER TABLE sessions ADD COLUMN last_extracted_at TIMESTAMPTZ` (nullable, no default → NULL = never extracted). History-preserving, additive.
- **SoulConfig** (new fields, defaults chosen so behaviour is safe out of the box):
  - `event_retention_days_per_importance: f64` (default `7.0`) — `K` in §3.1.
  - `event_max_age_days: i64` (default `180`) — cap in §3.1.
  - Validation: both `> 0`; `event_max_age_days ≤ 3650` (sanity).
- **Const** in `knowledge_extractor.rs`: `MIN_NEW_MESSAGES: usize = 4`.
- No change to `max_events_per_session` (still the per-run cap; with A it now bounds a single non-overlapping span, which is its honest meaning).

## 6. Components / boundaries

- `knowledge_extractor.rs` — owns A: reads `last_extracted_at`, applies `MIN_NEW_MESSAGES`, advances the watermark on success. New helper `load_messages_since(db, session_id, after: Option<DateTime>)` in `db/sessions.rs` (or extend `load_messages`), plus `set_last_extracted_at(db, session_id, ts)`.
- `scheduler/mod.rs` — owns C1: the new event sweep inside `run_memory_decay_cleanup`, reading the two SoulConfig retention values (threaded in, or read from a passed config). Note the cleanup cron currently takes only `db`; it will need the retention params — pass them at registration (`add_memory_decay_cleanup`) from config.
- `config/mod.rs` — the three SoulConfig fields + validation.
- `db/memory_queries.rs` — if a dedicated `delete_expired_events(db, k, max_age)` query reads cleaner than inline SQL in the cron, put it here (mirrors existing extracted query pattern; keeps the cron thin and unit-testable via the sqlx soul-guard test).

## 7. Testing

**A (incremental):**
- Watermark advances to newest included message on success; unchanged on failure (inject a failing save/parse).
- `MIN_NEW_MESSAGES` gate: `< N` new messages → early return, no extraction, watermark unchanged.
- NULL watermark (first run) → extracts from start, sets watermark.
- Only messages after the watermark are fed to the prompt (assert the conversation string excludes already-extracted messages).

**C1 (event decay):** `#[sqlx::test]` (Linux/x86_64, like the existing `soul_guard_tests`):
- Insert events at varied `(importance, created_at)`; run the cleanup; assert:
  - low-importance old event (e.g. imp 2, 20 days) is deleted; high-importance same-age (imp 10, 20 days) survives (20 < 10*7).
  - any event past `max_age_days` is deleted regardless of importance.
  - a `reflection` row eligible by age/score is NEVER deleted (extends the existing `decay_cleanup_spares_soul_kinds` intent — now facts+events sweep, reflections spared).
- Reuse/extend `run_memory_decay_cleanup` direct-call pattern (no SQL copy).

**Anti-tamper (unchanged, guard against regression):** confirm the agent `memory(delete)` + `refuse_if_biography` paths still refuse `event` rows (existing tests cover this; add an assertion if a gap exists).

## 8. Non-goals

- No "final extraction on session terminal" for the sub-`MIN_NEW_MESSAGES` tail (§2.3) — accepted small loss in v1.
- No semantic dedup of events on write (C1 decay + incremental generation make it unnecessary).
- No change to reflection triggering/cadence, `soul_retrieve` scoring, or SELF.md.
- No consumption-driven prune (C2 was rejected in favour of C1 temporal decay).
- No change to fact decay, the anti-tamper guards, or `max_events_per_session`.

## 9. Implementation batching

Two independent batches (own plans, own review/deploy), A first:

- **Batch A — incremental extraction:** migration (`last_extracted_at`) + `db/sessions.rs` helpers + `extract_and_save_inner` rewrite + `MIN_NEW_MESSAGES` + tests. Deployable on its own (immediately cuts event/fact/token volume).
- **Batch C1 — event decay:** SoulConfig fields + `run_memory_decay_cleanup` event sweep (+ optional `db/memory_queries.rs` query) + sqlx tests. Deployable on its own (bounds existing + future events).
