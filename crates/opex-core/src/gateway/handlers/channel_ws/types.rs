//! Internal types shared across the channel WS module:
//!
//! - [`CwsCtx`] — cluster-state bundle (was inline in `mod.rs`).
//! - [`SessionKey`] — `(agent, eff_user, eff_channel)` derived via
//!   `dm_scope_keys`; the locking key for in-flight per-session serialisation.
//! - [`OutboundMsg`] — what the dispatcher / inline handlers send to the
//!   single-writer task. Wraps [`opex_types::ChannelOutbound`] plus
//!   `Ping`/`Shutdown` control variants.
//! - [`InflightMessage`] — bookkeeping for an in-progress request so the
//!   reader can serve `Cancel` for ANY request_id (not just the
//!   currently-foregrounded one — that was the bug being fixed).
//! - [`InflightRegistry`] — concurrent map `request_id → InflightMessage`.

use crate::gateway::clusters::{
    AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor,
};
use opex_types::ChannelOutbound;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Cluster-state bundle. Cheap to clone — every field is `Arc`-backed.
#[derive(Clone)]
pub(super) struct CwsCtx {
    pub agents: AgentCore,
    pub auth:   AuthServices,
    pub bus:    ChannelBus,
    pub infra:  InfraServices,
    pub status: StatusMonitor,
    pub cfg:    ConfigServices,
}

/// Unique routing key for in-flight serialisation. Two messages with the
/// same `SessionKey` MUST be processed in receive order; messages with
/// different keys may run concurrently.
///
/// Computed via `opex_db::sessions::dm_scope_keys` so the key matches
/// what `get_or_create_session` will produce when the dispatcher actually
/// runs `handle_with_status`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct SessionKey {
    pub agent_name:  String,
    pub eff_user:    String,
    pub eff_channel: String,
}

impl SessionKey {
    pub fn from_inbound(agent_name: &str, user_id: &str, channel: &str, dm_scope: &str) -> Self {
        let (eff_user, eff_channel) =
            opex_db::sessions::dm_scope_keys(user_id, channel, dm_scope);
        Self {
            agent_name:  agent_name.to_string(),
            eff_user:    eff_user.to_string(),
            eff_channel: eff_channel.to_string(),
        }
    }
}

/// Wrapped message handed to the writer task.
#[derive(Debug)]
pub(super) enum OutboundMsg {
    Wire(ChannelOutbound),
    /// Raw WebSocket-level Ping for liveness check (separate from the
    /// `ChannelOutbound::Pong` reply variant).
    Ping,
    /// Reader→Writer signal: drain remaining messages and exit.
    Shutdown,
}

/// One in-flight message tracked by the dispatcher so a `Cancel` for ANY
/// request_id can stop it (not just the currently-foregrounded one).
pub(super) struct InflightMessage {
    pub join_handle: JoinHandle<()>,
    /// Per-turn cancellation token wired into `handle_with_status` → `execute`.
    /// R-CHANNEL: cancelling it stops the turn COOPERATIVELY (engine reaches
    /// finalize → session marked 'interrupted'), as opposed to a hard
    /// `join_handle.abort()` which guard-drops the session to 'failed'.
    pub cancel: CancellationToken,
}

/// Concurrent registry: `request_id` → in-flight task.
/// Reader inserts on `Message`, dispatcher removes on completion, reader
/// looks up + aborts on `Cancel`.
pub(super) type InflightRegistry = Arc<Mutex<HashMap<String, InflightMessage>>>;

/// Concurrent map of in-flight engine-action waiters: `action_id` → oneshot
/// reply channel. Inserted by the action-forwarder when sending an action
/// to the adapter, resolved by the reader when `ChannelInbound::ActionResult`
/// arrives, drained on disconnect (waiters get `Err("disconnected")`).
pub(super) type PendingActionsMap =
    Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<(), String>>>>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_key_per_channel_peer() {
        let k = SessionKey::from_inbound("Arty", "alice", "telegram", "per-channel-peer");
        assert_eq!(k.agent_name, "Arty");
        assert_eq!(k.eff_user, "alice");
        assert_eq!(k.eff_channel, "telegram");
    }

    #[test]
    fn session_key_shared_collapses_channel() {
        let tg = SessionKey::from_inbound("Arty", "alice", "telegram", "shared");
        let dc = SessionKey::from_inbound("Arty", "alice", "discord", "shared");
        assert_eq!(tg, dc, "shared dm_scope must collapse channel into '*'");
    }

    #[test]
    fn session_key_per_chat_collapses_user() {
        let a = SessionKey::from_inbound("Arty", "alice", "telegram", "per-chat");
        let b = SessionKey::from_inbound("Arty", "bob", "telegram", "per-chat");
        assert_eq!(a, b, "per-chat dm_scope must collapse user into '*'");
    }

    #[test]
    fn session_key_unknown_falls_back_to_per_channel_peer() {
        let k = SessionKey::from_inbound("Arty", "alice", "telegram", "garbage");
        assert_eq!(k.eff_user, "alice");
        assert_eq!(k.eff_channel, "telegram");
    }
}
