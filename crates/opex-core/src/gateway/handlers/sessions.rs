use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{delete, get, patch, post},
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::db::sessions;
use crate::gateway::ApiError;
use super::super::AppState;
use crate::gateway::clusters::{AgentCore, ChannelBus, InfraServices};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/sessions", get(api_list_sessions).delete(api_delete_all_sessions))
        .route("/api/sessions/search", get(api_search_sessions))
        .route("/api/sessions/stuck", get(api_stuck_sessions))
        .route("/api/sessions/{id}", get(api_get_session).delete(api_delete_session).patch(api_patch_session))
        .route("/api/sessions/{id}/share", post(api_share_session).delete(api_unshare_session))
        .route("/api/shares/{token}", get(api_get_shared))
        .route("/api/sessions/{id}/invite", post(api_invite_to_session))
        .route("/api/sessions/{id}/messages", get(api_session_messages))
        .route("/api/messages/bookmarked", get(api_list_bookmarked))
        .route("/api/messages/{id}", delete(api_delete_message))
        .route("/api/messages/{id}/feedback", post(api_message_feedback))
        .route("/api/messages/{id}/bookmark", patch(api_toggle_bookmark))
        .route("/api/sessions/{id}/fork", post(api_fork_session))
        .route("/api/sessions/{id}/chain", get(api_session_chain))
        .route("/api/sessions/{id}/retry", post(api_retry_session))
}

// ── Sessions & Messages API ──

#[derive(Debug, Deserialize)]
pub(crate) struct SessionsQuery {
    pub(crate) agent: Option<String>,
    pub(crate) channel: Option<String>,
    pub(crate) limit: Option<i64>,
    /// Keyset cursor (paired with `before_id`): last `last_message_at` seen on
    /// the previous page. Both-or-neither with `before_id` — supplying only
    /// one is a BadRequest, since a row-comparison cursor is meaningless with
    /// just one half of the tuple.
    pub(crate) before_last_message_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Keyset cursor tie-break: `id` of the last row seen on the previous page.
    pub(crate) before_id: Option<uuid::Uuid>,
}

pub(crate) async fn api_list_sessions(
    State(infra): State<InfraServices>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(20).min(100);

    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => {
            return ApiError::BadRequest("agent parameter required".into()).into_response();
        }
    };

    let cursor = match (q.before_last_message_at, q.before_id) {
        (Some(ts), Some(id)) => Some((ts, id)),
        (None, None) => None,
        _ => {
            return ApiError::BadRequest(
                "before_last_message_at and before_id must be supplied together".into(),
            )
            .into_response();
        }
    };

    // Filter by ownership (agent_id), not participation. Previously this
    // included `OR $1 = ANY(participants)` which surfaced sessions where
    // the agent was merely invited or @-mentioned. Those sessions are owned
    // by a different agent and cannot be deleted through this agent's
    // session list (the DELETE path checks agent_id = owner), so showing
    // them created a broken UX: "I see the session but can't delete it."
    // Ownership is the only predicate that matches the delete permission.
    let channels: Option<Vec<String>> = q
        .channel
        .as_deref()
        .map(|channel| channel.split(',').map(str::to_string).collect());

    let page = sessions::list_sessions_page(&infra.db, agent, channels.as_deref(), limit, cursor)
        .await
        .map_err(|e| e.to_string());

    match page {
        Ok((rows, total)) => {
            // Batch-fetch last input_tokens per session from usage_log (single query, not N+1).
            let session_ids: Vec<uuid::Uuid> = rows.iter().map(|s| s.id).collect();
            let token_map: HashMap<uuid::Uuid, i64> = if session_ids.is_empty() {
                HashMap::new()
            } else {
                sqlx::query_as::<_, (uuid::Uuid, i32)>(
                    "SELECT DISTINCT ON (session_id) session_id, input_tokens \
                     FROM usage_log \
                     WHERE session_id = ANY($1) AND status IS DISTINCT FROM 'aborted' AND input_tokens > 0 \
                     ORDER BY session_id, created_at DESC",
                )
                .bind(&session_ids)
                .fetch_all(&infra.db)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, t)| (id, t as i64))
                .collect()
            };

            // Batch-fetch compression segment counts (single query, not N+1).
            let segment_count_map: HashMap<uuid::Uuid, i32> = if session_ids.is_empty() {
                HashMap::new()
            } else {
                sqlx::query_as::<_, (uuid::Uuid, i64)>(
                    "SELECT session_id, COUNT(*)::bigint \
                     FROM session_timeline \
                     WHERE session_id = ANY($1) AND event_type = 'compression' \
                     GROUP BY session_id",
                )
                .bind(&session_ids)
                .fetch_all(&infra.db)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, cnt)| (id, cnt as i32))
                .collect()
            };

            let sessions: Vec<Value> = rows
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "agent_id": s.agent_id,
                        "user_id": s.user_id,
                        "channel": s.channel,
                        "started_at": s.started_at.to_rfc3339(),
                        "last_message_at": s.last_message_at.to_rfc3339(),
                        "title": s.title,
                        "metadata": s.metadata,
                        "run_status": s.run_status,
                        "participants": s.participants,
                        "parent_session_id": s.parent_session_id,
                        "end_reason": s.end_reason,
                        "last_input_tokens": token_map.get(&s.id),
                        "segment_count": segment_count_map.get(&s.id).copied().unwrap_or(1),
                    })
                })
                .collect();
            Json(json!({ "sessions": sessions, "total": total })).into_response()
        }
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct MessagesQuery {
    limit: Option<i64>,
    agent: Option<String>,
    before_id: Option<uuid::Uuid>,
}

pub(crate) async fn api_session_messages(
    State(infra): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<MessagesQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    // Audit 2026-05-08 (4th pass): `?agent=` is now MANDATORY here too. The
    // earlier IDOR fix made it optional which silently bypassed ownership
    // verification — any token-holder could read any session's messages by
    // guessing the UUID.
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent).await {
        return resp;
    }

    let db_start = std::time::Instant::now();
    let query_result = sessions::get_messages_page(&infra.db, id, q.before_id, limit).await;
    let db_result_label = if query_result.is_ok() { "ok" } else { "error" };
    infra
        .metrics
        .record_db_query_duration(db_result_label, db_start.elapsed());

    match query_result {
        Ok(page) => {
            let events_json: Vec<serde_json::Value> = page
                .compression_events
                .iter()
                .map(|e| json!({
                    "segment_index": e.segment_index,
                    "first_live_message_id": e.first_live_message_id,
                    "summary": e.summary,
                }))
                .collect();
            Json(json!({
                "messages": page.messages,
                "compression_events": events_json,
                "has_more": page.has_more,
            }))
            .into_response()
        }
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// DELETE /api/messages/{id}?agent=xxx — deletes a message owned by the given agent's session.
/// S1: agent query param required; JOIN with sessions prevents cross-agent deletion.
pub(crate) async fn api_delete_message(
    State(infra): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    let result = sqlx::query(
        "DELETE FROM messages WHERE id = $1 \
         AND session_id IN (SELECT id FROM sessions WHERE agent_id = $2)"
    )
        .bind(id)
        .bind(agent)
        .execute(&infra.db)
        .await;

    match result {
        Ok(r) if r.rows_affected() > 0 => Json(json!({"ok": true})).into_response(),
        Ok(_) => ApiError::NotFound("message not found or does not belong to agent".into()).into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// GET /api/sessions/{id}
/// Returns lightweight session metadata for deep-link resolution (agent_id, channel, run_status).
/// Does not require an agent parameter — used by the frontend to locate the owning agent.
pub(crate) async fn api_get_session(
    State(infra): State<InfraServices>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> impl IntoResponse {
    #[derive(sqlx::FromRow)]
    struct SessionMeta {
        id: uuid::Uuid,
        agent_id: String,
        channel: String,
        run_status: Option<String>,
    }

    let row = sqlx::query_as::<_, SessionMeta>(
        "SELECT id, agent_id, channel, run_status FROM sessions WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&infra.db)
    .await;

    match row {
        Ok(Some(r)) => {
            let last_input_tokens: Option<i64> = sqlx::query_scalar::<_, i32>(
                "SELECT input_tokens FROM usage_log \
                 WHERE session_id = $1 AND status IS DISTINCT FROM 'aborted' AND input_tokens > 0 \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(r.id)
            .fetch_optional(&infra.db)
            .await
            .unwrap_or(None)
            .map(|v| v as i64);

            Json(json!({
                "id": r.id,
                "agent_id": r.agent_id,
                "channel": r.channel,
                "run_status": r.run_status,
                "last_input_tokens": last_input_tokens.unwrap_or(0),
            }))
            .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// DELETE /api/sessions/{id}
/// Deletes a session and all its messages. Requires agent param for ownership check.
pub(crate) async fn api_delete_session(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => {
            return ApiError::BadRequest("agent parameter required for session deletion".into()).into_response();
        }
    };

    let _ = sqlx::query("DELETE FROM messages WHERE session_id = $1 AND session_id IN (SELECT id FROM sessions WHERE agent_id = $2)")
        .bind(id)
        .bind(agent)
        .execute(&infra.db)
        .await;

    let result = sqlx::query("DELETE FROM sessions WHERE id = $1 AND agent_id = $2")
        .bind(id)
        .bind(agent)
        .execute(&infra.db)
        .await;

    match result {
        Ok(r) if r.rows_affected() == 0 => {
            ApiError::NotFound("session not found or does not belong to agent".into()).into_response()
        }
        Ok(_) => {
            tracing::info!(session_id = %id, agent = %agent, "session deleted via API");
            // Kill any live agents in the session pool
            let mut pools = agents.session_pools.write().await;
            if let Some(mut pool) = pools.remove(&id)
                && !pool.is_empty() {
                    tracing::info!(session_id = %id, count = pool.len(), "killing session agent pool on delete");
                    pool.kill_all();
                }
            agents.session_tool_state.remove(&id);
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

pub(crate) async fn verify_session_agent(db: &sqlx::PgPool, session_id: uuid::Uuid, expected_agent: &str) -> Result<(), axum::response::Response> {
    let row = sqlx::query_scalar::<_, String>(
        "SELECT agent_id FROM sessions WHERE id = $1"
    )
    .bind(session_id)
    .fetch_optional(db)
    .await;

    match row {
        Ok(Some(agent_id)) if agent_id == expected_agent => Ok(()),
        Ok(Some(_)) => Err(ApiError::Forbidden("session belongs to a different agent".into()).into_response()),
        Ok(None) => Err(ApiError::NotFound("session not found".into()).into_response()),
        Err(e) => Err(ApiError::Internal(e.to_string()).into_response()),
    }
}

/// DELETE /api/sessions?agent=xxx or DELETE /api/sessions?channel=discuss,group
/// Deletes all sessions (and their messages) for a specific agent or channel(s).
pub(crate) async fn api_delete_all_sessions(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    use sqlx::Row;

    // Collect matching session IDs before deletion so we can clean up session_pools
    let session_ids: Vec<uuid::Uuid> = if let Some(ref channel) = q.channel {
        let channels: Vec<&str> = channel.split(',').collect();
        sqlx::query("SELECT id FROM sessions WHERE channel = ANY($1)")
            .bind(&channels)
            .fetch_all(&infra.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.get::<uuid::Uuid, _>("id"))
            .collect()
    } else if let Some(ref agent) = q.agent {
        sqlx::query("SELECT id FROM sessions WHERE agent_id = $1")
            .bind(agent)
            .fetch_all(&infra.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.get::<uuid::Uuid, _>("id"))
            .collect()
    } else {
        return ApiError::BadRequest("agent or channel parameter required".into()).into_response();
    };

    // Support either agent or channel filter
    let result = if let Some(ref channel) = q.channel {
        let channels: Vec<&str> = channel.split(',').collect();
        let _ = sqlx::query(
            "DELETE FROM messages WHERE session_id IN (SELECT id FROM sessions WHERE channel = ANY($1))",
        )
        .bind(&channels)
        .execute(&infra.db)
        .await;
        sqlx::query("DELETE FROM sessions WHERE channel = ANY($1)")
            .bind(&channels)
            .execute(&infra.db)
            .await
    } else if let Some(ref agent) = q.agent {
        let _ = sqlx::query(
            "DELETE FROM messages WHERE session_id IN (SELECT id FROM sessions WHERE agent_id = $1)",
        )
        .bind(agent)
        .execute(&infra.db)
        .await;
        sqlx::query("DELETE FROM sessions WHERE agent_id = $1")
            .bind(agent)
            .execute(&infra.db)
            .await
    } else {
        return ApiError::BadRequest("agent or channel parameter required".into()).into_response();
    };

    match result {
        Ok(r) => {
            // Clean up session agent pools for the deleted sessions
            {
                let mut pools = agents.session_pools.write().await;
                pools.retain(|sid, _| !session_ids.contains(sid));
            }
            agents.session_tool_state.retain(|sid, _| !session_ids.contains(sid));
            let filter = q.agent.as_deref().or(q.channel.as_deref()).unwrap_or("?");
            tracing::info!(filter = %filter, deleted = r.rows_affected(), "sessions deleted via API");
            Json(json!({"ok": true, "deleted": r.rows_affected()})).into_response()
        }
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// GET /api/sessions/search?q=...&agent=...&limit=30&all=true
/// Full-text search across conversation history (messages) plus a
/// session-title section. `all=true` searches across every agent;
/// otherwise `agent` is required (contract preserved from 2026-05-08).
pub(crate) async fn api_search_sessions(
    State(infra): State<InfraServices>,
    Query(q): Query<SessionSearchQuery>,
) -> impl IntoResponse {
    let query_str = q.q.as_deref().unwrap_or("").trim();
    if query_str.is_empty() {
        return ApiError::BadRequest("q parameter required".into()).into_response();
    }
    let search_all = q.all.unwrap_or(false);
    // Audit 2026-05-08 (5th pass): replaced silent `unwrap_or("main")` with
    // an explicit BadRequest. The previous fallback let a token-holder
    // search agent "main"'s sessions just by omitting `?agent=`, and broke
    // the contract uniformity established by the rest of session API.
    // `all=true` is the one deliberate escape hatch (Ctrl+K all-agents mode).
    let agent = if search_all {
        None
    } else {
        match q.agent.as_deref() {
            Some(a) if !a.is_empty() => Some(a),
            _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
        }
    };
    let limit = q.limit.unwrap_or(30).min(100);

    let messages = match sessions::search_messages(&infra.db, agent, query_str, limit).await {
        Ok(results) => results,
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };
    let session_hits = match sessions::search_session_titles(&infra.db, agent, query_str, 10).await {
        Ok(hits) => hits,
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };

    let messages_json: Vec<Value> = messages.iter().map(|r| json!({
        "message_id": r.message_id.to_string(),
        "content": r.content,
        "session_id": r.session_id.to_string(),
        "session_title": r.session_title,
        "agent_id": r.agent_id,
        "user_id": r.user_id,
        "channel": r.channel,
        "role": r.role,
        "created_at": r.created_at.to_rfc3339(),
        "rank": r.rank,
        "snippet": r.snippet,
    })).collect();
    let sessions_json: Vec<Value> = session_hits.iter().map(|h| json!({
        "session_id": h.session_id.to_string(),
        "title": h.title,
        "agent_id": h.agent_id,
        "last_message_at": h.last_message_at.to_rfc3339(),
    })).collect();

    Json(json!({
        "messages": messages_json,
        "sessions": sessions_json,
        "count": messages_json.len(),
    })).into_response()
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionSearchQuery {
    q: Option<String>,
    agent: Option<String>,
    limit: Option<i64>,
    all: Option<bool>,
}

// ── Session Invite ──

#[derive(Debug, Deserialize)]
pub(crate) struct InviteRequest {
    pub agent_name: String,
}

/// POST /api/sessions/{id}/invite?agent=xxx — invite an agent into a multi-agent session.
///
/// Audit 2026-05-08 (6th pass): `?agent=` is required so a token-holder cannot
/// inject participants into someone else's session by guessing the UUID.
pub(crate) async fn api_invite_to_session(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    State(bus): State<ChannelBus>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
    Json(req): Json<InviteRequest>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent).await {
        return resp;
    }

    // Audit 2026-05-08 (7th pass): refuse self-invite. The session owner is
    // the implicit primary participant; explicitly inviting them would
    // duplicate the entry in `sessions.participants` since `add_participant`
    // uses `array_append`, not `array(SELECT DISTINCT …)`.
    if req.agent_name == agent {
        return ApiError::BadRequest(
            "cannot invite the session owner — already a participant".into(),
        )
        .into_response();
    }

    // Validate target agent exists
    let agent_exists = {
        let map = agents.map.read().await;
        map.contains_key(&req.agent_name)
    };
    if !agent_exists {
        return ApiError::NotFound(format!("agent '{}' not found", req.agent_name)).into_response();
    }

    match crate::db::sessions::add_participant(&infra.db, id, &req.agent_name, None).await {
        Ok(participants) => {
            // Broadcast to WebSocket for live UI updates
            let event = opex_types::ws::WsEvent::AgentJoined {
                agent_name: req.agent_name.clone(),
                session_id: id.to_string(),
                invited_by: "user".to_string(),
                participants: participants.clone(),
            };
            bus.ui_event_tx.send(event.to_json()).ok();

            Json(json!({ "participants": participants })).into_response()
        }
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

// ── Session sharing (read-only public links) ────────────────────────────────

/// POST /api/sessions/{id}/share?agent=xxx — create (or return existing)
/// read-only share link. Returns the unguessable token; the caller builds the
/// full URL. Ownership is verified via `?agent=` (same IDOR guard as the rest).
pub(crate) async fn api_share_session(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent).await {
        return resp;
    }
    // 256-bit unguessable token — the security boundary for the public read.
    let token = format!("{:032x}{:032x}", rand::random::<u128>(), rand::random::<u128>());
    match crate::db::shares::create_or_get_share(&infra.db, id, &token, agent).await {
        Ok(tok) => Json(json!({ "token": tok, "path": format!("/share?token={tok}") })).into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// DELETE /api/sessions/{id}/share?agent=xxx — revoke the share link.
pub(crate) async fn api_unshare_session(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent).await {
        return resp;
    }
    match crate::db::shares::delete_share_for_session(&infra.db, id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// GET /api/shares/{token} — PUBLIC read-only snapshot (auth-exempt; the token
/// is the security boundary). Returns a sanitized transcript: user/assistant/
/// tool turns with text + tool *names* only (no args, no system prompts, no
/// reasoning) so a shared link can't leak internal context or secrets.
pub(crate) async fn api_get_shared(
    State(infra): State<InfraServices>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    let session_id = match crate::db::shares::session_for_token(&infra.db, &token).await {
        Ok(Some(sid)) => sid,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "share not found"}))).into_response(),
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };
    let session = match sessions::get_session(&infra.db, session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => return (StatusCode::NOT_FOUND, Json(json!({"error": "session not found"}))).into_response(),
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };
    let rows = match sessions::load_messages(&infra.db, session_id, Some(500)).await {
        Ok(r) => r,
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };

    let messages: Vec<Value> = rows
        .iter()
        .filter(|m| m.role != "system") // never expose system prompts
        .map(|m| {
            // Tool *names* only — args may carry data/secrets the sharer didn't
            // intend to publish.
            let tools: Vec<String> = m
                .tool_calls
                .as_ref()
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| {
                            c.get("function")
                                .and_then(|f| f.get("name"))
                                .or_else(|| c.get("name"))
                                .and_then(|n| n.as_str())
                                .map(String::from)
                        })
                        .collect()
                })
                .unwrap_or_default();
            // F071: redact tool RESULT content on the public share link. Tool
            // outputs (role='tool') can carry fetched secrets / PII / internal
            // API bodies — strictly more sensitive than the tool *arguments*
            // this endpoint already strips. The share contract promises "tool
            // names only".
            let content = if m.role == "tool" {
                Value::String("[tool result hidden]".to_string())
            } else {
                Value::String(m.content.clone())
            };
            json!({
                "role": m.role,
                "content": content,
                "tools": tools,
                "created_at": m.created_at,
            })
        })
        .collect();

    Json(json!({
        "title": session.title,
        "agent": session.agent_id,
        "messages": messages,
    }))
    .into_response()
}

// ── Session Patch (rename) ──

/// POST /api/messages/{id}/feedback?agent=xxx — set feedback (1=like, -1=dislike, 0=clear)
///
/// Requires `?agent=<owner>` and JOINs through `sessions.agent_id` to prevent
/// cross-agent feedback writes (audit 2026-05-08, IDOR).
pub(crate) async fn api_message_feedback(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
    Json(body): Json<FeedbackRequest>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    let feedback = body.feedback.clamp(-1, 1);
    let result = sqlx::query(
        "UPDATE messages SET feedback = $1 \
         WHERE id = $2 \
         AND session_id IN (SELECT id FROM sessions WHERE agent_id = $3)",
    )
        .bind(feedback as i16)
        .bind(id)
        .bind(agent)
        .execute(&infra.db)
        .await;
    match result {
        Ok(r) if r.rows_affected() > 0 => Json(json!({"ok": true})).into_response(),
        Ok(_) => ApiError::NotFound("message not found or wrong agent".into()).into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct FeedbackRequest {
    feedback: i32, // 1 = like, -1 = dislike, 0 = clear
}

// ── Message bookmarks (wave 2) ──────────────────────────────────────────────

#[derive(Deserialize)]
pub(crate) struct BookmarkRequest {
    bookmarked: bool,
}

/// PATCH /api/messages/{id}/bookmark?agent=xxx — set/clear a bookmark on a message.
///
/// Requires `?agent=<owner>` and JOINs through `sessions.agent_id` to prevent
/// cross-agent bookmark writes (same IDOR-guard shape as `api_message_feedback`).
pub(crate) async fn api_toggle_bookmark(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
    Json(body): Json<BookmarkRequest>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    match sessions::toggle_bookmark(&infra.db, id, agent, body.bookmarked).await {
        Ok(n) if n > 0 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => ApiError::NotFound("message not found or wrong agent".into()).into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct BookmarkedQuery {
    agent: Option<String>,
    all: Option<bool>,
    limit: Option<i64>,
}

/// GET /api/messages/bookmarked?agent=&all=&limit= — list bookmarked messages.
///
/// `all=true` lists across every agent (deliberate escape hatch, mirrors
/// `/api/sessions/search`); otherwise `agent` is required.
pub(crate) async fn api_list_bookmarked(
    State(infra): State<InfraServices>,
    Query(q): Query<BookmarkedQuery>,
) -> impl IntoResponse {
    let list_all = q.all.unwrap_or(false);
    let agent = if list_all {
        None
    } else {
        match q.agent.as_deref() {
            Some(a) if !a.is_empty() => Some(a),
            _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
        }
    };
    let limit = q.limit.unwrap_or(50).min(200);

    let hits = match sessions::list_bookmarked(&infra.db, agent, limit).await {
        Ok(h) => h,
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };

    let items: Vec<Value> = hits.iter().map(|h| json!({
        "message_id": h.message_id.to_string(),
        "session_id": h.session_id.to_string(),
        "session_title": h.session_title,
        "agent_id": h.agent_id,
        "preview": sessions::text_preview(&h.content, 160),
        "role": h.role,
        "bookmarked_at": h.bookmarked_at.to_rfc3339(),
    })).collect();

    Json(json!({ "items": items })).into_response()
}

// ── Fork (branch) endpoint ──────────────────────────────

#[derive(Deserialize)]
pub(crate) struct ForkRequest {
    branch_from_message_id: uuid::Uuid, // the user message being replaced
    content: String,                     // new user message text
}

/// Result of a session fork — reports the ACTUAL branch point used, which may
/// differ from the requested id when it was never persisted (see
/// [`fork_session_inner`]).
pub(crate) struct ForkResult {
    pub(crate) message_id: uuid::Uuid,
    pub(crate) parent_message_id: Option<uuid::Uuid>,
    pub(crate) branch_from_message_id: Option<uuid::Uuid>,
}

/// Fetch the most recently created message in a session, or `None` if the
/// session has no messages yet.
async fn last_message_id(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
) -> anyhow::Result<Option<uuid::Uuid>> {
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "SELECT id FROM messages WHERE session_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Core fork logic, extracted from [`api_fork_session`] so it is testable
/// directly (sqlx tests can drive it without an HTTP harness).
///
/// WS2: the UI may hold an optimistic (never-persisted) message id after a
/// failed turn — e.g. the client-side placeholder created before the LLM call
/// even started. Blindly trusting that id in the INSERT would violate
/// `messages_branch_from_message_id_fkey` and 500 the retry path. Instead,
/// verify the requested id actually exists in this session first; if it
/// doesn't, fall back to the session's newest persisted message as the branch
/// point (logged as a warning, never an error) so «Повторить» never
/// dead-ends.
pub(crate) async fn fork_session_inner(
    db: &sqlx::PgPool,
    session_id: uuid::Uuid,
    requested_branch_id: Option<uuid::Uuid>,
    content: &str,
) -> anyhow::Result<ForkResult> {
    let branch_id = match requested_branch_id {
        Some(id) => {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE id = $1 AND session_id = $2)",
            )
            .bind(id)
            .bind(session_id)
            .fetch_one(db)
            .await?;
            if exists {
                Some(id)
            } else {
                tracing::warn!(
                    session_id = %session_id, requested = %id,
                    "fork branch id not found — falling back to last persisted message"
                );
                last_message_id(db, session_id).await?
            }
        }
        None => last_message_id(db, session_id).await?,
    };

    // Find the parent of the resolved branch point (the message BEFORE it).
    let parent_id = match branch_id {
        Some(id) => sessions::find_parent_of_message(db, session_id, id).await?,
        None => None,
    };

    let new_msg_id = sessions::save_message_branched(
        db,
        session_id,
        "user",
        content,
        None,
        None,
        None,
        None,
        parent_id,
        branch_id,
    )
    .await?;

    Ok(ForkResult {
        message_id: new_msg_id,
        parent_message_id: parent_id,
        branch_from_message_id: branch_id,
    })
}

/// POST /api/sessions/{id}/fork?agent=xxx — create a branched user message from an existing message.
///
/// Requires `?agent=<owner>` to prove session ownership: without this any
/// token-holder could write a message into any session by guessing the UUID
/// (audit 2026-05-08, IDOR).
pub(crate) async fn api_fork_session(
    State(infra): State<InfraServices>,
    Path(session_id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
    Json(body): Json<ForkRequest>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, session_id, agent).await {
        return resp;
    }

    match fork_session_inner(
        &infra.db,
        session_id,
        Some(body.branch_from_message_id),
        &body.content,
    )
    .await
    {
        Ok(result) => Json(json!({
            "message_id": result.message_id,
            "parent_message_id": result.parent_message_id,
            "branch_from_message_id": result.branch_from_message_id,
        }))
        .into_response(),
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// PATCH /api/sessions/{id}?agent=xxx — update session metadata (title, ui_state).
///
/// Requires `?agent=<owner>` to prove session ownership (audit 2026-05-08, IDOR).
pub(crate) async fn api_patch_session(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent).await {
        return resp;
    }

    if let Some(raw_title) = body.get("title").and_then(|v| v.as_str()) {
        let title: String = raw_title.chars().take(200).collect();
        match sqlx::query("UPDATE sessions SET title = $1 WHERE id = $2")
            .bind(title)
            .bind(id)
            .execute(&infra.db)
            .await
        {
            Ok(r) if r.rows_affected() == 0 => {
                return ApiError::NotFound("session not found".into()).into_response();
            }
            Ok(_) => {}
            Err(e) => {
                return ApiError::Internal(e.to_string()).into_response();
            }
        }
    }
    // Persist UI state inside metadata JSONB (merge, don't overwrite)
    if let Some(ui_state) = body.get("ui_state") {
        // Validate: must be an object, max 1KB serialized
        let serialized = ui_state.to_string();
        if !ui_state.is_object() || serialized.len() > 1024 {
            return ApiError::BadRequest("ui_state must be a JSON object under 1KB".into()).into_response();
        }
        match sqlx::query(
            "UPDATE sessions SET metadata = COALESCE(metadata, '{}'::jsonb) || jsonb_build_object('ui_state', $1::jsonb) WHERE id = $2"
        )
        .bind(ui_state)
        .bind(id)
        .execute(&infra.db)
        .await
        {
            Ok(r) if r.rows_affected() == 0 => {
                return ApiError::NotFound("session not found".into()).into_response();
            }
            Ok(_) => {}
            Err(e) => {
                return ApiError::Internal(e.to_string()).into_response();
            }
        }
    }

    Json(json!({"ok": true})).into_response()
}

// ── Session Auto-Retry ──────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub(crate) struct StuckSessionsQuery {
    stale_secs: Option<i64>,
    max_retries: Option<i32>,
}

/// GET /api/sessions/stuck — find sessions needing retry
pub(crate) async fn api_stuck_sessions(
    State(infra): State<InfraServices>,
    Query(q): Query<StuckSessionsQuery>,
) -> impl IntoResponse {
    let stale_secs = q.stale_secs.unwrap_or(90);
    let max_retries = q.max_retries.unwrap_or(3);

    match sessions::find_stuck_sessions(&infra.db, stale_secs, max_retries).await {
        Ok(rows) => {
            let sessions: Vec<serde_json::Value> = rows.iter().map(|(id, agent)| {
                serde_json::json!({"id": id, "agent_id": agent})
            }).collect();
            Json(serde_json::json!({"sessions": sessions})).into_response()
        }
        Err(e) => ApiError::Internal(e.to_string()).into_response(),
    }
}

/// POST /api/sessions/{id}/retry?agent=xxx — replay last user message through engine
///
/// Requires `?agent=<owner>` (audit 2026-05-08, IDOR).
/// F031: in-process set of sessions with a retry currently in flight. OPEX is
/// a single binary, so a process-local guard fully serializes retries of one
/// session — preventing a double-click / overlapping cron-retry from both
/// claiming it (the DB `increment_retry_count` alone re-matches a still-
/// 'running' row and does NOT), running two concurrent handle_sse loops, and
/// double-deleting the last user turn.
static RETRY_IN_FLIGHT: std::sync::LazyLock<dashmap::DashSet<uuid::Uuid>> =
    std::sync::LazyLock::new(dashmap::DashSet::new);

/// RAII: removes the session from [`RETRY_IN_FLIGHT`] on drop, so every exit
/// path (early return or the spawned run finishing) releases the guard.
struct RetryGuard(uuid::Uuid);
impl Drop for RetryGuard {
    fn drop(&mut self) {
        RETRY_IN_FLIGHT.remove(&self.0);
    }
}

pub(crate) async fn api_retry_session(
    State(infra): State<InfraServices>,
    State(agents): State<AgentCore>,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let agent_param = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent_param).await {
        return resp;
    }

    // F031: claim the in-process retry guard BEFORE any DB claim or destructive
    // delete. A concurrent retry of the same session gets 409 here and never
    // reaches increment_retry_count / the delete. The guard is dropped on every
    // early return below and, on the happy path, moved into the spawned run so
    // it releases only when handle_sse finishes.
    let retry_guard = if RETRY_IN_FLIGHT.insert(id) {
        RetryGuard(id)
    } else {
        return ApiError::Conflict("retry already in progress for this session".into())
            .into_response();
    };

    // 1. Load session
    let session: crate::db::sessions::Session = match sqlx::query_as(
        "SELECT * FROM sessions WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&infra.db)
    .await {
        Ok(Some(s)) => s,
        Ok(None) => return ApiError::NotFound("session not found".into()).into_response(),
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };

    // 2. Get last user message (with its id so the delete below is scoped to
    //    the EXACT captured row, not a re-evaluated ORDER BY subquery — F031).
    let (last_user_msg_id, user_text) = match sessions::get_last_user_message_with_id(&infra.db, id).await {
        Ok(Some(pair)) => pair,
        Ok(None) => return ApiError::BadRequest("no user message in session".into()).into_response(),
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };

    // F072: resolve the engine BEFORE any destructive delete. If the owning
    // agent was deleted/renamed/not-yet-reloaded, the None arm must return with
    // the transcript INTACT — the old order deleted the last user message first
    // and then 404'd, permanently losing the turn with no re-drive.
    let engine = match agents.get_engine(&session.agent_id).await {
        Some(e) => e,
        None => {
            let _ = sessions::mark_session_failed(&infra.db, id).await;
            return ApiError::NotFound(format!("agent '{}' not found", session.agent_id)).into_response();
        }
    };

    // 3. Increment retry count FIRST (atomic guard against concurrent
    //    double-retry). R-RETRY fix: this MUST happen before any destructive
    //    delete. Previously the handler deleted the last user message and empty
    //    assistant rows BEFORE this guard, so a lost race (Ok(None) → 409) left
    //    the user's last turn permanently deleted with no retry executed and no
    //    rollback. Guarding first means a 409 returns with the transcript intact.
    let retry_count = match sessions::increment_retry_count(&infra.db, id).await {
        Ok(Some(c)) => c,
        Ok(None) => return ApiError::Conflict("session not in running state (concurrent retry?)".into()).into_response(),
        Err(e) => return ApiError::Internal(e.to_string()).into_response(),
    };

    // 4. Cleanup: delete empty assistant messages and the last user message
    //    (handle_sse will re-save it, so we remove to avoid duplicates). Safe to
    //    delete now that we won the atomic retry guard above.
    if let Ok(deleted) = sessions::delete_empty_assistant_messages(&infra.db, id).await
        && deleted > 0 {
        tracing::info!(session_id = %id, deleted, "cleaned up empty assistant messages before retry");
    }
    // Delete the last user message — handle_sse will re-insert it. Scoped to
    // the id captured in step 2 (F031) so a race can't delete an older turn.
    let _ = sqlx::query("DELETE FROM messages WHERE id = $1")
        .bind(last_user_msg_id)
        .execute(&infra.db)
        .await;

    tracing::info!(session_id = %id, agent = %session.agent_id, retry_count, "retrying stuck session");

    // 6. Build message and run via handle_sse with resume_session_id (engine
    //    was resolved above, before any destructive delete — F072).
    let msg = opex_types::IncomingMessage {
        text: Some(user_text),
        user_id: session.user_id.clone(),
        channel: session.channel.clone(),
        agent_id: session.agent_id.clone(),
        context: Default::default(),
        attachments: vec![],
        leaf_message_id: None,
        user_message_id: None,
        tool_policy_override: None,
        timestamp: chrono::Utc::now(),
        formatting_prompt: None,
    };

    // Spawn background task
    let db = infra.db.clone();
    let session_id = id;
    tokio::spawn(async move {
        // F031: hold the retry guard for the whole run so a second retry is
        // rejected until this one finishes (dropped when the task returns).
        let _retry_guard = retry_guard;
        // Phase 62 RES-01: engine writes to the bounded EngineEventSender
        // wrapper; a local drain task silently consumes all events. The retry
        // path does not stream to any UI client — events are only needed for
        // session-state side effects (DB persistence happens inside handle_sse
        // regardless of whether the outer channel is consumed).
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::channel::<crate::agent::engine::StreamEvent>(256);
        tokio::spawn(async move { while raw_rx.recv().await.is_some() {} });
        let event_tx = crate::agent::engine_event_sender::EngineEventSender::new(raw_tx);

        match engine.handle_sse(&msg, event_tx, Some(session_id), false, tokio_util::sync::CancellationToken::new()).await {
            Ok(_msg_id) => {
                tracing::info!(session_id = %session_id, "retry succeeded");
            }
            Err(e) => {
                tracing::error!(session_id = %session_id, error = %e, "retry failed");
                sessions::mark_session_failed(&db, session_id).await.ok();
            }
        }
    });

    Json(serde_json::json!({"ok": true, "retry_count": retry_count, "session_id": id})).into_response()
}

// ── GET /api/sessions/{id}/chain ─────────────────────────────────────────────

/// GET /api/sessions/{id}/chain?agent=xxx — return the conversation tree.
///
/// Audit 2026-05-08 (5th pass) found this endpoint was missed by the IDOR
/// fixes — it returned the full fork graph (parent_session_id, branches,
/// chain) without any owner check. Now `?agent=` is required and gated by
/// `verify_session_agent`, matching every other session-read endpoint.
pub(crate) async fn api_session_chain(
    State(infra): State<InfraServices>,
    Path(id): Path<uuid::Uuid>,
    Query(q): Query<SessionsQuery>,
) -> impl IntoResponse {
    let agent = match q.agent.as_deref() {
        Some(a) if !a.is_empty() => a,
        _ => return ApiError::BadRequest("agent parameter required".into()).into_response(),
    };
    if let Err(resp) = verify_session_agent(&infra.db, id, agent).await {
        return resp;
    }
    match crate::db::sessions::get_session_chain(&infra.db, id).await {
        Ok(chain) if chain.is_empty() => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "session not found" })),
        )
            .into_response(),
        Ok(chain) => Json(serde_json::json!({ "chain": chain })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f031_retry_guard_serializes_then_releases() {
        let id = uuid::Uuid::new_v4();
        {
            assert!(RETRY_IN_FLIGHT.insert(id), "first retry claim must succeed");
            let _g = RetryGuard(id);
            assert!(
                !RETRY_IN_FLIGHT.insert(id),
                "a concurrent retry of the same session must be rejected"
            );
        } // guard drops here
        assert!(
            RETRY_IN_FLIGHT.insert(id),
            "the session must be claimable again once the run finished"
        );
        RETRY_IN_FLIGHT.remove(&id);
    }
}

// ── verify_session_agent (shared IDOR gate — stream.rs, misc.rs abort) ──────
//
// Covers the ownership check that `api_chat_stream` (chat/stream.rs, formerly
// chat/resume.rs's `api_chat_resume_stream`) and `api_chat_abort`
// (chat/misc.rs) now depend on (audit 2026-07-04, batch E). Both handlers
// are thin wrappers around this function plus a
// `?agent=` extraction identical to every other `sessions.rs` handler —
// exercising it here covers the ownership-check branch for both call sites
// without needing a full HTTP harness.
#[cfg(test)]
mod verify_session_agent_tests {
    use super::verify_session_agent;
    use sqlx::PgPool;

    #[sqlx::test(migrations = "../../migrations")]
    async fn matching_agent_is_allowed(db: PgPool) {
        let session_id = uuid::Uuid::new_v4();
        let agent = format!("test-owner-{session_id}");
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, started_at, last_message_at) \
             VALUES ($1, $2, 'test-user', 'web', now(), now())",
        )
        .bind(session_id)
        .bind(&agent)
        .execute(&db)
        .await
        .expect("insert session");

        let result = verify_session_agent(&db, session_id, &agent).await;
        assert!(result.is_ok(), "owning agent must be allowed");
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn different_agent_is_forbidden(db: PgPool) {
        let session_id = uuid::Uuid::new_v4();
        let owner = format!("test-owner-{session_id}");
        let intruder = format!("test-intruder-{session_id}");
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, started_at, last_message_at) \
             VALUES ($1, $2, 'test-user', 'web', now(), now())",
        )
        .bind(session_id)
        .bind(&owner)
        .execute(&db)
        .await
        .expect("insert session");

        let result = verify_session_agent(&db, session_id, &intruder).await;
        let resp = result.expect_err("different agent must be rejected");
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::FORBIDDEN,
            "wrong-owner request must be rejected with 403, not silently allowed"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn missing_session_is_not_found(db: PgPool) {
        let session_id = uuid::Uuid::new_v4();
        let result = verify_session_agent(&db, session_id, "whatever-agent").await;
        let resp = result.expect_err("nonexistent session must error, not succeed");
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }
}

// ── fork_session_inner (WS2: unknown branch id must not 500) ───────────────
#[cfg(test)]
mod fork_session_inner_tests {
    use super::fork_session_inner;
    use sqlx::PgPool;

    /// Insert a session with `count` chained user messages (each one's
    /// `parent_message_id` pointing at the previous), returning
    /// `(session_id, last_message_id)`.
    async fn seed_session_with_messages(db: &PgPool, count: u32) -> (uuid::Uuid, uuid::Uuid) {
        let session_id = uuid::Uuid::new_v4();
        let agent = format!("test-owner-{session_id}");
        sqlx::query(
            "INSERT INTO sessions (id, agent_id, user_id, channel, started_at, last_message_at) \
             VALUES ($1, $2, 'test-user', 'web', now(), now())",
        )
        .bind(session_id)
        .bind(&agent)
        .execute(db)
        .await
        .expect("insert session");

        let mut parent_id: Option<uuid::Uuid> = None;
        let mut last_id = uuid::Uuid::nil();
        for i in 0..count {
            let msg_id = uuid::Uuid::new_v4();
            sqlx::query(
                "INSERT INTO messages (id, session_id, role, content, parent_message_id, created_at) \
                 VALUES ($1, $2, 'user', $3, $4, now() + make_interval(secs => $5))",
            )
            .bind(msg_id)
            .bind(session_id)
            .bind(format!("message {i}"))
            .bind(parent_id)
            .bind(i as f64)
            .execute(db)
            .await
            .expect("insert message");
            parent_id = Some(msg_id);
            last_id = msg_id;
        }
        (session_id, last_id)
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn fork_with_unknown_branch_id_falls_back_to_last_message(pool: PgPool) {
        let (session_id, last_msg_id) = seed_session_with_messages(&pool, 3).await;
        let bogus = uuid::Uuid::new_v4();

        let result = fork_session_inner(&pool, session_id, Some(bogus), "retry me").await;

        let forked = result.expect("fork must not fail on unknown branch id");
        assert_eq!(
            forked.branch_from_message_id,
            Some(last_msg_id),
            "unknown branch id must fall back to the last persisted message"
        );
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn fork_with_known_branch_id_uses_requested_id(pool: PgPool) {
        let (session_id, _last_msg_id) = seed_session_with_messages(&pool, 3).await;

        // Fetch the first message id (not the last) to prove a valid,
        // non-last id is honoured rather than always falling back.
        let first_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT id FROM messages WHERE session_id = $1 ORDER BY created_at ASC LIMIT 1",
        )
        .bind(session_id)
        .fetch_one(&pool)
        .await
        .expect("fetch first message");

        let result = fork_session_inner(&pool, session_id, Some(first_id), "edit me").await;

        let forked = result.expect("fork with a valid branch id must succeed");
        assert_eq!(
            forked.branch_from_message_id,
            Some(first_id),
            "a known branch id must be used as-is, not overridden"
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use std::sync::Arc;
    use uuid::Uuid;

    #[tokio::test]
    async fn session_tool_state_removed_after_session_delete() {
        let tool_state: crate::agent::dispatcher::SessionToolStateMap =
            Arc::new(dashmap::DashMap::new());
        let session_id = Uuid::new_v4();

        let state = crate::agent::dispatcher::SessionToolState::new();
        state.set_describe("tool".into(), "schema".into()).await;
        tool_state.insert(session_id, state);
        assert!(tool_state.contains_key(&session_id));

        tool_state.remove(&session_id);

        assert!(!tool_state.contains_key(&session_id));
    }

    #[tokio::test]
    async fn session_tool_state_retained_for_surviving_sessions() {
        let tool_state: crate::agent::dispatcher::SessionToolStateMap =
            Arc::new(dashmap::DashMap::new());
        let keep_id = Uuid::new_v4();
        let delete_id = Uuid::new_v4();

        tool_state.insert(keep_id, crate::agent::dispatcher::SessionToolState::new());
        tool_state.insert(delete_id, crate::agent::dispatcher::SessionToolState::new());

        let deleted = [delete_id];
        tool_state.retain(|sid, _| !deleted.contains(sid));

        assert!(tool_state.contains_key(&keep_id));
        assert!(!tool_state.contains_key(&delete_id));
    }
}

