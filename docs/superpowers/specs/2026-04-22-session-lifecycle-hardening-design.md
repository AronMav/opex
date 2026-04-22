# Session Lifecycle Hardening — Design Spec

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Three targeted fixes to session lifecycle integrity: WAL warm-up for LoopDetector after crash/resume (BUG-026), SessionStatus FSM enum with transition validation, and SIGHUP atomic-remove to eliminate the double-read-lock race.

**Architecture:** Additive changes only — new `session_status.rs` in `hydeclaw-db`, a new `LoopDetector::warm_up_from_wal` constructor, one new field on `BootstrapOutcome`, and a refactored SIGHUP handler. No existing public APIs change.

**Tech Stack:** Rust, sqlx/PostgreSQL, tokio, `hydeclaw-db`, `hydeclaw-core`

---

## Background

### W: WAL warm-up (BUG-026)

`bootstrap.rs` always creates `LoopDetector::new()` — fresh state on every session entry, including re-entry after crash. If a session was looping before crash (say 8/10 consecutive identical calls), the detector restarts from zero and gives the agent a full `break_threshold` of new chances.

The infrastructure is already scaffolded:

- `session_wal.rs::load_tool_events(db, session_id)` — queries `tool_end` WAL events ordered by time
- `LoopDetector::record_result_from_wal(tool_name, success)` — replays error-streak into detector
- `execute.rs` line 130: `for iteration in 0..loop_config.effective_max_iterations()` — iteration range is adjustable

Missing: a `warm_up_from_wal` constructor that calls `record_result_from_wal` for all prior events, and wiring in `bootstrap.rs`.

**Limitation:** WAL stores `tool_name` + `success` but not args. Hash-based consecutive detection (`consecutive`/`last_hash`) cannot be restored. Error-streak (`consecutive_errors`) and total iteration count ARE restored. This is acceptable — error-streak is the primary crash-loop trigger in practice.

### F: FSM validation

`set_session_run_status` guards only against `→ done` (via `IS DISTINCT FROM 'done'`). Transitions like `failed → done` and `cancelled → failed` are accepted at the DB level. At the Rust level there is no enum — status is passed as `&str` everywhere, making invalid transitions invisible to the type system and hard to catch in tests.

### S: SIGHUP double-lock

The SIGHUP handler acquires two separate `read()` locks for cancel (step 1) and drain (step 2). Between them a new request can arrive on the old engine, begin executing, and then get immediately cancelled — harmless in practice, but architecturally two independent operations on shared state.

---

## Fix Inventory

| ID | File(s) | Description |
| -- | ------- | ----------- |
| W1 | `tool_loop.rs` | Add `warm_up_from_wal(config, events) -> (Self, usize)` |
| W2 | `bootstrap.rs` | Query WAL events + call `warm_up_from_wal` instead of `LoopDetector::new` |
| W3 | `bootstrap.rs` | Add `warm_iterations: usize` to `BootstrapOutcome` |
| W4 | `execute.rs` | Destructure `warm_iterations`, start loop at `warm_iterations` |
| F1 | `hydeclaw-db/src/session_status.rs` | New file: `SessionStatus` enum + `can_transition_to` |
| F2 | `hydeclaw-db/src/lib.rs` | Export `pub use session_status::SessionStatus` |
| F3 | `hydeclaw-db/src/sessions.rs` | Tighten `set_session_run_status` SQL + add `validate_transition` call in Rust |
| F4 | `hydeclaw-core` finalize / callers | Use `SessionStatus` enum at call sites in finalize |
| S1 | `main.rs` | Refactor SIGHUP handler: write→remove → drain → create → write→insert |

---

## Detailed Design

### W1 — `LoopDetector::warm_up_from_wal`

**File:** `crates/hydeclaw-core/src/agent/tool_loop.rs`

Add after `LoopDetector::new`:

```rust
/// Reconstruct detector state from WAL tool_end events after crash/resume (BUG-026).
///
/// Replays error-streak (consecutive_errors + last_error_tool) from WAL history.
/// Hash-based consecutive detection (consecutive/last_hash) is NOT restored —
/// WAL does not store args. Returns (detector, event_count) where event_count
/// is used as the warm iteration offset in execute.rs.
pub fn warm_up_from_wal(config: &ToolLoopConfig, events: &[hydeclaw_db::session_wal::WalToolEvent]) -> (Self, usize) {
    let mut detector = Self::new(config);
    for e in events {
        detector.record_result_from_wal(&e.tool_name, e.success);
    }
    (detector, events.len())
}
```

### W2 + W3 — `bootstrap.rs` wiring

**File:** `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs`

Replace the `LoopDetector::new` block (current lines ~188-190):

```rust
// 7. LoopDetector: warm-up from WAL if session has prior tool history (BUG-026).
//    Restores error-streak state so a looping agent cannot get a free
//    break_threshold reset after crash/resume.
let loop_config = engine.tool_loop_config();
let wal_events = hydeclaw_db::session_wal::load_tool_events(&engine.cfg().db, session_id)
    .await
    .unwrap_or_default();
let (loop_detector, warm_iterations) = LoopDetector::warm_up_from_wal(&loop_config, &wal_events);
if warm_iterations > 0 {
    tracing::debug!(session = %session_id, warm_iterations, "LoopDetector warmed from WAL");
}
```

Add `warm_iterations: usize` to `BootstrapOutcome`:

```rust
pub struct BootstrapOutcome {
    // ... existing fields ...
    pub warm_iterations: usize,
}
```

Update the `Ok(BootstrapOutcome { ... })` at the end to include `warm_iterations`.

### W4 — `execute.rs` iteration offset

**File:** `crates/hydeclaw-core/src/agent/pipeline/execute.rs`

The `BootstrapOutcome` destructuring already uses explicit field names. Add `warm_iterations` to the destructure:

```rust
let BootstrapOutcome {
    session_id,
    mut messages,
    tools,
    mut loop_detector,
    warm_iterations,
    // ... rest unchanged ...
} = bootstrap_outcome;
```

Change the turn loop start:

```rust
// Before:
for iteration in 0..loop_config.effective_max_iterations() {

// After:
for iteration in warm_iterations..loop_config.effective_max_iterations() {
```

This correctly limits remaining iterations: a session with 45/50 prior iterations gets only 5 more.

---

### F1 — `session_status.rs`

**File:** `crates/hydeclaw-db/src/session_status.rs` (new)

```rust
/// Session lifecycle FSM. Enforces valid state transitions at the Rust level.
/// The SQL layer (`set_session_run_status`) provides the hard DB-level guard;
/// this enum provides early detection of logic errors in tests and code review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Done,
    Failed,
    Interrupted,
    Timeout,
    Cancelled,
}

impl SessionStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }

    /// FSM transition rules:
    /// - `done → anything`: false (done is the only immutable terminal)
    /// - `running → anything`: true
    /// - `soft-terminal → running`: true (session re-entry after interrupted/failed/etc.)
    /// - `soft-terminal → soft-terminal`: false (cannot jump between terminal states)
    pub fn can_transition_to(self, to: Self) -> bool {
        match self {
            Self::Done => false,
            Self::Running => true,
            _ => to == Self::Running,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "interrupted" => Some(Self::Interrupted),
            "timeout" => Some(Self::Timeout),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}
```

### F2 — `hydeclaw-db/src/lib.rs`

Add to the existing `pub mod` declarations and re-exports:

```rust
pub mod session_status;
pub use session_status::SessionStatus;
```

### F3 — `sessions.rs` SQL tightening

**File:** `crates/hydeclaw-db/src/sessions.rs`

`set_session_run_status` currently allows `soft-terminal → soft-terminal` (e.g. `failed → done`). Tighten to block all terminal → terminal transitions at SQL level, keeping only `→ running` and `running → terminal` paths:

```sql
-- Before:
WHERE id = $2 AND run_status IS DISTINCT FROM 'done'

-- After: block any transition from a terminal state to another terminal state.
-- Only running → terminal and any → running (via claim_session_running) are valid.
WHERE id = $2
  AND (
    run_status = 'running'                   -- normal finalize path
    OR run_status IS NULL                    -- new session, first status write
  )
```

`claim_session_running` keeps its `IS DISTINCT FROM 'done'` guard — it needs to allow re-entry from soft-terminal states (`failed → running`, `interrupted → running`).

**Edge case:** `cleanup_interrupted_sessions` queries sessions where `run_status = 'running' OR EXISTS(streaming messages)`. A session already marked `'interrupted'` with stale streaming messages will match the EXISTS clause, then `set_session_run_status(id, "interrupted")` will be a silent no-op under the new WHERE (not 'running', not NULL). This is correct — the streaming message repair (step 2) runs independently and already fixed the messages. The status write no-op is harmless.

`claim_session_running` stays as `IS DISTINCT FROM 'done'` — it needs to allow re-entry from soft-terminal states (user reopens a failed/interrupted session).

Add a Rust-level validation helper in `sessions.rs` for use by callers:

```rust
/// Log a warning if the requested transition violates the session FSM.
/// Does NOT abort — the SQL guard is the hard barrier. This is an early
/// diagnostic for test failures and log analysis.
pub fn warn_invalid_transition(from: Option<SessionStatus>, to: SessionStatus, session_id: Uuid) {
    if let Some(f) = from {
        if !f.can_transition_to(to) {
            tracing::warn!(
                from = f.as_str(), to = to.as_str(), %session_id,
                "session FSM violation: invalid status transition"
            );
        }
    }
}
```

### F4 — Call sites in finalize

**File:** `crates/hydeclaw-core/src/agent/pipeline/finalize.rs`

Where `set_session_run_status` is called with a known target status, call `warn_invalid_transition` first. The current status of the session is in the `SessionLifecycleGuard` outcome field (`SessionOutcome` enum). Map `SessionOutcome` → `SessionStatus` for the `to` argument, pass `None` for `from` (we don't query current DB status — the SQL guard handles correctness).

This is a logging-only addition, not a correctness change.

---

### S1 — SIGHUP atomic-remove

**File:** `crates/hydeclaw-core/src/main.rs` — `setup_sighup_handler`

Replace the two-read-lock pattern with atomic-remove → drain → create → insert:

```rust
for cfg in configs {
    let name = cfg.agent.name.clone();

    // 1. Atomically remove from map. During drain, the agent is absent from
    //    the map — new requests receive "agent not found" (acceptable for the
    //    drain window, typically < 1s for idle agents, up to 10s worst-case).
    let old_handle = state.agents.map.write().await.remove(&name);

    // 2. Cancel + drain on the extracted handle (write lock already released).
    //    No race: old engine is no longer in the map, so no new request can
    //    reach it between cancel and drain completion.
    if let Some(ref h) = old_handle {
        tracing::info!(agent = %name, "SIGHUP: cancelling and draining old agent");
        h.engine.state.cancel_all_requests();
        h.engine.state.wait_drain(std::time::Duration::from_secs(10)).await;
    }

    // 3. Create new engine, insert into map, then shut down old engine cleanly.
    match crate::gateway::start_agent_from_config(
        &cfg,
        &state.agents,
        &state.infra,
        &state.auth,
        &state.channels,
        &state.config,
        &state.status,
    ).await {
        Ok((new_handle, guard)) => {
            if let Some(old) = old_handle {
                old.shutdown(&state.agents.scheduler).await;
            }
            state.agents.map.write().await.insert(name.clone(), new_handle);
            if let Some(g) = guard {
                state.auth.access_guards.write().await.insert(name.clone(), g);
            }
            tracing::info!(agent = %name, "SIGHUP: agent reloaded");
        }
        Err(e) => {
            tracing::error!(agent = %name, error = %e, "SIGHUP: failed to create new engine");
            // Restore old handle if new engine failed to start, so the agent
            // is not permanently absent from the map.
            if let Some(old) = old_handle {
                state.agents.map.write().await.insert(name.clone(), old);
                tracing::warn!(agent = %name, "SIGHUP: restored old engine after create failure");
            }
        }
    }
}
```

Note the error path: if `start_agent_from_config` fails, the old handle is re-inserted into the map so the agent isn't permanently absent. This is a robustness improvement over the current code which leaves the agent unreachable on failure.

---

## Testing

### W: WAL warm-up

- Unit test: `LoopDetector::warm_up_from_wal` with a sequence of tool events — verify `consecutive_errors` is restored correctly
- Integration test (sqlx::test): simulate a session with 3 consecutive failed tool calls → crash → re-entry → verify LoopDetector fires on 4th failure, not requiring `error_break_threshold` fresh failures
- Integration test: verify `warm_iterations` offsets the turn counter — session with 45 prior events gets only 5 more iterations

### F: FSM

- Unit tests for `SessionStatus::can_transition_to` — all 6×6 = 36 transition pairs
- Integration test: call `set_session_run_status(db, id, "done")` on a failed session → verify rows_affected = 0 (SQL guard blocks it)

### S: SIGHUP

- Unit test on `setup_sighup_handler` is not straightforward (signal-based). Instead: extract the per-agent reload logic into a `reload_agent(state, cfg) -> Result<()>` function and test it directly with a mock AgentMap
- Test: verify that on `start_agent_from_config` failure, the old handle is restored to the map

---

## File Summary

| File | Action | Change |
| ---- | ------ | ------ |
| `crates/hydeclaw-core/src/agent/tool_loop.rs` | Modify | Add `warm_up_from_wal` constructor |
| `crates/hydeclaw-core/src/agent/pipeline/bootstrap.rs` | Modify | WAL query + warm_up_from_wal + `warm_iterations` field |
| `crates/hydeclaw-core/src/agent/pipeline/execute.rs` | Modify | Destructure `warm_iterations`, start loop at offset |
| `crates/hydeclaw-db/src/session_status.rs` | Create | `SessionStatus` enum + FSM methods |
| `crates/hydeclaw-db/src/lib.rs` | Modify | `pub mod session_status; pub use session_status::SessionStatus;` |
| `crates/hydeclaw-db/src/sessions.rs` | Modify | Tighten `set_session_run_status` SQL + `warn_invalid_transition` |
| `crates/hydeclaw-core/src/agent/pipeline/finalize.rs` | Modify | Call `warn_invalid_transition` before status writes |
| `crates/hydeclaw-core/src/main.rs` | Modify | Refactor SIGHUP handler to atomic-remove pattern with error recovery |
