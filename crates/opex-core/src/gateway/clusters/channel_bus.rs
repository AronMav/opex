use std::sync::Arc;

use tokio_util::task::TaskTracker;

use crate::gateway::state::ConnectedChannelsRegistry;
use crate::gateway::stream_registry::StreamRegistry;

// ── ChannelBus ─────────────────────────────────────────────────────────────
// Groups all real-time communication primitives: the connected-channel
// registry, log/UI broadcast senders, SSE stream registry, and background
// task tracker for graceful shutdown.

#[derive(Clone)]
pub struct ChannelBus {
    pub connected_channels: ConnectedChannelsRegistry,
    pub log_tx:             tokio::sync::broadcast::Sender<String>,
    pub ui_event_tx:        tokio::sync::broadcast::Sender<String>,
    pub stream_registry:    Arc<StreamRegistry>,
    pub bg_tasks:           Arc<TaskTracker>,
}

impl ChannelBus {
    pub fn new(
        connected_channels: ConnectedChannelsRegistry,
        log_tx: tokio::sync::broadcast::Sender<String>,
        ui_event_tx: tokio::sync::broadcast::Sender<String>,
        stream_registry: Arc<StreamRegistry>,
        bg_tasks: Arc<TaskTracker>,
    ) -> Self {
        Self { connected_channels, log_tx, ui_event_tx, stream_registry, bg_tasks }
    }

    /// Construct a minimal `ChannelBus` for unit tests.
    /// Uses a lazy (non-connecting) pool — no live DB is required.
    #[cfg(test)]
    pub fn test_new() -> Self {
        use sqlx::PgPool;

        let (log_tx, _) = tokio::sync::broadcast::channel(16);
        let (ui_event_tx, _) = tokio::sync::broadcast::channel(16);
        let db = PgPool::connect_lazy("postgres://invalid").unwrap();
        Self {
            connected_channels: Arc::new(tokio::sync::RwLock::new(Vec::new())),
            log_tx,
            ui_event_tx,
            stream_registry: Arc::new(StreamRegistry::new(db)),
            bg_tasks: Arc::new(TaskTracker::new()),
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn channel_bus_broadcast_senders_work() {
        let bus = ChannelBus::test_new();
        let _rx = bus.log_tx.subscribe();
        let _rx2 = bus.ui_event_tx.subscribe();
        // No panic = success
    }

    #[tokio::test]
    async fn channel_bus_bg_tasks_tracks_spawned_work() {
        let bus = ChannelBus::test_new();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        bus.bg_tasks.spawn(async move {
            let _ = tx.send(());
        });
        bus.bg_tasks.close();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            bus.bg_tasks.wait(),
        )
        .await
        .expect("bg_tasks should complete");
        rx.await.expect("spawned task ran");
    }
}
