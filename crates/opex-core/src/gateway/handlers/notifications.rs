use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, delete, patch},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::gateway::clusters::{ChannelBus, InfraServices};
use crate::gateway::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/notifications",
            get(api_list_notifications).post(api_create_notification),
        )
        .route("/api/notifications/read-all", post(api_mark_all_notifications_read))
        .route("/api/notifications/clear", delete(api_clear_all_notifications))
        .route("/api/notifications/{id}", patch(api_mark_notification_read))
        .route(
            "/api/notification-prefs",
            get(api_get_notification_prefs).put(api_put_notification_prefs),
        )
}

/// Body for `POST /api/notifications`.
#[derive(Debug, Deserialize)]
pub(crate) struct CreateNotificationBody {
    #[serde(default = "default_notification_type")]
    pub r#type: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub data: serde_json::Value,
}

fn default_notification_type() -> String { "watchdog_alert".to_string() }

// ── Cross-tab read-sync broadcast events ─────────────────────────────────────
// Emitted over `ui_event_tx` so every open tab reconciles read-state to the
// server-authoritative unread count (fixes blind local decrement drift).

pub(crate) fn notification_read_event(id: Uuid, unread_count: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "notification_read",
        "data": { "id": id.to_string(), "unread_count": unread_count }
    })
}

fn notifications_read_all_event(unread_count: i64) -> serde_json::Value {
    serde_json::json!({
        "type": "notifications_read_all",
        "data": { "unread_count": unread_count }
    })
}

fn notifications_cleared_event() -> serde_json::Value {
    serde_json::json!({ "type": "notifications_cleared" })
}

/// POST /api/notifications — create a notification (bell + WS broadcast).
/// Auth-gated like every other /api route. Used by internal ops (e.g. the
/// hourly YouTube-cookies health check) to surface alerts to the operator.
pub(crate) async fn api_create_notification(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    Json(req): Json<CreateNotificationBody>,
) -> impl IntoResponse {
    match notify(
        &infra.db,
        &bus.ui_event_tx,
        &req.r#type,
        &req.title,
        &req.body,
        req.data,
    )
    .await
    {
        Ok(()) => (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Query params ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// History cursor: RFC3339 `created_at` of the oldest row already loaded.
    #[serde(default)]
    pub before: Option<String>,
    /// History cursor tiebreak: `id` of that same oldest row.
    #[serde(default)]
    pub before_id: Option<Uuid>,
}

fn default_limit() -> i64 { 50 }

// ── REST handlers ───────────────────────────────────────────────

/// GET /api/notifications?limit=50&offset=0
/// GET /api/notifications?limit=20&before=<rfc3339>&before_id=<uuid>  (history page)
pub(crate) async fn api_list_notifications(
    State(infra): State<InfraServices>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);

    let result = match (q.before.as_deref(), q.before_id) {
        (Some(before_str), Some(before_id)) => {
            match chrono::DateTime::parse_from_rfc3339(before_str) {
                Ok(dt) => {
                    crate::db::notifications::list_notifications_before(
                        &infra.db,
                        dt.with_timezone(&chrono::Utc),
                        before_id,
                        limit,
                    )
                    .await
                }
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({"error": format!("invalid `before` cursor: {e}")})),
                    )
                        .into_response();
                }
            }
        }
        _ => crate::db::notifications::list_notifications(&infra.db, limit, offset).await,
    };

    match result {
        Ok((items, unread_count)) => Json(crate::db::notifications::NotificationsResponseDto {
            items,
            unread_count,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// PATCH /api/notifications/{id}  — mark single notification read
pub(crate) async fn api_mark_notification_read(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match crate::db::notifications::mark_read(&infra.db, id).await {
        Ok(updated) => {
            if updated {
                match crate::db::notifications::count_unread(&infra.db).await {
                    Ok(unread) => {
                        bus.ui_event_tx
                            .send(notification_read_event(id, unread).to_string())
                            .ok();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "count_unread failed after mark_read; skipping read-sync broadcast");
                    }
                }
            }
            Json(serde_json::json!({"ok": true, "updated": updated})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// POST /api/notifications/read-all  — mark all notifications read
pub(crate) async fn api_mark_all_notifications_read(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    match crate::db::notifications::mark_all_read(&infra.db).await {
        Ok(count) => {
            // After mark-all, unread count is authoritatively 0.
            bus.ui_event_tx
                .send(notifications_read_all_event(0).to_string())
                .ok();
            Json(serde_json::json!({"ok": true, "updated": count})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn api_clear_all_notifications(
    State(infra): State<InfraServices>,
    State(bus): State<ChannelBus>,
) -> impl IntoResponse {
    match sqlx::query("DELETE FROM notifications")
        .execute(&infra.db)
        .await
    {
        Ok(r) => {
            bus.ui_event_tx
                .send(notifications_cleared_event().to_string())
                .ok();
            Json(serde_json::json!({"ok": true, "deleted": r.rows_affected()})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// GET /api/notification-prefs — all configured per-type prefs (absent = defaults).
pub(crate) async fn api_get_notification_prefs(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    match crate::db::notification_prefs::list_prefs(&infra.db).await {
        Ok(prefs) => Json(serde_json::json!({ "prefs": prefs })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct UpdatePrefBody {
    #[serde(rename = "type")]
    r#type: String,
    muted: bool,
    sound: bool,
}

/// PUT /api/notification-prefs — upsert one type's prefs.
pub(crate) async fn api_put_notification_prefs(
    State(infra): State<InfraServices>,
    Json(body): Json<UpdatePrefBody>,
) -> impl IntoResponse {
    if body.r#type.is_empty() || body.r#type.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid notification type"})),
        )
            .into_response();
    }
    match crate::db::notification_prefs::upsert_pref(&infra.db, &body.r#type, body.muted, body.sound)
        .await
    {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Public notify() helper ──────────────────────────────────────

/// Create a notification, persist it to the DB, and broadcast it to all connected WS clients.
///
/// Called by Phase 6 trigger sites (access.rs, approvals handler, engine.rs, watchdog).
/// `notification_type` examples: "`access_request`", "`tool_approval`", "`agent_error`", "`watchdog_alert`"
pub async fn notify(
    db: &sqlx::PgPool,
    ui_event_tx: &tokio::sync::broadcast::Sender<String>,
    notification_type: &str,
    title: &str,
    body: &str,
    data: serde_json::Value,
) -> anyhow::Result<()> {
    let notification = crate::db::notifications::create_notification(
        db,
        notification_type,
        title,
        body,
        data,
    ).await?;

    // Broadcast to all connected WebSocket clients (fire-and-forget; drop errors)
    ui_event_tx.send(
        serde_json::json!({"type": "notification", "data": notification}).to_string()
    ).ok();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_event_shape() {
        let id = Uuid::nil();
        let ev = notification_read_event(id, 3);
        assert_eq!(ev["type"], "notification_read");
        assert_eq!(ev["data"]["id"], id.to_string());
        assert_eq!(ev["data"]["unread_count"], 3);
    }

    #[test]
    fn read_all_event_shape() {
        let ev = notifications_read_all_event(0);
        assert_eq!(ev["type"], "notifications_read_all");
        assert_eq!(ev["data"]["unread_count"], 0);
    }

    #[test]
    fn cleared_event_shape() {
        let ev = notifications_cleared_event();
        assert_eq!(ev["type"], "notifications_cleared");
    }
}
