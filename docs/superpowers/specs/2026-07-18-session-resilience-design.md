# Session Resilience Design

**Date:** 2026-07-18
**Status:** Approved (owner sign-off in session)
**Goal:** A turn may die — a session may not. Any disruption (client tab switch, network drop, provider error, cancel, slow compaction) leaves the session in a resumable state, and writing into it revives it.

## 1. Background — observed pains (all verified 2026-07-18 on prod)

| Pain | Observed evidence |
|---|---|
| «Соединение потеряно / signal is aborted without reason» | UI fetch aborts; 3× `guard_dropped` failures on Arty in one day |
| Tab switch mid-generation resets the streaming message | User repro; UI disposes stream state instead of reattaching |
| «Пиши не пиши» — failed session not revivable | «Повторить» → `regenerate()` → `POST /api/sessions/{id}/fork` → 4× HTTP 500 FK `messages_branch_from_message_id_fkey` |
| Sessions marked ОШИБКА (`failed`) after mere disconnect | channel WS disconnect → `SessionLifecycleGuard::drop` → `failed` |
| UUID «participants» in chat | Session died → engine continued in a NEW session → UI renders transition participant labeled by session UUID (no such agent exists in DB — verified) |
| 429 cascades kill turns | 4× `llm_error` 429 quota exhausted (ollama primary in every profile) |
| Slow compaction stalls the turn | All 3 `guard_dropped` events immediately follow `context window threshold reached, compacting` + blocking LLM call, inside the synchronous `http_request` span |

## 2. Current state (verified in code — what already matches the industry canon)

Reference research: openclaw (`D:/GIT/bogdan/openclaw-src`), opencode (`D:/GIT/opencode`), industry (OpenAI background mode, Vercel `resumable-stream`, LiteLLM Router, OpenRouter, LibreChat).

| Mechanism | Status | Anchor |
|---|---|---|
| Server-owned turn decoupled from client socket | ✅ present | `gateway/handlers/chat/sse.rs:283-342` (engine task on `bg_tasks`, client-gone ≠ cancel) |
| Event buffer + reconnect replay | ✅ present | `StreamRegistry` (`gateway/stream_registry.rs`) + `GET /api/chat/{id}/stream` (`chat/resume.rs`) |
| Interactive fallback + session recovery + empty-retry | ✅ present | `BehaviourLayers::for_interactive` (`agent/pipeline/behaviour.rs:298`), wired at `engine/run.rs:354,652,906` |
| Server-side resume of failed/interrupted session | ✅ present | `ReentryMode::ExplicitResume` (`agent/context_builder.rs:282-296`) |
| `/abort` cancel-grace marks `interrupted` before hard-abort | ✅ present (SSE path only) | `chat/sse_converter.rs:187-227` |
| Crash recovery (timeline warm-up, stale-claim reset) | ✅ present — ahead of both references | bootstrap + `claim_session_with_retry` |

### The six verified gaps

1. **Channel/backstop interruption marks `failed`, not `interrupted`.** `SessionLifecycleGuard::drop` (`agent/session_manager.rs:365-403`) claims running→`failed` on any unrecorded early exit; the channel-WS disconnect path has no pre-mark equivalent of the SSE cancel-grace.
2. **`Повторить` (regenerate) goes through session fork, which 500s** when `branch_from_message_id` references an unpersisted (client-only) message id — FK violation, dead end for the user.
3. **UI does not reattach on tab lifecycle.** Hidden-tab throttling parks the SSE reader; on return the visibility handler exists (`stores/streaming-renderer.ts`, `VISIBILITY_STALE_MS`) but the flow can dispose/reset the streaming message instead of resuming. This same gap is open in Vercel AI SDK (`vercel/ai #11865`) — we close it ourselves.
4. **Fallback uses a single reserve, no cooldown store.** `create_fallback_provider` (`agent/pipeline/llm_call.rs:47`) takes one name (profile `text[1]`); `text[2..]` is unreachable; `error_classify::cooldown_duration` exists but nothing consumes it; no primary re-probe.
5. **Compaction is blocking, on the synchronous request path, and provider-fragile.** `compact_if_needed` (`agent/history.rs:82`) makes a blocking LLM call before the turn's first token, inside the span the client is waiting on (UI `REQUEST_TIMEOUT = 30_000`, `ui/src/lib/api.ts:5`). A slow/dead compaction provider stalls the turn into the 30s client abort.
6. **UI renders unknown participants as raw UUIDs.**

## 3. Design decisions (validated against references)

**Adopted:**
- *Interruption taxonomy with partial salvage* (LibreChat abort flow; openclaw `abortedLastRun` + salvaged partial): every forced-termination path persists the partial and marks `interrupted` BEFORE any abort. `failed` is reserved for genuine engine/LLM errors after the fallback chain is exhausted.
- *Reattach on every lifecycle transition* (Ably/industry synthesis; closes the Vercel gap): `visibilitychange` + `online` + `pageshow` → replay-reattach. Hiding a tab never disposes a live stream.
- *Before-first-token failover with error-class cooldowns* (LiteLLM/OpenRouter canon; openclaw stepped cooldown + 30s primary probe): full profile chain, exclusion set per turn, cooldown map per provider, never swap mid-stream.
- *Fail-open compaction off the critical path* (openclaw exactly): time-budgeted, skip-on-failure, runs after 202 is returned, reuses the fallback chain. The existing reactive overflow-retry (`llm_call.rs:481,517` force-compact-and-retry-once) remains the safety net.

**Rejected:**
- Durable-queue engine rewrite (no reference does this for this problem class; OPEX's StreamRegistry + timeline already covers the recovery need).
- Mid-stream provider swap (explicit OpenRouter anti-pattern: "once a stream starts, you cannot switch models").
- Hard-fail compaction (opencode does this; openclaw's fail-open is strictly better for interactive UX).
- Channel queue-mode multiplexer (openclaw steer/followup/collect) — out of scope; OPEX channel FIFO queue is adequate.

## 4. Workstreams

### WS1 — Unified interruption semantics (core)

All forced-termination paths converge on: **persist partial → mark `interrupted` → then abort**.

- **Channel WS disconnect** (`gateway/handlers/channel_ws/`): when a live turn's transport dies and the turn is being torn down, call `cleanup_session_terminated(db, sid, "interrupted", "channel_disconnected")` before dropping/aborting — mirror of the SSE cancel-grace pre-mark (`sse_converter.rs:210-224`). If the engine keeps running to completion despite the disconnect (current SSE semantics), keep that — this applies only where the turn is actually torn down.
- **Supersede** (`stream_registry.rs:131-141`): verify (test) that a superseded turn finalizes as `interrupted` via cooperative cancellation → `finalize`; if any path lands `failed`, fix to `interrupted`.
- **Guard backstop** (`session_manager.rs:365+`): `Drop` while `Running` with **no recorded failure** (`recorded == false`) claims running→**`interrupted`** with reason `guard dropped (early exit)`. When a real failure was recorded (`recorded == true`), the explicit failure path has already finalized — guard behavior unchanged. `session_failures` forensic row still written (kind `guard_dropped`) in both cases.
- Shutdown-drain already cancels cooperatively → finalize handles it; add a test asserting `interrupted`.

**Invariant (testable):** a session may end `failed` ONLY if a `session_failures` row with `failure_kind = 'llm_error' | 'engine_error'`-class reason exists for that run.

### WS2 — Writing revives: fix the regenerate/fork dead end (core + UI)

- **Server:** `POST /api/sessions/{id}/fork` must not 500 on a `branch_from_message_id` that does not exist. Resolution order: (a) if the id exists → current behavior; (b) if not → fall back to the last persisted message of the session as the branch point (log a warning); the response reports the actual branch point used.
- **UI:** `regenerate()` and branch-picker flows pass only **persisted** message ids (ids seen in a sync envelope / history load), never optimistic client-side ids.
- **UI send-into-terminal-session:** sending a message while viewing a `failed`/`interrupted`/`done` session keeps that `session_id` in the POST (server already handles `ExplicitResume`). No silent new-session creation from the composer path.

### WS3 — Tab lifecycle reattach (UI)

- Hiding a tab (`document.hidden`) performs **no** dispose, no settle, no state reset — the stream session object stays; the parked socket is allowed to die silently.
- On `visibilitychange→visible`, `online`, `pageshow`: if the agent's phase is active OR the last known turn may still be live, run the existing staleness check and `connect()` (GET replay) with `settleMessages: false` — the replayed envelope reconciles the message in place. The streaming message id is stable across reattach (`boundary_message_id` from the envelope), so no visual reset.
- If the GET returns "no live stream" (turn finished while hidden): render from the sync envelope (persisted result) — message appears completed, never dropped.
- `REQUEST_TIMEOUT` stays 30s for plain API calls; the send-POST becomes fast (WS5), so no timeout change is needed.

### WS4 — Full-chain failover with cooldowns (core)

- **Chain:** replace single `fallback_provider_name` with the profile's ordered `text[]` chain. On a failover-worthy error (`is_failover_worthy`, unchanged) before the first streamed token of the current call: advance to the next chain entry not in the per-turn exclusion set and not in cooldown. Exhausted chain → the turn fails (existing semantics).
- **Cooldowns:** in-memory map (AppState) `provider_name → cooldown_until`, set on failure using the existing `error_classify::cooldown_duration(class)` values. Cooldown entries expire naturally; a cooled-down provider is skipped during chain selection.
- **Primary re-probe** (openclaw `MIN_PROBE_INTERVAL_MS = 30_000`): after a swap, subsequent turns re-try the chain head once its cooldown expires — no permanent demotion, no restart needed.
- **Mid-stream rule:** once the current LLM call has streamed any token, no swap for that call — the error surfaces through the existing retry/`reconnecting` machinery.
- Compaction and any auxiliary in-turn LLM calls use the same chain resolution.

### WS5 — Compaction: off the critical path, fail-open (core)

- Move `compact_if_needed` out of the synchronous bootstrap/context-build span: the send-POST returns 202 after the cheap bootstrap (claim, persist user message, register stream). Compaction runs inside the detached engine turn, before the first LLM call.
- **Time budget:** 15s (constant). Budget exceeded / provider error / all-chain-cooled → **skip compaction, proceed uncompacted**, log + `session_timeline` note. The reactive overflow-retry path remains the safety net.
- Compaction provider = WS4 chain (not a hardwired provider row).

### WS6 — UI participant hygiene

- A message/divider whose `agentId` is not a known agent name (e.g., a bare UUID) renders with a generic label (localized «агент» / current agent name), never the raw UUID. Root-cause reduction comes from WS1+WS2 (fewer session recreations), this is defense-in-depth for rendering.

## 5. Error semantics after this design

| Event | Session status | Resumable by writing? |
|---|---|---|
| Client tab hidden / browser closed | (turn keeps running) → `done` | — (turn finished normally) |
| Explicit `/abort`, cancel-grace exceeded | `interrupted` | ✅ |
| Channel transport died, turn torn down | `interrupted` | ✅ |
| Superseded by a new message (web) | `interrupted` (partial persisted) | ✅ (it IS the same session) |
| Process shutdown drain | `interrupted` | ✅ |
| Guard drop without recorded error (panic, unknown) | `interrupted` + forensic `session_failures` row | ✅ |
| LLM error after full chain exhausted | `failed` + `session_failures(llm_error)` | ✅ (ExplicitResume) |
| Compaction slow/failed | (no effect — turn proceeds) | — |

## 6. Testing

- **sqlx (server-authoritative):** WS1 invariant test (`failed` requires recorded llm/engine error); supersede → `interrupted`; guard-drop → `interrupted`; WS2 fork-fallback (unknown branch id → last persisted message, no 500).
- **Unit (core):** WS4 chain iteration (exclusion set, cooldown skip, re-probe after expiry, before-first-token rule); WS5 budget/fail-open (mock provider slow/dead → compaction skipped, turn proceeds).
- **vitest (UI):** WS3 hidden-tab no-dispose; visible→reattach reconciles the same message id; «Повторить» uses persisted ids; WS6 UUID participant renders generic label.
- **Server smoke (post-deploy):** start a live turn → kill the channel/SSE transport → session ends `interrupted`, partial persisted → write into it → same session revives (ExplicitResume) and completes.

## 7. Out of scope

- Channel queue modes (steer/followup/collect multiplexer) — current FIFO is adequate.
- MOEX / toolgate `web_fetch` SSL certificate failure — infrastructure issue, tracked separately.
- Event-sourcing rewrite of the message store.
- Persisted (cross-restart) cooldown store — in-memory is sufficient for a single-binary gateway; revisit only if provider flapping across restarts is observed.

## 8. Success criteria

1. Switching tabs mid-generation never loses or resets the streaming message; on return the turn is live or completed — never blank.
2. «Повторить» always produces a new attempt (no silent dead end).
3. No session ever shows ОШИБКА from a mere disconnect/cancel; those show as interrupted and are revivable by writing into them.
4. A provider 429/5xx/timeout mid-conversation swaps to the next chain entry without failing the turn (before first token), and the primary heals back automatically.
5. Send-POST returns within ~2s regardless of context size (compaction off-path).
6. No UUID participants in chat under any flow.
