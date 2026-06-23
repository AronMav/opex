use crate::resources::ResourceStatus;
use std::collections::HashMap;

#[derive(serde::Serialize)]
pub struct WatchdogStatus {
    pub last_check: String,
    pub uptime_secs: u64,
    pub checks: HashMap<String, ServiceStatus>,
    pub resources: Option<ResourceStatus>,
    pub containers: Vec<ContainerInfo>,
}

#[derive(Clone, serde::Serialize)]
pub struct ContainerInfo {
    pub name: String,
    pub docker_name: String,
    pub status: String,
    pub healthy: bool,
    pub group: String,
}

#[derive(Clone, serde::Serialize)]
pub struct ServiceStatus {
    pub ok: bool,
    pub latency_ms: u64,
    pub last_restart: Option<String>,
    pub error: Option<String>,
    pub flapping: bool,
    pub can_restart: bool,
}

const STATUS_PATH: &str = "/tmp/opex-watchdog.json";

pub fn write_status(status: &WatchdogStatus) {
    let tmp = format!("{STATUS_PATH}.tmp");
    if let Ok(json) = serde_json::to_string_pretty(status)
        && std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, STATUS_PATH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_status() {
        let mut checks = HashMap::new();
        checks.insert("core".to_string(), ServiceStatus {
            ok: true, latency_ms: 5, last_restart: None, error: None, flapping: false, can_restart: true,
        });
        let status = WatchdogStatus {
            last_check: "2026-01-01T00:00:00Z".to_string(),
            uptime_secs: 120,
            checks,
            resources: None,
            containers: vec![],
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"core\""));
        assert!(json.contains("\"ok\":true"));
        assert!(json.contains("\"uptime_secs\":120"));
    }

    #[test]
    fn serialize_with_resources() {
        let status = WatchdogStatus {
            last_check: "now".to_string(),
            uptime_secs: 0,
            checks: HashMap::new(),
            resources: Some(crate::resources::ResourceStatus {
                disk_free_gb: 50.0, disk_warning: false, disk_critical: false,
                ram_used_percent: 30.0, ram_warning: false, ram_critical: false,
                cpu_load_percent: 15.0,
            }),
            containers: vec![],
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"disk_free_gb\":50.0"));
        assert!(json.contains("\"cpu_load_percent\":15.0"));
    }
}
