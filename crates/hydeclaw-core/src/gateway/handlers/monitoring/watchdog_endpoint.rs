//! GET /api/watchdog/agent-activity — feeds the hydeclaw-watchdog
//! inactivity check. Returns per-agent latest activity + server-
//! computed next-expected-heartbeat so the watchdog needs no cron
//! parsing locally.

use axum::{extract::State, response::Json};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::gateway::clusters::{AgentCore, InfraServices};

#[derive(Debug, Serialize)]
pub(crate) struct AgentActivity {
    pub agent_id: String,
    pub latest_activity_at: Option<DateTime<Utc>>,
    pub next_expected_heartbeat_at: Option<DateTime<Utc>>,
}

pub(crate) async fn api_watchdog_agent_activity(
    State(agents): State<AgentCore>,
    State(infra): State<InfraServices>,
) -> Json<Vec<AgentActivity>> {
    // Snapshot agents under the read-lock, then drop it BEFORE any DB I/O
    // so writers (rename, reload) aren't blocked across SQL round-trips.
    // Matches the pattern in monitoring/doctor.rs §7.
    let snapshot: Vec<(String, Option<crate::config::HeartbeatConfig>)> = {
        let map = agents.map.read().await;
        map.iter()
            .map(|(name, handle)| (name.clone(), handle.engine.cfg().agent.heartbeat.clone()))
            .collect()
        // lock dropped here when `map` goes out of scope
    };

    let mut out: Vec<AgentActivity> = Vec::with_capacity(snapshot.len());

    // Every agent present in the AgentCore map is by definition "enabled":
    // it has a config file under config/agents/ that loaded successfully at
    // startup (or was added at runtime via PUT /api/agents). There is no
    // per-agent `enabled: bool` flag — removing the file is the only way
    // to disable an agent. So we iterate the whole snapshot without filtering.
    for (name, heartbeat_cfg) in snapshot {
        // Aggregate latest activity across all sessions for this agent.
        let latest_activity_at: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT COALESCE( \
                 GREATEST(MAX(activity_at), MAX(last_message_at)), \
                 MAX(activity_at), \
                 MAX(last_message_at)) \
             FROM sessions WHERE agent_id = $1",
        )
        .bind(name.as_str())
        .fetch_one(&infra.db)
        .await
        .ok()
        .flatten();

        // Compute next_expected_heartbeat_at only when the agent has a
        // [agent.heartbeat] config; otherwise leave as None.
        let next_expected_heartbeat_at: Option<DateTime<Utc>> =
            if let Some(hb) = &heartbeat_cfg {
                let last_heartbeat_at: Option<DateTime<Utc>> = sqlx::query_scalar(
                    "SELECT MAX(started_at) FROM sessions \
                     WHERE agent_id = $1 AND channel = 'heartbeat'",
                )
                .bind(name.as_str())
                .fetch_one(&infra.db)
                .await
                .ok()
                .flatten();
                let tz = hb.timezone.as_deref().unwrap_or("Europe/Samara");
                let after = last_heartbeat_at.unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
                crate::scheduler::compute_next_heartbeat_at(&hb.cron, tz, after)
            } else {
                None
            };

        out.push(AgentActivity {
            agent_id: name,
            latest_activity_at,
            next_expected_heartbeat_at,
        });
    }

    Json(out)
}
