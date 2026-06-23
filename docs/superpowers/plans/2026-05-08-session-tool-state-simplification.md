# SessionToolState Simplification — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove `call_counts` and `promoted` from `SessionToolState`, expose a clean
`get_describe`/`set_describe` API, delete the auto-promotion runtime path, and fix the
session-level lifecycle leak.

**Architecture:** Six sequential tasks ordered so the codebase compiles after every
commit. Tasks 1–5 are pure refactoring (zero behaviour change for describe_cache, zero
runtime change for promotion since dispatcher is off by default). Task 6 adds the
missing cleanup. Each task is a focused, self-contained change.

**Tech Stack:** Rust / tokio async, `dashmap`, `tokio::sync::RwLock`, sqlx, Axum handlers.

---

## File Map

| File | What changes |
| --- | --- |
| `crates/opex-core/src/agent/dispatcher/state.rs` | Rewrite struct; add `get_describe`/`set_describe`; remove old fields in Task 5 |
| `crates/opex-core/src/agent/tool_handlers/tool_use.rs` | Use new API; delete `promoted_set()`; pass `&HashSet::new()` |
| `crates/opex-core/src/agent/context_builder.rs` | Remove `agent_promotion_max` trait method; remove `state.promoted.read()` calls |
| `crates/opex-core/src/agent/engine/context_builder.rs` | Remove `agent_promotion_max` impl |
| `crates/opex-core/src/agent/pipeline/parallel.rs` | Remove both promotion blocks, `via_dispatcher_map`, `is_system_extension_tool`, `promotion_max` param |
| `crates/opex-core/src/agent/engine/tool_executor.rs` | Remove `promotion_max` read + pass-through |
| `crates/opex-core/src/gateway/handlers/sessions.rs` | Add `session_tool_state.remove()` and `.retain()` cleanup |

---

## Task 1: Add new `SessionToolState` API (keep old fields)

**Goal:** Introduce the clean `get_describe`/`set_describe` interface and tests. Old
fields (`call_counts`, `promoted`) stay intact so existing consumers compile unchanged.

**Files:**

- Modify: `crates/opex-core/src/agent/dispatcher/state.rs`

- [ ] **Step 1.1: Write failing tests**

Add a `#[cfg(test)]` module at the bottom of `state.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn describe_cache_miss_returns_none() {
        let state = SessionToolState::new();
        assert!(state.get_describe("my_tool").await.is_none());
    }

    #[tokio::test]
    async fn describe_cache_roundtrip() {
        let state = SessionToolState::new();
        state.set_describe("my_tool".to_string(), "schema text".to_string()).await;
        assert_eq!(
            state.get_describe("my_tool").await.as_deref(),
            Some("schema text")
        );
    }

    #[tokio::test]
    async fn describe_cache_different_keys_independent() {
        let state = SessionToolState::new();
        state.set_describe("tool_a".to_string(), "schema_a".to_string()).await;
        assert!(state.get_describe("tool_b").await.is_none());
    }
}
```

- [ ] **Step 1.2: Run tests — expect compile error**

```bash
cargo test -p opex-core dispatcher::state -- --nocapture
```

Expected: compile error — `get_describe`/`set_describe` not found.

- [ ] **Step 1.3: Add methods to `SessionToolState`**

In `state.rs`, add after the `new()` method:

```rust
/// Returns the cached rendered description for `name`, or `None` on miss.
pub async fn get_describe(&self, name: &str) -> Option<String> {
    self.describe_cache.read().await.get(name).cloned()
}

/// Inserts (or overwrites) the rendered description for `name`.
pub async fn set_describe(&self, name: String, value: String) {
    self.describe_cache.write().await.insert(name, value);
}
```

- [ ] **Step 1.4: Run tests — expect pass**

```bash
cargo test -p opex-core dispatcher::state -- --nocapture
```

Expected: 3 tests PASS.

- [ ] **Step 1.5: Compile check**

```bash
make check
```

Expected: no errors (old fields still present, old consumers unaffected).

- [ ] **Step 1.6: Commit**

```bash
git add crates/opex-core/src/agent/dispatcher/state.rs
git commit -m "refactor(dispatcher): add get_describe/set_describe API to SessionToolState"
```

---

## Task 2: Migrate `tool_use.rs` to new API

**Goal:** Replace direct `state.describe_cache.read/write().await` with `get_describe`/
`set_describe`. Delete `promoted_set()` — pass `&HashSet::new()` directly to
`build_extension_tool_list` and `find_extension_tool`.

**Files:**

- Modify: `crates/opex-core/src/agent/tool_handlers/tool_use.rs`

- [ ] **Step 2.1: Verify existing tests pass as baseline**

```bash
cargo test -p opex-core tool_handlers::tool_use -- --nocapture
```

Expected: all existing tests pass.

- [ ] **Step 2.2: Replace `promoted_set()` with inline `HashSet::new()`**

Delete this function entirely:

```rust
// DELETE THIS FUNCTION:
async fn promoted_set(deps: &ToolDeps<'_>) -> HashSet<String> {
    match deps.session_tool_state.as_ref() {
        Some(s) => s.promoted.read().await.clone(),
        None => HashSet::new(),
    }
}
```

In `handle_search`, replace:

```rust
// BEFORE:
let promoted = promoted_set(&deps).await;
let deny = deny_list(&deps);

let candidates = dispatcher::build_extension_tool_list(
    deps.agent_base,
    &deny,
    &promoted,
    deps.workspace_dir,
    deps.mcp,
).await;
```

```rust
// AFTER:
let deny = deny_list(&deps);

let candidates = dispatcher::build_extension_tool_list(
    deps.agent_base,
    &deny,
    &std::collections::HashSet::new(),
    deps.workspace_dir,
    deps.mcp,
).await;
```

In `handle_describe`, replace:

```rust
// BEFORE:
let promoted = promoted_set(&deps).await;
let deny = deny_list(&deps);

let tool = dispatcher::find_extension_tool(
    name,
    deps.agent_base,
    &deny,
    &promoted,
    deps.workspace_dir,
    deps.mcp,
).await;
```

```rust
// AFTER:
let deny = deny_list(&deps);

let tool = dispatcher::find_extension_tool(
    name,
    deps.agent_base,
    &deny,
    &std::collections::HashSet::new(),
    deps.workspace_dir,
    deps.mcp,
).await;
```

- [ ] **Step 2.3: Migrate describe cache to new API**

In `handle_describe`, replace the cache-read block:

```rust
// BEFORE:
if let Some(state) = deps.session_tool_state.as_ref() {
    let cache = state.describe_cache.read().await;
    if let Some(cached) = cache.get(name) {
        return cached.clone();
    }
}
```

```rust
// AFTER:
if let Some(state) = deps.session_tool_state.as_ref() {
    if let Some(cached) = state.get_describe(name).await {
        return cached;
    }
}
```

Replace the cache-write block:

```rust
// BEFORE:
if let Some(state) = deps.session_tool_state.as_ref() {
    let mut cache = state.describe_cache.write().await;
    cache.insert(name.to_string(), result.clone());
}
```

```rust
// AFTER:
if let Some(state) = deps.session_tool_state.as_ref() {
    state.set_describe(name.to_string(), result.clone()).await;
}
```

- [ ] **Step 2.4: Remove now-unused `HashSet` import from the top of `tool_use.rs`**

Check if `use std::collections::HashSet;` is still used after removing `promoted_set`.
If not (it was only for `promoted_set`'s return type), delete that import line. If the
compiler still needs it for other `HashSet` uses, keep it.

- [ ] **Step 2.5: Compile and test**

```bash
make check
cargo test -p opex-core tool_handlers::tool_use -- --nocapture
```

Expected: no errors, all existing tool_use tests pass.

- [ ] **Step 2.6: Commit**

```bash
git add crates/opex-core/src/agent/tool_handlers/tool_use.rs
git commit -m "refactor(tool_use): use SessionToolState new API; remove promoted_set()"
```

---

## Task 3: Remove `promoted` reads from `context_builder.rs`

**Goal:** Remove two `state.promoted.read().await.clone()` calls and the
`|| promoted.contains(&t.name)` filter from the partition retain. Remove the dead
`agent_promotion_max` trait method and its engine impl.

**Files:**

- Modify: `crates/opex-core/src/agent/context_builder.rs`
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs`

- [ ] **Step 3.1: Remove first `promoted_set` read (catalogue section)**

In `context_builder.rs` around line 376, replace:

```rust
// BEFORE:
let promoted_set = if let Some(state) = deps.session_tool_state(session_id) {
    state.promoted.read().await.clone()
} else {
    std::collections::HashSet::new()
};
```

```rust
// AFTER:
let promoted_set = std::collections::HashSet::new();
```

- [ ] **Step 3.2: Remove second `promoted` read (partition section)**

Around line 585, replace:

```rust
// BEFORE:
let promoted: std::collections::HashSet<String> =
    if let Some(state) = deps.session_tool_state(session_id) {
        state.promoted.read().await.clone()
    } else {
        std::collections::HashSet::new()
    };
```

```rust
// AFTER:
let promoted: std::collections::HashSet<String> = std::collections::HashSet::new();
```

- [ ] **Step 3.3: Remove `promoted_count` from tracing**

Find the `tracing::info!` call that includes `promoted_count` (around line 617) and
remove the `promoted_count` field:

```rust
// BEFORE:
let promoted_count = if let Some(state) = deps.session_tool_state(session_id) {
    state.promoted.read().await.len()
} else { 0 };
tracing::info!(
    agent = %deps.agent_name(),
    prompt_tokens = prompt_tokens,
    tools_tokens = tools_tokens,
    dispatcher_enabled = dispatcher_enabled,
    promoted_count = promoted_count,
    "context_size"
);
```

```rust
// AFTER:
tracing::info!(
    agent = %deps.agent_name(),
    prompt_tokens = prompt_tokens,
    tools_tokens = tools_tokens,
    dispatcher_enabled = dispatcher_enabled,
    "context_size"
);
```

- [ ] **Step 3.4: Remove `agent_promotion_max` from the `ContextBuilderDeps` trait**

In `context_builder.rs`, delete these lines:

```rust
// DELETE:
/// Cap on number of auto-promoted tools per session.
#[allow(dead_code)]
fn agent_promotion_max(&self) -> u32;
```

Also delete the comment block above it if it only describes `agent_promotion_max`
(check lines 133–140 — remove the part that mentions `agent_promotion_max` as
"pure plumbing for promotion-cap enforcement").

- [ ] **Step 3.5: Remove `agent_promotion_max` impl from `engine/context_builder.rs`**

Find and delete:

```rust
// DELETE:
fn agent_promotion_max(&self) -> u32 {
    self.cfg().agent.tool_dispatcher.promotion_max
}
```

- [ ] **Step 3.6: Compile and test**

```bash
make check
cargo test -p opex-core -- --nocapture 2>&1 | tail -20
```

Expected: no errors, no new test failures.

- [ ] **Step 3.7: Commit**

```bash
git add crates/opex-core/src/agent/context_builder.rs \
        crates/opex-core/src/agent/engine/context_builder.rs
git commit -m "refactor(context_builder): remove promoted reads and agent_promotion_max"
```

---

## Task 4: Remove promotion from `parallel.rs` and `tool_executor.rs`

**Goal:** Delete both promotion blocks, `via_dispatcher_map`, `is_system_extension_tool`,
the `promotion_max` parameter from `execute_tool_calls_partitioned`, and the
corresponding read + pass-through in `tool_executor.rs`.

**Files:**

- Modify: `crates/opex-core/src/agent/pipeline/parallel.rs`
- Modify: `crates/opex-core/src/agent/engine/tool_executor.rs`

- [ ] **Step 4.1: Remove `promotion_max` parameter from `execute_tool_calls_partitioned`**

In `parallel.rs`, remove line 218:

```rust
// DELETE this line from the function signature:
promotion_max: u32,
```

Also delete the comment above `via_dispatcher_map` that refers to `promotion_max`
(lines 229–232 — the "driven by `promotion_max`" comment block).

- [ ] **Step 4.2: Simplify `direct_pending` — remove the `bool` tracking**

Replace the `direct_pending` declaration and the loop that populates it:

```rust
// BEFORE:
let mut direct_pending: Vec<(ToolCall, bool)> = Vec::with_capacity(rewritten.len());
// ...
for (orig, r) in tool_calls.iter().zip(rewritten.into_iter()) {
    match r {
        crate::agent::dispatcher::RewriteResult::Direct(rewritten_tc) => {
            let via_dispatcher = orig.name == "tool_use" && rewritten_tc.name != "tool_use";
            direct_pending.push((rewritten_tc, via_dispatcher));
        }
        crate::agent::dispatcher::RewriteResult::Denied { id, reason } => {
            denied_results.push((id, reason));
        }
    }
}

let direct_calls: Vec<ToolCall> = direct_pending.iter().map(|(tc, _)| tc.clone()).collect();
// Maps tool_call_id → "originated as tool_use(action=call)?" — used by
// promotion logic at each `record_execution` site below.
let via_dispatcher_map: std::collections::HashMap<hydroclaw_types::ids::ToolCallId, bool> =
    direct_pending
        .iter()
        .map(|(tc, via)| (tc.id.clone(), *via))
        .collect();
```

```rust
// AFTER:
let mut direct_calls: Vec<ToolCall> = Vec::with_capacity(rewritten.len());
for r in rewritten.into_iter() {
    match r {
        crate::agent::dispatcher::RewriteResult::Direct(rewritten_tc) => {
            direct_calls.push(rewritten_tc);
        }
        crate::agent::dispatcher::RewriteResult::Denied { id, reason } => {
            denied_results.push((id, reason));
        }
    }
}
```

Note: the original loop used `tool_calls.iter().zip(rewritten.into_iter())` to
compute `via_dispatcher` per call. After removing promotion that flag is unused —
replace the `zip` with a plain iteration over `rewritten`. `direct_pending` is also
gone: the variable is now named `direct_calls` directly.

Immediately after this block, `parallel.rs` rebinds `tool_calls` to point at
`direct_calls` (line ~292: `let tool_calls: &[ToolCall] = &direct_calls;`). All
`tool_calls[i]` references inside the loop bodies are therefore safe — they index
into `direct_calls`, not the function parameter. No additional changes needed.

- [ ] **Step 4.3: Remove `is_system_extension_tool` function**

Delete lines 104–113:

```rust
// DELETE:
/// Variant A: only system extension tools may auto-promote. YAML and MCP
/// tools never promote — operators must add them to `core_extra` explicitly.
fn is_system_extension_tool(name: &str) -> bool {
    let core = crate::agent::pipeline::tool_defs::static_core_tool_names();
    if core.contains(&name) {
        return false;
    }
    let all_sys = crate::agent::pipeline::tool_defs::all_system_tool_names();
    all_sys.contains(&name)
}
```

- [ ] **Step 4.4: Remove first promotion block (parallel join_all path, ~line 498)**

Delete these lines (the entire `if via_dispatcher && success && ...` block):

```rust
// DELETE (parallel path, ~lines 498–533):
// Promote eligible system extension tools after threshold-many
// successful dispatcher-originated calls. Variant A — YAML/MCP
// never auto-promote.
let tc = &tool_calls[i];
let via_dispatcher =
    via_dispatcher_map.get(&tc.id).copied().unwrap_or(false);
if via_dispatcher
    && success
    && is_system_extension_tool(&tc.name)
    && let Some(state) = session_tool_state.as_ref()
{
    const PROMOTION_THRESHOLD: u32 = 2;
    let cap: u32 = promotion_max;

    let new_count = {
        let mut counts = state.call_counts.write().await;
        let entry = counts.entry(tc.name.clone()).or_insert(0);
        *entry += 1;
        *entry
    };

    if new_count >= PROMOTION_THRESHOLD {
        let mut promoted = state.promoted.write().await;
        if !promoted.contains(&tc.name)
            && (promoted.len() as u32) < cap
        {
            promoted.insert(tc.name.clone());
            tracing::info!(
                tool = %tc.name,
                count = new_count,
                promoted_total = promoted.len(),
                "tool_use promotion triggered"
            );
        }
    }
}
```

- [ ] **Step 4.5: Remove second promotion block (sequential path, ~line 675)**

Same pattern — delete the corresponding block in the sequential execution path:

```rust
// DELETE (sequential path, ~lines 675–710):
// Promote eligible system extension tools after threshold-many
// successful dispatcher-originated calls. Variant A — YAML/MCP
// never auto-promote.
let tc = &tool_calls[i];
let via_dispatcher =
    via_dispatcher_map.get(&tc.id).copied().unwrap_or(false);
if via_dispatcher
    && success
    && is_system_extension_tool(&tc.name)
    && let Some(state) = session_tool_state.as_ref()
{
    const PROMOTION_THRESHOLD: u32 = 2;
    let cap: u32 = promotion_max;

    let new_count = {
        let mut counts = state.call_counts.write().await;
        let entry = counts.entry(tc.name.clone()).or_insert(0);
        *entry += 1;
        *entry
    };

    if new_count >= PROMOTION_THRESHOLD {
        let mut promoted = state.promoted.write().await;
        if !promoted.contains(&tc.name)
            && (promoted.len() as u32) < cap
        {
            promoted.insert(tc.name.clone());
            tracing::info!(
                tool = %tc.name,
                count = new_count,
                promoted_total = promoted.len(),
                "tool_use promotion triggered"
            );
        }
    }
}
```

- [ ] **Step 4.6: Remove `promotion_max` read and pass-through in `tool_executor.rs`**

Delete line 137 and line 164:

```rust
// DELETE line 137:
let promotion_max = self.cfg().agent.tool_dispatcher.promotion_max;

// DELETE line 164 (the argument in the call to execute_tool_calls_partitioned):
promotion_max,
```

- [ ] **Step 4.7: Compile and test**

```bash
make check
cargo test -p opex-core -- --nocapture 2>&1 | tail -20
```

Expected: no errors, no new test failures.

- [ ] **Step 4.8: Commit**

```bash
git add crates/opex-core/src/agent/pipeline/parallel.rs \
        crates/opex-core/src/agent/engine/tool_executor.rs
git commit -m "refactor(parallel): remove auto-promotion path and promotion_max parameter"
```

---

## Task 5: Remove `call_counts` and `promoted` from `SessionToolState`

**Goal:** Now that no code writes to `call_counts` or `promoted`, remove them from the
struct. This is a safe deletion — the compiler will confirm zero remaining readers.

**Files:**

- Modify: `crates/opex-core/src/agent/dispatcher/state.rs`

- [ ] **Step 5.1: Remove `call_counts` and `promoted` fields**

Replace the current struct definition:

```rust
// BEFORE:
pub struct SessionToolState {
    /// Cached `describe()` rendered output, keyed by tool name.
    pub describe_cache: RwLock<HashMap<String, String>>,
    /// Number of successful calls per extension tool name in this session.
    /// Incremented in `pipeline/parallel.rs` after every successful
    /// dispatcher-originated `Direct` execution; promotion fires once the
    /// per-tool count reaches `PROMOTION_THRESHOLD`.
    pub call_counts: RwLock<HashMap<String, u32>>,
    /// System extension tools promoted to per-session core after threshold.
    pub promoted: RwLock<HashSet<String>>,
}

impl SessionToolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    // ... get_describe / set_describe added in Task 1
}
```

```rust
// AFTER:
/// Per-session describe cache for the tool dispatcher.
/// Avoids repeated filesystem reads (load_yaml_tools) within one session.
pub struct SessionToolState {
    describe_cache: RwLock<HashMap<String, String>>,
}

impl SessionToolState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { describe_cache: RwLock::new(HashMap::new()) })
    }

    pub async fn get_describe(&self, name: &str) -> Option<String> {
        self.describe_cache.read().await.get(name).cloned()
    }

    pub async fn set_describe(&self, name: String, value: String) {
        self.describe_cache.write().await.insert(name, value);
    }
}
```

Also remove `#[derive(Default)]` from the struct (no longer needed since `new()` is
explicit). Confirmed: no code calls `SessionToolState::default()` — safe to remove.
Remove unused imports at the top of `state.rs` — `HashSet` is no longer needed:

```rust
// CHECK and remove if unused after the change:
use std::collections::{HashMap, HashSet};  // → just: use std::collections::HashMap;
```

- [ ] **Step 5.2: Run all tests**

```bash
make check
cargo test -p opex-core dispatcher::state -- --nocapture
cargo test -p opex-core -- --nocapture 2>&1 | tail -20
```

Expected: all 3 describe_cache tests from Task 1 still pass. Full suite passes.

- [ ] **Step 5.3: Commit**

```bash
git add crates/opex-core/src/agent/dispatcher/state.rs
git commit -m "refactor(dispatcher): finalize SessionToolState — remove call_counts and promoted"
```

---

## Task 6: Add lifecycle cleanup in `sessions.rs`

**Goal:** Fix the memory leak — `session_tool_state` entries are now removed when
sessions are deleted, matching the existing `session_pools` cleanup pattern.

**Files:**

- Modify: `crates/opex-core/src/gateway/handlers/sessions.rs`

- [ ] **Step 6.1: Write a unit test for the cleanup invariant**

Add a `#[cfg(test)]` block at the bottom of `sessions.rs` (or in the existing `mod
tests` if one exists):

```rust
#[cfg(test)]
mod lifecycle_tests {
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn session_tool_state_removed_after_session_delete() {
        let tool_state: crate::agent::dispatcher::SessionToolStateMap =
            Arc::new(dashmap::DashMap::new());
        let session_id = Uuid::new_v4();

        // Simulate a describe call having populated state
        let state = crate::agent::dispatcher::SessionToolState::new();
        state.set_describe("tool".into(), "schema".into()).await;
        tool_state.insert(session_id, state);
        assert!(tool_state.contains_key(&session_id), "state should exist before delete");

        // Simulate the cleanup the handler will do
        tool_state.remove(&session_id);

        assert!(
            !tool_state.contains_key(&session_id),
            "state must be removed after session delete"
        );
    }

    #[tokio::test]
    async fn session_tool_state_retained_for_surviving_sessions() {
        let tool_state: crate::agent::dispatcher::SessionToolStateMap =
            Arc::new(dashmap::DashMap::new());
        let keep_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();

        tool_state.insert(keep_id, crate::agent::dispatcher::SessionToolState::new());
        tool_state.insert(delete_id, crate::agent::dispatcher::SessionToolState::new());

        // Simulate bulk delete of only delete_id
        let deleted = vec![delete_id];
        tool_state.retain(|sid, _| !deleted.contains(sid));

        assert!(tool_state.contains_key(&keep_id), "surviving session must stay");
        assert!(!tool_state.contains_key(&delete_id), "deleted session must be removed");
    }
}
```

- [ ] **Step 6.2: Run tests — expect pass (pure logic test, no handler deps)**

```bash
cargo test -p opex-core sessions::lifecycle_tests -- --nocapture
```

Expected: both tests PASS (the logic is just DashMap operations).

- [ ] **Step 6.3: Add cleanup to single-session delete handler**

In `sessions.rs`, find the `api_delete_session` handler. After the existing
`session_pools` cleanup block (around line 375–380), add one line:

```rust
// Existing:
let mut pools = agents.session_pools.write().await;
if let Some(mut pool) = pools.remove(&id)
    && !pool.is_empty() {
        tracing::info!(session_id = %id, count = pool.len(), "killing session agent pool on delete");
        pool.kill_all();
    }

// ADD after the pools block:
agents.session_tool_state.remove(&id);
```

- [ ] **Step 6.4: Add cleanup to bulk-delete handler**

In `api_delete_all_sessions`, find the existing `session_pools.retain` block (around
line 467–470) and add one line after it:

```rust
// Existing:
{
    let mut pools = agents.session_pools.write().await;
    pools.retain(|sid, _| !session_ids.contains(sid));
}

// ADD after the closing brace:
agents.session_tool_state.retain(|sid, _| !session_ids.contains(sid));
```

- [ ] **Step 6.5: Compile and full test**

```bash
make check
cargo test -p opex-core -- --nocapture 2>&1 | tail -20
```

Expected: no errors, all tests pass.

- [ ] **Step 6.6: Commit**

```bash
git add crates/opex-core/src/gateway/handlers/sessions.rs
git commit -m "fix(sessions): cleanup session_tool_state on session delete"
```

---

## Final Verification

- [ ] **Run full test suite**

```bash
make test
```

Expected: all tests pass, including the new tests from Tasks 1 and 6.

- [ ] **Run clippy**

```bash
make lint
```

Expected: no warnings about unused imports, dead code, or unused variables introduced
by the refactor.

- [ ] **Smoke-check dispatcher tests specifically**

```bash
cargo test -p opex-core dispatcher -- --nocapture
cargo test -p opex-core tool_handlers -- --nocapture
```

Expected: all pass.
