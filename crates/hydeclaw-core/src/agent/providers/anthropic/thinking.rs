//! Anthropic-specific "thinking" (extended reasoning) helpers. Decides
//! whether a model supports thinking, and builds the request-side config
//! block when it does.

#[derive(Debug, PartialEq)]
enum ThinkingMode {
    /// Opus 4.7+ and Mythos: only adaptive supported (manual → 400 error).
    AdaptiveOnly,
    /// Opus 4.6, Sonnet 4.6: adaptive recommended, manual deprecated.
    Adaptive,
    /// All others: manual budget_tokens.
    Manual,
}

fn thinking_mode(model: &str) -> ThinkingMode {
    if model.contains("claude-opus-4-7") || model.contains("claude-mythos") {
        ThinkingMode::AdaptiveOnly
    } else if model.contains("claude-opus-4-6") || model.contains("claude-sonnet-4-6") {
        ThinkingMode::Adaptive
    } else {
        ThinkingMode::Manual
    }
}

/// Returns the thinking config JSON value, or None if thinking should be disabled.
/// `effective_max_tokens` = `self.max_tokens.unwrap_or(8_192)`.
pub(super) fn thinking_config(level: u8, model: &str, effective_max_tokens: u32) -> Option<serde_json::Value> {
    if level == 0 {
        return None;
    }
    match thinking_mode(model) {
        ThinkingMode::AdaptiveOnly | ThinkingMode::Adaptive => {
            let effort = match level {
                1 | 2 => "low",
                3 => "medium",
                _ => "high",
            };
            Some(serde_json::json!({
                "type": "adaptive",
                "effort": effort,
                "display": "summarized"
            }))
        }
        ThinkingMode::Manual => {
            let budget: u32 = match level {
                1 => 1_024,
                2 => 4_096,
                3 => 10_000,
                4 => 20_000,
                _ => 32_000,
            };
            let clamped = budget.min(effective_max_tokens.saturating_sub(1_000));
            if clamped < 1_024 {
                tracing::warn!(
                    thinking_level = level,
                    model,
                    effective_max_tokens,
                    budget,
                    clamped,
                    "thinking disabled: budget after clamping is below 1024 — increase max_tokens"
                );
                return None;
            }
            Some(serde_json::json!({
                "type": "enabled",
                "budget_tokens": clamped
            }))
        }
    }
}

#[cfg(test)]
mod thinking_config_tests {
    use super::*;
    use super::super::{AnthropicProvider, CallOptions};

    #[test]
    fn level_zero_returns_none() {
        assert!(thinking_config(0, "claude-opus-4-7", 8_192).is_none());
    }

    #[test]
    fn opus47_level1_adaptive_low() {
        let cfg = thinking_config(1, "claude-opus-4-7", 8_192).unwrap();
        assert_eq!(cfg["type"], "adaptive");
        assert_eq!(cfg["effort"], "low");
        assert_eq!(cfg["display"], "summarized");
    }

    #[test]
    fn opus47_level3_adaptive_medium() {
        let cfg = thinking_config(3, "claude-opus-4-7", 8_192).unwrap();
        assert_eq!(cfg["type"], "adaptive");
        assert_eq!(cfg["effort"], "medium");
        assert_eq!(cfg["display"], "summarized");
    }

    #[test]
    fn opus46_level5_adaptive_high() {
        let cfg = thinking_config(5, "claude-opus-4-6", 16_000).unwrap();
        assert_eq!(cfg["type"], "adaptive");
        assert_eq!(cfg["effort"], "high");
        assert_eq!(cfg["display"], "summarized");
    }

    #[test]
    fn sonnet37_level3_manual_exact_budget() {
        let cfg = thinking_config(3, "claude-sonnet-3-7", 16_000).unwrap();
        assert_eq!(cfg["type"], "enabled");
        assert_eq!(cfg["budget_tokens"], 10_000_u64);
    }

    #[test]
    fn sonnet37_level3_budget_clamped() {
        let cfg = thinking_config(3, "claude-sonnet-3-7", 8_192).unwrap();
        assert_eq!(cfg["budget_tokens"], 7_192_u64);
    }

    #[test]
    fn tight_max_tokens_returns_none() {
        assert!(thinking_config(5, "claude-haiku-4-5", 2_000).is_none());
    }

    #[test]
    fn thinking_mode_opus47_is_adaptive_only() {
        assert!(matches!(thinking_mode("claude-opus-4-7"), ThinkingMode::AdaptiveOnly));
    }

    #[test]
    fn thinking_mode_sonnet46_is_adaptive() {
        assert!(matches!(thinking_mode("claude-sonnet-4-6"), ThinkingMode::Adaptive));
    }

    #[test]
    fn thinking_mode_sonnet37_is_manual() {
        assert!(matches!(thinking_mode("claude-sonnet-3-7"), ThinkingMode::Manual));
    }

    #[test]
    fn thinking_mode_haiku45_is_manual() {
        assert!(matches!(thinking_mode("claude-haiku-4-5"), ThinkingMode::Manual));
    }

    #[tokio::test]
    async fn temperature_enforced_to_1_when_thinking_enabled() {
        use std::sync::Arc;
        let secrets = Arc::new(crate::secrets::SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-opus-4-7".to_string(),
            0.3,
            Some(16_000),
            secrets,
        );
        let opts = CallOptions { thinking_level: 3, ..Default::default() };
        let (_, body) = provider.build_request_body(&[], &[], opts);
        let temp = body["temperature"].as_f64().expect("temperature must be in body");
        assert!(temp >= 1.0, "expected temperature >= 1.0 when thinking enabled, got {temp}");
        assert!(body.get("thinking").is_some(), "thinking field must be present");
    }

    #[tokio::test]
    async fn temperature_unchanged_when_thinking_disabled() {
        use std::sync::Arc;
        let secrets = Arc::new(crate::secrets::SecretsManager::new_noop());
        let provider = AnthropicProvider::for_tests(
            "claude-opus-4-7".to_string(),
            0.7,
            Some(16_000),
            secrets,
        );
        let opts = CallOptions { thinking_level: 0, ..Default::default() };
        let (_, body) = provider.build_request_body(&[], &[], opts);
        let temp = body["temperature"].as_f64().unwrap();
        assert!((temp - 0.7).abs() < f64::EPSILON);
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn manual_thinking_config_has_no_display_field() {
        let cfg = thinking_config(3, "claude-sonnet-3-7", 16_000).unwrap();
        assert_eq!(cfg["type"], "enabled");
        assert!(cfg.get("display").is_none(), "manual config must not contain 'display' field; got: {cfg}");
    }
}
