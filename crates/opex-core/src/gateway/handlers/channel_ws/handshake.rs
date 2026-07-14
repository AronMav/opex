//! `ChannelInbound::Ready` handler: sets adapter state, registers the
//! channel in `connected_channels`, subscribes to the channel-action
//! router, ships the `Config` reply, and replays any pending/outbound
//! messages saved while the adapter was disconnected.
//!
//! Hands off `(channel_type, channel_action_rx)` to the action-forwarder
//! task (which has been waiting on a oneshot since `channel_ws_loop`
//! started) so engine-emitted actions can finally flow to the writer.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use uuid::Uuid;

use opex_types::{ChannelActionDto, ChannelOutbound};

use super::reader::ReaderState;
use super::types::{CwsCtx, OutboundMsg};
use crate::agent::channel_actions::ChannelAction;
use crate::agent::engine::AgentEngine;
use crate::db::outbound;

/// One-shot payload handed from the Ready handler to the action-forwarder.
pub(super) struct ActionForwarderInit {
    pub channel_type: String,
    pub channel_action_rx: mpsc::Receiver<ChannelAction>,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_ready(
    ctx: &CwsCtx,
    engine: &Arc<AgentEngine>,
    agent_name: &str,
    out_tx: &mpsc::Sender<OutboundMsg>,
    adapter_type: String,
    version: String,
    formatting_prompt: Option<String>,
    state: &mut ReaderState,
    action_install_tx: &mut Option<oneshot::Sender<ActionForwarderInit>>,
    outbound_ids: &Arc<Mutex<HashMap<String, Uuid>>>,
) {
    tracing::info!(
        %agent_name, adapter = %adapter_type, %version,
        has_formatting_prompt = formatting_prompt.is_some(),
        "adapter ready",
    );

    // First `Ready` on this connection? (`action_install_tx` is taken on the
    // first Ready; the router always exists for a channel WS.) Drives both the
    // subscribe-once guard (#2) and connected_channels dedup (#7).
    let is_first_ready = action_install_tx.is_some();

    state.channel_type = adapter_type.clone();
    state.formatting_prompt = formatting_prompt.clone();

    // Cache formatting prompt on engine for cron/heartbeat use.
    {
        let mut cached = engine.state().channel_formatting_prompt.write().await;
        *cached = formatting_prompt;
    }

    // Resolve channel_id + display_name + config from agent_channels.
    let ch_row = sqlx::query_as::<_, (sqlx::types::Uuid, String, serde_json::Value)>(
        "SELECT id, display_name, config FROM agent_channels \
         WHERE agent_name = $1 AND channel_type = $2 \
         ORDER BY created_at LIMIT 1",
    )
    .bind(agent_name)
    .bind(&state.channel_type)
    .fetch_optional(&ctx.infra.db)
    .await
    .ok()
    .flatten();

    let (ch_id, ch_display, ch_config) = match ch_row {
        Some((id, name, cfg)) => (Some(id), name, cfg),
        None => (
            None,
            format!("{agent_name}/{}", state.channel_type),
            serde_json::Value::Object(Default::default()),
        ),
    };

    // Register / refresh in connected_channels (dedup on repeat Ready — #7).
    {
        let now = chrono::Utc::now();
        let entry = crate::gateway::state::ConnectedChannel {
            agent_name: agent_name.to_string(),
            channel_id: ch_id,
            channel_type: state.channel_type.clone(),
            display_name: ch_display,
            adapter_version: version,
            connected_at: now,
            last_activity: now,
        };
        let mut chans = ctx.bus.connected_channels.write().await;
        upsert_connected_channel(&mut chans, is_first_ready, entry);
    }
    ctx.bus
        .ui_event_tx
        .send(serde_json::json!({"type": "channels_changed", "agent": agent_name}).to_string())
        .ok();

    // Subscribe to the channel action router and hand off the receiver to the
    // action-forwarder — ONLY on the first Ready. A duplicate Ready must not
    // register a second (dead) subscription or overwrite channel_conn_id (#2).
    if let Some(ref router) = engine.state().channel_router {
        if let Some(tx) = action_install_tx.take() {
            let (id, rx) = router.subscribe(&state.channel_type).await;
            state.channel_conn_id = Some(id);
            let _ = tx.send(ActionForwarderInit {
                channel_type: state.channel_type.clone(),
                channel_action_rx: rx,
            });
        } else {
            tracing::warn!(%agent_name, "Ready received twice on same WS — subscribe skipped");
        }
    }

    // Reply with Config (language, owner, typing_mode).
    let owner_id = engine.cfg().agent.access.as_ref().and_then(|a| a.owner_id.clone());
    let typing_mode = ch_config
        .get("typing_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("instant")
        .to_string();
    let _ = out_tx
        .send(OutboundMsg::Wire(ChannelOutbound::Config {
            language: engine.cfg().agent.language.clone(),
            owner_id,
            typing_mode,
        }))
        .await;

    // First-run onboarding: if this agent has never had a session, kick off
    // the onboarding flow in the background.
    let session_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE agent_id = $1")
            .bind(agent_name)
            .fetch_one(&ctx.infra.db)
            .await
            .unwrap_or(0);

    if session_count == 0 {
        tracing::info!(%agent_name, "first-run detected, scheduling onboarding");
        let engine_clone = engine.clone();
        let agent_name_clone = agent_name.to_string();
        let workspace_dir = crate::config::WORKSPACE_DIR.to_string();
        tokio::spawn(async move {
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

    // Replay any pending messages from the previous disconnection.
    replay_pending_messages(ctx, agent_name, &state.channel_type, out_tx).await;

    // Replay unacked outbound queue actions for this channel.
    replay_outbound_queue(ctx, agent_name, &state.channel_type, out_tx, outbound_ids).await;
}

/// Take pending `done`/`error` frames the previous WS lifetime couldn't
/// deliver and re-emit them. If the writer rejects (channel closed), we
/// re-save the rest as pending so the next reconnect can pick them up.
async fn replay_pending_messages(
    ctx: &CwsCtx,
    agent_name: &str,
    channel_type: &str,
    out_tx: &mpsc::Sender<OutboundMsg>,
) {
    let pending = match crate::db::pending::take_pending(&ctx.infra.db, agent_name).await {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch pending messages");
            return;
        }
    };

    tracing::info!(%agent_name, count = pending.len(), "delivering pending messages after reconnect");
    let mut failed = false;
    for pm in pending {
        if failed {
            // Writer is gone — re-save remaining messages.
            crate::db::pending::save_pending(
                &ctx.infra.db,
                agent_name,
                &pm.request_id,
                &pm.channel,
                &pm.message_type,
                &pm.text,
            )
            .await
            .ok();
            continue;
        }
        let outbound = if pm.message_type == "done" {
            ChannelOutbound::Done {
                request_id: pm.request_id.clone(),
                text: pm.text.clone(),
            }
        } else {
            ChannelOutbound::Error {
                request_id: pm.request_id.clone(),
                message: pm.text.clone(),
            }
        };
        if out_tx.send(OutboundMsg::Wire(outbound)).await.is_err() {
            failed = true;
            crate::db::pending::save_pending(
                &ctx.infra.db,
                agent_name,
                &pm.request_id,
                channel_type,
                &pm.message_type,
                &pm.text,
            )
            .await
            .ok();
        }
    }
}

/// Replay channel actions that were queued but never acked (max 50 oldest).
/// Each replay gets a fresh `action_id`; the queue row is marked `sent` so
/// duplicate adapter restarts don't fan out the same action twice.
async fn replay_outbound_queue(
    ctx: &CwsCtx,
    agent_name: &str,
    channel_type: &str,
    out_tx: &mpsc::Sender<OutboundMsg>,
    outbound_ids: &Arc<Mutex<HashMap<String, Uuid>>>,
) {
    let queued = match outbound::get_pending(&ctx.infra.db, channel_type, 50).await {
        Ok(q) if !q.is_empty() => q,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %e, "failed to fetch outbound queue");
            return;
        }
    };

    tracing::info!(
        %agent_name, channel = %channel_type, count = queued.len(),
        "replaying outbound queue after reconnect",
    );
    for (queue_id, _q_agent_id, q_action_name, q_payload) in queued {
        let action_id = Uuid::new_v4().to_string();
        let dto = ChannelActionDto {
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
        let frame = ChannelOutbound::Action {
            action_id: action_id.clone(),
            action: dto,
        };
        if out_tx.send(OutboundMsg::Wire(frame)).await.is_err() {
            tracing::warn!(%agent_name, "writer closed during outbound queue replay");
            break;
        }
        let db = ctx.infra.db.clone();
        // AUDIT-FF-004: see docs/superpowers/specs/2026-05-06-s5-tech-debt-hygiene-design.md
        tokio::spawn(async move {
            if let Err(e) = outbound::mark_sent(&db, queue_id).await {
                tracing::warn!(queue_id = %queue_id, error = %e, "outbound mark_sent failed");
            }
        });
    }
}

/// Push a `connected_channels` entry only on the first `Ready` of a
/// connection; on a repeated `Ready`, update the matching row's
/// `last_activity` instead of duplicating it (defect #7). Pure so it is
/// unit-testable without a DB.
pub(super) fn upsert_connected_channel(
    chans: &mut Vec<crate::gateway::state::ConnectedChannel>,
    is_first_ready: bool,
    entry: crate::gateway::state::ConnectedChannel,
) {
    if is_first_ready {
        chans.push(entry);
        return;
    }
    if let Some(existing) = chans
        .iter_mut()
        .find(|c| c.agent_name == entry.agent_name && c.channel_type == entry.channel_type)
    {
        existing.last_activity = entry.last_activity;
    } else {
        // No prior row despite not-first-Ready (e.g. evicted) — push to stay consistent.
        chans.push(entry);
    }
}

#[cfg(test)]
mod ready_guard_tests {
    use super::*;
    use crate::gateway::state::ConnectedChannel;

    fn chan(agent: &str, ctype: &str) -> ConnectedChannel {
        let now = chrono::Utc::now();
        ConnectedChannel {
            agent_name: agent.to_string(),
            channel_id: None,
            channel_type: ctype.to_string(),
            display_name: format!("{agent}/{ctype}"),
            adapter_version: "test".to_string(),
            connected_at: now,
            last_activity: now,
        }
    }

    #[test]
    fn first_ready_pushes_row() {
        let mut chans: Vec<ConnectedChannel> = vec![];
        upsert_connected_channel(&mut chans, /*is_first_ready=*/ true, chan("Arty", "telegram"));
        assert_eq!(chans.len(), 1, "first Ready must push a row");
    }

    #[test]
    fn repeat_ready_does_not_duplicate_row() {
        let mut chans: Vec<ConnectedChannel> = vec![chan("Arty", "telegram")];
        let before = chans[0].last_activity;
        std::thread::sleep(std::time::Duration::from_millis(2));
        upsert_connected_channel(&mut chans, /*is_first_ready=*/ false, chan("Arty", "telegram"));
        assert_eq!(chans.len(), 1, "repeat Ready must not push a duplicate row");
        assert!(chans[0].last_activity > before, "repeat Ready must bump last_activity");
    }
}
