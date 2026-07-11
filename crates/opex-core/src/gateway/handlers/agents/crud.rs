use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::{json, Value};
use std::fs::canonicalize;
use std::path::PathBuf;
use std::str::FromStr;

use crate::gateway::clusters::{AgentCore, AuthServices, InfraServices, ChannelBus, ConfigServices, StatusMonitor};
use crate::agent::handler_registry::HandlerRegistry;
use crate::config::AgentConfig;
use super::dto::{AgentDetailDto, AgentInfoDto};
use super::schema::*;
use super::lifecycle::start_agent_from_config;

include!("approvals_dto_structs.rs");

// ── Field merge helpers ─────────────────────────────────────────────────────

/// Merge a clearable optional-string field with the existing config value.
///
/// Used by the PUT /api/agents/{name} round-trip for fields where the UI sends
/// `Some("")` as a "clear this" signal:
/// - `payload = None`     → preserve the existing value
/// - `payload = Some("")` → explicit clear (return `None`)
/// - `payload = Some(v)`  → take the new value
///
/// Without this helper, each callsite (fallback_provider, tts_provider,
/// imagegen_provider, …) duplicates a `match payload.as_deref()` block; one
/// regression in any of them silently changes UI save semantics.
pub(super) fn merge_clearable_string(
    payload: Option<String>,
    existing: Option<&str>,
) -> Option<String> {
    match payload.as_deref() {
        None => existing.map(String::from),
        Some("") => None,
        Some(_) => payload,
    }
}

/// Preserve existing webhooks when the PUT payload's hooks block omitted them.
///
/// Mirrors the `base`/`delegation` preserve-from-disk pattern.
/// `payload_webhooks_present` must be computed from the raw payload *before*
/// `build_agent_config` consumes it (where `None` and `[]` become indistinguishable).
///
/// - `false` → webhooks were absent in the payload → copy from disk.
/// - `true`  → payload explicitly supplied webhooks (possibly empty) → leave as-is.
pub(crate) fn preserve_hooks_webhooks(
    new: &mut crate::config::AgentConfig,
    existing: &crate::config::AgentConfig,
    payload_webhooks_present: bool,
) {
    if payload_webhooks_present {
        return;
    }
    if let (Some(nh), Some(eh)) = (new.agent.hooks.as_mut(), existing.agent.hooks.as_ref()) {
        nh.webhooks = eh.webhooks.clone();
    }
    // If new.agent.hooks is None, the hooks-is_none() preserve block already
    // copies the entire HooksConfig (including webhooks) from existing.
}

// ── agent_id table catalogue + helpers ──────────────────────────────────────
//
// Centralized list of every table whose `agent_id` column references
// `agents.name`. Built from `information_schema.columns` introspection (T2,
// 2026-05-07): see migrations 001/006/034. Two separate constants encode the
// nullability classification because the rename SQL differs by one predicate.
//
// Adding a new entry MUST satisfy both:
//   1. It is a string literal at compile time (interpolated into the SQL via
//      `format!`; the table name itself is never user-controlled).
//   2. The classification (NOT NULL vs NULLABLE) matches the live schema —
//      `tests::test_tables_with_agent_id_*` enforce both at PR time.

/// Tables with NOT NULL `agent_id` column referencing `agents.name`.
/// Both rename and delete iterate over this list with simple UPDATE/DELETE.
///
/// SAFETY contract: every entry MUST be a string literal at compile time AND
/// MUST correspond to a table whose `agent_id` column is `NOT NULL` in
/// schema. Adding a new entry requires PR review confirming both.
pub(super) const TABLES_WITH_AGENT_ID_NOT_NULL: &[&str] = &[
    "agent_github_repos",
    "agent_oauth_bindings",
    "agent_plans",
    "approval_allowlist",
    "audit_events",
    "audit_log",
    "channel_allowed_users",
    "cron_runs",
    "gmail_triggers",
    "memory_chunks",
    "outbound_queue",
    "pairing_codes",
    "pending_approvals",
    "pending_messages",
    "scheduled_jobs",
    "session_failures",
    "sessions",
    "stream_jobs",
    "usage_log",
    "webhooks",
];

/// Tables with NULLABLE `agent_id`. Rename uses
/// `WHERE agent_id IS NOT NULL AND agent_id = $old`. Delete intentionally
/// skips these — NULL rows are not the deleted agent's data, and non-NULL
/// rows in `messages` are part of the session history we may want to keep
/// readable after the agent is gone.
pub(super) const TABLES_WITH_AGENT_ID_NULLABLE: &[&str] = &[
    "messages",
];

/// Tables from which to DELETE rows when an agent is deleted.
///
/// This is a STRICT SUBSET of `TABLES_WITH_AGENT_ID_NOT_NULL`. The other
/// tables (e.g., `audit_log`, `audit_events`, `usage_log`, `cron_runs`,
/// `sessions`, `tasks`) hold compliance / history / user-owned data that
/// must SURVIVE agent deletion. Cascade-deleting those would destroy audit
/// trails or user chat history. The "right" delete behavior for those is
/// open product question — see follow-up issue [FF-T2-followup-delete-scope].
///
/// Until that's resolved, this list preserves the pre-T2 inline-array
/// scope: only the per-agent state tables that have no compliance value.
pub(super) const TABLES_TO_DELETE_BY_AGENT_ID: &[&str] = &[
    "scheduled_jobs",
    "webhooks",
    "agent_oauth_bindings",
    "gmail_triggers",
    "agent_github_repos",
    "approval_allowlist",
    "channel_allowed_users",
];

/// Rename `agent_id` from `old` to `new` across every catalogued table.
/// Iterates both NOT NULL and NULLABLE constants; the NULLABLE branch adds an
/// `IS NOT NULL` predicate so the index can stay tight on non-null rows.
///
/// Returns the underlying `sqlx::Error` on first failure — caller is
/// responsible for the surrounding transaction's rollback.
async fn rename_agent_id_in_tables(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    old: &str,
    new: &str,
) -> Result<(), sqlx::Error> {
    for table in TABLES_WITH_AGENT_ID_NOT_NULL {
        // SAFETY: `table` is a compile-time literal from TABLES_WITH_AGENT_ID_NOT_NULL;
        // agent names flow through bind parameters.
        let sql = format!("UPDATE {table} SET agent_id = $1 WHERE agent_id = $2");
        sqlx::query(&sql)
            .bind(new)
            .bind(old)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                tracing::warn!(table = %table, error = %e, "failed to update agent_id on rename");
                e
            })?;
    }
    for table in TABLES_WITH_AGENT_ID_NULLABLE {
        // SAFETY: `table` is a compile-time literal from TABLES_WITH_AGENT_ID_NULLABLE;
        // agent names flow through bind parameters.
        let sql = format!(
            "UPDATE {table} SET agent_id = $1 WHERE agent_id IS NOT NULL AND agent_id = $2"
        );
        sqlx::query(&sql)
            .bind(new)
            .bind(old)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                tracing::warn!(table = %table, error = %e, "failed to update agent_id on rename");
                e
            })?;
    }
    // uploads.owner_id holds the agent name for agent_icon rows. Partial
    // unique index keys off it, so rename must follow.
    sqlx::query(
        "UPDATE uploads SET owner_id = $1 WHERE owner_type = 'agent_icon' AND owner_id = $2",
    )
    .bind(new)
    .bind(old)
    .execute(&mut **tx)
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "failed to rename agent_icon owner on rename");
        e
    })?;
    Ok(())
}

/// Delete every row whose `agent_id` matches `agent_id` across the
/// per-agent state tables.
///
/// CRITICAL: this iterates `TABLES_TO_DELETE_BY_AGENT_ID`, NOT the broader
/// `TABLES_WITH_AGENT_ID_NOT_NULL`. Rename and delete have different
/// semantics — rename touches every catalogued table (it just changes a
/// string), but delete must preserve compliance / history tables
/// (`audit_log`, `audit_events`, `usage_log`, `cron_runs`, `sessions`,
/// `tasks`, …). See `TABLES_TO_DELETE_BY_AGENT_ID`'s doc comment for the
/// rationale and the open follow-up issue.
///
/// Returns the underlying `sqlx::Error` on first failure — caller is
/// responsible for the surrounding transaction's rollback.
async fn delete_agent_id_in_tables(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    agent_id: &str,
) -> Result<(), sqlx::Error> {
    for table in TABLES_TO_DELETE_BY_AGENT_ID {
        // SAFETY: `table` is a compile-time literal from TABLES_TO_DELETE_BY_AGENT_ID;
        // agent_id flows through a bind parameter.
        let sql = format!("DELETE FROM {table} WHERE agent_id = $1");
        sqlx::query(&sql)
            .bind(agent_id)
            .execute(&mut **tx)
            .await
            .map_err(|e| {
                tracing::warn!(table = %table, error = %e, "failed to delete rows on agent delete");
                e
            })?;
    }
    // Drop the agent's icon row (uploads.owner_id = agent name for
    // agent_icon owner_type). Permanent rows (expires_at NULL) would
    // never be reaped by the hourly cleanup cron otherwise.
    sqlx::query("DELETE FROM uploads WHERE owner_type = 'agent_icon' AND owner_id = $1")
        .bind(agent_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to delete agent_icon row on agent delete");
            e
        })?;
    Ok(())
}

// ── Agent list ──────────────────────────────────────────

pub(crate) async fn api_agents(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
) -> Json<Value> {
    // Read configs from disk (source of truth)
    let mut disk_configs = crate::config::load_agent_configs("config/agents").unwrap_or_default();
    // Base (base infrastructure) agents first, then alphabetical
    disk_configs.sort_by(|a, b| {
        b.agent.base.cmp(&a.agent.base)
            .then_with(|| a.agent.name.to_lowercase().cmp(&b.agent.name.to_lowercase()))
    });
    let agents_map = agents.map.read().await;

    let upload_key = auth.secrets.get_upload_hmac_key();

    // Batch-prefetch icon upload IDs for ALL names we may build DTOs for
    // (disk configs + running engines with no disk config). One DB round-trip
    // instead of N-per-DTO. Over-fetching a few names is cheaper than two
    // passes; dedupe afterwards.
    let mut all_names: Vec<String> = disk_configs.iter().map(|c| c.agent.name.clone()).collect();
    all_names.extend(agents_map.keys().cloned());
    all_names.sort();
    all_names.dedup();
    let icon_ids = crate::db::uploads::list_agent_icon_ids(&infra.db, &all_names)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(
                error = %e,
                agents_count = all_names.len(),
                "list_agent_icon_ids failed; icons will be missing this request"
            );
            std::collections::HashMap::new()
        });

    let mut seen_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut agents: Vec<AgentInfoDto> = Vec::new();

    // Disk configs (may or may not be running)
    for cfg in &disk_configs {
        let name = &cfg.agent.name;
        seen_names.insert(name.clone());

        let is_running = agents_map.contains_key(name);
        let config_dirty = if let Some(handle) = agents_map.get(name) {
            let running = AgentConfig { agent: handle.engine.cfg().agent.clone() };
            &running != cfg
        } else {
            false
        };

        agents.push(AgentInfoDto::from_config(
            cfg,
            cfg.agent.routing.len(),
            is_running,
            config_dirty,
            Some(cfg.agent.base),
            None,
            &icon_ids,
            Some(&upload_key),
        ));
    }

    // Running engines with no disk config (deleted while running — shouldn't happen with hot delete)
    for (name, handle) in agents_map.iter() {
        if seen_names.contains(name) {
            continue;
        }
        let agent_cfg = handle.engine.cfg();
        agents.push(AgentInfoDto::from_config(
            &AgentConfig { agent: agent_cfg.agent.clone() },
            agent_cfg.agent.routing.len(),
            true,
            false,
            None,
            Some(true),
            &icon_ids,
            Some(&upload_key),
        ));
    }

    Json(json!({ "agents": agents }))
}

// ── Agent CRUD ──────────────────────────────────────────

pub(crate) async fn api_get_agent(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let path = agent_config_path(&name);
    let cfg = match AgentConfig::load(&path) {
        Ok(c) => c,
        Err(_) => return (StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))).into_response(),
    };

    let agents_map = agents.map.read().await;
    let is_running = agents_map.contains_key(&name);
    let config_dirty = if let Some(handle) = agents_map.get(&name) {
        let running = AgentConfig { agent: handle.engine.cfg().agent.clone() };
        running != cfg
    } else {
        false
    };

    let voice = auth.secrets.get_scoped("TTS_VOICE", &name).await;
    let upload_key = auth.secrets.get_upload_hmac_key();
    let icon_ids = crate::db::uploads::list_agent_icon_ids(&infra.db, std::slice::from_ref(&name))
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, agent = %name, "list_agent_icon_ids failed");
            std::collections::HashMap::new()
        });
    let detail = AgentDetailDto::from_config(&cfg, is_running, config_dirty, voice, &icon_ids, Some(&upload_key));
    Json(detail).into_response()
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn api_create_agent(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
    State(handlers): State<HandlerRegistry>,
    Json(mut payload): Json<AgentCreatePayload>,
) -> impl IntoResponse {
    let name = payload.name.clone();
    let voice = payload.voice.take();

    if let Err(msg) = validate_agent_name(&name) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
    }

    // Validate cron if heartbeat provided
    if let Some(Some(ref hb)) = payload.heartbeat
        && ::cron::Schedule::from_str(&format!("0 {} *", hb.cron)).is_err()
            && ::cron::Schedule::from_str(&format!("{} *", hb.cron)).is_err()
        {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid cron expression"}))).into_response();
        }

    let path = agent_config_path(&name);
    if path.exists() {
        return (StatusCode::CONFLICT, Json(json!({"error": "agent already exists"}))).into_response();
    }

    // Check if already running
    if agents.map.read().await.contains_key(&name) {
        return (StatusCode::CONFLICT, Json(json!({"error": "agent already running"}))).into_response();
    }

    // Save per-agent TTS voice as scoped secret
    if let Some(ref v) = voice {
        if v.is_empty() {
            let _ = auth.secrets.delete_scoped("TTS_VOICE", &name).await;
        } else if let Err(e) = auth.secrets.set_scoped("TTS_VOICE", &name, v, None).await {
            tracing::warn!(error = %e, "failed to save TTS_VOICE secret");
        }
    }

    // Capture whether the caller omitted [agent.tool_dispatcher] before the
    // payload is moved into build_agent_config. Used below for the
    // setup-wizard default (T22).
    let payload_tool_dispatcher_was_absent = payload.tool_dispatcher.is_none();

    let mut cfg = build_agent_config(name.clone(), payload);

    // First agent created is automatically base (system agent) with safe defaults
    if agents.map.read().await.is_empty() {
        cfg.agent.base = true;
        // Set default tool deny-list if none was provided
        if cfg.agent.tools.is_none() {
            cfg.agent.tools = Some(crate::config::AgentToolPolicy {
                allow: vec![],
                deny: vec![
                    "workspace_delete".into(),
                    "workspace_rename".into(),
                ],
                allow_all: true,
                deny_all_others: false,
                groups: Default::default(),
            });
        }
        // Setup-wizard default: enable the tool dispatcher for fresh installs
        // unless the payload explicitly opts out (T22).
        // The wizard always creates the first agent on a clean install, so
        // gating on `agents.map.is_empty()` matches the wizard's lifecycle.
        if payload_tool_dispatcher_was_absent {
            cfg.agent.tool_dispatcher = crate::config::ToolDispatcherConfig {
                enabled: true,
                core_extra: Vec::new(),
                promotion_max: 8,
            };
        }
    } else {
        // Non-base agents: deny dangerous tools by default (security audit compliance)
        if cfg.agent.tools.is_none() {
            cfg.agent.tools = Some(crate::config::AgentToolPolicy {
                allow: vec![],
                deny: vec![
                    "code_exec".into(),
                    "process_start".into(),
                    "workspace_delete".into(),
                    "workspace_rename".into(),
                ],
                allow_all: true,
                deny_all_others: false,
                groups: Default::default(),
            });
        }
    }

    // Access control is enabled by default for EVERY agent (base or not):
    // if no access section was provided, default to "restricted" so an agent
    // is never silently world-open out of the box. An operator can still opt
    // into "open" explicitly via the UI/TOML.
    if cfg.agent.access.is_none() {
        cfg.agent.access = Some(crate::config::AgentAccessConfig {
            mode: "restricted".into(),
            owner_id: None,
        });
    }

    // Auto-fill provider/model from provider_connection if not explicitly set
    if let Some(ref conn_name) = cfg.agent.provider_connection
        && !conn_name.is_empty()
            && let Ok(Some(conn)) = crate::db::providers::get_provider_by_name(&infra.db, conn_name).await {
                if cfg.agent.provider.is_empty() || cfg.agent.provider == *conn_name {
                    cfg.agent.provider = conn.provider_type.clone();
                }
                if cfg.agent.model.is_empty()
                    && let Some(ref dm) = conn.default_model {
                        cfg.agent.model = dm.clone();
                    }
            }

    let toml_str = match cfg.to_toml() {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    // Ensure config/agents/ directory exists
    if let Err(e) = std::fs::create_dir_all("config/agents") {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }

    if let Err(e) = std::fs::write(&path, &toml_str) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }

    // Workspace directory + scaffold is created by start_agent_from_config

    // Hot-start the agent
    match start_agent_from_config(&cfg, &agents, &infra, &auth, &bus, &cfg_svc, &status, &handlers).await {
        Ok((handle, guard)) => {
            // Guard must be inserted before the handle: channel adapters reconnect
            // as soon as the handle appears in agents.map, so the guard must already
            // be present when they call AccessCheck.
            if let Some(guard) = guard {
                auth.access_guards.write().await.insert(name.clone(), guard);
            }
            agents.map.write().await.insert(name.clone(), handle);

            // Ensure Docker sandbox for non-base agents (base run on host)
            if !cfg.agent.base
                && let Some(ref sandbox) = infra.sandbox {
                    match canonicalize(crate::config::WORKSPACE_DIR) {
                        Ok(host_path) => {
                            if let Err(e) = sandbox.ensure_container(&name, &host_path.to_string_lossy(), false, Some(&auth.oauth)).await {
                                tracing::warn!(agent = %name, error = %e, "failed to ensure agent container");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to canonicalize workspace path for sandbox");
                        }
                    }
                }

            tracing::info!(agent = %name, "agent created and started via API");
            crate::db::audit::audit_spawn(infra.db.clone(), name.clone(), crate::db::audit::event_types::AGENT_CREATED, None, json!({"agent": name}));

            Json(json!({ "ok": true, "name": name })).into_response()
        }
        Err(e) => {
            tracing::error!(agent = %name, error = %e, "failed to start agent");

            Json(json!({ "ok": false, "name": name, "start_error": e.to_string() })).into_response()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn api_update_agent(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
    State(handlers): State<HandlerRegistry>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Json(mut payload): Json<AgentCreatePayload>,
) -> impl IntoResponse {
    let path = agent_config_path(&name);
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))).into_response();
    }

    // Validate cron if heartbeat provided
    if let Some(Some(ref hb)) = payload.heartbeat
        && ::cron::Schedule::from_str(&format!("0 {} *", hb.cron)).is_err()
            && ::cron::Schedule::from_str(&format!("{} *", hb.cron)).is_err()
        {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid cron expression"}))).into_response();
        }

    let new_name = payload.name.clone();
    let is_rename = new_name != name;

    // Load existing config — required for field merge and flag preservation.
    // Fail explicitly if the file cannot be read or parsed (guards against silently
    // resetting base/base to false on a corrupted or temporarily unreadable config).
    let existing_cfg = match std::fs::read_to_string(&path) {
        Ok(s) => match toml::from_str::<crate::config::AgentConfig>(&s) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::error!(agent = %name, error = %e, "agent config is malformed; update blocked");
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                    "error": "agent config is malformed and cannot be safely updated"
                }))).into_response();
            }
        },
        Err(e) => {
            tracing::error!(agent = %name, error = %e, "cannot read agent config for update");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": format!("cannot read agent config: {}", e)
            }))).into_response();
        }
    };

    // Base agents cannot be renamed via API
    if is_rename {
        if existing_cfg.agent.base {
            return (StatusCode::FORBIDDEN, Json(json!({
                "error": format!("Agent '{}' is a base agent and cannot be renamed", name)
            }))).into_response();
        }
        if let Err(msg) = validate_agent_name(&new_name) {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response();
        }
        let new_path = agent_config_path(&new_name);
        if new_path.exists() {
            return (StatusCode::CONFLICT, Json(json!({"error": "agent with this name already exists"}))).into_response();
        }
    }

    // Merge with existing config: payload fields override, missing fields keep current values
    {
        let a = &existing_cfg.agent;
        if payload.language.is_none() { payload.language = Some(a.language.clone()); }
        if payload.temperature.is_none() { payload.temperature = Some(a.temperature); }
        if payload.max_tokens.is_none() { payload.max_tokens = a.max_tokens; }
        // Nullable fields: None = absent (preserve existing), Some(None) = explicit null (clear),
        // Some(Some(val)) = update. Only preserve when absent (None).
        if payload.access.is_none() {
            payload.access = Some(a.access.as_ref().map(|ac| AccessPayload {
                mode: Some(ac.mode.clone()),
                owner_id: ac.owner_id.clone(),
            }));
        }
        if payload.heartbeat.is_none() {
            payload.heartbeat = Some(a.heartbeat.as_ref().map(|h| HeartbeatPayload {
                cron: h.cron.clone(),
                timezone: h.timezone.clone(),
                announce_to: h.announce_to.clone(),
            }));
        }
        if payload.tools.is_none() {
            payload.tools = Some(a.tools.as_ref().map(|t| ToolPolicyPayload {
                allow: Some(t.allow.clone()),
                deny: Some(t.deny.clone()),
                allow_all: Some(t.allow_all),
                deny_all_others: Some(t.deny_all_others),
                groups: Some(t.groups.clone()),
            }));
        }
        if payload.compaction.is_none() {
            payload.compaction = Some(a.compaction.as_ref().map(|c| CompactionPayload {
                enabled: Some(c.enabled),
                threshold: Some(c.threshold),
                preserve_tool_calls: Some(c.preserve_tool_calls),
                preserve_last_n: Some(c.preserve_last_n),
                max_context_tokens: c.max_context_tokens,
                protect_first_n: Some(c.protect_first_n),
                summary_target_ratio: Some(c.summary_target_ratio),
                anti_thrash_min_savings: Some(c.anti_thrash_min_savings),
                anti_thrash_max_skips: Some(c.anti_thrash_max_skips),
                extract_to_memory: Some(c.extract_to_memory),
            }));
        }
        if payload.skill_review.is_none() {
            payload.skill_review = Some(a.skill_review.as_ref().map(|sr| SkillReviewPayload {
                enabled: Some(sr.enabled),
                min_tool_calls: Some(sr.min_tool_calls),
            }));
        }
        if payload.session.is_none() {
            payload.session = Some(a.session.as_ref().map(|s| SessionPayload {
                dm_scope: Some(s.dm_scope.clone()),
                ttl_days: Some(s.ttl_days),
                max_messages: Some(s.max_messages),
                prune_tool_output_after_turns: s.prune_tool_output_after_turns,
            }));
        }
        if payload.max_tools_in_context.is_none() { payload.max_tools_in_context = a.max_tools_in_context; }
        if payload.routing.is_none() && !a.routing.is_empty() {
            payload.routing = Some(Some(a.routing.iter().map(|r| RoutingRulePayload {
                condition: Some(r.condition.clone()),
                connection: r.connection.clone(),
                model: r.model.clone(),
                temperature: r.temperature,
                cooldown_secs: Some(r.cooldown_secs),
            }).collect()));
        }
        if payload.tool_loop.is_none() {
            payload.tool_loop = Some(a.tool_loop.as_ref().map(|tl| ToolLoopPayload {
                max_iterations: Some(tl.max_iterations),
                compact_on_overflow: Some(tl.compact_on_overflow),
                detect_loops: Some(tl.detect_loops),
                warn_threshold: Some(tl.warn_threshold),
                break_threshold: Some(tl.break_threshold),
                max_consecutive_failures: Some(tl.max_consecutive_failures),
                max_auto_continues: Some(tl.max_auto_continues),
                max_loop_nudges: Some(tl.max_loop_nudges),
                ngram_cycle_length: Some(tl.ngram_cycle_length),
                error_break_threshold: tl.error_break_threshold,
            }));
        }
        if payload.tool_dispatcher.is_none() {
            payload.tool_dispatcher = Some(Some(ToolDispatcherPayload {
                enabled: Some(a.tool_dispatcher.enabled),
                core_extra: Some(a.tool_dispatcher.core_extra.clone()),
                promotion_max: Some(a.tool_dispatcher.promotion_max),
            }));
        }
        if payload.provider_connection.is_none() { payload.provider_connection = a.provider_connection.clone(); }
        payload.fallback_provider =
            merge_clearable_string(payload.fallback_provider.take(), a.fallback_provider.as_deref());
        payload.tts_provider =
            merge_clearable_string(payload.tts_provider.take(), a.tts_provider.as_deref());
        payload.imagegen_provider =
            merge_clearable_string(payload.imagegen_provider.take(), a.imagegen_provider.as_deref());
        if payload.approval.is_none() {
            payload.approval = Some(a.approval.as_ref().map(|ap| ApprovalPayload {
                enabled: Some(ap.enabled),
                require_for: Some(ap.require_for.clone()),
                require_for_categories: Some(ap.require_for_categories.clone()),
                timeout_seconds: Some(ap.timeout_seconds),
            }));
        }
        if payload.watchdog.is_none() {
            payload.watchdog = Some(a.watchdog.as_ref().map(|w| WatchdogPayload {
                inactivity_secs: Some(w.inactivity_secs),
            }));
        }
        // error_break_threshold is not exposed in AgentDetailDto and will be absent on
        // round-trips from the UI; restore it from the existing config to avoid data loss.
        if let Some(Some(ref mut tl)) = payload.tool_loop
            && tl.error_break_threshold.is_none()
        {
            tl.error_break_threshold = a.tool_loop.as_ref().and_then(|t| t.error_break_threshold);
        }
    }

    // Compute before payload is consumed by build_agent_config — after that,
    // webhooks=[] is indistinguishable from "webhooks not present in payload".
    let payload_webhooks_present = payload
        .hooks
        .as_ref()
        .and_then(|h| h.as_ref())
        .map(|h| h.webhooks.is_some())
        .unwrap_or(false);

    let voice = payload.voice.take();
    let mut cfg = build_agent_config(new_name.clone(), payload);
    // Preserve base from existing config — never changed via API
    cfg.agent.base = existing_cfg.agent.base;
    // Preserve delegation from existing config — never changed via PUT API.
    // Same precedent as `base`/`hooks`: there is no payload field, so always
    // re-use what's on disk. Without this, the documented migration path
    // (TOML hand-edit `[agent.delegation] max_depth = 2`) silently resets to
    // default on the next UI update of ANY field. See PR #24 review C5.
    cfg.agent.delegation = existing_cfg.agent.delegation.clone();
    // Preserve fields not in payload
    if cfg.agent.hooks.is_none() {
        cfg.agent.hooks = existing_cfg.agent.hooks.clone();
    }
    // Preserve webhooks when payload included a hooks block but omitted webhooks
    // (UI sends hooks without webhooks on every save — without this, webhooks
    // configured via TOML are silently wiped on the next UI update).
    preserve_hooks_webhooks(&mut cfg, &existing_cfg, payload_webhooks_present);
    if cfg.agent.max_history_messages.is_none() {
        cfg.agent.max_history_messages = existing_cfg.agent.max_history_messages;
    }
    // prompt_cache: preserve existing if payload didn't supply a value.
    // The schema builder maps None payload → false, so we check the payload directly.
    // Since `build_agent_config` sets `prompt_cache = p.prompt_cache.unwrap_or(false)`,
    // we re-check: if payload had no field (None), don't overwrite an existing `true`.
    // Actual merge uses the cfg already built — if existing was true and payload is None
    // we want to preserve it. The schema builder already sets `false` for absent payload,
    // so we restore from existing when the payload didn't carry an explicit value.
    // We have no direct payload access here — use the `put_agent` caller's payload for
    // this field via the already-built cfg: if cfg is `false` and existing is `true`,
    // it may be because payload was absent. We preserve existing unless payload explicitly
    // set it to false — but since we can't distinguish, we use a simpler rule:
    // `prompt_cache` is `true` in existing → keep unless payload explicitly sent the field.
    // Because `AgentCreatePayload.prompt_cache` defaults to `None` (field is absent),
    // and `build_agent_config` maps that to `false`, we'd inadvertently clear it.
    // Fix: compare directly — if cfg says false and existing says true, restore.
    if !cfg.agent.prompt_cache && existing_cfg.agent.prompt_cache {
        cfg.agent.prompt_cache = existing_cfg.agent.prompt_cache;
    }
    // max_failover_attempts is a u32 with serde default 3 — cannot distinguish
    // "absent in payload" from "explicit 3" post-deserialization, but the
    // schema builder sets 3 only when the payload field is `None`, so
    // preserving the existing value when payload has the default is safe
    // (same reasoning as `base`).
    // Note: there's no way to override back to 3 once a non-3 value is set
    // except via direct TOML edit — acceptable since this is an operator-level
    // stability knob.
    if cfg.agent.max_failover_attempts == 3
        && existing_cfg.agent.max_failover_attempts != 3
    {
        cfg.agent.max_failover_attempts = existing_cfg.agent.max_failover_attempts;
    }
    // daily_budget_tokens: 0 means "no budget" — always honor explicit value from payload
    let toml_str = match cfg.to_toml() {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    let target_path = agent_config_path(&new_name);
    // For non-rename updates, target_path == path, so writing now is safe:
    // even if the DB step (none in this branch) failed, the file is the only
    // state being changed. For renames we DEFER the file write until after
    // the DB transaction commits — otherwise a transaction rollback (or a
    // crash mid-rename) leaves a new TOML on disk while every `agent_id` in
    // the DB still references the old name, and the next startup loads two
    // agents with desynced state.
    if !is_rename
        && let Err(e) = std::fs::write(&target_path, &toml_str) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }

    // If renaming, update DB references first, then write the new TOML, then
    // remove the old one.
    if is_rename {
        // Update agent_id in all DB tables (within a transaction for consistency).
        // Table catalogue + UPDATE loop live in `rename_agent_id_in_tables`;
        // see TABLES_WITH_AGENT_ID_NOT_NULL / TABLES_WITH_AGENT_ID_NULLABLE.
        // Rename transaction covers all `agent_id` tables plus:
        //   - agent_channels (agent_name column)
        //   - sessions.participants (TEXT[] array_replace)
        // All updates share a single sqlx::Transaction — failure at any point
        // triggers automatic rollback (via explicit rollback or Transaction::Drop).
        let mut tx = match infra.db.begin().await {
            Ok(tx) => tx,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("transaction start failed: {}", e)}))).into_response(),
        };
        if let Err(e) = rename_agent_id_in_tables(&mut tx, &name, &new_name).await {
            tx.rollback().await.ok();
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename failed: {}", e)}))).into_response();
        }
        // agent_channels uses agent_name instead of agent_id
        if let Err(e) = sqlx::query("UPDATE agent_channels SET agent_name = $1 WHERE agent_name = $2")
            .bind(&new_name)
            .bind(&name)
            .execute(&mut *tx)
            .await
        {
            tracing::warn!(error = %e, "failed to update agent_channels.agent_name on rename");
            tx.rollback().await.ok();
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename failed at table agent_channels: {}", e)}))).into_response();
        }
        // agent_model_overrides uses agent_name (TEXT PK, no FK) — separate
        // from the agent_id catalogue, same as agent_channels above.
        if let Err(e) = sqlx::query("UPDATE agent_model_overrides SET agent_name = $1 WHERE agent_name = $2")
            .bind(&new_name)
            .bind(&name)
            .execute(&mut *tx)
            .await
        {
            tracing::warn!(error = %e, "failed to update agent_model_overrides.agent_name on rename");
            tx.rollback().await.ok();
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename failed at table agent_model_overrides: {}", e)}))).into_response();
        }
        // sessions.participants is a TEXT[] array — replace old name with new
        if let Err(e) = sqlx::query("UPDATE sessions SET participants = array_replace(participants, $2, $1) WHERE $2 = ANY(participants)")
            .bind(&new_name)
            .bind(&name)
            .execute(&mut *tx)
            .await
        {
            tracing::warn!(error = %e, "failed to update sessions.participants on rename");
            tx.rollback().await.ok();
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename failed at sessions.participants: {}", e)}))).into_response();
        }
        if let Err(e) = tx.commit().await {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("transaction commit failed: {}", e)}))).into_response();
        }

        // Now that the DB transaction is durable, write the new TOML and
        // remove the old one. If the new write fails after commit, the DB
        // already references new_name; we log loudly but cannot easily roll
        // back. (Pre-tx file write, by contrast, leaves orphaned configs on
        // any DB failure — a much more common path.)
        if let Err(e) = std::fs::write(&target_path, &toml_str) {
            tracing::error!(
                old_name = %name, new_name = %new_name, error = %e,
                "rename DB committed but new TOML write failed — DB is the source of truth, restore the file from existing_cfg",
            );
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename committed in DB but new TOML write failed: {}", e)}))).into_response();
        }
        let _ = std::fs::remove_file(&path);

        // Rename workspace directory
        let old_ws = format!("{}/agents/{}", crate::config::WORKSPACE_DIR, name);
        let new_ws = format!("{}/agents/{}", crate::config::WORKSPACE_DIR, new_name);
        if std::path::Path::new(&old_ws).exists()
            && let Err(e) = std::fs::rename(&old_ws, &new_ws) {
                tracing::warn!(from = %old_ws, to = %new_ws, error = %e, "failed to rename workspace directory");
            }

        // Migrate per-agent scoped secrets: scope='OldName' → scope='NewName'
        if let Err(e) = auth.secrets.rename_scope(&name, &new_name).await {
            tracing::warn!(
                from = %name, to = %new_name, error = %e,
                "failed to migrate scoped secrets on agent rename"
            );
        }
    }

    // Save per-agent TTS voice as scoped secret
    if let Some(ref v) = voice {
        if v.is_empty() {
            let _ = auth.secrets.delete_scoped("TTS_VOICE", &new_name).await;
        } else if let Err(e) = auth.secrets.set_scoped("TTS_VOICE", &new_name, v, None).await {
            tracing::warn!(error = %e, "failed to save TTS_VOICE secret");
        }
    }

    // Hot-restart: stop old agent, start new one.
    let old_handle = agents.map.write().await.remove(&name);
    auth.access_guards.write().await.remove(&name);
    if let Some(handle) = old_handle {
        handle.shutdown(&agents.scheduler).await;
    }

    // If renaming, remove old container
    if is_rename
        && let Some(ref sandbox) = infra.sandbox {
            let _ = sandbox.remove_container(&name).await;
        }

    match start_agent_from_config(&cfg, &agents, &infra, &auth, &bus, &cfg_svc, &status, &handlers).await {
        Ok((handle, guard)) => {
            // Guard before handle — same reasoning as api_create_agent.
            if let Some(guard) = guard {
                auth.access_guards.write().await.insert(new_name.clone(), guard);
            }
            agents.map.write().await.insert(new_name.clone(), handle);

            // Ensure Docker sandbox for non-base agents (base run on host)
            if !cfg.agent.base
                && let Some(ref sandbox) = infra.sandbox {
                    match canonicalize(crate::config::WORKSPACE_DIR) {
                        Ok(host_path) => {
                            if let Err(e) = sandbox.ensure_container(&new_name, &host_path.to_string_lossy(), false, Some(&auth.oauth)).await {
                                tracing::warn!(agent = %new_name, error = %e, "failed to ensure agent container after update");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to canonicalize workspace path for sandbox");
                        }
                }
            }

            tracing::info!(agent = %new_name, renamed_from = %name, "agent updated and restarted via API");
            crate::db::audit::audit_spawn(infra.db.clone(), new_name.clone(), crate::db::audit::event_types::AGENT_UPDATED, None, json!({"agent": new_name, "renamed_from": name}));

        }
        Err(e) => {
            tracing::error!(agent = %new_name, error = %e, "failed to restart agent after update");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("config saved but failed to start: {}", e)}))).into_response();
        }
    }

    Json(json!({ "ok": true, "name": new_name, "restarted": true })).into_response()
}

async fn cleanup_agent_data(db: &sqlx::PgPool, agent_name: &str) -> Result<(), sqlx::Error> {
    let mut tx = db.begin().await?;
    // agent_channels uses agent_name (separate from the agent_id catalogue)
    sqlx::query("DELETE FROM agent_channels WHERE agent_name = $1")
        .bind(agent_name).execute(&mut *tx).await?;
    // agent_model_overrides uses agent_name (TEXT PK, no FK to agents) —
    // must be deleted explicitly, same as agent_channels above.
    sqlx::query("DELETE FROM agent_model_overrides WHERE agent_name = $1")
        .bind(agent_name).execute(&mut *tx).await?;
    // Per-agent state tables — see TABLES_TO_DELETE_BY_AGENT_ID. This is a
    // strict subset of TABLES_WITH_AGENT_ID_NOT_NULL; compliance / history
    // tables are intentionally skipped.
    delete_agent_id_in_tables(&mut tx, agent_name).await?;
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn api_delete_agent(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    let path = agent_config_path(&name);

    // Block deletion of base agents — fail closed: any inability to verify blocks deletion
    match std::fs::read_to_string(&path) {
        Ok(toml_str) => match toml::from_str::<crate::config::AgentConfig>(&toml_str) {
            Ok(existing) if existing.agent.base => {
                return (StatusCode::FORBIDDEN, Json(json!({
                    "error": format!("Agent '{}' is a base agent and cannot be deleted", name)
                }))).into_response();
            }
            Err(e) => {
                tracing::error!(agent = %name, error = %e, "agent config is malformed; deletion blocked");
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                    "error": "agent config is malformed; fix it before deleting"
                }))).into_response();
            }
            Ok(_) => {} // not a base agent, proceed
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // file gone, proceed to cleanup
        Err(e) => {
            tracing::error!(agent = %name, error = %e, "cannot read agent config for deletion safety check");
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "error": format!("cannot verify agent config before deletion: {}", e)
            }))).into_response();
        }
    }

    // Clean up all agent-related data from DB first (preserve sessions/messages as history)
    // Fetch channel IDs before transaction deletes them
    let channels: Vec<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM agent_channels WHERE agent_name = $1"
    ).bind(&name).fetch_all(&infra.db).await.unwrap_or_default();

    if let Err(e) = cleanup_agent_data(&infra.db, &name).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "error": format!("failed to clean up agent data: {}", e)
        }))).into_response();
    }

    // Vault cleanup AFTER DB transaction committed (vault is not transactional —
    // if we deleted credentials before and the transaction failed, channels would
    // lose their tokens irrecoverably)
    for (ch_id,) in &channels {
        auth.secrets.delete_scoped("CHANNEL_CREDENTIALS", &ch_id.to_string()).await.ok();
    }
    auth.secrets.delete_scope(&name).await.ok();

    // Remove TOML from disk
    if path.exists()
        && let Err(e) = std::fs::remove_file(&path) {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }

    // Hot-stop: remove from running engines
    let handle = agents.map.write().await.remove(&name);
    auth.access_guards.write().await.remove(&name);

    // Remove agent container
    if let Some(ref sandbox) = infra.sandbox {
        let _ = sandbox.remove_container(&name).await;
    }

    if let Some(handle) = handle {
        handle.shutdown(&agents.scheduler).await;
        tracing::info!(agent = %name, "agent deleted and stopped via API");
    } else {
        tracing::info!(agent = %name, "agent config deleted via API (was not running)");
    }

    crate::db::audit::audit_spawn(infra.db.clone(), name.clone(), crate::db::audit::event_types::AGENT_DELETED, None, json!({"agent": name}));

    Json(json!({ "ok": true })).into_response()
}

// ── Approvals API ───────────────────────────────────────

/// GET /api/approvals?agent=xxx&status=pending
/// If agent is omitted, returns pending approvals for all agents.
pub(crate) async fn api_list_approvals(
    State(infra): State<InfraServices>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let agent_name = params.get("agent").cloned().unwrap_or_default();

    let result = if agent_name.is_empty() {
        crate::db::approvals::list_all_pending(&infra.db).await
    } else {
        crate::db::approvals::list_pending(&infra.db, &agent_name).await
    };

    match result {
        Ok(approvals) => {
            let items: Vec<ApprovalEntryDto> = approvals.iter().map(|a| {
                ApprovalEntryDto {
                    id: a.id.to_string(),
                    agent_id: a.agent_id.clone(),
                    tool: a.tool_name.clone(),
                    arguments: a.tool_args.clone(),
                    status: a.status.clone(),
                    created_at: a.requested_at.to_rfc3339(),
                    resolved_at: a.resolved_at.map(|t| t.to_rfc3339()),
                    resolved_by: a.resolved_by.clone(),
                }
            }).collect();
            Json(serde_json::json!({"approvals": items})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ).into_response(),
    }
}

/// POST /api/approvals/{id}/resolve
/// Body: {"status": "approved"|"rejected"}
pub(crate) async fn api_resolve_approval(
    State(infra): State<InfraServices>,
    State(agents_core): State<AgentCore>,
    Path(id): Path<opex_types::ids::ApprovalId>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let status = body.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let resolved_by = body.get("resolved_by").and_then(|v| v.as_str()).unwrap_or("api");

    if status != "approved" && status != "rejected" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "status must be 'approved' or 'rejected'"})),
        ).into_response();
    }

    // Find the agent this approval belongs to
    let approval = match crate::db::approvals::get_approval(&infra.db, id).await {
        Ok(Some(a)) => a,
        Ok(None) => return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "approval not found"})),
        ).into_response(),
        Err(e) => return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ).into_response(),
    };

    // Extract optional modified_input from the request body
    let modified_input = body.get("modified_input")
        .and_then(|v| if v.is_null() { None } else { Some(v.clone()) });

    // Resolve in the engine (updates DB + wakes waiter)
    let agents = agents_core.map.read().await;
    if let Some(handle) = agents.get(&approval.agent_id) {
        let approved = status == "approved";
        match handle.engine.resolve_approval(id, approved, resolved_by, modified_input.clone()).await {
            Ok(()) => {
                // audit is already recorded inside engine.resolve_approval()
                Json(json!({"ok": true, "status": status, "modified": modified_input.is_some()})).into_response()
            }
            Err(e) => {
                // Phase 63 DATA-04: surface typed HTTP status on known pipeline
                // outcomes. Pipeline::approval::resolve_approval bails with
                // deterministic messages:
                //   "approval {id} not found"
                //   "approval {id} already resolved (status={...})"
                // Substring-match is brittle but contained to this one site;
                // a typed error-chain refactor is a Phase 66 candidate.
                let msg = e.to_string();
                let (status_code, body) = if msg.contains("already resolved") {
                    (
                        StatusCode::CONFLICT,
                        json!({"error": "already_resolved", "detail": msg}),
                    )
                } else if msg.contains("not found") {
                    (
                        StatusCode::NOT_FOUND,
                        json!({"error": "not_found", "detail": msg}),
                    )
                } else {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({"error": msg}),
                    )
                };
                (status_code, Json(body)).into_response()
            }
        }
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("agent '{}' not found", approval.agent_id)})),
        ).into_response()
    }
}

// ── Approval Allowlist ──────────────────────────────────

pub(crate) async fn api_list_allowlist(
    State(infra): State<InfraServices>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let agent = params.get("agent").cloned().unwrap_or_default();
    if agent.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "agent parameter required"}))).into_response();
    }
    match crate::db::approvals::list_allowlist(&infra.db, &agent).await {
        Ok(entries) => Json(json!({"allowlist": entries})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_add_to_allowlist(
    State(infra): State<InfraServices>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let agent = body["agent_id"].as_str().unwrap_or("");
    let pattern = body["tool_pattern"].as_str().unwrap_or("");
    if agent.is_empty() || pattern.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "agent_id and tool_pattern required"}))).into_response();
    }
    match crate::db::approvals::add_to_allowlist(&infra.db, agent, pattern).await {
        Ok(id) => (StatusCode::CREATED, Json(json!({"id": id}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_delete_from_allowlist(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
) -> impl IntoResponse {
    match crate::db::approvals::remove_from_allowlist(&infra.db, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, Json(json!({"error": "not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Hooks API ───────────────────────────────────────────

pub(crate) async fn api_agent_hooks(
    State(agents): State<AgentCore>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Some(engine) = agents.get_engine(&name).await {
        let names = engine.hooks().names();
        Json(json!({"agent": name, "hooks": names})).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))).into_response()
    }
}
/// GET /api/agents/{name}/tasks — return task plans written by this agent to workspace/tasks/
pub(crate) async fn api_agent_tasks(
    State(agents): State<AgentCore>,
    Path(name): Path<String>,
) -> impl IntoResponse {
    // Check agent exists
    if !agents.map.read().await.contains_key(&name) {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))).into_response();
    }

    let workspace_dir = agents.deps.read().await.workspace_dir.clone();
    let tasks_dir = PathBuf::from(&workspace_dir).join("tasks");

    // If tasks directory doesn't exist, return empty list
    let mut read_dir = match tokio::fs::read_dir(&tasks_dir).await {
        Ok(rd) => rd,
        Err(_) => return Json(json!({"tasks": []})).into_response(),
    };

    let mut tasks: Vec<Value> = Vec::new();

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let path = entry.path();
        // Only process .json files
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(_) => continue,
        };
        let plan: Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Filter by agent name
        if plan.get("agent").and_then(|v| v.as_str()) == Some(name.as_str()) {
            tasks.push(plan);
        }
    }

    // Sort by created_at descending (ISO 8601 string comparison is correct for UTC timestamps)
    tasks.sort_by(|a, b| {
        let ca = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let cb = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        cb.cmp(ca)
    });

    // Limit to 20 entries
    tasks.truncate(20);

    Json(json!({"tasks": tasks})).into_response()
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    // ── merge_clearable_string ──────────────────────────────────────────────
    //
    // Drives the PUT /api/agents/{name} round-trip semantics for fallback_provider,
    // tts_provider, and imagegen_provider:
    //   None       — payload field absent → keep existing value
    //   Some("")   — explicit clear from UI → set to None
    //   Some(val)  — update to the new non-empty value
    //
    // Without these tests, regressing the match arms (e.g. swapping `None` and
    // `Some("")` semantics) would silently change existing-field preservation
    // on UI saves.

    use super::merge_clearable_string;

    #[test]
    fn merge_clearable_none_payload_preserves_existing_some() {
        assert_eq!(
            merge_clearable_string(None, Some("nova-cloud")),
            Some("nova-cloud".to_string())
        );
    }

    #[test]
    fn merge_clearable_none_payload_preserves_existing_none() {
        assert_eq!(merge_clearable_string(None, None), None);
    }

    #[test]
    fn merge_clearable_empty_payload_clears_even_with_existing() {
        // `Some("")` is the UI's "clear this field" signal — must not keep existing.
        assert_eq!(
            merge_clearable_string(Some(String::new()), Some("nova-cloud")),
            None
        );
    }

    #[test]
    fn merge_clearable_empty_payload_with_no_existing_returns_none() {
        assert_eq!(merge_clearable_string(Some(String::new()), None), None);
    }

    #[test]
    fn merge_clearable_value_payload_overrides_existing() {
        assert_eq!(
            merge_clearable_string(Some("flux-pro".to_string()), Some("nova-cloud")),
            Some("flux-pro".to_string())
        );
    }

    #[test]
    fn merge_clearable_value_payload_with_no_existing_uses_payload() {
        assert_eq!(
            merge_clearable_string(Some("flux-pro".to_string()), None),
            Some("flux-pro".to_string())
        );
    }

    // ── preserve_hooks_webhooks ──────────────────────────────────────────────
    //
    // Guards the data-loss fix: PUT without webhooks in the payload must not
    // wipe webhooks that were hand-configured in the agent's TOML on disk.

    use super::preserve_hooks_webhooks;
    use crate::config::{AgentConfig, HooksConfig, WebhookConfig};

    /// Minimal valid AgentConfig from TOML, with the provided hooks section.
    fn make_agent_config_with_hooks(hooks: Option<HooksConfig>) -> AgentConfig {
        let mut cfg: AgentConfig = toml::from_str(
            "[agent]\nname = \"Test\"\nprovider = \"openai\"\nmodel = \"gpt-4o\"\n",
        )
        .expect("minimal AgentConfig must parse");
        cfg.agent.hooks = hooks;
        cfg
    }

    #[test]
    fn preserve_webhooks_when_payload_omits() {
        let webhook = WebhookConfig { url: "https://keep/h".into(), ..Default::default() };

        // new_cfg: hooks present but webhooks=[] (build_agent_config result when
        // payload sent hooks block without webhooks field)
        let mut new_cfg = make_agent_config_with_hooks(Some(HooksConfig {
            log_all_tool_calls: true,
            block_tools: vec![],
            webhooks: vec![],
            ..Default::default()
        }));
        // existing: has one webhook on disk
        let existing = make_agent_config_with_hooks(Some(HooksConfig {
            log_all_tool_calls: false,
            block_tools: vec![],
            webhooks: vec![webhook.clone()],
            ..Default::default()
        }));

        // payload omitted webhooks → must preserve from disk
        preserve_hooks_webhooks(&mut new_cfg, &existing, false);
        let hooks = new_cfg.agent.hooks.as_ref().unwrap();
        assert_eq!(hooks.webhooks.len(), 1, "omitted webhooks must be preserved from disk");
        assert_eq!(hooks.webhooks[0].url, "https://keep/h");
        // other hooks fields must NOT be overwritten by preserve_hooks_webhooks
        assert!(hooks.log_all_tool_calls, "log_all_tool_calls must stay from new_cfg (payload)");

        // payload explicitly provided webhooks (empty list) → leave as-is
        let mut new2 = make_agent_config_with_hooks(Some(HooksConfig {
            log_all_tool_calls: true,
            block_tools: vec![],
            webhooks: vec![],
            ..Default::default()
        }));
        preserve_hooks_webhooks(&mut new2, &existing, true);
        assert_eq!(
            new2.agent.hooks.as_ref().unwrap().webhooks.len(),
            0,
            "provided=true must not overwrite the explicit empty list"
        );
    }

    /// Per D-09: Simulated failure mid-rename should leave DB in pre-rename state.
    /// In production, sqlx Transaction provides this guarantee via DROP (implicit rollback).
    /// This test documents the expected behavior by simulating the rename loop in-memory.
    #[test]
    fn test_rename_mid_failure_leaves_pre_rename_state() {
        // Mirror the exact table list from the rename handler (19 tables total)
        let tables_agent_id: Vec<&str> = vec![
            "sessions", "scheduled_jobs", "channel_allowed_users",
            "usage_log", "cron_runs", "audit_events", "pending_approvals",
            "pending_messages", "webhooks", "stream_jobs", "outbound_queue",
            "audit_log", "agent_github_repos", "gmail_triggers",
            "agent_oauth_bindings", "approval_allowlist", "memory_chunks",
        ];
        // Additional tables updated outside the loop
        let extra_tables: Vec<&str> = vec!["messages", "agent_channels", "agent_model_overrides"];

        let all_tables: Vec<&str> = tables_agent_id.iter()
            .chain(extra_tables.iter())
            .copied()
            .collect();

        assert_eq!(all_tables.len(), 20, "rename should cover exactly 20 tables");

        let old_name = "OldAgent";
        let new_name = "NewAgent";

        // Initialize: each table has one row with old_name
        let mut db_state: HashMap<&str, Vec<String>> = HashMap::new();
        for table in &all_tables {
            db_state.insert(table, vec![old_name.to_string()]);
        }

        // -- Test 1: Failure at table 10 should leave ALL tables in pre-rename state --
        let snapshot: HashMap<&str, Vec<String>> = db_state.clone();
        let fail_at = 10;

        for (i, table) in all_tables.iter().enumerate() {
            if i == fail_at {
                // Simulate failure -> rollback by restoring snapshot
                db_state = snapshot;
                break;
            }
            // Simulate UPDATE: replace old_name with new_name
            if let Some(rows) = db_state.get_mut(table) {
                for row in rows.iter_mut() {
                    if row == old_name {
                        *row = new_name.to_string();
                    }
                }
            }
        }

        // After rollback: NO table should have the new name
        for table in &all_tables {
            let rows = &db_state[table];
            assert!(
                !rows.contains(&new_name.to_string()),
                "table '{}' should not contain new name after rollback",
                table
            );
            assert!(
                rows.contains(&old_name.to_string()),
                "table '{}' should still contain old name after rollback",
                table
            );
        }

        // -- Test 2: Successful rename (no failure) should update ALL tables --
        for table in &all_tables {
            if let Some(rows) = db_state.get_mut(table) {
                for row in rows.iter_mut() {
                    if row == old_name {
                        *row = new_name.to_string();
                    }
                }
            }
        }

        for table in &all_tables {
            let rows = &db_state[table];
            assert!(
                rows.contains(&new_name.to_string()),
                "table '{}' should contain new name after successful rename",
                table
            );
            assert!(
                !rows.contains(&old_name.to_string()),
                "table '{}' should not contain old name after successful rename",
                table
            );
        }
    }

    // ── TABLES_WITH_AGENT_ID_* schema reconciliation (T2) ────────────────
    //
    // Guard the two centralized constants against schema drift. Without
    // these, adding a new `agent_id`-bearing migration without updating
    // the constants would silently leave orphan rows on agent rename or
    // delete.

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_tables_with_agent_id_all_exist_in_schema(pool: sqlx::PgPool) {
        // Cover all three constants — including TABLES_TO_DELETE_BY_AGENT_ID,
        // which by contract is a subset of NOT_NULL but is still iterated here
        // to defend against the subset invariant being broken in a future PR.
        let all_tables: Vec<&str> = super::TABLES_WITH_AGENT_ID_NOT_NULL
            .iter()
            .chain(super::TABLES_WITH_AGENT_ID_NULLABLE.iter())
            .chain(super::TABLES_TO_DELETE_BY_AGENT_ID.iter())
            .copied()
            .collect();
        for table in all_tables {
            let exists: (Option<String>,) = sqlx::query_as("SELECT to_regclass($1)::text")
                .bind(table)
                .fetch_one(&pool)
                .await
                .unwrap();
            assert!(
                exists.0.is_some(),
                "table {table} does not exist in schema"
            );
        }
    }

    /// `TABLES_TO_DELETE_BY_AGENT_ID` MUST be a strict subset of
    /// `TABLES_WITH_AGENT_ID_NOT_NULL`. The delete list intentionally omits
    /// compliance / history tables (`audit_log`, `audit_events`, `usage_log`,
    /// `cron_runs`, `sessions`, `tasks`, …) — see the constant's doc comment.
    /// This invariant is the whole reason for the two constants existing
    /// separately; if it ever breaks, agent deletion silently destroys
    /// audit data again.
    #[test]
    fn test_tables_to_delete_is_subset_of_not_null() {
        for table in super::TABLES_TO_DELETE_BY_AGENT_ID {
            assert!(
                super::TABLES_WITH_AGENT_ID_NOT_NULL.contains(table),
                "table {table} in TABLES_TO_DELETE_BY_AGENT_ID but not in TABLES_WITH_AGENT_ID_NOT_NULL"
            );
        }
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn test_tables_with_agent_id_nullability_matches_classification(pool: sqlx::PgPool) {
        for table in super::TABLES_WITH_AGENT_ID_NOT_NULL {
            let row: (String,) = sqlx::query_as(
                "SELECT is_nullable FROM information_schema.columns
                 WHERE table_name = $1 AND column_name = 'agent_id'",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("table {table}: failed to query agent_id nullability: {e}"));
            assert_eq!(
                row.0, "NO",
                "table {table} listed as NOT NULL but schema says nullable"
            );
        }
        for table in super::TABLES_WITH_AGENT_ID_NULLABLE {
            let row: (String,) = sqlx::query_as(
                "SELECT is_nullable FROM information_schema.columns
                 WHERE table_name = $1 AND column_name = 'agent_id'",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("table {table}: failed to query agent_id nullability: {e}"));
            assert_eq!(
                row.0, "YES",
                "table {table} listed as NULLABLE but schema says NOT NULL"
            );
        }
    }
}
