# Project Research Summary

**Project:** HydeClaw v0.20.0 -- Harness Quality
**Domain:** Production AI gateway harness engineering (Rust, ARM64, single-binary)
**Researched:** 2026-05-08
**Confidence:** HIGH

## Executive Summary

HydeClaw v0.20.0 is a harness-engineering milestone, not a feature-addition one. All six features either extend mechanisms that are already partially or fully built in the codebase, or perform an internal refactor on an isolated module. The biggest risk is not whether the features can be built, but whether interacting features will break each other. Prompt caching, auto-compaction, and tool defer_loading form a tight triangle -- each affects token counts, which affects the others. Building them in isolation and composing at the end is the correct strategy.

The recommended build order is: REF-03 (zero-risk cleanup first), then prompt caching (immediate ROI, no dependencies), then auto-compaction threshold verification (may be a one-liner), then model routing (self-contained, amplifies caching ROI), then tool defer_loading (leans on dispatcher infrastructure), and lastly hook API (highest interface risk, benefits from a stable pipeline). This order minimises integration risk by ensuring the most cross-cutting features are added after the features they wrap are stable.

The top cross-cutting concern is token counting correctness: when prompt caching is active, the input_tokens field from Anthropic is deflated (it excludes cached tokens). The compaction threshold check must use input_tokens + cache_read_input_tokens + cache_creation_input_tokens as the effective context size. The second concern is cache stability: placing the tool-array cache breakpoint on anything other than the stable tail of the tools array produces cache misses on every turn, turning a cost-saving feature into a cost-increasing one.
## Key Findings

### Recommended Stack

No new crates are required. DashMap 6.1.0 is already in Cargo.lock (transitively resolved). All other features are internal changes to existing modules. The stack is stable: Rust 2024, Axum 0.8, sqlx 0.8, tokio full, serde_json, existing AnthropicProvider, RoutingProvider, Compressor, HookRegistry, and DashMap. The zero-new-dependencies constraint is fully satisfied.

**Core technologies being extended:**
- AnthropicProvider::build_request_body() -- cache breakpoint for system message and stable tool-array tail
- Compressor + CompactionConfig -- threshold default bumped to 0.85; token counting corrected for cache-aware sessions
- RoutingProvider::select_route() -- new complexity + context_heavy conditions with AtomicU32 for accumulated token tracking
- YamlToolDef + DefaultContextBuilder -- defer_loading field and dispatcher-catalogue integration
- HookRegistry + HookEvent + HookAction -- SessionStart, PreToolUse (with arguments), PostToolUse (with output) variants
- AuthRateLimiter + RequestRateLimiter -- Mutex<HashMap> swapped for DashMap with 4-shard constructor

### Expected Features

**Must have (table stakes for the milestone):**
- Prompt cache markers on system message and tools -- every session turn pays full input cost without this
- Auto-compaction at 85% threshold -- sessions fail with context-exceeded errors in long conversations
- Token counting corrected for cached turns -- prerequisite for compaction correctness when caching is enabled
- SessionStart, PreToolUse, PostToolUse hook events with arguments and output -- existing events lack payload for inspection hooks

**Should have (differentiators):**
- Tool cache breakpoint stability across tool array changes -- without this, caching costs more than it saves when YAML tools vary
- Model routing by accumulated context size (context_heavy condition) -- routing by last-message length alone misroutes multi-turn sessions
- HookAction::Modify for PreToolUse argument rewriting -- enables Governor pattern without code changes
- defer_loading per-pipeline state (not per-engine) -- concurrent sessions must not share deferred-tool state

**Defer to later milestones:**
- Semantic retrieval-based tool gating (embedding in hot path, too slow on Pi)
- Hook marketplace / plugin system
- 1-hour TTL cache variant for cron agents
- Per-agent cache hit-rate dashboard

### Architecture Approach

All six features slot into the existing well-decomposed pipeline without touching the critical hot path. Natural injection points: SessionStart hooks at end of bootstrap.rs (after DB write, before first LLM call); PreToolUse/PostToolUse hooks in execute.rs around execute_batch; cache markers injected at serialization time inside build_request_body only (never in the Message struct); deferred tool state as a session-local HashSet<String> mirroring how LoopDetector is scoped.

**Major integration points:**
1. providers/anthropic.rs::build_request_body() -- cache markers; strictly Anthropic-only; other providers untouched
2. pipeline/execute.rs lines 762 and 815-826 -- PreToolUse and PostToolUse hook calls; must fire before needs_approval() check
3. config/mod.rs::CompactionConfig::default_threshold() -- single-value bump to 0.85
4. agent/compressor.rs::should_compress() -- token sum fix for cache-aware sessions
5. providers/routing.rs::select_route() -- new complexity and context_heavy condition arms
6. hydeclaw-gateway-util/src/rate_limiter.rs -- DashMap swap with 4-shard constructor

### Critical Pitfalls

1. **Token counting mismatch with prompt caching active (Pitfall 2.1)** -- input_tokens is deflated when cache is active; compaction never fires. Fix: use combined token sum (input + cache_read + cache_creation) in should_compress(). Must be coordinated between caching and compaction phases.

2. **Tool cache breakpoint on last tool is unstable (Pitfall 1.2)** -- if YAML tools change between turns, every request becomes a cache write with zero reads. Fix: breakpoint on the last stable (system) tool; volatile YAML tools after. Monitor cache_hit_rate; alert below 20%.

3. **Deferred tool state shared via AgentEngine Arc (Pitfall 3.3)** -- concurrent sessions race on loaded-tools state if stored in AgentEngine. Fix: per-pipeline-invocation HashSet<String> scoped to execute(), consistent with LoopDetector ownership.

4. **Hook fires before approval row creation is checked (Pitfall 4.3)** -- hook Block after the approval DB row exists leaves a ghost pending approval forever. Fix: hook check is the very first step in tool dispatch, before needs_approval() and before any DB write.

5. **DashMap guard held across await in async rate limiter (Pitfall 6.1)** -- DashMap shard guards are not Send; holding one across .await causes compile error or silent early release. Fix: fetch-clone-drop pattern.

## Implications for Roadmap

### Phase 1: REF-03 -- DashMap Rate Limiter Swap
**Rationale:** Zero-risk mechanical refactor, no dependencies on any other feature. Validates DashMap usage patterns (guard scoping, shard count) before more complex work.
**Delivers:** Sharded rate limiter, removal of async Mutex in hot path, validated DashMap patterns for the rest of the milestone.
**Addresses:** REF-03 from the feature list.
**Avoids:** Pitfall 6.1 (guard-across-await); 6.2 (with_capacity_and_shard_amount(0, 4)); 6.3 (per-key remove in sweeper instead of full-map retain).

### Phase 2: Prompt Caching -- System + Tools Breakpoints
**Rationale:** Highest-ROI change. Purely additive to anthropic.rs. No feature dependencies. Immediate cost savings visible in usage_log. Must precede compaction work so token counting is correct from the start.
**Delivers:** cache_control markers on system message and last stable tool, cache_hit_rate monitoring counter, provider-type guard preventing leakage to OpenAI/Google.
**Addresses:** Prompt caching table stakes from FEATURES.md.
**Avoids:** Pitfall 1.1 (non-Anthropic guard); 1.2 (stable breakpoint ordering); 1.3 (breakpoints on system/tools only, never messages array); 1.4 (skip caching if system < 1024 tokens).

### Phase 3: Auto-Compaction -- Threshold + Token Counting Fix
**Rationale:** Infrastructure is already built. Work is the default_threshold() change to 0.85, the critical token-counting fix for cache-aware sessions, and fixing default_context_for_model() for claude-opus-4-7/4-6 and claude-sonnet-4-6 (currently returns 200k, correct value is 1M).
**Delivers:** Correct compaction at 85%, correct effective token count in should_compress() when caching is active, correct 1M context limit for new Claude models, CancellationToken support for compaction LLM call.
**Addresses:** Auto-compaction table stakes; resolves the cross-cutting token-counting concern.
**Avoids:** Pitfall 2.1 (combined token sum); 2.2 (CancellationToken); 2.3 (anti-thrash reset on genuine context growth); ARM.2 (sliding window for compaction input, not full 170K token history).

### Phase 4: Model Routing -- Complexity + Context-Heavy Conditions
**Rationale:** Self-contained extension to routing.rs. No dependencies on hooks or defer_loading. The context_heavy condition depends on corrected last_prompt_tokens from Phase 3.
**Delivers:** complexity and context_heavy routing conditions, AtomicU32 for accumulated token tracking in RoutingProvider, routing decisions logged to session_events.
**Addresses:** Model routing differentiators from FEATURES.md.
**Avoids:** Pitfall 5.1 (no panicking code in select_route); 5.2 (context-size-aware routing); 5.3 (propagate prompt_cache from ProviderRow.options to routing overrides).

### Phase 5: Tool defer_loading -- Lazy Schema Stubs
**Rationale:** Leans on dispatcher infrastructure which must be stable. Amplifies prompt caching savings (smaller stable tool prefix = better cache hits). Must follow Phase 2 to correctly coordinate cache stability with tool array changes.
**Delivers:** defer_loading: bool in YamlToolDef, dispatcher-catalogue integration for deferred tools, per-pipeline-invocation loaded-tools state, lazy load on first dispatch call.
**Addresses:** Tool defer_loading table stakes and cache synergy.
**Avoids:** Pitfall 3.1 (load on first dispatch, not only on describe); 3.2 (coordinate tool loading with caching phase); 3.3 (per-pipeline state not per-engine).

### Phase 6: Hook API -- PreToolUse / PostToolUse / SessionStart
**Rationale:** Highest interface complexity, must be last. All other pipeline changes should be stable before hooks are added, as hooks wrap all of them. The TOML schema change touches AgentConfig loaded at startup for every agent.
**Delivers:** Extended HookEvent/HookAction variants with arguments and output, SessionStart after bootstrap DB write, PreToolUse as first dispatch step (before approval check), PostToolUse after result push, TOML [[hooks]] config with serde(default) backward compat.
**Addresses:** Hook API table stakes and differentiators from FEATURES.md.
**Avoids:** Pitfall 4.1 (fire_webhooks pattern for async hooks, no await while holding locks); 4.2 (after DB write boundary); 4.3 (before approval row creation); 4.4 (serde(default) on all new fields, snapshot test on existing agent TOMLs).

### Phase Ordering Rationale

- REF-03 first: zero feature dependencies, validates DashMap patterns used later
- Caching before compaction: Pitfall 2.1 token-counting fix requires both to work in coordination
- Compaction before routing: context_heavy condition depends on corrected last_prompt_tokens
- defer_loading after caching: primary value is amplifying cache hits; measuring it requires caching active
- Hooks last: interface must be stable; wraps all other features

### Research Flags

Phases needing deeper research during planning:
- **Phase 5 (tool defer_loading):** Does Anthropic accept a tool call response for a tool whose schema was an empty-properties stub in the request? Validate against live API before committing to two-pass approach.
- **Phase 6 (hook API):** Exact pipeline state at PreToolUse point (ToolCall struct at execute.rs line 762) needs confirmation before designing HookAction::Modify.

Phases with standard patterns (skip research-phase):
- **Phase 1 (REF-03):** DashMap substitution is mechanical and well-documented
- **Phase 2 (prompt caching):** Official Anthropic docs verified; existing implementation partially complete
- **Phase 3 (auto-compaction):** Infrastructure fully built; config change plus token-sum fix
- **Phase 4 (model routing):** RoutingProvider already exists; new conditions are pure Rust logic

## Confidence Assessment

| Area | Confidence | Notes |
|------|------------|-------|
| Stack | HIGH | Zero new crates. All changes in existing modules. DashMap already resolved. |
| Features | HIGH | All 6 features have existing infrastructure to extend. No greenfield components. |
| Architecture | HIGH | Findings from direct source code inspection, not inference. Integration points confirmed. |
| Pitfalls | HIGH | Pitfalls derived from actual code paths: token counting, guard scoping, approval ordering. |

**Overall confidence:** HIGH

### Gaps to Address

- **Token counting + caching combined test:** No existing test validates should_compress() with non-zero cache_read_input_tokens. This is the TDD prerequisite for Phase 3. Extend MockProvider with configurable TokenUsage per call including cache fields.

- **Anthropic stub-schema tool call behavior:** Not confirmed whether Anthropic validates tool input against the schema at the API level. If it does, the defer_loading stub approach (empty properties schema) may fail with 400. Verify with live API test before Phase 5.

- **hooks.rs reconciliation:** ARCHITECTURE.md describes Hook API as requiring a new file. STACK.md confirms there is an existing src/agent/hooks.rs with BeforeToolCall/AfterToolResult. The implementation is an extension of the existing file, not creation of a new one.

- **Context limit fallback for new Claude models:** default_context_for_model() returns 200,000 for all Claude models. For claude-opus-4-7, claude-opus-4-6, claude-sonnet-4-6 the correct value is 1,000,000. This affects both compaction thresholds and routing decisions. Fix belongs in Phase 3.

## Sources

### Primary (HIGH confidence)
- Anthropic Prompt Caching docs (verified 2026-05-08): https://platform.claude.com/docs/en/docs/build-with-claude/prompt-caching
- Anthropic Models overview (verified 2026-05-08): https://platform.claude.com/docs/en/docs/about-claude/models/overview
- Anthropic Compaction beta docs (verified 2026-05-08): https://platform.claude.com/docs/en/build-with-claude/compaction
- Direct source code inspection: providers/anthropic.rs, agent/compressor.rs, pipeline/execute.rs, pipeline/llm_call.rs, providers/routing.rs, agent/hooks.rs, hydeclaw-gateway-util/src/rate_limiter.rs, agent/context_builder.rs, tools/yaml_tools.rs

### Secondary (MEDIUM confidence)
- Tool Attention arxiv paper (April 2026): https://arxiv.org/abs/2604.21816 -- 95% tool token reduction claim
- Claude Agent SDK Hooks docs: https://code.claude.com/docs/en/agent-sdk/hooks -- hook pattern design
- RouteLLM framework: https://github.com/lm-sys/RouteLLM -- routing heuristic baselines
- DashMap crate docs: https://docs.rs/dashmap/latest/dashmap/ -- guard Send limitations

### Tertiary (LOW confidence / needs live validation)
- Anthropic stub-schema tool acceptance: inferred from caching docs, not confirmed against live API
- Pi memory behavior during compaction: estimated from 170K token serialization size; not measured on actual Pi hardware

---
*Research completed: 2026-05-08*
*Ready for roadmap: yes*
