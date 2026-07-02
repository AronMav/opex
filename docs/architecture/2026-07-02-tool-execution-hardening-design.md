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
| R5 | Per-tool TTL/threshold for semantic search cache | no | `[tool_cache]` in opex.toml | low |
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
four system tools (`searxng_search`, `brave_search`, `browser_render`, `web_search`).
`SemanticCache::check(…, 0.95)` (parallel.rs:296) and `SemanticCache::store(…, 3600)` (parallel.rs:~491)
hardcode threshold and TTL. YAML tools use a **separate** path (`engine_dispatch.rs:333`, `cfg.ttl`) —
out of scope.

**Change:** introduce a `[tool_cache]` section in `config/opex.toml` mapping tool name → cache config:

```toml
[tool_cache]
# defaults applied to the built-in cacheable tools; override per tool for time-sensitive queries
searxng_search = { ttl_secs = 3600, threshold = 0.95 }
brave_search   = { ttl_secs = 3600, threshold = 0.95 }
browser_render = { ttl_secs = 3600, threshold = 0.95 }
web_search     = { ttl_secs = 300,  threshold = 0.95 }   # example: news/rates → minutes
```

- New `ToolCacheConfig { ttl_secs: u64, threshold: f32 }`; the four names above are built-in defaults
  (ttl 3600, threshold 0.95) so an empty `[tool_cache]` reproduces today's behavior exactly.
- `is_tool_cacheable(name)` → `tool_cache_config(name).is_some()` (a tool is cacheable iff it has an
  entry, defaults included).
- `SemanticCache::check` gains an explicit `threshold` from config (already a param); `store` reads
  `ttl_secs` from config instead of the literal `3600`.
- `SemanticCache::store`/`check` signatures already carry ttl/threshold — no signature churn beyond
  threading the looked-up values.

**Tests:** unit test that (a) an empty config yields the four defaults with ttl 3600/threshold 0.95;
(b) an override changes the looked-up ttl; (c) an unknown tool is not cacheable.

### R4 — Total time budget for the decision-webhook chain

**Current:** `HookRegistry::fire_decision` (`agent/hooks.rs:120-206`) loops the decision webhooks
sequentially; each POST has its own timeout `cw.cfg.timeout_ms.min(30_000)` and its own
`on_failure: Open|Closed`. There is **no cumulative budget** — N slow hooks add up unbounded per call.

**Change:**
- Add `total_webhook_timeout_ms: Option<u64>` (default **10000**) and
  `on_chain_timeout: FailureMode` (default **Open**) to the hooks config struct.
- In `fire_decision`, stamp `Instant::now()` before the loop; before each webhook POST, if
  `elapsed() >= total_webhook_timeout_ms`, stop iterating and resolve per `on_chain_timeout`:
  **Open → return the decision accumulated so far (tool proceeds); Closed → `HookDecision::Block("webhook chain budget exceeded")`.**
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
`tool_start` events already persist `args_hash` in the JSONB payload (parallel.rs:335 via
`hash_call_raw`); `tool_end` events do **not**, and `load_tool_events` reads from `tool_end`.

**Change (no migration — JSONB payload):**
- Add `args_hash` to `end_payload` (`agent/pipeline/parallel.rs:338-346` and the sequential path
  `~557-563`), using the same `LoopDetector::hash_call_raw(name, args)` as the start payload.
- Add `args_hash: Option<String>` to `TimelineToolEvent` (`opex-db/src/session_timeline.rs:136-139`)
  and extend `load_tool_events` (`:143-167`) to read `payload->>'args_hash'`.
- Extend the warm-up: `record_result_from_timeline` (tool_loop.rs:110) and `warm_up_from_timeline`
  (:138) reconstruct `last_hash`, `consecutive`, and the `recent` deque from the replayed hashes
  (falling back to name-only when a legacy event lacks `args_hash`), so hash-repeat detection survives
  a restart.

**Tests:** extend `tool_loop.rs` tests — build a timeline of N identical (name+args_hash) tool_end
events, warm up, and assert the detector is already at/over `break_threshold` (i.e. the next identical
call breaks) — the current behavior would NOT break. Also a legacy-event test (no args_hash → name-only,
no panic).

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
- Thread `agent_name`:
  - `AuditEvent::ToolQuality` (`db/audit_queue.rs:80-96`) gains `agent_name: String`.
  - `engine_dispatch.rs:84-91` passes `self.cfg().agent.name`.
  - `record_tool_result` (`db/tool_quality.rs:73`) gains an `agent_name` param; UPSERT `ON CONFLICT
    (agent_name, tool_name)`.
- `PenaltyCache` → `HashMap<String /*agent*/, HashMap<String /*tool*/, f32>>`; `get_all_penalties`
  selects `(agent_name, tool_name, penalty_score)`; `get_penalties(agent_name)` returns that agent's
  submap (empty map if unseen). Consumer `context_builder.rs:393` already has `self.cfg().agent.name`.
- `get_degraded_tools` (`:179`) gains an optional `agent_name` filter; `/api/doctor` reports degraded
  tools per agent (payload gains `agent_name`).

**Tests:** `#[sqlx::test(migrations = "…/migrations")]` — record failures for tool T under agent A and
successes for T under agent B; assert `get_penalties("A")["T"]` is penalized while
`get_penalties("B")["T"]` is not; assert the composite UPSERT accumulates correctly.

## 3. Cross-cutting

- **Testing:** pure-Rust units for R1/R4/R5 and the R3 detector logic; `#[sqlx::test]` (auto-migrations,
  isolated Postgres :5434 via `make test-db`) for R2 and the R3 timeline read/write. Full gate before
  deploy: `make check` + `make lint` + `make test-db`.
- **Migration count:** exactly one new file (R2). R3 rides the existing JSONB payload.
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
