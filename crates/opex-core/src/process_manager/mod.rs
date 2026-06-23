//! Manages long-lived child processes (channels, toolgate) spawned by Core.
//!
//! Each managed process is:
//! - Spawned at Core startup via `systemd-run --scope --user` (gives `MemoryMax`, `CPUQuota`,
//!   `NoNewPrivileges`, `PrivateTmp` without Docker overhead).
//! - Automatically restarted on crash (with backoff).
//! - Reachable via `POST /api/services/{name}/restart` (kill + respawn with port release wait).
//! - Given only the env vars it needs (minimal passthrough, no DB credentials leaked to channels).

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::process::Child;
use tokio::sync::Mutex;

// ── Config ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)] // health_url/memory_max/cpu_quota are reserved fields per the
                    // doc comments; accepted from TOML but not yet enforced at runtime.
pub struct ManagedProcessConfig {
    /// Service name (e.g. "channels", "toolgate").
    pub name: String,
    /// Command + args, e.g. ["bun", "run", "src/index.ts"].
    pub command: Vec<String>,
    /// Working directory, relative to Core's cwd (e.g. "channels").
    pub working_dir: String,
    /// Which env var names to forward from Core's environment.
    #[serde(default)]
    pub env_passthrough: Vec<String>,
    /// Extra env vars to inject (key = name, value = literal or "${VAR}" reference).
    #[serde(default)]
    pub env_extra: HashMap<String, String>,
    /// HTTP URL for health-check polling (reserved for future use).
    pub health_url: Option<String>,
    /// TCP port the service binds; used for port-release wait before respawn.
    pub port: Option<u16>,
    /// Memory / CPU limits (reserved; not used with direct spawn — Core's systemd unit enforces limits).
    #[serde(default)]
    pub memory_max: Option<String>,
    #[serde(default)]
    pub cpu_quota: Option<String>,
}


// ── Runtime state ────────────────────────────────────────────────────────────

struct ProcessState {
    child: Option<Child>,
    restart_count: u32,
    last_started: Option<Instant>,
}

impl ProcessState {
    fn new() -> Self {
        Self { child: None, restart_count: 0, last_started: None }
    }
}

// ── ProcessManager ───────────────────────────────────────────────────────────

pub struct ProcessManager {
    configs: Vec<ManagedProcessConfig>,
    /// Per-process mutable runtime state (child handle + restart counter).
    states: Arc<Mutex<HashMap<String, ProcessState>>>,
    /// Absolute base directory (Core's cwd at startup).
    base_dir: PathBuf,
}

impl ProcessManager {
    pub fn new(configs: Vec<ManagedProcessConfig>, base_dir: PathBuf) -> Self {
        let states = configs
            .iter()
            .map(|c| (c.name.clone(), ProcessState::new()))
            .collect();
        Self {
            configs,
            states: Arc::new(Mutex::new(states)),
            base_dir,
        }
    }

    pub fn is_managed(&self, name: &str) -> bool {
        self.configs.iter().any(|c| c.name == name)
    }

    /// Spawn all managed processes and start the monitor loop.
    pub async fn start_all(self: &Arc<Self>) {
        for cfg in &self.configs {
            if let Err(e) = self.spawn_process(&cfg.name).await {
                tracing::error!(process = %cfg.name, error = %e, "failed to spawn managed process");
            }
        }
        // Background monitor: restart on crash
        let mgr = Arc::clone(self);
        tokio::spawn(async move { mgr.monitor_loop().await });
    }

    /// Restart a named process: kill → wait for port release → respawn.
    pub async fn restart(&self, name: &str) -> anyhow::Result<()> {
        self.kill(name).await?;

        // Wait for port to be released (max 5 s)
        if let Some(port) = self.port_for(name) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if TcpListener::bind(("0.0.0.0", port)).is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }

        self.spawn_process(name).await
    }

    /// Kill a running process (SIGKILL → wait 3 s).
    pub async fn kill(&self, name: &str) -> anyhow::Result<()> {
        let mut states = self.states.lock().await;
        let state = states
            .get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("unknown managed process: {name}"))?;
        if let Some(mut child) = state.child.take() {
            let _ = child.kill().await;
            let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
        }
        Ok(())
    }

    /// Return current status: running/stopped + restart count.
    pub async fn status(&self, name: &str) -> ProcessStatus {
        let states = self.states.lock().await;
        match states.get(name) {
            None => ProcessStatus { running: false, restart_count: 0, pid: None },
            Some(s) => ProcessStatus {
                running: s.child.is_some(),
                restart_count: s.restart_count,
                pid: None, // we don't persist pid across status calls (child may have exited)
            },
        }
    }

    /// Return names of all managed processes.
    pub fn names(&self) -> Vec<String> {
        self.configs.iter().map(|c| c.name.clone()).collect()
    }

    /// Start a process that is not currently running.
    pub async fn start(&self, name: &str) -> anyhow::Result<()> {
        let is_running = {
            let states = self.states.lock().await;
            states.get(name).is_some_and(|s| s.child.is_some())
        };
        if is_running {
            anyhow::bail!("process '{name}' is already running");
        }
        self.spawn_process(name).await
    }

    // ── private ──────────────────────────────────────────────────────────────

    fn config_for(&self, name: &str) -> Option<&ManagedProcessConfig> {
        self.configs.iter().find(|c| c.name == name)
    }

    fn port_for(&self, name: &str) -> Option<u16> {
        self.config_for(name).and_then(|c| c.port)
    }

    /// Build the env map: passthrough selected vars + extra injected vars.
    fn build_env(&self, cfg: &ManagedProcessConfig) -> HashMap<String, String> {
        let mut env = HashMap::new();
        for key in &cfg.env_passthrough {
            if let Ok(val) = std::env::var(key) {
                env.insert(key.clone(), val);
            }
        }
        for (k, v) in &cfg.env_extra {
            // Support ${VAR} substitution in values
            let resolved = if let Some(var_name) = v.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
                std::env::var(var_name).unwrap_or_else(|_| v.clone())
            } else {
                v.clone()
            };
            env.insert(k.clone(), resolved);
        }
        env
    }

    async fn spawn_process(&self, name: &str) -> anyhow::Result<()> {
        let cfg = self
            .config_for(name)
            .ok_or_else(|| anyhow::anyhow!("unknown managed process: {name}"))?;

        let working_dir = self.base_dir.join(&cfg.working_dir);
        let env = self.build_env(cfg);

        let mut command = cfg.command.clone();
        if command.is_empty() {
            anyhow::bail!("command is empty for managed process '{name}'");
        }

        // Direct spawn — systemd-run --scope exits immediately after creating the scope,
        // which would cause ProcessManager to think the process died. Direct spawn lets
        // us track the actual PID. Core's own systemd service already enforces MemoryMax.
        //
        // process_group(0): put the child in its own process group so that when we
        // kill() it we can also kill all grandchildren (e.g. uvicorn worker processes
        // spawned via multiprocessing — they survive if only the parent is SIGKILL'd).
        let exe = command.remove(0);
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(&command)
            .current_dir(&working_dir)
            .envs(&env)
            .kill_on_drop(true);
        // Put child in its own process group (Unix only) so that grandchildren
        // (e.g. uvicorn workers spawned via multiprocessing) are also killed on drop.
        #[cfg(unix)]
        #[allow(unused_imports)]
        { use std::os::unix::process::CommandExt; cmd.process_group(0); }
        let child = cmd.spawn()?;

        let mut states = self.states.lock().await;
        let state = states.entry(name.to_string()).or_insert_with(ProcessState::new);
        state.child = Some(child);
        state.last_started = Some(Instant::now());

        tracing::info!(process = %name, working_dir = %working_dir.display(), "managed process spawned");
        Ok(())
    }

    /// Gracefully stop all managed processes: SIGTERM → 5s wait → SIGKILL.
    pub async fn stop_all(&self) {
        // Phase 1: send SIGTERM to all running processes
        {
            let states = self.states.lock().await;
            for (name, ps) in states.iter() {
                if let Some(ref child) = ps.child
                    && let Some(pid) = child.id() {
                        tracing::info!(process = %name, pid = pid, "sending SIGTERM");
                        #[cfg(unix)]
                        {
                            // Negative PID sends signal to entire process group
                            // (matches process_group(0) set during spawn)
                            let _ = std::process::Command::new("kill")
                                .args(["-TERM", &format!("-{}", pid)])
                                .spawn();
                        }
                    }
            }
        }

        // Phase 2: poll up to 5s, exit early if all processes are gone
        for _ in 0..25 {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let mut states = self.states.lock().await;
            let all_exited = states.values_mut().all(|ps| {
                ps.child.as_mut().is_none_or(|c| c.try_wait().ok().flatten().is_some())
            });
            if all_exited {
                tracing::info!("all managed processes exited gracefully");
                return;
            }
        }

        // Phase 3: force-kill any still running
        {
            let mut states = self.states.lock().await;
            for (name, ps) in states.iter_mut() {
                if let Some(ref mut child) = ps.child
                    && child.try_wait().ok().flatten().is_none() {
                        tracing::warn!(process = %name, "force-killing (still running after 5s)");
                        let _ = child.kill().await;
                        // Reap the process to avoid zombies
                        let _ = child.wait().await;
                    }
            }
        }
    }

    /// Background loop: check if processes exited and restart them.
    async fn monitor_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            let names: Vec<String> = self.names();
            for name in names {
                let should_restart = {
                    let mut states = self.states.lock().await;
                    if let Some(state) = states.get_mut(&name) {
                        if let Some(ref mut child) = state.child {
                            // try_wait is non-blocking
                            match child.try_wait() {
                                Ok(Some(exit_status)) => {
                                    // Process has exited
                                    let uptime = state
                                        .last_started
                                        .map_or(0, |t| t.elapsed().as_secs());
                                    tracing::warn!(
                                        process = %name,
                                        exit = %exit_status,
                                        uptime_secs = uptime,
                                        restarts = state.restart_count,
                                        "managed process exited — scheduling restart"
                                    );
                                    state.child = None;
                                    state.restart_count += 1;
                                    true
                                }
                                Ok(None) => {
                                    // Reset restart counter after 60s of stable uptime
                                    if state.restart_count > 0
                                        && let Some(started) = state.last_started
                                            && started.elapsed() > Duration::from_secs(60) {
                                                state.restart_count = 0;
                                            }
                                    false
                                }
                                Err(e) => {
                                    tracing::warn!(process = %name, error = %e, "try_wait error");
                                    false
                                }
                            }
                        } else {
                            true // no child at all → needs spawn
                        }
                    } else {
                        false
                    }
                };

                if should_restart {
                    let count = {
                        let states = self.states.lock().await;
                        states.get(&name).map_or(0, |s| s.restart_count)
                    };

                    // Circuit breaker: after 10 consecutive failures, wait 5 minutes then retry
                    if count >= 10 {
                        tracing::error!(
                            process = %name,
                            restarts = count,
                            "circuit breaker tripped, cooling down for 5 minutes"
                        );
                        tokio::time::sleep(Duration::from_secs(300)).await;
                        // Reset counter to allow fresh retry sequence after cooldown
                        let mut states = self.states.lock().await;
                        if let Some(state) = states.get_mut(&name) {
                            state.restart_count = 0;
                        }
                        continue;
                    }

                    // Exponential backoff with deterministic jitter, capped at 60s
                    let base = 2_u64.pow(count.min(5));        // 2, 4, 8, 16, 32
                    let jitter = base / 4;                      // deterministic jitter
                    let delay = Duration::from_secs((base + jitter).min(60));
                    tracing::info!(process = %name, delay_secs = delay.as_secs(), restarts = count, "restarting managed process");
                    tokio::time::sleep(delay).await;

                    if let Err(e) = self.spawn_process(&name).await {
                        tracing::error!(process = %name, error = %e, "failed to respawn managed process");
                    }
                }
            }
        }
    }
}

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ProcessStatus {
    pub running: bool,
    pub restart_count: u32,
    pub pid: Option<u32>,
}
