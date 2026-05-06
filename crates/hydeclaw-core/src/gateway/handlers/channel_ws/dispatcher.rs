//! Per-message dispatcher. For each `ChannelInbound::Message`, spawns a
//! background task that:
//!   1. acquires the per-`SessionKey` lock,
//!   2. runs `engine.handle_with_status` with status/chunk callbacks that
//!      forward into the shared `OutboundMsg` channel,
//!   3. emits the final `Done` / `Error` to the writer,
//!   4. removes itself from the inflight registry.
//!
//! Engine channel-actions (approval_request, send_voice, etc.) come back
//! through `channel_action_rx` in a SEPARATE long-lived consumer (started
//! by `mod.rs` once per WS connection); they're not handled here.

use std::sync::Arc;
use tokio::sync::mpsc;

use hydeclaw_types::{ChannelOutbound, IncomingMessageDto};

use super::session_locks::SessionLockMap;
use super::types::{InflightMessage, InflightRegistry, OutboundMsg, SessionKey};
use crate::agent::engine::{AgentEngine, ProcessingPhase};

/// Spawn a per-message task. Registers the task in `inflight` BEFORE
/// returning so a `Cancel` for `request_id` arriving immediately after this
/// call cannot race the registration.
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_message(
    engine: Arc<AgentEngine>,
    agent_name: String,
    channel_type: String,
    formatting_prompt: Option<String>,
    request_id: String,
    msg: IncomingMessageDto,
    timeout_secs: u64,
    out_tx: mpsc::Sender<OutboundMsg>,
    lock_map: Arc<SessionLockMap>,
    inflight: InflightRegistry,
) {
    let dm_scope = engine
        .cfg()
        .agent
        .session
        .as_ref()
        .map(|s| s.dm_scope.as_str())
        .unwrap_or("per-channel-peer")
        .to_string();

    let session_key = SessionKey::from_inbound(&agent_name, &msg.user_id, &channel_type, &dm_scope);
    let req_id_for_task = request_id.clone();
    let agent_name_for_task = agent_name.clone();
    let inflight_for_cleanup = inflight.clone();

    // Hold the inflight lock across spawn+insert so a Cancel for this
    // request_id arriving in the reader CANNOT race the registration: we
    // only release after the JoinHandle is in the registry.
    let mut inflight_guard = inflight.lock().await;

    let join_handle = tokio::spawn(async move {
        // Acquire the session lock — held for the whole engine call so two
        // messages for the same session run in receive order.
        let _lock = lock_map.acquire(session_key).await;

        let incoming = msg.into_incoming(
            engine.cfg().agent.name.clone(),
            channel_type.clone(),
            formatting_prompt,
        );

        let (status_tx, mut status_rx) = mpsc::unbounded_channel::<ProcessingPhase>();
        let (chunk_tx, mut chunk_rx) = mpsc::channel::<String>(512);

        // Forward chunks → out_tx as they arrive.
        let chunk_out = out_tx.clone();
        let chunk_req = request_id.clone();
        let chunk_forwarder = tokio::spawn(async move {
            while let Some(text) = chunk_rx.recv().await {
                let m = ChannelOutbound::Chunk { request_id: chunk_req.clone(), text };
                if chunk_out.send(OutboundMsg::Wire(m)).await.is_err() { return; }
            }
        });

        // Forward phases → out_tx.
        let phase_out = out_tx.clone();
        let phase_req = request_id.clone();
        let phase_forwarder = tokio::spawn(async move {
            while let Some(phase) = status_rx.recv().await {
                let (p, t) = phase.to_wire();
                let m = ChannelOutbound::Phase {
                    request_id: phase_req.clone(),
                    phase: p,
                    tool_name: t,
                };
                if phase_out.send(OutboundMsg::Wire(m)).await.is_err() { return; }
            }
        });

        // Run the engine with optional request timeout.
        let engine_fut = engine.handle_with_status(&incoming, Some(status_tx), Some(chunk_tx));
        let result = if timeout_secs > 0 {
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), engine_fut).await {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!(
                    "Request timed out after {timeout_secs}s. The task was too complex or an external service was slow.",
                )),
            }
        } else {
            engine_fut.await
        };

        // Drain forwarders so any tail chunks/phases reach the wire.
        let _ = chunk_forwarder.await;
        let _ = phase_forwarder.await;

        // Final terminal frame.
        let final_msg = match result {
            Ok(text) => ChannelOutbound::Done { request_id: request_id.clone(), text },
            Err(e)  => ChannelOutbound::Error { request_id: request_id.clone(), message: e.to_string() },
        };
        if out_tx.send(OutboundMsg::Wire(final_msg)).await.is_err() {
            tracing::debug!(agent = %agent_name_for_task, %request_id, "out_tx closed before final frame");
        }

        // Remove ourselves from the inflight registry.
        inflight_for_cleanup.lock().await.remove(&request_id);
    });

    // Register inside the held lock, then release. Cancel arriving before
    // this point would block on the same lock and find the entry afterwards.
    inflight_guard.insert(req_id_for_task, InflightMessage { join_handle });
    drop(inflight_guard);
}

/// Abort the in-flight task for `request_id` (if any). The reader is
/// responsible for emitting any user-visible cancellation frame.
pub(super) async fn cancel(request_id: &str, inflight: &InflightRegistry) -> bool {
    if let Some(entry) = inflight.lock().await.remove(request_id) {
        entry.join_handle.abort();
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::handlers::channel_ws::types::InflightMessage;

    /// Cancel for an unregistered request_id is a no-op returning false.
    #[tokio::test]
    async fn cancel_unknown_returns_false() {
        let inflight: InflightRegistry =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        assert!(!cancel("never-registered", &inflight).await);
    }

    /// Cancel for a registered request_id aborts the task and returns true.
    #[tokio::test]
    async fn cancel_aborts_registered_task() {
        let inflight: InflightRegistry =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
        let req_id = "req-1".to_string();

        // Spawn a task that would otherwise run forever.
        let h = tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        inflight.lock().await.insert(req_id.clone(), InflightMessage { join_handle: h });

        assert!(cancel(&req_id, &inflight).await);
        assert!(inflight.lock().await.is_empty());
    }
}
