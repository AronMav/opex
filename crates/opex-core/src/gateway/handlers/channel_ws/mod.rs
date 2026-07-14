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
use crate::gateway::clusters::{AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor};

mod handshake;
mod inline;
mod reader;
mod session_queue;
mod types;
mod writer;

use types::CwsCtx;

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
///
/// Architecture (post-2026-05-06): three concurrent tasks per WS connection:
///   - **Reader** ([`reader::run`]) parses inbound, routes via `OutboundMsg`,
///     dispatcher, or inline handlers. Never awaits engine work, so
///     `ChannelInbound::Message` is never silently dropped during a busy
///     loop iteration (the bug the actor split was built to fix).
///   - **Writer** ([`writer::run`]) owns the WS sink and drains
///     `mpsc<OutboundMsg>` produced by reader / dispatcher / inline /
///     action-forwarder.
///   - **Action forwarder** receives engine-emitted `ChannelAction`s once
///     handshake hands off the receiver via a oneshot.
///
/// Per-`SessionKey` mutex serialises dispatch within one logical session
/// (FIFO per user/chat) while different sessions run concurrently.
async fn channel_ws_loop(
    socket: WebSocket,
    ctx: &CwsCtx,
    agent_name: &str,
    engine: &Arc<crate::agent::engine::AgentEngine>,
) -> String {
    use futures_util::StreamExt;
    use tokio::sync::Mutex;
    use tokio::sync::oneshot;

    let (ws_out, ws_in) = socket.split();

    let (out_tx, out_rx) = tokio::sync::mpsc::channel::<types::OutboundMsg>(256);
    let queue_map = session_queue::SessionQueueMap::new();
    let inflight: types::InflightRegistry = Arc::new(Mutex::new(HashMap::new()));
    let pending_actions: types::PendingActionsMap = Arc::new(Mutex::new(HashMap::new()));
    let outbound_ids: Arc<Mutex<HashMap<String, uuid::Uuid>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // One-shot delivery: handshake sends (channel_type, channel_action_rx)
    // exactly once after the adapter says Ready. The action forwarder waits
    // on this before draining engine actions to the writer.
    let (action_install_tx, action_install_rx) =
        oneshot::channel::<handshake::ActionForwarderInit>();

    // Writer task — single owner of ws_out.
    let writer_handle = tokio::spawn(writer::run(ws_out, out_rx));

    // Action forwarder task — drains channel_action_rx (after Ready) and
    // forwards every action to the writer. Tagged by channel_type for the
    // outbound queue persistence.
    let action_forwarder = {
        let action_out = out_tx.clone();
        let action_oids = outbound_ids.clone();
        let action_pending = pending_actions.clone();
        let action_db = ctx.infra.db.clone();
        let action_status = ctx.status.clone();
        let action_agent = agent_name.to_string();
        const MAX_PENDING_ACTIONS: usize = 100;
        tokio::spawn(async move {
            let init = match action_install_rx.await {
                Ok(v) => v,
                Err(_) => return, // sender dropped before handshake — quiet exit
            };
            let channel_type = init.channel_type;
            let mut rx = init.channel_action_rx;
            while let Some(action) = rx.recv().await {
                let action_id = uuid::Uuid::new_v4().to_string();
                let crate::agent::channel_actions::ChannelAction {
                    name, params, context, reply, ..
                } = action;
                tracing::info!(
                    agent = %action_agent, action = %name, %action_id,
                    "forwarding channel action",
                );
                let payload =
                    serde_json::json!({"action": &name, "params": &params, "context": &context});
                // AUDIT-FF-006 (post-2026-05-08): the previous code spawned
                // enqueue_action and the (action_id → qid) insertion as a
                // detached task, then immediately sent the outbound frame and
                // started a 5-attempt poll for mark_sent. If the adapter's
                // ActionResult raced ahead of that detached enqueue (slow DB,
                // burst), the reader's `outbound_ids.remove(action_id)` saw
                // nothing and never called `mark_acked`; the queue row leaked
                // and the next reconnect replayed an already-delivered
                // action. Insert (action_id → qid) BEFORE sending the wire
                // frame so the reader's ack path always observes it.
                let queue_id = match crate::db::outbound::enqueue_action(
                    &action_db, &action_agent, &channel_type, &name, &payload,
                )
                .await
                {
                    Ok(qid) => {
                        let mut g = action_oids.lock().await;
                        if g.len() > 1000 {
                            g.clear();
                            tracing::warn!("outbound_ids overflow, cleared");
                        }
                        g.insert(action_id.clone(), qid);
                        Some(qid)
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, action_id = %action_id, "enqueue_action failed; continuing without queue tracking");
                        None
                    }
                };
                let track_pending = {
                    let mut pa = action_pending.lock().await;
                    if pa.len() >= MAX_PENDING_ACTIONS {
                        tracing::warn!(
                            %action_id, pending = pa.len(), cap = MAX_PENDING_ACTIONS,
                            "pending_actions cap reached; result tracking degraded to fire-and-forget",
                        );
                        let _ = reply.send(Ok(()));
                        false
                    } else {
                        pa.insert(action_id.clone(), reply);
                        true
                    }
                };
                let _ = track_pending;

                let dto = opex_types::ChannelActionDto {
                    action: name,
                    params,
                    context,
                };
                let frame = opex_types::ChannelOutbound::Action {
                    action_id: action_id.clone(),
                    action: dto,
                };
                if action_out
                    .send(types::OutboundMsg::Wire(frame))
                    .await
                    .is_err()
                {
                    break;
                }
                action_status.polling_diagnostics.record_outbound();

                // AUDIT-FF-007: mark_sent now uses the queue_id captured
                // synchronously above. No polling required.
                if let Some(qid) = queue_id {
                    let db = action_db.clone();
                    tokio::spawn(async move {
                        if let Err(e) = crate::db::outbound::mark_sent(&db, qid).await {
                            tracing::warn!(queue_id = %qid, error = %e, "outbound mark_sent failed");
                        }
                    });
                }
            }
        })
    };

    // Reader runs the long-lived loop until the WS closes or errors.
    let state = reader::run(
        ws_in,
        ctx.clone(),
        engine.clone(),
        agent_name.to_string(),
        out_tx.clone(),
        queue_map,
        inflight.clone(),
        pending_actions,
        outbound_ids,
        Some(action_install_tx),
    )
    .await;

    // Tear down: stop action_forwarder FIRST so its clone of `out_tx` is
    // released. Otherwise, on an unclean shutdown where channel_action_rx is
    // still open (e.g. WS error before engine drops its end), the writer
    // would wait forever for all senders to drop while action_forwarder
    // sits idle on `rx.recv().await` holding a clone alive.
    action_forwarder.abort();
    let _ = action_forwarder.await; // ensure the clone is actually dropped
    drop(out_tx);
    let _ = writer_handle.await;

    // Tear down in-flight turns COOPERATIVELY (R-CHANNEL). The queue map was
    // dropped when the reader returned, so consumers are already draining to
    // exit; we only need to interrupt RUNNING turns promptly (cancel token →
    // execute() returns Interrupted → finalize marks 'interrupted') and, after a
    // bounded grace, hard-abort any sync-wedged turn via its stored AbortHandle.
    {
        let drained: Vec<_> = {
            let mut g = inflight.lock().await;
            g.drain().map(|(_, im)| im).collect()
        };
        if !drained.is_empty() {
            for im in &drained {
                im.cancel.cancel();
            }
            // Poll for cooperative wind-down; break as soon as every running
            // turn (those with an attached AbortHandle) has finished, instead of
            // always blocking the full grace. Queued turns have abort=None and
            // were already neutralized by draining `inflight`.
            let grace = std::time::Duration::from_secs(15);
            let poll = std::time::Duration::from_millis(50);
            let mut waited = std::time::Duration::ZERO;
            while waited < grace {
                if drained
                    .iter()
                    .all(|im| im.abort.as_ref().is_none_or(|a| a.is_finished()))
                {
                    break;
                }
                tokio::time::sleep(poll).await;
                waited += poll;
            }
            // Hard-abort any straggler that ignored the cooperative token.
            for im in &drained {
                if let Some(abort) = &im.abort
                    && !abort.is_finished()
                {
                    abort.abort();
                }
            }
        }
    }

    // Unsubscribe from channel_router on disconnect.
    if let Some(router) = &engine.state().channel_router
        && let Some(conn_id) = &state.channel_conn_id
    {
        router.unsubscribe(conn_id).await;
    }

    state.channel_type
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

                        let incoming = opex_types::IncomingMessage {
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
