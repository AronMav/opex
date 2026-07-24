use serde::{Deserialize, Deserializer};

use crate::config::AgentConfig;

// ── Deserialization helpers ──────────────────────────────

/// Deserialize a field that distinguishes absent (preserve) from explicit null (clear).
/// absent → None (outer), explicit null → Some(None) (inner), value → Some(Some(T)).
pub(crate) fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

// ── Validation ──────────────────────────────────────────

pub(crate) fn agent_config_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new("config/agents").join(format!("{name}.toml"))
}

pub(crate) fn validate_agent_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err("name must be 1-32 characters".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("name must contain only alphanumeric, dash, or underscore".into());
    }
    Ok(())
}

// ── Payload types ───────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct AgentCreatePayload {
    pub name: String,
    pub language: Option<String>,
    /// Name of the row in the `profiles` table this agent resolves providers
    /// from. `None`/empty → `crate::db::profiles::DEFAULT_PROFILE` on create;
    /// preserved from disk on update when absent (see `api_update_agent`).
    #[serde(default)]
    pub profile: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// Nullable fields: absent = preserve existing, explicit null = clear, value = update.
    #[serde(default, deserialize_with = "nullable")]
    pub access: Option<Option<AccessPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub heartbeat: Option<Option<HeartbeatPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub tools: Option<Option<ToolPolicyPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub compaction: Option<Option<CompactionPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub skill_review: Option<Option<SkillReviewPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub session: Option<Option<SessionPayload>>,
    pub max_tools_in_context: Option<usize>,
    #[serde(default, deserialize_with = "nullable")]
    pub routing: Option<Option<Vec<RoutingRulePayload>>>,
    pub voice: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub approval: Option<Option<ApprovalPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub tool_loop: Option<Option<ToolLoopPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub tool_dispatcher: Option<Option<ToolDispatcherPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub watchdog: Option<Option<WatchdogPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub hooks: Option<Option<HooksPayload>>,
    pub max_history_messages: Option<usize>,
    /// Enable Anthropic prompt caching for this agent. `None` = keep existing / default false.
    pub prompt_cache: Option<bool>,
    pub daily_budget_tokens: Option<u64>,
    /// Cap on fallback attempts per request in multi-provider routing (default 3).
    pub max_failover_attempts: Option<u32>,
    #[serde(default, deserialize_with = "nullable")]
    pub soul: Option<Option<SoulPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub drift: Option<Option<DriftPayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub initiative: Option<Option<InitiativePayload>>,
    #[serde(default, deserialize_with = "nullable")]
    pub emotion: Option<Option<EmotionPayload>>,
}

#[derive(Deserialize)]
pub(crate) struct HooksPayload {
    pub log_all_tool_calls: Option<bool>,
    pub block_tools: Option<Vec<String>>,
    pub webhooks: Option<Vec<crate::config::WebhookConfig>>,
    /// Operator knobs NOT rendered in the UI form — kept in the payload so they
    /// can be preserved on a UI round-trip (F-04 audit: previously absent, so
    /// they silently reset to defaults on every UI save).
    pub total_webhook_timeout_ms: Option<u64>,
    pub on_chain_timeout: Option<crate::config::FailureMode>,
}

#[derive(Deserialize)]
pub(crate) struct ApprovalPayload {
    pub enabled: Option<bool>,
    pub require_for: Option<Vec<String>>,
    pub require_for_categories: Option<Vec<String>>,
    pub timeout_seconds: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct RoutingRulePayload {
    pub condition: Option<String>,
    pub connection: Option<String>,
    pub model: Option<String>,
    pub temperature: Option<f64>,
    pub cooldown_secs: Option<u64>,
}

#[derive(Deserialize)]
pub(crate) struct AccessPayload {
    pub mode: Option<String>,
    pub owner_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct HeartbeatPayload {
    pub cron: String,
    pub timezone: Option<String>,
    pub announce_to: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct ToolPolicyPayload {
    pub allow: Option<Vec<String>>,
    pub deny: Option<Vec<String>>,
    pub allow_all: Option<bool>,
    pub deny_all_others: Option<bool>,
    pub groups: Option<crate::config::ToolGroups>,
}

#[derive(Deserialize)]
pub(crate) struct CompactionPayload {
    pub enabled: Option<bool>,
    pub threshold: Option<f64>,
    pub preserve_tool_calls: Option<bool>,
    pub preserve_last_n: Option<u32>,
    pub max_context_tokens: Option<u32>,
    pub protect_first_n: Option<usize>,
    pub summary_target_ratio: Option<f64>,
    pub anti_thrash_min_savings: Option<f64>,
    pub anti_thrash_max_skips: Option<u8>,
    pub extract_to_memory: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct SkillReviewPayload {
    pub enabled: Option<bool>,
    pub min_tool_calls: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct SessionPayload {
    pub dm_scope: Option<String>,
    pub ttl_days: Option<u32>,
    pub max_messages: Option<u32>,
    pub prune_tool_output_after_turns: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct ToolLoopPayload {
    pub max_iterations: Option<usize>,
    pub compact_on_overflow: Option<bool>,
    pub detect_loops: Option<bool>,
    pub warn_threshold: Option<usize>,
    pub break_threshold: Option<usize>,
    pub max_consecutive_failures: Option<usize>,
    pub max_auto_continues: Option<u8>,
    pub max_loop_nudges: Option<usize>,
    pub ngram_cycle_length: Option<usize>,
    pub error_break_threshold: Option<usize>,
}

#[derive(Deserialize)]
pub(crate) struct ToolDispatcherPayload {
    pub enabled: Option<bool>,
    pub core_extra: Option<Vec<String>>,
    pub promotion_max: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct WatchdogPayload {
    pub inactivity_secs: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SoulPayload {
    pub enabled: Option<bool>,
    pub reflection_threshold: Option<f64>,
    pub reflection_cooldown_minutes: Option<u64>,
    pub context_top_k: Option<usize>,
    pub context_budget_tokens: Option<u32>,
    pub max_events_per_session: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct DriftPayload {
    pub enabled: Option<bool>,
    pub threshold: Option<f32>,
    pub min_history: Option<usize>,
    pub baseline_turns: Option<usize>,
    pub z_fire: Option<f32>,
    pub z_release: Option<f32>,
    pub correct: Option<bool>,
    pub anchor: Option<String>,
    pub ecp: Option<bool>,
    pub ecp_recent_turns: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct InitiativePayload {
    pub enabled: Option<bool>,
    pub daily_proposal_cap: Option<u32>,
    pub decompose: Option<bool>,
    pub daily_plan: Option<bool>,
    pub auto_approve_day_plan: Option<bool>,
    pub daily_token_budget: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct EmotionPayload {
    pub enabled: Option<bool>,
    pub intensity_importance_k: Option<f32>,
    pub blend_rate: Option<f32>,
    pub decay_half_life_hours: Option<f32>,
    pub render_to_prompt: Option<bool>,
    pub coping: Option<bool>,
}

// ── Config builder ──────────────────────────────────────

pub(crate) fn build_agent_config(name: String, p: AgentCreatePayload) -> AgentConfig {
    use crate::config::{AgentConfig, AgentSettings, AgentAccessConfig, HeartbeatConfig, AgentToolPolicy, CompactionConfig, DelegationConfig, SoulConfig, DriftConfig, InitiativeConfig, EmotionConfig, ToolDispatcherConfig};

    AgentConfig {
        agent: AgentSettings {
            name,
            language: p.language.unwrap_or_else(|| "ru".to_string()),
            profile: p.profile.filter(|s| !s.is_empty())
                .unwrap_or_else(|| crate::db::profiles::DEFAULT_PROFILE.to_string()),
            // DEPRECATED (m084/profiles): no longer settable via the API — the
            // `profiles` table is now the source of truth. Retained as empty/None
            // on `AgentSettings` only because the startup migration still reads
            // them from pre-existing TOML on disk (see config/mod.rs).
            provider: String::new(),
            model: String::new(),
            provider_connection: None,
            fallback_provider: None,
            tts_provider: None,
            imagegen_provider: None,
            temperature: p.temperature.unwrap_or(1.0),
            max_tokens: p.max_tokens,
            access: p.access.flatten().map(|a| AgentAccessConfig {
                // Secure by default: an access section with no explicit mode is
                // treated as "restricted", never "open".
                mode: a.mode.unwrap_or_else(|| "restricted".to_string()),
                owner_id: a.owner_id,
            }),
            heartbeat: p.heartbeat.flatten().map(|h| HeartbeatConfig {
                cron: h.cron,
                timezone: h.timezone,
                announce_to: h.announce_to,
            }),
            tools: p.tools.flatten().map(|t| AgentToolPolicy {
                allow: t.allow.unwrap_or_default(),
                deny: t.deny.unwrap_or_default(),
                allow_all: t.allow_all.unwrap_or(false),
                deny_all_others: t.deny_all_others.unwrap_or(false),
                groups: t.groups.unwrap_or_default(),
            }),
            delegation: DelegationConfig::default(),
            soul: p.soul.flatten().map(|s| SoulConfig {
                enabled: s.enabled.unwrap_or(false),
                reflection_threshold: s.reflection_threshold.unwrap_or(150.0),
                reflection_cooldown_minutes: s.reflection_cooldown_minutes.unwrap_or(60),
                context_top_k: s.context_top_k.unwrap_or(6),
                context_budget_tokens: s.context_budget_tokens.unwrap_or(800),
                max_events_per_session: s.max_events_per_session.unwrap_or(10),
            }).unwrap_or_default(),
            drift: p.drift.flatten().map(|d| DriftConfig {
                enabled: d.enabled.unwrap_or(false),
                threshold: d.threshold.unwrap_or(0.15),
                min_history: d.min_history.unwrap_or(6),
                baseline_turns: d.baseline_turns.unwrap_or(8),
                z_fire: d.z_fire.unwrap_or(2.5),
                z_release: d.z_release.unwrap_or(1.0),
                correct: d.correct.unwrap_or(false),
                anchor: d.anchor.filter(|s| !s.is_empty()),
                ecp: d.ecp.unwrap_or(false),
                ecp_recent_turns: d.ecp_recent_turns.unwrap_or(1),
            }).unwrap_or_default(),
            initiative: p.initiative.flatten().map(|i| InitiativeConfig {
                enabled: i.enabled.unwrap_or(false),
                daily_proposal_cap: i.daily_proposal_cap.unwrap_or(1),
                decompose: i.decompose.unwrap_or(false),
                daily_plan: i.daily_plan.unwrap_or(false),
                auto_approve_day_plan: i.auto_approve_day_plan.unwrap_or(false),
                daily_token_budget: i.daily_token_budget.unwrap_or(0),
            }).unwrap_or_default(),
            emotion: p.emotion.flatten().map(|e| EmotionConfig {
                enabled: e.enabled.unwrap_or(false),
                intensity_importance_k: e.intensity_importance_k.unwrap_or(3.0),
                blend_rate: e.blend_rate.unwrap_or(0.3),
                decay_half_life_hours: e.decay_half_life_hours.unwrap_or(12.0),
                render_to_prompt: e.render_to_prompt.unwrap_or(false),
                coping: e.coping.unwrap_or(false),
            }).unwrap_or_default(),
            compaction: p.compaction.flatten().map(|c| CompactionConfig {
                enabled: c.enabled.unwrap_or(true),
                threshold: c.threshold.unwrap_or(0.8),
                preserve_tool_calls: c.preserve_tool_calls.unwrap_or(false),
                preserve_last_n: c.preserve_last_n.unwrap_or(10),
                max_context_tokens: c.max_context_tokens,
                protect_first_n: c.protect_first_n.unwrap_or(3),
                summary_target_ratio: c.summary_target_ratio.unwrap_or(0.20),
                anti_thrash_min_savings: c.anti_thrash_min_savings.unwrap_or(0.10),
                anti_thrash_max_skips: c.anti_thrash_max_skips.unwrap_or(2),
                extract_to_memory: c.extract_to_memory.unwrap_or(true),
            }),
            skill_review: p.skill_review.flatten().map(|sr| crate::config::SkillReviewConfig {
                enabled: sr.enabled.unwrap_or(false),
                min_tool_calls: sr.min_tool_calls.unwrap_or(3),
            }),
            max_tools_in_context: p.max_tools_in_context,
            routing: p.routing.flatten().unwrap_or_default().into_iter().map(|r| {
                crate::config::ProviderRouteConfig {
                    condition: r.condition.unwrap_or_else(|| "default".to_string()),
                    connection: r.connection,
                    model: r.model,
                    temperature: r.temperature,
                    cooldown_secs: r.cooldown_secs.unwrap_or(60),
                }
            }).collect(),
            session: p.session.flatten().map(|s| crate::config::SessionConfig {
                dm_scope: s.dm_scope.unwrap_or_else(|| "per-channel-peer".to_string()),
                ttl_days: s.ttl_days.unwrap_or(30),
                max_messages: s.max_messages.unwrap_or(0),
                prune_tool_output_after_turns: s.prune_tool_output_after_turns,
            }),
            approval: p.approval.flatten().map(|a| crate::config::ApprovalConfig {
                enabled: a.enabled.unwrap_or(false),
                require_for: a.require_for.unwrap_or_default(),
                require_for_categories: a.require_for_categories.unwrap_or_default(),
                timeout_seconds: a.timeout_seconds.unwrap_or(300),
            }),
            tool_loop: p.tool_loop.flatten().map(|tl| crate::config::ToolLoopSettings {
                max_iterations: tl.max_iterations.unwrap_or(50),
                compact_on_overflow: tl.compact_on_overflow.unwrap_or(true),
                detect_loops: tl.detect_loops.unwrap_or(true),
                warn_threshold: tl.warn_threshold.unwrap_or(5),
                break_threshold: tl.break_threshold.unwrap_or(10),
                max_consecutive_failures: tl.max_consecutive_failures.unwrap_or(3),
                max_auto_continues: tl.max_auto_continues.unwrap_or(5),
                max_loop_nudges: tl.max_loop_nudges.unwrap_or(3),
                ngram_cycle_length: tl.ngram_cycle_length.unwrap_or(6),
                error_break_threshold: tl.error_break_threshold,
            }),
            watchdog: p.watchdog.flatten().map(|w| crate::config::WatchdogConfig {
                inactivity_secs: w.inactivity_secs.unwrap_or(600),
            }),
            max_history_messages: p.max_history_messages,
            prompt_cache: p.prompt_cache.unwrap_or(false),
            hooks: p.hooks.flatten().map(|h| crate::config::HooksConfig {
                log_all_tool_calls: h.log_all_tool_calls.unwrap_or(false),
                block_tools: h.block_tools.unwrap_or_default(),
                webhooks: h.webhooks.unwrap_or_default(),
                // F-04 audit: these two operator knobs are no longer clobbered
                // to defaults on every UI save (HooksPayload carries them now
                // + api_update_agent restores them from existing config).
                total_webhook_timeout_ms: h.total_webhook_timeout_ms,
                on_chain_timeout: h.on_chain_timeout.unwrap_or_default(),
            }),
            daily_budget_tokens: p.daily_budget_tokens.unwrap_or(0),
            // Default 3 matches the `#[serde(default)]` on AgentSettings.
            max_failover_attempts: p.max_failover_attempts.unwrap_or(3),
            base: false,
            tool_dispatcher: p.tool_dispatcher.flatten().map(|td| ToolDispatcherConfig {
                enabled: td.enabled.unwrap_or(false),
                core_extra: td.core_extra.unwrap_or_default(),
                promotion_max: td.promotion_max.unwrap_or(8),
            }).unwrap_or_default(),
        },
    }
}

// ── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── validate_agent_name ──────────────────────────────

    #[test]
    fn validate_agent_name_accepts_simple_name() {
        assert!(validate_agent_name("Arty").is_ok());
    }

    #[test]
    fn validate_agent_name_accepts_dash_and_underscore() {
        assert!(validate_agent_name("my-agent_1").is_ok());
    }

    #[test]
    fn validate_agent_name_accepts_single_char() {
        assert!(validate_agent_name("A").is_ok());
    }

    #[test]
    fn validate_agent_name_accepts_32_chars() {
        assert!(validate_agent_name(&"a".repeat(32)).is_ok());
    }

    #[test]
    fn validate_agent_name_rejects_empty() {
        assert!(validate_agent_name("").is_err());
    }

    #[test]
    fn validate_agent_name_rejects_33_chars() {
        assert!(validate_agent_name(&"a".repeat(33)).is_err());
    }

    #[test]
    fn validate_agent_name_rejects_space() {
        assert!(validate_agent_name("my agent").is_err());
    }

    #[test]
    fn validate_agent_name_rejects_at_sign() {
        assert!(validate_agent_name("my@agent").is_err());
    }

    #[test]
    fn validate_agent_name_rejects_dot() {
        assert!(validate_agent_name("my.agent").is_err());
    }

    #[test]
    fn validate_agent_name_rejects_path_traversal() {
        // Guards api_delete_agent's soul-backup write and config path.
        assert!(validate_agent_name("../etc/passwd").is_err());
        assert!(validate_agent_name("a/b").is_err());
        assert!(validate_agent_name("..").is_err());
    }

    // ── build_agent_config ───────────────────────────────

    fn minimal_payload(name: &str) -> AgentCreatePayload {
        AgentCreatePayload {
            name: name.to_string(),
            language: None,
            profile: None,
            temperature: None,
            max_tokens: None,
            access: None,
            heartbeat: None,
            tools: None,
            compaction: None,
            skill_review: None,
            session: None,
            max_tools_in_context: None,
            routing: None,
            voice: None,
            approval: None,
            tool_loop: None,
            tool_dispatcher: None,
            watchdog: None,
            hooks: None,
            max_history_messages: None,
            prompt_cache: None,
            daily_budget_tokens: None,
            max_failover_attempts: None,
            soul: None,
            drift: None,
            initiative: None,
            emotion: None,
        }
    }

    #[test]
    fn build_agent_config_applies_defaults() {
        let payload = minimal_payload("TestAgent");
        let config = build_agent_config("TestAgent".to_string(), payload);
        assert_eq!(config.agent.name, "TestAgent");
        assert_eq!(config.agent.language, "ru");
        assert_eq!(config.agent.profile, crate::db::profiles::DEFAULT_PROFILE);
        assert!((config.agent.temperature - 1.0).abs() < f64::EPSILON);
        assert_eq!(config.agent.max_failover_attempts, 3);
        assert!(!config.agent.base);
        assert!(config.agent.access.is_none());
        assert!(config.agent.heartbeat.is_none());
        assert!(config.agent.tools.is_none());
        assert!(config.agent.compaction.is_none());
        assert!(config.agent.session.is_none());
        assert!(config.agent.approval.is_none());
        assert!(config.agent.tool_loop.is_none());
        assert!(config.agent.watchdog.is_none());
        assert!(config.agent.hooks.is_none());
        assert_eq!(config.agent.daily_budget_tokens, 0);
        assert!(config.agent.routing.is_empty());
    }

    #[test]
    fn build_agent_config_uses_explicit_language() {
        let mut payload = minimal_payload("TestAgent");
        payload.language = Some("en".to_string());
        let config = build_agent_config("TestAgent".to_string(), payload);
        assert_eq!(config.agent.language, "en");
    }

    #[test]
    fn build_agent_config_uses_explicit_profile() {
        let mut payload = minimal_payload("TestAgent");
        payload.profile = Some("Voice".to_string());
        let config = build_agent_config("TestAgent".to_string(), payload);
        assert_eq!(config.agent.profile, "Voice");
    }

    #[test]
    fn build_agent_config_empty_profile_falls_back_to_default() {
        let mut payload = minimal_payload("TestAgent");
        payload.profile = Some(String::new());
        let config = build_agent_config("TestAgent".to_string(), payload);
        assert_eq!(config.agent.profile, crate::db::profiles::DEFAULT_PROFILE);
    }

    #[test]
    fn build_agent_config_uses_explicit_temperature() {
        let mut payload = minimal_payload("TestAgent");
        payload.temperature = Some(0.5);
        let config = build_agent_config("TestAgent".to_string(), payload);
        assert!((config.agent.temperature - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn build_agent_config_name_comes_from_argument_not_payload() {
        // The `name` argument to build_agent_config is the canonical name;
        // payload.name is ignored in the builder.
        let payload = minimal_payload("PayloadName");
        let config = build_agent_config("ArgName".to_string(), payload);
        assert_eq!(config.agent.name, "ArgName");
    }

    #[test]
    fn build_agent_config_max_failover_attempts_explicit() {
        let mut payload = minimal_payload("TestAgent");
        payload.max_failover_attempts = Some(7);
        let config = build_agent_config("TestAgent".to_string(), payload);
        assert_eq!(config.agent.max_failover_attempts, 7);
    }

    // ── soul/drift/initiative/emotion mapping ────────────

    #[test]
    fn build_agent_config_maps_soul_payload() {
        let mut p = minimal_payload("T");
        p.soul = Some(Some(SoulPayload {
            enabled: Some(true),
            reflection_threshold: Some(200.0),
            ..Default::default()
        }));
        let cfg = build_agent_config("T".into(), p);
        assert!(cfg.agent.soul.enabled);
        assert_eq!(cfg.agent.soul.reflection_threshold, 200.0);
        // unset fields fall back to config defaults
        assert_eq!(cfg.agent.soul.context_top_k, 6);
    }

    #[test]
    fn build_agent_config_absent_soul_is_default() {
        let cfg = build_agent_config("T".into(), minimal_payload("T"));
        assert!(!cfg.agent.soul.enabled);
    }

    #[test]
    fn build_agent_config_maps_drift_payload() {
        let mut p = minimal_payload("T");
        p.drift = Some(Some(DriftPayload {
            enabled: Some(true),
            threshold: Some(0.42),
            anchor: Some("Owner is Bogdan".to_string()),
            ..Default::default()
        }));
        let cfg = build_agent_config("T".into(), p);
        assert!(cfg.agent.drift.enabled);
        assert!((cfg.agent.drift.threshold - 0.42).abs() < f32::EPSILON);
        assert_eq!(cfg.agent.drift.anchor.as_deref(), Some("Owner is Bogdan"));
        // unset fields fall back to config defaults
        assert_eq!(cfg.agent.drift.min_history, 6);
        assert_eq!(cfg.agent.drift.baseline_turns, 8);
        assert!((cfg.agent.drift.z_fire - 2.5).abs() < f32::EPSILON);
        assert!((cfg.agent.drift.z_release - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn build_agent_config_drift_empty_anchor_becomes_none() {
        let mut p = minimal_payload("T");
        p.drift = Some(Some(DriftPayload {
            anchor: Some(String::new()),
            ..Default::default()
        }));
        let cfg = build_agent_config("T".into(), p);
        assert!(cfg.agent.drift.anchor.is_none());
    }

    #[test]
    fn build_agent_config_absent_drift_is_default() {
        let cfg = build_agent_config("T".into(), minimal_payload("T"));
        assert!(!cfg.agent.drift.enabled);
    }

    #[test]
    fn build_agent_config_maps_initiative_payload() {
        let mut p = minimal_payload("T");
        p.initiative = Some(Some(InitiativePayload {
            enabled: Some(true),
            daily_proposal_cap: Some(5),
            daily_token_budget: Some(10_000),
            ..Default::default()
        }));
        let cfg = build_agent_config("T".into(), p);
        assert!(cfg.agent.initiative.enabled);
        assert_eq!(cfg.agent.initiative.daily_proposal_cap, 5);
        assert_eq!(cfg.agent.initiative.daily_token_budget, 10_000);
        // unset fields fall back to config defaults
        assert!(!cfg.agent.initiative.decompose);
        assert!(!cfg.agent.initiative.daily_plan);
        assert!(!cfg.agent.initiative.auto_approve_day_plan);
    }

    #[test]
    fn build_agent_config_absent_initiative_is_default() {
        let cfg = build_agent_config("T".into(), minimal_payload("T"));
        assert!(!cfg.agent.initiative.enabled);
        assert_eq!(cfg.agent.initiative.daily_proposal_cap, 1);
    }

    #[test]
    fn build_agent_config_maps_emotion_payload() {
        let mut p = minimal_payload("T");
        p.emotion = Some(Some(EmotionPayload {
            enabled: Some(true),
            blend_rate: Some(0.5),
            ..Default::default()
        }));
        let cfg = build_agent_config("T".into(), p);
        assert!(cfg.agent.emotion.enabled);
        assert!((cfg.agent.emotion.blend_rate - 0.5).abs() < f32::EPSILON);
        // unset fields fall back to config defaults
        assert!((cfg.agent.emotion.intensity_importance_k - 3.0).abs() < f32::EPSILON);
        assert!((cfg.agent.emotion.decay_half_life_hours - 12.0).abs() < f32::EPSILON);
    }

    #[test]
    fn build_agent_config_absent_emotion_is_default() {
        let cfg = build_agent_config("T".into(), minimal_payload("T"));
        assert!(!cfg.agent.emotion.enabled);
    }
}
