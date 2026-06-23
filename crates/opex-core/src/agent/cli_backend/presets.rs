//! Built-in CLI presets — known-good defaults for `claude-cli`, `gemini-cli`,
//! and `codex-cli`. Each preset declares argv shape, output format, session
//! mode, env-key name, and CLI-specific quirks (e.g. `clear_env`).
//!
//! [`resolve_cli_config`] is the canonical entry point: takes a preset id +
//! DB-options JSONB and returns a fully-populated `CliBackendConfig`.

use std::collections::HashMap;

use serde_json::Value;

use super::{
    CliBackendConfig, CliInputMode, CliOutputFormat, CliSessionMode, SystemPromptWhen,
};

/// Built-in CLI provider preset -- static defaults that work out of the box.
#[allow(dead_code)] // Some fields (name, resume_output) describe preset metadata for the
                    // /api/providers types endpoint and aren't read by the runtime path.
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
pub(super) fn claude_config() -> CliBackendConfig {
    preset_to_config(find_preset("claude-cli").expect("claude-cli preset"))
}

#[cfg(test)]
pub(super) fn gemini_config() -> CliBackendConfig {
    preset_to_config(find_preset("gemini-cli").expect("gemini-cli preset"))
}
