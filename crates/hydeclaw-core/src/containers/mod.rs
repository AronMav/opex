pub mod sandbox;

use anyhow::Result;
use bollard::container::{InspectContainerOptions, StartContainerOptions, StopContainerOptions};
use bollard::Docker;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::Instant;

use crate::config::McpConfig;

/// Manages Docker container lifecycle for on-demand MCP servers.
pub struct ContainerManager {
    docker: Docker,
    mcp: Arc<RwLock<HashMap<String, McpConfig>>>,
    /// Tracks last activity time for idle timeout.
    activity: Arc<RwLock<HashMap<String, Instant>>>,
}

impl ContainerManager {
    /// Connect to Docker via Unix socket (default) or TCP.
    pub fn new(docker_url: &str, mcp: HashMap<String, McpConfig>) -> Result<Self> {
        let docker = Docker::connect_with_http(docker_url, 10, bollard::API_DEFAULT_VERSION)?;
        Ok(Self {
            docker,
            mcp: Arc::new(RwLock::new(mcp)),
            activity: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Check if an MCP server is configured.
    pub async fn has_mcp(&self, name: &str) -> bool {
        self.mcp.read().await.contains_key(name)
    }

    /// Add or update an MCP server config at runtime.
    pub async fn add_or_update_mcp(&self, name: String, cfg: McpConfig) {
        self.mcp.write().await.insert(name, cfg);
    }

    /// Remove an MCP server config at runtime.
    pub async fn remove_mcp(&self, name: &str) {
        self.mcp.write().await.remove(name);
        self.activity.write().await.remove(name);
    }

    /// Ensure a MCP server is reachable. Returns the server's base HTTP URL.
    /// For URL-based MCPs, returns the URL directly without touching Docker.
    /// For Docker-based MCPs, starts the container if needed.
    pub async fn ensure_running(&self, mcp_name: &str) -> Result<String> {
        let entry = self.mcp.read().await.get(mcp_name).cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown MCP server: {mcp_name}"))?;

        // URL-based MCP — no Docker, just return the URL.
        if let Some(ref url) = entry.url {
            self.activity.write().await.insert(mcp_name.to_string(), Instant::now());
            return Ok(url.trim_end_matches('/').to_string());
        }

        // Docker-based MCP
        let container_name = entry.container.as_deref()
            .ok_or_else(|| anyhow::anyhow!("MCP '{mcp_name}' has no url or container"))?
            .to_string();
        let port = entry.port
            .ok_or_else(|| anyhow::anyhow!("MCP '{mcp_name}' has no url or port"))?;

        // Check if already running
        match self
            .docker
            .inspect_container(&container_name, None::<InspectContainerOptions>)
            .await
        {
            Ok(info) => {
                let running = info
                    .state
                    .as_ref()
                    .and_then(|s| s.running)
                    .unwrap_or(false);

                if !running {
                    tracing::info!(container = %container_name, "starting on-demand MCP server");
                    self.docker
                        .start_container(&container_name, None::<StartContainerOptions<String>>)
                        .await?;
                    self.wait_healthy(&container_name, Duration::from_secs(30)).await?;
                    // Probe the actual TCP port — wait_healthy returns immediately when
                    // no Docker HEALTHCHECK is defined (running state is enough for Docker,
                    // but the MCP HTTP server inside may still be initializing).
                    self.probe_port_ready(port, Duration::from_secs(20)).await?;
                }
            }
            Err(e) => {
                return Err(anyhow::anyhow!("container '{container_name}' not found: {e}"));
            }
        }

        // Record activity
        self.activity.write().await.insert(mcp_name.to_string(), Instant::now());

        // Use localhost since hydeclaw-core runs on the host, not inside Docker.
        Ok(format!("http://localhost:{port}"))
    }

    /// Stop a MCP container gracefully. No-op for URL-based MCPs.
    pub async fn stop(&self, mcp_name: &str) -> Result<()> {
        let entry = self.mcp.read().await.get(mcp_name).cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown MCP server: {mcp_name}"))?;

        // URL-based MCPs don't have a container to stop.
        if entry.url.is_some() {
            self.activity.write().await.remove(mcp_name);
            return Ok(());
        }

        let container_name = entry.container
            .ok_or_else(|| anyhow::anyhow!("MCP '{mcp_name}' has no container"))?;

        tracing::info!(container = %container_name, "stopping idle MCP server");
        self.docker
            .stop_container(&container_name, Some(StopContainerOptions { t: 10 }))
            .await?;

        self.activity.write().await.remove(mcp_name);
        Ok(())
    }

    /// Start all persistent MCP servers (mode = "persistent").
    pub async fn start_persistent(&self) -> Result<()> {
        let mcp = self.mcp.read().await.clone();
        for (name, entry) in &mcp {
            if entry.mode == "persistent" {
                let target = entry.url.as_deref()
                    .or(entry.container.as_deref())
                    .unwrap_or("?");
                tracing::info!(mcp = %name, target = %target, "starting persistent MCP server");
                match self.ensure_running(name).await {
                    Ok(_) => tracing::info!(mcp = %name, "persistent MCP server started"),
                    Err(e) => tracing::warn!(mcp = %name, error = %e, "failed to start persistent MCP server"),
                }
            }
        }
        Ok(())
    }

    /// Cleanup orphaned on-demand MCP containers that may be left from a previous crash.
    /// Called once at startup before the idle reaper begins.
    pub async fn cleanup_orphans(&self) {
        let mcp = self.mcp.read().await.clone();
        let mut stopped = 0;
        for (name, entry) in &mcp {
            if entry.url.is_some() || entry.mode != "on-demand" {
                continue;
            }
            let container_name = match entry.container.as_deref() {
                Some(c) => c,
                None => continue,
            };
            // Check if container is running
            if let Ok(info) = self.docker.inspect_container(container_name, None::<bollard::container::InspectContainerOptions>).await {
                let running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
                if running {
                    tracing::info!(container = %container_name, mcp = %name, "stopping orphaned on-demand MCP container");
                    let _ = self.docker.stop_container(
                        container_name,
                        Some(bollard::container::StopContainerOptions { t: 5 }),
                    ).await;
                    stopped += 1;
                }
            } // Err: container doesn't exist — fine
        }
        if stopped > 0 {
            tracing::info!(stopped, "cleaned up orphaned MCP containers at startup");
        }
    }

    /// Check idle MCP servers and stop those that exceeded their timeout.
    pub async fn reap_idle(&self) {
        let activity = self.activity.read().await.clone();
        let mcp = self.mcp.read().await.clone();
        let mut to_stop = vec![];

        for (name, last_active) in &activity {
            if let Some(entry) = mcp.get(name) {
                // URL-based MCPs are always available — no idle reaping.
                if entry.url.is_some() || entry.mode != "on-demand" {
                    continue;
                }

                let timeout = parse_duration(&entry.idle_timeout.clone().unwrap_or("5m".into()));
                if last_active.elapsed() > timeout {
                    to_stop.push(name.clone());
                }
            }
        }

        for name in to_stop {
            if let Err(e) = self.stop(&name).await {
                tracing::warn!(mcp = %name, error = %e, "failed to stop idle MCP server");
            }
        }
    }

    /// Poll a TCP port until it accepts connections or the deadline expires.
    ///
    /// Used after starting Docker containers that lack a HEALTHCHECK — ensures
    /// the MCP HTTP server inside is actually listening before the caller proceeds.
    async fn probe_port_ready(&self, port: u16, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                anyhow::bail!("MCP server port {port} not ready within {}s", timeout.as_secs());
            }
            let probe = tokio::time::timeout(
                Duration::from_secs(1),
                tokio::net::TcpStream::connect(("127.0.0.1", port)),
            )
            .await;
            match probe {
                Ok(Ok(_)) => {
                    tracing::debug!(port, "MCP server port ready");
                    return Ok(());
                }
                _ => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    /// Wait until a container passes health check or timeout.
    async fn wait_healthy(&self, container_name: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                anyhow::bail!("container '{container_name}' did not become healthy in time");
            }

            match self
                .docker
                .inspect_container(container_name, None::<InspectContainerOptions>)
                .await
            {
                Ok(info) => {
                    let running = info
                        .state
                        .as_ref()
                        .and_then(|s| s.running)
                        .unwrap_or(false);

                    if running {
                        // Check if health check exists and is passing
                        let health_status = info
                            .state
                            .as_ref()
                            .and_then(|s| s.health.as_ref())
                            .and_then(|h| h.status.as_ref())
                            .map(std::string::ToString::to_string);

                        match health_status.as_deref() {
                            Some("healthy") => return Ok(()),
                            Some("unhealthy") => {
                                anyhow::bail!("container '{container_name}' is unhealthy")
                            }
                            None => return Ok(()), // No healthcheck defined, running is enough
                            _ => {}                 // starting — keep waiting
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(container = %container_name, error = %e, "inspect failed during health wait");
                }
            }

            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
}

/// Parse a duration string like "5m", "30s", "2m".
fn parse_duration(s: &str) -> Duration {
    let s = s.trim();
    if let Some(mins) = s.strip_suffix('m')
        && let Ok(n) = mins.parse::<u64>() {
            return Duration::from_secs(n * 60);
        }
    if let Some(secs) = s.strip_suffix('s')
        && let Ok(n) = secs.parse::<u64>() {
            return Duration::from_secs(n);
        }
    tracing::warn!(input = %s, "unrecognized duration format, defaulting to 5m");
    Duration::from_secs(300) // default 5 min
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_five_minutes() {
        assert_eq!(parse_duration("5m"), Duration::from_secs(300));
    }

    #[test]
    fn parse_thirty_seconds() {
        assert_eq!(parse_duration("30s"), Duration::from_secs(30));
    }

    #[test]
    fn parse_two_minutes() {
        assert_eq!(parse_duration("2m"), Duration::from_secs(120));
    }

    #[test]
    fn parse_invalid_defaults_to_five_minutes() {
        assert_eq!(parse_duration("invalid"), Duration::from_secs(300));
    }

    #[test]
    fn parse_with_whitespace() {
        assert_eq!(parse_duration(" 5m "), Duration::from_secs(300));
    }
}
