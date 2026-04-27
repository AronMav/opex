use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::FromRef;
#[cfg(test)]
use sqlx::PgPool;

use crate::agent::handle::AgentHandle;
use crate::channels::access::AccessGuard;
use crate::gateway::clusters::{
    AgentCore, AuthServices, ChannelBus, ConfigServices, InfraServices, StatusMonitor,
};

/// Tracks which agents are currently processing a request.
/// Used to replay `agent_processing` state to newly connected WS clients.
pub type ProcessingTracker = Arc<std::sync::RwLock<HashMap<String, serde_json::Value>>>;

pub type AgentMap = Arc<tokio::sync::RwLock<HashMap<String, AgentHandle>>>;
pub type AccessGuardMap = Arc<tokio::sync::RwLock<HashMap<String, Arc<AccessGuard>>>>;

/// A channel adapter currently connected via WebSocket.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectedChannel {
    pub agent_name: String,
    pub channel_id: Option<uuid::Uuid>,
    pub channel_type: String,
    pub display_name: String,
    pub adapter_version: String,
    pub connected_at: chrono::DateTime<chrono::Utc>,
    /// Updated on every inbound message; used by stale-channel detector.
    pub last_activity: chrono::DateTime<chrono::Utc>,
}

pub type ConnectedChannelsRegistry = Arc<tokio::sync::RwLock<Vec<ConnectedChannel>>>;

/// Atomic counters for channel polling diagnostics.
/// Exposed via GET /api/doctor for "bot not responding" troubleshooting.
pub struct PollingDiagnostics {
    pub messages_in: AtomicU64,
    pub messages_out: AtomicU64,
    pub last_inbound_at: AtomicU64,
    pub last_outbound_at: AtomicU64,
}

impl PollingDiagnostics {
    pub fn new() -> Self {
        Self {
            messages_in: AtomicU64::new(0),
            messages_out: AtomicU64::new(0),
            last_inbound_at: AtomicU64::new(0),
            last_outbound_at: AtomicU64::new(0),
        }
    }

    pub fn record_inbound(&self) {
        self.messages_in.fetch_add(1, Ordering::Relaxed);
        self.last_inbound_at.store(
            chrono::Utc::now().timestamp() as u64,
            Ordering::Relaxed,
        );
    }

    pub fn record_outbound(&self) {
        self.messages_out.fetch_add(1, Ordering::Relaxed);
        self.last_outbound_at.store(
            chrono::Utc::now().timestamp() as u64,
            Ordering::Relaxed,
        );
    }
}

/// Cached WAN (public) IP address with CGNAT classification and a fetch timestamp.
#[derive(Clone)]
pub struct WanIpCache {
    pub ip: String,
    pub is_cgnat: bool,
    pub fetched_at: std::time::Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub agents:   AgentCore,
    pub auth:     AuthServices,
    pub infra:    InfraServices,
    pub channels: ChannelBus,
    pub config:   ConfigServices,
    pub status:   StatusMonitor,
}

impl FromRef<AppState> for AgentCore {
    fn from_ref(s: &AppState) -> Self { s.agents.clone() }
}
impl FromRef<AppState> for AuthServices {
    fn from_ref(s: &AppState) -> Self { s.auth.clone() }
}
impl FromRef<AppState> for InfraServices {
    fn from_ref(s: &AppState) -> Self { s.infra.clone() }
}
impl FromRef<AppState> for ChannelBus {
    fn from_ref(s: &AppState) -> Self { s.channels.clone() }
}
impl FromRef<AppState> for ConfigServices {
    fn from_ref(s: &AppState) -> Self { s.config.clone() }
}
impl FromRef<AppState> for StatusMonitor {
    fn from_ref(s: &AppState) -> Self { s.status.clone() }
}

/// Shared dependencies needed to start new agents at runtime (from CRUD endpoints).
pub struct AgentDeps {
    pub mcp: Option<Arc<crate::mcp::McpRegistry>>,
    pub workspace_dir: String,
    pub toolgate_url: Option<String>,
    pub sandbox: Option<Arc<crate::containers::sandbox::CodeSandbox>>,
    pub tool_embed_cache: Arc<crate::tools::embedding::ToolEmbeddingCache>,
    pub penalty_cache: Arc<crate::db::tool_quality::PenaltyCache>,
    pub audit_queue: Arc<crate::db::audit_queue::AuditQueue>,
}

#[cfg(test)]
impl AgentDeps {
    /// Construct a minimal `AgentDeps` for unit tests.
    /// Uses a lazy (never-connecting) pool so no real DB is needed.
    pub fn test_new() -> Self {
        let db = PgPool::connect_lazy("postgres://invalid").expect("lazy pool");
        Self {
            mcp: None,
            workspace_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            toolgate_url: None,
            sandbox: None,
            tool_embed_cache: Arc::new(crate::tools::embedding::ToolEmbeddingCache::new()),
            penalty_cache: Arc::new(crate::db::tool_quality::PenaltyCache::new(db.clone())),
            audit_queue: Arc::new(crate::db::audit_queue::AuditQueue::new(db)),
        }
    }
}

