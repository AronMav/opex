//! Manages long-lived child processes (channels, toolgate) spawned by Core.
//!
//! Each managed process is:
//! - Spawned at Core startup as a DIRECT child (`tokio::process::Command`), NOT
//!   via `systemd-run --scope`. F104: the `memory_max` / `cpu_quota` fields on
//!   `[[managed_process]]` are therefore accepted-but-NOT-enforced — no cgroup
//!   limit is applied to managed children. (Core's own systemd unit may cap the
//!   whole tree, but there is no per-child MemoryMax/CPUQuota.)
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
#[allow(dead_code)] // memory_max/cpu_quota are reserved fields per the doc
                    // comments; accepted from TOML but not enforced with
                    // direct spawn (Core's systemd unit enforces limits).
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
    /// HTTP URL for health-check polling. When set, the health loop probes it
    /// every `HEALTH_PROBE_INTERVAL_SECS`; `HEALTH_FAILURE_THRESHOLD`
    /// consecutive failures kill the process so the monitor loop respawns it.
    /// Catches hung-but-alive processes that `try_wait()` cannot see.
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

/// How often the health loop probes each `health_url`.
const HEALTH_PROBE_INTERVAL_SECS: u64 = 30;
/// Consecutive probe failures before the process is killed for restart.
const HEALTH_FAILURE_THRESHOLD: u32 = 3;
/// Startup grace: no probes until the process has been up this long
/// (toolgate needs a few seconds to import its provider stack).
const HEALTH_GRACE_SECS: u64 = 30;

struct ProcessState {
    child: Option<Child>,
    restart_count: u32,
    last_started: Option<Instant>,
    /// Consecutive failed health probes (reset on success and on spawn).
    health_failures: u32,
    /// F025: earliest instant this process may be respawned. The monitor stamps
    /// it (backoff / circuit-breaker window) instead of sleeping inline, so one
    /// flapping process's cooldown never blocks detection+restart of its peers.
    next_retry_at: Option<Instant>,
    /// F097: set while an API-triggered `restart()` is mid-flight (kill → wait for
    /// port release → respawn). monitor_loop must NOT treat the transient
    /// child==None during this window as a crash and race its own duplicate spawn.
    intended_down: bool,
}

impl ProcessState {
    fn new() -> Self {
        Self { child: None, restart_count: 0, last_started: None, health_failures: 0, next_retry_at: None, intended_down: false }
    }

    /// Stamp the next allowed respawn instant based on `restart_count`
    /// (exponential backoff, or the 5-minute circuit-breaker cooldown after 10
    /// consecutive failures). Replaces the old inline `tokio::time::sleep`.
    fn schedule_retry(&mut self, name: &str) {
        let now = Instant::now();
        if self.restart_count >= 10 {
            tracing::error!(
                process = %name,
                restarts = self.restart_count,
                "circuit breaker tripped, cooling down for 5 minutes"
            );
            self.next_retry_at = Some(now + Duration::from_secs(300));
            self.restart_count = 0; // fresh sequence after cooldown
        } else {
            let base = 2_u64.pow(self.restart_count.min(5)); // 2,4,8,16,32
            let jitter = base / 4;
            let delay = Duration::from_secs((base + jitter).min(60));
            tracing::info!(process = %name, delay_secs = delay.as_secs(), restarts = self.restart_count, "scheduling managed-process restart");
            self.next_retry_at = Some(now + delay);
        }
    }
}

// ── ProcessManager ───────────────────────────────────────────────────────────

pub struct ProcessManager {
    configs: Vec<ManagedProcessConfig>,
    /// Per-process mutable runtime state (child handle + restart counter).
    states: Arc<Mutex<HashMap<String, ProcessState>>>,
    /// Absolute base directory (Core's cwd at startup).
    base_dir: PathBuf,
    /// Client for `health_url` probes — short timeouts, loopback-only targets.
    http: reqwest::Client,
    /// F024: set by `stop_all` BEFORE it kills anything, so the monitor/health
    /// loops and `spawn_process` stop respawning the very processes shutdown is
    /// tearing down. Without it the loops race stop_all and a killed process
    /// briefly comes back up mid-shutdown.
    shutting_down: std::sync::atomic::AtomicBool,
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
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(1))
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            shutting_down: std::sync::atomic::AtomicBool::new(false),
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
        // Background health probes: restart on hang (only if any process
        // actually configures a health_url).
        if self.configs.iter().any(|c| c.health_url.is_some()) {
            let mgr = Arc::clone(self);
            tokio::spawn(async move { mgr.health_loop().await });
        }
    }

    /// Restart a named process: kill → wait for port release → respawn.
    pub async fn restart(&self, name: &str) -> anyhow::Result<()> {
        // F097: mark intentionally-down BEFORE releasing any lock, so monitor_loop
        // doesn't observe the transient child==None (during kill + the 5s
        // port-release wait) as a crash and spawn a racing duplicate that then
        // holds the port and stalls our own respawn.
        {
            let mut states = self.states.lock().await;
            if let Some(state) = states.get_mut(name) {
                state.intended_down = true;
            }
        }

        let result = async {
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
        .await;

        // Clear the flag regardless of outcome — on spawn failure the monitor
        // should resume its normal crash-respawn behavior.
        {
            let mut states = self.states.lock().await;
            if let Some(state) = states.get_mut(name) {
                state.intended_down = false;
            }
        }
        result
    }

    /// Send `signal` (e.g. "-KILL") to the child's WHOLE process group (F089).
    /// `process_group(0)` at spawn makes the child a group leader (PGID == PID),
    /// so `kill <signal> -<pid>` reaches grandchildren (uvicorn workers, adapter
    /// subprocesses) too — unlike tokio `Child::kill`, which SIGKILLs only the
    /// direct child PID and orphans grandchildren that keep holding the port.
    #[cfg(unix)]
    async fn kill_process_group(child: &mut tokio::process::Child, signal: &str) {
        if let Some(pid) = child.id() {
            let _ = tokio::process::Command::new("kill")
                .args([signal, &format!("-{pid}")])
                .status()
                .await;
        } else {
            let _ = child.kill().await;
        }
    }
    #[cfg(not(unix))]
    async fn kill_process_group(child: &mut tokio::process::Child, _signal: &str) {
        let _ = child.kill().await;
    }

    /// Kill a running process (SIGKILL → wait 3 s).
    pub async fn kill(&self, name: &str) -> anyhow::Result<()> {
        let mut states = self.states.lock().await;
        let state = states
            .get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("unknown managed process: {name}"))?;
        if let Some(mut child) = state.child.take() {
            // F089: group-kill so grandchildren die too (restart/health-kill path).
            Self::kill_process_group(&mut child, "-KILL").await;
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
        // F024: never (re)spawn once shutdown has begun — otherwise a monitor
        // respawn races stop_all and a killed process comes back up.
        if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("process manager is shutting down; refusing to spawn '{name}'");
        }
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
        state.health_failures = 0;

        tracing::info!(process = %name, working_dir = %working_dir.display(), "managed process spawned");
        Ok(())
    }

    /// Gracefully stop all managed processes: SIGTERM → 5s wait → SIGKILL.
    pub async fn stop_all(&self) {
        // F024: flip the shutdown flag FIRST so the monitor/health loops (and
        // any in-flight restart) stop respawning what we're about to kill.
        self.shutting_down.store(true, std::sync::atomic::Ordering::SeqCst);
        // Phase 1: send SIGTERM to all running processes
        {
            let states = self.states.lock().await;
            for (name, ps) in states.iter() {
                if let Some(ref child) = ps.child
                    && let Some(pid) = child.id() {
                        tracing::info!(process = %name, pid = pid, "sending SIGTERM");
                        #[cfg(unix)]
                        {
                            // Negative PID sends signal to the entire process group
                            // (matches process_group(0) set during spawn).
                            // F127: capture the result + reap the helper — the old
                            // fire-and-forget `let _ = ...spawn()` silently skipped
                            // graceful termination if `kill` failed to launch and
                            // left a short-lived zombie.
                            match tokio::process::Command::new("kill")
                                .args(["-TERM", &format!("-{}", pid)])
                                .status()
                                .await
                            {
                                Ok(s) if s.success() => {}
                                Ok(s) => tracing::warn!(process = %name, pid, code = ?s.code(), "SIGTERM (kill) returned non-zero"),
                                Err(e) => tracing::warn!(process = %name, pid, error = %e, "failed to send SIGTERM (kill spawn failed)"),
                            }
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
                        // F089: SIGKILL the whole group, not just the direct child.
                        Self::kill_process_group(child, "-KILL").await;
                        // Reap the process to avoid zombies
                        let _ = child.wait().await;
                    }
            }
        }
    }

    /// Background loop: probe `health_url` of each configured process and
    /// kill it after `HEALTH_FAILURE_THRESHOLD` consecutive failures — the
    /// monitor loop then respawns it with the usual backoff. Complements
    /// `monitor_loop`, which only detects *exited* processes; this catches
    /// hung-but-alive ones (e.g. a wedged uvicorn event loop).
    async fn health_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(HEALTH_PROBE_INTERVAL_SECS));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if self.shutting_down.load(std::sync::atomic::Ordering::SeqCst) {
                break; // F024: don't health-kill (→ respawn) during shutdown
            }
            for cfg in self.configs.iter() {
                let Some(url) = cfg.health_url.as_deref() else { continue };

                // Probe only a process that is running and past the startup
                // grace window; a dead child is monitor_loop's business.
                let eligible = {
                    let states = self.states.lock().await;
                    states.get(&cfg.name).is_some_and(|s| {
                        s.child.is_some()
                            && s.last_started.is_some_and(|t| {
                                t.elapsed() >= Duration::from_secs(HEALTH_GRACE_SECS)
                            })
                    })
                };
                if !eligible {
                    continue;
                }

                // HTTP GET outside the lock — a slow probe must not block
                // status/restart/kill callers.
                let healthy = match self.http.get(url).send().await {
                    Ok(resp) => resp.status().is_success(),
                    Err(_) => false,
                };

                let should_kill = {
                    let mut states = self.states.lock().await;
                    let Some(state) = states.get_mut(&cfg.name) else { continue };
                    // Exited (or was killed) while we probed — leave it to
                    // monitor_loop; a stale probe result must not count.
                    if state.child.is_none() {
                        continue;
                    }
                    if healthy {
                        if state.health_failures > 0 {
                            tracing::info!(process = %cfg.name, "health probe recovered");
                        }
                        state.health_failures = 0;
                        false
                    } else {
                        state.health_failures += 1;
                        tracing::warn!(
                            process = %cfg.name,
                            url = %url,
                            failures = state.health_failures,
                            threshold = HEALTH_FAILURE_THRESHOLD,
                            "health probe failed"
                        );
                        if state.health_failures >= HEALTH_FAILURE_THRESHOLD {
                            state.health_failures = 0;
                            // Count toward the restart backoff / circuit breaker.
                            state.restart_count += 1;
                            true
                        } else {
                            false
                        }
                    }
                };

                if should_kill {
                    tracing::error!(
                        process = %cfg.name,
                        "health probes exhausted — killing hung process for restart"
                    );
                    // kill() takes the states lock itself; monitor_loop sees
                    // child == None on its next tick and respawns with backoff.
                    if let Err(e) = self.kill(&cfg.name).await {
                        tracing::warn!(process = %cfg.name, error = %e, "health-kill failed");
                    }
                }
            }
        }
    }

    /// Background loop: check if processes exited and restart them.
    ///
    /// F025: the per-process backoff / circuit-breaker is a `next_retry_at`
    /// timestamp (stamped by `schedule_retry`), NOT an inline sleep — so a
    /// flapping / cooling-down process is simply skipped this tick and does not
    /// block detection+restart of its healthy-but-crashed peers.
    async fn monitor_loop(self: Arc<Self>) {
        use std::sync::atomic::Ordering;
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        loop {
            interval.tick().await;
            if self.shutting_down.load(Ordering::SeqCst) {
                break; // F024: don't respawn during shutdown
            }
            let names: Vec<String> = self.names();
            for name in names {
                if self.shutting_down.load(Ordering::SeqCst) {
                    break;
                }
                // Decide under the lock whether this process is due to spawn now.
                let spawn_now = {
                    let mut states = self.states.lock().await;
                    let Some(state) = states.get_mut(&name) else { continue };
                    if let Some(ref mut child) = state.child {
                        match child.try_wait() {
                            Ok(Some(exit_status)) => {
                                let uptime = state.last_started.map_or(0, |t| t.elapsed().as_secs());
                                tracing::warn!(
                                    process = %name,
                                    exit = %exit_status,
                                    uptime_secs = uptime,
                                    restarts = state.restart_count,
                                    "managed process exited — scheduling restart"
                                );
                                state.child = None;
                                state.restart_count += 1;
                                // Stamp the cooldown; the actual respawn happens on
                                // a later tick once next_retry_at has elapsed.
                                state.schedule_retry(&name);
                                false
                            }
                            Ok(None) => {
                                // Reset restart counter after 60s of stable uptime.
                                if state.restart_count > 0
                                    && let Some(started) = state.last_started
                                    && started.elapsed() > Duration::from_secs(60)
                                {
                                    state.restart_count = 0;
                                    state.next_retry_at = None;
                                }
                                false
                            }
                            Err(e) => {
                                tracing::warn!(process = %name, error = %e, "try_wait error");
                                false
                            }
                        }
                    } else if state.intended_down {
                        // F097: an API restart() owns this process right now
                        // (kill → port-wait → respawn). Don't race a duplicate.
                        false
                    } else {
                        // No child → needs spawn, but only once the cooldown has
                        // elapsed (skip-not-sleep so peers aren't blocked).
                        match state.next_retry_at {
                            Some(due) if Instant::now() < due => false,
                            _ => {
                                state.next_retry_at = None;
                                true
                            }
                        }
                    }
                };

                if spawn_now
                    && let Err(e) = self.spawn_process(&name).await
                {
                    tracing::error!(process = %name, error = %e, "failed to respawn managed process");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f025_schedule_retry_stamps_backoff_without_sleeping() {
        // schedule_retry must return immediately (no inline sleep) and stamp a
        // future next_retry_at — the property that keeps one process's cooldown
        // from blocking its peers.
        let mut s = ProcessState::new();
        s.restart_count = 1;
        let before = Instant::now();
        s.schedule_retry("t");
        assert!(before.elapsed() < Duration::from_millis(50), "must not sleep inline");
        assert!(s.next_retry_at.is_some_and(|d| d > Instant::now()), "must stamp a future retry");
    }

    #[test]
    fn f025_circuit_breaker_resets_count_and_sets_long_cooldown() {
        let mut s = ProcessState::new();
        s.restart_count = 10; // trips the breaker
        s.schedule_retry("t");
        assert_eq!(s.restart_count, 0, "circuit breaker resets restart_count for a fresh sequence");
        let due = s.next_retry_at.expect("cooldown stamped");
        assert!(
            due > Instant::now() + Duration::from_secs(250),
            "circuit-breaker cooldown should be ~5 minutes"
        );
    }
}
