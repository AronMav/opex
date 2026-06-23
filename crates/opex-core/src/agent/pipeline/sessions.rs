//! Pipeline step: sessions — session tool handlers as free functions.
//! Migrated from engine_sessions.rs to decouple from AgentEngine.

use sqlx::PgPool;
use uuid::Uuid;

use crate::agent::channel_actions::{ChannelAction, ChannelActionRouter};
use crate::agent::session_agent_pool::SessionPoolsMap;
use crate::agent::session_manager::SessionManager;
use crate::gateway::state::AgentMap;

use super::context::truncate_preview;

/// Extract session_id from enriched `_context` (same logic as engine_agent_tool).
fn extract_session_id(args: &serde_json::Value) -> Option<Uuid> {
    args.get("_context")
        .and_then(|ctx| ctx.get("session_id"))
        .and_then(|s| s.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
}

/// List recent sessions for a given agent.
pub async fn handle_sessions_list(
    db: &PgPool,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20)
        .min(100);
    let channel_filter = args.get("channel").and_then(|v| v.as_str());

    #[allow(clippy::type_complexity)]
    let rows: Result<
        Vec<(
            Uuid,
            String,
            String,
            chrono::DateTime<chrono::Utc>,
            i64,
        )>,
        _,
    > = sqlx::query_as(
        "SELECT s.id, s.user_id, s.channel, s.last_message_at, \
         COALESCE(mc.cnt, 0) as msg_count \
         FROM sessions s \
         LEFT JOIN (SELECT session_id, COUNT(*) as cnt FROM messages GROUP BY session_id) mc \
         ON mc.session_id = s.id \
         WHERE s.agent_id = $1 AND ($2::text IS NULL OR s.channel = $2) \
         ORDER BY s.last_message_at DESC LIMIT $3",
    )
    .bind(agent_name)
    .bind(channel_filter)
    .bind(limit)
    .fetch_all(db)
    .await;

    match rows {
        Ok(sessions) if sessions.is_empty() => "No sessions found.".to_string(),
        Ok(sessions) => {
            let mut out = format!("Sessions ({}):\n", sessions.len());
            for (id, user_id, channel, last_msg, msg_count) in &sessions {
                out.push_str(&format!(
                    "- `{}` | user: {} | channel: {} | msgs: {} | last: {}\n",
                    id,
                    user_id,
                    channel,
                    msg_count,
                    last_msg.format("%Y-%m-%d %H:%M"),
                ));
            }
            out
        }
        Err(e) => format!("Error listing sessions: {}", e),
    }
}

/// Retrieve message history from a specific session.
pub async fn handle_sessions_history(
    db: &PgPool,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let session_id_str = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(50)
        .min(200);

    let session_id = match Uuid::parse_str(session_id_str) {
        Ok(u) => u,
        Err(_) => return "Error: invalid session_id (expected UUID)".to_string(),
    };

    let rows: Result<Vec<(String, String, chrono::DateTime<chrono::Utc>)>, _> = sqlx::query_as(
        "SELECT m.role, LEFT(m.content, 300), m.created_at \
         FROM messages m JOIN sessions s ON s.id = m.session_id \
         WHERE m.session_id = $1 AND s.agent_id = $2 \
         ORDER BY m.created_at ASC LIMIT $3",
    )
    .bind(session_id)
    .bind(agent_name)
    .bind(limit)
    .fetch_all(db)
    .await;

    match rows {
        Ok(msgs) if msgs.is_empty() => {
            "No messages found (session not found or belongs to another agent).".to_string()
        }
        Ok(msgs) => {
            let mut out = format!("Session {} — {} messages:\n\n", session_id, msgs.len());
            for (role, content, created_at) in &msgs {
                out.push_str(&format!(
                    "**[{}]** {} {}\n",
                    role,
                    created_at.format("%H:%M:%S"),
                    content,
                ));
            }
            out
        }
        Err(e) => format!("Error loading messages: {}", e),
    }
}

/// List all running agents with their provider and model info.
pub async fn handle_agents_list(
    agent_map: Option<&AgentMap>,
    session_pools: Option<&SessionPoolsMap>,
    self_agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let agent_map = match agent_map {
        Some(m) => m,
        None => return "Error: agent map not available (subagent context)".to_string(),
    };

    // Collect active pool agents for the current session
    let session_id = extract_session_id(args);
    let mut active_in_session: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    if let (Some(sid), Some(pools)) = (session_id, session_pools) {
        let pools_read = pools.read().await;
        if let Some(pool) = pools_read.get(&sid) {
            for entry in pool.list() {
                active_in_session.insert(entry.name.clone());
            }
        }
    }

    let map = agent_map.read().await;
    if map.is_empty() {
        return "No agents running.".to_string();
    }
    let mut out = format!("Agents ({}):\n", map.len());
    for (name, handle) in map.iter() {
        let a = &handle.engine.cfg().agent;
        let is_self = name == self_agent_name;
        let base_tag = if a.base { " [BASE]" } else { "" };
        let active_tag = if active_in_session.contains(name.as_str()) {
            " [ACTIVE]"
        } else {
            ""
        };
        out.push_str(&format!(
            "- **{}**{}{}{}: {} / {} (lang: {})\n",
            name,
            if is_self { " (you)" } else { "" },
            base_tag,
            active_tag,
            a.provider,
            a.model,
            a.language,
        ));
    }

    if !active_in_session.is_empty() {
        out.push_str(&format!(
            "\nActive in session: {}\n",
            active_in_session
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out
}

/// Search messages across all sessions by content.
pub async fn handle_session_search(
    db: &PgPool,
    agent_name: &str,
    args: &serde_json::Value,
) -> String {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if query.is_empty() {
        return "Error: `query` parameter is required".to_string();
    }
    let limit = args
        .get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(20)
        .min(100);

    match SessionManager::new(db.clone())
        .search_messages(agent_name, query, limit)
        .await
    {
        Ok(results) if results.is_empty() => format!("No messages matching \"{}\".", query),
        Ok(results) => {
            let mut out = format!(
                "Found {} messages matching \"{}\":\n\n",
                results.len(),
                query
            );
            for r in &results {
                let preview = truncate_preview(&r.content, 200);
                out.push_str(&format!(
                    "- [{}] {} | session: {} | user: {} | {}\n  {}\n",
                    r.role,
                    r.created_at.format("%Y-%m-%d %H:%M"),
                    r.session_id,
                    r.user_id,
                    r.channel,
                    preview,
                ));
            }
            out
        }
        Err(e) => format!("Error searching messages: {}", e),
    }
}

/// Get metadata about a session (current or specified).
pub async fn handle_session_context(db: &PgPool, args: &serde_json::Value) -> String {
    let session_id_str = args
        .get("_context")
        .and_then(|c| c.get("session_id"))
        .and_then(|v| v.as_str())
        .or_else(|| args.get("session_id").and_then(|v| v.as_str()))
        .unwrap_or("");

    let session_id = match Uuid::parse_str(session_id_str) {
        Ok(u) => u,
        Err(_) => return "Error: no session_id available in current context".to_string(),
    };

    let sm = SessionManager::new(db.clone());
    match sm.get_session(session_id).await {
        Ok(Some(s)) => {
            let msg_count = sm.count_messages(session_id).await.unwrap_or(0);
            format!(
                "Current session:\n- ID: {}\n- Agent: {}\n- User: {}\n- Channel: {}\n- Messages: {}\n- Started: {}\n- Last activity: {}",
                s.id, s.agent_id, s.user_id, s.channel, msg_count,
                s.started_at.format("%Y-%m-%d %H:%M:%S"),
                s.last_message_at.format("%Y-%m-%d %H:%M:%S"),
            )
        }
        Ok(None) => format!("Session {} not found.", session_id),
        Err(e) => format!("Error getting session: {}", e),
    }
}

/// Send a message to a specific user/channel via channel adapter.
pub async fn handle_session_send(
    channel_router: Option<&ChannelActionRouter>,
    args: &serde_json::Value,
) -> String {
    let message = args
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let user_id = args
        .get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let channel = args
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or("telegram");

    if message.is_empty() {
        return "Error: `message` parameter is required".to_string();
    }
    if user_id.is_empty() {
        return "Error: `user_id` parameter is required".to_string();
    }

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let action = ChannelAction {
        name: "send_message".to_string(),
        params: serde_json::json!({
            "text": message,
            "chat_id": user_id,
        }),
        context: serde_json::json!({
            "channel": channel,
            "chat_id": user_id,
        }),
        reply: reply_tx,
        target_channel: Some(channel.to_string()),
    };

    if let Some(router) = channel_router {
        match router.send(action).await {
            Ok(_) => {
                match tokio::time::timeout(std::time::Duration::from_secs(5), reply_rx).await {
                    Ok(Ok(Ok(()))) => format!("Message sent to {} via {}.", user_id, channel),
                    Ok(Ok(Err(e))) => format!("Channel error: {}", e),
                    Ok(Err(_)) => format!("Message queued to {} (no confirmation).", user_id),
                    Err(_) => format!("Message queued to {} (timeout).", user_id),
                }
            }
            Err(e) => format!("Error sending message: {}", e),
        }
    } else {
        "Error: no channel adapter connected".to_string()
    }
}

/// Export a session's full conversation as text or JSON.
pub async fn handle_session_export(db: &PgPool, args: &serde_json::Value) -> String {
    let session_id_str = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let format = args
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("text");

    let session_id = match Uuid::parse_str(session_id_str) {
        Ok(u) => u,
        Err(_) => return "Error: invalid session_id (expected UUID)".to_string(),
    };

    match SessionManager::new(db.clone())
        .load_messages(session_id, Some(500))
        .await
    {
        Ok(msgs) if msgs.is_empty() => "No messages found in session.".to_string(),
        Ok(msgs) => {
            if format == "json" {
                let json_msgs: Vec<serde_json::Value> = msgs
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "role": m.role,
                            "content": m.content,
                            "created_at": m.created_at.to_rfc3339(),
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&json_msgs).unwrap_or_default()
            } else {
                let mut out = String::new();
                for m in &msgs {
                    out.push_str(&format!(
                        "[{}] {} {}\n\n",
                        m.role,
                        m.created_at.format("%Y-%m-%d %H:%M:%S"),
                        m.content,
                    ));
                }
                out
            }
        }
        Err(e) => format!("Error exporting session: {}", e),
    }
}
