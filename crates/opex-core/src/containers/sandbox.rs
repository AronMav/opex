/// Unified execution environment using per-agent persistent Docker containers.
/// All agents share the same host `workspace/` directory mounted at `/workspace`.
use anyhow::{Context, Result};
use bollard::container::{
    Config, CreateContainerOptions, InspectContainerOptions, LogOutput,
};
use bollard::image::CreateImageOptions;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::HostConfig;
use bollard::Docker;
use futures_util::StreamExt;

use crate::config::SandboxConfig;
use crate::oauth::OAuthManager;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i64,
}

// ── Sandbox ───────────────────────────────────────────────────────────────────

pub struct CodeSandbox {
    docker: Docker,
    image: String,
    memory_bytes: i64,
    /// Hard CPU cap in nano-CPUs (1 CPU = 1_000_000_000). Zero means unlimited.
    nano_cpus: i64,
    extra_binds: Vec<String>,
    timeout_secs: u64,
}

impl CodeSandbox {
    pub fn new(docker_url: &str, cfg: &SandboxConfig) -> Result<Self> {
        let docker = crate::containers::connect_docker(docker_url)?;
        Ok(Self {
            docker,
            image: cfg.image.clone(),
            memory_bytes: i64::from(cfg.memory_mb.max(512)) * 1024 * 1024,
            nano_cpus: (cfg.cpu_limit * 1_000_000_000.0) as i64,
            extra_binds: cfg.extra_binds.clone(),
            timeout_secs: cfg.timeout_secs,
        })
    }

    /// Hard wall-clock execution limit (seconds). Used to scope the codemode
    /// capability-token TTL to the actual maximum run length.
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    /// Sanitize agent name for use as a Docker container name.
    fn container_name(&self, agent_id: &str) -> String {
        container_name_for(agent_id)
    }

    /// Pull the configured image if it's not present locally.
    async fn pull_image_if_missing(&self) -> Result<()> {
        tracing::info!(image = %self.image, "checking if image exists locally...");
        
        let exists = self.docker.inspect_image(&self.image).await.is_ok();

        if !exists {
            tracing::info!(image = %self.image, "image not found, pulling from registry...");
            let pull_future = async {
                let mut stream = self.docker.create_image(
                    Some(CreateImageOptions {
                        from_image: self.image.clone(),
                        ..Default::default()
                    }),
                    None,
                    None,
                );

                while let Some(pull_result) = stream.next().await {
                    match pull_result {
                        Ok(info) => {
                            if let Some(status) = info.status {
                                tracing::debug!(image = %self.image, status = %status, "pulling...");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(image = %self.image, error = %e, "error during image pull");
                        }
                    }
                }
            };

            match tokio::time::timeout(std::time::Duration::from_secs(300), pull_future).await {
                Ok(()) => tracing::info!(image = %self.image, "image pull complete"),
                Err(_) => anyhow::bail!("image pull timed out after 5 min: {}", self.image),
            }
        }
        Ok(())
    }

    /// Collect git-related environment variables from OAuth bindings.
    /// For each binding to a git-capable provider, injects {PROVIDER}_`GIT_TOKEN` and {PROVIDER}_`GIT_HOST`.
    /// Also sets `GIT_AUTHOR_NAME/EMAIL` and `GIT_COMMITTER_NAME/EMAIL` from userinfo.
    pub async fn collect_git_env(&self, agent_id: &str, oauth: Option<&OAuthManager>) -> Vec<String> {
        let mut env = Vec::new();
        let Some(oauth) = oauth else { return env };

        let bindings = match oauth.list_bindings(agent_id).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(agent = %agent_id, error = %e, "sandbox: failed to list OAuth bindings for git env");
                return env;
            }
        };

        let mut git_name: Option<String> = None;
        let mut git_email: Option<String> = None;

        for binding in &bindings {
            let provider = binding.get("provider").and_then(|v| v.as_str()).unwrap_or("");
            let status = binding.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status != "connected" { continue; }

            let provider_cfg = crate::oauth::find_provider(provider);
            let Some(git_host) = provider_cfg.and_then(|p| p.git_host) else { continue };

            // Get access token from vault
            match oauth.get_token(provider, agent_id).await {
                Ok(token) => {
                    let upper = provider.to_uppercase();
                    env.push(format!("{upper}_GIT_TOKEN={token}"));
                    env.push(format!("{upper}_GIT_HOST={git_host}"));
                    tracing::info!(agent = %agent_id, provider = %provider, "sandbox: injected git credentials");
                }
                Err(e) => {
                    tracing::warn!(agent = %agent_id, provider = %provider, error = %e, "sandbox: failed to get OAuth token for git");
                }
            }

            // Git identity from first git-capable provider
            if git_name.is_none() {
                if let Some(name) = binding.get("display_name").and_then(|v| v.as_str()) {
                    git_name = Some(name.to_string());
                }
                if let Some(email) = binding.get("user_email").and_then(|v| v.as_str()) {
                    git_email = Some(email.to_string());
                }
            }
        }

        if let Some(name) = git_name {
            env.push(format!("GIT_AUTHOR_NAME={name}"));
            env.push(format!("GIT_COMMITTER_NAME={name}"));
        }
        if let Some(email) = git_email {
            env.push(format!("GIT_AUTHOR_EMAIL={email}"));
            env.push(format!("GIT_COMMITTER_EMAIL={email}"));
        }

        env
    }

    /// Ensure the agent's persistent container is created and running.
    /// `base`: if true, mount service source dirs (toolgate/, channels/) for editing.
    /// `oauth`: if provided, git credentials from OAuth bindings are injected as env vars.
    pub async fn ensure_container(&self, agent_id: &str, workspace_host_path: &str, base: bool, oauth: Option<&OAuthManager>) -> Result<String> {
        // Timeout the entire container setup to avoid hanging if Docker daemon is stuck
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            self.ensure_container_inner(agent_id, workspace_host_path, base, oauth),
        )
        .await
        .map_err(|_| anyhow::anyhow!("Docker container setup timed out after 30s"))?
    }

    async fn ensure_container_inner(&self, agent_id: &str, workspace_host_path: &str, base: bool, oauth: Option<&OAuthManager>) -> Result<String> {
        let name = self.container_name(agent_id);

        if let Ok(inspect) = self.docker.inspect_container(&name, None::<InspectContainerOptions>).await {
            let state = inspect.state.context("missing container state")?;
            if state.running.unwrap_or(false) {
                return Ok(name);
            }
            // Not running? Start it.
            self.docker.start_container::<String>(&name, None).await?;
            Ok(name)
        } else {
            let git_env = self.collect_git_env(agent_id, oauth).await;
            // Try to create, pull image if missing
            let res = self.create_and_start_container(&name, workspace_host_path, base, &git_env).await;

            if let Err(e) = res {
                let err_str = e.to_string();
                if err_str.contains("No such image") || err_str.contains("404") {
                    tracing::info!("image missing, attempting to pull...");
                    self.pull_image_if_missing().await?;
                    // Retry creation after pull
                    self.create_and_start_container(&name, workspace_host_path, base, &git_env).await?;
                    Ok(name)
                } else {
                    Err(e)
                }
            } else {
                Ok(name)
            }
        }
    }

    /// Recreate an agent's container with fresh env vars (e.g. after OAuth change).
    pub async fn restart_container(&self, agent_id: &str, workspace_host_path: &str, base: bool, oauth: Option<&OAuthManager>) -> Result<()> {
        let name = self.container_name(agent_id);
        tracing::info!(agent = %agent_id, container = %name, "restarting sandbox with fresh credentials");
        // Force remove existing
        let _ = self.docker.remove_container(
            &name,
            Some(bollard::container::RemoveContainerOptions { force: true, ..Default::default() })
        ).await;
        // Recreate with new env
        let git_env = self.collect_git_env(agent_id, oauth).await;
        self.create_and_start_container(&name, workspace_host_path, base, &git_env).await
    }

    async fn create_and_start_container(&self, name: &str, workspace_host_path: &str, base: bool, git_env: &[String]) -> Result<()> {
        tracing::info!(container = %name, "creating persistent agent container");
        
        let mut binds = vec![format!("{}:/workspace", workspace_host_path)];

        // Mount service source dirs (toolgate, channels) ONLY for base agents.
        // Non-base agents must not have direct filesystem access to service code;
        // they can only edit allowed subdirs via workspace_write (which checks is_read_only).
        if base
            && let Some(root) = std::path::Path::new(workspace_host_path).parent() {
                let toolgate_dir = root.join("docker/toolgate");
                if toolgate_dir.exists() {
                    binds.push(format!("{}:/toolgate", toolgate_dir.display()));
                }
                let channels_dir = root.join("docker/channels");
                if channels_dir.exists() {
                    binds.push(format!("{}:/channels", channels_dir.display()));
                }
            }

        // Extra binds from config
        // Blocked: sensitive host paths that could leak secrets or compromise the host.
        // BLOCKED_PREFIXES is checked against the CANONICAL path so a symlink
        // (e.g. /var/run -> /run on Debian/Ubuntu) cannot bypass via the
        // alternative spelling. Audit 2026-05-08.
        const BLOCKED_PREFIXES: &[&str] = &[
            "/etc/shadow", "/etc/passwd", "/root", "/home",
            "/proc", "/sys", "/dev", "/run", "/var/run",
        ];
        const BLOCKED_FILES: &[&str] = &[
            "/var/run/docker.sock", "/run/docker.sock",
        ];
        let ws_path = std::path::Path::new(workspace_host_path);
        let project_root = ws_path.parent(); // workspace parent = project root
        for bind in &self.extra_binds {
            if let Some((src, _dst)) = bind.split_once(':') {
                let src_path = std::path::Path::new(src);
                let abs_path: std::path::PathBuf = if src_path.is_relative() {
                    project_root.map_or_else(
                        || src_path.to_path_buf(),
                        |r| r.join(src_path),
                    )
                } else {
                    src_path.to_path_buf()
                };
                let canonical = std::fs::canonicalize(&abs_path).unwrap_or(abs_path.clone());
                let canonical_str = canonical.to_string_lossy();
                if BLOCKED_PREFIXES.iter().any(|p| canonical_str.starts_with(p))
                    || BLOCKED_FILES.iter().any(|f| canonical_str == *f)
                {
                    tracing::warn!(bind = %bind, canonical = %canonical_str, "sandbox: blocked sensitive bind mount");
                    continue;
                }
                if src_path.is_relative()
                    && let Some(root) = project_root {
                        let resolved = root.join(src_path);
                        binds.push(format!("{}:{}", resolved.display(), _dst));
                        continue;
                    }
            }
            binds.push(bind.clone());
        }

        // Synchronize time with host
        if std::path::Path::new("/etc/localtime").exists() {
            binds.push("/etc/localtime:/etc/localtime:ro".to_string());
        }
        if std::path::Path::new("/etc/timezone").exists() {
            binds.push("/etc/timezone:/etc/timezone:ro".to_string());
        }
        
        // Git credential env vars (from OAuth bindings)
        let env: Option<Vec<String>> = if git_env.is_empty() {
            None
        } else {
            Some(git_env.to_vec())
        };

        // Only base agents get network access to the opex Docker network.
        // Non-base agents run fully network-isolated to prevent access to
        // internal infrastructure (postgres, toolgate, searxng, etc.).
        let network_mode = if base { Some("opex".to_string()) } else { None };

        // Base agents get host.docker.internal → host-gateway so codemode
        // (tools-as-code) scripts can reach core's loopback endpoints
        // (/api/sandbox/tool-call). On Linux Docker, this DNS name is not
        // defined by default; the extra_hosts entry maps it to the host.
        let extra_hosts = if base {
            Some(vec!["host.docker.internal:host-gateway".to_string()])
        } else {
            None
        };

        self.docker.create_container(
            Some(CreateContainerOptions { name: name.to_string(), ..Default::default() }),
            Config {
                image: Some(self.image.clone()),
                env,
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                network_disabled: Some(!base),
                working_dir: Some("/workspace".to_string()),
                user: Some("1000:1000".to_string()),
                host_config: Some(HostConfig {
                    memory: Some(self.memory_bytes),
                    nano_cpus: if self.nano_cpus > 0 { Some(self.nano_cpus) } else { None },
                    network_mode,
                    extra_hosts,
                    binds: Some(binds),
                    // Audit 2026-05-08 sandbox hardening:
                    // * pids_limit caps the number of processes inside the
                    //   container so a fork-bomb cannot exhaust host PIDs.
                    // * security_opt: no-new-privileges blocks setuid/setgid
                    //   binaries inside the image from escalating to root
                    //   even if the image carries them.
                    // * cap_drop: ALL — drop the default Docker capabilities
                    //   (CHOWN, NET_RAW, SYS_CHROOT, KILL, …). The sandbox
                    //   image runs Python; none of the default caps are
                    //   required for normal interpreted code execution.
                    pids_limit: Some(256),
                    security_opt: Some(vec!["no-new-privileges:true".to_string()]),
                    cap_drop: Some(vec!["ALL".to_string()]),
                    restart_policy: Some(bollard::models::RestartPolicy {
                        name: Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }
        ).await?;

        self.docker.start_container::<String>(name, None).await?;
        Ok(())
    }

    /// Force remove an agent's container.
    pub async fn remove_container(&self, agent_id: &str) -> Result<()> {
        let name = self.container_name(agent_id);
        tracing::info!(agent = %agent_id, container = %name, "removing agent container");
        let _ = self.docker.remove_container(
            &name,
            Some(bollard::container::RemoveContainerOptions { force: true, ..Default::default() })
        ).await;
        Ok(())
    }

    /// Execute code or shell command inside the agent's persistent container.
    pub async fn execute(
        &self,
        agent_id: &str,
        code: &str,
        language: &str,
        packages: &[String],
        workspace_host_path: &str,
        base: bool,
    ) -> Result<ExecResult> {
        let container_name = self.ensure_container(agent_id, workspace_host_path, base, None).await?;
        
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, code.as_bytes());

        let run_cmd = match language {
            "bash" | "sh" | "shell" => {
                if code.contains('\n') || language == "bash" {
                    format!("echo '{b64}' | base64 -d | bash")
                } else {
                    format!("echo '{b64}' | base64 -d | sh")
                }
            }
            _ => {
                let pip_part = if packages.is_empty() {
                    String::new()
                } else {
                    // Validate package names to prevent shell injection.
                    // Allow version specifiers: >= <= == != ~= , but block shell metacharacters
                    for pkg in packages {
                        if !pkg.chars().all(|c| c.is_ascii_alphanumeric()
                            || "._-[],>=<!~".contains(c)) {
                            return Ok(ExecResult {
                                stdout: String::new(),
                                stderr: format!("Invalid package spec: '{pkg}' (shell metacharacters not allowed)"),
                                exit_code: 1,
                            });
                        }
                        // Reject shell redirection patterns (e.g. "> /etc/evil", "< /etc/passwd")
                        if pkg.contains("> ") || pkg.contains("< ") || pkg.contains("| ") || pkg.contains("; ") || pkg.contains("` ") {
                            return Ok(ExecResult {
                                stdout: String::new(),
                                stderr: format!("Invalid package spec: '{pkg}' (shell metacharacters not allowed)"),
                                exit_code: 1,
                            });
                        }
                    }
                    format!("pip install {} -q --user --disable-pip-version-check 2>&1 && ",
                        packages.iter().map(|p| format!("'{}'", p.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" "))
                };
                format!(
                    "{pip_part}echo '{b64}' | base64 -d > /tmp/s.py && python3 /tmp/s.py"
                )
            }
        };

        // F012: wrap the in-container command with coreutils `timeout -s KILL`
        // so a runaway process (e.g. `while True: pass`) is actually killed
        // rather than left running forever inside the persistent
        // (restart=unless-stopped) container — the old tokio-only timeout just
        // dropped the attach stream and leaked the process, stacking zombies
        // that pinned CPU/memory across reused executions. Race-free: the
        // in-container guard self-terminates; the outer tokio timeout is only a
        // backstop for a stalled docker daemon, so it must be strictly larger.
        let base_secs = self.timeout_secs.max(5);
        let hard_kill = format!("{base_secs}s");
        let exec = self.docker.create_exec(
            &container_name,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(timeout_wrapped_cmd(&hard_kill, &run_cmd)),
                ..Default::default()
            },
        ).await?;

        // Start and collect output with timeout + size limit
        let timeout = std::time::Duration::from_secs(base_secs + 10);
        const MAX_OUTPUT_BYTES: usize = 1024 * 1024; // 1 MB
        let collect = async {
            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut total_bytes: usize = 0;
            let start_result = self.docker.start_exec(&exec.id, None).await?;
            if let StartExecResults::Attached { mut output, .. } = start_result {
                while let Some(msg) = output.next().await {
                    match msg {
                        Ok(LogOutput::StdOut { message }) => {
                            total_bytes += message.len();
                            if total_bytes > MAX_OUTPUT_BYTES {
                                stderr.push_str("\n[output truncated at 1MB]");
                                break;
                            }
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(LogOutput::StdErr { message }) => {
                            total_bytes += message.len();
                            if total_bytes > MAX_OUTPUT_BYTES {
                                stderr.push_str("\n[output truncated at 1MB]");
                                break;
                            }
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }
            // If output was truncated, return exit code 1 without waiting for the process
            let exit_code = if total_bytes > MAX_OUTPUT_BYTES {
                1
            } else {
                let inspect = self.docker.inspect_exec(&exec.id).await?;
                inspect.exit_code.unwrap_or(0)
            };
            Ok::<_, anyhow::Error>(ExecResult { stdout, stderr, exit_code })
        };
        match tokio::time::timeout(timeout, collect).await {
            Ok(result) => result,
            Err(_) => Ok(ExecResult {
                stdout: String::new(),
                stderr: format!("Execution timed out after {}s", self.timeout_secs),
                exit_code: 124,
            }),
        }
    }

    /// Execute Python code with injected environment variables + SDK preamble.
    ///
    /// Used by `code_orchestrate` (codemode) to run a script that calls back
    /// into core via the loopback `/api/sandbox/tool-call` endpoint. The `env`
    /// pairs are set on the `docker exec` call (bollard `CreateExecOptions.env`).
    pub async fn execute_with_sdk(
        &self,
        agent_id: &str,
        code: &str,
        language: &str,
        env: &[(String, String)],
        workspace_host_path: &str,
        base: bool,
    ) -> Result<ExecResult> {
        let container_name = self
            .ensure_container(agent_id, workspace_host_path, base, None)
            .await?;

        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, code.as_bytes());

        let run_cmd = match language {
            "bash" | "sh" | "shell" => format!("echo '{b64}' | base64 -d | bash"),
            _ => format!("echo '{b64}' | base64 -d > /tmp/s.py && python3 /tmp/s.py"),
        };

        let env_vec: Vec<String> = env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        let env_refs: Vec<&str> = env_vec.iter().map(String::as_str).collect();

        // F012: same in-container `timeout -s KILL` wrapper as `execute` so a
        // runaway codemode script is killed, not leaked into the persistent
        // container. Outer tokio timeout is a strictly-larger docker backstop.
        let base_secs = self.timeout_secs.max(5);
        let hard_kill = format!("{base_secs}s");
        let exec = self
            .docker
            .create_exec(
                &container_name,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(timeout_wrapped_cmd(&hard_kill, &run_cmd)),
                    env: Some(env_refs),
                    ..Default::default()
                },
            )
            .await?;

        let timeout = std::time::Duration::from_secs(base_secs + 10);
        const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
        let collect = async {
            let mut stdout = String::new();
            let mut stderr = String::new();
            let mut total_bytes: usize = 0;
            let start_result = self.docker.start_exec(&exec.id, None).await?;
            if let StartExecResults::Attached { mut output, .. } = start_result {
                while let Some(msg) = output.next().await {
                    match msg {
                        Ok(LogOutput::StdOut { message }) => {
                            total_bytes += message.len();
                            if total_bytes > MAX_OUTPUT_BYTES {
                                stderr.push_str("\n[output truncated at 1MB]");
                                break;
                            }
                            stdout.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(LogOutput::StdErr { message }) => {
                            total_bytes += message.len();
                            if total_bytes > MAX_OUTPUT_BYTES {
                                stderr.push_str("\n[output truncated at 1MB]");
                                break;
                            }
                            stderr.push_str(&String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }
            let exit_code = if total_bytes > MAX_OUTPUT_BYTES {
                1
            } else {
                let inspect = self.docker.inspect_exec(&exec.id).await?;
                inspect.exit_code.unwrap_or(0)
            };
            Ok::<_, anyhow::Error>(ExecResult { stdout, stderr, exit_code })
        };
        match tokio::time::timeout(timeout, collect).await {
            Ok(result) => result,
            Err(_) => Ok(ExecResult {
                stdout: String::new(),
                stderr: format!("Execution timed out after {}s", self.timeout_secs),
                exit_code: 124,
            }),
        }
    }
}

/// Per-agent Docker container name. Pure free function so collision-freedom is
/// unit-testable.
///
/// F011: the readable prefix folds case and non-alphanumerics (`Ops`→`ops`,
/// `a.b`→`a_b`), so distinct agent ids could map onto ONE container — letting a
/// restricted agent reuse a base agent's networked, credential-bearing sandbox.
/// A short hash of the EXACT, case/punctuation-preserved agent id is appended
/// so the name is collision-free while staying stable (same id → same
/// container) and human-readable.
fn container_name_for(agent_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let sanitized: String = agent_id
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let digest = Sha256::digest(agent_id.as_bytes());
    let short = hex::encode(&digest[..4]); // 8 hex chars — collision-resistant enough for names
    format!("hc-agent-{}-{}", sanitized.to_lowercase(), short)
}

/// Build the `create_exec` argv, wrapping the shell command with an
/// in-container coreutils `timeout -s KILL` guard (F012). Kept as a pure free
/// function so the wrapper can't be silently dropped without failing a test —
/// a runaway process must be killed inside the persistent container, not left
/// leaking CPU/memory across reused executions.
fn timeout_wrapped_cmd<'a>(hard_kill: &'a str, run_cmd: &'a str) -> Vec<&'a str> {
    vec!["timeout", "-s", "KILL", hard_kill, "sh", "-c", run_cmd]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_name_is_collision_free_across_case_and_punctuation() {
        // F011: agent ids that fold to the same sanitized/lowercased string
        // must NOT share a container name.
        assert_ne!(container_name_for("Ops"), container_name_for("ops"));
        assert_ne!(container_name_for("a.b"), container_name_for("a_b"));
        // Stable: same id → same name (container reuse must keep working).
        assert_eq!(container_name_for("ops"), container_name_for("ops"));
        // Readable prefix retained.
        assert!(container_name_for("Ops").starts_with("hc-agent-ops-"), "{}", container_name_for("Ops"));
    }

    #[test]
    fn timeout_wrapped_cmd_prefixes_coreutils_timeout() {
        // F012 regression guard: the exec argv MUST start with a hard-kill
        // `timeout` so a `while True: pass` self-terminates in-container.
        let cmd = timeout_wrapped_cmd("30s", "echo hi && python3 /tmp/s.py");
        assert_eq!(cmd[0], "timeout");
        assert_eq!(&cmd[1..4], ["-s", "KILL", "30s"]);
        assert_eq!(&cmd[4..6], ["sh", "-c"]);
        assert_eq!(cmd[6], "echo hi && python3 /tmp/s.py");
    }
}
