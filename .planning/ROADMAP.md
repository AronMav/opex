# Roadmap: HydeClaw

## Milestones

- ✅ **v0.2.0–v0.11.0** — Core platform, Chat UI Polish, Engine Dispatcher (Phases 1–39, shipped)
- ✅ **v0.12.0 Chat Redesign** — Phases 40–45 (shipped 2026-04-09)
- ✅ **v0.13.0–v0.18.0** — Chat-store decomposition, sub-routers, async delegation, UI bug fixes, AgentEngine decomposition, backup fixes (Phases 54–60, shipped 2026-04-16)
- ✅ **v0.19.0 Stability & Performance Audit** — Phases 61–66 (shipped 2026-04-17)
- 🚧 **v0.29.0 Harness Quality** — Phases 67–72 (in progress)

## Phases

<details>
<summary>✅ v0.2.0–v0.11.0 (Phases 1–39) — SHIPPED</summary>

Covered: core platform stability, providers, channels, memory, tools, orchestrator, architecture cleanup, Chat UI Polish, Engine Dispatcher + Security Hardening. See git history and previous milestone artifacts.

</details>

<details>
<summary>✅ v0.12.0 Chat Redesign (Phases 40–45) — SHIPPED 2026-04-09</summary>

See `.planning/milestones/v0.12.0-ROADMAP.md` for phase details.

</details>

<details>
<summary>✅ v0.13.0–v0.18.0 (Phases 54–60) — SHIPPED 2026-04-10..16</summary>

See `.planning/milestones/v0.13.0-ROADMAP.md` and `.planning/milestones/v0.14.0-ROADMAP.md` for phase details.

</details>

<details>
<summary>✅ v0.19.0 Stability & Performance Audit (Phases 61–66) — SHIPPED 2026-04-17</summary>

- [x] **Phase 61: Integration Test Foundation** — Testcontainers-PG harness, mock LLM provider, characterization tests for approval/SSE/shutdown, ARM64 CI matrix
- [x] **Phase 62: Resilience** — SSE coalescing + drop counter, cleanup schedulers, rate-limiter sweepers, graceful shutdown drain, Docker resource limits, toolgate asyncio tuning
- [x] **Phase 63: Data Layer** — Composite + partial indexes (migration 022), window-function rewrite, batch insert, SELECT FOR UPDATE for approval resolve
- [x] **Phase 64: Security** — Unified DNS-pinned SSRF guard, is_read_only canonicalization, HMAC-signed /uploads, backup size cap, nginx CSP report-only
- [x] **Phase 65: Observability** — OpenTelemetry 0.27→0.31 + metrics, W3C trace-context middleware, /api/health/dashboard, cardinality guard, rustls-only CI invariant
- [x] **Phase 66: Refactoring** — engine.rs Parnas extraction (6 submodules), DashMap in approval_manager, LISTEN/NOTIFY in memory worker, React.memo + Zustand selectors, Box::leak → OnceLock\<Arc\<T\>\>

See `.planning/milestones/v0.19.0-ROADMAP.md` for full phase details and `.planning/milestones/v0.19.0-MILESTONE-AUDIT.md` for the final audit.

</details>

### 🚧 v0.29.0 Harness Quality (Phases 67–72)

- [x] **Phase 67: Rate Limiter DashMap Swap** — REF-03 carry-over: swap `Arc<Mutex<HashMap>>` for DashMap 6 in hydeclaw-gateway-util rate limiter; validates guard scoping patterns for the rest of the milestone (completed 2026-05-08)
- [ ] **Phase 68: Prompt Caching** — CACHE-01..04: cache_control breakpoints on system message and stable tool tail in AnthropicProvider; cache metrics in usage_log and /api/health/dashboard; provider-type guard
- [ ] **Phase 69: Auto-Compaction** — COMP-01..04: threshold default raised to 0.85, token counting fixed for cache-aware sessions (input + cache_read + cache_creation), context window corrected to 1M for new Claude models, custom compaction prompt support
- [ ] **Phase 70: Model Routing** — ROUTE-01..02: min_input_tokens and min_tool_count complexity conditions in RoutingProvider, per-route-target cache context isolation, routing decisions logged to session_events
- [ ] **Phase 71: Tool defer_loading** — DEFER-01..02: defer_loading field in YamlToolDef, stub-only LLM registration, lazy schema load on first dispatch, per-pipeline-invocation loaded-tools state
- [ ] **Phase 72: Hook API** — HOOK-01..04: PreToolUse / PostToolUse / SessionStart hooks with allow/deny/modify actions, TOML `[[hooks]]` config with serde(default) backward compat, hook fires before needs_approval() check

## Phase Details

### Phase 67: Rate Limiter DashMap Swap

**Goal**: Rate limiter uses lock-free DashMap 6 with correct guard scoping — no async Mutex in hot path

**Depends on**: Phase 66 (DashMap already used in approval_manager, patterns validated)

**Requirements**: REF-03

**Success Criteria** (what must be TRUE):

1. Rate-limiter middleware accepts requests with the same semantics as before the swap — existing integration tests pass unchanged
2. No DashMap guard is held across an `.await` point (compile-time enforced via `#![deny(clippy::await_holding_lock)]`)
3. Sweeper background task uses per-key remove rather than full-map retain, avoiding shard-lock amplification
4. `/api/health/dashboard` rate_limiter_sizes field reports correct entry counts with the new implementation

**Plans:** 1/1 plans complete

Plans:

- [x] 67-01-PLAN.md — Wave 0 lint+dep + DashMap swap of AuthRateLimiter/RequestRateLimiter + workspace verification

**Key constraint**: Fetch-clone-drop pattern required — never store a DashMap guard in a let binding that outlives the synchronous block. Mirror the approval_manager pattern from Phase 66.

### Phase 68: Prompt Caching

**Goal**: Anthropic sessions pay full input cost only on first turn — subsequent turns read from cache for system message and tool definitions

**Depends on**: Phase 67

**Requirements**: CACHE-01, CACHE-02, CACHE-03, CACHE-04

**Success Criteria** (what must be TRUE):

1. An agent with `prompt_cache = true` emits `cache_creation_input_tokens > 0` on the first turn and `cache_read_input_tokens > 0` on the second turn when using an Anthropic provider
2. An agent using a non-Anthropic provider (OpenAI, Google) processes requests identically to before — no error, no behavior change
3. `cache_read_input_tokens` and `cache_creation_input_tokens` appear in the `usage_log` table and are visible in `/api/health/dashboard`
4. The CLAUDE.md of the system agent is registered as a third cache breakpoint (after system prompt, after stable tool definitions)

**Plans:** 2/3 plans executed

Plans:

- [x] 68-01-PLAN.md — Wave 1: AgentSettings.prompt_cache field + factory wiring + tool breakpoint fix (last system tool, not YAML) + non-Anthropic ignore test (CACHE-01, CACHE-04)
- [ ] 68-02-PLAN.md — Wave 2: CLAUDE.md as third cache breakpoint via CallOptions.claude_md_content + workspace loader split + context_builder integration (CACHE-02)
- [x] 68-03-PLAN.md — Wave 1: hydeclaw-db cache_metrics() query + DashboardSnapshot extension + /api/health/dashboard 4-field emission (CACHE-03)

**Key constraint**: Cache breakpoint must be placed on the last stable (system) tool, not the last tool overall — YAML tools vary between turns and would cause every request to be a cache write with zero reads (Pitfall 1.2).

### Phase 69: Auto-Compaction

**Goal**: Long sessions compact at the right threshold with correct token accounting — context-exceeded errors eliminated

**Depends on**: Phase 68 (token counting must be cache-aware before compaction threshold check is reliable)

**Requirements**: COMP-01, COMP-02, COMP-03, COMP-04

**Success Criteria** (what must be TRUE):

1. A session with caching active that reaches 85% of the context window triggers compaction — the effective token count used for the check is `input_tokens + cache_read_input_tokens + cache_creation_input_tokens`
2. `default_context_for_model()` returns 1,000,000 for claude-opus-4-7, claude-opus-4-6, and claude-sonnet-4-6 instead of 200,000
3. An operator can set `compaction_threshold = 0.9` in agent TOML and the compaction fires at 90% rather than 85%
4. An operator can set `compaction_prompt = "..."` in agent TOML and the custom instruction replaces the default compaction system message

**Plans**: TBD

**Key constraint**: The token-sum fix (COMP-02) and the model context limit fix (COMP-03) must both land before COMP-01 threshold logic is testable end-to-end — ship them as a single atomic commit pair with a TDD reproducer test that uses MockProvider with non-zero cache fields.

### Phase 70: Model Routing

**Goal**: Requests are routed to the appropriate model tier based on task complexity and accumulated context size

**Depends on**: Phase 69 (corrected last_prompt_tokens from compaction phase required for context_heavy condition)

**Requirements**: ROUTE-01, ROUTE-02

**Success Criteria** (what must be TRUE):

1. A request with `input_tokens` exceeding `min_input_tokens` is routed to the configured heavier model without operator intervention
2. A request citing more tools than `min_tool_count` is routed to the configured heavier model
3. Each route target maintains its own cache context — switching from Haiku to Sonnet does not invalidate the Sonnet cache or vice versa
4. Routing decisions appear in `session_events` WAL entries for diagnostics

**Plans**: TBD

**Key constraint**: `context_heavy` condition reads `last_prompt_tokens` which is only correct after the COMP-02 token-sum fix lands in Phase 69. Do not implement ROUTE-01 before Phase 69 is complete. Propagate `prompt_cache` flag from ProviderRow.options into routing overrides (Pitfall 5.3).

### Phase 71: Tool defer_loading

**Goal**: YAML tools with `defer_loading: true` shrink the stable tool prefix — improving cache hit rate without losing tool availability

**Depends on**: Phase 68 (cache stability requires understanding which tools are in the stable prefix before deferring any)

**Requirements**: DEFER-01, DEFER-02

**Success Criteria** (what must be TRUE):

1. A YAML tool marked `defer_loading: true` appears in the LLM tool list with only its name and description — the full JSON schema is absent until the tool is called
2. When the LLM calls a deferred tool, the full schema is loaded and the call dispatches correctly, with no error returned to the LLM
3. Two concurrent sessions with different deferred-tool call histories do not share loaded-tool state — each pipeline invocation tracks its own loaded set

**Plans**: TBD

**Key constraint**: Verify with a live Anthropic API test that a tool call response is accepted when the original request contained only a stub schema (empty properties). If rejected, the two-pass approach must be replaced with an alternative before committing. Per-pipeline `HashSet<String>` scoped to execute(), mirroring LoopDetector ownership (Pitfall 3.3).

### Phase 72: Hook API

**Goal**: Agents can extend tool dispatch behavior through declarative TOML hooks — without code changes or forks

**Depends on**: Phase 71 (pipeline must be stable with caching, compaction, routing, and defer_loading active before wrapping it with hooks)

**Requirements**: HOOK-01, HOOK-02, HOOK-03, HOOK-04

**Success Criteria** (what must be TRUE):

1. An agent with a `[[hooks]]` PreToolUse entry matching a tool name can inspect the tool arguments and return `allow`, `deny`, or `modify` before the tool executes
2. An agent with a PostToolUse hook receives the tool result and can log or transform it after execution
3. A SessionStart hook fires exactly once per new session (`reentry_mode == NewSession`) — it does not fire on resume, continuation, or crash-recovery re-entry
4. Existing agents without `[[hooks]]` in their TOML start and process requests identically to before — no startup error, no behavior change (serde(default) backward compat)

**Plans**: TBD

**Key constraint**: PreToolUse hook check must be the first step in tool dispatch — before `needs_approval()` and before any DB write. A `deny` result after an approval row has been created leaves a ghost pending approval forever (Pitfall 4.3). SessionStart fires after the DB write boundary in bootstrap.rs, not before (Pitfall 4.2).

## Progress

| Phase | Milestone | Status | Completed |
|-------|-----------|--------|-----------|
| 61. Integration Test Foundation | v0.19.0 | Complete | 2026-04-17 |
| 62. Resilience | v0.19.0 | Complete | 2026-04-17 |
| 63. Data Layer | v0.19.0 | Complete | 2026-04-17 |
| 64. Security | v0.19.0 | Complete | 2026-04-17 |
| 65. Observability | v0.19.0 | Complete | 2026-04-17 |
| 66. Refactoring | v0.19.0 | Complete | 2026-04-17 |
| 67. Rate Limiter DashMap Swap | 1/1 | Complete    | 2026-05-08 |
| 68. Prompt Caching | 2/3 | In Progress|  |
| 69. Auto-Compaction | v0.29.0 | Not started | - |
| 70. Model Routing | v0.29.0 | Not started | - |
| 71. Tool defer_loading | v0.29.0 | Not started | - |
| 72. Hook API | v0.29.0 | Not started | - |
