use serde::Serialize;

use super::AppConfig;

/// A single field-level validation error returned by [`validate_config`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ValidationError {
    /// Dot-separated field path (e.g. "`toolgate_url`", "gateway.listen").
    pub field: String,
    /// Human-readable error description.
    pub message: String,
}

/// Validate semantic constraints on a parsed [`AppConfig`].
///
/// Returns a list of errors that should block saving. An empty list means the
/// config is semantically valid. This runs in addition to (not instead of) the
/// TOML parse validation already performed by `AppConfig::load()`.
///
/// # Scope
/// - URL format validation for optional URL fields
/// - Non-empty validation for required string fields
/// - Numeric range validation for fields with documented constraints
///
/// # Non-errors
/// - `None` on any `Option<T>` field — absence is valid unless the field is required
/// - `max_requests_per_minute = 0` — documented as "0 = disable rate limiting"
/// - `request_timeout_secs = 0` — documented as "0 = no limit"
pub fn validate_config(cfg: &AppConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // toolgate_url: if present and non-empty, must start with http:// or https://
    if let Some(url) = &cfg.toolgate_url
        && !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError {
                field: "toolgate_url".to_string(),
                message: "must start with http:// or https://".to_string(),
            });
        }

    // gateway.listen: must not be empty
    if cfg.gateway.listen.trim().is_empty() {
        errors.push(ValidationError {
            field: "gateway.listen".to_string(),
            message: "must not be empty (e.g. \"0.0.0.0:18789\")".to_string(),
        });
    }

    // gateway.public_url: if present and non-empty, must start with http:// or https://
    if let Some(url) = &cfg.gateway.public_url
        && !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
            errors.push(ValidationError {
                field: "gateway.public_url".to_string(),
                message: "must start with http:// or https://".to_string(),
            });
        }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    fn base_config() -> AppConfig {
        // Load from a minimal TOML string to produce a valid AppConfig with all defaults
        toml::from_str::<AppConfig>(
            r#"
            [gateway]
            listen = "0.0.0.0:18789"
            [database]
            url = "postgres://localhost/test"
            "#,
        )
        .expect("base config should parse")
    }

    #[test]
    fn test_validate_config_valid_toolgate_http() {
        let mut cfg = base_config();
        cfg.toolgate_url = Some("http://localhost:8080".to_string());
        assert!(validate_config(&cfg).is_empty());
    }

    #[test]
    fn test_validate_config_valid_toolgate_https() {
        let mut cfg = base_config();
        cfg.toolgate_url = Some("https://toolgate.example.com".to_string());
        assert!(validate_config(&cfg).is_empty());
    }

    #[test]
    fn test_validate_config_valid_toolgate_none() {
        let mut cfg = base_config();
        cfg.toolgate_url = None;
        assert!(validate_config(&cfg).is_empty());
    }

    #[test]
    fn test_validate_config_invalid_toolgate_url() {
        let mut cfg = base_config();
        cfg.toolgate_url = Some("not-a-url".to_string());
        let errors = validate_config(&cfg);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "toolgate_url");
    }

    #[test]
    fn test_validate_config_empty_listen() {
        let mut cfg = base_config();
        cfg.gateway.listen = "".to_string();
        let errors = validate_config(&cfg);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "gateway.listen");
    }

    #[test]
    fn test_validate_config_invalid_public_url() {
        let mut cfg = base_config();
        cfg.gateway.public_url = Some("ftp://bad-scheme".to_string());
        let errors = validate_config(&cfg);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "gateway.public_url");
    }

    #[test]
    fn test_validate_config_zero_max_requests_is_ok() {
        // 0 = disable rate limiting — not an error
        let mut cfg = base_config();
        cfg.limits.max_requests_per_minute = 0;
        assert!(validate_config(&cfg).is_empty());
    }

    #[test]
    fn test_validate_config_default_config_is_valid() {
        let cfg = base_config();
        assert!(validate_config(&cfg).is_empty(), "default config must be valid");
    }
}
