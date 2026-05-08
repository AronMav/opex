---
phase: 68-prompt-caching
plan: 02
subsystem: caching
tags: [rust, anthropic, prompt-caching, cache-control, system-prompt, context-builder]

# Dependency graph
requires:
  - phase: 68-01
    provides: AgentSettings.prompt_cache field, tool-level cache breakpoint in AnthropicProvider

provides:
  - CLAUDE.md as independently-cached third cache breakpoint in Anthropic requests
  - load_claude_md() and load_workspace_prompt_excluding_claude_md() workspace helpers
  - CallOptions.claude_md_content: Option<String> field for provider-agnostic plumbing
  - ContextSnapshot.claude_md_content and BootstrapOutcome.claude_md_content for pipeline propagation
  - ContextBuilderDeps: agent_prompt_cache(), load_claude_md(), load_workspace_prompt_excluding_claude_md()
  - All three execute.rs CallOptions sites wired with claude_md_content

affects:
  - phase-70-routing (ROUTE-02: RoutingProvider needs claude_md_content threading if wrapping Anthropic)
  - 68-verify (manual end-to-end: check system array is 2-block on base agent with prompt_cache=true)

# Tech tracking
tech-stack:
  added: []
  patterns:
    - "Separate-block content for volatile cache segments: operator-edited files get their own cache_control block to isolate invalidation from stable system prompt"
    - "Defensive whitespace guard in provider: empty/whitespace claude_md treated as absent even if context_builder sends it"
    - "Bootstrap-once, clone-many: claude_md_content loaded once in bootstrap, cloned at each CallOptions construction site"

key-files:
  created: []
  modified:
    - crates/hydeclaw-core/src/agent/providers/mod.rs
    - crates/hydeclaw-core/src/agent/workspace.rs
    - crates/hydeclaw-core/src/agent/providers/anthropic.rs
    - crates/hydeclaw-core/src/agent/context_builder.rs
    - crates/hydeclaw-core/src/agent/engine/context_builder.rs
    - crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs
    - crates/hydeclaw-core/src/agent/pipeline/execute.rs
    - crates/hydeclaw-core/src/agent/engine/run.rs
    - crates/hydeclaw-core/src/agent/pipeline/llm_call.rs
    - crates/hydeclaw-core/src/agent/providers/routing.rs

key-decisions:
  - "Removed Copy from CallOptions since Option<String> is not Copy; loop sites in llm_call.rs and routing.rs use .clone()"
  - "load_workspace_prompt_excluding_claude_md skips CLAUDE.md in the extra-files loop (name != CLAUDE.md guard) — all other .md files still included"
  - "MockContextBuilder gets claude_md_content: None since tests don't exercise the cache path"
  - "run.rs has 4 pairs of BootstrapOutcome destructure+construct — all 4 updated to thread claude_md_content"

patterns-established:
  - "Pitfall 5 guard: base-agent check (is_base && prompt_cache) before loading CLAUDE.md — non-base agents always use monolithic path"
  - "Forced-final-call sites MUST carry claude_md_content or cache third breakpoint is lost on those paths"

requirements-completed:
  - CACHE-02

# Metrics
duration: 45min
completed: 2026-05-08
---

# Phase 68 Plan 02: CLAUDE.md as Third Cache Breakpoint Summary

**CLAUDE.md split into separate system content block with cache_control: ephemeral when base agent + prompt_cache=true, amortizing 5-10KB project context across all turns within the 5-min TTL**

## Performance

- **Duration:** ~45 min
- **Started:** 2026-05-08T16:15:00Z
- **Completed:** 2026-05-08T17:00:00Z
- **Tasks:** 4 (Task 1, Task 2, Task 3a, Task 3b)
- **Files modified:** 10

## Accomplishments

- `CallOptions` extended with `claude_md_content: Option<String>`; `Copy` dropped in favour of `Clone`; all loop sites updated
- Two new workspace helpers: `load_claude_md` (returns `Ok(None)` for missing/whitespace), `load_workspace_prompt_excluding_claude_md`
- `AnthropicProvider::build_request_body` emits 2-element system array when `claude_md_content` is `Some(non-empty)` and `prompt_cache=true`
- Full pipeline plumbing: `ContextSnapshot` → `BootstrapOutcome` → `execute.rs` at all three `CallOptions` sites (main loop + 2 forced-final paths)
- 8 new tests across workspace.rs (4) and anthropic.rs (4); zero regressions in 55 lib tests

## Task Commits

1. **Task 1: Extend CallOptions + CLAUDE.md workspace loaders** - `fb408c3d` (feat)
2. **Task 2: Two-block system content in AnthropicProvider** - `febdcc6e` (feat)
3. **Task 3a: ContextBuilderDeps + ContextSnapshot + BootstrapOutcome** - `8b2ed2ee` (feat)
4. **Task 3b: Wire claude_md_content into all three CallOptions sites** - `54e1dd4b` (feat)

## Files Created/Modified

- `providers/mod.rs` — `CallOptions`: added `claude_md_content: Option<String>`, dropped `Copy`, kept `Clone + Default`
- `workspace.rs` — 2 new public async fns + 4 new TDD tests
- `providers/anthropic.rs` — system-message branch rewrite + 4 new CACHE-02 tests + existing test literal fixes
- `context_builder.rs` — `ContextSnapshot` 5th field; trait 3 new methods; `DefaultContextBuilder::build` cache-aware load path; `MockContextBuilder` updated
- `engine/context_builder.rs` — 3 new `ContextBuilderDeps` impl methods
- `pipeline/bootstrap.rs` — `BootstrapOutcome` 13th field; `ContextSnapshot` destructure + literal updated
- `pipeline/execute.rs` — `BootstrapOutcome` destructure binds `claude_md_content`; 3 `CallOptions` sites populated
- `engine/run.rs` — all 4 pairs of `BootstrapOutcome` destructure+construct updated
- `pipeline/llm_call.rs` — 3 loop sites use `opts.clone()` (overflow-recovery, transient-retry, deadline-inner)
- `providers/routing.rs` — 2 call sites use `opts.clone()` (chat primary + fallback, chat_stream primary + fallback)

## Decisions Made

- `Copy` removed from `CallOptions` because `Option<String>` is not `Copy`. All loop sites where `opts` was implicitly copied now call `opts.clone()`. Non-loop single-call sites still pass by value (move). No performance concern: `claude_md_content` is `None` for non-base agents; it is `Some(String)` only for base agents which is a single agent per deployment.
- `load_workspace_prompt_excluding_claude_md` is an exact copy of `load_workspace_prompt` except the extra-files loop adds `name != "CLAUDE.md"` guard. This is minimal duplication with clear intent; the two functions have divergent futures.
- `MockContextBuilder` gets `claude_md_content: None` — test helpers don't exercise the cache path and no test needs CLAUDE.md content.

## Deviations from Plan

### Auto-fixed Issues

**1. [Rule 1 - Bug] Copy derive incompatible with Option<String>**
- **Found during:** Task 1 (extending CallOptions)
- **Issue:** `CallOptions` had `#[derive(Default, Clone, Copy, Debug)]` but `Option<String>` is not `Copy`, causing immediate compile failure
- **Fix:** Removed `Copy` from the derive list; updated 3 loop sites in `llm_call.rs` (overflow-recovery, transient-retry, deadline-inner) and 2 sites in `routing.rs` (chat and chat_stream) to use `.clone()`
- **Files modified:** `providers/mod.rs`, `pipeline/llm_call.rs`, `providers/routing.rs`
- **Verification:** `cargo check -p hydeclaw-core` exits 0
- **Committed in:** `fb408c3d` (Task 1 commit)

**2. [Rule 1 - Bug] Existing anthropic.rs tests used bare CallOptions struct literals**
- **Found during:** Task 1 (running tests after field addition)
- **Issue:** Two existing tests had `CallOptions { thinking_level: N }` without the new field, causing E0063 compile error
- **Fix:** Changed both to `CallOptions { thinking_level: N, ..Default::default() }`
- **Files modified:** `providers/anthropic.rs`
- **Verification:** `cargo test -p hydeclaw-core "workspace"` exits 0
- **Committed in:** `fb408c3d` (Task 1 commit)

**3. [Rule 2 - Missing Critical] run.rs has 4 BootstrapOutcome pairs requiring update**
- **Found during:** Task 3a (grep of BootstrapOutcome construction sites)
- **Issue:** Plan documented bootstrap.rs and execute.rs only; `engine/run.rs` has 4 pairs of destructure+reconstruct that also needed the new field
- **Fix:** Updated all 4 pairs (handle_sse, handle_with_status, handle_streaming, handle_isolated_via_pipeline) to bind and thread `claude_md_content`
- **Files modified:** `engine/run.rs`
- **Verification:** `cargo check -p hydeclaw-core --tests` exits 0
- **Committed in:** `8b2ed2ee` (Task 3a commit)

---

**Total deviations:** 3 auto-fixed (2 Rule 1 bugs, 1 Rule 2 missing wiring)
**Impact on plan:** All auto-fixes required for correctness. No scope creep.

## Issues Encountered

None beyond the deviations listed above.

## Next Phase Readiness

- CACHE-02 complete: Anthropic requests for base agents with `prompt_cache=true` and a non-empty `workspace/agents/{Name}/CLAUDE.md` now emit a 2-element system array, each block with `cache_control: ephemeral`
- CACHE-01, CACHE-02, CACHE-03 all complete for Phase 68
- Remaining: manual end-to-end verification via `/gsd:verify-work` (capture request body, verify system array length = 2, verify `cache_read_input_tokens` covers CLAUDE.md tokens on turn 2)
- Phase 70 (ROUTE-02): if `RoutingProvider` wraps Anthropic calls, `claude_md_content` must be threaded through — currently `routing.rs prompt_cache: None` intentionally untouched per Phase 68-01 decision

---
*Phase: 68-prompt-caching*
*Completed: 2026-05-08*
