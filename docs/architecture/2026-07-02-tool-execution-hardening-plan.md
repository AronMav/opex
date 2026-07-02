# Tool-Execution Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship the five §13 recommendations of the tool-execution report as isolated, tested backend units.

**Architecture:** `crates/opex-core` (Rust). Six tasks in ascending-risk order (R1→R5→R4→R3→R2). Exactly one DB migration (R2); R3 rides the existing JSONB timeline payload. Each task is an independent TDD cycle behind its own reviewer gate.

**Tech Stack:** Rust 2024, tokio, sqlx (Postgres 17 + pgvector), serde, `#[sqlx::test]` for DB tests. rustls-tls only (never add OpenSSL).

## Global Constraints

- Design spec: `docs/architecture/2026-07-02-tool-execution-hardening-design.md` (authoritative).
- Commits: on `master`, NO `Co-Authored-By` trailer. Conventional prefixes (`refactor(agent):`, `feat(config):`, `fix(agent):`).
- Test harness: pure-Rust unit tests run under `cargo test -p opex-core`; DB tests use `#[sqlx::test(migrations = "../../migrations")]` and need Postgres — run the full suite with `make test-db` (boots isolated Postgres on :5434). Migrations auto-run on startup and in `#[sqlx::test]`.
- Gate before deploy: `make check` (cargo check --all-targets) + `make lint` (clippy -D warnings) + `make test-db`. Deploy is a single `make remote-deploy` at the very end (NOT per task).
- **R3 hard constraint:** the persisted/replayed loop hash MUST be computed over `loop_detector_key(tc)` (NOT `tc.name`) so it matches the live detector on the `tool_use` dispatcher path.
- **R4/R5 seams:** the webhook chain budget must be threaded into `HookRegistry` via `set_webhooks` (not just `HooksConfig`); the per-tool cache config must reach `parallel.rs` via a new `ToolExecutor` trait method (config is otherwise out of scope there).

## File Structure

| File | Responsibility | Tasks |
|---|---|---|
| `crates/opex-core/src/agent/pipeline/parallel.rs` | tool batch exec; shared merge surface (R1 signature, R3 payload+hash, R5 cache gate) | 1,4,2 |
| `crates/opex-core/src/agent/engine/tool_executor.rs` | `ToolExecutor` impls + the one call site of the free fn | 1,2 |
| `crates/opex-core/src/config/mod.rs` | `SemanticCacheToolConfig` + `[semantic_cache]`; `HooksConfig` chain-budget fields | 2,3 |
| `crates/opex-core/src/agent/hooks.rs` | `HookRegistry` chain budget + `fire_decision` | 3 |
| `crates/opex-core/src/gateway/handlers/agents/lifecycle.rs` | `set_webhooks` call site | 3 |
| `crates/opex-core/src/agent/tool_loop.rs` | LoopDetector hash-replay warm-up | 4 |
| `crates/opex-db/src/session_timeline.rs` | `TimelineToolEvent.args_hash` + reader | 4 |
| `migrations/070_tool_quality_per_agent.sql` | per-(agent,tool) penalty schema | 5 |
| `crates/opex-core/src/db/tool_quality.rs` | `record_tool_result`/`PenaltyCache`/`get_*` per-agent | 5,6 |
| `crates/opex-core/src/db/audit_queue.rs` | `AuditEvent::ToolQuality { agent_name }` | 5 |
| `crates/opex-core/src/agent/engine_dispatch.rs` | passes `agent_name` into the event | 5 |
| `crates/opex-core/src/agent/engine/context_builder.rs` | `tool_penalties()` passes agent name | 6 |
| `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs` | degraded-tools payload gains `agent_name` | 6 |

---

### Task 1: R1 — remove the dead `_session_tool_state` parameter

**Files:**
- Modify: `crates/opex-core/src/agent/pipeline/parallel.rs:186-210` (drop the param from `execute_tool_calls_partitioned`)
- Modify: `crates/opex-core/src/agent/engine/tool_executor.rs:128-130,156` (drop the lookup + the argument)

**Interfaces:**
- Consumes: nothing.
- Produces: `execute_tool_calls_partitioned(...)` with the `_session_tool_state` param removed (one fewer arg). No other change — `SessionToolState`/describe-cache stays live via `tool_registry.rs` + `tool_handlers/tool_use.rs` (untouched).

- [ ] **Step 1: Prove the param is dead — the compiler is the test.** Confirm no reads:

Run: `grep -n "_session_tool_state" crates/opex-core/src/agent/pipeline/parallel.rs`
Expected: exactly ONE line — the param declaration at ~:207 (no uses in the body).

- [ ] **Step 2: Remove the parameter from the signature.** In `parallel.rs`, delete the line:

```rust
    _session_tool_state: Option<Arc<crate::agent::dispatcher::SessionToolState>>,
```

- [ ] **Step 3: Remove the lookup + argument at the only call site.** In `engine/tool_executor.rs`, delete the retrieval block (~:128-130) that reads `self.cfg().session_tool_state` into a local, and delete the corresponding argument in the `execute_tool_calls_partitioned(...)` call (~:156). Leave `self.cfg().session_tool_state` itself and its other consumers intact.

- [ ] **Step 4: Verify it compiles (a broken caller would fail here).**

Run: `cargo check -p opex-core --all-targets`
Expected: compiles clean (0 errors). If any OTHER caller passed the param, this surfaces it — there are none (the other callers use the 10-arg wrapper method that lacks it).

- [ ] **Step 5: Run the existing tool-exec tests.**

Run: `cargo test -p opex-core loop_detector 2>&1 | tail -5`
Expected: PASS (unchanged behavior).

- [ ] **Step 6: Commit.**

```bash
git add crates/opex-core/src/agent/pipeline/parallel.rs crates/opex-core/src/agent/engine/tool_executor.rs
git commit -m "refactor(agent): remove dead _session_tool_state param from execute_tool_calls_partitioned"
```

---

### Task 2: R5 — per-tool TTL/threshold for the semantic search cache

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs` (add `SemanticCacheToolConfig` + `[semantic_cache]` map + default)
- Modify: `crates/opex-core/src/agent/pipeline/parallel.rs:68-84` (trait method), `:120-125` (`is_tool_cacheable`), `:296`/`:474`/`:614` (gates), `:491`/`:632` (store TTL), `:296` (check threshold)
- Modify: `crates/opex-core/src/agent/engine/tool_executor.rs:233`-area (impl the trait method)
- Test: `crates/opex-core/src/config/mod.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing.
- Produces: `SemanticCacheToolConfig { ttl_secs: u64, threshold: f32 }`; `AppConfig`/`app_config` exposes `fn semantic_cache_config(&self, tool: &str) -> Option<SemanticCacheToolConfig>` (with built-in defaults for the 4 tools); `ToolExecutor::semantic_cache_config(&self, tool: &str) -> Option<SemanticCacheToolConfig>` (default `None` on the trait, real impl reads config).

- [ ] **Step 1: Write the failing config test.** Add to `config/mod.rs` tests:

```rust
#[cfg(test)]
mod semantic_cache_tests {
    use super::*;

    #[test]
    fn defaults_cover_the_four_builtin_tools() {
        let cfg = SemanticCacheConfig::default();
        let s = cfg.for_tool("searxng_search").expect("searxng is cacheable by default");
        assert_eq!(s.ttl_secs, 3600);
        assert!((s.threshold - 0.95).abs() < f32::EPSILON);
        assert!(cfg.for_tool("web_search").is_some());
    }

    #[test]
    fn override_replaces_default_ttl() {
        let mut map = std::collections::HashMap::new();
        map.insert("web_search".to_string(), SemanticCacheToolConfig { ttl_secs: 300, threshold: 0.9 });
        let cfg = SemanticCacheConfig { tools: map };
        assert_eq!(cfg.for_tool("web_search").unwrap().ttl_secs, 300);
        // built-in tools NOT in the override map still resolve to defaults
        assert_eq!(cfg.for_tool("searxng_search").unwrap().ttl_secs, 3600);
    }

    #[test]
    fn unknown_tool_is_not_cacheable() {
        assert!(SemanticCacheConfig::default().for_tool("workspace_read").is_none());
    }
}
```

- [ ] **Step 2: Run it to confirm it fails.**

Run: `cargo test -p opex-core semantic_cache_tests 2>&1 | tail -8`
Expected: FAIL to compile — `SemanticCacheConfig` / `SemanticCacheToolConfig` / `for_tool` not found.

- [ ] **Step 3: Implement the config types + resolution.** Add to `config/mod.rs`:

```rust
/// Per-tool override for the semantic SEARCH cache (distinct from the YAML-tool
/// response cache `ToolCacheConfig`/`tools_cache`). TOML: `[semantic_cache]`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
pub struct SemanticCacheToolConfig {
    pub ttl_secs: u64,
    pub threshold: f32,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SemanticCacheConfig {
    /// tool_name → override. Missing built-in tools fall back to the 3600/0.95 default.
    #[serde(flatten)]
    pub tools: std::collections::HashMap<String, SemanticCacheToolConfig>,
}

impl SemanticCacheConfig {
    /// The four built-in cacheable search tools; used when a tool has no explicit override.
    fn builtin_default(tool: &str) -> Option<SemanticCacheToolConfig> {
        matches!(tool, "searxng_search" | "brave_search" | "browser_render" | "web_search")
            .then_some(SemanticCacheToolConfig { ttl_secs: 3600, threshold: 0.95 })
    }
    /// Resolve a tool's cache config: explicit override wins, else built-in default, else None.
    pub fn for_tool(&self, tool: &str) -> Option<SemanticCacheToolConfig> {
        self.tools.get(tool).copied().or_else(|| Self::builtin_default(tool))
    }
}
```

Add the field to the top-level app config struct (next to the existing `tools_cache`):

```rust
    #[serde(default)]
    pub semantic_cache: SemanticCacheConfig,
```

- [ ] **Step 4: Run the config test to green.**

Run: `cargo test -p opex-core semantic_cache_tests 2>&1 | tail -6`
Expected: PASS (3 tests).

- [ ] **Step 5: Add the `ToolExecutor` trait seam + impl.** In `parallel.rs` trait (`:68`), add a defaulted method next to `agent_safety_timeout`:

```rust
    /// Per-tool semantic-cache config (None = tool is not cacheable). Default: not cacheable.
    fn semantic_cache_config(&self, _tool: &str) -> Option<crate::config::SemanticCacheToolConfig> {
        None
    }
```

In `engine/tool_executor.rs` (the impl carrying `agent_safety_timeout` at :233), add:

```rust
    fn semantic_cache_config(&self, tool: &str) -> Option<crate::config::SemanticCacheToolConfig> {
        self.cfg().app_config.semantic_cache.for_tool(tool)
    }
```

- [ ] **Step 6: Route the four cache sites through the executor.** In `parallel.rs`:
  - Replace `is_tool_cacheable(&tc.name)` at the pre-check (`:287`), parallel-store (`:474`), sequential-store (`:614`) gates with `executor.semantic_cache_config(&tc.name).is_some()` (the `executor: &dyn ToolExecutor` is already in scope).
  - At the check (`:296`), replace the literal `0.95` with `executor.semantic_cache_config(&tc.name).map(|c| c.threshold).unwrap_or(0.95)`.
  - At both store sites (`:491`, `:632`), replace the literal `3600` with `executor.semantic_cache_config(&tool_calls[i].name).map(|c| c.ttl_secs as i64).unwrap_or(3600)`.
  - Delete the now-unused free fn `is_tool_cacheable` (`:120-125`).

- [ ] **Step 7: Compile + full lib tests.**

Run: `cargo check -p opex-core --all-targets && cargo test -p opex-core 2>&1 | tail -5`
Expected: compiles; tests PASS (empty `[semantic_cache]` reproduces today's 3600/0.95 behavior).

- [ ] **Step 8: Commit.**

```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/pipeline/parallel.rs crates/opex-core/src/agent/engine/tool_executor.rs
git commit -m "feat(config): per-tool TTL/threshold for the semantic search cache ([semantic_cache])"
```

---

### Task 3: R4 — total time budget for the decision-webhook chain

**Files:**
- Modify: `crates/opex-core/src/config/mod.rs:954-964` (`HooksConfig` + `default_webhook_chain_timeout_ms`)
- Modify: `crates/opex-core/src/agent/hooks.rs:33-40` (registry fields), `:79-113` (`set_webhooks`), `:120-206` (`fire_decision`)
- Modify: `crates/opex-core/src/gateway/handlers/agents/lifecycle.rs:150` (`set_webhooks` call)
- Test: `crates/opex-core/src/agent/hooks.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `crate::config::FailureMode` (existing enum, `Open` default / `Closed`).
- Produces: `HooksConfig.total_webhook_timeout_ms: u64` + `HooksConfig.on_chain_timeout: FailureMode`; `HookRegistry::set_webhooks(client, webhooks, total_webhook_timeout_ms, on_chain_timeout)`; a pure helper `fn webhook_chain_exceeded(elapsed: std::time::Duration, budget_ms: u64) -> bool`.

- [ ] **Step 1: Write the failing budget-helper test.** Add to `hooks.rs` tests:

```rust
#[cfg(test)]
mod chain_budget_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn budget_not_exceeded_below_limit() {
        assert!(!webhook_chain_exceeded(Duration::from_millis(9_000), 10_000));
    }
    #[test]
    fn budget_exceeded_at_or_above_limit() {
        assert!(webhook_chain_exceeded(Duration::from_millis(10_000), 10_000));
        assert!(webhook_chain_exceeded(Duration::from_millis(12_500), 10_000));
    }
    #[test]
    fn zero_budget_means_no_limit() {
        // 0 disables the chain budget entirely
        assert!(!webhook_chain_exceeded(Duration::from_secs(3_600), 0));
    }
}
```

- [ ] **Step 2: Run it to confirm it fails.**

Run: `cargo test -p opex-core chain_budget_tests 2>&1 | tail -6`
Expected: FAIL to compile — `webhook_chain_exceeded` not found.

- [ ] **Step 3: Implement the pure helper.** Add to `hooks.rs` (module level):

```rust
/// True when the cumulative decision-webhook chain has spent its budget.
/// A `budget_ms` of 0 disables the limit.
fn webhook_chain_exceeded(elapsed: std::time::Duration, budget_ms: u64) -> bool {
    budget_ms != 0 && (elapsed.as_millis() as u64) >= budget_ms
}
```

- [ ] **Step 4: Run the helper test to green.**

Run: `cargo test -p opex-core chain_budget_tests 2>&1 | tail -6`
Expected: PASS (3 tests).

- [ ] **Step 5: Add config fields.** In `config/mod.rs` `HooksConfig` (after `webhooks`):

```rust
    /// Total wall-clock budget across the whole decision-webhook chain per tool call
    /// (ms). 0 = no chain budget. Individual hooks keep their own `timeout_ms`.
    #[serde(default = "default_webhook_chain_timeout_ms")]
    pub total_webhook_timeout_ms: u64,
    /// What to do when the chain budget is exceeded. Default: Open (tool proceeds).
    #[serde(default)]
    pub on_chain_timeout: FailureMode,
```

And the default fn (next to `default_hook_timeout_ms`):

```rust
fn default_webhook_chain_timeout_ms() -> u64 {
    10_000
}
```

- [ ] **Step 6: Thread the budget into `HookRegistry`.** In `hooks.rs`, add fields to the struct:

```rust
    total_webhook_timeout_ms: u64,
    on_chain_timeout: crate::config::FailureMode,
```

Initialize them in `HookRegistry::new()` (`0` and `FailureMode::Open`). Extend `set_webhooks` signature + body:

```rust
    pub fn set_webhooks(
        &mut self,
        client: reqwest::Client,
        webhooks: Vec<crate::config::WebhookConfig>,
        total_webhook_timeout_ms: u64,
        on_chain_timeout: crate::config::FailureMode,
    ) {
        self.total_webhook_timeout_ms = total_webhook_timeout_ms;
        self.on_chain_timeout = on_chain_timeout;
        // ... existing body unchanged ...
    }
```

Update the production call site `gateway/handlers/agents/lifecycle.rs:150` to pass `hc.total_webhook_timeout_ms, hc.on_chain_timeout` (where `hc` is the agent's `HooksConfig`).

- [ ] **Step 7: Enforce the budget in `fire_decision`.** In `hooks.rs:120`, stamp the start before the loop and check before each POST:

```rust
    pub async fn fire_decision(&self, event: &HookEvent, extra: serde_json::Value) -> HookDecision {
        let chain_start = std::time::Instant::now();
        // ... existing setup (ev_name, tool, cur_extra, accumulators) ...
        for cw in self.webhooks.iter().filter(/* ... */) {
            // ... existing event/tool_matcher filters + client select ...
            if webhook_chain_exceeded(chain_start.elapsed(), self.total_webhook_timeout_ms) {
                tracing::warn!(budget_ms = self.total_webhook_timeout_ms, "decision webhook chain budget exceeded");
                match self.on_chain_timeout {
                    crate::config::FailureMode::Open => break, // fall through to the accumulation tail
                    crate::config::FailureMode::Closed => {
                        return HookDecision::Block("webhook chain budget exceeded".into());
                    }
                }
            }
            // ... existing POST + parse_decision ...
        }
        // ... existing tail: modified_args / transformed / injected / Continue ...
    }
```

(Note: `Instant::now()` is fine in production code; only the workflow-script sandbox forbids it.)

- [ ] **Step 8: Compile + tests.**

Run: `cargo check -p opex-core --all-targets && cargo test -p opex-core chain_budget_tests 2>&1 | tail -6`
Expected: compiles; PASS. (Absent config → `total_webhook_timeout_ms` defaults 10_000; `set_webhooks` callers pass it; existing hook behavior otherwise unchanged.)

- [ ] **Step 9: Commit.**

```bash
git add crates/opex-core/src/config/mod.rs crates/opex-core/src/agent/hooks.rs crates/opex-core/src/gateway/handlers/agents/lifecycle.rs
git commit -m "feat(agent): total time budget for the decision-webhook chain (on_chain_timeout, default open)"
```

---

### Task 4: R3 — LoopDetector warm-up restores repeat-hash detection

**Files:**
- Modify: `crates/opex-db/src/session_timeline.rs:136-139` (`TimelineToolEvent.args_hash`), `:143-167` (reader)
- Modify: `crates/opex-core/src/agent/tool_loop.rs:110-144` (replay method + warm-up)
- Modify: `crates/opex-core/src/agent/pipeline/parallel.rs:331-346` (`start_payload`/`end_payload` use `loop_detector_key`), sequential end-writer
- Test: `crates/opex-core/src/agent/tool_loop.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `loop_detector_key(tc)` (existing free fn, `parallel.rs:92-102`); `LoopDetector::hash_call_raw(name, args) -> u64` (existing).
- Produces: `TimelineToolEvent { tool_name, success, args_hash: Option<String> }` (hex string); `LoopDetector::warm_up_from_timeline` that restores `last_hash`/`consecutive`/`recent`.

- [ ] **Step 1: Write the failing warm-up hash test.** Add to `tool_loop.rs` tests:

```rust
    #[test]
    fn warm_up_from_timeline_restores_hash_repeat_detection() {
        let cfg = config(3); // break_threshold = 3
        let args = serde_json::json!({"q": "x"});
        // Live path keys on loop_detector_key; here the direct tool name == its key.
        let h = format!("{:x}", LoopDetector::hash_call_raw("web_search", &args));
        // Three identical successful calls already happened before the crash.
        let events = vec![
            TimelineToolEvent { tool_name: "web_search".into(), success: true, args_hash: Some(h.clone()) },
            TimelineToolEvent { tool_name: "web_search".into(), success: true, args_hash: Some(h.clone()) },
            TimelineToolEvent { tool_name: "web_search".into(), success: true, args_hash: Some(h.clone()) },
        ];
        let detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        // The next identical call must break NOW (consecutive already 3 >= threshold).
        assert!(
            matches!(detector.check_limits("web_search", &args), LoopStatus::Break(_)),
            "hash-repeat detection must survive warm-up (today it does NOT)"
        );
    }

    #[test]
    fn warm_up_tolerates_legacy_events_without_args_hash() {
        let cfg = config(3);
        let events = vec![
            TimelineToolEvent { tool_name: "fs".into(), success: false, args_hash: None },
            TimelineToolEvent { tool_name: "fs".into(), success: false, args_hash: None },
        ];
        let mut detector = LoopDetector::warm_up_from_timeline(&cfg, &events);
        // Error streak still restored (2 + 1 = trip at 3); no panic on missing hash.
        assert!(matches!(detector.record_result("fs", false), LoopStatus::Break(_)));
    }
```

Also update the THREE existing struct literals in this test module (`{ tool_name, success }` at ~:233-234 and ~:259-261) to add `args_hash: None`.

- [ ] **Step 2: Run to confirm failure.**

Run: `cargo test -p opex-core tool_loop 2>&1 | tail -12`
Expected: FAIL to compile — `TimelineToolEvent` has no `args_hash` field.

- [ ] **Step 3: Add `args_hash` to the timeline event + reader.** In `opex-db/src/session_timeline.rs`:

```rust
pub struct TimelineToolEvent {
    pub tool_name: String,
    pub success: bool,
    /// Hex of the loop hash (over loop_detector_key + args); None for legacy tool_end rows.
    pub args_hash: Option<String>,
}
```

Extend `load_tool_events` to select it:

```rust
    let rows = sqlx::query_as::<_, (String, Option<bool>, Option<String>)>(
        r#"
        SELECT
            payload->>'tool_name' AS tool_name,
            (payload->>'success')::bool AS success,
            payload->>'args_hash' AS args_hash
        FROM session_timeline
        WHERE session_id = $1 AND event_type = 'tool_end' AND payload->>'tool_name' IS NOT NULL
        ORDER BY created_at ASC
        "#,
    )
    // ...
    Ok(rows.into_iter().map(|(name, success, args_hash)| TimelineToolEvent {
        tool_name: name, success: success.unwrap_or(true), args_hash,
    }).collect())
```

- [ ] **Step 4: Restore hash state in warm-up.** In `tool_loop.rs`, replace `record_result_from_timeline` + `warm_up_from_timeline`:

```rust
    /// Replay a timeline event: restore hash-repeat state (when the row carries an
    /// args_hash) AND the error streak. Mirrors record_execution's consecutive logic.
    fn replay_from_timeline(&mut self, tool_name: &str, args_hash: Option<u64>, success: bool) {
        if let Some(hash) = args_hash {
            if self.last_hash == Some(hash) {
                self.consecutive += 1;
            } else {
                self.consecutive = 1;
                self.last_hash = Some(hash);
            }
            if self.recent.len() >= 64 { self.recent.pop_front(); self.recent_names.pop_front(); }
            self.recent.push_back(hash);
            self.recent_names.push_back(tool_name.to_string());
        }
        let _ = self.record_result(tool_name, success);
    }

    pub fn warm_up_from_timeline(config: &ToolLoopConfig, events: &[opex_db::session_timeline::TimelineToolEvent]) -> Self {
        let mut detector = Self::new(config);
        for e in events {
            let hash = e.args_hash.as_deref().and_then(|h| u64::from_str_radix(h, 16).ok());
            detector.replay_from_timeline(&e.tool_name, hash, e.success);
        }
        detector
    }
```

Delete the old `record_result_from_timeline` (replaced by `replay_from_timeline`).

- [ ] **Step 5: Run the warm-up tests to green.**

Run: `cargo test -p opex-core tool_loop 2>&1 | tail -8`
Expected: PASS (the new + existing warm-up tests).

- [ ] **Step 6: Persist the hash on tool_end (keyed on `loop_detector_key`).** In `parallel.rs`, change `end_payload` (`:338-346`) and the sequential-path end writer to include `args_hash`, and FIX `start_payload` (`:331-337`) to use the same key source:

```rust
    let start_payload = |tc: &ToolCall| -> Value {
        serde_json::json!({
            "tool_call_id": tc.id,
            "tool_name": tc.name,
            "args_hash": format!("{:x}", LoopDetector::hash_call_raw(&loop_detector_key(tc), &tc.arguments)),
        })
    };
    let end_payload = |tc: &ToolCall, res: &str| -> Value {
        let success = !res.to_lowercase().contains("error") && !res.to_lowercase().contains("failed");
        serde_json::json!({
            "tool_call_id": tc.id,
            "tool_name": tc.name,
            "success": success,
            "args_hash": format!("{:x}", LoopDetector::hash_call_raw(&loop_detector_key(tc), &tc.arguments)),
        })
    };
```

- [ ] **Step 7: Compile + full lib + timeline crate tests.**

Run: `cargo check -p opex-core --all-targets && cargo test -p opex-core tool_loop 2>&1 | tail -5 && cargo test -p opex-db session_timeline 2>&1 | tail -5`
Expected: all PASS.

- [ ] **Step 8: Commit.**

```bash
git add crates/opex-db/src/session_timeline.rs crates/opex-core/src/agent/tool_loop.rs crates/opex-core/src/agent/pipeline/parallel.rs
git commit -m "fix(agent): LoopDetector warm-up restores hash-repeat detection across restart (keyed on loop_detector_key)"
```

---

### Task 5: R2a — per-(agent,tool) penalty schema + record path

**Files:**
- Create: `migrations/070_tool_quality_per_agent.sql`
- Modify: `crates/opex-core/src/db/tool_quality.rs:73-159` (`record_tool_result` gains `agent_name`)
- Modify: `crates/opex-core/src/db/audit_queue.rs:22-27` (event variant) + `:80-96` (worker arm)
- Modify: `crates/opex-core/src/agent/engine_dispatch.rs:84-91` (pass agent name)
- Test: `crates/opex-core/src/db/tool_quality.rs` (`#[sqlx::test]`)

**Interfaces:**
- Consumes: nothing.
- Produces: `tool_quality` PK `(agent_name, tool_name)`; `record_tool_result(db, agent_name, tool_name, success, duration_ms, error)`; `AuditEvent::ToolQuality { agent_name, tool_name, success, duration_ms, error }`.

- [ ] **Step 1: Write the migration.** Create `migrations/070_tool_quality_per_agent.sql`:

```sql
-- Penalty is transient quality data (rolling 20-call window) that self-heals,
-- so we start fresh rather than backfill an ambiguous agent for existing rows.
DELETE FROM tool_quality;
ALTER TABLE tool_quality ADD COLUMN agent_name TEXT NOT NULL DEFAULT '';
ALTER TABLE tool_quality DROP CONSTRAINT tool_quality_pkey;
ALTER TABLE tool_quality ADD PRIMARY KEY (agent_name, tool_name);
```

- [ ] **Step 2: Write the failing per-agent DB test.** Add to `tool_quality.rs`:

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn penalty_is_scoped_per_agent(pool: sqlx::PgPool) {
        // Tool "T" fails repeatedly under agent A, succeeds under agent B.
        for _ in 0..5 { record_tool_result(&pool, "A", "T", false, 10, Some("boom")).await.unwrap(); }
        for _ in 0..5 { record_tool_result(&pool, "B", "T", true, 10, None).await.unwrap(); }

        let all = get_all_penalties(&pool).await.unwrap();
        assert!(all[&("A".to_string(), "T".to_string())] < 0.8, "A's T is penalized");
        assert!((all[&("B".to_string(), "T".to_string())] - 1.0).abs() < f32::EPSILON, "B's T is clean");
    }
```

- [ ] **Step 3: Run to confirm failure.**

Run: `make test-db 2>&1 | grep -A3 penalty_is_scoped_per_agent | tail -6`
Expected: FAIL to compile — `record_tool_result` arity / `get_all_penalties` key type mismatch.

- [ ] **Step 4: Add `agent_name` to `record_tool_result` + composite UPSERT.** In `tool_quality.rs`, change the signature and SQL:

```rust
pub async fn record_tool_result(
    db: &PgPool,
    agent_name: &str,
    tool_name: &str,
    success: bool,
    duration_ms: i32,
    error: Option<&str>,
) -> Result<()> {
    // INSERT ... (agent_name, tool_name, ...) VALUES ($1, $2, ...)
    // ON CONFLICT (agent_name, tool_name) DO UPDATE SET ...  (bind $1=agent_name, $2=tool_name, shift the rest)
```

Update the `INSERT INTO tool_quality (agent_name, tool_name, ...)` column list, the `VALUES ($1, $2, ...)` (agent_name first), and `ON CONFLICT (agent_name, tool_name)`. Change `get_all_penalties` to return `HashMap<(String, String), f32>` selecting `agent_name, tool_name, penalty_score`.

- [ ] **Step 5: Thread `agent_name` through the audit event.** In `db/audit_queue.rs`, add to the `ToolQuality` variant (`:22-27`):

```rust
    ToolQuality { agent_name: String, tool_name: String, success: bool, duration_ms: i32, error: Option<String> },
```

In the worker match-arm (`:80-96`) forward it: `record_tool_result(db, &agent_name, &tool_name, success, duration_ms, error.as_deref()).await`.

In `engine_dispatch.rs:84-91`, add `agent_name: self.cfg().agent.name.clone()` to the emitted `AuditEvent::ToolQuality { ... }`.

- [ ] **Step 6: Run the DB test to green.**

Run: `make test-db 2>&1 | grep -A3 penalty_is_scoped_per_agent | tail -6`
Expected: PASS.

- [ ] **Step 7: Commit.**

```bash
git add migrations/070_tool_quality_per_agent.sql crates/opex-core/src/db/tool_quality.rs crates/opex-core/src/db/audit_queue.rs crates/opex-core/src/agent/engine_dispatch.rs
git commit -m "feat(db): scope tool_quality penalty per (agent, tool) — migration 070 + record path"
```

---

### Task 6: R2b — per-agent PenaltyCache + consumer + doctor

**Files:**
- Modify: `crates/opex-core/src/db/tool_quality.rs:15-61` (`PenaltyCache` nested), `:179-205` (`get_degraded_tools`)
- Modify: `crates/opex-core/src/agent/engine/context_builder.rs:394` (`tool_penalties` passes agent)
- Modify: `crates/opex-core/src/gateway/handlers/monitoring/doctor.rs:471` (payload `agent_name`)
- Test: `crates/opex-core/src/db/tool_quality.rs` (`#[sqlx::test]`)

**Interfaces:**
- Consumes: `record_tool_result` (Task 5); `get_all_penalties() -> HashMap<(String,String), f32>` (Task 5).
- Produces: `PenaltyCache::get_penalties(&self, agent_name: &str) -> HashMap<String, f32>` (that agent's tool→penalty submap).

- [ ] **Step 1: Write the failing cache test.** Add to `tool_quality.rs`:

```rust
    #[sqlx::test(migrations = "../../migrations")]
    async fn penalty_cache_returns_per_agent_submap(pool: sqlx::PgPool) {
        for _ in 0..5 { record_tool_result(&pool, "A", "T", false, 10, Some("x")).await.unwrap(); }
        let cache = PenaltyCache::new(pool.clone());
        let a = cache.get_penalties("A").await;
        let b = cache.get_penalties("B").await;
        assert!(a.get("T").copied().unwrap_or(1.0) < 0.8, "A sees its penalty");
        assert!(b.get("T").is_none(), "B (unseen) gets an empty submap");
    }
```

- [ ] **Step 2: Run to confirm failure.**

Run: `make test-db 2>&1 | grep -A3 penalty_cache_returns_per_agent | tail -6`
Expected: FAIL to compile — `get_penalties` takes no arg / cache map shape.

- [ ] **Step 3: Make `PenaltyCache` per-agent.** In `tool_quality.rs`, change the cached type to `HashMap<String /*agent*/, HashMap<String /*tool*/, f32>>`, fill it in the refresh from `get_all_penalties()` (group the `(agent,tool)` rows by agent), and change the accessor:

```rust
    pub async fn get_penalties(&self, agent_name: &str) -> HashMap<String, f32> {
        // ...30s-refresh logic unchanged, then:
        guard.0.get(agent_name).cloned().unwrap_or_default()
    }
```

- [ ] **Step 4: Update the single consumer.** In `engine/context_builder.rs`, `tool_penalties()` (call at `:394`) passes the agent name:

```rust
    self.tex().penalty_cache.get_penalties(&self.cfg().agent.name).await
```

(Return type stays `HashMap<String, f32>` — downstream at `context_builder.rs:541` is unaffected.)

- [ ] **Step 5: Update `get_degraded_tools` + doctor payload (global, agent as a column).** In `tool_quality.rs:179`, add `agent_name` to the SELECT and the returned JSON:

```rust
    // SELECT agent_name, tool_name, penalty_score, total_calls, fail_calls, last_error
    //   FROM tool_quality WHERE penalty_score < 0.8 ORDER BY penalty_score ASC
    json!({ "agent_name": agent, "tool_name": name, "penalty_score": penalty, "total_calls": total, "fail_calls": fail, "last_error": last_error })
```

`doctor.rs:471` calls `get_degraded_tools(&db)` unchanged (no per-agent filter — the payload now carries `agent_name` per row).

- [ ] **Step 6: Run the cache test + full DB suite to green.**

Run: `make test-db 2>&1 | tail -6`
Expected: PASS (both R2 tests + no regressions).

- [ ] **Step 7: Full gate.**

Run: `make check && make lint && make test-db 2>&1 | tail -4`
Expected: check clean, clippy 0 warnings, all tests PASS.

- [ ] **Step 8: Commit.**

```bash
git add crates/opex-core/src/db/tool_quality.rs crates/opex-core/src/agent/engine/context_builder.rs crates/opex-core/src/gateway/handlers/monitoring/doctor.rs
git commit -m "feat(db): per-agent PenaltyCache + degraded-tools reporting; wire tool_penalties consumer"
```

---

## Final: whole-branch review + deploy

- [ ] Dispatch a whole-branch reviewer (opus) over the full range (base → HEAD): confirm the R3 hash uses `loop_detector_key` at BOTH write (start/end payload) and replay; the R4 budget reaches `fire_decision` via the registry; R5 empty-config parity; migration 070 applies cleanly; no unit regressed another (shared `parallel.rs` surface).
- [ ] Full gate green: `make check` + `make lint` + `make test-db`.
- [ ] Single deploy: `make remote-deploy` (git pull → cargo build → atomic swap → restart on the server). Verify `make doctor` after.
