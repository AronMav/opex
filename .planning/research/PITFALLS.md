# Domain Pitfalls: HydeClaw v0.20.0 Harness Quality

**Domain:** Adding harness features to an existing Rust AI gateway (single-binary, multi-provider, ARM64)
**Researched:** 2026-05-08
**Scope:** Feature-specific pitfalls for adding prompt caching, auto-compaction, tool defer_loading,
Hook API, model routing, and REF-03 to the existing codebase.

---

## Feature 1: Prompt Caching (Anthropic cache_control)

### CRITICAL — Pitfall 1.1: cache_control Applied to Non-Anthropic Providers

**What goes wrong:** The `cache_control: {"type": "ephemeral"}` block is an Anthropic-specific
extension. If `prompt_cache: true` is activated on a Google or OpenAI provider row, the serialized
request body either gets silently ignored (OpenAI spec) or causes a 400 validation error (Google).
The current `build_request_body` in `anthropic.rs` already guards this correctly — but the risk is
in the `RoutingProvider`: when the primary fails over to a non-Anthropic fallback, the fallback
provider receives messages built without cache blocks (since each provider builds its own body from
`Message`), so that part is safe. However, if `prompt_cache` config leaks into `ProviderOverrides`
and is passed to `build_provider(row, …, overrides)` for a Google/OpenAI row, the Google/OpenAI
implementations may silently accept the flag and produce corrupted bodies if they ever add a
`prompt_cache` code path.

**Warning sign:** Provider error 400 from Google/OpenAI mentioning unknown fields. Cache hit metric
stays 0 even on Anthropic while no error occurs (cache is silently ignored by body builder of wrong
provider type).

**Prevention:**
- Add a guard in the shared `ProviderOverrides` resolution: `prompt_cache` must only be `Some(true)`
  when the `ProviderRow.provider_type == "anthropic"`. Enforce this at the
  `create_routing_provider` boundary, not inside each provider impl.
- Unit test: `build_request_body` called with `prompt_cache: true` on an OpenAI provider row must
  NOT include `cache_control` in the serialized JSON.

**Phase:** Cache implementation phase (system prompt + tool defs caching).

---

### CRITICAL — Pitfall 1.2: Tool Cache Breakpoint Placement Invalidates Cache on Every Tool Change

**What goes wrong:** The current code places `cache_control` on the **last** tool in the tools
array. Anthropic requires cache breakpoints to mark positions in the content that are stable across
requests — content before the breakpoint must be identical to get a cache hit. If YAML tools are
added/removed dynamically (via `tool_create`/`tool_verify`) between two consecutive turns in the
same session, the tool array changes, the last tool shifts, and the cache breakpoint position moves.
This causes every Anthropic cache write to be a new creation with zero reads — exactly the
worst-case scenario (cache write cost with no savings).

**Warning sign:** `cache_creation_input_tokens` is always > 0 on every request but
`cache_read_input_tokens` is always 0. Anthropic billing shows unexpected cache write charges
without corresponding read savings.

**Prevention:**
- Place the cache breakpoint on the **last stable tool** (i.e., after the system tools that never
  change) and ensure all volatile YAML tools appear after the breakpoint. Document this ordering
  contract explicitly.
- Alternatively: place the cache breakpoint on the system message only (single breakpoint strategy)
  and skip tool caching until tool order is guaranteed stable.
- Add a monitoring counter: `cache_hit_rate = cache_read_tokens / (cache_read_tokens + cache_creation_tokens)`.
  Alert when it drops below 20% over a 10-minute window.
- Test: two consecutive identical requests with the same tool list must yield `cache_read_input_tokens > 0`
  on the second request (integration test against Anthropic — mark as `#[ignore]` for CI).

**Phase:** Cache implementation phase. Also review in tool defer_loading phase (changes tool count).

---

### Pitfall 1.3: Cache Interaction with Extended Thinking

**What goes wrong:** Extended thinking (thinking blocks in assistant messages) adds non-cacheable
content between turns. Anthropic's cache requires that everything before the breakpoint is identical
— thinking blocks contain unique `signature` values per response, making them inherently unstable.
If the system prompt cache breakpoint is placed after the conversational messages array (not just
the system field), it will always miss.

**Warning sign:** Agents with `thinking_level > 0` show zero cache reads while agents with
`thinking_level == 0` show normal cache hits. Observed indirectly via billing breakdown.

**Prevention:** Cache breakpoints must be on the **system message and tools only** — never on the
conversational `messages` array. The existing code does this correctly, but the constraint must be
preserved as CLAUDE.md content caching is extended to include CLAUDE.md (which would go into the
system block).

**Phase:** Cache implementation phase. Flag for extended thinking agent testing.

---

### Pitfall 1.4: Cache Min-Token Threshold Not Enforced

**What goes wrong:** Anthropic requires at least 1024 tokens for a cache block to be created.
System messages shorter than this get a silent cache write attempt that returns a 400 or simply
doesn't cache. For small agents with minimal SOUL.md content, caching may have zero effect while
adding serialization complexity.

**Warning sign:** `cache_creation_input_tokens == 0` and `cache_read_input_tokens == 0` on an
agent that should be caching. No error, just silent no-op.

**Prevention:** Add a check: if the estimated system prompt token count is below 1024, skip
`cache_control` attachment even when `prompt_cache: true`. Estimate tokens via byte count
(`bytes / 4` approximation is sufficient for this threshold check).

**Phase:** Cache implementation phase.

---

## Feature 2: Auto-Compaction at 85% Context Window

### CRITICAL — Pitfall 2.1: Token Counter Mismatch Between Actual Anthropic Usage and Local Estimate

**What goes wrong:** The `Compressor.last_prompt_tokens` is populated from the LLM response
`usage.input_tokens`. For Anthropic, when prompt caching is active, the actual prompt size tracked
by Anthropic includes `cache_creation_input_tokens` — but the `input_tokens` field in the response
reflects only non-cached tokens read, not the total context. If `should_compress()` is gated on
`last_prompt_tokens / context_limit`, using post-cache `input_tokens` (which is deflated) will
prevent compaction from triggering when the real context is at 85%.

**Warning sign:** Sessions grow beyond the model's actual context limit and get truncated/corrupted
before compaction triggers. Specifically when `prompt_cache: true` is active and cache hits reduce
the reported `input_tokens` below the threshold.

**Prevention:** When `cache_read_input_tokens > 0`, the effective context size is
`input_tokens + cache_read_input_tokens + cache_creation_input_tokens`. Use this sum, not
`input_tokens` alone, for the compaction threshold check. Add a specific test: mock a response
with cache fields and verify `should_compress()` reads the combined token count.

**Phase:** Auto-compaction implementation phase. Must be coordinated with the prompt caching phase.

---

### CRITICAL — Pitfall 2.2: Compaction LLM Call Blocks the Main Loop Without Cancellation Support

**What goes wrong:** The compaction is triggered inline in `execute.rs` before the next LLM turn.
The summarization call is itself a full LLM call. If the user cancels (SSE connection drops,
`CancellationToken` flips), the compaction LLM call may not respect the cancellation and will
block the pipeline for the duration of the compaction request.

**Warning sign:** Sessions show a "stuck" state in the WAL (WAL shows `running` for many minutes
after the user-visible SSE stream ended) during long sessions. The `ProcessingGuard` does not
finalize until the compaction call completes.

**Prevention:** The compaction LLM call must receive the same `CancellationToken` that the main
loop uses. Pass the token through `BehaviourLayers` or as a parameter to the compaction trigger.
Test: cancel mid-compaction and verify the WAL transitions to `interrupted` within 2 seconds.

**Phase:** Auto-compaction implementation phase.

---

### Pitfall 2.3: Anti-Thrash Logic Miscalibrated for Cron Sessions

**What goes wrong:** `ineffective_count` increments when compression saves less than
`anti_thrash_min_savings` of tokens. Cron sessions that do a fixed, predictable amount of work per
turn will legitimately have small context windows that compress poorly (few tokens saved per
summary). The anti-thrash logic will permanently disable compression for these sessions after
`anti_thrash_max_skips` consecutive "ineffective" compressions, even though the context is
genuinely growing.

**Warning sign:** Cron agent sessions hit context limit errors after a long series of turns even
though compaction is configured and was previously active.

**Prevention:** Anti-thrash counter should reset when the session context genuinely grows beyond
75% of the limit after a skip. Document the counter reset semantics. Consider a separate
`min_savings` threshold for cron vs. interactive sessions (configurable per-session type in
`CompactionConfig`).

**Phase:** Auto-compaction implementation phase. Flag for cron session testing.

---

### Pitfall 2.4: Compaction State Not Persisted Atomically with WAL Transition

**What goes wrong:** `CompressorState` is serialized into the session's `compaction_state` JSON
column (via `Compressor::to_json()`). If this column is not written atomically with the WAL
transition during finalize, a crash between compaction and the WAL `done` write produces a session
with a stale `last_prompt_tokens = 0` on resume. On resume, `should_compress()` returns `false`
because `last_prompt_tokens == 0`, and compaction won't trigger until the first LLM call completes.
This is a minor timing issue but may cause one extra "heavy" turn.

**Warning sign:** After a forced restart during a compaction-active session, the next turn shows
a much larger context than expected (compaction was not re-triggered immediately on resume).

**Prevention:** Persist `compaction_state` to the session row in the same DB transaction that
finalizes the message. Verify in the `finalize.rs` write path that this update is inside the
transaction boundary, not a separate UPDATE after commit.

**Phase:** Auto-compaction implementation phase. Verify during finalize review.

---

## Feature 3: Tool defer_loading (Lazy/Progressive Tool Schema Loading)

### CRITICAL — Pitfall 3.1: Deferred Tools Not Available When LLM Calls Them Immediately

**What goes wrong:** If the LLM is given a stub description for a deferred tool (`"use tool_use to
invoke extended tools"`) but the user's first message triggers the LLM to call a deferred tool
directly by name, the executor receives a tool call for a name it cannot resolve. The dispatcher
returns a tool-not-found error to the LLM, which may hallucinate a response or loop.

**Warning sign:** `tool_use` calls in the LLM output reference tool names not in the current
context. The `tool_loop.rs` records a `ToolNotFound` error on the first turn. Agents with many
YAML tools start producing spurious `tool_not_found` errors on first-turn tool calls.

**Prevention:** Deferred tools must be loaded on-demand before their executor is called — not only
on explicit `tool_use(action="describe")` invocation. Intercept the tool dispatch in
`tool_executor.rs`: if the tool name is in the deferred set, load it synchronously before executing.
This "lazy load on first call" pattern is safer than "lazy load only on describe".

**Phase:** Tool defer_loading implementation phase.

---

### Pitfall 3.2: Cache Invalidation When Deferred Tools Are Loaded Mid-Session

**What goes wrong:** Loading a YAML tool schema mid-session changes the tools array sent to
Anthropic. If prompt caching is active, this will bust the tool cache breakpoint (see Pitfall 1.2).
Every tool load mid-session becomes a cache miss + cache write. On a Pi with slow network to
Anthropic, each mid-session tool load adds latency.

**Warning sign:** `cache_creation_input_tokens` spikes on turns where the agent first calls a
deferred YAML tool. Compaction threshold check on token count becomes inaccurate as deferred tools
inflate the prompt size invisibly.

**Prevention:** Load all tools eagerly at session start but send only minimal schema stubs in the
LLM context (name + one-line description), sending full schemas only when the LLM requests them
via `tool_use(action="describe")`. This keeps the tools array stable for caching. This is
architecturally different from "don't include deferred tools at all".

**Phase:** Tool defer_loading implementation phase. Coordinate with prompt caching phase.

---

### Pitfall 3.3: Deferred Tool State Shared Across Sessions via AgentEngine Arc

**What goes wrong:** If the loaded-tools set is stored in `AgentEngine` (which is shared across
sessions via `Arc`), concurrent sessions for the same agent will each read and mutate the same
deferred-tool state. A session that has already loaded tool X will expose it to another session
that hasn't requested it yet, or two sessions will race on the lazy-load mutation.

**Warning sign:** Two concurrent sessions for the same agent see different available tool sets.
A session that has not yet loaded a deferred tool gets a tool call for it from the LLM (which
learned about it from a sibling session's context).

**Prevention:** Deferred-tool loaded state must be **per-pipeline-invocation** (i.e., local to
`execute()` or passed through `BootstrapOutcome`), NOT stored in `AgentEngine`. Use a session-local
`HashSet<String>` populated by each bootstrap. This is consistent with how `LoopDetector` is
managed (per-session, not per-engine).

**Phase:** Tool defer_loading implementation phase.

---

## Feature 4: Hook API (PreToolUse / PostToolUse / SessionStart)

### CRITICAL — Pitfall 4.1: Async Hook Invocation Deadlock with Tokio Mutex

**What goes wrong:** The current `HookRegistry::fire()` is synchronous (`Fn` not `async Fn`).
The docstring in `hooks.rs` explicitly states: "synchronous only (no async DB/HTTP calls inside
hooks)". If the new public Hook API allows user-configurable hooks to be async (e.g., calling a
local HTTP endpoint), and these hooks are fired while holding a lock on any shared state (session
Mutex, approval map DashMap, etc.), a deadlock occurs: the async hook task parks waiting for the
Tokio runtime, which may be saturated with tasks that are themselves waiting for the same lock.

**Warning sign:** Agents hang indefinitely after a tool call when a hook is registered that makes
an HTTP request. No error is visible — the session WAL shows `running` indefinitely. The issue
is non-deterministic (depends on Tokio runtime saturation).

**Prevention:** Keep `fire()` synchronous for blocking hooks. For async hooks, use the
`fire_webhooks()` pattern already in place: `tokio::spawn` a detached task with a hard timeout
(5s). Never hold a Mutex or RwLock guard across a `fire_webhooks()` call. Add a comment at every
callsite: "do not hold session lock while calling fire_webhooks".

**Phase:** Hook API implementation phase. Review all callsites in `execute.rs` and `tool_executor.rs`.

---

### Pitfall 4.2: SessionStart Hook Fires Before Session Is Written to DB

**What goes wrong:** A `SessionStart` hook that reads the session from the DB (e.g., to check
session metadata) will see a `NotFound` error if the hook fires before `bootstrap.rs` has
persisted the new session row. The `bootstrap.rs` creates the session record, then fires hooks —
but if the order is swapped (hook fires during bootstrap initialization, before the DB write), the
hook fails and the session never starts.

**Warning sign:** Webhook endpoint receives `SessionStart` events but the session UUID in the
payload returns 404 when queried via the HydeClaw API within the same request.

**Prevention:** `SessionStart` hook must fire after `bootstrap.rs` completes its DB write and
before the first LLM call. Insert the hook call at a specific documented point in `bootstrap.rs`
with a comment marking the "safe to fire" boundary. Add a test: mock the webhook endpoint, verify
the session exists in DB before the webhook is delivered.

**Phase:** Hook API implementation phase.

---

### Pitfall 4.3: Hook Block Action Does Not Cancel In-Flight Approval Wait

**What goes wrong:** If a `PreToolUse` hook returns `Block`, the tool call is cancelled. But if
the tool was already submitted to the approval system (the approval DB row was created and an
approval waiter `tokio::oneshot` is registered), blocking the tool via hook leaves an orphaned
approval entry. The approval UI will show a pending approval that can never be resolved because
the tool was already rejected by the hook.

**Warning sign:** Approval UI shows "ghost" pending approvals that persist after the agent
session ends. `approval_manager` DashMap leaks entries for sessions that ended with hook-blocked
tools.

**Prevention:** The hook must be fired **before** the approval row is created. In `execute.rs`,
the hook check must be the very first thing in the tool dispatch path — before `needs_approval()`
is checked and before any DB write. Document this ordering invariant in the dispatch code.

**Phase:** Hook API implementation phase. Add ordering test: hook block must not create an approval row.

---

### Pitfall 4.4: Per-Agent Hook Config TOML Deserialization Breaks Existing Agents

**What goes wrong:** Adding a new `[agent.hooks]` TOML section to the agent config schema must be
backward-compatible. If the deserialization uses `#[serde(deny_unknown_fields)]` or similar strict
mode, agents without a `[agent.hooks]` section will fail to load after the schema change. The
existing `AgentConfig` deserialization must handle missing `hooks` as `Default::default()`.

**Warning sign:** Existing agents fail to start after the v0.20.0 upgrade with a TOML parse error
mentioning `hooks` or a missing field. The base agent (which cannot be deleted) fails to load.

**Prevention:** All new TOML fields must use `#[serde(default)]`. Add a snapshot test: deserialize
every fixture agent TOML that does NOT have `[agent.hooks]` and verify no error. Run this test
before any code that reads the hooks config.

**Phase:** Hook API implementation phase (TOML schema). This is a TDD prerequisite.

---

## Feature 5: Model Routing (Haiku→Sonnet→Opus)

### CRITICAL — Pitfall 5.1: Routing Provider Std::Mutex Poisoned Under Panic

**What goes wrong:** `RoutingProvider.cooldowns` uses `std::sync::Mutex`. If any code path
panics while holding the cooldown lock (even in a dependency — unlikely but possible in complex
async code), the Mutex is poisoned. The existing code already handles this with
`.unwrap_or_else(|e| e.into_inner())` (poison recovery), which is correct. However, if a new
routing condition evaluation causes a panic (e.g., an index out of bounds in the new
complexity-based routing logic), the cooldown state is lost and all subsequent calls see zero
cooldowns — potentially hammering a failing provider.

**Warning sign:** After a panic in the routing path, the metrics show sudden bursts of 5xx errors
from a provider that was previously on cooldown. Logs show "cooldowns Mutex poisoned, recovering".

**Prevention:** The complexity evaluation logic for Haiku→Sonnet→Opus routing must not panic. Use
`get()` not `[]` for slice access. Use `checked_div` for token-ratio calculations. Add a
`#[test] fn routing_condition_no_panic_on_empty_messages()` test.

**Phase:** Model routing implementation phase.

---

### Pitfall 5.2: Complexity Score Based on Last Message Length Is Insufficient for Multi-Turn Sessions

**What goes wrong:** `select_route()` currently reads `last_user_msg.len()` to decide `short` vs.
`long`. A long conversation where the last user message is "ok" (2 bytes) but the accumulated
context is 150K tokens will route to Haiku (cheapest/fastest) even though the full-context LLM
call is expensive. The routing decision must account for the accumulated context size, not just
the latest message.

**Warning sign:** Expensive multi-turn sessions with short final messages are routed to Haiku,
causing quality regressions ("I forgot our earlier conversation") rather than cost savings.

**Prevention:** Pass token count from the previous LLM call (`last_prompt_tokens` from
`Compressor`) as a routing hint. Add a `context_heavy` routing condition: if
`last_prompt_tokens > 50000`, route to a high-quality model regardless of message length. The
condition set must include context-size-aware rules, not just message-length rules.

**Phase:** Model routing implementation phase.

---

### Pitfall 5.3: Routing Provider Does Not Propagate cache_control to Fallback Anthropic Providers

**What goes wrong:** When the primary Anthropic provider (with `prompt_cache: true`) fails and
the `RoutingProvider` falls back to a second Anthropic provider (e.g., Haiku as primary, Sonnet
as fallback), the fallback was built with `prompt_cache: None` in `ProviderOverrides`. The route
config currently only passes `prompt_cache: None` to all routes.

**Warning sign:** Fallback Anthropic provider calls show no cache hits in logs even though the
primary does. Token costs on fallback routes are higher than expected.

**Prevention:** In `create_routing_provider`, propagate `prompt_cache` from the provider row's
`opts.prompt_cache` field to the `ProviderOverrides` for each route. The `ProviderRow.options`
already stores `prompt_cache` per-provider — use that value when building the route overrides
instead of defaulting to `None`.

**Phase:** Model routing implementation phase. Review with prompt caching phase.

---

## Feature 6: REF-03 — Rate Limiter DashMap Swap

### CRITICAL — Pitfall 6.1: DashMap Read-Guard Held Across Await Points

**What goes wrong:** DashMap's shard locks are `std::sync::RwLock` internally. If code holds a
DashMap read/write guard (returned by `get()`, `get_mut()`, `entry()`) across an `.await` point
in an async function, the guard is not `Send` (it holds a raw pointer to the shard). This causes
a compile error in some configurations, or — if the guard is dropped before the `.await` — a
subtle bug where the guard is released unexpectedly early. The existing `Mutex<HashMap>` approach
avoids this because `tokio::sync::Mutex` guards ARE `Send`.

**Warning sign:** Compile error: "future cannot be sent between threads safely" mentioning
`DashMap` types. Or, at runtime, a counter that increments and then drops to zero unexpectedly
between an async operation and the next read.

**Prevention:**
- Never hold a DashMap guard across `.await`. Fetch, clone, drop — then await.
- Pattern: `let val = map.get("key").map(|v| *v); // explicit deref/clone, guard dropped here`
- Add a compile-time test using `#[tokio::test]` that exercises the async path with `Send` assertions.
- In the `AuthRateLimiter` and `RequestRateLimiter`, the hot path (`record_failure`, `check`)
  does NOT have await points inside the lock — this is the safe design to preserve.

**Phase:** REF-03 implementation phase (first phase to implement).

---

### Pitfall 6.2: DashMap Default Shard Count Exceeds Pi Memory Budget

**What goes wrong:** DashMap's default shard count is `(CPU count * 4)` rounded up to the next
power of two. On a Pi 4 (4 cores), this gives 16 shards. Each shard allocates its own
`RwLock<HashMap>`. For a rate limiter with few entries (10-50 IPs), the overhead of 16 shards
each with their own allocation and lock is larger than the original single `tokio::sync::Mutex<HashMap>`.
On Pi (1-8 GB RAM), excessive shard allocation for small maps wastes memory and increases
cache pressure.

**Warning sign:** RSS memory of the hydeclaw-core binary increases by more than 5 MB after
REF-03. The Pi's `free -m` shows measurably less available memory.

**Prevention:** Use `DashMap::with_capacity_and_shard_amount(capacity, 4)` to limit shard count
for the rate limiter maps. The rate limiter serves only unique IPs seen in the current window —
4 shards is sufficient. Profile RSS before and after on the Pi.

**Phase:** REF-03 implementation phase.

---

### Pitfall 6.3: Background Sweeper and Hot Path Deadlock on Same DashMap Shard

**What goes wrong:** The rate limiter has a background sweeper task (runs every 60s) and a hot
path in middleware (runs on every request). With DashMap, both will acquire shard write-locks.
If the sweeper calls `retain()` (which holds a write lock on every shard sequentially) while
the hot path tries to `insert()` into the same shard, neither deadlocks but the hot path spins
waiting. Under high request rate on Pi's 4-core scheduler, the sweeper's full-map `retain()`
can cause noticeable latency spikes during the sweep window.

**Warning sign:** HTTP request latency spikes to 50-100ms every 60 seconds, aligned with the
sweeper interval. Visible in the `/api/health/dashboard` metrics.

**Prevention:** Replace `retain()` (full-map lock) with a two-phase approach: collect keys to
remove while holding minimal locks, then remove them in a second pass. Or keep the sweeper
interval at 60s but use `DashMap::remove()` per-key (shard-local, does not block other shards).

**Phase:** REF-03 implementation phase.

---

## TDD-Specific Pitfalls

### Pitfall T.1: Prompt Caching Tests Require Live Anthropic API

**What goes wrong:** Cache hit/miss behavior (`cache_read_input_tokens > 0`) can only be verified
against the live Anthropic API. A mock that returns fixed token counts cannot simulate cache
behavior. Tests that assert "second identical request gets cache hit" will never pass in unit test
mode against a `MockProvider`.

**Prevention:** Split cache tests into two tiers:
1. **Unit tests** (no network): assert that `build_request_body` with `prompt_cache: true`
   produces JSON containing `"cache_control": {"type": "ephemeral"}`. These run in CI.
2. **Integration tests** (live API, `#[ignore]`): assert `cache_read_input_tokens > 0` on second
   call. Run manually or in a separate CI job with credentials.

**Phase:** Cache implementation phase.

---

### Pitfall T.2: Auto-Compaction Tests Require a Mock LLM That Returns Token Counts

**What goes wrong:** `should_compress()` depends on `last_prompt_tokens`, which comes from the
LLM response `usage.input_tokens`. The existing `MockProvider` in the test harness must return
a `TokenUsage` with realistic token counts, or `should_compress()` always returns `false`
(because `last_prompt_tokens == 0`). Tests that exercise the compaction path without configuring
the mock's token counts will never trigger compaction.

**Prevention:** Extend `MockProvider` to accept a `token_usage` configuration option that returns
a specific `TokenUsage` per call. Add a helper: `MockProvider::with_token_usage(input: 90_000,
output: 1_000)` so compaction tests can simulate a near-full context window.

**Phase:** Auto-compaction implementation phase.

---

### Pitfall T.3: Hook Webhook Tests Race with tokio::spawn Fire-and-Forget

**What goes wrong:** `fire_webhooks()` spawns a detached task that POSTs to the webhook URL. A
test that asserts "the webhook endpoint received the event" has a race condition: the assertion
may run before the spawned task has executed. `tokio::time::sleep` workarounds are brittle.

**Prevention:** Use a `tokio::sync::Notify` or a `tokio::sync::mpsc::channel` in the test server
to signal that the webhook was received. The test waits on the channel with a timeout. This is
the correct approach — the existing `fire_webhooks_is_fire_and_forget` test only verifies timing,
not content. Content verification requires the notify/channel pattern.

**Phase:** Hook API implementation phase.

---

### Pitfall T.4: Routing Tests Are Hard to Test Without Mocking the DB Provider Lookup

**What goes wrong:** `create_routing_provider` does async DB lookups (`get_provider_by_name`) to
resolve route connections. Tests for routing behavior (condition matching, failover, cooldown)
require either a live DB (slow) or the `new_for_test` / `new_for_test_with_cap` constructors that
skip DB resolution. New routing conditions (complexity-based) must be testable via the same
test-only constructors. If the complexity calculation pulls from the DB (e.g., per-agent model
thresholds), the test path breaks.

**Prevention:** All routing condition evaluation must be pure functions of the `messages` and
`tools` inputs plus configuration passed at construction time. No async DB calls inside
`select_route()`. Use the existing `new_for_test` constructor pattern for all new routing tests.

**Phase:** Model routing implementation phase.

---

## ARM64/Pi-Specific Pitfalls

### Pitfall ARM.1: Hook Overhead on the SSE Hot Path Degrades Latency

**What goes wrong:** Every tool call fires two hooks: `BeforeToolCall` and `AfterToolResult`.
On the Pi (ARM64 Cortex-A72), even synchronous function call overhead is meaningful when a session
calls 20+ tools. If the hook registry has 5 handlers (logging + block_tools + 3 webhooks), each
tool call iterates 5 handlers twice. For fire_webhooks, `tokio::spawn` is called for each webhook
per event, which adds thread pool overhead on the Pi's 4-core scheduler.

**Warning sign:** Session processing time increases by more than 50ms per tool call after enabling
hooks. Metrics: `tool_duration_ms` histogram shifts right.

**Prevention:** `fire()` (synchronous hooks) is O(n handlers) and is fast — keep it. For
`fire_webhooks`, the early return on `self.webhooks.is_empty()` is already present. Add an
additional early return: skip spawning for any event type that no registered webhook subscribes to
(filter before spawn, not after). This avoids `tokio::spawn` overhead for unwatched events.

**Phase:** Hook API implementation phase.

---

### Pitfall ARM.2: Compaction LLM Call Memory Spike During Long Sessions

**What goes wrong:** Auto-compaction triggers when the context is at 85% of the limit — for a
200K-token model, that is 170K tokens of accumulated messages. To summarize them, the compaction
LLM call must serialize all 170K tokens of messages into a JSON body, hold them in memory
simultaneously with the ongoing session state, and receive a response. On Pi 4 (1-4 GB RAM),
serializing 170K tokens of message history twice (once for the compaction call, once for the
in-memory `messages` Vec) can spike memory by 300-500 MB.

**Warning sign:** `hydeclaw-core` OOM-killed by the Pi's kernel during long sessions with
auto-compaction enabled. `/proc/meminfo` shows spike in anonymous pages correlated with
compaction events.

**Prevention:** The compaction call should receive a truncated message list — not all 170K tokens,
but a sliding window of the most recent N messages sufficient to produce a useful summary (e.g.,
the last 40K tokens). The `previous_summary` field already provides continuity for earlier context.
Do not serialize the full accumulated history for the compaction prompt.

**Phase:** Auto-compaction implementation phase. Required for Pi deployment.

---

## Phase-Specific Warnings Summary

| Phase Topic | Likely Pitfall | Mitigation |
|-------------|---------------|------------|
| Prompt caching | cache_control on non-Anthropic providers (1.1) | Provider-type guard in ProviderOverrides resolution |
| Prompt caching | Tool array instability busting cache (1.2) | Cache breakpoint on stable tail of tools array |
| Prompt caching + compaction | Token count mismatch with cached tokens (2.1) | Use combined token sum for compaction threshold |
| Auto-compaction | Compaction blocks on Pi OOM (ARM.2) | Truncate to sliding window, not full history |
| Auto-compaction | Cancellation not respected (2.2) | Thread CancellationToken through compaction call |
| Auto-compaction | Anti-thrash kills cron sessions (2.3) | Reset counter on genuine context growth |
| Tool defer_loading | Tools called before loaded (3.1) | Load on first dispatch call, not only on describe |
| Tool defer_loading | Cache bust on mid-session load (3.2) | Coordinate tool loading with caching phase |
| Tool defer_loading | Per-engine state sharing across sessions (3.3) | State must be per-pipeline-invocation, not per-engine |
| Hook API | Async deadlock in hooks (4.1) | Keep fire() sync; use fire_webhooks() pattern for async |
| Hook API | SessionStart before DB write (4.2) | Fire after bootstrap DB write, document boundary |
| Hook API | Ghost approvals from blocked hooks (4.3) | Hook check must precede approval row creation |
| Hook API | TOML backward compat (4.4) | serde(default) on all new fields; snapshot test |
| Model routing | std::Mutex poisoned on routing panic (5.1) | No panicking code in select_route; test empty messages |
| Model routing | Routing ignores accumulated context size (5.2) | Add context_heavy routing condition |
| Model routing | Fallback Anthropic loses cache config (5.3) | Propagate prompt_cache from ProviderRow.options |
| REF-03 | DashMap guard held across await (6.1) | Fetch-clone-drop pattern; no guard across .await |
| REF-03 | Excess shards on Pi (6.2) | with_capacity_and_shard_amount(N, 4) for rate limiters |
| REF-03 | Sweeper latency spikes (6.3) | Per-key remove instead of full-map retain |
| TDD | Cache tests require live API (T.1) | Two-tier: unit (JSON shape) + integration (live, ignored) |
| TDD | Compaction tests need token mocks (T.2) | Extend MockProvider with configurable TokenUsage |
| TDD | Webhook fire-and-forget race (T.3) | Use channel/Notify in test server, not sleep |
| TDD | Routing tests need DB-free constructors (T.4) | select_route must be pure; use new_for_test |

## Sources

- Codebase analysis: `crates/hydeclaw-core/src/agent/providers/anthropic.rs` — cache_control impl, token usage fields
- Codebase analysis: `crates/hydeclaw-core/src/agent/compressor.rs` — compaction state and threshold logic
- Codebase analysis: `crates/hydeclaw-core/src/agent/hooks.rs` — hook registry design and fire_webhooks pattern
- Codebase analysis: `crates/hydeclaw-core/src/agent/providers/routing.rs` — routing provider, cooldown Mutex, failover logic
- Codebase analysis: `crates/hydeclaw-gateway-util/src/rate_limiter.rs` — existing Mutex<HashMap> pattern for REF-03 baseline
- Codebase analysis: `crates/hydeclaw-core/src/agent/pipeline/tool_defs.rs` — tool catalogue and OnceLock usage
- Codebase analysis: `crates/hydeclaw-core/src/gateway/middleware.rs` — rate limiter middleware integration
- Anthropic prompt caching constraints: min 1024 tokens, system message must be array format, breakpoint stability requirement
- DashMap: guards are not Send across await boundaries — standard documented limitation
