//! `/api/status` and `/api/stats` — coarse counters surfaced on the
//! sidebar and the dashboard. Both are read-only and use small bounded
//! aggregate queries.

use axum::{extract::State, response::Json};
use serde_json::{Value, json};

use crate::gateway::clusters::{AgentCore, ConfigServices, InfraServices, StatusMonitor};

pub(crate) async fn api_status(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
) -> Json<Value> {
    let db_ok = sqlx::query("SELECT 1")
        .execute(&infra.db)
        .await
        .is_ok();

    let uptime_secs = status.started_at.elapsed().as_secs();

    let memory_chunks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memory_chunks")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    let scheduled_jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scheduled_jobs WHERE enabled = true")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    let active_sessions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE last_message_at > now() - interval '4 hours'",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0);

    let config = cfg_svc.shared_config.read().await;

    Json(json!({
        "status": if db_ok { "ok" } else { "degraded" },
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_seconds": uptime_secs,
        "db": db_ok,
        "listen": config.gateway.listen,
        "agents": agents.agent_names().await,
        "memory_chunks": memory_chunks,
        "scheduled_jobs": scheduled_jobs,
        "active_sessions": active_sessions,
        "tools_registered": agents.tools.len().await + {
            // Count YAML tool files without parsing them (avoid filesystem overhead per request)
            let yaml_count = match tokio::fs::read_dir("workspace/tools").await {
                Ok(mut dir) => {
                    let mut count = 0u64;
                    while let Ok(Some(entry)) = dir.next_entry().await {
                        if entry.path().extension().is_some_and(|e| e == "yaml" || e == "yml") {
                            count += 1;
                        }
                    }
                    count
                }
                Err(_) => 0,
            };
            yaml_count as usize
        },
    }))
}

pub(crate) async fn api_stats(State(infra): State<InfraServices>) -> Json<Value> {
    let messages_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE created_at > CURRENT_DATE",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0);

    let sessions_today: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE started_at > CURRENT_DATE",
    )
    .fetch_one(&infra.db)
    .await
    .unwrap_or(0);

    let total_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    let total_sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&infra.db)
        .await
        .unwrap_or(0);

    #[allow(clippy::type_complexity)]
    let recent_sessions: Vec<(uuid::Uuid, String, String, chrono::DateTime<chrono::Utc>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, agent_id, channel, last_message_at, title \
             FROM sessions \
             WHERE last_message_at > NOW() - INTERVAL '24 hours' \
             ORDER BY last_message_at DESC LIMIT 10",
        )
        .fetch_all(&infra.db)
        .await
        .unwrap_or_default();

    let recent: Vec<Value> = recent_sessions.iter().map(|(id, agent, channel, ts, title)| {
        json!({ "id": id, "agent_id": agent, "channel": channel, "last_message_at": ts, "title": title })
    }).collect();

    Json(json!({
        "messages_today": messages_today,
        "sessions_today": sessions_today,
        "total_messages": total_messages,
        "total_sessions": total_sessions,
        "recent_sessions": recent,
    }))
}
