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
/// runs `handle_with_status`. Includes `eff_chat_scope` (T03 triage Point 5)
/// so two different chats/groups on the same platform for the same user
/// serialise INDEPENDENTLY instead of contending on the same lock — they are
/// different sessions now, so they must not be forced into receive-order.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub(super) struct SessionKey {
    pub agent_name:     String,
    pub eff_user:       String,
    pub eff_channel:    String,
    pub eff_chat_scope: Option<String>,
}

impl SessionKey {
    pub fn from_inbound(
        agent_name: &str,
        user_id: &str,
        channel: &str,
        dm_scope: &str,
        chat_scope: Option<&str>,
    ) -> Self {
        let (eff_user, eff_channel, eff_chat_scope) =
            opex_db::sessions::dm_scope_keys(user_id, channel, dm_scope, chat_scope);
        Self {
            agent_name:     agent_name.to_string(),
            eff_user:       eff_user.to_string(),
            eff_channel:    eff_channel.to_string(),
            eff_chat_scope: eff_chat_scope.map(str::to_string),
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

/// One in-flight message tracked so a `Cancel` for ANY request_id can stop it.
/// Registered at ENQUEUE with `abort = None`; the per-session consumer fills
/// `abort` once it spawns the turn body, enabling a post-grace hard-abort of a
/// sync-wedged turn without killing the consumer (which serves the whole
/// session's queue).
pub(super) struct InflightMessage {
    /// Per-turn cooperative cancellation token wired into `handle_with_status`.
    /// R-CHANNEL: cancelling stops the turn COOPERATIVELY (finalize →
    /// 'interrupted'), not a hard abort (which guard-drops to 'failed').
    pub cancel: CancellationToken,
    /// Abort handle for the spawned turn task; `None` while the turn is still
    /// queued. Used only as a post-grace backstop for a sync-wedged turn.
    pub abort: Option<tokio::task::AbortHandle>,
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
        let k = SessionKey::from_inbound("Arty", "alice", "telegram", "per-channel-peer", None);
        assert_eq!(k.agent_name, "Arty");
        assert_eq!(k.eff_user, "alice");
        assert_eq!(k.eff_channel, "telegram");
        assert_eq!(k.eff_chat_scope, None);
    }

    #[test]
    fn session_key_shared_collapses_channel() {
        let tg = SessionKey::from_inbound("Arty", "alice", "telegram", "shared", Some("100"));
        let dc = SessionKey::from_inbound("Arty", "alice", "discord", "shared", Some("200"));
        assert_eq!(tg, dc, "shared dm_scope must collapse channel AND chat_scope into '*'/None");
    }

    #[test]
    fn session_key_per_chat_collapses_user() {
        let a = SessionKey::from_inbound("Arty", "alice", "telegram", "per-chat", Some("100"));
        let b = SessionKey::from_inbound("Arty", "bob", "telegram", "per-chat", Some("100"));
        assert_eq!(a, b, "per-chat dm_scope must collapse user into '*' (same chat_scope)");
    }

    #[test]
    fn session_key_unknown_falls_back_to_per_channel_peer() {
        let k = SessionKey::from_inbound("Arty", "alice", "telegram", "garbage", None);
        assert_eq!(k.eff_user, "alice");
        assert_eq!(k.eff_channel, "telegram");
    }

    /// T03 triage Point 5 regression: the same user_id writing in two
    /// different chats on the same platform must produce DIFFERENT session
    /// keys under the default "per-channel-peer" scope — previously both
    /// collapsed into the identical key (cross-chat context leak).
    #[test]
    fn session_key_different_chat_scope_differs_per_channel_peer() {
        let group_a = SessionKey::from_inbound("Arty", "alice", "telegram", "per-channel-peer", Some("100"));
        let group_b = SessionKey::from_inbound("Arty", "alice", "telegram", "per-channel-peer", Some("200"));
        assert_ne!(group_a, group_b, "different chat_scope must produce different SessionKey");
        assert_eq!(group_a.eff_chat_scope, Some("100".to_string()));
        assert_eq!(group_b.eff_chat_scope, Some("200".to_string()));
    }

    /// DM without a chat_scope (adapter context has no chat concept) must
    /// still produce a stable, non-panicking key — degrades to the bare
    /// (user, channel) pair, matching pre-fix behaviour.
    #[test]
    fn session_key_no_chat_scope_degrades_gracefully() {
        let k1 = SessionKey::from_inbound("Arty", "alice", "whatsapp", "per-channel-peer", None);
        let k2 = SessionKey::from_inbound("Arty", "alice", "whatsapp", "per-channel-peer", None);
        assert_eq!(k1, k2, "repeated calls with no chat_scope must be stable");
        assert_eq!(k1.eff_chat_scope, None);
    }
}
