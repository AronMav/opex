//! Backstop check: alert if scheduled infra jobs (backup / curator) are disabled.
//!
//! The watchdog only *alerts* — it never flips the flags. Re-enabling is the
//! Opex agent's job (HEARTBEAT self-heal). This is the deterministic safety net
//! that fires if the agent path fails and a job silently stays off.

use crate::alerter::{AlertConfig, Alerter};

/// Result of one infra-jobs evaluation.
#[derive(Debug, PartialEq)]
pub struct InfraAlert {
    /// Message to send, or None (no transition / throttled / all-ok).
    pub message: Option<String>,
    /// New "in disabled-alert state" flag to persist for throttling.
    pub now_alerted: bool,
}

/// Decide whether to alert. Alerts only on the transition into the disabled
/// state; stays silent while still disabled (throttle); resets when all enabled.
pub fn decide_infra_alert(
    curator_enabled: bool,
    backup_enabled: bool,
    previously_alerted: bool,
) -> InfraAlert {
    let mut disabled: Vec<&str> = Vec::new();
    if !curator_enabled {
        disabled.push("curator");
    }
    if !backup_enabled {
        disabled.push("backup");
    }

    if disabled.is_empty() {
        return InfraAlert {
            message: None,
            now_alerted: false,
        };
    }
    if previously_alerted {
        return InfraAlert {
            message: None,
            now_alerted: true,
        };
    }
    InfraAlert {
        message: Some(format!(
            "⚠️ Плановые задания выключены: {}. Включите через UI — Opex также включит их на следующем heartbeat.",
            disabled.join(", ")
        )),
        now_alerted: true,
    }
}

/// Fetch curator + backup enabled flags from `GET /api/config` and alert on the
/// transition into the disabled state. Returns the new `previously_alerted`
/// flag. Network / parse errors → `Err` (caller logs and keeps the prior flag —
/// missing data must NOT be treated as "disabled").
pub async fn tick(
    http: &reqwest::Client,
    core_url: &str,
    auth_token: &str,
    alerter: &Alerter,
    alert_config: &AlertConfig,
    previously_alerted: bool,
) -> anyhow::Result<bool> {
    let resp = http
        .get(format!("{core_url}/api/config"))
        .header("Authorization", format!("Bearer {auth_token}"))
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("GET /api/config returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let curator_enabled = read_nested_bool(&body, "curator")?;
    let backup_enabled = read_nested_bool(&body, "backup")?;

    let decision = decide_infra_alert(curator_enabled, backup_enabled, previously_alerted);
    if let Some(msg) = &decision.message {
        tracing::warn!(message = %msg, "infra-jobs backstop alert");
        alerter.send(alert_config, msg, "resource").await;
    }
    Ok(decision.now_alerted)
}

/// Read `body[section]["enabled"]` as a bool, erroring if the path is absent or
/// not a boolean (so a malformed response is logged, not read as "disabled").
fn read_nested_bool(body: &serde_json::Value, section: &str) -> anyhow::Result<bool> {
    body.get(section)
        .and_then(|s| s.get("enabled"))
        .and_then(|v| v.as_bool())
        .ok_or_else(|| anyhow::anyhow!("missing or non-bool {section}.enabled in /api/config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_enabled_no_alert() {
        let r = decide_infra_alert(true, true, false);
        assert_eq!(
            r,
            InfraAlert {
                message: None,
                now_alerted: false
            }
        );
    }

    #[test]
    fn curator_disabled_alerts_once() {
        let r = decide_infra_alert(false, true, false);
        let m = r.message.as_deref().unwrap();
        assert!(m.contains("curator"));
        assert!(!m.contains("backup"));
        assert!(r.now_alerted);
    }

    #[test]
    fn backup_disabled_alerts() {
        let r = decide_infra_alert(true, false, false);
        let m = r.message.as_deref().unwrap();
        assert!(m.contains("backup"));
        assert!(!m.contains("curator"));
        assert!(r.now_alerted);
    }

    #[test]
    fn both_disabled_lists_both() {
        let r = decide_infra_alert(false, false, false);
        let m = r.message.as_deref().unwrap();
        assert!(m.contains("curator") && m.contains("backup"));
        assert!(r.now_alerted);
    }

    #[test]
    fn throttled_while_still_disabled() {
        let r = decide_infra_alert(false, true, true);
        assert_eq!(
            r,
            InfraAlert {
                message: None,
                now_alerted: true
            }
        );
    }

    #[test]
    fn resets_when_reenabled() {
        let r = decide_infra_alert(true, true, true);
        assert_eq!(
            r,
            InfraAlert {
                message: None,
                now_alerted: false
            }
        );
    }

    #[test]
    fn read_nested_bool_parses_config_shape() {
        let body = serde_json::json!({
            "backup": { "enabled": true, "cron": "0 0 5 * * *" },
            "curator": { "enabled": false, "cron": "0 3 * * 0" },
        });
        assert!(read_nested_bool(&body, "backup").unwrap());
        assert!(!read_nested_bool(&body, "curator").unwrap());
        assert!(read_nested_bool(&body, "missing").is_err());
    }
}
