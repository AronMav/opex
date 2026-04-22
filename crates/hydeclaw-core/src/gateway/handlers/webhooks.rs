use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post, put},
};
use dashmap::DashMap;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::Instant;
use subtle::ConstantTimeEq;

use super::super::AppState;
use crate::gateway::clusters::{AgentCore, InfraServices};

include!("webhooks_dto_structs.rs");

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/webhooks", get(api_list_webhooks).post(api_create_webhook))
        .route("/api/webhooks/{id}", put(api_update_webhook).delete(api_delete_webhook))
        .route("/api/webhooks/{id}/regenerate-secret", post(api_regenerate_webhook_secret))
        .route("/webhook/{name}", post(webhook_handler))
}

// ── Webhook auth throttling ──

const WEBHOOK_AUTH_MAX_FAILURES: u32 = 5;
const WEBHOOK_AUTH_WINDOW_SECS: u64 = 300;
const WEBHOOK_AUTH_LOCKOUT_SECS: u64 = 600;

struct WebhookAuthState {
    failures: u32,
    first_failure: Instant,
    locked_until: Option<Instant>,
}

static WEBHOOK_AUTH_THROTTLE: LazyLock<DashMap<String, WebhookAuthState>> =
    LazyLock::new(DashMap::new);

fn webhook_auth_check(name: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if let Some(entry) = WEBHOOK_AUTH_THROTTLE.get(name)
        && let Some(locked_until) = entry.locked_until
            && Instant::now() < locked_until {
                let remaining = locked_until.saturating_duration_since(Instant::now()).as_secs();
                return Err((
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({"error": format!("webhook locked, retry after {}s", remaining)})),
                ));
            }
    Ok(())
}

fn webhook_auth_failure(name: &str) {
    let now = Instant::now();
    let mut entry = WEBHOOK_AUTH_THROTTLE
        .entry(name.to_string())
        .or_insert(WebhookAuthState {
            failures: 0,
            first_failure: now,
            locked_until: None,
        });

    if now.duration_since(entry.first_failure).as_secs() > WEBHOOK_AUTH_WINDOW_SECS {
        entry.failures = 0;
        entry.first_failure = now;
        entry.locked_until = None;
    }

    entry.failures += 1;
    if entry.failures >= WEBHOOK_AUTH_MAX_FAILURES {
        entry.locked_until =
            Some(now + std::time::Duration::from_secs(WEBHOOK_AUTH_LOCKOUT_SECS));
        tracing::warn!(
            webhook = %name,
            "webhook auth locked after {} failures",
            WEBHOOK_AUTH_MAX_FAILURES
        );
    }

    if WEBHOOK_AUTH_THROTTLE.len() > 100 {
        WEBHOOK_AUTH_THROTTLE.retain(|_, v| {
            v.locked_until.is_some_and(|u| now < u)
                || now.duration_since(v.first_failure).as_secs() < 3600
        });
    }
}

fn webhook_auth_success(name: &str) {
    WEBHOOK_AUTH_THROTTLE.remove(name);
}

// ── Webhook type enum ──

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
#[derive(Default)]
pub(crate) enum WebhookType {
    #[default]
    Generic,
    Github,
}


// ── DB row ──

#[derive(Debug, Clone, sqlx::FromRow)]
struct WebhookRow {
    id: uuid::Uuid,
    name: String,
    agent_id: String,
    secret: Option<String>,
    prompt_prefix: Option<String>,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    last_triggered_at: Option<chrono::DateTime<chrono::Utc>>,
    trigger_count: i32,
    webhook_type: WebhookType,
    event_filter: Option<Vec<String>>,
}

// ── CRUD endpoints ──

pub(crate) async fn api_list_webhooks(State(state): State<InfraServices>) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, WebhookRow>(
        "SELECT id, name, agent_id, secret, prompt_prefix, enabled, created_at, last_triggered_at, trigger_count, webhook_type, event_filter \
         FROM webhooks ORDER BY created_at DESC",
    )
    .fetch_all(&state.db)
    .await;

    match rows {
        Ok(webhooks) => {
            let list: Vec<WebhookEntryDto> = webhooks.iter().map(webhook_to_dto).collect();
            Json(json!({ "webhooks": list })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CreateWebhookRequest {
    name: String,
    agent: String,
    prompt_prefix: Option<String>,
    enabled: Option<bool>,
    webhook_type: Option<WebhookType>,
    event_filter: Option<Vec<String>>,
}

pub(crate) async fn api_create_webhook(
    State(state): State<InfraServices>,
    Json(req): Json<CreateWebhookRequest>,
) -> impl IntoResponse {
    if req.name.is_empty() || req.agent.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "name and agent are required"})),
        )
            .into_response();
    }

    // Generate 32-byte hex secret
    use rand::Rng;
    let secret: String = (0..32)
        .map(|_| format!("{:02x}", rand::rng().random::<u8>()))
        .collect();

    let enabled = req.enabled.unwrap_or(true);
    let prompt_prefix = req.prompt_prefix.unwrap_or_default();
    let webhook_type = req.webhook_type.unwrap_or_default();
    let event_filter = req.event_filter;

    let result = sqlx::query_as::<_, WebhookRow>(
        "INSERT INTO webhooks (name, agent_id, secret, prompt_prefix, enabled, webhook_type, event_filter) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING id, name, agent_id, secret, prompt_prefix, enabled, created_at, last_triggered_at, trigger_count, webhook_type, event_filter",
    )
    .bind(&req.name)
    .bind(&req.agent)
    .bind(&secret)
    .bind(&prompt_prefix)
    .bind(enabled)
    .bind(&webhook_type)
    .bind(&event_filter)
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(wh) => {
            // On create, return full secret (only chance to see it)
            let mut dto_json = serde_json::to_value(webhook_to_dto(&wh)).unwrap_or_default();
            if let Some(obj) = dto_json.as_object_mut() {
                obj.insert("secret".to_string(), serde_json::json!(wh.secret));
            }
            (StatusCode::CREATED, Json(dto_json)).into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("unique") || msg.contains("duplicate") {
                (
                    StatusCode::CONFLICT,
                    Json(json!({"error": format!("webhook '{}' already exists", req.name)})),
                )
                    .into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": msg})),
                )
                    .into_response()
            }
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateWebhookRequest {
    name: Option<String>,
    agent: Option<String>,
    prompt_prefix: Option<String>,
    enabled: Option<bool>,
    webhook_type: Option<WebhookType>,
    event_filter: Option<Vec<String>>,
}

pub(crate) async fn api_update_webhook(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Json(req): Json<UpdateWebhookRequest>,
) -> impl IntoResponse {
    // Fetch existing
    let existing = sqlx::query_as::<_, WebhookRow>(
        "SELECT id, name, agent_id, secret, prompt_prefix, enabled, created_at, last_triggered_at, trigger_count, webhook_type, event_filter \
         FROM webhooks WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await;

    let existing = match existing {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "webhook not found"})),
            )
                .into_response()
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };

    let name = req.name.unwrap_or(existing.name);
    let agent_id = req.agent.unwrap_or(existing.agent_id);
    let prompt_prefix = req.prompt_prefix.or(existing.prompt_prefix);
    let enabled = req.enabled.unwrap_or(existing.enabled);
    let webhook_type = req.webhook_type.unwrap_or(existing.webhook_type);
    let event_filter = req.event_filter.or(existing.event_filter);

    let result = sqlx::query_as::<_, WebhookRow>(
        "UPDATE webhooks SET name = $1, agent_id = $2, prompt_prefix = $3, enabled = $4, webhook_type = $5, event_filter = $6 \
         WHERE id = $7 \
         RETURNING id, name, agent_id, secret, prompt_prefix, enabled, created_at, last_triggered_at, trigger_count, webhook_type, event_filter",
    )
    .bind(&name)
    .bind(&agent_id)
    .bind(&prompt_prefix)
    .bind(enabled)
    .bind(&webhook_type)
    .bind(&event_filter)
    .bind(id)
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(wh) => Json(webhook_to_dto(&wh)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn api_delete_webhook(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    let result = sqlx::query("DELETE FROM webhooks WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => Json(json!({"ok": true})).into_response(),
        Ok(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "webhook not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Webhook trigger handler ──

pub(crate) async fn webhook_handler(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(name): axum::extract::Path<String>,
    req: Request<Body>,
) -> impl IntoResponse {
    use axum::body::to_bytes;

    let is_async = req.uri().query().is_some_and(|q| q.contains("async=true"));

    // Throttle check before DB lookup — minimize load under attack
    if let Err(resp) = webhook_auth_check(&name) {
        return resp.into_response();
    }

    // Find webhook in DB
    let wh = match sqlx::query_as::<_, WebhookRow>(
        "SELECT id, name, agent_id, secret, prompt_prefix, enabled, created_at, last_triggered_at, trigger_count, webhook_type, event_filter \
         FROM webhooks WHERE name = $1 AND enabled = true",
    )
    .bind(&name)
    .fetch_optional(&infra.db)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "webhook not found"}))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response();
        }
    };

    // Extract headers before consuming request body
    let github_event_header = req
        .headers()
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);
    let github_signature_header = req
        .headers()
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);

    // Authenticate based on webhook type — Bearer token (generic) vs HMAC (github)
    match wh.webhook_type {
        WebhookType::Generic => {
            if let Some(ref expected) = wh.secret
                && !expected.is_empty() {
                    let auth = auth_header.as_deref().unwrap_or("");
                    let provided = auth.strip_prefix("Bearer ").unwrap_or(auth);
                    if !bool::from(provided.as_bytes().ct_eq(expected.as_bytes())) {
                        webhook_auth_failure(&name);
                        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid token"}))).into_response();
                    }
                    webhook_auth_success(&name);
                }
        }
        WebhookType::Github => {
            // HMAC-SHA256 verification is deferred until after body is read
        }
    }

    // Read body
    let body_bytes = match to_bytes(req.into_body(), 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(json!({"error": "failed to read body"}))).into_response(),
    };

    // GitHub: HMAC-SHA256 verification + event filtering (requires body bytes)
    if wh.webhook_type == WebhookType::Github {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let secret = match wh.secret.as_ref().filter(|s| !s.is_empty()) {
            Some(s) => s,
            None => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": "GitHub webhook has no HMAC secret configured"}))).into_response(),
        };
        let sig_header = if let Some(s) = &github_signature_header { s.as_str() } else {
            webhook_auth_failure(&name);
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "missing X-Hub-Signature-256 header"}))).into_response();
        };
        let hex_sig = sig_header.strip_prefix("sha256=").unwrap_or(sig_header);
        let expected_bytes = if let Ok(b) = hex::decode(hex_sig) { b } else {
            webhook_auth_failure(&name);
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "invalid signature format"}))).into_response();
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(&body_bytes);
        let computed = mac.finalize().into_bytes();
        if !bool::from(computed.as_slice().ct_eq(&expected_bytes)) {
            webhook_auth_failure(&name);
            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "HMAC signature mismatch"}))).into_response();
        }
        webhook_auth_success(&name);
        // Event filtering
        if let Some(ref event_type) = github_event_header
            && let Some(ref filters) = wh.event_filter
                && !filters.is_empty() && !filters.iter().any(|f| f == event_type) {
                    return Json(json!({"ok": true, "filtered": true})).into_response();
                }
    }

    // Update trigger stats
    let _ = sqlx::query(
        "UPDATE webhooks SET trigger_count = trigger_count + 1, last_triggered_at = now() WHERE id = $1",
    )
    .bind(wh.id)
    .execute(&infra.db)
    .await;

    // Build text payload
    let prefix = wh.prompt_prefix.as_deref().unwrap_or("");
    let payload_text = if wh.webhook_type == WebhookType::Github {
        if let (Some(event_type), Ok(json_val)) = (
            &github_event_header,
            serde_json::from_slice::<serde_json::Value>(&body_bytes),
        ) {
            super::github_events::parse_github_event(event_type, &json_val).summary
        } else {
            String::from_utf8_lossy(&body_bytes).into_owned()
        }
    } else if let Ok(json_val) = serde_json::from_slice::<serde_json::Value>(&body_bytes) {
        serde_json::to_string_pretty(&json_val).unwrap_or_else(|_| String::from_utf8_lossy(&body_bytes).into_owned())
    } else {
        String::from_utf8_lossy(&body_bytes).into_owned()
    };
    let text = if prefix.is_empty() { payload_text } else { format!("{prefix}\n\n{payload_text}") };

    // Get agent engine
    let Some(engine) = agents.get_engine(&wh.agent_id).await else {
        return (StatusCode::NOT_FOUND, Json(json!({"error": "agent not running"}))).into_response();
    };

    let msg = hydeclaw_types::IncomingMessage {
        user_id: format!("webhook:{name}"),
        context: serde_json::json!({"webhook": name}),
        text: Some(text),
        attachments: vec![],
        agent_id: wh.agent_id.clone(),
        channel: "webhook".to_string(),
        timestamp: chrono::Utc::now(),
        formatting_prompt: None,
        tool_policy_override: None,
        leaf_message_id: None,
        user_message_id: None,
    };

    tracing::info!(webhook = %name, agent = %wh.agent_id, is_async, "webhook triggered");

    if is_async {
        tokio::spawn(async move {
            if let Err(e) = engine.handle(&msg).await {
                tracing::error!(webhook = %name, error = %e, "async webhook handler error");
            }
        });
        return Json(json!({"ok": true, "queued": true})).into_response();
    }

    match engine.handle(&msg).await {
        Ok(response) => Json(json!({"ok": true, "response": response})).into_response(),
        Err(e) => {
            tracing::error!(webhook = %name, error = %e, "webhook handler error");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response()
        }
    }
}

// ── Regenerate secret ──

pub(crate) async fn api_regenerate_webhook_secret(
    State(state): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    use rand::Rng;
    let new_secret = hex::encode(rand::rng().random::<[u8; 32]>());
    let result = sqlx::query("UPDATE webhooks SET secret = $1 WHERE id = $2")
        .bind(&new_secret)
        .bind(id)
        .execute(&state.db)
        .await;
    match result {
        Ok(r) if r.rows_affected() > 0 => {
            tracing::info!(webhook_id = %id, "webhook secret regenerated");
            Json(json!({"ok": true, "secret": new_secret})).into_response()
        }
        Ok(_) => (StatusCode::NOT_FOUND, Json(json!({"error": "webhook not found"}))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))).into_response(),
    }
}

// ── Helpers ──

fn webhook_to_dto(wh: &WebhookRow) -> WebhookEntryDto {
    let masked_secret = wh.secret.as_ref().map(|s| {
        if s.len() > 4 {
            format!("{}...{}", "*".repeat(s.len() - 4), &s[s.len() - 4..])
        } else {
            "*".repeat(s.len())
        }
    });
    WebhookEntryDto {
        id: wh.id.to_string(),
        name: wh.name.clone(),
        agent_id: wh.agent_id.clone(),
        secret: masked_secret,
        prompt_prefix: wh.prompt_prefix.clone(),
        enabled: wh.enabled,
        created_at: wh.created_at.to_rfc3339(),
        last_triggered_at: wh.last_triggered_at.map(|t| t.to_rfc3339()),
        trigger_count: wh.trigger_count,
        webhook_type: serde_json::to_value(&wh.webhook_type)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| "generic".to_string()),
        event_filter: wh.event_filter.clone(),
    }
}
