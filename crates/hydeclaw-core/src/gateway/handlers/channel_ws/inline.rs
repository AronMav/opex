//! Inline (non-`Message`, non-`ActionResult`, non-`Cancel`) inbound handlers.
//! Each function processes one `ChannelInbound` variant and emits any
//! response via the shared `OutboundMsg` channel — no engine work, so they
//! never block the reader.

use std::sync::Arc;
use tokio::sync::mpsc;

use hydeclaw_types::{ChannelOutbound, IncomingMessageDto};

use super::types::{CwsCtx, OutboundMsg};
use crate::agent::engine::AgentEngine;

/// Reply to `ChannelInbound::Ping`. Bumps `last_activity` for stale-channel
/// detection and emits a `Pong`.
pub(super) async fn handle_ping(
    ctx: &CwsCtx,
    agent_name: &str,
    channel_type: &str,
    out_tx: &mpsc::Sender<OutboundMsg>,
) {
    {
        let mut channels = ctx.bus.connected_channels.write().await;
        if let Some(ch) = channels
            .iter_mut()
            .find(|c| c.agent_name == agent_name && c.channel_type == channel_type)
        {
            ch.last_activity = chrono::Utc::now();
        }
    }
    let _ = out_tx.send(OutboundMsg::Wire(ChannelOutbound::Pong)).await;
}

/// Look up the live access guard and reply with `AccessResult`. The guard
/// is re-fetched on every check so an agent restart with a new access
/// config takes effect immediately for already-connected adapters.
pub(super) async fn handle_access_check(
    ctx: &CwsCtx,
    agent_name: &str,
    request_id: String,
    user_id: String,
    out_tx: &mpsc::Sender<OutboundMsg>,
) {
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let (allowed, is_owner) = if let Some(guard) = live_guard {
        let allowed = guard.is_allowed(&user_id).await;
        let is_owner = guard.is_owner(&user_id);
        tracing::debug!(
            %agent_name, %user_id,
            allowed, is_owner,
            owner_id = ?guard.owner_id,
            "access check"
        );
        (allowed, is_owner)
    } else {
        tracing::debug!(%agent_name, %user_id, "access check: no guard, open access");
        (true, false)
    };
    let _ = out_tx
        .send(OutboundMsg::Wire(ChannelOutbound::AccessResult {
            request_id,
            allowed,
            is_owner,
        }))
        .await;
}

/// Generate a pairing code for an unauthorized user. Notifies the UI via
/// `notifications::notify` so the owner can approve via web.
pub(super) async fn handle_pairing_create(
    ctx: &CwsCtx,
    agent_name: &str,
    request_id: String,
    user_id: String,
    display_name: Option<String>,
    out_tx: &mpsc::Sender<OutboundMsg>,
) {
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let code = if let Some(guard) = live_guard {
        let c = guard.create_pairing_code(&user_id, display_name.as_deref()).await;
        tracing::info!(%agent_name, %user_id, code = %c, "pairing code created");
        {
            let db = ctx.infra.db.clone();
            let tx = ctx.bus.ui_event_tx.clone();
            let uid = user_id.clone();
            let dname = display_name.clone();
            let code_val = c.clone();
            // AUDIT-FF-003: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
            tokio::spawn(async move {
                let display_label = dname.as_deref().map_or_else(
                    || uid.clone(),
                    std::string::ToString::to_string,
                );
                let body = format!("User {display_label} is requesting access (code: {code_val})");
                let data = serde_json::json!({"user_id": uid, "code": code_val, "display_name": dname});
                crate::gateway::handlers::notifications::notify(
                    &db, &tx, "access_request", "Access Request", &body, data,
                ).await.ok();
            });
        }
        c
    } else {
        tracing::warn!(%agent_name, %user_id, "pairing create: no access guard");
        "000000".to_string()
    };
    let _ = out_tx
        .send(OutboundMsg::Wire(ChannelOutbound::PairingCode { request_id, code }))
        .await;
}

/// Approve a pairing code. On success, `info` carries the display name; on
/// failure it carries the rejection reason (the protocol uses one field
/// for both — see `AccessGuard::approve_pairing`).
pub(super) async fn handle_pairing_approve(
    ctx: &CwsCtx,
    agent_name: &str,
    request_id: String,
    code: String,
    out_tx: &mpsc::Sender<OutboundMsg>,
) {
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let (success, error) = if let Some(guard) = live_guard {
        let (ok, info) = guard.approve_pairing(&code, "owner").await;
        (ok, Some(info))
    } else {
        (false, Some("no access guard".to_string()))
    };
    let _ = out_tx
        .send(OutboundMsg::Wire(ChannelOutbound::PairingResult {
            request_id,
            success,
            error,
        }))
        .await;
}

/// Reject a pairing code (always succeeds even if guard absent).
pub(super) async fn handle_pairing_reject(
    ctx: &CwsCtx,
    agent_name: &str,
    request_id: String,
    code: String,
    out_tx: &mpsc::Sender<OutboundMsg>,
) {
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    if let Some(guard) = live_guard {
        guard.reject_pairing(&code).await;
    }
    let _ = out_tx
        .send(OutboundMsg::Wire(ChannelOutbound::PairingResult {
            request_id,
            success: true,
            error: None,
        }))
        .await;
}

/// Intercept Telegram inline-button approval callbacks (`approve:UUID` /
/// `reject:UUID`). Returns `true` when the message was a callback and was
/// consumed (caller should `continue`), `false` if the message is a normal
/// chat message and should fall through to the dispatcher.
///
/// Only the agent's owner is allowed to resolve approvals — non-owner
/// callbacks receive an error frame and are also consumed.
pub(super) async fn handle_approval_callback(
    ctx: &CwsCtx,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    request_id: &str,
    msg: &IncomingMessageDto,
    out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    let is_callback = msg
        .context
        .get("is_callback")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !is_callback {
        return false;
    }

    let text = msg.text.as_deref().unwrap_or("");
    let approval_id_str = match text
        .strip_prefix("approve:")
        .or_else(|| text.strip_prefix("reject:"))
    {
        Some(s) => s,
        None => return false, // callback flag was set but format unfamiliar — let dispatcher try
    };
    let approved = text.starts_with("approve:");
    let user_id = msg.user_id.clone();

    // Security: only the owner can resolve approvals. Re-fetch live guard.
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let is_owner = live_guard.as_ref().is_some_and(|g| g.is_owner(&user_id));
    if !is_owner {
        tracing::warn!(%user_id, "non-owner attempted to resolve approval via callback");
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Error {
                request_id: request_id.to_string(),
                message: "Only the owner can approve or reject tool calls.".to_string(),
            }))
            .await;
        return true;
    }

    let approval_id: hydeclaw_types::ids::ApprovalId =
        match approval_id_str.parse() {
            Ok(id) => id,
            Err(_) => {
                // Malformed UUID — consume callback but don't error noisily.
                return true;
            }
        };

    let status = if approved { "approved" } else { "rejected" };
    match engine.resolve_approval(approval_id, approved, &user_id, None).await {
        Ok(()) => {
            tracing::info!(%approval_id, status, %user_id, "approval resolved via Telegram callback");
            let _ = out_tx
                .send(OutboundMsg::Wire(ChannelOutbound::Done {
                    request_id: request_id.to_string(),
                    text: format!(
                        "{} {}",
                        if approved { "✅ Approved" } else { "❌ Rejected" },
                        approval_id_str
                    ),
                }))
                .await;
        }
        Err(e) => {
            tracing::warn!(%approval_id, error = %e, "failed to resolve approval via callback");
            let _ = out_tx
                .send(OutboundMsg::Wire(ChannelOutbound::Error {
                    request_id: request_id.to_string(),
                    message: format!("Failed to resolve approval: {e}"),
                }))
                .await;
        }
    }
    true
}
