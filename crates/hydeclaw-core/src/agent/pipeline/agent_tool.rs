//! Pipeline step: agent_tool — session agent pool operations (migrated from engine_agent_tool.rs).
//!
//! Each function takes explicit dependencies instead of `&self` on `AgentEngine`.

use std::time::Duration;

use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::session_agent_pool::{self, SessionAgentPool, SessionPoolsMap};
use crate::config::AgentToolConfig;
use crate::gateway::state::AgentMap;

// ── Constants ────────────────────────────────────────────────────────────────

/// Polling cadence used by `wait_until_idle` and `wait_for_agent_result`.
/// Both loops re-acquire the pool read lock once per tick to observe agent
/// status; 1s is a balance between responsiveness and lock churn.
const MESSAGE_POLL_INTERVAL: Duration = Duration::from_millis(1000);

/// Per-call snapshot of UI-configurable timeouts — read once at dispatch time
/// from `AgentToolConfig` and threaded through every helper. Plain `Copy` so
/// it costs nothing to pass around. Callers obtain it via `From<&AgentToolConfig>`.
#[derive(Debug, Clone, Copy)]
pub struct AgentToolTimeouts {
    pub message_wait_for_idle: Duration,
    pub message_result: Duration,
    #[allow(dead_code)] // consumed by parallel.rs, kept here for symmetry
    pub safety: Duration,
}

impl From<&AgentToolConfig> for AgentToolTimeouts {
    fn from(cfg: &AgentToolConfig) -> Self {
        Self {
            message_wait_for_idle: Duration::from_secs(cfg.message_wait_for_idle_secs),
            message_result: Duration::from_secs(cfg.message_result_secs),
            safety: Duration::from_secs(cfg.safety_timeout_secs),
        }
    }
}

#[cfg(test)]
impl AgentToolTimeouts {
    /// Test-only helper that mirrors the previous compile-time constants
    /// (60s / 300s / 600s).
    pub fn legacy_defaults() -> Self {
        Self {
            message_wait_for_idle: Duration::from_secs(60),
            message_result: Duration::from_secs(300),
            safety: Duration::from_secs(600),
        }
    }
}

/// Extract session_id from enriched `_context` (per-invocation, race-free).
pub fn extract_session_id(args: &serde_json::Value) -> Option<Uuid> {
    args.get("_context")
        .and_then(|ctx| ctx.get("session_id"))
        .and_then(|s| s.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// Dispatch `agent` tool calls to the appropriate sub-handler based on `action`.
///
/// Actions: `run`, `message`, `status`, `kill`, `collect`.
///
/// `timeouts` is a per-call snapshot read from the live `AppConfig.agent_tool`
/// section by the caller — see `engine_dispatch.rs`. Passing it explicitly
/// (rather than reading a global) keeps hot-reload deterministic: each
/// invocation observes the timeout values that were live when the tool was
/// dispatched.
pub async fn handle_agent_tool(
    session_pools: Option<&SessionPoolsMap>,
    agent_map: Option<&AgentMap>,
    db: &PgPool,
    agent_name: &str,
    args: &serde_json::Value,
    timeouts: AgentToolTimeouts,
) -> String {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match action {
        "run" => handle_agent_run(session_pools, agent_map, db, agent_name, args, timeouts).await,
        "message" => handle_agent_message(session_pools, args, timeouts).await,
        "status" => handle_agent_status(session_pools, args).await,
        "kill" => handle_agent_kill(session_pools, args).await,
        "collect" => handle_agent_collect(session_pools, args, timeouts).await,
        other => format!(
            "Error: unknown agent action '{}'. Expected: run, message, status, kill, collect",
            other
        ),
    }
}

/// `run` — spawn a new live agent and wait for its result (blocking by default).
/// With `mode: "async"`, returns immediately for parallel spawning (use `collect` to get results).
pub async fn handle_agent_run(
    session_pools: Option<&SessionPoolsMap>,
    agent_map: Option<&AgentMap>,
    db: &PgPool,
    agent_name: &str,
    args: &serde_json::Value,
    timeouts: AgentToolTimeouts,
) -> String {
    let target = match args.get("target").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return "Error: 'target' parameter is required".to_string(),
    };
    let task = match args.get("task").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return "Error: 'task' parameter is required".to_string(),
    };
    let is_async = args.get("mode").and_then(|v| v.as_str()) == Some("async");

    let session_id = match extract_session_id(args) {
        Some(id) if id != Uuid::nil() => id,
        _ => {
            return "Error: no active session — agent tool requires session context via _context"
                .to_string()
        }
    };

    let agent_map = match agent_map {
        Some(m) => m,
        None => return "Error: agent_map not available (subagent context)".to_string(),
    };
    let target_engine = {
        let map = agent_map.read().await;
        match map.get(target) {
            Some(handle) => handle.engine.clone(),
            None => return format!("Error: agent '{}' not found", target),
        }
    };

    let _ = crate::db::sessions::add_participant(db, session_id, target).await;

    let pools = match session_pools {
        Some(p) => p,
        None => return "Error: session_pools not available".to_string(),
    };

    // Check for duplicate and insert — all under one write lock to prevent TOCTOU race.
    {
        let mut pools_write = pools.write().await;
        let pool = pools_write
            .entry(session_id)
            .or_insert_with(|| SessionAgentPool::new(session_id));
        if pool.contains(target) {
            return format!(
                "Error: {} is already running in this session. Use agent(action: \"message\") to communicate.",
                target
            );
        }

        let live_agent = match session_agent_pool::spawn_live_agent(
            target.to_string(),
            target_engine,
            task.to_string(),
            session_id,
        ) {
            Some(la) => la,
            None => {
                return format!(
                    "Error: failed to deliver initial task to agent '{}'",
                    target
                )
            }
        };
        pool.insert(live_agent);
    } // write lock released before blocking wait

    tracing::info!(
        from = %agent_name,
        target = %target,
        mode = if is_async { "async" } else { "sync" },
        "agent tool: spawned"
    );

    if is_async {
        // Async mode: return immediately, caller uses `collect` later.
        return serde_json::json!({
            "status": "started",
            "agent": target,
            "message": format!(
                "Agent '{}' started. Use agent(action: \"collect\", target: \"{}\") to get the result.",
                target, target
            ),
        })
        .to_string();
    }

    // Sync mode (default): block until the agent completes or times out.
    // Uses the operator-configured `message_result` deadline.
    let result =
        wait_for_agent_result(pools, session_id, target, timeouts.message_result).await;

    // Clean up: remove the completed agent from the pool so a subsequent `run` with the
    // same target doesn't get the "already running" error.
    {
        let mut pools_write = pools.write().await;
        if let Some(pool) = pools_write.get_mut(&session_id) {
            pool.remove(target);
        }
    }

    result
}

/// Block until a live agent becomes idle and return its last_result.
pub async fn wait_for_agent_result(
    pools: &SessionPoolsMap,
    session_id: Uuid,
    target: &str,
    timeout: std::time::Duration,
) -> String {
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(MESSAGE_POLL_INTERVAL).await;

        // Check if agent is done
        let result = {
            let pools_read = pools.read().await;
            if let Some(pool) = pools_read.get(&session_id) {
                if let Some(agent) = pool.get(target) {
                    if agent.is_idle() {
                        let lr = agent.last_result.read().await.clone();
                        Some(lr)
                    } else if agent.task_handle.is_finished() {
                        // Task exited (crash/cancel) while still "processing"
                        let lr = agent.last_result.read().await.clone();
                        Some(lr)
                    } else {
                        None // still processing
                    }
                } else {
                    // Agent was removed (killed by someone else)
                    Some(Some(format!(
                        "Agent '{}' was killed before completing",
                        target
                    )))
                }
            } else {
                Some(Some(format!("Session pool not found for {}", target)))
            }
        };

        if let Some(last_result) = result {
            let result_text = match last_result {
                Some(s) if !s.trim().is_empty() => s,
                _ => "(no result)".to_string(),
            };
            return serde_json::json!({
                "status": "completed",
                "agent": target,
                "result": result_text,
            })
            .to_string();
        }

        if start.elapsed() > timeout {
            return serde_json::json!({
                "status": "timeout",
                "agent": target,
                "message": format!(
                    "Agent '{}' did not complete within {} seconds",
                    target,
                    timeout.as_secs()
                ),
            })
            .to_string();
        }
    }
}

/// Wait for a live agent to enter the IDLE state without consuming its
/// `last_result`. Used by `handle_agent_message` to ensure the target is ready
/// to receive a new message before we `try_send`.
///
/// Returns `Ok(())` once the agent is idle. Returns `Err(message)` on:
///   - missing session pool / agent (already gone),
///   - the agent's processing task having exited (crash / cancel),
///   - timeout.
async fn wait_until_idle(
    pools: &SessionPoolsMap,
    session_id: Uuid,
    target: &str,
    timeout: Duration,
) -> Result<(), String> {
    let start = std::time::Instant::now();
    loop {
        {
            let pools_read = pools.read().await;
            let pool = pools_read
                .get(&session_id)
                .ok_or_else(|| format!("Error: no agent pool for session {}", session_id))?;
            let agent = pool
                .get(target)
                .ok_or_else(|| format!("Error: agent '{}' not found in session pool", target))?;
            if agent.is_idle() {
                return Ok(());
            }
            if agent.task_handle.is_finished() {
                return Err(format!(
                    "Error: agent '{}' processing loop has exited",
                    target
                ));
            }
        }

        if start.elapsed() > timeout {
            return Err(format!(
                "Error: agent '{}' did not become idle within {} seconds — still processing previous message",
                target,
                timeout.as_secs()
            ));
        }

        tokio::time::sleep(MESSAGE_POLL_INTERVAL).await;
    }
}

/// `message` — send a follow-up message to an already-running live agent.
///
/// **Sync by default**: blocks until the target agent finishes processing the
/// message and returns its `last_result`. This eliminates the race where an
/// orchestrator sends messages while the target is still busy (which used to
/// produce "queue is full" errors or silent message loss).
///
/// Pass `wait_for_result: false` for fire-and-forget broadcasts. In that mode
/// the call still waits for the target to be idle before sending (so the
/// message is guaranteed to be delivered), then returns immediately without
/// waiting for the response.
pub async fn handle_agent_message(
    session_pools: Option<&SessionPoolsMap>,
    args: &serde_json::Value,
    timeouts: AgentToolTimeouts,
) -> String {
    let target = match args.get("target").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return "Error: 'target' parameter is required".to_string(),
    };
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return "Error: 'text' parameter is required".to_string(),
    };

    // Default true: sync send-and-wait. Only false explicitly opts out.
    let wait_for_result = args
        .get("wait_for_result")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let session_id = match extract_session_id(args) {
        Some(id) if id != Uuid::nil() => id,
        _ => return "Error: no active session — session_id missing from _context".to_string(),
    };

    let pools = match session_pools {
        Some(p) => p,
        None => return "Error: session_pools not available".to_string(),
    };

    // Verify target exists up-front (cheap, gives a clean error before the wait loop).
    {
        let pools_read = pools.read().await;
        let pool = match pools_read.get(&session_id) {
            Some(p) => p,
            None => return format!("Error: no agent pool for session {}", session_id),
        };
        if pool.get(target).is_none() {
            return format!("Error: agent '{}' not found in session pool", target);
        }
    }

    // Wait until the target is idle (or fail with a clear error).
    if let Err(e) =
        wait_until_idle(pools, session_id, target, timeouts.message_wait_for_idle).await
    {
        return e;
    }

    // Re-acquire the agent reference under a fresh read lock to perform the send.
    //
    // Concurrency note: between `wait_until_idle` returning and this point, another
    // sender may have observed IDLE in *its own* `wait_until_idle` and CAS'd the
    // agent into PROCESSING. We must therefore use compare-exchange (not blind
    // store) to mark PROCESSING, so we know whether *we* own the IDLE→PROCESSING
    // transition. Only the CAS winner is allowed to revert to IDLE on send failure.
    // If two senders race and both manage to enqueue, the agent's mpsc receiver
    // will process them sequentially — that is fine.
    let send_result = {
        let pools_read = pools.read().await;
        let pool = match pools_read.get(&session_id) {
            Some(p) => p,
            None => return format!("Error: no agent pool for session {}", session_id),
        };
        let agent = match pool.get(target) {
            Some(a) => a,
            None => return format!("Error: agent '{}' not found in session pool", target),
        };

        // CAS IDLE → PROCESSING. If another sender beat us to it, the agent is
        // already PROCESSING; we still try to deliver (try_send), but we MUST NOT
        // revert status on Full — only the CAS winner has that right.
        let we_won_cas = agent
            .status
            .compare_exchange(
                session_agent_pool::STATUS_IDLE,
                session_agent_pool::STATUS_PROCESSING,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok();

        let r = agent.message_tx.try_send(session_agent_pool::AgentMessage {
            text: text.to_string(),
        });
        // On Full: revert to IDLE *only if we owned the IDLE→PROCESSING transition*.
        // If CAS failed (someone else owns PROCESSING), reverting would clobber
        // their state and falsely report idle while they are still in flight.
        // On Closed: keep PROCESSING — wait_for_agent_result detects
        // task_handle.is_finished() and returns the error rather than "(no result)".
        if we_won_cas
            && matches!(&r, Err(tokio::sync::mpsc::error::TrySendError::Full(_)))
        {
            agent.status.store(
                session_agent_pool::STATUS_IDLE,
                std::sync::atomic::Ordering::Release,
            );
        }
        r
    };

    match send_result {
        Ok(()) => {
            if !wait_for_result {
                return serde_json::json!({
                    "status": "sent",
                    "agent": target,
                    "message": "Message sent (fire-and-forget)",
                })
                .to_string();
            }
            // Sync mode: wait for the result. Agent stays in the pool — only
            // `kill` removes it.
            wait_for_agent_result(pools, session_id, target, timeouts.message_result).await
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            // Defense-in-depth: should be unreachable now that we wait for idle.
            format!(
                "Error: agent '{}' message queue is full — it may still be processing",
                target
            )
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            format!("Error: agent '{}' processing loop has exited", target)
        }
    }
}

/// `status` — return status of a single agent (if `target` given) or all agents in the pool.
pub async fn handle_agent_status(
    session_pools: Option<&SessionPoolsMap>,
    args: &serde_json::Value,
) -> String {
    let session_id = match extract_session_id(args) {
        Some(id) if id != Uuid::nil() => id,
        _ => return "Error: no active session — session_id missing from _context".to_string(),
    };

    let pools = match session_pools {
        Some(p) => p,
        None => return "Error: session_pools not available".to_string(),
    };

    let pools_read = pools.read().await;
    let pool = match pools_read.get(&session_id) {
        Some(p) => p,
        None => {
            return serde_json::json!({ "agents": [] }).to_string();
        }
    };

    // Single agent query — use "target" (same as other actions for consistency).
    if let Some(target) = args.get("target").and_then(|v| v.as_str()) {
        if let Some(agent) = pool.get(target) {
            let last_result_arc = agent.last_result.clone();
            let status_str = if agent.is_processing() {
                "processing"
            } else {
                "idle"
            };
            let iterations = agent.iterations();
            let elapsed = agent.elapsed().as_secs_f64();
            // Drop pools_read before awaiting last_result lock.
            drop(pools_read);
            let last_result = last_result_arc.read().await.clone();
            return serde_json::json!({
                "agent": target,
                "status": status_str,
                "iterations": iterations,
                "elapsed_secs": elapsed,
                "last_result": last_result,
            })
            .to_string();
        } else {
            return format!("Error: agent '{}' not found in session pool", target);
        }
    }

    // List all agents.
    let entries = pool.list();
    serde_json::json!({ "agents": entries }).to_string()
}

/// `kill` — remove (and drop) a live agent from the session pool.
pub async fn handle_agent_kill(
    session_pools: Option<&SessionPoolsMap>,
    args: &serde_json::Value,
) -> String {
    let target = match args.get("target").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return "Error: 'target' parameter is required".to_string(),
    };

    let session_id = match extract_session_id(args) {
        Some(id) if id != Uuid::nil() => id,
        _ => return "Error: no active session — session_id missing from _context".to_string(),
    };

    let pools = match session_pools {
        Some(p) => p,
        None => return "Error: session_pools not available".to_string(),
    };

    let mut pools_write = pools.write().await;
    let pool = match pools_write.get_mut(&session_id) {
        Some(p) => p,
        None => return format!("Error: no agent pool for session {}", session_id),
    };

    match pool.remove(target) {
        Some(_dropped) => {
            // Drop handles cleanup (cancel + abort).
            serde_json::json!({
                "status": "ok",
                "agent": target,
                "message": format!("Agent '{}' killed", target),
            })
            .to_string()
        }
        None => format!("Error: agent '{}' not found in session pool", target),
    }
}

/// `collect` — block until an async-spawned agent completes and return its result.
/// Used after `agent(action="run", mode="async")` for parallel agent patterns.
pub async fn handle_agent_collect(
    session_pools: Option<&SessionPoolsMap>,
    args: &serde_json::Value,
    timeouts: AgentToolTimeouts,
) -> String {
    let target = match args.get("target").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return "Error: 'target' parameter is required".to_string(),
    };

    let session_id = match extract_session_id(args) {
        Some(id) if id != Uuid::nil() => id,
        _ => {
            return "Error: no active session — agent tool requires session context via _context"
                .to_string()
        }
    };

    let pools = match session_pools {
        Some(p) => p,
        None => return "Error: session_pools not available".to_string(),
    };

    // Block until the agent completes (same logic as sync run).
    wait_for_agent_result(pools, session_id, target, timeouts.message_result).await
}

#[cfg(test)]
mod tests {
    // Verify that an empty or whitespace-only last_result is treated identically to None.
    // Regression for the bug where `run_subagent_with_session` returned Ok("") (reasoning model
    // produced only <think> blocks) and the caller received {"result":""} instead of
    // {"result":"(no result)"}.
    #[test]
    fn empty_last_result_yields_no_result_string() {
        let cases: &[(&str, &str)] = &[
            ("real result", "real result"),
            ("", "(no result)"),
            ("   ", "(no result)"),
            ("\n\t", "(no result)"),
        ];
        for (input, expected) in cases {
            let result_text = match Some(input.to_string()) {
                Some(s) if !s.trim().is_empty() => s,
                _ => "(no result)".to_string(),
            };
            assert_eq!(result_text, *expected, "input={input:?}");
        }
    }

    #[test]
    fn none_last_result_yields_no_result_string() {
        let result_text: String = match None::<String> {
            Some(s) if !s.trim().is_empty() => s,
            _ => "(no result)".to_string(),
        };
        assert_eq!(result_text, "(no result)");
    }

    // ── handle_agent_message: early-return validation paths ──────────────────
    //
    // These tests exercise the argument-parsing and pool-lookup short-circuits
    // without spawning a real LiveAgent (which would require an LLM provider).
    // The sync send-and-wait happy path is covered by integration tests that
    // run a full session (LLM mocking is out of scope here).

    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn empty_pools() -> SessionPoolsMap {
        Arc::new(RwLock::new(HashMap::new()))
    }

    #[tokio::test]
    async fn message_missing_target_returns_error() {
        let pools = empty_pools();
        let args = serde_json::json!({
            "action": "message",
            "text": "hi",
            "_context": { "session_id": Uuid::new_v4().to_string() },
        });
        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        assert!(out.starts_with("Error: 'target' parameter is required"), "got: {out}");
    }

    #[tokio::test]
    async fn message_missing_text_returns_error() {
        let pools = empty_pools();
        let args = serde_json::json!({
            "action": "message",
            "target": "Bob",
            "_context": { "session_id": Uuid::new_v4().to_string() },
        });
        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        assert!(out.starts_with("Error: 'text' parameter is required"), "got: {out}");
    }

    #[tokio::test]
    async fn message_missing_session_returns_error() {
        let pools = empty_pools();
        let args = serde_json::json!({
            "action": "message",
            "target": "Bob",
            "text": "hi",
        });
        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        assert!(out.contains("no active session"), "got: {out}");
    }

    #[tokio::test]
    async fn message_returns_error_if_no_session_pool() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        let args = serde_json::json!({
            "action": "message",
            "target": "Bob",
            "text": "hi",
            "_context": { "session_id": session_id.to_string() },
        });
        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        assert!(out.contains("no agent pool for session"), "got: {out}");
    }

    #[tokio::test]
    async fn message_returns_error_if_target_not_in_pool() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        // Insert an empty pool for the session so we hit the "agent not found"
        // branch rather than the "no agent pool" branch.
        {
            let mut pw = pools.write().await;
            pw.insert(session_id, SessionAgentPool::new(session_id));
        }
        let args = serde_json::json!({
            "action": "message",
            "target": "Bob",
            "text": "hi",
            "_context": { "session_id": session_id.to_string() },
        });
        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        assert!(
            out.contains("agent 'Bob' not found in session pool"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn message_no_session_pools_dependency_returns_error() {
        let args = serde_json::json!({
            "action": "message",
            "target": "Bob",
            "text": "hi",
            "_context": { "session_id": Uuid::new_v4().to_string() },
        });
        let out = handle_agent_message(None, &args, AgentToolTimeouts::legacy_defaults()).await;
        assert!(out.contains("session_pools not available"), "got: {out}");
    }

    // wait_for_result default + opt-out parsing (without exercising the wait
    // path, which needs a real spawned agent). We verify the parser by
    // inspecting the JSON value directly.
    #[test]
    fn wait_for_result_defaults_to_true() {
        let args = serde_json::json!({ "action": "message", "target": "x", "text": "y" });
        let v = args
            .get("wait_for_result")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        assert!(v, "default must be true");
    }

    #[test]
    fn wait_for_result_false_is_respected() {
        let args = serde_json::json!({
            "action": "message",
            "target": "x",
            "text": "y",
            "wait_for_result": false,
        });
        let v = args
            .get("wait_for_result")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        assert!(!v, "explicit false must be respected");
    }

    // ── Sync round-trip test (stub LiveAgent, no LLM) ───────────────────────
    //
    // Build a `LiveAgent` manually with a tokio task that just echoes incoming
    // messages: sets PROCESSING, writes the text into `last_result`, sets IDLE.
    // This exercises the full `handle_agent_message` send → wait_for_result
    // path without needing an LLM provider or spawning a real subagent.

    use crate::agent::session_agent_pool::{
        AgentMessage, LiveAgent, STATUS_IDLE, STATUS_PROCESSING,
    };
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
    use std::time::Instant;

    /// Spawn a stub `LiveAgent` whose processing loop simply echoes each
    /// incoming message text back into `last_result` and returns to IDLE.
    /// Starts in IDLE (the helper does not enqueue an initial task).
    fn spawn_echo_agent(name: &str) -> LiveAgent {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentMessage>(32);
        let status = Arc::new(AtomicU8::new(STATUS_IDLE));
        let last_result = Arc::new(RwLock::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let iteration_count = Arc::new(AtomicUsize::new(0));

        let status_for_task = status.clone();
        let last_result_for_task = last_result.clone();
        let cancel_for_task = cancel.clone();
        let iteration_count_for_task = iteration_count.clone();

        let task_handle = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if cancel_for_task.load(Ordering::Relaxed) {
                    break;
                }
                status_for_task.store(STATUS_PROCESSING, Ordering::Release);
                // Tiny yield so observers can see PROCESSING if they're racing.
                tokio::time::sleep(Duration::from_millis(20)).await;
                *last_result_for_task.write().await = Some(format!("echo: {}", msg.text));
                iteration_count_for_task.fetch_add(1, Ordering::Relaxed);
                status_for_task.store(STATUS_IDLE, Ordering::Release);
            }
        });

        LiveAgent {
            name: name.to_string(),
            message_tx: tx,
            status,
            last_result,
            cancel,
            created_at: Instant::now(),
            iteration_count,
            task_handle,
        }
    }

    #[tokio::test]
    async fn message_sync_round_trip_returns_echoed_last_result() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        {
            let mut pw = pools.write().await;
            let mut pool = SessionAgentPool::new(session_id);
            pool.insert(spawn_echo_agent("Echo"));
            pw.insert(session_id, pool);
        }

        let args = serde_json::json!({
            "action": "message",
            "target": "Echo",
            "text": "ping",
            // wait_for_result defaults to true (sync mode).
            "_context": { "session_id": session_id.to_string() },
        });

        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        // Should return the echoed last_result, NOT a "Message sent" stub.
        assert!(
            out.contains("\"status\":\"completed\"") && out.contains("echo: ping"),
            "expected completed status with echoed result, got: {out}"
        );
        // Sanity: the agent should be back to IDLE after completion.
        let pr = pools.read().await;
        let pool = pr.get(&session_id).expect("pool present");
        let agent = pool.get("Echo").expect("agent present");
        assert!(agent.is_idle(), "agent should be idle after completion");
    }

    #[tokio::test]
    async fn message_fire_and_forget_returns_quickly() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        {
            let mut pw = pools.write().await;
            let mut pool = SessionAgentPool::new(session_id);
            pool.insert(spawn_echo_agent("Echo"));
            pw.insert(session_id, pool);
        }

        let args = serde_json::json!({
            "action": "message",
            "target": "Echo",
            "text": "ping",
            "wait_for_result": false,
            "_context": { "session_id": session_id.to_string() },
        });

        let start = Instant::now();
        let out = handle_agent_message(Some(&pools), &args, AgentToolTimeouts::legacy_defaults()).await;
        let elapsed = start.elapsed();

        assert!(
            out.contains("\"status\":\"sent\"") && out.contains("fire-and-forget"),
            "expected fire-and-forget sent status, got: {out}"
        );
        // Echo task sleeps 20ms before writing last_result, but with
        // wait_for_result=false we should return well before any wait_for_agent_result
        // poll tick (1s). A 500ms ceiling gives plenty of slack on slow CI.
        assert!(
            elapsed < Duration::from_millis(500),
            "fire-and-forget took too long: {:?}",
            elapsed
        );
    }

    // ── AgentToolTimeouts plumbing ──────────────────────────────────────────
    //
    // Verify that a custom (very short) `message_wait_for_idle` deadline
    // surfaces a "did not become idle" error — i.e., the value is actually
    // threaded through to `wait_until_idle` rather than ignored in favour of a
    // hardcoded constant.

    #[tokio::test]
    async fn message_wait_for_idle_uses_configured_timeout() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();

        // Spawn a stub agent already marked PROCESSING with a frozen task —
        // it will never enter IDLE on its own, so `wait_until_idle` must
        // time out using the configured (1s) deadline rather than the old
        // hardcoded 60s.
        {
            let mut pw = pools.write().await;
            let mut pool = SessionAgentPool::new(session_id);
            let echo = spawn_echo_agent("Frozen");
            // Force PROCESSING and never let the echo task return to IDLE
            // (tx is dropped when LiveAgent is, so we keep the task alive
            // by never sending a message; status stays PROCESSING forever).
            echo.status.store(STATUS_PROCESSING, Ordering::Release);
            pool.insert(echo);
            pw.insert(session_id, pool);
        }

        let args = serde_json::json!({
            "action": "message",
            "target": "Frozen",
            "text": "ping",
            "_context": { "session_id": session_id.to_string() },
        });

        let timeouts = AgentToolTimeouts {
            message_wait_for_idle: Duration::from_secs(1),
            message_result: Duration::from_secs(60),
            safety: Duration::from_secs(120),
        };

        let start = Instant::now();
        let out = handle_agent_message(Some(&pools), &args, timeouts).await;
        let elapsed = start.elapsed();

        assert!(
            out.contains("did not become idle"),
            "expected idle-timeout error, got: {out}"
        );
        // Must time out fast (~1s), not after 60s. Allow 5s ceiling for slow CI.
        assert!(
            elapsed < Duration::from_secs(5),
            "wait_for_idle deadline ignored — elapsed = {:?}",
            elapsed
        );
    }
}
