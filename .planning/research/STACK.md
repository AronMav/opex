# Technology Stack — v0.20.0 Harness Quality

**Project:** HydeClaw  
**Researched:** 2026-05-08  
**Scope:** Stack additions/changes needed for 6 milestone features

---

## Feature 1: Prompt Caching

### Current State

Already partially implemented. `AnthropicProvider` has a `prompt_cache: bool` field (set via `ProviderOptions.prompt_cache` or `ProviderOverrides.prompt_cache`). When `true`, the `build_request_body` method:
- Wraps system prompt as an array of content blocks with `"cache_control": {"type": "ephemeral"}` on the last block
- Attaches `"cache_control": {"type": "ephemeral"}` to the last tool in the `tools` array

`AnthropicUsage` already deserializes `cache_creation_input_tokens` and `cache_read_input_tokens`. `TokenUsage` carries them through to `StreamingAnthropicUsage::into_token_usage()`.

### Anthropic API Fields (verified against current docs, 2026-05-08)

**Request — cache_control placement:**

```json
// System prompt (array form required):
"system": [{"type": "text", "text": "...", "cache_control": {"type": "ephemeral"}}]

// Tools array — attach to LAST tool only (creates one breakpoint):
"tools": [..., {"name": "...", "cache_control": {"type": "ephemeral"}}]

// Message content blocks (e.g. conversation prefix boundary):
"messages": [{"role": "user", "content": [
  {"type": "text", "text": "...", "cache_control": {"type": "ephemeral"}}
]}]
```

**Usage response fields:**
```json
{
  "usage": {
    "input_tokens": 50,
    "cache_creation_input_tokens": 5120,
    "cache_read_input_tokens": 1800,
    "output_tokens": 503
  }
}
```

Total input = `cache_read_input_tokens + cache_creation_input_tokens + input_tokens`.

**Model minimums** (tokens that must be present to cache):
- Opus 4.7, 4.6, 4.5: 4096 tokens minimum
- Sonnet 4.6, 4.5, 4: 2048 tokens minimum  
- Haiku 4.5: 4096 tokens minimum
- Haiku 3.5: 2048 tokens minimum

**TTL options:**
- `{"type": "ephemeral"}` — default 5-minute TTL, refreshed on cache hits at no extra cost
- `{"type": "ephemeral", "ttl": "1h"}` — 1-hour TTL, costs 2x base write price

**Maximum breakpoints:** 4 per request. Current impl uses 2 (system + tools). Leaves 2 available for conversation history prefix.

**Cache ordering:** Anthropic caches in this fixed order: `tools` → `system` → `messages`. Breakpoints must respect this order.

**Constraint:** Cache becomes available only after the first response starts streaming. Parallel requests race on the first write.

### What's Missing

1. **CLAUDE.md caching** — Agent's CLAUDE.md content is injected into the system prompt but NOT separately cached as a breakpoint. With long CLAUDE.md files (2k+ tokens), a dedicated breakpoint between the static CLAUDE.md block and the dynamic runtime context saves tokens on every turn. Requires splitting `system` into two blocks: one for the static base (with `cache_control`) and one for the dynamic per-request additions.

2. **Tool schema partial caching** — Currently all tools share one cache breakpoint (on the last tool). If tools change between turns (e.g. promoted tools in dispatcher mode), the entire tools block is re-written. Stable core tools should be in an earlier breakpoint; dynamic/promoted tools in a later one. Requires building `tools_json` in two segments.

3. **Cache effectiveness metrics** — `cache_read_input_tokens` is logged at `tracing::info!` but not surfaced to the UI or to the Phase 65 OtelMetrics histograms. Should be a counter (`cache_tokens_saved_total`).

4. **No `ttl: "1h"` option** — Config doesn't expose the 1-hour TTL variant. Useful for cron agents that run on a fixed schedule with a stable context between runs.

### No New Crates Required

The `cache_control` field is plain `serde_json::Value` already. No additional dependencies.

---

## Feature 2: Auto-Compaction at 85% Context Window

### Current State

The compaction infrastructure is **fully built**. `Compressor` struct in `src/agent/compressor.rs` tracks `last_prompt_tokens`, `context_limit`, and calls `should_compress()` against `CompactionConfig.threshold` (default `0.8` = 80%). `CompactionConfig` is in `config/mod.rs` with fields: `enabled`, `threshold`, `preserve_last_n`, `max_context_tokens`, `protect_first_n`, `summary_target_ratio`, `anti_thrash_min_savings`, `anti_thrash_max_skips`, `extract_to_memory`.

`resolve_context_limit()` in `pipeline/llm_call.rs` queries the provider API and falls back to `default_context_for_model()`. The cache is `OnceLock<Mutex<HashMap<String, u32>>>`.

**The 85% threshold is a config change**: set `fn default_threshold() -> f64 { 0.85 }` in `config/mod.rs`.

### Token Counting — How It Works

The system uses `input_tokens` from the LLM API response to track actual token consumption. No client-side tokenizer is needed — the provider counts tokens server-side.

Flow:
1. `AnthropicProvider.chat()` / `.chat_stream()` returns `LlmResponse { usage: Some(TokenUsage { input_tokens, ... }) }`
2. `pipeline/execute.rs` calls `compressor.update_token_count(usage.input_tokens)` after each LLM response
3. Before the next LLM call, `compressor.should_compress(&cfg)` checks `last_prompt_tokens >= context_limit * threshold`

### Context Limit Bug: Claude 4.x Models

**Current code in `default_context_for_model()`:**
```rust
if model.contains("claude") {
    200_000  // Incorrect for Opus 4.7, Opus 4.6, Sonnet 4.6 (1M context)
}
```

**Actual context windows (verified 2026-05-08):**
| Model | Context Window |
|-------|---------------|
| claude-opus-4-7 | 1,000,000 tokens |
| claude-opus-4-6 | 1,000,000 tokens |
| claude-sonnet-4-6 | 1,000,000 tokens |
| claude-sonnet-4-5 | 200,000 tokens |
| claude-haiku-4-5 | 200,000 tokens |
| claude-haiku-3-5 | 200,000 tokens |

The provider's `context_limit_hint()` call can override this via the Anthropic Models API (which returns `max_input_tokens`), but when that call fails or the provider doesn't implement it, the fallback 200k is wrong for 1M-context models — compaction fires 5x too early.

**Fix:** Update `default_context_for_model()` to pattern-match model names before the generic `claude` catch-all:
```rust
pub fn default_context_for_model(model: &str) -> usize {
    if model.contains("claude-opus-4-7")
        || model.contains("claude-opus-4-6")
        || model.contains("claude-sonnet-4-6") {
        1_000_000
    } else if model.contains("claude") {
        200_000
    } else if model.contains("gpt-4") {
        128_000
    } else if model.contains("MiniMax") || model.contains("M2.5") || model.contains("gemini") {
        1_000_000
    } else {
        128_000
    }
}
```

### No New Crates Required

No tokenizer crate needed — server-side `input_tokens` counting is already in place.

---

## Feature 3: Tool `defer_loading`

### Current State

Two existing mechanisms for limiting tools in context:

**Dispatcher mode** (`dispatcher_enabled`): when `[agent.tool_use] dispatcher = true`, `context_builder.rs` sends only a small set of "core" tools. Non-core tools are accessible via a `tool_use(action="search/describe/call")` meta-tool. This is the production-grade progressive loading mechanism for tool COUNT reduction.

**`max_tools_in_context`**: legacy semantic-similarity top-K selection (only active when dispatcher is off).

### What `defer_loading` Means

`defer_loading` is about tool schema SIZE, not count. The idea: emit only `name + description` for specified tools in the `tools` array sent to the LLM. The full `input_schema` is deferred until the LLM actually calls the tool, at which point the schema is loaded for argument validation.

### Practical Pattern

**YAML tool config — new field:**
```yaml
# workspace/tools/heavy_tool.yaml
name: heavy_tool
description: "Does heavy thing"
defer_loading: true   # schema not sent to LLM; loaded at call time
```

**`ToolDefinition` struct — new field:**
```rust
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// If true, input_schema is replaced with a minimal stub in the LLM tools array.
    /// Full schema is loaded from disk when the LLM invokes the tool.
    pub defer_schema: bool,
}
```

**In `build_request_body` (anthropic.rs):**
```rust
let tools_json: Vec<serde_json::Value> = tools.iter().map(|t| {
    if t.defer_schema {
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": {"type": "object", "properties": {}}
        })
    } else {
        serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        })
    }
}).collect();
```

**At call-dispatch time** (`pipeline/handlers.rs`): when the LLM invokes a deferred tool, `find_yaml_tool()` loads the full definition for argument validation before execution.

### Token Savings Estimate

A YAML tool with 10 parameters uses roughly 200–400 tokens. Deferring 15 of 20 tools saves ~4000 tokens per context call. This is modest (2% of 200k, 0.4% of 1M) but compounds across long sessions.

**Key constraint:** The Anthropic API will invoke a tool with whatever arguments it infers even with a minimal schema. Deferred loading should be limited to tools with simple or optional argument shapes, or tools already in the dispatcher's `describe` flow where the LLM knows to call `tool_use(action="describe")` first.

### No New Crates Required

YAML loading already uses `serde_yaml`. The `defer_loading` field is a `#[serde(default)] bool` in the YAML config struct.

---

## Feature 4: Hook API — PreToolUse/PostToolUse/SessionStart

### Current State

`src/agent/hooks.rs` has a working hook system:
- `HookEvent`: `BeforeMessage`, `AfterResponse`, `BeforeToolCall { agent, tool_name }`, `AfterToolResult { agent, tool_name, duration_ms }`, `OnError`
- `HookAction`: `Continue`, `Block(String)`
- `HookRegistry`: vector of `(name, Box<dyn Fn(&HookEvent) -> HookAction + Send + Sync>)` plus fire-and-forget webhook delivery
- `HooksConfig` in `config/mod.rs`: `log_all_tool_calls`, `block_tools`, `[[webhooks]]`

**What fires today** (from `engine/run.rs` and `tool_executor.rs`):
- `BeforeMessage` — fired before each session entry (all 4 transport paths, including resumes)
- `BeforeToolCall` — fired before each tool dispatch
- `AfterToolResult` — fired after each tool dispatch

### Gap Analysis

The milestone names `PreToolUse`, `PostToolUse`, `SessionStart`. These map to existing events with the following gaps:

| Milestone name | Existing equivalent | Gap |
|---------------|---------------------|-----|
| `PreToolUse` | `BeforeToolCall` | Carries only `agent + tool_name`. Missing: `arguments` (needed for input inspection/mutation hooks). |
| `PostToolUse` | `AfterToolResult` | Carries only `agent + tool_name + duration_ms`. Missing: tool output content. |
| `SessionStart` | `BeforeMessage` | `BeforeMessage` fires on every turn (including resumes). `SessionStart` must fire only when `reentry_mode == NewSession`. |

**Also missing:**
- `HookAction::Modify(serde_json::Value)` — to allow hooks to transform tool arguments before dispatch (PreToolUse intercept pattern)
- `OnError` carries no error details today

### Recommended Design

**Extend `HookEvent` and `HookAction` (no new crates):**

```rust
pub enum HookEvent {
    /// Fires once per NEW session (not on resume). Use for initialization.
    SessionStart { agent: String, session_id: uuid::Uuid },
    BeforeMessage,
    AfterResponse,
    /// Fires before each tool dispatch. Replaces BeforeToolCall.
    PreToolUse { agent: String, tool_name: String, arguments: serde_json::Value },
    /// Fires after each tool dispatch. Replaces AfterToolResult.
    PostToolUse { agent: String, tool_name: String, duration_ms: u64, output: String },
    OnError { agent: String, error: String },
}

pub enum HookAction {
    Continue,
    Block(String),
    /// Modify tool arguments before dispatch (PreToolUse only; ignored for other events).
    Modify(serde_json::Value),
}
```

**`SessionStart` firing point** in `engine/run.rs`: After `BootstrapOutcome` is built, check `reentry_mode` and fire `SessionStart` only when `reentry_mode == ReentryMode::NewSession`. All four transport paths (`handle_sse`, `handle_with_status`, `handle_streaming`, `handle_isolated_via_pipeline`) call `bootstrap()` — the firing goes into bootstrap's post-claim path.

**TOML config webhook event names** (for external HTTP delivery):
```toml
[[agent.hooks.webhooks]]
url = "https://myserver.com/hook"
events = ["PreToolUse", "PostToolUse", "SessionStart"]
```

**Backward compatibility:** The existing event names `BeforeToolCall` and `AfterToolResult` should remain valid in `events` arrays (map to `PreToolUse`/`PostToolUse` in `event_name()`).

### No New Crates Required

All extension is internal to `hooks.rs` and `config/mod.rs`.

---

## Feature 5: Model Routing by Difficulty

### Current State

`RoutingProvider` in `src/agent/providers/routing.rs` already implements condition-based dispatch. Current conditions in `select_route()`:
- `"short"` — user message < 300 chars
- `"long"` — user message > 2000 chars
- `"with_tools"` — tools list non-empty
- `"financial"` / `"analytical"` / `"code"` — keyword match
- `"default"` / `"always"` — catch-all
- `"fallback"` — explicit failover only

`create_routing_provider()` in `routing.rs` builds the route entries from `Vec<ProviderRouteConfig>` referencing named DB providers.

### What's Needed

**New conditions** for difficulty-based routing:

```rust
// In select_route(), alongside existing conditions:
"heavy" => ctx.last_input_tokens > route.min_input_tokens.unwrap_or(u32::MAX),
"medium" => ctx.last_input_tokens > route.min_input_tokens.unwrap_or(u32::MAX),
"multi_tool" => ctx.tool_count > route.min_tool_count.unwrap_or(usize::MAX),
```

**Extend `ProviderRouteConfig`** with threshold fields:

```rust
pub struct ProviderRouteConfig {
    pub condition: String,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub cooldown_secs: u64,
    /// Route if last LLM call's input_tokens exceeded this threshold.
    #[serde(default)]
    pub min_input_tokens: Option<u32>,
    /// Route if the tool count passed to the LLM exceeds this threshold.
    #[serde(default)]
    pub min_tool_count: Option<usize>,
}
```

**TOML example:**
```toml
[[agent.routing]]
condition = "heavy"
min_input_tokens = 50000
connection = "anthropic-opus"
model = "claude-opus-4-7"

[[agent.routing]]
condition = "medium"
min_input_tokens = 5000
connection = "anthropic-sonnet"
model = "claude-sonnet-4-6"

[[agent.routing]]
condition = "default"
connection = "anthropic-haiku"
model = "claude-haiku-4-5"
```

**`select_route()` signature change**: receives a `RouteContext` struct instead of raw `messages + tools` to carry `last_input_tokens`:

```rust
struct RouteContext<'a> {
    last_user_msg: &'a str,
    tool_count: usize,
    last_input_tokens: u32,
}
```

The `RoutingProvider` must track `last_input_tokens` across calls. Options:
- Pass it as part of `CallOptions` (already has `thinking_level: u8`, so easy to add `last_input_tokens: u32`)
- Store it in an `Arc<AtomicU32>` inside `RoutingProvider` (simpler, avoids API change)

The `AtomicU32` approach avoids changing the `LlmProvider` trait signature:
```rust
pub struct RoutingProvider {
    routes: Vec<RouteEntry>,
    cooldowns: std::sync::Mutex<HashMap<String, Instant>>,
    max_failover_attempts: u32,
    last_input_tokens: std::sync::atomic::AtomicU32,  // new field
}
```

### Recommended Heuristic Thresholds

Starting values (operators tune via config):

| Tier | `min_input_tokens` | Rationale |
|------|-------------------|-----------|
| Haiku 4.5 | default catch-all | $1/$5/MTok, 200k context, fastest latency |
| Sonnet 4.6 | 5,000 | $3/$15/MTok, 1M context; most mid-complexity tasks |
| Opus 4.7 | 50,000 | $5/$25/MTok, 1M context; reserved for long sessions |

### No New Crates Required

All logic is internal to `routing.rs` and `config/mod.rs`.

---

## Feature 6: REF-03 — DashMap Swap for Rate Limiter

### Current State

`crates/hydeclaw-gateway-util/src/rate_limiter.rs` has:
- `AuthRateLimiter`: `state: tokio::sync::Mutex<HashMap<String, (u32, Instant, Option<Instant>)>>`
- `RequestRateLimiter`: `state: tokio::sync::Mutex<HashMap<String, (u32, Instant)>>`

Both hold an async Mutex guarding the full HashMap. Every request hits `RequestRateLimiter::check()`, taking the async lock. The background sweeper calls `sweep()` every 60s.

`dashmap = "6.1.0"` is already in `Cargo.lock` (pulled transitively by `crates/hydeclaw-core`).

### What the DashMap Swap Achieves

`DashMap` shards the map into 64 buckets (default), each with its own `parking_lot::RwLock`. The hot path takes a shard lock for ~microseconds without suspending the async task. On the Pi (4 cores, bursty traffic), this eliminates async yield points in the most-called middleware.

| Aspect | Before (Mutex<HashMap>) | After (DashMap) |
|--------|------------------------|-----------------|
| Lock type | `tokio::sync::Mutex` (async, suspends task) | `parking_lot::RwLock` per shard (sync, never yields) |
| Contention | All IPs serialize on one lock | 64 shards, IPs rarely collide |
| `check()` | `async fn`, always `await`s | `fn`, sync |
| `sweep()` | `async fn` | `fn` |

### Migration Pattern

```rust
use dashmap::DashMap;

pub struct RequestRateLimiter {
    pub max_per_minute: u32,
    state: DashMap<String, (u32, Instant)>,
}

impl RequestRateLimiter {
    pub fn new(max_per_minute: u32) -> Self {
        Self { max_per_minute, state: DashMap::new() }
    }

    // Was async, now sync:
    pub fn check(&self, ip: &str) -> std::result::Result<(), u64> {
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);
        let mut entry = self.state.entry(ip.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) >= window {
            *entry = (0, now);
        }
        entry.0 += 1;
        if entry.0 > self.max_per_minute {
            let elapsed = now.duration_since(entry.1).as_secs();
            Err(60u64.saturating_sub(elapsed))
        } else {
            Ok(())
        }
    }

    // Was async, now sync:
    pub fn sweep(&self) {
        let now = Instant::now();
        let window = std::time::Duration::from_secs(60);
        self.state.retain(|_, (_, start)| now.duration_since(*start) < window);
    }

    pub fn snapshot_size(&self) -> usize { self.state.len() }
}
```

Same pattern for `AuthRateLimiter`. Test helpers `__test_insert` and `__test_len` become sync.

### Downstream Callers to Update

1. `middleware.rs`: `rate_limiter_sizes()` is currently `async fn` calling `.await` on `snapshot_size()`. After swap, remove `.await` (can stay `async fn` for API stability, just without internal await).
2. `gateway/mod.rs`: background sweeper task currently calls `limiter.sweep().await`. Remove `.await`.
3. `middleware.rs` `check()` callers: remove `.await` on `limiter.check(ip)`.
4. Test fixtures: remove `await` from `__test_insert`, `__test_len` call sites.

### Crate Version

`dashmap 6.1.0` is in `Cargo.lock`. Add as a direct dep in `hydeclaw-gateway-util/Cargo.toml`:

```toml
[dependencies]
dashmap = "6"
```

DashMap 6 requires Rust 1.65+. The codebase uses Rust 2024 edition (≥ 1.79). No compatibility issues.

---

## Dependency Summary

| Feature | New Crates | Config Changes | Code Changes |
|---------|-----------|---------------|-------------|
| Prompt caching | None | `HooksConfig`: optional `ttl` field | `build_request_body`: split system block; 2nd tool breakpoint |
| Auto-compaction 85% | None | `default_threshold()`: 0.8 → 0.85 | `default_context_for_model()`: add 1M entries for Opus/Sonnet 4.6+ |
| Tool defer_loading | None | `ToolDefinition`: add `defer_schema: bool` | `build_request_body`: conditional schema emit; `handlers.rs`: load schema at call time |
| Hook API | None | `HooksConfig`: extended events list | `HookEvent`/`HookAction` variants; `bootstrap.rs`: `SessionStart` firing |
| Model routing | None | `ProviderRouteConfig`: `min_input_tokens`, `min_tool_count` | `routing.rs`: `RouteContext` struct; `AtomicU32` for last_input_tokens |
| REF-03 DashMap | `dashmap = "6"` (already in Cargo.lock) | None | `rate_limiter.rs`: HashMap → DashMap; all callers: remove `.await` |

**Zero new crates** required (DashMap is already transitively resolved). All changes are internal to existing modules.

---

## Sources

- Anthropic Prompt Caching docs verified 2026-05-08: https://platform.claude.com/docs/en/docs/build-with-claude/prompt-caching  
- Anthropic Models overview verified 2026-05-08: https://platform.claude.com/docs/en/docs/about-claude/models/overview  
- `crates/hydeclaw-core/src/agent/providers/anthropic.rs` (lines 88-325): `prompt_cache` field, `build_request_body`, `AnthropicUsage`, `StreamingAnthropicUsage`
- `crates/hydeclaw-core/src/agent/compressor.rs`: `Compressor`, `should_compress`, `update_token_count`
- `crates/hydeclaw-core/src/agent/pipeline/llm_call.rs` (lines 120-169): `resolve_context_limit`, `default_context_for_model`
- `crates/hydeclaw-core/src/agent/hooks.rs`: `HookRegistry`, `HookEvent`, `HookAction`, firing points
- `crates/hydeclaw-core/src/agent/providers/routing.rs` (lines 167-213): `RoutingProvider`, `select_route`, condition matching
- `crates/hydeclaw-gateway-util/src/rate_limiter.rs`: `AuthRateLimiter`, `RequestRateLimiter` with `Mutex<HashMap>`
- `crates/hydeclaw-core/src/config/mod.rs` (lines 596-764): `CompactionConfig`, `HooksConfig`, `ProviderRouteConfig`
- `crates/hydeclaw-core/src/agent/context_builder.rs` (lines 540-597): `max_tools_in_context`, dispatcher partition
- `Cargo.lock` line 504: `dashmap = "6.1.0"` (transitively resolved)
