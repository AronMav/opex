use axum::{Router, extract::State, response::IntoResponse, routing::post, Json};
use serde_json::json;
use super::super::AppState;
use crate::gateway::clusters::AuthServices;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/ws-ticket", post(api_create_ws_ticket))
}

const TICKET_TTL_SECS: u64 = 30;

/// POST /api/auth/ws-ticket — issue a one-time WebSocket ticket.
/// Requires Bearer token authentication (handled by auth middleware).
/// The ticket is valid for 30 seconds and consumed on first use.
pub(crate) async fn api_create_ws_ticket(
    State(auth): State<AuthServices>,
) -> impl IntoResponse {
    let ticket = uuid::Uuid::new_v4().to_string();
    let mut store = auth.ws_tickets.lock().await;
    // Cleanup expired tickets on each call to prevent unbounded growth
    store.retain(|_, created| created.elapsed().as_secs() < TICKET_TTL_SECS);
    store.insert(ticket.clone(), std::time::Instant::now());
    Json(json!({ "ticket": ticket }))
}

/// Validate and consume a one-time WS ticket. Returns true if valid.
pub(crate) async fn validate_ws_ticket(
    tickets: &tokio::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    ticket: &str,
) -> bool {
    let mut map = tickets.lock().await;
    if let Some(created) = map.remove(ticket) {
        created.elapsed().as_secs() < TICKET_TTL_SECS
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::clusters::AuthServices;

    #[tokio::test]
    async fn ws_ticket_is_created() {
        let auth = AuthServices::test_new();
        let resp = api_create_ws_ticket(axum::extract::State(auth.clone())).await;
        let _ = axum::response::IntoResponse::into_response(resp);
        let tickets = auth.ws_tickets.lock().await;
        assert_eq!(tickets.len(), 1);
    }
}
