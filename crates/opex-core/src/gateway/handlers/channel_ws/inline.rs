//! Inline (non-`Message`, non-`ActionResult`, non-`Cancel`) inbound handlers.
//! Each function processes one `ChannelInbound` variant and emits any
//! response via the shared `OutboundMsg` channel — no engine work, so they
//! never block the reader.

use std::sync::Arc;
use tokio::sync::mpsc;

use opex_types::{ChannelOutbound, IncomingMessageDto};

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
        // Defense in depth: a guard is now always registered for every live
        // agent (see agents::lifecycle), so this branch is only reached in an
        // anomalous state (agent not fully started). Fail closed — deny access
        // rather than silently granting world-open access.
        tracing::warn!(%agent_name, %user_id, "access check: no guard, denying (fail-closed)");
        (false, false)
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

// ── Approval callback ─────────────────────────────────────────────────────────

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
    if !approval_matches(msg) {
        return false;
    }
    let text = msg.text.as_deref().unwrap_or("");
    let approval_id_str = match text
        .strip_prefix("approve:")
        .or_else(|| text.strip_prefix("reject:"))
    {
        Some(s) => s,
        None => {
            tracing::warn!(text = %text, "approval callback matched but had no known prefix");
            return false;
        }
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

    let approval_id: opex_types::ids::ApprovalId =
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

// ── Initiative callback ───────────────────────────────────────────────────────

/// Render a [`ProposalError`](crate::gateway::handlers::agents::initiative::ProposalError)
/// as a human-readable message for the Telegram error frame / log line.
/// `ProposalError` derives `Debug` (for sqlx-test `.unwrap()`) but no `Display`
/// impl, so this is the single place that knows how to unwrap its two variants.
fn describe_proposal_error(e: crate::gateway::handlers::agents::initiative::ProposalError) -> String {
    use crate::gateway::handlers::agents::initiative::ProposalError;
    match e {
        ProposalError::BaseAgent => "initiative is non-base only".to_string(),
        ProposalError::Db(msg) => msg,
    }
}

/// Fire-and-forget delivery of a "⏹ Отменить" inline button for a
/// newly-spawned initiative goal session, sent to the agent owner's channel.
/// Fail-soft: no channel router, no connected adapter, or no resolvable
/// owner target are all silently skipped — the goal already spawned
/// successfully via `approve_proposal`, this is best-effort UX only.
async fn send_cancel_button(ctx: &CwsCtx, engine: &Arc<AgentEngine>, agent_name: &str, session_id: uuid::Uuid) {
    let Some(router) = engine.channel_router_ref() else {
        return;
    };
    let owner_id = engine.agent_access().and_then(|a| a.owner_id.as_deref());
    let Some((channel, chat_id)) =
        crate::agent::initiative::delivery::resolve_owner_target(&ctx.infra.db, agent_name, owner_id).await
    else {
        return;
    };
    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
    let action = crate::agent::channel_actions::ChannelAction {
        name: "send_buttons".to_string(),
        params: serde_json::json!({
            "text": "Цель запущена",
            "buttons": [{ "text": "⏹ Отменить", "data": format!("icancel:{session_id}") }],
        }),
        context: serde_json::json!({ "chat_id": chat_id }),
        reply: reply_tx,
        target_channel: Some(channel),
    };
    let _ = router.send(action).await;
}

/// Intercept Telegram inline-button initiative callbacks (`iappr:UUID` /
/// `idismiss:UUID` / `icancel:UUID`). Returns `true` when the message was a
/// callback and was consumed (caller should `continue`), `false` if the
/// message should fall through to other interceptors / the dispatcher.
///
/// Only the agent's owner is allowed to approve/dismiss proposals or cancel
/// goals — non-owner callbacks receive an error frame and are also consumed
/// (fail-closed, same rule as [`handle_approval_callback`]).
pub(super) async fn handle_initiative_callback(
    ctx: &CwsCtx,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    request_id: &str,
    msg: &IncomingMessageDto,
    out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    if !initiative_matches(msg) {
        return false;
    }
    let text = msg.text.as_deref().unwrap_or("");

    let user_id = msg.user_id.clone();

    // Security: only the owner can approve/dismiss proposals or cancel goals.
    // Re-fetch live guard — fail-closed if absent.
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let is_owner = live_guard.as_ref().is_some_and(|g| g.is_owner(&user_id));
    if !is_owner {
        tracing::warn!(%user_id, "non-owner attempted to resolve initiative callback");
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Error {
                request_id: request_id.to_string(),
                message: "Only the owner can manage initiative proposals.".to_string(),
            }))
            .await;
        return true;
    }

    let db = &ctx.infra.db;

    if let Some(id_str) = text.strip_prefix("iappr:") {
        let Ok(id) = id_str.parse::<uuid::Uuid>() else {
            return true; // malformed UUID — consume but don't error noisily
        };
        match crate::gateway::handlers::agents::initiative::approve_proposal(db, engine, id).await {
            Ok(out) => {
                tracing::info!(proposal_id = %id, %user_id, "initiative proposal approved via Telegram callback");
                let _ = out_tx
                    .send(OutboundMsg::Wire(ChannelOutbound::Done {
                        request_id: request_id.to_string(),
                        text: "✅ Одобрено, цель запущена".to_string(),
                    }))
                    .await;
                if let Some(session_id) = out.session_id {
                    send_cancel_button(ctx, engine, agent_name, session_id).await;
                }
            }
            Err(e) => {
                let err_msg = describe_proposal_error(e);
                tracing::warn!(proposal_id = %id, error = %err_msg, "failed to approve initiative proposal via callback");
                let _ = out_tx
                    .send(OutboundMsg::Wire(ChannelOutbound::Error {
                        request_id: request_id.to_string(),
                        message: format!("Failed to approve proposal: {err_msg}"),
                    }))
                    .await;
            }
        }
        return true;
    }

    if let Some(id_str) = text.strip_prefix("idismiss:") {
        let Ok(id) = id_str.parse::<uuid::Uuid>() else {
            return true;
        };
        match crate::gateway::handlers::agents::initiative::dismiss_proposal(db, engine, id).await {
            Ok(_) => {
                tracing::info!(proposal_id = %id, %user_id, "initiative proposal dismissed via Telegram callback");
                let _ = out_tx
                    .send(OutboundMsg::Wire(ChannelOutbound::Done {
                        request_id: request_id.to_string(),
                        text: "❌ Отклонено".to_string(),
                    }))
                    .await;
            }
            Err(e) => {
                let err_msg = describe_proposal_error(e);
                tracing::warn!(proposal_id = %id, error = %err_msg, "failed to dismiss initiative proposal via callback");
                let _ = out_tx
                    .send(OutboundMsg::Wire(ChannelOutbound::Error {
                        request_id: request_id.to_string(),
                        message: format!("Failed to dismiss proposal: {err_msg}"),
                    }))
                    .await;
            }
        }
        return true;
    }

    if let Some(id_str) = text.strip_prefix("icancel:") {
        let Ok(session_id) = id_str.parse::<uuid::Uuid>() else {
            return true;
        };
        match crate::gateway::handlers::agents::initiative::cancel_goal(db, engine, session_id).await {
            Ok(_) => {
                tracing::info!(%session_id, %user_id, "initiative goal cancelled via Telegram callback");
                let _ = out_tx
                    .send(OutboundMsg::Wire(ChannelOutbound::Done {
                        request_id: request_id.to_string(),
                        text: "⏹ Отменено".to_string(),
                    }))
                    .await;
            }
            Err(e) => {
                let err_msg = describe_proposal_error(e);
                tracing::warn!(%session_id, error = %err_msg, "failed to cancel initiative goal via callback");
                let _ = out_tx
                    .send(OutboundMsg::Wire(ChannelOutbound::Error {
                        request_id: request_id.to_string(),
                        message: format!("Failed to cancel goal: {err_msg}"),
                    }))
                    .await;
            }
        }
        return true;
    }

    if let Some(d) = text.strip_prefix("dpm:approve:") {
        let Ok(date) = d.parse::<chrono::NaiveDate>() else { return true; };
        match crate::gateway::handlers::agents::initiative::approve_day_plan(db, engine, date).await {
            Ok(_) => { let _ = out_tx.send(OutboundMsg::Wire(ChannelOutbound::Done { request_id: request_id.to_string(), text: "✅ План принят".to_string() })).await; }
            Err(e) => { let m = describe_proposal_error(e); let _ = out_tx.send(OutboundMsg::Wire(ChannelOutbound::Error { request_id: request_id.to_string(), message: format!("Failed to approve day plan: {m}") })).await; }
        }
        return true;
    }
    if let Some(d) = text.strip_prefix("dpm:dismiss:") {
        let Ok(date) = d.parse::<chrono::NaiveDate>() else { return true; };
        match crate::gateway::handlers::agents::initiative::dismiss_day_plan(db, engine, date).await {
            Ok(_) => { let _ = out_tx.send(OutboundMsg::Wire(ChannelOutbound::Done { request_id: request_id.to_string(), text: "❌ План отклонён".to_string() })).await; }
            Err(e) => { let m = describe_proposal_error(e); let _ = out_tx.send(OutboundMsg::Wire(ChannelOutbound::Error { request_id: request_id.to_string(), message: format!("Failed to dismiss day plan: {m}") })).await; }
        }
        return true;
    }

    false
}

/// Intercept Telegram inline-button infra-decision callbacks (`infra:ok:UUID` /
/// `infra:no:UUID`). Returns `true` when the message was a callback and was
/// consumed (caller should `continue`), `false` if the message should fall
/// through to other interceptors / the dispatcher.
///
/// Only the agent's owner is allowed to resolve infra decisions — non-owner
/// callbacks receive an error frame and are also consumed (fail-closed, same
/// rule as [`handle_initiative_callback`]).
pub(super) async fn handle_infra_callback(
    ctx: &CwsCtx,
    _engine: &Arc<AgentEngine>,
    agent_name: &str,
    request_id: &str,
    msg: &IncomingMessageDto,
    out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    if !infra_matches(msg) {
        return false;
    }

    let text = msg.text.as_deref().unwrap_or("");
    let (rest, approved) = if let Some(r) = text.strip_prefix("infra:ok:") {
        (r, true)
    } else if let Some(r) = text.strip_prefix("infra:no:") {
        (r, false)
    } else {
        return false; // not an infra callback — let other interceptors / dispatcher try
    };

    let user_id = msg.user_id.clone();

    // Security: only the owner can resolve infra decisions. Re-fetch live
    // guard — fail-closed if absent.
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let is_owner = live_guard.as_ref().is_some_and(|g| g.is_owner(&user_id));
    if !is_owner {
        tracing::warn!(%user_id, "non-owner attempted to resolve infra decision");
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Error {
                request_id: request_id.to_string(),
                message: "Only the owner can resolve infra decisions.".to_string(),
            }))
            .await;
        return true;
    }

    let Ok(id) = rest.parse::<uuid::Uuid>() else {
        return true; // malformed UUID — consume but don't error noisily
    };
    match crate::gateway::handlers::infra::resolve_infra_decision(
        &ctx.infra, &ctx.agents, id, approved, &user_id,
    )
    .await
    {
        Ok(()) => {
            tracing::info!(decision_id = %id, %user_id, approved, "infra decision resolved via Telegram callback");
            let text = if approved { "✅ Одобрено, выполняю" } else { "❌ Отклонено" };
            let _ = out_tx
                .send(OutboundMsg::Wire(ChannelOutbound::Done {
                    request_id: request_id.to_string(),
                    text: text.to_string(),
                }))
                .await;
        }
        Err(e) => {
            tracing::warn!(decision_id = %id, error = %e, "failed to resolve infra decision via callback");
            let _ = out_tx
                .send(OutboundMsg::Wire(ChannelOutbound::Error {
                    request_id: request_id.to_string(),
                    message: format!("Не удалось обработать решение: {e}"),
                }))
                .await;
        }
    }
    true
}

// ── Clarify callback / text-intercept ────────────────────────────────────────

/// Parse a `clarify:{uuid}:{idx_or_other}` Telegram callback payload.
/// Returns `(clarify_id, slot)` where slot is `"0"`, `"1"`, ... or `"other"`.
pub(super) fn parse_clarify_callback(text: &str) -> Option<(uuid::Uuid, String)> {
    let rest = text.strip_prefix("clarify:")?;
    let (id_str, slot) = rest.split_once(':')?;
    let id = id_str.parse::<uuid::Uuid>().ok()?;
    if slot.is_empty() {
        return None;
    }
    Some((id, slot.to_string()))
}

/// True iff the adapter tagged this message as an inline-button callback.
pub(super) fn is_callback(msg: &IncomingMessageDto) -> bool {
    msg.context
        .get("is_callback")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Sync classifier: is this an approval callback (`approve:`/`reject:`)?
pub(super) fn approval_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    let t = msg.text.as_deref().unwrap_or("");
    t.starts_with("approve:") || t.starts_with("reject:")
}

/// Sync classifier: is this an initiative callback?
pub(super) fn initiative_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    let t = msg.text.as_deref().unwrap_or("");
    t.starts_with("iappr:") || t.starts_with("idismiss:") || t.starts_with("icancel:")
        || t.starts_with("dpm:approve:") || t.starts_with("dpm:dismiss:")
}

/// Sync classifier: is this an infra-decision callback?
pub(super) fn infra_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    let t = msg.text.as_deref().unwrap_or("");
    t.starts_with("infra:ok:") || t.starts_with("infra:no:")
}

/// Sync classifier: is this a clarify button callback (`clarify:{id}:{slot}`)?
pub(super) fn clarify_cb_matches(msg: &IncomingMessageDto) -> bool {
    if !is_callback(msg) { return false; }
    parse_clarify_callback(msg.text.as_deref().unwrap_or("")).is_some()
}

/// Intercept `clarify:{id}:{idx_or_other}` inline-button callbacks.
///
/// - `idx` (numeric) → look up choice text in the button payload; resolve waiter.
/// - `"other"` → flip `awaiting_text = true` via `mark_awaiting_text` so the
///   next plain text message is intercepted as a free-form answer.
///
/// Owner-gated (same rule as approval callbacks). Returns `true` when consumed.
pub(super) async fn handle_clarify_callback(
    ctx: &CwsCtx,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    request_id: &str,
    msg: &IncomingMessageDto,
    out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    if !clarify_cb_matches(msg) {
        return false;
    }
    let text = msg.text.as_deref().unwrap_or("");
    let (clarify_id, slot) = match parse_clarify_callback(text) {
        Some(v) => v,
        None => {
            tracing::warn!(text = %text, "clarify callback matched but could not be parsed");
            return false;
        }
    };
    let user_id = msg.user_id.clone();

    // Owner gate — same pattern as approval callbacks.
    let live_guard = ctx.auth.access_guards.read().await.get(agent_name).cloned();
    let is_owner = live_guard.as_ref().is_some_and(|g| g.is_owner(&user_id));
    if !is_owner {
        tracing::warn!(%user_id, "non-owner attempted to resolve clarify via callback");
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Error {
                request_id: request_id.to_string(),
                message: "Only the owner can answer clarification questions.".to_string(),
            }))
            .await;
        return true;
    }

    let clarify_mgr = &engine.cfg().clarify_manager;

    if slot == "other" {
        // Flip the waiter to awaiting_text mode so the next plain message
        // is intercepted as a free-form response.
        clarify_mgr.mark_awaiting_text(clarify_id);
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Done {
                request_id: request_id.to_string(),
                text: "Please type your answer.".to_string(),
            }))
            .await;
        return true;
    }

    // Numeric slot → resolve with the choice text.
    // The channel adapter is expected to echo `button_text` in the callback
    // context. Fall back to `"option {idx}"` if the adapter does not.
    let choice_text = if let Ok(idx) = slot.parse::<usize>() {
        msg.context
            .get("button_text")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("option {idx}"))
    } else {
        slot.clone()
    };

    let resolved = clarify_mgr.resolve(clarify_id, choice_text.clone());
    if resolved {
        tracing::info!(
            %clarify_id, choice = %choice_text, %user_id,
            "clarify resolved via channel button callback"
        );
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Done {
                request_id: request_id.to_string(),
                text: format!("✅ {choice_text}"),
            }))
            .await;
    } else {
        tracing::warn!(%clarify_id, "clarify callback: waiter not found (already resolved?)");
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Error {
                request_id: request_id.to_string(),
                message: "Clarify already resolved or timed out.".to_string(),
            }))
            .await;
    }
    true
}

/// Try to intercept a plain text message as a clarify open-ended response.
///
/// Priority: if there is an active approval waiter for this agent, we do NOT
/// intercept — approval takes priority over clarify text-intercept.
///
/// `channel_type` is the adapter-reported channel (e.g. `"telegram"`) — it
/// comes from `ReaderState.channel_type` which is known at the call site.
///
/// Returns `true` when the message was consumed as a clarify response.
pub(super) async fn handle_clarify_text(
    ctx: &CwsCtx,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    channel_type: &str,
    request_id: &str,
    msg: &IncomingMessageDto,
    out_tx: &mpsc::Sender<OutboundMsg>,
) -> bool {
    // Callbacks are handled by handle_clarify_callback — skip here.
    let is_callback = msg
        .context
        .get("is_callback")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if is_callback {
        return false;
    }

    // Fast-path: skip expensive session-lookup when no clarify waiter is active.
    if !engine.cfg().clarify_manager.has_any_pending() {
        return false;
    }

    // Owner gate (#5): only the owner may answer a clarify directed at the
    // owner. A non-owner's plain text falls through to a normal turn — unlike
    // the callback path, we do NOT consume with an error frame here.
    let is_owner = ctx
        .auth
        .access_guards
        .read()
        .await
        .get(agent_name)
        .is_some_and(|g| g.is_owner(&msg.user_id));
    if !clarify_text_is_owner_allowed(is_owner) {
        tracing::debug!(user_id = %msg.user_id, "clarify text-intercept: non-owner, falling through to turn");
        return false;
    }

    let text = match msg.text.as_deref() {
        Some(t) if !t.trim().is_empty() => t.trim().to_string(),
        _ => return false,
    };

    // Resolve the session for this channel peer so we can check for pending
    // clarify waiters. Uses the same lookup as mirror_to_session and the
    // channel_ws handshake (resolve_active_dm_session, ≤4h window).
    let dm_scope = engine
        .cfg()
        .agent
        .session
        .as_ref()
        .map(|s| s.dm_scope.as_str())
        .unwrap_or("per-channel-peer")
        .to_string();

    let chat_scope = msg.chat_scope();
    let session_id = match opex_db::sessions::resolve_active_dm_session(
        &ctx.infra.db,
        agent_name,
        &msg.user_id,
        channel_type,
        &dm_scope,
        chat_scope.as_deref(),
    )
    .await
    {
        Ok(Some((sid, _status))) => sid,
        _ => return false,
    };

    let clarify_mgr = &engine.cfg().clarify_manager;

    // Check for pending open-ended clarify waiter.
    let Some(clarify_id) = clarify_mgr.has_pending_text(session_id) else {
        return false;
    };

    // Priority: if this agent has ANY active approval waiter, let the
    // message fall through to the dispatcher so the approval flow is not
    // inadvertently consumed by the clarify interceptor.
    let approval_waiters = engine.tex().approval_waiters.len();
    if approval_waiters > 0 {
        tracing::debug!(
            %session_id, %clarify_id,
            "clarify text-intercept: skipping — approval waiter active"
        );
        return false;
    }

    let resolved = clarify_mgr.resolve(clarify_id, text.clone());
    if resolved {
        tracing::info!(
            %clarify_id, %session_id, %agent_name,
            "clarify resolved via channel text-intercept"
        );
        let _ = out_tx
            .send(OutboundMsg::Wire(ChannelOutbound::Done {
                request_id: request_id.to_string(),
                text: format!("✅ Got it: {text}"),
            }))
            .await;
        true
    } else {
        // Waiter disappeared between has_pending_text and resolve — race; fall through.
        false
    }
}

// ── Owner gate helpers ────────────────────────────────────────────────────────

/// Owner-gate decision for clarify text-intercept (#5): only the owner may
/// resolve a clarify the agent directed at the owner. A non-owner's message
/// falls through to a normal turn (caller returns `false`).
pub(super) fn clarify_text_is_owner_allowed(is_owner: bool) -> bool {
    is_owner
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod clarify_callback_tests {
    use super::parse_clarify_callback;
    use uuid::Uuid;

    #[test]
    fn parses_numeric_slot() {
        let id = Uuid::nil();
        let parsed = parse_clarify_callback(&format!("clarify:{id}:2"));
        assert_eq!(parsed, Some((id, "2".to_string())));
    }

    #[test]
    fn parses_other_slot() {
        let id = Uuid::nil();
        let parsed = parse_clarify_callback(&format!("clarify:{id}:other"));
        assert_eq!(parsed, Some((id, "other".to_string())));
    }

    #[test]
    fn rejects_non_clarify() {
        assert!(parse_clarify_callback("approve:abc").is_none());
        assert!(parse_clarify_callback("fse:abc:x").is_none());
    }

    #[test]
    fn rejects_malformed_uuid() {
        assert!(parse_clarify_callback("clarify:not-a-uuid:0").is_none());
    }

    #[test]
    fn rejects_empty_slot() {
        let id = Uuid::nil();
        assert!(parse_clarify_callback(&format!("clarify:{id}:")).is_none());
    }

    #[test]
    fn rejects_missing_slot() {
        let id = Uuid::nil();
        assert!(parse_clarify_callback(&format!("clarify:{id}")).is_none());
    }
}

#[cfg(test)]
mod clarify_text_owner_gate_tests {
    use super::clarify_text_is_owner_allowed;

    #[test]
    fn owner_allowed() {
        assert!(clarify_text_is_owner_allowed(true), "owner may resolve clarify text");
    }

    #[test]
    fn non_owner_falls_through() {
        assert!(!clarify_text_is_owner_allowed(false), "non-owner must not resolve — falls through to a turn");
    }
}

#[cfg(test)]
mod classifier_tests {
    use super::*;
    use opex_types::IncomingMessageDto;

    // `IncomingMessageDto` does not derive `Default` — construct explicitly.
    fn dto(text: &str, is_cb: bool) -> IncomingMessageDto {
        IncomingMessageDto {
            user_id: "u1".to_string(),
            display_name: None,
            text: Some(text.to_string()),
            attachments: Vec::new(),
            context: serde_json::json!({ "is_callback": is_cb }),
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn approval_matches_only_with_callback_flag() {
        assert!(approval_matches(&dto("approve:abc", true)));
        assert!(approval_matches(&dto("reject:abc", true)));
        assert!(!approval_matches(&dto("approve:abc", false)), "plain text lookalike is NOT a callback");
        assert!(!approval_matches(&dto("hello", true)));
    }

    #[test]
    fn initiative_matches_all_prefixes() {
        for p in ["iappr:x", "idismiss:x", "icancel:x", "dpm:approve:x", "dpm:dismiss:x"] {
            assert!(initiative_matches(&dto(p, true)), "{p} must match");
        }
        assert!(!initiative_matches(&dto("iappr:x", false)));
        assert!(!initiative_matches(&dto("infra:ok:x", true)));
    }

    #[test]
    fn infra_matches_prefixes() {
        assert!(infra_matches(&dto("infra:ok:x", true)));
        assert!(infra_matches(&dto("infra:no:x", true)));
        assert!(!infra_matches(&dto("infra:ok:x", false)));
        assert!(!infra_matches(&dto("infra:maybe:x", true)));
    }

    #[test]
    fn clarify_cb_matches_valid_form() {
        assert!(clarify_cb_matches(&dto("clarify:11111111-1111-1111-1111-111111111111:0", true)));
        assert!(!clarify_cb_matches(&dto("clarify:bad", true)), "malformed clarify is not a clarify callback");
        assert!(!clarify_cb_matches(&dto("clarify:11111111-1111-1111-1111-111111111111:0", false)));
    }

    #[test]
    fn plain_text_that_looks_like_prefix_is_not_a_callback() {
        // The design-review "message vanishes" case: user types "infra:ok: yes".
        let m = dto("infra:ok: yes", false);
        assert!(!approval_matches(&m) && !initiative_matches(&m) && !infra_matches(&m) && !clarify_cb_matches(&m),
            "no classifier may claim a plain-text message");
    }
}
