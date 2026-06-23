use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SubagentLogEntry {
    pub iteration: usize,
    pub timestamp: DateTime<Utc>,
    pub tool_calls: Vec<String>,
    pub content_preview: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SubagentHandle {
    pub id: String,
    pub task: String,
    pub status: SubagentStatus,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub log: Vec<SubagentLogEntry>,
    #[serde(skip)]
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Running,
}

/// Lightweight summary for cancellation checks on shutdown.
#[derive(Debug, Clone)]
pub struct SubagentSummary {
    pub id: String,
    pub status: SubagentStatus,
}

/// Per-agent registry of subagent handles. Clone-safe (inner Arc).
#[derive(Debug, Clone, Default)]
pub struct SubagentRegistry {
    inner: Arc<RwLock<HashMap<String, Arc<RwLock<SubagentHandle>>>>>,
}

impl SubagentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn get(&self, id: &str) -> Option<Arc<RwLock<SubagentHandle>>> {
        self.inner.read().await.get(id).cloned()
    }

    /// List summaries for cancellation on shutdown.
    pub async fn list_summary(&self) -> Vec<SubagentSummary> {
        let arcs: Vec<Arc<RwLock<SubagentHandle>>> = {
            self.inner.read().await.values().cloned().collect()
        };
        let mut result = Vec::with_capacity(arcs.len());
        for h in &arcs {
            let h = h.read().await;
            result.push(SubagentSummary {
                id: h.id.clone(),
                status: h.status,
            });
        }
        result
    }

}
