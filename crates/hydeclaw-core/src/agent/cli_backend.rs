//! Unified CLI LLM backend — configurable execution of CLI tools
//! (claude, gemini, etc.) with session management, timeouts, and serialization.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock, Semaphore};

use crate::containers::sandbox::{CodeSandbox, ExecResult};

// ── Configuration ────────────────────────────────────────────────────────────

/// Configuration for a CLI LLM backend (e.g. Claude CLI, Gemini CLI).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CliBackendConfig {
    /// CLI executable path/name
    pub command: String,
    /// Base args for fresh invocation
    #[serde(default)]
    pub args: Vec<String>,
    /// Args for resuming a session (template: {`session_id`} is replaced)
    #[serde(default)]
    pub resume_args: Vec<String>,
    /// Output format
    #[serde(default)]
    pub output: CliOutputFormat,
    /// How to pass user prompt
    #[serde(default)]
    pub input: CliInputMode,
    /// Flag for model selection (e.g. "--model")
    #[serde(default)]
    pub model_arg: Option<String>,
    /// Model name aliases
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
    /// Flag for session ID
    #[serde(default)]
    pub session_arg: Option<String>,
    /// Session mode
    #[serde(default)]
    pub session_mode: CliSessionMode,
    /// Flag for system prompt injection
    #[serde(default)]
    pub system_prompt_arg: Option<String>,
    /// Flag for prompt (e.g. "-p" for Gemini where prompt is a named arg, not positional)
    #[serde(default)]
    pub prompt_arg: Option<String>,
    /// Overall timeout in seconds
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// Serialize concurrent runs (queue, don't parallelize)
    #[serde(default = "default_true")]
    pub serialize: bool,
    /// Extra environment variables
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Secret name for API key (resolved from vault at runtime)
    #[serde(default)]
    pub env_key: Option<String>,
    /// Env vars to remove from child process before spawn (security)
    #[serde(default)]
    pub clear_env: Vec<String>,
    /// When to inject system prompt: first message only, always, or never
    #[serde(default)]
    pub system_prompt_when: SystemPromptWhen,
    /// Auto-switch to stdin if prompt exceeds this char count
    #[serde(default = "default_max_prompt_arg_chars")]
    pub max_prompt_arg_chars: usize,
    /// Kill CLI if no stdout for this many seconds
    #[serde(default = "default_no_output_timeout")]
    pub no_output_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    300
}
fn default_true() -> bool {
    true
}
fn default_max_prompt_arg_chars() -> usize {
    100_000
}
fn default_no_output_timeout() -> u64 {
    60
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CliOutputFormat {
    #[default]
    Json,
    Text,
    Jsonl,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CliInputMode {
    #[default]
    Arg,
    Stdin,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CliSessionMode {
    #[default]
    Always,
    Existing,
    None,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SystemPromptWhen {
    #[default]
    First,
    Always,
    Never,
}

// ── CLI Presets ─────────────────────────────────────────────────────────────

/// Built-in CLI provider preset -- static defaults that work out of the box.
#[allow(dead_code)]
pub struct CliPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
    pub resume_args: &'static [&'static str],
    pub output: CliOutputFormat,
    pub resume_output: Option<CliOutputFormat>,
    pub prompt_arg: Option<&'static str>,
    pub model_arg: Option<&'static str>,
    pub system_prompt_arg: Option<&'static str>,
    pub session_mode: CliSessionMode,
    pub session_arg: Option<&'static str>,
    pub env_key: &'static str,
    pub models_provider: &'static str,
    pub default_models: &'static [&'static str],
    pub system_prompt_when: SystemPromptWhen,
    pub clear_env: &'static [&'static str],
    pub max_prompt_arg_chars: usize,
    pub no_output_timeout_secs: u64,
}

pub static CLI_PRESETS: &[CliPreset] = &[
    CliPreset {
        id: "gemini-cli",
        name: "Gemini CLI",
        command: "gemini",
        args: &["--output-format", "json"],
        resume_args: &[],
        output: CliOutputFormat::Json,
        resume_output: None,
        prompt_arg: Some("-p"),
        model_arg: Some("--model"),
        system_prompt_arg: None,
        session_mode: CliSessionMode::None,
        session_arg: None,
        env_key: "GEMINI_API_KEY",
        models_provider: "google",
        default_models: &[
            "gemini-3.1-pro-preview",
            "gemini-3-flash-preview",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
        ],
        system_prompt_when: SystemPromptWhen::First,
        clear_env: &[],
        max_prompt_arg_chars: 100_000,
        no_output_timeout_secs: 60,
    },
    CliPreset {
        id: "claude-cli",
        name: "Claude CLI",
        command: "claude",
        args: &[
            "-p",
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
        ],
        resume_args: &[
            "-p",
            "--output-format",
            "json",
            "--permission-mode",
            "bypassPermissions",
            "--resume",
            "{session_id}",
        ],
        output: CliOutputFormat::Json,
        resume_output: None,
        prompt_arg: None,
        model_arg: Some("--model"),
        system_prompt_arg: Some("--append-system-prompt"),
        session_mode: CliSessionMode::Always,
        session_arg: Some("--session-id"),
        env_key: "ANTHROPIC_API_KEY",
        models_provider: "anthropic",
        default_models: &["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"],
        system_prompt_when: SystemPromptWhen::First,
        clear_env: &["CLAUDE_CODE_OAUTH_TOKEN"],
        max_prompt_arg_chars: 100_000,
        no_output_timeout_secs: 60,
    },
    CliPreset {
        id: "codex-cli",
        name: "Codex CLI",
        command: "codex",
        args: &["--output-format", "json"],
        resume_args: &[],
        output: CliOutputFormat::Json,
        resume_output: None,
        prompt_arg: None,
        model_arg: Some("--model"),
        system_prompt_arg: None,
        session_mode: CliSessionMode::None,
        session_arg: None,
        env_key: "OPENAI_API_KEY",
        models_provider: "openai",
        default_models: &["codex-mini", "gpt-4.1", "o4-mini"],
        system_prompt_when: SystemPromptWhen::First,
        clear_env: &[],
        max_prompt_arg_chars: 100_000,
        no_output_timeout_secs: 60,
    },
];

/// Find a built-in CLI preset by id.
pub fn find_preset(id: &str) -> Option<&'static CliPreset> {
    CLI_PRESETS.iter().find(|p| p.id == id)
}

/// Convert a built-in preset to a `CliBackendConfig` with default values.
pub fn preset_to_config(preset: &CliPreset) -> CliBackendConfig {
    CliBackendConfig {
        command: preset.command.to_string(),
        args: preset.args.iter().map(|s| (*s).to_string()).collect(),
        resume_args: preset.resume_args.iter().map(|s| (*s).to_string()).collect(),
        output: preset.output.clone(),
        input: CliInputMode::Arg,
        model_arg: preset.model_arg.map(std::string::ToString::to_string),
        model_aliases: HashMap::new(),
        session_arg: preset.session_arg.map(std::string::ToString::to_string),
        session_mode: preset.session_mode.clone(),
        system_prompt_arg: preset.system_prompt_arg.map(std::string::ToString::to_string),
        prompt_arg: preset.prompt_arg.map(std::string::ToString::to_string),
        timeout_secs: 300,
        serialize: true,
        env: HashMap::new(),
        env_key: Some(preset.env_key.to_string()),
        clear_env: preset.clear_env.iter().map(|s| (*s).to_string()).collect(),
        system_prompt_when: preset.system_prompt_when.clone(),
        max_prompt_arg_chars: preset.max_prompt_arg_chars,
        no_output_timeout_secs: preset.no_output_timeout_secs,
    }
}

/// Apply DB provider options (JSONB) on top of a `CliBackendConfig`.
/// Only non-null fields in the JSON override the preset defaults.
pub fn merge_db_overrides(config: &mut CliBackendConfig, options: &Value) {
    if let Some(obj) = options.as_object() {
        if let Some(v) = obj.get("command").and_then(|v| v.as_str()) {
            config.command = v.to_string();
        }
        if let Some(v) = obj.get("args").and_then(|v| v.as_array()) {
            config.args = v.iter().filter_map(|s| s.as_str().map(String::from)).collect();
        }
        if let Some(v) = obj.get("resume_args").and_then(|v| v.as_array()) {
            config.resume_args = v.iter().filter_map(|s| s.as_str().map(String::from)).collect();
        }
        if let Some(v) = obj.get("prompt_arg") {
            config.prompt_arg = v.as_str().map(String::from);
        }
        if let Some(v) = obj.get("model_arg") {
            config.model_arg = v.as_str().map(String::from);
        }
        if let Some(v) = obj.get("system_prompt_arg") {
            config.system_prompt_arg = v.as_str().map(String::from);
        }
        if let Some(v) = obj.get("session_arg") {
            config.session_arg = v.as_str().map(String::from);
        }
        if let Some(v) = obj.get("env_key").and_then(|v| v.as_str()) {
            config.env_key = Some(v.to_string());
        }
        if let Some(v) = obj.get("timeout_secs").and_then(serde_json::Value::as_u64) {
            config.timeout_secs = v;
        }
        if let Some(v) = obj.get("clear_env").and_then(|v| v.as_array()) {
            config.clear_env = v.iter().filter_map(|s| s.as_str().map(String::from)).collect();
        }
        if let Some(v) = obj.get("system_prompt_when")
            && let Ok(spw) = serde_json::from_value::<SystemPromptWhen>(v.clone()) {
                config.system_prompt_when = spw;
            }
        if let Some(v) = obj.get("max_prompt_arg_chars").and_then(serde_json::Value::as_u64) {
            config.max_prompt_arg_chars = v as usize;
        }
        if let Some(v) = obj.get("no_output_timeout_secs").and_then(serde_json::Value::as_u64) {
            config.no_output_timeout_secs = v;
        }
        if let Some(v) = obj.get("env").and_then(|v| v.as_object()) {
            for (ek, ev) in v {
                if let Some(s) = ev.as_str() {
                    config.env.insert(ek.clone(), s.to_string());
                }
            }
        }
    }
}

/// Resolve a CLI provider config from preset ID + optional DB overrides.
/// Returns None if the preset ID is not found.
pub fn resolve_cli_config(preset_id: &str, db_options: &Value) -> Option<CliBackendConfig> {
    let preset = find_preset(preset_id)?;
    let mut config = preset_to_config(preset);
    merge_db_overrides(&mut config, db_options);
    Some(config)
}

// ── Convenience helpers for tests ────────────────────────────────────────────

#[cfg(test)]
fn claude_config() -> CliBackendConfig {
    preset_to_config(find_preset("claude-cli").expect("claude-cli preset"))
}

#[cfg(test)]
fn gemini_config() -> CliBackendConfig {
    preset_to_config(find_preset("gemini-cli").expect("gemini-cli preset"))
}

// ── CliOutput ────────────────────────────────────────────────────────────────

/// Parsed CLI output.
pub struct CliOutput {
    pub text: String,
    pub session_id: Option<String>,
    pub usage: Option<hydeclaw_types::TokenUsage>,
}

// ── Error Classification ─────────────────────────────────────────────────────

/// Classified CLI error reason for cooldown decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CliErrorReason {
    /// Rate limited (429, "too many requests", quota exceeded)
    RateLimit,
    /// Auth error (401/403, invalid key, revoked, banned)
    Auth,
    /// Billing issue (402, insufficient credits)
    Billing,
    /// Overloaded (503, "high demand")
    Overloaded,
    /// Timeout (process took too long)
    Timeout,
    /// Other/unknown error
    Unknown,
}

impl CliErrorReason {
    /// Cooldown duration for this error type (exponential: base * 5^(n-1), capped).
    fn cooldown_ms(&self, error_count: u32) -> u64 {
        let n = error_count.min(4);
        match self {
            // Rate limit / overload: 1m → 5m → 25m → 1h max
            CliErrorReason::RateLimit | CliErrorReason::Overloaded => {
                let ms = 60_000u64 * 5u64.pow(n.saturating_sub(1));
                ms.min(3_600_000) // 1 hour max
            }
            // Auth / billing: 5h → 10h → 20h → 24h max
            CliErrorReason::Auth | CliErrorReason::Billing => {
                let ms = 5 * 3_600_000u64 * 2u64.pow(n.saturating_sub(1));
                ms.min(24 * 3_600_000) // 24 hours max
            }
            // Timeout / unknown: 30s → 2m → 10m → 30m max
            CliErrorReason::Timeout | CliErrorReason::Unknown => {
                let ms = 30_000u64 * 5u64.pow(n.saturating_sub(1).min(3));
                ms.min(30 * 60_000) // 30 min max
            }
        }
    }
}

/// Classify an error from CLI output using shared `error_classify` module.
fn classify_cli_error(stderr: &str, stdout: &str, _exit_code: i64) -> CliErrorReason {
    use crate::agent::error_classify::{classify_str, LlmErrorClass};
    let combined = format!("{stderr} {stdout}");
    match classify_str(&combined) {
        LlmErrorClass::RateLimit => CliErrorReason::RateLimit,
        LlmErrorClass::AuthPermanent => CliErrorReason::Auth,
        LlmErrorClass::Billing => CliErrorReason::Billing,
        LlmErrorClass::Overloaded | LlmErrorClass::TransientHttp => CliErrorReason::Overloaded,
        _ => CliErrorReason::Unknown,
    }
}

// ── Cooldown Tracker ─────────────────────────────────────────────────────────

struct CooldownState {
    /// Number of consecutive errors
    error_count: u32,
    /// Cooldown expires at this instant
    cooldown_until: Option<Instant>,
    /// Last error reason
    last_reason: Option<CliErrorReason>,
}

impl CooldownState {
    fn new() -> Self {
        Self { error_count: 0, cooldown_until: None, last_reason: None }
    }

    /// Check if currently in cooldown. If expired, reset state.
    fn is_in_cooldown(&mut self) -> Option<Duration> {
        if let Some(until) = self.cooldown_until {
            let now = Instant::now();
            if now < until {
                return Some(until - now);
            }
            // Expired — reset (circuit breaker half-open → closed)
            self.error_count = 0;
            self.cooldown_until = None;
            self.last_reason = None;
        }
        None
    }

    /// Record a failure and start cooldown.
    fn record_failure(&mut self, reason: CliErrorReason) {
        // Don't extend active cooldown window (OpenClaw pattern)
        if self.cooldown_until.is_some_and(|u| Instant::now() < u) {
            return;
        }
        self.error_count += 1;
        self.last_reason = Some(reason);
        let cooldown_ms = reason.cooldown_ms(self.error_count);
        self.cooldown_until = Some(Instant::now() + Duration::from_millis(cooldown_ms));
        tracing::warn!(
            reason = ?reason,
            error_count = self.error_count,
            cooldown_secs = cooldown_ms / 1000,
            "CLI provider entering cooldown"
        );
    }

    /// Record success — reset error count.
    fn record_success(&mut self) {
        self.error_count = 0;
        self.cooldown_until = None;
        self.last_reason = None;
    }
}

// ── CliRunner ────────────────────────────────────────────────────────────────

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
        let mut hashes = self.session_hashes.write().await;
        let prev = hashes.insert(agent_name.to_string(), context_hash);
        if let Some(prev_hash) = prev
            && prev_hash != context_hash {
                // Context changed -- invalidate session
                self.sessions.write().await.remove(agent_name);
                tracing::info!(agent = %agent_name, "CLI session invalidated: context hash changed");
                return true;
            }
        false
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

        // Build argv
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
        let start = std::time::Instant::now();
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

// ── Output parsing ───────────────────────────────────────────────────────────

fn parse_cli_json(raw: &str) -> CliOutput {
    #[derive(Deserialize)]
    struct JsonOut {
        #[serde(alias = "result", alias = "response", alias = "content")]
        text: Option<String>,
        #[serde(alias = "session_id", alias = "sessionId", alias = "conversation_id")]
        session_id: Option<String>,
        #[serde(default)]
        cost_usd: Option<f64>,
        #[serde(default)]
        input_tokens: Option<u32>,
        #[serde(default)]
        output_tokens: Option<u32>,
        #[serde(default)]
        usage: Option<serde_json::Value>,
    }

    let parsed: Option<JsonOut> = serde_json::from_str(raw.trim()).ok();
    match parsed {
        Some(p) => {
            if let Some(cost) = p.cost_usd {
                tracing::info!(cost_usd = cost, "CLI cost");
            }
            let usage = match (p.input_tokens, p.output_tokens) {
                (Some(inp), Some(out)) => Some(hydeclaw_types::TokenUsage {
                    input_tokens: inp,
                    output_tokens: out,
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                    reasoning_tokens: None,
                }),
                _ => {
                    // Try nested usage object (Anthropic CLI format includes cache fields)
                    p.usage.as_ref().and_then(|u| {
                        let inp =
                            u.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                        let out =
                            u.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                        if inp > 0 || out > 0 {
                            Some(hydeclaw_types::TokenUsage {
                                input_tokens: inp,
                                output_tokens: out,
                                cache_read_tokens: u
                                    .get("cache_read_input_tokens")
                                    .and_then(serde_json::Value::as_u64)
                                    .map(|v| v as u32),
                                cache_creation_tokens: u
                                    .get("cache_creation_input_tokens")
                                    .and_then(serde_json::Value::as_u64)
                                    .map(|v| v as u32),
                                reasoning_tokens: None,
                            })
                        } else {
                            None
                        }
                    })
                }
            };
            CliOutput {
                text: p.text.unwrap_or_default(),
                session_id: p.session_id,
                usage,
            }
        }
        None => CliOutput {
            text: raw.trim().to_string(),
            session_id: None,
            usage: None,
        },
    }
}

fn parse_cli_jsonl(raw: &str) -> CliOutput {
    let mut texts = Vec::new();
    let mut session_id = None;
    let mut usage = None;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            // Extract session_id
            if session_id.is_none() {
                session_id = v
                    .get("session_id")
                    .or_else(|| v.get("thread_id"))
                    .and_then(|s| s.as_str())
                    .map(std::string::ToString::to_string);
            }
            // Extract text
            if let Some(text) = v
                .get("text")
                .or_else(|| v.get("result"))
                .and_then(|t| t.as_str())
            {
                texts.push(text.to_string());
            }
            if let Some(item) = v.get("item")
                && let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    texts.push(text.to_string());
                }
            // Extract usage
            if let Some(u) = v.get("usage") {
                let inp =
                    u.get("input_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                let out =
                    u.get("output_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
                if inp > 0 || out > 0 {
                    usage = Some(hydeclaw_types::TokenUsage {
                        input_tokens: inp,
                        output_tokens: out,
                        cache_read_tokens: u
                            .get("cache_read_input_tokens")
                            .and_then(serde_json::Value::as_u64)
                            .map(|v| v as u32),
                        cache_creation_tokens: u
                            .get("cache_creation_input_tokens")
                            .and_then(serde_json::Value::as_u64)
                            .map(|v| v as u32),
                        reasoning_tokens: None,
                    });
                }
            }
        }
    }

    CliOutput {
        text: texts.join("\n"),
        session_id,
        usage,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Format messages for CLI prompt. Returns (`user_prompt`, `system_prompt`).
pub fn format_messages_for_cli(
    messages: &[hydeclaw_types::Message],
) -> (String, Option<String>) {
    use hydeclaw_types::MessageRole;
    let mut system_parts = Vec::new();
    let mut prompt_parts = Vec::new();
    for msg in messages {
        match msg.role {
            MessageRole::System => system_parts.push(msg.content.clone()),
            MessageRole::User => prompt_parts.push(msg.content.clone()),
            MessageRole::Assistant => {
                prompt_parts.push(format!("[Assistant]\n{}", msg.content));
            }
            MessageRole::Tool => {
                prompt_parts.push(format!("[Tool result]\n{}", msg.content));
            }
        }
    }
    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };
    (prompt_parts.join("\n\n"), system)
}

/// Simple shell escaping — wraps in single quotes, escaping inner single quotes.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hydeclaw_types::{Message, MessageRole};

    // ── parse_cli_json ──────────────────────────────────────────────────────

    #[test]
    fn parse_json_valid_result() {
        let json = r#"{"result": "Hello", "session_id": "abc-123", "input_tokens": 10, "output_tokens": 20}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "Hello");
        assert_eq!(out.session_id, Some("abc-123".to_string()));
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn parse_json_response_alias() {
        let json = r#"{"response": "World", "sessionId": "s-42"}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "World");
        assert_eq!(out.session_id, Some("s-42".to_string()));
        assert!(out.usage.is_none());
    }

    #[test]
    fn parse_json_content_alias() {
        let json = r#"{"content": "Hi", "conversation_id": "c-1"}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "Hi");
        assert_eq!(out.session_id, Some("c-1".to_string()));
    }

    #[test]
    fn parse_json_invalid_returns_raw() {
        let raw = "Not a JSON at all";
        let out = parse_cli_json(raw);
        assert_eq!(out.text, "Not a JSON at all");
        assert!(out.session_id.is_none());
        assert!(out.usage.is_none());
    }

    #[test]
    fn parse_json_empty_string() {
        let out = parse_cli_json("");
        assert_eq!(out.text, "");
        assert!(out.session_id.is_none());
        assert!(out.usage.is_none());
    }

    #[test]
    fn parse_json_nested_usage() {
        let json = r#"{"result": "ok", "usage": {"input_tokens": 100, "output_tokens": 50}}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "ok");
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
    }

    #[test]
    fn claude_cli_returns_none_for_unsupported_cache_fields() {
        // Top-level only; no nested usage object → cache fields stay None.
        let json = r#"{"result": "...", "input_tokens": 100, "output_tokens": 50}"#;
        let out = parse_cli_json(json);
        let u = out.usage.expect("usage present");
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_tokens, None);
        assert_eq!(u.cache_creation_tokens, None);
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn claude_cli_maps_cache_fields_when_nested_usage_has_them() {
        // Anthropic CLI JSON puts cache fields inside the nested `usage` object.
        let json = r#"{
            "result": "ok",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 700,
                "cache_creation_input_tokens": 200
            }
        }"#;
        let out = parse_cli_json(json);
        let u = out.usage.expect("usage present");
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 50);
        assert_eq!(u.cache_read_tokens, Some(700));
        assert_eq!(u.cache_creation_tokens, Some(200));
        assert_eq!(u.reasoning_tokens, None);
    }

    #[test]
    fn parse_json_cost_usd_present() {
        // cost_usd is logged but not returned in CliOutput; just verify no panic
        let json = r#"{"result": "done", "cost_usd": 0.003, "input_tokens": 5, "output_tokens": 3}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "done");
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 5);
        assert_eq!(u.output_tokens, 3);
    }

    #[test]
    fn parse_json_no_text_field() {
        // JSON is valid but has no recognized text field -> empty string
        let json = r#"{"session_id": "s-99", "input_tokens": 1, "output_tokens": 2}"#;
        let out = parse_cli_json(json);
        assert_eq!(out.text, "");
        assert_eq!(out.session_id, Some("s-99".to_string()));
    }

    // ── parse_cli_jsonl ─────────────────────────────────────────────────────

    #[test]
    fn parse_jsonl_multiple_lines() {
        let raw = r#"{"text": "Hello", "session_id": "s-1"}
{"text": " world"}
{"usage": {"input_tokens": 10, "output_tokens": 20}}"#;
        let out = parse_cli_jsonl(raw);
        assert_eq!(out.text, "Hello\n world");
        assert_eq!(out.session_id, Some("s-1".to_string()));
        let u = out.usage.unwrap();
        assert_eq!(u.input_tokens, 10);
        assert_eq!(u.output_tokens, 20);
    }

    #[test]
    fn parse_jsonl_item_text() {
        let raw = r#"{"item": {"text": "nested content"}, "thread_id": "t-5"}"#;
        let out = parse_cli_jsonl(raw);
        assert_eq!(out.text, "nested content");
        assert_eq!(out.session_id, Some("t-5".to_string()));
    }

    #[test]
    fn parse_jsonl_empty() {
        let out = parse_cli_jsonl("");
        assert_eq!(out.text, "");
        assert!(out.session_id.is_none());
        assert!(out.usage.is_none());
    }

    // ── format_messages_for_cli ─────────────────────────────────────────────

    fn msg(role: MessageRole, content: &str) -> Message {
        Message {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            thinking_blocks: vec![],
            db_id: None,
        }
    }

    #[test]
    fn format_system_and_user() {
        let msgs = vec![
            msg(MessageRole::System, "Be helpful"),
            msg(MessageRole::User, "Hi there"),
        ];
        let (prompt, system) = format_messages_for_cli(&msgs);
        assert_eq!(prompt, "Hi there");
        assert_eq!(system, Some("Be helpful".to_string()));
    }

    #[test]
    fn format_user_only() {
        let msgs = vec![msg(MessageRole::User, "Hello")];
        let (prompt, system) = format_messages_for_cli(&msgs);
        assert_eq!(prompt, "Hello");
        assert!(system.is_none());
    }

    #[test]
    fn format_with_assistant_and_tool() {
        let msgs = vec![
            msg(MessageRole::User, "Question"),
            msg(MessageRole::Assistant, "Let me check"),
            msg(MessageRole::Tool, "result=42"),
            msg(MessageRole::User, "Thanks"),
        ];
        let (prompt, system) = format_messages_for_cli(&msgs);
        assert!(prompt.contains("Question"));
        assert!(prompt.contains("[Assistant]\nLet me check"));
        assert!(prompt.contains("[Tool result]\nresult=42"));
        assert!(prompt.contains("Thanks"));
        assert!(system.is_none());
    }

    #[test]
    fn format_empty_messages() {
        let msgs: Vec<Message> = vec![];
        let (prompt, system) = format_messages_for_cli(&msgs);
        assert_eq!(prompt, "");
        assert!(system.is_none());
    }

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

    // ── CooldownState ───────────────────────────────────────────────────────

    #[test]
    fn cooldown_new_not_in_cooldown() {
        let mut state = CooldownState::new();
        assert!(state.is_in_cooldown().is_none());
        assert_eq!(state.error_count, 0);
    }

    #[test]
    fn cooldown_after_failure() {
        let mut state = CooldownState::new();
        state.record_failure(CliErrorReason::RateLimit);
        assert_eq!(state.error_count, 1);
        assert!(state.cooldown_until.is_some());
        assert!(state.is_in_cooldown().is_some());
    }

    #[test]
    fn cooldown_after_success_reset() {
        let mut state = CooldownState::new();
        state.record_failure(CliErrorReason::Unknown);
        assert_eq!(state.error_count, 1);
        state.record_success();
        assert_eq!(state.error_count, 0);
        assert!(state.cooldown_until.is_none());
        assert!(state.last_reason.is_none());
        assert!(state.is_in_cooldown().is_none());
    }

    #[test]
    fn cooldown_no_extend_during_active() {
        let mut state = CooldownState::new();
        state.record_failure(CliErrorReason::RateLimit);
        assert_eq!(state.error_count, 1);
        let remaining1 = state.is_in_cooldown().unwrap();
        // Second failure during active cooldown should NOT increment
        state.record_failure(CliErrorReason::RateLimit);
        assert_eq!(state.error_count, 1); // unchanged
        let remaining2 = state.is_in_cooldown().unwrap();
        assert!(remaining2 <= remaining1);
    }

    // ── CliErrorReason::cooldown_ms ─────────────────────────────────────────

    #[test]
    fn cooldown_ms_rate_limit_escalation() {
        // First error: 60s
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(1), 60_000);
        // Second: 300s (5min)
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(2), 300_000);
        // Third: 1500s (25min)
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(3), 1_500_000);
        // Fourth: capped at 3600s (1h)
        assert_eq!(CliErrorReason::RateLimit.cooldown_ms(4), 3_600_000);
    }

    #[test]
    fn cooldown_ms_unknown_escalation() {
        // First: 30s
        assert_eq!(CliErrorReason::Unknown.cooldown_ms(1), 30_000);
        // Second: 150s
        assert_eq!(CliErrorReason::Unknown.cooldown_ms(2), 150_000);
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

        // Set initial hash
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
