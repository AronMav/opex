use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct WatchdogConfig {
    pub watchdog: WatchdogSettings,
    #[serde(default)]
    pub checks: Vec<CheckConfig>,
    #[serde(default)]
    pub resources: ResourceSettings,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WatchdogSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_60")]
    pub interval_secs: u64,
    #[serde(default = "default_300")]
    pub cooldown_secs: u64,
    #[serde(default = "default_60")]
    pub grace_period_secs: u64,
    #[serde(default = "default_600")]
    pub flap_window_secs: u64,
    #[serde(default = "default_3")]
    pub flap_threshold: u32,
    #[serde(default = "default_true")]
    pub session_retry_enabled: bool,
    #[serde(default = "default_90")]
    pub session_retry_stale_secs: u64,
    #[serde(default = "default_3")]
    pub session_retry_max_attempts: u32,
    #[serde(default = "default_stale_activity_timeout_hours")]
    pub stale_activity_timeout_hours: u64,
    #[serde(default = "default_missed_heartbeat_grace_minutes")]
    pub missed_heartbeat_grace_minutes: u64,
    /// Kill-switch for the self-healing infra pass (устойчиво-проблемные
    /// docker-контейнеры → триггер Opex через /api/internal/infra-event).
    /// Opt-in: defaults to `false` so the feature stays inert immediately
    /// after deploy until explicitly enabled in watchdog config.
    #[serde(default)]
    pub self_healing_enabled: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckConfig {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub check_cmd: Option<String>,
    #[serde(default)]
    pub restart_cmd: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_5")]
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceSettings {
    #[serde(default = "default_5")]
    pub disk_warning_gb: u64,
    #[serde(default = "default_1")]
    pub disk_critical_gb: u64,
    #[serde(default = "default_85")]
    pub ram_warning_percent: u64,
    #[serde(default = "default_95")]
    pub ram_critical_percent: u64,
    #[serde(default = "default_300")]
    pub check_interval_secs: u64,
}

impl Default for ResourceSettings {
    fn default() -> Self {
        Self {
            disk_warning_gb: 5, disk_critical_gb: 1,
            ram_warning_percent: 85, ram_critical_percent: 95,
            check_interval_secs: 300,
        }
    }
}

fn default_true() -> bool { true }
fn default_90() -> u64 { 90 }
fn default_stale_activity_timeout_hours() -> u64 { 6 }
fn default_missed_heartbeat_grace_minutes() -> u64 { 10 }
fn default_1() -> u64 { 1 }
fn default_3() -> u32 { 3 }
fn default_5() -> u64 { 5 }
fn default_60() -> u64 { 60 }
fn default_85() -> u64 { 85 }
fn default_95() -> u64 { 95 }
fn default_300() -> u64 { 300 }
fn default_600() -> u64 { 600 }

pub fn load_config(path: &str) -> anyhow::Result<WatchdogConfig> {
    let text = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
[watchdog]
enabled = true
"#;
        let cfg: WatchdogConfig = toml::from_str(toml).unwrap();
        assert!(cfg.watchdog.enabled);
        assert_eq!(cfg.watchdog.interval_secs, 60);
        assert!(cfg.checks.is_empty());
        assert_eq!(cfg.watchdog.stale_activity_timeout_hours, 6);
        assert_eq!(cfg.watchdog.missed_heartbeat_grace_minutes, 10);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
[watchdog]
enabled = true
interval_secs = 30
grace_period_secs = 10

[[checks]]
name = "core"
url = "http://localhost:18789/health"
restart_cmd = "systemctl restart core"

[[checks]]
name = "db"
check_cmd = "pg_isready"
enabled = false

[resources]
disk_warning_gb = 10
ram_warning_percent = 80
"#;
        let cfg: WatchdogConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.watchdog.interval_secs, 30);
        assert_eq!(cfg.watchdog.grace_period_secs, 10);
        assert_eq!(cfg.checks.len(), 2);
        assert_eq!(cfg.checks[0].name, "core");
        assert!(cfg.checks[0].enabled);
        assert!(!cfg.checks[1].enabled);
        assert_eq!(cfg.resources.disk_warning_gb, 10);
        assert_eq!(cfg.resources.ram_warning_percent, 80);
    }

    #[test]
    fn defaults_applied() {
        let toml = "[watchdog]\n";
        let cfg: WatchdogConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.watchdog.cooldown_secs, 300);
        assert_eq!(cfg.watchdog.flap_window_secs, 600);
        assert_eq!(cfg.watchdog.flap_threshold, 3);
        assert_eq!(cfg.resources.check_interval_secs, 300);
    }

    // I2 (final review): self-healing is opt-in — must default to false so the
    // feature stays inert immediately after deploy until explicitly enabled.
    #[test]
    fn self_healing_defaults_false() {
        let toml = "[watchdog]\nenabled = true\n";
        let cfg: WatchdogConfig = toml::from_str(toml).unwrap();
        assert!(!cfg.watchdog.self_healing_enabled);
    }

    #[test]
    fn self_healing_can_be_enabled() {
        let toml = "[watchdog]\nenabled = true\nself_healing_enabled = true\n";
        let cfg: WatchdogConfig = toml::from_str(toml).unwrap();
        assert!(cfg.watchdog.self_healing_enabled);
    }
}
