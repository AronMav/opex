//! `CliRunner` — manages CLI execution with sessions, timeouts,
//! serialized concurrency, and circuit-breaker cooldown.
//!
//! Two execution paths:
//! - `execute_on_host` — direct `tokio::process::Command` for `base = true`
//!   agents that must run the CLI on the host (no Docker available).
//! - sandbox path — delegates to [`CodeSandbox`] which runs the CLI
//!   inside the agent's Docker container.
//!
//! [`shell_escape`] is a private helper used by the sandbox path to
//! safely build a single shell-string from argv.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::{Mutex, RwLock, Semaphore};

use super::cooldown::{CliErrorReason, CooldownState, classify_cli_error};
use super::parser::{parse_cli_json, parse_cli_jsonl};
use super::{CliBackendConfig, CliInputMode, CliOutput, CliOutputFormat, CliSessionMode, SystemPromptWhen};
use crate::containers::sandbox::{CodeSandbox, ExecResult};

/// Manages CLI execution with sessions, timeouts, serialization, and cooldown.
pub struct CliRunner {
    config: CliBackendConfig,
    sessions: RwLock<HashMap<String, String>>,
    session_hashes: RwLock<HashMap<String, u64>>,
    semaphore: Semaphore,
    cooldown: Mutex<CooldownState>,
}

impl CliRunner {
    pub fn new(config: CliBackendConfig) -> Self {
        let permits = if config.serialize { 1 } else { 64 };
        Self {
            config,
            sessions: RwLock::new(HashMap::new()),
            session_hashes: RwLock::new(HashMap::new()),
            semaphore: Semaphore::new(permits),
            cooldown: Mutex::new(CooldownState::new()),
        }
    }

    /// Check if context hash changed; if so, clear stored session for this agent.
    /// Returns true if session was invalidated.
    pub async fn check_and_invalidate_session(&self, agent_name: &str, context_hash: u64) -> bool {
        // Decide invalidation under `session_hashes`, then DROP it before
        // taking the `sessions` write lock — never hold both across an await
        // (avoids a lock-ordering hazard and stops blocking concurrent
        // hash-checkers for the duration of the sessions write).
        let invalidate = {
            let mut hashes = self.session_hashes.write().await;
            let prev = hashes.insert(agent_name.to_string(), context_hash);
            prev.is_some_and(|prev_hash| prev_hash != context_hash)
        };
        if invalidate {
            self.sessions.write().await.remove(agent_name);
            tracing::info!(agent = %agent_name, "CLI session invalidated: context hash changed");
        }
        invalidate
    }

    /// Execute CLI with prompt, returning parsed response.
    /// `extra_env` is merged on top of `self.config.env` (e.g. vault-resolved API keys).
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        agent_name: &str,
        prompt: &str,
        system_prompt: Option<&str>,
        model: &str,
        sandbox: Option<&CodeSandbox>,
        workspace_dir: &str,
        base: bool,
        extra_env: &HashMap<String, String>,
    ) -> Result<CliOutput> {
        // Check cooldown before acquiring permit
        {
            let mut cd = self.cooldown.lock().await;
            if let Some(remaining) = cd.is_in_cooldown() {
                anyhow::bail!(
                    "CLI provider in cooldown ({:?} remaining, reason: {:?}, {} consecutive errors)",
                    remaining, cd.last_reason, cd.error_count
                );
            }
        }

        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("CLI semaphore closed"))?;

        // Resolve model alias
        let resolved_model = self
            .config
            .model_aliases
            .get(model)
            .map_or(model, std::string::String::as_str);

        // Check for existing session
        let existing_session = self.sessions.read().await.get(agent_name).cloned();
        let use_resume = existing_session.is_some() && !self.config.resume_args.is_empty();

        let (argv, use_stdin) = self.build_argv(
            resolved_model,
            prompt,
            system_prompt,
            existing_session.as_deref(),
            use_resume,
        );

        let stdin_input = if use_stdin { Some(prompt) } else { None };
        let timeout = Duration::from_secs(self.config.timeout_secs);

        // Execute
        let start = Instant::now();
        tracing::debug!(agent = %agent_name, argv = ?argv, use_stdin, workspace = %workspace_dir, base, "CLI executing");
        // Merge config env with extra_env (vault secrets override config)
        let merged_env = if extra_env.is_empty() {
            self.config.env.clone()
        } else {
            let mut env = self.config.env.clone();
            env.extend(extra_env.iter().map(|(k, v)| (k.clone(), v.clone())));
            env
        };

        let exec_result = if base {
            // Base agents always run CLI on host (not in Docker sandbox)
            execute_on_host(&argv, &merged_env, workspace_dir, timeout, &self.config.clear_env, stdin_input, self.config.no_output_timeout_secs).await
        } else if let Some(sb) = sandbox {
            let base_cmd = argv.iter().map(|a| shell_escape(a)).collect::<Vec<_>>().join(" ");
            let cmd = if let Some(input) = stdin_input {
                format!("echo {} | {}", shell_escape(input), base_cmd)
            } else {
                base_cmd
            };
            let host_path = std::fs::canonicalize(workspace_dir)
                .unwrap_or_default().to_string_lossy().to_string();
            sb.execute(agent_name, &cmd, "bash", &[], &host_path, base).await
        } else {
            anyhow::bail!("CLI provider requires either base host access or Docker sandbox")
        };

        let exec_result = match exec_result {
            Ok(r) => r,
            Err(e) => {
                let msg = e.to_string().to_lowercase();
                let reason = if msg.contains("timed out") || msg.contains("timeout") {
                    CliErrorReason::Timeout
                } else {
                    CliErrorReason::Unknown
                };
                self.cooldown.lock().await.record_failure(reason);
                return Err(e);
            }
        };

        let elapsed = start.elapsed();

        if exec_result.exit_code != 0 {
            let reason = classify_cli_error(&exec_result.stderr, &exec_result.stdout, exec_result.exit_code);
            self.cooldown.lock().await.record_failure(reason);
            anyhow::bail!(
                "CLI exited with code {} ({:?}): {}",
                exec_result.exit_code,
                reason,
                exec_result.stderr.chars().take(500).collect::<String>()
            );
        }

        // Success — reset cooldown
        self.cooldown.lock().await.record_success();

        // Parse output
        let output = match self.config.output {
            CliOutputFormat::Json => parse_cli_json(&exec_result.stdout),
            CliOutputFormat::Jsonl => parse_cli_jsonl(&exec_result.stdout),
            CliOutputFormat::Text => CliOutput {
                text: exec_result.stdout.trim().to_string(),
                session_id: None,
                usage: None,
            },
        };

        // Store session for next call
        if let Some(ref sid) = output.session_id {
            self.sessions
                .write()
                .await
                .insert(agent_name.to_string(), sid.clone());
        }

        tracing::info!(
            command = %self.config.command,
            model = %resolved_model,
            content_len = output.text.len(),
            elapsed_ms = elapsed.as_millis() as u64,
            session_id = ?output.session_id,
            "CLI response"
        );

        Ok(output)
    }

    /// Build CLI argument vector. Returns (argv, `use_stdin`) where `use_stdin=true` means
    /// the prompt was excluded from argv and must be piped via stdin.
    fn build_argv(
        &self,
        model: &str,
        prompt: &str,
        system_prompt: Option<&str>,
        session_id: Option<&str>,
        use_resume: bool,
    ) -> (Vec<String>, bool) {
        let mut argv = vec![self.config.command.clone()];
        let has_session = session_id.is_some();

        if use_resume {
            // Use resume args, replace {session_id} template
            let sid = session_id.unwrap_or("");
            for arg in &self.config.resume_args {
                argv.push(arg.replace("{session_id}", sid));
            }
        } else {
            // Fresh invocation
            argv.extend(self.config.args.clone());

            // --model
            if let Some(ref model_arg) = self.config.model_arg
                && !model.is_empty() {
                    argv.push(model_arg.clone());
                    argv.push(model.to_string());
                }

            // --append-system-prompt (controlled by system_prompt_when)
            let should_include_system_prompt = match self.config.system_prompt_when {
                SystemPromptWhen::Never => false,
                SystemPromptWhen::First => !has_session,
                SystemPromptWhen::Always => true,
            };
            if should_include_system_prompt
                && let Some(ref sp_arg) = self.config.system_prompt_arg
                && let Some(sp) = system_prompt
                    && !sp.is_empty() {
                        argv.push(sp_arg.clone());
                        argv.push(sp.to_string());
                    }

            // --session-id
            if self.config.session_mode != CliSessionMode::None
                && let Some(ref s_arg) = self.config.session_arg {
                    let sid = session_id.unwrap_or("");
                    if !sid.is_empty() {
                        argv.push(s_arg.clone());
                        argv.push(sid.to_string());
                    }
                }
        }

        // Add prompt (or signal stdin mode if too large)
        let use_stdin = if self.config.input == CliInputMode::Arg {
            if prompt.len() > self.config.max_prompt_arg_chars {
                // Prompt too large for CLI arg — must be piped via stdin
                true
            } else {
                if let Some(ref pa) = self.config.prompt_arg {
                    argv.push(pa.clone());
                }
                argv.push(prompt.to_string());
                false
            }
        } else {
            // CliInputMode::Stdin — always use stdin
            true
        };

        (argv, use_stdin)
    }
}

// ── Host execution ───────────────────────────────────────────────────────────

async fn execute_on_host(
    argv: &[String],
    env: &HashMap<String, String>,
    workspace_dir: &str,
    timeout: Duration,
    clear_env: &[String],
    stdin_input: Option<&str>,
    no_output_timeout_secs: u64,
) -> Result<ExecResult> {
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(workspace_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Inject env vars (vault secrets)
    for (k, v) in env {
        cmd.env(k, v);
    }

    // Remove env vars listed in clear_env (security: prevent credential leakage)
    for key in clear_env {
        cmd.env_remove(key);
    }

    // Unconditional strip of Core's own secrets, independent of per-preset
    // clear_env (which may not enumerate them) — see T04 Пункт 4.
    crate::tools::spawn_env::strip_host_secrets(&mut cmd);

    if stdin_input.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn CLI '{}': {}", argv[0], e))?;

    // If stdin_input provided, write to stdin then close it
    if let Some(input) = stdin_input
        && let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(input.as_bytes()).await
                .map_err(|e| anyhow::anyhow!("failed to write to CLI stdin: {e}"))?;
            // Drop stdin to close the pipe (signals EOF to child)
        }

    let no_output_timeout = Duration::from_secs(no_output_timeout_secs);

    // Read stdout with no-output watchdog + overall timeout
    let read_future = async {
        // Spawn stderr reader in parallel
        let stderr_handle = child.stderr.take();
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut stderr) = stderr_handle {
                let _ = stderr.read_to_end(&mut buf).await;
            }
            String::from_utf8_lossy(&buf).to_string()
        });

        // Read stdout with per-chunk no-output watchdog
        let mut stdout_buf = Vec::new();
        if let Some(mut stdout) = child.stdout.take() {
            let mut chunk = [0u8; 8192];
            loop {
                let read_result = tokio::time::timeout(no_output_timeout, stdout.read(&mut chunk)).await;
                match read_result {
                    Ok(Ok(0)) => break, // EOF
                    Ok(Ok(n)) => stdout_buf.extend_from_slice(&chunk[..n]),
                    Ok(Err(e)) => return Err(anyhow::anyhow!("stdout read error: {e}")),
                    Err(_) => {
                        // No output timeout fired -- kill the process
                        tracing::warn!("CLI killed: no stdout output for {}s", no_output_timeout_secs);
                        let _ = child.kill().await;
                        anyhow::bail!("CLI killed: no stdout output for {no_output_timeout_secs}s");
                    }
                }
            }
        }

        // Wait for process to complete
        let status = child.wait().await
            .map_err(|e| anyhow::anyhow!("CLI process error: {e}"))?;
        let stderr_str = stderr_task.await.unwrap_or_default();

        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&stdout_buf).to_string(),
            stderr: stderr_str,
            exit_code: i64::from(status.code().unwrap_or(-1)),
        })
    };

    // Wrap everything in the overall timeout.
    // When the timeout fires, the future is dropped, which drops `child` and kills the process.
    match tokio::time::timeout(timeout, read_future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!("CLI timed out after {}s", timeout.as_secs()),
    }
}

/// Simple shell escaping — wraps in single quotes, escaping inner single quotes.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::super::presets::{claude_config, gemini_config};
    use super::*;

    // ── shell_escape ────────────────────────────────────────────────────────

    #[test]
    fn shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn shell_escape_with_single_quote() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_escape_empty() {
        assert_eq!(shell_escape(""), "''");
    }

    // ── default configs ─────────────────────────────────────────────────────

    #[test]
    fn default_claude_config() {
        let cfg = claude_config();
        assert_eq!(cfg.command, "claude");
        assert!(cfg.serialize);
        assert_eq!(cfg.session_mode, CliSessionMode::Always);
        assert_eq!(cfg.model_arg, Some("--model".to_string()));
        assert_eq!(cfg.system_prompt_arg, Some("--append-system-prompt".to_string()));
        assert_eq!(cfg.timeout_secs, 300);
    }

    #[test]
    fn default_gemini_config() {
        let cfg = gemini_config();
        assert_eq!(cfg.command, "gemini");
        assert_eq!(cfg.session_mode, CliSessionMode::None);
        assert!(cfg.resume_args.is_empty());
        assert!(cfg.system_prompt_arg.is_none());
        assert!(cfg.serialize);
    }

    // ── classify_cli_error ──────────────────────────────────────────────────

    #[test]
    fn classify_rate_limit() {
        let reason = classify_cli_error("429 too many requests", "", 1);
        assert_eq!(reason, CliErrorReason::RateLimit);
    }

    #[test]
    fn classify_auth() {
        let reason = classify_cli_error("401 unauthorized: invalid api key", "", 1);
        assert_eq!(reason, CliErrorReason::Auth);
    }

    #[test]
    fn classify_billing() {
        let reason = classify_cli_error("402 payment required", "", 1);
        assert_eq!(reason, CliErrorReason::Billing);
    }

    #[test]
    fn classify_overloaded() {
        let reason = classify_cli_error("overloaded_error: server at capacity", "", 1);
        assert_eq!(reason, CliErrorReason::Overloaded);
    }

    #[test]
    fn classify_unknown() {
        let reason = classify_cli_error("something weird", "", 1);
        assert_eq!(reason, CliErrorReason::Unknown);
    }

    // ── CliRunner::build_argv ───────────────────────────────────────────────

    #[test]
    fn build_argv_fresh_with_model_and_system() {
        let runner = CliRunner::new(claude_config());
        let (argv, use_stdin) = runner.build_argv("sonnet", "Hello world", Some("Be kind"), None, false);
        assert_eq!(argv[0], "claude");
        assert!(argv.contains(&"--model".to_string()));
        assert!(argv.contains(&"sonnet".to_string()));
        assert!(argv.contains(&"--append-system-prompt".to_string()));
        assert!(argv.contains(&"Be kind".to_string()));
        // Prompt is the last element (Arg input mode)
        assert_eq!(argv.last().unwrap(), "Hello world");
        assert!(!use_stdin);
    }

    #[test]
    fn build_argv_resume_with_session() {
        let runner = CliRunner::new(claude_config());
        let (argv, _) = runner.build_argv("sonnet", "Follow up", None, Some("sess-42"), true);
        assert_eq!(argv[0], "claude");
        assert!(argv.contains(&"--resume".to_string()));
        assert!(argv.contains(&"sess-42".to_string()));
        // Model is NOT added during resume (resume_args used, not args)
        assert!(!argv.contains(&"--model".to_string()));
        assert_eq!(argv.last().unwrap(), "Follow up");
    }

    #[test]
    fn build_argv_no_model_arg() {
        let mut cfg = gemini_config();
        cfg.model_arg = None;
        let runner = CliRunner::new(cfg);
        let (argv, _) = runner.build_argv("gemini-pro", "Test", None, None, false);
        // Without model_arg, model should not appear in argv
        assert!(!argv.contains(&"gemini-pro".to_string()));
        assert_eq!(argv.last().unwrap(), "Test");
    }

    #[test]
    fn build_argv_empty_model_skipped() {
        let runner = CliRunner::new(claude_config());
        let (argv, _) = runner.build_argv("", "Prompt", None, None, false);
        // Empty model string should not produce --model flag
        assert!(!argv.contains(&"--model".to_string()));
    }

    #[test]
    fn build_argv_session_mode_none_skips_session() {
        let runner = CliRunner::new(gemini_config());
        let (argv, _) = runner.build_argv("gemini-pro", "Hi", None, Some("s-1"), false);
        // Gemini has session_mode=None, so session_id should not appear
        assert!(!argv.contains(&"s-1".to_string()));
    }

    #[test]
    fn build_argv_empty_system_prompt_skipped() {
        let runner = CliRunner::new(claude_config());
        let (argv, _) = runner.build_argv("sonnet", "Prompt", Some(""), None, false);
        // Empty system prompt should not produce the flag
        assert!(!argv.contains(&"--append-system-prompt".to_string()));
    }

    #[test]
    fn build_argv_gemini_prompt_arg() {
        let runner = CliRunner::new(gemini_config());
        let (argv, _) = runner.build_argv("gemini-2.5-flash", "Hello world", None, None, false);
        // Gemini uses prompt_arg="-p", so prompt must follow "-p" at the end
        let p_idx = argv.iter().position(|a| a == "-p").expect("-p must be in argv");
        assert_eq!(argv[p_idx + 1], "Hello world");
        // -p should NOT be in the base args (it's added via prompt_arg)
        assert_eq!(argv.iter().filter(|a| *a == "-p").count(), 1);
    }

    #[test]
    fn build_argv_claude_no_prompt_arg() {
        let runner = CliRunner::new(claude_config());
        let (argv, use_stdin) = runner.build_argv("sonnet", "Hello", None, None, false);
        // Claude has prompt_arg=None, prompt is positional (last element)
        assert_eq!(argv.last().unwrap(), "Hello");
        // -p is in base args (as flag), not as prompt_arg
        assert!(argv.contains(&"-p".to_string()));
        assert!(!use_stdin);
    }

    // ── SystemPromptWhen tests ─────────────────────────────────────────────

    #[test]
    fn test_system_prompt_when_never() {
        let mut cfg = claude_config();
        cfg.system_prompt_when = SystemPromptWhen::Never;
        let runner = CliRunner::new(cfg);
        let (argv, _) = runner.build_argv("sonnet", "Hello", Some("Be helpful"), None, false);
        // Never mode: system prompt arg should NOT appear even if system_prompt provided
        assert!(!argv.contains(&"--append-system-prompt".to_string()));
        assert!(!argv.contains(&"Be helpful".to_string()));
    }

    #[test]
    fn test_system_prompt_when_first_no_session() {
        let mut cfg = claude_config();
        cfg.system_prompt_when = SystemPromptWhen::First;
        let runner = CliRunner::new(cfg);
        // No session (has_session=false via session_id=None) -> system prompt IS included
        let (argv, _) = runner.build_argv("sonnet", "Hello", Some("Be helpful"), None, false);
        assert!(argv.contains(&"--append-system-prompt".to_string()));
        assert!(argv.contains(&"Be helpful".to_string()));
    }

    #[test]
    fn test_system_prompt_when_first_with_session() {
        let mut cfg = claude_config();
        cfg.system_prompt_when = SystemPromptWhen::First;
        let runner = CliRunner::new(cfg);
        // Has existing session -> system prompt NOT included (First mode skips on subsequent)
        let (argv, _) = runner.build_argv("sonnet", "Hello", Some("Be helpful"), Some("sess-1"), false);
        assert!(!argv.contains(&"--append-system-prompt".to_string()));
        assert!(!argv.contains(&"Be helpful".to_string()));
    }

    #[test]
    fn test_system_prompt_when_always_with_session() {
        let mut cfg = claude_config();
        cfg.system_prompt_when = SystemPromptWhen::Always;
        let runner = CliRunner::new(cfg);
        // Always mode: system prompt IS included even with existing session
        let (argv, _) = runner.build_argv("sonnet", "Hello", Some("Be helpful"), Some("sess-1"), false);
        assert!(argv.contains(&"--append-system-prompt".to_string()));
        assert!(argv.contains(&"Be helpful".to_string()));
    }

    // ── Auto-stdin tests ───────────────────────────────────────────────────

    #[test]
    fn test_auto_stdin_large_prompt() {
        let mut cfg = claude_config();
        cfg.max_prompt_arg_chars = 10;
        let runner = CliRunner::new(cfg);
        let long_prompt = "a".repeat(20);
        let (argv, use_stdin) = runner.build_argv("sonnet", &long_prompt, None, None, false);
        // Prompt exceeds max_prompt_arg_chars -> excluded from argv, stdin flag true
        assert!(!argv.contains(&long_prompt));
        assert!(use_stdin);
    }

    #[test]
    fn test_auto_stdin_small_prompt() {
        let mut cfg = claude_config();
        cfg.max_prompt_arg_chars = 100;
        let runner = CliRunner::new(cfg);
        let short_prompt = "Hello world";
        let (argv, use_stdin) = runner.build_argv("sonnet", short_prompt, None, None, false);
        // Prompt within limit -> included in argv, stdin flag false
        assert!(argv.contains(&short_prompt.to_string()));
        assert!(!use_stdin);
    }

    // ── Session invalidation tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_session_invalidation_same_hash() {
        let runner = CliRunner::new(claude_config());
        // Insert a session manually
        runner.sessions.write().await.insert("agent-1".to_string(), "sess-abc".to_string());

        // Call check_and_invalidate with hash A twice
        let invalidated1 = runner.check_and_invalidate_session("agent-1", 12345).await;
        assert!(!invalidated1, "first call should not invalidate (no previous hash)");

        let invalidated2 = runner.check_and_invalidate_session("agent-1", 12345).await;
        assert!(!invalidated2, "same hash should not invalidate");

        // Session should still be present
        let sessions = runner.sessions.read().await;
        assert_eq!(sessions.get("agent-1"), Some(&"sess-abc".to_string()));
    }

    #[tokio::test]
    async fn test_session_invalidation_changed_hash() {
        let runner = CliRunner::new(claude_config());
        // Insert a session manually
        runner.sessions.write().await.insert("agent-1".to_string(), "sess-abc".to_string());

        let invalidated1 = runner.check_and_invalidate_session("agent-1", 12345).await;
        assert!(!invalidated1, "first call should not invalidate (no previous hash)");

        // Change hash -> should invalidate
        let invalidated2 = runner.check_and_invalidate_session("agent-1", 99999).await;
        assert!(invalidated2, "different hash should invalidate session");

        // Session should be removed
        let sessions = runner.sessions.read().await;
        assert!(sessions.get("agent-1").is_none(), "session should be cleared after invalidation");
    }

    #[tokio::test]
    async fn test_session_invalidation_no_session_stored() {
        let runner = CliRunner::new(claude_config());
        // No session stored -- invalidation should not panic
        let invalidated1 = runner.check_and_invalidate_session("agent-2", 111).await;
        assert!(!invalidated1, "first call has no previous hash");

        // Hash changed -- method returns true even though no session was stored
        // (the sessions.remove is a harmless no-op)
        let invalidated2 = runner.check_and_invalidate_session("agent-2", 222).await;
        assert!(invalidated2, "hash mismatch detected regardless of session presence");
    }
}
