use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post, delete, patch},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::gateway::clusters::InfraServices;
use crate::gateway::AppState;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/notifications", get(api_list_notifications))
        .route("/api/notifications/read-all", post(api_mark_all_notifications_read))
        .route("/api/notifications/clear", delete(api_clear_all_notifications))
        .route("/api/notifications/{id}", patch(api_mark_notification_read))
}

// ── Query params ────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub(crate) struct ListQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 { 50 }

// ── REST handlers ───────────────────────────────────────────────

/// GET /api/notifications?limit=50&offset=0
pub(crate) async fn api_list_notifications(
    State(infra): State<InfraServices>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let limit = q.limit.clamp(1, 200);
    let offset = q.offset.max(0);
    match crate::db::notifications::list_notifications(&infra.db, limit, offset).await {
        Ok((items, unread_count)) => Json(crate::db::notifications::NotificationsResponseDto {
            items,
            unread_count,
            limit,
            offset,
        }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response(),
    }
}

/// PATCH /api/notifications/{id}  — mark single notification read
pub(crate) async fn api_mark_notification_read(
    State(infra): State<InfraServices>,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    match crate::db::notifications::mark_read(&infra.db, id).await {
        Ok(updated) => Json(serde_json::json!({"ok": true, "updated": updated})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response(),
    }
}

/// POST /api/notifications/read-all  — mark all notifications read
pub(crate) async fn api_mark_all_notifications_read(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    match crate::db::notifications::mark_all_read(&infra.db).await {
        Ok(count) => Json(serde_json::json!({"ok": true, "updated": count})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response(),
    }
}

pub(crate) async fn api_clear_all_notifications(
    State(infra): State<InfraServices>,
) -> impl IntoResponse {
    match sqlx::query("DELETE FROM notifications")
        .execute(&infra.db)
        .await
    {
        Ok(r) => Json(serde_json::json!({"ok": true, "deleted": r.rows_affected()})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ).into_response(),
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
