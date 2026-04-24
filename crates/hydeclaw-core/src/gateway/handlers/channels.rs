use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, delete},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, AuthServices, ChannelBus, InfraServices};

include!("channels_dto_structs.rs");

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/channels", get(api_list_all_channels))
        .route("/api/channels/active", get(api_channels_active))
        .route("/api/channels/notify", post(api_channel_notify))
        .route("/api/agents/{name}/hooks", get(super::agents::api_agent_hooks))
        .route("/api/agents/{name}/channels", get(api_channels_list).post(api_channel_create))
        .route("/api/agents/{name}/channels/{id}", delete(api_channel_delete).put(api_channel_update))
        .route("/api/agents/{name}/channels/{id}/restart", post(api_channel_restart))
        .route("/api/agents/{name}/channels/{id}/ack", post(api_channel_ack))
        .route("/api/agents/{name}/channels/{id}/status", get(api_channel_status))
}

/// Config keys that contain sensitive credentials — stored in vault, masked in API responses.
const CREDENTIAL_KEYS: &[&str] = &[
    "bot_token",
    "access_token",
    "password",
    "app_token",
    "verify_token",
];

// ── Channel management ────────────────────────────────────────────────────────

/// Invalidate channel info cache on the agent engine after CRUD operations.
async fn invalidate_agent_channel_cache(agents: &AgentCore, agent_name: &str) {
    if let Some(engine) = agents.get_engine(agent_name).await {
        engine.invalidate_channel_cache().await;
    }
}

#[derive(sqlx::FromRow)]
struct AgentChannelRow {
    pub id: sqlx::types::Uuid,
    pub agent_name: String,
    pub channel_type: String,
    pub display_name: String,
    pub config: serde_json::Value,
    pub status: String,
    pub error_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChannelCreateBody {
    channel_type: String,
    display_name: String,
    #[serde(default)]
    config: serde_json::Value,
}

pub(crate) async fn api_channels_list(
    State(infra): State<InfraServices>,
    Path(agent_name): Path<String>,
) -> impl IntoResponse {
    let rows: Result<Vec<AgentChannelRow>, _> = sqlx::query_as(
        "SELECT id, agent_name, channel_type, display_name, config, status, error_msg
         FROM agent_channels WHERE agent_name = $1 ORDER BY created_at"
    )
    .bind(&agent_name)
    .fetch_all(&infra.db)
    .await;

    match rows {
        Ok(rows) => {
            let items: Vec<ChannelRowDto> = rows.iter().map(to_channel_row_dto).collect();
            Json(json!(items)).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

pub(crate) async fn api_channel_create(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    Path(agent_name): Path<String>,
    Json(body): Json<ChannelCreateBody>,
) -> impl IntoResponse {
    const SUPPORTED: &[&str] = &["telegram", "discord", "matrix", "irc", "slack", "whatsapp"];
    if !SUPPORTED.contains(&body.channel_type.as_str()) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("unknown channel_type: {}. Supported: {}", body.channel_type, SUPPORTED.join(", "))}))).into_response();
    }

    let config = if body.config.is_null() { serde_json::json!({}) } else { body.config };

    // Extract credentials before inserting — they go to vault, not JSONB
    let credentials = extract_credentials(&config);
    let config_redacted = redact_credentials(config);

    let row: Result<AgentChannelRow, _> = sqlx::query_as(
        "INSERT INTO agent_channels (agent_name, channel_type, display_name, config, status)
         VALUES ($1, $2, $3, $4, 'stopped')
         RETURNING id, agent_name, channel_type, display_name, config, status, error_msg"
    )
    .bind(&agent_name)
    .bind(&body.channel_type)
    .bind(&body.display_name)
    .bind(&config_redacted)
    .fetch_one(&infra.db)
    .await;

    let r = match row {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    };

    // Store credentials in vault after we have the channel UUID
    if let Some(creds_json) = credentials {
        let desc = format!("Channel credentials for {}/{}", agent_name, body.channel_type);
        if let Err(e) = auth.secrets.set_scoped(
            "CHANNEL_CREDENTIALS",
            &r.id.to_string(),
            &creds_json,
            Some(desc.as_str()),
        ).await {
            tracing::error!(channel_id = %r.id, error = %e, "Failed to store channel credentials in vault");
            // Rollback: remove the inserted row to keep state consistent
            if let Err(re) = sqlx::query("DELETE FROM agent_channels WHERE id = $1")
                .bind(r.id)
                .execute(&infra.db)
                .await
            {
                tracing::error!(channel_id = %r.id, error = %re, "rollback DELETE failed after vault error");
            }
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "failed to store credentials"}))).into_response();
        }
    }

    tracing::info!(agent = %agent_name, channel_id = %r.id, "channel created");
    invalidate_agent_channel_cache(&agents, &agent_name).await;
    Json(json!({"ok": true, "id": r.id, "status": "stopped"})).into_response()
}

pub(crate) async fn api_channel_delete(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    Path((agent_name, id)): Path<(String, String)>,
) -> impl IntoResponse {
    let Ok(uuid) = id.parse::<sqlx::types::Uuid>() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid id"}))).into_response();
    };

    let row: Option<AgentChannelRow> = sqlx::query_as(
        "SELECT id, agent_name, channel_type, display_name, config, status, error_msg
         FROM agent_channels WHERE id = $1 AND agent_name = $2"
    )
    .bind(uuid)
    .bind(&agent_name)
    .fetch_optional(&infra.db)
    .await
    .unwrap_or(None);

    let Some(_row) = row else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "channel not found"}))).into_response();
    };

    if let Err(e) = sqlx::query("DELETE FROM agent_channels WHERE id = $1")
        .bind(uuid)
        .execute(&infra.db)
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("delete failed: {}", e)}))).into_response();
    }

    // Best-effort: delete vault credentials (non-fatal if already absent)
    if let Err(e) = auth.secrets.delete_scoped("CHANNEL_CREDENTIALS", &uuid.to_string()).await {
        tracing::warn!(channel_id = %uuid, error = %e, "Failed to delete channel credentials from vault (non-fatal)");
    }

    tracing::info!(agent = %agent_name, channel_id = %uuid, "channel deleted");
    invalidate_agent_channel_cache(&agents, &agent_name).await;
    Json(json!({"ok": true})).into_response()
}

pub(crate) async fn api_channel_restart(
    State(infra): State<InfraServices>,
    Path((agent_name, id)): Path<(String, String)>,
) -> impl IntoResponse {
    let Ok(uuid) = id.parse::<sqlx::types::Uuid>() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid id"}))).into_response();
    };

    // Verify channel exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM agent_channels WHERE id = $1 AND agent_name = $2)"
    )
    .bind(uuid)
    .bind(&agent_name)
    .fetch_one(&infra.db)
    .await
    .unwrap_or(false);

    if !exists {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "channel not found"}))).into_response();
    }

    // Mark as pending restart — external container picks up the status change
    if let Err(e) = sqlx::query(
        "UPDATE agent_channels SET status = 'pending_restart', error_msg = NULL WHERE id = $1"
    )
    .bind(uuid)
    .execute(&infra.db)
    .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("restart failed: {}", e)}))).into_response();
    }

    tracing::info!(agent = %agent_name, channel_id = %uuid, "channel marked for restart");
    Json(json!({"ok": true, "status": "pending_restart"})).into_response()
}

/// Acknowledge channel status change from adapter (running/stopped).
pub(crate) async fn api_channel_ack(
    State(infra): State<InfraServices>,
    Path((_agent_name, channel_id)): Path<(String, String)>,
    body: Option<Json<serde_json::Value>>,
) -> impl IntoResponse {
    let uuid = match uuid::Uuid::parse_str(&channel_id) {
        Ok(u) => u,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid id"}))).into_response(),
    };

    let status = body
        .as_ref()
        .and_then(|b| b.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("running");

    let valid_statuses = ["running", "stopped", "error", "pending_restart"];
    if !valid_statuses.contains(&status) {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid channel status"}))).into_response();
    }

    if let Err(e) = sqlx::query(
        "UPDATE agent_channels SET status = $1, error_msg = NULL WHERE id = $2 AND agent_name = $3"
    )
    .bind(status)
    .bind(uuid)
    .bind(&_agent_name)
    .execute(&infra.db)
    .await
    {
        tracing::warn!(channel_id = %uuid, error = %e, "channel ack DB update failed");
    }

    Json(json!({"ok": true})).into_response()
}

#[derive(Deserialize)]
pub(crate) struct ChannelUpdateBody {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

pub(crate) async fn api_channel_update(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    Path((agent_name, id)): Path<(String, String)>,
    Json(body): Json<ChannelUpdateBody>,
) -> impl IntoResponse {
    let Ok(uuid) = id.parse::<sqlx::types::Uuid>() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid id"}))).into_response();
    };

    // Check channel exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM agent_channels WHERE id = $1 AND agent_name = $2)"
    )
    .bind(uuid)
    .bind(&agent_name)
    .fetch_one(&infra.db)
    .await
    .unwrap_or(false);

    if !exists {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "channel not found"}))).into_response();
    }

    // Update fields
    if let Some(ref dn) = body.display_name
        && let Err(e) = sqlx::query("UPDATE agent_channels SET display_name = $1 WHERE id = $2")
            .bind(dn)
            .bind(uuid)
            .execute(&infra.db)
            .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("update display_name failed: {}", e)}))).into_response();
    }
    if let Some(ref new_cfg) = body.config {
        // Store new credentials in vault if provided
        if let Some(creds_json) = extract_credentials(new_cfg) {
            let desc = format!("Channel credentials for agent {agent_name}");
            if let Err(e) = auth.secrets.set_scoped(
                "CHANNEL_CREDENTIALS",
                &uuid.to_string(),
                &creds_json,
                Some(desc.as_str()),
            ).await {
                tracing::error!(channel_id = %uuid, error = %e, "Failed to update channel credentials in vault");
                return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "failed to update credentials"}))).into_response();
            }
        }

        // Merge non-credential config fields into existing JSONB (never store credentials)
        let existing_cfg: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT config FROM agent_channels WHERE id = $1"
        )
        .bind(uuid)
        .fetch_optional(&infra.db)
        .await
        .ok()
        .flatten();

        let new_non_cred: serde_json::Value = redact_credentials(new_cfg.clone());
        let merged = if let Some(serde_json::Value::Object(mut old)) = existing_cfg {
            if let serde_json::Value::Object(new_map) = new_non_cred {
                for (k, v) in new_map {
                    if v.as_str().is_none_or(|s| !s.is_empty()) {
                        old.insert(k, v);
                    }
                }
            }
            serde_json::Value::Object(old)
        } else {
            new_non_cred
        };

        if let Err(e) = sqlx::query("UPDATE agent_channels SET config = $1 WHERE id = $2")
            .bind(&merged)
            .bind(uuid)
            .execute(&infra.db)
            .await
        {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": format!("update config failed: {}", e)}))).into_response();
        }
    }

    tracing::info!(agent = %agent_name, channel_id = %uuid, "channel updated");
    invalidate_agent_channel_cache(&agents, &agent_name).await;
    Json(json!({"ok": true})).into_response()
}

pub(crate) async fn api_channel_status(
    State(infra): State<InfraServices>,
    Path((agent_name, id)): Path<(String, String)>,
) -> impl IntoResponse {
    let Ok(uuid) = id.parse::<sqlx::types::Uuid>() else {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "invalid id"}))).into_response();
    };

    let row: Option<AgentChannelRow> = sqlx::query_as(
        "SELECT id, agent_name, channel_type, display_name, config, status, error_msg
         FROM agent_channels WHERE id = $1 AND agent_name = $2"
    )
    .bind(uuid)
    .bind(&agent_name)
    .fetch_optional(&infra.db)
    .await
    .unwrap_or(None);

    let Some(row) = row else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "channel not found"}))).into_response();
    };

    Json(json!({"id": uuid, "status": row.status, "error_msg": row.error_msg})).into_response()
}

// ── Global channel endpoints ─────────────────────────────────────────────────

/// GET /api/channels — list ALL channels across all agents.
pub(crate) async fn api_list_all_channels(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let reveal = query.get("reveal").is_some_and(|v| v == "true");
    let rows = sqlx::query_as::<_, AgentChannelRow>(
        "SELECT id, agent_name, channel_type, display_name, config, status, error_msg \
         FROM agent_channels ORDER BY agent_name, created_at",
    )
    .fetch_all(&infra.db)
    .await;

    match rows {
        Ok(rows) => {
            if reveal {
                let mut channels = Vec::with_capacity(rows.len());
                for row in &rows {
                    let config = match auth.secrets.get_scoped(
                        "CHANNEL_CREDENTIALS",
                        &row.id.to_string(),
                    ).await {
                        Some(creds_json) => inject_credentials(row.config.clone(), &creds_json),
                        None => row.config.clone(), // no credentials in vault (webhook-only channel)
                    };
                    channels.push(json!({
                        "id": row.id,
                        "agent_name": row.agent_name,
                        "channel_type": row.channel_type,
                        "display_name": row.display_name,
                        "config": config,
                        "status": row.status,
                        "error_msg": row.error_msg,
                    }));
                }
                Json(json!({ "channels": channels })).into_response()
            } else {
                let channels: Vec<ChannelRowDto> = rows.iter().map(to_channel_row_dto).collect();
                Json(json!({ "channels": channels })).into_response()
            }
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

/// GET /api/channels/active — list currently connected channel adapters.
pub(crate) async fn api_channels_active(
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    let channels = bus.connected_channels.read().await;
    let dtos: Vec<ActiveChannelDto> = channels.iter().map(to_active_channel_dto).collect();
    Json(json!({ "channels": dtos }))
}

/// Mask sensitive fields in channel config for API responses.
fn mask_config(cfg: &Value) -> Value {
    match cfg {
        Value::Object(map) => {
            let mut masked = map.clone();
            for key in CREDENTIAL_KEYS {
                if let Some(val) = masked.get(*key)
                    && let Some(s) = val.as_str() {
                        if s.len() > 4 {
                            masked.insert((*key).to_string(), Value::String(format!("****{}", &s[s.floor_char_boundary(s.len().saturating_sub(4))..])));
                        } else {
                            masked.insert((*key).to_string(), Value::String("****".to_string()));
                        }
                    }
            }
            Value::Object(masked)
        }
        other => other.clone(),
    }
}

/// Extract credential fields from a channel config, returning them as a JSON
/// string suitable for vault storage, or None if no credentials are present.
fn extract_credentials(config: &serde_json::Value) -> Option<String> {
    let obj = config.as_object()?;
    let creds: serde_json::Map<String, serde_json::Value> = CREDENTIAL_KEYS
        .iter()
        .filter_map(|k| {
            obj.get(*k)
                .filter(|v| !v.is_null())
                .filter(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                .map(|v| ((*k).to_string(), v.clone()))
        })
        .collect();

    if creds.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&creds).expect("serialization is infallible"))
    }
}

/// Return a copy of config with all credential fields removed.
fn redact_credentials(mut config: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = config.as_object_mut() {
        for key in CREDENTIAL_KEYS {
            obj.remove(*key);
        }
    }
    config
}

/// Merge credential fields from vault JSON blob back into a config value.
/// Silently ignores malformed vault data.
fn inject_credentials(mut config: serde_json::Value, creds_json: &str) -> serde_json::Value {
    if let Ok(serde_json::Value::Object(creds_obj)) =
        serde_json::from_str::<serde_json::Value>(creds_json)
        && let Some(config_obj) = config.as_object_mut() {
            config_obj.extend(creds_obj);
        }
    config
}

fn to_channel_row_dto(r: &AgentChannelRow) -> ChannelRowDto {
    ChannelRowDto {
        id: r.id.to_string(),
        agent_name: r.agent_name.clone(),
        channel_type: r.channel_type.clone(),
        display_name: r.display_name.clone(),
        config: mask_config(&r.config),
        status: r.status.clone(),
        error_msg: r.error_msg.clone(),
    }
}

fn to_active_channel_dto(c: &crate::gateway::state::ConnectedChannel) -> ActiveChannelDto {
    ActiveChannelDto {
        agent_name: c.agent_name.clone(),
        channel_id: c.channel_id.map(|id| id.to_string()),
        channel_type: c.channel_type.clone(),
        display_name: c.display_name.clone(),
        adapter_version: c.adapter_version.clone(),
        connected_at: c.connected_at.to_rfc3339(),
        last_activity: c.last_activity.to_rfc3339(),
    }
}

/// POST /api/channels/notify — send a text message to a specific channel.
/// Body: {"`channel_id"`: "uuid", "text": "message"}
/// Used by watchdog for alerting.
pub(crate) async fn api_channel_notify(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(bus): State<ChannelBus>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let channel_id = match body["channel_id"].as_str() {
        Some(id) => id,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "channel_id required"}))).into_response(),
    };
    let text = match body["text"].as_str() {
        Some(t) if !t.is_empty() => t,
        _ => return (StatusCode::BAD_REQUEST, Json(json!({"error": "text required"}))).into_response(),
    };

    // Look up channel to find agent_name and channel_type
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT agent_name, channel_type FROM agent_channels WHERE id = $1::uuid",
    )
    .bind(channel_id)
    .fetch_optional(&infra.db)
    .await
    .ok()
    .flatten();

    let Some((agent_name, channel_type)) = row else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "channel not found"}))).into_response();
    };

    // Get the agent's engine for channel_router access
    let engine = match agents.get_engine(&agent_name).await {
        Some(e) => e,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": format!("agent '{}' not running", agent_name)}))).into_response(),
    };

    let router = match &engine.state().channel_router {
        Some(r) => r,
        None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "no channel router"}))).into_response(),
    };

    // Get owner_id for context (channel adapter uses it to determine recipient)
    let owner_id = engine.cfg().agent.access.as_ref()
        .and_then(|a| a.owner_id.clone())
        .unwrap_or_default();

    let channel_type_for_notify = channel_type.clone();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    if let Err(e) = router.send(crate::agent::channel_actions::ChannelAction {
        name: "send_message".to_string(),
        params: serde_json::json!({ "text": text }),
        context: serde_json::json!({ "chat_id": owner_id, "owner_id": owner_id }),
        reply: reply_tx,
        target_channel: Some(channel_type),
    }).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response();
    }

    match reply_rx.await {
        Ok(Ok(())) => {
            let ok_response = Json(json!({"ok": true})).into_response();
            {
                let db = infra.db.clone();
                let tx = bus.ui_event_tx.clone();
                let body = text.to_string();
                let agent = agent_name.clone();
                let ctype = channel_type_for_notify;
                tokio::spawn(async move {
                    let data = serde_json::json!({"agent": agent, "channel_type": ctype});
                    crate::gateway::handlers::notifications::notify(
                        &db, &tx, "watchdog_alert", "Watchdog Alert", &body, data,
                    ).await.ok();
                });
            }
            ok_response
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "channel action timed out"}))).into_response(),
    }
}

/// One-time startup migration: move credential fields from `agent_channels.config`
/// into the encrypted secrets vault. Idempotent — rows with no credentials are skipped.
pub async fn migrate_credentials_to_vault(
    db: &sqlx::PgPool,
    secrets: &crate::secrets::SecretsManager,
) {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: sqlx::types::Uuid,
        agent_name: String,
        channel_type: String,
        config: serde_json::Value,
    }

    let rows: Vec<Row> = match sqlx::query_as(
        "SELECT id, agent_name, channel_type, config FROM agent_channels",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "migrate_credentials_to_vault: failed to fetch channels");
            return;
        }
    };

    let mut migrated = 0u32;
    for row in rows {
        let Some(creds_json) = extract_credentials(&row.config) else {
            continue; // already migrated or no credentials
        };

        let desc = format!("Channel credentials for {}/{}", row.agent_name, row.channel_type);
        if let Err(e) = secrets
            .set_scoped("CHANNEL_CREDENTIALS", &row.id.to_string(), &creds_json, Some(desc.as_str()))
            .await
        {
            tracing::error!(channel_id = %row.id, error = %e, "migrate_credentials_to_vault: vault write failed");
            continue;
        }

        let redacted = redact_credentials(row.config);
        if let Err(e) = sqlx::query("UPDATE agent_channels SET config = $1 WHERE id = $2")
            .bind(&redacted)
            .bind(row.id)
            .execute(db)
            .await
        {
            tracing::error!(channel_id = %row.id, error = %e, "migrate_credentials_to_vault: DB redaction failed");
            continue;
        }

        migrated += 1;
        tracing::info!(channel_id = %row.id, "migrate_credentials_to_vault: migrated channel");
    }

    if migrated > 0 {
        tracing::info!(count = migrated, "migrate_credentials_to_vault: complete");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_returns_none_when_no_credentials() {
        let config = json!({"channel_username": "test_bot"});
        assert!(extract_credentials(&config).is_none());
    }

    #[test]
    fn extract_returns_none_for_empty_string_credential() {
        let config = json!({"bot_token": ""});
        assert!(extract_credentials(&config).is_none());
    }

    #[test]
    fn extract_captures_bot_token() {
        let config = json!({"bot_token": "123:ABC", "channel_username": "x"});
        let result = extract_credentials(&config).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["bot_token"], "123:ABC");
        assert!(parsed.get("channel_username").is_none());
    }

    #[test]
    fn redact_removes_all_credential_keys() {
        let config = json!({"bot_token": "secret", "channel_username": "x", "password": "pw"});
        let redacted = redact_credentials(config);
        assert!(redacted.get("bot_token").is_none());
        assert!(redacted.get("password").is_none());
        assert_eq!(redacted["channel_username"], "x");
    }

    #[test]
    fn inject_merges_credentials_into_config() {
        let config = json!({"channel_username": "x"});
        let creds = r#"{"bot_token":"123:ABC"}"#;
        let result = inject_credentials(config, creds);
        assert_eq!(result["bot_token"], "123:ABC");
        assert_eq!(result["channel_username"], "x");
    }

    #[test]
    fn inject_ignores_malformed_vault_data() {
        let config = json!({"channel_username": "x"});
        let result = inject_credentials(config.clone(), "NOT_JSON");
        assert_eq!(result, config);
    }

    #[test]
    fn inject_ignores_valid_non_object_json() {
        // Valid JSON but not an object — must be a no-op, not panic
        let config = json!({"channel_username": "x"});
        let result = inject_credentials(config.clone(), "true");
        assert_eq!(result, config);
        let result2 = inject_credentials(config.clone(), "[1,2,3]");
        assert_eq!(result2, config);
    }

    #[test]
    fn extract_returns_none_for_non_object_json() {
        assert!(extract_credentials(&serde_json::Value::Null).is_none());
        assert!(extract_credentials(&json!([])).is_none());
        assert!(extract_credentials(&json!(42)).is_none());
    }
}
