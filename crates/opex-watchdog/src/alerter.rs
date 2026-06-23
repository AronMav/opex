#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alert_config_default() {
        let cfg = AlertConfig::default();
        assert!(cfg.channel_ids.is_empty(), "default channel_ids should be empty");
        assert_eq!(
            cfg.events,
            vec!["down", "restart", "recovery", "resource"],
            "default events should have 4 entries"
        );
    }

    /// Verify send() returns immediately when channel_ids is empty (short-circuit).
    /// We point the alerter at a non-existent URL; if the early return works the
    /// future completes without attempting a network connection.
    #[tokio::test]
    async fn test_send_skips_empty_channels() {
        let alerter = Alerter::new("http://127.0.0.1:1", "token");
        // channel_ids is empty → should return before any HTTP attempt
        let cfg = AlertConfig {
            channel_ids: vec![],
            events: vec!["down".into()],
        };
        // Must complete without error / panic
        alerter.send(&cfg, "test message", "down").await;
    }

    /// Verify send() skips when event_type is not in config.events.
    #[tokio::test]
    async fn test_send_skips_unmatched_event() {
        let alerter = Alerter::new("http://127.0.0.1:1", "token");
        let cfg = AlertConfig {
            channel_ids: vec!["chan-1".into()],
            events: vec!["down".into()],
        };
        // "recovery" is not in events → should return before any HTTP attempt
        alerter.send(&cfg, "test message", "recovery").await;
    }
}

pub struct Alerter {
    http: reqwest::Client,
    core_url: String,
    auth_token: String,
}

#[derive(Debug, Clone)]
pub struct AlertConfig {
    pub channel_ids: Vec<String>,
    pub events: Vec<String>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            channel_ids: vec![],
            events: vec!["down".into(), "restart".into(), "recovery".into(), "resource".into()],
        }
    }
}

impl Alerter {
    pub fn new(core_url: &str, auth_token: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
            core_url: core_url.to_string(),
            auth_token: auth_token.to_string(),
        }
    }

    /// Fetch alert settings from Core API (DB-backed).
    /// Returns None if Core is unreachable (caller should use cached config).
    pub async fn fetch_config(&self) -> Option<AlertConfig> {
        let resp = self
            .http
            .get(format!("{}/api/watchdog/settings", self.core_url))
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let body: serde_json::Value = resp.json().await.ok()?;

        let channel_ids = body["alert_channel_ids"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        let events = body["alert_events"]
            .as_array().map_or_else(|| vec!["down".into(), "restart".into(), "recovery".into(), "resource".into()], |arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect());

        Some(AlertConfig { channel_ids, events })
    }

    /// Send alert to all configured channels via POST /api/channels/notify.
    pub async fn send(&self, config: &AlertConfig, message: &str, event_type: &str) {
        if config.channel_ids.is_empty() || !config.events.contains(&event_type.to_string()) {
            return;
        }

        for channel_id in &config.channel_ids {
            let body = serde_json::json!({
                "channel_id": channel_id,
                "text": message,
            });
            match self
                .http
                .post(format!("{}/api/channels/notify", self.core_url))
                .header("Authorization", format!("Bearer {}", self.auth_token))
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    tracing::info!(event = event_type, channel = %channel_id, "alert sent");
                }
                Ok(r) => {
                    let status = r.status();
                    let text = r.text().await.unwrap_or_default();
                    tracing::warn!(channel = %channel_id, status = %status, body = %text, "alert failed");
                }
                Err(e) => tracing::warn!(error = %e, "alert failed (core unreachable)"),
            }
        }
    }
}
