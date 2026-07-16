# Soul Audit Fix-Wave — Batch R (Rust) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the confirmed Rust-side defects from the 2026-07-16 soul audit: (1) a malformed `events[]` item no longer aborts the whole extraction; (2) a hung reflection cycle no longer wedges an agent's reflection lock forever; (3) three small safety/observability gaps (biography-safe `wipe_agent_memory`, reject initiative on base agents at config-validate time, log the silent initiative gate).

**Architecture:** Four small, independent edits across `knowledge_extractor.rs`, `soul/reflection.rs`, `opex-db/memory_queries.rs`, `config/mod.rs`, and `initiative/{tick,day_plan}.rs`. No schema change, no new deps.

**Tech Stack:** Rust 2024, tokio, serde, sqlx. rustls-tls only.

## Global Constraints

- Rust + rustls-tls only — no new external dependency.
- Do NOT touch `docker/docker-compose.yml` or anything under `docs/testing/`.
- Do NOT push, do NOT deploy — controller runs server tests + deploy after review, on explicit user approval.
- Windows dev host cannot run the bin-target / `#[sqlx::test]` suite — authority is the Linux server. Local gate: `cargo check -p <crate> --all-targets` + `cargo clippy -p <crate> --all-targets -- -D warnings`.
- **NO `Co-Authored-By` / Claude attribution trailer in ANY commit** — the user forbids it. Commit message = subject line only (+ body if useful), never an attribution trailer.
- Preserve existing behaviour outside the named defect — no drive-by refactors.
- Source: soul audit findings (extractor Important #1; reflection Critical C2; retrieval Important #2; initiative Critical C1 + Important I1).

## File Structure

- `crates/opex-core/src/agent/knowledge_extractor.rs` — events fail-soft (Task 1).
- `crates/opex-core/src/agent/soul/reflection.rs` — run_cycle overall timeout (Task 2).
- `crates/opex-db/src/memory_queries.rs`, `crates/opex-core/src/config/mod.rs`, `crates/opex-core/src/agent/initiative/{tick,day_plan}.rs` — three small fixes (Task 3).

---

### Task 1: Events fail-soft (one bad event item no longer aborts extraction)

**Files:**
- Modify: `crates/opex-core/src/agent/knowledge_extractor.rs` (`ExtractedKnowledge.events` field ~line 43-44; the events consumption in `extract_and_save_inner` ~line 185-190; a small `#[cfg(test)]` test)

**Background:** `ExtractedKnowledge.events: Vec<EventItem>` deserializes as part of the single atomic `serde_json::from_value::<ExtractedKnowledge>` (`parse_extraction`). One event object missing `text` (or wrong-typed `importance`) fails the whole parse, aborting facts/outcomes/feedback/open_items/emotion too. The `emotion` field already dodges this by being `Option<serde_json::Value>` mapped fallibly later (see the comment at lines 50-57). Apply the same pattern to `events`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` mod in `knowledge_extractor.rs` (there is already a tests module — add there):

```rust
    #[test]
    fn one_malformed_event_item_does_not_drop_the_rest() {
        // events[] with a good item, a malformed item (missing "text"), and another good item.
        let json = serde_json::json!({
            "user_facts": ["fact A"],
            "events": [
                {"text": "good 1", "importance": 7},
                {"importance": 5},                      // malformed: no "text"
                {"text": "good 2"}                      // importance defaults
            ]
        });
        let extracted: ExtractedKnowledge = serde_json::from_value(json).expect("payload must parse despite a bad event");
        // Facts survived (the whole parse didn't abort).
        assert_eq!(extracted.user_facts, vec!["fact A".to_string()]);
        // The per-item map keeps the 2 valid events, drops the malformed one.
        let events = map_event_items(extracted.events);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].text, "good 1");
        assert_eq!(events[1].text, "good 2");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p opex-core --bin opex-core one_malformed_event_item -- --nocapture` (SERVER; local `cargo check --all-targets` shows the compile error)
Expected: FAIL — `events` is `Vec<EventItem>` so the payload doesn't parse, and `map_event_items` is undefined.

- [ ] **Step 3: Change the field to `Vec<serde_json::Value>` + add the mapper**

In `ExtractedKnowledge` (line 43-44), change:

```rust
    #[serde(default)]
    events: Vec<EventItem>,
```

to:

```rust
    /// Kept as raw `serde_json::Value` (not `EventItem`) so one malformed event
    /// object (missing `text`, wrong-typed `importance`) can NEVER fail the
    /// top-level parse of the whole extraction payload — same fail-soft rule as
    /// `emotion` above (spec §5). The per-item best-effort mapping into
    /// `EventItem` happens in `map_event_items`, dropping only the bad items.
    #[serde(default)]
    events: Vec<serde_json::Value>,
```

Add a free function near `select_events` (or `EventItem`):

```rust
/// Best-effort map raw event JSON values into `EventItem`, dropping any item
/// that fails to deserialize (fail-soft: a single bad event must not lose the
/// others). A dropped item is logged at debug.
pub(crate) fn map_event_items(raw: Vec<serde_json::Value>) -> Vec<EventItem> {
    raw.into_iter()
        .filter_map(|v| match serde_json::from_value::<EventItem>(v) {
            Ok(item) => Some(item),
            Err(e) => {
                tracing::debug!(error = %e, "dropping malformed extracted event item");
                None
            }
        })
        .collect()
}
```

- [ ] **Step 4: Use the mapper at the events consumption site**

In `extract_and_save_inner`, the block that saves events (~line 185-190) currently reads:

```rust
    if soul_deps.cfg.enabled && !extracted.events.is_empty() {
        let intensity = appraised.as_ref().map(|a| a.intensity);
        let n = save_events(
            session_id, agent_name, memory_store, &soul_deps.cfg, extracted.events,
            intensity, soul_deps.emotion.intensity_importance_k,
        ).await;
        ...
    }
```

Change it to map first (and gate on the MAPPED vec being non-empty):

```rust
    let events = map_event_items(std::mem::take(&mut extracted.events));
    if soul_deps.cfg.enabled && !events.is_empty() {
        let intensity = appraised.as_ref().map(|a| a.intensity);
        let n = save_events(
            session_id, agent_name, memory_store, &soul_deps.cfg, events,
            intensity, soul_deps.emotion.intensity_importance_k,
        ).await;
        ...
    }
```

(`std::mem::take` avoids a borrow/clone; `extracted` must be `mut` — it already is, since `emotion.take()` is called on it earlier. If `save_events`' signature types the `events` param as `Vec<EventItem>`, it is unchanged — you're passing the mapped `Vec<EventItem>`.)

- [ ] **Step 5: Run tests + local gate**

Run (SERVER): `cargo test -p opex-core --bin opex-core one_malformed_event_item -- --nocapture` → PASS.
Local: `cargo check -p opex-core --all-targets` + `cargo clippy -p opex-core --all-targets -- -D warnings` → clean.

- [ ] **Step 6: Commit** (NO Co-Authored-By trailer)

```bash
git add crates/opex-core/src/agent/knowledge_extractor.rs
git commit -m "fix(soul): fail-soft event parsing — one bad events[] item no longer aborts extraction"
```

---

### Task 2: Reflection `run_cycle` overall timeout (no permanent lock wedge)

**Files:**
- Modify: `crates/opex-core/src/agent/soul/reflection.rs` (`maybe_reflect`, the `run_cycle` call ~line 97; a const near the other timeouts ~line 16-18)

**Background:** `maybe_reflect` acquires the per-agent `deps.runtime.lock.try_lock()` guard (line 86) held across the whole `run_cycle` call. Only the LLM calls inside are individually timed (`LLM_TIMEOUT=60s`); the DB calls have no statement timeout. If a DB call genuinely hangs, `run_cycle` never returns, the guard never releases, and EVERY later `maybe_reflect` for that agent short-circuits at the `try_lock` forever (until process restart). Wrap `run_cycle` in an overall timeout so a hang becomes a bounded failure (which then follows the normal backoff path and releases the lock when `maybe_reflect` returns).

- [ ] **Step 1: Add the const**

Near `LLM_TIMEOUT` (~line 16-18) add:

```rust
/// Overall wall-clock bound on a single reflection cycle. A cycle makes several
/// LLM calls (each capped at LLM_TIMEOUT=60s) plus DB work; this ceiling sits
/// above a legitimate multi-call cycle but converts a genuinely hung DB call
/// into a bounded failure so the per-agent reflection lock cannot wedge forever.
const CYCLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
```

- [ ] **Step 2: Wrap the `run_cycle` call**

Replace the `match run_cycle(db, agent, provider, memory_store, deps).await {` block (line 97) so the call is wrapped in `tokio::time::timeout`, mapping an elapsed timeout into the same `Err` path (backoff + notify):

```rust
    let cycle_result = match tokio::time::timeout(
        CYCLE_TIMEOUT,
        run_cycle(db, agent, provider, memory_store, deps),
    )
    .await
    {
        Ok(r) => r,
        Err(_elapsed) => Err(anyhow::anyhow!("reflection cycle timed out after {CYCLE_TIMEOUT:?}")),
    };
    match cycle_result {
        Ok(()) => {
            *deps.runtime.backoff.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (0, None);
            tracing::info!(agent, "reflection cycle complete");
        }
        Err(e) => {
            // ... UNCHANGED existing Err arm (warn + backoff increment + notify) ...
        }
    }
```

Keep the existing `Err(e)` arm body byte-identical (the warn log, backoff increment to `BACKOFF_AFTER_FAILURES`, the `paused` notify). Only the `match` subject changed from `run_cycle(...).await` to the timeout-wrapped `cycle_result`.

- [ ] **Step 3: Local gate**

Run: `cargo check -p opex-core --all-targets` + `cargo clippy -p opex-core --all-targets -- -D warnings` → clean. (No unit test — this is a timeout wrapper on an integration path; the server run + review cover it. If you can add a cheap test that `run_cycle`-times-out via a fake provider that sleeps, do so, but do not fabricate a tautological one.)

- [ ] **Step 4: Commit** (NO trailer)

```bash
git add crates/opex-core/src/agent/soul/reflection.rs
git commit -m "fix(soul): bound reflection run_cycle with CYCLE_TIMEOUT so a hung DB call can't wedge the lock"
```

---

### Task 3: Three small safety/observability fixes

**Files:**
- Modify: `crates/opex-db/src/memory_queries.rs` (`wipe_agent_memory` ~line 646)
- Modify: `crates/opex-core/src/config/mod.rs` (`validate_sections` ~line 2189+)
- Modify: `crates/opex-core/src/agent/initiative/tick.rs` (gate ~line 71) and `crates/opex-core/src/agent/initiative/day_plan.rs` (gate ~line 94)

**Background:** Three independent, low-risk fixes: (a) `wipe_agent_memory`'s `DELETE FROM memory_chunks WHERE agent_id = $1` has no `kind` filter — currently dead code, but a landmine that would violate biography-immutability if ever wired into agent-delete. (b) A `base=true` agent can carry `[agent.initiative] enabled=true` in config and pass validation, yet initiative is permanently inert for base agents — no validation error, a silent misconfiguration. (c) The initiative early-return gate logs nothing, so a misconfigured/stale-reloaded agent silently no-ops with zero signal.

- [ ] **Step 1: (a) biography-safe `wipe_agent_memory`**

In `crates/opex-db/src/memory_queries.rs` (~646), change the DELETE to spare biography (consistent with the other four hard-delete paths):

```rust
pub async fn wipe_agent_memory(db: &PgPool, agent_id: &str) -> Result<u64> {
    // Biography (kind event/reflection) is immortal via routine paths — deliberate
    // removal is the raw-SQL quarantine runbook only. This admin wipe spares it,
    // matching run_memory_decay / cleanup / reindex-purge / clear_embeddings.
    let result = sqlx::query("DELETE FROM memory_chunks WHERE agent_id = $1 AND kind = 'fact'")
        .bind(agent_id)
        .execute(db)
        .await?;
    Ok(result.rows_affected())
}
```

- [ ] **Step 2: (b) reject initiative on base agents in `validate_sections`**

Read `validate_sections` (`config/mod.rs` ~2189+) to find where it pushes section errors (it has access to `self.agent.base` and `self.agent.initiative`). Add, alongside the existing initiative cross-checks:

```rust
        if self.agent.base && self.agent.initiative.enabled {
            errs.push(
                "[agent.initiative] enabled=true is not allowed for a base agent \
                 (initiative is non-base only)".to_string(),
            );
        }
```

(Match the actual error-accumulator variable name used in `validate_sections` — it may be `errors` not `errs`. Also match how existing initiative checks reference the fields, e.g. `self.agent.initiative.enabled`.)

- [ ] **Step 3: (c) log the initiative gate**

In `crates/opex-core/src/agent/initiative/tick.rs`, the gate at ~line 71:

```rust
    if deps.is_base || !deps.cfg.enabled || deps.owner_id.is_none() {
        tracing::debug!(
            agent = agent_name, is_base = deps.is_base,
            enabled = deps.cfg.enabled, has_owner = deps.owner_id.is_some(),
            "initiative_tick gated out",
        );
        return Ok(());
    }
```

Do the SAME in `crates/opex-core/src/agent/initiative/day_plan.rs` at its equivalent gate (~line 94), with message `"day_plan_tick gated out"`. (Confirm the local variable names — `agent_name` vs `agent`, `deps` — match each file.)

- [ ] **Step 4: Local gate (both crates)**

Run: `cargo check -p opex-db --all-targets && cargo check -p opex-core --all-targets` then `cargo clippy -p opex-db --all-targets -- -D warnings && cargo clippy -p opex-core --all-targets -- -D warnings` → clean.

- [ ] **Step 5: Commit** (NO trailer)

```bash
git add crates/opex-db/src/memory_queries.rs crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/initiative/tick.rs crates/opex-core/src/agent/initiative/day_plan.rs
git commit -m "fix(soul): biography-safe wipe_agent_memory; reject initiative on base; log initiative gate"
```

---

## Post-implementation (controller, after whole-branch review + user approval)

- Server test session (throttled): `cargo test -p opex-core --bin opex-core one_malformed_event_item` + `cargo clippy --all-targets -D warnings` (opex-core + opex-db). No new migration.
- Deploy: throttled release build + `server-deploy.sh --skip-build` + restart.
- Post-deploy verify: (1) an agent whose config has `[agent.initiative] enabled=true, base=true` now fails to load with the new validation error (or confirm none exist); (2) initiative-gate debug lines appear for gated agents; (3) reflection still fires for Arty (timeout wrapper didn't break the happy path).

## NOT in this batch (deferred emotion-tuning; noted so absence isn't mistaken for a gap)

- Emotion `emotion_appraised` payload missing `mood_valence_after`/`boosted_event`, and mood-persistence coupled to `memory_store.is_available()` — both touch the just-shipped extractor hot path + emotion is default-off; deferred to a dedicated emotion-observability batch.
- Reflection-representation quota in `soul_candidates` retrieval — its own change (Step 5 of the audit follow-up).
- Drift-metric v2 (self-calibrating threshold + hysteresis) — its own design cycle.
