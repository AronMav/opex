# LLM-Loop Unification — Implementation Plan

**Status:** Active
**Owner:** Phase 67+1 (post-observability)
**Tracking artifact:** this file
**Origin:** Architecture review 2026-05-06 (`docs/architecture/2026-05-06-architecture-review.md`), top-priority recommendation #1.

## Goal

Delete `engine::stream::handle_isolated`. Funnel every LLM+tools loop in HydeClaw through one entry point — `pipeline::execute` — with the features that today only exist on the cron path (`fallback_provider`, `auto-continue`, `session-corruption recovery`, `tool_policy_override`, forced-final-LLM-call) re-expressed as **opt-in behaviour layers** that can be composed onto any caller (SSE, channel, cron, future agent-to-agent).

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

We add a `BehaviourLayers` parameter (default-constructed for the SSE path; populated for cron) that bundles opt-in policy:

```rust
pub struct BehaviourLayers {
    pub fallback_provider: Option<FallbackPolicy>,
    pub auto_continue:     Option<AutoContinuePolicy>,
    pub session_recovery:  Option<SessionRecoveryPolicy>,
    pub tool_policy_override: Option<AgentToolPolicy>,
    pub forced_final_call: bool,  // emit one extra non-tools LLM call on iteration limit
}
```

Each policy is a small data-only struct describing **what** the layer does, not **how** the engine calls it. The dispatch lives inside `execute()` as cheap `if let Some(...)` guards — same control-flow shape as today, just with one decision point per feature instead of an entire parallel function.

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
- Add a unit test in `pipeline::execute::tests` that constructs a fake provider returning Err N times, asserts the fallback path activates on the (N+1)-th call.
- Cron callers continue to use `handle_isolated` for now — Phase 3 only proves the layer works inside `execute()`.
- **Exit:** new test passes. SSE chat still produces identical traces (with the layer off, the new code path is byte-identical to the old). `cargo test` green.

### Phase 4 — Wire `auto_continue`

- Same pattern. Move `looks_incomplete()` + nudge logic out of `handle_isolated` and into `execute()` behind `BehaviourLayers::auto_continue`.
- Add unit test: provider returns "remaining steps but didn't execute them" text twice, asserts auto-continue counter increments and turn continues.
- **Exit:** test green. SSE traces unchanged.

### Phase 5 — Wire `session_recovery` + `tool_policy_override` + `forced_final_call`

- Three smaller layers, same pattern. `tool_policy_override` is essentially a no-op for `execute()` itself — `bootstrap.rs` applies it; the layer just makes the parameter explicit.
- Add focused unit tests for each.
- **Exit:** all five behaviour layers have unit-test coverage. SSE path unaffected.

### Phase 6 — Replace `handle_isolated` callers

- Find every caller of `handle_isolated`. Today it's `agent::engine::run_isolated` for cron jobs and possibly a couple of test sites.
- For each caller, replace with:
  - `bootstrap::bootstrap()` → `BootstrapOutcome` (the cron path's session creation moves into bootstrap as `BootstrapOutcome::isolated_session_for_cron(...)` if the existing call surface doesn't already cover it)
  - `pipeline::execute(engine, bootstrap_outcome, ChunkSink::new(), cancel_token, compressor, BehaviourLayers::for_cron(cfg, msg))`
  - `pipeline::finalize(...)` exit
- The `ChunkSink` already exists for this exact purpose — it consumes `PipelineEvent`s and produces a plain-text chunk channel, which matches what `handle_isolated` returned.
- Knowledge extraction (background `extract_and_save` spawn) moves into `finalize::execute()` behind a small caller-supplied flag — already half-done; complete it.
- **Exit:** no caller of `handle_isolated` remains. The function still exists, dead-coded behind `#[allow(dead_code)]`. `cargo check --all-targets` is green; integration tests pass.

### Phase 7 — Delete `handle_isolated` + Pi verification

- Remove `handle_isolated` from `engine/stream.rs` (and the `pub use` in `engine/mod.rs`).
- Remove the long architectural-divergence comment that documents the parallel implementation — it stops being true.
- Remove `tool_loop_helpers::*` items that only the legacy path used (if any).
- Build with `--features otel`, deploy to Pi, run a cron-driven dynamic job that exercises:
  - One fallback-provider switch (force the primary to 503 twice)
  - One auto-continue nudge (provider response that names remaining work but doesn't execute)
  - One session-corruption recovery (synthetic provider that returns the corruption error class once)
- Verify in Jaeger that the cron job produces a span tree shaped identically to an SSE chat (`pipeline.execute` → `llm.call` × N → `pipeline.execute_tools` × N → `pipeline.finalize`).
- Verify the existing `test-pi-chaos.py` still passes.
- **Exit:** `handle_isolated` gone, Pi cron jobs produce normal pipeline traces, no regressions.

### Phase 8 — Documentation

- Update `docs/architecture/2026-05-05-id-based-dedup.md` "Negative / open" section: cron unification was previously listed as an architectural backlog item; mark it resolved.
- Update `CLAUDE.md` "Three entry points on `AgentEngine`" — only two remain (`handle_sse` and `handle_with_status`); `handle_streaming` likely also collapses into the same `pipeline::execute` route, document that.
- Add a one-paragraph "Composable behaviour layers" section to `CLAUDE.md` so the next maintainer understands what `BehaviourLayers` is for.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Behaviour drift mid-refactor — a layer doesn't quite reproduce the legacy semantics | Each phase ships unit tests that lock the layer's behaviour against the documented legacy behaviour before any caller is migrated. The cron caller is the last thing to move. |
| Span tree changes for SSE callers | `BehaviourLayers::default()` produces zero new span emissions and zero new control-flow branches in the hot path. SSE traces stay byte-identical. |
| Crash recovery / WAL semantics differ between paths | Phase 1's appendix is the gate: every state variable in `handle_isolated` gets explicit re-homing before any code moves. WAL writes happen via the same helpers in both paths today (`session_wal::log_event`); the layers don't touch them. |
| Pi cron jobs hit an unexpected layer interaction in production | Phase 7's deploy is gated behind a manual operator approval. The chaos test on Pi catches resume-correctness; if a regression appears, `git revert` of the deletion commit immediately restores the legacy path. |
| Test-DB unavailable at CI time for some new tests | New tests in phases 3–5 use `MockProvider` and don't need a database; integration tests with `sqlx::test` come in phase 7 and are gated to the existing `test-db` Makefile target. |

## Telemetry / success metrics

- **Code:** `engine/stream.rs` shrinks by ~280 lines (handle_isolated body + comment).
- **Coverage:** five new behaviour-layer unit tests, ~40 new test cases.
- **Span uniformity:** every cron run after Phase 7 produces a `pipeline.execute` parent span. Verified by checking Jaeger's "operations" list for the `hydeclaw-core` service: only `pipeline.*` and `llm.*` ops, no `engine.handle_isolated` blank slot.
- **Behaviour parity:** `test-pi-chaos.py` continues to pass; the new cron-feature exercise script (Phase 7) passes on the unified path.

## Out of scope

- Streaming-vs-RPC contract divergence is preserved. `handle_with_status` still returns a final `String` to the channel adapter; `handle_sse` still streams events to the SSE consumer. The unification is about the **loop body**, not the **transport contract**. Both transports already feed into `pipeline::execute` via `EventSink` implementations; that doesn't change.
- The architecture review's other recommendations (handler decomposition, `lib.rs` facade cleanup, blocking clippy) are tracked separately. Each gets its own plan when it becomes the next priority.

## Appendix A — Divergent feature map (to be filled in Phase 1)

*To be completed during Phase 1. This section will document, per feature, the trigger condition, mutated state, side effects, and observability events that each behaviour layer must reproduce.*
