//! Pipeline step: agent_tool — session agent pool operations (migrated from engine_agent_tool.rs).
//!
//! Each function takes explicit dependencies instead of `&self` on `AgentEngine`.

use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::session_agent_pool::{self, SessionAgentPool, SessionPoolsMap};
use crate::gateway::state::AgentMap;

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
pub async fn handle_agent_tool(
    session_pools: Option<&SessionPoolsMap>,
    agent_map: Option<&AgentMap>,
    db: &PgPool,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match action {
        "run" => handle_agent_run(session_pools, agent_map, db, agent_name, args).await,
        "message" => handle_agent_message(session_pools, args).await,
        "status" => handle_agent_status(session_pools, args).await,
        "kill" => handle_agent_kill(session_pools, args).await,
        "collect" => handle_agent_collect(session_pools, args).await,
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
    let result =
        wait_for_agent_result(pools, session_id, target, std::time::Duration::from_secs(300))
            .await;

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
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

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

/// `message` — send a follow-up message to an already-running live agent.
pub async fn handle_agent_message(
    session_pools: Option<&SessionPoolsMap>,
    args: &serde_json::Value,
) -> String {
    let target = match args.get("target").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n,
        _ => return "Error: 'target' parameter is required".to_string(),
    };
    let text = match args.get("text").and_then(|v| v.as_str()) {
        Some(t) if !t.is_empty() => t,
        _ => return "Error: 'text' parameter is required".to_string(),
    };

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
        None => return format!("Error: no agent pool for session {}", session_id),
    };
    let agent = match pool.get(target) {
        Some(a) => a,
        None => return format!("Error: agent '{}' not found in session pool", target),
    };

    // Set PROCESSING *before* try_send to close TOCTOU window: if we set it after,
    // the agent loop may process the message and go IDLE before our store, then we
    // overwrite IDLE→PROCESSING causing collect/status to hang.
    agent.status.store(
        session_agent_pool::STATUS_PROCESSING,
        std::sync::atomic::Ordering::Release,
    );
    match agent.message_tx.try_send(session_agent_pool::AgentMessage {
        text: text.to_string(),
    }) {
        Ok(()) => serde_json::json!({
            "status": "ok",
            "agent": target,
            "message": "Message sent",
        })
        .to_string(),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            // Revert: send failed, agent didn't get the message
            agent.status.store(
                session_agent_pool::STATUS_IDLE,
                std::sync::atomic::Ordering::Release,
            );
            format!(
                "Error: agent '{}' message queue is full — it may still be processing",
                target
            )
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            // Don't revert to IDLE — agent loop is dead. Leave PROCESSING so that
            // collect/wait_for_agent_result detects task_handle.is_finished() and
            // returns the error instead of silently returning "(no result)".
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
    wait_for_agent_result(pools, session_id, target, std::time::Duration::from_secs(300)).await
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
}
