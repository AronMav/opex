//! Unified CLI LLM backend — configurable execution of CLI tools
//! (claude, gemini, codex, etc.) with session management, timeouts,
//! cooldowns, and serialized concurrency.
//!
//! Sub-modules:
//! - [`presets`] — built-in `CLI_PRESETS` + `find_preset` /
//!   `preset_to_config` / `resolve_cli_config` (DB-options merge).
//! - [`cooldown`] — `CliErrorReason` + `CooldownState` (circuit breaker
//!   on auth/rate-limit/timeout).
//! - [`runner`] — `CliRunner` (semaphore + sessions + spawn) and the
//!   `execute_on_host` helper.
//! - [`parser`] — `parse_cli_json` / `parse_cli_jsonl` for the supported
//!   output formats, plus `format_messages_for_cli` used by the
//!   `claude-cli` provider.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

mod cooldown;
mod parser;
mod presets;
mod runner;

pub use parser::format_messages_for_cli;
pub use presets::{CLI_PRESETS, find_preset, resolve_cli_config};
pub use runner::CliRunner;

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

// ── CliOutput ────────────────────────────────────────────────────────────────

/// Parsed CLI output.
pub struct CliOutput {
    pub text: String,
    pub session_id: Option<String>,
    pub usage: Option<opex_types::TokenUsage>,
}
