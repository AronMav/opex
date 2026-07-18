# Session Resilience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any disruption (tab switch, transport drop, provider error, cancel, slow compaction) leaves the session resumable; writing into it revives it. Spec: `docs/superpowers/specs/2026-07-18-session-resilience-design.md`.

**Architecture:** Six surgical workstreams on the existing pipeline — no engine rewrite. Core: interruption semantics (`interrupted`, not `failed`), fork-fallback, provider cooldown registry, budgeted fail-open compaction. UI: tab-lifecycle reattach, persisted-id regenerate, participant hygiene.

**Tech Stack:** Rust (axum/tokio/sqlx), Next.js 16 + vitest, PostgreSQL 17.

## Global Constraints

- **Invariant G1:** a session ends `failed` ONLY when a genuine engine/LLM error was recorded (`session_failures` row from the explicit failure path). Every forced termination (cancel-grace, transport death, supersede, shutdown, guard backstop without recorded error) ends `interrupted` with partial persisted.
- **Invariant G2 (before-first-token):** provider swap happens only between LLM calls / before the current call streamed any token. Never mid-stream.
- **Invariant G3 (fail-open compaction):** compaction may never fail or stall a turn. Budget 15s; on timeout/error the turn proceeds uncompacted. The existing reactive overflow-retry (`llm_call.rs` force-compact-and-retry-once) stays as safety net.
- **Invariant G4:** UI never disposes a live stream on `document.hidden`; reattach uses `settleMessages: false`; the streaming message id must be stable across reattach.
- Fork endpoint never 500s on an unknown `branch_from_message_id` — falls back to the session's last persisted message.
- sqlx tests are server-authoritative (`#[sqlx::test(migrations = "../../migrations")]`, run on the server with the :5434 test Postgres); Windows runs `cargo check --all-targets` only.
- Already verified as PRESENT (do not re-implement): full fallback chain walking (`fallback_chain_idx` → `text[1+idx]`, behaviour.rs), interactive layers (`for_interactive`), server-side `ExplicitResume`, StreamRegistry GET-replay, SSE cancel-grace pre-mark.

---

### Task 1: Guard backstop marks `interrupted` (WS1a)

**Files:**
- Modify: `crates/opex-core/src/agent/session_manager.rs` (Drop impl, ~line 365-445)
- Test: same file, `mod tests`

**Interfaces:**
- Consumes: `crate::db::sessions::cleanup_session_terminated(&db, sid, status, reason) -> Result<bool>` (atomic running→terminal claim, idempotent).
- Produces: Drop-while-Running semantics — `interrupted` when no failure was recorded, `failed` only when `recorded == true` (explicit failure path already finalized or is finalizing).

- [ ] **Step 1: Write the failing test** (in `session_manager.rs` tests, next to the existing guard tests around line 478+):

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn guard_drop_without_recorded_error_marks_interrupted(pool: sqlx::PgPool) {
    let session_id = create_running_session(&pool).await; // reuse the existing test helper in this mod
    {
        let _guard = SessionLifecycleGuard::new(pool.clone(), session_id)
            .with_agent("TestAgent");
        // dropped here while outcome == Running, recorded == false
    }
    // Drop spawns async cleanup — poll until terminal
    let status = wait_for_terminal_status(&pool, session_id).await;
    assert_eq!(status, "interrupted", "unrecorded early exit must be resumable, not failed");
    // forensic row still written
    let kind: Option<String> = sqlx::query_scalar(
        "SELECT failure_kind FROM session_failures WHERE session_id = $1")
        .bind(session_id).fetch_optional(&pool).await.unwrap();
    assert_eq!(kind.as_deref(), Some("guard_dropped"));
}
```

If helpers `create_running_session` / `wait_for_terminal_status` don't exist under those names, reuse/extract from the existing guard tests in this module (they already create sessions and poll for status).

- [ ] **Step 2: Run on server, verify FAIL** (expected: status is `failed`).
- [ ] **Step 3: Implement** — in `Drop for SessionLifecycleGuard`, compute the claim status from `already_recorded`:

```rust
// G1: an unrecorded early exit (abort, panic, transport death) is a
// forced termination, not an engine error — claim `interrupted` so the
// session stays resumable. Only a recorded genuine failure keeps `failed`.
let claim_status = if already_recorded { "failed" } else { "interrupted" };
match crate::db::sessions::cleanup_session_terminated(
    &db, sid, claim_status, "guard dropped (early exit)",
).await {
```

Keep the forensic `session_failures` insert (kind `guard_dropped`) exactly as is (it is already skipped when `already_recorded`).

- [ ] **Step 4: Check the existing tests in this module that assert `failed` on guard drop** — update their expectations to `interrupted` where the scenario is an unrecorded drop. Do NOT weaken tests where the explicit failure path recorded an error first.
- [ ] **Step 5: `cargo check --all-targets` clean locally; commit** `fix(sessions): guard backstop claims interrupted, not failed (G1)`.

### Task 2: Channel teardown + supersede + shutdown land `interrupted` (WS1b)

**Files:**
- Investigate/Modify: `crates/opex-core/src/gateway/handlers/channel_ws/` (dispatcher.rs, reader.rs), `crates/opex-core/src/agent/pipeline/finalize.rs`, `crates/opex-core/src/gateway/stream_registry.rs:131-141`
- Test: `crates/opex-core/src/agent/pipeline/finalize.rs` (or session_manager tests) + a supersede test near `stream_registry.rs`

**Interfaces:**
- Consumes: `cleanup_session_terminated` (same as Task 1); `CancellationToken` cooperative cancel; `finalize::finalize` outcome classification.
- Produces: all three paths end `interrupted` with partial persisted.

- [ ] **Step 1: Investigate.** Trace what happens to an in-flight turn when: (a) the channel WS connection dies mid-turn (reader/writer/dispatcher teardown — does anything abort the engine task, or does it run to completion? Today's prod evidence: `WebSocket client disconnected` immediately followed by `guard dropped while Running`); (b) a new web stream supersedes (`stream_registry.rs:131` cancels the old token — confirm the cancelled engine reaches `finalize` and what status it writes); (c) shutdown drain. Write findings into the task report.
- [ ] **Step 2: Write failing tests** for whichever of the three paths does NOT land `interrupted`. Supersede test sketch (cooperative cancel → finalize):

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn cancelled_turn_finalizes_interrupted(pool: sqlx::PgPool) {
    // Drive finalize with SessionOutcome for a cancelled run — assert the
    // session row lands 'interrupted' and the partial assistant text (if any)
    // is persisted. Reuse finalize's existing test harness/mocks in this module.
}
```

- [ ] **Step 3: Fix.** For any teardown that hard-drops the engine future without finalize: pre-mark with `cleanup_session_terminated(&db, sid, "interrupted", "<reason>")` BEFORE the drop/abort — mirror of the SSE cancel-grace pre-mark (`sse_converter.rs:210-224`). Reasons: `"channel_disconnected"`, `"superseded"`, `"shutdown"`. With Task 1 in place, the guard backstop already converts unrecorded drops to `interrupted` — this task makes the reason explicit and persists partials where a pre-mark point exists.
- [ ] **Step 4: Run tests on server, verify PASS; commit** `fix(sessions): channel/supersede/shutdown teardown lands interrupted with explicit reasons (G1)`.

### Task 3: Fork endpoint fallback — «Повторить» never dead-ends (WS2a)

**Files:**
- Modify: `crates/opex-core/src/gateway/handlers/sessions.rs` (fork handler, `POST /api/sessions/{id}/fork`)
- Test: same file

**Interfaces:**
- Produces: fork response additionally reports the actual branch point: `{"branch_from_message_id": "<uuid>", ...existing fields}`.

- [ ] **Step 1: Read the fork handler.** Find where `branch_from_message_id` from the request body is used in the INSERT that today violates `messages_branch_from_message_id_fkey`.
- [ ] **Step 2: Failing test:**

```rust
#[sqlx::test(migrations = "../../migrations")]
async fn fork_with_unknown_branch_id_falls_back_to_last_message(pool: sqlx::PgPool) {
    let (session_id, last_msg_id) = seed_session_with_messages(&pool, 3).await;
    let bogus = uuid::Uuid::new_v4();
    let result = fork_session_inner(&pool, session_id, Some(bogus), /*...*/).await;
    let forked = result.expect("fork must not fail on unknown branch id");
    assert_eq!(forked.branch_from_message_id, last_msg_id,
        "unknown branch id must fall back to the last persisted message");
}
```

(Adapt to the handler's actual internal function; if logic is inline in the axum handler, extract a testable `fork_session_inner` — that extraction is in scope.)

- [ ] **Step 3: Implement** — before using the requested id:

```rust
// WS2: the UI may hold an optimistic (never-persisted) message id after a
// failed turn. An unknown id must not 500 the retry path — fall back to the
// session's newest persisted message as the branch point.
let branch_id = match requested_branch_id {
    Some(id) => {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE id = $1 AND session_id = $2)")
            .bind(id).bind(session_id).fetch_one(&db).await?;
        if exists { Some(id) } else {
            tracing::warn!(session_id = %session_id, requested = %id,
                "fork branch id not found — falling back to last persisted message");
            last_message_id(&db, session_id).await?
        }
    }
    None => last_message_id(&db, session_id).await?,
};
```

- [ ] **Step 4: Server tests PASS; commit** `fix(sessions): fork falls back to last persisted message on unknown branch id`.

### Task 4: Provider cooldown registry (WS4)

**Files:**
- Create: `crates/opex-core/src/agent/provider_cooldown.rs`
- Modify: `crates/opex-core/src/agent/mod.rs` (module decl), `crates/opex-core/src/agent/pipeline/execute.rs` (fallback-layer insertion points), `crates/opex-core/src/agent/engine/loop_detector_integration.rs` (`create_fallback_provider` skip-cooled), engine state wiring (one shared registry per process — `AppState` or a `static`)
- Test: `provider_cooldown.rs` unit tests + one behaviour test in `behaviour.rs`/`execute.rs` tests

**Interfaces:**
- Produces:

```rust
/// Process-wide provider cooldown registry (G2 scope: consulted between
/// calls only). In-memory by design — see spec §7.
pub struct ProviderCooldowns { map: dashmap::DashMap<String, std::time::Instant> }

impl ProviderCooldowns {
    pub fn new() -> Self;
    /// Record a failover-worthy failure: cooldown_until = now + cooldown_duration(class).
    pub fn record_failure(&self, provider_name: &str, class: &LlmErrorClass);
    /// Clear on success (primary healed).
    pub fn record_success(&self, provider_name: &str);
    /// True while now < cooldown_until (expired entries are evicted lazily).
    pub fn is_cooled(&self, provider_name: &str) -> bool;
}
```

- Consumes: `error_classify::cooldown_duration` (exists, currently unconsumed for this purpose), profile chain resolution (`text[1 + chain_idx]`).

- [ ] **Step 1: TDD the registry module** (pure unit tests, no DB): record RateLimit → cooled for 60s (use injected clock or short test durations via `#[cfg(test)]` constructor taking `Duration` overrides); success clears; expiry un-cools.
- [ ] **Step 2: Implement the module** (~60 LoC + tests). Use `dashmap` (already a workspace dep — verify; if not, use `std::sync::RwLock<HashMap>`).
- [ ] **Step 3: Wire recording** — at the fallback layer's failover point in `execute.rs` (where `is_failover_worthy` triggers the swap), call `cooldowns.record_failure(current_provider_name, &class)`; on any successful LLM call, `record_success(provider_name)`.
- [ ] **Step 4: Wire skipping** — two places: (a) `create_fallback_provider(chain_idx)`: skip chain entries whose provider `is_cooled` (advance `chain_idx` past them, preserving the exhaustion semantics); (b) turn start: if the PRIMARY (`text[0]`) is cooled and at least one non-cooled reserve exists, start the turn on the first non-cooled entry (log + `session_timeline` note `provider_cooldown_skip`). Primary self-heals automatically: each new turn re-checks `is_cooled`, expired cooldown → primary again (openclaw probe semantics without a timer).
- [ ] **Step 5: Behaviour test:** chain `[P0, P1, P2]`, P0 cooled → turn resolves P1 first; P1 fails failover-worthy → records cooldown → P2; after cooldown expiry P0 selected again.
- [ ] **Step 6: `cargo check --all-targets`; commit** `feat(providers): process-wide cooldown registry — cooled entries skipped in chain resolution (WS4)`.

### Task 5: Compaction — budgeted, fail-open, off the synchronous path (WS5)

**Files:**
- Modify: `crates/opex-core/src/agent/history.rs` (`compact_if_needed_inner`), `crates/opex-core/src/agent/pipeline/context.rs` (`compact_messages`), call-site relocation: `crates/opex-core/src/agent/context_builder.rs` / `crates/opex-core/src/agent/pipeline/bootstrap.rs` → `crates/opex-core/src/agent/pipeline/execute.rs`
- Test: `history.rs` unit tests

**Interfaces:**
- Produces: `pub const COMPACTION_BUDGET: Duration = Duration::from_secs(15);` in `history.rs`; compaction returns `Ok(None)` (skip) on budget/provider failure instead of propagating.

- [ ] **Step 1: Locate the synchronous call site.** Prod evidence: `context window threshold reached, compacting` + a blocking `providers::openai::chat` call appear INSIDE the `http_request` span (before 202). Trace from `ContextBuilder::build` / bootstrap which path invokes `compact_if_needed` synchronously. Record findings in the report.
- [ ] **Step 2: Failing unit test** (mock provider that sleeps > budget):

```rust
#[tokio::test(start_paused = true)]
async fn compaction_budget_exceeded_is_fail_open() {
    let slow = SlowMockProvider::new(Duration::from_secs(120)); // chat() sleeps
    let mut messages = make_compactable_messages(); // existing helper, history.rs:1209
    let before = messages.clone();
    let result = compact_if_needed(&mut messages, &slow, None, 100, 2, None).await;
    assert!(matches!(result, Ok(None)), "budget overrun must SKIP, not error");
    assert_eq!(messages.len(), before.len(), "messages untouched on skip");
}
```

- [ ] **Step 3: Implement budget + fail-open** in `compact_if_needed_inner` — wrap the LLM summarize call:

```rust
// G3: compaction may never stall or fail the turn. Budget the LLM call and
// treat ANY failure (timeout, provider error, exhausted chain) as a skip —
// the reactive overflow-retry in llm_call.rs remains the safety net.
let summary = match tokio::time::timeout(COMPACTION_BUDGET, active_provider.chat(/*…*/)).await {
    Ok(Ok(s)) => s,
    Ok(Err(e)) => {
        tracing::warn!(error = %e, "compaction LLM call failed — proceeding uncompacted (fail-open)");
        return Ok(None);
    }
    Err(_) => {
        tracing::warn!(budget_secs = COMPACTION_BUDGET.as_secs(),
            "compaction budget exceeded — proceeding uncompacted (fail-open)");
        return Ok(None);
    }
};
```

- [ ] **Step 4: Relocate the synchronous call site** found in Step 1 so the send-POST returns 202 without waiting for compaction: the pre-turn compaction runs inside the detached engine phase (`pipeline::execute` before the first LLM call — the in-loop `compact_messages` machinery already exists there; prefer reusing it and DELETING the synchronous bootstrap-path call over adding a new phase). Preserve cron/isolated behavior.
- [ ] **Step 5: Full test sweep** (`cargo check --all-targets` + existing history/compaction tests green locally, sqlx on server); **commit** `feat(compaction): 15s budget, fail-open, off the synchronous POST path (G3)`.

### Task 6: UI — tab lifecycle reattach, no dispose on hidden (WS3)

**Files:**
- Modify: `ui/src/stores/streaming-renderer.ts`
- Test: `ui/src/stores/__tests__/streaming-renderer-visibility.test.ts` (create)

**Interfaces:**
- Consumes: existing `connect(agent, {isRetry, settleMessages})` single reconnect path; `VISIBILITY_STALE_MS = 15_000`; `boundary_message_id` stability from the sync envelope.
- Produces: reattach listeners on `visibilitychange` (visible), `online`, `pageshow`; explicit guarantee that the hidden branch performs NO dispose/settle.

- [ ] **Step 1: Failing vitest:** simulate an active stream (`connectionPhase: "streaming"`), fire `document` `visibilitychange` with `document.hidden === true` → assert store state untouched (same `streamGeneration`, same messages, phase unchanged). Then `hidden === false` after staleness window → assert `connect` called with `{settleMessages: false}` and the streaming message id unchanged. Mock `connect` at module seam like the existing `streaming-renderer-reconnect.test.ts` does.
- [ ] **Step 2: Implement:** in the visibility handler, ensure the hidden branch is a no-op (remove/guard any dispose/reset). Add `online` + `pageshow` listeners funnelled into the SAME staleness-checked reattach used for `visibilitychange→visible` (one code path, three triggers). All listeners removed in `dispose()`.
- [ ] **Step 3: Repro-check the reported reset:** while a turn streams, remount/agent-switch back to the same agent (navigation action path) must resume the live turn via GET-replay, not clear it. If the navigation path force-settles the streaming message, gate that on «different session» only. Add a vitest for return-to-same-session.
- [ ] **Step 4: `cd ui && npx vitest run src/stores` green; commit** `fix(ui): tab hide never disposes a live stream; reattach on visible/online/pageshow (G4)`.

### Task 7: UI — «Повторить» uses persisted ids; send revives terminal sessions (WS2b)

**Files:**
- Modify: `ui/src/stores/chat/actions/stream-control.ts` (regenerate), `ui/src/stores/chat/actions/composer.ts` (send)
- Test: existing `ui/src/__tests__/regenerate-model.test.ts` + new cases

**Interfaces:**
- Consumes: Task 3's fork response field `branch_from_message_id`; message `status` («complete» = persisted) in chat store.

- [ ] **Step 1: Failing vitest:** regenerate after a failed turn where the leaf message is client-only (status not `complete` / id not present in history) → assert the fork/regenerate request carries the last PERSISTED message id (or omits the branch id, deferring to the server fallback from Task 3).
- [ ] **Step 2: Implement:** in `regenerate()`, resolve the branch id by walking back to the newest message known-persisted; if none — omit the field. In `composer` send: when the active session has run_status `failed`/`interrupted`/`done`, KEEP `session_id` in the POST (no forced new session). Verify against the current code — if already correct, prove it with the test and report.
- [ ] **Step 3: vitest green; commit** `fix(ui): regenerate uses persisted branch ids; send revives terminal sessions (WS2)`.

### Task 8: UI — participant hygiene (WS6)

**Files:**
- Modify: the participant/divider rendering (locate: `AgentTransitionDivider` and message header rendering in `ui/src/components/chat/` and `ui/src/app/(authenticated)/chat/MessageItem.tsx`)
- Test: component vitest

- [ ] **Step 1: Failing vitest:** a message/divider whose `agentId` matches `/^[0-9a-f]{8}-[0-9a-f]{4}-/i` (UUID shape) or is not in the known-agents list renders the localized generic label (add i18n keys `chat.unknown_agent` = "Агент" / "Agent"), never the raw UUID.
- [ ] **Step 2: Implement** a `displayAgentName(agentId, knownAgents, t)` helper used by both render sites; UUID/unknown → `t("chat.unknown_agent")`.
- [ ] **Step 3: vitest green + `npx tsc --noEmit`; commit** `fix(ui): unknown participants render generic label, never a raw UUID (WS6)`.

### Task 9: Deploy + server-authoritative verification + live smoke (controller)

- [ ] **Step 1:** Push → `server-deploy.sh` (standing approval covers this execution). UI: `deploy-ui.sh` (server-deploy swaps only Rust — см. reference_deploy_gaps).
- [ ] **Step 2:** Server sqlx sweep: `DATABASE_URL=postgres://opex_test:opex_test@127.0.0.1:5434/opex_test CARGO_BUILD_JOBS=4 nice ionice -c3 cargo test -p opex-core --bin opex-core -- session_manager finalize fork provider_cooldown history` — all green.
- [ ] **Step 3: Live smoke (G1/WS2):** start a real turn on a scratch agent → kill the transport mid-turn → session ends `interrupted` with partial persisted → POST a follow-up with the same session_id → same session revives (ExplicitResume) and completes. Check `journalctl` for zero panics.
- [ ] **Step 4: Live smoke (WS3/WS5):** browser: send a long prompt, switch tab 20s, return → message intact/live; send-POST latency < 3s on a session with large history.
- [ ] **Step 5:** Update ledger + memory; report final status.

---

## Self-review notes

- Spec coverage: WS1→T1+T2, WS2→T3+T7, WS3→T6, WS4→T4, WS5→T5, WS6→T8, testing/smoke→T9. Invariants G1-G4 carried into Global Constraints.
- Type consistency: `cleanup_session_terminated(&db, sid, &str, &str)` matches session_manager.rs usage; `compact_if_needed(&mut msgs, provider, compaction_provider, max_tokens, preserve_last_n, lang)` matches history.rs:49; cooldown API is self-contained in T4.
- Investigation-first steps (T2.1, T5.1) are deliberate: the exact teardown/call-site lines need live tracing; contracts and end-states are fully specified so the reviewer can gate on them.
