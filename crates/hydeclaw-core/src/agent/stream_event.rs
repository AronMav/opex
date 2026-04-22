//! Leaf module for the `StreamEvent` enum — SSE protocol payload.
//!
//! Extracted from `engine.rs` so integration tests in
//! `crates/hydeclaw-core/tests/` can reach `StreamEvent` via the lib
//! facade (`hydeclaw_core::agent::stream_event::StreamEvent`) without
//! the facade cascading the whole `engine.rs` module tree (dozens of
//! `super::*` imports — secrets, providers, tool_loop, workspace, …).
//!
//! `engine.rs` preserves its public API by doing `pub use
//! stream_event::StreamEvent;` in the same module namespace, so every
//! existing `crate::agent::engine::StreamEvent` path continues to resolve.
//!
//! Dependencies: stdlib + `serde_json::Value` only. NO `crate::*` imports.

/// Events emitted during SSE streaming (AI SDK UI Message Stream Protocol v1).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum StreamEvent {
    /// Session ID resolved/created by `build_context` — emitted first so the UI can track it.
    SessionId(String),
    MessageStart { message_id: String },
    StepStart { step_id: String },
    TextDelta(String),
    ToolCallStart { id: String, name: String },
    ToolCallArgs { id: String, args_text: String },
    ToolResult { id: String, result: String },
    StepFinish { step_id: String, finish_reason: String },
    /// Rich card embedded inline in the message stream (tables, metrics, etc.).
    RichCard {
        card_type: String,
        data: serde_json::Value,
    },
    /// File/media attachment (image, audio, etc.) — displayed inline in UI chat.
    File {
        url: String,
        media_type: String,
    },
    Finish {
        finish_reason: String,
        continuation: bool,
    },
    /// Approval needed: a tool call is waiting for human approval.
    ApprovalNeeded {
        approval_id: String,
        tool_name: String,
        tool_input: serde_json::Value,
        timeout_ms: u64,
    },
    /// Approval resolved: a pending approval was approved, rejected, or timed out.
    ApprovalResolved {
        approval_id: String,
        action: String, // "approved" | "rejected" | "timeout_rejected"
        modified_input: Option<serde_json::Value>,
    },
    /// Internal event: signals that a different agent is now responding (multi-agent session).
    /// Converter task updates `current_responding_agent`; no SSE is emitted to the client.
    /// Retained for API compatibility — not currently emitted.
    AgentSwitch {
        agent_name: String,
    },
    Error(String),
    /// LLM deadline retry: model timed out and is being retried after `delay_ms`.
    Reconnecting { attempt: u32, delay_ms: u64 },
}
