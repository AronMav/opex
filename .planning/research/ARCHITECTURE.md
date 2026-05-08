# Architecture: v0.20.0 Harness Quality — Integration Map

**Project:** HydeClaw v0.20.0
**Researched:** 2026-05-08
**Confidence:** HIGH (all findings derived from reading actual source files)

---

## Executive Summary

Six features need to integrate into an existing pipeline that is already
well-decomposed. The pipeline is split across `pipeline/bootstrap.rs`,
`pipeline/execute.rs`, `pipeline/finalize.rs`, and `pipeline/behaviour.rs`;
provider code lives under `agent/providers/`; gateway middleware is in
`gateway/mod.rs` + `hydeclaw-gateway-util/src/rate_limiter.rs`.

Good news: four of the six features (cache_control, auto-compaction,
hook API, model routing) slot into existing extension points without
touching the critical hot path. Two (tool defer_loading, REF-03) require
new fields or a module swap but have zero pipeline entanglement.

---

## Feature 1: Prompt Caching (cache_control markers)

### Integration Point

`AnthropicProvider::build_request_body()` in
`crates/hydeclaw-core/src/agent/providers/anthropic.rs` lines 288-322.

The Anthropic provider already has partial prompt caching behind the
`self.prompt_cache` flag (controlled by `ProviderOptions.prompt_cache`). When
`prompt_cache = true` it:
- Wraps the system message as `[{"type":"text","text":...,"cache_control":{"type":"ephemeral"}}]`
- Appends `"cache_control":{"type":"ephemeral"}` to the last tool in the tools array

What is missing: cache_control is not applied to the last human turn in
long conversations. Anthropic's recommended pattern allows up to 4 cache
breakpoints; the current implementation uses 2 (system + last tool).

`CLAUDE.md` and the workspace prompt end up inside the system message string,
so they are automatically cached when `prompt_cache = true`. No separate
marker is needed for workspace content.

### Files Modified

- `crates/hydeclaw-core/src/agent/providers/anthropic.rs` — extend
  `build_request_body()` to optionally add `cache_control` to the last
  human-role message. Gate behind a new `prompt_cache_turns: bool` field
  in `ProviderOptions` (default false for backward compat).
- `crates/hydeclaw-core/src/agent/providers/timeouts.rs` — add
  `prompt_cache_turns: bool` to `ProviderOptions` struct.

### New Files

None. The plumbing already exists; the change is additive inside
`build_request_body()`.

### Critical Constraint

`cache_control` is an Anthropic-only wire-format field. The `Message` internal
type and `messages_to_openai_format()` must NOT receive cache_control markers.
They are injected at serialization time inside `build_request_body()` only.
OpenAI, Google, and ClaudeCLI providers are unaffected — no changes needed
there.

### Verification

The `Usage` SSE event emitted by `pipeline/execute.rs` lines 438-445 already
carries `cache_read_tokens` and `cache_creation_tokens` fields. Use those to
confirm cache hits after the feature ships.

### Build Order Note

Build first. No dependencies on other v0.20.0 features. Immediately reduces
token costs on every turn.

---

## Feature 2: Auto-Compaction at 85%

### Integration Point

`pipeline/execute.rs` already has proactive compression built in (lines
248-279). The existing `Compressor::should_compress()` method in
`crates/hydeclaw-core/src/agent/compressor.rs` checks
`last_prompt_tokens >= threshold` where threshold = `context_limit * cfg.threshold`.

What exists:
- `Compressor` struct (`crates/hydeclaw-core/src/agent/compressor.rs`)
- `CompactionConfig.threshold: f64` in agent config
- `compressor.update_token_count(usage.input_tokens)` called after each LLM
  response (`execute.rs` line 448)
- `crate::agent::history::compress_messages()` called at the top of the loop
  (`execute.rs` lines 256-279) when `compressor.should_compress()` is true

What "85% auto-compaction" means here: the mechanism already exists. Setting
`CompactionConfig.threshold` to `0.85` achieves the target behaviour.

### Files to Verify / Modify

- `crates/hydeclaw-core/src/config/mod.rs` — locate `CompactionConfig` struct
  and its `threshold` field default. If it is not 0.85 already, change the
  `fn default_threshold()` function (or the serde default attribute) to return
  `0.85`.
- No changes to `execute.rs` or `compressor.rs` are required unless the
  threshold was previously set to a different value.

### New Files

None.

### Build Order Note

Verify before building. Read `config/mod.rs` to find the current
`default_threshold()`. If already 0.85, this is a zero-code task. If not,
it is a one-line change. Either way, do this early so QA runs with the
correct threshold.

---

## Feature 3: Tool defer_loading

### Current State

`DefaultContextBuilder::build()` in `context_builder.rs` lines 526-578 calls
`deps.load_yaml_tools_cached()` and includes all YAML tools in the initial
tool list on every turn. There is no per-tool lazy loading.

The Dispatcher already implements the closest analog: when
`dispatcher_enabled = true`, the context only passes a small "static core"
tool list to the LLM and exposes an extension catalogue via a
`tool_use(action="search")` → `tool_use(action="describe")` → call flow
(`context_builder.rs` lines 247-383). Tools not in the static core are
reachable through this dispatcher — they are already "deferred" in practice.

### Integration Points

1. `crates/hydeclaw-core/src/tools/yaml_tools.rs` — `YamlToolDef` struct
   (around line 413): add `#[serde(default)] pub defer_loading: bool` field.
   `serde` default handles YAML files that omit the field (backward compat).

2. `crates/hydeclaw-core/src/agent/context_builder.rs` — `build()` method,
   tool-list assembly section (lines 527-578): after `filter_tools_by_policy`,
   filter out `defer_loading = true` tools from the initial tool list.
   Register them in the extension catalogue instead (the same catalogue used
   by the dispatcher path, lines 248-263).

3. Guard: when `dispatcher_enabled = false`, deferred tools should be included
   normally (the dispatcher is not present to serve them). Log a warning if
   `defer_loading = true` tools are found and the dispatcher is off.

### Files Modified

- `crates/hydeclaw-core/src/tools/yaml_tools.rs` — add `defer_loading: bool`
  field to `YamlToolDef`
- `crates/hydeclaw-core/src/agent/context_builder.rs` — filter deferred tools
  from initial list; ensure they appear in the extension catalogue

### New Files

None. Uses the existing dispatcher machinery.

### Build Order Note

Build after Feature 1 (prompt caching) and Feature 2 verification. The
dispatcher infrastructure it leans on must be stable before adding deferred
tool routing on top of it.

---

## Feature 4: Hook API (PreToolUse / PostToolUse / SessionStart)

### Pipeline Hook Sites

The pipeline has three natural hook sites:

**SessionStart hook:**
`crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` — end of the bootstrap
function, after `ProcessingGuard` is acquired and the user message is
persisted, before returning `BootstrapOutcome`. This is after WAL `running`
is written and before `execute()` is called.

**PreToolUse hook:**
`crates/hydeclaw-core/src/agent/pipeline/execute.rs` around line 762 —
immediately before `tool_executor.execute_batch(...)`. A cancel check already
exists at line 762; insert the hook call after the cancel check and before
the `execute_batch` call.

**PostToolUse hook:**
`crates/hydeclaw-core/src/agent/pipeline/execute.rs` around lines 815-826 —
inside the loop that processes `outcome.results` after `execute_batch` returns,
after each tool result is pushed to `messages`. Fire the PostToolUse hook with
the tool name and result string.

### How Hooks Are Stored

Recommended approach: TOML config per agent, consistent with how channel
configs and delegation configs are stored in this codebase.

```toml
# config/agents/MyAgent.toml
[[hooks]]
event = "pre_tool_use"
tool_filter = ["workspace_write", "code_exec"]  # optional; empty = all tools
action = "http"
url = "http://localhost:9099/hook"
timeout_ms = 500
on_error = "ignore"  # or "abort"
```

`AgentConfig` gets a `hooks: Vec<HookConfig>` field. Hook configs are loaded
at agent startup alongside the rest of the TOML and passed through the engine
config so the pipeline can access them.

### New Components Needed

- `crates/hydeclaw-core/src/agent/hooks.rs` — new file. Contains:
  - `HookConfig` struct (mirrors the TOML schema above)
  - `HookEvent` enum (`SessionStart`, `PreToolUse { tool_name }`,
    `PostToolUse { tool_name, result }`)
  - `fire_hook(configs, event, cancel)` async function that filters by
    `event` and `tool_filter`, makes the HTTP POST, and handles timeout +
    ignore/abort logic. Must respect the `CancellationToken`.

### Files Modified

- `crates/hydeclaw-core/src/config/mod.rs` — add `hooks: Vec<HookConfig>` to
  `AgentConfig` struct (serde default empty vec for backward compat)
- `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` — fire SessionStart
  hook at end of bootstrap, before returning `BootstrapOutcome`
- `crates/hydeclaw-core/src/agent/pipeline/execute.rs` — fire PreToolUse
  around line 762 and PostToolUse around line 826
- `crates/hydeclaw-core/src/agent/engine/mod.rs` — expose `hooks` from
  `AgentEngine::cfg()` so the pipeline can call `fire_hook`

### Build Order Note

Build after Features 1 and 2. Hooks sit on top of the pipeline without
modifying its control flow except when `on_error = "abort"` is implemented.
Implement the `ignore` error mode first. The abort mode (which requires a new
`ExecuteStatus::HookAborted` variant) can be added in a later task.

---

## Feature 5: Model Routing

### Current State

`RoutingProvider` in `crates/hydeclaw-core/src/agent/providers/routing.rs`
already implements condition-based dispatch. The `select_route()` method
at line 171 evaluates conditions in order: `short`, `long`, `with_tools`,
`financial`, `analytical`, `code`, `default`/`always`, `fallback`.

Condition matching inspects: message length (for `short`/`long`), keyword
sets (domain conditions), and `!tools.is_empty()` (for `with_tools`). Each
route maps to a different DB provider entry, which can have a different model,
temperature, and max_tokens.

### Integration Point

`crates/hydeclaw-core/src/agent/providers/routing.rs` — `select_route()`
method. Add a `complexity` condition arm that scores the user message by
estimated complexity:

- Input under a character threshold AND no tools present AND no complexity
  keywords → route to lightweight model (Haiku)
- Otherwise → route to primary model (Sonnet)
- Complexity keywords present (`analyze`, `compare`, `summarize deeply`,
  `write a detailed`, etc.) → route to heavy model (Opus)

The new `complexity` condition string is used in the agent TOML routing config
alongside existing conditions:

```toml
# config/agents/MyAgent.toml
[[provider.routes]]
condition = "short"
connection = "haiku-provider"

[[provider.routes]]
condition = "complexity"
connection = "opus-provider"

[[provider.routes]]
condition = "default"
connection = "sonnet-provider"
```

### Files Modified

- `crates/hydeclaw-core/src/agent/providers/routing.rs` — add `"complexity"`
  condition arm to `select_route()` match block. Add a
  `COMPLEXITY_KEYWORDS: &[&str]` constant (parallel to existing
  `ANALYTICAL_KEYWORDS`, `CODE_KEYWORDS`).
- Optionally: `crates/hydeclaw-core/src/config/mod.rs` — add
  `complexity_threshold_chars: Option<usize>` to `ProviderRouteConfig` if
  per-route tuning is desired. Otherwise hard-code a sensible default (e.g.
  500 chars).

### New Files

None.

### Cross-Provider Constraint

`select_route()` receives `messages` and `tools`. It has no access to actual
token counts from the provider — those come back in `LlmResponse.usage` after
the call. The complexity condition must be estimated from message text length
and structure, not exact token counts.

### Build Order Note

Build independently. `RoutingProvider` is self-contained. No dependency on
other v0.20.0 features. Prompt caching (Feature 1) becomes more impactful
when routing is active (Haiku requests hit cache more often), so validate
Feature 1 first before measuring routing's cost impact.

---

## Feature 6: REF-03 — Rate-Limiter DashMap Swap

### Current State

`AuthRateLimiter` and `RequestRateLimiter` in
`crates/hydeclaw-gateway-util/src/rate_limiter.rs` use:

```rust
state: Mutex<HashMap<String, (u32, Instant, Option<Instant>)>>,
```

The background sweeper (spawned in `gateway/mod.rs` lines 187-198) calls
`sweep().await` every 60s. The hot path (`record_failure`, `check`) takes
the `Mutex` lock synchronously on every request, blocking the tokio thread
for the duration of the HashMap access.

REF-03 goal: Replace `Mutex<HashMap>` with `DashMap` for sharded concurrent
access, eliminating lock contention under load.

### Integration Point

`crates/hydeclaw-gateway-util/src/rate_limiter.rs` — the only file to change.
`DashMap` is already a workspace dependency: it was added in v0.19.0 for
`approval_manager` (noted in PROJECT.md: "approval_manager Arc<DashMap>").
Check the version in use there and use the same major version.

The public API of `AuthRateLimiter` and `RequestRateLimiter`
(`is_locked`, `record_failure`, `record_success`, `check`, `sweep`,
`snapshot_size`, `__test_insert`, `__test_len`) must be preserved exactly.
The consuming code in `gateway/middleware.rs` and the integration tests
treat this as a stable API.

The `async fn` signatures on the public methods can be retained (they become
no-ops over DashMap, which is sync) — retaining them avoids call-site changes
in `gateway/mod.rs`.

### Files Modified

- `crates/hydeclaw-gateway-util/src/rate_limiter.rs` — swap
  `Mutex<HashMap<...>>` for `DashMap<String, (u32, Instant, Option<Instant>)>`
  in both `AuthRateLimiter` and `RequestRateLimiter`. Update all method bodies.
- `crates/hydeclaw-gateway-util/Cargo.toml` — add `dashmap = "{same major as
  workspace}"` dependency.

### New Files

None.

### Sweeper Interaction

`DashMap::retain()` is sync and sharded. The background sweeper in
`gateway/mod.rs` calls `rate_limiter.sweep().await`. With DashMap, `sweep()`
no longer needs to be async — but keeping the `async fn` signature avoids
changes to the sweeper call sites.

### Build Order Note

Build last. It is a pure performance refactor with no functional impact.
All existing integration tests for the rate limiter must pass unchanged.
Run `make test-db` to verify the sweeper tests (which involve the gateway)
before merging.

---

## Build Order (Recommended)

| Step | Feature | Rationale |
|------|---------|-----------|
| 1 | Prompt caching (Feature 1) | Immediate cost savings; purely additive; isolated to one provider file; no cross-feature deps |
| 2 | Auto-compaction verification (Feature 2) | Confirm threshold default; may be a one-liner or docs-only change |
| 3 | Model routing (Feature 5) | Self-contained; no deps; pairs with caching for cost measurement |
| 4 | Tool defer_loading (Feature 3) | Leans on dispatcher (already tested); adds one struct field + filter |
| 5 | Hook API (Feature 4) | New module + three pipeline hook calls; should be added after the loop is stable |
| 6 | REF-03 DashMap (Feature 6) | Refactor only; build last; integration-test heavily |

---

## New vs Modified Files Summary

| Feature | New Files | Modified Files |
|---------|-----------|----------------|
| 1 Prompt caching | None | `providers/anthropic.rs`, `providers/timeouts.rs` |
| 2 Auto-compaction | None | `config/mod.rs` (threshold default only) |
| 3 Tool defer_loading | None | `tools/yaml_tools.rs`, `agent/context_builder.rs` |
| 4 Hook API | `agent/hooks.rs` | `config/mod.rs`, `pipeline/bootstrap.rs`, `pipeline/execute.rs`, `agent/engine/mod.rs` |
| 5 Model routing | None | `providers/routing.rs` |
| 6 REF-03 DashMap | None | `hydeclaw-gateway-util/src/rate_limiter.rs`, `hydeclaw-gateway-util/Cargo.toml` |

---

## Architecture Conflict Checklist

- **cache_control + non-Anthropic providers:** SAFE. `cache_control` is
  injected only inside `AnthropicProvider::build_request_body()`. The `Message`
  struct and the in-memory `messages` vec never carry cache_control markers.
  OpenAI, Google, ClaudeCLI are unaffected.

- **auto-compaction + token counting:** SAFE. `compressor.update_token_count()`
  is called from the actual `LlmResponse.usage` returned by the provider.
  The char-estimate fallback (execute.rs lines 451-456) only fires when usage
  is None (some Ollama models). No change needed to the fallback path.

- **defer_loading + dispatcher disabled:** Must guard the filter behind a
  `if dispatcher_enabled` check. When the dispatcher is off, deferred tools
  have no mechanism to be discovered, so they must either be included normally
  or logged as misconfigured.

- **hooks + cancellation:** `fire_hook()` must respect the
  `CancellationToken`. If cancelled before the HTTP call completes, abandon
  the hook and proceed to the Interrupted path. Never let a hook block the
  pipeline shutdown.

- **REF-03 + sweeper:** DashMap `retain()` is internally sharded and safe to
  call from a background tokio task. No `tokio::sync::Mutex` is needed. The
  sweep timing (every 60s) stays the same.

- **model routing + prompt caching:** Compatible. `RoutingProvider` selects a
  provider; that provider may or may not have `prompt_cache = true`. Cache
  behaviour is per-provider, not per-routing-condition. No interaction issues.

---

## Sources

All findings from direct source code inspection:
- `crates/hydeclaw-core/src/agent/pipeline/execute.rs`
- `crates/hydeclaw-core/src/agent/context_builder.rs`
- `crates/hydeclaw-core/src/agent/providers/mod.rs`
- `crates/hydeclaw-core/src/agent/providers/anthropic.rs`
- `crates/hydeclaw-core/src/agent/providers/routing.rs`
- `crates/hydeclaw-core/src/agent/compressor.rs`
- `crates/hydeclaw-core/src/tools/yaml_tools.rs`
- `crates/hydeclaw-core/src/gateway/mod.rs`
- `crates/hydeclaw-core/src/gateway/middleware.rs`
- `crates/hydeclaw-gateway-util/src/rate_limiter.rs`
- `.planning/PROJECT.md`
