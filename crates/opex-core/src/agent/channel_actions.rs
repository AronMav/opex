use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};

/// Generic action that the engine can send to the channel adapter for execution.
/// Channel-agnostic: `name` is the action type, `params` and `context` are opaque JSON.
#[derive(Debug)]
pub struct ChannelAction {
    /// Action name: "react", "pin", "unpin", "edit", "delete", "reply",
    /// "`send_message`", "`send_voice`", etc.
    pub name: String,
    /// Action-specific parameters (e.g. {"emoji": "👍"}, {"text": "..."}).
    pub params: serde_json::Value,
    /// Opaque context echoed from the incoming message (e.g. {"`chat_id"`: 123, "`message_id"`: 42}).
    pub context: serde_json::Value,
    /// Reply channel for the action result.
    pub reply: oneshot::Sender<Result<(), String>>,
    /// Target channel type (e.g. "telegram", "webhook"). None = first available.
    pub target_channel: Option<String>,
}

/// Per-channel sender for engine → channel adapter.
pub type ChannelActionRx = mpsc::Receiver<ChannelAction>;

/// Bounded queue capacity per channel adapter.
/// Large enough for burst but prevents OOM from binary payloads (images/audio).
const CHANNEL_ACTION_CAPACITY: usize = 64;

/// Multi-channel dispatcher: routes actions to the correct channel adapter.
/// Key = "{`channel_type}:{uuid`}" (e.g. "telegram:abc-123").
#[derive(Clone)]
pub struct ChannelActionRouter {
    channels: Arc<RwLock<HashMap<String, mpsc::Sender<ChannelAction>>>>,
}

impl ChannelActionRouter {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Register a new channel adapter. Returns (`connection_id`, receiver).
    /// Uses unique connection ID to prevent race conditions on reconnect.
    pub async fn subscribe(&self, channel_type: &str) -> (String, ChannelActionRx) {
        let conn_id = format!("{}:{}", channel_type, uuid::Uuid::new_v4());
        let (tx, rx) = mpsc::channel(CHANNEL_ACTION_CAPACITY);
        self.channels.write().await.insert(conn_id.clone(), tx);
        (conn_id, rx)
    }

    /// Unregister a channel adapter by connection ID (called on WS disconnect).
    pub async fn unsubscribe(&self, conn_id: &str) {
        self.channels.write().await.remove(conn_id);
    }

    /// Send an action to the appropriate channel.
    /// If `target_channel` is set, routes to that specific channel.
    /// Otherwise sends to the first available channel.
    pub async fn send(&self, action: ChannelAction) -> Result<(), String> {
        let channels = self.channels.read().await;
        let target = action.target_channel.clone();

        if let Some(ref target_type) = target {
            if let Some((_, tx)) = channels.iter().find(|(k, _)| k.starts_with(&format!("{target_type}:"))) {
                try_send_action(tx, action, target_type)?;
                return Ok(());
            }
            return Err(format!("channel '{target_type}' not connected"));
        }

        // No target specified — send to first available
        if let Some((_, tx)) = channels.iter().next() {
            try_send_action(tx, action, "default")?;
            return Ok(());
        }

        Err("no channels connected".to_string())
    }

}

/// Try to send an action to a bounded channel, formatting errors consistently.
fn try_send_action(
    tx: &mpsc::Sender<ChannelAction>,
    action: ChannelAction,
    label: &str,
) -> Result<(), String> {
    tx.try_send(action).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => format!("channel '{label}' queue full ({CHANNEL_ACTION_CAPACITY})"),
        mpsc::error::TrySendError::Closed(_) => format!("channel '{label}' disconnected"),
    })
}
