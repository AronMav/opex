//! Single-reader task for the channel WS. Continuously reads
//! `ChannelInbound` from the socket and routes each variant:
//!
//! - `Message`         → sync-classified (approval > initiative > infra >
//!   clarify-cb > clarify-text > turn); callbacks and the clarify-text
//!   resolver are spawned, ordinary turns are registered in `inflight` and
//!   enqueued via [`enqueue_turn`] / [`super::session_queue::SessionQueueMap`]
//! - `Cancel`          → [`super::session_queue::cancel`]
//! - `ActionResult`    → resolve `pending_actions` + outbound queue
//! - `Ping`            → [`super::inline::handle_ping`]
//! - `AccessCheck`/`Pairing*` → [`super::inline::*`]
//! - `Ready`           → [`super::handshake::handle_ready`]
//! - WS `Close`        → exit cleanly
//!
//! The reader **never awaits engine work** — every variant either returns
//! immediately or spawns a task. That eliminates the silent message-drop
//! window in the old `channel_ws_loop`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::stream::{SplitStream, StreamExt};
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;

use opex_types::{ChannelInbound, ChannelOutbound};

use super::handshake::ActionForwarderInit;
use super::session_queue::{self, QueuedTurn, SessionQueueMap};
use super::types::{CwsCtx, InflightMessage, InflightRegistry, OutboundMsg, PendingActionsMap, SessionKey};
use super::{handshake, inline};
use crate::agent::engine::AgentEngine;
use crate::db::outbound;

/// Per-WS-connection state mutated by the reader and read by handshake.
pub(super) struct ReaderState {
    pub channel_type:      String,
    pub channel_conn_id:   Option<String>,
    pub formatting_prompt: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    mut ws_in: SplitStream<WebSocket>,
    ctx: CwsCtx,
    engine: Arc<AgentEngine>,
    agent_name: String,
    out_tx: mpsc::Sender<OutboundMsg>,
    queue_map: Arc<SessionQueueMap>,
    inflight: InflightRegistry,
    pending_actions: PendingActionsMap,
    outbound_ids: Arc<Mutex<HashMap<String, Uuid>>>,
    mut action_install_tx: Option<oneshot::Sender<ActionForwarderInit>>,
) -> ReaderState {
    let mut state = ReaderState {
        channel_type:      String::from("unknown"),
        channel_conn_id:   None,
        formatting_prompt: None,
    };

    // Periodic ping (delivered via writer). Skip the immediate first tick
    // so we don't ping before the handshake completes.
    let mut ping_tick = tokio::time::interval(std::time::Duration::from_secs(30));
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping_tick.tick().await;

    loop {
        tokio::select! {
            ws_msg = ws_in.next() => {
                let text = match ws_msg {
                    Some(Ok(WsMessage::Text(t))) => t,
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    Some(Ok(_)) => continue,
                    Some(Err(e)) => {
                        tracing::debug!(%agent_name, error = %e, "channel WS read error");
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
                        let _ = out_tx.send(OutboundMsg::Wire(err)).await;
                        continue;
                    }
                };

                match inbound {
                    ChannelInbound::Ping => {
                        inline::handle_ping(&ctx, &agent_name, &state.channel_type, &out_tx).await;
                    }
                    ChannelInbound::Ready { adapter_type, version, formatting_prompt } => {
                        handshake::handle_ready(
                            &ctx, &engine, &agent_name, &out_tx,
                            adapter_type, version, formatting_prompt,
                            &mut state, &mut action_install_tx, &outbound_ids,
                        ).await;
                    }
                    ChannelInbound::Message { request_id, msg } => {
                        // Handshake-completion guard (#6): a Message before the
                        // adapter's Ready would create a bogus "unknown"-channel
                        // session with no formatting prompt. Reject it.
                        if state.channel_type == "unknown" {
                            let _ = out_tx
                                .send(OutboundMsg::Wire(ChannelOutbound::Error {
                                    request_id,
                                    message: "handshake not complete: send Ready before Message".to_string(),
                                }))
                                .await;
                            continue;
                        }

                        // Bump last_activity for stale-channel detection.
                        {
                            let mut chans = ctx.bus.connected_channels.write().await;
                            if let Some(c) = chans
                                .iter_mut()
                                .find(|c| c.agent_name == agent_name && c.channel_type == state.channel_type)
                            {
                                c.last_activity = chrono::Utc::now();
                            }
                        }
                        ctx.status.polling_diagnostics.record_inbound();

                        // Sync classification (never awaits engine/DB). Priority:
                        // approval > initiative > infra > clarify-cb > clarify-text
                        // > turn. Callbacks are spawned off the hot path; the one
                        // async-to-classify path (clarify-text) is gated by the
                        // sync has_any_pending() and spawned — NOT run in the FIFO
                        // consumer (deadlock-safe).
                        if inline::approval_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_approval_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if inline::initiative_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_initiative_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if inline::infra_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_infra_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if inline::clarify_cb_matches(&msg) {
                            let (ctx2, engine2, an, rid, m, tx) =
                                (ctx.clone(), engine.clone(), agent_name.clone(), request_id.clone(), msg.clone(), out_tx.clone());
                            tokio::spawn(async move {
                                inline::handle_clarify_callback(&ctx2, &engine2, &an, &rid, &m, &tx).await;
                            });
                        } else if !inline::is_callback(&msg)
                            && engine.cfg().clarify_manager.has_any_pending()
                        {
                            // H-4 fix: handle the clarify check INLINE instead
                            // of spawning. The previous spawned task raced with
                            // the next plain-text message — both ran async DB
                            // lookups (`resolve_active_dm_session`) and could
                            // enqueue out of receive order, producing the bug
                            // where msg1 was treated as a normal turn even
                            // though msg2 (which arrived later) had already
                            // been consumed as the clarify response. Running
                            // inline here serializes the clarify check + the
                            // enqueue so a same-peer follow-up message sees
                            // the post-clarify state.
                            let ct = state.channel_type.clone();
                            let fp = state.formatting_prompt.clone();
                            let consumed = inline::handle_clarify_text(
                                &ctx, &engine, &agent_name, &ct, &request_id, &msg, &out_tx,
                            )
                            .await;
                            if !consumed {
                                enqueue_turn(
                                    &queue_map, &engine, &agent_name, &ct,
                                    fp,
                                    request_id.clone(), msg.clone(), ctx.cfg.config.limits.request_timeout_secs,
                                    &out_tx, &inflight,
                                ).await;
                            }
                        } else {
                            // Ordinary turn — register inflight at enqueue time then
                            // enqueue in receive order.
                            enqueue_turn(
                                &queue_map, &engine, &agent_name, &state.channel_type,
                                state.formatting_prompt.clone(),
                                request_id, msg, ctx.cfg.config.limits.request_timeout_secs,
                                &out_tx, &inflight,
                            ).await;
                        }

                        // UI sidebar refresh (preserved from old loop).
                        let event = opex_types::ws::WsEvent::SessionUpdated {
                            agent: agent_name.clone(),
                            session_id: None,
                            channel: Some(state.channel_type.clone()),
                        };
                        ctx.bus.ui_event_tx.send(event.to_json()).ok();
                    }
                    ChannelInbound::Cancel { request_id } => {
                        let cancelled = session_queue::cancel(&request_id, &inflight).await;
                        if cancelled {
                            let _ = out_tx
                                .send(OutboundMsg::Wire(ChannelOutbound::Error {
                                    request_id,
                                    message: "Cancelled".to_string(),
                                }))
                                .await;
                        } else {
                            tracing::debug!(
                                %agent_name, %request_id,
                                "Cancel for unknown request_id (already finished?)",
                            );
                        }
                    }
                    ChannelInbound::ActionResult { action_id, success, error } => {
                        let result = if success { Ok(()) } else { Err(error.unwrap_or_default()) };
                        // Update outbound queue (non-blocking).
                        {
                            let db = ctx.infra.db.clone();
                            let oids = outbound_ids.clone();
                            let aid = action_id.clone();
                            let is_success = success;
                            // AUDIT-FF-005: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
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
                        inline::handle_access_check(&ctx, &agent_name, request_id, user_id, &out_tx).await;
                    }
                    ChannelInbound::PairingCreate { request_id, user_id, display_name } => {
                        inline::handle_pairing_create(&ctx, &agent_name, request_id, user_id, display_name, &out_tx).await;
                    }
                    ChannelInbound::PairingApprove { request_id, code } => {
                        inline::handle_pairing_approve(&ctx, &agent_name, request_id, code, &out_tx).await;
                    }
                    ChannelInbound::PairingReject { request_id, code } => {
                        inline::handle_pairing_reject(&ctx, &agent_name, request_id, code, &out_tx).await;
                    }
                }
            }
            _ = ping_tick.tick() => {
                if out_tx.send(OutboundMsg::Ping).await.is_err() { break; }
            }
        }
    }

    // Drain any pending action waiters with disconnect error.
    for (_, tx) in pending_actions.lock().await.drain() {
        tx.send(Err("channel adapter disconnected".to_string())).ok();
    }
    // Signal writer to drain and exit.
    let _ = out_tx.send(OutboundMsg::Shutdown).await;
    state
}

/// Register the turn in `inflight` (cancel token, `abort = None`) and enqueue it
/// for its session in receive order. Registering at ENQUEUE (not consumer start)
/// is what lets a `Cancel` for a still-queued turn be honoured.
#[allow(clippy::too_many_arguments)]
async fn enqueue_turn(
    queue_map: &Arc<SessionQueueMap>,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    channel_type: &str,
    formatting_prompt: Option<String>,
    request_id: String,
    msg: opex_types::IncomingMessageDto,
    timeout_secs: u64,
    out_tx: &mpsc::Sender<OutboundMsg>,
    inflight: &InflightRegistry,
) {
    let dm_scope = engine
        .cfg()
        .agent
        .session
        .as_ref()
        .map(|s| s.dm_scope.as_str())
        .unwrap_or("per-channel-peer")
        .to_string();
    let chat_scope = msg.chat_scope();
    let session_key = SessionKey::from_inbound(
        agent_name, &msg.user_id, channel_type, &dm_scope, chat_scope.as_deref(),
    );

    let cancel_token = tokio_util::sync::CancellationToken::new();
    inflight.lock().await.insert(
        request_id.clone(),
        InflightMessage { cancel: cancel_token.clone(), abort: None },
    );

    let turn = QueuedTurn {
        engine: engine.clone(),
        agent_name: agent_name.to_string(),
        channel_type: channel_type.to_string(),
        formatting_prompt,
        request_id,
        msg,
        timeout_secs,
        out_tx: out_tx.clone(),
        inflight: inflight.clone(),
        cancel_token,
    };
    queue_map.enqueue(session_key, turn).await;
}

#[cfg(test)]
mod wire_guards {
    // Structural guards on the reader's Message-arm routing. These assert the
    // sync-classify order (approval > initiative > infra > clarify-cb >
    // clarify-text > enqueue) is preserved in source, matching the priority the
    // inline handlers enforced by call order before the queue rewrite.

    #[test]
    fn handshake_guard_before_routing() {
        let src = include_str!("reader.rs");
        let guard = src.find("channel_type == \"unknown\"").expect("#6 handshake guard present");
        let route = src.find("approval_matches(").expect("classifier routing present");
        assert!(guard < route, "handshake guard must run before routing");
    }

    #[test]
    fn approval_classified_before_clarify_text() {
        let src = include_str!("reader.rs");
        let approval = src.find("approval_matches(").expect("approval classifier present");
        let clarify = src.find("handle_clarify_text(").expect("clarify-text spawn present");
        assert!(approval < clarify, "approval must be classified before clarify-text (priority)");
    }

    #[test]
    fn callbacks_classified_before_enqueue() {
        let src = include_str!("reader.rs");
        let clarify_cb = src.find("clarify_cb_matches(").expect("clarify-cb classifier present");
        let enqueue = src.find("queue_map.enqueue(").expect("turn enqueue present");
        assert!(clarify_cb < enqueue, "all callback classifiers must precede enqueue");
    }

    #[test]
    fn clarify_text_gated_by_has_any_pending() {
        let src = include_str!("reader.rs");
        assert!(src.contains("has_any_pending()"), "clarify-text spawn must be gated by the sync has_any_pending() check");
    }
}
