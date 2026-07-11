//! Single-reader task for the channel WS. Continuously reads
//! `ChannelInbound` from the socket and routes each variant:
//!
//! - `Message`         → [`super::dispatcher::dispatch_message`]
//! - `Cancel`          → [`super::dispatcher::cancel`]
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
use super::session_locks::SessionLockMap;
use super::types::{CwsCtx, InflightRegistry, OutboundMsg, PendingActionsMap};
use super::{dispatcher, handshake, inline};
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
    lock_map: Arc<SessionLockMap>,
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

                        // Approval-callback intercept.
                        let consumed = inline::handle_approval_callback(
                            &ctx, &engine, &agent_name, &request_id, &msg, &out_tx,
                        ).await;
                        if consumed { continue; }

                        // Initiative callback intercept (approve/dismiss proposal,
                        // cancel goal — owner-gated, `iappr:`/`idismiss:`/`icancel:`).
                        let consumed_initiative = inline::handle_initiative_callback(
                            &ctx, &engine, &agent_name, &request_id, &msg, &out_tx,
                        ).await;
                        if consumed_initiative { continue; }

                        // Clarify button-callback intercept (owner-gated).
                        let consumed_clarify_cb = inline::handle_clarify_callback(
                            &ctx, &engine, &agent_name, &request_id, &msg, &out_tx,
                        ).await;
                        if consumed_clarify_cb { continue; }

                        // Clarify text-intercept (open-ended / «Other»).
                        // Priority: approval > clarify (checked inside).
                        let consumed_clarify_text = inline::handle_clarify_text(
                            &ctx, &engine, &agent_name, &state.channel_type,
                            &request_id, &msg, &out_tx,
                        ).await;
                        if consumed_clarify_text { continue; }

                        dispatcher::dispatch_message(
                            engine.clone(),
                            agent_name.clone(),
                            state.channel_type.clone(),
                            state.formatting_prompt.clone(),
                            request_id,
                            msg,
                            ctx.cfg.config.limits.request_timeout_secs,
                            out_tx.clone(),
                            lock_map.clone(),
                            inflight.clone(),
                        ).await;

                        // UI sidebar refresh (preserved from old loop).
                        let event = serde_json::json!({
                            "type": "session_updated",
                            "agent": agent_name,
                            "channel": state.channel_type,
                        });
                        ctx.bus.ui_event_tx.send(event.to_string()).ok();
                    }
                    ChannelInbound::Cancel { request_id } => {
                        let cancelled = dispatcher::cancel(&request_id, &inflight).await;
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

#[cfg(test)]
mod wire_guards {
    #[test]
    fn clarify_callback_wired_before_dispatch() {
        let src = include_str!("reader.rs");
        let cb = src.find("handle_clarify_callback").expect("clarify callback intercept must be wired");
        let dispatch = src.find("dispatcher::dispatch_message(").expect("dispatcher present");
        assert!(cb < dispatch, "clarify callback intercept must run before dispatch_message");
    }

    #[test]
    fn clarify_text_wired_before_dispatch() {
        let src = include_str!("reader.rs");
        let txt = src.find("handle_clarify_text").expect("clarify text-intercept must be wired");
        let dispatch = src.find("dispatcher::dispatch_message(").expect("dispatcher present");
        assert!(txt < dispatch, "clarify text-intercept must run before dispatch_message");
    }

    #[test]
    fn approval_wired_before_clarify() {
        // Approval must be wired BEFORE clarify to enforce priority.
        let src = include_str!("reader.rs");
        let approval = src.find("handle_approval_callback").expect("approval intercept present");
        let clarify_txt = src.find("handle_clarify_text").expect("clarify text-intercept present");
        assert!(approval < clarify_txt, "approval intercept must be wired before clarify text-intercept");
    }

    #[test]
    fn initiative_callback_wired_before_dispatch() {
        let src = include_str!("reader.rs");
        let cb = src.find("handle_initiative_callback").expect("initiative callback intercept must be wired");
        let dispatch = src.find("dispatcher::dispatch_message(").expect("dispatcher present");
        assert!(cb < dispatch, "initiative callback intercept must run before dispatch_message");
    }
}
