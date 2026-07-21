//! Typed builder for SSE wire-protocol emission. Owns contextual state
//! (text-id counter, tool_name_map, current_agent) that the converter
//! previously kept as local mutable variables.
//!
//! Usage pattern: converter creates one `SseStreamWriter` per stream,
//! calls `build_*` methods to produce frame Strings, then forwards them
//! through the existing `send_and_buffer!` macro to the engine event
//! sender + StreamRegistry buffer.
//!
//! See docs/superpowers/specs/2026-05-07-s6.5-sse-codegen-design.md §6.

use std::collections::HashMap;

use opex_types::ids::{IterationId, MessageId, ParallelBatchId, ToolCallId};
use opex_types::sse::{DataSessionIdPayload, SseEvent, UsagePayload};

use crate::agent::stream_event::StreamEvent;

// ── From<StreamEvent> for pure-mapped variants ──────────────────────────

/// Pure mapping for StreamEvent variants that have a 1:1 SseEvent
/// counterpart with no contextual data needed.
///
/// Variants requiring contextual data (text id counter, tool_name_map,
/// agent_name) are NOT handled here — see SseStreamWriter::build_*.
/// `unimplemented!` panics serve as guard rails for misuse — tests
/// verify the right API is used. Production code goes through
/// SseStreamWriter, which never invokes the unimplemented branches.
impl From<StreamEvent> for SseEvent {
    fn from(e: StreamEvent) -> Self {
        match e {
            StreamEvent::ToolCallArgs { id, args_text } => SseEvent::ToolInputDelta {
                tool_call_id: id,
                input_text_delta: args_text,
            },
            StreamEvent::ToolResult { id, result } => SseEvent::ToolOutputAvailable {
                tool_call_id: id,
                output: result,
            },
            StreamEvent::File { url, media_type } => SseEvent::File { url, media_type },
            StreamEvent::ClarifyNeeded {
                clarify_id,
                question,
                choices,
                timeout_ms,
            } => SseEvent::ClarifyNeeded {
                clarify_id,
                question,
                choices,
                timeout_ms,
            },
            StreamEvent::ApprovalNeeded {
                approval_id,
                tool_name,
                tool_input,
                timeout_ms,
            } => SseEvent::ToolApprovalNeeded {
                approval_id,
                tool_name,
                tool_input,
                timeout_ms,
            },
            StreamEvent::ApprovalResolved {
                approval_id,
                action,
                modified_input,
            } => SseEvent::ToolApprovalResolved {
                approval_id,
                action,
                modified_input,
            },
            StreamEvent::Reconnecting { attempt, delay_ms } => {
                SseEvent::Reconnecting { attempt, delay_ms }
            }
            StreamEvent::Error(text) => SseEvent::Error { error_text: text },

            // Variants needing context — handled by SseStreamWriter methods:
            StreamEvent::SessionId { .. }
            | StreamEvent::MessageStart { .. }
            | StreamEvent::StepStart { .. }
            | StreamEvent::TextDelta(_)
            | StreamEvent::ToolCallStart { .. }
            | StreamEvent::Usage { .. }
            | StreamEvent::Finish { .. }
            | StreamEvent::RichCard { .. } => {
                unimplemented!(
                    "StreamEvent variant requires SseStreamWriter context — \
                     use the appropriate build_* method"
                )
            }

            // Dropped on wire (currently never reach SSE):
            StreamEvent::StepFinish { .. } | StreamEvent::AgentSwitch { .. } => {
                unimplemented!(
                    "StreamEvent variant is `continue`-skipped in current \
                     converter — caller must skip, not call From"
                )
            }
        }
    }
}

// ── SseStreamWriter ─────────────────────────────────────────────────────

/// Owns contextual state for SSE wire emission. Constructed once per
/// stream by `sse_converter::run_converter`; methods produce SSE wire
/// frame Strings that are forwarded through the `send_and_buffer!`
/// macro to the engine event sender + StreamRegistry buffer.
pub struct SseStreamWriter {
    text_id_counter: u32,
    current_text_id: Option<String>,
    tool_name_map: HashMap<String, String>,
    // Bug 18: remember the parallel_batch_id emitted with tool-input-start so
    // tool-input-available can carry the same value for UI grouping.
    tool_batch_id_map: HashMap<String, Option<ParallelBatchId>>,
    current_agent: String,
}

impl SseStreamWriter {
    pub fn new(initial_agent: String) -> Self {
        Self {
            text_id_counter: 0,
            current_text_id: None,
            tool_name_map: HashMap::new(),
            tool_batch_id_map: HashMap::new(),
            current_agent: initial_agent,
        }
    }

    pub fn set_agent(&mut self, agent: String) {
        self.current_agent = agent;
    }

    pub fn current_agent(&self) -> &str {
        &self.current_agent
    }

    /// Read-only access to the tool_name_map populated by
    /// `build_tool_input_start`. Used by `sse_converter.rs` for DB-side
    /// `accumulated_tools` aggregation (which keeps a parallel record
    /// outside the wire emission path).
    pub fn tool_name_for(&self, tool_call_id: &str) -> Option<String> {
        self.tool_name_map.get(tool_call_id).cloned()
    }

    pub fn build_session_id(
        &mut self,
        session_id: String,
        context_limit: Option<u32>,
    ) -> String {
        self.frame(&SseEvent::DataSessionId {
            data: DataSessionIdPayload {
                session_id,
                context_limit,
            },
            transient: true,
        })
    }

    pub fn build_start(&mut self, message_id: MessageId) -> String {
        self.frame(&SseEvent::Start {
            message_id,
            agent_name: self.current_agent.clone(),
        })
    }

    pub fn build_step_start(&mut self, iteration: IterationId) -> String {
        self.frame(&SseEvent::StepStart {
            step_id: format!("step_{}", iteration.index),
            message_id: iteration.message_id,
            agent_name: self.current_agent.clone(),
        })
    }

    /// Returns (Some(text_start_frame), text_delta_frame) on first delta
    /// of a block, or (None, text_delta_frame) on subsequent deltas.
    pub fn build_text_delta(&mut self, delta: String) -> (Option<String>, String) {
        let start_frame = if self.current_text_id.is_none() {
            self.text_id_counter += 1;
            let id = format!("text-{}", self.text_id_counter);
            self.current_text_id = Some(id.clone());
            Some(self.frame(&SseEvent::TextStart {
                id,
                agent_name: self.current_agent.clone(),
            }))
        } else {
            None
        };
        // SAFETY (logically): `current_text_id` was just set above if it was
        // None, and nothing in this function clears it between the assignment
        // and the clone below. The match stays defensive against future
        // refactors, but the previous synthetic-id fallback (`text-orphan-N`)
        // was removed because it produced a brand-new id the client had never
        // seen — the UI rendered it as a separate text block, duplicating
        // content. Skipping the delta entirely is the safer degradation: the
        // user just doesn't see this single chunk, the next delta recovers.
        let Some(id) = self.current_text_id.clone() else {
            tracing::error!("build_text_delta: current_text_id still None after assignment — skipping delta");
            return (start_frame, String::new());
        };
        let delta_frame = self.frame(&SseEvent::TextDelta { id, delta });
        (start_frame, delta_frame)
    }

    /// Returns Some(text_end_frame) if a text block was open, None otherwise.
    pub fn build_text_end_if_open(&mut self) -> Option<String> {
        let id = self.current_text_id.take()?;
        Some(self.frame(&SseEvent::TextEnd { id }))
    }

    pub fn build_tool_input_start(
        &mut self,
        tool_call_id: ToolCallId,
        tool_name: String,
        parallel_batch_id: Option<ParallelBatchId>,
    ) -> String {
        self.tool_name_map
            .insert(tool_call_id.as_str().to_string(), tool_name.clone());
        // Bug 18: persist so build_tool_input_available can forward it.
        self.tool_batch_id_map
            .insert(tool_call_id.as_str().to_string(), parallel_batch_id);
        self.frame(&SseEvent::ToolInputStart {
            tool_call_id,
            tool_name,
            agent_name: self.current_agent.clone(),
            parallel_batch_id,
        })
    }

    pub fn build_tool_input_delta(
        &mut self,
        tool_call_id: ToolCallId,
        args_text: String,
    ) -> String {
        self.frame(&SseEvent::ToolInputDelta {
            tool_call_id,
            input_text_delta: args_text,
        })
    }

    pub fn build_tool_input_available(
        &mut self,
        tool_call_id: ToolCallId,
        input: serde_json::Value,
    ) -> String {
        let tool_name = self
            .tool_name_map
            .get(tool_call_id.as_str())
            .cloned()
            .unwrap_or_default();
        // Bug 18: forward the parallel_batch_id recorded at tool-input-start.
        let parallel_batch_id = self
            .tool_batch_id_map
            .get(tool_call_id.as_str())
            .cloned()
            .unwrap_or(None);
        self.frame(&SseEvent::ToolInputAvailable {
            tool_call_id,
            tool_name,
            input,
            parallel_batch_id,
        })
    }

    pub fn build_finish(&mut self) -> String {
        // Reset text_id_counter for next iteration (matches converter behavior).
        self.text_id_counter = 0;
        self.current_text_id = None;
        self.frame(&SseEvent::Finish {
            agent_name: self.current_agent.clone(),
        })
    }

    pub fn build_error(&mut self, text: String) -> String {
        self.frame(&SseEvent::Error { error_text: text })
    }

    pub fn build_usage(
        &mut self,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: Option<u32>,
        cache_creation_tokens: Option<u32>,
        reasoning_tokens: Option<u32>,
    ) -> String {
        self.frame(&SseEvent::Usage(UsagePayload {
            input_tokens,
            output_tokens,
            agent_name: self.current_agent.clone(),
            cache_read_tokens,
            cache_creation_tokens,
            reasoning_tokens,
        }))
    }

    pub fn build_rich_card(
        &mut self,
        card_type: String,
        data: serde_json::Value,
    ) -> String {
        self.frame(&SseEvent::rich_card_from_stream(card_type, data))
    }

    /// Convenience for pure-mapped middle events. Delegates to
    /// `From<StreamEvent>`. Caller must NOT pass variants that require
    /// context (use the dedicated build_* method instead) — From panics.
    pub fn build_pure(&mut self, ev: StreamEvent) -> String {
        self.frame(&SseEvent::from(ev))
    }

    /// Serialize an SseEvent to JSON. The caller (`send_and_buffer!` macro
    /// in `sse_converter.rs`) wraps this string in `axum::response::sse::Event::data(...)`
    /// which adds the `id: <seq>\ndata: <json>\n\n` framing. Returning the
    /// raw JSON here avoids double-wrapping by axum.
    ///
    /// `&self` (immutable) since we no longer track a seq counter — the SSE
    /// event-id sequencing is handled by `StreamRegistry::push_event` in the
    /// macro, and axum writes it as the `id:` line. Made `&self` for clarity.
    fn frame(&self, ev: &SseEvent) -> String {
        serde_json::to_string(ev).expect("SseEvent must serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opex_types::approvals::ApprovalAction;
    use opex_types::ids::{ApprovalId, MessageId, ParallelBatchId, ToolCallId};
    use uuid::Uuid;

    #[test]
    fn from_tool_result_byte_equal_to_old_json() {
        let id = ToolCallId::from("tool_abc".to_string());
        let stream = StreamEvent::ToolResult {
            id: id.clone(),
            result: "ok".to_string(),
        };
        let sse = SseEvent::from(stream);
        let json = serde_json::to_string(&sse).unwrap();
        assert_eq!(
            json,
            r#"{"type":"tool-output-available","toolCallId":"tool_abc","output":"ok"}"#
        );
    }

    #[test]
    fn from_file_byte_equal() {
        let stream = StreamEvent::File {
            url: "/uploads/x.png".to_string(),
            media_type: "image/png".to_string(),
        };
        let json = serde_json::to_string(&SseEvent::from(stream)).unwrap();
        assert_eq!(
            json,
            r#"{"type":"file","url":"/uploads/x.png","mediaType":"image/png"}"#
        );
    }

    #[test]
    fn from_clarify_needed_wire_shape() {
        let id = uuid::Uuid::nil();
        let stream = StreamEvent::ClarifyNeeded {
            clarify_id: id,
            question: "Which format?".to_string(),
            choices: vec!["JSON".to_string(), "CSV".to_string()],
            timeout_ms: 60_000_u64,
        };
        let json = serde_json::to_string(&SseEvent::from(stream)).unwrap();
        assert!(json.contains(r#""type":"clarify-needed""#));
        assert!(json.contains(r#""clarifyId":"00000000-0000-0000-0000-000000000000""#));
        assert!(json.contains(r#""question":"Which format?""#));
        assert!(json.contains(r#""choices":["JSON","CSV"]"#));
        assert!(json.contains(r#""timeoutMs":60000"#));
    }

    #[test]
    fn from_approval_needed_with_u64_timeout() {
        let aid = ApprovalId::from(Uuid::nil());
        let stream = StreamEvent::ApprovalNeeded {
            approval_id: aid,
            tool_name: "code_exec".to_string(),
            tool_input: serde_json::json!({"cmd": "ls"}),
            timeout_ms: 300_000_u64,
        };
        let json = serde_json::to_string(&SseEvent::from(stream)).unwrap();
        assert!(json.contains(r#""type":"tool-approval-needed""#));
        assert!(json.contains(r#""approvalId":"00000000-0000-0000-0000-000000000000""#));
        assert!(json.contains(r#""toolName":"code_exec""#));
        assert!(json.contains(r#""timeoutMs":300000"#));
    }

    #[test]
    fn from_approval_resolved_with_action_string() {
        let aid = ApprovalId::from(Uuid::nil());
        let stream = StreamEvent::ApprovalResolved {
            approval_id: aid,
            action: ApprovalAction::Approved,
            modified_input: None,
        };
        let json = serde_json::to_string(&SseEvent::from(stream)).unwrap();
        assert!(json.contains(r#""action":"approved""#));
        // None modified_input must be omitted (skip_serializing_if).
        assert!(!json.contains("modifiedInput"));
    }

    #[test]
    fn from_reconnecting_keeps_snake_case_delay_ms() {
        let stream = StreamEvent::Reconnecting {
            attempt: 2,
            delay_ms: 500_u64,
        };
        let json = serde_json::to_string(&SseEvent::from(stream)).unwrap();
        assert_eq!(
            json,
            r#"{"type":"reconnecting","attempt":2,"delay_ms":500}"#
        );
    }

    #[test]
    fn from_error_byte_equal() {
        let json = serde_json::to_string(&SseEvent::from(StreamEvent::Error(
            "boom".to_string(),
        )))
        .unwrap();
        assert_eq!(json, r#"{"type":"error","errorText":"boom"}"#);
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_text_delta_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::TextDelta("hi".to_string()));
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_session_id_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::SessionId {
            session_id: "s1".to_string(),
            context_limit: 8000,
        });
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_message_start_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::MessageStart {
            message_id: MessageId::from(Uuid::nil()),
        });
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_step_start_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::StepStart {
            iteration: opex_types::ids::IterationId {
                index: 0,
                message_id: MessageId::from(Uuid::nil()),
            },
        });
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_tool_call_start_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::ToolCallStart {
            id: ToolCallId::from("tc1".to_string()),
            name: "code_exec".to_string(),
            parallel_batch_id: None,
        });
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_usage_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            reasoning_tokens: None,
        });
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_finish_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::Finish {
            finish_reason: "stop".to_string(),
            continuation: false,
        });
    }

    #[test]
    #[should_panic(expected = "requires SseStreamWriter context")]
    fn from_rich_card_panics_use_writer_instead() {
        let _ = SseEvent::from(StreamEvent::RichCard {
            card_type: "table".to_string(),
            data: serde_json::json!({}),
        });
    }

    #[test]
    #[should_panic(expected = "caller must skip")]
    fn from_step_finish_panics_caller_must_skip() {
        let _ = SseEvent::from(StreamEvent::StepFinish {
            step_id: "step_0".to_string(),
            finish_reason: "tool_use".to_string(),
        });
    }

    #[test]
    #[should_panic(expected = "caller must skip")]
    fn from_agent_switch_panics_caller_must_skip() {
        let _ = SseEvent::from(StreamEvent::AgentSwitch {
            agent_name: "Opex".to_string(),
        });
    }

    #[test]
    fn writer_session_id_with_context_limit() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let json = w.build_session_id("sess-1".to_string(), Some(8000));
        assert!(json.contains(r#""type":"data-session-id""#));
        assert!(json.contains(r#""sessionId":"sess-1""#));
        assert!(json.contains(r#""contextLimit":8000"#));
        assert!(json.contains(r#""transient":true"#));
    }

    #[test]
    fn writer_session_id_without_context_limit() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let json = w.build_session_id("sess-1".to_string(), None);
        // context_limit is Option, skip_serializing_if = is_none.
        assert!(!json.contains("contextLimit"));
    }

    #[test]
    fn writer_start_includes_agent_name_and_message_id() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let mid = MessageId::from(Uuid::nil());
        let json = w.build_start(mid);
        assert!(json.contains(r#""type":"start""#));
        assert!(json.contains(r#""messageId":"00000000-0000-0000-0000-000000000000""#));
        assert!(json.contains(r#""agentName":"Opex""#));
    }

    #[test]
    fn writer_step_start_formats_step_id_and_includes_agent() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let iter = IterationId {
            index: 3,
            message_id: MessageId::from(Uuid::nil()),
        };
        let json = w.build_step_start(iter);
        assert!(json.contains(r#""stepId":"step_3""#));
        assert!(json.contains(r#""agentName":"Opex""#));
    }

    #[test]
    fn writer_text_delta_emits_start_then_delta_on_first_call() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let (start_json, delta_json) = w.build_text_delta("hello".to_string());
        let s_json = start_json.expect("first delta opens a block");
        assert!(s_json.contains(r#""type":"text-start""#));
        assert!(s_json.contains(r#""id":"text-1""#));
        assert!(delta_json.contains(r#""type":"text-delta""#));
        assert!(delta_json.contains(r#""id":"text-1""#));
        assert!(delta_json.contains(r#""delta":"hello""#));
    }

    #[test]
    fn writer_text_delta_subsequent_calls_reuse_block_id() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let _ = w.build_text_delta("first".to_string());
        let (start_json, delta_json) = w.build_text_delta(" second".to_string());
        assert!(start_json.is_none(), "second delta does not open new block");
        assert!(delta_json.contains(r#""id":"text-1""#));
    }

    #[test]
    fn writer_text_end_if_open_emits_when_open_else_none() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        assert!(w.build_text_end_if_open().is_none());
        let _ = w.build_text_delta("x".to_string());
        let e_json = w.build_text_end_if_open().expect("block was open");
        assert!(e_json.contains(r#""type":"text-end""#));
        assert!(e_json.contains(r#""id":"text-1""#));
        // Block now closed:
        assert!(w.build_text_end_if_open().is_none());
    }

    #[test]
    fn writer_tool_input_start_remembers_tool_name_for_later_lookup() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let id = ToolCallId::from("tc-1".to_string());
        let _ = w.build_tool_input_start(id.clone(), "code_exec".to_string(), None);
        // Synthetic tool-input-available looks up tool_name via map:
        let a_json =
            w.build_tool_input_available(id, serde_json::json!({"cmd": "ls"}));
        assert!(a_json.contains(r#""toolName":"code_exec""#));
    }

    #[test]
    fn writer_tool_input_start_with_parallel_batch_id() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let id = ToolCallId::from("tc-1".to_string());
        let batch = ParallelBatchId::from(Uuid::nil());
        let json = w.build_tool_input_start(
            id,
            "code_exec".to_string(),
            Some(batch),
        );
        assert!(json.contains(r#""parallelBatchId":"00000000-0000-0000-0000-000000000000""#));
    }

    #[test]
    fn writer_tool_input_start_without_parallel_batch_id_omits_field() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let id = ToolCallId::from("tc-1".to_string());
        let json = w.build_tool_input_start(id, "code_exec".to_string(), None);
        assert!(!json.contains("parallelBatchId"));
    }

    #[test]
    fn writer_finish_includes_agent_and_resets_text_state() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let _ = w.build_text_delta("x".to_string()); // opens text block
        let json = w.build_finish();
        assert!(json.contains(r#""type":"finish""#));
        assert!(json.contains(r#""agentName":"Opex""#));
        // After finish, text counter resets so next stream starts at text-1:
        let (start_json, _) = w.build_text_delta("y".to_string());
        let s_json = start_json.unwrap();
        assert!(s_json.contains(r#""id":"text-1""#));
    }

    #[test]
    fn writer_usage_includes_agent_and_omits_none_extended_fields() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let json = w.build_usage(100, 50, None, None, None);
        assert!(json.contains(r#""agentName":"Opex""#));
        assert!(!json.contains("cacheReadTokens"));
        assert!(!json.contains("cacheCreationTokens"));
        assert!(!json.contains("reasoningTokens"));
    }

    #[test]
    fn writer_usage_emits_extended_fields_when_present() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let json = w.build_usage(100, 50, Some(20), Some(5), Some(3));
        assert!(json.contains(r#""cacheReadTokens":20"#));
        assert!(json.contains(r#""cacheCreationTokens":5"#));
        assert!(json.contains(r#""reasoningTokens":3"#));
    }

    #[test]
    fn writer_rich_card_known_table_routes_correctly() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let json = w.build_rich_card(
            "table".to_string(),
            serde_json::json!({"columns": ["a"], "rows": []}),
        );
        assert!(json.contains(r#""type":"rich-card""#));
        assert!(json.contains(r#""cardType":"table""#));
    }

    #[test]
    fn writer_rich_card_unknown_falls_back_to_other() {
        let mut w = SseStreamWriter::new("Opex".to_string());
        let json = w.build_rich_card(
            "future_card".to_string(),
            serde_json::json!({"foo": 1}),
        );
        assert!(json.contains(r#""cardType":"future_card""#));
    }

    #[test]
    fn writer_frame_returns_raw_json_no_sse_wire_prefix() {
        // Guards against re-introducing the double-wrapping bug:
        // frame() must return ONLY JSON. axum::Event::data(...) adds
        // the `id:`/`data:` framing in the converter macro.
        let mut w = SseStreamWriter::new("Opex".to_string());
        let f1 = w.build_session_id("s1".to_string(), None);
        let f2 = w.build_start(MessageId::from(Uuid::nil()));
        assert!(f1.starts_with('{'), "frame must be raw JSON, got: {f1}");
        assert!(f2.starts_with('{'), "frame must be raw JSON, got: {f2}");
        assert!(!f1.contains("\ndata:"));
        assert!(!f2.contains("\ndata:"));
        assert_ne!(f1, f2);
    }
}
