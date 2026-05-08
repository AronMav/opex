# Feature Landscape — HydeClaw v0.20.0 Harness Quality

**Domain:** Production AI agent gateway (harness engineering)
**Researched:** 2026-05-08
**Milestone scope:** 6 specific features — prompt caching, auto-compaction, tool defer_loading, Hook API, model routing, REF-03 (internal refactor)

---

## Feature Breakdown

### Feature 1: Prompt Caching (cache_control)

**What it does:** Marks stable portions of the request (system prompt, tool definitions, skill files) with `cache_control: {type: "ephemeral"}` so Anthropic's KV cache serves subsequent turns at 10x lower cost. Cache reads cost 0.1x base input price; cache writes cost 1.25x (5-min TTL) or 2.0x (1-hour TTL). Min cacheable size: 2048 tokens for Sonnet 4.6; 4096 tokens for Opus models.

| Aspect | Detail |
|--------|--------|
| **Table Stakes** | - System prompt cached across turns in same session (otherwise every turn pays full input cost for a large system prompt)<br>- Tool definitions cached (for agents with 20+ tools this is thousands of tokens per turn)<br>- Cache-hit/miss metrics exposed in usage response (`cache_creation_input_tokens`, `cache_read_input_tokens`)<br>- Transparent to the user — they see faster responses and lower usage cost, not the mechanism |
| **Differentiators** | - Per-agent configurable cache TTL (5 min vs 1 hour based on session frequency)<br>- Cache hit-rate dashboard in operator UI (so operator can see ROI)<br>- Extended 1-hour TTL for agents with long idle gaps (e.g., cron agents that run hourly)<br>- Automatic breakpoint placement: stable content (system) cached first, then skills, then tool defs, dynamic message history not cached |
| **Anti-features** | - Caching timestamps or per-request metadata inside the cached prefix (invalidates cache every turn; metadata belongs outside or in non-cached suffix)<br>- Caching tool definitions that change frequently between turns (triggers cache miss + write penalty, net cost increase)<br>- Caching the full message history (message history grows every turn, invalidating cache; only the stable prefix should be cached)<br>- Treating 0 cache_read_input_tokens as an error (it just means prompt was below min threshold or first-ever turn) |
| **Complexity** | **Medium** — Anthropic API already supports it. HydeClaw's ContextBuilder must tag the right content blocks with `cache_control`. The ordering matters: tools, system, then messages. Up to 4 breakpoints per request. |
| **Dependencies** | - Requires knowing which content is stable vs. dynamic before each LLM call (ContextBuilder already does this)<br>- Tool defer_loading (Feature 3) amplifies savings: fewer tool schemas in context = smaller cached prefix = faster writes<br>- Auto-compaction (Feature 2) can *invalidate* prompt cache if it rewrites the history region that sits under a cache breakpoint — compaction and caching breakpoints must be placed to avoid overlap |

**User/operator observable behavior:**
- Operator sees response latency drop by 30-85% on cached turns (Anthropic reports 85ms per 100K cached tokens)
- Operator sees cost reduction in usage_log (cache_read tokens at 10% price vs full input)
- First turn of a session: slightly higher cost (cache write overhead). Subsequent turns: much lower cost.
- No behavior change in chat output — caching is transparent to the agent's responses

**Confidence:** HIGH — based on official Anthropic docs (verified 2026-05-08)

---

### Feature 2: Auto-Compaction

**What it does:** When context usage exceeds a configurable threshold (85-95% of model's context window), automatically summarize earlier conversation history, replacing raw turns with a `compaction` block containing a structured summary. Allows indefinitely long sessions without hitting context limits.

Anthropic's official API (beta `compact-2026-01-12`) provides this server-side. Trigger is configurable via `input_tokens` value (min 50K, default 150K). Custom summarization instructions are supported. `pause_after_compaction: true` lets the gateway inspect the summary before continuing.

| Aspect | Detail |
|--------|--------|
| **Table Stakes** | - Sessions don't crash or return 400 errors when context fills — graceful handling is mandatory<br>- Compaction is triggered automatically at a safe threshold (not at 100% which leaves no room for response)<br>- The agent continues conversation correctly after compaction (summary preserves task state, not just "we talked")<br>- Configurable threshold per agent (a cron agent doing 10-turn jobs needs different settings than a chat agent with 500-turn sessions)<br>- Operator is informed when compaction fired (observable in session events / UI) |
| **Differentiators** | - Domain-specific summarization instructions per agent (a code agent should preserve variable names and diffs; a support agent should preserve issue IDs and decisions)<br>- `pause_after_compaction` mode: gateway captures the summary before the agent continues, enabling logging/indexing of the compacted transcript<br>- Compaction fired at 85% not 95% for long-running sessions (earlier = more headroom for tool calls with large outputs)<br>- Operator-tunable threshold in agent TOML (`context_management.compaction_threshold_tokens`) |
| **Anti-features** | - Compacting too aggressively (low threshold triggers compaction every 5 turns, destroying conversational coherence; users notice "the agent forgot what we decided 10 minutes ago")<br>- Generic summarization that loses task-critical detail (e.g., code context, pending tool calls, decisions made)<br>- Compaction on every session regardless of length (adds unnecessary API overhead for short sessions that never approach the window)<br>- Silently swallowing compaction errors (if the summary LLM call fails, the session should fail gracefully, not continue with corrupted state)<br>- Using client-side compaction when Anthropic's server-side API is available (server-side is more reliable and interacts correctly with prompt caching) |
| **Complexity** | **Medium** — Anthropic beta API is the hard part handled by the provider. HydeClaw must: (1) detect when to enable `compact_20260112` per-request, (2) handle `compaction` blocks in response content when replaying sessions, (3) log compaction events to session_events WAL, (4) expose threshold config in agent TOML |
| **Dependencies** | - Prompt caching (Feature 1): compaction invalidates cached message prefixes. Breakpoints must be placed in the stable system region, below where compaction operates.<br>- Token counting: to know when 85% threshold is hit, gateway must track `input_tokens` from each turn's usage response (already in usage_log)<br>- Model routing (Feature 5): if routing to Haiku for simple turns, Haiku's context window may be smaller — per-model threshold needed |

**User/operator observable behavior:**
- User: conversation continues naturally beyond what would have been a context error. May notice agent occasionally "catching up" if pause_after_compaction is off (slightly longer first response after compaction).
- Operator: session_events log shows `compaction_fired` event with token count at trigger and summary length. Dashboard shows sessions-that-compacted count.
- Operator with pause_after_compaction: sees summary content in logs, can audit what was preserved.
- No compaction on sessions < 50K tokens (too short to trigger).

**Confidence:** HIGH — based on official Anthropic compaction beta docs (verified 2026-05-08)

---

### Feature 3: Tool defer_loading (Lazy Schema Loading)

**What it does:** Instead of injecting full JSON schemas for all tools into every LLM turn, inject only compact one-line summaries (name + description, ~60 tokens each). When the model selects a tool, fetch the full schema for that tool only and retry the call with it, or pass the schema inline. This eliminates the "tools tax" — in deployments with 20-120 YAML tools, this can be 10K-60K tokens saved per turn.

Research finding: arxiv paper from April 2026 shows 95% per-turn tool token reduction (47K to 2.4K tokens) and 84% prompt cache hit rate (vs 22% with eager injection).

| Aspect | Detail |
|--------|--------|
| **Table Stakes** | - Tool selection accuracy must not degrade (agent still finds the right tool; just sees summary names, not full schemas, during selection)<br>- Latency overhead of the second-pass schema fetch must be < 200ms (it's a local in-memory lookup, not a network call)<br>- No regression on tools the agent uses every turn (frequently-used tools should be pre-loaded, not deferred)<br>- The agent cannot call a tool it was never told exists (tool names/descriptions must always be visible) |
| **Differentiators** | - Per-agent configurable eager_tools list (tools the agent uses frequently are always pre-loaded with full schemas to skip the second pass)<br>- Cache synergy: deferred schemas produce a smaller, more stable context prefix, dramatically improving prompt cache hit rates<br>- Token usage reduction visible to operator in usage_log (input_tokens drop per turn)<br>- Progressive disclosure: agent gets rich schema only for the tool it's actually about to call |
| **Anti-features** | - Deferring schemas for tools that fire on almost every turn (write_workspace fires ~80% of turns for a coding agent — deferring it adds latency without savings)<br>- Using semantic retrieval/embedding-based gating without fallback (if the retrieval model hallucinates an out-of-scope tool, agent fails; simpler keyword/prefix matching is more reliable)<br>- Two-round-trip on every tool call (should only be two passes when schema is not pre-loaded; cache the schema in memory after first load)<br>- Exposing this complexity in agent TOML with too many knobs (operators don't care about thresholds; a smart default with an optional override list is sufficient) |
| **Complexity** | **Medium** — HydeClaw's ContextBuilder already knows which tools are registered. The change is: (1) build a compact summary list for all tools (name + one-line description), (2) when LLM returns a tool_call, check if the full schema was already in context — if not, re-send with full schema inserted. This is a pure ContextBuilder + pipeline/execute change. No new external APIs. |
| **Dependencies** | - Prompt caching (Feature 1): defer_loading's primary value is enabling prompt cache hits. Without caching, the savings are real but the compounding effect is lost.<br>- Tool registry must store description separately from full schema — `tool_registry.rs` already loads YAML tools with separate fields.<br>- Pipeline execute must handle a two-pass loop: first pass (compact context) to model picks tool to second pass (full schema, if needed) to execute |

**User/operator observable behavior:**
- Operator sees input token count per turn drop significantly (especially on agents with many YAML tools)
- Latency may decrease slightly per turn (fewer tokens to process) but first-tool-call of a session may have one extra round trip
- No behavior change for the user in chat — agent still executes the correct tool
- Operator can set `eager_tools = ["workspace_write", "memory_search"]` in agent TOML to pre-load specific tool schemas

**Confidence:** MEDIUM — research paper confirms the approach. Anthropic reportedly shipped "Tool Search" in late 2025 as a server-side variant. HydeClaw's client-side implementation is well-precedented but specific API details need validation against current Anthropic SDK.

---

### Feature 4: Hook API (PreToolUse / PostToolUse / SessionStart)

**What it does:** Allows operators to register callback functions that fire at specific lifecycle points in an agent's execution. Hooks can observe, modify, block, or enrich behavior without changing agent config files. This is the "Governor pattern" — a deterministic control plane outside the probabilistic agent.

Industry convergence: By 2026, every major agent runtime (Claude Code, OpenAI Agents SDK, LangChain, Google ADK, Strands, ragbits) has this pattern. It is table stakes for production agent deployments.

| Aspect | Detail |
|--------|--------|
| **Table Stakes** | - `PreToolUse`: fires before any tool executes; can allow, deny, or modify the tool input<br>- `PostToolUse`: fires after a tool completes; can observe the result or inject additional context<br>- `SessionStart`: fires when a new session begins; can initialize per-session state, inject context<br>- Hooks can be registered per-agent (not globally)<br>- Hook failures must not crash the agent — a hook that throws an exception should be caught and logged, then continue with default behavior<br>- Deny decisions are respected: if a PreToolUse hook returns deny, the tool does not execute; the agent receives the denial reason as tool result |
| **Differentiators** | - Async hooks with a `fire_and_forget` flag (logging hooks don't need to block tool execution; PostToolUse audit hooks should be async)<br>- Regex-pattern matching on tool name (e.g., `matcher: "workspace_.*"` fires for all workspace tools)<br>- `systemMessage` injection: hooks can inject text into the conversation visible to the model (to explain why a tool was blocked)<br>- `updatedInput`: PreToolUse hooks can rewrite tool arguments before execution (e.g., sandbox path redirection)<br>- Multiple hooks per event can be chained; deny from any one hook blocks the operation<br>- Hook configuration in agent TOML (not code), so operators can add hooks without redeployment |
| **Anti-features** | - Hooks that spawn their own LLM calls (creates recursive loops unless depth is tracked)<br>- Hooks as the primary security boundary (they're the *extensibility* layer; HydeClaw's existing workspace.is_read_only() and SSRF guard remain the hard security boundary)<br>- Synchronous hooks that do heavy I/O on the hot path (a hook that queries an external policy service on every tool call adds hundreds of ms per tool)<br>- Hooks that silently modify tool input without logging the modification (makes debugging impossible; all mutations should emit a log entry)<br>- Treating hooks as a plugin marketplace (out of scope per PROJECT.md constraints) |
| **Complexity** | **High** — Requires: (1) defining a Rust trait/enum for HookEvent and HookResult, (2) wiring hook dispatch into pipeline/execute before and after each tool call, (3) TOML configuration schema for per-agent hooks, (4) hook registry that loads from agent config, (5) error isolation so panicking hooks don't bring down the pipeline, (6) async hook execution with timeout, (7) test harness integration. High because the interface design must be right the first time — it becomes a public API. |
| **Dependencies** | - Pipeline execute (already refactored in v0.19.0) — hooks wire into the existing pre/post tool dispatch points<br>- Agent config TOML must support a `[[agent.hooks]]` section<br>- Existing tool approval system (needs_approval) is NOT replaced — hooks are additive extensibility<br>- Session events WAL should record hook decisions (deny/allow/modify) for auditability<br>- Auto-compaction's PreCompact hook (Feature 2) is one specific hook type |

**User/operator observable behavior:**
- Operator can declare in agent TOML: `[[agent.hooks]] event = "PreToolUse" matcher = "code_exec" action = "require_approval"` — no code change needed
- Agent UI shows "tool blocked by hook: [reason]" when a hook denies a tool call
- Session events log shows hook fire + decision for each tool execution
- Developer can write Rust hooks (compiled into HydeClaw) or HTTP hooks (POSTed to a configured endpoint) for external policy services

**Confidence:** HIGH — industry-standard pattern, well-documented in Claude Agent SDK and multiple other frameworks. Implementation scope is clear.

---

### Feature 5: Model Routing

**What it does:** Automatically routes each LLM turn to a different model based on detected task complexity. Simple turns (short prompt, lookup-style, no multi-step reasoning) go to a cheaper/faster model (Haiku). Complex turns (long context, analytical, multi-step planning) use the primary model (Sonnet/Opus). Can reduce LLM costs by 60-80% on agents that mix simple and complex work.

Industry evidence: RouteLLM achieves 85% cost reduction while maintaining 95% of GPT-4 performance. Production systems use simple heuristics (token count + keyword markers) not learned classifiers for real-time routing.

| Aspect | Detail |
|--------|--------|
| **Table Stakes** | - Routing must be transparent to the user — output quality should not detectably degrade<br>- Fall-through to primary model if routing model fails or returns low confidence<br>- Per-agent routing config (a support chat agent and a code execution agent have very different complexity distributions)<br>- Routing decision must add < 5ms latency (rule-based heuristics, not a separate LLM call for classification)<br>- Routing must respect the current provider's available models (if Anthropic, route within Anthropic tiers; no cross-provider implicit routing) |
| **Differentiators** | - Operator-visible routing decisions in session events ("turn 3: routed to haiku, reason: short_prompt_low_complexity")<br>- Configurable routing rules in agent TOML: complexity signals, model tiers, threshold values<br>- Confidence-based escalation: route to cheap model, if it returns a tool call that implies it's out of depth, re-route to expensive model for the next turn<br>- Per-turn model shown in UI (operator mode) so operators can audit routing decisions<br>- Cost savings estimate in usage_log (actual cost vs what it would have been at primary model) |
| **Anti-features** | - Using a separate LLM call to classify complexity (doubles the cost for the classification itself)<br>- Routing to a different provider without the operator's knowledge (e.g., silently going from Anthropic to OpenAI) — violates user trust and data agreements<br>- Over-routing to cheap models (false economies): if haiku hallucinates tool calls and the agent retries at Sonnet, total cost exceeds single-Sonnet cost<br>- Complex routing ML models that need retraining (not maintainable on a self-hosted Pi deployment)<br>- Routing that changes the active model mid-session in a way that breaks prompt cache (each routing target needs its own cache state) |
| **Complexity** | **Medium** — Routing logic itself (heuristics + config) is simple Rust. The complexity is: (1) plumbing the routing decision into the existing provider factory before each LLM call, (2) ensuring cache state is maintained per model tier (not shared between Haiku and Sonnet caches), (3) handling model capabilities (Haiku has smaller context window — routing must check token count first), (4) fallback chain if routed model is unavailable. |
| **Dependencies** | - Provider factory/routing already exists in `agent/providers/routing.rs` — this extends it with heuristic signals<br>- Token counting (from usage_log or from Anthropic's token count API) is needed for context-window-aware routing<br>- Auto-compaction (Feature 2): if routing to Haiku which has smaller context, compaction threshold must adjust proportionally<br>- Prompt caching (Feature 1): each model tier maintains a separate cache prefix; routing must not mix cache contexts between tiers |

**User/operator observable behavior:**
- End user: responses feel faster on simple turns (Haiku is much faster) with no quality regression on complex turns
- Operator: usage_log shows per-turn model used; cost dashboard shows routing savings
- Operator: configures `[agent.model_routing] primary = "claude-sonnet-4-6" secondary = "claude-haiku-4-5" complexity_threshold = "auto"` in agent TOML
- Routing bypass: operator can disable routing per-agent for deterministic behavior in production

**Confidence:** MEDIUM — routing patterns are well-established. Specific integration with HydeClaw's existing routing infrastructure needs implementation-time validation of provider.rs interfaces.

---

### Feature 6: REF-03 — Rate-Limiter DashMap Swap

**What it does:** Internal refactor only. Replaces the current `RwLock<HashMap<IpAddr, RateLimitState>>` in the rate limiter with `DashMap<IpAddr, RateLimitState>`. DashMap uses per-shard RwLocks (16 shards by default), reducing lock contention under concurrent request bursts from multiple IPs. No user-facing behavior change.

Context from PROJECT.md: REF-03 was deferred from v0.19.0 specifically to avoid introducing a new deadlock surface simultaneously with other DashMap additions (`approval_manager`). Now that approval_manager DashMap is stable, REF-03 can proceed.

| Aspect | Detail |
|--------|--------|
| **Table Stakes** | - Rate limit semantics unchanged: same per-IP limits, same reset windows, same exempt paths (loopback, authenticated tokens)<br>- No performance regression vs current implementation under low concurrency<br>- Thread-safety: DashMap is `Send + Sync`; no new unsafe blocks needed<br>- All existing rate limiter tests pass unchanged (correctness must be bit-identical) |
| **Differentiators** | - Better throughput under concurrent bursts from many IPs (sharded locking eliminates global write lock contention)<br>- Eliminates background sweeper's need to acquire a global write lock for cleanup (DashMap supports `retain()` without a full lock)<br>- Simpler code: removes `Arc<RwLock<...>>` wrapper boilerplate |
| **Anti-features** | - DashMap for single-IP sequential workloads (no benefit; DashMap has slightly higher memory overhead per entry due to shard metadata)<br>- Using DashMap::get_mut() which re-introduces contention by holding a shard lock during complex mutations (use entry() API with interior mutability instead)<br>- Over-engineering: adding DashMap everywhere without a measured contention problem to solve |
| **Complexity** | **Low** — This is a mechanical substitution. `Arc<RwLock<HashMap<K,V>>>` becomes `Arc<DashMap<K,V>>`. Read operations: `.get(&key)` returns a `Ref<K,V>` instead. Write operations: `.insert()` / `entry().or_insert()` API is similar. The background sweeper uses `retain()`. Type signatures change but logic does not. |
| **Dependencies** | - `dashmap` crate already in Cargo.toml (added for approval_manager in v0.19.0 — no new dependency needed)<br>- Rate limiter background sweeper task must use DashMap's `retain()` instead of `HashMap::retain()` via a write guard<br>- No interaction with other v0.20.0 features |

**User/operator observable behavior:**
- None — this is invisible to users and operators
- Performance improvement is only measurable under high-concurrency load (300+ rpm from many unique IPs simultaneously hitting the rate limiter)
- The change is a correctness/maintenance improvement: reduces lock contention risk, simplifies code

**Confidence:** HIGH — DashMap is well-understood, already in the dependency tree. The pattern is mechanical.

---

## Feature Dependency Graph

```
prompt_caching
    amplified by: tool_defer_loading (smaller stable prefix = better cache hits)
    must avoid overlap with: auto_compaction (compaction must not touch cached region)
    must separate by: model_routing (each model tier has its own cache state)

auto_compaction
    requires: token counting from usage_log (to know when threshold is hit)
    triggers on: context window per routed model (model_routing informs threshold)
    optional hook: PreCompact hook (Feature 4 exposes this)

tool_defer_loading
    amplifies: prompt_caching savings
    requires: tool registry provides description separate from full schema

hook_api
    wires into: pipeline/execute pre/post tool dispatch
    extends: existing needs_approval system (does not replace)
    can wrap: auto_compaction (PreCompact hook)

model_routing
    extends: providers/routing.rs (already exists)
    requires: per-model context window awareness (for compaction threshold)
    must: maintain separate cache contexts per model tier

REF-03
    no feature dependencies -- pure internal refactor
    dashmap already in Cargo.toml from v0.19.0
```

## MVP Recommendation

Build in this order based on dependencies and bang-for-buck:

1. **REF-03** (1 day, zero risk, unblocks cleaner rate limiter for future load tests)
2. **Prompt caching** (3-4 days, direct cost savings, no new external APIs, high ROI)
3. **Tool defer_loading** (4-5 days, amplifies prompt caching savings, purely client-side)
4. **Auto-compaction** (4-5 days, enables long sessions, beta API integration needed)
5. **Model routing** (4-5 days, depends on stable routing.rs, test against real providers)
6. **Hook API** (7-10 days, high complexity, interface must be right first time — ship last to benefit from stable pipeline)

Defer: semantic-retrieval-based tool gating (too complex, lower confidence, requires embedding service in the hot path).

## Sources

- Anthropic Prompt Caching docs: https://platform.claude.com/docs/en/build-with-claude/prompt-caching (verified 2026-05-08)
- Anthropic Compaction beta docs: https://platform.claude.com/docs/en/build-with-claude/compaction (verified 2026-05-08)
- Tool Attention arxiv paper (April 2026): https://arxiv.org/abs/2604.21816
- Claude Agent SDK Hooks docs: https://code.claude.com/docs/en/agent-sdk/hooks (verified 2026-05-08)
- Strands Agents SDK Hooks: https://strandsagents.com/docs/user-guide/concepts/agents/hooks/
- RouteLLM framework: https://github.com/lm-sys/RouteLLM
- LogRocket LLM routing guide: https://blog.logrocket.com/llm-routing-right-model-for-requests/
- DashMap crate: https://docs.rs/dashmap/latest/dashmap/
- Prompt caching anti-patterns research: https://arxiv.org/html/2601.06007v2
- JetBrains context management research: https://blog.jetbrains.com/research/2025/12/efficient-context-management/
