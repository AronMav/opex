use crate::config::ResourceSettings;

#[derive(Clone, serde::Serialize)]
pub struct ResourceStatus {
    pub disk_free_gb: f64,
    pub disk_warning: bool,
    pub disk_critical: bool,
    pub ram_used_percent: f64,
    pub ram_warning: bool,
    pub ram_critical: bool,
    pub cpu_load_percent: f64,
}

pub async fn check_resources(
    cfg: &ResourceSettings,
    _http: &reqwest::Client,
    _core_url: &str,
    _auth_token: &str,
) -> ResourceStatus {
    let disk_free_gb = get_disk_free_gb().await;
    let ram_used_percent = get_ram_used_percent().await;
    let cpu_load_percent = get_cpu_load_percent().await;

    ResourceStatus {
        disk_free_gb,
        disk_warning: disk_free_gb < cfg.disk_warning_gb as f64,
        disk_critical: disk_free_gb < cfg.disk_critical_gb as f64,
        ram_used_percent,
        ram_warning: ram_used_percent > cfg.ram_warning_percent as f64,
        ram_critical: ram_used_percent > cfg.ram_critical_percent as f64,
        cpu_load_percent,
    }
}

async fn get_disk_free_gb() -> f64 {
    let output = tokio::process::Command::new("df")
        .args(["--output=avail", "-BG", "/"])
        .output()
        .await;
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .nth(1)
            .and_then(|l| l.trim().trim_end_matches('G').parse().ok())
            .unwrap_or(0.0),
        Err(_) => 0.0,
    }
}

async fn get_ram_used_percent() -> f64 {
    let output = tokio::process::Command::new("free")
        .args(["-m"])
        .output()
        .await;
    match output {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            if let Some(line) = text.lines().find(|l| l.starts_with("Mem:")) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let total: f64 = parts[1].parse().unwrap_or(1.0);
                    let used: f64 = parts[2].parse().unwrap_or(0.0);
                    return (used / total) * 100.0;
                }
            }
            0.0
        }
        Err(_) => 0.0,
    }
}

async fn get_cpu_load_percent() -> f64 {
    // Read 1-minute load average from /proc/loadavg, divide by nproc
    let loadavg = tokio::fs::read_to_string("/proc/loadavg").await.unwrap_or_default();
    let load1: f64 = loadavg.split_whitespace().next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let nproc: f64 = tokio::fs::read_to_string("/proc/cpuinfo").await
        .map(|s| s.matches("processor").count() as f64)
        .unwrap_or(1.0)
        .max(1.0);
    (load1 / nproc) * 100.0
}
