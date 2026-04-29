use serde::{Deserialize, Serialize};

/// Four-tier LLM timeout model. Every LLM call is governed by all four.
/// Zero means "no limit" for `request_secs`, `stream_inactivity_secs`,
/// `stream_max_duration_secs`. `connect_secs` must be non-zero (a connect
/// with no upper bound cannot fail over — enforced in `validate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeoutsConfig {
    #[serde(default = "default_connect_secs")]
    pub connect_secs: u64,
    #[serde(default = "default_request_secs")]
    pub request_secs: u64,
    #[serde(default = "default_stream_inactivity_secs")]
    pub stream_inactivity_secs: u64,
    #[serde(default = "default_stream_max_duration_secs")]
    pub stream_max_duration_secs: u64,
    /// Maximum wall-clock duration for ALL retry attempts combined (seconds).
    /// Zero = no limit; the session runs until the model responds or user cancels.
    #[serde(default)]
    pub run_max_duration_secs: u64,
}

fn default_connect_secs() -> u64 { 10 }
fn default_request_secs() -> u64 { 120 }
fn default_stream_inactivity_secs() -> u64 { 60 }
fn default_stream_max_duration_secs() -> u64 { 600 }

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self {
            connect_secs: default_connect_secs(),
            request_secs: default_request_secs(),
            stream_inactivity_secs: default_stream_inactivity_secs(),
            stream_max_duration_secs: default_stream_max_duration_secs(),
            run_max_duration_secs: 0,
        }
    }
}

use std::collections::HashMap;
use serde_json::Value;

/// Persisted JSON under `providers.options`. Typed `extra` (not
/// `serde_json::Value`) catches typos in known field names — misspelled
/// keys (e.g., `timeout` instead of `timeouts`) land in `extra` and the
/// loader emits a WARN log listing unknown keys.
fn default_max_retries() -> u32 { 3 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ProviderOptions {
    #[serde(default)]
    pub timeouts: TimeoutsConfig,
    #[serde(default)]
    pub api_key_envs: Vec<String>,
    /// Anthropic-only: enable prompt caching. Ignored by other providers.
    /// Lives here (instead of on `ProviderRouteConfig`) per spec §4.7
    /// which removed duplicated per-route provider fields.
    #[serde(default)]
    pub prompt_cache: bool,
    /// Max HTTP retry attempts on transient errors (429/5xx). Default 3, range 1–10.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl TimeoutsConfig {
    /// Returns human-readable error message if any field is out of range.
    /// Called on every load and on every PUT /api/providers write.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=120).contains(&self.connect_secs) {
            return Err(format!(
                "connect_secs must be in 1..=120 (got {})",
                self.connect_secs
            ));
        }
        if self.request_secs > 3600 {
            return Err(format!(
                "request_secs must be in 0..=3600 (got {})",
                self.request_secs
            ));
        }
        if self.stream_inactivity_secs > 3600 {
            return Err(format!(
                "stream_inactivity_secs must be in 0..=3600 (got {})",
                self.stream_inactivity_secs
            ));
        }
        if self.stream_max_duration_secs > 7200 {
            return Err(format!(
                "stream_max_duration_secs must be in 0..=7200 (got {})",
                self.stream_max_duration_secs
            ));
        }
        if self.run_max_duration_secs > 86400 {
            return Err(format!(
                "run_max_duration_secs must be in 0..=86400 (got {})",
                self.run_max_duration_secs
            ));
        }
        Ok(())
    }
}

impl ProviderOptions {
    pub fn validate(&self) -> Result<(), String> {
        self.timeouts.validate()?;
        if !(1..=10).contains(&self.max_retries) {
            return Err(format!("max_retries must be in 1..=10 (got {})", self.max_retries));
        }
        Ok(())
    }
}

/// Emit a WARN log when any unknown keys are present. Call on every load.
pub fn warn_unknown_keys(provider_name: &str, opts: &ProviderOptions) {
    if !opts.extra.is_empty() {
        let keys: Vec<&str> = opts.extra.keys().map(String::as_str).collect();
        tracing::warn!(
            provider = provider_name,
            unknown_keys = ?keys,
            "provider options contain unknown keys — possible typo"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_connect_secs_is_10() {
        assert_eq!(TimeoutsConfig::default().connect_secs, 10);
    }

    #[test]
    fn default_request_secs_is_120() {
        assert_eq!(TimeoutsConfig::default().request_secs, 120);
    }

    #[test]
    fn default_stream_inactivity_secs_is_60() {
        assert_eq!(TimeoutsConfig::default().stream_inactivity_secs, 60);
    }

    #[test]
    fn default_stream_max_duration_secs_is_600() {
        assert_eq!(TimeoutsConfig::default().stream_max_duration_secs, 600);
    }

    #[test]
    fn json_roundtrip_partial_object_fills_defaults() {
        let input = r#"{"request_secs": 30}"#;
        let cfg: TimeoutsConfig = serde_json::from_str(input).unwrap();
        assert_eq!(cfg.connect_secs, 10);
        assert_eq!(cfg.request_secs, 30);
        assert_eq!(cfg.stream_inactivity_secs, 60);
        assert_eq!(cfg.stream_max_duration_secs, 600);
    }

    #[test]
    fn json_roundtrip_empty_object_fills_all_defaults() {
        let cfg: TimeoutsConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, TimeoutsConfig::default());
    }

    #[test]
    fn provider_options_default_has_default_timeouts() {
        let opts = ProviderOptions::default();
        assert_eq!(opts.timeouts, TimeoutsConfig::default());
        assert!(opts.api_key_envs.is_empty());
        assert!(opts.extra.is_empty());
    }

    #[test]
    fn provider_options_roundtrip_nested_timeouts() {
        let input = r#"{"timeouts":{"request_secs":45},"api_key_envs":["KEY"]}"#;
        let opts: ProviderOptions = serde_json::from_str(input).unwrap();
        assert_eq!(opts.timeouts.request_secs, 45);
        assert_eq!(opts.api_key_envs, vec!["KEY".to_string()]);
    }

    #[test]
    fn unknown_fields_land_in_extra_not_silently_dropped() {
        let input = r#"{"timeouts":{},"mystery":"wut","other":123}"#;
        let opts: ProviderOptions = serde_json::from_str(input).unwrap();
        assert_eq!(opts.extra.get("mystery").and_then(|v| v.as_str()), Some("wut"));
        assert_eq!(opts.extra.get("other").and_then(|v| v.as_i64()), Some(123));
    }

    #[test]
    fn legacy_flat_timeout_secs_lands_in_extra() {
        // Proves the migrator (Task 7) is the only path that resurrects
        // the legacy key. Loaders see it in `extra` and will warn.
        let input = r#"{"timeout_secs":120}"#;
        let opts: ProviderOptions = serde_json::from_str(input).unwrap();
        assert_eq!(opts.timeouts, TimeoutsConfig::default());
        assert_eq!(opts.extra.get("timeout_secs").and_then(|v| v.as_u64()), Some(120));
    }

    #[test]
    fn validate_rejects_connect_zero() {
        let cfg = TimeoutsConfig { connect_secs: 0, ..Default::default() };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("connect_secs"), "{err}");
    }

    #[test]
    fn validate_rejects_connect_over_120() {
        let cfg = TimeoutsConfig { connect_secs: 121, ..Default::default() };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("connect_secs"), "{err}");
    }

    #[test]
    fn validate_accepts_request_zero() {
        let cfg = TimeoutsConfig { request_secs: 0, ..Default::default() };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_request_over_3600() {
        let cfg = TimeoutsConfig { request_secs: 3601, ..Default::default() };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("request_secs"), "{err}");
    }

    #[test]
    fn validate_rejects_stream_max_over_7200() {
        let cfg = TimeoutsConfig { stream_max_duration_secs: 7201, ..Default::default() };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("stream_max_duration_secs"), "{err}");
    }

    #[test]
    fn validate_rejects_stream_inactivity_over_3600() {
        let cfg = TimeoutsConfig { stream_inactivity_secs: 3601, ..Default::default() };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("stream_inactivity_secs"), "{err}");
    }

    #[test]
    fn validate_accepts_all_boundary_values() {
        let cfg = TimeoutsConfig {
            connect_secs: 1,
            request_secs: 3600,
            stream_inactivity_secs: 3600,
            stream_max_duration_secs: 7200,
            run_max_duration_secs: 0,
        };
        assert!(cfg.validate().is_ok());
        let cfg2 = TimeoutsConfig {
            connect_secs: 120,
            request_secs: 0,
            stream_inactivity_secs: 0,
            stream_max_duration_secs: 0,
            run_max_duration_secs: 0,
        };
        assert!(cfg2.validate().is_ok());
    }

    #[test]
    fn run_max_duration_secs_defaults_to_zero() {
        assert_eq!(TimeoutsConfig::default().run_max_duration_secs, 0);
    }

    #[test]
    fn run_max_duration_secs_round_trips_json() {
        let input = r#"{"run_max_duration_secs": 3600}"#;
        let cfg: TimeoutsConfig = serde_json::from_str(input).unwrap();
        assert_eq!(cfg.run_max_duration_secs, 3600);
    }

    #[test]
    fn run_max_duration_secs_zero_is_valid() {
        let cfg = TimeoutsConfig { run_max_duration_secs: 0, ..Default::default() };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_accepts_run_max_duration_secs_boundary() {
        let cfg = TimeoutsConfig { run_max_duration_secs: 86400, ..Default::default() };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn validate_rejects_run_max_duration_secs_over_86400() {
        let cfg = TimeoutsConfig { run_max_duration_secs: 86401, ..Default::default() };
        let err = cfg.validate().unwrap_err();
        assert!(err.contains("run_max_duration_secs"), "{err}");
    }
}
