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
/// Actions: `ask`, `status`, `kill`. Anything else is rejected.
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
        "ask" => handle_agent_ask(session_pools, agent_map, db, agent_name, args, timeouts).await,
        "status" => handle_agent_status(session_pools, args).await,
        "kill" => handle_agent_kill(session_pools, args).await,
        other => format!(
            "Error: unknown action '{other}'. Use 'ask', 'status', or 'kill'."
        ),
    }
}

/// `ask` — single canonical "talk to a peer" verb.
///
/// - **Pool miss** → spawn the peer with `text` as its first user message and
///   block until the peer produces a result. The peer is left alive in the
///   pool for follow-ups (no auto-cleanup; this differs from the old `run`).
/// - **Pool hit** → deliver `text` as the next user message in the existing
///   dialog and block for the result.
/// - `fresh = true` → kill any existing instance of `target` first, then
///   fall through to the spawn path.
///
/// Always synchronous. No `mode`, no `wait_for_result`. Parallel fan-out is
/// handled at the engine level (multiple `ask` tool calls in one batch are
/// run concurrently by `pipeline::parallel`).
pub async fn handle_agent_ask(
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
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return "Error: 'text' parameter is required".to_string(),
    };
    let fresh = args
        .get("fresh")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

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

    // fresh=true: tear down any existing instance first. Tolerate "not found".
    if fresh {
        let mut pools_write = pools.write().await;
        if let Some(pool) = pools_write.get_mut(&session_id) {
            let _ = pool.remove(target);
        }
        // write lock dropped at end of scope before we proceed to lookup
    }

    // Pool hit check (read lock).
    let target_in_pool = {
        let pools_read = pools.read().await;
        pools_read
            .get(&session_id)
            .map(|pool| pool.contains(target))
            .unwrap_or(false)
    };

    if target_in_pool {
        // Pool hit — continue dialog with the live agent.
        return ask_continue_existing(pools, session_id, target, text, timeouts).await;
    }

    // Pool miss — spawn a fresh live agent and wait for its first result.
    let agent_map = match agent_map {
        Some(m) => m,
        None => return "Error: agent_map not available (subagent context)".to_string(),
    };
    let target_engine = {
        let map = agent_map.read().await;
        match map.get(target) {
            Some(handle) => handle.engine.clone(),
            None => return format!("Error: agent '{target}' not found"),
        }
    };

    let _ = crate::db::sessions::add_participant(db, session_id, target).await;

    // Single write lock for the spawn — prevents TOCTOU where two concurrent
    // `ask` calls both observe pool-miss and both try to spawn.
    {
        let mut pools_write = pools.write().await;
        let pool = pools_write
            .entry(session_id)
            .or_insert_with(|| SessionAgentPool::new(session_id));
        // Re-check under write lock: another `ask` may have raced us.
        if pool.contains(target) {
            // Fall through to the continue-existing path after dropping the lock.
            drop(pools_write);
            return ask_continue_existing(pools, session_id, target, text, timeouts).await;
        }

        let live_agent = match session_agent_pool::spawn_live_agent(
            target.to_string(),
            target_engine,
            text.to_string(),
            session_id,
        ) {
            Some(la) => la,
            None => {
                return format!(
                    "Error: failed to deliver initial task to agent '{target}'"
                )
            }
        };
        pool.insert(live_agent);
    }

    tracing::info!(
        from = %agent_name,
        target = %target,
        fresh,
        "agent ask: spawned"
    );

    // Block for the spawn-time result. The agent stays alive in the pool
    // afterwards — no auto-cleanup (the old `run` removed it; we don't).
    wait_for_agent_result(pools, session_id, target, timeouts.message_result).await
}

/// Continue an existing dialog with a live agent: wait for idle → CAS PROCESSING
/// → try_send → wait for result. Mirrors the old `handle_agent_message` sync path.
async fn ask_continue_existing(
    pools: &SessionPoolsMap,
    session_id: Uuid,
    target: &str,
    text: &str,
    timeouts: AgentToolTimeouts,
) -> String {
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
            None => return format!("Error: no agent pool for session {session_id}"),
        };
        let agent = match pool.get(target) {
            Some(a) => a,
            None => return format!("Error: agent '{target}' not found in session pool"),
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
            // Wait for the result. Agent stays in the pool — only `kill` /
            // `fresh=true` / session end removes it.
            wait_for_agent_result(pools, session_id, target, timeouts.message_result).await
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            // Defense-in-depth: should be unreachable now that we wait for idle.
            format!(
                "Error: agent '{target}' message queue is full — it may still be processing"
            )
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            format!("Error: agent '{target}' processing loop has exited")
        }
    }
}

/// Block until a live agent completes its current task, then return its result.
///
/// Uses `LiveAgent::result_notify` for near-zero latency instead of polling.
/// Falls back to the timeout if the agent never transitions to idle.
pub async fn wait_for_agent_result(
    pools: &SessionPoolsMap,
    session_id: Uuid,
    target: &str,
    timeout: std::time::Duration,
) -> String {
    let deadline = std::time::Instant::now() + timeout;

    loop {
        // Snapshot: is the agent done? If so, grab the result.
        // If not, grab the Notify handle so we can wait for the next signal.
        let maybe_notify = {
            let pools_read = pools.read().await;
            let Some(pool) = pools_read.get(&session_id) else {
                return serde_json::json!({
                    "status": "error",
                    "agent": target,
                    "result": format!("Session pool not found for {target}"),
                }).to_string();
            };
            let Some(agent) = pool.get(target) else {
                return serde_json::json!({
                    "status": "error",
                    "agent": target,
                    "result": format!("Agent '{target}' was removed before completing"),
                }).to_string();
            };

            if agent.is_idle() || agent.task_handle.is_finished() {
                // Done — read the result outside this lock scope.
                let result_arc = agent.last_result.clone();
                drop(pools_read);
                let result = result_arc.read().await.clone();
                let result_text = match result {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => "(no result)".to_string(),
                };
                return serde_json::json!({
                    "status": "completed",
                    "agent": target,
                    "result": result_text,
                }).to_string();
            }

            // Still processing — get the Notify handle.
            agent.result_notify.clone()
        };

        // Check timeout before waiting.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return serde_json::json!({
                "status": "timeout",
                "agent": target,
                "message": format!(
                    "Agent '{}' did not complete within {} seconds",
                    target,
                    timeout.as_secs()
                ),
            }).to_string();
        }

        // Wait for the agent to signal completion (or timeout).
        // `notify_one()` stores a permit, so if it fired between our check
        // and this line, `notified()` returns immediately.
        let _ = tokio::time::timeout(remaining, maybe_notify.notified()).await;
    }
}

/// Wait for a live agent to enter the IDLE state without consuming its result.
///
/// Uses `LiveAgent::result_notify` for near-zero latency instead of polling.
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
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let maybe_notify = {
            let pools_read = pools.read().await;
            let pool = pools_read
                .get(&session_id)
                .ok_or_else(|| format!("Error: no agent pool for session {session_id}"))?;
            let agent = pool
                .get(target)
                .ok_or_else(|| format!("Error: agent '{target}' not found in session pool"))?;

            if agent.is_idle() {
                return Ok(());
            }
            if agent.task_handle.is_finished() {
                return Err(format!(
                    "Error: agent '{target}' processing loop has exited"
                ));
            }

            agent.result_notify.clone()
        };

        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "Error: agent '{}' did not become idle within {} seconds — still processing previous message",
                target,
                timeout.as_secs()
            ));
        }

        let _ = tokio::time::timeout(remaining, maybe_notify.notified()).await;
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
            return format!("Error: agent '{target}' not found in session pool");
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
        None => return format!("Error: no agent pool for session {session_id}"),
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
        None => format!("Error: agent '{target}' not found in session pool"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

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

    fn empty_pools() -> SessionPoolsMap {
        Arc::new(RwLock::new(HashMap::new()))
    }

    // ── Stub LiveAgent (echo) helper ────────────────────────────────────────

    use crate::agent::session_agent_pool::{
        AgentMessage, LiveAgent, STATUS_IDLE, STATUS_PROCESSING,
    };
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
    use std::time::Instant;
    use tokio::sync::Notify;

    /// Spawn a stub `LiveAgent` whose processing loop simply echoes each
    /// incoming message text back into `last_result` and returns to IDLE.
    /// Starts in IDLE (the helper does not enqueue an initial task).
    fn spawn_echo_agent(name: &str) -> LiveAgent {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentMessage>(32);
        let status = Arc::new(AtomicU8::new(STATUS_IDLE));
        let last_result = Arc::new(RwLock::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let iteration_count = Arc::new(AtomicUsize::new(0));
        let result_notify = Arc::new(Notify::new());

        let status_for_task = status.clone();
        let last_result_for_task = last_result.clone();
        let cancel_for_task = cancel.clone();
        let iteration_count_for_task = iteration_count.clone();
        let result_notify_for_task = result_notify.clone();

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
                result_notify_for_task.notify_one();
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
            result_notify,
        }
    }

    // ── Validation paths ────────────────────────────────────────────────────

    /// Build a fake `PgPool`. We never actually issue queries on the pool —
    /// `handle_agent_ask` short-circuits on validation before reaching DB
    /// access. The single DB call (`add_participant`) is on the spawn path,
    /// which the unit tests below intentionally do not exercise (it requires
    /// a real `AgentMap` + `AgentEngine`).
    async fn fake_db() -> PgPool {
        // Lazy connect option: returns immediately without contacting Postgres.
        // Any actual query against this pool would hang/fail — tests must not
        // hit a code path that touches the pool.
        sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
            .expect("lazy connect cannot fail")
    }

    #[tokio::test]
    async fn ask_rejects_empty_target() {
        let pools = empty_pools();
        let db = fake_db().await;
        let args = serde_json::json!({
            "action": "ask",
            "text": "hi",
            "_context": { "session_id": Uuid::new_v4().to_string() },
        });
        let out = handle_agent_ask(
            Some(&pools),
            None,
            &db,
            "Caller",
            &args,
            AgentToolTimeouts::legacy_defaults(),
        )
        .await;
        assert!(
            out.starts_with("Error: 'target' parameter is required"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn ask_rejects_empty_text() {
        let pools = empty_pools();
        let db = fake_db().await;
        let args = serde_json::json!({
            "action": "ask",
            "target": "Bob",
            "_context": { "session_id": Uuid::new_v4().to_string() },
        });
        let out = handle_agent_ask(
            Some(&pools),
            None,
            &db,
            "Caller",
            &args,
            AgentToolTimeouts::legacy_defaults(),
        )
        .await;
        assert!(
            out.starts_with("Error: 'text' parameter is required"),
            "got: {out}"
        );
    }

    #[tokio::test]
    async fn ask_rejects_missing_session() {
        let pools = empty_pools();
        let db = fake_db().await;
        let args = serde_json::json!({
            "action": "ask",
            "target": "Bob",
            "text": "hi",
        });
        let out = handle_agent_ask(
            Some(&pools),
            None,
            &db,
            "Caller",
            &args,
            AgentToolTimeouts::legacy_defaults(),
        )
        .await;
        assert!(out.contains("no active session"), "got: {out}");
    }

    /// Old actions (`run`, `message`, `collect`) must be rejected with a
    /// clean error pointing at the new action set.
    #[tokio::test]
    async fn unknown_actions_rejected_cleanly() {
        let pools = empty_pools();
        let db = fake_db().await;
        let session_id = Uuid::new_v4();

        for old in ["run", "message", "collect", "garbage"] {
            let args = serde_json::json!({
                "action": old,
                "target": "Bob",
                "_context": { "session_id": session_id.to_string() },
            });
            let out = handle_agent_tool(
                Some(&pools),
                None,
                &db,
                "Caller",
                &args,
                AgentToolTimeouts::legacy_defaults(),
            )
            .await;
            assert!(
                out.contains(&format!("unknown action '{old}'"))
                    && out.contains("'ask'")
                    && out.contains("'status'")
                    && out.contains("'kill'"),
                "expected dispatcher rejection for action={old}, got: {out}"
            );
        }
    }

    // ── Pool-hit (continue dialog) — sync round-trip with stub LiveAgent ────
    //
    // Pool-miss (spawn) path is exercised indirectly by integration tests with
    // a real `AgentEngine`; mocking `AgentMap` here is too invasive.

    #[tokio::test]
    async fn ask_continues_when_target_in_pool() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        let db = fake_db().await;
        {
            let mut pw = pools.write().await;
            let mut pool = SessionAgentPool::new(session_id);
            pool.insert(spawn_echo_agent("Echo"));
            pw.insert(session_id, pool);
        }

        let args = serde_json::json!({
            "action": "ask",
            "target": "Echo",
            "text": "ping",
            "_context": { "session_id": session_id.to_string() },
        });

        let out = handle_agent_ask(
            Some(&pools),
            None,
            &db,
            "Caller",
            &args,
            AgentToolTimeouts::legacy_defaults(),
        )
        .await;
        assert!(
            out.contains("\"status\":\"completed\"") && out.contains("echo: ping"),
            "expected completed status with echoed result, got: {out}"
        );

        // Agent must REMAIN in the pool after `ask` (no auto-cleanup). This is
        // the behavior change vs. the old `run`.
        let pr = pools.read().await;
        let pool = pr.get(&session_id).expect("pool present");
        assert!(
            pool.contains("Echo"),
            "Echo should still be in the pool after ask completion (no auto-cleanup)"
        );
        let agent = pool.get("Echo").expect("agent present");
        assert!(agent.is_idle(), "agent should be idle after completion");
    }

    #[tokio::test]
    async fn ask_returns_error_if_no_session_pools_dependency() {
        let db = fake_db().await;
        let args = serde_json::json!({
            "action": "ask",
            "target": "Bob",
            "text": "hi",
            "_context": { "session_id": Uuid::new_v4().to_string() },
        });
        let out = handle_agent_ask(
            None,
            None,
            &db,
            "Caller",
            &args,
            AgentToolTimeouts::legacy_defaults(),
        )
        .await;
        assert!(out.contains("session_pools not available"), "got: {out}");
    }

    /// `wait_for_idle` deadline must come from the per-call `AgentToolTimeouts`,
    /// not a hardcoded constant. We force a target into PROCESSING and never let
    /// it return — the call must error out fast (~1s) using the configured value.
    #[tokio::test]
    async fn ask_wait_for_idle_uses_configured_timeout() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        let db = fake_db().await;

        {
            let mut pw = pools.write().await;
            let mut pool = SessionAgentPool::new(session_id);
            let echo = spawn_echo_agent("Frozen");
            // Force PROCESSING and never let the echo task return to IDLE
            // (we never send a message; status stays PROCESSING forever).
            echo.status.store(STATUS_PROCESSING, Ordering::Release);
            pool.insert(echo);
            pw.insert(session_id, pool);
        }

        let args = serde_json::json!({
            "action": "ask",
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
        let out = handle_agent_ask(
            Some(&pools),
            None,
            &db,
            "Caller",
            &args,
            timeouts,
        )
        .await;
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

    /// `fresh=true` against a non-existent target must not error — it should
    /// tolerate "not found" and proceed to the spawn path. Without an
    /// `AgentMap` we won't get past spawn, but the failure mode must be the
    /// agent_map error, NOT a kill error.
    #[tokio::test]
    async fn ask_fresh_tolerates_missing_target() {
        let pools = empty_pools();
        let session_id = Uuid::new_v4();
        let db = fake_db().await;

        // Empty pool — no live agents. fresh=true should be a no-op here.
        {
            let mut pw = pools.write().await;
            pw.insert(session_id, SessionAgentPool::new(session_id));
        }

        let args = serde_json::json!({
            "action": "ask",
            "target": "Ghost",
            "text": "hello",
            "fresh": true,
            "_context": { "session_id": session_id.to_string() },
        });
        // No agent_map — the spawn path will fail with "agent_map not available",
        // which proves we got past the fresh-kill phase without erroring on
        // "not found".
        let out = handle_agent_ask(
            Some(&pools),
            None,
            &db,
            "Caller",
            &args,
            AgentToolTimeouts::legacy_defaults(),
        )
        .await;
        assert!(
            out.contains("agent_map not available"),
            "fresh=true should pass through to spawn path; got: {out}"
        );
    }
}
