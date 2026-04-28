use crate::config::CheckConfig;
use crate::status::ContainerInfo;

pub struct CheckResult {
    pub ok: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

pub async fn run_check(cfg: &CheckConfig, http: &reqwest::Client) -> CheckResult {
    let start = std::time::Instant::now();

    let (ok, error) = if let Some(ref url) = cfg.url {
        match tokio::time::timeout(
            std::time::Duration::from_secs(cfg.timeout_secs),
            http.get(url).send(),
        )
        .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => (true, None),
            Ok(Ok(resp)) => (false, Some(format!("HTTP {}", resp.status()))),
            Ok(Err(e)) => (false, Some(format!("{e}"))),
            Err(_) => (false, Some(format!("timeout {}s", cfg.timeout_secs))),
        }
    } else if let Some(ref cmd) = cfg.check_cmd {
        match run_shell(cmd).await {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("exit code != 0".into())),
            Err(e) => (false, Some(format!("{e}"))),
        }
    } else {
        (true, None)
    };

    CheckResult {
        ok,
        latency_ms: start.elapsed().as_millis() as u64,
        error,
    }
}

/// Check all Docker containers — returns all non-MCP containers with health status.
pub async fn check_docker_containers() -> Vec<ContainerInfo> {
    let output = tokio::process::Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Names}}\t{{.Status}}"])
        .output()
        .await;

    let mut containers = Vec::new();
    if let Ok(o) = output {
        let text = String::from_utf8_lossy(&o.stdout);
        for line in text.lines() {
            let parts: Vec<&str> = line.splitn(2, '\t').collect();
            if parts.len() == 2 {
                let name = parts[0];
                let status = parts[1];
                if name.starts_with("mcp-") { continue; }
                // Skip containers already monitored via health checks
                if name.contains("postgres") { continue; }
                let healthy = status.starts_with("Up");
                let friendly = friendly_name(name);
                let group = if name.starts_with("hc-agent-") { "agent" } else { "infra" };
                containers.push(ContainerInfo {
                    name: friendly,
                    docker_name: name.to_string(),
                    status: status.to_string(),
                    healthy,
                    group: group.to_string(),
                });
            }
        }
    }
    // Sort: unhealthy first, then by name
    containers.sort_by(|a, b| b.healthy.cmp(&a.healthy).reverse().then(a.name.cmp(&b.name)));
    containers
}

fn friendly_name(docker_name: &str) -> String {
    // Compose containers: docker-postgres-1, docker-searxng-1, docker-browser-renderer-1
    // Bollard containers: hc-agent-{name}
    match docker_name {
        n if n.contains("postgres") => "PostgreSQL".into(),
        n if n.contains("searxng") => "SearXNG".into(),
        n if n.contains("browser-renderer") => "Browser Renderer".into(),
        _ if docker_name.starts_with("hc-agent-") => {
            let agent = docker_name.strip_prefix("hc-agent-").unwrap_or(docker_name);
            let mut chars = agent.chars();
            match chars.next() {
                Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                None => docker_name.to_string(),
            }
        }
        _ => docker_name.to_string(),
    }
}

async fn run_shell(cmd: &str) -> anyhow::Result<bool> {
    let output = tokio::process::Command::new("bash")
        .args(["-c", cmd])
        .output()
        .await?;
    Ok(output.status.success())
}
