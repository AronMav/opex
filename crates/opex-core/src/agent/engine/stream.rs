//! `ProcessingPhase` wire enum + `ProcessingGuard` RAII tracker + `StreamEvent` re-export.

use anyhow::Result;
use opex_types::IncomingMessage;

use super::AgentEngine;

/// Phase 62 RES-01: `StreamEvent` extracted to a leaf module
/// (`agent/stream_event.rs`) so the lib facade can expose it to integration
/// tests without cascading the whole `engine.rs` dependency tree. Re-exported
/// here so every existing `crate::agent::engine::StreamEvent` path resolves.
pub use crate::agent::stream_event::StreamEvent;


/// Status phases emitted during message processing.
#[derive(Debug, Clone)]
#[allow(dead_code)] // CallingTool/Composing are part of the wire enum;
                    // emitted via channel adapters that build them outside this crate.
pub enum ProcessingPhase {
    Thinking,
    CallingTool(String),
    Composing,
}

impl ProcessingPhase {
    /// Convert to wire format: (`phase_name`, `optional_tool_name`).
    pub fn to_wire(&self) -> (String, Option<String>) {
        match self {
            ProcessingPhase::Thinking => ("thinking".to_string(), None),
            ProcessingPhase::CallingTool(name) => ("calling_tool".to_string(), Some(name.clone())),
            ProcessingPhase::Composing => ("composing".to_string(), None),
        }
    }
}

/// RAII guard: inserts into processing tracker on creation, removes + broadcasts "end" on drop.
/// Uses `session_id` as tracker key (not `agent_name`) to support concurrent sessions per agent.
pub(crate) struct ProcessingGuard {
    tx: Option<tokio::sync::broadcast::Sender<String>>,
    processing_tracker: Option<crate::gateway::ProcessingTracker>,
    agent_name: String,
    /// Tracker key — `session_id` for unique identification across concurrent sessions.
    tracker_key: String,
    session_id: Option<String>,
}

impl ProcessingGuard {
    pub(crate) fn new(
        tx: Option<tokio::sync::broadcast::Sender<String>>,
        tracker: Option<crate::gateway::ProcessingTracker>,
        agent_name: String,
        start_event: &serde_json::Value,
    ) -> Self {
        let session_id = start_event.get("session_id").and_then(|v| v.as_str()).map(std::string::ToString::to_string);
        // Use session_id as key (supports multiple concurrent sessions for same agent).
        // Fallback to agent_name if session_id is missing (shouldn't happen).
        let tracker_key = session_id.clone().unwrap_or_else(|| agent_name.clone());
        if let Some(ref t) = tracker
            && let Ok(mut map) = t.write() {
                map.insert(tracker_key.clone(), start_event.clone());
                tracing::debug!(agent = %agent_name, key = %tracker_key, "processing_tracker: inserted");
            }
        Self { tx, processing_tracker: tracker, agent_name, tracker_key, session_id }
    }
}

impl Drop for ProcessingGuard {
    fn drop(&mut self) {
        if let Some(ref tracker) = self.processing_tracker
            && let Ok(mut map) = tracker.write() {
                map.remove(&self.tracker_key);
            }
        if let Some(ref tx) = self.tx {
            tx.send(
                opex_types::ws::WsEvent::AgentProcessing {
                    agent: self.agent_name.clone(),
                    status: "end".to_string(),
                    session_id: self.session_id.clone(),
                    channel: None,
                }
                .to_json(),
            )
            .ok();
        }
    }
}

impl AgentEngine {
    /// Handle an incoming message: build context, call LLM, execute tools, return response.
    pub async fn handle(&self, msg: &IncomingMessage) -> Result<String> {
        // No external canceller for this convenience wrapper — pass a fresh,
        // never-cancelled token.
        self.handle_with_status(msg, None, None, tokio_util::sync::CancellationToken::new()).await
    }

}
