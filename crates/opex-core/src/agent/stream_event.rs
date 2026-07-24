//! Leaf module for the `StreamEvent` enum — SSE protocol payload.
//!
//! Extracted from `engine.rs` so integration tests in
//! `crates/opex-core/tests/` can reach `StreamEvent` via the lib
//! facade (`opex_core::agent::stream_event::StreamEvent`) without
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
#[allow(dead_code)] // Some variants (AgentSwitch) and field bindings are part of the
                    // wire protocol but pattern-matched with `_` in the converter.
pub enum StreamEvent {
    /// Session ID resolved/created by `build_context` — emitted first so the UI can track it.
    /// `context_limit` is the resolved token budget for this model (from /api/show or heuristic).
    SessionId { session_id: String, context_limit: u32 },
    /// First-iteration legacy event — pre-allocated assistant message id.
    /// `message_id` is `MessageId` (Uuid newtype) post-T5. Wire format is
    /// unchanged: `sse_converter.rs` calls `.to_string()` to produce the
    /// legacy `"messageId": "<uuid>"` field.
    MessageStart { message_id: opex_types::ids::MessageId },
    /// `iteration` carries the (index, message_id) pair: `index` is the
    /// 0-based tool-loop iteration number; `message_id` is the pre-allocated
    /// UUID for the assistant DB row this iteration will produce. Frontend
    /// uses the message_id to open a new live ChatMessage with the SAME id
    /// the DB row will eventually receive — enabling pure ID-based dedup
    /// between live overlay and history without content-matching heuristics.
    ///
    /// Wire format on the SSE side stays `stepId: "step_{N}"` (string) +
    /// `messageId: "<uuid>"` (string) — conversion happens manually in
    /// `sse_converter.rs`, NOT via Serde derive.
    StepStart { iteration: opex_types::ids::IterationId },
    TextDelta(String),
    /// `parallel_batch_id` — `Some(id)` when this tool call belongs to a
    /// parallel batch (≥2 tool calls executed concurrently in one turn);
    /// `None` for sequential / single-tool turns. Frontend ignores it
    /// initially; analytics queries `messages.parallel_batch_id` (m047)
    /// to group tools that ran in the same batch. See spec
    /// `docs/superpowers/specs/2026-05-07-s2-identity-first-stream-objects-design.md` (T3).
    ///
    /// T6: `id` is `ToolCallId` (newtype over `String`). Wire format on the
    /// SSE side is unchanged — `sse_converter.rs` calls `.as_str()` to
    /// produce the legacy `"toolCallId": "<provider-supplied id>"` field.
    ToolCallStart {
        id: opex_types::ids::ToolCallId,
        name: String,
        parallel_batch_id: Option<opex_types::ids::ParallelBatchId>,
    },
    ToolCallArgs {
        id: opex_types::ids::ToolCallId,
        args_text: String,
    },
    ToolResult {
        id: opex_types::ids::ToolCallId,
        result: String,
    },
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
        /// Display name for the attachment (e.g. "image.png"). Present when
        /// the producer knows it; the UI falls back to a media-type label.
        filename: Option<String>,
    },
    Finish {
        finish_reason: String,
        continuation: bool,
    },
    /// Clarification needed: agent is waiting for user input before continuing.
    ///
    /// `clarify_id` uniquely identifies this pending clarification request so
    /// the UI can match a user response to the waiting engine call.
    ClarifyNeeded {
        clarify_id: uuid::Uuid,
        question: String,
        choices: Vec<String>,
        timeout_ms: u64,
    },
    /// Approval needed: a tool call is waiting for human approval.
    ///
    /// `approval_id` is `ApprovalId` (Uuid newtype) post-T4. Wire format on
    /// the SSE side is unchanged — `sse_converter.rs` calls `.to_string()`
    /// to produce the legacy `"approvalId": "<uuid>"` field.
    ApprovalNeeded {
        approval_id: opex_types::ids::ApprovalId,
        tool_name: String,
        tool_input: serde_json::Value,
        timeout_ms: u64,
    },
    /// Approval resolved: a pending approval was approved, rejected, or timed out.
    ApprovalResolved {
        approval_id: opex_types::ids::ApprovalId,
        action: opex_types::approvals::ApprovalAction,
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
    /// Token usage from the most recent LLM response. Emitted by pipeline/execute
    /// after each LLM call so the UI can display a context window indicator.
    /// Extended fields are subsets of input/output (NOT additive):
    /// - `cache_read_tokens` ⊆ `input_tokens` (cost ×0.1 Anthropic, ×0.5 OpenAI)
    /// - `cache_creation_tokens` ⊆ `input_tokens` (cost ×1.25, Anthropic only)
    /// - `reasoning_tokens` ⊆ `output_tokens` (OpenAI o1/o3, Gemini thinking)
    Usage {
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: Option<u32>,
        cache_creation_tokens: Option<u32>,
        reasoning_tokens: Option<u32>,
    },
}
