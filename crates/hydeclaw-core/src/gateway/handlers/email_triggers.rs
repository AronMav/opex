use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post, delete},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, AuthServices, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/triggers/email/push", post(gmail_push_handler))
        .route("/api/triggers/email", get(api_list_gmail_triggers).post(api_create_gmail_trigger))
        .route("/api/triggers/email/{id}", delete(api_delete_gmail_trigger))
}

// ── Gmail API helpers ──

pub struct EmailSummary {
    pub from: String,
    pub subject: String,
    pub snippet: String,
}

/// Subscribe Gmail inbox to Pub/Sub topic. Returns (historyId, `expiration_unix_ms`).
pub async fn gmail_watch(
    client: &reqwest::Client,
    token: &str,
    pubsub_topic: &str,
) -> anyhow::Result<(String, i64)> {
    let resp = client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/watch")
        .bearer_auth(token)
        .json(&json!({
            "labelIds": ["INBOX"],
            "topicName": pubsub_topic,
        }))
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let history_id = resp["historyId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("no historyId in watch response"))?
        .to_string();
    let expiration = resp["expiration"]
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    Ok((history_id, expiration))
}

/// Stop Gmail watch.
pub async fn gmail_stop(client: &reqwest::Client, token: &str) -> anyhow::Result<()> {
    client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/stop")
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

/// Fetch new messages since historyId.
pub async fn gmail_history(
    client: &reqwest::Client,
    token: &str,
    since_history_id: &str,
) -> anyhow::Result<Vec<EmailSummary>> {
    let hist = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
        .bearer_auth(token)
        .query(&[
            ("startHistoryId", since_history_id),
            ("historyTypes", "messageAdded"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let empty_vec = vec![];
    let message_ids: Vec<String> = hist["history"]
        .as_array()
        .unwrap_or(&empty_vec)
        .iter()
        .flat_map(|h| {
            let empty = vec![];
            h["messagesAdded"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .filter_map(|m| m["message"]["id"].as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect();

    let mut results = Vec::new();
    for id in message_ids.iter().take(10) {
        let msg = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}"
            ))
            .bearer_auth(token)
            .query(&[("format", "metadata"), ("metadataHeaders", "From,Subject")])
            .send()
            .await?
            .error_for_status()?
            .json::<serde_json::Value>()
            .await?;

        let headers = msg["payload"]["headers"].as_array();
        let get_hdr = |name: &str| -> String {
            headers
                .and_then(|hs| {
                    hs.iter()
                        .find(|h| h["name"].as_str() == Some(name))
                        .and_then(|h| h["value"].as_str())
                })
                .unwrap_or("")
                .to_string()
        };

        results.push(EmailSummary {
            from: get_hdr("From"),
            subject: get_hdr("Subject"),
            snippet: msg["snippet"].as_str().unwrap_or("").to_string(),
        });
    }
    Ok(results)
}

// ── Pub/Sub push handler ──

#[derive(Deserialize)]
pub(crate) struct PubsubPush {
    message: PubsubMsg,
}

#[derive(Deserialize)]
pub(crate) struct PubsubMsg {
    data: String,
}

#[derive(Debug, sqlx::FromRow)]
struct TriggerRow {
    agent_id: String,
    history_id: Option<String>,
}

pub(crate) async fn gmail_push_handler(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    State(agents): State<AgentCore>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<PubsubPush>,
) -> impl IntoResponse {
    use base64::Engine;

    // Verify push authentication token — the Pub/Sub subscription URL must include
    // ?token=HYDECLAW_AUTH_TOKEN so only our own Google Cloud project can trigger this handler.
    let expected_token = std::env::var("HYDECLAW_AUTH_TOKEN").unwrap_or_default();
    let provided_token = params.get("token").map_or("", std::string::String::as_str);
    use subtle::ConstantTimeEq;
    if expected_token.is_empty()
        || !bool::from(provided_token.as_bytes().ct_eq(expected_token.as_bytes()))
    {
        tracing::warn!("gmail push: rejected request (missing or invalid token)");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let raw = match base64::engine::general_purpose::STANDARD.decode(&body.message.data) {
        Ok(r) => r,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let notification: serde_json::Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    let email = notification["emailAddress"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let history_id = notification["historyId"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if email.is_empty() || history_id.is_empty() {
        return StatusCode::NO_CONTENT.into_response();
    }

    let trigger = match sqlx::query_as::<_, TriggerRow>(
        "SELECT agent_id, history_id FROM gmail_triggers \
         WHERE email_address = $1 AND enabled = true LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&infra.db)
    .await
    {
        Ok(Some(t)) => t,
        _ => return StatusCode::NO_CONTENT.into_response(),
    };

    let token = match auth.oauth.get_token("google", &trigger.agent_id).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                agent = %trigger.agent_id,
                error = %e,
                "gmail push: cannot get OAuth token"
            );
            return StatusCode::NO_CONTENT.into_response();
        }
    };

    let since = if let Some(h) = trigger.history_id.as_deref() { h.to_string() } else {
        // No baseline cursor yet — update cursor and skip processing this push
        if let Err(e) = sqlx::query(
            "UPDATE gmail_triggers SET history_id = $1 WHERE email_address = $2",
        )
        .bind(&history_id)
        .bind(&email)
        .execute(&infra.db)
        .await
        {
            tracing::warn!(error = %e, email = %email, "gmail: failed to initialize history_id cursor");
        }
        tracing::debug!(email = %email, "gmail: initializing cursor, skipping first push");
        return axum::http::StatusCode::NO_CONTENT.into_response();
    };

    let messages = match gmail_history(&auth.oauth.client, &token, &since).await {
        Ok(msgs) => msgs,
        Err(e) => {
            tracing::warn!(error = %e, email = %email, "gmail: history fetch failed, not advancing cursor");
            return axum::http::StatusCode::NO_CONTENT.into_response();
        }
    };

    // Update history_id cursor only after successful history fetch
    if let Err(e) = sqlx::query(
        "UPDATE gmail_triggers SET history_id = $1 WHERE email_address = $2",
    )
    .bind(&history_id)
    .bind(&email)
    .execute(&infra.db)
    .await
    {
        tracing::warn!(error = %e, "gmail: failed to update history_id cursor");
    }

    // Dispatch to agent
    let agent_map = agents.map.read().await;
    for msg_summary in messages {
        let Some(handle) = agent_map.get(&trigger.agent_id) else {
            continue;
        };
        let engine = handle.engine.clone();
        let agent_id = trigger.agent_id.clone();
        let email_clone = email.clone();
        let prompt = format!(
            "New email:\nFrom: {}\nSubject: {}\n\n{}",
            msg_summary.from, msg_summary.subject, msg_summary.snippet
        );
        tokio::spawn(async move {
            let incoming = hydeclaw_types::IncomingMessage {
                user_id: format!("gmail:{email_clone}"),
                context: json!({"source": "gmail", "email": email_clone}),
                text: Some(prompt),
                attachments: vec![],
                agent_id: agent_id.clone(),
                channel: "gmail".to_string(),
                timestamp: chrono::Utc::now(),
                formatting_prompt: None,
                tool_policy_override: None,
                leaf_message_id: None,
                user_message_id: None,
            };
            if let Err(e) = engine.handle(&incoming).await {
                tracing::error!(agent = %agent_id, error = %e, "gmail push handler error");
            }
        });
    }
    StatusCode::NO_CONTENT.into_response()
}

// ── CRUD handlers ──

#[derive(Deserialize)]
pub(crate) struct CreateGmailTriggerReq {
    pub agent_id: String,
    pub email_address: String,
    pub pubsub_topic: String,
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
struct GmailTriggerRow {
    id: uuid::Uuid,
    agent_id: String,
    email_address: String,
    history_id: Option<String>,
    watch_expiry: Option<chrono::DateTime<chrono::Utc>>,
    pubsub_topic: String,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) async fn api_create_gmail_trigger(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    Json(req): Json<CreateGmailTriggerReq>,
) -> impl IntoResponse {
    let token = match auth.oauth.get_token("google", &req.agent_id).await {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Google not connected: {e}"),
            )
                .into_response()
        }
    };

    let (history_id, expiry_ms) =
        match gmail_watch(&auth.oauth.client, &token, &req.pubsub_topic).await {
            Ok(r) => r,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        };

    let watch_expiry: Option<chrono::DateTime<chrono::Utc>> =
        chrono::DateTime::from_timestamp_millis(expiry_ms)
            .map(|dt| dt.with_timezone(&chrono::Utc));

    let result = sqlx::query_scalar::<_, uuid::Uuid>(
        "INSERT INTO gmail_triggers (agent_id, email_address, history_id, watch_expiry, pubsub_topic) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (agent_id, email_address) DO UPDATE \
         SET history_id = $3, watch_expiry = $4, pubsub_topic = $5, enabled = true \
         RETURNING id",
    )
    .bind(&req.agent_id)
    .bind(&req.email_address)
    .bind(&history_id)
    .bind(watch_expiry)
    .bind(&req.pubsub_topic)
    .fetch_one(&infra.db)
    .await;

    match result {
        Ok(id) => Json(json!({"ok": true, "id": id})).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

pub(crate) async fn api_list_gmail_triggers(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, GmailTriggerRow>(
        "SELECT id, agent_id, email_address, history_id, watch_expiry, pubsub_topic, enabled, created_at \
         FROM gmail_triggers ORDER BY created_at DESC",
    )
    .fetch_all(&infra.db)
    .await;

    match rows {
        Ok(rows) => {
            let list: Vec<_> = rows
                .iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "agent_id": r.agent_id,
                        "email_address": r.email_address,
                        "watch_expiry": r.watch_expiry.map(|t| t.to_rfc3339()),
                        "pubsub_topic": r.pubsub_topic,
                        "enabled": r.enabled,
                        "created_at": r.created_at.to_rfc3339(),
                    })
                })
                .collect();
            Json(json!({"triggers": list})).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, sqlx::FromRow)]
struct AgentIdRow {
    agent_id: String,
}

pub(crate) async fn api_delete_gmail_trigger(
    State(infra): State<InfraServices>,
    State(auth): State<AuthServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    // Fetch trigger info first for OAuth token
    let row = match sqlx::query_as::<_, AgentIdRow>(
        "SELECT agent_id FROM gmail_triggers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&infra.db)
    .await
    {
        Ok(Some(r)) => r,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, "trigger not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    };

    // Try to stop Gmail watch (best effort)
    if let Ok(token) = auth.oauth.get_token("google", &row.agent_id).await {
        let _ = gmail_stop(&auth.oauth.client, &token).await;
    }

    match sqlx::query("DELETE FROM gmail_triggers WHERE id = $1")
        .bind(id)
        .execute(&infra.db)
        .await
    {
        Ok(r) if r.rows_affected() > 0 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

// ── Background renewal ──

#[derive(Debug, sqlx::FromRow)]
struct ExpiringRow {
    id: uuid::Uuid,
    agent_id: String,
    email_address: String,
    pubsub_topic: String,
}

/// Renew Gmail watches expiring within 24 hours. Called from background task.
pub async fn renew_expiring_gmail_watches(
    db: &sqlx::PgPool,
    oauth: &crate::oauth::OAuthManager,
) {
    let expiring = match sqlx::query_as::<_, ExpiringRow>(
        "SELECT id, agent_id, email_address, pubsub_topic FROM gmail_triggers \
         WHERE enabled = true AND (watch_expiry IS NULL OR watch_expiry < now() + INTERVAL '24 hours')",
    )
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to query expiring gmail watches");
            return;
        }
    };

    for t in expiring {
        match oauth.get_token("google", &t.agent_id).await {
            Ok(token) => match gmail_watch(&oauth.client, &token, &t.pubsub_topic).await {
                Ok((hid, exp_ms)) => {
                    let new_expiry: Option<chrono::DateTime<chrono::Utc>> =
                        chrono::DateTime::from_timestamp_millis(exp_ms)
                            .map(|dt| dt.with_timezone(&chrono::Utc));
                    if let Err(e) = sqlx::query(
                        "UPDATE gmail_triggers SET history_id = $1, watch_expiry = $2 WHERE id = $3",
                    )
                    .bind(&hid)
                    .bind(new_expiry)
                    .bind(t.id)
                    .execute(db)
                    .await
                    {
                        tracing::warn!(error = %e, email = %t.email_address, "gmail: failed to update history_id cursor");
                    }
                    tracing::info!(email = %t.email_address, "Gmail watch renewed");
                }
                Err(e) => tracing::error!(
                    email = %t.email_address,
                    error = %e,
                    "Failed to renew Gmail watch"
                ),
            },
            Err(e) => tracing::warn!(
                agent = %t.agent_id,
                error = %e,
                "Cannot get Google OAuth token for Gmail watch renewal"
            ),
        }
    }
}
