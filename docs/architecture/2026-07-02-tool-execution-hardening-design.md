# Design: Tool-Execution Hardening

Date: 2026-07-02
Area: backend (`crates/opex-core`)
Source: the §13 recommendations of `docs/architecture/2026-07-02-agent-tools-and-services-report.md`

## 1. Overview

Five independent hardening changes to the agent tool-execution pipeline, delivered as one
cohesive spec → one plan → one `make remote-deploy`. Each is an isolated unit with a
well-defined boundary; only **R2** requires a DB migration.

Implementation order (ascending risk): **R1 → R5 → R4 → R3 → R2**.

| # | Change | DB migration | Config | Risk |
|---|---|---|---|---|
| R1 | Remove dead `_session_tool_state` param | no | no | trivial |
| R5 | Per-tool TTL/threshold for semantic search cache | no | `[semantic_cache]` in opex.toml | low |
| R4 | Total time budget for decision-webhook chain | no | hooks config | low |
| R3 | LoopDetector warm-up restores repeat-hash detection | no (JSONB payload) | no | medium |
| R2 | `penalty_score` scoped per-(agent, tool) | **yes** | no | medium |

**Non-goals:** touching the YAML-tool cache path (already per-tool via `cache.ttl`); changing
`SessionToolState`/describe-cache behavior (stays live); altering the per-hook timeout/fail-mode
semantics (only *adding* a chain-level budget); reworking `PenaltyCache`'s 30 s refresh cadence.

## 2. Units

### R1 — Remove dead `_session_tool_state` parameter (cleanup)

**Current:** `execute_tool_calls_partitioned` (`agent/pipeline/parallel.rs:186-210`) takes
`_session_tool_state: Option<Arc<…SessionToolState>>` (line 207) — never read (leading underscore;
comment "Kept for future per-session tool state access"). It's retrieved and passed at
`agent/engine/tool_executor.rs:128,156`.

`SessionToolState` itself (the per-session describe-cache) is **live** via
`agent/tool_handlers/tool_use.rs:114,152`, `agent/tool_registry.rs:54-90`,
`agent/agent_config.rs:57`, and `agent/dispatcher/state.rs`. Those stay untouched.

**Change:** delete the param from the `parallel.rs` signature; delete its retrieval (tool_executor.rs:128-130)
and the argument at the call site (tool_executor.rs:156). No behavior change (dead code only).

**Tests:** existing `parallel.rs` / tool-executor tests must still compile+pass (compile-time proof
the param was unused). No new test needed.

### R5 — Per-tool TTL/threshold for the semantic search cache

**Current:** `is_tool_cacheable(name)` (`agent/pipeline/parallel.rs:120-125`) is a hardcoded match of
four system tools (`searxng_search`, `brave_search`, `browser_render`, `web_search`); it gates cache use
at **three** call sites — pre-check `:287`, parallel-store `:474`, sequential-store `:614`.
`SemanticCache::check(…, 0.95)` (parallel.rs:296) hardcodes the threshold, and `SemanticCache::store(…, 3600)`
hardcodes the TTL at **two** sites — the parallel branch (`:491`) and the sequential branch (`:632`). All
five sites must route through the config. YAML tools use a **separate** path (`engine_dispatch.rs:333`,
`cfg.ttl`) — out of scope.

**Change:** introduce a `[semantic_cache]` section in `config/opex.toml` mapping tool name → cache config.
(The name `[semantic_cache]` avoids collision with the EXISTING `ToolCacheConfig`/`tools_cache`
`[tools_cache]` at `config/mod.rs:69,513`, which configures the unrelated YAML-tool response cache.)

```toml
[semantic_cache]
# defaults applied to the built-in cacheable tools; override per tool for time-sensitive queries
searxng_search = { ttl_secs = 3600, threshold = 0.95 }
brave_search   = { ttl_secs = 3600, threshold = 0.95 }
browser_render = { ttl_secs = 3600, threshold = 0.95 }
web_search     = { ttl_secs = 300,  threshold = 0.95 }   # example: news/rates → minutes
```

- New `SemanticCacheToolConfig { ttl_secs: u64, threshold: f32 }` (distinct name — see collision note
  above); the four names above are built-in defaults (ttl 3600, threshold 0.95) so an empty
  `[semantic_cache]` reproduces today's behavior exactly.
- **Config-delivery seam (the real work):** `execute_tool_calls_partitioned` is a free function with NO
  config access — config reaches it today only through the `ToolExecutor` trait (e.g.
  `agent_safety_timeout()`, `parallel.rs:81`, impl `engine/tool_executor.rs:233`). Add a trait method
  `fn semantic_cache_config(&self, tool: &str) -> Option<SemanticCacheToolConfig>` mirroring that
  pattern; its impl reads `self.cfg()`. `is_tool_cacheable(name)` becomes
  `executor.semantic_cache_config(name).is_some()`, and the check/store sites read `threshold`/`ttl_secs`
  from it.
- `SemanticCache::check`/`store` signatures already carry threshold/ttl (`semantic_cache.rs:10-16,44-51`),
  so the change is threading looked-up values into the **five** sites listed in Current (check `:296`;
  store `:491` + `:632`; gates `:287`,`:474`,`:614`).

**Tests:** unit test that (a) an empty config yields the four defaults with ttl 3600/threshold 0.95;
(b) an override changes the looked-up ttl; (c) an unknown tool is not cacheable.

### R4 — Total time budget for the decision-webhook chain

**Current:** `HookRegistry::fire_decision` (`agent/hooks.rs:120-206`) loops the decision webhooks
sequentially; each POST has its own timeout `cw.cfg.timeout_ms.min(30_000)` and its own
`on_failure: Open|Closed`. There is **no cumulative budget** — N slow hooks add up unbounded per call.

**Change:**
- Add `total_webhook_timeout_ms: Option<u64>` (default **10000** via a `default_*` fn, matching the
  file's `#[serde(default=…)]` convention — cf. per-hook `default_hook_timeout_ms=3000` at
  `config/mod.rs:993`) and `on_chain_timeout: FailureMode` (default **Open**) to `HooksConfig`
  (`config/mod.rs:954`, per-agent `[agent.hooks]`).
- **Registry seam (required):** `fire_decision` is a method on `HookRegistry`, which stores only
  `Vec<CompiledWebhook>` + clients (`hooks.rs:33-40`) — it does NOT hold `HooksConfig`. So also add the
  two fields to `HookRegistry`, extend `set_webhooks(...)` (`hooks.rs:79`) to accept+store them, and
  update the one production call site `gateway/handlers/agents/lifecycle.rs:150` to pass them.
- In `fire_decision`, stamp `Instant::now()` before the loop; before each webhook POST, if
  `elapsed() >= total_webhook_timeout_ms`, resolve per `on_chain_timeout`: **Open → `break` out of the
  loop and fall through to the existing accumulation tail (`hooks.rs:202-205`), so any
  ModifyArgs/TransformResult already gathered still apply and the tool proceeds; Closed →
  `HookDecision::Block("webhook chain budget exceeded")`.**
- Rationale for the Open default: the chain budget is a *latency guard*; each hook already enforces its
  own security fail-mode. This was the user-approved decision.

**Tests:** unit test with two stub webhooks where the first consumes the whole budget → the second is
skipped; assert Open proceeds and Closed blocks. (Webhook HTTP is mocked/stubbed at the `fire_decision`
seam; no live server.)

### R3 — LoopDetector warm-up restores repeat-hash detection

**Current:** after a crash/reopen, `LoopDetector::warm_up_from_timeline` (`agent/tool_loop.rs:138-144`)
replays only `tool_name + success` (via `record_result_from_timeline`, line 110), so the error-streak
is restored but **hash-based repeat detection (`last_hash`/`consecutive`/`recent`) is not** — a
repeating loop gets a fresh budget of up to `break_threshold` (default 10) iterations after restart.
`tool_start` events persist an `args_hash` in the JSONB payload (parallel.rs:335) but keyed on the RAW
`tc.name`; `tool_end` events persist none, and `load_tool_events` reads from `tool_end`.

**CRITICAL correctness constraint (review C1):** the live detector does NOT key on `tc.name`. It keys on
`loop_detector_key(tc)` (`parallel.rs:92-102,451`), which for the dispatcher path returns
`"tool_use:{action}"` (the default path — `rewrite.rs:44-50` leaves `tc.name == "tool_use"`). So the
persisted+replayed hash MUST be `hash_call_raw(&loop_detector_key(tc), &tc.arguments)`, NOT
`hash_call_raw(&tc.name, …)`. Hashing `tc.name` would produce `hash("tool_use")` while the live check
compares `hash("tool_use:search")` — they never match, silently defeating the fix for the most common
loop shape. (For plain direct tools `loop_detector_key == tc.name`, which is why the mismatch is easy to
miss.) The existing `start_payload` at `parallel.rs:335` has this same latent bug and is corrected here too.

**Change (no migration — JSONB payload):**
- Add `args_hash` to `end_payload` (`agent/pipeline/parallel.rs:338-346`) and the sequential-path
  end-event writer, computing it as `hash_call_raw(&loop_detector_key(tc), &tc.arguments)`. Fix
  `start_payload` (`:335`) to use the same `loop_detector_key` source (both `tool_end` writers are at
  `parallel.rs:501,641`; no other timeline tool-event writers exist).
- Add `args_hash: Option<String>` to `TimelineToolEvent` (`opex-db/src/session_timeline.rs:136-139`)
  and extend `load_tool_events` (`:143-167`) to read `payload->>'args_hash'`. (Other timeline readers —
  `finalize.rs`, `evolution.rs` — read only `tool_name`/`tool_call_id`, so the additive field is safe.)
- Extend the warm-up: `record_result_from_timeline` (tool_loop.rs:110) and `warm_up_from_timeline`
  (:138) reconstruct `last_hash`, `consecutive`, and the `recent` deque from the replayed hashes so that
  `consecutive` reproduces the SAME count `record_execution` would have (consecutive IDENTICAL hashes —
  reset to 1 on a differing hash, increment on a match). Legacy events lacking `args_hash` fall back to
  name-only (error-streak only, as today), no panic.

**Tests:** extend `tool_loop.rs` tests — (1) a timeline of N identical `tool_end` events (same
`loop_detector_key` + args_hash) → warm up → assert `check_limits` on the next identical call returns
`Break` (today it would NOT); (2) a `tool_use`-style key (`"tool_use:search"`) round-trips (guards C1);
(3) a legacy event with no `args_hash` → name-only, no panic. NOTE: adding `args_hash` to
`TimelineToolEvent` breaks the existing struct-literal test constructors (`tool_loop.rs:233-234,259-261`)
— update them (or add `..Default::default()`).

### R2 — `penalty_score` scoped per-(agent, tool)  *(the one migration)*

**Current:** `tool_quality` PK = `tool_name` (`migrations/001_init.sql:410`); `PenaltyCache` is a
process-wide `HashMap<String, f32>` (`db/tool_quality.rs:15-61`) refreshed every 30 s. A tool that
degrades under one agent's config is deprioritized for **all** agents. Agent identity is available at
the record site (`engine_dispatch.rs:84-91`, `self.cfg().agent.name`) but is dropped from
`AuditEvent::ToolQuality`.

**Change:**
- **Migration (fresh start, user-approved):** new migration `070_tool_quality_per_agent.sql` —
  `DELETE FROM tool_quality;` then add `agent_name TEXT NOT NULL DEFAULT ''`, drop the old PK, add
  composite PK `(agent_name, tool_name)`. (Penalty is transient quality data that self-heals within the
  rolling 20-call window; discarding the global rows avoids ambiguous backfill.)
- Thread `agent_name` (two edit sites for the event — the enum variant AND the worker match-arm):
  - `AuditEvent::ToolQuality` — the **enum variant** (`db/audit_queue.rs:22-27`) gains `agent_name: String`,
    and the **worker match-arm** that handles it (`:80-96`) forwards it to `record_tool_result`.
  - `engine_dispatch.rs:86` passes `self.cfg().agent.name` into the event (agent name confirmed available
    there via `&self` on AgentEngine).
  - `record_tool_result` (`db/tool_quality.rs:73`) gains an `agent_name` param; UPSERT `ON CONFLICT
    (agent_name, tool_name)`.
- `PenaltyCache` → `HashMap<String /*agent*/, HashMap<String /*tool*/, f32>>`; `get_all_penalties`
  (`:165`) selects `(agent_name, tool_name, penalty_score)`; `get_penalties(agent_name)` returns that
  agent's submap (empty map if unseen). The ONLY consumer is `engine/context_builder.rs:394` inside
  `tool_penalties()` (which has `self.cfg().agent.name`); its return type stays `HashMap<String,f32>` (the
  submap), so the downstream at `context_builder.rs:541` is unaffected.
- `get_degraded_tools` (def `db/tool_quality.rs:179`) — its only consumer is the GLOBAL health endpoint
  `gateway/handlers/monitoring/doctor.rs:471` with no "current agent" in scope. So "per agent" here means
  the query SELECTs `agent_name` as an additional column and the JSON payload gains `agent_name` (a
  grouped/labelled list) — NOT a per-agent filter.

**Tests:** `#[sqlx::test(migrations = "../../migrations")]` (the concrete relative path for tests under
`crates/opex-core/src/db/`, cf. `db/providers.rs:217`) — record failures for tool T under agent A and
successes for T under agent B; assert `get_penalties("A")["T"]` is penalized while
`get_penalties("B")["T"]` is not; assert the composite UPSERT accumulates correctly.

## 3. Cross-cutting

- **Testing:** pure-Rust units for R1/R4/R5 and the R3 detector logic; `#[sqlx::test]` (auto-migrations,
  isolated Postgres :5434 via `make test-db`) for R2 and the R3 timeline read/write. Full gate before
  deploy: `make check` + `make lint` + `make test-db`.
- **Migration count:** exactly one new file (R2). R3 rides the existing JSONB payload.
- **Shared merge surface:** R1, R3, and R5 all edit `parallel.rs::execute_tool_calls_partitioned` (R1 the
  signature; R3 `start_payload`/`end_payload` + warm-up; R5 `is_tool_cacheable` + check/store). The
  implementation order R1→R5→…→R3 lands R1's signature change first, then R5 and R3 edit disjoint regions
  of the body — no logical conflict, but treat this one function as a shared surface when sequencing.
- **Backward compatibility:** R5 empty config = today's behavior; R4 absent config = today's behavior
  (no budget); R3 legacy events without `args_hash` fall back to name-only; R1 is dead-code removal.
  API contracts unchanged except the additive `agent_name` in the doctor degraded-tools payload.
- **Deploy:** single `make remote-deploy` (git pull → cargo build → atomic swap → restart) after the
  gate is green.

## 4. Risks & mitigations

- **R2 migration on a live table:** `DELETE` + PK change is fast on a small transient table; the 30 s
  cache simply repopulates. Mitigation: migration is idempotent and gated behind the standard startup
  auto-migrate.
- **R3 hash reconstruction correctness:** the replayed `consecutive` must match live semantics — covered
  by the "N identical events → next call breaks" test mirroring `record_execution`.
- **R4 default budget (10 s):** conservative; individual hooks keep their own timeouts, so the chain
  budget only bites pathological multi-hook chains. Configurable.
