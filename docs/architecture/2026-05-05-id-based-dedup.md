# ID-Based Dedup Architecture (2026-05-05)

> **Status:** Implemented and live-verified on Pi.
> **Supersedes:** Heuristic-based content dedup in `mergeLiveOverlay` (2026-04 era).
>
> **Note (post-refactor):** References below to `gateway/handlers/chat.rs`
> describe the pre-2026-05-06 layout. The SSE conversion logic discussed
> here now lives in `gateway/handlers/chat/sse_converter.rs` after the
> chat.rs decomposition. The behaviour and invariants documented are
> unchanged.

## Context

Live chat rendering merged two independent sources of truth into one view:

  * **History** — React Query cache fetched from `/api/sessions/{id}/messages`.
  * **Live** — SSE buffer accumulated by `streaming-renderer` from `/api/chat`.

Each had its own pipeline. `convertHistory` turned DB rows into `ChatMessage[]`.
`stream-processor` accumulated SSE events into a `StreamBuffer`. `mergeLiveOverlay`
reconciled the two on every render.

The reconciliation was content-based:

  * `lastHistAssistantTexts: Set<string>` — drop a live text part if its trimmed
    content already appeared in the history bubble.
  * `dedupeBubbleTextParts` — within one bubble, drop text parts ≥ 20 chars
    whose content repeated.
  * `historyEndsWithNewUserTurn` flag.
  * Continuation merge that appended live tools into the previous history
    assistant.

These heuristics fired in different orders depending on RQ cache state, SSE
replay timing, and which iteration of the LLM tool loop was active. Each user-
reported duplicate ("снова дубли!") added a new heuristic on top of the
previous ones, never replacing them. The architecture became a stack of
band-aids around a missing identity contract.

The root problem: **live `ChatMessage` IDs and DB row IDs were unrelated**.

  * `pipeline::execute` pre-allocated **one** UUID for the final assistant
    message before the loop.
  * Intermediate iteration rows got `Uuid::new_v4()` inside the loop body,
    independent of the SSE pre-allocation.
  * Frontend `streaming-renderer` opened a new live `ChatMessage` per `start`
    SSE event — using the backend-supplied id, but only for the first
    iteration.
  * convertHistory built ChatMessages keyed by **the first row's id** when
    merging intermediate iterations of one turn.

Every dedup heuristic was compensation for that ID gap.

## Decision

Establish a single identity contract: **every assistant DB row has a UUID
known by both backend and frontend before the row is persisted**.

Concretely, the SSE `step-start` event carries `messageId: Uuid` — the same
UUID the row will be saved under. The frontend opens a fresh live ChatMessage
under that exact id at the start of each iteration. Once the row is persisted,
ID-based dedup in `mergeLiveOverlay` is sufficient: live id matches history
id, the live overlay drops the duplicate.

Five concrete changes carried this:

### Phase 1 — Per-iteration UUID in StepStart

`crates/opex-core/src/agent/stream_event.rs`:

```rust
StepStart { step_id: String, message_id: String }
```

`crates/opex-core/src/agent/pipeline/execute.rs`:

```rust
for iteration in 0..max_iterations {
    let iter_msg_id = Uuid::new_v4();
    sink.emit(StepStart { step_id, message_id: iter_msg_id.to_string() }).await?;
    // ... LLM call ...
    spawn_persist_assistant_message(&db, iter_msg_id, ..., None /* parent */, Some(iteration));
}
```

The first iteration also emits a legacy `MessageStart` with the same id for
backward compatibility with existing frontend handlers.

`crates/opex-core/src/gateway/handlers/chat.rs`:

```rust
StreamEvent::StepStart { step_id, message_id } => {
    json!({"type": "step-start", "stepId": step_id, "messageId": message_id, ...})
}
```

`ui/src/stores/stream/stream-processor.ts`:

```ts
case "step-start": {
    if (event.messageId) {
        if (session.buffer.assistantId === event.messageId) break; // iter 0 dedup
        if (session.buffer.snapshot().length > 0) session.commit();
        session.buffer.reset();
        session.buffer.assistantId = event.messageId;
    }
    break;
}
```

### Phase 2 — Pure ID-based dedup, heuristics removed

`mergeLiveOverlay` collapsed to four invariants:

  1. ID-based dedup (live message id present in history → skip).
  2. Tool dedup by `toolCallId`.
  3. Merge consecutive in-flight live assistants into one bubble.
  4. Otherwise push to overlay.

Removed: `lastHistAssistantTexts`, `dedupeWithinSteps`, `historyEndsWithNewUserTurn`,
content-based continuation merge filtering.

To preserve the existing UX (one bubble per turn even when multiple
intermediate rows exist), `ChatMessage` gained a `mergedIds: string[]` field.
`convertHistory` keeps the merge-by-tool_calls behavior and tracks every
merged row id in `mergedIds`. `mergeLiveOverlay`'s `historyIds` set seeds
from `m.id` plus `m.mergedIds` so live ChatMessages keyed by any merged row
are correctly recognized as already in history.

### Phase 3 — Last-Event-ID offset tracking

`StreamRegistry::push_event` already returned a monotonic seq. `chat.rs`
emits SSE `id: <seq>` on every event. `api_chat_resume_stream` reads the
client's `Last-Event-ID` header (or `?last_event_id=` query) and replays
only events with `seq > last_event_id`. Frontend `stream-processor` tracks
the highest seq in `StreamSession.lastEventId` (and persists to agent
state for survival across StreamSession disposal); `streaming-renderer`
attaches the header on every resume fetch. New session resets the id
because the backend's seq counter is per-session.

### Phase 4 — Persistent step_id in DB

Migration `046_messages_step_id.sql`:

```sql
ALTER TABLE messages ADD COLUMN step_id INT;
CREATE INDEX messages_session_step_idx ON messages(session_id, step_id)
  WHERE step_id IS NOT NULL;
```

`spawn_persist_assistant_message` accepts `step_id: Option<i32>`. When set,
a follow-up `UPDATE` populates the column after the insert (detached
spawn, retried 3× with backoff, non-fatal on persistent failure).

`pipeline::execute` passes `iteration as i32`. `engine/stream.rs::handle_isolated`
(legacy cron path) does the same.

NULL means "not part of a tool-loop iteration" — final assistant rows,
user rows, tool-result rows. Frontend treats NULL as "no step info".

### Phase 5 — BatchOutcome guarantees ToolResult emission

`pipeline::parallel::execute_tool_calls_partitioned` previously returned
`Result<Vec<ToolBatchResult>, LoopBreak>`. The error path threw away every
tool that had completed before the loop detector raised the break.
Frontend left perpetual spinners on those tools.

```rust
pub struct BatchOutcome {
    pub results: Vec<ToolBatchResult>,
    pub loop_break: Option<Option<String>>,
}
```

Callers (`pipeline::execute`, `engine::stream`, `subagent_runner`,
`openai_compat`) unconditionally iterate `outcome.results` to emit
`ToolResult` events, then check `outcome.loop_break` for the nudge/terminate
decision.

Inside `parallel.rs` the `join_all` loop records `results[i] = Some(result)`
**before** the loop-detector check, so a break in iteration N still surfaces
results for tools 0..N.

## Consequences

### Positive

  * **Pure ID-based dedup**: 5 heuristics removed (`lastHistAssistantTexts`,
    `dedupeWithinSteps`, `historyEndsWithNewUserTurn`, content-based
    continuation filtering, prefix-strip dedup). 78 net lines deleted from
    `chat-overlay-dedup.ts`.
  * **Backend `Finish` event guaranteed**: Failed/Interrupted/Errored exit
    paths in `finalize.rs` and `run.rs` now always close the SSE stream
    with `Finish`. Frontend's reconnect loop bug (lingering loader after
    backend marked session done) is fixed at the source.
  * **Idempotent SSE replay**: `Last-Event-ID` makes reconnect protocol-
    level dedup-free, no duplicates regardless of how often the client
    reconnects.
  * **`step_id` queryability**: analytics can `SELECT * FROM messages
    WHERE step_id IS NOT NULL ORDER BY session_id, step_id`. Future per-
    step UI features (iteration markers, deep-links into a specific
    iteration) become a one-liner.
  * **Concurrency safety**: tested with two parallel POST /api/chat for
    same agent — distinct sessions, disjoint messageIds, both emit `finish`.
  * **Migration safety**: legacy ChatMessages without `mergedIds` continue
    to dedup correctly (only by primary id).

### Negative / open

  * **Resolved 2026-05-06**: `engine/stream.rs::handle_isolated` is
    deleted. Cron, agent-to-agent, and other RPC-style callers now
    route through `pipeline::execute` via
    `engine::run::handle_isolated_via_pipeline`, with the previously
    cron-only features (fallback provider, auto-continue,
    session-corruption recovery, tool-policy override, forced-final
    LLM call) re-expressed as opt-in `BehaviourLayers` consulted by
    `execute()` at well-defined insertion points. SSE callers pass
    `BehaviourLayers::none()` for byte-identical legacy semantics;
    cron callers pass `BehaviourLayers::for_cron(loop_config, msg)`.
    `engine/stream.rs` shrunk from 469 to 101 lines. Single span
    shape across all turn types (cron sessions now produce
    `pipeline.execute` + `pipeline.finalize` traces in Jaeger,
    matching SSE chats). Documented in
    [2026-05-06-llm-loop-unification-plan.md](./2026-05-06-llm-loop-unification-plan.md).
  * **Resolved 2026-05-05**: Mid-level helpers extracted to
    `pipeline::tool_loop_helpers` (precursor to the unification
    above). Both `pipeline::execute` and the (then-still-living)
    `handle_isolated` shared a single source of truth for loop-nudge
    wording, loop-break bookkeeping, intermediate-assistant append,
    persist-payload encoding, and per-iteration UUID allocation.
  * **Resolved 2026-05-05**: Backend unit tests for `BatchOutcome`
    invariants are now in `parallel.rs::tests` (4 tests covering
    no-loop-break, loop-break-preserves-results, loop-break-without-
    reason, optional `tool_msg_id`). `tool_loop_helpers::tests`
    adds 9 more covering loop-nudge bookkeeping and helper purity.
    Integration tests via `sqlx::test` run against an isolated
    Postgres on `127.0.0.1:5434` — see `make test-db`. CI infrastructure
    documented in [observability-setup.md](./observability-setup.md)
    and the test-DB Makefile targets.
  * Render-side merge of consecutive same-agent ChatMessages
    (`continuesPrevious` in `MessageList`) is a UX choice that re-introduces
    visual coupling between technically-independent messages. Trade-off:
    semantic isolation lost (each ChatMessage is its own DB row) for
    user-facing simplicity (one bubble per turn).
  * OTel observability is wired and ready (`pipeline.execute`,
    `pipeline.finalize`, `pipeline.execute_tools` spans) but the Pi
    operator step is manual — set `[otel] enabled = true` in
    `opex.toml`, set `OTEL_EXPORTER_OTLP_ENDPOINT` in the systemd
    unit, run `make deploy-jaeger`. End-to-end span validation under
    load is operator-driven (Jaeger UI on port 16686).

### Verification

  * Backend: **1122 unit tests passing** (full opex-core suite,
    including all sqlx::test integration tests against the isolated
    test Postgres on `127.0.0.1:5434`). 9 new helper tests in
    `pipeline::tool_loop_helpers::tests` cover the shared loop
    mechanics; 4 in `parallel.rs::tests` cover BatchOutcome invariants.
  * Frontend: 905 unit tests, 6 integration tests for multi-iteration scenarios.
  * Pi e2e: `test-pi-e2e.py` validates SSE contract (Phase 1+3+5+Finish),
    `test-pi-concurrency.py` validates parallel-session isolation,
    `test-pi-chaos.py` validates Last-Event-ID resume across mid-stream
    drops (PASSED on Pi: 4 unique step-start IDs, no duplicate seq IDs
    across drop boundary).
  * Playwright: `architecture.spec.ts` runs against the deployed Pi UI.
  * Live UI verification: 5 DB rows (4 intermediate + 1 final) render as
    one ARTY bubble via `mergedIds` + `continuesPrevious`.
  * Test infrastructure: `docker-compose.test.yml` boots an ephemeral
    Postgres on port 5434 (tmpfs storage) so sqlx::test can
    CREATE/DROP per-test databases without touching dev/prod data.
    `make test-db` runs the full backend suite against it.
  * Observability: `[otel]` feature compiles cleanly,
    `docker-compose.observability.yml` runs Jaeger all-in-one
    (verified live on Pi: UI 200 on port 16686, OTLP receiver on 4317).
    Cross-process tracing wired end-to-end:
      * Core (`opex-core` service) — `pipeline.execute`,
        `pipeline.finalize`, `pipeline.execute_tools` spans with
        `session_id`, `agent`, `iterations`, `assistant_message_id`,
        `outcome`, `tool_count` tags populated.
      * Toolgate (`toolgate` service) — FastAPI auto-instrumented
        (`POST /v1/embeddings` etc.), httpx outgoing instrumented.
      * Channels (`channels` service) — Node SDK auto-instrumentations
        for outbound HTTP (Telegram/Discord/Slack APIs).
      * Core's reqwest calls inject W3C `traceparent` via
        `trace_propagation::inject_trace_context` so the Toolgate
        span attaches to its Core parent. Verified live on Pi:
        single trace (53 spans) contains both `opex-core` and
        `toolgate` spans linked under one `pipeline.execute` parent
        — see screenshot artifact in commit message of
        `arch: cross-process tracing — Core → Toolgate → Channels`.
    Detailed Pi rollout in [observability-setup.md](./observability-setup.md).

### End-to-end identity (the contract)

For session `64552e34-2bc1-4eec-a1f6-faf8f538ddbb` observed live on Pi
2026-05-05:

| Layer | Iteration 0 | Iteration 1 | Iteration 2 | Iteration 3 | Final |
|---|---|---|---|---|---|
| SSE step-start `messageId` | `7400e25f-…` | `2c750850-…` | `a0f70ae6-…` | `8c6c575e-…` | (n/a) |
| `messages.id` | `7400e25f-…` | `2c750850-…` | `a0f70ae6-…` | `8c6c575e-…` | `8796aa00-…` |
| `messages.step_id` | `0` | `1` | `2` | `3` | `NULL` |
| Frontend `ChatMessage.id` | `7400e25f-…` (mergedIds includes 1,2,3) | merged | merged | merged | `8796aa00-…` |
| Visual rendering | ┐ | │ | │ | │ | ┘ — one ARTY bubble |

One UUID per logical message, threading from SSE event → DB row →
frontend state → DOM render. No content-based dedup heuristics required.

## References

  * Migration: `migrations/046_messages_step_id.sql`
  * Backend Finish guarantee: `crates/opex-core/src/agent/pipeline/finalize.rs`
    (Failed + Interrupted paths) and `engine/run.rs` (panic path)
  * Shared loop mechanics: `crates/opex-core/src/agent/pipeline/tool_loop_helpers.rs`
    (single source of truth for both `pipeline::execute` and
    `engine/stream.rs::handle_isolated`)
  * Frontend dedup: `ui/src/stores/chat-overlay-dedup.ts`
  * Tests:
    * `ui/src/stores/__tests__/chat-overlay-dedup.test.ts`
    * `ui/src/stores/stream/__tests__/multi-iteration-integration.test.ts`
    * `ui/src/__e2e__/architecture.spec.ts`
    * `test-pi-e2e.py`, `test-pi-concurrency.py`, `test-pi-chaos.py`
  * Test infrastructure:
    * `docker/docker-compose.test.yml` — isolated Postgres for sqlx::test
    * `Makefile` targets: `test-db-up`, `test-db`, `test-db-down`
  * Observability:
    * `docker/docker-compose.observability.yml` — Jaeger all-in-one
    * `Makefile` targets: `build-arm64-otel`, `deploy-binary-otel`,
      `jaeger-up`, `jaeger-down`, `deploy-jaeger`
    * Operational runbook: [observability-setup.md](./observability-setup.md)

## 2026-05-07 Amendment — Strong-typed Uuid newtypes (S2)

The 2026-05-05 contract used `Option<String>` for all ID fields. This
amendment escalates to typed wrappers: each ID kind becomes a distinct
newtype around `Uuid` (or `String` where provider-supplied) with
`Serialize`/`Deserialize`/`sqlx::Type` impls.

The change is internal to Rust. Wire format (SSE JSON, DB schema) is
unchanged. The contract gains compile-time guarantees:
  * **Boundary parsing**: malformed UUIDs rejected at SSE/DB deserialization
  * **Type-mismatch protection**: passing one ID type where another is
    expected is a compile error.

Kind-by-kind migration in commits:
  * `ApprovalId(Uuid)` — see commit `832c944c`
  * `MessageId(Uuid)` — see commit `2b9cd1a8`
  * `ToolCallId(String)` — see commit `674d4069`
  * `ParallelBatchId(Uuid)` — new in this amendment, see commit `79962516`

`IterationId` (T2) is an explicit struct that bundles `index: u32` plus
`MessageId` — replaces the implicit combo threaded today through
`StepStart` event payload.
