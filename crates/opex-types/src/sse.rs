//! SSE wire-protocol types for Core ↔ UI streaming.
//!
//! These types are codegen'd into TypeScript via ts-rs (dest = "ui-sse",
//! → ui/src/types/sse.generated.ts). Wire format invariants verified
//! against `sse_converter.rs` line-by-line (S6.5 spec §5):
//!
//! - Discriminator `"type"` is kebab-case (e.g., "text-delta")
//! - Field names are camelCase (e.g., "toolCallId") with one exception:
//!   `delay_ms` on Reconnecting is snake_case (pre-existing wire shape)
//! - Most fields are required (always emitted by the converter); only
//!   genuinely conditional fields are Option<T> + skip_serializing_if
//! - `agent_name` is required on most events because the converter
//!   ALWAYS emits the currently-responding agent
//!
//! ID newtypes (MessageId, ToolCallId, ApprovalId, ParallelBatchId) do
//! NOT have a ts-rs derive in `ids.rs` — they use #[serde(transparent)]
//! at runtime (uuid → string). For ts-rs codegen we explicitly override
//! each ID field with `#[ts(type = "string")]` so generated TS sees
//! plain `string` types without needing TS derives on the ID newtypes
//! themselves.
//!
//! Round-trip lock-in via fixtures in T7.

use crate::approvals::ApprovalAction;
use crate::ids::{ApprovalId, MessageId, ParallelBatchId, ToolCallId};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SseEvent {
    /// First event of any stream. transient: true is always emitted.
    DataSessionId {
        data: DataSessionIdPayload,
        transient: bool,
    },
    /// Pre-allocated assistant message ID.
    Start {
        #[serde(rename = "messageId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        message_id: MessageId,
        #[serde(rename = "agentName")]
        agent_name: String,
    },
    /// Tool-loop iteration boundary. step_id is `format!("step_{}", iteration.index)`.
    StepStart {
        #[serde(rename = "stepId")]
        step_id: String,
        #[serde(rename = "messageId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        message_id: MessageId,
        #[serde(rename = "agentName")]
        agent_name: String,
    },
    /// Synthetic text-block opener (no StreamEvent counterpart).
    TextStart {
        id: String,
        #[serde(rename = "agentName")]
        agent_name: String,
    },
    /// Streaming text chunk. id matches most recent text-start.
    TextDelta {
        id: String,
        delta: String,
    },
    /// Synthetic text-block closer.
    TextEnd {
        id: String,
    },
    ToolInputStart {
        #[serde(rename = "toolCallId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        tool_call_id: ToolCallId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(rename = "agentName")]
        agent_name: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "parallelBatchId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string | null"))]
        parallel_batch_id: Option<ParallelBatchId>,
    },
    ToolInputDelta {
        #[serde(rename = "toolCallId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        tool_call_id: ToolCallId,
        #[serde(rename = "inputTextDelta")]
        input_text_delta: String,
    },
    /// Synthetic event after parsing accumulated tool-input-delta into JSON.
    ToolInputAvailable {
        #[serde(rename = "toolCallId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        tool_call_id: ToolCallId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
        input: serde_json::Value,
        // Bug 18: mirrors the parallelBatchId carried by tool-input-start so
        // the UI can group parallel tool calls consistently.
        #[serde(skip_serializing_if = "Option::is_none", rename = "parallelBatchId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string | null"))]
        parallel_batch_id: Option<ParallelBatchId>,
    },
    /// Tool execution result. output is opaque String.
    ToolOutputAvailable {
        #[serde(rename = "toolCallId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        tool_call_id: ToolCallId,
        output: String,
    },
    /// Inline media attachment. media_type is required.
    File {
        url: String,
        #[serde(rename = "mediaType")]
        media_type: String,
    },
    /// Rich-card payload. Newtype variant — discriminator `cardType` lives
    /// at top level alongside `type`.
    RichCard(RichCardData),
    ClarifyNeeded {
        #[serde(rename = "clarifyId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        clarify_id: uuid::Uuid,
        question: String,
        choices: Vec<String>,
        #[serde(rename = "timeoutMs")]
        #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
        timeout_ms: u64,
    },
    ToolApprovalNeeded {
        #[serde(rename = "approvalId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        approval_id: ApprovalId,
        #[serde(rename = "toolName")]
        tool_name: String,
        #[serde(rename = "toolInput")]
        #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
        tool_input: serde_json::Value,
        /// u64 to match StreamEvent. Rendered as `number` in TS via override.
        #[serde(rename = "timeoutMs")]
        #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
        timeout_ms: u64,
    },
    /// `action` is a typed enum (`ApprovalAction`) — wire shape is identical
    /// to the previous `String` form (`"approved" | "rejected" | "timeout_rejected"`).
    ToolApprovalResolved {
        #[serde(rename = "approvalId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string"))]
        approval_id: ApprovalId,
        action: ApprovalAction,
        #[serde(skip_serializing_if = "Option::is_none", rename = "modifiedInput")]
        #[cfg_attr(feature = "ts-gen", ts(type = "unknown | null"))]
        modified_input: Option<serde_json::Value>,
    },
    /// End-of-stream. agent_name always emitted. finish_reason and
    /// continuation from StreamEvent::Finish are dropped on wire.
    Finish {
        #[serde(rename = "agentName")]
        agent_name: String,
    },
    Error {
        #[serde(rename = "errorText")]
        error_text: String,
    },
    /// Server-side LLM retry signal (also client-emitted with same shape
    /// on connection loss). delay_ms is snake_case on wire (pre-existing).
    Reconnecting {
        attempt: u32,
        #[serde(rename = "delay_ms")]
        #[cfg_attr(feature = "ts-gen", ts(type = "number"))]
        delay_ms: u64,
    },
    /// Resume snapshot — emitted by `gateway/handlers/chat/stream.rs`
    /// (`api_chat_stream`'s DB-only branch) when a client connects/reconnects
    /// to a session with no live in-memory stream (finished/errored/
    /// interrupted, or Core restarted mid-run). NOT emitted by
    /// sse_converter (live streaming path); only by the GET stream handler's
    /// `stream_jobs` fallback.
    Sync {
        content: String,
        #[serde(rename = "toolCalls")]
        #[cfg_attr(feature = "ts-gen", ts(type = "Array<unknown>"))]
        tool_calls: Vec<serde_json::Value>,
        status: SyncStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Открывает авторитетный snapshot-конверт: всё, что придёт до SyncEnd,
    /// клиент применяет батчем (без анимации). boundaryMessageId — id
    /// user-сообщения активного хода: история рендерится ВПЛОТЬ ДО него
    /// включительно, всё после — live-состояние. None + finished = активного
    /// хода нет, конверт пуст (клиент рисует чисто REST-историю).
    #[serde(rename = "sync_begin")]
    SyncBegin {
        #[serde(rename = "boundaryMessageId")]
        #[cfg_attr(feature = "ts-gen", ts(type = "string | null"))]
        boundary_message_id: Option<uuid::Uuid>,
        #[serde(rename = "runStatus")]
        run_status: SyncStatus,
        /// Буфер переполнился — replay неполон; клиент берёт частичный текст
        /// из REST (streaming_db персистит инкрементально) + хвост буфера.
        truncated: bool,
    },
    /// Закрывает конверт. lastSeq — seq последнего replay-события (None при
    /// пустом конверте). После него идут live-события.
    #[serde(rename = "sync_end")]
    SyncEnd {
        #[serde(rename = "lastSeq")]
        #[cfg_attr(feature = "ts-gen", ts(type = "number | null"))]
        last_seq: Option<u64>,
    },
    Usage(UsagePayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DataSessionIdPayload {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(tag = "cardType", content = "data", rename_all = "snake_case")]
pub enum RichCardData {
    Table(TableCard),
    Metric(MetricCard),
    /// Fallback for unknown cardType or malformed payload. Keeps From
    /// infallible. Emitted by `SseEvent::rich_card_from_stream` when
    /// card_type is not recognized OR `serde_json::from_value` fails.
    Other {
        #[serde(rename = "cardType")]
        card_type: String,
        #[cfg_attr(feature = "ts-gen", ts(type = "unknown"))]
        data: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct TableCard {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub columns: Vec<String>,
    /// Each cell is mixed string|number per existing UI consumer.
    #[cfg_attr(feature = "ts-gen", ts(type = "Array<Array<string | number>>"))]
    pub rows: Vec<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct MetricCard {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend: Option<MetricTrend>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum MetricTrend {
    Up,
    Down,
    Flat,
}

/// Status values for Sync event. Verified against the `job.status` →
/// `SyncStatus` mapping in `gateway/handlers/chat/stream.rs`'s
/// `api_chat_stream` DB-only branch (line-pinning avoided here — that
/// function has moved once already after the `resume.rs` → `stream.rs`
/// rename).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum SyncStatus {
    Finished,
    Error,
    Interrupted,
    Running,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-gen", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct UsagePayload {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Required — converter ALWAYS emits the currently-responding agent.
    pub agent_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
}

impl SseEvent {
    /// Best-effort RichCard mapping. Falls back to `RichCardData::Other`
    /// if card_type is unknown OR data doesn't deserialize into the
    /// known shape. Always returns Ok — never panics.
    pub fn rich_card_from_stream(card_type: String, data: serde_json::Value) -> Self {
        let inner = match card_type.as_str() {
            "table" => match serde_json::from_value::<TableCard>(data.clone()) {
                Ok(t) => RichCardData::Table(t),
                Err(_) => RichCardData::Other { card_type, data },
            },
            "metric" => match serde_json::from_value::<MetricCard>(data.clone()) {
                Ok(m) => RichCardData::Metric(m),
                Err(_) => RichCardData::Other { card_type, data },
            },
            _ => RichCardData::Other { card_type, data },
        };
        SseEvent::RichCard(inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_card_known_table_routes_to_table_variant() {
        let ev = SseEvent::rich_card_from_stream(
            "table".to_string(),
            serde_json::json!({"columns": ["a"], "rows": [["x"]]}),
        );
        match ev {
            SseEvent::RichCard(RichCardData::Table(t)) => {
                assert_eq!(t.columns, vec!["a"]);
            }
            _ => panic!("expected Table variant"),
        }
    }

    #[test]
    fn rich_card_unknown_type_routes_to_other() {
        let ev = SseEvent::rich_card_from_stream(
            "unknown_card".to_string(),
            serde_json::json!({"foo": "bar"}),
        );
        match ev {
            SseEvent::RichCard(RichCardData::Other { card_type, data }) => {
                assert_eq!(card_type, "unknown_card");
                assert_eq!(data, serde_json::json!({"foo": "bar"}));
            }
            _ => panic!("expected Other variant"),
        }
    }

    #[test]
    fn rich_card_table_with_malformed_data_falls_back_to_other() {
        // TableCard requires `columns: Vec<String>` and `rows: Vec<Vec<Value>>`.
        // Pass invalid data (columns is a number) to trigger fallback.
        let ev = SseEvent::rich_card_from_stream(
            "table".to_string(),
            serde_json::json!({"columns": 42, "rows": []}),
        );
        match ev {
            SseEvent::RichCard(RichCardData::Other { card_type, .. }) => {
                assert_eq!(card_type, "table");
            }
            _ => panic!("expected Other fallback on malformed table"),
        }
    }

    #[test]
    fn sync_envelope_wire_format() {
        let b = SseEvent::SyncBegin {
            boundary_message_id: None,
            run_status: SyncStatus::Finished,
            truncated: false,
        };
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains("\"sync_begin\""), "{s}");
        let e = SseEvent::SyncEnd { last_seq: Some(41) };
        assert!(serde_json::to_string(&e).unwrap().contains("\"sync_end\""));
    }
}
