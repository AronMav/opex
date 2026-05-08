---
phase: 68-prompt-caching
plan: 01
subsystem: api
tags: [anthropic, prompt-caching, cache_control, provider, agent-config]

# Dependency graph
requires: []
provides:
  - "AgentSettings.prompt_cache: bool field wired from TOML through ProviderOverrides into AnthropicProvider"
  - "Stable-tool cache breakpoint via all_system_tool_names() lookup (Pitfall 1.2 fix)"
  - "CACHE-01: operator can toggle prompt_cache = true/false in agent TOML"
  - "CACHE-04: non-Anthropic providers ignore prompt_cache silently"
affects:
  - 68-prompt-caching
  - 70-routing

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Stable-tool lookup via all_system_tool_names() for cache breakpoint placement — generalizable to future provider implementations"
    - "Agent-level bool field with #[serde(default)] wired through payload → AgentSettings → ProviderOverrides chain"

key-files:
  created: []
  modified:
    - crates/hydeclaw-core/src/config/mod.rs
    - crates/hydeclaw-core/src/agent/providers/factory.rs
    - crates/hydeclaw-core/src/agent/providers/anthropic.rs
    - crates/hydeclaw-core/src/agent/providers/build_provider_tests.rs
    - crates/hydeclaw-core/src/gateway/handlers/agents/schema.rs
    - crates/hydeclaw-core/src/gateway/handlers/agents/crud.rs

key-decisions:
  - "Use all_system_tool_names() OnceLock catalogue for stable-tool breakpoint lookup — O(1) after first call, no DB dependency"
  - "agent TOML prompt_cache: Some(false) overrides provider options prompt_cache: true — agent-level config wins over provider defaults (Pitfall 3)"
  - "routing.rs intentionally left with prompt_cache: None, TODO(Phase-70/ROUTE-02) marker added"
  - "Tests changed from #[test] to #[tokio::test] because SecretsManager::new_noop() uses PgPool::connect_lazy which requires Tokio context"

patterns-established:
  - "Stable-tool breakpoint: iterate tools in reverse, find last whose name is in all_system_tool_names(), stamp only that index"
  - "Agent payload → AgentSettings chain: add Option<T> to payload, unwrap_or in build_agent_config, preserve-existing logic in put_agent merge"

requirements-completed: [CACHE-01, CACHE-04]

# Metrics
duration: 35min
completed: 2026-05-08
---

# Phase 68 Plan 01: Prompt Cache TOML Knob + Tool Breakpoint Fix Summary

**AgentSettings.prompt_cache bool wired from agent TOML through ProviderOverrides into AnthropicProvider, with stable-tool cache breakpoint via all_system_tool_names() replacing the broken last-tool-overall approach**

## Performance

- **Duration:** 35 min
- **Started:** 2026-05-08T15:33:00Z
- **Completed:** 2026-05-08T16:08:34Z
- **Tasks:** 2
- **Files modified:** 6

## Accomplishments

- Added `pub prompt_cache: bool` with `#[serde(default)]` to `AgentSettings` — legacy TOMLs parse without the field defaulting to false
- Threaded `agent.prompt_cache` through `resolve_provider_from_row` into `ProviderOverrides.prompt_cache: Some(bool)` — agent TOML value now reaches `AnthropicProvider::build_request_body` via `self.prompt_cache`
- Fixed Pitfall 1.2: replaced `tools_json.last_mut()` with `all_system_tool_names()` reverse-scan so the cache breakpoint lands on the last SYSTEM tool, not the last YAML/MCP tool
- 6 new tests: 2 TOML parse tests, 3 breakpoint placement tests, 1 CACHE-04 non-Anthropic silent-ignore test

## Task Commits

1. **Task 1: Add AgentSettings.prompt_cache + factory wiring** - `86e3784d` (feat)
2. **Task 2: Fix tool cache breakpoint to last system tool** - `a7b322d8` (fix)

## Files Created/Modified

- `crates/hydeclaw-core/src/config/mod.rs` — Added `pub prompt_cache: bool` field with doc comment and `#[serde(default)]`; 2 new TOML deserialization tests; updated 2 roundtrip test struct constructions
- `crates/hydeclaw-core/src/agent/providers/factory.rs` — Added `agent_prompt_cache: bool` param to `resolve_provider_from_row`; changed `prompt_cache: None` to `prompt_cache: Some(agent_prompt_cache)`; added TODO(Phase-70/ROUTE-02) comment
- `crates/hydeclaw-core/src/agent/providers/anthropic.rs` — Replaced broken breakpoint logic with `all_system_tool_names()` stable-tool lookup; 3 new `#[tokio::test]` breakpoint tests
- `crates/hydeclaw-core/src/agent/providers/build_provider_tests.rs` — Added CACHE-04 test: OpenAI provider accepts `prompt_cache: Some(true)` without error
- `crates/hydeclaw-core/src/gateway/handlers/agents/schema.rs` — Added `prompt_cache: Option<bool>` to `AgentCreatePayload`; wired into `build_agent_config`
- `crates/hydeclaw-core/src/gateway/handlers/agents/crud.rs` — Added preserve-existing logic for `prompt_cache` in PUT agent merge

## Decisions Made

- **Tokio context for tests:** Plan specified `#[tokio::test]` for breakpoint tests but the initial implementation used `#[test]`. Tests failed with "requires a Tokio context" because `SecretsManager::new_noop()` calls `PgPool::connect_lazy`. Fixed by using `#[tokio::test]`.
- **routing.rs untouched:** Per plan spec, `routing.rs:110` retains `prompt_cache: None` with a TODO marker for Phase 70.
- **Agent API surface:** Added `prompt_cache: Option<bool>` to `AgentCreatePayload` and `build_agent_config` so agents can be created/updated with `prompt_cache = true` via API.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 2 - Missing Critical] Added prompt_cache field to AgentCreatePayload and PUT merge logic**
- **Found during:** Task 1 (compile step)
- **Issue:** `build_agent_config` in `schema.rs` constructs `AgentSettings` exhaustively — missing `prompt_cache` caused compile error. Additionally, the PUT merge in `crud.rs` needed preserve-existing logic to avoid clearing `prompt_cache` when payload omits the field.
- **Fix:** Added `pub prompt_cache: Option<bool>` to `AgentCreatePayload`; wired `p.prompt_cache.unwrap_or(false)` in `build_agent_config`; added preserve-existing guard in `put_agent` merge
- **Files modified:** `schema.rs`, `crud.rs`
- **Verification:** `cargo check -p hydeclaw-core` exits 0
- **Committed in:** `86e3784d` (Task 1 commit)

**2. [Rule 1 - Bug] Tests changed from #[test] to #[tokio::test]**
- **Found during:** Task 2 (test execution)
- **Issue:** Plan spec showed `#[tokio::test]` for breakpoint tests but this was initially implemented as `#[test]`. Tests panicked with "this functionality requires a Tokio context" because `SecretsManager::new_noop()` uses `PgPool::connect_lazy`.
- **Fix:** Changed all three breakpoint tests to `#[tokio::test] async fn`
- **Files modified:** `anthropic.rs`
- **Verification:** All 3 tests pass
- **Committed in:** `a7b322d8` (Task 2 commit)

---

**Total deviations:** 2 auto-fixed (1 missing critical, 1 bug)
**Impact on plan:** Both auto-fixes necessary for correctness. The API surface extension is the minimal change to make `AgentSettings` compilable with the new field. No scope creep.

## Issues Encountered

- Parallel agent 68-03 was working simultaneously on `DashboardSnapshot` and `monitoring/mod.rs`. This caused initial `cargo check` failures (missing cache fields in monitoring handler). The 68-03 agent had already added the fields to `DashboardSnapshot` struct in `metrics.rs` but the monitoring handler code was partially applied. After `git stash pop`, the monitoring handler had the complete 68-03 changes, resolving the conflict.

## Known Stubs

None — all new fields are fully wired. `prompt_cache = true` in agent TOML will now result in `cache_control: ephemeral` on the system message and the last stable system tool when the provider is Anthropic-typed.

## Next Phase Readiness

- CACHE-01 complete: `AgentSettings.prompt_cache` is available for Plan 02 which adds the CLAUDE.md third breakpoint
- CACHE-04 proven by test: non-Anthropic providers silently ignore `prompt_cache`
- Plan 02 will extend `build_request_body` system field to a multi-block array with CLAUDE.md as a second cached content block
- Phase 70 (ROUTE-02) must update `routing.rs:110` — TODO marker added at the site

---
*Phase: 68-prompt-caching*
*Completed: 2026-05-08*
