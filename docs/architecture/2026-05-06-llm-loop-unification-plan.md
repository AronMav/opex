# LLM-Loop Unification — Implementation Plan

> **Historical record (completed 2026-05-06).** This plan describes the
> design for deleting `engine::stream::handle_isolated` and unifying every
> LLM-loop path through `pipeline::execute` + behaviour layers. It was
> implemented in full. References below to
> `engine::stream::handle_isolated`, `chat_with_transient_retry_using`,
> `SessionManager::create_isolated`, etc., describe the **pre-refactor
> source tree** — those names no longer exist in the codebase. Kept for
> the design rationale; not maintained against the current layout.

**Status:** Completed 2026-05-06 (commits `7b4c4cd`, then cron migration in subsequent commits, dead-code purge in `6168252`).
**Owner:** Phase 67+1 (post-observability)
**Tracking artifact:** this file
**Origin:** Architecture review 2026-05-06 (`docs/architecture/2026-05-06-architecture-review.md`), top-priority recommendation #1.
**Plan revisions:** v2 (2026-05-06) after self-review found three text-vs-code drifts and two under-specified phases — see "Revision history" at the end.

## Goal

Delete `engine::stream::handle_isolated`. Funnel every LLM+tools loop in OPEX through one entry point — `pipeline::execute` — with the features that today only exist on the cron path (`fallback_provider`, `auto-continue`, `session-corruption recovery`, `tool_policy_override`, forced-final-LLM-call) re-expressed as **opt-in behaviour layers** that can be composed onto any caller (SSE, channel, cron, future agent-to-agent).

The exit criteria is twofold:

1. `engine::stream::handle_isolated` is removed from the codebase. No callers remain. The crate compiles and `cargo test` is green.
2. A live cron run on Pi exercises the new path end-to-end (one fallback-provider switch, one auto-continue nudge, one session-corruption recovery) and produces traces with the same span shape as a regular SSE chat — `pipeline.execute` → `llm.call` → `pipeline.finalize`, all under a single `trace_id`.

## Why now

Phase 66 already pushed mid-level helpers (`tool_loop_helpers`) into a shared module so the SSE pipeline and the cron RPC path share their loop bookkeeping. What didn't get unified is the **divergent feature set** — fallback/auto-continue/recovery still live only inside `handle_isolated`, which means:

- The cron-only LLM features are unavailable to the SSE caller. A user typing a message in the chat doesn't get fallback or auto-continue, even though the same engine could provide them.
- Span shape diverges. Cron sessions don't emit `pipeline.execute` / `pipeline.finalize` spans at all because they take the `handle_isolated` path; observability is lopsided.
- Drift is non-zero. Whenever a future change touches the loop ("add a new metric", "change persist semantics"), it has to be applied in two places. Phase 66 helpers reduced the surface but didn't eliminate it.

The architecture review put this as the highest-priority structural debt — "weight, not rot" — because the system works today, but every new feature pays a 2× cost until this is fixed.

## Non-goals

- **No behaviour change** for any existing caller. SSE chats get exactly the same span tree, the same DB writes, the same UI events. Cron jobs get exactly the same fallback semantics, auto-continue counter, and session-corruption recovery they had before.
- **No new feature flags** or migration knobs. The internal API changes; the external surface is preserved.
- **No performance regression.** Behaviour layers compose at construction time, not per-iteration; the runtime hot path stays the same shape.

## Architecture: composable behaviours

The unified `pipeline::execute` already takes:

```rust
pub async fn execute<S: EventSink>(
    engine: &AgentEngine,
    bootstrap_outcome: BootstrapOutcome,
    sink: &mut S,
    cancel: CancellationToken,
    compressor: &mut Compressor,
) -> anyhow::Result<ExecuteOutcome>;
```

We add a `BehaviourLayers` parameter (constructed via `none()` for the SSE path; populated via `for_cron(...)` for cron) that bundles opt-in policy. As implemented in commit `7b4c4cd`:

```rust
pub struct BehaviourLayers {
    pub fallback_provider:    Option<FallbackPolicy>,
    pub auto_continue:        Option<AutoContinuePolicy>,
    pub session_recovery:     Option<SessionRecoveryPolicy>,
    pub tool_policy_override: Option<ToolPolicyOverride>,    // wrapper, see below
    pub forced_final_call:    Option<ForcedFinalCallPolicy>, // unit struct, see below
}
```

Two intentional deviations from the first sketch of this plan:

1. **`tool_policy_override` carries a wrapper struct (`ToolPolicyOverride { policy: AgentToolPolicy }`) instead of `Option<AgentToolPolicy>` directly.** Cost: one struct definition. Benefit: a place to add per-layer metadata later without touching every call-site (e.g., `applied_at_iteration` for diagnostic spans, or a `reason: &'static str` for telemetry). This is a small forward-looking choice, not a present requirement.
2. **`forced_final_call` is `Option<ForcedFinalCallPolicy>` (unit struct) rather than a `bool`.** Cost: one extra `Option` layer for a feature with no current parameters. Benefit: identical `is_some()` predicate across all five fields makes the `execute()` body uniform — every layer guard reads the same way. If `forced_final_call` ever needs configuration (e.g., a custom prompt for the final call), the type is already in the right shape.

Each policy is a small data-only struct describing **what** the layer does, not **how** the engine calls it. The dispatch lives inside `execute()` as cheap `if let Some(...)` guards — same control-flow shape as today, just with one decision point per feature instead of an entire parallel function.

**Constructor naming.** The plan uses `BehaviourLayers::none()` everywhere it talks about "all layers off" rather than the bare `Default::default()` trait method. Both exist and are byte-equivalent (`none() { Self::default() }`); `none()` is preferred at call sites because it reads as a deliberate decision, not as accidental defaulting. A unit test (`none_equals_default`) pins the equivalence.

**Key design choice:** behaviours are **data**, not traits. Trait-object indirection here would buy us nothing (only one implementation per layer ever), would muddy the OTel span tree (one extra layer of dynamic dispatch per call), and would push the divergent features even further from the call site. A flat `BehaviourLayers` struct keeps the loop readable and the span tree clean.

## Phase breakdown

### Phase 1 — Map divergent features (read-only)

- For each of the five divergent paths (fallback / auto-continue / session-recovery / tool-policy-override / forced-final-call), produce a one-paragraph map: trigger condition, state it mutates, side effects, observability events. Store in this plan as Appendix A.
- Identify any state today carried by `handle_isolated` local variables that the unified pipeline doesn't yet have a home for — `consecutive_failures`, `using_fallback`, `did_reset_session`, `empty_retry_count`, `auto_continue_count`, `loop_nudge_count`, `context_chars`. Map each to: keep in `execute()` local state / promote to `ExecuteOutcome` / hide in a behaviour-layer struct.
- **Exit:** Appendix A is complete. No code changes.

### Phase 2 — Design `BehaviourLayers` + insertion points

- Define the four policy structs + `BehaviourLayers` parameter object in `pipeline::behaviour`.
- For each policy, identify the exact insertion point in `execute()` where it slots in. Most fit naturally:
  - `fallback_provider` — `chat_with_transient_retry_using(...)` swap inside the iteration loop's LLM call
  - `auto_continue` — after the no-tool-calls branch, before the break, push a nudge message and `continue`
  - `session_recovery` — match arm on `SessionCorruption` LLM error, reset messages, `continue`
  - `tool_policy_override` — applied once during bootstrap (already in `bootstrap.rs` for cron; just make the parameter explicit)
  - `forced_final_call` — replaces the existing `Finish { reason: "turn_limit" }` no-op return with one extra non-tools LLM call when the layer is enabled
- Produce a `BehaviourLayers::default()` constructor that returns "all layers off" — the SSE caller continues to use this and gets identical behaviour.
- Produce a `BehaviourLayers::for_cron(cfg, msg)` constructor that returns the same set of layers `handle_isolated` enables today.
- **Exit:** policy structs and `BehaviourLayers` exist as types in the crate; not yet wired into `execute()`. `cargo check` passes.

### Phase 3 — Wire `fallback_provider`

- Plumb `BehaviourLayers` through `execute()` as a `&BehaviourLayers` parameter (immutable; layers are policy, not state).
- Inside the iteration loop, replace the unconditional `chat_stream_with_deadline_retry(provider, ...)` with a small helper that consults `BehaviourLayers::fallback_provider` and switches the live provider on consecutive-failure threshold.
- Add a unit test in `pipeline::behaviour::tests` (not `pipeline::execute::tests` — the construction of `BehaviourLayers::for_cron` and the policy types is what we pin; the integration of the layer into `execute()` is exercised by the existing 1100+ test suite plus the Phase 7 Pi run).
- Cron callers continue to use `handle_isolated` for now — Phase 3 only proves the layer works inside `execute()`.
- **Exit:** new tests pass. SSE chat still produces identical traces (with the layer off, the new code path is byte-identical to the old). `cargo test` green.

### Phase 4 — Wire `auto_continue`

- Same pattern. Move `looks_incomplete()` + nudge logic out of `handle_isolated` and into `execute()` behind `BehaviourLayers::auto_continue`.
- Add unit test: provider returns "remaining steps but didn't execute them" text twice, asserts auto-continue counter increments and turn continues.
- **Exit:** test green. SSE traces unchanged.

### Phase 5 — Wire `session_recovery` + `tool_policy_override` + `forced_final_call`

- Three smaller layers, same pattern. `tool_policy_override` is essentially a no-op for `execute()` itself — `bootstrap.rs` applies it; the layer just makes the parameter explicit.
- Add focused unit tests for each.
- **Exit:** all five behaviour layers have unit-test coverage. SSE path unaffected.

### Phase 6 — Replace `handle_isolated` callers

This phase is more invasive than the first sketch suggested. `handle_isolated` does five things `bootstrap()` doesn't currently do, and the cron-style `finalize` differs from the SSE-style one in ways that need explicit migration. Split into two sub-phases:

#### Phase 6a — Extend `bootstrap` and `finalize` for the isolated case

`handle_isolated` performs these steps that `bootstrap`/`finalize` don't yet cover or differ on:

| Step (in handle_isolated) | Current state in pipeline | What Phase 6a adds |
|---|---|---|
| `SessionManager::create_isolated(name, user_id, channel)` | `bootstrap` calls `engine.build_context(msg, force_new_session=true, None, false)` which creates a session via `build_context`'s session-creation path | Verify the two paths produce identical session rows (`channel`, `user_id`, `agent_id`, `created_at`); add `BootstrapContext::isolated: bool` if any divergence requires per-path branching |
| `enrich_message_text(...)` (toolgate `/web` + `/transcribe` + `/describe`) before user-message persist | `bootstrap` already calls `enrich_message_text` for the SSE path | None — already covered |
| `sm.save_message_ex(... sender_agent_id ...)` with `agent:` prefix handling | `bootstrap` open-codes the `agent:` prefix check inside `extract_sender_agent_id`; cron path duplicates it | Move the duplicated check to `bootstrap::extract_sender_agent_id` and ensure cron's `BootstrapContext` reaches that helper |
| `compact_messages(&mut messages, None)` (model-aware budget) before loop | `bootstrap` calls `compact_messages` via `build_context`, but only conditionally based on history mode | Add unit test: empty-history isolated session triggers compaction when context_chars > budget |
| `sm.save_message_ex(... assistant ...)` post-loop, synchronous | `pipeline::finalize` writes via `save_message_ex_with_id` with pre-allocated UUID + WAL `done`/`failed`/`interrupted` lifecycle | **Behavioural change**: cron-saved assistant rows will now carry the same UUID-aligned semantics as SSE rows. Verify `messages` table queries return cron-saved rows correctly (the row shape matches; only the insert path changes) |
| Background `knowledge_extractor::extract_and_save` spawn when `messages.len() >= 5` | `pipeline::finalize` already spawns `extract_and_save` with the same threshold | None — already covered, just remove duplication in cron path |
| Returns `Result<String>` to caller | `pipeline::execute → finalize` returns `FinalizeOutcome` with `final_text: String` | Caller wrapper unwraps `final_text` from `FinalizeOutcome` to preserve the `Result<String>` API |

**Exit (6a):** `bootstrap` and `finalize` accept an isolated-style call without behaviour change for SSE. New unit test in `pipeline::bootstrap::tests` exercises `force_new_session=true, use_history=false, sender_agent_id="agent:Foo"` and asserts the resulting `BootstrapOutcome` matches what `handle_isolated` builds today (compare `messages`, `tools`, `session_id` shape).

#### Phase 6b — Migrate cron callers

- Find every caller of `handle_isolated`. As of commit `7b4c4cd`, callers are: `gateway/handlers/cron.rs:450`, `scheduler/mod.rs:918, 1057, 1398, 1518` (5 sites total).
- Add a thin wrapper method `AgentEngine::handle_isolated_via_pipeline(msg) -> Result<String>` that:
  - Builds a `BootstrapContext { msg, resume_session_id: None, force_new_session: true, use_history: false }`
  - Constructs a `ChunkSink` (transparent — chunks are dropped, pipeline events are no-ops) and a `cancel_token`
  - Calls `bootstrap::bootstrap(self, ctx, &mut sink)` → `BootstrapOutcome`
  - Calls `execute::execute(self, outcome, &mut sink, cancel_token, &mut compressor, &BehaviourLayers::for_cron(loop_config, msg))` → `ExecuteOutcome`
  - Calls `finalize::finalize(...)` and unwraps `final_text`
- The `ChunkSink` already exists for this exact purpose — it consumes `PipelineEvent`s and produces a plain-text chunk channel, which matches what `handle_isolated` returned.
- Switch the 5 call sites one at a time from `handle_isolated` to `handle_isolated_via_pipeline`. After each swap, run `cargo test` — if green, commit. If a test breaks, the rollback is one git revert.
- **Exit:** no caller of `handle_isolated` remains. The function still exists, dead-coded behind `#[allow(dead_code)]`. `cargo check --all-targets` is green; full integration test suite passes (1127 + new bootstrap/finalize tests).

### Phase 7 — Delete `handle_isolated` + Pi verification

Split into two sub-phases so the validation-script work doesn't gate the deletion commit, and so we can demonstrate the layers actually fire on Pi before removing the legacy path.

#### Phase 7a — Build the validation script

The plan's exit criteria requires "one fallback switch, one auto-continue nudge, one session-corruption recovery" exercised on Pi. No existing test produces those three signals — `test-pi-chaos.py` only validates SSE drop + Last-Event-ID resume. Phase 7a creates the missing test:

- New script `tests/integration/pi/test-pi-cron-features.py` (next to the existing chaos test).
- The script POSTs three independent cron-trigger requests through `/api/cron/trigger`, each configured to use a **mock provider** that returns the desired failure mode:
  - **Run 1: fallback switch.** Mock returns HTTP 503 twice in a row, then a normal completion. Asserts that the resulting Jaeger trace contains exactly one `llm.request` span with `http.status_code=503, retry_attempts=N` followed by `llm.request` spans with `provider=<fallback>` rather than `<primary>`.
  - **Run 2: auto-continue nudge.** Mock returns one no-tool-calls response containing the substring `"далее нужно"` (a documented `looks_incomplete` trigger), then a normal completion. Asserts that the trace contains two `llm.call` spans inside one `pipeline.execute`, and that the `messages` table for the resulting session has an `AUTO_CONTINUE_NUDGE` user message.
  - **Run 3: session-corruption recovery.** Mock returns one error whose class maps to `LlmErrorClass::SessionCorruption` (provider-specific HTTP shape). Asserts that the trace contains exactly two `llm.call` spans and the post-recovery messages list has system messages only plus one fresh user message.
- The mock provider is a small Python fixture that registers as a temporary provider via `POST /api/providers` and gets cleaned up on teardown. Reuses the same Jaeger-query helper as `test-pi-chaos.py`.
- **Exit:** new script passes against the **current** legacy `handle_isolated` path on Pi (so we know the assertions are calibrated to actual cron behaviour, not just to what the new pipeline path produces).

#### Phase 7b — Delete and re-verify

- Remove `handle_isolated` from `engine/stream.rs` (and the `pub use` in `engine/mod.rs`).
- Remove the long architectural-divergence comment that documents the parallel implementation — it stops being true.
- Remove `tool_loop_helpers::*` items that only the legacy path used (if any).
- Remove the legacy `AUTO_CONTINUE_NUDGE` constant from `engine/stream.rs` — the new home in `pipeline::behaviour` is the single source of truth (during Phases 4–7a both definitions coexist; the legacy one becomes dead code only after deletion in this step).
- Build with `--features otel`, deploy to Pi, re-run `test-pi-cron-features.py` against the new path. All three runs must still pass.
- Verify in Jaeger that the cron job produces a span tree shaped identically to an SSE chat (`pipeline.execute` → `llm.call` × N → `pipeline.execute_tools` × N → `pipeline.finalize`).
- Verify the existing `test-pi-chaos.py` still passes (regression guard for SSE behaviour).
- **Pre-delete telemetry snapshot.** Before deploying the deletion commit, capture from Jaeger UI a 24-hour count of:
  - `pipeline.execute` spans (SSE) — should not change post-deploy
  - cron-job notifications (`auto_continue`, `iteration_limit`, `agent_loop_detected`) — should be ≥ pre-deploy count if cron jobs continue running normally
  These act as a smoke-test for "did we accidentally turn off the layers" — a 24h post-deploy comparison flags the regression early.
- **Exit:** `handle_isolated` gone, Pi cron jobs produce normal pipeline traces, no regressions, post-deploy telemetry within ±5% of pre-deploy baseline for the listed metrics.

### Phase 8 — Documentation

- Update `docs/architecture/2026-05-05-id-based-dedup.md` "Negative / open" section: cron unification was previously listed as an architectural backlog item; mark it resolved.
- Update `CLAUDE.md` "Three entry points on `AgentEngine`" — only two remain (`handle_sse` and `handle_with_status`); `handle_streaming` likely also collapses into the same `pipeline::execute` route, document that.
- Add a one-paragraph "Composable behaviour layers" section to `CLAUDE.md` so the next maintainer understands what `BehaviourLayers` is for.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Behaviour drift mid-refactor — a layer doesn't quite reproduce the legacy semantics | Each phase ships unit tests that lock the layer's behaviour against the documented legacy behaviour before any caller is migrated. The cron caller is the last thing to move. Phase 7a additionally calibrates the validation script against the **current** `handle_isolated` path before deletion, so post-deletion assertions are anchored to real behaviour. |
| Span tree changes for SSE callers | `BehaviourLayers::none()` produces zero new span emissions and zero new control-flow branches in the hot path. SSE traces stay byte-identical. Pinned by re-running the SSE-side assertions in `test-pi-chaos.py` post-deploy. |
| Crash recovery / WAL semantics differ between paths | Phase 1's appendix is the gate: every state variable in `handle_isolated` gets explicit re-homing before any code moves. WAL writes happen via the same helpers in both paths today (`session_wal::log_event`); the layers don't touch them. Phase 6a's `bootstrap`/`finalize` rework explicitly preserves WAL `done`/`failed`/`interrupted` lifecycle for the cron path. |
| Pi cron jobs hit an unexpected layer interaction in production | Phase 7's deploy is gated behind a manual operator approval. The new `test-pi-cron-features.py` exercises all three layer-driven paths before deletion. If a regression appears, `git revert` of the deletion commit immediately restores the legacy path. |
| Test-DB unavailable at CI time for some new tests | New tests in phases 3–5 use mock policy data only and don't need a database; integration tests with `sqlx::test` come in phase 6a and are gated to the existing `test-db` Makefile target. The Pi-side validation in phase 7 needs a live Pi but no CI infrastructure. |
| Layer silently disengages post-deploy (e.g., a guard becomes always-false through a typo) | Pre-deploy telemetry snapshot in Phase 7b records 24-hour counts of `auto_continue` / `iteration_limit` / `agent_loop_detected` notifications. A post-deploy 24-hour comparison flags a >5% drop as a regression — silent layer disengagement reads as zero notifications, which the snapshot catches. |
| User-visible behaviour change for operators with cron jobs that depend on legacy quirks | The intent is "no behaviour change for any existing caller" (see Non-goals). However, cron jobs that previously observed only `iteration_limit` notifications without `pipeline.execute` traces will start observing both. This is additive — no existing notification or DB row is removed. Worth a one-line CHANGELOG note in the v0.27 release notes when this ships. |
| Two parallel `AUTO_CONTINUE_NUDGE` constants drift between Phases 4 and 7b | Mitigated by replacing the legacy constant in `engine/stream.rs` with `pub use crate::agent::pipeline::behaviour::AUTO_CONTINUE_NUDGE` immediately after Phase 4 lands — keeps a single source of truth across the ~weeks the two paths coexist. (Tracked as a sub-task in Phase 7b's deletion list to confirm the re-export goes away with the legacy file.) |

## Telemetry / success metrics

- **Code:** `engine/stream.rs` shrinks by ~280 lines (handle_isolated body + comment).
- **Coverage:** five new behaviour-layer unit tests, ~40 new test cases.
- **Span uniformity:** every cron run after Phase 7 produces a `pipeline.execute` parent span. Verified by checking Jaeger's "operations" list for the `opex-core` service: only `pipeline.*` and `llm.*` ops, no `engine.handle_isolated` blank slot.
- **Behaviour parity:** `test-pi-chaos.py` continues to pass; the new cron-feature exercise script (Phase 7) passes on the unified path.

## Out of scope

- Streaming-vs-RPC contract divergence is preserved. `handle_with_status` still returns a final `String` to the channel adapter; `handle_sse` still streams events to the SSE consumer. The unification is about the **loop body**, not the **transport contract**. Both transports already feed into `pipeline::execute` via `EventSink` implementations; that doesn't change.
- The architecture review's other recommendations (handler decomposition, `lib.rs` facade cleanup, blocking clippy) are tracked separately. Each gets its own plan when it becomes the next priority.

## Appendix A — Divergent feature map (Phase 1 output)

Mapped from `crates/opex-core/src/agent/engine/stream.rs` (lines 137–490, the body of `handle_isolated`). Six divergent behaviours need re-homing; one is one-shot bootstrap policy; one is post-loop side-effect.

### A1 — `fallback_provider`

* **Trigger.** LLM call returns `Err(e)` and `consecutive_failures >= loop_config.max_consecutive_failures` and `!using_fallback`.
* **Action.** Lazily construct fallback provider via `engine.create_fallback_provider().await`. If construction succeeds, switch live provider to the fallback for all subsequent iterations and reset `consecutive_failures = 0`. If fallback is `None`, fall through to error path.
* **Local state.** `consecutive_failures: usize`, `using_fallback: bool`, `fallback_provider: Option<Arc<dyn LlmProvider>>`.
* **Observability.** One `tracing::warn!("switching to fallback provider after consecutive failures")` on switch.
* **DB / UI side effects.** None.
* **Reset on success.** `consecutive_failures = 0` on every successful LLM call (whether on primary or fallback).

### A2 — `auto_continue`

* **Trigger.** Inside the no-tool-calls branch, after `final_response = strip_thinking(content)`, when `auto_continue_count < loop_config.max_auto_continues && !final_response.is_empty() && looks_incomplete(&final_response)`.
* **Action.** Increment `auto_continue_count`, push an `AUTO_CONTINUE_NUDGE` user message into `messages`, advance `context_chars`, and `continue` the loop (don't break with the current `final_response`).
* **Local state.** `auto_continue_count: u8`.
* **Observability.** `tracing::info!("auto-continue: response looks incomplete, nudging LLM")` per attempt.
* **DB / UI side effects.** Spawned `notify()` to `ui_event_tx` with `auto_continue` notification type, body `"Agent continued unfinished task (attempt {cnt}/{max})"`.
* **Constants.** `AUTO_CONTINUE_NUDGE` is a static string defined in `engine/stream.rs` at module level.

### A3 — `session_recovery` (SessionCorruption)

* **Trigger.** LLM call returns `Err(e)`, `error_classify::classify(&e) == LlmErrorClass::SessionCorruption`, and `!did_reset_session` (one-shot per turn).
* **Action.** Set `did_reset_session = true`, retain only `MessageRole::System` messages in `messages`, push the original `user_text` back as a fresh user message, recompute `context_chars`, `continue` (next iteration retries on the cleaned context).
* **Local state.** `did_reset_session: bool` (flips once per turn).
* **Observability.** `tracing::warn!("session corrupted, resetting context")`.
* **DB / UI side effects.** None.
* **Order.** Must be checked **before** `consecutive_failures`/fallback logic — a SessionCorruption error shouldn't count as a "consecutive failure" for fallback-switch purposes.

### A4 — `tool_policy_override`

* **Trigger.** `msg.tool_policy_override` is `Some(json)` and the JSON deserializes to `AgentToolPolicy`.
* **Action.** One-shot at bootstrap time: `available_tools = engine.apply_tool_policy_override(available_tools, &override_policy)`.
* **Local state.** None — applied once before the loop starts, mutates `available_tools` directly.
* **Observability.** `tracing::info!("cron tool policy override applied")` with `before` and `after` tool counts when the count changed.
* **DB / UI side effects.** None.
* **Architectural placement.** This belongs in `bootstrap`, not in the loop. The behaviour layer carries the override; bootstrap consumes it.

### A5 — `forced_final_call` (iteration limit + loop break)

* **Trigger.** `loop_broken || iteration == loop_config.effective_max_iterations() - 1` at the end of the iteration.
* **Action.** One extra LLM call with `provider.chat(&messages, &[], CallOptions::default()).await` — note the empty `&[]` tools list, this is a no-tools call to extract a final natural-language summary. The result replaces `final_response`.
* **Local state.** None.
* **Observability.** Two notifications spawned conditionally:
  * On `loop_broken && nudges_exhausted` → `agent_loop_detected` notification.
  * On `iteration == max - 1 && !loop_broken` → `iteration_limit` notification.
* **DB / UI side effects.** Notification spawns via `crate::gateway::notify(...)`.
* **Failure handling.** If the forced final call itself errors, `final_response = error_classify::format_user_error(&e)` (degrade gracefully).

### A6 — `empty_retry`

* **Trigger.** Inside no-tool-calls branch, `final_response.is_empty() && empty_retry_count < 1`.
* **Action.** Increment `empty_retry_count`, `continue` the loop (provider gets one shot to produce non-empty output).
* **Local state.** `empty_retry_count: u8` (capped at 1).
* **Observability.** `tracing::warn!("LLM returned empty response, retrying once")`.
* **DB / UI side effects.** None.
* **Decision.** Small enough to fold into `auto_continue` as a sub-policy, but architecturally cleaner as its own one-line check inside `BehaviourLayers`. Treat as part of the auto-continue policy struct (`AutoContinuePolicy { max_continues: u8, retry_on_empty: bool }`).

### A7 — Inter-agent message sender (`sender_agent_id`)

* **Trigger.** `msg.user_id.starts_with("agent:")` at bootstrap.
* **Action.** Strip the `agent:` prefix and pass the remainder as `sender_agent_id` to `sm.save_message_ex(...)` — DB row gains a non-NULL `sender_agent_id`.
* **Local state.** None.
* **Observability.** None (the DB row carries the provenance).
* **Architectural placement.** Already partially in `bootstrap.rs` for the SSE path; the cron path open-codes the prefix check. Phase 6 moves the open-coded check into bootstrap so both paths share it.

### A8 — Post-loop knowledge extraction

* **Trigger.** `messages.len() >= 5` after the loop exits.
* **Action.** Background `tokio::spawn` calls `knowledge_extractor::extract_and_save(...)`.
* **Architectural placement.** Already in `pipeline::finalize` for the SSE path with the same threshold. The cron path duplicates the logic. Phase 6 just removes the duplication; not a behaviour-layer concern.

### State variable re-homing summary

| Variable | Today's home | After Phase 6 |
|---|---|---|
| `loop_nudge_count` | both paths | already in `pipeline::execute` |
| `did_reset_session` | only handle_isolated | local in `execute()`, set guard `layers.session_recovery.is_some()` |
| `empty_retry_count` | only handle_isolated | local in `execute()`, set guard `layers.auto_continue.as_ref().map_or(false, \|p\| p.retry_on_empty)` |
| `auto_continue_count` | only handle_isolated | local in `execute()`, set guard `layers.auto_continue.is_some()` |
| `context_chars` | both paths | already in `pipeline::execute` |
| `consecutive_failures` | only handle_isolated | local in `execute()`, set guard `layers.fallback_provider.is_some()` |
| `using_fallback`, `fallback_provider` | only handle_isolated | local in `execute()`, lazily constructed when threshold trips |
| `final_response` | only handle_isolated | already represented by `ExecuteOutcome::final_text` |

All state stays as `let mut` locals inside `execute()`. Behaviour layers are read-only **policy** that gates whether each piece of state is active. No layer carries mutable state across iterations — that would force unnecessary borrow gymnastics.

## Revision history

### v2 — 2026-05-06 (post-Phase-5 self-review)

After Phases 1–5 landed in commit `7b4c4cd`, a self-review found the plan's text had drifted from the implementation in three places, and that two phases were under-specified for the engineer who'd pick up Phases 6–7. This revision:

- **Fixed type signatures in "Architecture: composable behaviours".** `tool_policy_override` is `Option<ToolPolicyOverride>` (wrapper) not `Option<AgentToolPolicy>`. `forced_final_call` is `Option<ForcedFinalCallPolicy>` (unit struct) not `bool`. Both deviations are intentional and now documented inline with the trade-off.
- **Documented the `none()` vs `default()` constructor naming.** Both exist; `none()` is preferred at call sites; a unit test pins their equivalence.
- **Corrected Phase 3's test location.** New tests live in `pipeline::behaviour::tests`, not `pipeline::execute::tests`.
- **Split Phase 6 into 6a (extend bootstrap/finalize) + 6b (migrate callers).** The bootstrap rework was previously hand-waved; the new sub-phase enumerates the seven specific differences between `handle_isolated`'s startup and `bootstrap()`'s, and pins finalize-side semantics for the cron path.
- **Split Phase 7 into 7a (build validation script) + 7b (delete + re-verify).** The previous wording assumed a script existed for "exercise fallback / auto-continue / recovery on Pi"; it didn't. 7a creates `tests/integration/pi/test-pi-cron-features.py` calibrated against the legacy path before the deletion happens.
- **Expanded the risk table** with two new rows: silent-layer-disengagement (caught by 24h pre/post-deploy notification snapshot) and operator-visible behaviour-additive change (CHANGELOG note in v0.27). Added the `AUTO_CONTINUE_NUDGE` deduplication note pointing at the `pub use` re-export pattern that keeps one source of truth across the weeks the two paths coexist.

### v1 — 2026-05-06 (initial)

Initial plan.
