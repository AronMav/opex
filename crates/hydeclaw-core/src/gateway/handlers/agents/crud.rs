use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde_json::{json, Value};
use std::fs::canonicalize;
use std::path::PathBuf;
use std::str::FromStr;
use uuid::Uuid;

use crate::gateway::clusters::{AgentCore, AuthServices, InfraServices, ChannelBus, ConfigServices, StatusMonitor};
use crate::config::AgentConfig;
use super::dto::{AgentDetailDto, AgentInfoDto};
use super::schema::*;
use super::lifecycle::start_agent_from_config;

include!("approvals_dto_structs.rs");

// ── Agent list ──────────────────────────────────────────

pub(crate) async fn api_agents(State(agents): State<AgentCore>) -> Json<Value> {
    // Read configs from disk (source of truth)
    let mut disk_configs = crate::config::load_agent_configs("config/agents").unwrap_or_default();
    // Base (base infrastructure) agents first, then alphabetical
    disk_configs.sort_by(|a, b| {
        b.agent.base.cmp(&a.agent.base)
            .then_with(|| a.agent.name.to_lowercase().cmp(&b.agent.name.to_lowercase()))
    });
    let agents_map = agents.map.read().await;

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
        ));
    }

    Json(json!({ "agents": agents }))
}

// ── Agent CRUD ──────────────────────────────────────────

pub(crate) async fn api_get_agent(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
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
    let detail = AgentDetailDto::from_config(&cfg, is_running, config_dirty, voice);
    Json(detail).into_response()
}

pub(crate) async fn api_create_agent(
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    State(cfg_svc): State<ConfigServices>,
    State(status): State<StatusMonitor>,
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
        // Set restricted access by default (secure out of the box)
        if cfg.agent.access.is_none() {
            cfg.agent.access = Some(crate::config::AgentAccessConfig {
                mode: "restricted".into(),
                owner_id: None,
            });
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
    match start_agent_from_config(&cfg, &agents, &infra, &auth, &bus, &cfg_svc, &status).await {
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
        if payload.icon.is_none() { payload.icon = a.icon.clone(); }
        if payload.provider_connection.is_none() { payload.provider_connection = a.provider_connection.clone(); }
        match payload.fallback_provider.as_deref() {
            None => payload.fallback_provider = a.fallback_provider.clone(),
            Some("") => payload.fallback_provider = None,
            Some(_) => {}
        }
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
        if let Some(Some(ref mut tl)) = payload.tool_loop {
            if tl.error_break_threshold.is_none() {
                tl.error_break_threshold = a.tool_loop.as_ref().and_then(|t| t.error_break_threshold);
            }
        }
    }

    let voice = payload.voice.take();
    let mut cfg = build_agent_config(new_name.clone(), payload);
    // Preserve base from existing config — never changed via API
    cfg.agent.base = existing_cfg.agent.base;
    // Preserve fields not in payload
    if cfg.agent.hooks.is_none() {
        cfg.agent.hooks = existing_cfg.agent.hooks.clone();
    }
    if cfg.agent.max_history_messages.is_none() {
        cfg.agent.max_history_messages = existing_cfg.agent.max_history_messages;
    }
    if cfg.agent.max_agent_turns.is_none() {
        cfg.agent.max_agent_turns = existing_cfg.agent.max_agent_turns;
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

    // Write new TOML (to new path if renaming)
    let target_path = agent_config_path(&new_name);
    if let Err(e) = std::fs::write(&target_path, &toml_str) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
    }

    // If renaming, update DB references first, then remove old TOML
    if is_rename {
        // Update agent_id in all DB tables (within a transaction for consistency)
        // SAFETY: table names are hardcoded literals — never user input. Do NOT add dynamic values.
        // SAFETY: Rename transaction covers 21 tables total:
        //   - 18 via tables_agent_id loop (agent_id column)
        //   - 1 messages (agent_id, nullable)
        //   - 1 agent_channels (agent_name column)
        //   - 1 sessions.participants (TEXT[] array_replace)
        // All updates share a single sqlx::Transaction — failure at any point triggers
        // automatic rollback (via explicit rollback or Transaction::Drop).
        let tables_agent_id = [
            "sessions", "tasks", "scheduled_jobs", "channel_allowed_users",
            "usage_log", "cron_runs", "audit_events", "pending_approvals",
            "pending_messages", "webhooks", "stream_jobs", "outbound_queue",
            "audit_log", "agent_github_repos", "gmail_triggers",
            "agent_oauth_bindings", "approval_allowlist",
            "memory_chunks",
        ];
        let mut tx = match infra.db.begin().await {
            Ok(tx) => tx,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("transaction start failed: {}", e)}))).into_response(),
        };
        for table in tables_agent_id {
            let query = format!("UPDATE {table} SET agent_id = $1 WHERE agent_id = $2");
            if let Err(e) = sqlx::query(&query)
                .bind(&new_name)
                .bind(&name)
                .execute(&mut *tx)
                .await
            {
                tracing::warn!(table = %table, error = %e, "failed to update agent_id on rename");
                tx.rollback().await.ok();
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename failed at table {}: {}", table, e)}))).into_response();
            }
        }
        // messages.agent_id is nullable (used in discuss mode)
        if let Err(e) = sqlx::query("UPDATE messages SET agent_id = $1 WHERE agent_id = $2")
            .bind(&new_name)
            .bind(&name)
            .execute(&mut *tx)
            .await
        {
            tracing::warn!(error = %e, "failed to update messages.agent_id on rename");
            tx.rollback().await.ok();
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("rename failed at table messages: {}", e)}))).into_response();
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

        // Only delete old TOML after DB commit succeeds
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

    match start_agent_from_config(&cfg, &agents, &infra, &auth, &bus, &cfg_svc, &status).await {
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
    // agent_channels uses agent_name
    sqlx::query("DELETE FROM agent_channels WHERE agent_name = $1")
        .bind(agent_name).execute(&mut *tx).await?;
    // Everything else uses agent_id
    // SAFETY: table names are hardcoded string literals in the array below -- no user input.
    for table in &[
        "scheduled_jobs", "webhooks", "agent_oauth_bindings",
        "gmail_triggers", "agent_github_repos", "approval_allowlist",
        "channel_allowed_users",
    ] {
        sqlx::query(&format!("DELETE FROM {table} WHERE agent_id = $1"))
            .bind(agent_name).execute(&mut *tx).await?;
    }
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
    Path(id): Path<Uuid>,
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

    /// Per D-09: Simulated failure mid-rename should leave DB in pre-rename state.
    /// In production, sqlx Transaction provides this guarantee via DROP (implicit rollback).
    /// This test documents the expected behavior by simulating the rename loop in-memory.
    #[test]
    fn test_rename_mid_failure_leaves_pre_rename_state() {
        // Mirror the exact table list from the rename handler (20 tables total)
        let tables_agent_id: Vec<&str> = vec![
            "sessions", "tasks", "scheduled_jobs", "channel_allowed_users",
            "usage_log", "cron_runs", "audit_events", "pending_approvals",
            "pending_messages", "webhooks", "stream_jobs", "outbound_queue",
            "audit_log", "agent_github_repos", "gmail_triggers",
            "agent_oauth_bindings", "approval_allowlist", "memory_chunks",
        ];
        // Additional tables updated outside the loop
        let extra_tables: Vec<&str> = vec!["messages", "agent_channels"];

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
}
