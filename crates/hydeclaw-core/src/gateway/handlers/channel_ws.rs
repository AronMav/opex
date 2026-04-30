use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message as WsMessage, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

use super::super::AppState;
use crate::db::outbound;
use crate::gateway::clusters::{AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor};

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/ws/channel/{agent_name}", get(channel_ws_handler))
}

/// Serialize a value to a WebSocket text frame.
/// Logs and returns a fallback error frame on failure — never panics.
pub(crate) fn ws_json(msg: &impl serde::Serialize) -> WsMessage {
    match serde_json::to_string(msg) {
        Ok(s) => WsMessage::Text(s.into()),
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize WebSocket message");
            WsMessage::Text(r#"{"error":"serialization error"}"#.into())
        }
    }
}

// ── Context bundle for channel WS loop ────────────────────────────────────────

/// All cluster state needed by the channel WS loop. Cheap to clone (all Arc-backed).
#[derive(Clone)]
struct CwsCtx {
    agents:  AgentCore,
    auth:    AuthServices,
    bus:     ChannelBus,
    infra:   InfraServices,
    status:  StatusMonitor,
    cfg:     ConfigServices,
}

// ── Channel Connector WebSocket (external adapters: Telegram, Discord, etc.) ──

#[allow(clippy::too_many_arguments)]
pub(crate) async fn channel_ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::Path(agent_name): axum::extract::Path<String>,
    State(agents): State<AgentCore>,
    State(auth): State<AuthServices>,
    State(bus): State<ChannelBus>,
    State(infra): State<InfraServices>,
    State(status): State<StatusMonitor>,
    State(cfg): State<ConfigServices>,
) -> impl IntoResponse {
    // Look up the agent — check it exists and has channel support.
    let agents_map = agents.map.read().await;
    let handle = if let Some(h) = agents_map.get(&agent_name) { h } else {
        drop(agents_map);
        return (StatusCode::NOT_FOUND, Json(json!({"error": "agent not found"}))).into_response();
    };

    if handle.channel_router.is_none() {
        drop(agents_map);
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "agent is not in external channel mode"}))).into_response();
    }
    drop(agents_map);

    let ctx = CwsCtx { agents, auth, bus, infra, status, cfg };
    ws.on_upgrade(move |socket| handle_channel_ws(socket, ctx, agent_name))
}

async fn handle_channel_ws(socket: WebSocket, ctx: CwsCtx, agent_name: String) {
    tracing::info!(agent = %agent_name, "channel adapter connected");

    // Subscribe to channel actions (non-exclusive — multiple channels per agent).
    let engine = {
        let agents_map = ctx.agents.map.read().await;
        let handle = if let Some(h) = agents_map.get(&agent_name) { h } else {
            tracing::warn!(agent = %agent_name, "agent disappeared before WS upgrade");
            return;
        };
        let engine = handle.engine.clone();
        // Log whether a guard exists at connect-time for diagnostics; access checks
        // always re-fetch live from ctx.auth.access_guards so this is informational only.
        let has_guard = ctx.auth.access_guards.read().await.contains_key(&agent_name);
        tracing::info!(agent = %agent_name, has_guard, "channel WS: connected");
        engine
    };

    // Run the main WS loop — channel_action_rx is created inside after Ready handshake.
    let connected_channel_type = channel_ws_loop(
        socket, &ctx, &agent_name, &engine,
    ).await;

    // Deregister from connected channels registry
    {
        let mut channels = ctx.bus.connected_channels.write().await;
        channels.retain(|c| !(c.agent_name == agent_name && c.channel_type == connected_channel_type));
    }
    // Notify UI about channel disconnect
    ctx.bus.ui_event_tx.send(
        serde_json::json!({"type": "channels_changed", "agent": &agent_name}).to_string()
    ).ok();

    tracing::info!(agent = %agent_name, channel = %connected_channel_type, "channel adapter disconnected");
}

/// Main WS event loop for a channel adapter. Returns the `channel_type` on disconnect.
async fn channel_ws_loop(
    socket: WebSocket,
    ctx: &CwsCtx,
    agent_name: &str,
    engine: &Arc<crate::agent::engine::AgentEngine>,
) -> String {
    use futures_util::{SinkExt, StreamExt};
    use hydeclaw_types::{ChannelInbound, ChannelOutbound, ChannelActionDto};
    use crate::agent::channel_actions::ChannelAction;
    use crate::agent::engine::ProcessingPhase;

    let mut channel_type = String::from("unknown");
    let mut channel_conn_id: Option<String> = None;
    let mut formatting_prompt: Option<String> = None;

    // Channel action receiver — starts as a dummy, replaced after Ready with a subscribed one.
    let (_dummy_tx, dummy_rx) = tokio::sync::mpsc::channel::<ChannelAction>(1);
    let mut channel_action_rx = dummy_rx;

    let (mut ws_sink, mut ws_stream) = socket.split();

    // Pending action results: action_id → oneshot sender (capped at 100)
    const MAX_PENDING_ACTIONS: usize = 100;
    #[allow(clippy::type_complexity)]
    let pending_actions: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<(), String>>>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // Outbound queue: action_id → outbound_queue UUID (for ack/fail tracking)
    let outbound_ids: Arc<tokio::sync::Mutex<HashMap<String, uuid::Uuid>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // Periodic WS ping to detect dead TCP connections (important on Pi network)
    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            // Inbound from adapter
            ws_msg = ws_stream.next() => {
                let text = match ws_msg {
                    Some(Ok(WsMessage::Text(t))) => t,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        tracing::debug!(agent = %agent_name, error = %e, "channel WS receive error");
                        break;
                    }
                };

                let inbound: ChannelInbound = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        let err = ChannelOutbound::Error {
                            request_id: String::new(),
                            message: format!("invalid message: {e}"),
                        };
                        if ws_sink.send(ws_json(&err)).await.is_err() { break; }
                        continue;
                    }
                };

                match inbound {
                    ChannelInbound::Ping => {
                        // Bump last_activity — ping proves adapter is alive
                        {
                            let mut channels = ctx.bus.connected_channels.write().await;
                            if let Some(ch) = channels.iter_mut().find(|c| c.agent_name == agent_name && c.channel_type == channel_type) {
                                ch.last_activity = chrono::Utc::now();
                            }
                        }
                        if ws_sink.send(ws_json(&ChannelOutbound::Pong)).await.is_err() { break; }
                    }
                    ChannelInbound::Ready { adapter_type: at, version, formatting_prompt: fp } => {
                        tracing::info!(agent = %agent_name, adapter = %at, version = %version, has_formatting_prompt = fp.is_some(), "adapter ready");
                        channel_type = at;
                        formatting_prompt = fp.clone();

                        // Cache formatting prompt on engine for cron/heartbeat use
                        {
                            let mut cached = engine.state().channel_formatting_prompt.write().await;
                            *cached = fp;
                        }

                        // Register in connected channels registry, resolving channel_id from DB
                        let ch_row = sqlx::query_as::<_, (sqlx::types::Uuid, String, serde_json::Value)>(
                            "SELECT id, display_name, config FROM agent_channels \
                             WHERE agent_name = $1 AND channel_type = $2 \
                             ORDER BY created_at LIMIT 1",
                        )
                        .bind(agent_name)
                        .bind(&channel_type)
                        .fetch_optional(&ctx.infra.db)
                        .await
                        .ok()
                        .flatten();

                        let (ch_id, ch_display, ch_config) = match ch_row {
                            Some((id, name, cfg)) => (Some(id), name, cfg),
                            None => (None, format!("{agent_name}/{channel_type}"), serde_json::Value::Object(Default::default())),
                        };

                        {
                            let now = chrono::Utc::now();
                            let entry = crate::gateway::state::ConnectedChannel {
                                agent_name: agent_name.to_string(),
                                channel_id: ch_id,
                                channel_type: channel_type.clone(),
                                display_name: ch_display,
                                adapter_version: version.clone(),
                                connected_at: now,
                                last_activity: now,
                            };
                            ctx.bus.connected_channels.write().await.push(entry);
                        }
                        ctx.bus.ui_event_tx.send(
                            serde_json::json!({"type": "channels_changed", "agent": agent_name}).to_string()
                        ).ok();

                        // Subscribe to channel action router (non-exclusive, multi-channel)
                        if let Some(ref router) = engine.state().channel_router {
                            let (id, rx) = router.subscribe(&channel_type).await;
                            channel_action_rx = rx;
                            channel_conn_id = Some(id);
                        }

                        // Send Config with language and owner_id for access UI.
                        // Channel secrets (bot_token, api_url) are read by the adapter from its own env.
                        let owner_id = engine.cfg().agent.access.as_ref()
                            .and_then(|a| a.owner_id.clone());

                        let typing_mode = ch_config.get("typing_mode")
                            .and_then(|v| v.as_str())
                            .unwrap_or("instant")
                            .to_string();

                        let config_msg = ChannelOutbound::Config {
                            language: engine.cfg().agent.language.clone(),
                            owner_id,
                            typing_mode,
                        };
                        if ws_sink.send(ws_json(&config_msg)).await.is_err() { break; }

                        // First-run check: if no sessions exist for this agent, trigger onboarding.
                        let session_count: i64 = sqlx::query_scalar(
                            "SELECT COUNT(*) FROM sessions WHERE agent_id = $1",
                        )
                        .bind(agent_name)
                        .fetch_one(&ctx.infra.db)
                        .await
                        .unwrap_or(0);

                        // Deliver any pending messages from previous disconnection.
                        // Re-save undelivered ones if WS fails mid-delivery.
                        match crate::db::pending::take_pending(&ctx.infra.db, agent_name).await {
                            Ok(pending) if !pending.is_empty() => {
                                tracing::info!(agent = %agent_name, count = pending.len(), "delivering pending messages after reconnect");
                                let mut failed = false;
                                for pm in pending {
                                    if failed {
                                        // WS already broken — re-save remaining messages
                                        crate::db::pending::save_pending(
                                            &ctx.infra.db, agent_name, &pm.request_id, &pm.channel, &pm.message_type, &pm.text,
                                        ).await.ok();
                                        continue;
                                    }
                                    let outbound = if pm.message_type == "done" {
                                        ChannelOutbound::Done { request_id: pm.request_id.clone(), text: pm.text.clone() }
                                    } else {
                                        ChannelOutbound::Error { request_id: pm.request_id.clone(), message: pm.text.clone() }
                                    };
                                    if ws_sink.send(ws_json(&outbound)).await.is_err() {
                                        failed = true;
                                        // Re-save this message too
                                        crate::db::pending::save_pending(
                                            &ctx.infra.db, agent_name, &pm.request_id, &pm.channel, &pm.message_type, &pm.text,
                                        ).await.ok();
                                    }
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "failed to fetch pending messages"),
                            _ => {}
                        }

                        // Replay unacked outbound queue actions for this channel.
                        match outbound::get_pending(&ctx.infra.db, &channel_type, 50).await {
                            Ok(queued) if !queued.is_empty() => {
                                tracing::info!(agent = %agent_name, channel = %channel_type, count = queued.len(), "replaying outbound queue after reconnect");
                                for (queue_id, _q_agent_id, q_action_name, q_payload) in queued {
                                    let action_id = uuid::Uuid::new_v4().to_string();
                                    let dto = hydeclaw_types::ChannelActionDto {
                                        action: q_action_name,
                                        params: q_payload.get("params").cloned().unwrap_or(serde_json::Value::Null),
                                        context: q_payload.get("context").cloned().unwrap_or(serde_json::Value::Null),
                                    };
                                    {
                                        let mut oids = outbound_ids.lock().await;
                                        if oids.len() > 1000 {
                                            oids.clear();
                                            tracing::warn!("outbound_ids overflow, cleared");
                                        }
                                        oids.insert(action_id.clone(), queue_id);
                                    }
                                    let outbound_msg = ChannelOutbound::Action { action_id: action_id.clone(), action: dto };
                                    if ws_sink.send(ws_json(&outbound_msg)).await.is_err() {
                                        tracing::warn!(agent = %agent_name, "WS send failed during outbound queue replay");
                                        break;
                                    }
                                    // Mark as sent (non-blocking)
                                    let db = ctx.infra.db.clone();
                                    tokio::spawn(async move {
                                        if let Err(e) = outbound::mark_sent(&db, queue_id).await {
                                            tracing::warn!(queue_id = %queue_id, error = %e, "outbound mark_sent failed");
                                        }
                                    });
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, "failed to fetch outbound queue"),
                            _ => {}
                        }

                        if session_count == 0 {
                            tracing::info!(agent = %agent_name, "first-run detected, scheduling onboarding");
                            let engine_clone = engine.clone();
                            let agent_name_clone = agent_name.to_string();
                            let workspace_dir = crate::config::WORKSPACE_DIR.to_string();
                            tokio::spawn(async move {
                                // Small delay to let WS loop settle before sending actions.
                                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                match crate::scheduler::run_first_run_onboarding(
                                    &engine_clone,
                                    &workspace_dir,
                                    &agent_name_clone,
                                )
                                .await
                                {
                                    Ok(()) => tracing::info!(agent = %agent_name_clone, "first-run onboarding completed"),
                                    Err(e) => tracing::error!(agent = %agent_name_clone, error = %e, "first-run onboarding failed"),
                                }
                            });
                        }
                    }
                    ChannelInbound::Message { request_id, msg } => {
                        // Bump last_activity for stale-channel detection
                        {
                            let mut channels = ctx.bus.connected_channels.write().await;
                            if let Some(ch) = channels.iter_mut().find(|c| c.agent_name == agent_name && c.channel_type == channel_type) {
                                ch.last_activity = chrono::Utc::now();
                            }
                        }
                        ctx.status.polling_diagnostics.record_inbound();
                        // Intercept approval callback buttons (approve:UUID / reject:UUID)
                        let is_callback = msg.context.get("is_callback").and_then(serde_json::Value::as_bool).unwrap_or(false);
                        if is_callback {
                            let text = msg.text.as_deref().unwrap_or("");
                            if let Some(approval_id_str) = text.strip_prefix("approve:").or_else(|| text.strip_prefix("reject:")) {
                                let approved = text.starts_with("approve:");
                                let user_id = msg.user_id.clone();
                                // Security: only the owner can resolve approvals. Re-fetch the live
                                // guard so an agent config change is reflected immediately.
                                let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
                                let is_owner = live_guard.as_ref().is_none_or(|g| g.is_owner(&user_id));
                                if !is_owner {
                                    tracing::warn!(user_id = %user_id, "non-owner attempted to resolve approval via callback");
                                    let reply = ChannelOutbound::Error {
                                        request_id: request_id.clone(),
                                        message: "Only the owner can approve or reject tool calls.".to_string(),
                                    };
                                    ws_sink.send(ws_json(&reply)).await.ok();
                                    continue;
                                }
                                if let Ok(approval_id) = uuid::Uuid::parse_str(approval_id_str) {
                                    let status = if approved { "approved" } else { "rejected" };
                                    match engine.resolve_approval(approval_id, approved, &user_id, None).await {
                                        Ok(()) => {
                                            tracing::info!(approval_id = %approval_id, status, user = %user_id, "approval resolved via Telegram callback");
                                            let reply = ChannelOutbound::Done {
                                                request_id: request_id.clone(),
                                                text: format!("{} {}", if approved { "✅ Approved" } else { "❌ Rejected" }, approval_id_str),
                                            };
                                            ws_sink.send(ws_json(&reply)).await.ok();
                                        }
                                        Err(e) => {
                                            tracing::warn!(approval_id = %approval_id, error = %e, "failed to resolve approval via callback");
                                            let reply = ChannelOutbound::Error {
                                                request_id: request_id.clone(),
                                                message: format!("Failed to resolve approval: {e}"),
                                            };
                                            ws_sink.send(ws_json(&reply)).await.ok();
                                        }
                                    }
                                }
                                continue; // Don't process as normal message
                            }
                        }

                        let incoming = msg.into_incoming(engine.cfg().agent.name.clone(), channel_type.clone(), formatting_prompt.clone());
                        let engine_clone = engine.clone();
                        let req_id = request_id.clone();

                        // Create status + chunk channels
                        let (status_tx, mut status_rx) = tokio::sync::mpsc::unbounded_channel::<ProcessingPhase>();
                        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<String>(512);

                        // Spawn engine processing with request timeout
                        let timeout_secs = ctx.cfg.config.limits.request_timeout_secs;
                        let mut engine_handle = tokio::spawn(async move {
                            let fut = engine_clone.handle_with_status(&incoming, Some(status_tx), Some(chunk_tx));
                            if timeout_secs > 0 {
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(timeout_secs), fut
                                ).await {
                                    Ok(result) => result,
                                    Err(_) => Err(anyhow::anyhow!(
                                        "Request timed out after {timeout_secs}s. The task was too complex or an external service was slow."
                                    )),
                                }
                            } else {
                                fut.await
                            }
                        });

                        // Forward chunks, phases, AND channel actions until engine completes
                        let mut cancelled = false;
                        let mut chunk_rx_closed = false;
                        let mut status_rx_closed = false;
                        let mut engine_result: Option<Result<Result<String, anyhow::Error>, tokio::task::JoinError>> = None;
                        loop {
                            tokio::select! {
                                phase = status_rx.recv(), if !status_rx_closed => {
                                    match phase {
                                        Some(phase) => {
                                            let (phase_str, tool_name) = phase.to_wire();
                                            let msg = ChannelOutbound::Phase { request_id: req_id.clone(), phase: phase_str, tool_name };
                                            if ws_sink.send(ws_json(&msg)).await.is_err() { break; }
                                        }
                                        None => { status_rx_closed = true; }
                                    }
                                }
                                chunk = chunk_rx.recv(), if !chunk_rx_closed => {
                                    match chunk {
                                        Some(text) => {
                                            let msg = ChannelOutbound::Chunk { request_id: req_id.clone(), text };
                                            if ws_sink.send(ws_json(&msg)).await.is_err() { break; }
                                        }
                                        None => {
                                            // Streaming done — but don't break: engine may still send
                                            // channel actions (approval_request) during tool execution.
                                            chunk_rx_closed = true;
                                        }
                                    }
                                }
                                // Engine completed — drain remaining channel actions before breaking
                                result = &mut engine_handle, if chunk_rx_closed && status_rx_closed => {
                                    // Process any channel actions that arrived between last select and engine completion
                                    while let Ok(action) = channel_action_rx.try_recv() {
                                        let action_id = uuid::Uuid::new_v4().to_string();
                                        let ChannelAction { name, params, context, reply, .. } = action;
                                        tracing::info!(agent = %agent_name, action = %name, action_id = %action_id, "draining channel action before engine exit");
                                        let dto = ChannelActionDto { action: name.clone(), params: params.clone(), context: context.clone() };
                                        // Enqueue to persistent outbound queue (non-blocking)
                                        let payload = serde_json::json!({"action": &name, "params": &params, "context": &context});
                                        {
                                            let db = ctx.infra.db.clone();
                                            let agent_id = agent_name.to_string();
                                            let ch = channel_type.clone();
                                            let act_name = name.clone();
                                            let aid = action_id.clone();
                                            let oids = outbound_ids.clone();
                                            tokio::spawn(async move {
                                                if let Ok(qid) = outbound::enqueue_action(&db, &agent_id, &ch, &act_name, &payload).await {
                                                    let mut oids = oids.lock().await;
                                                    if oids.len() > 1000 {
                                                        oids.clear();
                                                        tracing::warn!("outbound_ids overflow, cleared");
                                                    }
                                                    oids.insert(aid, qid);
                                                }
                                            });
                                        }
                                        {
                                            let mut pa = pending_actions.lock().await;
                                            pa.insert(action_id.clone(), reply);
                                        }
                                        let outbound_msg = ChannelOutbound::Action { action_id: action_id.clone(), action: dto };
                                        if ws_sink.send(ws_json(&outbound_msg)).await.is_ok() {
                                            // Mark sent (non-blocking) — queue_id may not be available yet from spawn
                                            let db = ctx.infra.db.clone();
                                            let oids = outbound_ids.clone();
                                            let aid = action_id;
                                            tokio::spawn(async move {
                                                // Retry with backoff: enqueue spawn may not have completed yet
                                                for _ in 0..5 {
                                                    if let Some(qid) = oids.lock().await.get(&aid).copied() {
                                                        if let Err(e) = outbound::mark_sent(&db, qid).await {
                                                            tracing::warn!(queue_id = %qid, error = %e, "outbound mark_sent failed");
                                                        }
                                                        return;
                                                    }
                                                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                                }
                                                tracing::debug!(action_id = %aid, "mark_sent: enqueue not found after retries");
                                            });
                                        }
                                    }
                                    engine_result = Some(result);
                                    break;
                                }
                                // Channel actions from engine (e.g. approval_request, send_voice)
                                action = channel_action_rx.recv() => {
                                    let action = match action {
                                        Some(a) => a,
                                        None => break,
                                    };
                                    let action_id = uuid::Uuid::new_v4().to_string();
                                    let ChannelAction { name, params, context, reply, .. } = action;
                                    tracing::info!(agent = %agent_name, action = %name, action_id = %action_id, "forwarding channel action to adapter via WS");
                                    // Enqueue to persistent outbound queue (non-blocking)
                                    let payload = serde_json::json!({"action": &name, "params": &params, "context": &context});
                                    {
                                        let db = ctx.infra.db.clone();
                                        let agent_id = agent_name.to_string();
                                        let ch = channel_type.clone();
                                        let act_name = name.clone();
                                        let aid = action_id.clone();
                                        let oids = outbound_ids.clone();
                                        tokio::spawn(async move {
                                            if let Ok(qid) = outbound::enqueue_action(&db, &agent_id, &ch, &act_name, &payload).await {
                                                let mut oids = oids.lock().await;
                                                if oids.len() > 1000 {
                                                    oids.clear();
                                                    tracing::warn!("outbound_ids overflow, cleared");
                                                }
                                                oids.insert(aid, qid);
                                            }
                                        });
                                    }
                                    let dto = ChannelActionDto { action: name, params, context };
                                    {
                                        let mut pa = pending_actions.lock().await;
                                        if pa.len() >= MAX_PENDING_ACTIONS {
                                            tracing::warn!("pending_actions cap reached, rejecting incoming action");
                                            let _ = reply.send(Err("too many pending actions".into()));
                                            continue;
                                        }
                                        pa.insert(action_id.clone(), reply);
                                    }
                                    let msg = ChannelOutbound::Action { action_id: action_id.clone(), action: dto };
                                    if ws_sink.send(ws_json(&msg)).await.is_err() { break; }
                                    // Mark sent (non-blocking)
                                    {
                                        let db = ctx.infra.db.clone();
                                        let oids = outbound_ids.clone();
                                        tokio::spawn(async move {
                                            // Retry with backoff: enqueue spawn may not have completed yet
                                            for _ in 0..5 {
                                                if let Some(qid) = oids.lock().await.get(&action_id).copied() {
                                                    if let Err(e) = outbound::mark_sent(&db, qid).await {
                                                        tracing::warn!(queue_id = %qid, error = %e, "outbound mark_sent failed");
                                                    }
                                                    return;
                                                }
                                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                            }
                                            tracing::debug!(action_id = %action_id, "mark_sent: enqueue not found after retries");
                                        });
                                    }
                                }
                                // Must also read WS for ActionResult replies and Cancel from adapter
                                ws_msg = ws_stream.next() => {
                                    match ws_msg {
                                        Some(Ok(WsMessage::Text(text))) => {
                                            match serde_json::from_str::<ChannelInbound>(&text) {
                                                Ok(ChannelInbound::ActionResult { action_id, success, error }) => {
                                                    let result = if success { Ok(()) } else { Err(error.unwrap_or_default()) };
                                                    // Update outbound queue status (non-blocking)
                                                    {
                                                        let db = ctx.infra.db.clone();
                                                        let oids = outbound_ids.clone();
                                                        let is_success = success;
                                                        let aid = action_id.clone();
                                                        tokio::spawn(async move {
                                                            if let Some(qid) = oids.lock().await.remove(&aid) {
                                                                if is_success {
                                                                    if let Err(e) = outbound::mark_acked(&db, qid).await {
                                                                        tracing::warn!(queue_id = %qid, error = %e, "outbound mark_acked failed");
                                                                    }
                                                                } else if let Err(e) = outbound::mark_failed(&db, qid).await {
                                                                    tracing::warn!(queue_id = %qid, error = %e, "outbound mark_failed failed");
                                                                }
                                                            }
                                                        });
                                                    }
                                                    if let Some(tx) = pending_actions.lock().await.remove(&action_id) {
                                                        tx.send(result).ok();
                                                    }
                                                }
                                                Ok(ChannelInbound::Cancel { request_id: cancel_id }) if cancel_id == req_id => {
                                                    tracing::info!(agent = %agent_name, request_id = %req_id, "cancelling request");
                                                    engine_handle.abort();
                                                    cancelled = true;
                                                    break;
                                                }
                                                _ => {} // Other inbound messages during processing are ignored
                                            }
                                        }
                                        Some(Ok(WsMessage::Close(_))) | None => break,
                                        _ => {}
                                    }
                                }
                            }
                        }

                        if cancelled {
                            let msg = ChannelOutbound::Error { request_id: req_id, message: "Cancelled".to_string() };
                            ws_sink.send(ws_json(&msg)).await.ok();
                            continue; // skip engine_handle.await since we aborted it
                        }

                        // Drain any remaining status messages
                        while let Ok(phase) = status_rx.try_recv() {
                            let (phase_str, tool_name) = phase.to_wire();
                            let msg = ChannelOutbound::Phase { request_id: req_id.clone(), phase: phase_str, tool_name };
                            ws_sink.send(ws_json(&msg)).await.ok();
                        }

                        // Get final result — use cached result if we got it from select, otherwise await.
                        let handle_result = match engine_result {
                            Some(r) => r,
                            None => engine_handle.await,
                        };
                        let delivery = match handle_result {
                            Ok(Ok(response)) => Some(("done", response)),
                            Ok(Err(e)) => Some(("error", e.to_string())),
                            Err(e) if e.is_cancelled() => None, // already handled above
                            Err(e) => Some(("error", format!("engine panicked: {e}"))),
                        };
                        if let Some((msg_type, text)) = delivery {
                            let outbound = if msg_type == "done" {
                                ChannelOutbound::Done { request_id: req_id.clone(), text: text.clone() }
                            } else {
                                ChannelOutbound::Error { request_id: req_id.clone(), message: text.clone() }
                            };
                            if ws_sink.send(ws_json(&outbound)).await.is_err() {
                                tracing::warn!(agent = %agent_name, msg_type, "WS send failed, saving as pending");
                                crate::db::pending::save_pending(
                                    &ctx.infra.db, agent_name, &req_id, &channel_type, msg_type, &text,
                                ).await.ok();
                            }
                        }
                        // Notify UI about session update so sidebar refreshes
                        let event = serde_json::json!({
                            "type": "session_updated",
                            "agent": agent_name,
                            "channel": channel_type,
                        });
                        ctx.bus.ui_event_tx.send(event.to_string()).ok();
                    }
                    ChannelInbound::ActionResult { action_id, success, error } => {
                        let result = if success { Ok(()) } else { Err(error.unwrap_or_default()) };
                        // Update outbound queue status (non-blocking)
                        {
                            let db = ctx.infra.db.clone();
                            let oids = outbound_ids.clone();
                            let is_success = success;
                            let aid = action_id.clone();
                            tokio::spawn(async move {
                                if let Some(qid) = oids.lock().await.remove(&aid) {
                                    if is_success {
                                        if let Err(e) = outbound::mark_acked(&db, qid).await {
                                            tracing::warn!(queue_id = %qid, error = %e, "outbound mark_acked failed");
                                        }
                                    } else if let Err(e) = outbound::mark_failed(&db, qid).await {
                                        tracing::warn!(queue_id = %qid, error = %e, "outbound mark_failed failed");
                                    }
                                }
                            });
                        }
                        if let Some(tx) = pending_actions.lock().await.remove(&action_id) {
                            tx.send(result).ok();
                        }
                    }
                    ChannelInbound::AccessCheck { request_id, user_id } => {
                        // Re-fetch guard on every check: the agent may have been restarted with a
                        // new access config after this WS session was established.
                        let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
                        let (allowed, is_owner) = if let Some(guard) = live_guard {
                            let allowed = guard.is_allowed(&user_id).await;
                            let is_owner = guard.is_owner(&user_id);
                            tracing::debug!(
                                agent = %agent_name, user_id = %user_id,
                                allowed, is_owner,
                                owner_id = ?guard.owner_id,
                                "access check"
                            );
                            (allowed, is_owner)
                        } else {
                            tracing::debug!(agent = %agent_name, user_id = %user_id, "access check: no guard, open access");
                            (true, false) // No guard means open access
                        };
                        let msg = ChannelOutbound::AccessResult { request_id, allowed, is_owner };
                        ws_sink.send(ws_json(&msg)).await.ok();
                    }
                    ChannelInbound::PairingCreate { request_id, user_id, display_name } => {
                        let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
                        let code = if let Some(guard) = live_guard {
                            let c = guard.create_pairing_code(&user_id, display_name.as_deref()).await;
                            tracing::info!(agent = %agent_name, user_id = %user_id, code = %c, "pairing code created");
                            {
                                let db = ctx.infra.db.clone();
                                let tx = ctx.bus.ui_event_tx.clone();
                                let uid = user_id.clone();
                                let dname = display_name.clone();
                                let code_val = c.clone();
                                tokio::spawn(async move {
                                    let display_label = dname.as_deref().map_or_else(|| uid.clone(), std::string::ToString::to_string);
                                    let body = format!("User {display_label} is requesting access (code: {code_val})");
                                    let data = serde_json::json!({"user_id": uid, "code": code_val, "display_name": dname});
                                    crate::gateway::handlers::notifications::notify(
                                        &db, &tx, "access_request", "Access Request", &body, data,
                                    ).await.ok();
                                });
                            }
                            c
                        } else {
                            tracing::warn!(agent = %agent_name, user_id = %user_id, "pairing create: no access guard");
                            "000000".to_string()
                        };
                        let msg = ChannelOutbound::PairingCode { request_id, code };
                        ws_sink.send(ws_json(&msg)).await.ok();
                    }
                    ChannelInbound::PairingApprove { request_id, code } => {
                        let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
                        let (success, error) = if let Some(guard) = live_guard {
                            let (ok, info) = guard.approve_pairing(&code, "owner").await;
                            // Always pass info — on success it's the display name, on failure the error reason
                            (ok, Some(info))
                        } else {
                            (false, Some("no access guard".to_string()))
                        };
                        let msg = ChannelOutbound::PairingResult { request_id, success, error };
                        ws_sink.send(ws_json(&msg)).await.ok();
                    }
                    ChannelInbound::PairingReject { request_id, code } => {
                        let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
                        if let Some(guard) = live_guard {
                            guard.reject_pairing(&code).await;
                        }
                        let msg = ChannelOutbound::PairingResult { request_id, success: true, error: None };
                        ws_sink.send(ws_json(&msg)).await.ok();
                    }
                    ChannelInbound::Cancel { request_id } => {
                        // Cancel received outside of message processing — ignore (already handled inline)
                        tracing::debug!(agent = %agent_name, request_id = %request_id, "cancel received outside message processing");
                    }
                }
            }
            // Periodic ping to keep TCP alive and detect dead connections
            _ = ping_interval.tick() => {
                if ws_sink.send(axum::extract::ws::Message::Ping(vec![1, 2, 3, 4].into())).await.is_err() {
                    tracing::warn!(agent = %agent_name, "WS ping failed — connection dead");
                    break;
                }
            }
            // Channel actions from engine → forward to adapter
            action = channel_action_rx.recv() => {
                let action = match action {
                    Some(a) => a,
                    None => break, // Engine dropped the tx
                };

                let action_id = uuid::Uuid::new_v4().to_string();

                let ChannelAction { name, params, context, reply, .. } = action;
                tracing::info!(agent = %agent_name, action = %name, action_id = %action_id, params_len = params.to_string().len(), "forwarding channel action to adapter via WS");

                // Enqueue to persistent outbound queue (non-blocking)
                let payload = serde_json::json!({"action": &name, "params": &params, "context": &context});
                {
                    let db = ctx.infra.db.clone();
                    let agent_id = agent_name.to_string();
                    let ch = channel_type.clone();
                    let act_name = name.clone();
                    let aid = action_id.clone();
                    let oids = outbound_ids.clone();
                    tokio::spawn(async move {
                        if let Ok(qid) = outbound::enqueue_action(&db, &agent_id, &ch, &act_name, &payload).await {
                            let mut oids = oids.lock().await;
                            if oids.len() > 1000 {
                                oids.clear();
                                tracing::warn!("outbound_ids overflow, cleared");
                            }
                            oids.insert(aid, qid);
                        }
                    });
                }

                let dto = ChannelActionDto { action: name, params, context };

                // Store the reply sender for when we get ActionResult back
                {
                    let mut pa = pending_actions.lock().await;
                    if pa.len() >= MAX_PENDING_ACTIONS {
                        tracing::warn!("pending_actions cap reached, rejecting incoming action");
                        let _ = reply.send(Err("too many pending actions".into()));
                        continue;
                    }
                    pa.insert(action_id.clone(), reply);
                }

                let msg = ChannelOutbound::Action { action_id: action_id.clone(), action: dto };
                let json_str = serde_json::to_string(&msg).unwrap_or_default();
                tracing::info!(agent = %agent_name, action_id = %action_id, msg_len = json_str.len(), "sending WS message to adapter");
                if ws_sink.send(WsMessage::Text(json_str.into())).await.is_err() {
                    tracing::error!(agent = %agent_name, "WS sink send failed for action");
                    break;
                }
                ctx.status.polling_diagnostics.record_outbound();
                tracing::info!(agent = %agent_name, action_id = %action_id, "WS message sent to adapter OK");
                // Mark sent (non-blocking)
                {
                    let db = ctx.infra.db.clone();
                    let oids = outbound_ids.clone();
                    let aid = action_id;
                    tokio::spawn(async move {
                        // Retry with backoff: enqueue spawn may not have completed yet
                        for _ in 0..5 {
                            if let Some(qid) = oids.lock().await.get(&aid).copied() {
                                if let Err(e) = outbound::mark_sent(&db, qid).await {
                                    tracing::warn!(queue_id = %qid, error = %e, "outbound mark_sent failed");
                                }
                                return;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                        tracing::debug!(action_id = %aid, "mark_sent: enqueue not found after retries");
                    });
                }
            }
        }
    }

    // Clear any stale pending actions
    for (_, tx) in pending_actions.lock().await.drain() {
        tx.send(Err("channel adapter disconnected".to_string())).ok();
    }

    // Unsubscribe from channel router on disconnect (by connection ID, not type)
    if let Some(router) = &engine.state().channel_router
        && let Some(conn_id) = &channel_conn_id {
            router.unsubscribe(conn_id).await;
        }

    channel_type
}

// ── WebSocket endpoint for UI ──

/// WebSocket protocol messages (UI ↔ Core).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum WsClientMessage {
    /// Send a chat message to an agent.
    #[serde(rename = "chat")]
    Chat {
        agent: Option<String>,
        text: String,
    },
    /// Ping to keep connection alive.
    #[serde(rename = "ping")]
    Ping,
    /// Subscribe to real-time log stream.
    #[serde(rename = "subscribe_logs")]
    SubscribeLogs,
    /// Unsubscribe from log stream.
    #[serde(rename = "unsubscribe_logs")]
    UnsubscribeLogs,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsServerMessage {
    /// Streaming text chunk from agent.
    #[serde(rename = "chunk")]
    Chunk { text: String },
    /// Agent response complete.
    #[serde(rename = "done")]
    Done,
    /// Error occurred.
    #[serde(rename = "error")]
    Error { message: String },
    /// Pong response.
    #[serde(rename = "pong")]
    Pong,
}

pub(crate) async fn ws_handler(
    ws: WebSocketUpgrade,
    State(agents): State<AgentCore>,
    State(bus): State<ChannelBus>,
    State(status): State<StatusMonitor>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_ws(socket, agents, bus, status))
}

async fn handle_ws(socket: WebSocket, agents: AgentCore, bus: ChannelBus, status: StatusMonitor) {
    use futures_util::{SinkExt, StreamExt};
    use tokio::sync::mpsc;

    tracing::info!("WebSocket client connected");

    let (mut ws_sink, mut ws_stream) = socket.split();
    let mut log_rx: Option<tokio::sync::broadcast::Receiver<String>> = None;
    let mut ui_event_rx = bus.ui_event_tx.subscribe();

    // Send current processing state to newly connected client
    {
        let events: Vec<String> = match status.processing_tracker.read() {
            Ok(t) => t.values().map(std::string::ToString::to_string).collect(),
            Err(_) => vec![],
        };
        if !events.is_empty() {
            tracing::info!(count = events.len(), "sending initial processing state to WS client");
        }
        for event in events {
            ws_sink.send(WsMessage::Text(event.into())).await.ok();
        }
    }

    loop {
        // Build the log future only if subscribed
        let log_fut = async {
            if let Some(ref mut rx) = log_rx {
                rx.recv().await.ok()
            } else {
                // Never resolves — effectively disabled
                std::future::pending::<Option<String>>().await
            }
        };

        tokio::select! {
            ws_msg = ws_stream.next() => {
                let ws_msg = match ws_msg {
                    Some(Ok(WsMessage::Text(text))) => text,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        tracing::debug!(error = %e, "WebSocket receive error");
                        break;
                    }
                };

                let client_msg: WsClientMessage = match serde_json::from_str(&ws_msg) {
                    Ok(m) => m,
                    Err(e) => {
                        let err = WsServerMessage::Error { message: format!("invalid message: {e}") };
                        if ws_sink.send(ws_json(&err)).await.is_err() { break; }
                        continue;
                    }
                };

                match client_msg {
                    WsClientMessage::Ping => {
                        if ws_sink.send(ws_json(&WsServerMessage::Pong)).await.is_err() { break; }
                    }
                    WsClientMessage::SubscribeLogs => {
                        log_rx = Some(bus.log_tx.subscribe());
                        tracing::debug!("WS client subscribed to logs");
                    }
                    WsClientMessage::UnsubscribeLogs => {
                        log_rx = None;
                        tracing::debug!("WS client unsubscribed from logs");
                    }
                    WsClientMessage::Chat { agent, text } => {
                        let agent_name = agent.as_deref().unwrap_or("");
                        let engine = if agent_name.is_empty() {
                            agents.first_engine().await
                        } else {
                            agents.get_engine(agent_name).await
                        };

                        let engine = if let Some(e) = engine { e } else {
                            let err = WsServerMessage::Error { message: "no agent available".to_string() };
                            ws_sink.send(ws_json(&err)).await.ok();
                            continue;
                        };

                        let incoming = hydeclaw_types::IncomingMessage {
                            user_id: crate::agent::channel_kind::channel::UI.to_string(),
                            text: Some(text),
                            attachments: vec![],
                            agent_id: engine.cfg().agent.name.clone(),
                            channel: crate::agent::channel_kind::channel::UI.to_string(),
                            context: serde_json::Value::Null,
                            timestamp: chrono::Utc::now(),
                            formatting_prompt: None,
                            tool_policy_override: None,
                            leaf_message_id: None,
                            user_message_id: None,
                        };

                        let (chunk_tx, mut chunk_rx) = mpsc::channel::<String>(512);
                        let engine_clone = engine.clone();
                        let handle = tokio::spawn(async move {
                            engine_clone.handle_streaming(&incoming, chunk_tx).await
                        });

                        while let Some(chunk) = chunk_rx.recv().await {
                            let msg = WsServerMessage::Chunk { text: chunk };
                            if ws_sink.send(ws_json(&msg)).await.is_err() { break; }
                        }

                        match handle.await {
                            Ok(Ok(_)) => {
                                ws_sink.send(ws_json(&WsServerMessage::Done)).await.ok();
                            }
                            Ok(Err(e)) => {
                                let err = WsServerMessage::Error { message: e.to_string() };
                                ws_sink.send(ws_json(&err)).await.ok();
                            }
                            Err(e) => {
                                let err = WsServerMessage::Error { message: format!("engine panicked: {e}") };
                                ws_sink.send(ws_json(&err)).await.ok();
                            }
                        }
                    }
                }
            }
            log_line = log_fut => {
                if let Some(line) = log_line
                    && ws_sink.send(WsMessage::Text(line.into())).await.is_err() { break; }
            }
            event = ui_event_rx.recv() => {
                match event {
                    Ok(event_json) => {
                        if ws_sink.send(WsMessage::Text(event_json.into())).await.is_err() { break; }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(lagged = n, "UI WS event receiver lagged, skipping old events");
                        // Continue — next recv() will get the latest events
                    }
                    Err(_) => break, // Channel closed
                }
            }
        }
    }

    tracing::info!("WebSocket client disconnected");
}
