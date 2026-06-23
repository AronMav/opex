//! Per-agent inactivity checks (stale activity, missed heartbeat).
//! Pure logic; HTTP fetch and orchestration live in main.rs (Task 4 + 5).
//!
//! Types and functions are `pub` (not `pub(crate)`) so integration
//! tests in `tests/` can drive them. The watchdog crate is a binary
//! — there's no public API surface to leak externally.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;

use crate::alerter::{AlertConfig, Alerter};
use crate::config::WatchdogSettings;

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub enum AlertType {
    StaleActivity,
    MissedHeartbeat,
}

#[derive(Debug, Clone)]
pub struct AlertState {
    pub fired_at: DateTime<Utc>,
}

pub type EpisodeKey = (String, AlertType);

#[derive(Debug, Clone, Deserialize)]
pub struct AgentActivity {
    pub agent_id: String,
    pub latest_activity_at: Option<DateTime<Utc>>,
    pub next_expected_heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Fire {
    pub agent_id: String,
    pub alert_type: AlertType,
    pub latest_activity_at: Option<DateTime<Utc>>,
    pub next_expected_heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct Recover {
    pub agent_id: String,
    pub alert_type: AlertType,
}

/// Pure classification: given one agent's activity snapshot and thresholds,
/// returns which alerts are currently firing (zero, one, or both).
///
/// There is no `enabled` check — every agent the endpoint returns is by
/// definition "loaded into AgentCore.map", which is the only meaning of
/// "enabled" in this codebase (`AgentSettings` has no `enabled` field).
pub fn classify(
    agent: &AgentActivity,
    now: DateTime<Utc>,
    stale_threshold: Duration,
    heartbeat_grace: Duration,
) -> Vec<AlertType> {
    let mut out = Vec::new();

    if let Some(latest) = agent.latest_activity_at
        && now - latest > stale_threshold
    {
        out.push(AlertType::StaleActivity);
    }

    if let Some(expected) = agent.next_expected_heartbeat_at
        && now > expected + heartbeat_grace
    {
        out.push(AlertType::MissedHeartbeat);
    }

    out
}

/// Pure dedup: walks classified results AND the set of currently-known
/// agents (so disappeared agents are silently cleaned up). Mutates state,
/// returns the events to emit.
pub fn reconcile(
    classified: HashMap<String, Vec<AlertType>>,
    activity: &HashMap<String, AgentActivity>,
    known_agents: &HashSet<String>,
    state: &mut HashMap<EpisodeKey, AlertState>,
    now: DateTime<Utc>,
) -> (Vec<Fire>, Vec<Recover>) {
    let mut fires = Vec::new();
    let mut recovers = Vec::new();

    // 1. Fires: any currently-classified alert with no open episode.
    for (agent_id, alert_types) in &classified {
        for alert_type in alert_types {
            let key = (agent_id.clone(), *alert_type);
            if state.contains_key(&key) {
                continue;
            }
            state.insert(key, AlertState { fired_at: now });
            let act = activity.get(agent_id);
            fires.push(Fire {
                agent_id: agent_id.clone(),
                alert_type: *alert_type,
                latest_activity_at: act.and_then(|a| a.latest_activity_at),
                next_expected_heartbeat_at: act.and_then(|a| a.next_expected_heartbeat_at),
            });
        }
    }

    // 2. Cleanup / recovery: walk every existing key.
    let keys_to_check: Vec<EpisodeKey> = state.keys().cloned().collect();
    for key in keys_to_check {
        let (agent_id, alert_type) = (&key.0, &key.1);
        if !known_agents.contains(agent_id) {
            // Agent renamed or deleted — silent removal, no Recover alert.
            state.remove(&key);
            continue;
        }
        let still_firing = classified
            .get(agent_id)
            .map(|v| v.contains(alert_type))
            .unwrap_or(false);
        if !still_firing {
            state.remove(&key);
            recovers.push(Recover {
                agent_id: agent_id.clone(),
                alert_type: *alert_type,
            });
        }
    }

    (fires, recovers)
}

pub async fn fetch_agent_activity(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
) -> anyhow::Result<Vec<AgentActivity>> {
    let resp = http
        .get(format!("{core_url}/api/watchdog/agent-activity"))
        .header("Authorization", format!("Bearer {auth_token}"))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("agent-activity endpoint returned status {status}");
    }
    let list: Vec<AgentActivity> = resp.json().await?;
    Ok(list)
}

pub async fn tick(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
    cfg: &WatchdogSettings,
    state: &mut HashMap<EpisodeKey, AlertState>,
    alerter: &Alerter,
    alert_config: &AlertConfig,
) -> anyhow::Result<()> {
    let activity = fetch_agent_activity(http, core_url, auth_token).await?;

    let now = Utc::now();
    let stale = Duration::hours(cfg.stale_activity_timeout_hours as i64);
    let grace = Duration::minutes(cfg.missed_heartbeat_grace_minutes as i64);

    let mut classified: HashMap<String, Vec<AlertType>> = HashMap::new();
    let mut activity_map: HashMap<String, AgentActivity> = HashMap::new();
    let mut known_agents: HashSet<String> = HashSet::new();

    for a in &activity {
        known_agents.insert(a.agent_id.clone());
        let alerts = classify(a, now, stale, grace);
        if !alerts.is_empty() {
            classified.insert(a.agent_id.clone(), alerts);
        }
        activity_map.insert(a.agent_id.clone(), a.clone());
    }

    let (fires, recovers) = reconcile(classified, &activity_map, &known_agents, state, now);

    // Reuse existing "down"/"recovery" event types so UI's existing
    // ALL_ALERT_EVENTS toggle covers these without UI changes.
    for fire in fires {
        let msg = format_fire_message(&fire);
        alerter.send(alert_config, &msg, "down").await;
    }
    for rec in recovers {
        let msg = format_recover_message(&rec);
        alerter.send(alert_config, &msg, "recovery").await;
    }

    Ok(())
}

fn format_fire_message(f: &Fire) -> String {
    match f.alert_type {
        AlertType::StaleActivity => {
            let last = f
                .latest_activity_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".to_string());
            format!("agent {} inactive (last activity: {})", f.agent_id, last)
        }
        AlertType::MissedHeartbeat => {
            let expected = f
                .next_expected_heartbeat_at
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "?".to_string());
            format!(
                "agent {} missed heartbeat (expected at {})",
                f.agent_id, expected
            )
        }
    }
}

fn format_recover_message(r: &Recover) -> String {
    let kind = match r.alert_type {
        AlertType::StaleActivity => "activity",
        AlertType::MissedHeartbeat => "heartbeat",
    };
    format!("agent {} recovered ({})", r.agent_id, kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, latest: Option<DateTime<Utc>>, next_hb: Option<DateTime<Utc>>) -> AgentActivity {
        AgentActivity {
            agent_id: name.to_string(),
            latest_activity_at: latest,
            next_expected_heartbeat_at: next_hb,
        }
    }

    fn t(hours_ago: i64) -> DateTime<Utc> {
        Utc::now() - Duration::hours(hours_ago)
    }

    #[test]
    fn classify_stale_activity_triggers() {
        let a = agent("A", Some(t(10)), None);
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert_eq!(result, vec![AlertType::StaleActivity]);
    }

    #[test]
    fn classify_stale_activity_skips_never_active() {
        let a = agent("A", None, None);
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert!(result.is_empty());
    }

    #[test]
    fn classify_missed_heartbeat_triggers() {
        // expected 30 min ago, grace 10 min → overdue by 20 min → fire
        let a = agent("A", Some(Utc::now()), Some(Utc::now() - Duration::minutes(30)));
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert_eq!(result, vec![AlertType::MissedHeartbeat]);
    }

    #[test]
    fn classify_missed_heartbeat_respects_grace() {
        // expected 5 min ago, grace 10 min → still in grace → no fire
        let a = agent("A", Some(Utc::now()), Some(Utc::now() - Duration::minutes(5)));
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert!(result.is_empty());
    }

    #[test]
    fn classify_no_expected_heartbeat_no_alert() {
        let a = agent("A", Some(Utc::now()), None);
        let result = classify(&a, Utc::now(), Duration::hours(6), Duration::minutes(10));
        assert!(result.is_empty());
    }

    #[test]
    fn reconcile_fires_once() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        let mut classified: HashMap<String, Vec<AlertType>> = HashMap::new();
        classified.insert("A".to_string(), vec![AlertType::StaleActivity]);
        let activity = HashMap::from([("A".to_string(), agent("A", Some(t(10)), None))]);
        let known: HashSet<String> = ["A".to_string()].into_iter().collect();

        let (fires1, recs1) = reconcile(classified.clone(), &activity, &known, &mut state, now);
        assert_eq!(fires1.len(), 1);
        assert!(recs1.is_empty());

        let (fires2, recs2) = reconcile(classified, &activity, &known, &mut state, now);
        assert!(fires2.is_empty(), "second pass with same input must not re-fire");
        assert!(recs2.is_empty());
    }

    #[test]
    fn reconcile_recovers_on_resolution() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        state.insert(("A".to_string(), AlertType::StaleActivity), AlertState { fired_at: now });
        let activity = HashMap::from([("A".to_string(), agent("A", Some(now), None))]);
        let known: HashSet<String> = ["A".to_string()].into_iter().collect();

        let (fires, recs) = reconcile(HashMap::new(), &activity, &known, &mut state, now);
        assert!(fires.is_empty());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].alert_type, AlertType::StaleActivity);
        assert!(state.is_empty(), "state must be empty after recovery");
    }

    #[test]
    fn reconcile_independent_alert_types() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        state.insert(("A".to_string(), AlertType::StaleActivity), AlertState { fired_at: now });

        let mut classified: HashMap<String, Vec<AlertType>> = HashMap::new();
        classified.insert("A".to_string(), vec![AlertType::StaleActivity, AlertType::MissedHeartbeat]);
        let activity = HashMap::from([("A".to_string(), agent("A", Some(t(10)), Some(t(1))))]);
        let known: HashSet<String> = ["A".to_string()].into_iter().collect();

        let (fires, recs) = reconcile(classified, &activity, &known, &mut state, now);
        assert_eq!(fires.len(), 1, "stale already open, only missed_heartbeat is new");
        assert_eq!(fires[0].alert_type, AlertType::MissedHeartbeat);
        assert!(recs.is_empty());
    }

    #[test]
    fn reconcile_silent_cleanup_on_disappeared_agent() {
        let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
        let now = Utc::now();
        state.insert(("Hyde".to_string(), AlertType::StaleActivity), AlertState { fired_at: now });

        // Hyde no longer in endpoint response (renamed / deleted).
        let known: HashSet<String> = ["Alma".to_string()].into_iter().collect();
        let activity: HashMap<String, AgentActivity> = HashMap::new();

        let (fires, recs) = reconcile(HashMap::new(), &activity, &known, &mut state, now);
        assert!(fires.is_empty());
        assert!(recs.is_empty(), "silent cleanup must NOT emit Recover for vanished agent");
        assert!(state.is_empty(), "vanished agent's episode entry must be removed");
    }
}
