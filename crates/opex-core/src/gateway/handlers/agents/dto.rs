use std::collections::HashMap;
use uuid::Uuid;
use crate::config::AgentConfig;
use crate::uploads::{mint_uploads_url, HISTORICAL_URL_TTL_SECS};

// ── Struct definitions (leaf module — no crate-internal imports) ─────────────
// Pulled in via include! so that `lib.rs` can expose dto_structs.rs under the
// `ts-gen` feature without cascading config/memory/etc. into the lib facade.
include!("dto_structs.rs");

// ── icon_url helper ──────────────────────────────────────────────────────────

/// Build a long-TTL signed `/api/uploads/{id}` URL when both an icon upload
/// ID (from a precomputed batch lookup) and an HMAC key are available.
/// Returns `None` otherwise so JSON callers can distinguish "no icon" from
/// "icon unsigned".
///
/// `icon_ids` is built once per request by the handler via
/// `db::uploads::list_agent_icon_ids` to avoid an N+1 per-DTO DB lookup.
fn signed_icon_url(
    agent_name: &str,
    icon_ids: &HashMap<String, Uuid>,
    upload_key: Option<&[u8; 32]>,
) -> Option<String> {
    let id = icon_ids.get(agent_name)?;
    let key = upload_key?;
    // base = "" — relative URL, the UI joins origin itself.
    Some(mint_uploads_url("", *id, key, HISTORICAL_URL_TTL_SECS))
}

// ── capabilities helper ──────────────────────────────────────────────────────

/// A capability is `true` when the resolved profile has at least one provider
/// entry in that slot. Callers resolve `Slots` via
/// `agent::profile_resolver::resolve_slots_for_agent` (single agent, has DB)
/// or `agent::profile_resolver::resolve_slots_offline` (batch list, no
/// per-agent round trip) before calling this — kept a pure function of
/// `Slots` so the DTO constructors stay synchronous and unit-testable.
pub(crate) fn capabilities_from_slots(slots: &crate::db::profiles::Slots) -> AgentCapabilitiesDto {
    let has = |key: &str| slots.get(key).is_some_and(|v| !v.is_empty());
    AgentCapabilitiesDto {
        text: has("text"),
        stt: has("stt"),
        tts: has("tts"),
        vision: has("vision"),
        imagegen: has("imagegen"),
        websearch: has("websearch"),
    }
}

// ── Constructor impl ─────────────────────────────────────────────────────────

impl AgentDetailDto {
    #[allow(clippy::too_many_arguments)]
    pub fn from_config(
        cfg: &AgentConfig,
        is_running: bool,
        config_dirty: bool,
        voice: Option<String>,
        icon_ids: &HashMap<String, Uuid>,
        upload_key: Option<&[u8; 32]>,
        profile_slots: &crate::db::profiles::Slots,
    ) -> Self {
        let a = &cfg.agent;
        Self {
            name: a.name.clone(),
            language: a.language.clone(),
            profile: a.profile.clone(),
            capabilities: capabilities_from_slots(profile_slots),
            temperature: a.temperature,
            max_tokens: a.max_tokens,
            access: a.access.as_ref().map(|ac| AgentDetailAccessDto {
                mode: ac.mode.clone(),
                owner_id: ac.owner_id.clone(),
            }),
            heartbeat: a.heartbeat.as_ref().map(|h| AgentDetailHeartbeatDto {
                cron: h.cron.clone(),
                timezone: h.timezone.clone(),
                announce_to: h.announce_to.clone(),
            }),
            tools: a.tools.as_ref().map(|t| AgentDetailToolsDto {
                allow: t.allow.clone(),
                deny: t.deny.clone(),
                allow_all: t.allow_all,
                deny_all_others: t.deny_all_others,
                groups: AgentDetailToolGroupsDto {
                    git: t.groups.git,
                    tool_management: t.groups.tool_management,
                    skill_editing: t.groups.skill_editing,
                    session_tools: t.groups.session_tools,
                },
            }),
            compaction: a.compaction.as_ref().map(|c| AgentDetailCompactionDto {
                enabled: c.enabled,
                threshold: c.threshold,
                preserve_tool_calls: c.preserve_tool_calls,
                preserve_last_n: c.preserve_last_n,
                max_context_tokens: c.max_context_tokens,
            }),
            skill_review: a.skill_review.as_ref().map(|sr| AgentDetailSkillReviewDto {
                enabled: sr.enabled,
                min_tool_calls: sr.min_tool_calls,
            }),
            session: a.session.as_ref().map(|s| AgentDetailSessionDto {
                dm_scope: s.dm_scope.clone(),
                ttl_days: s.ttl_days,
                max_messages: s.max_messages,
                prune_tool_output_after_turns: s.prune_tool_output_after_turns,
            }),
            icon_url: signed_icon_url(&a.name, icon_ids, upload_key),
            max_tools_in_context: a.max_tools_in_context,
            tool_loop: a.tool_loop.as_ref().map(|tl| AgentDetailToolLoopDto {
                max_iterations: tl.max_iterations,
                compact_on_overflow: tl.compact_on_overflow,
                detect_loops: tl.detect_loops,
                warn_threshold: tl.warn_threshold,
                break_threshold: tl.break_threshold,
                max_consecutive_failures: tl.max_consecutive_failures,
                max_auto_continues: tl.max_auto_continues,
                max_loop_nudges: tl.max_loop_nudges,
                ngram_cycle_length: tl.ngram_cycle_length,
            }),
            tool_dispatcher: Some(AgentDetailToolDispatcherDto {
                enabled: a.tool_dispatcher.enabled,
                core_extra: a.tool_dispatcher.core_extra.clone(),
                promotion_max: a.tool_dispatcher.promotion_max,
            }),
            approval: a.approval.as_ref().map(|ap| AgentDetailApprovalDto {
                enabled: ap.enabled,
                require_for: ap.require_for.clone(),
                require_for_categories: ap.require_for_categories.clone(),
                timeout_seconds: ap.timeout_seconds,
            }),
            routing: a.routing.iter().map(|r| AgentDetailRoutingDto {
                condition: r.condition.clone(),
                connection: r.connection.clone(),
                model: r.model.clone(),
                temperature: r.temperature,
                cooldown_secs: r.cooldown_secs,
            }).collect(),
            watchdog: a.watchdog.as_ref().map(|w| AgentDetailWatchdogDto {
                inactivity_secs: w.inactivity_secs,
            }),
            hooks: a.hooks.as_ref().map(|h| AgentDetailHooksDto {
                log_all_tool_calls: h.log_all_tool_calls,
                block_tools: h.block_tools.clone(),
                webhooks: h.webhooks.iter().map(|w| WebhookDto {
                    url: w.url.clone(),
                    events: w.events.clone(),
                    mode: match w.mode {
                        crate::config::WebhookMode::Async => "async",
                        crate::config::WebhookMode::Decision => "decision",
                    }.to_string(),
                    tool_matcher: w.tool_matcher.clone(),
                    on_failure: match w.on_failure {
                        crate::config::FailureMode::Open => "open",
                        crate::config::FailureMode::Closed => "closed",
                    }.to_string(),
                    timeout_ms: w.timeout_ms,
                    allow_internal: w.allow_internal,
                }).collect(),
            }),
            max_history_messages: a.max_history_messages,
            daily_budget_tokens: a.daily_budget_tokens,
            max_failover_attempts: a.max_failover_attempts,
            is_running,
            config_dirty,
            voice,
        }
    }
}

impl AgentInfoDto {
    #[allow(clippy::too_many_arguments)]
    pub fn from_config(
        cfg: &AgentConfig,
        routing_count: usize,
        is_running: bool,
        config_dirty: bool,
        base: Option<bool>,
        pending_delete: Option<bool>,
        icon_ids: &HashMap<String, Uuid>,
        upload_key: Option<&[u8; 32]>,
        profile_slots: &crate::db::profiles::Slots,
    ) -> Self {
        let a = &cfg.agent;
        Self {
            name: a.name.clone(),
            language: a.language.clone(),
            profile: a.profile.clone(),
            capabilities: capabilities_from_slots(profile_slots),
            icon_url: signed_icon_url(&a.name, icon_ids, upload_key),
            temperature: a.temperature,
            has_access: a.access.is_some(),
            access_mode: a.access.as_ref().map(|ac| ac.mode.clone()),
            has_heartbeat: a.heartbeat.is_some(),
            heartbeat_cron: a.heartbeat.as_ref().map(|h| h.cron.clone()),
            heartbeat_timezone: a.heartbeat.as_ref().and_then(|h| h.timezone.clone()),
            tool_policy: a.tools.as_ref().map(|t| AgentInfoToolPolicyDto {
                allow: t.allow.clone(),
                deny: t.deny.clone(),
                allow_all: t.allow_all,
            }),
            routing_count,
            is_running,
            config_dirty,
            base,
            pending_delete,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;

    fn load_fixture(name: &str) -> AgentConfig {
        let path = format!("{}/tests/fixtures/agents/{name}.toml", env!("CARGO_MANIFEST_DIR"));
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        toml::from_str(&content).unwrap_or_else(|e| panic!("parse {path}: {e}"))
    }

    #[test]
    fn agent_detail_dto_snapshot_min() {
        let cfg = load_fixture("SnapshotMin");
        let icons: HashMap<String, Uuid> = HashMap::new();
        let slots = crate::db::profiles::Slots::new();
        let dto = AgentDetailDto::from_config(&cfg, false, false, None, &icons, None, &slots);
        insta::assert_json_snapshot!("agent_detail_snapshot_min", dto);
    }

    #[test]
    fn agent_detail_dto_snapshot_full() {
        let cfg = load_fixture("SnapshotFull");
        let icons: HashMap<String, Uuid> = HashMap::new();
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert("text".into(), vec![crate::db::profiles::SlotEntry {
            provider: "main-claude".into(), model: None, voice: None,
        }]);
        slots.insert("tts".into(), vec![]);
        let dto = AgentDetailDto::from_config(&cfg, false, false, None, &icons, None, &slots);
        insta::assert_json_snapshot!("agent_detail_snapshot_full", dto);
    }

    #[test]
    fn agent_info_dto_snapshot_min() {
        let cfg = load_fixture("SnapshotMin");
        let icons: HashMap<String, Uuid> = HashMap::new();
        let slots = crate::db::profiles::Slots::new();
        let dto = AgentInfoDto::from_config(&cfg, 0, false, false, Some(false), None, &icons, None, &slots);
        insta::assert_json_snapshot!("agent_info_snapshot_min", dto);
    }

    #[test]
    fn capabilities_from_slots_true_only_for_nonempty_vec() {
        let mut slots = crate::db::profiles::Slots::new();
        slots.insert("text".into(), vec![crate::db::profiles::SlotEntry {
            provider: "p".into(), model: None, voice: None,
        }]);
        slots.insert("tts".into(), vec![]); // present but empty → false
        let caps = capabilities_from_slots(&slots);
        assert!(caps.text);
        assert!(!caps.tts);
        assert!(!caps.stt);
        assert!(!caps.vision);
        assert!(!caps.imagegen);
        assert!(!caps.websearch);
    }

    #[test]
    fn hooks_dto_serializes_webhooks() {
        let dto = AgentDetailHooksDto {
            log_all_tool_calls: false,
            block_tools: vec![],
            webhooks: vec![WebhookDto {
                url: "https://x/h".into(),
                events: vec!["BeforeToolCall".into()],
                mode: "decision".into(),
                tool_matcher: Some("code_.*".into()),
                on_failure: "closed".into(),
                timeout_ms: 1500,
                allow_internal: true,
            }],
        };
        let j = serde_json::to_value(&dto).unwrap();
        assert_eq!(j["webhooks"][0]["mode"], "decision");
        assert_eq!(j["webhooks"][0]["on_failure"], "closed");
        assert_eq!(j["webhooks"][0]["timeout_ms"], 1500);
    }
}
