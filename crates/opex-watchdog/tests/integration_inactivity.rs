//! Integration: watchdog inactivity::tick against a wiremock-mocked
//! core endpoint and channels/notify endpoint. Drives `tick` directly.

use std::collections::HashMap;

use opex_watchdog::alerter::{AlertConfig, Alerter};
use opex_watchdog::config::WatchdogSettings;
use opex_watchdog::inactivity::{self, AlertState, AlertType, EpisodeKey};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn minimal_settings() -> WatchdogSettings {
    WatchdogSettings {
        enabled: true,
        interval_secs: 60,
        cooldown_secs: 300,
        grace_period_secs: 60,
        flap_window_secs: 600,
        flap_threshold: 3,
        session_retry_enabled: true,
        session_retry_stale_secs: 90,
        session_retry_max_attempts: 3,
        stale_activity_timeout_hours: 6,
        missed_heartbeat_grace_minutes: 10,
        self_healing_enabled: false,
    }
}

#[tokio::test]
async fn tick_fires_alert_for_stale_agent() {
    let mock_server = MockServer::start().await;

    let very_old = chrono::Utc::now() - chrono::Duration::hours(10);
    Mock::given(method("GET"))
        .and(path("/api/watchdog/agent-activity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "agent_id": "Opex",
                "latest_activity_at": very_old.to_rfc3339(),
                "next_expected_heartbeat_at": null
            }
        ])))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/channels/notify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .named("notify-on-fire")
        .mount(&mock_server)
        .await;

    let http = reqwest::Client::new();
    let alerter = Alerter::new(&mock_server.uri(), "test-token");
    let alert_config = AlertConfig {
        channel_ids: vec!["test-channel-uuid".to_string()],
        events: vec!["down".into(), "recovery".into()],
    };
    let cfg = minimal_settings();
    let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();

    inactivity::tick(
        &http,
        &mock_server.uri(),
        "test-token",
        &cfg,
        &mut state,
        &alerter,
        &alert_config,
    )
    .await
    .expect("tick must succeed against the mock");

    assert_eq!(state.len(), 1, "one episode should be open after fire");
}

#[tokio::test]
async fn tick_emits_recovery_when_agent_returns() {
    let mock_server = MockServer::start().await;

    let fresh = chrono::Utc::now();
    Mock::given(method("GET"))
        .and(path("/api/watchdog/agent-activity"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {
                "agent_id": "Opex",
                "latest_activity_at": fresh.to_rfc3339(),
                "next_expected_heartbeat_at": null
            }
        ])))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/channels/notify"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .expect(1)
        .named("notify-on-recovery")
        .mount(&mock_server)
        .await;

    let http = reqwest::Client::new();
    let alerter = Alerter::new(&mock_server.uri(), "test-token");
    let alert_config = AlertConfig {
        channel_ids: vec!["test-channel-uuid".to_string()],
        events: vec!["down".into(), "recovery".into()],
    };
    let cfg = minimal_settings();

    let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();
    state.insert(
        ("Opex".to_string(), AlertType::StaleActivity),
        AlertState {
            fired_at: chrono::Utc::now() - chrono::Duration::hours(1),
        },
    );

    inactivity::tick(
        &http,
        &mock_server.uri(),
        "test-token",
        &cfg,
        &mut state,
        &alerter,
        &alert_config,
    )
    .await
    .expect("tick must succeed");

    assert!(state.is_empty(), "episode must be cleared after recovery");
}

#[tokio::test]
async fn tick_tolerates_endpoint_500() {
    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/watchdog/agent-activity"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&mock_server)
        .await;

    let http = reqwest::Client::new();
    let alerter = Alerter::new(&mock_server.uri(), "test-token");
    let alert_config = AlertConfig::default();
    let cfg = minimal_settings();
    let mut state: HashMap<EpisodeKey, AlertState> = HashMap::new();

    let result = inactivity::tick(
        &http,
        &mock_server.uri(),
        "test-token",
        &cfg,
        &mut state,
        &alerter,
        &alert_config,
    )
    .await;
    assert!(result.is_err(), "tick returns Err on endpoint failure");
    assert!(state.is_empty(), "no episodes opened on error path");
}
